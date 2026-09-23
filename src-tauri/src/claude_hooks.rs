//! Claude Code hook bridge. Records which permission prompts a session is showing,
//! so status detection can tell a real prompt from a tool that is simply still
//! running (a sub-agent, a long build).
//!
//! Claude Code runs `c9watch hooks` asynchronously on each registered event. The
//! command folds the event into `<config>/c9watch/hooks/<session_id>.json`, and the
//! poller reads that file. Only event names, tool names and agent ids are stored.
use crate::claude_usage::{config_dir, shell_quote, update_settings, SettingsUpdate};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime};

/// Events the bridge registers for. `PermissionRequest` opens a pending prompt;
/// every other event closes one or more.
pub const HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "PermissionRequest",
    "PermissionDenied",
    "PostToolUse",
    "PostToolUseFailure",
    "PostToolBatch",
    "Stop",
    "StopFailure",
    "SubagentStop",
];

/// State files untouched for this long belong to sessions that ended without a
/// `SessionEnd` (crash, kill) and are pruned on the next `SessionStart`.
const STALE_STATE_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// A permission prompt Claude Code reported and has not yet reported resolved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingPermission {
    /// Sub-agent that raised the prompt; `None` for the main thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub tool_name: String,
    /// Unix milliseconds.
    pub requested_at: i64,
}

/// Hook-reported state for one session.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookSessionState {
    #[serde(default)]
    pub pending: Vec<PendingPermission>,
}

/// The subset of a hook payload the bridge uses. Unknown fields (tool input and
/// output, prompts) are skipped without being buffered.
#[derive(Debug, Deserialize)]
pub struct HookInput {
    pub session_id: String,
    pub hook_event_name: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
}

/// Folds one hook event into a session's state. Returns `None` when the state
/// file should be removed.
pub fn apply(
    mut state: HookSessionState,
    input: &HookInput,
    now_ms: i64,
) -> Option<HookSessionState> {
    let agent = input.agent_id.as_deref();
    let remove_one = |state: &mut HookSessionState| {
        if let Some(tool) = input.tool_name.as_deref() {
            if let Some(i) = state
                .pending
                .iter()
                .position(|p| p.agent_id.as_deref() == agent && p.tool_name == tool)
            {
                state.pending.remove(i);
            }
        }
    };
    match input.hook_event_name.as_str() {
        "SessionEnd" => return None,
        "SessionStart" => state.pending.clear(),
        "PermissionRequest" => {
            if let Some(tool) = &input.tool_name {
                state.pending.push(PendingPermission {
                    agent_id: input.agent_id.clone(),
                    tool_name: tool.clone(),
                    requested_at: now_ms,
                });
            }
        }
        "PermissionDenied" | "PostToolUse" | "PostToolUseFailure" => remove_one(&mut state),
        // Every call in the batch has resolved, including any that were prompted.
        "PostToolBatch" => state.pending.retain(|p| p.agent_id.as_deref() != agent),
        "SubagentStop" if agent.is_some() => {
            state.pending.retain(|p| p.agent_id.as_deref() != agent);
        }
        // The main thread moved on. Background sub-agents may still be prompting.
        "Stop" | "StopFailure" | "UserPromptSubmit" if agent.is_none() => {
            state.pending.retain(|p| p.agent_id.is_some());
        }
        _ => {}
    }
    Some(state)
}

pub fn state_dir() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("c9watch/hooks"))
}

/// Session ids become file names, so anything outside `[A-Za-z0-9_-]` is refused.
fn state_path(dir: &Path, session_id: &str) -> Option<PathBuf> {
    let valid = !session_id.is_empty()
        && session_id.len() <= 128
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    valid.then(|| dir.join(format!("{session_id}.json")))
}

#[cfg(unix)]
fn lock(file: &fs::File, exclusive: bool) {
    use std::os::unix::io::AsRawFd;
    let op = if exclusive {
        libc::LOCK_EX
    } else {
        libc::LOCK_SH
    };
    // Best effort: an unlocked read at worst sees a torn write, which parses as
    // invalid and is treated as "no hook data" for one poll.
    unsafe {
        libc::flock(file.as_raw_fd(), op);
    }
}

