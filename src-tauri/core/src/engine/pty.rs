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
//! - A group is signalled only with proof, read at that moment, that it is
//!   still ours (D2): the PTY child is not reaped yet (a session leader
//!   cannot leave its group, and its unreaped pid keeps the group id from
//!   reuse), or a process seen by pid and start time is in the group now
//!   (its pgid read now, not remembered). Without proof only the processes
//!   seen are signalled, one by one. A round keeps the identities that
//!   prove each target and proves it again just before its signal
//!   (`send`, which also names the gap that is left). So `try_wait` only looks
//!   (`waitid` with `WNOWAIT`), and `kill` reaps the child last, once the
//!   group and every process seen are gone.

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

/// The processes of a PTY child that `kill` has seen, by pid and start
/// time: its descendants, and the members of its group while the group is
/// proven ours. A process stays a root for later walks after its own
/// parent exits, so its children are still found.
#[derive(Default)]
struct Descendants(Vec<peer::ProcEntry>);

impl Descendants {
    /// Walk `all` again from `roots` and every process seen so far, and
    /// with `group` take every process in that group too (only while the
    /// group is proven ours). Returns the newly seen ones.
    fn sweep(
        &mut self,
        all: &[peer::ProcEntry],
        roots: &[ChildId],
        group: Option<libc::pid_t>,
    ) -> Vec<peer::ProcEntry> {
        let mut known: Vec<(libc::pid_t, u64)> = roots.iter().map(|r| (r.pid, r.start)).collect();
        known.extend(self.0.iter().map(|e| (e.info.pid, e.info.start)));
        let mut new: Vec<peer::ProcEntry> = Vec::new();
        let in_group = all.iter().filter(|e| Some(e.pgid) == group && !e.zombie);
        for e in peer::descendants(all, &known)
            .into_iter()
            .chain(in_group.cloned())
        {
            let key = (e.info.pid, e.info.start);
            if !known.contains(&key) {
                known.push(key);
                new.push(e);
            }
        }
        self.0.extend(new.iter().cloned());
        new
    }

    /// The current state of every process seen that is still the same
    /// process (`now`).
    fn current(&self) -> Vec<peer::ProcEntry> {
        self.0.iter().filter_map(now).collect()
    }

    /// Whether any process seen still runs.
    fn any_runs(&self) -> bool {
        self.0.iter().any(runs)
    }

    /// The processes seen that still run, with their current group.
    fn alive(&self) -> Vec<Survivor> {
        self.0
            .iter()
            .filter_map(now)
            .filter(|e| !e.zombie)
            .map(|e| Survivor {
                pid: e.info.pid,
                start: e.info.start,
                pgid: e.pgid,
            })
            .collect()
    }
}

/// The current state of the process `e` names, while it is still that
/// process (same pid and start time). A zombie counts: until it is reaped
/// it holds its pid, and its group with it.
fn now(e: &peer::ProcEntry) -> Option<peer::ProcEntry> {
    peer::proc_entry(e.info.pid).filter(|p| p.info.start == e.info.start)
}

/// Whether the process `e` names (same pid and start time) still runs. A
/// zombie runs nothing.
fn runs(e: &peer::ProcEntry) -> bool {
    now(e).is_some_and(|p| !p.zombie)
}

/// Whether a process other than a zombie is in group `pgid` in `all`.
/// The list, not `kill(-pgid, 0)`: macOS answers EPERM for a group that
/// holds only a zombie, and the unreaped PTY child is one.
fn group_runs(all: &[peer::ProcEntry], pgid: libc::pid_t) -> bool {
    all.iter().any(|e| e.pgid == pgid && !e.zombie)
}

/// One target of a signal round. It keeps the identities that prove it,
/// so `send` proves it again just before the signal.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Hit {
    /// Group `pgid`. `held`: the unreaped native PTY child leads it, which
    /// needs no new proof (only `kill` reaps it). Otherwise a process of
    /// `by`, by pid and start time, must be in the group at the signal.
    Group {
        pgid: libc::pid_t,
        held: bool,
        by: Vec<ChildId>,
    },
    /// One process, by pid and start time.
    Pid(ChildId),
}

