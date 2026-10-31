//! Lookup of Claude Code transcript files under `~/.claude/projects/*/<id>.jsonl`.

use std::path::{Path, PathBuf};

/// Find the transcript JSONL for a Claude Code session in any project directory
/// under the user's home. The first matching project directory (in `read_dir`
/// order) wins.
pub fn find_claude_session_file(session_id: &str) -> Option<PathBuf> {
    find_claude_session_file_under(&dirs::home_dir()?, session_id)
}

/// Same as [`find_claude_session_file`], with an explicit home directory.
pub fn find_claude_session_file_under(home: &Path, session_id: &str) -> Option<PathBuf> {
    find_session_file_in_projects_dir(&home.join(".claude").join("projects"), session_id)
}

/// A session id that names one file inside a project directory: not
/// empty, not absolute, and without separators, `..` or NUL.
fn is_plain_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && !session_id.contains(['/', '\\', '\0'])
        && !session_id.contains("..")
        && !Path::new(session_id).is_absolute()
}

/// Scan the direct child directories of `projects_dir` for `<session_id>.jsonl`.
/// Files at the `projects_dir` level are ignored. An id that is not a plain
/// file name (`is_plain_session_id`) finds nothing, so a caller's id can
/// never reach outside the project directories.
pub(crate) fn find_session_file_in_projects_dir(
    projects_dir: &Path,
    session_id: &str,
) -> Option<PathBuf> {
    if !is_plain_session_id(session_id) {
        return None;
    }
    let session_filename = format!("{}.jsonl", session_id);
    std::fs::read_dir(projects_dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .map(|path| path.join(&session_filename))
        .find(|session_file| session_file.exists())
}

#[cfg(test)]
mod tests {
    use super::find_claude_session_file_under;

    const SESSION_ID: &str = "ffffffff-ffff-ffff-ffff-ffffffffffff";

    #[test]
    fn finds_session_file_in_second_project_dir() {
        let home = tempfile::tempdir().unwrap();
        let projects = home.path().join(".claude").join("projects");
        let first = projects.join("-first-project");
        let second = projects.join("-second-project");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(first.join("other.jsonl"), "").unwrap();
        let jsonl = second.join(format!("{SESSION_ID}.jsonl"));
        std::fs::write(&jsonl, "").unwrap();

        assert_eq!(
            find_claude_session_file_under(home.path(), SESSION_ID),
            Some(jsonl)
        );
    }

    #[test]
    fn missing_session_file_returns_none() {
        let home = tempfile::tempdir().unwrap();
        let project = home.path().join(".claude").join("projects").join("-p");
        std::fs::create_dir_all(&project).unwrap();

        assert_eq!(
            find_claude_session_file_under(home.path(), SESSION_ID),
            None
        );
    }

    #[test]
    fn missing_projects_dir_returns_none() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            find_claude_session_file_under(home.path(), SESSION_ID),
            None
        );
    }

    /// An id that is a path, or climbs out with `..`, finds nothing, even
    /// where `<id>.jsonl` exists; a plain id is still found.
    #[test]
    fn path_like_ids_find_nothing() {
        let home = tempfile::tempdir().unwrap();
        let projects = home.path().join(".claude").join("projects");
        let project = projects.join("-p");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join(format!("{SESSION_ID}.jsonl")), "").unwrap();
        let outside = home.path().join("secret");
        std::fs::write(home.path().join("secret.jsonl"), "").unwrap();
        std::fs::write(projects.join("up.jsonl"), "").unwrap();

        for id in [
            outside.to_str().unwrap(),
            "../../../secret",
            "../up",
            "..",
            "",
            "a\0b",
        ] {
            assert_eq!(
                find_claude_session_file_under(home.path(), id),
                None,
                "{id}"
            );
        }
        assert_eq!(
            find_claude_session_file_under(home.path(), SESSION_ID),
            Some(project.join(format!("{SESSION_ID}.jsonl")))
        );
    }

    #[test]
    fn file_at_projects_level_is_skipped() {
        let home = tempfile::tempdir().unwrap();
        let projects = home.path().join(".claude").join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        std::fs::write(projects.join(format!("{SESSION_ID}.jsonl")), "").unwrap();

        assert_eq!(
            find_claude_session_file_under(home.path(), SESSION_ID),
            None
        );
    }
}
