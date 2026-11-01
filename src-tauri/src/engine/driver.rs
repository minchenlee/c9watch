//! The `Driver` trait: how the engine runs one agent process and talks to
//! its terminal.
//!
//! C1a has one implementation, the native PTY in `engine/pty.rs`. A driver
//! knows nothing about Claude Code: the spawn profile (argv, model,
//! settings, env) comes from the caller in a `SpawnSpec`. The spike's
//! hard-coded model, home path and scratch dir (`c0-engine.rs:45-46`,
//! `:380`) have no place here.
//!
//! The child runs in its own session and process group, so `signal` and
//! `kill` reach every process the agent starts (its tools, its shells),
//! not only the PTY child.

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::Duration;

/// Engine env keys an agent inherits. Everything else is cleared (spike
/// `c0-engine.rs:392-397`).
pub const INHERITED_ENV: &[&str] = &["HOME", "PATH", "USER", "LOGNAME", "SHELL", "LANG", "TMPDIR"];

/// How long `kill` waits after SIGTERM before it sends SIGKILL.
pub const KILL_GRACE: Duration = Duration::from_secs(3);

/// How long `kill` waits after SIGKILL for the group to be gone.
pub const KILL_VERIFY: Duration = Duration::from_secs(2);

/// What to run and how big its terminal is.
#[derive(Debug, Clone)]
pub struct SpawnSpec {
    /// `argv[0]` is the program. It is not looked up on `PATH` by the
    /// driver when the caller passes an absolute path.
    pub argv: Vec<OsString>,
    pub cwd: PathBuf,
    /// The child's whole environment. The engine's env is never passed
    /// through as a whole.
    pub env: Vec<(OsString, OsString)>,
    pub cols: u16,
    pub rows: u16,
}

impl SpawnSpec {
    /// A 120x40 terminal with `base_env()`.
    pub fn new(argv: Vec<OsString>, cwd: PathBuf) -> SpawnSpec {
        SpawnSpec {
            argv,
            cwd,
            env: base_env(),
            cols: 120,
            rows: 40,
        }
    }

    /// Set (or replace) one env var.
    pub fn env(&mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> &mut Self {
        let key = key.into();
        self.env.retain(|(k, _)| *k != key);
        self.env.push((key, value.into()));
        self
    }
}

/// `INHERITED_ENV` from the engine's env, plus the terminal type.
pub fn base_env() -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = INHERITED_ENV
        .iter()
        .filter_map(|k| std::env::var_os(k).map(|v| (OsString::from(k), v)))
        .collect();
    env.push(("TERM".into(), "xterm-256color".into()));
    env.push(("COLORTERM".into(), "truecolor".into()));
    env
}

/// The PTY child as the engine records it for ops and for agent identity
/// (C1a-33). Pids get reused, so the pid is only valid with its start time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildId {
    /// Also the process group id and the session id.
    pub pid: libc::pid_t,
    /// Start-time token from `engine::peer::proc_info`.
    pub start: u64,
}

/// Bytes read from the terminal since the last `output` call.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Output {
    pub bytes: Vec<u8>,
    /// Bytes dropped because nobody read them in time (oldest first).
    pub dropped: u64,
    /// The terminal is closed: no more output will come.
    pub eof: bool,
}

/// The rendered terminal at one moment (`engine::vt`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenSnapshot {
    /// The visible rows, trailing spaces trimmed.
    pub text: String,
    /// The last OSC 0/2 window title.
    pub title: String,
    /// Bumped on every title change, also to the same title.
    pub title_seq: u64,
    /// (row, col), zero-based.
    pub cursor: (usize, usize),
    pub alt_screen: bool,
    /// Time since the last output byte, or since the spawn when none came.
    pub quiet: Duration,
}

/// How a `kill` ended. `kill` returns an error, not a report, when the
/// group is still alive after SIGKILL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KillReport {
    /// The group was gone before the SIGKILL deadline, so no SIGKILL was sent.
    pub term_was_enough: bool,
    /// The PTY child's exit status.
    pub exit: Option<ExitStatus>,
}

/// One agent process behind a terminal.
pub trait Driver: Send {
    /// Start the child in a new session on a new terminal.
    fn spawn(spec: &SpawnSpec) -> io::Result<Self>
    where
        Self: Sized;

    /// Pid and start time of the PTY child.
    fn child(&self) -> ChildId;

    /// Send bytes to the terminal, as typed input.
    fn write(&mut self, bytes: &[u8]) -> io::Result<()>;

    /// Bytes the child wrote since the last call. Waits up to `wait` when
    /// none are there yet; returns at once on EOF.
    fn output(&mut self, wait: Duration) -> Output;

    /// The terminal as the VT model renders it, with its title.
    fn screen(&self) -> ScreenSnapshot;

    /// Change the terminal size. The child gets SIGWINCH.
    fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()>;

    /// Send `sig` to the child's whole process group. Fails once the child
    /// has exited and its group is empty.
    fn signal(&mut self, sig: libc::c_int) -> io::Result<()>;

    /// The PTY child's exit status once it has exited. A native PTY does
    /// not reap it here: `kill` does, last, so the group id stays ours
    /// until the group is gone (D2).
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>>;

    /// SIGTERM to the group and to the groups of the child's descendants
    /// that left it, SIGKILL after `KILL_GRACE`, then check that no process
    /// of the group and no descendant seen is left.
    fn kill(&mut self) -> io::Result<KillReport>;
}
