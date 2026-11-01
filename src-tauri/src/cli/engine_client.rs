//! CLI side of the c9 engine: `c9watch engine`, `c9watch engine restart`,
//! `c9watch api <method> '<json>'`, and auto-start.
//!
//! Auto-start uses a client-side spawn
//! lock (`engine.spawn.lock`) makes sure only one caller spawns, and the
//! engine runs in its own session (`setsid`) with an allowlisted
//! environment. The engine counts as up only when `hello` succeeds, which is
//! stricter than the legacy daemon's lock probe.

use crate::engine::home::{engine_home, sock_path};
pub use crate::engine_proto::spawn::{
    engine_env, ensure_engine, log_path, refuse_agent_caller, restart_engine, ConnectError,
    EngineConn, Frames, AGENT_CALLER_REFUSED, ENGINE_ENV_ALLOWLIST, ENGINE_NOT_READY,
};
pub(crate) use crate::engine_proto::spawn::{error_line, process_env, ENV_AGENT_ID};
use std::path::{Path, PathBuf};
use std::time::Duration;

// ── Commands ──────────────────────────────────────────────────────────

pub(crate) fn current_exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("Failed to get current exe path: {}", e))
}

/// `c9watch engine`: run the engine in the foreground until SIGTERM/SIGINT.
pub fn cmd_engine_run() -> Result<(), String> {
    refuse_agent_caller(&process_env)?;
    let config = crate::engine::run::EngineConfig::new(engine_home()?)?;
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| format!("Failed to create tokio runtime: {}", e))?;
    let done = rt.block_on(crate::engine::run::run(&config));
    // The drain's hard deadline bounds the exit too: dropping the runtime
    // would wait for a supervisor stuck in a kill (reviews L4).
    rt.shutdown_timeout(Duration::from_secs(1));
    done
}

/// `c9watch engine restart`
pub fn cmd_engine_restart(pretty: bool) -> Result<(), String> {
    let home = engine_home()?;
    let hello = restart_engine(&home, &current_exe()?)?;
    super::print_json(&hello, pretty)
}

/// `c9watch api <method> '<json>'`: auto-start the engine, send one raw
/// request, print the response frame. An `ok:false` response exits 1.
pub fn cmd_api(method: &str, params: &str, pretty: bool) -> Result<(), String> {
    refuse_agent_caller(&process_env)?;
    let params: serde_json::Value =
        serde_json::from_str(params).map_err(|e| format!("api: params is not JSON: {}", e))?;
    let home = engine_home()?;
    ensure_engine(&home, &current_exe()?)?;
    let mut conn = EngineConn::connect(&sock_path(&home)).map_err(ConnectError::into_msg)?;
    let resp = conn.call(method, params)?;
    super::print_json(&resp, pretty)?;
    if resp.ok {
        Ok(())
    } else {
        Err(error_line(&resp))
    }
}

/// Self queries use a real engine-stamped connection, never CLI assumptions.
pub fn cmd_auth(action: &super::AuthAction, pretty: bool) -> Result<(), String> {
    let home = engine_home()?;
    // An agent can inspect its existing connection, but cannot start core.
    let mut conn = match EngineConn::connect(&sock_path(&home)) {
        Ok(conn) => conn,
        Err(_) => {
            refuse_agent_caller(&process_env)?;
            ensure_engine(&home, &current_exe()?)?;
            EngineConn::connect(&sock_path(&home)).map_err(ConnectError::into_msg)?
        }
    };
    let (method, params) = match action {
        super::AuthAction::CanI { method, .. } => {
            ("auth.can", serde_json::json!({"method": method}))
        }
        super::AuthAction::Rules => ("auth.rules", serde_json::json!({})),
    };
    let resp = conn.call(method, params)?;
    if !resp.ok {
        return Err(error_line(&resp));
    }
    let result = resp.result.unwrap_or_default();
    match action {
        super::AuthAction::CanI { json: false, .. } => {
            let allowed = result["allowed"].as_bool().unwrap_or(false);
            println!("{}", if allowed { "yes" } else { "no" });
            if !allowed {
                return Err(result["reason"]
                    .as_str()
                    .unwrap_or("not allowed")
                    .to_string());
            }
        }
        _ => super::print_json(&result, pretty)?,
    }
    Ok(())
}

/// `c9watch integration status|show`: the adapters the engine knows and
/// their integration files. Reads files only; never starts the engine and
/// never writes a vendor config (managed CC gets its hooks per spawn
/// through `--settings`, D24.1; observed sessions use `c9watch hooks`).
pub fn cmd_integration(action: &super::IntegrationAction, pretty: bool) -> Result<(), String> {
    let home = engine_home()?;
    let report = integration_report(action, &home)?;
    super::print_json(&report, pretty)
}

