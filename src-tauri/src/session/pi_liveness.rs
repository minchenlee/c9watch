//! Process evidence for pi session liveness.
//!
//! pi writes no pid file, lock file or session variable, and it does not hold
//! the transcript open while idle. The only process-anchored fact is the
//! working directory of each live `pi` process. A transcript records the same
//! cwd in its session header, so per cwd: N live pi processes keep the N most
//! recently modified fresh transcripts alive, and the older ones have ended.
//! A process can only keep a transcript written after it started, so a `pi`
//! restarted in the same cwd does not revive the killed session's card
//! before its own first prompt.
//!
//! Any uncertainty (listing failed, a candidate cwd or argv unreadable, a
//! transcript without an exact header cwd) keeps the mtime-based freshness
//! windows in `pi.rs`. Process evidence only ever ends a session early; it
//! never extends one past those windows.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::Path;

/// A transcript modified this recently stays alive without a matching
/// process, and a process may keep a transcript modified up to this long
/// before its recorded start. The probe runs after the transcript stat in
/// the same poll, so the skew between the two reads is milliseconds; the
/// window mainly covers whole-second process start times, coarse mtime
/// resolution (1 s on HFS+, 2 s on FAT/SMB) and clock skew on network
/// volumes. Ten seconds is under three 3.5 s polls, so a killed session
/// still ends within a few polls of its last write.
pub(crate) const PI_PROCESS_GRACE_MS: i64 = 10_000;

/// Live pi processes per exact cwd, or no usable evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PiProcessEvidence {
    /// Listing failed or at least one candidate could not be attributed.
    /// Callers keep the current mtime-based behaviour for every transcript.
    Unavailable,
    /// Start times (ms since epoch; `None` when unknown) of the live pi
    /// processes, keyed by normalized cwd.
    Live(HashMap<String, Vec<Option<i64>>>),
}

/// One fresh transcript that process evidence may end.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PiLivenessCandidate<'a> {
    /// Exact cwd from a session header. `None` when only a lossy dirname
    /// decode is available; such transcripts are never ended.
    pub cwd: Option<&'a str>,
    pub modified_ms: i64,
}

/// For each candidate, whether it stays alive under the process evidence.
pub(crate) fn pi_alive_flags(
    candidates: &[PiLivenessCandidate<'_>],
    evidence: &PiProcessEvidence,
    now_ms: i64,
) -> Vec<bool> {
    let PiProcessEvidence::Live(live) = evidence else {
        return vec![true; candidates.len()];
    };
    let mut alive = vec![false; candidates.len()];
    let mut by_cwd: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, candidate) in candidates.iter().enumerate() {
        if now_ms.saturating_sub(candidate.modified_ms) < PI_PROCESS_GRACE_MS {
            alive[index] = true;
        }
        match candidate.cwd {
            Some(cwd) => by_cwd.entry(normalize_cwd(cwd)).or_default().push(index),
            None => alive[index] = true,
        }
    }
    for (cwd, mut indexes) in by_cwd {
        let Some(starts) = live.get(&cwd) else {
            continue;
        };
        // Unknown start times may claim any transcript (never end on doubt).
        let mut starts: Vec<i64> = starts.iter().map(|s| s.unwrap_or(i64::MIN)).collect();
        starts.sort_unstable();
        // Newest transcript first; ties keep input order (deterministic).
        indexes.sort_by_key(|&index| std::cmp::Reverse(candidates[index].modified_ms));
        for index in indexes {
            // Give each transcript the latest-started process that could have
            // written it, so earlier-started processes stay free for older
            // transcripts. This greedy order maximizes kept transcripts.
            let limit = candidates[index]
                .modified_ms
                .saturating_add(PI_PROCESS_GRACE_MS);
            let fits = starts.partition_point(|&start| start <= limit);
            if fits > 0 {
                starts.remove(fits - 1);
                alive[index] = true;
            }
        }
    }
    alive
}

/// What one process-table row says about pi.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PiProcessClass {
    NotPi,
    /// A pi process; `cwd` is `None` when it could not be read.
    Pi {
        cwd: Option<String>,
        started_ms: Option<i64>,
    },
    /// A same-user node/bun process whose argv could not be read.
    Unknown,
}

