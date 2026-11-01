//! State merge (plan C1a-22, A:1042): one activity per engine agent from
//! four evidence sources.
//!
//! Priority: hook > transcript > screen > process. Each source keeps only
//! its newest claim. The newest known claim decides, except that for
//! `SETTLE` after a claim, a stronger source's claim beats the weaker
//! claims made after it: a lagging or racing weaker source cannot undo a
//! hook within that window (the screen still shows the spinner just after
//! the Stop hook). After the window the newest evidence wins again, so a
//! change the hooks miss (an Esc interrupt fires no Stop hook) still shows.
//! The window can end with no new claim, so the supervisor asks
//! `stale_row_state` and writes the result (`hooks::expire_hold`).
//! Process evidence only says the process lives, which is `Unknown`; with nothing else the merge is
//! `Unknown`, never idle (A:1037).
//!
//! Turn end (D24.6): Stop hook, then JSONL `turn_duration`, then the
//! screen. The first signal after a `Working` claim ends the turn; a
//! stronger signal for the same turn within `SETTLE` only upgrades who
//! ended it. A turn end means idle only, never that a task is done
//! (D22.3).
//!
//! `needs_attention` (an auto-mode denial, a prompt) is a flag on top of
//! the activity, not an activity. It stays until the next turn starts
//! after a turn end.
//!
//! Pure apart from the `MergeTable` lock: the caller passes the clock.
//!
//! Commit first (D5): a caller that writes the merge's result to the db
//! runs its change on a copy (`MergeTable::stage`, `stage_existing`), and
//! the same change runs on the live entry only once its transaction
//! commits (`MergeTable::publish_on_commit`). A write that fails leaves the
//! table as it was, so a retried event finds the same merge and writes the
//! same turn end. Publishing runs the change again instead of swapping in
//! the copy, so a change made to the entry in between (the supervisor's
//! `peek`) is kept.
//!
//! Each `with`, `peek` and `stage` takes the next revision of the table
//! when the caller acts (a staged change keeps its stage-time revision for
//! the replay). The attention flag is ordered by revision, never by the
//! wall clock: a flag from a later revision is never replaced by an
//! earlier one, and a turn start clears only a flag from an earlier
//! revision. A clock that jumps back cannot pin a stale reason.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// How long a stronger claim holds against weaker claims made after it.
pub const SETTLE_MS: i64 = 2_000;

/// Evidence sources, weakest first, so `Ord` is the priority (A:1042).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Process,
    Screen,
    Transcript,
    Hook,
}

impl Source {
    pub const ALL: [Source; 4] = [
        Source::Process,
        Source::Screen,
        Source::Transcript,
        Source::Hook,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Source::Process => "process",
            Source::Screen => "screen",
            Source::Transcript => "transcript",
            Source::Hook => "hook",
        }
    }
}

/// What an agent is doing, as one source sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    /// No rule matched. Never read as idle.
    Unknown,
    Working,
    /// Idle at the prompt (the adapter's `ready`).
    Ready,
    /// A permission prompt is open.
    Blocked,
}

/// One source's claim at `at_ms` (unix ms).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Claim {
    pub source: Source,
    pub activity: Activity,
    pub at_ms: i64,
}

impl Claim {
    pub fn new(source: Source, activity: Activity, at_ms: i64) -> Claim {
        Claim {
            source,
            activity,
            at_ms,
        }
    }
}

/// The merged activity and the source that decided it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Merged {
    pub activity: Activity,
    pub by: Option<Source>,
}

/// Merge the sources' newest claims at `now_ms` (see the module doc).
pub fn merge(claims: &[Claim], now_ms: i64) -> Merged {
    let known = claims.iter().filter(|c| c.activity != Activity::Unknown);
    let Some(newest) = known.clone().max_by_key(|c| (c.at_ms, c.source)).copied() else {
        return Merged {
            activity: Activity::Unknown,
            by: None,
        };
    };
    // A stronger claim still in its window beats the weaker, newer one.
    let holding = known
        .filter(|c| {
            c.source > newest.source
                && newest.at_ms - c.at_ms < SETTLE_MS
                && now_ms - c.at_ms < SETTLE_MS
        })
        .max_by_key(|c| (c.source, c.at_ms))
        .copied();
    let win = holding.unwrap_or(newest);
    Merged {
        activity: win.activity,
        by: Some(win.source),
    }
}

