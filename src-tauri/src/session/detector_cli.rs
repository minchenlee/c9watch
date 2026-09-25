use super::detector::encode_path_for_matching;
use super::pid_is_alive;
use super::source::{
    CliActivity, DetectedSession, DetectionDiagnostics, SessionDetectorError, SessionKind,
    SessionSource,
};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};
use wait_timeout::ChildExt;

#[derive(Deserialize, Debug, Clone)]
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

/// Longest a cached `claude agents --json` result is reused while the session
/// registry looks unchanged. Bounds staleness for anything the command reports
/// that doesn't touch `~/.claude/sessions/`.
const AGENTS_CACHE_MAX_AGE: Duration = Duration::from_secs(120);

/// Name, mtime and size of each `*.json` file in `~/.claude/sessions/`,
/// sorted by name. Claude Code rewrites a session's file on start, exit and
/// every status change, so an unchanged signature means `claude agents --json`
/// would return the same rows.
type RegistrySignature = Vec<(String, Option<SystemTime>, u64)>;

struct AgentsCache {
    signature: RegistrySignature,
    agents: Vec<CliAgent>,
    fetched_at: Instant,
}

pub struct CliSessionSource {
    claude_bin: PathBuf,
    path_cache: HashMap<String, PathBuf>,
    agents_cache: Option<AgentsCache>,
}

impl CliSessionSource {
    pub fn new() -> Self {
        // Relies on PATH lookup at spawn time. We don't pull in `which` as a
        // direct dep just for this — the probe already verified `claude` is on PATH.
        Self {
            claude_bin: PathBuf::from("claude"),
            path_cache: HashMap::new(),
            agents_cache: None,
        }
    }

    fn project_path_for_session(&mut self, cwd: &Path, session_id: &str) -> PathBuf {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        lookup_with_cache(&home, &mut self.path_cache, cwd, session_id)
    }

    fn map_agent_to_session(&mut self, a: CliAgent, entrypoint: Option<String>) -> DetectedSession {
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
            entrypoint,
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
            cursor_summary: None,
            pi_summary: None,
            opencode_summary: None,
        }
    }

    /// `claude agents --json` rows, reusing the previous result while the
    /// session registry is unchanged and the result is younger than
    /// `AGENTS_CACHE_MAX_AGE`. Each spawn costs ~0.2s of CPU, so running it on
    /// every poll dominated c9watch's energy use.
    fn agents(&mut self) -> Result<Vec<CliAgent>, SessionDetectorError> {
        let dir = dirs::home_dir().map(|home| sessions_dir(&home));
        let signature = dir.as_deref().and_then(registry_signature);
        if let Some(cache) = &mut self.agents_cache {
            let now = Instant::now();
            if cache_is_fresh(cache, signature.as_ref(), now) {
                return Ok(cache.agents.clone());
            }
            // Claude Code rewrites a session's file on every status change. When
            // only existing files changed, apply their new status in place
            // rather than spawning the command.
            if let (Some(dir), Some(signature)) = (&dir, &signature) {
                if now.saturating_duration_since(cache.fetched_at) < AGENTS_CACHE_MAX_AGE
                    && refresh_changed_rows(dir, &mut cache.agents, &cache.signature, signature)
                {
                    cache.signature = signature.clone();
                    return Ok(cache.agents.clone());
                }
            }
        }
        self.agents_cache = None;
        let agents = self.run_agents_command()?;
        // Taken before the spawn: a change that lands mid-spawn makes the next
        // poll's signature differ, so it's picked up then.
        if let Some(signature) = signature {
            self.agents_cache = Some(AgentsCache {
                signature,
                agents: agents.clone(),
                fetched_at: Instant::now(),
            });
        }
        Ok(agents)
    }

    fn run_agents_command(&self) -> Result<Vec<CliAgent>, SessionDetectorError> {
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

        parse_cli_agents(&buf)
    }
}

