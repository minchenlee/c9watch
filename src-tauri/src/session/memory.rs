use super::source::SessionProvider;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

const CODEX_MEMORY_FILES: [&str; 2] = ["MEMORY.md", "memory_summary.md"];

/// A single memory file (e.g., MEMORY.md, profile.md)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryFile {
    pub filename: String,
    pub content: String,
}

/// All memory files for a single project
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectMemory {
    /// Provider that owns this memory group. Older payloads default to Claude Code.
    #[serde(default)]
    pub provider: SessionProvider,
    /// Human-readable project name (last path segment, e.g. "c9watch")
    pub project_name: String,
    /// Decoded full project path (e.g. "/Users/liminchen/Documents/GitHub/c9watch")
    pub project_path: String,
    /// Absolute path to the memory directory (for "Reveal in Finder")
    pub memory_dir_path: String,
    /// Memory files found in this project
    pub files: Vec<MemoryFile>,
}

/// Decode a Claude projects directory name back to a real path.
/// e.g. "-Users-liminchen-Documents-GitHub-c9watch" → "/Users/liminchen/Documents/GitHub/c9watch"
fn decode_project_dir(dir_name: &str) -> String {
    if dir_name.starts_with('-') {
        format!("/{}", dir_name[1..].replace('-', "/"))
    } else {
        dir_name.replace('-', "/")
    }
}

/// Scan supported Claude Code and Codex memory locations.
pub fn get_memory_files() -> Result<Vec<ProjectMemory>, String> {
    let home_dir = dirs::home_dir().ok_or("Failed to get home directory")?;
    get_memory_files_from_home(&home_dir)
}

fn get_memory_files_from_home(home_dir: &Path) -> Result<Vec<ProjectMemory>, String> {
    let mut results = get_claude_memory_files(home_dir)?;

    if let Some(codex_memory) = get_codex_memory_files(home_dir) {
        results.push(codex_memory);
    }

    // Sort projects alphabetically by name
    results.sort_by(|a, b| {
        a.project_name
            .to_lowercase()
            .cmp(&b.project_name.to_lowercase())
    });

    Ok(results)
}

/// Scan ~/.claude/projects/*/memory/*.md and return files grouped by project.
fn get_claude_memory_files(home_dir: &Path) -> Result<Vec<ProjectMemory>, String> {
    let projects_dir = home_dir.join(".claude").join("projects");

    if !projects_dir.exists() {
        return Ok(Vec::new());
    }

    let entries = fs::read_dir(&projects_dir)
        .map_err(|e| format!("Failed to read projects directory: {}", e))?;

    let mut results: Vec<ProjectMemory> = Vec::new();

    for entry in entries.flatten() {
        let project_dir = entry.path();
        if !project_dir.is_dir() {
            continue;
        }

        let memory_dir = project_dir.join("memory");
        if !memory_dir.is_dir() {
            continue;
        }

        let dir_name = project_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();

        let decoded_path = decode_project_dir(&dir_name);
        let project_name = decoded_path
            .rsplit('/')
            .find(|s| !s.is_empty())
            .unwrap_or(&dir_name)
            .to_string();

        let mut files: Vec<MemoryFile> = Vec::new();

        if let Ok(mem_entries) = fs::read_dir(&memory_dir) {
            for mem_entry in mem_entries.flatten() {
                let file_path = mem_entry.path();
                if file_path.extension().and_then(|e| e.to_str()) == Some("md") {
                    let filename = file_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("")
                        .to_string();

                    if let Ok(content) = fs::read_to_string(&file_path) {
                        files.push(MemoryFile { filename, content });
                    }
                }
            }
        }

        if !files.is_empty() {
            // Sort: MEMORY.md first, then alphabetical
            files.sort_by(|a, b| {
                let a_is_main = a.filename == "MEMORY.md";
                let b_is_main = b.filename == "MEMORY.md";
                b_is_main.cmp(&a_is_main).then(a.filename.cmp(&b.filename))
            });

            results.push(ProjectMemory {
                provider: SessionProvider::ClaudeCode,
                project_name,
                project_path: decoded_path,
                memory_dir_path: memory_dir.to_string_lossy().to_string(),
                files,
            });
        }
    }

    Ok(results)
}

/// Load only Codex's durable top-level memory documents. Rollout transcripts and
/// other nested or implementation-specific files are intentionally excluded.
fn get_codex_memory_files(home_dir: &Path) -> Option<ProjectMemory> {
    let memory_dir = home_dir.join(".codex").join("memories");
    if !memory_dir.is_dir() {
        return None;
    }

    let files = CODEX_MEMORY_FILES
        .iter()
        .filter_map(|filename| {
            let file_path = memory_dir.join(filename);
            fs::read_to_string(file_path)
                .ok()
                .map(|content| MemoryFile {
                    filename: (*filename).to_string(),
                    content,
                })
        })
        .collect::<Vec<_>>();

    if files.is_empty() {
        return None;
    }

    Some(ProjectMemory {
        provider: SessionProvider::Codex,
        project_name: "Codex memory".to_string(),
        project_path: memory_dir.to_string_lossy().to_string(),
        memory_dir_path: memory_dir.to_string_lossy().to_string(),
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn discovers_claude_and_durable_codex_memory_without_rollouts() {
        let home = tempfile::tempdir().unwrap();
        write_file(
            &home
                .path()
                .join(".claude/projects/-tmp-example/memory/MEMORY.md"),
            "claude memory",
        );
        write_file(
            &home.path().join(".codex/memories/MEMORY.md"),
            "codex registry",
        );
        write_file(
            &home.path().join(".codex/memories/memory_summary.md"),
            "codex summary",
        );
        write_file(
            &home
                .path()
                .join(".codex/memories/rollout_summaries/private.md"),
            "must not load",
        );
        write_file(
            &home.path().join(".codex/memories/raw_memories.md"),
            "implementation detail",
        );

        let groups = get_memory_files_from_home(home.path()).unwrap();

        assert_eq!(groups.len(), 2);
        let claude = groups
            .iter()
            .find(|group| group.provider == SessionProvider::ClaudeCode)
            .unwrap();
        assert_eq!(claude.project_name, "example");
        assert_eq!(claude.files.len(), 1);

        let codex = groups
            .iter()
            .find(|group| group.provider == SessionProvider::Codex)
            .unwrap();
        assert_eq!(codex.project_name, "Codex memory");
        assert_eq!(codex.files.len(), 2);
        assert_eq!(codex.files[0].filename, "MEMORY.md");
        assert_eq!(codex.files[1].filename, "memory_summary.md");
        assert!(codex
            .files
            .iter()
            .all(|file| file.filename != "raw_memories.md"));
    }

    #[test]
    fn discovers_codex_memory_without_a_claude_projects_directory() {
        let home = tempfile::tempdir().unwrap();
        write_file(
            &home.path().join(".codex/memories/MEMORY.md"),
            "codex registry",
        );

        let groups = get_memory_files_from_home(home.path()).unwrap();

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].provider, SessionProvider::Codex);
    }

    #[test]
    fn missing_provider_deserializes_as_claude_code() {
        let group: ProjectMemory = serde_json::from_value(serde_json::json!({
            "projectName": "legacy",
            "projectPath": "/tmp/legacy",
            "memoryDirPath": "/tmp/legacy/memory",
            "files": []
        }))
        .unwrap();

        assert_eq!(group.provider, SessionProvider::ClaudeCode);
    }
}
