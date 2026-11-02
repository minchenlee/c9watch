//! Event contract shared by `c9watch watch` and `c9watch wait`.
//!
//! [`diff_sessions`] compares the previous poll with the current one and
//! returns the events. [`wait_outcome`] decides whether a `wait` condition is
//! met by one event. Both are pure; the polling loops live in `cli/mod.rs`.
//! The JSON fields are documented in `docs/cli-events.md`.

use crate::session::SessionStatus;
use clap::ValueEnum;
use std::collections::{HashMap, HashSet};

/// Version of the event line format. Bump only for breaking changes; new
/// fields can be added without a bump.
pub const EVENT_SCHEMA: u32 = 1;

/// Exit code when `wait --timeout` runs out (same as `timeout(1)`).
pub const EXIT_TIMEOUT: i32 = 124;
/// Exit code when the session reference matches no live session, or more than one.
pub const EXIT_NOT_FOUND: i32 = 2;
/// Exit code when the session stops before a `needs-input` condition is met.
pub const EXIT_ENDED_FIRST: i32 = 3;

/// What `watch` and `wait` know about one session in one poll.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub key: String,
    pub id: String,
    pub status: SessionStatus,
    pub pending_tool: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// First time the session is seen.
    Started,
    /// Status or pending tool changed.
    StatusChanged,
    /// The session is no longer active.
    Stopped,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Started => "started",
            EventKind::StatusChanged => "status_changed",
            EventKind::Stopped => "stopped",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub kind: EventKind,
    pub key: String,
    pub id: String,
    /// Current status; `None` for `stopped`.
    pub status: Option<SessionStatus>,
    /// Status in the previous poll; `None` for `started`.
    pub previous_status: Option<SessionStatus>,
}

/// Compare the previous poll with `current` and update `prev` in place.
///
/// Events come in the order of `current`, then `stopped` events sorted by key.
/// With `include_started = false` a new session is recorded silently.
pub fn diff_sessions(
    prev: &mut HashMap<String, Snapshot>,
    current: Vec<Snapshot>,
    include_started: bool,
) -> Vec<Change> {
    let mut changes = Vec::new();
    let mut seen = HashSet::new();
    for snap in current {
        seen.insert(snap.key.clone());
        match prev.get(&snap.key) {
            None if include_started => changes.push(Change {
                kind: EventKind::Started,
                key: snap.key.clone(),
                id: snap.id.clone(),
                status: Some(snap.status.clone()),
                previous_status: None,
            }),
            None => {}
            Some(old) if old.status != snap.status || old.pending_tool != snap.pending_tool => {
                changes.push(Change {
                    kind: EventKind::StatusChanged,
                    key: snap.key.clone(),
                    id: snap.id.clone(),
                    status: Some(snap.status.clone()),
                    previous_status: Some(old.status.clone()),
                })
            }
            Some(_) => {}
        }
        prev.insert(snap.key.clone(), snap);
    }

    let mut gone: Vec<String> = prev
        .keys()
        .filter(|key| !seen.contains(*key))
        .cloned()
        .collect();
    gone.sort();
    for key in gone {
        if let Some(old) = prev.remove(&key) {
            changes.push(Change {
                kind: EventKind::Stopped,
                key,
                id: old.id,
                status: None,
                previous_status: Some(old.status),
            });
        }
    }
    changes
}

/// Event line for `watch` and `wait`. `session` is the session payload
/// (compact or full); `provider` comes from the live session when known.
pub fn event_json(
    change: &Change,
    provider: Option<serde_json::Value>,
    session: Option<serde_json::Value>,
    timestamp: &str,
) -> serde_json::Value {
    let mut line = serde_json::json!({
        "schema": EVENT_SCHEMA,
        "event": change.kind.as_str(),
        "sessionId": change.id,
        "sessionKey": change.key,
        "timestamp": timestamp,
    });
    let obj = line.as_object_mut().expect("json object");
    if let Some(status) = &change.status {
        obj.insert("status".into(), serde_json::json!(status));
    }
    if let Some(previous) = &change.previous_status {
        obj.insert("previousStatus".into(), serde_json::json!(previous));
    }
    if let Some(provider) = provider {
        obj.insert("provider".into(), provider);
    }
    if let Some(session) = session {
        obj.insert("session".into(), session);
    }
    line
}

