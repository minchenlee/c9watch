//! Claude Code sessions that hide themselves from every registry c9watch reads.
//!
//! An app launched by a Claude Code process (with `open` or `osascript`)
//! inherits `CLAUDE_CODE_CHILD_SESSION=1`. Every `claude` started inside that
//! app treats itself as a child session: it writes only
//! `~/.claude/sessions/<pid>.<hash>.key`, no `<pid>.json` and no transcript,
//! so `claude agents --json` doesn't list it and c9watch has nothing to show.
//! c9watch can't show these sessions; it can only count them and say why.

use super::detector_cli::StartTimes;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Environment variable Claude Code sets in everything it launches.
const CHILD_SESSION_VAR: &str = "CLAUDE_CODE_CHILD_SESSION";
/// Pid of the Claude Code process that launched the app.
const PARENT_PID_VAR: &str = "CLAUDE_PID";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum HiddenReason {
    /// The process inherited `CLAUDE_CODE_CHILD_SESSION`, so Claude Code
    /// turned off its registry entry and transcript.
    ChildSessionMarker,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HiddenClaudeSession {
    pub pid: u32,
    pub cwd: Option<String>,
    pub reason: HiddenReason,
    /// `CLAUDE_PID` from the hidden process: the Claude Code process that
    /// opened the app it runs in, when it's set.
    pub parent_claude_pid: Option<u32>,
}

/// What c9watch needs to know about one running process.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ProcessFacts {
    pub exe: Option<PathBuf>,
    pub argv0: Option<String>,
    pub cwd: Option<PathBuf>,
    /// The process belongs to the user c9watch runs as.
    pub same_user: bool,
    pub child_marker: bool,
    pub parent_claude_pid: Option<u32>,
}

impl ProcessFacts {
    /// Fills the environment-derived fields from `KEY=VALUE` entries.
    pub(crate) fn with_env(mut self, environ: &[OsString]) -> Self {
        for entry in environ {
            let Some(entry) = entry.to_str() else {
                continue;
            };
            let Some((key, value)) = entry.split_once('=') else {
                continue;
            };
            match key {
                CHILD_SESSION_VAR => self.child_marker = !value.is_empty() && value != "0",
                PARENT_PID_VAR => self.parent_claude_pid = value.parse().ok(),
                _ => {}
            }
        }
        self
    }
}

/// Pids with a `<pid>.<hash>.key` file but no `<pid>.json` in the session
/// registry, sorted.
pub(crate) fn pids_without_registry_entry<S: AsRef<str>>(file_names: &[S]) -> Vec<u32> {
    let mut keyed = BTreeSet::new();
    let mut registered = BTreeSet::new();
    for name in file_names {
        let name = name.as_ref();
        if let Some(pid) = name.strip_suffix(".json") {
            if let Ok(pid) = pid.parse::<u32>() {
                registered.insert(pid);
            }
        } else if let Some(stem) = name.strip_suffix(".key") {
            let pid = stem.split_once('.').map_or(stem, |(pid, _)| pid);
            if let Ok(pid) = pid.parse::<u32>() {
                keyed.insert(pid);
            }
        }
    }
    keyed.difference(&registered).copied().collect()
}

/// Whether the process is the real Claude Code binary, not a script that
/// happens to be named `claude`.
fn is_claude_code(facts: &ProcessFacts, home: &Path) -> bool {
    let versions = home.join(".local/share/claude/versions");
    let exe_is_claude = facts
        .exe
        .as_deref()
        .is_some_and(|exe| exe.starts_with(&versions));
    let argv0_is_claude = facts
        .argv0
        .as_deref()
        .and_then(|argv0| Path::new(argv0).file_name())
        .is_some_and(|name| name == "claude");
    exe_is_claude || argv0_is_claude
}

/// The live Claude Code sessions the registry hides because of the
/// child-session marker. `processes` holds the live processes by pid; a pid
/// missing from it is treated as dead.
pub(crate) fn hidden_sessions<S: AsRef<str>>(
    file_names: &[S],
    processes: &HashMap<u32, ProcessFacts>,
    home: &Path,
) -> Vec<HiddenClaudeSession> {
    pids_without_registry_entry(file_names)
        .into_iter()
        .filter_map(|pid| {
            let facts = processes.get(&pid)?;
            (facts.same_user && facts.child_marker && is_claude_code(facts, home)).then(|| {
                HiddenClaudeSession {
                    pid,
                    cwd: facts.cwd.as_ref().map(|cwd| cwd.display().to_string()),
                    reason: HiddenReason::ChildSessionMarker,
                    parent_claude_pid: facts.parent_claude_pid,
                }
            })
        })
        .collect()
}

pub(crate) type FactsReader = fn(&[u32]) -> HashMap<u32, ProcessFacts>;

