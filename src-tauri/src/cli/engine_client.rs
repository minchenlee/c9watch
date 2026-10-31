//! CLI side of the c9 engine: `c9watch engine`, `c9watch engine restart`,
//! `c9watch api <method> '<json>'`, and auto-start.
//!
//! Auto-start follows `cli/pm.rs` (`ensure_daemon`): a client-side spawn
//! lock (`engine.spawn.lock`) makes sure only one caller spawns, and the
//! engine runs in its own session (`setsid`) with an allowlisted
//! environment. The engine counts as up only when `hello` succeeds, which is
//! stricter than the daemon's lock probe.
//!
//! Nothing here talks to the legacy `c9watch daemon`: no RPC, no
//! `retire_legacy_daemon`, on every channel.

use crate::engine::fsutil::ensure_private_dir;
use crate::engine::home::{engine_home, lock_path, sock_path};
use crate::engine::lock::{engine_lock_held, engine_pid};
use crate::engine::peer::{self, PeerCred};
use crate::engine::proto::{
    ClientInfo, HelloParams, HelloResult, Request, Response, API_MAJOR, API_MINOR,
};
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Error code: a caller inside an engine agent (`C9_AGENT_ID` set) may not
/// run or auto-start the engine. This guards against mistakes, not attacks:
/// `C9_AGENT_ID` is not a secret.
pub const AGENT_CALLER_REFUSED: &str = "agent_caller_refused";
/// Error code: the engine did not answer `hello` in time.
pub const ENGINE_NOT_READY: &str = "engine_not_ready";

pub(crate) const ENV_AGENT_ID: &str = "C9_AGENT_ID";

/// How long a client waits for a new engine to answer `hello`.
const READY_TIMEOUT: Duration = Duration::from_secs(10);
/// How long `engine restart` waits for the old engine to release its lock:
/// its drain may take up to the 90 s hard deadline (C1a-27), plus room to
/// close.
const EXIT_TIMEOUT: Duration =
    Duration::from_secs(crate::engine::drain::HARD_DEADLINE.as_secs() + 5);
/// Read and write timeout of one engine connection.
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(50);

/// The only variables an auto-started engine inherits. Everything else
/// (agent ids, engine sockets, API keys, `CLAUDE_*`) is dropped, so the
/// engine never runs with a caller's session identity.
pub const ENGINE_ENV_ALLOWLIST: &[&str] = &[
    "HOME",
    "PATH",
    "USER",
    "LOGNAME",
    "SHELL",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "XDG_STATE_HOME",
    "XDG_RUNTIME_DIR",
    "RUST_BACKTRACE",
    crate::engine::home::ENGINE_HOME_ENV,
];

/// Fail with `agent_caller_refused` when `C9_AGENT_ID` is set to anything
/// but the empty string, including a value that is not UTF-8.
pub fn refuse_agent_caller(get: &dyn Fn(&str) -> Option<OsString>) -> Result<(), String> {
    match get(ENV_AGENT_ID).filter(|v| !v.is_empty()) {
        Some(id) => Err(format!(
            "{}: {} is set ({}); agents may not run or auto-start the engine",
            AGENT_CALLER_REFUSED,
            ENV_AGENT_ID,
            id.to_string_lossy()
        )),
        None => Ok(()),
    }
}

/// The process environment, as `refuse_agent_caller` reads it.
fn process_env(k: &str) -> Option<OsString> {
    std::env::var_os(k)
}

/// Keep only the allowlisted variables of `vars`.
pub fn engine_env(
    vars: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    vars.into_iter()
        .filter(|(k, _)| ENGINE_ENV_ALLOWLIST.iter().any(|a| k == *a))
        .collect()
}

// ── Connection ────────────────────────────────────────────────────────

/// One c9p/1 connection after a successful `hello`.
pub struct EngineConn {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
    pub hello: HelloResult,
}

/// Connect to `sock` and check that the server runs under our euid. A
/// socket served by another uid is refused before anything is sent.
fn connect_checked(sock: &Path) -> Result<(UnixStream, PeerCred), ConnectError> {
    let stream = UnixStream::connect(sock).map_err(|e| {
        ConnectError::Unreachable(format!("cannot reach the engine at {:?}: {}", sock, e))
    })?;
    let server = peer::peer_cred(stream.as_raw_fd())
        .and_then(|c| peer::check_peer_uid(&c, peer::engine_uid()).map(|_| c))
        .map_err(|e| ConnectError::Refused(format!("engine socket {:?}: {}", sock, e)))?;
    Ok((stream, server))
}

