use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System, UpdateKind};

use super::pid_check::{pid_was_reused, RecordedStart};
use super::source::{DetectedSession, DetectionDiagnostics, SessionDetectorError, SessionSource};

/// Session detector that finds running Claude processes and matches them to session files
pub struct LegacySessionSource {
    system: System,
    claude_projects_dir: PathBuf,
    claude_sessions_dir: PathBuf,
}

impl LegacySessionSource {
    /// Creates a new LegacySessionSource
    pub fn new() -> Result<Self, SessionDetectorError> {
        let home_dir = dirs::home_dir().ok_or(SessionDetectorError::HomeDirectoryNotFound)?;

        let claude_projects_dir = home_dir.join(".claude").join("projects");
        let claude_sessions_dir = home_dir.join(".claude").join("sessions");

        Ok(Self {
            system: System::new_with_specifics(
                RefreshKind::new().with_processes(
                    ProcessRefreshKind::new()
                        .with_exe(UpdateKind::OnlyIfNotSet)
                        .with_cmd(UpdateKind::OnlyIfNotSet)
                        .with_cwd(UpdateKind::OnlyIfNotSet),
                ),
            ),
            claude_projects_dir,
            claude_sessions_dir,
        })
    }

    /// Detects all active Claude Code sessions
    pub fn detect_sessions(
        &mut self,
    ) -> Result<(Vec<DetectedSession>, DetectionDiagnostics), SessionDetectorError> {
        // Refresh process information (only what we need: name, cwd, start_time)
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::new()
                .with_exe(UpdateKind::OnlyIfNotSet)
                .with_cmd(UpdateKind::OnlyIfNotSet)
                .with_cwd(UpdateKind::OnlyIfNotSet),
        );

        // Find all running Claude processes
        let claude_processes = self.find_claude_processes();

        let total = claude_processes.len() as u32;
        let with_cwd = claude_processes.iter().filter(|p| p.cwd.is_some()).count() as u32;
        let diagnostics = DetectionDiagnostics {
            claude_processes_found: total,
            processes_with_cwd: with_cwd,
            fda_likely_needed: total > 0 && with_cwd == 0,
            ..DetectionDiagnostics::default()
        };

        // If no Claude processes are running, return empty
        if claude_processes.is_empty() {
            return Ok((Vec::new(), diagnostics));
        }

        // Get all session project directories
        let project_dirs = self.enumerate_project_directories()?;

        // Find recently active sessions (modified in last 30 minutes)
        // and associate them with running processes
        let sessions = self.find_active_sessions(&claude_processes, &project_dirs);

