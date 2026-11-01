//! One deny-by-default authority table. The pipeline is frame limits →
//! authentication → authorize → admission → handler → egress → audit.
//! Discovery and self queries consult this table without executing admission.
//!
//! The table is the one answer to "may principal X call method Y": the
//! principal kinds a method takes, and the admission checks (`Admission`)
//! that `api::dispatch` runs, in order, before the handler. Handlers do not
//! repeat them (D9).
use super::principal::Principal;
use super::proto::{
    ApiError, AuthCanResult, AuthNeeds, AuthRulesResult, Ceiling, ErrorCode, MethodInfo,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const L: &str = "local-unverified";
const A: &str = "agent";
const C: &str = "client";
pub(crate) const O: &str = "observed";
const ALL: &[&str] = &[L, A, C, O];

/// A check `api::dispatch` runs after the principal kind passed and before
/// the handler. Each needs connection facts (the peer, the params) that the
/// kind alone does not carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Admission {
    /// The peer is a Unix peer; a gateway peer is refused.
    UnixPeer,
    /// `local-unverified` and `observed:` pass
    /// `principal::check_mutating_peer`: the whole ancestry, no live agent
    /// and no other observed session in it, no sandbox. Any other
    /// principal is refused.
    MutatingPeer,
    /// An `observed:` principal names only its own session in
    /// `params.sessionKey`.
    OwnSession,
    /// `annotations::admit`: force or check the principal's namespace.
    AnnotationNamespace,
    /// `local-unverified` passes the mutating-peer check.
    AnnotationPeer,
    /// `local-unverified` passes the mutating-peer check; an agent reads
    /// only its own mail.
    MailPeer,
    /// `mail::check_sender`.
    MailSender,
    /// The agent names itself and the peer runs the engine's
    /// `c9watch-agent` copy.
    AgentExe,
}

use Admission::*;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    pub resource: &'static str,
    pub verb: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_ceiling: Option<Ceiling>,
    pub principals: &'static [&'static str],
    pub admission: &'static [Admission],
}
fn rule(
    resource: &'static str,
    verb: &'static str,
    principals: &'static [&'static str],
    min_ceiling: Option<Ceiling>,
    admission: &'static [Admission],
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
        "annotation.list" => rule("annotation", "read", ALL, None, &[]),
        "annotation.set" | "annotation.delete" | "annotation.patch" => rule(
            "annotation",
            "write",
            ALL,
            None,
            &[AnnotationNamespace, AnnotationPeer],
        ),
        "agent.list" => rule("agent", "read", ALL, None, &[]),
        // P10 opens these to agents; until then only the terminal (D8).
        "agent.create" | "agent.retire" | "agent.resume" => {
            rule("agent", "write", &[L], None, &[UnixPeer, MutatingPeer])
        }
        "detect.recheck" => rule("detect", "write", &[L], None, &[UnixPeer, MutatingPeer]),
        // An observed session may stop itself, never another (D8).
        "session.stop" => rule(
            "session",
            "stop",
            &[L, C, O],
            None,
            &[OwnSession, MutatingPeer],
        ),
        "session.rename" => rule("session", "rename", &[L, C], None, &[MutatingPeer]),
        // Existing diagnostic access for agents is deliberately retained.
        "provider.opencode.status" => rule("provider", "read", &[L, A, O], None, &[]),
        "provider.opencode.connect" => {
            rule("provider", "write", &[L], None, &[UnixPeer, MutatingPeer])
        }
        "conversation.get" | "subagent.transcript" | "session.tasks" => rule(
            "conversation",
            "read",
            &[L, C, O],
            Some(Ceiling::Conversation),
            &[],
        ),
        "history.list" => rule(
            "history",
            "read",
            &[L, C, O],
            Some(Ceiling::Conversation),
            &[],
        ),
        "history.search" => rule("history", "read", &[L, O], Some(Ceiling::Conversation), &[]),
        "memory.list" => rule(
            "memory",
            "read",
            &[L, C, O],
            Some(Ceiling::Conversation),
            &[],
        ),
        "cost.get" | "usage.subscriptions" | "subagent.list" => {
            rule("cost", "read", &[L, C, O], None, &[])
        }
        // An observed session has no mailbox and controls none.
        "msg.list" | "msg.read" => rule("msg", "read", &[L, A], None, &[UnixPeer, MailPeer]),
        "msg.cancel" => rule("msg", "cancel", &[L], None, &[UnixPeer, MutatingPeer]),
        "mail.pause" | "mail.resume" => {
            rule("mail", "write", &[L], None, &[UnixPeer, MutatingPeer])
        }
        // The message names an observed sender as such (D8).
        "msg.send" => rule("msg", "send", &[L, A, O], None, &[UnixPeer, MailSender]),
        "hook.session_start"
        | "hook.stop"
        | "hook.permission_request"
        | "hook.permission_denied"
        | "hook.notification"
        | "hook.user_prompt_submit"
        | "hook.post_tool_use"
        | "task.done"
        | "agent.handoff" => rule("hook", "write", &[A], None, &[UnixPeer, AgentExe]),
        _ => return None,
    })
}

