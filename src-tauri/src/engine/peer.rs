//! Peer identity for the engine socket: who is on the other end of a
//! connection, which processes it descends from, and which executable it
//! runs.
//!
//! - Peer pid and uid: `LOCAL_PEERPID` and `getpeereid` on macOS,
//!   `SO_PEERCRED` on Linux.
//! - Process info (ppid and start time): `proc_pidinfo(PROC_PIDTBSDINFO)` on
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

/// Ppid and start time of `pid`, or `None` when it is gone or unreadable.
#[cfg(target_os = "macos")]
pub fn proc_info(pid: libc::pid_t) -> Option<ProcInfo> {
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
    Some(ProcInfo {
        pid: info.pbi_pid as libc::pid_t,
        ppid: info.pbi_ppid as libc::pid_t,
        start: info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec,
    })
}

/// Ppid and start time of `pid`, or `None` when it is gone or unreadable.
#[cfg(target_os = "linux")]
pub fn proc_info(pid: libc::pid_t) -> Option<ProcInfo> {
    if pid <= 0 {
        return None;
    }
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
    let (ppid, start) = parse_proc_stat(&stat)?;
    Some(ProcInfo { pid, ppid, start })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn proc_info(_pid: libc::pid_t) -> Option<ProcInfo> {
    None
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

/// Ppid (field 4) and starttime (field 22) from a `/proc/<pid>/stat` line.
/// The comm field (2) is in parentheses and may itself hold spaces and
/// parentheses, so the fields are counted from the last `)`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_proc_stat(stat: &str) -> Option<(libc::pid_t, u64)> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // fields[0] is field 3 (state), so field N is fields[N - 3].
    let ppid = fields.get(1)?.parse().ok()?;
    let start = fields.get(19)?.parse().ok()?;
    Some((ppid, start))
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
    fn proc_stat_is_parsed_after_the_last_paren() {
        // comm "a) (b" holds spaces and parentheses.
        let mut stat = String::from("123 (a) (b) S 77");
        for n in 5..=21 {
            stat.push_str(&format!(" {}", n));
        }
        stat.push_str(" 987654 rest of the line");
        assert_eq!(parse_proc_stat(&stat), Some((77, 987654)));
        assert_eq!(parse_proc_stat("123 (x) S"), None);
        assert_eq!(parse_proc_stat("no paren"), None);
    }
}