#[cfg(not(unix))]
fn lock(_file: &fs::File, _exclusive: bool) {}

/// Records one hook event under `dir`.
pub fn record(input: &HookInput, dir: &Path, now_ms: i64) -> Result<(), String> {
    let path = state_path(dir, &input.session_id).ok_or("Invalid session id")?;
    if input.hook_event_name == "SessionStart" {
        prune_stale(dir);
    }
    fs::create_dir_all(dir).map_err(|_| "Cannot create hook state directory")?;
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(|_| "Cannot open hook state")?;
    lock(&file, true);
    let mut content = String::new();
    file.read_to_string(&mut content)
        .map_err(|_| "Cannot read hook state")?;
    let current: HookSessionState = serde_json::from_str(&content).unwrap_or_default();
    match apply(current.clone(), input, now_ms) {
        None => {
            fs::remove_file(&path).map_err(|_| "Cannot remove hook state")?;
        }
        Some(next) if next != current || content.is_empty() => {
            let bytes = serde_json::to_vec(&next).map_err(|_| "Cannot encode hook state")?;
            file.set_len(0).map_err(|_| "Cannot write hook state")?;
            file.seek(SeekFrom::Start(0))
                .map_err(|_| "Cannot write hook state")?;
            file.write_all(&bytes)
                .map_err(|_| "Cannot write hook state")?;
        }
        Some(_) => {}
    }
    Ok(())
}

fn prune_stale(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > STALE_STATE_AGE);
        if stale {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Reads a session's hook state from `dir`. `None` means the session has never
/// reported a hook event (or the file is unreadable).
pub fn read_state_in(dir: &Path, session_id: &str) -> Option<HookSessionState> {
    let path = state_path(dir, session_id)?;
    let mut file = fs::File::open(path).ok()?;
    lock(&file, false);
    let mut content = String::new();
    file.read_to_string(&mut content).ok()?;
    if content.is_empty() {
        return Some(HookSessionState::default());
    }
    serde_json::from_str(&content).ok()
}

/// Hook state for a session whose permission prompts are reported by hooks:
/// the bridge must be installed and the session must have sent an event.
pub fn session_state(session_id: &str) -> Option<HookSessionState> {
    let config = config_dir().ok()?;
    if !installed_cached(&config) {
        return None;
    }
    read_state_in(&config.join("c9watch/hooks"), session_id)
}

/// Settings path, its mtime when read, and whether the bridge was installed.
type InstalledStamp = (PathBuf, Option<SystemTime>, bool);

static INSTALLED_CACHE: LazyLock<Mutex<Option<InstalledStamp>>> =
    LazyLock::new(|| Mutex::new(None));

/// [`installed`] for `<config>/settings.json`, re-read only when its mtime changes.
fn installed_cached(config: &Path) -> bool {
    let path = config.join("settings.json");
    let mtime = fs::metadata(&path).and_then(|m| m.modified()).ok();
    let read = || {
        fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|settings| installed(&settings))
    };
    let Ok(mut cache) = INSTALLED_CACHE.lock() else {
        return read();
    };
    if let Some((cached_path, cached_mtime, value)) = cache.as_ref() {
        if *cached_path == path && *cached_mtime == mtime {
            return *value;
        }
    }
    let value = read();
    *cache = Some((path, mtime, value));
    value
}

/// Whether `settings` enables the bridge for `PermissionRequest`, the event the
/// status logic depends on.
pub fn installed(settings: &Value) -> bool {
    if settings["disableAllHooks"] == true {
        return false;
    }
    settings["hooks"]["PermissionRequest"]
        .as_array()
        .is_some_and(|groups| groups.iter().any(group_has_bridge))
}

fn group_has_bridge(group: &Value) -> bool {
    group["hooks"]
        .as_array()
        .is_some_and(|hooks| hooks.iter().any(is_bridge_hook))
}