/// A turn end and the strongest source that reported it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnEnd {
    pub at_ms: i64,
    pub by: Source,
}

/// The attention flag: why, and since when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attention {
    pub reason: String,
    /// Wall-clock time, for events and display only; `rev` orders flags.
    pub at_ms: i64,
    /// The merge revision that raised it (see the module doc).
    rev: u64,
}

/// What a claim changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Change {
    /// A turn ended with this claim.
    pub turn_end: Option<TurnEnd>,
    /// A new turn started (and cleared the attention flag, if any).
    pub turn_start: bool,
}

/// The merge state of one agent generation.
#[derive(Debug, Clone, Default)]
pub struct AgentMerge {
    claims: HashMap<Source, Claim>,
    turn_open: bool,
    last_turn_end: Option<TurnEnd>,
    attention: Option<Attention>,
    /// The `row_state` last written to the row.
    written: Option<&'static str>,
    /// The `tool_use_id` of the call a hook said is waiting for
    /// permission: only that call's PostToolUse means work goes on.
    blocked_call: Option<String>,
    /// The revision of the table operation now running on this merge.
    op_rev: Option<u64>,
    /// A merge used outside a table numbers its own calls.
    local_rev: u64,
}

impl AgentMerge {
    pub fn new() -> AgentMerge {
        AgentMerge::default()
    }

    /// The revision of the change being made now.
    fn rev(&mut self) -> u64 {
        match self.op_rev {
            Some(r) => r,
            None => {
                self.local_rev += 1;
                self.local_rev
            }
        }
    }

    /// Run `f` as table operation `rev`.
    fn run<R>(&mut self, rev: u64, f: impl FnOnce(&mut AgentMerge) -> R) -> R {
        self.op_rev = Some(rev);
        let r = f(self);
        self.op_rev = None;
        r
    }

    /// Take one claim. Only a source's newest claim counts; an older one is
    /// ignored.
    pub fn claim(&mut self, c: Claim) -> Change {
        let mut change = Change::default();
        let rev = self.rev();
        if let Some(prev) = self.claims.get(&c.source) {
            if c.at_ms < prev.at_ms {
                return change;
            }
        }
        self.claims.insert(c.source, c);
        let after_last_end = self.last_turn_end.is_none_or(|e| c.at_ms > e.at_ms);
        match c.activity {
            Activity::Working | Activity::Blocked if !self.turn_open && after_last_end => {
                // Blocked also means a turn runs: CC asks while it works.
                self.turn_open = true;
                change.turn_start = true;
                if self.last_turn_end.is_some()
                    && self.attention.as_ref().is_some_and(|a| a.rev < rev)
                {
                    self.attention = None;
                }
            }
            Activity::Ready if c.source != Source::Process => {
                if self.turn_open {
                    self.turn_open = false;
                    let end = TurnEnd {
                        at_ms: c.at_ms,
                        by: c.source,
                    };
                    self.last_turn_end = Some(end);
                    change.turn_end = Some(end);
                } else if let Some(end) = self.last_turn_end.as_mut() {
                    // The same turn end from a stronger source.
                    if c.source > end.by && (c.at_ms - end.at_ms).abs() < SETTLE_MS {
                        end.by = c.source;
                    }
                }
            }
            _ => {}
        }
        change
    }

    /// Raise the attention flag (an auto-mode denial, a prompt). A flag
    /// from a later revision stays: a staged change that publishes late
    /// (see `MergeTable::stage`) must not replace a newer reason.
    pub fn attention(&mut self, reason: &str, at_ms: i64) {
        let rev = self.rev();
        if self.attention.as_ref().is_some_and(|a| a.rev > rev) {
            return;
        }
        self.attention = Some(Attention {
            reason: reason.to_string(),
            at_ms,
            rev,
        });
    }

