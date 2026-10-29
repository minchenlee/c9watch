//! Peer uid for clients and the engine: who is on the other end of a
//! connected Unix socket (`LOCAL_PEERPID` and `getpeereid` on macOS,
//! `SO_PEERCRED` on Linux).
//!
//! This part lives outside the `engine` feature, so a dev GUI can check
//! that the engine socket it reads is served by its own uid. The rest of
//! the peer checks (ancestry, executable) are in `engine::peer`.

use std::os::unix::io::RawFd;

/// Error code for a peer that runs under another uid than the engine.
pub const PEER_UID_MISMATCH: &str = "peer_uid_mismatch";

/// The peer of a connected Unix socket, as the kernel reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerCred {
    pub pid: libc::pid_t,
    pub uid: libc::uid_t,
}

/// Pid and uid of the peer on `fd`.
#[cfg(target_os = "macos")]
pub fn peer_cred(fd: RawFd) -> Result<PeerCred, String> {
    let (mut uid, mut gid) = (0, 0);
    if unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } != 0 {
        return Err(format!(
            "getpeereid failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    let r = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            &mut pid as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    if r != 0 {
        return Err(format!(
            "LOCAL_PEERPID failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(PeerCred { pid, uid })
}

/// Pid and uid of the peer on `fd`.
#[cfg(target_os = "linux")]
pub fn peer_cred(fd: RawFd) -> Result<PeerCred, String> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let r = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    if r != 0 {
        return Err(format!(
            "SO_PEERCRED failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(PeerCred {
        pid: cred.pid,
        uid: cred.uid,
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn peer_cred(_fd: RawFd) -> Result<PeerCred, String> {
    Err("peer credentials are only supported on macOS and Linux".to_string())
}

/// Accept the peer only when it runs under `engine_uid`. The socket is 0600,
/// so only root can get past the file mode with another uid.
pub fn check_peer_uid(cred: &PeerCred, engine_uid: libc::uid_t) -> Result<(), String> {
    if cred.uid == engine_uid {
        Ok(())
    } else {
        Err(format!(
            "{}: peer pid {} has uid {}, the engine runs as {}",
            PEER_UID_MISMATCH, cred.pid, cred.uid, engine_uid
        ))
    }
}

/// The engine's own effective uid.
pub fn engine_uid() -> libc::uid_t {
    unsafe { libc::geteuid() }
}
