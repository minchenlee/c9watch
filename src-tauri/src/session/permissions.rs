use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Claude Code settings structure (partial - only what we need)
#[derive(Debug, Deserialize)]
pub struct ClaudeSettings {
    pub permissions: Option<Permissions>,
}

#[derive(Debug, Deserialize)]
pub struct Permissions {
    pub allow: Option<Vec<String>>,
}

/// Tools that never show a permission prompt, whatever the settings say.
const NEVER_PROMPTING_TOOLS: &[&str] = &[
    "Read",
    "Glob",
    "Grep",
    "WebFetch",
    "WebSearch",
    "Agent",
    "Task",
    "TaskList",
    "TaskGet",
    "TaskCreate",
    "TaskUpdate",
    "TaskOutput",
    "TaskStop",
    "TodoWrite",
    "Workflow",
    "Skill",
    "ToolSearch",
    "SendMessage",
    "ListAgents",
    "ScheduleWakeup",
    "AskUserQuestion",
];

/// How long a cached checker is trusted before its settings files are re-read.
const CACHE_TTL: Duration = Duration::from_secs(30);

/// Checkers keyed by project directory (`None` for user-level settings only).
type CheckerCache = HashMap<Option<PathBuf>, (Instant, Arc<PermissionChecker>)>;

static CHECKER_CACHE: LazyLock<Mutex<CheckerCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Cached permissions for quick lookup
#[derive(Debug, Clone, Default)]
pub struct PermissionChecker {
    allowed_patterns: Vec<AllowPattern>,
    /// Treat every pending tool as approved. Used for sessions whose permission
    /// prompts are reported by Claude Code hooks instead of inferred here.
    assume_approved: bool,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
enum AllowPattern {
    /// Bash command pattern, e.g., "git add" from "Bash(git add:*)"
    Bash { prefix: String, wildcard: bool },
    /// Full tool allow, e.g., "Read" means all Read operations are allowed
    Tool { name: String },
    /// MCP tool pattern
    Mcp { name: String },
    /// Skill pattern
    Skill { name: String },
}

impl PermissionChecker {
    /// Load permissions from the user-level settings files
    pub fn from_settings_file() -> Self {
        Self::from_files(&settings_files(None))
    }

    /// Load permissions from a specific file
    pub fn from_file(path: &Path) -> Self {
        Self::from_files(&[path.to_path_buf()])
    }

    /// Load and merge the allow rules from several settings files. Missing or
    /// unparseable files contribute nothing.
    pub fn from_files(paths: &[PathBuf]) -> Self {
        let allowed_patterns = paths
            .iter()
            .filter_map(|path| fs::read_to_string(path).ok())
            .filter_map(|content| serde_json::from_str::<ClaudeSettings>(&content).ok())
            .flat_map(|settings| {
                settings
                    .permissions
                    .and_then(|p| p.allow)
                    .unwrap_or_default()
            })
            .filter_map(|s| Self::parse_pattern(&s))
            .collect();

        Self {
            allowed_patterns,
            assume_approved: false,
        }
    }

    /// A checker that reports every tool as approved.
    pub fn assume_approved() -> Self {
        Self {
            allowed_patterns: Vec::new(),
            assume_approved: true,
        }
    }

    /// Checker for a session running in `project_dir` (user settings merged with
    /// that project's settings), cached for [`CACHE_TTL`].
    pub fn cached_for(project_dir: Option<&Path>) -> Arc<Self> {
        let key = project_dir.map(Path::to_path_buf);
        let Ok(mut cache) = CHECKER_CACHE.lock() else {
            return Arc::new(Self::from_files(&settings_files(project_dir)));
        };
        if let Some((loaded, checker)) = cache.get(&key) {
            if loaded.elapsed() < CACHE_TTL {
                return checker.clone();
            }
        }
        let checker = Arc::new(Self::from_files(&settings_files(project_dir)));
        cache.insert(key, (Instant::now(), checker.clone()));
        checker
    }

