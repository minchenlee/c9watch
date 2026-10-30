//! `spawn --mode free` as an engine client (C1a-38).
//!
//! `--mode managed`, the default, keeps the old `claude -p` / bg worker
//! path until C1b. `--mode free` auto-starts the engine and calls
//! `agent.create` with the cwd and the name. The engine answers once the
//! agent is `ready` or has failed, so the call waits up to the engine's
//! ready timeout plus a margin, longer than the usual 5 s read timeout.
//!
//! Until C1a-35 the engine answers `experimental_disabled` unless it runs
//! with `C9WATCH_EXPERIMENTAL_CREATE=1`. A caller inside an engine agent
//! (`C9_AGENT_ID` set) gets `not_authorized` here, without a connection:
//! C1a has no orch, and only an orch may create agents (A:262, §1.1).

use super::engine_client::{current_exe, ensure_engine, EngineConn, ENV_AGENT_ID};
use crate::engine::home::{engine_home, sock_path};
use crate::engine::proto::{AgentCreateParams, Response};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Error code: an agent (`C9_AGENT_ID` set) may not create agents in C1a.
pub const NOT_AUTHORIZED: &str = "not_authorized";

/// How long `spawn --mode free` waits for the `agent.create` response: the
/// engine's ready timeout (30 s) plus room for the spawn and the reply.
pub const CREATE_TIMEOUT: Duration =
    Duration::from_secs(crate::engine::cc_screen::READY_TIMEOUT.as_secs() + 15);

/// `spawn --mode`.
#[derive(clap::ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SpawnMode {
    /// A c9watch worker (`claude -p` or bg), through the legacy daemon
    #[default]
    Managed,
    /// An engine agent in the given cwd: no worktree, no task (`agent.create`)
    Free,
}

/// The `spawn` flags that only `--mode managed` uses, as given.
#[derive(Debug, Default)]
pub struct ManagedFlags<'a> {
    pub append_system_prompt: bool,
    pub append_system_prompt_file: bool,
    pub permission_mode: &'a str,
    pub model: bool,
    pub add_dir: bool,
    pub prompt: bool,
}

/// The default of `spawn --permission-mode`.
pub const DEFAULT_PERMISSION_MODE: &str = "bypassPermissions";

/// Fail when a managed-only flag is given with `--mode free`: the engine
/// sets the profile of a free agent itself (C1a-23).
pub fn check_free_flags(f: &ManagedFlags) -> Result<(), String> {
    let given: Vec<&str> = [
        (f.append_system_prompt, "--append-system-prompt"),
        (f.append_system_prompt_file, "--append-system-prompt-file"),
        (
            f.permission_mode != DEFAULT_PERMISSION_MODE,
            "--permission-mode",
        ),
        (f.model, "--model"),
        (f.add_dir, "--add-dir"),
        (f.prompt, "--prompt"),
    ]
    .into_iter()
    .filter_map(|(set, name)| set.then_some(name))
    .collect();
    if given.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "bad_args: --mode free takes only --cwd and --name, not {}",
            given.join(", ")
        ))
    }
}

/// Fail with `not_authorized` when `C9_AGENT_ID` is set to anything but "".
pub fn refuse_agent_caller(env: &dyn Fn(&str) -> Option<OsString>) -> Result<(), String> {
    match env(ENV_AGENT_ID).filter(|v| !v.is_empty()) {
        Some(id) => Err(format!(
            "{}: {} is set ({}); an agent may not create agents in C1a (only an orch may)",
            NOT_AUTHORIZED,
            ENV_AGENT_ID,
            id.to_string_lossy()
        )),
        None => Ok(()),
    }
}

/// The absolute cwd to send: `cwd` against `base` when it is relative, or
/// `base` itself. The engine takes the realpath.
pub fn absolute_cwd(cwd: Option<&str>, base: &Path) -> PathBuf {
    match cwd {
        Some(c) if Path::new(c).is_absolute() => PathBuf::from(c),
        Some(c) => base.join(c),
        None => base.to_path_buf(),
    }
}

