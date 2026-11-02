//! Peer identity for the engine socket: who is on the other end of a
//! connection, which processes it descends from, and which executable it
//! runs.
//!
//! - Peer pid and uid: `LOCAL_PEERPID` and `getpeereid` on macOS,
//!   `SO_PEERCRED` on Linux.
//! - Process info (ppid, group and start time; the process list):
//!   `proc_listallpids` and `proc_pidinfo(PROC_PIDTBSDINFO)` on
//!   macOS (`proc_info` falls back to `sysctl(KERN_PROC_PID)` for
//!   another user's process), `/proc/<pid>/stat` on Linux. Never `ps`.
//! - Executable: `proc_pidpath` on macOS, `/proc/<pid>/exe` on Linux.
//!
//! The macOS part is ported from the C0 spike `peer.rs`, on top of the libc
//! crate instead of hand-written FFI. Pids get reused, so every identity
//! check compares the pid **and** the start time.
//!
//! Peer uid (`peer_cred`, `check_peer_uid`) lives in `engine_proto::peer`,
//! outside the `engine` feature, and is re-exported here.

pub use crate::engine_proto::peer::{
    check_peer_uid, engine_uid, peer_cred, PeerCred, PEER_UID_MISMATCH,
};
use std::path::PathBuf;

/// How many levels `ancestry` walks up at most (D:252).
pub const MAX_ANCESTRY: usize = 10;

/// One process in an ancestry chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcInfo {
    pub pid: libc::pid_t,
    pub ppid: libc::pid_t,
    /// Start time as an opaque token: microseconds since the epoch on macOS,
    /// clock ticks since boot on Linux. Compare it only with another value
    /// from this module on the same machine.
    pub start: u64,
}

/// One process as `processes` lists it: its info, its process group, and
/// whether it is a zombie (exited, not yet reaped; it runs nothing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcEntry {
    pub info: ProcInfo,
    pub pgid: libc::pid_t,
    pub zombie: bool,
}

/// Ppid, group, start time and state of `pid`, or `None` when it is gone
/// or unreadable.
#[cfg(target_os = "macos")]
pub fn proc_entry(pid: libc::pid_t) -> Option<ProcEntry> {
    if pid <= 0 {
        return None;
    }
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if n != size {
        return None;
    }
    Some(ProcEntry {
        info: ProcInfo {
            pid: info.pbi_pid as libc::pid_t,
            ppid: info.pbi_ppid as libc::pid_t,
            start: info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec,
        },
        pgid: info.pbi_pgid as libc::pid_t,
        zombie: info.pbi_status == libc::SZOMB,
    })
}

/// `proc_entry` through `sysctl(KERN_PROC_PID)`, which reads processes of
/// any user. The start token is the same as from `proc_pidinfo`.
///
/// The libc crate has no macOS `kinfo_proc`, so the fields are read at
/// their offsets in the 64-bit layout (648 bytes). Source: xnu
/// `bsd/sys/sysctl.h` (`struct kinfo_proc { struct extern_proc kp_proc;
/// struct eproc kp_eproc; }`) and `bsd/sys/proc.h` (`struct extern_proc`,
/// 296 bytes):
/// - `kp_proc.p_un.__p_starttime` (`struct timeval`): tv_sec at 0, tv_usec at 8;
/// - `kp_proc.p_stat` (char) at 36, `kp_proc.p_pid` at 40;
/// - `kp_eproc` at 296, after `e_paddr`, `e_sess`, `e_pcred` (104),
///   `e_ucred` (76) and `e_vm` (64): `e_ppid` at 560, `e_pgid` at 564.
///
/// Checked against `ps` and `proc_pidinfo` (test
/// `sysctl_reads_a_process_as_proc_pidinfo_does`). Fail-closed: a size
/// other than 648, a `p_pid` other than `pid`, or a start time out of
/// range reads as `None`, so wrong offsets refuse instead of allowing.
#[cfg(target_os = "macos")]
fn kinfo_entry(pid: libc::pid_t) -> Option<ProcEntry> {
    if pid <= 0 {
        return None;
    }
    const SIZE: usize = 648;
    let mut buf = [0u8; SIZE];
    let mut len = SIZE;
    let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PID, pid];
    let r = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buf.as_mut_ptr() as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if r != 0 || len != SIZE {
        return None;
    }
    let i32_at = |o: usize| i32::from_ne_bytes(buf[o..o + 4].try_into().unwrap());
    let i64_at = |o: usize| i64::from_ne_bytes(buf[o..o + 8].try_into().unwrap());
    // Sanity check on the offsets: the record must name the pid asked for.
    if i32_at(40) != pid {
        return None;
    }
    let (sec, usec) = (i64_at(0), i32_at(8));
    if sec <= 0 || !(0..1_000_000).contains(&usec) {
        return None;
    }
    Some(ProcEntry {
        info: ProcInfo {
            pid,
            ppid: i32_at(560),
            start: sec as u64 * 1_000_000 + usec as u64,
        },
        pgid: i32_at(564),
        zombie: buf[36] as libc::c_int == libc::SZOMB as libc::c_int,
    })
}

