//! Claude Code hook bridge. Records which permission prompts a session is showing,
//! so status detection can tell a real prompt from a tool that is simply still
//! running (a sub-agent, a long build).
//!
//! Claude Code runs `c9watch hooks` asynchronously on each registered event. The
//! command folds the event into `<config>/c9watch/hooks/<session_id>.json`, and the
//! poller reads that file. Tool names, agent ids and correlated call ids are stored.
use crate::claude_usage::{config_dir, shell_quote, update_settings, SettingsUpdate};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Events the bridge registers for. Permission/completion call evidence is folded;
/// lifecycle events without call identity cannot resolve a pending prompt.
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
    /// Call identity read from the transcript, not an id supplied by PermissionRequest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
}

/// Hook-reported state for one session.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookSessionState {
    #[serde(default)]
    pub pending: Vec<PendingPermission>,
    /// Recent exact completion identities; identity-free requests still stay uncertain.
    #[serde(default)]
    pub resolved: Vec<PendingPermission>,
}

/// The subset of a bounded hook payload used for call correlation. Tool input is
/// compared with transcript evidence, but is not duplicated into hook state.
#[derive(Debug, Clone, Deserialize)]
pub struct HookInput {
    pub session_id: String,
    pub hook_event_name: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    #[serde(default)]
    pub transcript_path: Option<PathBuf>,
    #[serde(default)]
    pub tool_input: Option<Value>,
}

/// Folds call-correlated evidence. Identity-free lifecycle events cannot close waits.
pub fn apply(
    mut state: HookSessionState,
    input: &HookInput,
    now_ms: i64,
) -> Option<HookSessionState> {
    let same_call = |p: &PendingPermission| {
        p.agent_id == input.agent_id && p.tool_use_id.is_some()
            && p.tool_use_id == input.tool_use_id
    };
    match input.hook_event_name.as_str() {
        "PermissionRequest" => {
            if let Some(tool) = input.tool_name.as_ref().filter(|name| !name.is_empty()) {
                let duplicate = state.pending.iter().any(|p| {
                    p.agent_id == input.agent_id && p.tool_name == *tool
                        && p.tool_use_id == input.tool_use_id
                });
                if !duplicate && !state.resolved.iter().any(same_call) {
                    state.pending.push(PendingPermission {
                        agent_id: input.agent_id.clone(), tool_name: tool.clone(),
                        requested_at: now_ms, tool_use_id: input.tool_use_id.clone(),
                    });
                }
            }
        }
        "PermissionDenied" | "PostToolUse" | "PostToolUseFailure" => {
            // Only a supplied completion call id can close a correlated prompt.
            // Tool names and handler receipt order are not event identities.
            if input.tool_use_id.is_some() {
                state.pending.retain(|p| !same_call(p));
                if !state.resolved.iter().any(same_call) {
                    state.resolved.push(PendingPermission {
                        agent_id: input.agent_id.clone(),
                        tool_name: input.tool_name.clone().unwrap_or_default(),
                        requested_at: now_ms, tool_use_id: input.tool_use_id.clone(),
                    });
                }
            }
        }
        // Lifecycle/batch events lack call identity. Delayed deliveries must not
        // clear newer waits. Transcript results resolve them; stale files expire.
        _ => {}
    }
    // Retire oldest completion evidence instead of rejecting a current exact
    // completion. This bounded history cannot guarantee arbitrarily late deduplication.
    let excess = state.resolved.len().saturating_sub(MAX_PERMISSIONS);
    state.resolved.drain(..excess);
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

const MAX_STATE_BYTES: u64 = 128 * 1024;
const MAX_INPUT_BYTES: u64 = 256 * 1024;
const MAX_TRANSCRIPT_BYTES: u64 = 512 * 1024;
const MAX_PERMISSIONS: usize = 256;
const MAX_SETTINGS_BYTES: u64 = 256 * 1024;
// Async hook writers retain their input while brief reader/writer contention clears.
// Every acquisition remains nonblocking; an indefinitely held lock has a finite budget.
const WRITE_LOCK_BUDGET: Duration = Duration::from_millis(250);

#[cfg(unix)]
fn safe_directory(path: &Path, create: bool) -> Result<fs::File, String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let absolute = if path.is_absolute() { path.to_path_buf() }
        else { std::env::current_dir().map_err(|_| "Cannot locate hook directory")?.join(path) };
    let mut directory = fs::File::open("/").map_err(|_| "Cannot open root directory")?;
    for component in absolute.components() {
        let std::path::Component::Normal(name) = component else {
            if matches!(component, std::path::Component::ParentDir) { return Err("Invalid directory path".into()); }
            continue;
        };
        let name = CString::new(name.as_bytes()).map_err(|_| "Invalid directory component")?;
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        let mut fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 && create && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
            let made = unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o700) };
            if made < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists {
                return Err("Cannot create hook directory".into());
            }
            fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        }
        if fd < 0 { return Err("Hook directory must contain no symlinks".into()); }
        directory = unsafe { fs::File::from_raw_fd(fd) };
    }
    Ok(directory)
}

#[cfg(unix)]
fn safe_file(directory: &fs::File, name: &std::ffi::OsStr, write: bool) -> Result<fs::File, String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    let name = CString::new(name.as_bytes()).map_err(|_| "Invalid file name")?;
    let access = if write { libc::O_RDWR | libc::O_CREAT } else { libc::O_RDONLY };
    let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(),
        access | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC, 0o600) };
    if fd < 0 { return Err("Cannot safely open hook file".into()); }
    let file = unsafe { fs::File::from_raw_fd(fd) };
    let metadata = file.metadata().map_err(|_| "Cannot inspect hook file")?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err("Hook file must be regular and have one link".into());
    }
    Ok(file)
}

/// Owns both descriptor and lock. Closing the parent descriptor alone is not
/// enough when a concurrent fork inherits its open-file description before exec.
#[cfg(unix)]
struct LockedFile(fs::File);

#[cfg(unix)]
impl std::ops::Deref for LockedFile {
    type Target = fs::File;
    fn deref(&self) -> &Self::Target { &self.0 }
}

#[cfg(unix)]
impl std::ops::DerefMut for LockedFile {
    fn deref_mut(&mut self) -> &mut Self::Target { &mut self.0 }
}

#[cfg(unix)]
impl Drop for LockedFile {
    fn drop(&mut self) {
        // Unlock before close on success and every early error. Each attempt is
        // nonblocking; a signal interruption may be retried without an open-ended loop.
        for _ in 0..3 {
            if unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN | libc::LOCK_NB) } == 0
                || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                break;
            }
        }
    }
}

#[cfg(unix)]
fn lock(file: fs::File, exclusive: bool) -> Result<LockedFile, String> {
    let op = if exclusive { libc::LOCK_EX } else { libc::LOCK_SH };
    if unsafe { libc::flock(file.as_raw_fd(), op | libc::LOCK_NB) } != 0 {
        return Err("Hook state is busy".into());
    }
    Ok(LockedFile(file))
}

#[cfg(unix)]
fn lock_writer(file: fs::File) -> Result<LockedFile, String> {
    let started = Instant::now();
    loop {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(LockedFile(file));
        }
        let error = std::io::Error::last_os_error();
        if !matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted)
            || started.elapsed() >= WRITE_LOCK_BUDGET {
            return Err("Hook state is busy or cannot be locked".into());
        }
        std::thread::sleep(Duration::from_millis(5).min(WRITE_LOCK_BUDGET.saturating_sub(started.elapsed())));
    }
}

fn bounded_read(file: &mut fs::File, limit: u64) -> Result<Vec<u8>, String> {
    if file.metadata().map_err(|_| "Cannot inspect file")?.len() > limit {
        return Err("Hook file exceeds size limit".into());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).map_err(|_| "Cannot read hook file")?;
    if bytes.len() as u64 > limit { return Err("Hook file exceeds size limit".into()); }
    Ok(bytes)
}

