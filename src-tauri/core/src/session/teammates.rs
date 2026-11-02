//! Agent-team teammates: Claude Code sessions a lead session spawns in tmux
//! panes (`CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS`).
//!
//! `claude agents --json` doesn't list a teammate, and a teammate writes no
//! `~/.claude/sessions/<pid>.json`, only `<pid>.<hash>.key`. What identifies
//! one is its argv: `--agent-id <name>@<team> --team-name <team>
//! --parent-session-id <lead uuid>`, with `--agent-name` and `--agent-type`
//! when set. Its parent process is tmux, not the lead.
//!
//! A teammate's own session ID appears nowhere on the process. Its transcript
//! is found by what the transcript itself records: every entry carries
//! `agentName` and `teamName`, and the first one is written seconds after the
//! process starts. [`find_transcript`] takes the matching transcript in the
//! teammate's project dir that started no earlier than the process. A
//! respawned teammate of the same name started earlier, so its transcript
//! never matches; when nothing or more than one candidate is left, the
//! teammate gets no transcript at all rather than a wrong one.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How long before the process start a transcript may begin. `ps` start
/// times have whole-second precision; the first entry is written later.
const START_SLACK_SECS: i64 = 5;
/// Largest transcript header read. The first timestamped entry holds the
/// teammate's prompt; a header that doesn't fit is not matched.
const HEADER_LIMIT: u64 = 1 << 20;

/// The teammate flags of one process's argv.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TeammateArgs {
    pub agent_name: String,
    pub team_name: String,
    pub lead_session_id: String,
    pub agent_type: Option<String>,
}

/// The link from a teammate session to its team and lead.
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeammateLink {
    /// The teammate's name in its team (`--agent-name`).
    pub agent_name: String,
    /// The team, `session-<first 8 hex digits of the lead session ID>`.
    pub team_name: String,
    /// Session ID of the lead session that spawned this teammate.
    pub lead_session_id: String,
    /// The agent type (`--agent-type`, or the team config), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
}

/// Names that are safe as one path component and in the agent ID.
fn is_plain_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && !s.starts_with('.')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn is_session_id(s: &str) -> bool {
    s.len() == 36 && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// The teammate flags in `argv`, or `None` when the process is not a
/// teammate or its flags don't agree with each other.
pub(crate) fn parse_argv(argv: &[OsString]) -> Option<TeammateArgs> {
    let mut flags: HashMap<&str, String> = HashMap::new();
    let mut args = argv.iter().skip(1).filter_map(|a| a.to_str());
    while let Some(arg) = args.next() {
        let Some(flag) = arg.strip_prefix("--") else {
            continue;
        };
        let (name, value) = match flag.split_once('=') {
            Some((name, value)) => (name, value.to_string()),
            None => match flag {
                "agent-id" | "agent-name" | "team-name" | "parent-session-id" | "agent-type" => {
                    (flag, args.next()?.to_string())
                }
                _ => continue,
            },
        };
        if matches!(
            name,
            "agent-id" | "agent-name" | "team-name" | "parent-session-id" | "agent-type"
        ) {
            flags.entry(name).or_insert(value);
        }
    }
    let agent_id = flags.remove("agent-id")?;
    let team_name = flags.remove("team-name")?;
    let lead_session_id = flags.remove("parent-session-id")?;
    let (id_name, id_team) = agent_id.split_once('@')?;
    let agent_name = flags
        .remove("agent-name")
        .unwrap_or_else(|| id_name.to_string());
    let consistent = id_name == agent_name
        && id_team == team_name
        && is_plain_name(&agent_name)
        && is_plain_name(&team_name)
        && is_session_id(&lead_session_id);
    consistent.then(|| TeammateArgs {
        agent_name,
        team_name,
        lead_session_id,
        agent_type: flags.remove("agent-type").filter(|t| !t.is_empty()),
    })
}

/// One member of `~/.claude/teams/<team>/config.json`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Member {
    name: String,
    #[serde(default)]
    agent_type: Option<String>,
    #[serde(default)]
    cwd: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TeamConfig {
    lead_session_id: String,
    #[serde(default)]
    members: Vec<Member>,
}

/// The team config's entry for `args`, when its lead agrees with the argv.
fn team_member(home: &Path, args: &TeammateArgs) -> Option<Member> {
    let path = home
        .join(".claude")
        .join("teams")
        .join(&args.team_name)
        .join("config.json");
    let config: TeamConfig = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    if config.lead_session_id != args.lead_session_id {
        return None;
    }
    config
        .members
        .into_iter()
        .find(|m| m.name == args.agent_name)
}

/// The link, the agent type filled from the team config when argv has none,
/// and the cwds to look for the transcript under (the process cwd first).
pub(crate) fn link_for(
    home: &Path,
    args: &TeammateArgs,
    process_cwd: Option<&Path>,
) -> (TeammateLink, Vec<PathBuf>) {
    let member = team_member(home, args);
    let mut cwds: Vec<PathBuf> = process_cwd.map(Path::to_path_buf).into_iter().collect();
    if let Some(cwd) = member.as_ref().and_then(|m| m.cwd.clone()) {
        if !cwds.contains(&cwd) {
            cwds.push(cwd);
        }
    }
    let agent_type = args
        .agent_type
        .clone()
        .or_else(|| member.and_then(|m| m.agent_type));
    (
        TeammateLink {
            agent_name: args.agent_name.clone(),
            team_name: args.team_name.clone(),
            lead_session_id: args.lead_session_id.clone(),
            agent_type,
        },
        cwds,
    )
}

/// What the start of a transcript says about who wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Header {
    session_id: Option<String>,
    agent_name: Option<String>,
    team_name: Option<String>,
    /// The first entry's `timestamp`, in ms since the epoch.
    started_ms: i64,
}