/// Raw facts for one process-table row, decoupled from sysinfo for tests.
#[derive(Debug, Clone)]
pub(crate) struct PiProcessRow<'a> {
    pub name: &'a str,
    pub argv: &'a [&'a str],
    pub cwd: Option<&'a str>,
    pub started_ms: Option<i64>,
    /// `false` only when the row is known to belong to another user.
    pub same_user: bool,
}

/// Process names that may be a pi process and need argv/cwd to decide.
/// pi is a Node script that rewrites its title: `ps` shows `pi`, while the
/// exec path (what sysinfo reports as `name()` on macOS) stays `node`.
pub(crate) fn is_pi_candidate_name(name: &str) -> bool {
    matches!(name, "pi" | "node" | "bun")
}

pub(crate) fn classify_pi_process(row: &PiProcessRow<'_>) -> PiProcessClass {
    if !is_pi_candidate_name(row.name) {
        return PiProcessClass::NotPi;
    }
    let is_pi = row.name == "pi"
        || row.argv.first().is_some_and(|arg| basename(arg) == "pi")
        // Before the title rewrite, argv is `node /…/bin/pi …`.
        || row.argv.get(1).is_some_and(|arg| basename(arg) == "pi");
    if is_pi {
        return PiProcessClass::Pi {
            cwd: row.cwd.map(str::to_string),
            started_ms: row.started_ms,
        };
    }
    if row.argv.is_empty() && row.same_user {
        // pi always runs as the user, so only same-user rows can hide one.
        return PiProcessClass::Unknown;
    }
    PiProcessClass::NotPi
}

/// Fold classified rows into evidence. One unattributable pi process could
/// belong to any cwd, so it makes the whole snapshot uncertain.
pub(crate) fn evidence_from_classes(
    classes: impl IntoIterator<Item = PiProcessClass>,
) -> PiProcessEvidence {
    let mut live: HashMap<String, Vec<Option<i64>>> = HashMap::new();
    for class in classes {
        match class {
            PiProcessClass::NotPi => {}
            PiProcessClass::Pi {
                cwd: Some(cwd),
                started_ms,
            } => live
                .entry(normalize_cwd(&cwd))
                .or_default()
                .push(started_ms),
            PiProcessClass::Pi { cwd: None, .. } | PiProcessClass::Unknown => {
                return PiProcessEvidence::Unavailable;
            }
        }
    }
    PiProcessEvidence::Live(live)
}

fn basename(arg: &str) -> &str {
    Path::new(arg)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or(arg)
}

fn normalize_cwd(cwd: &str) -> String {
    let trimmed = cwd.trim_end_matches('/');
    if trimmed.is_empty() {
        cwd.to_string()
    } else {
        trimmed.to_string()
    }
}

// ── Probe ───────────────────────────────────────────────────────────

/// Source of process evidence. Injected so tests never read the real table.
pub(crate) trait PiProcessProbe: Send {
    fn scan(&mut self) -> PiProcessEvidence;
}

/// Probe that never has evidence; keeps the mtime-only behaviour.
pub(crate) struct NoPiProcessProbe;

impl PiProcessProbe for NoPiProcessProbe {
    fn scan(&mut self) -> PiProcessEvidence {
        PiProcessEvidence::Unavailable
    }
}

/// sysinfo-backed probe. The `System` persists across polls so sysinfo only
/// creates rows for new pids. A scan lists every pid with the minimal
/// refresh kind, then reads argv and cwd only for node/bun/pi rows.
pub(crate) struct SysinfoPiProcessProbe {
    system: sysinfo::System,
}

impl SysinfoPiProcessProbe {
    pub(crate) fn new() -> Self {
        Self {
            system: sysinfo::System::new(),
        }
    }
}