/// What `send` fires once a hit is proven.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shot {
    Group(libc::pid_t),
    Pid(ChildId),
}

fn child_id(e: &peer::ProcEntry) -> ChildId {
    ChildId {
        pid: e.info.pid,
        start: e.info.start,
    }
}

/// What one signal round sends (D2). `target` is the PTY child, whose
/// group is hit only with proof (see the module doc): `held`, or the child
/// itself in the group at the signal. `members` is the current state
/// (`now`) of the processes seen, so each pgid is the one read now: a
/// process that called `setsid` since it was seen proves its new group,
/// not the old one. Each group once, with every process seen in it as its
/// proof; never the engine's own group (`own`): a process seen in it gets
/// its own signal.
fn plan(
    target: Option<ChildId>,
    held: bool,
    members: &[peer::ProcEntry],
    own: libc::pid_t,
) -> Vec<Hit> {
    let mut hits = Vec::new();
    if let Some(t) = target.filter(|t| t.pid > 1 && t.pid != own) {
        hits.push(Hit::Group {
            pgid: t.pid,
            held,
            by: vec![t],
        });
    }
    for e in members {
        if e.pgid > 1 && e.pgid != own {
            let found = hits.iter_mut().find_map(|h| match h {
                Hit::Group { pgid, by, .. } if *pgid == e.pgid => Some(by),
                _ => None,
            });
            match found {
                Some(by) if !by.contains(&child_id(e)) => by.push(child_id(e)),
                Some(_) => {}
                None => hits.push(Hit::Group {
                    pgid: e.pgid,
                    held: false,
                    by: vec![child_id(e)],
                }),
            }
        } else if !e.zombie && !hits.contains(&Hit::Pid(child_id(e))) {
            hits.push(Hit::Pid(child_id(e)));
        }
    }
    hits
}

/// Send `sig` to every hit of `plan`, each proven again just before its
/// signal (P1-A): a process by pid and start time, not a zombie; a group
/// `held`, or with a process of `by` (same pid and start, a zombie
/// counts) in it now. A group without proof gets none: each process of
/// `by` that still proves itself gets its own signal instead.
///
/// What is left is the gap between the proof and the signal. On Linux a
/// single process is signalled through a pidfd opened before its proof,
/// so its pid cannot be reused in that gap. A group has no pidfd, and
/// macOS has none at all: there the gap is the few microseconds between
/// one `proc_pidinfo` and one `kill`, in which a group's last process
/// would have to exit, be reaped by init and its id be taken by a new
/// group.
fn send(hits: &[Hit], sig: libc::c_int) {
    dispatch(hits, &peer::proc_entry, &mut |shot| {
        let _ = match shot {
            Shot::Group(g) => signal_group(g, sig),
            Shot::Pid(id) => signal_exact(id, sig),
        };
    });
}

/// The proving half of `send`: `look` reads a process now, `fire` signals.
fn dispatch(
    hits: &[Hit],
    look: &dyn Fn(libc::pid_t) -> Option<peer::ProcEntry>,
    fire: &mut dyn FnMut(Shot),
) {
    let is = |id: &ChildId| look(id.pid).filter(|e| e.info.start == id.start);
    let runs = |id: &ChildId| id.pid > 1 && is(id).is_some_and(|e| !e.zombie);
    for hit in hits {
        match hit {
            Hit::Group { pgid, held, by } => {
                if *held || by.iter().any(|id| is(id).is_some_and(|e| e.pgid == *pgid)) {
                    fire(Shot::Group(*pgid));
                } else {
                    for id in by.iter().filter(|id| runs(id)) {
                        fire(Shot::Pid(*id));
                    }
                }
            }
            Hit::Pid(id) => {
                if runs(id) {
                    fire(Shot::Pid(*id));
                }
            }
        }
    }
}

