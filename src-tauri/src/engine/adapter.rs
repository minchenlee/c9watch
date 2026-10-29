//! Adapter descriptors (plan C1a-22, A:1017-1032): what the engine knows
//! about one agent kind as data, plus the few runtime facts C1a needs for
//! Claude Code, the only kind it runs.
//!
//! - `Descriptor`: kind, executables, tier, hook trust, permission flags,
//!   prompt delivery, path guard, and the hook kinds the engine accepts
//!   for the kind (A:1056).
//! - Integration files: for a managed CC spawn the hooks come per spawn
//!   through `--settings` (D24.1), so CC's integration file is that hooks
//!   block, not a global install. `c9watch integration` shows it and its
//!   status. Observed sessions keep the existing `c9watch hooks` install.
//! - `resume_argv`: `claude --resume <id>` (A:1040).
//!
//! **Deviation from A:1024:** A lists claude as `identity_only`. D24.6
//! (later) makes the Stop hook the first turn-end signal for managed CC, so
//! the CC descriptor is `turn_end`: the hook decides turn end, the
//! transcript and screen the rest.

use std::path::Path;

/// Claude Code runtime pieces: the spawn profile (`adapter/cc/profile.rs`).
pub mod cc {
    pub mod profile;
}

/// `full` | `basic` | `later` (A:1022).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Full,
    Basic,
    Later,
}

/// How far the engine trusts the kind's hooks (A:1023).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HookTrust {
    /// The hook alone decides idle (pi, omp, opencode, kilo).
    Authoritative,
    /// Only the hook's turn end counts (codex; claude per D24.6).
    TurnEnd,
    /// The hook binds identity only.
    IdentityOnly,
}

/// How a prompt reaches the agent (A:1025).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptDelivery {
    BracketedPaste,
    Typed,
    /// First prompt only.
    Argv,
}

/// argv for each permission posture (A:1024).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct PermissionFlags {
    pub manual: &'static [&'static str],
    pub auto: &'static [&'static str],
    pub yolo: &'static [&'static str],
}

/// One agent kind, as data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Descriptor {
    /// Stable id, as hooks send it in `kind`.
    pub kind: &'static str,
    /// The `agents.provider` value: the `SessionProvider` key, so engine
    /// rows and detection agree on the provider (A:1020).
    pub provider: &'static str,
    pub executables: &'static [&'static str],
    pub tier: Tier,
    pub hook_trust: HookTrust,
    pub permission_flags: PermissionFlags,
    pub prompt_delivery: PromptDelivery,
    /// `merge_only` for every adapter (A:1032, D15).
    pub path_guard: &'static str,
    /// Hook `kind` values the engine accepts for this kind, its own first.
    pub hook_kinds: &'static [&'static str],
}

impl Descriptor {
    /// Whether a hook message of `kind` may update an agent of this kind.
    pub fn accepts_hook_kind(&self, kind: &str) -> bool {
        self.hook_kinds.contains(&kind)
    }
}

/// Claude Code.
pub const CLAUDE: Descriptor = Descriptor {
    kind: "claude",
    provider: "claudeCode",
    executables: &["claude"],
    tier: Tier::Full,
    hook_trust: HookTrust::TurnEnd,
    permission_flags: PermissionFlags {
        manual: &["--permission-mode", "default"],
        auto: &["--permission-mode", "auto"],
        yolo: &["--dangerously-skip-permissions"],
    },
    prompt_delivery: PromptDelivery::BracketedPaste,
    path_guard: "merge_only",
    hook_kinds: &["claude"],
};

/// Every kind the engine knows. C1a: Claude Code only.
pub const DESCRIPTORS: &[&Descriptor] = &[&CLAUDE];

/// The descriptor of `kind`.
pub fn descriptor(kind: &str) -> Option<&'static Descriptor> {
    DESCRIPTORS.iter().copied().find(|d| d.kind == kind)
}

/// The descriptor of the kind whose `agents.provider` is `provider`.
pub fn descriptor_for_provider(provider: &str) -> Option<&'static Descriptor> {
    DESCRIPTORS.iter().copied().find(|d| d.provider == provider)
}

/// argv that resumes a CC session (A:1040).
pub fn claude_resume_argv(session_id: &str) -> Vec<String> {
    vec!["claude".into(), "--resume".into(), session_id.into()]
}

/// CC hook events in the per-spawn settings, and the `c9watch-agent hook`
/// event each runs (`cli::task::HOOK_EVENTS`).
pub const CC_HOOKS: &[(&str, &str)] = &[
    ("SessionStart", "session-start"),
    ("Stop", "stop"),
    ("PermissionRequest", "permission-request"),
    // CC 2.1.312: runs after the auto-mode classifier denies a call.
    ("PermissionDenied", "permission-denied"),
    ("Notification", "notification"),
];