pub fn topic(name: &str) -> Option<Rule> {
    Some(match name {
        "session.view"
        | "session.gone"
        | "notification.needs_attention"
        | "notification.waiting_for_input"
        | "diag.detection"
        | "detect.health" => rule("session", "read", ALL, None, &[]),
        "agent.upsert" | "agent.attention" | "agent.turn_end" | "agent.gone" | "agent.failed" => {
            rule("agent", "read", ALL, None, &[])
        }
        "annotation.changed" | "annotation.removed" => rule("annotation", "read", ALL, None, &[]),
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
        Principal::Observed { .. } => O,
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
    // Discovery uses the agent-only declaration. Preserve the established
    // local hook refusal code; dispatch audits this denial exactly once.
    let code = if matches!(principal, Principal::LocalUnverified)
        && method(name).is_some_and(|r| r.admission.contains(&AgentExe))
    {
        ErrorCode::PeerNotAuthorized
    } else {
        ErrorCode::NotAllowed
    };
    Err(
        ApiError::new(code, checked.reason.clone().unwrap_or_default())
            .with_data(serde_json::json!({"needs": checked.needs})),
    )
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
    if (name == "annotation.list" && !allowed)
        || matches!(
            name,
            "annotation.set" | "annotation.delete" | "annotation.patch"
        )
    {
        return AuditLevel::Hash;
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

/// Every scope that can be granted to a gateway token. Presets expand once.
pub const SCOPES: &[&str] = &[
    "session:read",
    "session:stop",
    "session:rename",
    "conversation:read",
    "history:read",
    "memory:read",
    "cost:read",
    "agent:read",
    "annotation:read",
    "annotation:write",
];

pub fn subset(
    principal: &Principal,
    scopes: &BTreeSet<String>,
    ceiling: Ceiling,
    policy: &crate::gateway::policy::RemoteAccess,
) -> Result<(), ApiError> {
    if !matches!(principal, Principal::LocalUnverified) {
        return Err(ApiError::new(
            ErrorCode::NotAllowed,
            "only a local grantor can grant authority",
        ));
    }
    for scope in scopes {
        let permitted = SCOPES.contains(&scope.as_str())
            && match scope.as_str() {
                "conversation:read" => policy.allow_conversation,
                "history:read" => policy.allow_history,
                "memory:read" => policy.allow_memory,
                _ => true,
            };
        if !permitted {
            return Err(ApiError::new(
                ErrorCode::NotAllowed,
                "grant exceeds the machine policy",
            ));
        }
        if matches!(
            scope.as_str(),
            "conversation:read" | "history:read" | "memory:read"
        ) && ceiling < Ceiling::Conversation
        {
            return Err(ApiError::new(
                ErrorCode::NotAllowed,
                "content scopes require a conversation ceiling",
            ));
        }
    }
    if ceiling == Ceiling::Conversation && !policy.allow_conversation {
        return Err(ApiError::new(
            ErrorCode::NotAllowed,
            "grant exceeds the machine ceiling",
        ));
    }
    Ok(())
}

pub use crate::gateway::policy::is_widening;

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
            Principal::Observed {
                session_key: "claudeCode:s".into(),
            },
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
    fn hook_discovery_is_agent_only_and_local_refusals_keep_their_code() {
        let local = Principal::LocalUnverified;
        let agent = Principal::Agent {
            id: "a".into(),
            gen: 1,
        };
        let client = Principal::Client { id: "c".into() };
        for name in super::super::api::METHODS
            .iter()
            .filter(|m| super::super::proto::is_hook_class(m))
        {
            assert_eq!(method(name).unwrap().principals, &[A]);
            for p in [&local, &client] {
                assert!(!can(name, p, None).allowed, "{p}: {name}");
                assert!(!methods(p, None).contains_key(*name), "{p}: {name}");
                assert!(!rules(p, None).methods.contains_key(*name), "{p}: {name}");
            }
            assert_eq!(
                authorize(name, &local, None).unwrap_err().code,
                ErrorCode::PeerNotAuthorized
            );
            assert_eq!(
                authorize(name, &client, None).unwrap_err().code,
                ErrorCode::NotAllowed
            );
            assert!(can(name, &agent, None).allowed, "{name}");
            assert!(methods(&agent, None).contains_key(*name), "{name}");
        }
    }

    #[test]
    fn mail_is_local_only_and_controls_are_human_only() {
        let local = Principal::LocalUnverified;
        let agent = Principal::Agent {
            id: "a".into(),
            gen: 1,
        };
        let client = Principal::Client { id: "c".into() };
        let access = Access {
            scopes: ["msg:read", "msg:cancel", "mail:write"]
                .map(String::from)
                .into(),
            ceiling: Ceiling::Conversation,
            ..Default::default()
        };
        for name in [
            "msg.list",
            "msg.read",
            "msg.cancel",
            "mail.pause",
            "mail.resume",
        ] {
            assert!(can(name, &local, None).allowed, "{name}");
            assert_eq!(
                can(name, &agent, None).allowed,
                matches!(name, "msg.list" | "msg.read"),
                "{name}"
            );
            assert!(!can(name, &client, Some(&access)).allowed, "{name}");
            assert!(
                !methods(&client, Some(&access)).contains_key(name),
                "{name}"
            );
        }
        assert!(!topics(&client, Some(&access)).contains(&"msg.state".into()));
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
    #[test]
    fn subset_prevents_granting_past_principal_and_policy() {
        let scopes = ["session:read".into(), "conversation:read".into()].into();
        let mut policy = crate::gateway::policy::RemoteAccess::default();
        let local = Principal::LocalUnverified;
        assert!(subset(&local, &scopes, Ceiling::Conversation, &policy).is_err());
        policy.allow_conversation = true;
        assert!(subset(&local, &scopes, Ceiling::Conversation, &policy).is_ok());
        assert!(subset(&local, &scopes, Ceiling::Metadata, &policy).is_err());
        for p in [
            Principal::Client { id: "c".into() },
            Principal::Agent {
                id: "a".into(),
                gen: 1,
            },
        ] {
            assert_eq!(
                subset(&p, &scopes, Ceiling::Conversation, &policy)
                    .unwrap_err()
                    .code,
                ErrorCode::NotAllowed
            );
        }
        assert!(subset(
            &local,
            &["gateway:write".into()].into(),
            Ceiling::Metadata,
            &policy
        )
        .is_err());
        assert!(subset(
            &local,
            &["history:read".into()].into(),
            Ceiling::Conversation,
            &policy
        )
        .is_err());
    }
    #[test]
    fn widening_is_mechanical_and_new_fields_fail_closed() {
        use serde_json::json;
        let old = json!({"scopes":["session:read"],"ceiling":"metadata","expiresAt":1000,"lanEnabled":false,"allowedOrigins":[],"extraAllowedHosts":[]});
        assert!(!is_widening(&old, &old));
        for (key, value) in [
            ("scopes", json!(["session:read", "session:stop"])),
            ("ceiling", json!("conversation")),
            ("expiresAt", json!(1001)),
            ("lanEnabled", json!(true)),
            ("allowedOrigins", json!(["https://view.example:443"])),
            ("extraAllowedHosts", json!(["box.example"])),
        ] {
            let mut next = old.clone();
            next[key] = value;
            assert!(is_widening(&old, &next), "{key}");
        }
        let narrow = json!({"scopes":[],"ceiling":"public","expiresAt":999,"lanEnabled":false,"allowedOrigins":[],"extraAllowedHosts":[]});
        assert!(!is_widening(&old, &narrow));
        assert!(
            is_widening(&old, &json!({"futureFlag":false})),
            "unknown field cannot silently skip confirmation"
        );
        let policy = serde_json::to_value(crate::gateway::policy::RemoteAccess::default()).unwrap();
        assert!(
            !is_widening(&policy, &policy),
            "every serialized policy field must be explicitly classified"
        );
    }
    #[test]
    fn every_scope_gated_method_and_topic_requires_its_own_scope_and_ceiling() {
        let client = Principal::Client { id: "c".into() };
        for name in super::super::api::METHODS {
            let r = method(name).unwrap();
            let full = Access {
                scopes: SCOPES.iter().map(|s| s.to_string()).collect(),
                ceiling: Ceiling::Conversation,
                ..Default::default()
            };
            if !r.principals.contains(&C) {
                assert!(!can(name, &client, Some(&full)).allowed, "{name}");
                continue;
            }
            if r.resource == "protocol" {
                continue;
            }
            let needed = format!("{}:{}", r.resource, r.verb);
            let mut access = full.clone();
            access.scopes.remove(&needed);
            let review = can(name, &client, Some(&access));
            assert!(!review.allowed, "{name}");
            assert_eq!(review.needs.unwrap().scopes, [needed.clone()], "{name}");
            access.scopes.insert(needed);
            assert!(can(name, &client, Some(&access)).allowed, "{name}");
            if r.min_ceiling == Some(Ceiling::Conversation) {
                access.ceiling = Ceiling::Metadata;
                assert!(!can(name, &client, Some(&access)).allowed, "{name}");
            }
        }
        for (name, _) in super::super::discovery::TOPICS {
            let r = topic(name).unwrap();
            if !r.principals.contains(&C) {
                assert!(!review(Some(&r), &client, None).allowed);
                continue;
            }
            let access = Access {
                scopes: [format!("{}:{}", r.resource, r.verb)].into(),
                ..Default::default()
            };
            assert!(review(Some(&r), &client, Some(&access)).allowed, "{name}");
            assert!(!review(Some(&r), &client, None).allowed, "{name}");
        }
    }

    /// D8: an observed session reads as the terminal does, sends mail and
    /// stops (its own) session, and never creates, retires or controls.
    #[test]
    fn an_observed_session_is_narrower_than_the_terminal() {
        let o = Principal::Observed {
            session_key: "claudeCode:s".into(),
        };
        for name in [
            "agent.create",
            "agent.retire",
            "detect.recheck",
            "session.rename",
            "provider.opencode.connect",
            "msg.list",
            "msg.read",
            "msg.cancel",
            "mail.pause",
            "mail.resume",
            "hook.stop",
        ] {
            let e = authorize(name, &o, None).unwrap_err();
            assert_eq!(e.code, ErrorCode::NotAllowed, "{name}");
        }
        for name in [
            "msg.send",
            "session.stop",
            "session.views",
            "history.search",
        ] {
            assert!(can(name, &o, None).allowed, "{name}");
        }
        assert!(method("session.stop")
            .unwrap()
            .admission
            .contains(&OwnSession));
        assert!(!topics(&o, None).contains(&"msg.state".into()));
        assert!(topics(&o, None).contains(&"session.view".into()));
    }

    /// D9: the checks that used to sit in handlers are table rows.
    #[test]
    fn the_table_declares_every_admission_check() {
        let needs = |name: &str, a: Admission| method(name).unwrap().admission.contains(&a);
        for name in super::super::api::METHODS {
            let r = method(name).unwrap();
            // Whatever mutates for the terminal checks the whole peer.
            if r.principals == [L] && r.verb != "read" {
                assert!(needs(name, MutatingPeer), "{name}");
            }
            if r.admission.contains(&MutatingPeer)
                && !matches!(*name, "session.stop" | "session.rename")
            {
                assert!(needs(name, UnixPeer), "{name}");
            }
            if r.principals.contains(&O) && r.verb != "read" {
                assert!(
                    r.admission
                        .iter()
                        .any(|a| matches!(a, MutatingPeer | MailSender | AnnotationNamespace)),
                    "{name}"
                );
            }
        }
        for name in ["msg.list", "msg.read"] {
            assert!(needs(name, UnixPeer) && needs(name, MailPeer), "{name}");
        }
        assert!(needs("msg.send", UnixPeer) && needs("msg.send", MailSender));
        for name in super::super::proto::HOOK_METHODS {
            assert!(needs(name, UnixPeer) && needs(name, AgentExe), "{name}");
        }
        assert!(needs("task.done", AgentExe) && needs("agent.handoff", AgentExe));
        assert_eq!(method("agent.create").unwrap().principals, &[L]);
        assert_eq!(method("agent.retire").unwrap().principals, &[L]);
    }
}