/// Signal the process `id`, only while it is still that process.
#[cfg(target_os = "linux")]
fn signal_exact(id: ChildId, sig: libc::c_int) -> io::Result<()> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, id.pid, 0) };
    if fd < 0 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::ENOSYS) {
            return signal_pid(id, sig);
        }
        return Err(e);
    }
    // The pidfd names one process for good: proven once it is open, the
    // signal cannot reach another one that took the pid.
    let fd = unsafe { OwnedFd::from_raw_fd(fd as libc::c_int) };
    if !peer::proc_entry(id.pid).is_some_and(|e| e.info.start == id.start) {
        return Err(io::Error::from_raw_os_error(libc::ESRCH));
    }
    let null = std::ptr::null::<libc::siginfo_t>();
    if unsafe { libc::syscall(libc::SYS_pidfd_send_signal, fd.as_raw_fd(), sig, null, 0) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn signal_exact(id: ChildId, sig: libc::c_int) -> io::Result<()> {
    signal_pid(id, sig)
}

/// `kill(pid, sig)`; the caller proved `id` just before.
fn signal_pid(id: ChildId, sig: libc::c_int) -> io::Result<()> {
    if id.pid <= 1 {
        return Err(io::Error::from_raw_os_error(libc::ESRCH));
    }
    if unsafe { libc::kill(id.pid, sig) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The PTY child as a survivor: it leads its own group.
fn leader_survivor(id: ChildId) -> Survivor {
    Survivor {
        pid: id.pid,
        start: id.start,
        pgid: id.pid,
    }
}

/// The engine's own process group.
fn own_group() -> libc::pid_t {
    unsafe { libc::getpgrp() }
}

/// A process an unconfirmed kill left: pid and start time, and its group
/// when it was seen (a later kill reads the group again).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Survivor {
    pub pid: libc::pid_t,
    pub start: u64,
    pub pgid: libc::pid_t,
}

impl Survivor {
    pub fn id(&self) -> ChildId {
        ChildId {
            pid: self.pid,
            start: self.start,
        }
    }
}

/// A kill that could not confirm that every process ended (D2). `alive`
/// holds what still ran at the end, so a later kill (`kill_orphans`) can
/// find them without the PTY child.
#[derive(Debug)]
pub struct Unconfirmed {
    pub alive: Vec<Survivor>,
    what: String,
}

impl Unconfirmed {
    pub fn new(alive: Vec<Survivor>, what: impl Into<String>) -> Unconfirmed {
        Unconfirmed {
            alive,
            what: what.into(),
        }
    }
}

impl std::fmt::Display for Unconfirmed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} is still alive after SIGKILL", self.what)?;
        if !self.alive.is_empty() {
            let pids: Vec<String> = self.alive.iter().map(|c| c.pid.to_string()).collect();
            write!(f, " (pids {})", pids.join(", "))?;
        }
        Ok(())
    }
}

impl std::error::Error for Unconfirmed {}