    /// Parse a permission pattern string into an AllowPattern
    fn parse_pattern(pattern: &str) -> Option<AllowPattern> {
        // Pattern formats:
        // - "Bash(command:*)" or "Bash(command)" - bash command
        // - "Read" - full tool access
        // - "mcp__server__tool" - MCP tool
        // - "Skill(name)" - skill

        if pattern.starts_with("Bash(") && pattern.ends_with(")") {
            // Extract the command pattern
            let inner = &pattern[5..pattern.len() - 1];

            // Check for wildcard
            if let Some(prefix) = inner.strip_suffix(":*") {
                let prefix = prefix.to_string();
                Some(AllowPattern::Bash {
                    prefix,
                    wildcard: true,
                })
            } else {
                Some(AllowPattern::Bash {
                    prefix: inner.to_string(),
                    wildcard: false,
                })
            }
        } else if pattern.starts_with("mcp__") {
            Some(AllowPattern::Mcp {
                name: pattern.to_string(),
            })
        } else if pattern.starts_with("Skill(") && pattern.ends_with(")") {
            let inner = &pattern[6..pattern.len() - 1];
            Some(AllowPattern::Skill {
                name: inner.to_string(),
            })
        } else if !pattern.contains('(') && !pattern.contains("__") {
            // Simple tool name like "Read", "Write", etc.
            Some(AllowPattern::Tool {
                name: pattern.to_string(),
            })
        } else {
            None
        }
    }

    /// Check if a tool use is auto-approved
    ///
    /// # Arguments
    /// * `tool_name` - The name of the tool (e.g., "Bash", "Read", "Glob")
    /// * `tool_input` - The tool input as a JSON value
    ///
    /// # Returns
    /// true if the tool is auto-approved, false if it needs user permission
    pub fn is_auto_approved(&self, tool_name: &str, tool_input: &serde_json::Value) -> bool {
        if self.assume_approved || NEVER_PROMPTING_TOOLS.contains(&tool_name) {
            return true;
        }

        // For Bash, check against allowed patterns
        if tool_name == "Bash" {
            let command = tool_input
                .get("command")
                .and_then(|c| c.as_str())
                .unwrap_or("");

            return self.is_tool_allowed("Bash") || self.is_bash_allowed(command);
        }

        // For Write/Edit, check if explicitly allowed
        if tool_name == "Write" || tool_name == "Edit" || tool_name == "NotebookEdit" {
            // These typically need permission unless explicitly allowed
            return self.is_tool_allowed(tool_name);
        }

        // For MCP tools, check pattern
        if tool_name.starts_with("mcp__") {
            return self.is_mcp_allowed(tool_name);
        }

        // Default: needs permission unless the tool is allowed by name
        self.is_tool_allowed(tool_name)
    }

    /// Check if a bash command matches any allowed pattern
    fn is_bash_allowed(&self, command: &str) -> bool {
        let command_trimmed = command.trim();

        for pattern in &self.allowed_patterns {
            if let AllowPattern::Bash { prefix, wildcard } = pattern {
                if *wildcard {
                    // Prefix match with wildcard
                    if command_trimmed.starts_with(prefix) {
                        return true;
                    }
                } else {
                    // Exact match
                    if command_trimmed == prefix {
                        return true;
                    }
                }
            }
        }

        false
    }

    /// Check if a tool is explicitly allowed
    fn is_tool_allowed(&self, tool_name: &str) -> bool {
        for pattern in &self.allowed_patterns {
            if let AllowPattern::Tool { name } = pattern {
                if name == tool_name {
                    return true;
                }
            }
        }
        false
    }