    pub fn attention_flag(&self) -> Option<&Attention> {
        self.attention.as_ref()
    }

    /// The last turn end, with the strongest source that reported it.
    pub fn last_turn_end(&self) -> Option<TurnEnd> {
        self.last_turn_end
    }

    pub fn merged(&self, now_ms: i64) -> Merged {
        let claims: Vec<Claim> = self.claims.values().copied().collect();
        merge(&claims, now_ms)
    }

    /// Whether a turn is open.
    pub fn turn_open(&self) -> bool {
        self.turn_open
    }

    /// A turn is open and no source has said anything for `SETTLE_MS`:
    /// the screen's unchanged `ready` should be claimed again (a turn
    /// start with no turn end, such as a hook `working` the screen never
    /// showed).
    pub fn stale_turn(&self, now_ms: i64) -> bool {
        self.turn_open && self.claims.values().all(|c| now_ms - c.at_ms >= SETTLE_MS)
    }

    /// Note the call a hook said waits for permission.
    pub fn set_blocked_call(&mut self, call: Option<String>) {
        self.blocked_call = call;
    }

    /// Whether `call` is the one that waited for permission; it is
    /// forgotten when it matches.
    pub fn take_blocked_call(&mut self, call: &str) -> bool {
        let hit = self.blocked_call.as_deref() == Some(call);
        if hit {
            self.blocked_call = None;
        }
        hit
    }

    /// Record that `state` was written to the row.
    pub fn written(&mut self, state: Option<&'static str>) {
        if state.is_some() {
            self.written = state;
        }
    }

    /// The `row_state` at `now_ms` when it differs from the one last
    /// written: a hold ran out with no new claim.
    pub fn stale_row_state(&self, now_ms: i64) -> Option<&'static str> {
        self.row_state(now_ms).filter(|s| self.written != Some(*s))
    }

    /// The `agents.state` for the merge, or `None` while it is `Unknown`
    /// (the row keeps what it has). Detection owns `agents.status` of a pty
    /// row (C1a-15); the merge owns `state`.
    pub fn row_state(&self, now_ms: i64) -> Option<&'static str> {
        match self.merged(now_ms).activity {
            Activity::Unknown => None,
            Activity::Working => Some("working"),
            Activity::Ready => Some("ready"),
            Activity::Blocked => Some("blocked"),
        }
    }
}

type Agents = Arc<Mutex<HashMap<String, (i64, AgentMerge)>>>;

/// The merge state of every live agent, keyed by agent id. A new
/// generation starts with a clean merge.
#[derive(Debug, Default)]
pub struct MergeTable {
    agents: Agents,
    /// The last revision handed out; taken under the `agents` lock.
    revs: std::sync::atomic::AtomicU64,
}

/// A merge change computed on a copy, not yet in the table (see the
/// module doc). Publishing runs the same change again on the live entry,
/// so a change made there in between (an attention flag the supervisor
/// raised) is kept.
#[must_use = "a staged merge changes nothing until it is published"]
pub struct Staged {
    agent_id: String,
    gen: i64,
    /// The revision taken at stage time; the replay runs as it.
    rev: u64,
    change: Box<dyn FnOnce(&mut AgentMerge) + Send>,
    /// From `stage_existing`: publish only while the agent is still at
    /// `gen`, never create it.
    existing: bool,
}

impl std::fmt::Debug for Staged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Staged")
            .field("agent_id", &self.agent_id)
            .field("gen", &self.gen)
            .field("rev", &self.rev)
            .field("existing", &self.existing)
            .finish_non_exhaustive()
    }
}

impl MergeTable {
    pub fn new() -> MergeTable {
        MergeTable::default()
    }