impl SessionSource for CliSessionSource {
    fn detect(
        &mut self,
    ) -> Result<(Vec<DetectedSession>, DetectionDiagnostics), SessionDetectorError> {
        let agents = self.agents()?;

        // `claude agents --json` is a registry Claude Code writes and prunes itself;
        // pruning runs in the agent's own exit path, so a hard kill (an external
        // SIGKILL, or a process wedged in an uninterruptible syscall no signal can
        // interrupt) can leave a stale entry pointing at a pid that no longer exists,
        // reported as busy/idle/waiting like any live agent. Drop those before they
        // ever reach status inference.
        let agents = filter_live_agents(agents, pid_is_alive);

        // Filter out non-CLI entrypoints (e.g. sdk-ts from Zed/IDE integrations).
        // `claude agents --json` lists every live agent including SDK-driven ones,
        // but those don't write project JSONLs and aren't what c9watch monitors.
        // The per-pid metadata at ~/.claude/sessions/<pid>.json carries `entrypoint`.
        // If the file is missing or unreadable, keep the agent (older CC versions).
        // Read once per agent and reuse for both the filter decision and the
        // DetectedSession field, instead of reading the file twice.
        let sessions: Vec<DetectedSession> = agents
            .into_iter()
            .filter_map(|a| {
                let entrypoint = pid_entrypoint(a.pid);
                is_monitored_entrypoint(entrypoint.as_deref())
                    .then(|| self.map_agent_to_session(a, entrypoint))
            })
            .collect();

        Ok((sessions, DetectionDiagnostics::default()))
    }

    fn backend_name(&self) -> &'static str {
        "cli"
    }
}

fn sessions_dir(home: &Path) -> PathBuf {
    home.join(".claude").join("sessions")
}

/// `None` when the directory can't be listed, which disables caching.
fn registry_signature(dir: &Path) -> Option<RegistrySignature> {
    let mut signature: RegistrySignature = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if !name.ends_with(".json") {
                return None;
            }
            let meta = entry.metadata().ok()?;
            Some((name, meta.modified().ok(), meta.len()))
        })
        .collect();
    signature.sort();
    Some(signature)
}

/// Updates `status` and `name` of cached rows from registry files that changed
/// between `old` and `new`. Returns false, leaving `agents` possibly partially
/// updated, when the change needs a full `claude agents --json` run instead:
/// files were added or removed, a changed file is unreadable, it belongs to no
/// cached row, or a field other than `status`/`name` differs.
fn refresh_changed_rows(
    dir: &Path,
    agents: &mut [CliAgent],
    old: &RegistrySignature,
    new: &RegistrySignature,
) -> bool {
    if old.len() != new.len() || old.iter().zip(new).any(|(a, b)| a.0 != b.0) {
        return false;
    }
    for (_, entry) in old.iter().zip(new).filter(|(a, b)| a != b) {
        let Some(row) = std::fs::read_to_string(dir.join(&entry.0))
            .ok()
            .and_then(|raw| serde_json::from_str::<CliAgent>(&raw).ok())
        else {
            return false;
        };
        let Some(agent) = agents.iter_mut().find(|a| a.pid == row.pid) else {
            return false;
        };
        if agent.session_id != row.session_id
            || agent.cwd != row.cwd
            || agent.kind != row.kind
            || agent.started_at != row.started_at
        {
            return false;
        }
        agent.status = row.status;
        agent.name = row.name;
    }
    true
}

fn cache_is_fresh(
    cache: &AgentsCache,
    signature: Option<&RegistrySignature>,
    now: Instant,
) -> bool {
    signature == Some(&cache.signature)
        && now.saturating_duration_since(cache.fetched_at) < AGENTS_CACHE_MAX_AGE
}

/// Drops any agent whose reported pid is no longer an actual running process.
/// Takes the liveness check as a parameter so tests can fake it without
/// spawning/killing real processes.
fn filter_live_agents(agents: Vec<CliAgent>, is_alive: impl Fn(u32) -> bool) -> Vec<CliAgent> {
    agents.into_iter().filter(|a| is_alive(a.pid)).collect()
}