/// CC's own hook timeout, in seconds. The client gives up after 0.5 s
/// (A:1054); this only bounds a stuck process.
pub const CC_HOOK_TIMEOUT_S: u64 = 5;

/// Single-quote `s` for the `sh -c` CC runs a hook command in. The engine
/// home may contain a space (`Application Support`).
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// CC's integration file: the `hooks` block of the per-spawn `--settings`
/// file (D24.1). Each hook runs the engine's own `c9watch-agent` copy by
/// absolute path, never a binary the agent can write (D28 Q6=B). The M5
/// spawn profile adds this block to the rest of its settings.
pub fn claude_hook_settings(agent_bin: &Path) -> serde_json::Value {
    let bin = sh_quote(&agent_bin.to_string_lossy());
    let mut hooks = serde_json::Map::new();
    for (cc_event, event) in CC_HOOKS {
        hooks.insert(
            (*cc_event).into(),
            serde_json::json!([{
                "hooks": [{
                    "type": "command",
                    "command": format!("{} hook {} --kind {}", bin, event, CLAUDE.kind),
                    "timeout": CC_HOOK_TIMEOUT_S,
                }]
            }]),
        );
    }
    serde_json::json!({ "hooks": hooks })
}

/// A kind's integration state (A:1029).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationStatus {
    /// Everything the integration needs is in place.
    Current,
    /// A file is there but differs from what the engine would write.
    Outdated,
    Missing,
    /// Another tool owns the file.
    ForeignConflict,
}

/// CC's integration status: the per-spawn hooks run the engine's agent
/// copy, so CC is `current` when that copy is in place and executable, and
/// `missing` otherwise. There is no global file to compare.
pub fn claude_integration_status(agent_bin: &Path) -> IntegrationStatus {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(agent_bin) {
        Ok(m) if m.is_file() && m.permissions().mode() & 0o111 != 0 => IntegrationStatus::Current,
        _ => IntegrationStatus::Missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::task::hook_method;

    #[test]
    fn the_claude_descriptor() {
        let d = descriptor("claude").unwrap();
        assert_eq!(d, &CLAUDE);
        assert!(descriptor("codex").is_none());
        assert_eq!(
            d.provider,
            crate::session::source::SessionProvider::ClaudeCode.as_key()
        );
        assert_eq!(descriptor_for_provider("claudeCode"), Some(d));
        assert!(descriptor_for_provider("claude").is_none());
        assert!(d.accepts_hook_kind("claude"));
        assert!(!d.accepts_hook_kind("grok"));
        assert!(!d.accepts_hook_kind(""));
        assert_eq!(d.path_guard, "merge_only");
        assert_eq!(d.permission_flags.auto, ["--permission-mode", "auto"]);
        assert_eq!(claude_resume_argv("s-1"), vec!["claude", "--resume", "s-1"]);
        let v = serde_json::to_value(d).unwrap();
        assert_eq!(v["hookTrust"], "turn_end");
        assert_eq!(v["promptDelivery"], "bracketed_paste");
    }

    #[test]
    fn hook_settings_run_the_engine_copy_quoted() {
        let bin = Path::new("/Users/me/Library/Application Support/c9/bin/c9watch-agent");
        let s = claude_hook_settings(bin);
        let hooks = s["hooks"].as_object().unwrap();
        assert_eq!(hooks.len(), CC_HOOKS.len());
        for (cc_event, event) in CC_HOOKS {
            // Every event the settings send has a c9p/1 method.
            assert!(hook_method(event).is_some(), "{}", event);
            let h = &hooks[*cc_event][0]["hooks"][0];
            assert_eq!(h["type"], "command");
            assert_eq!(h["timeout"], CC_HOOK_TIMEOUT_S);
            assert_eq!(
                h["command"],
                format!(
                    "'/Users/me/Library/Application Support/c9/bin/c9watch-agent' hook {} --kind claude",
                    event
                )
            );
        }
        let odd = claude_hook_settings(Path::new("/a'b/c9watch-agent"));
        assert!(odd["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .starts_with(r"'/a'\''b/c9watch-agent' hook stop"));
    }

    #[test]
    fn integration_status_follows_the_agent_copy() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let bin = t.path().join("c9watch-agent");
        assert_eq!(claude_integration_status(&bin), IntegrationStatus::Missing);
        std::fs::write(&bin, b"x").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o444)).unwrap();
        assert_eq!(claude_integration_status(&bin), IntegrationStatus::Missing);
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o555)).unwrap();
        assert_eq!(claude_integration_status(&bin), IntegrationStatus::Current);
        assert_eq!(
            claude_integration_status(t.path()),
            IntegrationStatus::Missing
        );
    }
}
