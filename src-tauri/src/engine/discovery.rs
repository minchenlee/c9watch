//! What `hello` tells a client about this engine: each method with the
//! minor that added it, its shape revision and its stability; the preview
//! methods that changed after the client's minor (`breakingSince`); the
//! topics; and the limits.

use super::api::{MAX_CONNECTIONS, MAX_STREAMS};
use super::db::outbox::{RETENTION_EVENTS, RETENTION_MS};
use super::proto::{Limits, MethodInfo, MAX_FRAME};
use std::collections::BTreeMap;

pub const PREVIEW: &str = "preview";
pub const INTERNAL: &str = "internal";

/// One method's version facts.
pub struct Method {
    pub name: &'static str,
    /// The minor that added it.
    pub since: u32,
    /// 1, plus one per breaking change to its shape.
    pub rev: u32,
    /// The minor of the last breaking change (`since` while `rev` is 1).
    pub rev_minor: u32,
    pub stability: &'static str,
}

const fn m(name: &'static str, since: u32, stability: &'static str) -> Method {
    Method {
        name,
        since,
        rev: 1,
        rev_minor: since,
        stability,
    }
}

/// Every method `METHODS` lists. A breaking change to a preview method
/// raises its `rev` and sets `rev_minor` to the minor that ships it. The
/// hook class is internal: only `c9watch-agent` calls it.
pub const METHOD_INFO: &[Method] = &[
    m("hello", 0, PREVIEW),
    m("snapshot.get", 1, PREVIEW),
    m("events.subscribe", 1, PREVIEW),
    m("session.list", 1, PREVIEW),
    m("agent.list", 1, PREVIEW),
    Method {
        rev: 2,
        rev_minor: 10,
        ..m("agent.create", 3, PREVIEW)
    },
    m("agent.retire", 3, PREVIEW),
    m("agent.resume", 11, PREVIEW),
    m("hook.session_start", 2, INTERNAL),
    m("hook.stop", 2, INTERNAL),
    m("hook.permission_request", 2, INTERNAL),
    m("hook.permission_denied", 2, INTERNAL),
    m("hook.notification", 2, INTERNAL),
    m("hook.user_prompt_submit", 4, INTERNAL),
    m("hook.post_tool_use", 4, INTERNAL),
    m("task.done", 2, INTERNAL),
    Method {
        rev: 2,
        rev_minor: 10,
        ..m("agent.handoff", 2, INTERNAL)
    },
    m("msg.send", 4, PREVIEW),
    // C1b-M2: mailbox data and human controls.
    m("msg.list", 7, PREVIEW),
    m("msg.read", 7, PREVIEW),
    m("msg.cancel", 7, PREVIEW),
    m("mail.pause", 7, PREVIEW),
    m("mail.resume", 7, PREVIEW),
    m("session.views", 5, PREVIEW),
    m("session.away", 5, PREVIEW),
    m("detect.recheck", 5, PREVIEW),
    m("session.stop", 5, PREVIEW),
    m("session.rename", 5, PREVIEW),
    m("diag.detection", 6, PREVIEW),
    m("conversation.get", 6, PREVIEW),
    m("history.list", 6, PREVIEW),
    m("history.search", 6, PREVIEW),
    m("cost.get", 6, PREVIEW),
    m("memory.list", 6, PREVIEW),
    m("subagent.list", 6, PREVIEW),
    m("subagent.transcript", 6, PREVIEW),
    m("session.tasks", 6, PREVIEW),
    m("usage.subscriptions", 6, PREVIEW),
    m("provider.opencode.status", 6, PREVIEW),
    m("provider.opencode.connect", 6, PREVIEW),
    m("events.unsubscribe", 6, PREVIEW),
    m("ping", 6, PREVIEW),
    m("annotation.set", 9, PREVIEW),
    m("annotation.delete", 9, PREVIEW),
    m("annotation.patch", 9, PREVIEW),
    m("annotation.list", 9, PREVIEW),
    m("auth.can", 8, PREVIEW),
    m("auth.rules", 8, PREVIEW),
];

/// Every event topic and its stability. `legacy.inbox` carries the
/// imported worker inbox for the CLI only.
pub const TOPICS: &[(&str, &str)] = &[
    ("agent.upsert", PREVIEW),
    ("agent.attention", PREVIEW),
    ("agent.turn_end", PREVIEW),
    ("agent.gone", PREVIEW),
    ("agent.failed", PREVIEW),
    ("msg.state", PREVIEW),
    ("legacy.inbox", INTERNAL),
    ("session.view", PREVIEW),
    ("session.gone", PREVIEW),
    ("notification.needs_attention", PREVIEW),
    ("notification.waiting_for_input", PREVIEW),
    ("diag.detection", PREVIEW),
    ("detect.health", PREVIEW),
    ("annotation.changed", PREVIEW),
    ("annotation.removed", PREVIEW),
];

