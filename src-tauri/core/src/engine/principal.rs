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
//!    A process of an agent-team teammate that detection has not listed
//!    (no transcript matched yet, or its argv is forged) counts too, with
//!    an `unlisted-teammate:<pid>` key that names no session: a teammate's
//!    descendants are never `local-unverified`.
//! 3. `local-unverified`: everything else (A:187), unless the
//!    provenance table (D34, `engine::provenance`) saw a process of the
//!    chain descend from one of the above before it was reparented. Then
//!    the peer is stamped from that origin: the agent while its generation
//!    is live (else `observed:gone-agent:<id>`), the session's
//!    `observed:<sessionKey>`, or the teammate's key. A process the full
//!    table kept out (it or an ancestor should have been added) is
//!    `observed:unknown-origin:<pid>`. A process under no root, such as the
//!    owner's terminal or the GUI, has no origin and stays
//!    `local-unverified`.
//!
//! Unix stamping never returns `human` (that waits for the C1.5 code-signature check)
//! and never stamps `client:<id>` on the engine socket: `kind=gateway` in
//! `hello` is a claim any same-uid process can make (N1, N2).

use super::peer::{self, ProcInfo, MAX_ANCESTRY};
use super::provenance::Origin;
use serde::{Serialize, Serializer};
use std::collections::HashSet;

/// The session key prefix of an `observed:` principal stamped for a
/// teammate detection has not listed. No session has such a key, so it
/// passes no own-session check.
pub const UNLISTED_TEAMMATE: &str = "unlisted-teammate:";

/// The session key prefix of an `observed:` principal stamped for a
/// reparented process of an agent whose generation is no longer live.
pub const GONE_AGENT: &str = "gone-agent:";

/// The session key prefix of an `observed:` principal stamped for a
/// process the full provenance table could not tell.
pub const UNKNOWN_ORIGIN: &str = "unknown-origin:";

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

impl Principal {
    /// An `observed:` stamp with a key no session has: a teammate
    /// detection has not listed (or a chain process that could not be
    /// read), a gone agent's reparented process, or one of unknown origin.
    /// It may only read.
    pub fn is_read_only(&self) -> bool {
        matches!(self, Principal::Observed { session_key }
            if [UNLISTED_TEAMMATE, GONE_AGENT, UNKNOWN_ORIGIN]
                .iter()
                .any(|p| session_key.starts_with(p)))
    }
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

    /// The pids in `chain` whose argv is an agent-team teammate's, read
    /// now, whether or not detection lists them. Only argv decides: a
    /// forged teammate narrows its descendants, which fails closed.
    /// `Err(pid)` when a process of the chain can't be told apart from a
    /// teammate (`peer::teammate_pids`); callers fail closed on it.
    fn teammates(&self, _chain: &[ProcInfo]) -> Result<HashSet<libc::pid_t>, libc::pid_t> {
        Ok(HashSet::new())
    }

    /// Where the process `pid` that started at `start` came from, as the
    /// provenance table recorded it (D34). `None` when it descends from
    /// nothing core watches.
    fn origin(&self, _pid: libc::pid_t, _start: u64) -> Option<Origin> {
        None
    }

