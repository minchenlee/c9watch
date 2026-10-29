//! The native PTY driver: `openpty`, a child in its own session with the
//! PTY as its controlling terminal, and a reader thread on the master.
//!
//! Ported from the C0 spike (`c0-engine.rs:421-456` spawn, `:487-490` write,
//! `:749-766` kill and verify), on top of the libc crate instead of
//! portable-pty: the spike used one call of it, and the whole crate is a
//! dozen new dependencies.
//!
//! The reader thread feeds a VT model (`engine::vt`), so `screen` is the
//! rendered terminal, and answers the terminal queries the child sends
//! (cursor position, device attributes, colours) at once: CC waits for
//! those replies at startup.
//!
//! A writer thread owns every write to the master. A child that stops
//! reading its input fills the terminal's input queue, and a write then
//! blocks; on that thread it blocks neither the reader (which must keep
//! draining output) nor the engine (`write` gives up after
//! `WRITE_TIMEOUT`). Query replies wait in a bounded queue and are dropped
//! when it is full.
//!
//! Process-group rules:
//! - The child calls `setsid`, so its pid is also its process group id.
//!   Every process it starts stays in that group unless it calls `setsid`
//!   or `setpgid` itself.
//! - CC does call `setsid` for its Bash tool and its hooks (Node's
//!   `detached`). `kill` therefore also walks the PTY child's descendants
//!   by ppid and signals each of their groups, and waits until every
//!   descendant it saw is gone. A process whose parent had already exited
//!   before `kill` (moved to init) cannot be found this way.
//! - The kernel does not reuse a pid while a group with that id exists, so
//!   signalling the group is safe until the group is empty. Once it is
//!   empty, `kill` and `signal` stop sending.

use super::driver::{
    ChildId, Driver, KillReport, Output, ScreenSnapshot, SpawnSpec, KILL_GRACE, KILL_VERIFY,
};
use super::peer;
use super::vt::{self, Screen};
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Unread output kept at most. Older bytes are dropped and counted.
const PENDING_MAX: usize = 1 << 20;

/// How often `kill` polls the group.
const POLL: Duration = Duration::from_millis(20);

/// Writes queued for the writer thread at most.
const WRITE_QUEUE: usize = 64;

/// How long `write` waits for the child to take its bytes.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// One write for the writer thread.
enum Job {
    /// A query reply from the reader thread. Nobody waits for it.
    Reply(Vec<u8>),
    /// Input from `write`, which waits for the result.
    Input(Vec<u8>, SyncSender<io::Result<()>>),
}

/// The PTY child's descendants that `kill` has seen, by pid and start
/// time. A descendant stays a root for later walks after its own parent
/// exits, so its children are still found.
#[derive(Default)]
struct Descendants(Vec<peer::ProcEntry>);

impl Descendants {
    /// Walk the process list again from `root` and every descendant seen
    /// so far. Returns the newly seen ones.
    fn sweep(&mut self, root: ChildId) -> Vec<peer::ProcEntry> {
        let mut roots = vec![(root.pid, root.start)];
        roots.extend(self.0.iter().map(|e| (e.info.pid, e.info.start)));
        let found = peer::descendants(&peer::processes(), &roots);
        let new: Vec<_> = found
            .into_iter()
            .filter(|e| !roots.contains(&(e.info.pid, e.info.start)))
            .collect();
        self.0.extend(new.iter().cloned());
        new
    }

    /// Send `sig` to the groups of `procs` other than `pgid` and the
    /// engine's own, each group once, and only while a member seen in it
    /// still runs: then the group id is not free.
    fn signal(procs: &[peer::ProcEntry], pgid: libc::pid_t, sig: libc::c_int) {
        let mut sent = vec![pgid, unsafe { libc::getpgrp() }];
        for e in procs {
            if !sent.contains(&e.pgid) && runs(e) {
                sent.push(e.pgid);
                let _ = signal_group(e.pgid, sig);
            }
        }
    }

    /// Whether any descendant seen still runs.
    fn any_runs(&self) -> bool {
        self.0.iter().any(runs)
    }
}

/// Whether the process `e` names (same pid and start time) still runs. A
/// zombie runs nothing.
fn runs(e: &peer::ProcEntry) -> bool {
    peer::proc_entry(e.info.pid).is_some_and(|p| p.info.start == e.info.start && !p.zombie)
}