/// The JSON `cmd_integration` prints for `home`.
pub fn integration_report(
    action: &super::IntegrationAction,
    home: &Path,
) -> Result<serde_json::Value, String> {
    use crate::engine::adapter::{claude_hook_settings, descriptor, DESCRIPTORS};
    use crate::engine::agent_bin::installed_path;
    let bin = installed_path(home);
    match action {
        super::IntegrationAction::Status => {
            let kinds: Vec<serde_json::Value> = DESCRIPTORS
                .iter()
                .map(|d| d.integration_report(&bin))
                .collect();
            Ok(serde_json::json!({ "adapters": kinds }))
        }
        super::IntegrationAction::Show { kind } => match descriptor(kind) {
            Some(d) if d.kind == "claude" => Ok(claude_hook_settings(&bin)),
            Some(d) => Ok(d.integration_report(&bin)),
            _ => Err(format!("integration: unknown adapter kind {:?}", kind)),
        },
    }
}

/// `c9watch engine shadow-report`: the dev GUI's daily shadow diff
/// coverage from `<home>/shadow-coverage.ndjson`, oldest day first, plus
/// where the unexplained diffs are. Reads files only; never starts the
/// engine.
#[cfg(c9_channel_dev)]
pub fn cmd_shadow_report(pretty: bool) -> Result<(), String> {
    use crate::engine_proto::shadow::{read_coverage, DIFF_FILE};
    let home = engine_home()?;
    let days = read_coverage(&home)?;
    let report = serde_json::json!({
        "days": days,
        "diffs": home.join(DIFF_FILE),
    });
    super::print_json(&report, pretty)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn integration_reports_the_adapters_and_the_per_spawn_hooks() {
        use super::super::IntegrationAction;
        use crate::engine::agent_bin::installed_path;
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("engine home");
        let status = integration_report(&IntegrationAction::Status, &home).unwrap();
        let claude = &status["adapters"][0];
        assert_eq!(claude["descriptor"]["kind"], "claude");
        assert_eq!(claude["descriptor"]["provider"], "claudeCode");
        assert_eq!(claude["status"], "missing");
        // The engine installed its agent copy: CC's integration is current.
        let bin = installed_path(&home);
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, b"x").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o555)).unwrap();
        let status = integration_report(&IntegrationAction::Status, &home).unwrap();
        assert_eq!(status["adapters"][0]["status"], "current");
        let show = IntegrationAction::Show {
            kind: "claude".into(),
        };
        let hooks = integration_report(&show, &home).unwrap();
        let stop = hooks["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(
            stop.contains("engine home/bin/c9watch-agent' hook stop"),
            "{stop}"
        );
        let bad = IntegrationAction::Show {
            kind: "grok".into(),
        };
        assert!(integration_report(&bad, &home).is_err());
    }

    #[test]
    fn all_p1_kinds_are_reported_without_installing_or_claiming_unshipped_hooks() {
        use super::super::IntegrationAction;
        use crate::engine::adapter::DESCRIPTORS;
        use crate::engine::agent_bin::installed_path;
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("engine");
        let status = integration_report(&IntegrationAction::Status, &home).unwrap();
        assert!(
            !home.exists(),
            "status must not create an engine or vendor home"
        );
        let adapters = status["adapters"].as_array().unwrap();
        assert_eq!(adapters.len(), 7);
        for (d, actual) in DESCRIPTORS.iter().zip(adapters) {
            assert_eq!(actual["descriptor"]["kind"], d.kind);
            assert_eq!(actual["status"], "missing");
        }
        // Installing CC's agent binary must not mark the other kinds current.
        let bin = installed_path(&home);
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, b"fake helper").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o555)).unwrap();
        let status = integration_report(&IntegrationAction::Status, &home).unwrap();
        assert_eq!(status["adapters"][0]["status"], "current");
        for (i, d) in DESCRIPTORS.iter().enumerate().skip(1) {
            let shown = integration_report(
                &IntegrationAction::Show {
                    kind: d.kind.into(),
                },
                &home,
            )
            .unwrap();
            assert_eq!(shown, status["adapters"][i]);
            assert_eq!(shown["status"], "missing");
            assert_eq!(shown["reason"], "engine_integration_not_implemented");
            assert_eq!(shown["integrationFiles"], serde_json::json!([]));
        }
        assert_eq!(std::fs::read_dir(t.path()).unwrap().count(), 1);
        assert_eq!(std::fs::read_dir(&home).unwrap().count(), 1);
        assert_eq!(std::fs::read_dir(home.join("bin")).unwrap().count(), 1);
        assert_eq!(std::fs::read(&bin).unwrap(), b"fake helper");
    }
}
