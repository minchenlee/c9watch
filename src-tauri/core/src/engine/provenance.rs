//! Durable provenance (D34). A process that double-forks is moved under
//! launchd or init, and its ancestry then shows no agent or session: read
//! from the live process table alone, it would be `local-unverified`.
//! Core therefore remembers, for each process it has seen descend from an
//! agent's PTY child, an observed session or an agent-team teammate, where
//! it came from, and keeps that after the process is reparented.
//!
//! A process is keyed by pid **and** start time, so a reused pid inherits
//! nothing. The table is rebuilt from the process list on every scan:
//! entries of processes that exited are dropped, so it holds at most the
//! live processes, and never more than `MAX_TRACKED`. When it is full, a
//! root or descendant that would have been added is kept out but marked
//! denied, and so are its descendants: they have an `Unknown` origin,
//! which fails closed. A process that descends from no root (the owner's
//! terminal, the GUI) has no origin at all, full table or not.
//!
//! Residual gap: a process that reparents before core sees it (between two
//! scans, or before core started) has no entry and is judged by its live
//! ancestry, as before.

use super::peer::{ProcEntry, ProcInfo};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

/// How often the engine scans the process table.
pub const SCAN: Duration = Duration::from_secs(1);

/// The most processes the table tracks.
pub const MAX_TRACKED: usize = 65_536;

/// Where a process came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// The PTY child of this agent's generation, or a descendant of it.
    Agent { id: String, gen: i64 },
    /// The process of the observed session with this key, or a descendant.
    Session { key: String },
    /// A process whose argv is an agent-team teammate's, or a descendant.
    Teammate { pid: libc::pid_t, start: u64 },
    /// Not known: the table was full when the process (or an ancestor it
    /// inherits from) should have been added.
    Unknown,
}

/// The roots of one scan: the PTY children of live agents, and the
/// processes of observed sessions as detection lists them (no start time).
#[derive(Debug, Default)]
pub struct Roots {
    pub agents: Vec<(ProcInfo, String, i64)>,
    pub sessions: Vec<(libc::pid_t, String)>,
}

#[derive(Default)]
struct Table {
    origins: HashMap<(libc::pid_t, u64), Origin>,
    /// Processes whose argv was read and is not a teammate's.
    checked: HashSet<(libc::pid_t, u64)>,
    /// Processes kept out because the table was full. A subset of the live
    /// processes, so bounded by them.
    denied: HashSet<(libc::pid_t, u64)>,
}

/// The provenance table.
#[derive(Default)]
pub struct Provenance(Mutex<Table>);

impl Provenance {
    pub fn new() -> Self {
        Self::default()
    }

    /// Where the process `pid` that started at `start` came from. `None`
    /// when it descends from nothing core watches.
    pub fn origin(&self, pid: libc::pid_t, start: u64) -> Option<Origin> {
        let t = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match t.origins.get(&(pid, start)) {
            Some(o) => Some(o.clone()),
            None if t.denied.contains(&(pid, start)) => Some(Origin::Unknown),
            None => None,
        }
    }