/// The condition `wait --until` waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Until {
    /// The session is not Working. Met at once when it is not Working already.
    Done,
    /// The session needs you: NeedsAttention or WaitingForInput.
    NeedsInput,
    /// The session stops (a `stopped` event).
    #[value(alias = "stopped")]
    Ended,
    /// Any `status_changed` or `stopped` event after the first poll.
    AnyChange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOutcome {
    /// The condition is met: print the event, exit 0.
    Met,
    /// The session stopped first and can never meet the condition: print the
    /// event, exit [`EXIT_ENDED_FIRST`].
    EndedFirst,
}

/// Whether `change` (an event of the waited-for session) ends a `wait`.
pub fn wait_outcome(until: Until, change: &Change) -> Option<WaitOutcome> {
    let needs_input = matches!(
        change.status,
        Some(SessionStatus::NeedsAttention | SessionStatus::WaitingForInput)
    );
    let not_working = change
        .status
        .as_ref()
        .is_some_and(|s| *s != SessionStatus::Working);
    let met = match (until, change.kind) {
        (Until::NeedsInput, EventKind::Started | EventKind::StatusChanged) => needs_input,
        (Until::NeedsInput, EventKind::Stopped) => return Some(WaitOutcome::EndedFirst),
        (Until::Done, EventKind::Started) => not_working,
        (Until::Done, EventKind::StatusChanged) => {
            not_working && change.previous_status == Some(SessionStatus::Working)
        }
        // A session that stops has stopped working.
        (Until::Done, EventKind::Stopped) => true,
        (Until::Ended, kind) => kind == EventKind::Stopped,
        (Until::AnyChange, kind) => kind != EventKind::Started,
    };
    met.then_some(WaitOutcome::Met)
}

