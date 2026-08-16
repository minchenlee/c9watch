use super::detector::encode_path_for_matching;
use super::source::{
    CliActivity, DetectedSession, DetectionDiagnostics, SessionDetectorError, SessionKind,
    SessionSource,
};
use serde::Deserialize;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

#[derive(Deserialize, Debug)]
struct CliAgent {
    pid: u32,
    cwd: PathBuf,
    // `kind` was added alongside background-pinned sessions in CC 2.1.147.
    // 2.1.145–146 emit `claude agents --json` without it; default to "interactive".
    #[serde(default = "default_kind")]
    kind: String,
    #[serde(rename = "startedAt")]
    started_at: i64,
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

fn default_kind() -> String {
    "interactive".to_string()
}

pub struct CliSessionSource {
    claude_bin: PathBuf,
    path_cache: HashMap<String, PathBuf>,
}

impl CliSessionSource {
    pub fn new() -> Self {
        // Relies on PATH lookup at spawn time. We don't pull in `which` as a
        // direct dep just for this — the probe already verified `claude` is on PATH.
        Self {
            claude_bin: PathBuf::from("claude"),
            path_cache: HashMap::new(),
        }
    }

    fn project_path_for_session(&mut self, cwd: &Path, session_id: &str) -> PathBuf {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        lookup_with_cache(&home, &mut self.path_cache, cwd, session_id)
    }

    fn map_agent_to_session(&mut self, a: CliAgent) -> DetectedSession {
        let project_path = self.project_path_for_session(&a.cwd, &a.session_id);
        DetectedSession {
            pid: a.pid,
            project_name: a
                .cwd
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            session_id: Some(a.session_id),
            project_path,
            kind: match a.kind.as_str() {
                "interactive" => SessionKind::Interactive,
                "background" => SessionKind::Background,
                _ => SessionKind::Unknown,
            },
            started_at_ms: Some(a.started_at),
            official_name: a.name,
            cli_activity: match a.status.as_deref() {
                Some("busy") => Some(CliActivity::Busy),
                Some("idle") => Some(CliActivity::Idle),
                _ => None,
            },
            cwd: a.cwd,
            provider: super::source::SessionProvider::ClaudeCode,
            surface: super::source::SessionSurface::ClaudeCode,
            agent_kind: super::source::AgentKind::Root,
            parent_thread_id: None,
            root_session_id: None,
            agent_path: None,
            agent_nickname: None,
            agent_role: None,
            internal_kind: None,
            can_open: true,
            can_stop: true,
            can_rename: true,
            codex_summary: None,
        }
    }
}

impl SessionSource for CliSessionSource {
    fn detect(
        &mut self,
    ) -> Result<(Vec<DetectedSession>, DetectionDiagnostics), SessionDetectorError> {
        let mut child = Command::new(&self.claude_bin)
            .args(["agents", "--json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| SessionDetectorError::CliFailed(format!("spawn: {e}")))?;

        // Drain stdout on a background thread to avoid pipe-buffer deadlock
        // when the child writes more than the OS pipe buffer (typically 64KB).
        // The reader exits naturally when the child closes stdout (on exit or kill).
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| SessionDetectorError::CliFailed("stdout pipe missing".to_string()))?;
        let reader = std::thread::spawn(move || {
            let mut stdout = stdout;
            let mut buf = Vec::new();
            let _ = stdout.read_to_end(&mut buf);
            buf
        });

        let timeout = Duration::from_secs(2);
        let status = match child
            .wait_timeout(timeout)
            .map_err(|e| SessionDetectorError::CliFailed(format!("wait: {e}")))?
        {
            Some(s) => s,
            None => {
                let _ = child.kill();
                // Reap to avoid leaving a zombie on Unix; then join the reader
                // (kill closes the pipe so the reader exits naturally).
                let _ = child.wait();
                let _ = reader.join();
                return Err(SessionDetectorError::Timeout(timeout.as_millis()));
            }
        };

        let buf = reader
            .join()
            .map_err(|_| SessionDetectorError::CliFailed("stdout reader panicked".to_string()))?;

        if !status.success() {
            return Err(SessionDetectorError::CliFailed(format!(
                "exit code {status:?}"
            )));
        }

        let agents: Vec<CliAgent> =
            serde_json::from_slice(&buf).map_err(|e| SessionDetectorError::Parse(e.to_string()))?;

        // Filter out non-CLI entrypoints (e.g. sdk-ts from Zed/IDE integrations).
        // `claude agents --json` lists every live agent including SDK-driven ones,
        // but those don't write project JSONLs and aren't what c9watch monitors.
        // The per-pid metadata at ~/.claude/sessions/<pid>.json carries `entrypoint`.
        // If the file is missing or unreadable, keep the agent (older CC versions).
        let sessions: Vec<DetectedSession> = agents
            .into_iter()
            .filter(|a| is_cli_entrypoint(a.pid))
            .map(|a| self.map_agent_to_session(a))
            .collect();

        Ok((sessions, DetectionDiagnostics::default()))
    }

    fn backend_name(&self) -> &'static str {
        "cli"
    }
}

/// Returns true if the agent at this pid was launched as a `claude` CLI (not via
/// the TypeScript/Python SDK). Reads `~/.claude/sessions/<pid>.json` and inspects
/// the `entrypoint` field. Missing/unreadable file → keep (older CC builds didn't
/// write this metadata; better to over-report than drop real CLIs).
fn is_cli_entrypoint(pid: u32) -> bool {
    match dirs::home_dir() {
        Some(home) => is_cli_entrypoint_under(&home, pid),
        None => true,
    }
}

fn is_cli_entrypoint_under(home: &Path, pid: u32) -> bool {
    let path = home
        .join(".claude")
        .join("sessions")
        .join(format!("{pid}.json"));
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return true,
    };
    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return true,
    };
    match value.get("entrypoint").and_then(|v| v.as_str()) {
        Some(ep) => ep == "cli",
        None => true,
    }
}