    /// Rebuild the table from the process list `all`. Roots take their
    /// origin; a process already in the table keeps the origin it had; a
    /// new process whose parent has an origin, and did not start after it,
    /// inherits it. `is_teammate` reads a new process's argv: `None` when
    /// it can't be read yet, which is tried again on the next scan.
    ///
    /// `all` may be partial (a failed listing or a pid it could not read),
    /// so a tracked process missing from it is dropped only when `exited`
    /// confirms that pid is gone or now runs another process
    /// (`peer::exited`). Otherwise it is kept.
    pub fn scan(
        &self,
        all: &[ProcEntry],
        roots: &Roots,
        is_teammate: impl Fn(&ProcInfo) -> Option<bool>,
        exited: impl Fn(libc::pid_t, u64) -> bool,
    ) {
        let by_pid: HashMap<libc::pid_t, &ProcInfo> =
            all.iter().map(|e| (e.info.pid, &e.info)).collect();
        let mut t = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let old = std::mem::take(&mut t.origins);
        let old_checked = std::mem::take(&mut t.checked);
        let old_denied = std::mem::take(&mut t.denied);
        let alive = |key: &(libc::pid_t, u64)| by_pid.get(&key.0).is_some_and(|p| p.start == key.1);
        let kept = |key: &(libc::pid_t, u64)| match by_pid.get(&key.0) {
            Some(p) => p.start == key.1,
            None => !exited(key.0, key.1),
        };
        let mut next: HashMap<(libc::pid_t, u64), Origin> = HashMap::new();
        let mut denied: HashSet<(libc::pid_t, u64)> = old_denied.into_iter().filter(kept).collect();
        // Add `key` (a root replaces what it had); past the cap it is
        // denied instead. Whether it is now tracked or denied.
        let put = |next: &mut HashMap<_, _>,
                   denied: &mut HashSet<_>,
                   key: (libc::pid_t, u64),
                   origin: Origin,
                   root: bool| {
            if next.contains_key(&key) {
                if root {
                    next.insert(key, origin);
                }
                return false;
            }
            if next.len() >= MAX_TRACKED {
                return denied.insert(key);
            }
            denied.remove(&key);
            next.insert(key, origin);
            true
        };
        for (p, id, gen) in &roots.agents {
            if alive(&(p.pid, p.start)) {
                let origin = Origin::Agent {
                    id: id.clone(),
                    gen: *gen,
                };
                put(&mut next, &mut denied, (p.pid, p.start), origin, true);
            }
        }
        for (pid, key) in &roots.sessions {
            if let Some(p) = by_pid.get(pid) {
                if !next.contains_key(&(p.pid, p.start)) {
                    let origin = Origin::Session { key: key.clone() };
                    put(&mut next, &mut denied, (p.pid, p.start), origin, true);
                }
            }
        }
        for (key, origin) in old {
            if kept(&key) {
                put(&mut next, &mut denied, key, origin, false);
            }
        }
        let mut checked = HashSet::new();
        for p in by_pid.values() {
            let key = (p.pid, p.start);
            if next.contains_key(&key) || denied.contains(&key) {
                continue;
            }
            if old_checked.contains(&key) {
                checked.insert(key);
                continue;
            }
            match is_teammate(p) {
                Some(true) => {
                    put(
                        &mut next,
                        &mut denied,
                        key,
                        Origin::Teammate {
                            pid: p.pid,
                            start: p.start,
                        },
                        false,
                    );
                }
                Some(false) => {
                    checked.insert(key);
                }
                None => {}
            }
        }
        let mut children: HashMap<libc::pid_t, Vec<&ProcInfo>> = HashMap::new();
        for p in by_pid.values() {
            children.entry(p.ppid).or_default().push(p);
        }
        // A denied process's descendants are denied too.
        let mut frontier: Vec<((libc::pid_t, u64), Origin)> = next
            .iter()
            .map(|(k, o)| (*k, o.clone()))
            .chain(denied.iter().map(|k| (*k, Origin::Unknown)))
            .collect();
        while let Some(((pid, start), origin)) = frontier.pop() {
            for child in children.get(&pid).into_iter().flatten() {
                let key = (child.pid, child.start);
                if child.pid == pid || child.start < start || denied.contains(&key) {
                    continue;
                }
                let added = match origin {
                    Origin::Unknown => !next.contains_key(&key) && denied.insert(key),
                    _ => put(&mut next, &mut denied, key, origin.clone(), false),
                };
                if added {
                    checked.remove(&key);
                    let inherited = next.get(&key).cloned().unwrap_or(Origin::Unknown);
                    frontier.push((key, inherited));
                }
            }
        }
        t.origins = next;
        t.checked = checked;
        t.denied = denied;
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.0.lock().unwrap().origins.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(pid: libc::pid_t, ppid: libc::pid_t, start: u64) -> ProcEntry {
        ProcEntry {
            info: ProcInfo { pid, ppid, start },
            pgid: pid,
            zombie: false,
        }
    }

    fn session(pid: libc::pid_t, key: &str) -> Roots {
        Roots {
            sessions: vec![(pid, key.into())],
            ..Roots::default()
        }
    }

    /// Every pid missing from a list has exited.
    fn gone(_: libc::pid_t, _: u64) -> bool {
        true
    }

    fn not_teammate(_: &ProcInfo) -> Option<bool> {
        Some(false)
    }

    fn key(k: &str) -> Option<Origin> {
        Some(Origin::Session { key: k.into() })
    }

    /// A double-forked process keeps the session it came from after it is
    /// moved under init, and so do its later children.
    #[test]
    fn a_reparented_process_keeps_its_origin() {
        let p = Provenance::new();
        // session 100 -> shell 200 -> daemon 300
        let before = [
            entry(1, 0, 1),
            entry(100, 1, 10),
            entry(200, 100, 20),
            entry(300, 200, 30),
        ];
        p.scan(&before, &session(100, "claudeCode:s1"), not_teammate, gone);
        assert_eq!(p.origin(300, 30), key("claudeCode:s1"));
        assert_eq!(p.origin(1, 1), None);
        // The shell exits; the daemon is moved under init and forks.
        let after = [
            entry(1, 0, 1),
            entry(100, 1, 10),
            entry(300, 1, 30),
            entry(400, 300, 40),
        ];
        p.scan(&after, &session(100, "claudeCode:s1"), not_teammate, gone);
        assert_eq!(p.origin(300, 30), key("claudeCode:s1"));
        assert_eq!(p.origin(400, 40), key("claudeCode:s1"));
        // The session ends and detection no longer lists it: kept.
        let ended = [entry(1, 0, 1), entry(300, 1, 30)];
        p.scan(&ended, &Roots::default(), not_teammate, gone);
        assert_eq!(p.origin(300, 30), key("claudeCode:s1"));
    }

    /// A reused pid is another process: it inherits nothing, and an exited
    /// process's entry is dropped.
    #[test]
    fn a_reused_pid_inherits_nothing() {
        let p = Provenance::new();
        let before = [entry(1, 0, 1), entry(100, 1, 10), entry(300, 100, 30)];
        p.scan(&before, &session(100, "claudeCode:s1"), not_teammate, gone);
        assert_eq!(p.origin(300, 30), key("claudeCode:s1"));
        // 300 exits and its pid is given to a new process under init.
        let after = [entry(1, 0, 1), entry(100, 1, 10), entry(300, 1, 50)];
        p.scan(&after, &session(100, "claudeCode:s1"), not_teammate, gone);
        assert_eq!(p.origin(300, 50), None);
        assert_eq!(p.origin(300, 30), None);
        assert_eq!(p.len(), 1);
        // A "child" that started before its parent holds a reused ppid.
        let odd = [entry(1, 0, 1), entry(100, 1, 10), entry(500, 100, 5)];
        p.scan(&odd, &session(100, "claudeCode:s1"), not_teammate, gone);
        assert_eq!(p.origin(500, 5), None);
    }

    /// An agent's PTY child counts only with the start time the engine
    /// recorded; a teammate is found by argv, once per process.
    #[test]
    fn agent_and_teammate_roots() {
        let p = Provenance::new();
        let all = [
            entry(1, 0, 1),
            entry(100, 1, 10),
            entry(110, 100, 11),
            entry(200, 1, 20),
            entry(210, 200, 21),
        ];
        let roots = Roots {
            agents: vec![(
                ProcInfo {
                    pid: 100,
                    ppid: 1,
                    start: 10,
                },
                "a1".into(),
                3,
            )],
            ..Roots::default()
        };
        let reads = std::cell::Cell::new(0);
        let teammate = |q: &ProcInfo| {
            reads.set(reads.get() + 1);
            Some(q.pid == 200)
        };
        p.scan(&all, &roots, teammate, gone);
        let agent = Some(Origin::Agent {
            id: "a1".into(),
            gen: 3,
        });
        assert_eq!(p.origin(110, 11), agent);
        assert_eq!(
            p.origin(210, 21),
            Some(Origin::Teammate {
                pid: 200,
                start: 20
            })
        );
        let first = reads.get();
        p.scan(&all, &roots, teammate, gone);
        assert_eq!(reads.get(), first, "argv is read once per process");
        // A PTY child whose start time differs is not the agent's.
        let stale = Roots {
            agents: vec![(
                ProcInfo {
                    pid: 100,
                    ppid: 1,
                    start: 99,
                },
                "a1".into(),
                3,
            )],
            ..Roots::default()
        };
        let q = Provenance::new();
        q.scan(&all, &stale, not_teammate, gone);
        assert_eq!(q.origin(110, 11), None);
    }

    /// A process whose argv can't be read yet is read again next scan.
    #[test]
    fn an_unreadable_argv_is_read_again() {
        let p = Provenance::new();
        let all = [entry(1, 0, 1), entry(200, 1, 20)];
        p.scan(&all, &Roots::default(), |_| None, gone);
        assert_eq!(p.origin(200, 20), None);
        p.scan(&all, &Roots::default(), |q| Some(q.pid == 200), gone);
        assert_eq!(
            p.origin(200, 20),
            Some(Origin::Teammate {
                pid: 200,
                start: 20
            })
        );
    }

    /// Past `MAX_TRACKED`, a process that should have been added, and its
    /// descendants, are `Unknown`; one under no root still has no origin.
    #[test]
    fn a_full_table_fails_closed_only_for_what_it_should_hold() {
        let p = Provenance::new();
        let n = MAX_TRACKED as libc::pid_t + 10;
        // Teammates 2..n under init; the last few don't fit. A shell (n+1)
        // under the last one, and a plain terminal (n+2) under init.
        let all: Vec<_> = [entry(1, 0, 1)]
            .into_iter()
            .chain((2..n).map(|pid| entry(pid, 1, 5)))
            .chain([entry(n + 1, n - 1, 6), entry(n + 2, 1, 6)])
            .collect();
        let teammate = |q: &ProcInfo| Some(q.pid > 1 && q.pid < n);
        p.scan(&all, &Roots::default(), teammate, gone);
        assert_eq!(p.len(), MAX_TRACKED);
        let denied = (2..n).find(|pid| p.origin(*pid, 5) == Some(Origin::Unknown));
        let denied = denied.expect("a teammate past the cap is denied");
        assert!(p.origin(n - 1, 5).is_some());
        assert_eq!(p.origin(n + 2, 6), None, "the plain terminal");
        assert_eq!(p.origin(1, 1), None);
        // A child of a denied process is denied too, on a later scan.
        let child = [entry(n + 3, denied, 7)];
        let more: Vec<_> = all.iter().cloned().chain(child).collect();
        p.scan(&more, &Roots::default(), teammate, gone);
        assert_eq!(p.origin(n + 3, 7), Some(Origin::Unknown));
        assert_eq!(p.origin(n + 2, 6), None);
    }

    /// A process launched from a plain terminal (no agent, session or
    /// teammate above it) has no origin.
    #[test]
    fn a_plain_terminal_process_has_no_origin() {
        let p = Provenance::new();
        // launchd -> terminal 50 -> shell 60 -> c9watch 70; a session 100
        // elsewhere.
        let all = [
            entry(1, 0, 1),
            entry(50, 1, 5),
            entry(60, 50, 6),
            entry(70, 60, 7),
            entry(100, 1, 10),
        ];
        p.scan(&all, &session(100, "claudeCode:s1"), not_teammate, gone);
        assert_eq!(p.origin(70, 7), None);
        assert_eq!(p.origin(50, 5), None);
    }

    /// A failed or partial listing is not an exit: a tracked process missing
    /// from it is kept unless its pid is confirmed gone or reused.
    #[test]
    fn a_partial_listing_keeps_what_it_misses() {
        let p = Provenance::new();
        let before = [entry(1, 0, 1), entry(100, 1, 10), entry(300, 100, 30)];
        p.scan(&before, &session(100, "claudeCode:s1"), not_teammate, gone);
        let unsure = |_: libc::pid_t, _: u64| false;
        p.scan(&[], &Roots::default(), not_teammate, unsure);
        assert_eq!(p.origin(300, 30), key("claudeCode:s1"));
        p.scan(&[entry(1, 0, 1)], &Roots::default(), not_teammate, unsure);
        assert_eq!(p.origin(300, 30), key("claudeCode:s1"));
        assert_eq!(p.origin(100, 10), key("claudeCode:s1"));
        // Confirmed gone: dropped.
        p.scan(
            &[entry(1, 0, 1)],
            &Roots::default(),
            not_teammate,
            |pid, _| pid == 300,
        );
        assert_eq!(p.origin(300, 30), None);
        assert_eq!(p.origin(100, 10), key("claudeCode:s1"));
    }

    /// The same for a process the full table denied.
    #[test]
    fn a_partial_listing_keeps_denied_processes() {
        let p = Provenance::new();
        let n = MAX_TRACKED as libc::pid_t + 2;
        let all: Vec<_> = [entry(1, 0, 1)]
            .into_iter()
            .chain((2..=n).map(|pid| entry(pid, 1, 5)))
            .collect();
        let teammate = |q: &ProcInfo| Some(q.pid > 1);
        p.scan(&all, &Roots::default(), teammate, gone);
        let denied: Vec<_> = (2..=n)
            .filter(|pid| p.origin(*pid, 5) == Some(Origin::Unknown))
            .collect();
        assert!(!denied.is_empty());
        p.scan(&[], &Roots::default(), teammate, |_, _| false);
        for pid in denied {
            assert_eq!(p.origin(pid, 5), Some(Origin::Unknown));
        }
    }
}