/// What a subscriber must know about a topic beyond its data type.
pub fn topic_description(topic: &str) -> Option<&'static str> {
    match topic {
        "session.view" => Some(
            "Latest state: the log keeps only the newest view of each live \
             session (and none of an ended one). A subscriber that reads on from \
             an old cursor gets the newest view of every live session, not every \
             view in between. Live subscribers still get each view as it is written.",
        ),
        _ => None,
    }
}

pub fn stability(method: &str) -> Option<&'static str> {
    METHOD_INFO
        .iter()
        .find(|x| x.name == method)
        .map(|x| x.stability)
}

/// `hello.result.methods`.
pub fn methods() -> BTreeMap<String, MethodInfo> {
    METHOD_INFO
        .iter()
        .map(|x| {
            (
                x.name.to_string(),
                MethodInfo {
                    needs: None,
                    since: x.since,
                    rev: x.rev,
                    stability: x.stability.to_string(),
                },
            )
        })
        .collect()
}

/// The preview methods of `table` whose shape changed after
/// `client_minor`: their last breaking change shipped in a later minor.
/// A method the client's minor did not have yet is new to it, not broken.
pub fn breaking_since(table: &[Method], client_minor: u32) -> Vec<String> {
    table
        .iter()
        .filter(|x| x.stability == PREVIEW && x.rev > 1)
        .filter(|x| x.since <= client_minor && x.rev_minor > client_minor)
        .map(|x| x.name.to_string())
        .collect()
}

/// The topics a socket connection receives: all of them. Gateway scopes
/// narrow this later.
pub fn topics() -> Vec<String> {
    TOPICS.iter().map(|(t, _)| t.to_string()).collect()
}

pub fn limits() -> Limits {
    Limits {
        max_frame: MAX_FRAME as u64,
        max_streams: MAX_STREAMS as u64,
        max_connections: MAX_CONNECTIONS as u64,
        retention_hours: (RETENTION_MS / 3_600_000) as u64,
        retention_events: RETENTION_EVENTS as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_info_covers_exactly_the_served_methods() {
        let mut info: Vec<&str> = METHOD_INFO.iter().map(|x| x.name).collect();
        let mut served = super::super::api::METHODS.to_vec();
        info.sort();
        served.sort();
        assert_eq!(info, served);
        let current = super::super::proto::API_MINOR;
        for x in METHOD_INFO {
            assert!(x.since <= current && x.rev_minor <= current, "{}", x.name);
            assert!(x.rev >= 1 && x.rev_minor >= x.since, "{}", x.name);
            assert_eq!(x.rev == 1, x.rev_minor == x.since, "{}", x.name);
        }
    }

    /// A method that broke in minor 8 is listed for a client built at 7,
    /// not for one built at 8; a method added after the client's minor is
    /// not listed; internal methods are never listed.
    #[test]
    fn breaking_since_lists_preview_methods_changed_after_the_client() {
        let table = [
            Method {
                name: "a.broke",
                since: 2,
                rev: 2,
                rev_minor: 8,
                stability: PREVIEW,
            },
            Method {
                name: "a.broke_twice",
                since: 2,
                rev: 3,
                rev_minor: 6,
                stability: PREVIEW,
            },
            m("a.same", 2, PREVIEW),
            Method {
                name: "a.new",
                since: 9,
                rev: 2,
                rev_minor: 9,
                stability: PREVIEW,
            },
            Method {
                name: "a.internal",
                since: 2,
                rev: 2,
                rev_minor: 8,
                stability: INTERNAL,
            },
        ];
        assert_eq!(breaking_since(&table, 7), ["a.broke"]);
        assert_eq!(breaking_since(&table, 5), ["a.broke", "a.broke_twice"]);
        assert!(breaking_since(&table, 8).is_empty());
        assert!(breaking_since(&table, 1).is_empty());
        assert!(breaking_since(METHOD_INFO, 0).is_empty());
    }

    #[test]
    fn every_topic_is_listed_once() {
        let mut t = topics();
        let n = t.len();
        t.sort();
        t.dedup();
        assert_eq!(t.len(), n);
    }
}