/// Ppid, group, start time and state of `pid`, or `None` when it is gone
/// or unreadable.
#[cfg(target_os = "linux")]
pub fn proc_entry(pid: libc::pid_t) -> Option<ProcEntry> {
    if pid <= 0 {
        return None;
    }
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
    let f = parse_proc_stat(&stat)?;
    Some(ProcEntry {
        info: ProcInfo {
            pid,
            ppid: f.ppid,
            start: f.start,
        },
        pgid: f.pgid,
        zombie: f.zombie,
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn proc_entry(_pid: libc::pid_t) -> Option<ProcEntry> {
    None
}

/// Ppid and start time of `pid`, or `None` when it is gone or unreadable.
///
/// On macOS `proc_pidinfo` refuses another user's process (EPERM), such as
/// the root-owned `/usr/bin/login` that Terminal, Ghostty and iTerm start
/// each shell under, so this falls back to sysctl. `proc_entry` and
/// `processes` keep to `proc_pidinfo`: the pty kill walk only follows
/// processes it can signal.
pub fn proc_info(pid: libc::pid_t) -> Option<ProcInfo> {
    let entry = proc_entry(pid);
    #[cfg(target_os = "macos")]
    let entry = entry.or_else(|| kinfo_entry(pid));
    entry.map(|e| e.info)
}

/// Every process this module can read, zombies included.
#[cfg(target_os = "macos")]
pub fn processes() -> Vec<ProcEntry> {
    let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if n <= 0 {
        return Vec::new();
    }
    // Room for processes started since the count.
    let mut pids = vec![0 as libc::pid_t; n as usize + 64];
    let bytes = (pids.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int;
    let n = unsafe { libc::proc_listallpids(pids.as_mut_ptr() as *mut libc::c_void, bytes) };
    pids.truncate(n.max(0) as usize);
    pids.into_iter().filter_map(proc_entry).collect()
}

/// Every process this module can read, zombies included.
#[cfg(target_os = "linux")]
pub fn processes() -> Vec<ProcEntry> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
        .filter_map(proc_entry)
        .collect()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn processes() -> Vec<ProcEntry> {
    Vec::new()
}

/// The descendants of `roots` (pid and start time) in the list `all`, at
/// any depth, roots left out. A root counts only when `all` holds it with
/// the same start time, and a child only when it did not start before its
/// parent, so a reused pid adds nothing. A process whose parent exited has
/// been moved to init and is not found from its old ancestors.
pub fn descendants(all: &[ProcEntry], roots: &[(libc::pid_t, u64)]) -> Vec<ProcEntry> {
    let mut frontier: Vec<&ProcInfo> = all
        .iter()
        .map(|e| &e.info)
        .filter(|p| roots.contains(&(p.pid, p.start)))
        .collect();
    let mut seen: Vec<libc::pid_t> = frontier.iter().map(|p| p.pid).collect();
    let mut out = Vec::new();
    while let Some(parent) = frontier.pop() {
        for e in all {
            let p = &e.info;
            if p.ppid == parent.pid && p.start >= parent.start && !seen.contains(&p.pid) {
                seen.push(p.pid);
                out.push(e.clone());
                frontier.push(p);
            }
        }
    }
    out
}

/// The current time as a start-time token (see `ProcInfo::start`), or
/// `None` when the clock cannot be read. A process that started after this
/// token was taken has a larger `start`.
#[cfg(target_os = "macos")]
pub fn start_token_now() -> Option<u64> {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Some(d.as_secs() * 1_000_000 + u64::from(d.subsec_micros()))
}

/// The current time as a start-time token (see `ProcInfo::start`), or
/// `None` when the clock cannot be read. `/proc/<pid>/stat` counts clock
/// ticks on the boot-time clock, so this does too, rounded down like the
/// kernel.
#[cfg(target_os = "linux")]
pub fn start_token_now() -> Option<u64> {
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if hz <= 0 {
        return None;
    }
    let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) } != 0 {
        return None;
    }
    let hz = hz as u64;
    Some(ts.tv_sec as u64 * hz + ts.tv_nsec as u64 * hz / 1_000_000_000)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn start_token_now() -> Option<u64> {
    None
}

/// The fields of a `/proc/<pid>/stat` line the engine reads.
#[derive(Debug, PartialEq, Eq)]
struct StatFields {
    ppid: libc::pid_t,
    pgid: libc::pid_t,
    start: u64,
    zombie: bool,
}

/// State (field 3), ppid (4), pgrp (5) and starttime (22) from a
/// `/proc/<pid>/stat` line. The comm field (2) is in parentheses and may
/// itself hold spaces and parentheses, so the fields are counted from the
/// last `)`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_proc_stat(stat: &str) -> Option<StatFields> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // fields[0] is field 3 (state), so field N is fields[N - 3].
    Some(StatFields {
        ppid: fields.get(1)?.parse().ok()?,
        pgid: fields.get(2)?.parse().ok()?,
        start: fields.get(19)?.parse().ok()?,
        zombie: *fields.first()? == "Z",
    })
}

/// The chain of processes from `pid` up, `pid` first, at most `max` levels.
/// It stops below pid 1 and at the first process it cannot read.
///
/// Each level is re-checked before the walk goes on: the child must still
/// have the same start time and the same ppid after its parent was read, and
/// the parent must not have started after the child. Either failure means a
/// pid was reused during the walk, and the chain ends there.
pub fn ancestry(pid: libc::pid_t, max: usize) -> Vec<ProcInfo> {
    let mut out = Vec::new();
    let Some(mut cur) = proc_info(pid) else {
        return out;
    };
    out.push(cur.clone());
    while out.len() < max && cur.ppid > 1 {
        let Some(parent) = proc_info(cur.ppid) else {
            break;
        };
        match proc_info(cur.pid) {
            Some(again) if again.start == cur.start && again.ppid == cur.ppid => {}
            _ => break,
        }
        if parent.start > cur.start {
            break;
        }
        out.push(parent.clone());
        cur = parent;
    }
    out
}

/// How many levels `full_ancestry` walks at most: a bound against a loop,
/// not a policy.
const MAX_FULL_ANCESTRY: usize = 1024;

/// The whole chain from `pid` up to a child of init, `pid` first, or `None`
/// when the walk stopped before it got there (an unreadable parent, a
/// reused pid, the bound). A mutating call needs the whole chain: a peer
/// that nests itself deeper than `MAX_ANCESTRY` must not look unrelated
/// to the agent it runs under.
pub fn full_ancestry(pid: libc::pid_t) -> Option<Vec<ProcInfo>> {
    let chain = ancestry(pid, MAX_FULL_ANCESTRY);
    match chain.last() {
        Some(last) if last.ppid <= 1 => Some(chain),
        _ => None,
    }
}

/// Where and why `full_ancestry(pid)` stops short of init, as read now,
/// for an error message only: it decides nothing. The same walk as
/// `ancestry`, naming the step that ended it.
pub fn ancestry_gap(pid: libc::pid_t) -> String {
    let unreadable = |p: libc::pid_t| {
        format!(
            "pid {} is unreadable ({})",
            p,
            std::io::Error::last_os_error()
        )
    };
    let Some(mut cur) = proc_info(pid) else {
        return unreadable(pid);
    };
    let mut levels = 1;
    while cur.ppid > 1 {
        if levels >= MAX_FULL_ANCESTRY {
            return format!("more than {} levels", MAX_FULL_ANCESTRY);
        }
        let Some(parent) = proc_info(cur.ppid) else {
            return format!("{}, the parent of pid {}", unreadable(cur.ppid), cur.pid);
        };
        match proc_info(cur.pid) {
            Some(again) if again.start == cur.start && again.ppid == cur.ppid => {}
            Some(again) => {
                return format!(
                    "pid {} changed during the walk (ppid {} -> {}, start {} -> {})",
                    cur.pid, cur.ppid, again.ppid, cur.start, again.start
                )
            }
            None => return format!("{} after its parent was read", unreadable(cur.pid)),
        }
        if parent.start > cur.start {
            return format!(
                "parent pid {} started after its child pid {} ({} > {})",
                parent.pid, cur.pid, parent.start, cur.start
            );
        }
        cur = parent;
        levels += 1;
    }
    format!("the walk reaches init now ({} levels)", levels)
}

/// Whether `pid` runs confined in a way the engine does not: a CC agent's
/// Bash runs under the vendor sandbox, and so does everything it starts,
/// even a process that `setsid`/`nohup` moved under init. `None` when it
/// cannot be told.
///
/// macOS: `sandbox_check(pid, NULL, SANDBOX_FILTER_NONE)` (Seatbelt; a
/// gone pid reads as sandboxed). An engine that itself runs sandboxed
/// treats a sandboxed peer as its equal.
#[cfg(target_os = "macos")]
pub fn confined(pid: libc::pid_t) -> Option<bool> {
    extern "C" {
        fn sandbox_check(
            pid: libc::pid_t,
            operation: *const libc::c_char,
            filter_type: libc::c_int,
            ...
        ) -> libc::c_int;
    }
    // SANDBOX_FILTER_NONE
    let check = |p: libc::pid_t| unsafe { sandbox_check(p, std::ptr::null(), 0) } != 0;
    if pid <= 0 {
        return None;
    }
    Some(check(pid) && !check(std::process::id() as libc::pid_t))
}

/// Whether `pid` runs confined in a way the engine does not. Linux: the
/// vendor sandbox (bubblewrap) gives it its own mount namespace, so a peer
/// in another mount namespace than the engine is confined. `None` when it
/// cannot be told.
#[cfg(target_os = "linux")]
pub fn confined(pid: libc::pid_t) -> Option<bool> {
    if pid <= 0 {
        return None;
    }
    let ns = |p: &str| std::fs::read_link(format!("/proc/{}/ns/mnt", p)).ok();
    Some(ns(&pid.to_string())? != ns("self")?)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn confined(_pid: libc::pid_t) -> Option<bool> {
    None
}

/// The executable `pid` runs. C1a-21 checks it against
/// `<home>/bin/c9watch-agent`. On Linux a replaced or deleted file reads as
/// `<path> (deleted)`, which never equals the expected path.
#[cfg(target_os = "macos")]
pub fn peer_exe_path(pid: libc::pid_t) -> Result<PathBuf, String> {
    use std::os::unix::ffi::OsStrExt;
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let n =
        unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr() as *mut libc::c_void, buf.len() as u32) };
    if n <= 0 {
        return Err(format!(
            "proc_pidpath({}) failed: {}",
            pid,
            std::io::Error::last_os_error()
        ));
    }
    buf.truncate(n as usize);
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(&buf)))
}

