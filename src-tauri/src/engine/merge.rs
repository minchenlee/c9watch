//! State merge (plan C1a-22, A:1042): one activity per engine agent from
//! four evidence sources.
//!
//! Priority: hook > transcript > screen > process. Each source keeps only
//! its newest claim. The newest known claim decides, except that for
//! `SETTLE` after a claim, a stronger source's claim beats the weaker
//! claims made after it: a lagging or racing weaker source cannot undo a
//! hook within that window (the screen still shows the spinner just after
//! the Stop hook). After the window the newest evidence wins again, so a
//! new turn the hook cannot report (CC has no turn-start hook in the
//! profile) still shows as `working`. Process evidence only says the
//! process lives, which is `Unknown`; with nothing else the merge is
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

use std::collections::HashMap;
use std::sync::Mutex;

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
    pub at_ms: i64,
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
}

impl AgentMerge {
    pub fn new() -> AgentMerge {
        AgentMerge::default()
    }

    /// Take one claim. Only a source's newest claim counts; an older one is
    /// ignored.
    pub fn claim(&mut self, c: Claim) -> Change {
        let mut change = Change::default();
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
                    && self.attention.as_ref().is_some_and(|a| a.at_ms < c.at_ms)
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

    /// Raise the attention flag (an auto-mode denial, a prompt).
    pub fn attention(&mut self, reason: &str, at_ms: i64) {
        self.attention = Some(Attention {
            reason: reason.to_string(),
            at_ms,
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

/// The merge state of every live agent, keyed by agent id. A new
/// generation starts with a clean merge.
#[derive(Debug, Default)]
pub struct MergeTable {
    agents: Mutex<HashMap<String, (i64, AgentMerge)>>,
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
        f(&mut entry.1)
    }

    /// Forget `agent_id` (retired or gone).
    pub fn remove(&self, agent_id: &str) {
        let mut map = self.agents.lock().unwrap_or_else(|e| e.into_inner());
        map.remove(agent_id);
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
}