/// The `agent.create` params of a free agent.
pub fn create_params(cwd: &Path, name: Option<&str>) -> serde_json::Value {
    serde_json::to_value(AgentCreateParams {
        kind: "claude".into(),
        mode: "free".into(),
        cwd: cwd.to_string_lossy().into_owned(),
        name: name.map(str::to_string),
    })
    .expect("AgentCreateParams serializes")
}

/// `"<code>: <msg>; <hint>"` of an error response.
fn error_with_hint(resp: &Response) -> String {
    match &resp.error {
        Some(e) => format!("{}: {}; {}", e.code, e.msg, e.hint),
        None => "the engine sent ok:false without an error".into(),
    }
}

/// `c9watch spawn --mode free`: print `{"agent": <row>}` once the agent is
/// ready. Any engine error exits 1 with its code.
pub fn cmd_spawn_free(cwd: Option<&str>, name: Option<&str>, pretty: bool) -> Result<(), String> {
    refuse_agent_caller(&|k| std::env::var_os(k))?;
    let base =
        std::env::current_dir().map_err(|e| format!("Failed to get current directory: {}", e))?;
    let cwd = absolute_cwd(cwd, &base);
    let home = engine_home()?;
    ensure_engine(&home, &current_exe()?)?;
    let mut conn = EngineConn::connect(&sock_path(&home)).map_err(|e| e.into_msg())?;
    conn.set_read_timeout(CREATE_TIMEOUT);
    let resp = conn.call("agent.create", create_params(&cwd, name))?;
    if !resp.ok {
        return Err(error_with_hint(&resp));
    }
    super::print_json(&resp.result.unwrap_or_default(), pretty)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[test]
    fn an_agent_caller_is_not_authorized() {
        let err = refuse_agent_caller(&env_of(&[("C9_AGENT_ID", "agt_1")])).unwrap_err();
        assert!(err.starts_with("not_authorized: "), "{}", err);
        assert!(err.contains("agt_1"), "{}", err);
        assert!(refuse_agent_caller(&env_of(&[("C9_AGENT_ID", "")])).is_ok());
        assert!(refuse_agent_caller(&env_of(&[])).is_ok());
    }

    #[test]
    fn the_create_waits_past_the_ready_timeout() {
        assert!(CREATE_TIMEOUT > crate::engine::cc_screen::READY_TIMEOUT);
        assert!(CREATE_TIMEOUT <= Duration::from_secs(60));
    }

    #[test]
    fn the_cwd_is_absolute() {
        let base = Path::new("/w/base");
        assert_eq!(absolute_cwd(None, base), PathBuf::from("/w/base"));
        assert_eq!(
            absolute_cwd(Some("sub"), base),
            PathBuf::from("/w/base/sub")
        );
        assert_eq!(absolute_cwd(Some("/x"), base), PathBuf::from("/x"));
    }

    #[test]
    fn create_params_are_a_free_claude_agent() {
        let p = create_params(Path::new("/w"), Some("ann"));
        assert_eq!(
            p,
            serde_json::json!({"kind": "claude", "mode": "free", "cwd": "/w", "name": "ann"})
        );
        let p = create_params(Path::new("/w"), None);
        assert!(p.get("name").is_none(), "{}", p);
    }

    #[test]
    fn free_mode_refuses_the_managed_flags() {
        let none = ManagedFlags {
            permission_mode: DEFAULT_PERMISSION_MODE,
            ..Default::default()
        };
        assert!(check_free_flags(&none).is_ok());
        let some = ManagedFlags {
            permission_mode: "plan",
            prompt: true,
            ..Default::default()
        };
        let err = check_free_flags(&some).unwrap_err();
        assert_eq!(
            err,
            "bad_args: --mode free takes only --cwd and --name, not --permission-mode, --prompt"
        );
    }
}