/// The executable `pid` runs. C1a-21 checks it against
/// `<home>/bin/c9watch-agent`. On Linux a replaced or deleted file reads as
/// `<path> (deleted)`, which never equals the expected path.
#[cfg(target_os = "linux")]
pub fn peer_exe_path(pid: libc::pid_t) -> Result<PathBuf, String> {
    std::fs::read_link(format!("/proc/{}/exe", pid))
        .map_err(|e| format!("Failed to read /proc/{}/exe: {}", pid, e))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn peer_exe_path(_pid: libc::pid_t) -> Result<PathBuf, String> {
    Err("peer exe path is only supported on macOS and Linux".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::UnixStream;

    fn my_pid() -> libc::pid_t {
        std::process::id() as libc::pid_t
    }

    #[test]
    fn peer_cred_over_a_socketpair_is_ourselves() {
        let (a, _b) = UnixStream::pair().unwrap();
        let cred = peer_cred(a.as_raw_fd()).unwrap();
        assert_eq!(cred.pid, my_pid());
        assert_eq!(cred.uid, engine_uid());
        check_peer_uid(&cred, engine_uid()).unwrap();
    }

    #[test]
    fn another_uid_is_rejected() {
        let cred = PeerCred {
            pid: 4242,
            uid: engine_uid().wrapping_add(1),
        };
        let err = check_peer_uid(&cred, engine_uid()).unwrap_err();
        assert!(err.starts_with(PEER_UID_MISMATCH), "{}", err);
    }

    #[test]
    fn ancestry_starts_at_the_test_process() {
        let me = my_pid();
        let chain = ancestry(me, MAX_ANCESTRY);
        assert!(chain.len() >= 2, "{:?}", chain);
        assert!(chain.len() <= MAX_ANCESTRY);
        assert_eq!(chain[0].pid, me);
        assert_eq!(chain[0].ppid, unsafe { libc::getppid() });
        for pair in chain.windows(2) {
            assert_eq!(pair[1].pid, pair[0].ppid);
            assert!(pair[1].start <= pair[0].start, "{:?}", pair);
        }
        assert!(chain.iter().all(|p| p.start > 0));
        // The same process read twice has the same start time.
        assert_eq!(proc_info(me).unwrap().start, chain[0].start);
    }

    #[test]
    #[ignore = "diagnostic: C9_WALK_PIDS=<pid,...> cargo test ... -- --ignored --nocapture"]
    fn walk_given_pids() {
        for pid in std::env::var("C9_WALK_PIDS").unwrap_or_default().split(',') {
            let Ok(pid) = pid.trim().parse::<libc::pid_t>() else {
                continue;
            };
            println!(
                "pid {} ancestry {:?}",
                pid,
                ancestry(pid, MAX_FULL_ANCESTRY)
            );
            println!("pid {} full {:?}", pid, full_ancestry(pid).map(|c| c.len()));
            println!("pid {} gap {}", pid, ancestry_gap(pid));
        }
    }

    #[test]
    fn ancestry_gap_names_where_the_walk_stops() {
        assert!(ancestry_gap(my_pid()).starts_with("the walk reaches init now"));
        assert!(ancestry_gap(-1).starts_with("pid -1 is unreadable"));
    }

    #[test]
    fn full_ancestry_of_the_test_process_reaches_init() {
        let chain = full_ancestry(my_pid()).expect("a complete chain");
        assert_eq!(chain[0].pid, my_pid());
        assert!(chain.last().unwrap().ppid <= 1, "{:?}", chain);
        assert!(full_ancestry(-1).is_none());
    }

    /// The sysctl fallback reads the fields at the right offsets: it agrees
    /// with `proc_pidinfo` on processes both can read.
    #[cfg(target_os = "macos")]
    #[test]
    fn sysctl_reads_a_process_as_proc_pidinfo_does() {
        let me = my_pid();
        for pid in [me, unsafe { libc::getppid() }] {
            assert_eq!(kinfo_entry(pid), proc_entry(pid), "pid {}", pid);
        }
        assert!(kinfo_entry(-1).is_none());
        assert!(kinfo_entry(0).is_none());
        assert!(kinfo_entry(libc::pid_t::MAX).is_none());
    }

    /// A root-owned process (`proc_pidinfo` gives EPERM) is still read, and
    /// a chain through it reaches init: a shell that Terminal, Ghostty or
    /// iTerm started under the setuid `/usr/bin/login`, when one runs.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_chain_through_a_root_process_reaches_init() {
        let all: Vec<libc::pid_t> = {
            let mut pids = vec![0 as libc::pid_t; 8192];
            let bytes = (pids.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int;
            let n =
                unsafe { libc::proc_listallpids(pids.as_mut_ptr() as *mut libc::c_void, bytes) };
            pids.truncate(n.max(0) as usize);
            pids
        };
        let root: Vec<ProcInfo> = all
            .iter()
            .filter(|&&p| p > 1 && proc_entry(p).is_none())
            .filter_map(|&p| proc_info(p))
            .collect();
        assert!(!root.is_empty(), "no process that only sysctl can read");
        // Candidates: user processes whose parent only sysctl can read,
        // then those parents. Any of them, or an ancestor, may exit while
        // it is walked (seen on the GitHub macOS runner): such a candidate
        // is skipped, and the check fails only when none gives a chain.
        let under_root: Vec<libc::pid_t> = all
            .iter()
            .copied()
            .filter(|&p| {
                proc_entry(p)
                    .is_some_and(|e| e.info.ppid > 1 && root.iter().any(|r| r.pid == e.info.ppid))
            })
            .take(16)
            .collect();
        let candidates = under_root
            .iter()
            .map(|&p| (p, true))
            .chain(root.iter().take(16).map(|r| (r.pid, false)));
        let mut tried = Vec::new();
        for (start, user) in candidates {
            let Some(chain) = full_ancestry(start) else {
                tried.push(start);
                continue;
            };
            assert!(chain.last().unwrap().ppid <= 1, "{:?}", chain);
            if user {
                // The walk went through the root parent, not around it. A
                // parent that exited meanwhile leaves the child under init.
                if chain.len() < 2 || !root.iter().any(|r| r.pid == chain[1].pid) {
                    tried.push(start);
                    continue;
                }
            } else {
                eprintln!(
                    "no user process under a root parent gave a chain; walked {}",
                    start
                );
            }
            return;
        }
        panic!("no candidate gave a complete chain; tried {:?}", tried);
    }

    #[test]
    fn the_test_process_is_not_confined() {
        assert_eq!(confined(my_pid()), Some(false));
        assert_eq!(confined(0), None);
    }

    /// A process under a Seatbelt profile is confined, also after it was
    /// moved under init; the engine is not.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_sandboxed_process_is_confined() {
        let mut child = match std::process::Command::new("/usr/bin/sandbox-exec")
            .args(["-p", "(version 1)(allow default)", "/bin/sleep", "30"])
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("skip: no sandbox-exec: {}", e);
                return;
            }
        };
        let pid = child.id() as libc::pid_t;
        // sandbox-exec applies the profile and then execs sleep.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut got = confined(pid);
        while got != Some(true) && std::time::Instant::now() < deadline {
            if let Ok(Some(st)) = child.try_wait() {
                eprintln!("skip: sandbox-exec exited ({}); nested sandbox?", st);
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
            got = confined(pid);
        }
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(got, Some(true));
    }

    #[test]
    fn ancestry_respects_the_level_limit() {
        assert_eq!(ancestry(my_pid(), 1).len(), 1);
        assert!(ancestry(-1, MAX_ANCESTRY).is_empty());
        assert!(ancestry(0, MAX_ANCESTRY).is_empty());
    }

    #[test]
    fn a_reaped_pid_has_no_info() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id() as libc::pid_t;
        child.wait().unwrap();
        // Reaped: the pid is free. It could be reused at once, but then it
        // would be another process with a later start time.
        if let Some(info) = proc_info(pid) {
            assert!(info.start >= proc_info(my_pid()).unwrap().start);
        }
    }

    #[test]
    fn exe_of_the_test_process_is_readable() {
        let exe = peer_exe_path(my_pid()).unwrap();
        let want = std::env::current_exe().unwrap().canonicalize().unwrap();
        assert_eq!(exe.canonicalize().unwrap(), want);
        assert!(peer_exe_path(-1).is_err());
    }

    #[test]
    fn processes_started_before_now_are_not_later_than_the_token() {
        let before = start_token_now().unwrap();
        assert!(proc_info(my_pid()).unwrap().start <= before);
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .unwrap();
        let pid = child.id() as libc::pid_t;
        let start = proc_info(pid).map(|p| p.start);
        let after = start_token_now().unwrap();
        let _ = child.kill();
        let _ = child.wait();
        let start = start.unwrap();
        assert!(start >= before, "{} < {}", start, before);
        assert!(start <= after, "{} > {}", start, after);
    }

    #[test]
    fn processes_hold_the_test_process_and_its_children() {
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .unwrap();
        let pid = child.id() as libc::pid_t;
        let all = processes();
        let _ = child.kill();
        let _ = child.wait();
        let me = proc_info(my_pid()).unwrap();
        let mine = all.iter().find(|e| e.info.pid == me.pid).unwrap();
        assert_eq!(mine.info, me);
        assert_eq!(mine.pgid, unsafe { libc::getpgrp() });
        assert!(!mine.zombie);
        let found = descendants(&all, &[(me.pid, me.start)]);
        assert!(found.iter().any(|e| e.info.pid == pid), "{:?}", found);
        // A root with another start time is a reused pid: nothing below it.
        assert!(descendants(&all, &[(me.pid, me.start + 1)]).is_empty());
    }

    #[test]
    fn descendants_walk_every_level_and_skip_later_parents() {
        let e = |pid, ppid, start| ProcEntry {
            info: ProcInfo { pid, ppid, start },
            pgid: pid,
            zombie: false,
        };
        // 10 -> 11 -> 12, 10 -> 13; 14 claims parent 11 but started first.
        let all = [
            e(10, 1, 100),
            e(11, 10, 110),
            e(12, 11, 120),
            e(13, 10, 130),
            e(14, 11, 105),
            e(15, 1, 150),
        ];
        let mut pids: Vec<_> = descendants(&all, &[(10, 100)])
            .iter()
            .map(|e| e.info.pid)
            .collect();
        pids.sort();
        assert_eq!(pids, vec![11, 12, 13]);
        // An orphan kept as a root still yields its own children.
        let all = [e(11, 1, 110), e(12, 11, 120)];
        assert_eq!(descendants(&all, &[(10, 100), (11, 110)]).len(), 1);
    }

    #[test]
    fn proc_stat_is_parsed_after_the_last_paren() {
        // comm "a) (b" holds spaces and parentheses.
        let mut stat = String::from("123 (a) (b) S 77");
        for n in 5..=21 {
            stat.push_str(&format!(" {}", n));
        }
        stat.push_str(" 987654 rest of the line");
        assert_eq!(
            parse_proc_stat(&stat),
            Some(StatFields {
                ppid: 77,
                pgid: 5,
                start: 987654,
                zombie: false
            })
        );
        let stat = stat.replace(" S 77", " Z 77");
        assert!(parse_proc_stat(&stat).unwrap().zombie);
        assert_eq!(parse_proc_stat("123 (x) S"), None);
        assert_eq!(parse_proc_stat("no paren"), None);
    }
}
