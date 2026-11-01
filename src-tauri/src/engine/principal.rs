//! Principal stamping. The engine decides who a connection is from
//! connection facts only; what the client says in `hello` never counts.
//!
//! Unix connections have two possible principals. Authenticated gateway
//! connections additionally have `client:<id>`:
//! 1. `agent:<id>`: the peer's ancestry (at most `MAX_ANCESTRY` levels)
//!    reaches the PTY child of a live agent, with the same pid **and** start
//!    time. The principal carries that agent's generation, as the engine
//!    recorded it at spawn (C1a-33). The live set is the engine's table in
//!    `engine::agents`, filled only from PTY children the engine spawned, so
//!    an observed session is never an agent (A:186).
//! 2. `observed:<sessionKey>`: no live agent is in the chain, but a
//!    process of a session detection observed (one the engine did not
//!    create) is. Such a peer runs inside the user's own session, so it is
//!    narrower than `local-unverified`: no `agent.create`/`agent.retire`, and
//!    `session.stop` only for its own session (D8).
//! 3. `local-unverified`: everything else (A:187).
//!
//! Unix stamping never returns `human` (that waits for the C1.5 code-signature check)
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
    /// A process inside the observed session with this key (D8).
    Observed {
        session_key: String,
    },
    /// Only constructed after gateway token authentication; never by stamp_peer.
    Client {
        id: String,
    },
}

impl std::fmt::Display for Principal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Principal::Agent { id, .. } => write!(f, "agent:{}", id),
            Principal::LocalUnverified => f.write_str("local-unverified"),
            Principal::Observed { session_key } => write!(f, "observed:{}", session_key),
            Principal::Client { id } => write!(f, "client:{}", id),
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
    /// The live agent whose PTY child has `pid` and started at `start`, if
    /// any. A row with the same pid and another start time is a reused pid
    /// and does not match.
    fn by_pty_child(&self, pid: libc::pid_t, start: u64) -> Option<LiveAgent>;

    /// The current generation of `agent_id`, if it is live. A hook event
    /// stamped with another generation is stale (C1a-22).
    fn current_gen(&self, agent_id: &str) -> Option<i64>;
}

/// Lookup of observed sessions by the pid of their process, as detection
/// last saw them. A session the engine created matches too; `stamp` asks
/// `LiveAgents` first, so its peers stay `agent:<id>`.
pub trait ObservedSessions: Send + Sync {
    /// The `sessionKey` of the observed session whose process has `pid`.
    fn by_pid(&self, pid: libc::pid_t) -> Option<String>;
}

/// No session is observed.
pub struct NoObservedSessions;

impl ObservedSessions for NoObservedSessions {
    fn by_pid(&self, _pid: libc::pid_t) -> Option<String> {
        None
    }
}

/// No agent is live. Every connection is `local-unverified`.
pub struct NoLiveAgents;

impl LiveAgents for NoLiveAgents {
    fn by_pty_child(&self, _pid: libc::pid_t, _start: u64) -> Option<LiveAgent> {
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
        if let Some(agent) = agents.by_pty_child(p.pid, p.start) {
            return Principal::Agent {
                id: agent.id,
                gen: agent.gen,
            };
        }
    }
    Principal::LocalUnverified
}

/// `stamp`, then observed sessions: with no live agent in the chain, the
/// first process that is an observed session's makes the peer
/// `observed:<sessionKey>`. Detection has no process start time, so a
/// session's pid that exits and is reused can still match. That fails
/// closed: the new process is stamped observed, which only narrows it
/// (no create, retire or stop of other sessions), and never widens it.
pub fn stamp_observed(
    chain: &[ProcInfo],
    agents: &dyn LiveAgents,
    observed: &dyn ObservedSessions,
) -> Principal {
    match stamp(chain, agents) {
        Principal::LocalUnverified => chain
            .iter()
            .take(MAX_ANCESTRY)
            .find_map(|p| observed.by_pid(p.pid))
            .map_or(Principal::LocalUnverified, |session_key| {
                Principal::Observed { session_key }
            }),
        p => p,
    }
}

/// Stamp the principal of the peer with `pid`. `accepted` is the start-time
/// token taken when the connection was accepted. A peer that started after
/// it holds a reused pid, not the process that connected, and is
/// `local-unverified`; so is every peer when the clock could not be read.
/// This does not catch a reuse between connect and accept.
///
/// Also returns the peer's start time from the same read, so a later check
/// of the peer (its exe) is tied to the process the principal came from.
pub fn stamp_peer(
    pid: libc::pid_t,
    accepted: Option<u64>,
    agents: &dyn LiveAgents,
    observed: &dyn ObservedSessions,
) -> (Principal, Option<u64>) {
    let chain = peer::ancestry(pid, MAX_ANCESTRY);
    let start = chain.first().map(|p| p.start);
    match (start, accepted) {
        (Some(first), Some(at)) if first <= at => (stamp_observed(&chain, agents, observed), start),
        _ => (Principal::LocalUnverified, start),
    }
}