/// A hook entry written by [`configured`], for any c9watch executable path.
fn is_bridge_hook(hook: &Value) -> bool {
    hook["type"] == "command"
        && hook["command"]
            .as_str()
            .is_some_and(|c| c.ends_with("' hooks") && c.contains("c9watch"))
}

fn bridge_command(executable: &Path) -> Result<String, String> {
    Ok(format!(
        "{} hooks",
        shell_quote(executable.to_str().ok_or("Invalid executable path")?)
    ))
}

/// Removes bridge entries from every event, dropping groups and events left
/// empty. Other hooks are untouched.
pub fn unconfigured(mut settings: Value) -> Result<Value, String> {
    if !settings.is_object() {
        return Err("Claude settings must be a JSON object".into());
    }
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else {
        return Ok(settings);
    };
    for event in HOOK_EVENTS {
        let Some(groups) = hooks.get_mut(*event).and_then(Value::as_array_mut) else {
            continue;
        };
        for group in groups.iter_mut() {
            if let Some(entries) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                entries.retain(|hook| !is_bridge_hook(hook));
            }
        }
        groups.retain(|group| !group["hooks"].as_array().is_some_and(Vec::is_empty));
        if groups.is_empty() {
            hooks.remove(*event);
        }
    }
    if hooks.is_empty() {
        settings.as_object_mut().map(|s| s.remove("hooks"));
    }
    Ok(settings)
}

/// Registers the bridge on every event in [`HOOK_EVENTS`], replacing bridge
/// entries for other executable paths. Idempotent.
pub fn configured(settings: Value, executable: &Path) -> Result<Value, String> {
    let command = bridge_command(executable)?;
    if !settings.is_object() {
        return Err("Claude settings must be a JSON object".into());
    }
    if !settings["hooks"].is_null() && !settings["hooks"].is_object() {
        return Err("Claude settings `hooks` must be an object".into());
    }
    let expected = json!({"hooks": [{"type": "command", "command": command, "async": true}]});
    let already = HOOK_EVENTS.iter().all(|event| {
        settings["hooks"][*event].as_array().is_some_and(|groups| {
            groups.contains(&expected) && groups.iter().filter(|g| group_has_bridge(g)).count() == 1
        })
    });
    if already {
        return Ok(settings);
    }
    let mut settings = unconfigured(settings)?;
    let hooks = settings
        .as_object_mut()
        .expect("checked above")
        .entry("hooks")
        .or_insert_with(|| json!({}));
    for event in HOOK_EVENTS {
        let groups = hooks
            .as_object_mut()
            .expect("checked above")
            .entry(*event)
            .or_insert_with(|| json!([]));
        groups
            .as_array_mut()
            .ok_or_else(|| format!("Claude settings `hooks.{event}` must be an array"))?
            .push(expected.clone());
    }
    Ok(settings)
}

fn describe(update: SettingsUpdate, done: &str, unchanged: &str) -> String {
    match update {
        SettingsUpdate::Unchanged => unchanged.into(),
        SettingsUpdate::Written {
            backup: Some(backup),
        } => {
            format!("{done} Previous settings backup: {}", backup.display())
        }
        SettingsUpdate::Written { backup: None } => {
            format!("{done} No previous settings file existed.")
        }
    }
}

pub fn install(directory: &Path, executable: &Path) -> Result<String, String> {
    #[cfg(windows)]
    {
        let _ = (directory, executable);
        return Err("Claude Code hooks aren't supported by c9watch on Windows".into());
    }
    #[cfg(not(windows))]
    {
        let update = update_settings(directory, |settings| configured(settings, executable))?;
        Ok(describe(
            update,
            "Claude Code hooks enabled. Running sessions pick them up automatically.",
            "Claude Code hooks are already enabled",
        ))
    }
}

pub fn uninstall(directory: &Path) -> Result<String, String> {
    let update = update_settings(directory, unconfigured)?;
    Ok(describe(
        update,
        "Claude Code hooks removed.",
        "Claude Code hooks are not enabled",
    ))
}

