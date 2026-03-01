use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

const MAX_AGE_SECS: u64 = 600; // 10 minutes

/// A tracked subagent from ~/.claude/active-subagents.json
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackedSubagent {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub agent_type: String,
    pub description: String,
    pub prompt: String,
    pub parent_session_id: String,
    pub parent_pid: u32,
    pub started_at: u64, // milliseconds since epoch
}

/// Read active subagents from the JSON file, filtering out expired entries
pub fn read_active_subagents() -> Vec<TrackedSubagent> {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return Vec::new(),
    };

    let file_path: PathBuf = home.join(".claude").join("active-subagents.json");

    if !file_path.exists() {
        return Vec::new();
    }

    let content = match fs::read_to_string(&file_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let subagents: Vec<TrackedSubagent> = match serde_json::from_str(&content) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    // Filter out expired entries
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    subagents
        .into_iter()
        .filter(|s| {
            let age_ms = now_ms.saturating_sub(s.started_at);
            age_ms < (MAX_AGE_SECS * 1000)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_active_subagents_no_file() {
        // Should return empty vec when file doesn't exist
        let result = read_active_subagents();
        // May or may not be empty depending on machine state
        println!("Found {} subagents", result.len());
    }

    #[test]
    fn test_parse_subagent_json() {
        let json = r#"[{
            "id": "sa-123",
            "name": "test-agent",
            "type": "general-purpose",
            "description": "Test",
            "prompt": "Do something",
            "parentSessionId": "abc",
            "parentPid": 1234,
            "startedAt": 1709312345678
        }]"#;

        let parsed: Vec<TrackedSubagent> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "test-agent");
        assert_eq!(parsed[0].agent_type, "general-purpose");
    }
}