/// Output shared between the reader thread and the driver.
struct Buf {
    pending: Vec<u8>,
    dropped: u64,
    screen: Screen,
    last_output: Instant,
    eof: bool,
}

struct Shared {
    buf: Mutex<Buf>,
    ready: Condvar,
}

/// One child on a native PTY. Dropping it kills the process group.
pub struct NativePty {
    child: Child,
    id: ChildId,
    master: File,
    writer: SyncSender<Job>,
    shared: Arc<Shared>,
    exit: Option<ExitStatus>,
    /// `kill` has run and found the group gone.
    killed: bool,
}

fn winsize(cols: u16, rows: u16) -> libc::winsize {
    libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

/// `open(path, O_RDWR | O_NOCTTY | O_CLOEXEC)`.
fn open_tty(path: &CStr) -> io::Result<OwnedFd> {
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
        )
    };
    if fd == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// The path of the slave side of `master`.
#[cfg(target_os = "macos")]
fn slave_name(master: &OwnedFd) -> io::Result<CString> {
    // `ptsname` uses a static buffer; the ioctl does not.
    let mut buf = [0 as libc::c_char; 128];
    if unsafe {
        libc::ioctl(
            master.as_raw_fd(),
            libc::TIOCPTYGNAME as _,
            buf.as_mut_ptr(),
        )
    } == -1
    {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { CStr::from_ptr(buf.as_ptr()) }.to_owned())
}

/// The path of the slave side of `master`.
#[cfg(not(target_os = "macos"))]
fn slave_name(master: &OwnedFd) -> io::Result<CString> {
    let mut buf = [0 as libc::c_char; 128];
    let rc = unsafe { libc::ptsname_r(master.as_raw_fd(), buf.as_mut_ptr(), buf.len()) };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc));
    }
    Ok(unsafe { CStr::from_ptr(buf.as_ptr()) }.to_owned())
}