/// Records one hook event under `dir`, without following any symlink components.
#[cfg(unix)]
pub fn record(input: &HookInput, dir: &Path, now_ms: i64) -> Result<(), String> {
    let path = state_path(dir, &input.session_id).ok_or("Invalid session id")?;
    let directory = safe_directory(dir, true)?;
    if input.hook_event_name == "SessionStart" { prune_stale(dir, &directory); }
    let file = safe_file(&directory, path.file_name().ok_or("Invalid state path")?, true)?;
    let mut file = lock_writer(file)?;
    let content = bounded_read(&mut file, MAX_STATE_BYTES)?;
    let current: HookSessionState = serde_json::from_slice(&content).unwrap_or_default();
    let mut correlated = input.clone();
    if input.hook_event_name == "PermissionRequest" {
        // This event has no tool_use_id. Bind only a unique exact transcript
        // tool/name/input match in a complete bounded observation. This is
        // transcript correlation, not proof of hook event order; ambiguity stays unbound.
        correlated.tool_use_id = correlate_request(input);
        // A completion in state may precede its transcript result. It cannot
        // identify an identity-free request as a duplicate rather than a new wait.
        if current.resolved.iter().any(|p| p.agent_id == input.agent_id
            && p.tool_use_id.is_some() && p.tool_use_id == correlated.tool_use_id) {
            correlated.tool_use_id = None;
        }
    }
    let mut next = apply(current.clone(), &correlated, now_ms).unwrap_or_default();
    if next.pending.len() > MAX_PERMISSIONS || next.resolved.len() > MAX_PERMISSIONS {
        return Err("Too many hook call identities".into());
    }
    if next != current || content.is_empty() {
        let mut bytes = serde_json::to_vec(&next).map_err(|_| "Cannot encode hook state")?;
        // Historical completions must not consume the byte budget needed for a
        // current request/completion. Pending evidence retains priority.
        while bytes.len() as u64 > MAX_STATE_BYTES && !next.resolved.is_empty() {
            next.resolved.remove(0);
            bytes = serde_json::to_vec(&next).map_err(|_| "Cannot encode hook state")?;
        }
        if bytes.len() as u64 > MAX_STATE_BYTES { return Err("Hook state exceeds size limit".into()); }
        file.set_len(0).map_err(|_| "Cannot write hook state")?;
        file.seek(SeekFrom::Start(0)).map_err(|_| "Cannot write hook state")?;
        file.write_all(&bytes).map_err(|_| "Cannot write hook state")?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn record(_input: &HookInput, _dir: &Path, _now_ms: i64) -> Result<(), String> {
    Err("Safe hook state IO requires Unix".into())
}

#[cfg(unix)]
fn prune_stale(path: &Path, directory: &fs::File) {
    let Ok(entries) = fs::read_dir(path) else { return; };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(id) = name.to_str().and_then(|n| n.strip_suffix(".json")) else { continue; };
        if state_path(path, id).is_none() { continue; }
        let Ok(file) = safe_file(directory, &name, false) else { continue; };
        let Ok(file) = lock(file, true) else { continue; };
        let stale = file.metadata().and_then(|m| m.modified()).ok()
            .and_then(|modified| modified.elapsed().ok()).is_some_and(|age| age > STALE_STATE_AGE);
        if stale {
            use std::os::unix::ffi::OsStrExt;
            if let Ok(name) = std::ffi::CString::new(name.as_bytes()) {
                unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0); }
            }
        }
    }
}

/// Bounded regular-file state read. Invalid/busy state contributes no hook evidence.
#[cfg(unix)]
pub fn read_state_in(dir: &Path, session_id: &str) -> Option<HookSessionState> {
    let path = state_path(dir, session_id)?;
    let directory = safe_directory(dir, false).ok()?;
    let file = safe_file(&directory, path.file_name()?, false).ok()?;
    let mut file = lock(file, false).ok()?;
    let bytes = bounded_read(&mut file, MAX_STATE_BYTES).ok()?;
    let state: HookSessionState = serde_json::from_slice(&bytes).ok()?;
    (state.pending.len() <= MAX_PERMISSIONS && state.resolved.len() <= MAX_PERMISSIONS).then_some(state)
}

#[cfg(not(unix))]
pub fn read_state_in(_dir: &Path, _session_id: &str) -> Option<HookSessionState> { None }

struct TranscriptCalls {
    calls: Vec<(String, String, Value)>, resolved: Vec<String>,
    ambiguous: Vec<String>, complete: bool,
}

#[cfg(unix)]
fn transcript_calls(path: &Path) -> Option<TranscriptCalls> {
    let directory = safe_directory(path.parent()?, false).ok()?;
    let mut file = safe_file(&directory, path.file_name()?, false).ok()?;
    // A partial bounded prefix is useful for explicit results but not for binding.
    let before = file.metadata().ok()?;
    let size = before.len();
    let mut bytes = Vec::new();
    (&mut file).take(MAX_TRANSCRIPT_BYTES).read_to_end(&mut bytes).ok()?;
    let after = file.metadata().ok()?;
    let stable = size == after.len() && before.modified().ok().is_some()
        && before.modified().ok() == after.modified().ok();
    let mut out = TranscriptCalls { calls: Vec::new(), resolved: Vec::new(), ambiguous: Vec::new(),
        complete: stable && size <= MAX_TRANSCRIPT_BYTES && bytes.len() as u64 == size };
    for line in bytes.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
        let Ok(value) = serde_json::from_slice::<Value>(line) else { out.complete = false; continue; };
        // A compacted observation cannot establish uniqueness across older calls.
        if value["type"] == "summary" || value["subtype"] == "compact_boundary" { out.complete = false; }
        if !value.is_object() { out.complete = false; continue; }
        if value["type"] != "assistant" && value["type"] != "user" {
            // An unsupported outer record cannot hide tool-bearing blocks while
            // another matching call is considered uniquely covered.
            if let Some(content) = value.get("message").and_then(|m| m.get("content")) {
                let mut unknown_block = |block: &Value| {
                    if matches!(block["type"].as_str(), Some("tool_use" | "tool_result")) {
                        out.complete = false;
                        if block["type"] == "tool_use" {
                            if let Some(id) = block["id"].as_str() { out.ambiguous.push(id.to_string()); }
                        }
                    }
                };
                if let Some(blocks) = content.as_array() { for block in blocks { unknown_block(block); } }
                else { unknown_block(content); }
            }
            continue;
        }
        let Some(content) = value.get("message").and_then(|m| m.get("content")) else {
            out.complete = false; continue;
        };
        // String content is a legitimate plain-text record. Other non-array
        // shapes cannot establish coverage of call-bearing messages.
        if content.is_string() { continue; }
        let Some(blocks) = content.as_array() else {
            out.complete = false;
            if content["type"] == "tool_use" {
                if let Some(id) = content["id"].as_str() { out.ambiguous.push(id.to_string()); }
            }
            continue;
        };
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("tool_use") => {
                    let id = block.get("id").and_then(Value::as_str).filter(|id| !id.is_empty());
                    if value["type"] == "assistant" {
                        if let (Some(id), Some(name), Some(input)) = (id,
                            block.get("name").and_then(Value::as_str).filter(|name| !name.is_empty()),
                            block.get("input").filter(|input| input.is_object())) {
                            let call = (id.to_string(), name.to_string(), input.clone());
                            if out.calls.iter().any(|old| old.0 == call.0 && old != &call) {
                                out.complete = false; out.ambiguous.push(id.to_string());
                            }
                            if !out.calls.contains(&call) { out.calls.push(call); }
                            continue;
                        }
                    }
                    out.complete = false;
                    if let Some(id) = id { out.ambiguous.push(id.to_string()); }
                }
                Some("tool_result") => {
                    if value["type"] == "user"
                        && block.get("content").is_none_or(|c| c.is_string() || c.is_array())
                        && block.get("is_error").is_none_or(Value::is_boolean) {
                        if let Some(id) = block.get("tool_use_id").and_then(Value::as_str).filter(|id| !id.is_empty()) {
                            out.resolved.push(id.to_string()); continue;
                        }
                    }
                    out.complete = false;
                }
                Some("text") => {
                    if !block.get("text").is_some_and(Value::is_string) { out.complete = false; }
                }
                // Non-tool blocks do not claim a call identity, but a missing
                // type or unsupported message shape makes coverage uncertain.
                Some("thinking" | "redacted_thinking" | "image" | "document") => {}
                _ => { out.complete = false; }
            }
        }
    }
    Some(out)
}