        Ok((sessions, diagnostics))
    }

    /// Find sessions that are likely active based on running process count
    fn find_active_sessions(
        &self,
        processes: &[ClaudeProcess],
        project_dirs: &[PathBuf],
    ) -> Vec<DetectedSession> {
        // Collect all session files with their modification times and project path
        // Tuple: (modified_time, jsonl_path, project_dir, project_path, project_name, has_reliable_path)
        let mut session_files: Vec<(
            std::time::SystemTime,
            PathBuf,
            PathBuf,
            PathBuf,
            String,
            bool,
        )> = Vec::new();

        for project_dir in project_dirs {
            if let Ok(entries) = fs::read_dir(project_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();

                    // Check if it's a JSONL file (UUID format, not subagent files)
                    if path.is_file() && path.extension().is_some_and(|ext| ext == "jsonl") {
                        // Skip files that don't look like UUIDs (e.g., agent-*.jsonl)
                        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                            if stem.starts_with("agent-") {
                                continue;
                            }
                        }

                        if let Ok(metadata) = fs::metadata(&path) {
                            if let Ok(modified) = metadata.modified() {
                                // Get session ID and project info
                                if let Some(session_id) = path
                                    .file_stem()
                                    .and_then(|s| s.to_str())
                                    .map(|s| s.to_string())
                                {
                                    // Try to get project info from sessions-index.json
                                    // This is the ONLY reliable source of project path
                                    let (project_path, project_name, has_reliable_path) = match self
                                        .get_project_info_from_index(project_dir, &session_id)
                                    {
                                        Some((path, name)) => (path, name, true),
                                        None => {
                                            // No reliable path available - use directory name as display only
                                            // Don't try to decode it (decoding is ambiguous due to dashes)
                                            let dir_name = project_dir
                                                .file_name()
                                                .and_then(|n| n.to_str())
                                                .unwrap_or("unknown");

                                            // Just use the last segment after splitting on dash as a rough name
                                            // This is for display only, not for matching
                                            let name = dir_name
                                                .rsplit('-')
                                                .next()
                                                .unwrap_or("unknown")
                                                .to_string();

                                            // Use the project_dir as a placeholder (will use fallback PID assignment)
                                            (project_dir.clone(), name, false)
                                        }
                                    };

                                    session_files.push((
                                        modified,
                                        path,
                                        project_dir.clone(),
                                        project_path,
                                        project_name,
                                        has_reliable_path,
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }

        // Sort by modification time (most recent first)
        session_files.sort_by(|a, b| b.0.cmp(&a.0));

        // Process-centric approach: for each process, find its matching session
        // This ensures we only show sessions that have actual running processes
        let mut sessions = Vec::new();
        let mut used_session_ids: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        // Sessions some process names explicitly: a registry row in
        // ~/.claude/sessions/ or a `--resume <id>` argument. Only that process
        // may claim them. A hard-killed process leaves its row behind, so its
        // session stays named here and the cwd fallback below cannot hand it
        // to another live process in the same directory (issue #131).
        let mut named_session_ids = self.registry_session_ids();
        named_session_ids.extend(
            processes
                .iter()
                .filter_map(|proc| proc.resumed_session_id.clone()),
        );

        // Sort processes by start_time (newest first) to match newest processes first
        let mut sorted_processes: Vec<&ClaudeProcess> = processes.iter().collect();
        sorted_processes.sort_by(|a, b| b.start_time.cmp(&a.start_time));

        for proc in sorted_processes {
            let proc_cwd = match &proc.cwd {
                Some(cwd) => cwd,
                None => continue, // Skip processes without cwd
            };

            // Primary: the session this process names. ~/.claude/sessions/<pid>.json
            // is authoritative and is updated after /clear; `--resume <id>`
            // covers a process without a registry row.
            // A registry row whose recorded start is older than this process
            // was left by a hard-killed process whose pid was reused; it
            // names that dead session, not this one's.
            let named = self
                .read_session_metadata(proc.pid)
                .filter(|meta| !pid_was_reused(&meta.recorded_start(), Some(proc.start_time)))
                .map(|meta| meta.session_id)
                .into_iter()
                .chain(proc.resumed_session_id.clone());
            let mut claimed_named = false;
            for session_id in named {
                if used_session_ids.contains(&session_id) {
                    continue;
                }
                // Find the matching session file and project info
                if let Some((_, _, project_dir, _, project_name, _)) =
                    session_files.iter().find(|(_, path, _, _, _, _)| {
                        path.file_stem().and_then(|s| s.to_str()) == Some(session_id.as_str())
                    })
                {
                    used_session_ids.insert(session_id.clone());
                    sessions.push(DetectedSession::with_legacy_defaults(
                        proc.pid,
                        proc_cwd.clone(),
                        project_dir.clone(),
                        Some(session_id),
                        project_name.clone(),
                    ));
                    claimed_named = true;
                    break;
                }
            }
            if claimed_named {
                continue;
            }

            // Fallback: heuristic matching by encoded CWD + modification time.
            // Used when pid.json doesn't exist (e.g., older Claude Code versions).
            let cwd_str = proc_cwd.to_string_lossy();
            let encoded_cwd = encode_path_for_matching(&cwd_str);

            // Helper closure to check if a session matches the process path
            let path_matches =
                |project_dir: &Path, project_path: &Path, has_reliable_path: bool| -> bool {
                    let dir_name = project_dir
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("");

                    // Method 1: Direct path comparison (exact or subdirectory match)
                    let direct_match = if has_reliable_path {
                        proc_cwd == project_path || proc_cwd.starts_with(project_path)
                    } else {
                        false
                    };

                    // Method 2: Encoded path comparison
                    let encoded_match = dir_name == encoded_cwd;

                    direct_match || encoded_match
                };

            // Helper closure to check if session is not already used or named
            // by another process
            let session_available = |path: &Path| -> bool {
                match path.file_stem().and_then(|s| s.to_str()) {
                    Some(id) => !used_session_ids.contains(id) && !named_session_ids.contains(id),
                    None => false,
                }
            };

            // Find session with activity after process start
            // Only match sessions that were modified AFTER the process started
            // This prevents matching a new Claude instance (with no session file yet)
            // to an older session from the same project directory
            let matching_session = session_files.iter().find(
                |(modified, path, project_dir, project_path, _, has_reliable_path)| {
                    if !session_available(path) {
                        return false;
                    }

                    // Check if the session was modified after the process started
                    let session_active_after_proc_start =
                        match modified.duration_since(std::time::UNIX_EPOCH) {
                            Ok(duration) => {
                                let session_modified_secs = duration.as_secs();
                                // Session must have been modified at or after process start (with 5s buffer)
                                session_modified_secs + 5 >= proc.start_time
                            }
                            Err(_) => false,
                        };

                    session_active_after_proc_start
                        && path_matches(project_dir, project_path, *has_reliable_path)
                },
            );

            if let Some((_, path, project_dir, _, project_name, _)) = matching_session {
                if let Some(session_id) = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
                {
                    used_session_ids.insert(session_id.clone());

                    sessions.push(DetectedSession::with_legacy_defaults(
                        proc.pid,
                        proc_cwd.clone(),
                        project_dir.clone(),
                        Some(session_id),
                        project_name.clone(),
                    ));
                }
            } else {
                crate::debug_log::log_warn(&format!(
                    "PID={}: no matching session found for cwd={}",
                    proc.pid,
                    proc_cwd.display()
                ));
            }
        }

        sessions
    }

    /// Session ids named by any registry row in ~/.claude/sessions/, live or not.
    fn registry_session_ids(&self) -> std::collections::HashSet<String> {
        let Ok(entries) = fs::read_dir(&self.claude_sessions_dir) else {
            return std::collections::HashSet::new();
        };
        entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .filter_map(|path| fs::read_to_string(path).ok())
            .filter_map(|raw| serde_json::from_str::<PidSessionMeta>(&raw).ok())
            .map(|meta| meta.session_id)
            .collect()
    }

    /// Reads session metadata from ~/.claude/sessions/<pid>.json if it exists.
    fn read_session_metadata(&self, pid: u32) -> Option<PidSessionMeta> {
        let meta_path = self.claude_sessions_dir.join(format!("{}.json", pid));
        let content = fs::read_to_string(&meta_path).ok()?;
        serde_json::from_str::<PidSessionMeta>(&content).ok()
    }

    /// Get project info from sessions-index.json for a given session ID
    fn get_project_info_from_index(
        &self,
        project_dir: &Path,
        session_id: &str,
    ) -> Option<(PathBuf, String)> {
        let index_path = project_dir.join("sessions-index.json");

        if let Ok(content) = fs::read_to_string(&index_path) {
            if let Ok(index) = serde_json::from_str::<SessionsIndex>(&content) {
                if let Some(entries) = &index.entries {
                    for entry in entries {
                        if entry.session_id == session_id {
                            if let Some(proj_path) = &entry.project_path {
                                let path = PathBuf::from(proj_path);
                                let name = path
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("unknown")
                                    .to_string();
                                return Some((path, name));
                            }
                        }
                    }

                    // If session not found in index, use first entry's project path as fallback
                    if let Some(first) = entries.first() {
                        if let Some(proj_path) = &first.project_path {
                            let path = PathBuf::from(proj_path);
                            let name = path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("unknown")
                                .to_string();
                            return Some((path, name));
                        }
                    }
                }
            }
        }

        None
    }

    /// Finds all processes with name "claude" or whose command-line args contain "claude".
    /// On macOS, npm-installed Claude Code runs as "node" so we also check process.cmd().
    fn find_claude_processes(&self) -> Vec<ClaudeProcess> {
        let mut processes = Vec::new();

        for (pid, process) in self.system.processes() {
            let name = process.name().to_string_lossy();

            // Skip our own process
            if name.contains("c9watch") {
                continue;
            }

            let cmd: Vec<String> = process
                .cmd()
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();

            if is_claude_code_process(&name, process.exe(), &cmd) {
                let cwd = process.cwd().map(|p| p.to_path_buf());
                let start_time = process.start_time();

                processes.push(ClaudeProcess {
                    pid: pid.as_u32(),
                    cwd,
                    start_time,
                    resumed_session_id: resumed_session_id(&cmd),
                });
            }
        }

        processes
    }

    /// Enumerates all project directories in ~/.claude/projects/
    fn enumerate_project_directories(&self) -> Result<Vec<PathBuf>, SessionDetectorError> {
        let mut project_dirs = Vec::new();

        // Check if the claude projects directory exists
        if !self.claude_projects_dir.exists() {
            return Ok(project_dirs);
        }

        // Read all entries in the projects directory
        let entries = fs::read_dir(&self.claude_projects_dir)?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();

            // Only include directories
            if path.is_dir() {
                project_dirs.push(path);
            }
        }

        Ok(project_dirs)
    }
}

impl Default for LegacySessionSource {
    fn default() -> Self {
        Self::new().expect("Failed to create LegacySessionSource")
    }
}

impl SessionSource for LegacySessionSource {
    fn detect(
        &mut self,
    ) -> Result<(Vec<DetectedSession>, DetectionDiagnostics), SessionDetectorError> {
        self.detect_sessions()
    }
    fn backend_name(&self) -> &'static str {
        "legacy"
    }
}

/// Encodes a path the same way Claude Code does for its project directory names:
/// every non-alphanumeric character is replaced with a dash.
/// Whether a process is a Claude Code CLI: the `claude` executable itself
/// (native installer, Homebrew or an npm shim), or a Node/Bun runtime executing
/// the Claude Code package.
///
/// Matches whole program names rather than any `claude` substring in the
/// command line: Claude Desktop's helper processes carry arguments like
/// `--standard-schemes=claude-media`, and each one would otherwise be paired
/// with an unrelated recent transcript.
fn is_claude_code_process(name: &str, exe: Option<&Path>, cmd: &[String]) -> bool {
    let is_claude_name = |n: &str| n == "claude" || n == "claude.exe";
    let base = |s: &str| {
        Path::new(s)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default()
    };

    let argv0 = cmd.first().map(|a| base(a)).unwrap_or_default();
    if is_claude_name(name) || is_claude_name(&argv0) {
        return true;
    }
    if let Some(exe) = exe {
        let exe_name = exe.file_name().unwrap_or_default().to_string_lossy();
        if is_claude_name(&exe_name) {
            return true;
        }
        // Native installer: `~/.local/bin/claude` symlinks to
        // `~/.local/share/claude/versions/<version>`, so the resolved exe is
        // named after the version.
        if dirs::home_dir().is_some_and(|home| is_native_claude_executable(exe, &home)) {
            return true;
        }
    }

    // The package entry script identifies Claude Code even when Node is named
    // nodejs, versioned, or explicitly renamed.
    if cmd
        .iter()
        .skip(1)
        .any(|arg| Path::new(&arg.replace('\\', "/")).ends_with("@anthropic-ai/claude-code/cli.js"))
    {
        return true;
    }

    let is_runtime = ["node", "node.exe", "bun", "bun.exe"]
        .iter()
        .any(|r| name == *r || argv0 == *r);
    is_runtime
        && cmd.iter().skip(1).any(|arg| {
            is_claude_name(&base(arg))
                || arg.replace('\\', "/").contains("@anthropic-ai/claude-code")
        })
}

fn is_native_claude_executable(exe: &Path, home: &Path) -> bool {
    if exe.parent() != Some(home.join(".local/share/claude/versions").as_path()) {
        return false;
    }
    let Some(version) = exe.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let mut parts = version.split('.');
    parts.clone().count() == 3
        && parts.all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

/// The session id passed as `--resume <id>`, `--resume=<id>` or `-r <id>`.
/// A bare `--resume` opens a picker and names no session.
fn resumed_session_id(cmd: &[String]) -> Option<String> {
    let mut args = cmd.iter().skip(1);
    while let Some(arg) = args.next() {
        let value = match arg.as_str() {
            "--resume" | "-r" => args.next().map(String::as_str),
            other => other.strip_prefix("--resume="),
        };
        if let Some(id) = value.filter(|id| looks_like_session_id(id)) {
            return Some(id.to_string());
        }
    }
    None
}

fn looks_like_session_id(value: &str) -> bool {
    value.len() == 36 && value.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

pub(crate) fn encode_path_for_matching(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Internal representation of a Claude process
#[derive(Debug, Clone)]
struct ClaudeProcess {
    pid: u32,
    cwd: Option<PathBuf>,
    start_time: u64, // Process start time (seconds since epoch)
    /// Session named on the command line by `--resume <id>`.
    resumed_session_id: Option<String>,
}

/// Session metadata from ~/.claude/sessions/<pid>.json
/// Written by Claude Code with the authoritative session ID for each process.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PidSessionMeta {
    session_id: String,
    #[serde(default)]
    proc_start: Option<String>,
    #[serde(default)]
    started_at: Option<i64>,
}

impl PidSessionMeta {
    fn recorded_start(&self) -> RecordedStart {
        RecordedStart::from_record(&serde_json::json!({
            "procStart": self.proc_start,
            "startedAt": self.started_at,
        }))
    }
}

/// Structure of sessions-index.json
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionsIndex {
    #[allow(dead_code)]
    version: Option<u32>,
    entries: Option<Vec<SessionEntry>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionEntry {
    session_id: String,
    project_path: Option<String>,
    #[allow(dead_code)]
    full_path: Option<String>,
    #[allow(dead_code)]
    first_prompt: Option<String>,
    #[allow(dead_code)]
    summary: Option<String>,
    #[allow(dead_code)]
    message_count: Option<u32>,
    #[allow(dead_code)]
    git_branch: Option<String>,
    #[allow(dead_code)]
    modified: Option<String>,
}

/// Test doubles shared with `accuracy_matrix`.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// Runs the legacy matcher over `claude_dir/{projects,sessions}` with a
    /// fake process table of `(pid, cwd, start_time_secs)` rows.
    pub(crate) fn detect_with_processes(
        claude_dir: &Path,
        processes: &[(u32, PathBuf, u64)],
    ) -> Vec<DetectedSession> {
        let source = LegacySessionSource {
            system: System::new(),
            claude_projects_dir: claude_dir.join("projects"),
            claude_sessions_dir: claude_dir.join("sessions"),
        };
        let processes: Vec<ClaudeProcess> = processes
            .iter()
            .map(|(pid, cwd, start_time)| ClaudeProcess {
                pid: *pid,
                cwd: Some(cwd.clone()),
                start_time: *start_time,
                resumed_session_id: None,
            })
            .collect();
        let dirs = source.enumerate_project_directories().unwrap();
        source.find_active_sessions(&processes, &dirs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detector_creation() {
        let result = LegacySessionSource::new();
        assert!(result.is_ok());
    }

    #[test]
    fn test_find_claude_processes() {
        let detector = LegacySessionSource::new().unwrap();
        let processes = detector.find_claude_processes();
        // This test will vary based on whether claude is running
        println!("Found {} claude processes", processes.len());
    }

    fn cmd(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn matches_native_install_by_name_argv0_or_versioned_exe() {
        assert!(is_claude_code_process("claude", None, &cmd(&["claude"])));
        assert!(is_claude_code_process(
            "2.1.283",
            Some(Path::new("/Users/u/.local/share/claude/versions/2.1.283")),
            &cmd(&["/Users/u/.local/bin/claude", "-p"])
        ));
        assert!(is_native_claude_executable(
            Path::new("/Users/u/.local/share/claude/versions/2.1.283"),
            Path::new("/Users/u")
        ));
        assert!(is_claude_code_process("claude.exe", None, &cmd(&[])));
    }

    #[test]
    fn matches_npm_install_run_by_node() {
        assert!(is_claude_code_process(
            "node",
            Some(Path::new("/opt/homebrew/bin/node")),
            &cmd(&["node", "/opt/homebrew/bin/claude", "--resume"])
        ));
        assert!(is_claude_code_process(
            "node",
            None,
            &cmd(&[
                "node",
                "/usr/lib/node_modules/@anthropic-ai/claude-code/cli.js"
            ])
        ));
    }

    #[test]
    fn matches_claude_code_entry_script_with_alternative_runtime_names() {
        for (name, exe) in [
            ("nodejs", "/usr/bin/nodejs"),
            ("node-22", "/opt/node/bin/node-22"),
            ("renamed-runtime", "/opt/custom/renamed-runtime"),
        ] {
            assert!(
                is_claude_code_process(
                    name,
                    Some(Path::new(exe)),
                    &cmd(&[
                        name,
                        "/usr/lib/node_modules/@anthropic-ai/claude-code/cli.js"
                    ])
                ),
                "runtime {name}"
            );
        }
    }

    #[test]
    fn rejects_unrelated_executable_in_claude_versions_directory() {
        assert!(!is_claude_code_process(
            "server",
            Some(Path::new("/tmp/claude/versions/server")),
            &cmd(&[])
        ));
    }

    #[test]
    fn native_version_fallback_requires_installer_location_and_version_filename() {
        let home = Path::new("/fixture/home");
        assert!(is_native_claude_executable(
            &home.join(".local/share/claude/versions/2.1.283"),
            home
        ));
        for exe in [
            "/tmp/claude/versions/2.1.283",
            "/other/home/.local/share/claude/versions/2.1.283",
            "/fixture/home/.local/share/claude/versions/server",
            "/fixture/home/.local/share/claude/versions/2..283",
        ] {
            assert!(!is_native_claude_executable(Path::new(exe), home), "{exe}");
        }
    }

    #[test]
    fn rejects_claude_desktop_helpers_and_other_claude_substrings() {
        assert!(!is_claude_code_process(
            "Claude Helper",
            Some(Path::new(
                "/Applications/Claude.app/Contents/Frameworks/Claude Helper.app/Contents/MacOS/Claude Helper"
            )),
            &cmd(&[
                "/Applications/Claude.app/Contents/Frameworks/Claude Helper.app/Contents/MacOS/Claude Helper",
                "--type=utility",
                "--standard-schemes=cowork-artifact,claude-media",
            ])
        ));
        assert!(!is_claude_code_process(
            "Claude",
            None,
            &cmd(&["/Applications/Claude.app/Contents/MacOS/Claude"])
        ));
        assert!(!is_claude_code_process("ClaudeBar", None, &cmd(&[])));
        assert!(!is_claude_code_process(
            "bash",
            None,
            &cmd(&["bash", "/Users/u/.claude/scripts/statusline-command.sh"])
        ));
        assert!(!is_claude_code_process(
            "node",
            None,
            &cmd(&["node", "/Users/u/claude-tools/server.js"])
        ));
    }

    const KILLED: &str = "11111111-1111-1111-1111-111111111111";
    const UNOWNED: &str = "22222222-2222-2222-2222-222222222222";

    /// A legacy source rooted at a tempdir with one project dir for `cwd`.
    fn legacy_fixture(
        cwd: &Path,
        transcripts: &[&str],
    ) -> (tempfile::TempDir, LegacySessionSource, Vec<PathBuf>) {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp
            .path()
            .join("projects")
            .join(encode_path_for_matching(&cwd.to_string_lossy()));
        fs::create_dir_all(&project_dir).unwrap();
        fs::create_dir_all(tmp.path().join("sessions")).unwrap();
        for id in transcripts {
            fs::write(
                project_dir.join(format!("{id}.jsonl")),
                "{\"type\":\"user\"}\n",
            )
            .unwrap();
        }
        let source = LegacySessionSource {
            system: System::new(),
            claude_projects_dir: tmp.path().join("projects"),
            claude_sessions_dir: tmp.path().join("sessions"),
        };
        (tmp, source, vec![project_dir])
    }

    fn write_registry_row(tmp: &Path, pid: u32, session_id: &str) {
        fs::write(
            tmp.join("sessions").join(format!("{pid}.json")),
            format!("{{\"pid\":{pid},\"sessionId\":\"{session_id}\",\"entrypoint\":\"sdk-cli\"}}"),
        )
        .unwrap();
    }

    fn process(pid: u32, cwd: &Path, started_secs_ago: u64) -> ClaudeProcess {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        ClaudeProcess {
            pid,
            cwd: Some(cwd.to_path_buf()),
            start_time: now - started_secs_ago,
            resumed_session_id: None,
        }
    }

    fn claimed(sessions: &[DetectedSession]) -> Vec<(u32, String)> {
        sessions
            .iter()
            .map(|s| (s.pid, s.session_id.clone().unwrap_or_default()))
            .collect()
    }

    /// Issue #131: a hard-killed `claude -p` leaves its registry row
    /// (`~/.claude/sessions/<pid>.json`) and transcript behind. Another live
    /// Claude process in the same cwd without its own registry row must not
    /// adopt that transcript through the cwd + mtime fallback, or the dead
    /// session is shown as live for as long as the claimant runs.
    #[test]
    fn cwd_fallback_does_not_adopt_a_session_owned_by_a_dead_pid() {
        let cwd = Path::new("/fixture/wma");
        let (tmp, source, dirs) = legacy_fixture(cwd, &[KILLED]);
        write_registry_row(tmp.path(), 999_999, KILLED);
        let live_without_row = process(4242, cwd, 3600);

        let sessions = source.find_active_sessions(&[live_without_row], &dirs);

        assert_eq!(claimed(&sessions), vec![]);
    }

    /// The owner of a registry row keeps its session even when a newer
    /// process without a row is matched first.
    #[test]
    fn cwd_fallback_leaves_a_registry_owned_session_to_its_owner() {
        let cwd = Path::new("/fixture/wma");
        let (tmp, source, dirs) = legacy_fixture(cwd, &[KILLED]);
        write_registry_row(tmp.path(), 5000, KILLED);
        let owner = process(5000, cwd, 7200);
        let newer_without_row = process(6000, cwd, 3600);

        let sessions = source.find_active_sessions(&[owner, newer_without_row], &dirs);

        assert_eq!(claimed(&sessions), vec![(5000, KILLED.to_string())]);
    }

    /// Older Claude Code builds write no registry row; the cwd fallback still
    /// pairs such a process with an unowned transcript.
    #[test]
    fn cwd_fallback_still_claims_an_unowned_session() {
        let cwd = Path::new("/fixture/wma");
        let (tmp, source, dirs) = legacy_fixture(cwd, &[KILLED, UNOWNED]);
        write_registry_row(tmp.path(), 999_999, KILLED);
        let live_without_row = process(4242, cwd, 3600);

        let sessions = source.find_active_sessions(&[live_without_row], &dirs);

        assert_eq!(claimed(&sessions), vec![(4242, UNOWNED.to_string())]);
    }

    /// A process started as `claude --resume <id>` with no registry row
    /// writes to `<id>.jsonl`, wherever that lives. The cwd fallback must not
    /// pair it with an unrelated transcript in its cwd instead.
    #[test]
    fn resumed_process_claims_its_named_session_not_a_cwd_neighbour() {
        let cwd = Path::new("/fixture/home");
        let (_tmp, source, dirs) = legacy_fixture(cwd, &[UNOWNED, KILLED]);
        // The neighbour is the newest transcript written since the process
        // started, so it is the one the cwd heuristic would pick.
        let older = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
        fs::File::options()
            .write(true)
            .open(dirs[0].join(format!("{KILLED}.jsonl")))
            .unwrap()
            .set_modified(older)
            .unwrap();
        let mut resumed = process(4242, cwd, 3600);
        resumed.resumed_session_id = Some(KILLED.to_string());

        let sessions = source.find_active_sessions(&[resumed], &dirs);

        assert_eq!(claimed(&sessions), vec![(4242, KILLED.to_string())]);
    }

    /// Registry row for `pid` as current Claude Code writes it, for a process
    /// that started `started_secs_ago`.
    fn write_timed_registry_row(tmp: &Path, pid: u32, session_id: &str, started_secs_ago: i64) {
        let start = chrono::Utc::now() - chrono::Duration::seconds(started_secs_ago);
        fs::write(
            tmp.join("sessions").join(format!("{pid}.json")),
            serde_json::json!({
                "pid": pid,
                "sessionId": session_id,
                "procStart": start.format("%a %b %e %H:%M:%S %Y").to_string(),
                "startedAt": start.timestamp_millis() + 2000,
                "pidDomain": "darwin",
            })
            .to_string(),
        )
        .unwrap();
    }

    /// The pid of a hard-killed Claude Code was handed to a newer Claude
    /// process in the same cwd. Its stale registry row must not bind the
    /// dead session to the newcomer.
    #[test]
    fn reused_pid_does_not_inherit_the_dead_processes_session() {
        let cwd = Path::new("/fixture/wma");
        let (tmp, source, dirs) = legacy_fixture(cwd, &[KILLED]);
        write_timed_registry_row(tmp.path(), 5000, KILLED, 7200);
        let newcomer = process(5000, cwd, 60);

        let sessions = source.find_active_sessions(&[newcomer], &dirs);

        assert_eq!(claimed(&sessions), vec![]);
    }

    #[test]
    fn registry_row_with_a_matching_start_still_names_the_session() {
        let cwd = Path::new("/fixture/wma");
        let (tmp, source, dirs) = legacy_fixture(cwd, &[KILLED]);
        write_timed_registry_row(tmp.path(), 5000, KILLED, 7200);
        let owner = process(5000, cwd, 7200);

        let sessions = source.find_active_sessions(&[owner], &dirs);

        assert_eq!(claimed(&sessions), vec![(5000, KILLED.to_string())]);
    }

    #[test]
    fn resumed_session_id_reads_the_named_session_only() {
        let id = KILLED;
        assert_eq!(
            resumed_session_id(&cmd(&["claude", "--resume", id])).as_deref(),
            Some(id)
        );
        assert_eq!(
            resumed_session_id(&cmd(&["claude", &format!("--resume={id}")])).as_deref(),
            Some(id)
        );
        assert_eq!(
            resumed_session_id(&cmd(&["claude", "-r", id])).as_deref(),
            Some(id)
        );
        assert_eq!(resumed_session_id(&cmd(&["claude", "--resume"])), None);
        assert_eq!(
            resumed_session_id(&cmd(&["claude", "--resume", "--model", "x"])),
            None
        );
        assert_eq!(resumed_session_id(&cmd(&["claude", "-p", id])), None);
    }

    #[test]
    fn test_enumerate_project_directories() {
        let detector = LegacySessionSource::new().unwrap();
        let result = detector.enumerate_project_directories();
        assert!(result.is_ok());

        if let Ok(dirs) = result {
            println!("Found {} project directories", dirs.len());
        }
    }

    #[test]
    fn test_encode_path_for_matching() {
        // Must match Claude Code's encoding: replace every non-alphanumeric char with '-'.
        assert_eq!(
            encode_path_for_matching("/Users/Name/My_Project"),
            "-Users-Name-My-Project"
        );
        // Dots in paths (hidden dirs) become dashes
        assert_eq!(
            encode_path_for_matching("/Users/Name/.config/project"),
            "-Users-Name--config-project"
        );
        // Spaces become dashes
        assert_eq!(
            encode_path_for_matching("/Users/Name/My Project"),
            "-Users-Name-My-Project"
        );
        // Dots in project names
        assert_eq!(
            encode_path_for_matching("/Users/Name/project.v2"),
            "-Users-Name-project-v2"
        );
    }
}