/// Parse the Claude agent registry without allowing one known stopped
/// background entry to discard healthy live rows.
///
/// Claude Code can retain an observed background job row after its process has
/// gone away. The known contract from issue130 is `kind: "background"`,
/// `state: "blocked"`, and no usable `pid`; that row is not a session c9watch
/// can monitor or control, so it is omitted. The predicate is intentionally
/// narrow: other missing/invalid required fields remain parse errors rather
/// than silently accepting an unknown schema change.
fn parse_cli_agents(buf: &[u8]) -> Result<Vec<CliAgent>, SessionDetectorError> {
    let rows: Vec<Value> =
        serde_json::from_slice(buf).map_err(|e| SessionDetectorError::Parse(e.to_string()))?;
    let mut agents = Vec::with_capacity(rows.len());

    for (index, row) in rows.into_iter().enumerate() {
        let Some(object) = row.as_object() else {
            return Err(SessionDetectorError::Parse(format!(
                "agent row {index} is not an object"
            )));
        };

        match object.get("pid") {
            None | Some(Value::Null) if is_known_blocked_background(object) => continue,
            None | Some(Value::Null) => {
                return Err(SessionDetectorError::Parse(format!(
                    "agent row {index} is missing pid"
                )));
            }
            Some(_) => {}
        }

        let agent = serde_json::from_value::<CliAgent>(row)
            .map_err(|error| SessionDetectorError::Parse(format!("agent row {index}: {error}")))?;
        agents.push(agent);
    }

    Ok(agents)
}

fn is_known_blocked_background(object: &Map<String, Value>) -> bool {
    let is_background = object.get("kind").and_then(Value::as_str) == Some("background");
    let state = object.get("state").and_then(Value::as_str);
    is_background && state == Some("blocked")
}

/// `entrypoint` values known to be third-party SDK integrations (Zed/IDE)
/// that never write project JSONLs — the thing the entrypoint filter actually
/// needs to exclude. Everything else is kept, including CLI-launched
/// entrypoints we don't recognize yet: CC has already added new ones without
/// notice (e.g. "sdk-cli" for headless `claude -p`, alongside the interactive
/// "cli", as of 2.1.278) and a real CLI process writes a real transcript
/// regardless of which entrypoint label it gets, so failing open here is
/// safer than an allowlist that silently drops CLI runs on every CC bump.
const NON_MONITORED_SDK_ENTRYPOINTS: &[&str] = &["sdk-ts", "sdk-py"];

/// Reads `~/.claude/sessions/<pid>.json`'s `entrypoint` field (e.g. "cli",
/// "sdk-cli", "claude-vscode", "mcp", "remote_desktop"...), for both the
/// non-monitored-SDK filter and surfacing the raw value to the frontend.
/// `None` on a missing/unreadable/malformed file or missing field (older CC
/// builds didn't write this metadata) — callers treat that as "unknown, but
/// still a real CLI process" rather than excluding it.
fn pid_entrypoint(pid: u32) -> Option<String> {
    dirs::home_dir().and_then(|home| pid_entrypoint_under(&home, pid))
}