/// Stateless resolver used by both production code (with real `home_dir()`) and
/// tests (with a tempdir as `home`). Encoded-cwd fast-path then directory scan.
fn resolve_project_path_under(home: &Path, cwd: &Path, session_id: &str) -> Option<PathBuf> {
    let projects_root = home.join(".claude").join("projects");
    let encoded = encode_path_for_matching(&cwd.to_string_lossy());
    let fast = projects_root.join(&encoded);
    if fast.join(format!("{session_id}.jsonl")).is_file() {
        return Some(fast);
    }
    let entries = std::fs::read_dir(&projects_root).ok()?;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() && p.join(format!("{session_id}.jsonl")).is_file() {
            return Some(p);
        }
    }
    None
}

fn fallback_path_under(home: &Path, cwd: &Path) -> PathBuf {
    let encoded = encode_path_for_matching(&cwd.to_string_lossy());
    home.join(".claude").join("projects").join(encoded)
}

/// Cache-aware resolver. Verifies stale cache entries (jsonl gone) and re-resolves.
/// Falls back to `fallback_path_under` when resolution fails so enrichment has SOMETHING
/// to try (and skip cleanly when JSONL never appears).
fn lookup_with_cache(
    home: &Path,
    cache: &mut HashMap<String, PathBuf>,
    cwd: &Path,
    session_id: &str,
) -> PathBuf {
    if let Some(cached) = cache.get(session_id).cloned() {
        if cached.join(format!("{session_id}.jsonl")).is_file() {
            return cached;
        }
        cache.remove(session_id);
    }
    if let Some(resolved) = resolve_project_path_under(home, cwd, session_id) {
        cache.insert(session_id.to_string(), resolved.clone());
        return resolved;
    }
    fallback_path_under(home, cwd)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_schema_json() -> &'static str {
        r#"[
          {"pid":1,"cwd":"/tmp/a","kind":"interactive","startedAt":100,"sessionId":"sid-a","status":"busy"},
          {"pid":2,"cwd":"/tmp/b","kind":"background","startedAt":200,"sessionId":"sid-b","name":"my-bg","status":"idle"}
        ]"#
    }

    #[test]
    fn parse_cli_output_full_schema() {
        let agents: Vec<CliAgent> = serde_json::from_str(full_schema_json()).unwrap();
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].pid, 1);
        assert_eq!(agents[0].session_id, "sid-a");
        assert_eq!(agents[1].name.as_deref(), Some("my-bg"));
    }

    #[test]
    fn parse_cli_output_missing_status_yields_none_in_mapping() {
        let json = r#"[{"pid":1,"cwd":"/tmp","kind":"interactive","startedAt":1,"sessionId":"x"}]"#;
        let agents: Vec<CliAgent> = serde_json::from_str(json).unwrap();
        assert!(agents[0].status.is_none());
        let mapped_activity = match agents[0].status.as_deref() {
            Some("busy") => Some(CliActivity::Busy),
            Some("idle") => Some(CliActivity::Idle),
            _ => None,
        };
        assert!(mapped_activity.is_none());
    }

    #[test]
    fn parse_cli_output_busy_yields_some_busy() {
        let json = r#"[{"pid":1,"cwd":"/tmp","kind":"interactive","startedAt":1,"sessionId":"x","status":"busy"}]"#;
        let agents: Vec<CliAgent> = serde_json::from_str(json).unwrap();
        let mapped = match agents[0].status.as_deref() {
            Some("busy") => Some(CliActivity::Busy),
            Some("idle") => Some(CliActivity::Idle),
            _ => None,
        };
        assert_eq!(mapped, Some(CliActivity::Busy));
    }

    #[test]
    fn parse_cli_output_unknown_status_yields_none() {
        let json = r#"[{"pid":1,"cwd":"/tmp","kind":"interactive","startedAt":1,"sessionId":"x","status":"on_fire"}]"#;
        let agents: Vec<CliAgent> = serde_json::from_str(json).unwrap();
        let mapped = match agents[0].status.as_deref() {
            Some("busy") => Some(CliActivity::Busy),
            Some("idle") => Some(CliActivity::Idle),
            _ => None,
        };
        assert!(mapped.is_none());
    }

    #[test]
    fn parse_cli_output_unknown_kind_yields_unknown() {
        let json = r#"[{"pid":1,"cwd":"/tmp","kind":"chimera","startedAt":1,"sessionId":"x"}]"#;
        let agents: Vec<CliAgent> = serde_json::from_str(json).unwrap();
        let mapped_kind = match agents[0].kind.as_str() {
            "interactive" => SessionKind::Interactive,
            "background" => SessionKind::Background,
            _ => SessionKind::Unknown,
        };
        assert_eq!(mapped_kind, SessionKind::Unknown);
    }

    #[test]
    fn parse_cli_output_missing_kind_defaults_to_interactive() {
        // CC 2.1.145–146 emit `claude agents --json` without the `kind` field.
        let json = r#"[{"pid":1,"cwd":"/tmp","startedAt":1,"sessionId":"x"}]"#;
        let agents: Vec<CliAgent> = serde_json::from_str(json).unwrap();
        assert_eq!(agents[0].kind, "interactive");
    }

    #[test]
    fn parse_cli_output_empty_array() {
        let agents: Vec<CliAgent> = serde_json::from_str("[]").unwrap();
        assert!(agents.is_empty());
    }

    #[test]
    fn parse_cli_output_malformed_returns_err() {
        let result: Result<Vec<CliAgent>, _> = serde_json::from_str("not json");
        assert!(result.is_err());
    }

    #[test]
    fn resolve_project_path_finds_via_fast_path() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let cwd = PathBuf::from("/Users/test/proj");
        let session_id = "sess-fast";
        let encoded = encode_path_for_matching(&cwd.to_string_lossy());
        let proj_dir = home.join(".claude").join("projects").join(&encoded);
        std::fs::create_dir_all(&proj_dir).unwrap();
        std::fs::write(proj_dir.join(format!("{session_id}.jsonl")), b"").unwrap();

        let result = resolve_project_path_under(home, &cwd, session_id);
        assert_eq!(result.as_deref(), Some(proj_dir.as_path()));
    }

    #[test]
    fn resolve_project_path_falls_back_to_scan_when_encoding_mismatches() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let cwd = PathBuf::from("/Users/test/proj");
        let session_id = "sess-scan";
        let wrong_dir = home
            .join(".claude")
            .join("projects")
            .join("totally-different-dir");
        std::fs::create_dir_all(&wrong_dir).unwrap();
        std::fs::write(wrong_dir.join(format!("{session_id}.jsonl")), b"").unwrap();

        let result = resolve_project_path_under(home, &cwd, session_id);
        assert_eq!(result.as_deref(), Some(wrong_dir.as_path()));
    }

    #[test]
    fn resolve_project_path_returns_none_when_jsonl_missing_everywhere() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let cwd = PathBuf::from("/Users/test/proj");
        let result = resolve_project_path_under(home, &cwd, "absent");
        assert!(result.is_none());
    }

    #[test]
    fn path_cache_returns_cached_value_when_jsonl_still_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let cwd = PathBuf::from("/Users/test/proj");
        let session_id = "cache-hit";
        let encoded = encode_path_for_matching(&cwd.to_string_lossy());
        let proj_dir = home.join(".claude").join("projects").join(&encoded);
        std::fs::create_dir_all(&proj_dir).unwrap();
        std::fs::write(proj_dir.join(format!("{session_id}.jsonl")), b"").unwrap();

        let mut cache: HashMap<String, PathBuf> = HashMap::new();
        cache.insert(session_id.to_string(), proj_dir.clone());

        let result = lookup_with_cache(home, &mut cache, &cwd, session_id);
        assert_eq!(result, proj_dir);
        assert_eq!(cache.len(), 1, "cache size unchanged on hit");
    }

    fn write_session_meta(home: &Path, pid: u32, entrypoint: Option<&str>) {
        let dir = home.join(".claude").join("sessions");
        std::fs::create_dir_all(&dir).unwrap();
        let body = match entrypoint {
            Some(ep) => format!(r#"{{"pid":{pid},"entrypoint":"{ep}"}}"#),
            None => format!(r#"{{"pid":{pid}}}"#),
        };
        std::fs::write(dir.join(format!("{pid}.json")), body).unwrap();
    }

    #[test]
    fn entrypoint_filter_keeps_cli() {
        let tmp = tempfile::tempdir().unwrap();
        write_session_meta(tmp.path(), 1, Some("cli"));
        assert!(is_cli_entrypoint_under(tmp.path(), 1));
    }

    #[test]
    fn entrypoint_filter_drops_sdk_ts() {
        let tmp = tempfile::tempdir().unwrap();
        write_session_meta(tmp.path(), 2, Some("sdk-ts"));
        assert!(!is_cli_entrypoint_under(tmp.path(), 2));
    }

    #[test]
    fn entrypoint_filter_drops_sdk_py() {
        let tmp = tempfile::tempdir().unwrap();
        write_session_meta(tmp.path(), 3, Some("sdk-py"));
        assert!(!is_cli_entrypoint_under(tmp.path(), 3));
    }

    #[test]
    fn entrypoint_filter_keeps_when_meta_missing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(is_cli_entrypoint_under(tmp.path(), 999));
    }

    #[test]
    fn entrypoint_filter_keeps_when_field_missing() {
        let tmp = tempfile::tempdir().unwrap();
        write_session_meta(tmp.path(), 4, None);
        assert!(is_cli_entrypoint_under(tmp.path(), 4));
    }

    #[test]
    fn entrypoint_filter_keeps_when_meta_malformed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".claude").join("sessions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("5.json"), "not json").unwrap();
        assert!(is_cli_entrypoint_under(tmp.path(), 5));
    }

    #[test]
    fn path_cache_evicts_stale_entry_when_jsonl_gone() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let cwd = PathBuf::from("/Users/test/proj");
        let session_id = "stale";
        let stale_dir = home.join(".claude").join("projects").join("old-location");
        std::fs::create_dir_all(&stale_dir).unwrap();
        let mut cache: HashMap<String, PathBuf> = HashMap::new();
        cache.insert(session_id.to_string(), stale_dir.clone());

        let _result = lookup_with_cache(home, &mut cache, &cwd, session_id);
        assert!(!cache.contains_key(session_id));
    }
}