/// The extra check before a `local-unverified` or `observed:` peer may call
/// a mutating method (`agent.create`, `agent.retire`, `session.stop`). The stamp walks at most
/// `MAX_ANCESTRY` levels and calls the rest `local-unverified`, which is
/// fine for reads; a mutation fails closed instead (security review H1):
/// - `chain` is the peer's whole ancestry up to init (`None`: the walk
///   stopped early, so an agent may be above the cut);
/// - its first entry is the process stamped at accept (`peer_start`);
/// - no process in it is a live agent's PTY child, at any depth;
/// - no process in it is an observed session's, at any depth, except the
///   session `own` (the key an `observed:` principal was stamped with): a
///   `local-unverified` stamp only walks `MAX_ANCESTRY` levels;
/// - the peer is not `confined`: an agent's Bash runs in the vendor
///   sandbox, and a process it moved under init stays there.
pub fn check_mutation(
    chain: Option<&[ProcInfo]>,
    peer_start: Option<u64>,
    confined: Option<bool>,
    agents: &dyn LiveAgents,
    observed: &dyn ObservedSessions,
    own: Option<&str>,
) -> Result<(), String> {
    let chain = chain.ok_or("the peer's ancestry could not be read up to init")?;
    match (chain.first(), peer_start) {
        (Some(first), Some(start)) if first.start == start => {}
        _ => return Err("the peer is not the process that connected".into()),
    }
    if let Some(agent) = chain
        .iter()
        .find_map(|p| agents.by_pty_child(p.pid, p.start))
    {
        return Err(format!("the peer runs under agent:{}", agent.id));
    }
    if let Some(key) = chain
        .iter()
        .filter_map(|p| observed.by_pid(p.pid))
        .find(|k| Some(k.as_str()) != own)
    {
        return Err(format!("the peer runs under observed:{}", key));
    }
    match confined {
        Some(false) => Ok(()),
        Some(true) => Err("the peer runs in a sandbox or a different mount namespace; on Linux, run the core and terminal in the same namespace (systemd ProtectHome/PrivateTmp/ProtectSystem/PrivateDevices and flatpak/snap terminals can isolate them)".into()),
        None => Err("whether the peer runs in a sandbox could not be told".into()),
    }
}