#[cfg(not(unix))]
fn transcript_calls(_path: &Path) -> Option<TranscriptCalls> { None }

fn correlate_request(input: &HookInput) -> Option<String> {
    // Common transcript_path points at the parent transcript for subagent events.
    let mut path = input.transcript_path.clone()?;
    if let Some(agent) = &input.agent_id {
        if agent.is_empty() || !agent.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') { return None; }
        path = path.with_extension("").join("subagents").join(format!("agent-{agent}.jsonl"));
    }
    let evidence = transcript_calls(&path)?;
    if !evidence.complete { return None; }
    let mut matching = evidence.calls.iter().filter(|(_, name, args)|
        Some(name) == input.tool_name.as_ref() && Some(args) == input.tool_input.as_ref());
    let first = matching.next()?;
    if matching.next().is_some() { return None; }
    // A stable transcript can lag a new request. A completed historical match
    // is not evidence that this event belongs to that old call.
    if evidence.resolved.contains(&first.0) { return None; }
    Some(first.0.clone())
}

/// Only an explicit result for the correlated call proves resolution. Absence,
/// truncation, malformed input and a missing transcript mean uncertainty.
pub(crate) fn prompt_is_open(prompt: &PendingPermission, transcript: &Path) -> bool {
    let Some(id) = &prompt.tool_use_id else { return true; };
    transcript_calls(transcript).is_none_or(|evidence| evidence.ambiguous.contains(id) || !evidence.resolved.contains(id))
}

pub(crate) fn prompt_input(prompt: &PendingPermission, transcript: &Path) -> Option<Value> {
    let id = prompt.tool_use_id.as_ref()?;
    let evidence = transcript_calls(transcript)?;
    if evidence.ambiguous.contains(id) || evidence.resolved.contains(id) { return None; }
    evidence.calls.into_iter().find(|(call, name, _)| call == id && name == &prompt.tool_name)
        .map(|(_, _, input)| input)
}