/// Runs [`hidden_sessions`] on the real registry and process table. Reading
/// another process's environment is the costly part, so it happens only for
/// pids that have a `.key` file and no `.json`, and the result is kept until
/// that pid exits or is reused.
pub(crate) struct HiddenSessionScanner {
    cache: HashMap<u32, (u64, Option<ProcessFacts>)>,
    start_times: fn(&[u32]) -> StartTimes,
    read_facts: FactsReader,
}

impl HiddenSessionScanner {
    pub(crate) fn new(start_times: fn(&[u32]) -> StartTimes) -> Self {
        Self::with_reader(start_times, read_process_facts)
    }

    pub(crate) fn with_reader(
        start_times: fn(&[u32]) -> StartTimes,
        read_facts: FactsReader,
    ) -> Self {
        Self {
            cache: HashMap::new(),
            start_times,
            read_facts,
        }
    }

    pub(crate) fn scan(&mut self, registry: &Path, home: &Path) -> Vec<HiddenClaudeSession> {
        let names: Vec<String> = std::fs::read_dir(registry)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect();
        let candidates = pids_without_registry_entry(&names);
        if candidates.is_empty() {
            self.cache.clear();
            return Vec::new();
        }

        let started = (self.start_times)(&candidates);
        self.cache
            .retain(|pid, (start, _)| started.get(pid) == Some(start));
        let unknown: Vec<u32> = started
            .keys()
            .copied()
            .filter(|pid| !self.cache.contains_key(pid))
            .collect();
        if !unknown.is_empty() {
            let mut read = (self.read_facts)(&unknown);
            for pid in unknown {
                self.cache.insert(pid, (started[&pid], read.remove(&pid)));
            }
        }

        let processes: HashMap<u32, ProcessFacts> = self
            .cache
            .iter()
            .filter_map(|(pid, (_, facts))| Some((*pid, facts.clone()?)))
            .collect();
        hidden_sessions(&names, &processes, home)
    }
}

#[cfg(unix)]
fn read_process_facts(pids: &[u32]) -> HashMap<u32, ProcessFacts> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let pids: Vec<Pid> = pids.iter().map(|&pid| Pid::from_u32(pid)).collect();
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&pids),
        true,
        ProcessRefreshKind::new()
            .with_exe(UpdateKind::Always)
            .with_cmd(UpdateKind::Always)
            .with_cwd(UpdateKind::Always)
            .with_environ(UpdateKind::Always)
            .with_user(UpdateKind::Always),
    );
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    pids.iter()
        .filter_map(|&pid| {
            let process = system.process(pid)?;
            let facts = ProcessFacts {
                exe: process.exe().map(Path::to_path_buf),
                argv0: process
                    .cmd()
                    .first()
                    .map(|arg| arg.to_string_lossy().into_owned()),
                cwd: process.cwd().map(Path::to_path_buf),
                same_user: process.user_id().is_some_and(|owner| **owner == uid),
                ..ProcessFacts::default()
            }
            .with_env(process.environ());
            Some((pid.as_u32(), facts))
        })
        .collect()
}

