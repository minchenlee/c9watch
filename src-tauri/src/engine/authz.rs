//! One deny-by-default authority table. The pipeline is frame limits →
//! authentication → authorize → admission → handler → egress → audit.
//! Discovery and self queries consult this table without executing admission.
use super::principal::Principal;
use super::proto::{
    ApiError, AuthCanResult, AuthNeeds, AuthRulesResult, Ceiling, ErrorCode, MethodInfo,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

const L: &str = "local-unverified";
const A: &str = "agent";
const C: &str = "client";
const ALL: &[&str] = &[L, A, C];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    pub resource: &'static str,
    pub verb: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_ceiling: Option<Ceiling>,
    pub principals: &'static [&'static str],
    pub admission: &'static [&'static str],
}
fn rule(
    resource: &'static str,
    verb: &'static str,
    principals: &'static [&'static str],
    min_ceiling: Option<Ceiling>,
    admission: &'static [&'static str],
) -> Rule {
    Rule {
        resource,
        verb,
        principals,
        min_ceiling,
        admission,
    }
}

/// Explicit names only: a new handler never inherits access from a prefix.
pub fn method(name: &str) -> Option<Rule> {
    Some(match name {
        "hello" | "ping" | "events.subscribe" | "events.unsubscribe" | "auth.can"
        | "auth.rules" => rule("protocol", "read", ALL, None, &[]),
        "snapshot.get" | "session.list" | "session.views" | "session.away" | "diag.detection" => {
            rule("session", "read", ALL, None, &[])
        }
        "agent.list" => rule("agent", "read", ALL, None, &[]),
        "agent.create" | "agent.retire" => rule("agent", "write", &[L], None, &["mutating_peer"]),
        "detect.recheck" => rule("detect", "write", &[L], None, &["mutating_peer"]),
        "session.stop" => rule("session", "stop", &[L, C], None, &["mutating_peer"]),
        "session.rename" => rule("session", "rename", &[L, C], None, &["mutating_peer"]),
        // Existing diagnostic access for agents is deliberately retained.
        "provider.opencode.status" => rule("provider", "read", &[L, A], None, &[]),
        "provider.opencode.connect" => rule("provider", "write", &[L], None, &["mutating_peer"]),
        "conversation.get" | "subagent.transcript" | "session.tasks" => rule(
            "conversation",
            "read",
            &[L, C],
            Some(Ceiling::Conversation),
            &[],
        ),
        "history.list" => rule("history", "read", &[L, C], Some(Ceiling::Conversation), &[]),
        "history.search" => rule("history", "read", &[L], Some(Ceiling::Conversation), &[]),
        "memory.list" => rule("memory", "read", &[L, C], Some(Ceiling::Conversation), &[]),
        "cost.get" | "usage.subscriptions" | "subagent.list" => {
            rule("cost", "read", &[L, C], None, &[])
        }
        "msg.list" | "msg.read" => rule("msg", "read", &[L, A], None, &["mail_peer"]),
        "msg.cancel" => rule("msg", "cancel", &[L], None, &["mutating_peer"]),
        "mail.pause" | "mail.resume" => rule("mail", "write", &[L], None, &["mutating_peer"]),
        "msg.send" => rule("msg", "send", &[L, A], None, &["mail_sender"]),
        "hook.session_start"
        | "hook.stop"
        | "hook.permission_request"
        | "hook.permission_denied"
        | "hook.notification"
        | "hook.user_prompt_submit"
        | "hook.post_tool_use"
        | "task.done"
        | "agent.handoff" => rule("hook", "write", &[L, A], None, &["agent_exe"]),
        _ => return None,
    })
}

pub fn topic(name: &str) -> Option<Rule> {
    Some(match name {
        "session.view"
        | "session.gone"
        | "notification.needs_attention"
        | "notification.waiting_for_input"
        | "diag.detection" => rule("session", "read", ALL, None, &[]),
        "agent.upsert" | "agent.attention" | "agent.turn_end" | "agent.gone" | "agent.failed" => {
            rule("agent", "read", ALL, None, &[])
        }
        "msg.state" => rule("msg", "read", &[L, A], None, &[]),
        "legacy.inbox" => rule("legacy", "read", &[L], None, &[]),
        _ => return None,
    })
}

