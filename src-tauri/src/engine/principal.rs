//! Principal stamping. The engine decides who a connection is from
//! connection facts only; what the client says in `hello` never counts.
//!
//! C1a has two principals:
//! 1. `agent:<id>`: the peer's ancestry (at most `MAX_ANCESTRY` levels)
//!    reaches the PTY child of a live agent, with the same pid **and** start
//!    time. The principal carries that agent's generation, as the engine
//!    recorded it at spawn (C1a-33). The live set is the engine's table in
//!    `engine::agents`, filled only from PTY children the engine spawned, so
//!    an observed session is never an agent (A:186).
//! 2. `local-unverified`: everything else (A:187).
//!
//! C1a never stamps `human` (that waits for the C1.5 code-signature check)
//! and never stamps `client:<id>` on the engine socket: `kind=gateway` in
//! `hello` is a claim any same-uid process can make (N1, N2).

use super::peer::{self, ProcInfo, MAX_ANCESTRY};
use serde::{Serialize, Serializer};

/// Who a connection is, as the engine stamped it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    /// A live agent and the generation whose PTY child the peer descends
    /// from.
    Agent {
        id: String,
        gen: i64,
    },
    LocalUnverified,
}

impl std::fmt::Display for Principal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Principal::Agent { id, .. } => write!(f, "agent:{}", id),
            Principal::LocalUnverified => f.write_str("local-unverified"),
        }
    }
}

impl Serialize for Principal {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

/// The PTY child of a live agent's current generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveAgent {
    pub id: String,
    pub gen: i64,
    pub pty_child_pid: libc::pid_t,
    pub pty_child_start: u64,
}

/// Lookup of live agents by the pid of their PTY child. The engine backs it
/// with its live table (`engine::agents::LiveTable`); `NoLiveAgents` serves
/// tests of the connection layer.
pub trait LiveAgents: Send + Sync {
    /// The live agent whose PTY child has `pid`, if any. The caller checks
    /// the start time.
    fn by_pty_child(&self, pid: libc::pid_t) -> Option<LiveAgent>;

    /// The current generation of `agent_id`, if it is live. A hook event
    /// stamped with another generation is stale (C1a-22).
    fn current_gen(&self, agent_id: &str) -> Option<i64>;
}

/// No agent is live. Every connection is `local-unverified`.
pub struct NoLiveAgents;

impl LiveAgents for NoLiveAgents {
    fn by_pty_child(&self, _pid: libc::pid_t) -> Option<LiveAgent> {
        None
    }

    fn current_gen(&self, _agent_id: &str) -> Option<i64> {
        None
    }
}

/// Stamp the principal for an ancestry chain (the peer first). The first
/// process in the chain that is a live agent's PTY child decides; a pid
/// whose start time differs was reused and does not count.
pub fn stamp(chain: &[ProcInfo], agents: &dyn LiveAgents) -> Principal {
    for p in chain.iter().take(MAX_ANCESTRY) {
        if let Some(agent) = agents.by_pty_child(p.pid) {
            if agent.pty_child_start == p.start {
                return Principal::Agent {
                    id: agent.id,
                    gen: agent.gen,
                };
            }
        }
    }
    Principal::LocalUnverified
}

