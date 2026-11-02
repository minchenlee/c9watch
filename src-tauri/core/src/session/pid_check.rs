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

    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result == 0 {
        return true;
    }

    // EPERM means the process exists but is not inspectable by this caller.
    // Treating it as dead would incorrectly clear a real session when c9watch
    // lacks permission to inspect the process.
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
pub(crate) fn pid_is_alive(_pid: u32) -> bool {
    true
}

/// How much later than the recorded start the process holding a pid may have
/// started and still count as the same process. Both sides are whole seconds
/// (the registry's `procStart` string, sysinfo's `start_time()`), so the
/// truncation alone can differ by 1s; Linux derives `start_time()` from boot
/// time plus ticks, which can add about another second. A real reuse needs
/// the old process to exit and the pid space to wrap around, which takes far
/// longer than 5s, so the slack costs no real detection.
const PID_REUSE_TOLERANCE_SECS: u64 = 5;

/// How far before `startedAt` a parsed `procStart` may lie and still be
/// trusted. Claude Code writes `startedAt` once it has loaded, which was
/// observed up to ~11s after `procStart` on a busy machine. A larger gap
/// means `procStart` was read in the wrong timezone or format, so it is
/// ignored and `startedAt` is used instead.
const PROC_START_MAX_LEAD_SECS: u64 = 10 * 60;

/// When the Claude Code process behind a `~/.claude/sessions/<pid>.json`
/// record started, as the record states it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RecordedStart {
    /// `procStart` (e.g. `"Wed Oct 14 13:35:57 2026"`, UTC), seconds since
    /// the epoch. `None` when absent or not parseable.
    pub proc_start_secs: Option<u64>,
    /// `startedAt`, milliseconds since the epoch.
    pub started_at_ms: Option<i64>,
}

impl RecordedStart {
    pub(crate) fn from_record(record: &serde_json::Value) -> Self {
        Self {
            proc_start_secs: record
                .get("procStart")
                .and_then(serde_json::Value::as_str)
                .and_then(parse_proc_start),
            started_at_ms: record.get("startedAt").and_then(serde_json::Value::as_i64),
        }
    }

    /// The best start time to compare against: `procStart` when it agrees
    /// with `startedAt` (or `startedAt` is absent), else `startedAt`.
    fn start_secs(&self) -> Option<u64> {
        let started_at = self
            .started_at_ms
            .and_then(|ms| u64::try_from(ms / 1000).ok());
        match (self.proc_start_secs, started_at) {
            (Some(proc), Some(started))
                if proc <= started + PID_REUSE_TOLERANCE_SECS
                    && proc + PROC_START_MAX_LEAD_SECS >= started =>
            {
                Some(proc)
            }
            (Some(proc), None) => Some(proc),
            (_, started) => started,
        }
    }
}

/// Whether the process now holding a pid is not the one a registry record
/// describes: it started after that process did. A reused pid always belongs
/// to a newer process, so only a later start counts. `None` on either side
/// (an older Claude Code without start fields, or a start time c9watch can't
/// read) never counts as reuse.
pub(crate) fn pid_was_reused(recorded: &RecordedStart, actual_start_secs: Option<u64>) -> bool {
    match (recorded.start_secs(), actual_start_secs) {
        (Some(recorded), Some(actual)) => actual > recorded + PID_REUSE_TOLERANCE_SECS,
        _ => false,
    }
}

/// Parses Claude Code's `procStart`, the `ps -o lstart` format in UTC
/// (`"Wed Oct 14 13:35:57 2026"`; a one-digit day may be space-padded).
fn parse_proc_start(raw: &str) -> Option<u64> {
    let normalized = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let time = chrono::NaiveDateTime::parse_from_str(&normalized, "%a %b %e %H:%M:%S %Y").ok()?;
    u64::try_from(time.and_utc().timestamp()).ok()
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

    /// 2026-10-14T13:35:57Z, the `procStart` in [`record`].
    const PROC_START: u64 = 1_791_984_957;

    fn record() -> RecordedStart {
        RecordedStart::from_record(&serde_json::json!({
            "procStart": "Wed Oct 14 13:35:57 2026",
            "startedAt": 1_791_984_959_765_i64,
        }))
    }

    #[test]
    fn parses_proc_start_as_utc() {
        assert_eq!(
            parse_proc_start("Wed Oct 14 13:35:57 2026"),
            Some(PROC_START)
        );
        assert_eq!(
            parse_proc_start("Sun Oct  4 13:35:57 2026"),
            Some(PROC_START - 10 * 86_400)
        );
        assert_eq!(parse_proc_start("2026-10-14T13:35:57Z"), None);
        assert_eq!(parse_proc_start(""), None);
    }

    #[test]
    fn same_process_is_not_reused() {
        assert!(!pid_was_reused(&record(), Some(PROC_START)));
        assert!(!pid_was_reused(&record(), Some(PROC_START + 5)));
    }

    #[test]
    fn later_process_on_the_same_pid_is_reused() {
        assert!(pid_was_reused(&record(), Some(PROC_START + 6)));
        assert!(pid_was_reused(&record(), Some(PROC_START + 3600)));
    }

    #[test]
    fn unknown_start_on_either_side_is_never_reuse() {
        assert!(!pid_was_reused(&record(), None));
        assert!(!pid_was_reused(
            &RecordedStart::default(),
            Some(PROC_START + 3600)
        ));
    }

    #[test]
    fn started_at_is_used_when_proc_start_is_missing_or_unparsable() {
        for record in [
            serde_json::json!({"startedAt": 1_791_984_959_765_i64}),
            serde_json::json!({"procStart": "garbage", "startedAt": 1_791_984_959_765_i64}),
        ] {
            let recorded = RecordedStart::from_record(&record);
            assert!(!pid_was_reused(&recorded, Some(PROC_START)));
            assert!(!pid_was_reused(&recorded, Some(PROC_START + 7)));
            assert!(pid_was_reused(&recorded, Some(PROC_START + 8)));
        }
    }

    /// A `procStart` written in local time west of UTC parses hours before
    /// `startedAt`; trusting it would end a live session.
    #[test]
    fn proc_start_far_from_started_at_falls_back_to_started_at() {
        let recorded = RecordedStart::from_record(&serde_json::json!({
            "procStart": "Wed Oct 14 08:35:57 2026",
            "startedAt": 1_791_984_959_765_i64,
        }));
        assert!(!pid_was_reused(&recorded, Some(PROC_START)));
    }

    #[cfg(unix)]
    #[test]
    fn detects_live_pid() {
        // Our own pid must be alive.
        assert!(pid_is_alive(std::process::id()));
    }
}
