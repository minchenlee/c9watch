//! One compatibility decision for the GUI. Discovery is authoritative;
//! unknown versions and agent state fail closed (no silent restart).
use super::wire::{HelloResult, API_MAJOR, API_MINOR};

pub const GUI_METHODS: &[&str] = &[
    "hello",
    "ping",
    "events.subscribe",
    "events.unsubscribe",
    "agent.list",
    "session.views",
    "session.away",
    "session.stop",
    "session.rename",
    "session.tasks",
    "detect.recheck",
    "diag.detection",
    "conversation.get",
    "history.list",
    "history.search",
    "cost.get",
    "memory.list",
    "subagent.list",
    "subagent.transcript",
    "usage.subscriptions",
    "provider.opencode.status",
    "provider.opencode.connect",
];

pub fn core_usable(hello: &HelloResult) -> bool {
    hello.api_major == API_MAJOR
        && GUI_METHODS.iter().all(|m| hello.methods.contains_key(*m))
        && !GUI_METHODS
            .iter()
            .any(|m| hello.breaking_since.iter().any(|b| b == m))
}

/// An unparseable version may be newer; never downgrade it.
pub fn engine_newer(version: &str, gui_build: &str) -> bool {
    match (
        semver::Version::parse(version),
        semver::Version::parse(gui_build),
    ) {
        (Ok(core), Ok(gui)) => core > gui,
        _ => true,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum CoreAction {
    Use,
    Fallback,
    Restart,
}

pub fn core_action(hello: &HelloResult, gui_build: &str, controlled: Option<bool>) -> CoreAction {
    if core_usable(hello) {
        return CoreAction::Use;
    }
    if engine_newer(&hello.engine_version, gui_build)
        || hello.api_major > API_MAJOR
        || hello.api_minor > API_MINOR
        || controlled != Some(false)
    {
        return CoreAction::Fallback;
    }
    let older = matches!((semver::Version::parse(&hello.engine_version), semver::Version::parse(gui_build)), (Ok(core), Ok(gui)) if core < gui)
        || hello.api_minor < API_MINOR;
    if older {
        CoreAction::Restart
    } else {
        CoreAction::Fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn hello() -> HelloResult {
        HelloResult {
            api_major: API_MAJOR,
            api_minor: API_MINOR,
            engine_version: "0.11.1".into(),
            methods: GUI_METHODS
                .iter()
                .map(|m| (m.to_string(), Default::default()))
                .collect(),
            ..Default::default()
        }
    }
    #[test]
    fn discovery_and_breaks_are_required() {
        let mut h = hello();
        assert!(core_usable(&h));
        h.methods.remove("session.views");
        assert!(!core_usable(&h));
        h = hello();
        h.breaking_since.push("session.views".into());
        assert!(!core_usable(&h));
        h = hello();
        h.api_major += 1;
        assert!(!core_usable(&h));
    }
    #[test]
    fn old_core_waits_for_controlled_agents_and_unknown_state() {
        let mut h = hello();
        h.methods.remove("session.views");
        h.api_minor -= 1;
        assert_eq!(core_action(&h, "0.11.1", Some(true)), CoreAction::Fallback);
        assert_eq!(core_action(&h, "0.11.1", None), CoreAction::Fallback);
        assert_eq!(core_action(&h, "0.11.1", Some(false)), CoreAction::Restart);
    }
    #[test]
    fn never_restart_newer_or_unknown_core() {
        for v in ["0.12.0", "1.0.0", "unrecognized"] {
            let mut h = hello();
            h.engine_version = v.into();
            h.methods.clear();
            assert_eq!(core_action(&h, "0.11.1", Some(false)), CoreAction::Fallback);
        }
        assert!(!engine_newer("0.11.0", "0.11.1"));
    }
    #[test]
    fn newer_compatible_core_is_usable() {
        let mut h = hello();
        h.engine_version = "1.0.0".into();
        assert_eq!(core_action(&h, "0.11.1", Some(true)), CoreAction::Use);
    }
}
