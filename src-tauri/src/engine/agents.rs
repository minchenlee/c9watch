//! The engine's live-agent table: one current generation per agent the
//! engine runs behind a PTY (C1a-33).
//!
//! A generation's identity is `(agent_id, gen, pty_child_pid,
//! pty_child_start)`. It lives here, in the engine, and nowhere in the
//! agent's env: no token proves who an agent is (D28 Q6=B). A peer is
//! `agent:<id>` only when its ancestry reaches the PTY child of the
//! **current** generation with the same start time (`engine::principal`),
//! so a process left over from an older generation, or a process that got a
//! reused pid, is `local-unverified`.
//!
//! Rows come in only through `register`, which takes the `ChildId` a
//! `Driver` returned for a child the engine spawned. Detection never writes
//! here, so an observed session is never an agent (A:186).
//!
//! The table is in memory. The `agent_state` table holds the same facts for
//! boot reconcile, which the ops journal fills (M5).

use super::driver::ChildId;
use super::principal::{LiveAgent, LiveAgents};
use std::collections::HashMap;
use std::sync::RwLock;

/// One agent's current generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Generation {
    pub gen: i64,
    pub pty_child: ChildId,
}

/// The live agents, by agent id. Few rows (one per live agent), so the
/// lookup by PTY child pid scans them.
#[derive(Debug, Default)]
pub struct LiveTable {
    by_agent: RwLock<HashMap<String, Generation>>,
}