/// The first timestamped entry of a transcript, or `None` when there is none
/// yet (or it doesn't fit in `HEADER_LIMIT`).
pub(crate) fn read_header(path: &Path) -> Option<Header> {
    let file = std::fs::File::open(path).ok()?;
    let reader = BufReader::new(file.take(HEADER_LIMIT));
    for line in reader.split(b'\n') {
        let line = line.ok()?;
        let Ok(entry) = serde_json::from_slice::<serde_json::Value>(&line) else {
            continue;
        };
        let Some(ts) = entry.get("timestamp").and_then(|t| t.as_str()) else {
            continue;
        };
        let started_ms = chrono::DateTime::parse_from_rfc3339(ts)
            .ok()?
            .timestamp_millis();
        let text = |key: &str| entry.get(key).and_then(|v| v.as_str()).map(str::to_string);
        return Some(Header {
            session_id: text("sessionId"),
            agent_name: text("agentName"),
            team_name: text("teamName"),
            started_ms,
        });
    }
    None
}

/// Headers already read. A header is the start of an append-only file, so it
/// never changes once it is complete.
#[derive(Default)]
pub(crate) struct HeaderCache(HashMap<PathBuf, Header>);

impl HeaderCache {
    fn get(&mut self, path: &Path) -> Option<Header> {
        if let Some(header) = self.0.get(path) {
            return Some(header.clone());
        }
        let header = read_header(path)?;
        self.0.insert(path.to_path_buf(), header.clone());
        Some(header)
    }

    /// Drops the entries of `dir`'s files that its last pass didn't see.
    /// Entries of other directories are kept.
    fn retain(&mut self, dir: &Path, seen: &[PathBuf]) {
        self.0
            .retain(|path, _| path.parent() != Some(dir) || seen.contains(path));
    }
}

