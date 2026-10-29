//! Peer identity for the engine socket: who is on the other end of a
//! connection, which processes it descends from, and which executable it
//! runs.
//!
//! - Peer pid and uid: `LOCAL_PEERPID` and `getpeereid` on macOS,
//!   `SO_PEERCRED` on Linux.
//! - Process info (ppid, group and start time; the process list):
//!   `proc_listallpids` and `proc_pidinfo(PROC_PIDTBSDINFO)` on
//!   macOS, `/proc/<pid>/stat` on Linux. Never `ps`.
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
pub fn proc_info(pid: libc::pid_t) -> Option<ProcInfo> {
    proc_entry(pid).map(|e| e.info)
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
