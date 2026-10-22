//! Lightweight session reference resolution (full id, prefix, or provider-scoped key)
//! that scans session files on disk instead of running full detection.

use crate::session;

#[cfg(test)]
fn resolve_session_id_lightweight_under(
    prefix: &str,
    home_dir: &std::path::Path,
) -> Result<String, String> {
    resolve_session_reference_lightweight_under(prefix, home_dir).map(|(id, _)| id)
}

/// Resolve a raw ID/prefix while retaining the provider namespace when it is
/// discoverable. A full ID that cannot be enumerated keeps `None` for
/// backwards compatibility with older installations; conversation lookup can
/// then use its legacy provider fallback.
pub(super) fn resolve_session_reference_lightweight(
    prefix: &str,
) -> Result<(String, Option<session::SessionProvider>), String> {
    let home_dir = dirs::home_dir().ok_or("Failed to get home directory")?;
    resolve_session_reference_lightweight_under(prefix, &home_dir)
}

fn resolve_session_reference_lightweight_under(
    reference: &str,
    home_dir: &std::path::Path,
) -> Result<(String, Option<session::SessionProvider>), String> {
    let (provider_filter, prefix) = split_provider_scoped(reference)
        .map_or((None, reference), |(provider, raw_prefix)| {
            (Some(provider), raw_prefix)
        });
    // Explicit full OpenCode IDs do not depend on the monitor's freshness window.
    if provider_filter == Some(session::SessionProvider::Opencode)
        && session::opencode::is_full_session_reference(prefix)
    {
        return Ok((prefix.to_string(), provider_filter));
    }
    let projects_dir = home_dir.join(".claude").join("projects");

    let mut matches: Vec<session::SessionIdentity> = Vec::new();

    if let Ok(project_entries) = std::fs::read_dir(&projects_dir) {
        for project_entry in project_entries.flatten() {
            let project_path = project_entry.path();
            if !project_path.is_dir() {
                continue;
            }
            if let Ok(files) = std::fs::read_dir(&project_path) {
                for file_entry in files.flatten() {
                    let file_path = file_entry.path();
                    if file_path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                        if let Some(stem) = file_path.file_stem().and_then(|s| s.to_str()) {
                            if stem.starts_with(prefix) && !stem.starts_with("agent-") {
                                let id = stem.to_string();
                                let identity = session::SessionIdentity::new(
                                    session::SessionProvider::ClaudeCode,
                                    id,
                                );
                                if !matches.contains(&identity) {
                                    matches.push(identity);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    matches.extend(
        session::codex_session_ids(home_dir, prefix)
            .into_iter()
            .map(|id| session::SessionIdentity::new(session::SessionProvider::Codex, id)),
    );
    matches.extend(
        session::cursor_session_ids(home_dir, prefix)
            .into_iter()
            .map(|id| session::SessionIdentity::new(session::SessionProvider::Cursor, id)),
    );

    matches.extend(
        session::pi_session_ids(home_dir, prefix)
            .into_iter()
            .map(|id| session::SessionIdentity::new(session::SessionProvider::Pi, id)),
    );

    if provider_filter.is_none() || provider_filter == Some(session::SessionProvider::Opencode) {
        matches.extend(
            session::opencode::detect_once()
                .into_iter()
                .filter_map(|s| s.identity())
                .filter(|s| s.session_id.starts_with(prefix)),
        );
    }

    if let Some(provider) = provider_filter {
        matches.retain(|identity| identity.provider == provider);
    }

    resolve_session_reference_matches(reference, prefix, provider_filter, matches)
}

/// Parse the canonical provider-scoped key emitted by `list`/`watch`.
/// Unknown prefixes remain ordinary raw references for backwards compatibility.
pub(super) fn split_provider_scoped(
    reference: &str,
) -> Option<(session::SessionProvider, &str)> {
    let (provider, raw_prefix) = reference.split_once(':')?;
    if raw_prefix.is_empty() {
        return None;
    }
    Some((session::SessionProvider::from_key(provider)?, raw_prefix))
}

fn resolve_session_reference_matches(
    reference: &str,
    raw_prefix: &str,
    provider_filter: Option<session::SessionProvider>,
    mut matches: Vec<session::SessionIdentity>,
) -> Result<(String, Option<session::SessionProvider>), String> {
    matches.sort_by_key(|identity| identity.key());
    matches.dedup();

    match matches.len() {
        0 => {
            // If it looks like a full UUID, let it through (might be a valid ID
            // in a project dir we couldn't enumerate). Otherwise, fail clearly.
            if raw_prefix.len() >= 32 {
                Ok((raw_prefix.to_string(), provider_filter))
            } else {
                Err(format!(
                    "No session found matching prefix '{}' across Claude Code, Codex, and Cursor",
                    reference
                ))
            }
        }
        1 => {
            let identity = matches.into_iter().next().unwrap();
            Ok((identity.session_id, Some(identity.provider)))
        }
        n => Err(format!(
            "Ambiguous session ID prefix '{}' matches {} sessions across Claude Code, Codex, and Cursor",
            reference, n
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::source::SessionProvider;

    #[test]
    fn split_provider_scoped_accepts_every_provider_key() {
        for provider in SessionProvider::ALL {
            let reference = format!("{}:abc", provider.as_key());
            assert_eq!(split_provider_scoped(&reference), Some((provider, "abc")));
        }
        assert_eq!(split_provider_scoped("bogus:abc"), None);
        assert_eq!(split_provider_scoped("codex:"), None);
        assert_eq!(split_provider_scoped("abc"), None);
    }

    #[test]
    fn explicit_opencode_id_bypasses_recent_session_discovery() {
        let home = tempfile::tempdir().unwrap();
        let id = "ses_f835ea1f8ffelnS2uzHgkM9qUq";
        let result = super::resolve_session_reference_lightweight_under(
            &format!("opencode:{id}"),
            home.path(),
        )
        .unwrap();
        assert_eq!(result, (id.to_string(), Some(SessionProvider::Opencode)));
        let scoped = format!("{id}?directory=%2Farchive%2F%E4%B8%AD%E6%96%87");
        let result = super::resolve_session_reference_lightweight_under(
            &format!("opencode:{scoped}"),
            home.path(),
        )
        .unwrap();
        assert_eq!(result, (scoped, Some(SessionProvider::Opencode)));
    }

    #[test]
    fn session_prefix_resolves_unique_codex_id() {
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join(".codex/sessions/2026/07/13");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(
            sessions.join("rollout-codex-unique.jsonl"),
            r#"{"timestamp":"2026-07-13T02:03:04Z","type":"session_meta","payload":{"id":"codex-unique-thread","cwd":"/tmp/codex","source":"cli","originator":"codex-tui"}}"#,
        )
        .unwrap();

        assert_eq!(
            resolve_session_id_lightweight_under("codex-unique", home.path()).unwrap(),
            "codex-unique-thread"
        );
    }

    #[test]
    fn session_prefix_reports_ambiguity_across_providers() {
        let home = tempfile::tempdir().unwrap();
        let claude_project = home.path().join(".claude/projects/project");
        std::fs::create_dir_all(&claude_project).unwrap();
        std::fs::write(claude_project.join("shared-claude-session.jsonl"), "").unwrap();
        let codex_sessions = home.path().join(".codex/sessions/2026/07/13");
        std::fs::create_dir_all(&codex_sessions).unwrap();
        std::fs::write(
            codex_sessions.join("rollout-shared-codex.jsonl"),
            r#"{"timestamp":"2026-07-13T02:03:04Z","type":"session_meta","payload":{"id":"shared-codex-thread","cwd":"/tmp/codex","source":"cli","originator":"codex-tui"}}"#,
        )
        .unwrap();

        let error = resolve_session_id_lightweight_under("shared", home.path()).unwrap_err();

        assert!(error.contains("Ambiguous session ID prefix 'shared'"));
        assert!(error.contains("matches 2 sessions"));
        assert!(error.contains("Claude Code, Codex, and Cursor"));
    }

    #[test]
    fn exact_same_id_is_ambiguous_across_provider_namespaces() {
        let home = tempfile::tempdir().unwrap();
        let same_id = "same-provider-id";
        let claude_project = home.path().join(".claude/projects/project");
        std::fs::create_dir_all(&claude_project).unwrap();
        std::fs::write(claude_project.join(format!("{same_id}.jsonl")), "").unwrap();

        let codex_sessions = home.path().join(".codex/sessions/2026/07/13");
        std::fs::create_dir_all(&codex_sessions).unwrap();
        std::fs::write(
            codex_sessions.join("rollout-same-provider-id.jsonl"),
            format!(
                r#"{{"timestamp":"2026-07-13T02:03:04Z","type":"session_meta","payload":{{"id":"{same_id}","cwd":"/tmp/codex","source":"cli","originator":"codex-tui"}}}}"#
            ),
        )
        .unwrap();

        let error = resolve_session_reference_lightweight_under(same_id, home.path()).unwrap_err();
        assert!(error.contains("matches 2 sessions"));

        let (resolved_id, provider) =
            resolve_session_reference_lightweight_under(&format!("codex:{same_id}"), home.path())
                .unwrap();
        assert_eq!(resolved_id, same_id);
        assert_eq!(provider, Some(SessionProvider::Codex));
    }

    #[test]
    fn session_prefix_reports_no_match_across_providers() {
        let home = tempfile::tempdir().unwrap();
        let error = resolve_session_id_lightweight_under("missing", home.path()).unwrap_err();

        assert_eq!(
            error,
            "No session found matching prefix 'missing' across Claude Code, Codex, and Cursor"
        );
    }
}