pub fn kind(principal: &Principal) -> &'static str {
    match principal {
        Principal::LocalUnverified => L,
        Principal::Agent { .. } => A,
        Principal::Client { .. } => C,
    }
}

/// Verified token authority, already intersected with the live machine policy.
/// Absence is never interpreted as a full-authority token.
#[derive(Debug, Clone, Default)]
pub struct Access {
    pub scopes: BTreeSet<String>,
    pub ceiling: Ceiling,
    pub expires_at: Option<i64>,
    pub idle_expires_at: Option<i64>,
}

pub fn review(
    rule: Option<&Rule>,
    principal: &Principal,
    access: Option<&Access>,
) -> AuthCanResult {
    let deny = |reason: &str, needs| AuthCanResult {
        allowed: false,
        reason: Some(reason.into()),
        needs,
    };
    let Some(rule) = rule else {
        return deny("no authorization declaration", None);
    };
    if !rule.principals.contains(&kind(principal)) {
        return deny("principal is not permitted", None);
    }
    let mut needs = AuthNeeds::default();
    if matches!(principal, Principal::Client { .. }) {
        let scope = format!("{}:{}", rule.resource, rule.verb);
        if rule.resource != "protocol" && !access.is_some_and(|a| a.scopes.contains(&scope)) {
            needs.scopes.push(scope);
        }
        if let Some(min) = rule.min_ceiling {
            if access.map(|a| a.ceiling).unwrap_or(Ceiling::Public) < min {
                needs.ceiling = Some(min);
            }
        }
        if !needs.scopes.is_empty() || needs.ceiling.is_some() {
            return deny("insufficient authority", Some(needs));
        }
    }
    AuthCanResult {
        allowed: true,
        reason: None,
        needs: None,
    }
}

pub fn can(name: &str, principal: &Principal, access: Option<&Access>) -> AuthCanResult {
    review(method(name).as_ref(), principal, access)
}

pub fn authorize(
    name: &str,
    principal: &Principal,
    access: Option<&Access>,
) -> Result<Rule, ApiError> {
    let checked = can(name, principal, access);
    if checked.allowed {
        return Ok(method(name).expect("reviewed declaration"));
    }
    Err(ApiError::new(
        ErrorCode::NotAllowed,
        checked.reason.clone().unwrap_or_default(),
    )
    .with_data(serde_json::json!({"needs": checked.needs})))
}

pub fn methods(principal: &Principal, access: Option<&Access>) -> BTreeMap<String, MethodInfo> {
    super::discovery::methods()
        .into_iter()
        .filter_map(|(name, mut info)| {
            let r = method(&name)?;
            if !r.principals.contains(&kind(principal)) {
                return None;
            }
            info.needs = review(Some(&r), principal, access).needs;
            Some((name, info))
        })
        .collect()
}

pub fn topics(principal: &Principal, access: Option<&Access>) -> Vec<String> {
    super::discovery::TOPICS
        .iter()
        .filter(|(name, _)| review(topic(name).as_ref(), principal, access).allowed)
        .map(|(n, _)| n.to_string())
        .collect()
}