/// Why `EngineConn::connect` failed.
#[derive(Debug)]
pub enum ConnectError {
    /// No engine answers on the socket (yet).
    Unreachable(String),
    /// An engine answered `hello` with an error. Waiting does not help.
    Refused(String),
}

impl ConnectError {
    pub(crate) fn into_msg(self) -> String {
        match self {
            ConnectError::Unreachable(m) | ConnectError::Refused(m) => m,
        }
    }
}

impl EngineConn {
    /// Connect to `sock` and send `hello`.
    pub fn connect(sock: &Path) -> Result<Self, ConnectError> {
        let (stream, _) = connect_checked(sock)?;
        let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
        let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
        let writer = stream
            .try_clone()
            .map_err(|e| ConnectError::Unreachable(format!("Failed to clone socket: {}", e)))?;
        let mut conn = EngineConn {
            reader: BufReader::new(stream),
            writer,
            next_id: 1,
            hello: placeholder_hello(),
        };
        let params = HelloParams {
            api_major: Some(API_MAJOR),
            client: Some(ClientInfo {
                kind: Some("cli".into()),
                name: Some("c9watch".into()),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
            want: Vec::new(),
            api_minor: Some(API_MINOR),
        };
        let params = serde_json::to_value(params)
            .map_err(|e| ConnectError::Refused(format!("Failed to encode hello: {}", e)))?;
        // A write or read failure here means the engine is not serving yet
        // (or went away); an error response means it refused us.
        let resp = conn
            .call("hello", params)
            .map_err(ConnectError::Unreachable)?;
        if !resp.ok {
            return Err(ConnectError::Refused(error_line(&resp)));
        }
        conn.hello = serde_json::from_value(resp.result.unwrap_or_default())
            .map_err(|e| ConnectError::Refused(format!("bad hello result: {}", e)))?;
        Ok(conn)
    }

    /// Wait up to `d` for each response instead of the default 5 s, for a
    /// method that answers late (`agent.create` answers once the agent is
    /// ready).
    pub fn set_read_timeout(&mut self, d: Duration) {
        let _ = self.reader.get_ref().set_read_timeout(Some(d));
    }

    /// Send one request and read its response.
    pub fn call(&mut self, method: &str, params: serde_json::Value) -> Result<Response, String> {
        self.call_idem(method, params, None)
    }

    /// `call` with the envelope's `idem` key, which makes a resend safe for
    /// a method that honours it (`msg.send`).
    pub fn call_idem(
        &mut self,
        method: &str,
        params: serde_json::Value,
        idem: Option<&str>,
    ) -> Result<Response, String> {
        let id = self.next_id.to_string();
        self.next_id += 1;
        let req = Request {
            v: Some(API_MAJOR),
            id: Some(id),
            method: Some(method.to_string()),
            params,
            idem: idem.map(str::to_string),
        };
        let mut line =
            serde_json::to_vec(&req).map_err(|e| format!("Failed to encode the request: {}", e))?;
        line.push(b'\n');
        self.writer
            .write_all(&line)
            .map_err(|e| format!("Failed to write to the engine: {}", e))?;
        let mut buf = String::new();
        let n = self
            .reader
            .read_line(&mut buf)
            .map_err(|e| format!("No response from the engine: {}", e))?;
        if n == 0 {
            return Err("The engine closed the connection".into());
        }
        serde_json::from_str(buf.trim_end())
            .map_err(|e| format!("Bad response from the engine ({}): {:?}", e, buf.trim_end()))
    }
}

/// The frames an `events.subscribe` connection reads after its response.
/// A reader thread sends each line, with no read timeout, until the
/// connection closes. Dropping it shuts the connection down, which ends the
/// thread.
pub struct Frames {
    pub lines: mpsc::Receiver<String>,
    stream: UnixStream,
}

impl Drop for Frames {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

impl EngineConn {
    /// Read the rest of the connection on a thread, line by line. Lines the
    /// reader already buffered come first, so no frame is lost.
    pub fn into_frames(self) -> Result<Frames, String> {
        let stream = self
            .writer
            .try_clone()
            .map_err(|e| format!("Failed to clone socket: {}", e))?;
        let mut reader = self.reader;
        let _ = reader.get_ref().set_read_timeout(None);
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            let _writer = self.writer;
            let mut buf = String::new();
            loop {
                buf.clear();
                match reader.read_line(&mut buf) {
                    Ok(n) if n > 0 && buf.ends_with('\n') => {
                        if tx.send(buf.trim_end().to_string()).is_err() {
                            return;
                        }
                    }
                    // EOF, an error, or a last line cut short.
                    _ => return,
                }
            }
        });
        Ok(Frames { lines, stream })
    }
}

fn placeholder_hello() -> HelloResult {
    HelloResult {
        api_major: 0,
        api_minor: 0,
        engine_version: String::new(),
        machine_id: String::new(),
        principal: String::new(),
        capabilities: Vec::new(),
        cursor: None,
        ..Default::default()
    }
}

/// `"<code>: <msg>"` of an error response.
pub(crate) fn error_line(resp: &Response) -> String {
    match &resp.error {
        Some(e) => format!("{}: {}", e.code, e.msg),
        None => "the engine sent ok:false without an error".into(),
    }
}

// ── Auto-start ────────────────────────────────────────────────────────

/// `<home>/engine.spawn.lock`: clients take it to serialize spawning.
fn spawn_lock_path(home: &Path) -> PathBuf {
    home.join("engine.spawn.lock")
}

/// `<home>/engine.log`: the auto-started engine's stdout and stderr.
pub fn log_path(home: &Path) -> PathBuf {
    home.join("engine.log")
}

/// Open `path` 0600 without following a symlink.
fn open_private(path: &Path, opts: &mut OpenOptions) -> Result<File, String> {
    opts.mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| match e.raw_os_error() {
            Some(libc::ELOOP) => format!("{:?} is a symlink, refusing to use it", path),
            _ => format!("Failed to open {:?}: {}", path, e),
        })
}