    /// Run `f` on the merge of `agent_id`'s generation `gen`. An older
    /// generation's merge is replaced by a clean one.
    pub fn with<R>(&self, agent_id: &str, gen: i64, f: impl FnOnce(&mut AgentMerge) -> R) -> R {
        let mut map = self.agents.lock().unwrap_or_else(|e| e.into_inner());
        let entry = map
            .entry(agent_id.to_string())
            .or_insert_with(|| (gen, AgentMerge::new()));
        if entry.0 != gen {
            *entry = (gen, AgentMerge::new());
        }
        let rev = self.next_rev();
        entry.1.run(rev, f)
    }

    /// The next revision. Callers hold the `agents` lock, so revisions
    /// follow the order in which operations take it.
    fn next_rev(&self) -> u64 {
        self.revs.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
    }

    /// Run `f` on the merge of `agent_id` only when it is at generation
    /// `gen`; nothing is created or replaced.
    pub fn peek<R>(
        &self,
        agent_id: &str,
        gen: i64,
        f: impl FnOnce(&mut AgentMerge) -> R,
    ) -> Option<R> {
        let mut map = self.agents.lock().unwrap_or_else(|e| e.into_inner());
        match map.get_mut(agent_id) {
            Some((g, m)) if *g == gen => {
                let rev = self.next_rev();
                Some(m.run(rev, f))
            }
            _ => None,
        }
    }

    /// Forget `agent_id` (retired or gone).
    pub fn remove(&self, agent_id: &str) {
        let mut map = self.agents.lock().unwrap_or_else(|e| e.into_inner());
        map.remove(agent_id);
    }

    /// `with` on a copy: run `f` on a copy of the merge of `agent_id`'s
    /// generation `gen` (a clean one for a new generation) and return its
    /// result. The table does not change; publishing runs `f` again on the
    /// live entry.
    pub fn stage<R>(
        &self,
        agent_id: &str,
        gen: i64,
        f: impl Fn(&mut AgentMerge) -> R + Send + 'static,
    ) -> (R, Staged) {
        let (mut copy, rev) = {
            let map = self.agents.lock().unwrap_or_else(|e| e.into_inner());
            let copy = match map.get(agent_id) {
                Some((g, m)) if *g == gen => m.clone(),
                _ => AgentMerge::new(),
            };
            (copy, self.next_rev())
        };
        let r = copy.run(rev, &f);
        (r, Staged::new(agent_id, gen, rev, f, false))
    }

    /// `peek` on a copy: `None` unless `agent_id` is at generation `gen`.
    pub fn stage_existing<R>(
        &self,
        agent_id: &str,
        gen: i64,
        f: impl Fn(&mut AgentMerge) -> R + Send + 'static,
    ) -> Option<(R, Staged)> {
        let (mut copy, rev) = {
            let map = self.agents.lock().unwrap_or_else(|e| e.into_inner());
            let copy = match map.get(agent_id) {
                Some((g, m)) if *g == gen => m.clone(),
                _ => return None,
            };
            (copy, self.next_rev())
        };
        let r = copy.run(rev, &f);
        Some((r, Staged::new(agent_id, gen, rev, f, true)))
    }

    /// Run `staged`'s change on the live entry. A newer generation of the
    /// agent wins over it; a `stage_existing` change of an agent that went
    /// is dropped.
    pub fn publish(&self, staged: Staged) {
        publish(&self.agents, staged);
    }

    /// Publish `staged` when `tx` commits; if it rolls back, the table
    /// does not change.
    pub fn publish_on_commit(&self, tx: &mut super::db::Tx, staged: Staged) {
        let agents = self.agents.clone();
        tx.after_commit(move || publish(&agents, staged));
    }
}

impl Staged {
    fn new<R>(
        agent_id: &str,
        gen: i64,
        rev: u64,
        f: impl Fn(&mut AgentMerge) -> R + Send + 'static,
        existing: bool,
    ) -> Staged {
        Staged {
            agent_id: agent_id.to_string(),
            gen,
            rev,
            change: Box::new(move |m| {
                f(m);
            }),
            existing,
        }
    }
}