fn read_input(reader: &mut impl Read) -> Result<HookInput, String> {
    let mut bytes = Vec::new();
    reader.take(MAX_INPUT_BYTES + 1).read_to_end(&mut bytes).map_err(|_| "Cannot read hook input")?;
    if bytes.len() as u64 > MAX_INPUT_BYTES { return Err("Hook input exceeds size limit".into()); }
    let input: HookInput = serde_json::from_slice(&bytes).map_err(|_| "Invalid hook JSON")?;
    if input.hook_event_name == "PermissionRequest" && input.tool_name.as_ref().is_none_or(|name| name.is_empty()) {
        return Err("PermissionRequest requires tool_name".into());
    }
    Ok(input)
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

/// Settings reads use the same explicit no-symlink, regular/single-link policy
/// as state IO. Unknown/unreadable evidence is not a durable disabled installation.
#[cfg(unix)]
pub(crate) fn read_settings(path: &Path) -> Result<Vec<u8>, String> {
    let directory = safe_directory(path.parent().ok_or("Invalid settings path")?, false)?;
    let mut file = safe_file(&directory, path.file_name().ok_or("Invalid settings path")?, false)?;
    bounded_read(&mut file, MAX_SETTINGS_BYTES)
}

#[cfg(not(unix))]
pub(crate) fn read_settings(_path: &Path) -> Result<Vec<u8>, String> {
    Err("Safe settings IO requires Unix".into())
}

// Only successful observations are cached. File IO never holds this global mutex.
type InstalledStamp = (PathBuf, Vec<u8>, bool);
static INSTALLED_CACHE: LazyLock<Mutex<Option<InstalledStamp>>> =
    LazyLock::new(|| Mutex::new(None));

fn installed_cached(config: &Path) -> bool {
    let path = config.join("settings.json");
    let Ok(bytes) = read_settings(&path) else { return false; };
    // Compare the bounded bytes, not mtime: mode recovery or same-mtime rewrites
    // must be observable. Safe IO is retried on every call, even a cached miss.
    if let Ok(cache) = INSTALLED_CACHE.try_lock() {
        if let Some((cached_path, cached_bytes, value)) = cache.as_ref() {
            if *cached_path == path && *cached_bytes == bytes { return *value; }
        }
    }
    let Ok(settings) = serde_json::from_slice::<Value>(&bytes) else { return false; };
    if !settings.is_object() { return false; }
    let value = installed(&settings);
    if let Ok(mut cache) = INSTALLED_CACHE.try_lock() { *cache = Some((path, bytes, value)); }
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
    let Some(command) = hook["command"].as_str() else { return false; };
    let Some(quoted) = command.strip_suffix(" hooks") else { return false; };
    let Some(path) = quoted.strip_prefix('\'').and_then(|p| p.strip_suffix('\'')) else { return false; };
    let decoded = path.replace("'\\''", "'");
    let executable = Path::new(&decoded);
    executable.is_absolute() && executable.file_name().is_some_and(|name| name == "c9watch")
        && bridge_command(executable).is_ok_and(|expected| expected == command)
        && *hook == json!({"type": "command", "command": command, "async": true})
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
        groups.retain_mut(|group| {
            let Some(entries) = group.get_mut("hooks").and_then(Value::as_array_mut) else { return true; };
            let owned = entries.iter().any(is_bridge_hook);
            entries.retain(|hook| !is_bridge_hook(hook));
            !owned || !entries.is_empty()
        });
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
    let input = read_input(&mut stdin)?;
    record(&input, &state_dir()?, chrono::Utc::now().timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> tempfile::TempDir {
        // macOS exposes its temporary root through /var -> /private/var.
        // Fixtures use a real parent; product IO must still reject symlinks.
        let parent = std::env::temp_dir().canonicalize().unwrap();
        tempfile::tempdir_in(parent).unwrap()
    }

    // Internal fold fixture: call ids have already been correlated by record.
    fn event(name: &str, agent: Option<&str>, tool: Option<&str>) -> HookInput {
        HookInput {
            session_id: "s1".into(),
            hook_event_name: name.into(),
            agent_id: agent.map(str::to_owned),
            tool_name: tool.map(str::to_owned),
            tool_use_id: tool.map(|tool| format!("call-{tool}")),
            transcript_path: None,
            tool_input: None,
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
    fn stop_without_call_identity_preserves_all_prompts() {
        let state = fold(&[
            event("PermissionRequest", None, Some("Write")),
            event("PermissionRequest", Some("a1"), Some("Bash")),
            event("Stop", None, None),
        ])
        .unwrap();
        assert_eq!(tools(&state), vec![(None, "Write"), (Some("a1"), "Bash")]);
        let state = apply(state, &event("SubagentStop", Some("a1"), None), 2).unwrap();
        assert_eq!(state.pending.len(), 2);
    }

    #[test]
    fn batch_and_user_prompt_without_call_identity_do_not_clear_waits() {
        let state = fold(&[
            event("PermissionRequest", None, Some("Edit")),
            event("PermissionRequest", Some("a1"), Some("Bash")),
            event("PostToolBatch", Some("a1"), None),
        ])
        .unwrap();
        assert_eq!(tools(&state), vec![(None, "Edit"), (Some("a1"), "Bash")]);
        let state = apply(state, &event("UserPromptSubmit", None, None), 2).unwrap();
        assert_eq!(state.pending.len(), 2);
    }

    #[test]
    fn delayed_session_end_cannot_remove_newer_waits() {
        assert_eq!(fold(&[
            event("PermissionRequest", None, Some("Bash")),
            event("SessionEnd", None, None)
        ])
        .unwrap().pending.len(), 1);
    }

    #[test]
    fn record_round_trips_and_rejects_path_like_ids() {
        let dir = fixture();
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
        assert_eq!(read_state_in(dir.path(), "s1").unwrap().pending.len(), 1);

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
        let dir = fixture();
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

#[cfg(test)]
mod repair_regressions {
    use super::*;
    fn wire(event: &str, tool: Option<&str>, id: Option<&str>, transcript: Option<&Path>, args: Option<Value>) -> HookInput {
        serde_json::from_value(json!({"session_id":"s1", "hook_event_name":event,
            "tool_name":tool, "tool_use_id":id, "transcript_path":transcript, "tool_input":args})).unwrap()
    }
    fn fixture() -> tempfile::TempDir {
        // macOS exposes its temporary root through /var -> /private/var.
        // Fixtures use a real parent; product IO must still reject symlinks.
        let parent = std::env::temp_dir().canonicalize().unwrap();
        tempfile::tempdir_in(parent).unwrap()
    }
    fn call(id: &str, command: &str) -> String {
        json!({"type":"assistant","uuid":id,"timestamp":"2026-09-23T00:00:00Z",
            "message":{"model":"m","id":id,"role":"assistant","content":[
                {"type":"tool_use","id":id,"name":"Bash","input":{"command":command}}]}}).to_string()
    }
    fn result(id: &str) -> String {
        json!({"type":"user","uuid":"result","timestamp":"2026-09-23T00:00:01Z",
            "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":id,"content":"done"}]}}).to_string()
    }
    fn request(transcript: &Path, command: &str) -> HookInput {
        // PermissionRequest wire payload intentionally supplies NO tool_use_id.
        wire("PermissionRequest", Some("Bash"), None, Some(transcript), Some(json!({"command":command})))
    }
    #[test]
    fn delayed_lifecycle_events_do_not_clear_a_new_request() {
        let state = apply(HookSessionState::default(), &request(Path::new("/missing"), "make"), 10).unwrap();
        for event in ["UserPromptSubmit", "Stop", "StopFailure", "PostToolBatch", "SessionStart", "SessionEnd"] {
            let after = apply(state.clone(), &wire(event,None,None,None,None), 20);
            assert!(after.is_some_and(|s| s.pending.len() == 1), "delayed {event} cleared a wait");
        }
    }
    #[test]
    fn duplicate_request_deduplicates_and_exact_completion_clears_bound_entry() {
        let dir=fixture(); let transcript=dir.path().join("s1.jsonl");
        fs::write(&transcript,call("old","make")).unwrap();
        let req=request(&transcript,"make");
        record(&req,dir.path(),1).unwrap(); record(&req,dir.path(),2).unwrap();
        assert_eq!(read_state_in(dir.path(),"s1").unwrap().pending.len(),1,"duplicate pending prompt");
        record(&wire("PostToolUse",Some("Bash"),Some("old"),None,None),dir.path(),3).unwrap();
        assert!(read_state_in(dir.path(),"s1").unwrap().pending.is_empty());
        record(&req,dir.path(),4).unwrap();
        let uncertain=read_state_in(dir.path(),"s1").unwrap();
        assert_eq!(uncertain.pending.len(),1,"identity-free late delivery may be a new request");
        assert!(uncertain.pending[0].tool_use_id.is_none());
    }
    #[test]
    fn earlier_completion_cannot_clear_a_different_call_of_same_tool() {
        let dir=fixture();let transcript=dir.path().join("s1.jsonl");
        fs::write(&transcript,format!("{}\n{}\n{}\n",call("old","first"),result("old"),call("new","second"))).unwrap();
        record(&request(&transcript,"second"),dir.path(),1).unwrap();
        record(&wire("PostToolUse",Some("Bash"),Some("old"),None,None),dir.path(),2).unwrap();
        let state=read_state_in(dir.path(),"s1").unwrap();
        assert_eq!(state.pending.len(),1,"tool-name completion erased newer call");
        assert_eq!(serde_json::to_value(&state.pending[0]).unwrap()["toolUseId"],"new");
    }
    #[test]
    fn completion_without_id_cannot_erase_a_correlated_prompt() {
        let dir=fixture();let transcript=dir.path().join("s1.jsonl");fs::write(&transcript,call("new","make")).unwrap();
        record(&request(&transcript,"make"),dir.path(),1).unwrap();
        record(&wire("PostToolUse",Some("Bash"),None,None,None),dir.path(),2).unwrap();
        assert_eq!(read_state_in(dir.path(),"s1").unwrap().pending.len(),1);
    }
    #[test]
    fn same_input_in_two_calls_is_not_associated_by_tool_name_or_receipt_time() {
        let dir=fixture();let transcript=dir.path().join("s1.jsonl");
        fs::write(&transcript,format!("{}\n{}\n{}\n",call("old","make"),result("old"),call("new","make"))).unwrap();
        record(&request(&transcript,"make"),dir.path(),999).unwrap();
        let p=read_state_in(dir.path(),"s1").unwrap().pending.remove(0);
        assert!(serde_json::to_value(&p).unwrap()["toolUseId"].is_null(),"ambiguous call attached to a guessed id");
        assert!(prompt_input(&p,&transcript).is_none(),"arguments came from an unrelated call");
    }
    #[test]
    fn symlink_state_cannot_read_or_overwrite_an_unrelated_file() {
        let dir=fixture();let target=dir.path().join("settings.json");fs::write(&target,b"keep").unwrap();
        std::os::unix::fs::symlink(&target,dir.path().join("s1.json")).unwrap();
        assert!(record(&wire("PermissionRequest",Some("Bash"),None,None,None),dir.path(),1).is_err());
        assert_eq!(fs::read(&target).unwrap(),b"keep");
        assert!(read_state_in(dir.path(),"s1").is_none());
    }
    #[test]
    fn symlink_directory_component_is_rejected() {
        let dir=fixture();let actual=dir.path().join("actual");fs::create_dir(&actual).unwrap();
        let link=dir.path().join("link");std::os::unix::fs::symlink(&actual,&link).unwrap();
        assert!(record(&wire("SessionStart",None,None,None,None),&link.join("nested"),1).is_err());
        assert!(!actual.join("nested").exists());
    }
    #[test]
    fn nonregular_directory_state_is_rejected() {
        let dir=fixture();fs::create_dir(dir.path().join("s1.json")).unwrap();
        assert!(read_state_in(dir.path(),"s1").is_none());
        assert!(record(&wire("SessionStart",None,None,None,None),dir.path(),1).is_err());
    }
    #[test]
    fn fifo_state_is_rejected_without_waiting_for_a_writer() {
        use std::os::unix::ffi::OsStrExt;
        let dir=fixture();let path=dir.path().join("s1.json");let c=std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe {libc::mkfifo(c.as_ptr(),0o600)},0);
        assert!(read_state_in(dir.path(),"s1").is_none());
        assert!(record(&wire("SessionStart",None,None,None,None),dir.path(),1).is_err());
    }
    #[test]
    fn oversized_state_is_rejected_without_rewriting_it() {
        let dir=fixture();let path=dir.path().join("s1.json");let bytes=vec![b' ';128*1024+1];fs::write(&path,&bytes).unwrap();
        assert!(read_state_in(dir.path(),"s1").is_none(),"oversized state was read as empty authority");
        assert!(record(&wire("PermissionRequest",Some("Bash"),None,None,None),dir.path(),1).is_err());
        assert_eq!(fs::read(path).unwrap(),bytes);
    }
    #[test]
    fn streaming_input_is_bounded_even_for_unknown_fields() {
        let payload=json!({"session_id":"s1","hook_event_name":"SessionStart","extra":"x".repeat(256*1024)}).to_string();
        let mut cursor=std::io::Cursor::new(payload.as_bytes());
        assert!(read_input(&mut cursor).is_err(),"oversized unknown field bypassed input bound");
        assert!(cursor.position()<=256*1024+1,"read past input bound");
    }
    #[test]
    fn permission_request_schema_requires_a_nonempty_tool_name() {
        for payload in [r#"{"session_id":"s1","hook_event_name":"PermissionRequest"}"#,
            r#"{"session_id":"s1","hook_event_name":"PermissionRequest","tool_name":""}"#,
            r#"{"session_id":"s1","hook_event_name":"PermissionRequest","tool_name":23}"#] {
            assert!(read_input(&mut payload.as_bytes()).is_err(),"schema error accepted");
        }
    }
    #[test]
    fn exact_hook_ownership_preserves_unrelated_commands_and_custom_options() {
        let other=json!({"type":"command","command":"'/Users/alice/c9watch-project/audit' hooks","async":true});
        let custom=json!({"type":"command","command":"'/usr/local/bin/c9watch' hooks","async":false,"timeout":10});
        let settings=json!({"hooks":{"Stop":[{"matcher":"*","hooks":[other,custom]},{"hooks":[]}]},"model":"opus"});
        let next=configured(settings.clone(),Path::new("/new/c9watch")).unwrap();
        assert_eq!(unconfigured(next).unwrap(),settings,"unrelated hook or empty group was removed");
    }
    #[test]
    fn exact_escaped_bridge_command_is_owned_and_idempotent() {
        let next=configured(json!({}),Path::new("/tmp/it's/c9watch")).unwrap();
        assert!(installed(&next));
        assert_eq!(configured(next.clone(),Path::new("/tmp/it's/c9watch")).unwrap(),next);
        assert_eq!(unconfigured(next).unwrap(),json!({}));
    }
}

#[cfg(test)]
mod repair_additional_regressions {
    use super::*;

    fn fixture() -> tempfile::TempDir {
        // macOS exposes its temporary root through /var -> /private/var.
        // Fixtures use a real parent; product IO must still reject symlinks.
        let parent = std::env::temp_dir().canonicalize().unwrap();
        tempfile::tempdir_in(parent).unwrap()
    }
    fn event(name:&str,id:Option<&str>,transcript:Option<&Path>)->HookInput {
        serde_json::from_value(json!({"session_id":"s1","hook_event_name":name,"tool_name":"Bash",
            "tool_use_id":id,"transcript_path":transcript,"tool_input":{"command":"make"}})).unwrap()
    }
    fn call(id:&str)->String {
        json!({"type":"assistant","uuid":id,"timestamp":"2026-09-23T00:00:00Z",
            "message":{"role":"assistant","model":"m","id":id,"content":[
                {"type":"tool_use","id":id,"name":"Bash","input":{"command":"make"}}]}}).to_string()
    }
    #[test]
    fn completion_before_identity_free_request_preserves_uncertain_signal() {
        let dir=fixture();let transcript=dir.path().join("s1.jsonl");fs::write(&transcript,call("old")).unwrap();
        record(&event("PostToolUse",Some("old"),None),dir.path(),1).unwrap();
        record(&event("PermissionRequest",None,Some(&transcript)),dir.path(),2).unwrap();
        let uncertain=read_state_in(dir.path(),"s1").unwrap();
        assert_eq!(uncertain.pending.len(),1,"completion cannot identify a later identity-free request");
        assert!(uncertain.pending[0].tool_use_id.is_none());
    }
    #[test]
    fn hardlink_state_cannot_overwrite_an_unrelated_file() {
        let dir=fixture();let target=dir.path().join("settings.json");fs::write(&target,b"keep").unwrap();
        fs::hard_link(&target,dir.path().join("s1.json")).unwrap();
        assert!(record(&event("PermissionRequest",None,None),dir.path(),1).is_err());
        assert_eq!(fs::read(target).unwrap(),b"keep");
        assert!(read_state_in(dir.path(),"s1").is_none());
    }
    #[test]
    fn held_state_lock_does_not_block_the_reader_or_writer() {
        use std::os::unix::io::AsRawFd;
        let dir=fixture();let path=dir.path().join("s1.json");fs::write(&path,b"{\"pending\":[]}").unwrap();
        let file=fs::OpenOptions::new().read(true).write(true).open(path).unwrap();
        assert_eq!(unsafe{libc::flock(file.as_raw_fd(),libc::LOCK_EX|libc::LOCK_NB)},0);
        assert!(read_state_in(dir.path(),"s1").is_none());
        assert!(record(&event("PermissionRequest",None,None),dir.path(),1).is_err());
    }
    #[test]
    fn compaction_marker_prevents_claiming_a_unique_call_association() {
        let dir=fixture();let transcript=dir.path().join("s1.jsonl");
        fs::write(&transcript,format!("{{\"type\":\"summary\",\"summary\":\"older calls omitted\"}}\n{}\n",call("new"))).unwrap();
        record(&event("PermissionRequest",None,Some(&transcript)),dir.path(),1).unwrap();
        let p=read_state_in(dir.path(),"s1").unwrap().pending.remove(0);
        assert!(serde_json::to_value(&p).unwrap()["toolUseId"].is_null());
        assert!(prompt_input(&p,&transcript).is_none());
    }
}

#[cfg(all(test, unix))]
mod review_regressions_v3 {
    use super::*;
    use std::os::unix::{io::AsRawFd, fs::PermissionsExt};
    use std::sync::mpsc;

    fn fixture() -> tempfile::TempDir {
        tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap()
    }
    fn call(id: &str, command: &str) -> String {
        json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":id,"name":"Bash","input":{"command":command}}]}}).to_string()
    }
    fn done(id: &str) -> String {
        json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":id,"content":"done"}]}}).to_string()
    }
    fn wire(event: &str, tool: &str, id: Option<&str>, path: Option<&Path>) -> HookInput {
        serde_json::from_value(json!({"session_id":"s1","hook_event_name":event,"tool_name":tool,
            "tool_use_id":id,"transcript_path":path,"tool_input":{"command":"make"}})).unwrap()
    }
    fn bound(id: &str) -> PendingPermission {
        PendingPermission { agent_id:None, tool_name:"Bash".into(), requested_at:1, tool_use_id:Some(id.into()) }
    }
    fn installed_settings() -> Vec<u8> {
        serde_json::to_vec(&json!({"hooks":{"PermissionRequest":[{"hooks":[{
            "type":"command","command":"'/fixture/c9watch' hooks","async":true}]}]}})).unwrap()
    }
    #[test]
    fn lagging_transcript_new_request_must_not_bind_to_completed_old_call() {
        let dir=fixture();let path=dir.path().join("s1.jsonl");
        fs::write(&path,format!("{}\n{}\n",call("old","make"),done("old"))).unwrap();
        record(&wire("PostToolUse","Bash",Some("old"),None),dir.path(),1).unwrap();
        record(&wire("PermissionRequest","Bash",None,Some(&path)),dir.path(),2).unwrap();
        let state=read_state_in(dir.path(),"s1").unwrap();
        assert_eq!(state.pending.len(),1,"new request dropped as completed old call");
        assert!(state.pending[0].tool_use_id.is_none());
        assert!(prompt_input(&state.pending[0],&path).is_none());
    }
    #[test]
    fn transcript_result_without_hook_tombstone_still_cannot_identify_new_request() {
        let dir=fixture();let path=dir.path().join("s1.jsonl");
        fs::write(&path,format!("{}\n{}\n",call("old","make"),done("old"))).unwrap();
        record(&wire("PermissionRequest","Bash",None,Some(&path)),dir.path(),2).unwrap();
        let state=read_state_in(dir.path(),"s1").unwrap();
        assert_eq!(state.pending.len(),1);assert!(state.pending[0].tool_use_id.is_none());
    }
    #[test]
    fn reader_contention_eventually_persists_hook_only_read_prompt() {
        let dir=fixture();record(&wire("SessionStart","Read",None,None),dir.path(),1).unwrap();
        let held=fs::OpenOptions::new().read(true).open(dir.path().join("s1.json")).unwrap();
        assert_eq!(unsafe {libc::flock(held.as_raw_fd(),libc::LOCK_SH|libc::LOCK_NB)},0);
        let path=dir.path().to_path_buf();let (tx,rx)=mpsc::channel();
        let writer=std::thread::spawn(move || {tx.send(record(&wire("PermissionRequest","Read",None,None),&path,2)).unwrap();});
        let early=rx.recv_timeout(Duration::from_millis(30)).ok();
        drop(held); // A normal poll reader releases while the async hook is still alive.
        let result=early.unwrap_or_else(|| rx.recv_timeout(Duration::from_secs(1)).unwrap());
        writer.join().unwrap();
        let state=read_state_in(dir.path(),"s1").unwrap();
        assert_eq!(state.pending.len(),1,"ordinary reader collision permanently lost request: {result:?}");
        assert_eq!(state.pending[0].tool_name,"Read");assert!(result.is_ok());
        assert!(crate::session::permissions::PermissionChecker::default().is_auto_approved("Read",&json!({})),
            "this signal is hook-only; fallback whitelist cannot rescue its loss");
    }
    #[test]
    fn two_writers_after_short_exclusive_contention_preserve_both_requests() {
        let dir=fixture();record(&wire("SessionStart","Read",None,None),dir.path(),1).unwrap();
        let held=fs::OpenOptions::new().read(true).write(true).open(dir.path().join("s1.json")).unwrap();
        assert_eq!(unsafe {libc::flock(held.as_raw_fd(),libc::LOCK_EX|libc::LOCK_NB)},0);
        let (tx,rx)=mpsc::channel();let mut writers=Vec::new();
        for tool in ["Read","Glob"] {
            let path=dir.path().to_path_buf();let tx=tx.clone();
            writers.push(std::thread::spawn(move || tx.send(record(&wire("PermissionRequest",tool,None,None),&path,2)).unwrap()));
        }
        std::thread::sleep(Duration::from_millis(30));drop(held);
        let results=[rx.recv_timeout(Duration::from_secs(1)).unwrap(),rx.recv_timeout(Duration::from_secs(1)).unwrap()];
        for writer in writers {writer.join().unwrap();}
        let state=read_state_in(dir.path(),"s1").unwrap();
        assert_eq!(state.pending.len(),2,"ordinary writer collisions lost evidence: {results:?}");
        assert!(results.iter().all(Result::is_ok));
    }
    #[test]
    fn indefinitely_held_lock_has_finite_writer_budget_and_immediate_reader() {
        let dir=fixture();record(&wire("SessionStart","Read",None,None),dir.path(),1).unwrap();
        let held=fs::OpenOptions::new().read(true).write(true).open(dir.path().join("s1.json")).unwrap();
        assert_eq!(unsafe {libc::flock(held.as_raw_fd(),libc::LOCK_EX|libc::LOCK_NB)},0);
        let start=Instant::now();assert!(read_state_in(dir.path(),"s1").is_none());
        assert!(start.elapsed()<WRITE_LOCK_BUDGET,"reader must not wait for hostile lock");
        let start=Instant::now();let result=record(&wire("PermissionRequest","Read",None,None),dir.path(),2);
        assert!(result.is_err());assert!(start.elapsed()<Duration::from_secs(1),"hostile lock waits indefinitely");
        drop(held);
    }
    #[test]
    fn malformed_message_schema_must_not_establish_unique_call_binding() {
        let dir=fixture();let path=dir.path().join("s1.jsonl");
        let bad=json!({"type":"assistant","message":{"content":{"type":"tool_use","id":"old","name":"Bash","input":{"command":"make"}}}});
        fs::write(&path,format!("{bad}\n{}\n",call("new","make"))).unwrap();
        record(&wire("PermissionRequest","Bash",None,Some(&path)),dir.path(),2).unwrap();
        assert!(read_state_in(dir.path(),"s1").unwrap().pending[0].tool_use_id.is_none());
    }
    #[test]
    fn invalid_call_blocks_remain_uncertain_but_legitimate_text_records_bind() {
        let dir=fixture();let path=dir.path().join("s1.jsonl");
        for content in [Value::Null,json!(7),json!([null]),json!([{"type":"tool_use","id":"old","name":"Bash","input":[]}]),
            json!([{"type":"tool_result","tool_use_id":"new"}]),json!([{"type":"text","text":3}])] {
            let bad=json!({"type":"assistant","message":{"content":content}});
            fs::write(&path,format!("{bad}\n{}\n",call("new","make"))).unwrap();
            assert!(correlate_request(&wire("PermissionRequest","Bash",None,Some(&path))).is_none(),"invalid call-bearing coverage bound a call");
        }
        for unknown in [json!({"message":{"content":[{"type":"tool_use","id":"old","name":"Bash","input":{"command":"make"}}]}}),
            json!({"type":"unsupported","message":{"content":{"type":"tool_use","id":"old","name":"Bash","input":{"command":"make"}}}}),json!(7)] {
            fs::write(&path,format!("{unknown}\n{}\n",call("new","make"))).unwrap();
            assert!(correlate_request(&wire("PermissionRequest","Bash",None,Some(&path))).is_none());
        }
        for content in [json!("plain text"),json!([{"type":"text","text":"plain text"}])] {
            let text=json!({"type":"assistant","message":{"content":content}});
            fs::write(&path,format!("{text}\n{}\n",call("new","make"))).unwrap();
            assert_eq!(correlate_request(&wire("PermissionRequest","Bash",None,Some(&path))),Some("new".into()));
        }
    }
    #[test]
    fn malformed_tool_result_cannot_prove_resolution() {
        let dir=fixture();let path=dir.path().join("s1.jsonl");
        fs::write(&path,format!("{}\n{}\n",call("same","make"),json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"same","content":null}]}}))).unwrap();
        assert!(prompt_is_open(&bound("same"),&path));
    }
    #[test]
    fn conflicting_call_identity_must_not_expose_guessed_arguments() {
        let dir=fixture();let path=dir.path().join("s1.jsonl");
        fs::write(&path,format!("{}\n{}\n",call("same","first-synthetic-input"),call("same","second-synthetic-input"))).unwrap();
        assert!(prompt_input(&bound("same"),&path).is_none());
    }
    #[test]
    fn valid_associated_arguments_survive_unrelated_transcript_uncertainty() {
        let dir=fixture();let path=dir.path().join("s1.jsonl");
        fs::write(&path,format!("{}\n{}\n{}\nmalformed\n",call("same","make"),call("other","first"),call("other","second"))).unwrap();
        assert_eq!(prompt_input(&bound("same"),&path),Some(json!({"command":"make"})),"privacy waiver preserves valid associated input");
        fs::write(&path,format!("{}\n{}\n",call("same","make"),json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"same","name":"Bash"}]}}))).unwrap();
        assert!(prompt_input(&bound("same"),&path).is_none(),"same-ID invalid evidence must not select first input");
    }
    #[test]
    fn installed_loader_fifo_is_unknown_without_waiting_for_writer() {
        use std::os::unix::ffi::OsStrExt;
        let dir=fixture();let path=dir.path().join("settings.json");
        let name=std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe {libc::mkfifo(name.as_ptr(),0o600)},0);
        let cfg=dir.path().to_path_buf();let (tx,rx)=mpsc::channel();
        let reader=std::thread::spawn(move || tx.send(installed_cached(&cfg)).unwrap());
        let timely=rx.recv_timeout(Duration::from_millis(250));
        if timely.is_err() {
            use std::os::unix::fs::OpenOptionsExt;
            let mut writer=fs::OpenOptions::new().write(true).custom_flags(libc::O_NONBLOCK).open(&path).unwrap();
            writer.write_all(&installed_settings()).unwrap();drop(writer);
        }
        reader.join().unwrap();
        assert!(timely.is_ok(),"settings FIFO blocked eligibility before secure state/fallback");
        assert!(!timely.unwrap());
    }
    #[test]
    fn installed_loader_rejects_symlink_nonregular_hardlink_and_size() {
        let dir=fixture();let source=dir.path().join("real.json");fs::write(&source,installed_settings()).unwrap();
        let linkdir=dir.path().join("link");fs::create_dir(&linkdir).unwrap();
        std::os::unix::fs::symlink(&source,linkdir.join("settings.json")).unwrap();assert!(!installed_cached(&linkdir));
        let hard=dir.path().join("hard");fs::create_dir(&hard).unwrap();fs::hard_link(&source,hard.join("settings.json")).unwrap();assert!(!installed_cached(&hard));
        let nonregular=dir.path().join("directory");fs::create_dir_all(nonregular.join("settings.json")).unwrap();assert!(!installed_cached(&nonregular));
        let oversized=dir.path().join("large");fs::create_dir(&oversized).unwrap();fs::write(oversized.join("settings.json"),vec![b' ';MAX_SETTINGS_BYTES as usize+1]).unwrap();assert!(!installed_cached(&oversized));
        let actual=dir.path().join("actual");fs::create_dir(&actual).unwrap();fs::write(actual.join("settings.json"),installed_settings()).unwrap();
        let alias=dir.path().join("alias");std::os::unix::fs::symlink(&actual,&alias).unwrap();assert!(!installed_cached(&alias));assert!(installed_cached(&actual));
    }
    #[test]
    fn settings_io_does_not_hold_or_wait_for_global_installed_cache() {
        let dir=fixture();fs::write(dir.path().join("settings.json"),installed_settings()).unwrap();
        let held=INSTALLED_CACHE.lock().unwrap();let cfg=dir.path().to_path_buf();let (tx,rx)=mpsc::channel();
        let reader=std::thread::spawn(move || tx.send(installed_cached(&cfg)).unwrap());
        let timely=rx.recv_timeout(Duration::from_millis(250));drop(held);reader.join().unwrap();
        assert!(timely.is_ok_and(|installed|installed),"settings loader waited for global cache");
    }
    #[test]
    fn installed_loader_retries_read_failure_after_permission_recovery_without_mtime_change() {
        let dir=fixture();let path=dir.path().join("settings.json");fs::write(&path,installed_settings()).unwrap();
        let mtime=fs::metadata(&path).unwrap().modified().unwrap();
        fs::set_permissions(&path,fs::Permissions::from_mode(0)).unwrap();
        assert!(fs::read(&path).is_err(),"hermetic basis must actually deny this uid");
        assert!(!installed_cached(dir.path()));
        fs::set_permissions(&path,fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(),mtime);
        assert!(installed_cached(dir.path()));
    }
    #[test]
    fn installed_loader_observes_same_mtime_settings_rewrite_and_invalid_json_recovery() {
        let dir=fixture();let path=dir.path().join("settings.json");fs::write(&path,b"malformed").unwrap();assert!(!installed_cached(dir.path()));
        fs::write(&path,installed_settings()).unwrap();assert!(installed_cached(dir.path()));
        let mtime=fs::metadata(&path).unwrap().modified().unwrap();
        fs::write(&path,br#"{"disableAllHooks":true}"#).unwrap();
        fs::OpenOptions::new().write(true).open(&path).unwrap().set_times(fs::FileTimes::new().set_modified(mtime)).unwrap();
        assert!(!installed_cached(dir.path()));
    }
    #[test]
    fn resolved_identity_limit_must_not_prevent_exact_current_completion() {
        let dir=fixture();let path=dir.path().join("s1.jsonl");
        for n in 0..256 {record(&wire("PostToolUse","Bash",Some(&format!("old-{n}")),None),dir.path(),n).unwrap();}
        let state=read_state_in(dir.path(),"s1").unwrap();assert_eq!(state.resolved.len(),256);
        fs::write(&path,call("current","make")).unwrap();record(&wire("PermissionRequest","Bash",None,Some(&path)),dir.path(),257).unwrap();
        assert_eq!(read_state_in(dir.path(),"s1").unwrap().pending[0].tool_use_id.as_deref(),Some("current"));
        let result=record(&wire("PostToolUse","Bash",Some("current"),None),dir.path(),258);
        let state=read_state_in(dir.path(),"s1").unwrap();
        assert!(state.pending.is_empty(),"exact current completion lost at capacity: {result:?}");
        assert!(result.is_ok());assert_eq!(state.resolved.len(),256);
        assert!(state.resolved.iter().any(|p|p.tool_use_id.as_deref()==Some("current")));
        for n in 256..300 {record(&wire("PostToolUse","Bash",Some(&format!("old-{n}")),None),dir.path(),n).unwrap();}
        assert_eq!(read_state_in(dir.path(),"s1").unwrap().resolved.len(),256);
        assert!(fs::metadata(dir.path().join("s1.json")).unwrap().len()<=MAX_STATE_BYTES);
    }
    #[test]
    fn resolved_byte_budget_cannot_prevent_current_request_or_exact_completion() {
        let dir=fixture();let path=dir.path().join("s1.jsonl");
        for n in 0..40 {
            let id=format!("{n}-{}","x".repeat(4096));
            let result=record(&wire("PostToolUse","Bash",Some(&id),None),dir.path(),n);
            assert!(result.is_ok(),"historical byte capacity prevents current updates: {result:?}");
            assert!(fs::metadata(dir.path().join("s1.json")).unwrap().len()<=MAX_STATE_BYTES);
        }
        fs::write(&path,call("current","make")).unwrap();
        record(&wire("PermissionRequest","Bash",None,Some(&path)),dir.path(),41).unwrap();
        assert_eq!(read_state_in(dir.path(),"s1").unwrap().pending.len(),1);
        let result=record(&wire("PostToolUse","Bash",Some("current"),None),dir.path(),42);
        assert!(read_state_in(dir.path(),"s1").unwrap().pending.is_empty(),"exact completion failed at byte capacity: {result:?}");
        assert!(result.is_ok());assert!(fs::metadata(dir.path().join("s1.json")).unwrap().len()<=MAX_STATE_BYTES);
    }

}