/// The last non-empty line the engine wrote to `engine.log` after byte
/// `from`, cut to 500 bytes. An engine that fails at boot (for example with
/// `db_newer_than_engine`) prints its error there before it exits.
fn log_tail(home: &Path, from: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = File::open(log_path(home)).ok()?;
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut buf = Vec::new();
    f.take(64 * 1024).read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let line = text.lines().rev().map(str::trim).find(|l| !l.is_empty())?;
    let mut end = line.len().min(500);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    Some(line[..end].to_string())
}

/// Poll `hello` on `sock` until it succeeds. Fails early when the engine
/// refuses us or when `child` (the engine we spawned, and the `engine.log`
/// offset it started writing at) exits.
fn wait_until_ready(
    home: &Path,
    mut child: Option<(&mut Child, u64)>,
    timeout: Duration,
) -> Result<HelloResult, String> {
    let sock = sock_path(home);
    let deadline = Instant::now() + timeout;
    loop {
        let last = match EngineConn::connect(&sock) {
            Ok(conn) => return Ok(conn.hello),
            Err(ConnectError::Refused(e)) => return Err(e),
            Err(ConnectError::Unreachable(e)) => e,
        };
        if let Some((c, log_from)) = child.as_mut() {
            if let Ok(Some(status)) = c.try_wait() {
                let said = log_tail(home, *log_from)
                    .map(|l| format!(": {}", l))
                    .unwrap_or_default();
                return Err(format!(
                    "The engine exited ({}) before it answered hello{}; see {:?}",
                    status,
                    said,
                    log_path(home)
                ));
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{}: no hello within {} s ({}); see {:?}",
                ENGINE_NOT_READY,
                timeout.as_secs(),
                last,
                log_path(home)
            ));
        }
        std::thread::sleep(POLL);
    }
}

/// Make sure an engine serves `home`, spawning `exe engine` if none does.
/// Returns the engine's `hello` result.
pub fn ensure_engine(home: &Path, exe: &Path) -> Result<HelloResult, String> {
    ensure_engine_with(home, exe, &process_env)
}