fn pid_entrypoint_under(home: &Path, pid: u32) -> Option<String> {
    let path = sessions_dir(home).join(format!("{pid}.json"));
    let raw = std::fs::read_to_string(&path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    value
        .get("entrypoint")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Whether an agent with this resolved `entrypoint` should be monitored (kept
/// in the detected list) — i.e. not one of `NON_MONITORED_SDK_ENTRYPOINTS`.
fn is_monitored_entrypoint(entrypoint: Option<&str>) -> bool {
    entrypoint
        .map(|ep| !NON_MONITORED_SDK_ENTRYPOINTS.contains(&ep))
        .unwrap_or(true)
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
        let agents = parse_cli_agents(full_schema_json().as_bytes()).unwrap();
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].pid, 1);
        assert_eq!(agents[0].session_id, "sid-a");
        assert_eq!(agents[1].name.as_deref(), Some("my-bg"));
    }

    #[test]
    fn map_agent_to_session_preserves_entrypoint() {
        let mut source = CliSessionSource::new();
        let session = source.map_agent_to_session(
            CliAgent {
                pid: std::process::id(),
                cwd: PathBuf::from("/tmp/entrypoint-fixture"),
                kind: "interactive".to_string(),
                started_at: 1,
                session_id: "entrypoint-session".to_string(),
                name: None,
                status: None,
            },
            Some("sdk-cli".to_string()),
        );

        assert_eq!(session.entrypoint.as_deref(), Some("sdk-cli"));
    }

    #[test]
    fn parse_cli_output_missing_status_yields_none_in_mapping() {
        let json = r#"[{"pid":1,"cwd":"/tmp","kind":"interactive","startedAt":1,"sessionId":"x"}]"#;
        let agents = parse_cli_agents(json.as_bytes()).unwrap();
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
        let agents = parse_cli_agents(json.as_bytes()).unwrap();
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
        let agents = parse_cli_agents(json.as_bytes()).unwrap();
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
        let agents = parse_cli_agents(json.as_bytes()).unwrap();
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
        let agents = parse_cli_agents(json.as_bytes()).unwrap();
        assert_eq!(agents[0].kind, "interactive");
    }

    fn cache_with(signature: RegistrySignature, fetched_at: Instant) -> AgentsCache {
        AgentsCache {
            signature,
            agents: parse_cli_agents(full_schema_json().as_bytes()).unwrap(),
            fetched_at,
        }
    }

    #[test]
    fn registry_signature_lists_only_json_files_sorted() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("20.json"), "{}").unwrap();
        std::fs::write(tmp.path().join("10.json"), "{\"a\":1}").unwrap();
        std::fs::write(tmp.path().join("10.abc.key"), "secret").unwrap();
        let names: Vec<String> = registry_signature(tmp.path())
            .unwrap()
            .into_iter()
            .map(|(name, _, len)| format!("{name}:{len}"))
            .collect();
        assert_eq!(names, ["10.json:7", "20.json:2"]);
    }

    #[test]
    fn registry_signature_is_none_for_missing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(registry_signature(&tmp.path().join("absent")).is_none());
    }

    #[test]
    fn registry_signature_changes_on_rewrite_add_and_remove() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("10.json");
        std::fs::write(&file, r#"{"status":"idle"}"#).unwrap();
        let initial = registry_signature(tmp.path()).unwrap();
        assert_eq!(registry_signature(tmp.path()).unwrap(), initial);

        std::fs::write(&file, r#"{"status":"busy"}"#).unwrap();
        let t = SystemTime::now() + Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(t)
            .unwrap();
        let rewritten = registry_signature(tmp.path()).unwrap();
        assert_ne!(rewritten, initial);

        std::fs::write(tmp.path().join("20.json"), "{}").unwrap();
        let added = registry_signature(tmp.path()).unwrap();
        assert_ne!(added, rewritten);

        std::fs::remove_file(tmp.path().join("20.json")).unwrap();
        assert_eq!(registry_signature(tmp.path()).unwrap(), rewritten);
    }

    #[test]
    fn cache_is_fresh_only_for_same_signature_within_max_age() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("10.json"), "{}").unwrap();
        let signature = registry_signature(tmp.path()).unwrap();
        let fetched_at = Instant::now();
        let cache = cache_with(signature.clone(), fetched_at);

        assert!(cache_is_fresh(&cache, Some(&signature), fetched_at));
        assert!(cache_is_fresh(
            &cache,
            Some(&signature),
            fetched_at + AGENTS_CACHE_MAX_AGE - Duration::from_millis(1)
        ));
        assert!(!cache_is_fresh(
            &cache,
            Some(&signature),
            fetched_at + AGENTS_CACHE_MAX_AGE
        ));
        assert!(!cache_is_fresh(&cache, None, fetched_at));

        std::fs::write(tmp.path().join("20.json"), "{}").unwrap();
        let changed = registry_signature(tmp.path()).unwrap();
        assert!(!cache_is_fresh(&cache, Some(&changed), fetched_at));
    }

    /// Writes a registry file for one of `full_schema_json`'s rows and bumps its
    /// mtime so the signature changes even within one timestamp tick.
    fn write_registry_row(dir: &Path, pid: u32, body: &str, bump_secs: u64) {
        let file = dir.join(format!("{pid}.json"));
        std::fs::write(&file, body).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(SystemTime::now() + Duration::from_secs(bump_secs))
            .unwrap();
    }

    const ROW_A_IDLE: &str = r#"{"pid":1,"cwd":"/tmp/a","kind":"interactive","startedAt":100,"sessionId":"sid-a","status":"idle","entrypoint":"cli"}"#;
    const ROW_B_IDLE: &str = r#"{"pid":2,"cwd":"/tmp/b","kind":"background","startedAt":200,"sessionId":"sid-b","name":"my-bg","status":"idle"}"#;

    #[test]
    fn refresh_changed_rows_applies_status_and_name_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        write_registry_row(tmp.path(), 1, ROW_A_IDLE, 0);
        write_registry_row(tmp.path(), 2, ROW_B_IDLE, 0);
        let old = registry_signature(tmp.path()).unwrap();
        let mut agents = parse_cli_agents(full_schema_json().as_bytes()).unwrap();

        write_registry_row(
            tmp.path(),
            1,
            &ROW_A_IDLE.replace(
                r#""status":"idle""#,
                r#""status":"waiting","name":"renamed""#,
            ),
            5,
        );
        let new = registry_signature(tmp.path()).unwrap();

        assert!(refresh_changed_rows(tmp.path(), &mut agents, &old, &new));
        assert_eq!(agents[0].status.as_deref(), Some("waiting"));
        assert_eq!(agents[0].name.as_deref(), Some("renamed"));
        // Unchanged file: row untouched even though it disagrees with the cache.
        assert_eq!(agents[1].status.as_deref(), Some("idle"));
    }

    #[test]
    fn refresh_changed_rows_needs_spawn_for_added_or_removed_files() {
        let tmp = tempfile::tempdir().unwrap();
        write_registry_row(tmp.path(), 1, ROW_A_IDLE, 0);
        let old = registry_signature(tmp.path()).unwrap();
        let mut agents = parse_cli_agents(full_schema_json().as_bytes()).unwrap();

        write_registry_row(tmp.path(), 2, ROW_B_IDLE, 0);
        let added = registry_signature(tmp.path()).unwrap();
        assert!(!refresh_changed_rows(tmp.path(), &mut agents, &old, &added));
        assert!(!refresh_changed_rows(tmp.path(), &mut agents, &added, &old));
    }

    #[test]
    fn refresh_changed_rows_needs_spawn_for_unknown_pid_identity_change_or_bad_file() {
        let tmp = tempfile::tempdir().unwrap();
        write_registry_row(tmp.path(), 1, ROW_A_IDLE, 0);
        let old = registry_signature(tmp.path()).unwrap();

        let cases = [
            ROW_A_IDLE.replace(r#""pid":1"#, r#""pid":9"#),
            ROW_A_IDLE.replace("/tmp/a", "/tmp/moved"),
            ROW_A_IDLE.replace("sid-a", "sid-new"),
            ROW_A_IDLE.replace(r#""startedAt":100"#, r#""startedAt":101"#),
            "not json".to_string(),
        ];
        for (i, body) in cases.iter().enumerate() {
            let mut agents = parse_cli_agents(full_schema_json().as_bytes()).unwrap();
            write_registry_row(tmp.path(), 1, body, 5 + i as u64);
            let new = registry_signature(tmp.path()).unwrap();
            assert!(
                !refresh_changed_rows(tmp.path(), &mut agents, &old, &new),
                "case {i} should need a spawn"
            );
        }
    }

    #[test]
    fn filter_live_agents_drops_dead_pids() {
        let agents = parse_cli_agents(full_schema_json().as_bytes()).unwrap();
        assert_eq!(agents.len(), 2);
        // sid-a's pid (1) reported alive, sid-b's pid (2) reported dead.
        let filtered = filter_live_agents(agents, |pid| pid == 1);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].session_id, "sid-a");
    }

    #[test]
    fn filter_live_agents_keeps_all_when_all_alive() {
        let agents = parse_cli_agents(full_schema_json().as_bytes()).unwrap();
        let filtered = filter_live_agents(agents, |_| true);
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn filter_live_agents_drops_all_when_none_alive() {
        let agents = parse_cli_agents(full_schema_json().as_bytes()).unwrap();
        let filtered = filter_live_agents(agents, |_| false);
        assert!(filtered.is_empty());
    }

    #[test]
    fn parse_cli_output_empty_array() {
        let agents = parse_cli_agents(b"[]").unwrap();
        assert!(agents.is_empty());
    }

    #[test]
    fn parse_cli_output_skips_known_blocked_background_and_keeps_live_rows() {
        let json = r#"[
          {"id":"stopped","cwd":"/tmp/stopped","kind":"background","startedAt":1,"sessionId":"stopped","state":"blocked"},
          {"pid":7,"cwd":"/tmp/live","kind":"interactive","startedAt":2,"sessionId":"live","status":"idle"}
        ]"#;
        let agents = parse_cli_agents(json.as_bytes()).unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].pid, 7);
        assert_eq!(agents[0].session_id, "live");
    }

    #[test]
    fn parse_cli_output_missing_pid_on_unknown_shape_is_an_error() {
        let json =
            r#"[{"cwd":"/tmp/unknown","kind":"interactive","startedAt":1,"sessionId":"unknown"}]"#;
        let error = parse_cli_agents(json.as_bytes()).unwrap_err();
        assert!(error.to_string().contains("agent row 0 is missing pid"));
    }

    #[test]
    fn parse_cli_output_invalid_live_row_is_an_error() {
        let json = r#"[{"pid":"not-a-number","cwd":"/tmp/live","kind":"interactive","startedAt":1,"sessionId":"live"}]"#;
        let error = parse_cli_agents(json.as_bytes()).unwrap_err();
        assert!(error.to_string().contains("agent row 0"));
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

    /// Combines `pid_entrypoint_under` + `is_monitored_entrypoint`, mirroring
    /// what `detect()`'s filter_map does per-agent.
    fn is_cli_entrypoint_under(home: &Path, pid: u32) -> bool {
        is_monitored_entrypoint(pid_entrypoint_under(home, pid).as_deref())
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
    fn entrypoint_filter_keeps_sdk_cli() {
        // "sdk-cli" is CC 2.1.278's entrypoint for headless `claude -p`
        // launches (e.g. scheduled jobs) — a real CLI process that writes
        // a real transcript, not a third-party SDK integration.
        let tmp = tempfile::tempdir().unwrap();
        write_session_meta(tmp.path(), 6, Some("sdk-cli"));
        assert!(is_cli_entrypoint_under(tmp.path(), 6));
    }

    #[test]
    fn entrypoint_filter_keeps_unrecognized_value() {
        // Fail open on any future entrypoint value we don't know about yet,
        // rather than requiring this excludelist to be updated on every CC
        // release that adds one.
        let tmp = tempfile::tempdir().unwrap();
        write_session_meta(tmp.path(), 7, Some("some-future-entrypoint"));
        assert!(is_cli_entrypoint_under(tmp.path(), 7));
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
    fn pid_entrypoint_returns_raw_value() {
        let tmp = tempfile::tempdir().unwrap();
        write_session_meta(tmp.path(), 8, Some("claude-vscode"));
        assert_eq!(
            pid_entrypoint_under(tmp.path(), 8),
            Some("claude-vscode".to_string())
        );
    }

    #[test]
    fn pid_entrypoint_none_when_meta_missing() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(pid_entrypoint_under(tmp.path(), 999), None);
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