/// Entry point for `c9watch hooks`. Never fails loudly while handling an event:
/// an async hook's errors are invisible to the user, and a broken bridge must
/// not disturb the session.
pub fn run(install_hooks: bool, uninstall_hooks: bool) -> Result<(), String> {
    if install_hooks {
        let exe = std::env::current_exe().map_err(|_| "Cannot locate c9watch executable")?;
        println!("{}", install(&config_dir()?, &exe)?);
        return Ok(());
    }
    if uninstall_hooks {
        println!("{}", uninstall(&config_dir()?)?);
        return Ok(());
    }
    let mut stdin = std::io::stdin().lock();
    let input: HookInput = serde_json::from_reader(&mut stdin).map_err(|_| "Invalid hook JSON")?;
    // Drain anything after the payload so Claude Code never sees a broken pipe.
    let _ = std::io::copy(&mut stdin, &mut std::io::sink());
    record(&input, &state_dir()?, chrono::Utc::now().timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(name: &str, agent: Option<&str>, tool: Option<&str>) -> HookInput {
        HookInput {
            session_id: "s1".into(),
            hook_event_name: name.into(),
            agent_id: agent.map(str::to_owned),
            tool_name: tool.map(str::to_owned),
        }
    }

    fn fold(events: &[HookInput]) -> Option<HookSessionState> {
        events
            .iter()
            .try_fold(HookSessionState::default(), |state, e| apply(state, e, 1))
    }

    fn tools(state: &HookSessionState) -> Vec<(Option<&str>, &str)> {
        state
            .pending
            .iter()
            .map(|p| (p.agent_id.as_deref(), p.tool_name.as_str()))
            .collect()
    }

    #[test]
    fn permission_request_is_pending_until_tool_resolves() {
        let state = fold(&[event("PermissionRequest", None, Some("Bash"))]).unwrap();
        assert_eq!(tools(&state), vec![(None, "Bash")]);
        let state = apply(state, &event("PostToolUse", None, Some("Bash")), 2).unwrap();
        assert!(state.pending.is_empty());
    }

    #[test]
    fn post_tool_use_only_clears_the_matching_agent_and_tool() {
        let state = fold(&[
            event("PermissionRequest", None, Some("Bash")),
            event("PermissionRequest", Some("a1"), Some("Bash")),
            event("PostToolUse", None, Some("Read")),
            event("PostToolUse", Some("a2"), Some("Bash")),
        ])
        .unwrap();
        assert_eq!(tools(&state), vec![(None, "Bash"), (Some("a1"), "Bash")]);
        let state = apply(
            state,
            &event("PostToolUseFailure", Some("a1"), Some("Bash")),
            2,
        )
        .unwrap();
        assert_eq!(tools(&state), vec![(None, "Bash")]);
    }

    #[test]
    fn stop_clears_main_thread_but_not_subagent_prompts() {
        let state = fold(&[
            event("PermissionRequest", None, Some("Write")),
            event("PermissionRequest", Some("a1"), Some("Bash")),
            event("Stop", None, None),
        ])
        .unwrap();
        assert_eq!(tools(&state), vec![(Some("a1"), "Bash")]);
        let state = apply(state, &event("SubagentStop", Some("a1"), None), 2).unwrap();
        assert!(state.pending.is_empty());
    }

    #[test]
    fn batch_and_user_prompt_clear_their_own_scope() {
        let state = fold(&[
            event("PermissionRequest", None, Some("Edit")),
            event("PermissionRequest", Some("a1"), Some("Bash")),
            event("PostToolBatch", Some("a1"), None),
        ])
        .unwrap();
        assert_eq!(tools(&state), vec![(None, "Edit")]);
        let state = apply(state, &event("UserPromptSubmit", None, None), 2).unwrap();
        assert!(state.pending.is_empty());
    }

    #[test]
    fn session_end_removes_state() {
        assert!(fold(&[
            event("PermissionRequest", None, Some("Bash")),
            event("SessionEnd", None, None)
        ])
        .is_none());
    }

    #[test]
    fn record_round_trips_and_rejects_path_like_ids() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_state_in(dir.path(), "s1"), None);
        record(&event("SessionStart", None, None), dir.path(), 1).unwrap();
        assert_eq!(
            read_state_in(dir.path(), "s1"),
            Some(HookSessionState::default())
        );
        record(
            &event("PermissionRequest", None, Some("Bash")),
            dir.path(),
            5,
        )
        .unwrap();
        let state = read_state_in(dir.path(), "s1").unwrap();
        assert_eq!(state.pending[0].requested_at, 5);
        record(&event("SessionEnd", None, None), dir.path(), 6).unwrap();
        assert_eq!(read_state_in(dir.path(), "s1"), None);

        let mut bad = event("SessionStart", None, None);
        bad.session_id = "../escape".into();
        assert!(record(&bad, dir.path(), 1).is_err());
        assert_eq!(read_state_in(dir.path(), "../escape"), None);
    }

    #[test]
    fn hook_input_ignores_large_unknown_fields() {
        let payload = json!({
            "session_id": "s1",
            "hook_event_name": "PostToolUse",
            "tool_name": "Read",
            "tool_input": {"file_path": "/x"},
            "tool_response": "x".repeat(10_000),
        });
        let input: HookInput = serde_json::from_value(payload).unwrap();
        assert_eq!(input.tool_name.as_deref(), Some("Read"));
        assert_eq!(input.agent_id, None);
    }

    #[test]
    fn configure_preserves_other_hooks_and_is_idempotent() {
        let exe = Path::new("/Applications/c9watch.app/Contents/MacOS/c9watch");
        let other = json!({"type": "command", "command": "afplay ding.aiff"});
        let settings = json!({
            "permissions": {"allow": ["Read"]},
            "hooks": {"Stop": [{"hooks": [other]}]}
        });
        let next = configured(settings.clone(), exe).unwrap();
        assert!(installed(&next));
        assert_eq!(next["permissions"], settings["permissions"]);
        assert_eq!(next["hooks"]["Stop"][0]["hooks"][0], other);
        for event in HOOK_EVENTS {
            let bridge = next["hooks"][*event].as_array().unwrap().last().unwrap();
            assert_eq!(bridge["hooks"][0]["async"], true);
        }
        assert_eq!(configured(next.clone(), exe).unwrap(), next);

        let removed = unconfigured(next).unwrap();
        assert_eq!(removed, settings);
        assert!(!installed(&removed));
    }

    #[test]
    fn configure_replaces_bridge_for_an_old_executable_path() {
        let old = configured(json!({}), Path::new("/old/c9watch")).unwrap();
        let next = configured(old, Path::new("/new/c9watch")).unwrap();
        let groups = next["hooks"]["PermissionRequest"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["hooks"][0]["command"], "'/new/c9watch' hooks");
    }

    #[test]
    fn disable_all_hooks_means_not_installed() {
        let mut settings = configured(json!({}), Path::new("/x/c9watch")).unwrap();
        settings["disableAllHooks"] = json!(true);
        assert!(!installed(&settings));
    }

    #[test]
    fn install_writes_settings_with_backup() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("settings.json"), br#"{"model":"opus"}"#).unwrap();
        let exe = Path::new("/x/c9watch");
        let message = install(dir.path(), exe).unwrap();
        assert!(message.contains("backup"), "{message}");
        let written: Value =
            serde_json::from_slice(&fs::read(dir.path().join("settings.json")).unwrap()).unwrap();
        assert_eq!(written["model"], "opus");
        assert!(installed(&written));
        assert_eq!(
            install(dir.path(), exe).unwrap(),
            "Claude Code hooks are already enabled"
        );
        uninstall(dir.path()).unwrap();
        let restored: Value =
            serde_json::from_slice(&fs::read(dir.path().join("settings.json")).unwrap()).unwrap();
        assert_eq!(restored, json!({"model": "opus"}));
    }
}