/// `ensure_engine` with the caller's environment read through `env`.
fn ensure_engine_with(
    home: &Path,
    exe: &Path,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> Result<HelloResult, String> {
    refuse_agent_caller(env)?;
    ensure_private_dir(home)?;

    // Fast path: an engine answers.
    match EngineConn::connect(&sock_path(home)) {
        Ok(conn) => return Ok(conn.hello),
        Err(ConnectError::Refused(e)) => return Err(e),
        Err(ConnectError::Unreachable(_)) => {}
    }
    // An engine holds the lock but is still binding.
    if engine_lock_held(&lock_path(home))? {
        return wait_until_ready(home, None, READY_TIMEOUT);
    }

    // Only the holder of the spawn lock spawns. The others wait for hello.
    let spawn_lock = open_private(
        &spawn_lock_path(home),
        OpenOptions::new().create(true).read(true).write(true),
    )?;
    if unsafe { libc::flock(spawn_lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        drop(spawn_lock);
        return wait_until_ready(home, None, READY_TIMEOUT);
    }
    // Re-check under the spawn lock: another caller may have started the
    // engine between our probe and the lock.
    if engine_lock_held(&lock_path(home))? {
        return wait_until_ready(home, None, READY_TIMEOUT);
    }

    let log_from = std::fs::metadata(log_path(home)).map_or(0, |m| m.len());
    let mut child = spawn_engine(home, exe)?;
    // `spawn_lock` is released when we return; by then the engine holds
    // `engine.lock`, so other callers take the fast path.
    wait_until_ready(home, Some((&mut child, log_from)), READY_TIMEOUT)
}

/// Spawn `exe engine` in a new session, with the allowlisted environment
/// and its output appended to `engine.log`.
fn spawn_engine(home: &Path, exe: &Path) -> Result<Child, String> {
    let log = open_private(
        &log_path(home),
        OpenOptions::new().create(true).append(true),
    )?;
    let log_err = log
        .try_clone()
        .map_err(|e| format!("Failed to clone log file handle: {}", e))?;
    let mut cmd = Command::new(exe);
    cmd.arg("engine")
        .env_clear()
        .envs(engine_env(std::env::vars_os()))
        // Pin the home, so the engine serves the home this client resolved.
        .env(crate::engine::home::ENGINE_HOME_ENV, home)
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    // SAFETY: setsid is async-signal-safe and touches no Rust state.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn()
        .map_err(|e| format!("Failed to spawn the engine {:?}: {}", exe, e))
}

// ── Restart ───────────────────────────────────────────────────────────

/// The pid of the engine that serves `home`, taken from a connection to its
/// socket (the kernel's peer pid), not from the lock file alone: the file
/// can still hold a dead engine's pid while a new engine starts. The socket
/// peer must run under our uid and must be the pid the lock holder wrote.
/// Waits up to `timeout` for an engine that holds the lock but has not
/// bound yet.
fn serving_engine_pid(home: &Path, timeout: Duration) -> Result<libc::pid_t, String> {
    let deadline = Instant::now() + timeout;
    let server = loop {
        match connect_checked(&sock_path(home)) {
            Ok((_, server)) => break server,
            Err(ConnectError::Refused(e)) => return Err(e),
            Err(ConnectError::Unreachable(e)) => {
                if Instant::now() >= deadline {
                    return Err(format!(
                        "{}: the engine holds {:?} but does not serve its socket ({})",
                        ENGINE_NOT_READY,
                        lock_path(home),
                        e
                    ));
                }
            }
        }
        std::thread::sleep(POLL);
    };
    match engine_pid(&lock_path(home))? {
        Some(pid) if pid == server.pid => Ok(pid),
        other => Err(format!(
            "The engine socket is served by pid {}, but the lock holder is {:?}; not signalling anything",
            server.pid, other
        )),
    }
}

/// SIGTERM the engine of `home`, wait for it to exit, start a new one with
/// `exe`, and return the new engine's `hello`. Starts an engine when none
/// runs. Live agents are fine: the SIGTERM drains them and the new engine
/// resumes them (C1a-27, C1a-28).
pub fn restart_engine(home: &Path, exe: &Path) -> Result<HelloResult, String> {
    restart_engine_with(home, exe, &process_env)
}

fn restart_engine_with(
    home: &Path,
    exe: &Path,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> Result<HelloResult, String> {
    refuse_agent_caller(env)?;
    ensure_private_dir(home)?;
    let lock = lock_path(home);
    if engine_lock_held(&lock)? {
        let pid = serving_engine_pid(home, READY_TIMEOUT)?;
        if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::ESRCH) {
                return Err(format!("Failed to SIGTERM engine pid {}: {}", pid, err));
            }
        }
        let deadline = Instant::now() + EXIT_TIMEOUT;
        // Wait for this engine to let go. A lock held by another pid is a
        // new engine that some other client (say a `watch` reconnecting)
        // already started: the old one is gone.
        while engine_lock_held(&lock)? && engine_pid(&lock)? == Some(pid) {
            if Instant::now() >= deadline {
                return Err(format!(
                    "Engine pid {} still holds {:?} {} s after SIGTERM",
                    pid,
                    lock,
                    EXIT_TIMEOUT.as_secs()
                ));
            }
            std::thread::sleep(POLL);
        }
        // `watch` and `wait` wait `RESTART_GRACE` before they start an
        // engine, so this is rare; the new engine then runs the other
        // client's binary and environment, not ours.
        if let Some(other) = engine_pid(&lock)?.filter(|p| *p != pid) {
            eprintln!(
                "[c9watch] Another client started engine pid {} before this restart; \
                 it runs that client's binary and environment",
                other
            );
        }
    }
    ensure_engine_with(home, exe, env)
}

// ── Commands ──────────────────────────────────────────────────────────

pub(crate) fn current_exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("Failed to get current exe path: {}", e))
}