/// A new PTY pair `(master, slave)`, both opened close-on-exec. The flag
/// must be there from the first moment: fd flags are copied at fork, so a
/// child that another thread forks or spawns before a later `fcntl` keeps
/// both ends past its exec, and could drive this terminal. `openpty`
/// cannot do this, so the pair is opened by hand.
fn open_pty(cols: u16, rows: u16) -> io::Result<(OwnedFd, OwnedFd)> {
    let master = open_tty(c"/dev/ptmx")?;
    let raw = master.as_raw_fd();
    if unsafe { libc::grantpt(raw) } != 0 || unsafe { libc::unlockpt(raw) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let slave = open_tty(&slave_name(&master)?)?;
    let ws = winsize(cols, rows);
    if unsafe { libc::ioctl(raw, libc::TIOCSWINSZ as _, &ws as *const libc::winsize) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok((master, slave))
}

/// `kill(-pgid, sig)`.
fn signal_group(pgid: libc::pid_t, sig: libc::c_int) -> io::Result<()> {
    if pgid <= 1 {
        return Err(io::Error::from_raw_os_error(libc::ESRCH));
    }
    if unsafe { libc::kill(-pgid, sig) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Whether any process (zombies included) is still in group `pgid`.
fn group_alive(pgid: libc::pid_t) -> bool {
    match signal_group(pgid, 0) {
        Ok(()) => true,
        Err(e) => e.raw_os_error() == Some(libc::EPERM),
    }
}

/// Reads the master until EOF. On Linux a master whose slave side is fully
/// closed reads EIO, which also counts as EOF.
///
/// Query replies go to the writer thread after the lock is released, one
/// per distinct reply in a chunk. A full queue drops them: the reader never
/// waits on the child's input.
fn read_loop(mut master: File, replies: SyncSender<Job>, shared: Arc<Shared>) {
    let mut chunk = [0u8; 65536];
    loop {
        let n = match master.read(&mut chunk) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => 0,
        };
        let mut buf = shared.buf.lock().unwrap_or_else(|e| e.into_inner());
        if n == 0 {
            buf.eof = true;
            shared.ready.notify_all();
            return;
        }
        let data = &chunk[..n];
        buf.pending.extend_from_slice(data);
        if buf.pending.len() > PENDING_MAX {
            let over = buf.pending.len() - PENDING_MAX;
            buf.pending.drain(..over);
            buf.dropped += over as u64;
        }
        buf.last_output = Instant::now();
        buf.screen.feed_bytes(data);
        let cursor = (buf.screen.y, buf.screen.x);
        let mut answers: Vec<String> = Vec::new();
        for q in std::mem::take(&mut buf.screen.queries) {
            if let Some(a) = vt::query_reply(&q, cursor) {
                if !answers.contains(&a) {
                    answers.push(a);
                }
            }
        }
        shared.ready.notify_all();
        drop(buf);
        for a in answers {
            if let Err(TrySendError::Full(_)) = replies.try_send(Job::Reply(a.into_bytes())) {
                break;
            }
        }
    }
}

/// Runs the queued writes in order until every sender is gone. A write to
/// a master whose slave side is fully closed fails at once, so the thread
/// does not outlive the child for long.
fn write_loop(mut master: File, jobs: Receiver<Job>) {
    for job in jobs {
        match job {
            Job::Reply(bytes) => {
                let _ = master.write_all(&bytes);
            }
            Job::Input(bytes, done) => {
                let _ = done.send(master.write_all(&bytes).and_then(|_| master.flush()));
            }
        }
    }
}

impl NativePty {
    /// Reap the PTY child if it has exited.
    fn reap(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.exit.is_none() {
            self.exit = self.child.try_wait()?;
        }
        Ok(self.exit)
    }

    /// Wait up to `limit` for the child to be reaped, its group empty and
    /// every descendant in `seen` gone. A descendant first seen meanwhile
    /// gets `sig` too.
    fn wait_gone(&mut self, limit: Duration, seen: &mut Descendants, sig: libc::c_int) -> bool {
        let deadline = Instant::now() + limit;
        loop {
            let _ = self.reap();
            Descendants::signal(&seen.sweep(self.id), self.id.pid, sig);
            if self.exit.is_some() && !group_alive(self.id.pid) && !seen.any_runs() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(POLL);
        }
    }
}

impl Driver for NativePty {
    fn spawn(spec: &SpawnSpec) -> io::Result<NativePty> {
        let Some(program) = spec.argv.first() else {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty argv"));
        };
        let (master, slave) = open_pty(spec.cols, spec.rows)?;
        let mut cmd = Command::new(program);
        cmd.args(&spec.argv[1..])
            .current_dir(&spec.cwd)
            .env_clear()
            .envs(spec.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave.try_clone()?));
        // Runs in the forked child before exec: only async-signal-safe calls.
        // A new session makes the child a group leader without a terminal;
        // TIOCSCTTY on its stdin (the slave) makes the PTY its terminal.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn()?;
        // The slave copies must all close here, or the master never sees EOF.
        drop(cmd);
        drop(slave);
        let pid = child.id() as libc::pid_t;
        let master = File::from(master);
        let (writer, jobs) = mpsc::sync_channel(WRITE_QUEUE);
        let shared = Arc::new(Shared {
            buf: Mutex::new(Buf {
                pending: Vec::new(),
                dropped: 0,
                screen: Screen::new(spec.cols as usize, spec.rows as usize),
                last_output: Instant::now(),
                eof: false,
            }),
            ready: Condvar::new(),
        });
        let mut pty = NativePty {
            child,
            id: ChildId { pid, start: 0 },
            master,
            writer,
            shared,
            exit: None,
            killed: false,
        };
        let Some(info) = peer::proc_info(pid) else {
            let _ = pty.kill();
            return Err(io::Error::other(format!(
                "cannot read the start time of PTY child {}",
                pid
            )));
        };
        pty.id.start = info.start;
        let reader = pty.master.try_clone()?;
        let output = pty.master.try_clone()?;
        let replies = pty.writer.clone();
        let shared = pty.shared.clone();
        std::thread::Builder::new()
            .name(format!("pty-write-{}", pid))
            .spawn(move || write_loop(output, jobs))?;
        std::thread::Builder::new()
            .name(format!("pty-read-{}", pid))
            .spawn(move || read_loop(reader, replies, shared))?;
        Ok(pty)
    }

    fn child(&self) -> ChildId {
        self.id
    }

    /// Fails with `WouldBlock` when the write queue is full, and with
    /// `TimedOut` when the child has not taken the bytes within
    /// `WRITE_TIMEOUT`; they may still arrive later.
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let gone = || io::Error::new(io::ErrorKind::BrokenPipe, "the PTY writer has stopped");
        let (done, result) = mpsc::sync_channel(1);
        match self.writer.try_send(Job::Input(bytes.to_vec(), done)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "the PTY write queue is full",
                ))
            }
            Err(TrySendError::Disconnected(_)) => return Err(gone()),
        }
        match result.recv_timeout(WRITE_TIMEOUT) {
            Ok(r) => r,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the child did not read its input in time",
            )),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(gone()),
        }
    }

    fn output(&mut self, wait: Duration) -> Output {
        let deadline = Instant::now() + wait;
        let mut buf = self.shared.buf.lock().unwrap_or_else(|e| e.into_inner());
        while buf.pending.is_empty() && !buf.eof {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            buf = self
                .shared
                .ready
                .wait_timeout(buf, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        Output {
            bytes: std::mem::take(&mut buf.pending),
            dropped: std::mem::take(&mut buf.dropped),
            eof: buf.eof,
        }
    }

    fn screen(&self) -> ScreenSnapshot {
        let buf = self.shared.buf.lock().unwrap_or_else(|e| e.into_inner());
        let s = &buf.screen;
        ScreenSnapshot {
            text: s.text(),
            title: s.title.clone(),
            title_seq: s.title_seq,
            cursor: (s.y, s.x),
            alt_screen: s.in_alt,
            quiet: buf.last_output.elapsed(),
        }
    }

    fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        let ws = winsize(cols, rows);
        if unsafe {
            libc::ioctl(
                self.master.as_raw_fd(),
                libc::TIOCSWINSZ as _,
                &ws as *const libc::winsize,
            )
        } == -1
        {
            return Err(io::Error::last_os_error());
        }
        let mut buf = self.shared.buf.lock().unwrap_or_else(|e| e.into_inner());
        buf.screen.resize(cols as usize, rows as usize);
        Ok(())
    }

    fn signal(&mut self, sig: libc::c_int) -> io::Result<()> {
        let _ = self.reap();
        // An empty group's id may already belong to another group.
        if self.killed || (self.exit.is_some() && !group_alive(self.id.pid)) {
            self.killed = true;
            return Err(io::Error::from_raw_os_error(libc::ESRCH));
        }
        signal_group(self.id.pid, sig)
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.reap()
    }

    fn kill(&mut self) -> io::Result<KillReport> {
        let pgid = self.id.pid;
        let _ = self.reap();
        // Nothing left to signal: the group id may already be free.
        if self.killed || (self.exit.is_some() && !group_alive(pgid)) {
            self.killed = true;
            return Ok(KillReport {
                term_was_enough: true,
                exit: self.exit,
            });
        }
        // Walk the descendants before the first signal: once a parent
        // dies, its children move to init and the walk loses them.
        let mut seen = Descendants::default();
        seen.sweep(self.id);
        let _ = signal_group(pgid, libc::SIGTERM);
        Descendants::signal(&seen.0, pgid, libc::SIGTERM);
        let term_was_enough = self.wait_gone(KILL_GRACE, &mut seen, libc::SIGTERM);
        if !term_was_enough {
            let _ = signal_group(pgid, libc::SIGKILL);
            Descendants::signal(&seen.0, pgid, libc::SIGKILL);
            if !self.wait_gone(KILL_VERIFY, &mut seen, libc::SIGKILL) {
                return Err(io::Error::other(format!(
                    "process group {} or a descendant is still alive after SIGKILL",
                    pgid
                )));
            }
        }
        self.killed = true;
        Ok(KillReport {
            term_was_enough,
            exit: self.exit,
        })
    }
}

