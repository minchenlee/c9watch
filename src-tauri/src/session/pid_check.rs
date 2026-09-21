//! Shared PID liveness check.
//!
//! `claude agents --json` is a registry Claude Code itself writes and prunes;
//! pruning happens in the agent's own exit path, so a hard kill (SIGKILL, or
//! a process wedged in an uninterruptible syscall that a signal can't
//! interrupt) can leave a stale entry behind indefinitely. Anything that
//! trusts this list as "currently running" should cross-check the reported
//! pid against the real process table first.

#[cfg(unix)]
pub(crate) fn pid_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
pub(crate) fn pid_is_alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn detects_dead_pid() {
        // PID 0 is never a real process.
        assert!(!pid_is_alive(0));
        // PID 999999 is extremely unlikely to be live (kernel default pid_max
        // on macOS is 99999, and even on Linux systems with expanded range
        // it's highly unlikely to hit this).
        assert!(!pid_is_alive(999_999));
    }

    #[cfg(unix)]
    #[test]
    fn detects_live_pid() {
        // Our own pid must be alive.
        assert!(pid_is_alive(std::process::id()));
    }
}