impl PiProcessProbe for SysinfoPiProcessProbe {
    fn scan(&mut self) -> PiProcessEvidence {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, UpdateKind};
        if !sysinfo::IS_SUPPORTED_SYSTEM {
            return PiProcessEvidence::Unavailable;
        }
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::new(),
        );
        // The table always contains c9watch itself; empty means it failed.
        if self.system.processes().is_empty() {
            return PiProcessEvidence::Unavailable;
        }
        let candidates: Vec<sysinfo::Pid> = self
            .system
            .processes()
            .iter()
            .filter(|(_, process)| process.name().to_str().is_some_and(is_pi_candidate_name))
            .map(|(pid, _)| *pid)
            .collect();
        if candidates.is_empty() {
            return PiProcessEvidence::Live(HashMap::new());
        }
        // A pid may be reused between polls, so argv and cwd are re-read.
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&candidates),
            false,
            ProcessRefreshKind::new()
                .with_cmd(UpdateKind::Always)
                .with_cwd(UpdateKind::Always),
        );
        let own_uid = current_uid();
        let classes = candidates.iter().filter_map(|pid| {
            let process = self.system.process(*pid)?;
            let name = process.name().to_string_lossy();
            let argv_owned: Vec<String> = process
                .cmd()
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            let argv: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
            let cwd = process.cwd().map(|cwd| cwd.to_string_lossy().into_owned());
            let start_secs = process.start_time();
            let started_ms = (start_secs > 0).then(|| (start_secs as i64).saturating_mul(1000));
            let same_user = match (own_uid, process.user_id()) {
                (Some(own), Some(uid)) => uid_matches(uid, own),
                _ => true,
            };
            Some(classify_pi_process(&PiProcessRow {
                name: name.as_ref(),
                argv: &argv,
                cwd: cwd.as_deref(),
                started_ms,
                same_user,
            }))
        });
        evidence_from_classes(classes.collect::<Vec<_>>())
    }
}

#[cfg(unix)]
fn current_uid() -> Option<u32> {
    // SAFETY: getuid has no preconditions and cannot fail.
    Some(unsafe { libc::getuid() })
}

#[cfg(not(unix))]
fn current_uid() -> Option<u32> {
    None
}

#[cfg(unix)]
fn uid_matches(uid: &sysinfo::Uid, own: u32) -> bool {
    **uid == own
}