pub fn rules(principal: &Principal, access: Option<&Access>) -> AuthRulesResult {
    AuthRulesResult {
        principal: principal.to_string(),
        scopes: access
            .map(|a| a.scopes.iter().cloned().collect())
            .unwrap_or_default(),
        ceiling: access.map(|a| a.ceiling).unwrap_or(
            if matches!(principal, Principal::Client { .. }) {
                Ceiling::Public
            } else {
                Ceiling::Conversation
            },
        ),
        expires_at: access.and_then(|a| a.expires_at),
        idle_expires_at: access.and_then(|a| a.idle_expires_at),
        methods: methods(principal, access),
        topics: topics(principal, access),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditLevel {
    None,
    Metadata,
    Hash,
}
/// First match wins; no request/response bodies are ever persisted.
pub fn audit_level(
    name: &str,
    principal: &Principal,
    allowed: bool,
    ceiling: Ceiling,
) -> AuditLevel {
    if matches!(name, "auth.can" | "auth.rules") {
        return AuditLevel::None;
    }
    if !allowed {
        return AuditLevel::Metadata;
    }
    if super::proto::is_hook_class(name) || matches!(name, "hello" | "ping") {
        return AuditLevel::None;
    }
    if matches!(principal, Principal::Client { .. })
        && ((name == "events.subscribe" && ceiling == Ceiling::Conversation)
            || method(name).is_some_and(|r| r.min_ceiling == Some(Ceiling::Conversation)))
    {
        return AuditLevel::Metadata;
    }
    if method(name).is_some_and(|r| r.verb != "read") {
        return AuditLevel::Hash;
    }
    AuditLevel::None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schema_has_one_authority_declaration_for_every_method_and_topic() {
        let schema = super::super::api::schema();
        for group in ["methods", "topics"] {
            for (name, value) in schema[group].as_object().unwrap() {
                let expected = if group == "methods" {
                    method(name)
                } else {
                    topic(name)
                }
                .unwrap();
                assert_eq!(
                    value["x-authz"],
                    serde_json::to_value(expected).unwrap(),
                    "{group}.{name}"
                );
            }
        }
    }
    #[test]
    fn self_review_and_discovery_share_authority_and_unknown_is_closed() {
        let ps = [
            Principal::LocalUnverified,
            Principal::Agent {
                id: "a".into(),
                gen: 1,
            },
            Principal::Client { id: "c".into() },
        ];
        let access = Access {
            scopes: ["session:read".into()].into(),
            ..Default::default()
        };
        for p in &ps {
            for name in super::super::api::METHODS {
                let c = can(name, p, Some(&access));
                assert_eq!(
                    c.allowed,
                    authorize(name, p, Some(&access)).is_ok(),
                    "{p} {name}"
                );
                let r = method(name).unwrap();
                assert_eq!(
                    methods(p, Some(&access)).contains_key(*name),
                    r.principals.contains(&kind(p))
                );
                if matches!(p, Principal::Agent { .. }) && !r.principals.contains(&A) {
                    assert!(!c.allowed, "{name}");
                }
            }
            assert!(!can("unclassified.read", p, None).allowed);
        }
        assert!(can("diag.detection", &ps[1], None).allowed);
        assert!(can("provider.opencode.status", &ps[1], None).allowed);
        assert!(!topics(&ps[1], None).contains(&"legacy.inbox".into()));
        assert!(!topics(&ps[2], Some(&access)).contains(&"agent.upsert".into()));
        let needs = can("conversation.get", &ps[2], Some(&access))
            .needs
            .unwrap();
        assert_eq!(needs.scopes, ["conversation:read"]);
        assert_eq!(needs.ceiling, Some(Ceiling::Conversation));
    }
    #[test]
    fn audit_never_records_local_reads_or_self_queries() {
        let l = Principal::LocalUnverified;
        let c = Principal::Client { id: "c".into() };
        assert_eq!(
            audit_level("conversation.get", &l, true, Ceiling::Conversation),
            AuditLevel::None
        );
        assert_eq!(
            audit_level("conversation.get", &c, true, Ceiling::Conversation),
            AuditLevel::Metadata
        );
        assert_eq!(
            audit_level("conversation.get", &c, false, Ceiling::Metadata),
            AuditLevel::Metadata
        );
        assert_eq!(
            audit_level("auth.can", &c, false, Ceiling::Metadata),
            AuditLevel::None
        );
        assert_eq!(
            audit_level("session.rename", &l, true, Ceiling::Conversation),
            AuditLevel::Hash
        );
        assert_eq!(
            audit_level("hook.stop", &l, true, Ceiling::Conversation),
            AuditLevel::None
        );
    }
}