/// `c9watch engine`: run the engine in the foreground until SIGTERM/SIGINT.
pub fn cmd_engine_run() -> Result<(), String> {
    refuse_agent_caller(&process_env)?;
    let config = crate::engine::run::EngineConfig::new(engine_home()?)?;
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| format!("Failed to create tokio runtime: {}", e))?;
    let done = rt.block_on(crate::engine::run::run(&config));
    // The drain's hard deadline bounds the exit too: dropping the runtime
    // would wait for a supervisor stuck in a kill (reviews L4).
    rt.shutdown_timeout(Duration::from_secs(1));
    done
}

/// `c9watch engine restart`
pub fn cmd_engine_restart(pretty: bool) -> Result<(), String> {
    let home = engine_home()?;
    let hello = restart_engine(&home, &current_exe()?)?;
    super::print_json(&hello, pretty)
}

/// `c9watch api <method> '<json>'`: auto-start the engine, send one raw
/// request, print the response frame. An `ok:false` response exits 1.
pub fn cmd_api(method: &str, params: &str, pretty: bool) -> Result<(), String> {
    refuse_agent_caller(&process_env)?;
    let params: serde_json::Value =
        serde_json::from_str(params).map_err(|e| format!("api: params is not JSON: {}", e))?;
    let home = engine_home()?;
    ensure_engine(&home, &current_exe()?)?;
    let mut conn = EngineConn::connect(&sock_path(&home)).map_err(ConnectError::into_msg)?;
    let resp = conn.call(method, params)?;
    super::print_json(&resp, pretty)?;
    if resp.ok {
        Ok(())
    } else {
        Err(error_line(&resp))
    }
}

/// `c9watch integration status|show`: the adapters the engine knows and
/// their integration files. Reads files only; never starts the engine and
/// never writes a vendor config (managed CC gets its hooks per spawn
/// through `--settings`, D24.1; observed sessions use `c9watch hooks`).
pub fn cmd_integration(action: &super::IntegrationAction, pretty: bool) -> Result<(), String> {
    let home = engine_home()?;
    let report = integration_report(action, &home)?;
    super::print_json(&report, pretty)
}

/// The JSON `cmd_integration` prints for `home`.
pub fn integration_report(
    action: &super::IntegrationAction,
    home: &Path,
) -> Result<serde_json::Value, String> {
    use crate::engine::adapter::{claude_hook_settings, descriptor, DESCRIPTORS};
    use crate::engine::agent_bin::installed_path;
    let bin = installed_path(home);
    match action {
        super::IntegrationAction::Status => {
            let kinds: Vec<serde_json::Value> = DESCRIPTORS
                .iter()
                .map(|d| d.integration_report(&bin))
                .collect();
            Ok(serde_json::json!({ "adapters": kinds }))
        }
        super::IntegrationAction::Show { kind } => match descriptor(kind) {
            Some(d) if d.kind == "claude" => Ok(claude_hook_settings(&bin)),
            Some(d) => Ok(d.integration_report(&bin)),
            _ => Err(format!("integration: unknown adapter kind {:?}", kind)),
        },
    }
}