/// `check_mutation` for the peer with `pid`, as read now. Blocking.
pub fn check_mutating_peer(
    pid: libc::pid_t,
    peer_start: Option<u64>,
    agents: &dyn LiveAgents,
    observed: &dyn ObservedSessions,
    own: Option<&str>,
) -> Result<(), String> {
    let chain = peer::full_ancestry(pid);
    check_mutation(
        chain.as_deref(),
        peer_start,
        peer::confined(pid),
        agents,
        observed,
        own,
    )
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
        fn by_pty_child(&self, pid: libc::pid_t, start: u64) -> Option<LiveAgent> {
            self.0
                .get(&pid)
                .filter(|a| a.pty_child_start == start)
                .cloned()
        }

        fn current_gen(&self, agent_id: &str) -> Option<i64> {
            self.0.values().find(|a| a.id == agent_id).map(|a| a.gen)
        }
    }

    /// Observed sessions keyed by process pid.
    pub(crate) struct FakeObserved(pub HashMap<libc::pid_t, String>);

    impl FakeObserved {
        pub(crate) fn one(key: &str, pid: libc::pid_t) -> Self {
            Self(HashMap::from([(pid, key.to_string())]))
        }
    }

    impl ObservedSessions for FakeObserved {
        fn by_pid(&self, pid: libc::pid_t) -> Option<String> {
            self.0.get(&pid).cloned()
        }
    }

    fn observed(key: &str) -> Principal {
        Principal::Observed {
            session_key: key.into(),
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
        assert_eq!(
            stamp_peer(me, now, &agents, &NoObservedSessions).0,
            agent("p")
        );
        assert_eq!(
            stamp_peer(
                me,
                now,
                &FakeAgents::one("p", parent, pstart + 1),
                &NoObservedSessions
            )
            .0,
            Principal::LocalUnverified
        );
        assert_eq!(
            stamp_peer(me, now, &NoLiveAgents, &NoObservedSessions).0,
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
            stamp_peer(me, Some(mine.start - 1), &agents, &NoObservedSessions).0,
            Principal::LocalUnverified
        );
        assert_eq!(
            stamp_peer(me, None, &agents, &NoObservedSessions).0,
            Principal::LocalUnverified
        );
        assert_eq!(
            stamp_peer(me, Some(mine.start), &agents, &NoObservedSessions).0,
            agent("p")
        );
        // The start time comes back from the same read, even when refused.
        assert_eq!(
            stamp_peer(me, None, &agents, &NoObservedSessions).1,
            Some(mine.start)
        );
        assert_eq!(stamp_peer(-1, None, &agents, &NoObservedSessions).1, None);
    }

    /// H1: a mutation fails closed where the stamp says
    /// `local-unverified`.
    #[test]
    fn a_mutation_needs_the_whole_chain_and_no_sandbox() {
        let chain = bash_tool_chain(100);
        let ok = |c: Option<&[ProcInfo]>, start, confined, agents: &dyn LiveAgents| {
            check_mutation(c, start, confined, agents, &NoObservedSessions, None)
        };
        assert_eq!(
            ok(Some(&chain), Some(300), Some(false), &NoLiveAgents),
            Ok(())
        );
        // The walk stopped before init.
        let e = ok(None, Some(300), Some(false), &NoLiveAgents).unwrap_err();
        assert!(e.contains("up to init"), "{}", e);
        // Another process than the one stamped at accept.
        assert!(ok(Some(&chain), Some(301), Some(false), &NoLiveAgents).is_err());
        assert!(ok(Some(&chain), None, Some(false), &NoLiveAgents).is_err());
        // Sandboxed, or not known.
        let e = ok(Some(&chain), Some(300), Some(true), &NoLiveAgents).unwrap_err();
        assert!(e.contains("sandbox"), "{}", e);
        assert!(ok(Some(&chain), Some(300), None, &NoLiveAgents).is_err());
        // A live agent's PTY child far past `MAX_ANCESTRY`.
        let deep: Vec<ProcInfo> = (0..40).map(|i| pi(100 + i, 101 + i, 7)).collect();
        let agents = FakeAgents::one("a1", 135, 7);
        assert_eq!(stamp(&deep, &agents), Principal::LocalUnverified);
        let e = ok(Some(&deep), Some(7), Some(false), &agents).unwrap_err();
        assert!(e.contains("agent:a1"), "{}", e);
    }

    /// D8: a peer inside an observed session is `observed:<key>`, not the
    /// broad `local-unverified` bucket; a live agent still wins.
    #[test]
    fn a_peer_inside_an_observed_session_is_observed() {
        let chain = bash_tool_chain(100);
        let seen = FakeObserved::one("claudeCode:s1", 10);
        let p = stamp_observed(&chain, &NoLiveAgents, &seen);
        assert_eq!(p, observed("claudeCode:s1"));
        assert_eq!(p.to_string(), "observed:claudeCode:s1");
        assert_eq!(
            stamp_observed(&chain, &FakeAgents::one("a1", 10, 100), &seen),
            agent("a1")
        );
        assert_eq!(
            stamp_observed(&chain, &NoLiveAgents, &NoObservedSessions),
            Principal::LocalUnverified
        );
    }

    /// D8: a mutation refuses a peer under an observed session at any
    /// depth, except the observed principal's own session.
    #[test]
    fn a_mutation_refuses_an_observed_session_other_than_its_own() {
        let deep: Vec<ProcInfo> = (0..40).map(|i| pi(100 + i, 101 + i, 7)).collect();
        let seen = FakeObserved::one("claudeCode:s1", 135);
        let check =
            |own| check_mutation(Some(&deep), Some(7), Some(false), &NoLiveAgents, &seen, own);
        let e = check(None).unwrap_err();
        assert!(e.contains("observed:claudeCode:s1"), "{}", e);
        assert!(check(Some("claudeCode:s2")).is_err());
        assert_eq!(check(Some("claudeCode:s1")), Ok(()));
    }

    #[test]
    fn the_test_process_may_mutate() {
        let me = std::process::id() as libc::pid_t;
        let start = peer::proc_info(me).map(|p| p.start);
        assert_eq!(
            check_mutating_peer(me, start, &NoLiveAgents, &NoObservedSessions, None),
            Ok(())
        );
        let parent = peer::proc_info(me).unwrap().ppid;
        let pstart = peer::proc_info(parent).unwrap().start;
        assert!(check_mutating_peer(
            me,
            start,
            &FakeAgents::one("p", parent, pstart),
            &NoObservedSessions,
            None
        )
        .is_err());
    }

    #[test]
    fn serializes_as_a_string() {
        assert_eq!(
            serde_json::to_value(Principal::LocalUnverified).unwrap(),
            "local-unverified"
        );
        assert_eq!(serde_json::to_value(agent("x")).unwrap(), "agent:x");
        assert_eq!(
            serde_json::to_value(observed("claudeCode:s")).unwrap(),
            "observed:claudeCode:s"
        );
    }
    #[test]
    fn unix_stamp_never_constructs_a_client() {
        let (principal, _) = stamp_peer(
            std::process::id() as libc::pid_t,
            peer::start_token_now(),
            &NoLiveAgents,
            &NoObservedSessions,
        );
        assert!(matches!(principal, Principal::LocalUnverified));
        assert!(!matches!(
            stamp(&bash_tool_chain(100), &FakeAgents::one("a1", 10, 100)),
            Principal::Client { .. }
        ));
    }
}