fn publish(agents: &Agents, staged: Staged) {
    let mut map = agents.lock().unwrap_or_else(|e| e.into_inner());
    let live = map.get(&staged.agent_id).map(|(g, _)| *g);
    match live {
        Some(g) if g > staged.gen => return,
        Some(g) if g == staged.gen => {}
        _ if staged.existing => return,
        // No entry, or an older generation's: this one starts clean.
        _ => {
            map.insert(staged.agent_id.clone(), (staged.gen, AgentMerge::new()));
        }
    }
    if let Some((_, m)) = map.get_mut(&staged.agent_id) {
        m.run(staged.rev, staged.change);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Activity::*;
    use Source::*;

    fn c(source: Source, activity: Activity, at_ms: i64) -> Claim {
        Claim::new(source, activity, at_ms)
    }

    /// The priority table (A:1042). Each row: the claims, the clock, and
    /// the merged activity and its source.
    #[test]
    fn merge_priority_table() {
        let rows: &[(&str, &[Claim], i64, Activity, Option<Source>)] = &[
            ("nothing", &[], 0, Unknown, None),
            (
                "process alone is unknown",
                &[c(Process, Unknown, 0)],
                0,
                Unknown,
                None,
            ),
            (
                "screen over process",
                &[c(Process, Unknown, 5), c(Screen, Working, 0)],
                10,
                Working,
                Some(Screen),
            ),
            (
                "same time: hook > transcript > screen",
                &[
                    c(Screen, Working, 100),
                    c(Transcript, Blocked, 100),
                    c(Hook, Ready, 100),
                ],
                100,
                Ready,
                Some(Hook),
            ),
            (
                "same time: transcript > screen",
                &[c(Screen, Working, 100), c(Transcript, Ready, 100)],
                100,
                Ready,
                Some(Transcript),
            ),
            (
                "hook Stop holds against a screen spinner just after it",
                &[c(Hook, Ready, 1_000), c(Screen, Working, 1_500)],
                1_600,
                Ready,
                Some(Hook),
            ),
            (
                "hook holds against a transcript row just after it",
                &[c(Hook, Ready, 1_000), c(Transcript, Working, 1_100)],
                1_200,
                Ready,
                Some(Hook),
            ),
            (
                "after the window the newer screen wins (a new turn)",
                &[c(Hook, Ready, 1_000), c(Screen, Working, 1_500)],
                1_000 + SETTLE_MS,
                Working,
                Some(Screen),
            ),
            (
                "a weaker claim far after the hook wins",
                &[c(Hook, Ready, 1_000), c(Screen, Working, 9_000)],
                9_000,
                Working,
                Some(Screen),
            ),
            (
                "a stronger claim newer than a weaker one wins",
                &[c(Screen, Ready, 1_000), c(Transcript, Working, 1_200)],
                1_300,
                Working,
                Some(Transcript),
            ),
            (
                "screen blocked, hook PermissionRequest confirms",
                &[c(Screen, Blocked, 1_000), c(Hook, Blocked, 1_300)],
                1_400,
                Blocked,
                Some(Hook),
            ),
            (
                "screen blocked over an older transcript working",
                &[c(Transcript, Working, 500), c(Screen, Blocked, 4_000)],
                4_100,
                Blocked,
                Some(Screen),
            ),
            (
                "unknown screen does not hide the transcript",
                &[c(Transcript, Working, 500), c(Screen, Unknown, 4_000)],
                4_100,
                Working,
                Some(Transcript),
            ),
            (
                "the strongest of several holding claims",
                &[
                    c(Transcript, Ready, 1_000),
                    c(Hook, Blocked, 1_000),
                    c(Screen, Working, 1_100),
                ],
                1_200,
                Blocked,
                Some(Hook),
            ),
        ];
        for (name, claims, now, activity, by) in rows {
            let m = merge(claims, *now);
            assert_eq!((m.activity, m.by), (*activity, *by), "{}", name);
        }
    }

    #[test]
    fn turn_end_order_is_stop_then_turn_duration_then_screen() {
        // Stop first: one turn end, by the hook; the later signals confirm.
        let mut m = AgentMerge::new();
        assert!(m.claim(c(Screen, Working, 0)).turn_start);
        let end = m.claim(c(Hook, Ready, 1_000)).turn_end;
        assert_eq!(
            end,
            Some(TurnEnd {
                at_ms: 1_000,
                by: Hook
            })
        );
        assert_eq!(m.claim(c(Transcript, Ready, 1_050)), Change::default());
        assert_eq!(m.claim(c(Screen, Ready, 1_300)), Change::default());
        assert_eq!(m.last_turn_end().unwrap().by, Hook);

        // No Stop: turn_duration ends the turn.
        let mut m = AgentMerge::new();
        m.claim(c(Transcript, Working, 0));
        assert_eq!(
            m.claim(c(Transcript, Ready, 900)).turn_end.map(|e| e.by),
            Some(Transcript)
        );

        // Neither: the screen ends it.
        let mut m = AgentMerge::new();
        m.claim(c(Screen, Working, 0));
        assert_eq!(
            m.claim(c(Screen, Ready, 900)).turn_end.map(|e| e.by),
            Some(Screen)
        );
        // The screen saw it first, the Stop hook came in the window: the
        // turn end belongs to the hook, and it is still one turn end.
        assert_eq!(m.claim(c(Hook, Ready, 1_000)), Change::default());
        assert_eq!(m.last_turn_end().unwrap().by, Hook);
        // Out of the window it is not the same turn end.
        let mut m = AgentMerge::new();
        m.claim(c(Screen, Working, 0));
        m.claim(c(Screen, Ready, 900));
        m.claim(c(Hook, Ready, 900 + SETTLE_MS));
        assert_eq!(m.last_turn_end().unwrap().by, Screen);

        // The first ready of a generation is not a turn end.
        let mut m = AgentMerge::new();
        assert_eq!(m.claim(c(Screen, Ready, 0)), Change::default());
        assert_eq!(m.last_turn_end(), None);
    }

    #[test]
    fn a_late_row_from_before_the_turn_end_does_not_reopen_the_turn() {
        let mut m = AgentMerge::new();
        m.claim(c(Screen, Working, 0));
        m.claim(c(Hook, Ready, 1_000));
        // The transcript row is read late, but it is from 900.
        assert!(!m.claim(c(Transcript, Working, 900)).turn_start);
        assert_eq!(m.row_state(5_000), Some("ready"));
        // An older claim of a source than the one it has is ignored.
        m.claim(c(Screen, Ready, 1_300));
        m.claim(c(Screen, Working, 1_200));
        assert_eq!(m.merged(5_000).by, Some(Screen));
        assert_eq!(m.merged(5_000).activity, Ready);
    }

    #[test]
    fn attention_stays_until_the_next_turn_starts() {
        let mut m = AgentMerge::new();
        m.claim(c(Screen, Working, 0));
        m.attention("auto_mode_denied", 500);
        // CC goes on with the turn after a denial.
        m.claim(c(Transcript, Working, 600));
        assert_eq!(m.row_state(700), Some("working"));
        assert!(m.attention_flag().is_some());
        m.claim(c(Hook, Ready, 1_000));
        assert_eq!(m.row_state(1_100), Some("ready"));
        assert_eq!(m.attention_flag().unwrap().reason, "auto_mode_denied");
        assert!(m.claim(c(Screen, Working, 9_000)).turn_start);
        assert!(m.attention_flag().is_none());
        assert_eq!(m.row_state(9_100), Some("working"));
    }

    #[test]
    fn unknown_keeps_the_row_and_a_new_generation_starts_clean() {
        let mut m = AgentMerge::new();
        m.claim(c(Process, Unknown, 0));
        assert_eq!(m.row_state(0), None);
        let t = MergeTable::new();
        t.with("a1", 1, |m| m.claim(c(Hook, Blocked, 0)));
        assert_eq!(t.with("a1", 1, |m| m.row_state(10)), Some("blocked"));
        assert_eq!(t.with("a1", 2, |m| m.row_state(10)), None);
        t.remove("a1");
    }

    #[test]
    fn a_staged_merge_changes_the_table_only_when_published() {
        let t = MergeTable::new();
        let (change, staged) = t.stage("a1", 1, |m| m.claim(c(Hook, Working, 0)));
        assert!(change.turn_start);
        assert_eq!(t.peek("a1", 1, |m| m.turn_open()), None);
        t.publish(staged);
        assert_eq!(t.peek("a1", 1, |m| m.turn_open()), Some(true));
        // An unpublished copy is dropped with no trace.
        let _ = t.stage("a1", 1, |m| m.claim(c(Hook, Ready, 1_000)));
        assert_eq!(t.peek("a1", 1, |m| m.turn_open()), Some(true));
        // A copy of an older generation does not replace a newer one, and
        // a `stage_existing` copy does not bring back a removed agent.
        let (_, old) = t.stage("a1", 1, |m| m.claim(c(Hook, Ready, 1_000)));
        t.with("a1", 2, |_| ());
        t.publish(old);
        assert_eq!(t.peek("a1", 2, |m| m.turn_open()), Some(false));
        assert!(t.stage_existing("a1", 1, |_| ()).is_none());
        let (_, gone) = t.stage_existing("a1", 2, |m| m.attention("x", 5)).unwrap();
        t.remove("a1");
        t.publish(gone);
        assert_eq!(t.peek("a1", 2, |_| ()), None);
    }

    /// Attention staged at t1, a flag raised by a later action (t2) before
    /// the commit: the later one survives the publish. The reverse: a flag
    /// raised before the stage loses to the staged one.
    #[test]
    fn a_late_publish_keeps_the_newer_attention() {
        let t = MergeTable::new();
        t.with("a1", 1, |_| ());
        let (_, staged) = t
            .stage_existing("a1", 1, |m| m.attention("auto_mode_denied: x", 1_000))
            .unwrap();
        t.peek("a1", 1, |m| m.attention("mail_uncertain: m1", 2_000));
        t.publish(staged);
        let flag = t.peek("a1", 1, |m| m.attention_flag().cloned()).flatten();
        assert_eq!(
            flag.map(|a| (a.reason, a.at_ms)),
            Some(("mail_uncertain: m1".into(), 2_000))
        );

        let t = MergeTable::new();
        t.with("a1", 1, |_| ());
        t.peek("a1", 1, |m| m.attention("mail_uncertain: m1", 1_000));
        let (_, staged) = t
            .stage_existing("a1", 1, |m| m.attention("mail_held: m2", 2_000))
            .unwrap();
        t.publish(staged);
        let flag = t.peek("a1", 1, |m| m.attention_flag().cloned()).flatten();
        assert_eq!(
            flag.map(|a| (a.reason, a.at_ms)),
            Some(("mail_held: m2".into(), 2_000))
        );
    }

    /// The clock jumps back: a later action stamped earlier still wins,
    /// and a later turn start still clears the flag.
    #[test]
    fn attention_follows_the_revision_not_the_clock() {
        let t = MergeTable::new();
        t.with("a1", 1, |m| m.claim(c(Screen, Working, 0)));
        t.peek("a1", 1, |m| m.attention("auto_mode_denied: x", 2_000));
        // Rolled back: a later flag at 1_500 replaces the one at 2_000.
        t.peek("a1", 1, |m| m.attention("mail_uncertain: m1", 1_500));
        let flag = t.peek("a1", 1, |m| m.attention_flag().cloned()).flatten();
        assert_eq!(
            flag.map(|a| (a.reason, a.at_ms)),
            Some(("mail_uncertain: m1".into(), 1_500))
        );
        // A staged flag taken before the rolled-back one loses to it, too.
        let (_, staged) = t
            .stage_existing("a1", 1, |m| m.attention("mail_held: m2", 3_000))
            .unwrap();
        t.peek("a1", 1, |m| m.attention("mail_uncertain: m3", 100));
        t.publish(staged);
        let reason = t.peek("a1", 1, |m| m.attention_flag().map(|a| a.reason.clone()));
        assert_eq!(reason.flatten().as_deref(), Some("mail_uncertain: m3"));
        // The clear path: the turn ends, then a new turn starts at a time
        // before the flag's stamp; it is a later action, so it clears.
        t.with("a1", 1, |m| m.claim(c(Hook, Ready, 4_000)));
        t.peek("a1", 1, |m| m.attention("mail_uncertain: m4", 9_000));
        let started = t.with("a1", 1, |m| m.claim(c(Screen, Working, 5_000)));
        assert!(started.turn_start);
        assert_eq!(
            t.peek("a1", 1, |m| m.attention_flag().is_none()),
            Some(true)
        );
        // A staged turn start from before the flag does not clear it on replay.
        t.with("a1", 1, |m| m.claim(c(Hook, Ready, 6_000)));
        let (_, staged) = t.stage("a1", 1, |m| m.claim(c(Hook, Working, 7_000)));
        t.peek("a1", 1, |m| m.attention("mail_uncertain: m5", 1));
        t.publish(staged);
        t.peek("a1", 1, |m| {
            assert!(m.turn_open());
            assert_eq!(m.attention_flag().unwrap().reason, "mail_uncertain: m5");
        });
    }

    /// A change made to the live entry between stage and publish (the
    /// supervisor's `peek`) survives the publish.
    #[test]
    fn publish_keeps_a_change_made_after_the_stage() {
        let t = MergeTable::new();
        t.with("a1", 1, |m| m.claim(c(Screen, Working, 0)));
        let (change, staged) = t.stage("a1", 1, |m| m.claim(c(Hook, Ready, 1_000)));
        assert_eq!(change.turn_end.map(|e| e.by), Some(Hook));
        t.peek("a1", 1, |m| m.attention("mail_uncertain: m1", 1_100));
        t.publish(staged);
        t.peek("a1", 1, |m| {
            assert!(!m.turn_open());
            assert_eq!(m.last_turn_end().map(|e| e.by), Some(Hook));
            assert_eq!(m.attention_flag().unwrap().reason, "mail_uncertain: m1");
        });
        // A new generation's change starts clean, even over an older entry.
        let (_, staged) = t.stage("a1", 2, |m| m.claim(c(Hook, Working, 2_000)));
        t.publish(staged);
        assert_eq!(
            t.peek("a1", 2, |m| m.attention_flag().is_none()),
            Some(true)
        );
    }

    #[test]
    fn a_turn_with_no_new_claim_goes_stale() {
        let mut m = AgentMerge::new();
        assert!(!m.stale_turn(10_000));
        m.claim(c(Hook, Working, 1_000));
        assert!(m.turn_open());
        assert!(!m.stale_turn(1_000 + SETTLE_MS - 1));
        assert!(m.stale_turn(1_000 + SETTLE_MS));
        // The screen's `ready`, claimed again, closes it.
        m.claim(c(Screen, Ready, 1_000 + SETTLE_MS));
        assert!(!m.turn_open());
        assert!(!m.stale_turn(9_000));
        // A blocked call is forgotten once it matches.
        m.set_blocked_call(Some("toolu_1".into()));
        assert!(!m.take_blocked_call("toolu_2"));
        assert!(m.take_blocked_call("toolu_1"));
        assert!(!m.take_blocked_call("toolu_1"));
        let t = MergeTable::new();
        assert_eq!(t.peek("a1", 1, |m| m.turn_open()), None);
        t.with("a1", 1, |m| m.claim(c(Hook, Working, 0)));
        assert_eq!(t.peek("a1", 1, |m| m.turn_open()), Some(true));
        assert_eq!(t.peek("a1", 2, |m| m.turn_open()), None);
        assert_eq!(t.peek("a1", 1, |m| m.turn_open()), Some(true));
    }
}