/// The session ID of the transcript `args` writes in `project_dir`: the
/// transcript whose first entry names this agent and team and was written no
/// earlier than the process started. Of several, the one that started last
/// (a `/clear` starts a new transcript in the same process). `None` when no
/// transcript matches, or the latest start is shared.
pub(crate) fn find_transcript(
    project_dir: &Path,
    args: &TeammateArgs,
    process_start_secs: u64,
    cache: &mut HeaderCache,
) -> Option<String> {
    let earliest_ms = (process_start_secs as i64 - START_SLACK_SECS) * 1000;
    let not_before = SystemTime::UNIX_EPOCH
        + Duration::from_secs(process_start_secs.saturating_sub(START_SLACK_SECS as u64));
    let mut seen = Vec::new();
    let mut best: Option<(i64, String)> = None;
    let mut tied = false;
    for entry in std::fs::read_dir(project_dir).ok()?.flatten() {
        let path = entry.path();
        let Some(stem) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".jsonl"))
        else {
            continue;
        };
        // A transcript written to since the process started has an mtime
        // after it; older files can't be this process's.
        let modified = entry.metadata().ok().and_then(|m| m.modified().ok());
        if modified.is_none_or(|m| m < not_before) {
            continue;
        }
        seen.push(path.clone());
        let Some(header) = cache.get(&path) else {
            continue;
        };
        let matches = header.agent_name.as_deref() == Some(args.agent_name.as_str())
            && header.team_name.as_deref() == Some(args.team_name.as_str())
            && header.session_id.as_deref() == Some(stem)
            && header.started_ms >= earliest_ms;
        if !matches {
            continue;
        }
        match &best {
            Some((started, _)) if *started > header.started_ms => {}
            Some((started, _)) if *started == header.started_ms => tied = true,
            _ => {
                best = Some((header.started_ms, stem.to_string()));
                tied = false;
            }
        }
    }
    cache.retain(project_dir, &seen);
    if tied {
        return None;
    }
    best.map(|(_, id)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEAD: &str = "52d94f40-632b-43c7-a783-f31783dfe57f";

    fn argv(args: &[&str]) -> Vec<OsString> {
        std::iter::once("/x/versions/2.1.314")
            .chain(args.iter().copied())
            .map(OsString::from)
            .collect()
    }

    fn teammate() -> Vec<OsString> {
        argv(&[
            "--agent-id",
            "p10-multi-cwd@session-52d94f40",
            "--agent-name",
            "p10-multi-cwd",
            "--team-name",
            "session-52d94f40",
            "--agent-color",
            "orange",
            "--parent-session-id",
            LEAD,
            "--permission-mode",
            "auto",
        ])
    }

    fn args() -> TeammateArgs {
        parse_argv(&teammate()).unwrap()
    }

    #[test]
    fn parses_the_teammate_flags() {
        assert_eq!(
            args(),
            TeammateArgs {
                agent_name: "p10-multi-cwd".into(),
                team_name: "session-52d94f40".into(),
                lead_session_id: LEAD.into(),
                agent_type: None,
            }
        );
        let typed = argv(&[
            "--agent-id=w@session-1",
            "--team-name=session-1",
            "--parent-session-id",
            LEAD,
            "--agent-type",
            "general-purpose",
        ]);
        let typed = parse_argv(&typed).unwrap();
        assert_eq!(typed.agent_name, "w", "the name defaults to the agent ID's");
        assert_eq!(typed.agent_type.as_deref(), Some("general-purpose"));
    }

    #[test]
    fn rejects_processes_that_are_not_consistent_teammates() {
        assert_eq!(parse_argv(&argv(&["--permission-mode", "auto"])), None);
        // The agent ID must name the same team and agent.
        let other_team = argv(&[
            "--agent-id",
            "w@session-2",
            "--team-name",
            "session-1",
            "--parent-session-id",
            LEAD,
        ]);
        assert_eq!(parse_argv(&other_team), None);
        let other_name = argv(&[
            "--agent-id",
            "w@session-1",
            "--agent-name",
            "v",
            "--team-name",
            "session-1",
            "--parent-session-id",
            LEAD,
        ]);
        assert_eq!(parse_argv(&other_name), None);
        // A team name is a path component of the team config.
        let traversal = argv(&[
            "--agent-id",
            "w@../x",
            "--team-name",
            "../x",
            "--parent-session-id",
            LEAD,
        ]);
        assert_eq!(parse_argv(&traversal), None);
        let bad_lead = argv(&[
            "--agent-id",
            "w@session-1",
            "--team-name",
            "session-1",
            "--parent-session-id",
            "lead",
        ]);
        assert_eq!(parse_argv(&bad_lead), None);
        // A flag with no value.
        assert_eq!(parse_argv(&argv(&["--agent-id"])), None);
    }

    fn write_team_config(home: &Path, lead: &str) {
        let dir = home.join(".claude/teams/session-52d94f40");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.json"),
            serde_json::json!({
                "name": "session-52d94f40",
                "leadSessionId": lead,
                "members": [
                    {"agentId": "team-lead@session-52d94f40", "name": "team-lead", "agentType": "team-lead", "cwd": "/repo"},
                    {"agentId": "p10-multi-cwd@session-52d94f40", "name": "p10-multi-cwd", "agentType": "general-purpose", "cwd": "/repo/sub", "tmuxPaneId": "%12", "backendType": "tmux", "isActive": true}
                ]
            })
            .to_string(),
        )
        .unwrap();
    }

    #[test]
    fn the_team_config_fills_the_agent_type_and_cwd() {
        let home = tempfile::tempdir().unwrap();
        write_team_config(home.path(), LEAD);
        let (link, cwds) = link_for(home.path(), &args(), Some(Path::new("/repo")));
        assert_eq!(
            link,
            TeammateLink {
                agent_name: "p10-multi-cwd".into(),
                team_name: "session-52d94f40".into(),
                lead_session_id: LEAD.into(),
                agent_type: Some("general-purpose".into()),
            }
        );
        assert_eq!(
            cwds,
            vec![PathBuf::from("/repo"), PathBuf::from("/repo/sub")]
        );
    }

    #[test]
    fn a_team_config_for_another_lead_is_ignored() {
        let home = tempfile::tempdir().unwrap();
        write_team_config(home.path(), "00000000-0000-0000-0000-000000000000");
        let (link, cwds) = link_for(home.path(), &args(), Some(Path::new("/repo")));
        assert_eq!(link.lead_session_id, LEAD, "argv decides the lead");
        assert_eq!(link.agent_type, None);
        assert_eq!(cwds, vec![PathBuf::from("/repo")]);
    }

    /// Writes `<id>.jsonl` whose first timestamped entry is at `secs`.
    fn transcript(dir: &Path, id: &str, name: Option<&str>, team: &str, secs: i64) {
        let ts = chrono::DateTime::from_timestamp(secs, 155_000_000)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let mut first = serde_json::json!({
            "type": "user", "sessionId": id, "timestamp": ts, "teamName": team,
            "message": {"role": "user", "content": "go"}
        });
        if let Some(name) = name {
            first["agentName"] = name.into();
        }
        let body = format!(
            "{}\n{}\n",
            serde_json::json!({"type": "mode", "mode": "normal", "sessionId": id}),
            first
        );
        std::fs::write(dir.join(format!("{id}.jsonl")), body).unwrap();
    }

    const START: u64 = 1_793_000_000;

    fn id(n: u8) -> String {
        format!("{n:08}-0000-0000-0000-000000000000")
    }

    #[test]
    fn finds_the_transcript_this_process_started() {
        let dir = tempfile::tempdir().unwrap();
        let start = START as i64;
        // The real one, written 5 s after the process started.
        transcript(
            dir.path(),
            &id(1),
            Some("p10-multi-cwd"),
            "session-52d94f40",
            start + 5,
        );
        // A respawned teammate with the same name that ran earlier.
        transcript(
            dir.path(),
            &id(2),
            Some("p10-multi-cwd"),
            "session-52d94f40",
            start - 3600,
        );
        // Another teammate of the same team, and the lead's own transcript.
        transcript(
            dir.path(),
            &id(3),
            Some("p2-db-logs"),
            "session-52d94f40",
            start + 5,
        );
        transcript(dir.path(), &id(4), None, "session-52d94f40", start + 5);
        // Same name in another team.
        transcript(
            dir.path(),
            &id(5),
            Some("p10-multi-cwd"),
            "session-99999999",
            start + 5,
        );
        let found = find_transcript(dir.path(), &args(), START, &mut HeaderCache::default());
        assert_eq!(found, Some(id(1)));
    }

    #[test]
    fn a_clear_moves_the_teammate_to_its_newer_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let start = START as i64;
        transcript(
            dir.path(),
            &id(1),
            Some("p10-multi-cwd"),
            "session-52d94f40",
            start + 5,
        );
        transcript(
            dir.path(),
            &id(6),
            Some("p10-multi-cwd"),
            "session-52d94f40",
            start + 900,
        );
        let found = find_transcript(dir.path(), &args(), START, &mut HeaderCache::default());
        assert_eq!(found, Some(id(6)));
    }

    #[test]
    fn no_match_or_a_tie_attaches_no_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let start = START as i64;
        // Only an earlier teammate's transcript.
        transcript(
            dir.path(),
            &id(2),
            Some("p10-multi-cwd"),
            "session-52d94f40",
            start - 60,
        );
        assert_eq!(
            find_transcript(dir.path(), &args(), START, &mut HeaderCache::default()),
            None
        );
        // Two transcripts that both claim the same start.
        transcript(
            dir.path(),
            &id(7),
            Some("p10-multi-cwd"),
            "session-52d94f40",
            start + 5,
        );
        transcript(
            dir.path(),
            &id(8),
            Some("p10-multi-cwd"),
            "session-52d94f40",
            start + 5,
        );
        assert_eq!(
            find_transcript(dir.path(), &args(), START, &mut HeaderCache::default()),
            None
        );
        // A missing project dir.
        let gone = dir.path().join("missing");
        assert_eq!(
            find_transcript(&gone, &args(), START, &mut HeaderCache::default()),
            None
        );
    }

    #[test]
    fn a_transcript_whose_session_id_differs_from_its_name_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        transcript(
            dir.path(),
            &id(1),
            Some("p10-multi-cwd"),
            "session-52d94f40",
            START as i64 + 5,
        );
        std::fs::rename(
            dir.path().join(format!("{}.jsonl", id(1))),
            dir.path().join(format!("{}.jsonl", id(9))),
        )
        .unwrap();
        assert_eq!(
            find_transcript(dir.path(), &args(), START, &mut HeaderCache::default()),
            None
        );
    }

    #[test]
    fn a_transcript_with_no_entry_yet_is_read_again_later() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("{}.jsonl", id(1)));
        std::fs::write(&path, "{\"type\":\"mode\",\"mode\":\"normal\"}\n").unwrap();
        let mut cache = HeaderCache::default();
        assert_eq!(
            find_transcript(dir.path(), &args(), START, &mut cache),
            None
        );
        transcript(
            dir.path(),
            &id(1),
            Some("p10-multi-cwd"),
            "session-52d94f40",
            START as i64 + 5,
        );
        assert_eq!(
            find_transcript(dir.path(), &args(), START, &mut cache),
            Some(id(1))
        );
    }
}