    /// Whether the process `pid` that started at `start` still runs: a root
    /// a provenance entry names (a teammate) counts only while it is the
    /// same process, never a later one that reused its pid.
    fn running(&self, pid: libc::pid_t, start: u64) -> bool {
        peer::proc_info(pid).is_some_and(|p| p.start == start)
    }
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
/// `observed:<sessionKey>`, and the first that is a teammate detection has
/// not listed makes it `observed:unlisted-teammate:<pid>`. Detection has no
/// process start time, so a session's pid that exits and is reused can
/// still match. That fails closed: the new process is stamped observed,
/// which only narrows it (no create, retire or stop of other sessions),
/// and never widens it.
pub fn stamp_observed(
    chain: &[ProcInfo],
    agents: &dyn LiveAgents,
    observed: &dyn ObservedSessions,
) -> Principal {
    match stamp(chain, agents) {
        Principal::LocalUnverified => {
            let chain = &chain[..chain.len().min(MAX_ANCESTRY)];
            let teammates = match observed.teammates(chain) {
                Ok(t) => t,
                // A process that can't be read may be a teammate.
                Err(pid) => {
                    return Principal::Observed {
                        session_key: format!("{UNLISTED_TEAMMATE}{pid}"),
                    }
                }
            };
            if let Some(session_key) = chain.iter().find_map(|p| {
                observed.by_pid(p.pid).or_else(|| {
                    teammates
                        .contains(&p.pid)
                        .then(|| format!("{UNLISTED_TEAMMATE}{}", p.pid))
                })
            }) {
                return Principal::Observed { session_key };
            }
            chain
                .iter()
                .find_map(|p| observed.origin(p.pid, p.start).map(|o| (p.pid, o)))
                .map_or(Principal::LocalUnverified, |(pid, origin)| {
                    from_origin(pid, origin, agents, observed)
                })
        }
        p => p,
    }
}

/// The stamp of a process whose live ancestry shows nothing but whose
/// provenance (D34) is `origin`; `pid` is the chain process it was found for.
fn from_origin(
    pid: libc::pid_t,
    origin: Origin,
    agents: &dyn LiveAgents,
    observed: &dyn ObservedSessions,
) -> Principal {
    let session_key = match origin {
        Origin::Agent { id, gen } if agents.current_gen(&id) == Some(gen) => {
            return Principal::Agent { id, gen }
        }
        Origin::Agent { id, .. } => format!("{GONE_AGENT}{id}"),
        Origin::Session { key } => key,
        Origin::Teammate { pid, start } => observed
            .by_pid(pid)
            .filter(|_| observed.running(pid, start))
            .unwrap_or_else(|| format!("{UNLISTED_TEAMMATE}{pid}")),
        Origin::Unknown => format!("{UNKNOWN_ORIGIN}{pid}"),
    };
    Principal::Observed { session_key }
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
/// - every teammate in it (by argv, read now) is that observed session
///   `own`: one detection has not listed fails closed, whether its
///   transcript is missing, ambiguous or not found yet; so does a process
///   of the chain whose argv can't be read;
/// - no process in it was recorded (D34) as descending from an agent,
///   from an observed session other than `own`, or from a teammate other
///   than `own`; one the full table can't tell fails closed;
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
    no_unlisted_teammate(chain, observed)?;
    no_foreign_origin(chain, observed, own)?;
    match confined {
        Some(false) => Ok(()),
        Some(true) => Err("the peer runs in a sandbox or a different mount namespace; on Linux, run the core and terminal in the same namespace (systemd ProtectHome/PrivateTmp/ProtectSystem/PrivateDevices and flatpak/snap terminals can isolate them)".into()),
        None => Err("whether the peer runs in a sandbox could not be told".into()),
    }
}

/// Refuse a chain with an agent-team teammate that session detection has
/// not listed, or with a process whose argv can't be read, at any depth.
/// Every process counts: a listed session nearer to the peer does not
/// clear an unlisted teammate further up, because a same-user process
/// under that teammate can start something detection lists.
fn no_unlisted_teammate(chain: &[ProcInfo], observed: &dyn ObservedSessions) -> Result<(), String> {
    let teammates = observed.teammates(chain).map_err(|pid| {
        format!(
            "pid {} in the peer's ancestry could not be read (gone, unreadable or reused), so it may be an agent-team teammate",
            pid
        )
    })?;
    match chain
        .iter()
        .find(|p| teammates.contains(&p.pid) && observed.by_pid(p.pid).is_none())
    {
        Some(p) => Err(format!(
            "the peer runs under agent-team teammate pid {}, which session detection has not listed",
            p.pid
        )),
        None => Ok(()),
    }
}

/// Refuse a chain with a process whose recorded provenance (D34) is an
/// agent, a session other than `own`, a teammate whose session is not
/// `own`, or unknown.
fn no_foreign_origin(
    chain: &[ProcInfo],
    observed: &dyn ObservedSessions,
    own: Option<&str>,
) -> Result<(), String> {
    for p in chain {
        let why = match observed.origin(p.pid, p.start) {
            None => continue,
            Some(Origin::Agent { id, .. }) => format!("agent:{id}"),
            Some(Origin::Session { key }) if Some(key.as_str()) == own => continue,
            Some(Origin::Session { key }) => format!("observed:{key}"),
            Some(Origin::Teammate { pid, start }) => match observed.by_pid(pid) {
                Some(key) if Some(key.as_str()) == own && observed.running(pid, start) => continue,
                _ => format!("agent-team teammate pid {pid}"),
            },
            Some(Origin::Unknown) => {
                return Err(format!(
                    "where pid {} in the peer's ancestry came from is not known (the provenance table is full)",
                    p.pid
                ))
            }
        };
        return Err(format!(
            "pid {} in the peer's ancestry descends from {} (it was reparented)",
            p.pid, why
        ));
    }
    Ok(())
}

/// The teammate and provenance parts of [`check_mutation`] alone, for a
/// write whose other admission checks don't walk the peer's ancestry (an
/// observed session's annotation or mail). `own` is the key an `observed:`
/// principal was stamped with. Blocking.
pub fn check_teammate_ancestry(
    pid: libc::pid_t,
    peer_start: Option<u64>,
    observed: &dyn ObservedSessions,
    own: Option<&str>,
) -> Result<(), String> {
    let chain = peer::full_ancestry(pid).ok_or_else(|| {
        format!(
            "the peer's ancestry could not be read up to init: {}",
            peer::ancestry_gap(pid)
        )
    })?;
    match (chain.first(), peer_start) {
        (Some(first), Some(start)) if first.start == start => {}
        _ => return Err("the peer is not the process that connected".into()),
    }
    no_unlisted_teammate(&chain, observed)?;
    no_foreign_origin(&chain, observed, own)
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
    let complete = chain.is_some();
    check_mutation(
        chain.as_deref(),
        peer_start,
        peer::confined(pid),
        agents,
        observed,
        own,
    )
    .map_err(|why| {
        // Still refused; the message only says where the walk stopped.
        if complete {
            why
        } else {
            format!("{}: {}", why, peer::ancestry_gap(pid))
        }
    })
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

    /// Observed sessions plus the pids whose argv is a teammate's.
    pub(crate) struct FakeTeam {
        pub observed: FakeObserved,
        pub teammates: HashSet<libc::pid_t>,
        /// A pid whose argv can't be read.
        pub unreadable: Option<libc::pid_t>,
    }

    impl ObservedSessions for FakeTeam {
        fn by_pid(&self, pid: libc::pid_t) -> Option<String> {
            self.observed.by_pid(pid)
        }

        fn teammates(&self, chain: &[ProcInfo]) -> Result<HashSet<libc::pid_t>, libc::pid_t> {
            if let Some(pid) = self
                .unreadable
                .filter(|u| chain.iter().any(|p| p.pid == *u))
            {
                return Err(pid);
            }
            Ok(chain
                .iter()
                .map(|p| p.pid)
                .filter(|pid| self.teammates.contains(pid))
                .collect())
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

    /// D8 for an agent-team teammate: its parent is tmux, not the lead, but
    /// the teammate is a Claude Code process detection observes, so its Bash
    /// tool's processes are `observed:` the teammate. Before detection found
    /// teammates they were `local-unverified`.
    #[test]
    fn a_peer_inside_a_teammate_is_observed_though_its_parent_is_tmux() {
        // bash tool (30) -> shell (20) -> teammate claude (88) -> tmux (70) -> launchd (1).
        // The lead (16405) is not in the chain.
        let chain = vec![
            pi(30, 20, 300),
            pi(20, 88, 200),
            pi(88, 70, 150),
            pi(70, 1, 50),
        ];
        let seen = FakeObserved(HashMap::from([
            (88, "claudeCode:mate".to_string()),
            (16405, "claudeCode:lead".to_string()),
        ]));
        assert_eq!(
            stamp_observed(&chain, &NoLiveAgents, &seen),
            observed("claudeCode:mate")
        );
        let lead_only = FakeObserved::one("claudeCode:lead", 16405);
        assert_eq!(
            stamp_observed(&chain, &NoLiveAgents, &lead_only),
            Principal::LocalUnverified
        );
    }

    /// D8 fails closed for a teammate detection does not list: its
    /// transcript is missing, two candidates tie, or the next detection tick
    /// hasn't run yet. Detection lists nothing in each case, so they are
    /// one case here. Its descendants are stamped `observed:` with a key no
    /// session has, and every mutation is refused, by argv alone.
    #[test]
    fn a_peer_under_an_unlisted_teammate_is_never_local_unverified() {
        // bash tool (30) -> shell (20) -> teammate claude (88) -> tmux (70).
        let chain = vec![
            pi(30, 20, 300),
            pi(20, 88, 200),
            pi(88, 70, 150),
            pi(70, 1, 50),
        ];
        let unlisted = FakeTeam {
            observed: FakeObserved::one("claudeCode:lead", 16405),
            teammates: HashSet::from([88]),
            unreadable: None,
        };
        let p = stamp_observed(&chain, &NoLiveAgents, &unlisted);
        assert_eq!(p, observed("unlisted-teammate:88"));
        let check = |own, team: &FakeTeam| {
            check_mutation(
                Some(&chain),
                Some(300),
                Some(false),
                &NoLiveAgents,
                team,
                own,
            )
        };
        for own in [None, Some("unlisted-teammate:88"), Some("claudeCode:lead")] {
            let e = check(own, &unlisted).unwrap_err();
            assert!(e.contains("teammate pid 88"), "{}", e);
        }
        // Past `MAX_ANCESTRY` the stamp can't see it; the mutation still can.
        let mut deep: Vec<ProcInfo> = (0..40).map(|i| pi(100 + i, 101 + i, 7)).collect();
        deep.push(pi(140, 1, 7));
        let far = FakeTeam {
            observed: FakeObserved(HashMap::new()),
            teammates: HashSet::from([135]),
            unreadable: None,
        };
        assert_eq!(
            stamp_observed(&deep, &NoLiveAgents, &far),
            Principal::LocalUnverified
        );
        assert!(
            check_mutation(Some(&deep), Some(7), Some(false), &NoLiveAgents, &far, None)
                .unwrap_err()
                .contains("teammate pid 135")
        );
        // Once detection lists it, the teammate's own session may mutate
        // what an observed session may, and nothing else.
        let listed = FakeTeam {
            observed: FakeObserved::one("claudeCode:mate", 88),
            teammates: HashSet::from([88]),
            unreadable: None,
        };
        assert_eq!(
            stamp_observed(&chain, &NoLiveAgents, &listed),
            observed("claudeCode:mate")
        );
        assert_eq!(check(Some("claudeCode:mate"), &listed), Ok(()));
        assert!(check(None, &listed).is_err());
    }

    /// Codex r3: a listed session nearer to the peer does not clear an
    /// unlisted teammate further up. The stamp names the listed session;
    /// the write is refused anyway.
    #[test]
    fn a_listed_session_does_not_clear_an_unlisted_teammate_above_it() {
        // tool (30) -> spoofed listed claude (20) -> shell (88's child) ->
        // unlisted teammate (88) -> tmux (70).
        let chain = vec![
            pi(30, 20, 300),
            pi(20, 25, 200),
            pi(25, 88, 180),
            pi(88, 70, 150),
            pi(70, 1, 50),
        ];
        let team = FakeTeam {
            observed: FakeObserved::one("claudeCode:spoof", 20),
            teammates: HashSet::from([20, 88]),
            unreadable: None,
        };
        assert_eq!(
            stamp_observed(&chain, &NoLiveAgents, &team),
            observed("claudeCode:spoof")
        );
        let e = check_mutation(
            Some(&chain),
            Some(300),
            Some(false),
            &NoLiveAgents,
            &team,
            Some("claudeCode:spoof"),
        )
        .unwrap_err();
        assert!(e.contains("teammate pid 88"), "{}", e);
        assert!(no_unlisted_teammate(&chain, &team)
            .unwrap_err()
            .contains("teammate pid 88"));
        assert_eq!(no_unlisted_teammate(&chain[..3], &team), Ok(()));
    }

    /// A chain process whose argv can't be read at admission (it exited
    /// after the ancestry walk, or its pid was reused) may have been a
    /// teammate: the mutation is refused, even past `MAX_ANCESTRY`, and the
    /// stamp is narrowed.
    #[test]
    fn an_unreadable_chain_process_fails_closed() {
        let mut deep: Vec<ProcInfo> = (0..40).map(|i| pi(100 + i, 101 + i, 7)).collect();
        deep.push(pi(140, 1, 7));
        let gone = FakeTeam {
            observed: FakeObserved(HashMap::new()),
            teammates: HashSet::new(),
            unreadable: Some(135),
        };
        let e = check_mutation(
            Some(&deep),
            Some(7),
            Some(false),
            &NoLiveAgents,
            &gone,
            None,
        )
        .unwrap_err();
        assert!(e.contains("pid 135"), "{}", e);
        let near = FakeTeam {
            unreadable: Some(103),
            ..gone
        };
        let p = stamp_observed(&deep, &NoLiveAgents, &near);
        assert_eq!(p, observed("unlisted-teammate:103"));
        assert!(p.is_read_only());
        assert!(!observed("claudeCode:s1").is_read_only());
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

    /// Observed sessions plus a provenance table, keyed by pid and start.
    struct FakeOrigins {
        observed: FakeObserved,
        origins: HashMap<(libc::pid_t, u64), Origin>,
        running: HashSet<(libc::pid_t, u64)>,
    }

    impl ObservedSessions for FakeOrigins {
        fn by_pid(&self, pid: libc::pid_t) -> Option<String> {
            self.observed.by_pid(pid)
        }

        fn origin(&self, pid: libc::pid_t, start: u64) -> Option<Origin> {
            self.origins.get(&(pid, start)).cloned()
        }

        fn running(&self, pid: libc::pid_t, start: u64) -> bool {
            self.running.contains(&(pid, start))
        }
    }

    fn origins(list: &[((libc::pid_t, u64), Origin)]) -> FakeOrigins {
        FakeOrigins {
            observed: FakeObserved(HashMap::new()),
            origins: list.iter().cloned().collect(),
            running: HashSet::from([(77, 70)]),
        }
    }

    /// daemon (300) reparented under launchd (1).
    fn orphan() -> Vec<ProcInfo> {
        vec![pi(300, 1, 30), pi(1, 0, 1)]
    }

    /// D34: a reparented process is stamped from where it came from, never
    /// `local-unverified`.
    #[test]
    fn a_reparented_process_is_stamped_from_its_origin() {
        let session = origins(&[(
            (300, 30),
            Origin::Session {
                key: "claudeCode:s1".into(),
            },
        )]);
        assert_eq!(
            stamp_observed(&orphan(), &NoLiveAgents, &session),
            observed("claudeCode:s1")
        );
        // The same pid with another start time is another process.
        let reused = vec![pi(300, 1, 31), pi(1, 0, 1)];
        assert_eq!(
            stamp_observed(&reused, &NoLiveAgents, &session),
            Principal::LocalUnverified
        );

        let of_agent = origins(&[(
            (300, 30),
            Origin::Agent {
                id: "a1".into(),
                gen: 1,
            },
        )]);
        let live = FakeAgents::one("a1", 10, 100);
        assert_eq!(stamp_observed(&orphan(), &live, &of_agent), agent("a1"));
        // The agent is gone, or runs another generation: read-only.
        let gone = stamp_observed(&orphan(), &NoLiveAgents, &of_agent);
        assert_eq!(gone, observed("gone-agent:a1"));
        assert!(gone.is_read_only());

        let mate = origins(&[((300, 30), Origin::Teammate { pid: 77, start: 70 })]);
        let p = stamp_observed(&orphan(), &NoLiveAgents, &mate);
        assert_eq!(p, observed("unlisted-teammate:77"));
        assert!(p.is_read_only());
        let listed = FakeOrigins {
            observed: FakeObserved::one("claudeCode:mate", 77),
            ..origins(&[((300, 30), Origin::Teammate { pid: 77, start: 70 })])
        };
        assert_eq!(
            stamp_observed(&orphan(), &NoLiveAgents, &listed),
            observed("claudeCode:mate")
        );

        let full = origins(&[((300, 30), Origin::Unknown)]);
        let p = stamp_observed(&orphan(), &NoLiveAgents, &full);
        assert_eq!(p, observed("unknown-origin:300"));
        assert!(p.is_read_only());
    }

    /// The live ancestry still decides first: provenance only fills in
    /// when it shows nothing.
    #[test]
    fn the_live_ancestry_comes_before_provenance() {
        let o = FakeOrigins {
            observed: FakeObserved::one("claudeCode:live", 20),
            running: HashSet::new(),
            origins: HashMap::from([(
                (30, 300),
                Origin::Session {
                    key: "claudeCode:old".into(),
                },
            )]),
        };
        assert_eq!(
            stamp_observed(&bash_tool_chain(100), &NoLiveAgents, &o),
            observed("claudeCode:live")
        );
    }

    /// A mutation refuses a chain with a recorded origin other than the
    /// peer's own session, at any depth.
    #[test]
    fn a_mutation_refuses_a_foreign_origin() {
        let chain = orphan();
        let session = origins(&[(
            (300, 30),
            Origin::Session {
                key: "claudeCode:s1".into(),
            },
        )]);
        let check = |o: &FakeOrigins, own: Option<&str>| {
            check_mutation(Some(&chain), Some(30), Some(false), &NoLiveAgents, o, own)
        };
        assert!(check(&session, None)
            .unwrap_err()
            .contains("descends from observed:claudeCode:s1"));
        assert_eq!(check(&session, Some("claudeCode:s1")), Ok(()));
        assert!(check(&session, Some("claudeCode:s2")).is_err());
        let of_agent = origins(&[(
            (300, 30),
            Origin::Agent {
                id: "a1".into(),
                gen: 1,
            },
        )]);
        assert!(check(&of_agent, None).unwrap_err().contains("agent:a1"));
        let mate = origins(&[((1, 1), Origin::Teammate { pid: 77, start: 70 })]);
        assert!(check(&mate, None).unwrap_err().contains("teammate pid 77"));
        let full = origins(&[((300, 30), Origin::Unknown)]);
        assert!(check(&full, None).unwrap_err().contains("table is full"));
        assert_eq!(check(&origins(&[]), None), Ok(()));
    }

    /// Observed sessions backed by a real provenance table.
    struct Scanned {
        observed: FakeObserved,
        table: crate::engine::provenance::Provenance,
    }

    impl ObservedSessions for Scanned {
        fn by_pid(&self, pid: libc::pid_t) -> Option<String> {
            self.observed.by_pid(pid)
        }

        fn origin(&self, pid: libc::pid_t, start: u64) -> Option<Origin> {
            self.table.origin(pid, start)
        }
    }

    /// End to end over a scanned table: a double-forked child of a session
    /// is that session's, a reused pid is not, and a process from the
    /// owner's plain terminal stays `local-unverified` and may mutate.
    #[test]
    fn scanned_provenance_stamps_and_checks() {
        use crate::engine::peer::ProcEntry;
        use crate::engine::provenance::{Provenance, Roots};
        let e = |pid, ppid, start| ProcEntry {
            info: pi(pid, ppid, start),
            pgid: pid,
            zombie: false,
        };
        let roots = Roots {
            sessions: vec![(100, "claudeCode:s1".into())],
            ..Roots::default()
        };
        let table = Provenance::new();
        // launchd 1; session 100 -> shell 200 -> daemon 300; terminal 50 -> shell 60.
        let before = [
            e(1, 0, 1),
            e(100, 1, 10),
            e(200, 100, 20),
            e(300, 200, 30),
            e(50, 1, 5),
            e(60, 50, 6),
        ];
        table.scan(&before, &roots, |_| Some(false), |_, _| true);
        // The shell exits, the daemon is reparented; the owner runs c9watch.
        let after = [
            e(1, 0, 1),
            e(100, 1, 10),
            e(300, 1, 30),
            e(50, 1, 5),
            e(60, 50, 6),
            e(70, 60, 7),
        ];
        table.scan(&after, &roots, |_| Some(false), |_, _| true);
        let o = Scanned {
            observed: FakeObserved::one("claudeCode:s1", 100),
            table,
        };

        let daemon = vec![pi(300, 1, 30), pi(1, 0, 1)];
        assert_eq!(
            stamp_observed(&daemon, &NoLiveAgents, &o),
            observed("claudeCode:s1")
        );
        let mutate = |chain: &[ProcInfo], own: Option<&str>| {
            check_mutation(
                Some(chain),
                Some(chain[0].start),
                Some(false),
                &NoLiveAgents,
                &o,
                own,
            )
        };
        assert!(mutate(&daemon, None).is_err());
        assert!(mutate(&daemon, Some("claudeCode:s2")).is_err());

        let reused = vec![pi(300, 1, 31), pi(1, 0, 1)];
        assert_eq!(
            stamp_observed(&reused, &NoLiveAgents, &o),
            Principal::LocalUnverified
        );

        let cli = vec![pi(70, 60, 7), pi(60, 50, 6), pi(50, 1, 5), pi(1, 0, 1)];
        assert_eq!(
            stamp_observed(&cli, &NoLiveAgents, &o),
            Principal::LocalUnverified
        );
        assert_eq!(mutate(&cli, None), Ok(()));
    }

    /// A teammate root binds by pid and start time: once its pid is reused
    /// by another session, its orphans don't get that session's key and
    /// may not stop it.
    #[test]
    fn a_teammate_origin_ignores_a_reused_pid() {
        let reused = FakeOrigins {
            observed: FakeObserved::one("claudeCode:other", 77),
            running: HashSet::from([(77, 99)]),
            ..origins(&[((300, 30), Origin::Teammate { pid: 77, start: 70 })])
        };
        let p = stamp_observed(&orphan(), &NoLiveAgents, &reused);
        assert_eq!(p, observed("unlisted-teammate:77"));
        assert!(p.is_read_only());
        let chain = orphan();
        assert!(check_mutation(
            Some(&chain),
            Some(30),
            Some(false),
            &NoLiveAgents,
            &reused,
            Some("claudeCode:other"),
        )
        .unwrap_err()
        .contains("teammate pid 77"));
        // The same teammate, still running and listed: its key.
        let same = FakeOrigins {
            running: HashSet::from([(77, 70)]),
            ..reused
        };
        assert_eq!(
            stamp_observed(&orphan(), &NoLiveAgents, &same),
            observed("claudeCode:other")
        );
    }
}