/// Resolve a `wait` reference against the live sessions.
///
/// Accepts the same forms as the other commands: a provider-scoped key
/// (`codex:abc…`), a full session id, or a unique prefix of either. A
/// `provider:` prefix limits the match to that provider's keys.
pub fn resolve_live_session(reference: &str, live: &[Snapshot]) -> Result<String, String> {
    let reference = reference.trim();
    if reference.is_empty() {
        return Err("session reference is empty".to_string());
    }
    if let Some(s) = live
        .iter()
        .find(|s| s.key == reference || s.id == reference)
    {
        return Ok(s.key.clone());
    }
    let matches: Vec<&Snapshot> = live
        .iter()
        .filter(|s| s.key.starts_with(reference) || s.id.starts_with(reference))
        .collect();
    match matches.as_slice() {
        [one] => Ok(one.key.clone()),
        [] => Err(format!("No live session matches '{}'", reference)),
        many => Err(format!(
            "Ambiguous session reference '{}' matches {} live sessions",
            reference,
            many.len()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use SessionStatus::*;

    fn snap(key: &str, status: SessionStatus) -> Snapshot {
        Snapshot {
            key: key.into(),
            id: key.rsplit(':').next().unwrap().into(),
            status,
            pending_tool: None,
        }
    }

    fn change(kind: EventKind, prev: Option<SessionStatus>, now: Option<SessionStatus>) -> Change {
        Change {
            kind,
            key: "claudeCode:a".into(),
            id: "a".into(),
            status: now,
            previous_status: prev,
        }
    }

    #[test]
    fn first_poll_emits_started_unless_changes_only() {
        let mut prev = HashMap::new();
        let events = diff_sessions(&mut prev, vec![snap("claudeCode:a", Working)], true);
        assert_eq!(
            events,
            vec![change(EventKind::Started, None, Some(Working))]
        );

        let mut quiet = HashMap::new();
        assert!(diff_sessions(&mut quiet, vec![snap("claudeCode:a", Working)], false).is_empty());
        assert!(
            quiet.contains_key("claudeCode:a"),
            "state is still recorded"
        );
    }

    #[test]
    fn same_status_emits_nothing() {
        let mut prev = HashMap::new();
        diff_sessions(&mut prev, vec![snap("claudeCode:a", Working)], true);
        assert!(diff_sessions(&mut prev, vec![snap("claudeCode:a", Working)], true).is_empty());
    }

    #[test]
    fn status_or_pending_tool_change_emits_status_changed() {
        let mut prev = HashMap::new();
        diff_sessions(&mut prev, vec![snap("claudeCode:a", Working)], true);
        let events = diff_sessions(&mut prev, vec![snap("claudeCode:a", NeedsAttention)], true);
        assert_eq!(
            events,
            vec![change(
                EventKind::StatusChanged,
                Some(Working),
                Some(NeedsAttention)
            )]
        );

        let mut with_tool = snap("claudeCode:a", NeedsAttention);
        with_tool.pending_tool = Some("Bash".into());
        let events = diff_sessions(&mut prev, vec![with_tool], true);
        assert_eq!(
            events,
            vec![change(
                EventKind::StatusChanged,
                Some(NeedsAttention),
                Some(NeedsAttention)
            )]
        );
    }

    #[test]
    fn a_session_that_disappears_emits_stopped_once() {
        let mut prev = HashMap::new();
        diff_sessions(
            &mut prev,
            vec![
                snap("codex:b", Working),
                snap("claudeCode:a", WaitingForInput),
            ],
            true,
        );
        let events = diff_sessions(&mut prev, vec![], true);
        assert_eq!(
            events
                .iter()
                .map(|e| (e.kind, e.key.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (EventKind::Stopped, "claudeCode:a"),
                (EventKind::Stopped, "codex:b")
            ]
        );
        assert_eq!(events[0].previous_status, Some(WaitingForInput));
        assert_eq!(events[0].status, None);
        assert!(diff_sessions(&mut prev, vec![], true).is_empty());
    }

    #[test]
    fn same_id_from_two_providers_are_separate_sessions() {
        let mut prev = HashMap::new();
        let events = diff_sessions(
            &mut prev,
            vec![snap("claudeCode:x", Working), snap("codex:x", Working)],
            true,
        );
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn event_json_has_schema_and_keeps_existing_fields() {
        let line = event_json(
            &change(
                EventKind::StatusChanged,
                Some(Working),
                Some(WaitingForInput),
            ),
            Some(serde_json::json!("claudeCode")),
            Some(serde_json::json!({"id": "a"})),
            "2026-10-14T00:00:00Z",
        );
        assert_eq!(
            line,
            serde_json::json!({
                "schema": 1,
                "event": "status_changed",
                "sessionId": "a",
                "sessionKey": "claudeCode:a",
                "status": "WaitingForInput",
                "previousStatus": "Working",
                "provider": "claudeCode",
                "session": {"id": "a"},
                "timestamp": "2026-10-14T00:00:00Z",
            })
        );
        let stopped = event_json(
            &change(EventKind::Stopped, Some(Working), None),
            None,
            None,
            "t",
        );
        assert_eq!(
            stopped,
            serde_json::json!({
                "schema": 1,
                "event": "stopped",
                "sessionId": "a",
                "sessionKey": "claudeCode:a",
                "previousStatus": "Working",
                "timestamp": "t",
            })
        );
    }

    #[test]
    fn needs_input_matches_attention_and_waiting() {
        for status in [NeedsAttention, WaitingForInput] {
            for kind in [EventKind::Started, EventKind::StatusChanged] {
                assert_eq!(
                    wait_outcome(
                        Until::NeedsInput,
                        &change(kind, Some(Working), Some(status.clone()))
                    ),
                    Some(WaitOutcome::Met)
                );
            }
        }
        for status in [Working, Connecting] {
            assert_eq!(
                wait_outcome(
                    Until::NeedsInput,
                    &change(EventKind::Started, None, Some(status))
                ),
                None
            );
        }
        assert_eq!(
            wait_outcome(
                Until::NeedsInput,
                &change(EventKind::Stopped, Some(Working), None)
            ),
            Some(WaitOutcome::EndedFirst)
        );
    }

    #[test]
    fn done_is_working_to_not_working_or_not_working_at_start() {
        let met = Some(WaitOutcome::Met);
        assert_eq!(
            wait_outcome(
                Until::Done,
                &change(EventKind::Started, None, Some(WaitingForInput))
            ),
            met
        );
        assert_eq!(
            wait_outcome(
                Until::Done,
                &change(EventKind::Started, None, Some(Working))
            ),
            None
        );
        assert_eq!(
            wait_outcome(
                Until::Done,
                &change(
                    EventKind::StatusChanged,
                    Some(Working),
                    Some(NeedsAttention)
                )
            ),
            met
        );
        // A tool change while idle is not "done": it never was Working.
        assert_eq!(
            wait_outcome(
                Until::Done,
                &change(
                    EventKind::StatusChanged,
                    Some(WaitingForInput),
                    Some(NeedsAttention)
                )
            ),
            None
        );
        assert_eq!(
            wait_outcome(
                Until::Done,
                &change(EventKind::Stopped, Some(Working), None)
            ),
            met
        );
    }

    #[test]
    fn ended_and_any_change() {
        let started = change(EventKind::Started, None, Some(WaitingForInput));
        let changed = change(EventKind::StatusChanged, Some(Working), Some(Working));
        let stopped = change(EventKind::Stopped, Some(Working), None);
        assert_eq!(wait_outcome(Until::Ended, &started), None);
        assert_eq!(wait_outcome(Until::Ended, &changed), None);
        assert_eq!(wait_outcome(Until::Ended, &stopped), Some(WaitOutcome::Met));
        assert_eq!(wait_outcome(Until::AnyChange, &started), None);
        assert_eq!(
            wait_outcome(Until::AnyChange, &changed),
            Some(WaitOutcome::Met)
        );
        assert_eq!(
            wait_outcome(Until::AnyChange, &stopped),
            Some(WaitOutcome::Met)
        );
    }

    #[test]
    fn until_accepts_stopped_as_an_alias_of_ended() {
        assert_eq!(Until::from_str("stopped", false), Ok(Until::Ended));
        assert_eq!(Until::from_str("needs-input", false), Ok(Until::NeedsInput));
        assert_eq!(Until::from_str("any-change", false), Ok(Until::AnyChange));
    }

    #[test]
    fn resolve_live_session_by_key_id_or_unique_prefix() {
        let live = vec![
            snap("claudeCode:6d4bf9af-1946", Working),
            snap("codex:6d4c0000-aaaa", Working),
            snap("claudeCode:e9f09eb1-206e", Working),
        ];
        assert_eq!(
            resolve_live_session("claudeCode:6d4bf9af-1946", &live).unwrap(),
            "claudeCode:6d4bf9af-1946"
        );
        assert_eq!(
            resolve_live_session("e9f09eb1-206e", &live).unwrap(),
            "claudeCode:e9f09eb1-206e"
        );
        assert_eq!(
            resolve_live_session("e9f0", &live).unwrap(),
            "claudeCode:e9f09eb1-206e"
        );
        assert_eq!(
            resolve_live_session("codex:6d4", &live).unwrap(),
            "codex:6d4c0000-aaaa"
        );
        assert!(resolve_live_session("6d4", &live)
            .unwrap_err()
            .contains("Ambiguous"));
        assert!(resolve_live_session("ffff", &live)
            .unwrap_err()
            .contains("No live session"));
        assert!(resolve_live_session("", &live).is_err());
    }
}