#[cfg(all(test, unix))]
mod settings_size_regression_v3 {
    use super::*;
    #[test]
    fn oversized_valid_installed_settings_are_unknown_and_not_rewritten() {
        let dir=tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let path=dir.path().join("settings.json");
        let mut bytes=serde_json::to_vec(&json!({"hooks":{"PermissionRequest":[{"hooks":[{
            "type":"command","command":"'/fixture/c9watch' hooks","async":true}]}]}})).unwrap();
        bytes.extend(vec![b' ';256*1024]);fs::write(&path,&bytes).unwrap();
        assert!(!installed_cached(dir.path()),"oversized valid settings were accepted");
        assert_eq!(fs::read(path).unwrap(),bytes);
    }
}

#[cfg(all(test, unix))]
mod inherited_lock_regressions_v4 {
    use super::*;

    // The child deliberately parks before exec. O_CLOEXEC does not close a fork
    // child's descriptor until exec; only async-signal-safe libc runs there.
    struct FixtureChild { pid: libc::pid_t, release: fs::File }
    impl FixtureChild {
        fn inherit(locked_fd: i32) -> Self {
            unsafe {
                assert_ne!(libc::fcntl(locked_fd,libc::F_GETFD)&libc::FD_CLOEXEC,0);
                let mut ready=[0;2];let mut finish=[0;2];
                assert_eq!(libc::pipe(ready.as_mut_ptr()),0);assert_eq!(libc::pipe(finish.as_mut_ptr()),0);
                let pid=libc::fork();assert!(pid>=0);
                if pid==0 {
                    libc::close(ready[0]);libc::close(finish[1]);
                    let byte=1u8;libc::write(ready[1],(&byte as *const u8).cast(),1);
                    let mut byte=0u8;
                    loop {
                        let n=libc::read(finish[0],(&mut byte as *mut u8).cast(),1);
                        if n>=0 || *libc_errno()!=libc::EINTR {break;}
                    }
                    libc::_exit(0);
                }
                libc::close(ready[1]);libc::close(finish[0]);
                let child=Self {pid,release:fs::File::from_raw_fd(finish[1])};
                let mut ready=fs::File::from_raw_fd(ready[0]);let mut byte=[0];
                ready.read_exact(&mut byte).unwrap();child
            }
        }
    }
    // Access errno without invoking Rust/allocator machinery after fork.
    #[cfg(target_os="macos")]
    unsafe fn libc_errno() -> *mut i32 {libc::__error()}
    #[cfg(not(target_os="macos"))]
    unsafe fn libc_errno() -> *mut i32 {libc::__errno_location()}
    impl Drop for FixtureChild {
        fn drop(&mut self) {
            self.release.write_all(&[1]).unwrap();
            loop {
                let mut status=0;let waited=unsafe {libc::waitpid(self.pid,&mut status,0)};
                if waited==self.pid {assert_eq!(status,0);break;}
                assert_eq!(std::io::Error::last_os_error().kind(),std::io::ErrorKind::Interrupted);
            }
        }
    }
    fn acquire(file: fs::File, exclusive: bool) -> Result<LockedFile,String> {
        if exclusive {lock_writer(file)} else {lock(file,false)}
    }
    fn check(exclusive: bool, early_error: bool) {
        let dir=tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let event:HookInput=serde_json::from_value(json!({"session_id":"s1","hook_event_name":"SessionStart"})).unwrap();
        record(&event,dir.path(),1).unwrap();
        let directory=safe_directory(dir.path(),false).unwrap();
        let file=safe_file(&directory,std::ffi::OsStr::new("s1.json"),exclusive).unwrap();
        let guarded=acquire(file,exclusive).unwrap();let child=FixtureChild::inherit(guarded.as_raw_fd());
        if early_error {
            let result=(move || -> Result<(),String> {
                let mut file=guarded;bounded_read(&mut file,0)?;Ok(())
            })();
            assert!(result.is_err());
        } else {drop(guarded);}
        let observed=if exclusive {
            read_state_in(dir.path(),"s1").is_some()
        } else {
            let request:HookInput=serde_json::from_value(json!({"session_id":"s1","hook_event_name":"PermissionRequest","tool_name":"Read"})).unwrap();
            record(&request,dir.path(),2).is_ok()
        };
        drop(child); // Clean up/join even when observing the old lifetime defect.
        assert!(read_state_in(dir.path(),"s1").is_some());
        assert!(observed,"completed scope left an inherited {} lock held (early_error={early_error})",if exclusive {"writer"} else {"reader"});
        if !exclusive {assert_eq!(read_state_in(dir.path(),"s1").unwrap().pending.len(),1);}
    }
    #[test] fn inherited_writer_lock_releases_on_success(){check(true,false);}
    #[test] fn inherited_writer_lock_releases_on_early_error(){check(true,true);}
    #[test] fn inherited_reader_lock_releases_on_success(){check(false,false);}
    #[test] fn inherited_reader_lock_releases_on_early_error(){check(false,true);}
}

