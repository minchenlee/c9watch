//! Which status changes notify (#10). Shared by the GUI poll and the
//! engine's detection, which each keep their own `prev` map: the GUI fires
//! native and web notifications, the engine emits `notification.*` events.

use crate::session::{Session, SessionStatus};
use std::collections::{HashMap, HashSet};

/// Update `prev` with this cycle's statuses and return the sessions that
/// moved from `Working` to `NeedsAttention` or `WaitingForInput`.
///
/// On the first cycle the map is only seeded and nothing is returned.
/// Keys of sessions that are no longer present are pruned.
pub fn notification_transitions<'a>(
    prev: &mut HashMap<String, SessionStatus>,
    first_cycle: bool,
    sessions: &'a [Session],
) -> Vec<&'a Session> {
    let mut transitioned = Vec::new();
    for session in sessions {
        if !first_cycle {
            let should_notify = matches!(
                (prev.get(&session.session_key), &session.status),
                (
                    Some(SessionStatus::Working),
                    SessionStatus::NeedsAttention | SessionStatus::WaitingForInput,
                )
            );
            if should_notify {
                transitioned.push(session);
            }
        }
        prev.insert(session.session_key.clone(), session.status.clone());
    }
    let current: HashSet<&str> = sessions.iter().map(|s| s.session_key.as_str()).collect();
    prev.retain(|id, _| current.contains(id.as_str()));
    transitioned
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(key: &str, status: SessionStatus) -> Session {
        let mut s = Session::fixture(key, status);
        s.session_key = key.to_string();
        s
    }

    fn keys(sessions: Vec<&Session>) -> Vec<&str> {
        sessions.iter().map(|s| s.session_key.as_str()).collect()
    }

    #[test]
    fn transitions_first_cycle_seeds_without_notifying() {
        let mut prev = HashMap::new();
        prev.insert("gone".to_string(), SessionStatus::Working);
        let sessions = vec![session("a", SessionStatus::WaitingForInput)];

        assert!(notification_transitions(&mut prev, true, &sessions).is_empty());
        assert_eq!(prev.len(), 1);
        assert_eq!(prev.get("a"), Some(&SessionStatus::WaitingForInput));
    }

    #[test]
    fn transitions_working_to_waiting_or_attention_notifies() {
        let mut prev = HashMap::new();
        prev.insert("a".to_string(), SessionStatus::Working);
        prev.insert("b".to_string(), SessionStatus::Working);
        prev.insert("c".to_string(), SessionStatus::Working);
        let sessions = vec![
            session("a", SessionStatus::WaitingForInput),
            session("b", SessionStatus::NeedsAttention),
            session("c", SessionStatus::Working),
        ];

        let fired = notification_transitions(&mut prev, false, &sessions);
        assert_eq!(keys(fired), vec!["a", "b"]);
        assert_eq!(prev.get("a"), Some(&SessionStatus::WaitingForInput));
        assert_eq!(prev.get("b"), Some(&SessionStatus::NeedsAttention));
        assert_eq!(prev.get("c"), Some(&SessionStatus::Working));
    }

    #[test]
    fn transitions_from_non_working_or_unknown_do_not_notify() {
        let mut prev = HashMap::new();
        prev.insert("a".to_string(), SessionStatus::WaitingForInput);
        prev.insert("b".to_string(), SessionStatus::NeedsAttention);
        let sessions = vec![
            session("a", SessionStatus::NeedsAttention),
            session("b", SessionStatus::WaitingForInput),
            session("new", SessionStatus::WaitingForInput),
        ];

        assert!(notification_transitions(&mut prev, false, &sessions).is_empty());
        assert_eq!(prev.get("new"), Some(&SessionStatus::WaitingForInput));
    }

    #[test]
    fn transitions_prune_disappeared_sessions() {
        let mut prev = HashMap::new();
        prev.insert("a".to_string(), SessionStatus::Working);
        prev.insert("gone".to_string(), SessionStatus::Working);
        let sessions = vec![session("a", SessionStatus::Working)];

        assert!(notification_transitions(&mut prev, false, &sessions).is_empty());
        assert!(!prev.contains_key("gone"));
        assert_eq!(prev.len(), 1);
    }
}
