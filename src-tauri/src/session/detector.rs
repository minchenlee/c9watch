use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System, UpdateKind};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum SessionDetectorError {
    #[error("Failed to read directory: {0}")]
    DirectoryRead(#[from] std::io::Error),

    #[error("Failed to get home directory")]
    HomeDirectoryNotFound,

    #[error("Failed to refresh process information")]
    ProcessRefreshError,
}

/// Information about a detected Claude Code session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedSession {
    /// Process ID of the running claude process
    pub pid: u32,

    /// Current working directory of the process
    pub cwd: PathBuf,

    /// Path to the session's project directory in ~/.claude/projects/
    pub project_path: PathBuf,

    /// Session ID (UUID from session file)
    pub session_id: Option<String>,

    /// Project name (derived from cwd)
    pub project_name: String,
}

/// Session detector that finds running Claude processes and matches them to session files
pub struct SessionDetector {
    system: System,
    claude_projects_dir: PathBuf,
}

impl SessionDetector {
    /// Creates a new SessionDetector
    pub fn new() -> Result<Self, SessionDetectorError> {
        let home_dir = dirs::home_dir().ok_or(SessionDetectorError::HomeDirectoryNotFound)?;

        let claude_projects_dir = home_dir.join(".claude").join("projects");

        Ok(Self {
            system: System::new_with_specifics(
                RefreshKind::new().with_processes(
                    ProcessRefreshKind::new()
                        .with_exe(UpdateKind::OnlyIfNotSet)
                        .with_cwd(UpdateKind::OnlyIfNotSet),
                ),
            ),
            claude_projects_dir,
        })
    }

    /// Detects all active Claude Code sessions
    pub fn detect_sessions(&mut self) -> Result<Vec<DetectedSession>, SessionDetectorError> {
        // Refresh process information (only what we need: name, cwd, start_time)
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::new()
                .with_exe(UpdateKind::OnlyIfNotSet)
                .with_cwd(UpdateKind::OnlyIfNotSet),
        );

        // Find all running Claude processes
        let claude_processes = self.find_claude_processes();

        // If no Claude processes are running, return empty
        if claude_processes.is_empty() {
            return Ok(Vec::new());
        }

        // Get all session project directories
        let project_dirs = self.enumerate_project_directories()?;

        // Find recently active sessions (modified in last 30 minutes)
        // and associate them with running processes
        let sessions = self.find_active_sessions(&claude_processes, &project_dirs);

        Ok(sessions)
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

        // Sort processes by start_time (newest first) to match newest processes first
        let mut sorted_processes: Vec<&ClaudeProcess> = processes.iter().collect();
        sorted_processes.sort_by(|a, b| b.start_time.cmp(&a.start_time));

        for proc in sorted_processes {
            let proc_cwd = match &proc.cwd {
                Some(cwd) => cwd,
                None => continue, // Skip processes without cwd
            };

            // Encode the process cwd for matching
            let cwd_str = proc_cwd.to_string_lossy();
            let encoded_cwd = cwd_str.replace(['/', '_', '.'], "-");

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

            // Helper closure to check if session is not already used
            let session_available = |path: &Path| -> bool {
                match path.file_stem().and_then(|s| s.to_str()) {
                    Some(id) => !used_session_ids.contains(id),
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

                    sessions.push(DetectedSession {
                        pid: proc.pid,
                        cwd: proc_cwd.clone(),
                        project_path: project_dir.clone(),
                        session_id: Some(session_id),
                        project_name: project_name.clone(),
                    });
                }
            }
        }

        sessions
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

    /// Finds all processes with name "claude"
    fn find_claude_processes(&self) -> Vec<ClaudeProcess> {
        let mut processes = Vec::new();

        for (pid, process) in self.system.processes() {
            // Check if the process name is "claude"
            let name = process.name().to_string_lossy();

            if name.contains("claude") && !name.contains("c9watch") {
                // Get the current working directory of the process
                let cwd = process.cwd().map(|p| p.to_path_buf());
                let start_time = process.start_time();

                processes.push(ClaudeProcess {
                    pid: pid.as_u32(),
                    cwd,
                    start_time,
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

impl Default for SessionDetector {
    fn default() -> Self {
        Self::new().expect("Failed to create SessionDetector")
    }
}

/// Internal representation of a Claude process
#[derive(Debug, Clone)]
struct ClaudeProcess {
    pid: u32,
    cwd: Option<PathBuf>,
    start_time: u64, // Process start time (seconds since epoch)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detector_creation() {
        let result = SessionDetector::new();
        assert!(result.is_ok());
    }

    #[test]
    fn test_find_claude_processes() {
        let detector = SessionDetector::new().unwrap();
        let processes = detector.find_claude_processes();
        // This test will vary based on whether claude is running
        println!("Found {} claude processes", processes.len());
    }

    #[test]
    fn test_enumerate_project_directories() {
        let detector = SessionDetector::new().unwrap();
        let result = detector.enumerate_project_directories();
        assert!(result.is_ok());

        if let Ok(dirs) = result {
            println!("Found {} project directories", dirs.len());
        }
    }
}