#[cfg(all(test, unix))]
mod empty_result_regressions_v5 {
    use super::*;
    fn fixture() -> tempfile::TempDir {
        tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap()
    }
    fn call(id: &str, command: &str) -> Value {
        json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":id,"name":"Bash","input":{"command":command}}]}})
    }
    fn result(role: &str, block: Value) -> Value {
        json!({"type":role,"message":{"content":[block]}})
    }
    fn bound(id: &str) -> PendingPermission {
        PendingPermission {agent_id:None,tool_name:"Bash".into(),requested_at:1,tool_use_id:Some(id.into())}
    }
    fn transcript(path: &Path, records: &[Value]) {
        fs::write(path,records.iter().map(|value| format!("{value}\n")).collect::<String>()).unwrap();
    }
    #[test]
    fn valid_empty_tool_result_resolves_exact_bound_wait() {
        let dir=fixture();let path=dir.path().join("s1.jsonl");
        for block in [json!({"type":"tool_result","tool_use_id":"same"}),
            json!({"type":"tool_result","tool_use_id":"same","content":""}),
            json!({"type":"tool_result","tool_use_id":"same","content":[]}),
            json!({"type":"tool_result","tool_use_id":"same","is_error":false}),
            json!({"type":"tool_result","tool_use_id":"same","is_error":true})] {
            transcript(&path,&[call("same","make"),result("user",block)]);
            assert!(!prompt_is_open(&bound("same"),&path),"valid empty result left exact bound wait open");
            assert!(prompt_input(&bound("same"),&path).is_none(),"completed call must no longer supply pending arguments");
        }
    }
    #[test]
    fn omitted_content_resolves_only_exact_wait_and_preserves_other_arguments() {
        let dir=fixture();let path=dir.path().join("s1.jsonl");
        transcript(&path,&[call("same","completed"),call("other","still-pending"),
            result("user",json!({"type":"tool_result","tool_use_id":"same"}))]);
        assert!(!prompt_is_open(&bound("same"),&path));
        assert!(prompt_is_open(&bound("other"),&path));
        assert_eq!(prompt_input(&bound("other"),&path),Some(json!({"command":"still-pending"})),"valid associated arguments stay exposed");
    }
    #[test]
    fn omitted_content_wrong_role_cannot_resolve_bound_wait() {
        let dir=fixture();let path=dir.path().join("s1.jsonl");
        for role in ["assistant","system","unsupported"] {
            transcript(&path,&[call("same","make"),result(role,json!({"type":"tool_result","tool_use_id":"same"}))]);
            assert!(prompt_is_open(&bound("same"),&path),"wrong role resolved bound wait");
        }
    }
    #[test]
    fn omitted_content_ambiguous_call_identity_keeps_wait_open() {
        let dir=fixture();let path=dir.path().join("s1.jsonl");
        transcript(&path,&[call("same","first"),call("same","conflicting"),
            result("user",json!({"type":"tool_result","tool_use_id":"same"}))]);
        assert!(prompt_is_open(&bound("same"),&path));
        assert!(prompt_input(&bound("same"),&path).is_none(),"ambiguous identity must not expose guessed arguments");
    }
    #[test]
    fn invalid_provided_result_content_cannot_prove_resolution() {
        let dir=fixture();let path=dir.path().join("s1.jsonl");
        for content in [Value::Null,json!(7),json!(false),json!({"invalid":"shape"})] {
            transcript(&path,&[call("same","make"),result("user",json!({"type":"tool_result","tool_use_id":"same","content":content}))]);
            assert!(prompt_is_open(&bound("same"),&path),"invalid provided content resolved bound wait");
        }
    }
    #[test]
    fn invalid_result_identity_or_error_flag_cannot_resolve_bound_wait() {
        let dir=fixture();let path=dir.path().join("s1.jsonl");
        for block in [json!({"type":"tool_result"}),json!({"type":"tool_result","tool_use_id":""}),
            json!({"type":"tool_result","tool_use_id":7}),json!({"type":"tool_result","tool_use_id":null}),
            json!({"type":"tool_result","tool_use_id":"same","is_error":"false"}),
            json!({"type":"tool_result","tool_use_id":"same","is_error":null})] {
            transcript(&path,&[call("same","make"),result("user",block)]);
            assert!(prompt_is_open(&bound("same"),&path),"invalid supplied field resolved bound wait");
        }
    }
}