/// The processes an unconfirmed kill left (`Unconfirmed::alive`), or none
/// when `e` is another error.
pub fn survivors(e: &io::Error) -> Vec<Survivor> {
    e.get_ref()
        .and_then(|e| e.downcast_ref::<Unconfirmed>())
        .map(|u| u.alive.clone())
        .unwrap_or_default()
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
    /// The PTY child's exit status once it has exited. It is not reaped
    /// until `kill` has found its group and every process seen gone.
    exit: Option<ExitStatus>,
    /// The PTY child is reaped: its pid, and with it the group id, may
    /// belong to another process now.
    reaped: bool,
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
    /// The PTY child's exit status if it has exited, without reaping it
    /// (`waitid` with `WNOWAIT`): its unreaped pid keeps its group id ours.
    fn peek(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.exit.is_none() && !self.reaped {
            self.exit = peek_exit(self.id.pid)?;
        }
        Ok(self.exit)
    }

    /// Whether the group is proven ours now: the PTY child is not reaped.
    fn pinned(&self) -> bool {
        !self.reaped
    }

    /// Walk `all` from the PTY child, and take its group's members while it
    /// is pinned. Returns the newly seen ones.
    fn sweep(&self, all: &[peer::ProcEntry], seen: &mut Descendants) -> Vec<peer::ProcEntry> {
        let group = self.pinned().then_some(self.id.pid);
        seen.sweep(all, &[self.id], group)
    }

    /// The exited child, its group and every process seen are gone.
    fn gone(&self, all: &[peer::ProcEntry], seen: &Descendants) -> bool {
        self.exit.is_some() && !group_runs(all, self.id.pid) && !seen.any_runs()
    }

    /// One signal round to the group (while pinned) and to every process
    /// seen, by its current group.
    fn signal_all(&self, members: &[peer::ProcEntry], sig: libc::c_int) {
        send(
            &plan(Some(self.id), self.pinned(), members, own_group()),
            sig,
        );
    }

    /// Wait up to `limit` for the child to exit, its group to empty and
    /// every process in `seen` to be gone. A process first seen meanwhile
    /// gets `sig` too.
    fn wait_gone(&mut self, limit: Duration, seen: &mut Descendants, sig: libc::c_int) -> bool {
        let deadline = Instant::now() + limit;
        loop {
            let _ = self.peek();
            let all = peer::processes();
            let new: Vec<_> = self.sweep(&all, seen).iter().filter_map(now).collect();
            send(&plan(None, false, &new, own_group()), sig);
            if self.gone(&all, seen) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(POLL);
        }
    }

    /// Reap the PTY child: everything is gone.
    fn finish(&mut self) {
        if !self.reaped {
            if let Ok(Some(status)) = self.child.try_wait() {
                self.exit = Some(status);
                self.reaped = true;
            }
        }
        self.killed = true;
    }
}

