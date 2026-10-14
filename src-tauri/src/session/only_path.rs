//! Recording filter: `C9WATCH_ONLY_PATH` hides every session outside the given folders.
//!
//! Meant for demos and screen recordings, so real sessions on the machine stay off
//! camera. Unset (the default) shows everything. The value is a `:`-separated list of
//! folders; a session is kept when its cwd or project path is inside one of them.

use super::source::DetectedSession;
use std::path::{Path, PathBuf};

pub const ENV_VAR: &str = "C9WATCH_ONLY_PATH";

/// Folders from `C9WATCH_ONLY_PATH`, or `None` when the variable is unset or empty.
pub fn roots_from_env() -> Option<Vec<PathBuf>> {
    let raw = std::env::var_os(ENV_VAR)?;
    let roots = parse_roots(&raw);
    (!roots.is_empty()).then_some(roots)
}

/// Each listed folder, plus its canonical form when that differs
/// (on macOS `/tmp` is a symlink to `/private/tmp`).
fn parse_roots(raw: &std::ffi::OsStr) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for root in std::env::split_paths(raw).filter(|p| !p.as_os_str().is_empty()) {
        if let Ok(canonical) = root.canonicalize() {
            if canonical != root {
                roots.push(canonical);
            }
        }
        roots.push(root);
    }
    roots
}

fn is_inside(path: &Path, roots: &[PathBuf]) -> bool {
    if path.as_os_str().is_empty() {
        return false;
    }
    if roots.iter().any(|root| path.starts_with(root)) {
        return true;
    }
    path.canonicalize()
        .map(|canonical| roots.iter().any(|root| canonical.starts_with(root)))
        .unwrap_or(false)
}

/// Keep only the sessions whose cwd or project path is inside `roots`.
pub fn retain_inside(sessions: Vec<DetectedSession>, roots: &[PathBuf]) -> Vec<DetectedSession> {
    sessions
        .into_iter()
        .filter(|s| is_inside(&s.cwd, roots) || is_inside(&s.project_path, roots))
        .collect()
}

/// Apply `C9WATCH_ONLY_PATH` when it is set; otherwise return `sessions` unchanged.
pub fn apply_env_filter(sessions: Vec<DetectedSession>) -> Vec<DetectedSession> {
    match roots_from_env() {
        Some(roots) => retain_inside(sessions, &roots),
        None => sessions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(cwd: &str, project: &str) -> DetectedSession {
        DetectedSession::with_legacy_defaults(
            1,
            PathBuf::from(cwd),
            PathBuf::from(project),
            Some(format!("id-{cwd}")),
            "demo".to_string(),
        )
    }

    fn cwds(sessions: &[DetectedSession]) -> Vec<String> {
        sessions
            .iter()
            .map(|s| s.cwd.display().to_string())
            .collect()
    }

    #[test]
    fn keeps_only_sessions_inside_the_root() {
        let roots = parse_roots("/nonexistent/c9-demo".as_ref());
        let kept = retain_inside(
            vec![
                session(
                    "/nonexistent/c9-demo/todo-api",
                    "/nonexistent/c9-demo/todo-api",
                ),
                session("/Users/me/secret", "/Users/me/secret"),
                session("/nonexistent/c9-demo", "/nonexistent/c9-demo"),
            ],
            &roots,
        );
        assert_eq!(
            cwds(&kept),
            ["/nonexistent/c9-demo/todo-api", "/nonexistent/c9-demo"]
        );
    }

    #[test]
    fn a_sibling_folder_with_the_same_prefix_is_not_inside() {
        let roots = parse_roots("/nonexistent/c9-demo".as_ref());
        let kept = retain_inside(vec![session("/nonexistent/c9-demo-other", "")], &roots);
        assert!(kept.is_empty());
    }

    #[test]
    fn project_path_alone_is_enough() {
        let roots = parse_roots("/nonexistent/c9-demo".as_ref());
        let kept = retain_inside(vec![session("", "/nonexistent/c9-demo/notes-site")], &roots);
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn several_roots_are_separated_by_colons() {
        let roots = parse_roots("/nonexistent/a:/nonexistent/b".as_ref());
        let kept = retain_inside(
            vec![
                session("/nonexistent/a/x", ""),
                session("/nonexistent/b/y", ""),
                session("/nonexistent/c/z", ""),
            ],
            &roots,
        );
        assert_eq!(cwds(&kept), ["/nonexistent/a/x", "/nonexistent/b/y"]);
    }

    #[test]
    fn empty_value_means_no_filter() {
        assert!(parse_roots("".as_ref()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_root_matches_the_resolved_session_path() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir_all(real.join("todo-api")).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        // Root given through the symlink, session reported by its resolved path.
        let roots = parse_roots(link.as_os_str());
        let resolved = real.join("todo-api").canonicalize().unwrap();
        let kept = retain_inside(vec![session(resolved.to_str().unwrap(), "")], &roots);
        assert_eq!(kept.len(), 1);

        // And the other way round.
        let roots = parse_roots(real.as_os_str());
        let via_link = link.join("todo-api");
        let kept = retain_inside(vec![session(via_link.to_str().unwrap(), "")], &roots);
        assert_eq!(kept.len(), 1);
    }
}
