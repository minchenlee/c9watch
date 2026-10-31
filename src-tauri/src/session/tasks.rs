//! TodoWrite tasks of a session: `~/.claude/tasks/<session id>/*.json`.
//! The GUI's `get_session_tasks`, the CLI and the engine's `session.tasks`
//! read them here.

pub fn read_session_tasks(session_id: &str) -> Result<Vec<serde_json::Value>, String> {
    let home_dir = dirs::home_dir().ok_or("Failed to get home directory")?;
    read_session_tasks_in(&home_dir, session_id)
}

pub(crate) fn read_session_tasks_in(
    home_dir: &std::path::Path,
    session_id: &str,
) -> Result<Vec<serde_json::Value>, String> {
    let tasks_dir = home_dir.join(".claude").join("tasks").join(session_id);

    if !tasks_dir.exists() {
        return Ok(vec![]);
    }

    let mut tasks: Vec<serde_json::Value> = Vec::new();

    for entry in std::fs::read_dir(&tasks_dir)
        .map_err(|e| format!("Failed to read tasks dir: {}", e))?
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(task) = serde_json::from_str::<serde_json::Value>(&content) {
                tasks.push(task);
            }
        }
    }

    tasks.sort_by(|a, b| {
        let id_a = a
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        let id_b = b
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        id_a.cmp(&id_b)
    });

    Ok(tasks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_task(home: &std::path::Path, session_id: &str, file: &str, body: &str) {
        let dir = home.join(".claude").join("tasks").join(session_id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(file), body).unwrap();
    }

    #[test]
    fn returns_empty_when_dir_missing() {
        let home = tempdir().unwrap();
        let got = read_session_tasks_in(home.path(), "no-such-session").unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn parses_and_sorts_by_numeric_id() {
        let home = tempdir().unwrap();
        let sid = "session-abc";
        write_task(
            home.path(),
            sid,
            "10.json",
            r#"{"id":"10","content":"ten","status":"pending"}"#,
        );
        write_task(
            home.path(),
            sid,
            "2.json",
            r#"{"id":"2","content":"two","status":"in_progress"}"#,
        );
        write_task(
            home.path(),
            sid,
            "1.json",
            r#"{"id":"1","content":"one","status":"completed"}"#,
        );

        let got = read_session_tasks_in(home.path(), sid).unwrap();
        let ids: Vec<&str> = got
            .iter()
            .map(|t| t.get("id").and_then(|v| v.as_str()).unwrap_or(""))
            .collect();
        assert_eq!(ids, vec!["1", "2", "10"]);
    }

    #[test]
    fn skips_malformed_json_and_non_json_files() {
        let home = tempdir().unwrap();
        let sid = "session-xyz";
        write_task(
            home.path(),
            sid,
            "1.json",
            r#"{"id":"1","content":"good","status":"pending"}"#,
        );
        write_task(home.path(), sid, "2.json", "not valid json {{{");
        write_task(home.path(), sid, "ignore.txt", r#"{"id":"99"}"#);

        let got = read_session_tasks_in(home.path(), sid).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].get("id").and_then(|v| v.as_str()), Some("1"));
    }
}