/// The exit status of child `pid` if it has exited, without reaping it.
pub(crate) fn peek_exit(pid: libc::pid_t) -> io::Result<Option<ExitStatus>> {
    use std::os::unix::process::ExitStatusExt;
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    loop {
        let flags = libc::WEXITED | libc::WNOHANG | libc::WNOWAIT;
        if unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, flags) } == 0 {
            break;
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
    // Nothing has exited yet: `waitid` left `info` zeroed.
    if info.si_signo != libc::SIGCHLD {
        return Ok(None);
    }
    let status = unsafe { info.si_status() };
    let raw = match info.si_code {
        libc::CLD_EXITED => (status & 0xff) << 8,
        libc::CLD_KILLED => status & 0x7f,
        libc::CLD_DUMPED => (status & 0x7f) | 0x80,
        _ => return Ok(None),
    };
    Ok(Some(ExitStatus::from_raw(raw)))
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
            reaped: false,
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
        let _ = self.peek();
        // Nothing of the group runs: nothing to signal. A reaped child's
        // group id may already belong to another group.
        if self.killed
            || !self.pinned()
            || (self.exit.is_some() && !group_runs(&peer::processes(), self.id.pid))
        {
            return Err(io::Error::from_raw_os_error(libc::ESRCH));
        }
        signal_group(self.id.pid, sig)
    }

    /// Does not reap the child (see `peek`); `kill` does, last.
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.peek()
    }

    fn kill(&mut self) -> io::Result<KillReport> {
        if self.killed {
            return Ok(KillReport {
                term_was_enough: true,
                exit: self.exit,
            });
        }
        let _ = self.peek();
        // Walk the descendants before the first signal: once a parent
        // dies, its children move to init and the walk loses them.
        let mut seen = Descendants::default();
        let all = peer::processes();
        self.sweep(&all, &mut seen);
        if self.gone(&all, &seen) {
            self.finish();
            return Ok(KillReport {
                term_was_enough: true,
                exit: self.exit,
            });
        }
        // The child stays unreaped until the end, so every round below,
        // the SIGKILL too, has the group pinned (P1-1): a child reaped
        // while a detached process kept the wait going would free the
        // group id before the escalation.
        self.signal_all(&seen.current(), libc::SIGTERM);
        let term_was_enough = self.wait_gone(KILL_GRACE, &mut seen, libc::SIGTERM);
        if !term_was_enough {
            self.signal_all(&seen.current(), libc::SIGKILL);
            if !self.wait_gone(KILL_VERIFY, &mut seen, libc::SIGKILL) {
                // Not reaped: the unreaped child keeps the group id ours
                // for as long as this engine runs.
                let mut alive = seen.alive();
                if self.exit.is_none() {
                    alive.insert(0, leader_survivor(self.id));
                }
                return Err(io::Error::other(Unconfirmed {
                    alive,
                    what: format!("process group {} or a descendant", self.id.pid),
                }));
            }
        }
        self.finish();
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
/// C1a-26), with its group and descendants, as `NativePty::kill` does.
/// `kill_orphans` with no other process.
pub fn kill_orphan(id: ChildId) -> io::Result<Orphan> {
    kill_orphans(id, &[])
}

/// Kill the PTY child `id` of an engine that is gone, its group, its
/// descendants and `others`: the processes an earlier unconfirmed kill
/// left (`Unconfirmed::alive`). The engine holds no handle to any of them,
/// so each is checked by pid **and** start time first: a process that got
/// the pid since is never signalled. The group `id.pid` is signalled only
/// with current proof that it is ours (see the module doc): `id` itself,
/// a zombie included, or a process seen in it now. `Gone` or `PidReused`
/// (for `id`) when none of them still ran. Blocking: up to `KILL_GRACE` +
/// `KILL_VERIFY`.
pub fn kill_orphans(id: ChildId, others: &[ChildId]) -> io::Result<Orphan> {
    let leader = peer::ProcEntry {
        info: peer::ProcInfo {
            pid: id.pid,
            ppid: 0,
            start: id.start,
        },
        pgid: id.pid,
        zombie: false,
    };
    // The leader proves its group while it is still that process and in
    // the group now; a zombie still holds its pid.
    let pinned = || now(&leader).is_some_and(|e| e.pgid == id.pid);
    let mut seen = Descendants::default();
    let sweep = |seen: &mut Descendants, all: &[peer::ProcEntry]| {
        let group = pinned().then_some(id.pid);
        let mut roots = vec![id];
        roots.extend_from_slice(others);
        let known: Vec<peer::ProcEntry> = others
            .iter()
            .filter_map(|o| peer::proc_entry(o.pid).filter(|e| e.info.start == o.start))
            .filter(|e| {
                !seen
                    .0
                    .iter()
                    .any(|s| (s.info.pid, s.info.start) == (e.info.pid, e.info.start))
            })
            .collect();
        seen.0.extend(known);
        seen.sweep(all, &roots, group)
    };
    let all = peer::processes();
    sweep(&mut seen, &all);
    let leader_runs = runs(&leader);
    if !leader_runs && !seen.any_runs() {
        return Ok(match peer::proc_entry(id.pid) {
            Some(e) if e.info.start != id.start => Orphan::PidReused,
            _ => Orphan::Gone,
        });
    }
    let gone = |seen: &mut Descendants, limit: Duration, sig| {
        let deadline = Instant::now() + limit;
        loop {
            let all = peer::processes();
            let new: Vec<_> = sweep(seen, &all).iter().filter_map(now).collect();
            send(&plan(None, false, &new, own_group()), sig);
            let group_left = pinned() && group_runs(&all, id.pid);
            if !runs(&leader) && !seen.any_runs() && !group_left {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(POLL);
        }
    };
    // The group is proven again at each signal (`send`): the leader, by
    // pid and start time, still in it.
    send(
        &plan(Some(id), false, &seen.current(), own_group()),
        libc::SIGTERM,
    );
    if !gone(&mut seen, KILL_GRACE, libc::SIGTERM) {
        send(
            &plan(Some(id), false, &seen.current(), own_group()),
            libc::SIGKILL,
        );
        if !gone(&mut seen, KILL_VERIFY, libc::SIGKILL) {
            let mut alive = seen.alive();
            if runs(&leader) {
                alive.insert(0, leader_survivor(id));
            }
            return Err(io::Error::other(Unconfirmed {
                alive,
                what: format!("orphan PTY child {} or a descendant", id.pid),
            }));
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
        read_pids(pty, &[marker])[0]
    }

    /// The pid printed before each marker. One read for all of them: lines
    /// that arrive in the same chunk are not lost between calls.
    fn read_pids(pty: &mut NativePty, markers: &[&str]) -> Vec<libc::pid_t> {
        let mut text = String::new();
        for marker in markers {
            let want = format!("{}\r\n", marker);
            if !text.contains(&want) {
                text.push_str(&read_until(pty, &want));
            }
        }
        markers
            .iter()
            .map(|marker| {
                let line = text
                    .lines()
                    .find_map(|l| l.trim().strip_suffix(marker).map(str::to_string))
                    .unwrap();
                line.trim().parse().unwrap()
            })
            .collect()
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
        // One read: "50 100" and "done" can arrive in one chunk.
        let text = read_until(&mut pty, "done");
        assert!(text.contains("50 100"), "{:?}", text);
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
        let pids = read_pids(&mut pty, &[" d1", " d2"]);
        let (d1, d2) = (pids[0], pids[1]);
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

    /// D2: the leader dies on SIGTERM, a member of its group ignores it
    /// (and the hangup the leader's exit sends).
    /// The group signal still goes out with SIGKILL, because that member,
    /// seen by pid and start time, still holds the group id.
    ///
    /// The member prints its own pid after its trap is set (`exec sh`
    /// keeps an ignored signal ignored). A pid the leader prints from `$!`
    /// comes right after the fork, before the trap: on a loaded machine
    /// SIGTERM then got there first and the kill ended early.
    #[test]
    fn an_orphan_group_member_that_ignores_sigterm_gets_sigkill() {
        let mut pty = NativePty::spawn(&sh(
            "(trap '' TERM HUP; exec sh -c 'echo \"$$ bg\"; exec sleep 300') & wait",
        ))
        .unwrap();
        let bg = read_pid(&mut pty, " bg");
        let id = pty.child();
        assert_eq!(unsafe { libc::getpgid(bg) }, id.pid);
        let started = Instant::now();
        assert_eq!(kill_orphan(id).unwrap(), Orphan::Killed);
        assert!(started.elapsed() >= KILL_GRACE);
        assert!(gone_soon(bg), "group member {} survived", bg);
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

    /// The process `pid` with start time `start`, while it is that one.
    fn same(pid: libc::pid_t, start: u64) -> Option<peer::ProcEntry> {
        peer::proc_entry(pid).filter(|e| e.info.start == start)
    }

    /// D2 (P1-1): `try_wait` reports the exit but leaves the child
    /// unreaped, so its pid still holds the group id; `kill` reaps it.
    #[test]
    fn try_wait_reports_the_exit_without_reaping() {
        for (script, code, sig) in [("exit 3", Some(3), None), ("kill -9 $$", None, Some(9))] {
            let mut pty = NativePty::spawn(&sh(script)).unwrap();
            let id = pty.child();
            let deadline = Instant::now() + Duration::from_secs(10);
            let status = loop {
                if let Some(s) = pty.try_wait().unwrap() {
                    break s;
                }
                assert!(Instant::now() < deadline, "no exit");
                std::thread::sleep(POLL);
            };
            use std::os::unix::process::ExitStatusExt;
            assert_eq!((status.code(), status.signal()), (code, sig), "{}", script);
            assert_eq!(pty.try_wait().unwrap(), Some(status));
            assert!(alive(id.pid), "{} was reaped by try_wait", script);
            let report = pty.kill().unwrap();
            assert_eq!(report.exit, Some(status));
            assert!(same(id.pid, id.start).is_none(), "{} not reaped", script);
        }
    }

    /// D2 (P1-1): the leader dies on SIGTERM while a member of its group
    /// ignores it and keeps the kill waiting. The leader stays unreaped for
    /// the whole wait, so the group id cannot be reused before the SIGKILL,
    /// and is reaped once the group is gone.
    #[test]
    fn kill_keeps_the_leader_unreaped_until_the_group_is_gone() {
        let mut pty = NativePty::spawn(&sh(
            "(trap '' TERM HUP; exec sh -c 'echo \"$$ bg\"; exec sleep 300') & wait",
        ))
        .unwrap();
        let bg = read_pid(&mut pty, " bg");
        let id = pty.child();
        let killer = std::thread::spawn(move || {
            let r = pty.kill();
            (r, pty)
        });
        std::thread::sleep(KILL_GRACE / 2);
        // Exited (macOS cannot read a zombie's info) but not reaped: its
        // pid still answers `kill(pid, 0)`.
        assert!(!runs_pid(id.pid), "the leader ignored SIGTERM");
        assert!(alive(id.pid), "the leader was reaped during the wait");
        assert!(runs_pid(bg));
        let (report, _pty) = killer.join().unwrap();
        assert!(!report.unwrap().term_was_enough);
        assert!(gone_soon(bg), "group member {} survived", bg);
        assert!(
            same(id.pid, id.start).is_none(),
            "the leader was never reaped"
        );
    }

    /// D2 (P1-2): a group member that calls `setsid` after the first sweep
    /// is signalled in its new group, read at signal time, not in the
    /// group it was seen in.
    #[test]
    fn kill_follows_a_member_that_leaves_the_group_after_the_sweep() {
        let mut pty = NativePty::spawn(&sh(
            "perl -e 'use POSIX; $SIG{TERM} = q(IGNORE); $SIG{HUP} = q(IGNORE); \
             print \"$$ m\\n\"; select(undef, undef, undef, 0.5); POSIX::setsid(); \
             sleep 300' & wait",
        ))
        .unwrap();
        let m = read_pid(&mut pty, " m");
        let id = pty.child();
        assert_eq!(unsafe { libc::getpgid(m) }, id.pid);
        let report = pty.kill().unwrap();
        assert!(!report.term_was_enough);
        assert_ne!(unsafe { libc::getpgid(m) }, id.pid);
        assert!(gone_soon(m), "member {} survived its setsid", m);
    }

    fn entry(pid: libc::pid_t, start: u64, pgid: libc::pid_t, zombie: bool) -> peer::ProcEntry {
        peer::ProcEntry {
            info: peer::ProcInfo {
                pid,
                ppid: 1,
                start,
            },
            pgid,
            zombie,
        }
    }

    fn cid(pid: libc::pid_t) -> ChildId {
        ChildId { pid, start: 1 }
    }

    /// What `dispatch` fires for `hits` when the processes are `now`.
    fn shots(hits: &[Hit], now: &[peer::ProcEntry]) -> Vec<Shot> {
        let now = now.to_vec();
        let look = move |pid| now.iter().find(|e| e.info.pid == pid).cloned();
        let mut fired = Vec::new();
        dispatch(hits, &look, &mut |s| fired.push(s));
        fired
    }

    /// D2 (P1-2): a signal round uses each member's current group. The
    /// group of the PTY child is hit only with proof: held, or the child
    /// or a member in it at the signal. The engine's own group never; its
    /// members one by one.
    #[test]
    fn plan_signals_only_groups_proven_now() {
        let e = |pid, pgid, zombie| entry(pid, 1, pgid, zombie);
        let (t, own) = (Some(cid(100)), 7);
        // Held: the group once, and a member's own group.
        let m = [e(101, 100, false), e(102, 102, false)];
        assert_eq!(
            shots(&plan(t, true, &m, own), &m),
            vec![Shot::Group(100), Shot::Group(102)]
        );
        // Not held, the child gone and the member seen in it has left:
        // not the group.
        let m = [e(101, 101, false)];
        assert_eq!(shots(&plan(t, false, &m, own), &m), vec![Shot::Group(101)]);
        // Not held, but a member is in it now: that proves it.
        let m = [e(101, 100, false)];
        assert_eq!(shots(&plan(t, false, &m, own), &m), vec![Shot::Group(100)]);
        // In the engine's group: the process alone; a zombie not at all.
        let m = [e(103, own, false), e(104, own, true)];
        assert_eq!(
            shots(&plan(t, false, &m, own), &m),
            vec![Shot::Pid(cid(103))]
        );
        assert!(plan(None, false, &[], own).is_empty());
    }

    /// D2 (P1-A): each hit is proven again at the signal, not at the plan.
    /// A process whose pid another one took since, or that left the group,
    /// is no proof: the group is not signalled, and only what still proves
    /// itself gets its own signal.
    #[test]
    fn a_target_that_changes_between_plan_and_send_is_not_signalled() {
        let own = 7;
        let seen = [
            entry(101, 1, 200, false),
            entry(102, 1, 200, false),
            entry(103, 1, 7, false),
        ];
        let hits = plan(Some(cid(100)), false, &seen, own);
        // As planned: both groups and the lone process.
        let leader = entry(100, 1, 100, false);
        let mut now = seen.to_vec();
        now.push(leader.clone());
        assert_eq!(
            shots(&hits, &now),
            vec![Shot::Group(100), Shot::Group(200), Shot::Pid(cid(103))]
        );
        // Meanwhile: the leader's pid runs a new process, 101's pid too,
        // 102 left group 200 with setsid, and 103's pid was reused.
        let now = [
            entry(100, 2, 100, false),
            entry(101, 2, 200, false),
            entry(102, 1, 102, false),
            entry(103, 2, 7, false),
        ];
        assert_eq!(shots(&hits, &now), vec![Shot::Pid(cid(102))]);
        // Nothing runs any more: nothing at all.
        assert!(shots(&hits, &[]).is_empty());
        // A held group needs no new proof.
        let hits = plan(Some(cid(100)), true, &[], own);
        assert_eq!(shots(&hits, &[]), vec![Shot::Group(100)]);
    }

    /// D2 (P1-3): a later kill finds what an unconfirmed one left by pid
    /// and start time, after the PTY child is gone, and never signals a
    /// pid that runs another process now.
    #[test]
    fn kill_orphans_takes_the_survivors_of_an_earlier_kill() {
        let mut pty = NativePty::spawn(&sh(
            "perl -e 'use POSIX; POSIX::setsid(); $SIG{TERM} = q(IGNORE); \
             print \"$$ d\\n\"; sleep 300' & read x; exit 0",
        ))
        .unwrap();
        let d = read_pid(&mut pty, " d");
        wait_detached(d);
        pty.write(b"\n").unwrap();
        let id = pty.child();
        let d_id = ChildId {
            pid: d,
            start: peer::proc_info(d).unwrap().start,
        };
        // The PTY child is gone and reaped, as after an engine restart.
        let deadline = Instant::now() + Duration::from_secs(10);
        while pty.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "leader did not exit");
            std::thread::sleep(POLL);
        }
        pty.child.wait().unwrap();
        pty.reaped = true;
        pty.killed = true;
        let stale = ChildId {
            start: d_id.start + 1,
            ..d_id
        };
        assert_eq!(kill_orphans(id, &[stale]).unwrap(), Orphan::Gone);
        std::thread::sleep(Duration::from_millis(100));
        assert!(runs_pid(d), "a reused pid was hit");
        assert_eq!(kill_orphans(id, &[d_id]).unwrap(), Orphan::Killed);
        assert!(gone_soon(d), "survivor {} survived", d);
    }

    #[test]
    fn an_unconfirmed_kill_error_carries_its_survivors() {
        let alive = vec![Survivor {
            pid: 42,
            start: 9,
            pgid: 42,
        }];
        let e = io::Error::other(Unconfirmed::new(alive.clone(), "process group 42"));
        assert_eq!(survivors(&e), alive);
        assert!(e.to_string().contains("(pids 42)"), "{}", e);
        assert!(survivors(&io::Error::other("other")).is_empty());
    }
}