/// `c9watch engine shadow-report`: the dev GUI's daily shadow diff
/// coverage from `<home>/shadow-coverage.ndjson`, oldest day first, plus
/// where the unexplained diffs are. Reads files only; never starts the
/// engine.
#[cfg(c9_channel_dev)]
pub fn cmd_shadow_report(pretty: bool) -> Result<(), String> {
    use crate::engine_proto::shadow::{read_coverage, DIFF_FILE};
    let home = engine_home()?;
    let days = read_coverage(&home)?;
    let report = serde_json::json!({
        "days": days,
        "diffs": home.join(DIFF_FILE),
    });
    super::print_json(&report, pretty)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::home::ENGINE_HOME_ENV;
    use crate::engine::lock::acquire_engine_lock;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let m: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        move |k| m.get(k).cloned()
    }

    /// No `C9_AGENT_ID`, whatever the shell running the tests has set.
    fn no_env(_: &str) -> Option<OsString> {
        None
    }

    #[test]
    fn agent_callers_are_refused() {
        let err = refuse_agent_caller(&env_of(&[("C9_AGENT_ID", "a1")])).unwrap_err();
        assert!(err.starts_with(AGENT_CALLER_REFUSED), "{err}");
        assert!(refuse_agent_caller(&env_of(&[("C9_AGENT_ID", "")])).is_ok());
        assert!(refuse_agent_caller(&env_of(&[])).is_ok());

        // A value that is not UTF-8 is still set.
        use std::os::unix::ffi::OsStringExt;
        let bad = |_: &str| Some(OsString::from_vec(vec![0xff, b'a']));
        let err = refuse_agent_caller(&bad).unwrap_err();
        assert!(err.starts_with(AGENT_CALLER_REFUSED), "{err}");
    }

    #[test]
    fn the_engine_env_keeps_only_the_allowlist() {
        let vars = [
            ("HOME", "/h"),
            ("PATH", "/bin"),
            (ENGINE_HOME_ENV, "/e"),
            ("C9_AGENT_ID", "a1"),
            ("C9_ENGINE_SOCK", "/s"),
            ("C9_AGENT_TOKEN", "t"),
            ("ANTHROPIC_API_KEY", "k"),
            ("CLAUDECODE", "1"),
        ]
        .map(|(k, v)| (OsString::from(k), OsString::from(v)));
        let mut kept: Vec<String> = engine_env(vars)
            .into_iter()
            .map(|(k, _)| k.into_string().unwrap())
            .collect();
        kept.sort();
        assert_eq!(kept, ["C9WATCH_ENGINE_HOME", "HOME", "PATH"]);
    }

    #[test]
    fn an_engine_that_exits_early_fails_the_auto_start() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("e");
        // `false engine` exits 1 without binding anything.
        let err = ensure_engine_with(&home, Path::new("/usr/bin/false"), &no_env).unwrap_err();
        assert!(err.contains("exited"), "{err}");
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(log_path(&home))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(!sock_path(&home).exists());
    }

    #[test]
    fn restart_signals_only_the_pid_that_serves_the_socket() {
        use std::os::unix::net::UnixListener;
        let t = tempfile::tempdir().unwrap();
        // The lock is held, but the file names another pid than the socket's
        // server (this test process): a stale pid. Nothing is signalled.
        let lock = acquire_engine_lock(&lock_path(t.path())).unwrap();
        std::fs::write(lock_path(t.path()), b"999999").unwrap();
        let _listener = UnixListener::bind(sock_path(t.path())).unwrap();
        let err = restart_engine_with(t.path(), Path::new("/nonexistent"), &no_env).unwrap_err();
        assert!(err.contains("not signalling anything"), "{err}");

        // Held lock, empty pid file: also an error, not a silent no-op.
        std::fs::write(lock_path(t.path()), b"").unwrap();
        let err = serving_engine_pid(t.path(), Duration::ZERO).unwrap_err();
        assert!(err.contains("None"), "{err}");

        // When both facts agree, that pid is the target.
        std::fs::write(lock_path(t.path()), std::process::id().to_string()).unwrap();
        assert_eq!(
            serving_engine_pid(t.path(), Duration::ZERO).unwrap(),
            std::process::id() as libc::pid_t
        );
        drop(lock);
    }

    #[test]
    fn a_held_lock_without_a_socket_is_not_ready() {
        let t = tempfile::tempdir().unwrap();
        let _lock = acquire_engine_lock(&lock_path(t.path())).unwrap();
        let err = serving_engine_pid(t.path(), Duration::from_millis(100)).unwrap_err();
        assert!(err.starts_with(ENGINE_NOT_READY), "{err}");
    }

    #[test]
    fn a_refused_hello_is_not_retried() {
        use std::os::unix::net::UnixListener;
        let t = tempfile::tempdir().unwrap();
        let sock = sock_path(t.path());
        let listener = UnixListener::bind(&sock).unwrap();
        let server = std::thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&s).read_line(&mut line).unwrap();
            let resp = Response::err(
                Some("1".into()),
                crate::engine::proto::ApiError::new(
                    crate::engine::proto::ErrorCode::UpgradeRequired,
                    "old",
                ),
            );
            (&s).write_all(&crate::engine::proto::encode(&resp))
                .unwrap();
        });
        let start = Instant::now();
        let err = wait_until_ready(t.path(), None, Duration::from_secs(5)).unwrap_err();
        assert!(err.starts_with("upgrade_required"), "{err}");
        assert!(start.elapsed() < Duration::from_secs(4));
        server.join().unwrap();
    }

    #[test]
    fn integration_reports_the_adapters_and_the_per_spawn_hooks() {
        use super::super::IntegrationAction;
        use crate::engine::agent_bin::installed_path;
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("engine home");
        let status = integration_report(&IntegrationAction::Status, &home).unwrap();
        let claude = &status["adapters"][0];
        assert_eq!(claude["descriptor"]["kind"], "claude");
        assert_eq!(claude["descriptor"]["provider"], "claudeCode");
        assert_eq!(claude["status"], "missing");
        // The engine installed its agent copy: CC's integration is current.
        let bin = installed_path(&home);
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, b"x").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o555)).unwrap();
        let status = integration_report(&IntegrationAction::Status, &home).unwrap();
        assert_eq!(status["adapters"][0]["status"], "current");
        let show = IntegrationAction::Show {
            kind: "claude".into(),
        };
        let hooks = integration_report(&show, &home).unwrap();
        let stop = hooks["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(
            stop.contains("engine home/bin/c9watch-agent' hook stop"),
            "{stop}"
        );
        let bad = IntegrationAction::Show {
            kind: "grok".into(),
        };
        assert!(integration_report(&bad, &home).is_err());
    }

    #[test]
    fn all_p1_kinds_are_reported_without_installing_or_claiming_unshipped_hooks() {
        use super::super::IntegrationAction;
        use crate::engine::adapter::DESCRIPTORS;
        use crate::engine::agent_bin::installed_path;
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("engine");
        let status = integration_report(&IntegrationAction::Status, &home).unwrap();
        assert!(
            !home.exists(),
            "status must not create an engine or vendor home"
        );
        let adapters = status["adapters"].as_array().unwrap();
        assert_eq!(adapters.len(), 7);
        for (d, actual) in DESCRIPTORS.iter().zip(adapters) {
            assert_eq!(actual["descriptor"]["kind"], d.kind);
            assert_eq!(actual["status"], "missing");
        }
        // Installing CC's agent binary must not mark the other kinds current.
        let bin = installed_path(&home);
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, b"fake helper").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o555)).unwrap();
        let status = integration_report(&IntegrationAction::Status, &home).unwrap();
        assert_eq!(status["adapters"][0]["status"], "current");
        for (i, d) in DESCRIPTORS.iter().enumerate().skip(1) {
            let shown = integration_report(
                &IntegrationAction::Show {
                    kind: d.kind.into(),
                },
                &home,
            )
            .unwrap();
            assert_eq!(shown, status["adapters"][i]);
            assert_eq!(shown["status"], "missing");
            assert_eq!(shown["reason"], "engine_integration_not_implemented");
            assert_eq!(shown["integrationFiles"], serde_json::json!([]));
        }
        assert_eq!(std::fs::read_dir(t.path()).unwrap().count(), 1);
        assert_eq!(std::fs::read_dir(&home).unwrap().count(), 1);
        assert_eq!(std::fs::read_dir(home.join("bin")).unwrap().count(), 1);
        assert_eq!(std::fs::read(&bin).unwrap(), b"fake helper");
    }
}