impl LiveTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Make `gen` with PTY child `child` the current generation of
    /// `agent_id`. It replaces an older generation, whose processes stop
    /// being the agent at once. Fails when `gen` is not newer than the
    /// current one, or when another live agent has the same PTY child.
    pub fn register(&self, agent_id: &str, gen: i64, child: ChildId) -> Result<(), String> {
        let mut map = self.by_agent.write().unwrap_or_else(|e| e.into_inner());
        if let Some(cur) = map.get(agent_id) {
            if gen <= cur.gen {
                return Err(format!(
                    "generation {} of {} is not newer than the live generation {}",
                    gen, agent_id, cur.gen
                ));
            }
        }
        if let Some((other, _)) = map
            .iter()
            .find(|(id, g)| id.as_str() != agent_id && g.pty_child.pid == child.pid)
        {
            return Err(format!(
                "pid {} is already the PTY child of {}",
                child.pid, other
            ));
        }
        map.insert(
            agent_id.to_string(),
            Generation {
                gen,
                pty_child: child,
            },
        );
        Ok(())
    }

    /// Drop `agent_id` if `gen` is still its current generation. Returns
    /// whether a row went away. A late call for an older generation leaves
    /// the newer one alone.
    pub fn unregister(&self, agent_id: &str, gen: i64) -> bool {
        let mut map = self.by_agent.write().unwrap_or_else(|e| e.into_inner());
        match map.get(agent_id) {
            Some(cur) if cur.gen == gen => {
                map.remove(agent_id);
                true
            }
            _ => false,
        }
    }

    /// The current generation of `agent_id`.
    pub fn current(&self, agent_id: &str) -> Option<Generation> {
        let map = self.by_agent.read().unwrap_or_else(|e| e.into_inner());
        map.get(agent_id).copied()
    }

    /// How many agents are live.
    pub fn len(&self) -> usize {
        let map = self.by_agent.read().unwrap_or_else(|e| e.into_inner());
        map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl LiveAgents for LiveTable {
    fn by_pty_child(&self, pid: libc::pid_t) -> Option<LiveAgent> {
        let map = self.by_agent.read().unwrap_or_else(|e| e.into_inner());
        map.iter()
            .find(|(_, g)| g.pty_child.pid == pid)
            .map(|(id, g)| LiveAgent {
                id: id.clone(),
                gen: g.gen,
                pty_child_pid: g.pty_child.pid,
                pty_child_start: g.pty_child.start,
            })
    }

    fn current_gen(&self, agent_id: &str) -> Option<i64> {
        self.current(agent_id).map(|g| g.gen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::driver::{Driver, SpawnSpec};
    use crate::engine::peer;
    use crate::engine::principal::{stamp_peer, Principal};
    use crate::engine::pty::NativePty;
    use std::time::{Duration, Instant};

    fn child(pid: libc::pid_t, start: u64) -> ChildId {
        ChildId { pid, start }
    }

    fn agent(id: &str, gen: i64) -> Principal {
        Principal::Agent { id: id.into(), gen }
    }

    /// A PTY child (`/bin/sh`) with one direct child (`D=<pid>`, as a hook
    /// is) and one grandchild through a second shell (`G=<pid>`, as a Bash
    /// tool call is, D:252). Returns the PTY and both pids.
    fn agent_tree() -> (NativePty, libc::pid_t, libc::pid_t) {
        let script = "sleep 30 & echo \"D=$!.\"; \
                      /bin/sh -c 'sleep 30 & echo \"G=$!.\"; wait' & wait";
        let spec = SpawnSpec::new(
            vec!["/bin/sh".into(), "-c".into(), script.into()],
            std::env::temp_dir(),
        );
        let mut pty = NativePty::spawn(&spec).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = String::new();
        loop {
            let out = pty.output(Duration::from_millis(100));
            seen.push_str(&String::from_utf8_lossy(&out.bytes));
            if let (Some(d), Some(g)) = (pid_after(&seen, "D="), pid_after(&seen, "G=")) {
                return (pty, d, g);
            }
            assert!(!out.eof, "EOF; got {:?}", seen);
            assert!(Instant::now() < deadline, "no pids; got {:?}", seen);
        }
    }

    /// The pid printed as `<marker><pid>.`.
    fn pid_after(text: &str, marker: &str) -> Option<libc::pid_t> {
        let rest = &text[text.find(marker)? + marker.len()..];
        rest[..rest.find('.')?].parse().ok()
    }

    fn stamp(pid: libc::pid_t, table: &LiveTable) -> Principal {
        stamp_peer(pid, peer::start_token_now(), table)
    }

    #[test]
    fn hook_as_direct_child_and_bash_as_grandchild_are_the_agent() {
        let (mut pty, hook, bash) = agent_tree();
        let table = LiveTable::new();
        table.register("a1", 3, pty.child()).unwrap();
        // The hook's parent is the PTY child; the Bash tool's grandparent is.
        assert_eq!(peer::proc_info(hook).unwrap().ppid, pty.child().pid);
        let shell = peer::proc_info(bash).unwrap().ppid;
        assert_eq!(peer::proc_info(shell).unwrap().ppid, pty.child().pid);
        assert_eq!(stamp(hook, &table), agent("a1", 3));
        assert_eq!(stamp(bash, &table), agent("a1", 3));
        pty.kill().unwrap();
    }

    #[test]
    fn a_reused_pid_is_local_unverified() {
        let (mut pty, hook, bash) = agent_tree();
        let id = pty.child();
        // The table holds the same pid with another start time: the live
        // process got the pid of a PTY child that is gone.
        let table = LiveTable::new();
        table
            .register("a1", 1, child(id.pid, id.start + 1))
            .unwrap();
        assert_eq!(stamp(hook, &table), Principal::LocalUnverified);
        assert_eq!(stamp(bash, &table), Principal::LocalUnverified);
        pty.kill().unwrap();
    }

    #[test]
    fn processes_of_an_older_generation_are_local_unverified() {
        let (mut old, old_hook, _) = agent_tree();
        let (mut new, new_hook, new_bash) = agent_tree();
        let table = LiveTable::new();
        table.register("a1", 1, old.child()).unwrap();
        assert_eq!(stamp(old_hook, &table), agent("a1", 1));
        table.register("a1", 2, new.child()).unwrap();
        assert_eq!(stamp(old_hook, &table), Principal::LocalUnverified);
        assert_eq!(stamp(new_hook, &table), agent("a1", 2));
        assert_eq!(stamp(new_bash, &table), agent("a1", 2));
        // A late unregister of the old generation keeps the new one.
        assert!(!table.unregister("a1", 1));
        assert_eq!(stamp(new_hook, &table), agent("a1", 2));
        assert!(table.unregister("a1", 2));
        assert_eq!(stamp(new_hook, &table), Principal::LocalUnverified);
        old.kill().unwrap();
        new.kill().unwrap();
    }

    #[test]
    fn an_observed_session_is_never_an_agent() {
        // A detected session is just a process the engine did not spawn:
        // pretend the test runner is one. Nothing is in the table, so its
        // descendants (this test process) are not an agent.
        let me = std::process::id() as libc::pid_t;
        let table = LiveTable::new();
        assert_eq!(stamp(me, &table), Principal::LocalUnverified);
        // An engine agent elsewhere changes nothing for it.
        let (mut pty, _, _) = agent_tree();
        table.register("a1", 1, pty.child()).unwrap();
        assert_eq!(stamp(me, &table), Principal::LocalUnverified);
        pty.kill().unwrap();
    }

    #[test]
    fn register_needs_a_newer_generation_and_an_unused_child() {
        let table = LiveTable::new();
        table.register("a1", 2, child(10, 100)).unwrap();
        assert!(table.register("a1", 2, child(11, 110)).is_err());
        assert!(table.register("a1", 1, child(11, 110)).is_err());
        assert!(table.register("a2", 1, child(10, 100)).is_err());
        table.register("a2", 1, child(12, 120)).unwrap();
        table.register("a1", 3, child(11, 110)).unwrap();
        assert_eq!(
            table.current("a1"),
            Some(Generation {
                gen: 3,
                pty_child: child(11, 110)
            })
        );
        // The old PTY child of a1 is free again.
        assert!(table.by_pty_child(10).is_none());
        assert_eq!(table.by_pty_child(11).unwrap().gen, 3);
        assert_eq!(table.len(), 2);
        assert!(table.unregister("a2", 1));
        assert!(!table.unregister("a2", 1));
        assert!(table.current("a2").is_none());
        assert!(!table.is_empty());
    }
}