/// Stamp the principal of the peer with `pid`. `accepted` is the start-time
/// token taken when the connection was accepted. A peer that started after
/// it holds a reused pid, not the process that connected, and is
/// `local-unverified`; so is every peer when the clock could not be read.
/// This does not catch a reuse between connect and accept.
pub fn stamp_peer(pid: libc::pid_t, accepted: Option<u64>, agents: &dyn LiveAgents) -> Principal {
    let chain = peer::ancestry(pid, MAX_ANCESTRY);
    match (chain.first(), accepted) {
        (Some(first), Some(at)) if first.start <= at => stamp(&chain, agents),
        _ => Principal::LocalUnverified,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A fixed set of live agents, keyed by PTY child pid.
    pub(crate) struct FakeAgents(pub HashMap<libc::pid_t, LiveAgent>);

    impl FakeAgents {
        pub(crate) fn one(id: &str, pid: libc::pid_t, start: u64) -> Self {
            Self(HashMap::from([(
                pid,
                LiveAgent {
                    id: id.into(),
                    gen: 1,
                    pty_child_pid: pid,
                    pty_child_start: start,
                },
            )]))
        }
    }

    impl LiveAgents for FakeAgents {
        fn by_pty_child(&self, pid: libc::pid_t) -> Option<LiveAgent> {
            self.0.get(&pid).cloned()
        }

        fn current_gen(&self, agent_id: &str) -> Option<i64> {
            self.0.values().find(|a| a.id == agent_id).map(|a| a.gen)
        }
    }

    fn agent(id: &str) -> Principal {
        Principal::Agent {
            id: id.into(),
            gen: 1,
        }
    }

    fn pi(pid: libc::pid_t, ppid: libc::pid_t, start: u64) -> ProcInfo {
        ProcInfo { pid, ppid, start }
    }

    /// bash tool (30) -> shell (20) -> claude, the PTY child (10) -> engine (5)
    fn bash_tool_chain(claude_start: u64) -> Vec<ProcInfo> {
        vec![
            pi(30, 20, 300),
            pi(20, 10, 200),
            pi(10, 5, claude_start),
            pi(5, 1, 50),
        ]
    }

    #[test]
    fn agent_bash_subprocess_is_the_agent() {
        let agents = FakeAgents::one("a1", 10, 100);
        assert_eq!(stamp(&bash_tool_chain(100), &agents), agent("a1"));
        assert_eq!(
            stamp(&bash_tool_chain(100), &agents).to_string(),
            "agent:a1"
        );
    }

    #[test]
    fn reused_pid_is_local_unverified() {
        let agents = FakeAgents::one("a1", 10, 100);
        assert_eq!(
            stamp(&bash_tool_chain(101), &agents),
            Principal::LocalUnverified
        );
    }

    #[test]
    fn no_live_agent_is_local_unverified() {
        assert_eq!(
            stamp(&bash_tool_chain(100), &NoLiveAgents),
            Principal::LocalUnverified
        );
        assert_eq!(
            stamp(&[], &FakeAgents::one("a1", 10, 100)),
            Principal::LocalUnverified
        );
    }

    #[test]
    fn match_beyond_the_level_limit_does_not_count() {
        // The PTY child sits at depth 10, one level past the limit.
        let chain: Vec<ProcInfo> = (0..12).map(|i| pi(100 + i, 101 + i, 7)).collect();
        assert_eq!(
            stamp(&chain, &FakeAgents::one("a1", 110, 7)),
            Principal::LocalUnverified
        );
        assert_eq!(stamp(&chain, &FakeAgents::one("a1", 109, 7)), agent("a1"));
    }

    #[test]
    fn real_ancestry_of_the_test_process() {
        let me = std::process::id() as libc::pid_t;
        let parent = peer::proc_info(me).unwrap().ppid;
        let pstart = peer::proc_info(parent).unwrap().start;
        let now = peer::start_token_now();
        let agents = FakeAgents::one("p", parent, pstart);
        assert_eq!(stamp_peer(me, now, &agents), agent("p"));
        assert_eq!(
            stamp_peer(me, now, &FakeAgents::one("p", parent, pstart + 1)),
            Principal::LocalUnverified
        );
        assert_eq!(
            stamp_peer(me, now, &NoLiveAgents),
            Principal::LocalUnverified
        );
    }

    #[test]
    fn a_peer_that_started_after_the_accept_is_local_unverified() {
        let me = std::process::id() as libc::pid_t;
        let mine = peer::proc_info(me).unwrap();
        let agents = FakeAgents::one("p", mine.ppid, peer::proc_info(mine.ppid).unwrap().start);
        // Accepted just before we started: our pid would be a reused one.
        assert_eq!(
            stamp_peer(me, Some(mine.start - 1), &agents),
            Principal::LocalUnverified
        );
        assert_eq!(stamp_peer(me, None, &agents), Principal::LocalUnverified);
        assert_eq!(stamp_peer(me, Some(mine.start), &agents), agent("p"));
    }

    #[test]
    fn serializes_as_a_string() {
        assert_eq!(
            serde_json::to_value(Principal::LocalUnverified).unwrap(),
            "local-unverified"
        );
        assert_eq!(serde_json::to_value(agent("x")).unwrap(), "agent:x");
    }
}