#[cfg(not(unix))]
fn read_process_facts(_pids: &[u32]) -> HashMap<u32, ProcessFacts> {
    HashMap::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const HOME: &str = "/Users/me";

    fn claude(cwd: &str) -> ProcessFacts {
        ProcessFacts {
            exe: Some(PathBuf::from(
                "/Users/me/.local/share/claude/versions/2.1.300",
            )),
            argv0: Some("claude".into()),
            cwd: Some(PathBuf::from(cwd)),
            same_user: true,
            child_marker: true,
            parent_claude_pid: Some(7),
        }
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn key_without_json_is_a_candidate() {
        let files = names(&[
            "10.aa.key",
            "10.json",
            "20.bb.key",
            "30.json",
            "x.key",
            "notes.txt",
        ]);
        assert_eq!(pids_without_registry_entry(&files), vec![20]);
    }

    #[test]
    fn env_sets_marker_and_parent_pid() {
        let env: Vec<OsString> = [
            "PATH=/bin",
            "CLAUDE_CODE_CHILD_SESSION=1",
            "CLAUDE_PID=4242",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        let facts = ProcessFacts::default().with_env(&env);
        assert!(facts.child_marker);
        assert_eq!(facts.parent_claude_pid, Some(4242));

        let off: Vec<OsString> = vec!["CLAUDE_CODE_CHILD_SESSION=0".into()];
        assert!(!ProcessFacts::default().with_env(&off).child_marker);
    }

    #[test]
    fn child_marker_session_is_reported() {
        let files = names(&["15535.ab.key", "47746.cd.key", "47746.json"]);
        let procs = HashMap::from([(15535, claude("/Users/me/spark")), (47746, claude("/x"))]);
        assert_eq!(
            hidden_sessions(&files, &procs, Path::new(HOME)),
            vec![HiddenClaudeSession {
                pid: 15535,
                cwd: Some("/Users/me/spark".into()),
                reason: HiddenReason::ChildSessionMarker,
                parent_claude_pid: Some(7),
            }]
        );
    }

    #[test]
    fn key_only_without_marker_is_not_reported() {
        // Like pid 67034: a .key and no .json, but no child-session marker.
        let files = names(&["67034.ef.key"]);
        let procs = HashMap::from([(
            67034,
            ProcessFacts {
                child_marker: false,
                parent_claude_pid: None,
                ..claude("/x")
            },
        )]);
        assert!(hidden_sessions(&files, &procs, Path::new(HOME)).is_empty());
    }

    #[test]
    fn fake_claude_script_is_not_reported() {
        // A demo script named `claude` (like flockbook-demo's) writes no .key,
        // and even with a stray .key its exe is an interpreter, not Claude Code.
        let fake = ProcessFacts {
            exe: Some(PathBuf::from("/bin/bash")),
            argv0: Some("/bin/bash".into()),
            ..claude("/demo")
        };
        let procs = HashMap::from([(500, fake.clone()), (501, fake)]);
        assert!(hidden_sessions(&names(&[]), &procs, Path::new(HOME)).is_empty());
        assert!(hidden_sessions(&names(&["501.zz.key"]), &procs, Path::new(HOME)).is_empty());
    }

    #[test]
    fn argv0_claude_counts_without_versions_exe() {
        let procs = HashMap::from([(
            9,
            ProcessFacts {
                exe: Some(PathBuf::from("/opt/homebrew/bin/node")),
                argv0: Some("/opt/homebrew/bin/claude".into()),
                ..claude("/x")
            },
        )]);
        assert_eq!(
            hidden_sessions(&names(&["9.k.key"]), &procs, Path::new(HOME)).len(),
            1
        );
    }

    #[test]
    fn dead_pid_and_other_user_are_not_reported() {
        let files = names(&["1.a.key", "2.b.key"]);
        let other = ProcessFacts {
            same_user: false,
            ..claude("/x")
        };
        let procs = HashMap::from([(2, other)]);
        assert!(hidden_sessions(&files, &procs, Path::new(HOME)).is_empty());
    }

    static READS: AtomicUsize = AtomicUsize::new(0);

    fn fake_start_times(pids: &[u32]) -> StartTimes {
        pids.iter()
            .filter(|&&p| p != 3)
            .map(|&p| (p, 100))
            .collect()
    }

    fn fake_facts(pids: &[u32]) -> HashMap<u32, ProcessFacts> {
        READS.fetch_add(pids.len(), Ordering::SeqCst);
        pids.iter().map(|&p| (p, claude("/w"))).collect()
    }

    #[test]
    fn scanner_reads_env_once_per_live_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        for name in ["1.a.key", "2.b.key", "2.json", "3.c.key", "4.d.key"] {
            std::fs::write(tmp.path().join(name), "").unwrap();
        }
        let mut scanner = HiddenSessionScanner::with_reader(fake_start_times, fake_facts);
        let first: Vec<u32> = scanner
            .scan(tmp.path(), Path::new(HOME))
            .iter()
            .map(|s| s.pid)
            .collect();
        // 2 has a .json; 3 is dead.
        assert_eq!(first, vec![1, 4]);
        assert_eq!(READS.load(Ordering::SeqCst), 2);

        scanner.scan(tmp.path(), Path::new(HOME));
        assert_eq!(
            READS.load(Ordering::SeqCst),
            2,
            "second scan uses the cache"
        );

        std::fs::remove_file(tmp.path().join("4.d.key")).unwrap();
        let after: Vec<u32> = scanner
            .scan(tmp.path(), Path::new(HOME))
            .iter()
            .map(|s| s.pid)
            .collect();
        assert_eq!(after, vec![1]);
    }

    /// Measures one cold and one warm scan of the real registry.
    /// `cargo test hidden_sessions::tests::scan_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn scan_cost() {
        let home = dirs::home_dir().unwrap();
        let registry = home.join(".claude/sessions");
        let mut scanner =
            HiddenSessionScanner::new(super::super::detector_cli::process_start_times);
        let t = std::time::Instant::now();
        let cold = scanner.scan(&registry, &home);
        let cold_time = t.elapsed();
        let t = std::time::Instant::now();
        let warm = scanner.scan(&registry, &home);
        let warm_time = t.elapsed();
        println!(
            "cold {cold_time:?} ({} hidden), warm {warm_time:?} ({} hidden)",
            cold.len(),
            warm.len()
        );
        println!("{}", serde_json::to_string_pretty(&warm).unwrap());
    }
}