impl Drop for NativePty {
    fn drop(&mut self) {
        // The reader thread ends by itself at EOF, once the last slave copy
        // is gone with the group.
        let _ = self.kill();
    }
}

/// What `kill_orphan` found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orphan {
    /// No process runs with that pid (a zombie runs nothing).
    Gone,
    /// The pid runs, with another start time: it was reused. Not touched.
    PidReused,
    /// The PTY child and its group were killed.
    Killed,
}

/// Kill the PTY child `id` of an engine that is gone (boot reconcile,
/// C1a-26), with its group and descendants, as `NativePty::kill` does. The
/// engine holds no handle to it, so it is checked by pid **and** start time
/// first: a process that got the pid since is never signalled. Blocking:
/// up to `KILL_GRACE` + `KILL_VERIFY`.
pub fn kill_orphan(id: ChildId) -> io::Result<Orphan> {
    match peer::proc_entry(id.pid) {
        None => return Ok(Orphan::Gone),
        Some(e) if e.info.start != id.start => return Ok(Orphan::PidReused),
        Some(e) if e.zombie => return Ok(Orphan::Gone),
        Some(_) => {}
    }
    let leader = peer::ProcEntry {
        info: peer::ProcInfo {
            pid: id.pid,
            ppid: 0,
            start: id.start,
        },
        pgid: id.pid,
        zombie: false,
    };
    // Walk the descendants before the first signal, as `kill` does.
    let mut seen = Descendants::default();
    seen.sweep(id);
    // While the leader runs, its pid is also its group id (it called
    // `setsid`), so the group signal reaches the right processes.
    let signal = |seen: &Descendants, sig| {
        if runs(&leader) {
            let _ = signal_group(id.pid, sig);
        }
        Descendants::signal(&seen.0, id.pid, sig);
    };
    let gone = |seen: &mut Descendants, limit: Duration, sig| {
        let deadline = Instant::now() + limit;
        loop {
            Descendants::signal(&seen.sweep(id), id.pid, sig);
            if !runs(&leader) && !seen.any_runs() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(POLL);
        }
    };
    signal(&seen, libc::SIGTERM);
    if !gone(&mut seen, KILL_GRACE, libc::SIGTERM) {
        signal(&seen, libc::SIGKILL);
        if !gone(&mut seen, KILL_VERIFY, libc::SIGKILL) {
            return Err(io::Error::other(format!(
                "orphan PTY child {} or a descendant is still alive after SIGKILL",
                id.pid
            )));
        }
    }
    Ok(Orphan::Killed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::PathBuf;

    fn sh(script: &str) -> SpawnSpec {
        SpawnSpec::new(
            vec!["/bin/sh".into(), "-c".into(), script.into()],
            std::env::temp_dir(),
        )
    }

    /// Read until `want` shows up in the output, or panic after 10 s.
    fn read_until(pty: &mut NativePty, want: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = Vec::new();
        loop {
            let out = pty.output(Duration::from_millis(100));
            seen.extend_from_slice(&out.bytes);
            let text = String::from_utf8_lossy(&seen).into_owned();
            if text.contains(want) {
                return text;
            }
            assert!(!out.eof, "EOF before {:?}; got {:?}", want, text);
            assert!(Instant::now() < deadline, "no {:?}; got {:?}", want, text);
        }
    }

    /// The first line of output that parses as a pid.
    fn read_pid(pty: &mut NativePty, marker: &str) -> libc::pid_t {
        let text = read_until(pty, &format!("{}\r\n", marker));
        let line = text
            .lines()
            .find_map(|l| l.trim().strip_suffix(marker).map(str::to_string))
            .unwrap();
        line.trim().parse().unwrap()
    }

    fn alive(pid: libc::pid_t) -> bool {
        unsafe { libc::kill(pid, 0) == 0 }
    }

    /// Wait up to 2 s for `pid` to be gone (an orphan is reaped by init).
    fn gone_soon(pid: libc::pid_t) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        while alive(pid) {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(POLL);
        }
        true
    }

    #[test]
    fn spawn_records_pid_and_start_in_a_new_session() {
        let mut pty = NativePty::spawn(&sh("echo started; sleep 30")).unwrap();
        read_until(&mut pty, "started");
        let id = pty.child();
        let info = peer::proc_info(id.pid).unwrap();
        assert_eq!(info.start, id.start);
        assert_eq!(info.ppid, std::process::id() as libc::pid_t);
        assert_eq!(unsafe { libc::getpgid(id.pid) }, id.pid);
        assert_eq!(unsafe { libc::getsid(id.pid) }, id.pid);
        let report = pty.kill().unwrap();
        assert!(report.term_was_enough);
        assert!(!alive(id.pid));
    }

    #[test]
    fn the_pty_is_the_controlling_terminal() {
        let mut pty = NativePty::spawn(&sh("test -t 0 && tty && echo isatty > /dev/tty")).unwrap();
        let text = read_until(&mut pty, "isatty");
        assert!(text.contains("/dev/"), "{:?}", text);
    }

    #[test]
    fn env_is_only_the_spec_env() {
        let mut spec =
            sh("echo \"T=$TERM X=${C9_PTY_TEST-unset} M=${CARGO_MANIFEST_DIR-unset} end\"");
        spec.env("C9_PTY_TEST", "yes");
        let mut pty = NativePty::spawn(&spec).unwrap();
        let text = read_until(&mut pty, " end");
        assert!(
            text.contains("T=xterm-256color X=yes M=unset end"),
            "{:?}",
            text
        );
    }

    #[test]
    fn write_reaches_the_child() {
        let mut pty = NativePty::spawn(&sh("echo ready; read line; echo \"got:$line\"")).unwrap();
        read_until(&mut pty, "ready");
        pty.write(b"hello\n").unwrap();
        read_until(&mut pty, "got:hello");
        assert!(pty.screen().text.contains("got:hello"));
    }

    #[test]
    fn resize_changes_the_terminal_size() {
        let mut pty = NativePty::spawn(&sh("stty size; read x; stty size; echo done")).unwrap();
        read_until(&mut pty, "40 120");
        pty.resize(100, 50).unwrap();
        pty.write(b"\n").unwrap();
        read_until(&mut pty, "50 100");
        read_until(&mut pty, "done");
    }

    #[test]
    fn output_ends_with_eof_and_the_exit_status() {
        let mut pty = NativePty::spawn(&sh("sleep 0.2; echo bye; exit 3")).unwrap();
        read_until(&mut pty, "bye");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !pty.output(Duration::from_millis(100)).eof {
            assert!(Instant::now() < deadline, "no EOF");
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(s) = pty.try_wait().unwrap() {
                break s;
            }
            assert!(Instant::now() < deadline, "no exit");
            std::thread::sleep(POLL);
        };
        assert_eq!(status.code(), Some(3));
        // Kill after a clean exit sends nothing and reports the status.
        let report = pty.kill().unwrap();
        assert_eq!(report.exit.and_then(|s| s.code()), Some(3));
    }

    #[test]
    fn kill_takes_the_whole_group_with_sigterm() {
        let mut pty = NativePty::spawn(&sh("sleep 300 & echo \"$! bg\"; wait")).unwrap();
        let bg = read_pid(&mut pty, " bg");
        let leader = pty.child().pid;
        assert_eq!(unsafe { libc::getpgid(bg) }, leader);
        let report = pty.kill().unwrap();
        assert!(report.term_was_enough);
        assert!(!alive(leader));
        assert!(gone_soon(bg), "background child {} survived", bg);
    }

    #[test]
    fn kill_escalates_to_sigkill_when_sigterm_is_ignored() {
        let mut pty =
            NativePty::spawn(&sh("trap '' TERM; sleep 300 & echo \"$! bg\"; wait")).unwrap();
        let bg = read_pid(&mut pty, " bg");
        let started = Instant::now();
        let report = pty.kill().unwrap();
        assert!(!report.term_was_enough);
        assert!(started.elapsed() >= KILL_GRACE);
        assert!(gone_soon(bg), "background child {} survived", bg);
        // Signals after kill are refused, not sent to a free group id.
        assert!(pty.signal(libc::SIGTERM).is_err());
    }

    #[test]
    fn kill_after_the_leader_exits_leaves_no_orphan() {
        // The session leader's exit hangs up the terminal; the background
        // child ignores SIGHUP, as a detached tool would.
        let mut pty =
            NativePty::spawn(&sh("trap '' HUP; sleep 300 & echo \"$! bg\"; exit 0")).unwrap();
        let bg = read_pid(&mut pty, " bg");
        let deadline = Instant::now() + Duration::from_secs(10);
        while pty.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "leader did not exit");
            std::thread::sleep(POLL);
        }
        // The leader is gone but its background child keeps the group.
        assert!(alive(bg));
        let report = pty.kill().unwrap();
        assert!(report.term_was_enough);
        assert!(gone_soon(bg), "orphan {} survived", bg);
    }

    /// A shell command that runs `sleep 300` in a new session, as CC runs
    /// its Bash tool and hooks, and prints its pid with `marker`.
    fn detached_sleep(marker: &str) -> String {
        format!(
            "perl -e 'use POSIX; POSIX::setsid(); exec @ARGV' sleep 300 & echo \"$! {}\"",
            marker
        )
    }

    /// Wait up to 10 s for `pid` to lead its own session.
    fn wait_detached(pid: libc::pid_t) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while unsafe { libc::getpgid(pid) } != pid {
            assert!(Instant::now() < deadline, "{} never called setsid", pid);
            std::thread::sleep(POLL);
        }
    }

    #[test]
    fn kill_takes_detached_descendants_too() {
        let script = format!("{}; {}; wait", detached_sleep("d1"), detached_sleep("d2"));
        let mut pty = NativePty::spawn(&sh(&script)).unwrap();
        let d1 = read_pid(&mut pty, " d1");
        let d2 = read_pid(&mut pty, " d2");
        wait_detached(d1);
        wait_detached(d2);
        assert_ne!(unsafe { libc::getpgid(d1) }, pty.child().pid);
        let report = pty.kill().unwrap();
        assert!(report.term_was_enough);
        assert!(gone_soon(d1), "detached {} survived", d1);
        assert!(gone_soon(d2), "detached {} survived", d2);
    }

    #[test]
    fn kill_escalates_for_a_detached_descendant_that_ignores_sigterm() {
        let mut pty = NativePty::spawn(&sh(
            "perl -e 'use POSIX; POSIX::setsid(); $SIG{TERM} = q(IGNORE); \
             print \"$$ d\\n\"; sleep 300' & wait",
        ))
        .unwrap();
        let d = read_pid(&mut pty, " d");
        wait_detached(d);
        let report = pty.kill().unwrap();
        assert!(!report.term_was_enough);
        assert!(gone_soon(d), "detached {} survived", d);
    }

    #[test]
    fn signal_after_the_group_is_gone_is_refused() {
        let mut pty = NativePty::spawn(&sh("exit 0")).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while pty.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "leader did not exit");
            std::thread::sleep(POLL);
        }
        let err = pty.signal(libc::SIGINT).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::ESRCH));
    }

    #[test]
    fn signal_reaches_the_group() {
        let mut pty = NativePty::spawn(&sh(
            "trap 'echo got-usr1' USR1; echo ready; while :; do sleep 0.05; done",
        ))
        .unwrap();
        read_until(&mut pty, "ready");
        pty.signal(libc::SIGUSR1).unwrap();
        read_until(&mut pty, "got-usr1");
        pty.kill().unwrap();
    }

    #[test]
    fn drop_kills_the_group() {
        let mut pty = NativePty::spawn(&sh("sleep 300 & echo \"$! bg\"; wait")).unwrap();
        let bg = read_pid(&mut pty, " bg");
        let leader = pty.child().pid;
        drop(pty);
        assert!(gone_soon(leader));
        assert!(gone_soon(bg), "background child {} survived", bg);
    }

    #[test]
    fn spawn_failures_are_errors() {
        let missing = SpawnSpec::new(
            vec![OsString::from("/nonexistent/c9-no-such-binary")],
            std::env::temp_dir(),
        );
        assert!(NativePty::spawn(&missing).is_err());
        let mut bad_cwd = sh("true");
        bad_cwd.cwd = PathBuf::from("/nonexistent/c9-no-such-dir");
        assert!(NativePty::spawn(&bad_cwd).is_err());
        let empty = SpawnSpec::new(vec![], std::env::temp_dir());
        assert!(NativePty::spawn(&empty).is_err());
    }

    #[test]
    fn spec_env_replaces_a_key() {
        let mut spec = sh("true");
        spec.env("TERM", "dumb");
        let terms: Vec<_> = spec.env.iter().filter(|(k, _)| k == "TERM").collect();
        assert_eq!(terms.len(), 1);
        assert_eq!(terms[0].1, "dumb");
    }

    /// Wait up to 10 s for `f` to hold on the screen.
    fn screen_until(pty: &mut NativePty, f: impl Fn(&ScreenSnapshot) -> bool) -> ScreenSnapshot {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            pty.output(Duration::from_millis(50));
            let snap = pty.screen();
            if f(&snap) {
                return snap;
            }
            assert!(
                Instant::now() < deadline,
                "screen never matched: {:?}",
                snap
            );
        }
    }

    #[test]
    fn screen_is_the_rendered_terminal_with_its_title() {
        let mut pty = NativePty::spawn(&sh(
            "printf 'gone\\r\\033[Kkept\\033]0;\\342\\234\\263 idle\\007'; sleep 30",
        ))
        .unwrap();
        let snap = screen_until(&mut pty, |s| s.title_seq == 1);
        assert_eq!(snap.title, "\u{2733} idle");
        assert_eq!(snap.text.lines().next(), Some("kept"));
        assert_eq!(snap.cursor, (0, 4));
        // No output since: the quiet time grows.
        let snap = screen_until(&mut pty, |s| s.quiet >= Duration::from_millis(300));
        assert!(!snap.text.contains("gone"));
    }

    #[test]
    fn the_pty_pair_is_close_on_exec_from_the_start() {
        let (master, slave) = open_pty(80, 24).unwrap();
        for fd in [&master, &slave] {
            let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
            assert_ne!(flags & libc::FD_CLOEXEC, 0);
        }
        let mut ws = winsize(0, 0);
        assert_ne!(
            unsafe { libc::ioctl(slave.as_raw_fd(), libc::TIOCGWINSZ as _, &mut ws) },
            -1
        );
        assert_eq!((ws.ws_col, ws.ws_row), (80, 24));
    }

    #[test]
    fn a_child_that_never_reads_stalls_neither_output_nor_write() {
        // A flood of queries while stdin goes unread: the replies fill the
        // terminal's input queue, and output must still drain.
        let mut pty = NativePty::spawn(&sh("stty -icanon -echo; i=0; while [ $i -lt 3000 ]; do \
             printf '\\033[5n\\033[c\\033[6n'; i=$((i+1)); echo \"tick $i\"; done; sleep 30"))
        .unwrap();
        read_until(&mut pty, "tick 3000\r\n");
        let started = Instant::now();
        let err = pty.write(&[b'x'; 1 << 16]).unwrap_err();
        assert!(
            matches!(
                err.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ),
            "{:?}",
            err
        );
        assert!(started.elapsed() < WRITE_TIMEOUT + Duration::from_secs(2));
        pty.kill().unwrap();
    }

    #[test]
    fn terminal_queries_are_answered() {
        // Device status report: the reply is ESC [ 0 n, read raw.
        let mut pty = NativePty::spawn(&sh("stty -icanon -echo; printf '\\033[5n'; \
             dd bs=1 count=4 2>/dev/null | od -An -c; stty sane; echo end"))
        .unwrap();
        let text = read_until(&mut pty, "end");
        assert!(text.contains("[   0   n"), "{:?}", text);
    }

    /// Whether `pid` runs something (a zombie does not).
    fn runs_pid(pid: libc::pid_t) -> bool {
        peer::proc_entry(pid).is_some_and(|e| !e.zombie)
    }

    #[test]
    fn an_orphan_is_killed_with_its_descendants() {
        // The engine that spawned it is "gone": nothing calls `kill` on the
        // handle, only `kill_orphan` with the recorded pid and start time.
        let mut pty =
            NativePty::spawn(&sh("sleep 30 & echo \"$!:child\"; trap '' TERM; wait")).unwrap();
        let child = read_pid(&mut pty, ":child");
        let id = pty.child();
        assert_eq!(kill_orphan(id).unwrap(), Orphan::Killed);
        assert!(!runs_pid(id.pid));
        assert!(gone_soon(child));
        // Gone now: a second call finds nothing to do.
        assert_eq!(kill_orphan(id).unwrap(), Orphan::Gone);
    }

    #[test]
    fn a_reused_pid_is_never_killed() {
        let mut pty = NativePty::spawn(&sh("echo up; sleep 30")).unwrap();
        read_until(&mut pty, "up");
        let id = pty.child();
        // The journal holds the same pid with another start time.
        let stale = ChildId {
            pid: id.pid,
            start: id.start + 1,
        };
        assert_eq!(kill_orphan(stale).unwrap(), Orphan::PidReused);
        std::thread::sleep(Duration::from_millis(200));
        assert!(runs_pid(id.pid));
        pty.kill().unwrap();
    }
}