#[cfg(not(unix))]
fn uid_matches(_uid: &sysinfo::Uid, _own: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000_000;

    /// `n` live processes per cwd with unknown start times.
    fn live(pairs: &[(&str, usize)]) -> PiProcessEvidence {
        PiProcessEvidence::Live(
            pairs
                .iter()
                .map(|(cwd, n)| (cwd.to_string(), vec![None; *n]))
                .collect(),
        )
    }

    /// Live processes in one cwd, started `ages` ms before `NOW`.
    fn started(cwd: &str, ages: &[i64]) -> PiProcessEvidence {
        let starts: Vec<Option<i64>> = ages.iter().map(|age| Some(NOW - age)).collect();
        PiProcessEvidence::Live(HashMap::from([(cwd.to_string(), starts)]))
    }

    fn pi(cwd: &str) -> PiProcessClass {
        PiProcessClass::Pi {
            cwd: Some(cwd.into()),
            started_ms: None,
        }
    }

    fn candidate(cwd: &str, age_ms: i64) -> PiLivenessCandidate<'_> {
        PiLivenessCandidate {
            cwd: Some(cwd),
            modified_ms: NOW - age_ms,
        }
    }

    /// Manual probe: records what sysinfo reports for pi-like processes and
    /// how long a scan takes. Run with a stand-in such as
    /// `node -e "process.title='pi'; setInterval(() => {}, 1e6)"`:
    /// `cargo test --lib pi_liveness::tests::live_probe_report -- --ignored --nocapture`
    #[test]
    #[ignore = "reads the live process table"]
    fn live_probe_report() {
        use std::time::Instant;
        let mut probe = SysinfoPiProcessProbe::new();
        let started = Instant::now();
        let first = probe.scan();
        let cold = started.elapsed();
        let mut warm = Vec::new();
        for _ in 0..10 {
            let started = Instant::now();
            probe.scan();
            warm.push(started.elapsed());
        }
        for (pid, process) in probe.system.processes() {
            let name = process.name().to_string_lossy();
            let argv0 = process
                .cmd()
                .first()
                .map(|a| a.to_string_lossy().into_owned());
            if name == "pi" || argv0.as_deref().is_some_and(|a| basename(a) == "pi") {
                eprintln!(
                    "pid={pid} name={name:?} cmd={:?} exe={:?} cwd={:?} start={}",
                    process.cmd(),
                    process.exe(),
                    process.cwd(),
                    process.start_time()
                );
            }
        }
        eprintln!(
            "processes={} evidence={first:?}",
            probe.system.processes().len()
        );
        eprintln!("cold={cold:?} warm={warm:?}");
    }

    #[test]
    fn no_process_in_cwd_ends_the_session() {
        let flags = pi_alive_flags(&[candidate("/w/a", 60_000)], &live(&[]), NOW);
        assert_eq!(flags, vec![false]);
    }

    #[test]
    fn one_process_keeps_only_the_newest_of_three_transcripts() {
        let candidates = [
            candidate("/w/a", 300_000),
            candidate("/w/a", 60_000),
            candidate("/w/a", 600_000),
        ];
        let flags = pi_alive_flags(&candidates, &live(&[("/w/a", 1)]), NOW);
        assert_eq!(flags, vec![false, true, false]);
    }

    #[test]
    fn cwds_are_counted_independently() {
        let candidates = [
            candidate("/w/a", 60_000),
            candidate("/w/a", 120_000),
            candidate("/w/b", 60_000),
            candidate("/w/b", 120_000),
            candidate("/w/c", 60_000),
        ];
        let flags = pi_alive_flags(&candidates, &live(&[("/w/a", 2), ("/w/b", 1)]), NOW);
        assert_eq!(flags, vec![true, true, true, false, false]);
    }

    #[test]
    fn listing_failure_keeps_every_transcript() {
        let candidates = [candidate("/w/a", 60_000), candidate("/w/b", 600_000)];
        let flags = pi_alive_flags(&candidates, &PiProcessEvidence::Unavailable, NOW);
        assert_eq!(flags, vec![true, true]);
    }

    #[test]
    fn grace_window_keeps_a_just_written_transcript() {
        let candidates = [
            candidate("/w/a", PI_PROCESS_GRACE_MS - 1),
            candidate("/w/a", PI_PROCESS_GRACE_MS),
        ];
        let flags = pi_alive_flags(&candidates, &live(&[]), NOW);
        assert_eq!(flags, vec![true, false]);
    }

    #[test]
    fn transcript_without_exact_cwd_is_never_ended() {
        let candidates = [PiLivenessCandidate {
            cwd: None,
            modified_ms: NOW - 600_000,
        }];
        assert_eq!(pi_alive_flags(&candidates, &live(&[]), NOW), vec![true]);
    }

    #[test]
    fn live_process_without_transcript_creates_nothing() {
        // A fresh `pi` with no prompt yet has a cwd but no transcript.
        let flags = pi_alive_flags(&[], &live(&[("/w/a", 1)]), NOW);
        assert!(flags.is_empty());
        let flags = pi_alive_flags(&[candidate("/w/b", 60_000)], &live(&[("/w/a", 1)]), NOW);
        assert_eq!(flags, vec![false]);
    }

    #[test]
    fn trailing_slash_does_not_split_a_cwd() {
        let flags = pi_alive_flags(&[candidate("/w/a/", 60_000)], &live(&[("/w/a", 1)]), NOW);
        assert_eq!(flags, vec![true]);
        let evidence = evidence_from_classes([pi("/w/a/")]);
        assert_eq!(evidence, live(&[("/w/a", 1)]));
    }

    #[test]
    fn restarted_process_does_not_revive_the_killed_session() {
        // Old transcript last written 5 min ago; its pi was killed and a new
        // pi started 1 min ago in the same cwd without a prompt yet.
        let candidates = [candidate("/w/a", 300_000)];
        let flags = pi_alive_flags(&candidates, &started("/w/a", &[60_000]), NOW);
        assert_eq!(flags, vec![false]);
        // Once the new pi writes its own transcript, only that one is live.
        let candidates = [candidate("/w/a", 300_000), candidate("/w/a", 20_000)];
        let flags = pi_alive_flags(&candidates, &started("/w/a", &[60_000]), NOW);
        assert_eq!(flags, vec![false, true]);
    }

    #[test]
    fn resumed_session_shows_ended_until_its_first_write() {
        // Known limit, locked deliberately: `pi -c` / `--resume` loads the old
        // transcript without writing to it (pi 0.87.1 `SessionManager`
        // `_setSessionFile` only loads entries; `createAgentSession` appends a
        // `thinking_level_change` on resume only when the file lacks one). Its
        // mtime then predates the new process, so the card stays ended.
        let candidates = [candidate("/w/a", 600_000)];
        let flags = pi_alive_flags(&candidates, &started("/w/a", &[30_000]), NOW);
        assert_eq!(flags, vec![false]);
        // The next write (a prompt, or any ledger line) moves the mtime past
        // the process start and the same transcript is live again.
        let candidates = [candidate("/w/a", 20_000)];
        let flags = pi_alive_flags(&candidates, &started("/w/a", &[30_000]), NOW);
        assert_eq!(flags, vec![true]);
    }

    #[test]
    fn start_time_grace_and_assignment_keep_every_ownable_transcript() {
        // A process started a few seconds after its transcript's last write
        // (whole-second start times) still owns it.
        let flags = pi_alive_flags(
            &[candidate("/w/a", 60_000)],
            &started("/w/a", &[60_000 - PI_PROCESS_GRACE_MS]),
            NOW,
        );
        assert_eq!(flags, vec![true]);
        // The newest transcript must not take the only process old enough
        // for the older transcript when a younger process also fits it.
        let candidates = [candidate("/w/a", 600_000), candidate("/w/a", 30_000)];
        let flags = pi_alive_flags(&candidates, &started("/w/a", &[3_600_000, 60_000]), NOW);
        assert_eq!(flags, vec![true, true]);
    }

    #[test]
    fn classifier_accepts_title_rewritten_node_and_startup_argv() {
        let rewritten = PiProcessRow {
            name: "node",
            argv: &["pi"],
            cwd: Some("/w/a"),
            started_ms: None,
            same_user: true,
        };
        assert_eq!(classify_pi_process(&rewritten), pi("/w/a"));
        let startup = PiProcessRow {
            name: "node",
            argv: &["/usr/local/bin/node", "/opt/homebrew/bin/pi", "-c"],
            cwd: Some("/w/a"),
            started_ms: None,
            same_user: true,
        };
        assert_eq!(classify_pi_process(&startup), pi("/w/a"));
        let named = PiProcessRow {
            name: "pi",
            argv: &[],
            cwd: Some("/w/a"),
            started_ms: None,
            same_user: true,
        };
        assert_eq!(classify_pi_process(&named), pi("/w/a"));
        let bun = PiProcessRow {
            name: "bun",
            argv: &["pi"],
            cwd: Some("/w/a"),
            started_ms: None,
            same_user: true,
        };
        assert_eq!(classify_pi_process(&bun), pi("/w/a"));
    }

    #[test]
    fn classifier_rejects_other_processes() {
        let server = PiProcessRow {
            name: "node",
            argv: &["node", "server.js"],
            cwd: Some("/w/a"),
            started_ms: None,
            same_user: true,
        };
        assert_eq!(classify_pi_process(&server), PiProcessClass::NotPi);
        let shell = PiProcessRow {
            name: "zsh",
            argv: &["pi"],
            cwd: Some("/w/a"),
            started_ms: None,
            same_user: true,
        };
        assert_eq!(classify_pi_process(&shell), PiProcessClass::NotPi);
        let foreign = PiProcessRow {
            name: "node",
            argv: &[],
            cwd: None,
            started_ms: None,
            same_user: false,
        };
        assert_eq!(classify_pi_process(&foreign), PiProcessClass::NotPi);
    }

    #[test]
    fn unreadable_cwd_or_argv_makes_the_snapshot_uncertain() {
        let no_cwd = classify_pi_process(&PiProcessRow {
            name: "node",
            argv: &["pi"],
            cwd: None,
            started_ms: None,
            same_user: true,
        });
        assert_eq!(
            no_cwd,
            PiProcessClass::Pi {
                cwd: None,
                started_ms: None
            }
        );
        assert_eq!(
            evidence_from_classes([pi("/w/a"), no_cwd]),
            PiProcessEvidence::Unavailable
        );
        let no_argv = classify_pi_process(&PiProcessRow {
            name: "node",
            argv: &[],
            cwd: Some("/w/a"),
            started_ms: None,
            same_user: true,
        });
        assert_eq!(no_argv, PiProcessClass::Unknown);
        assert_eq!(
            evidence_from_classes([no_argv]),
            PiProcessEvidence::Unavailable
        );
    }
}