    /// Check if an MCP tool is allowed
    fn is_mcp_allowed(&self, tool_name: &str) -> bool {
        for pattern in &self.allowed_patterns {
            if let AllowPattern::Mcp { name } = pattern {
                if name == tool_name {
                    return true;
                }
            }
        }
        false
    }
}

/// Settings files whose `permissions.allow` rules apply to a session in
/// `project_dir`, in Claude Code's precedence order (all of them are merged).
fn settings_files(project_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(config_dir) = crate::claude_usage::config_dir() {
        files.push(config_dir.join("settings.json"));
        files.push(config_dir.join("settings.local.json"));
    }
    if let Some(dir) = project_dir {
        files.push(dir.join(".claude").join("settings.json"));
        files.push(dir.join(".claude").join("settings.local.json"));
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_bash_pattern_with_wildcard() {
        let pattern = PermissionChecker::parse_pattern("Bash(git add:*)");
        assert!(
            matches!(pattern, Some(AllowPattern::Bash { prefix, wildcard: true }) if prefix == "git add")
        );
    }

    #[test]
    fn test_parse_bash_pattern_exact() {
        let pattern = PermissionChecker::parse_pattern("Bash(npm ci)");
        assert!(
            matches!(pattern, Some(AllowPattern::Bash { prefix, wildcard: false }) if prefix == "npm ci")
        );
    }

    #[test]
    fn test_parse_mcp_pattern() {
        let pattern = PermissionChecker::parse_pattern("mcp__atlassian__getJiraIssue");
        assert!(
            matches!(pattern, Some(AllowPattern::Mcp { name }) if name == "mcp__atlassian__getJiraIssue")
        );
    }

    #[test]
    fn test_always_allowed_tools() {
        let checker = PermissionChecker::default();

        assert!(checker.is_auto_approved("Read", &serde_json::json!({})));
        assert!(checker.is_auto_approved("Glob", &serde_json::json!({})));
        assert!(checker.is_auto_approved("Grep", &serde_json::json!({})));
    }

    #[test]
    fn test_bash_command_matching() {
        let checker = PermissionChecker {
            assume_approved: false,
            allowed_patterns: vec![
                AllowPattern::Bash {
                    prefix: "git add".to_string(),
                    wildcard: true,
                },
                AllowPattern::Bash {
                    prefix: "npm ci".to_string(),
                    wildcard: false,
                },
            ],
        };

        // Should match git add with wildcard
        assert!(checker.is_auto_approved("Bash", &serde_json::json!({"command": "git add ."})));

        // Should match exact npm ci
        assert!(checker.is_auto_approved("Bash", &serde_json::json!({"command": "npm ci"})));

        // Should NOT match npm ci with arguments (exact match required)
        assert!(!checker.is_auto_approved(
            "Bash",
            &serde_json::json!({"command": "npm ci --legacy-peer-deps"})
        ));

        // Should NOT match random command
        assert!(!checker.is_auto_approved("Bash", &serde_json::json!({"command": "rm -rf /"})));
    }

    #[test]
    fn test_subagent_and_orchestration_tools_never_prompt() {
        let checker = PermissionChecker::default();
        for tool in [
            "Agent",
            "Task",
            "Workflow",
            "Skill",
            "ToolSearch",
            "SendMessage",
        ] {
            assert!(
                checker.is_auto_approved(tool, &serde_json::json!({})),
                "{tool}"
            );
        }
        assert!(!checker.is_auto_approved("ExitPlanMode", &serde_json::json!({})));
    }

    #[test]
    fn test_assume_approved_approves_everything() {
        let checker = PermissionChecker::assume_approved();
        assert!(checker.is_auto_approved("Bash", &serde_json::json!({"command": "rm -rf /"})));
        assert!(checker.is_auto_approved("Write", &serde_json::json!({})));
    }

    #[test]
    fn test_from_files_merges_allow_rules() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("settings.json");
        let local = dir.path().join("settings.local.json");
        std::fs::write(&user, r#"{"permissions":{"allow":["Bash(git status)"]}}"#).unwrap();
        std::fs::write(&local, r#"{"permissions":{"allow":["Write","mcp__x__y"]}}"#).unwrap();
        let checker =
            PermissionChecker::from_files(&[user, local, dir.path().join("missing.json")]);
        let bash = serde_json::json!({"command": "git status"});
        assert!(checker.is_auto_approved("Bash", &bash));
        assert!(checker.is_auto_approved("Write", &serde_json::json!({})));
        assert!(checker.is_auto_approved("mcp__x__y", &serde_json::json!({})));
        assert!(!checker.is_auto_approved("Edit", &serde_json::json!({})));
    }

    #[test]
    fn test_plain_tool_allow_applies_to_bash_and_other_tools() {
        let checker = PermissionChecker {
            assume_approved: false,
            allowed_patterns: vec![
                AllowPattern::Tool {
                    name: "Bash".to_string(),
                },
                AllowPattern::Tool {
                    name: "ExitPlanMode".to_string(),
                },
            ],
        };
        assert!(checker.is_auto_approved("Bash", &serde_json::json!({"command": "anything"})));
        assert!(checker.is_auto_approved("ExitPlanMode", &serde_json::json!({})));
    }

    #[test]
    fn test_load_from_real_settings() {
        // This test uses the real settings file if available
        let checker = PermissionChecker::from_settings_file();

        // Just verify it loads without crashing
        println!("Loaded {} patterns", checker.allowed_patterns.len());
    }
}
