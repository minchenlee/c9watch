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
use std::collections::{HashMap, HashSet};
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

/// The transcript selection matched: its device, inode and birth time
/// (where the file system keeps one), and the header read from it.
/// Enrichment reads the transcript only through a descriptor whose file
/// has the same identity and still starts with the same header. An inode
/// number alone is not enough: ext4 hands a freed inode to the next file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptFile {
    pub(crate) dev: u64,
    pub(crate) ino: u64,
    pub(crate) birth: Option<SystemTime>,
    pub(crate) header: Header,
}

/// What a header cache entry is valid for: the file's identity plus its
/// change time and size, all from one `fstat`. Any change (an append
/// included) reads the header again; a header is a few KB at most.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    birth: Option<SystemTime>,
    changed_ns: i128,
    size: u64,
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
/// `None` unless the team config lists this agent under the same lead: argv
/// alone is a claim any process can make.
pub(crate) fn link_for(
    home: &Path,
    args: &TeammateArgs,
    process_cwd: Option<&Path>,
) -> Option<(TeammateLink, Vec<PathBuf>)> {
    let member = team_member(home, args)?;
    let mut cwds: Vec<PathBuf> = process_cwd.map(Path::to_path_buf).into_iter().collect();
    if let Some(cwd) = member.cwd {
        if !cwds.contains(&cwd) {
            cwds.push(cwd);
        }
    }
    let agent_type = args.agent_type.clone().or(member.agent_type);
    Some((
        TeammateLink {
            agent_name: args.agent_name.clone(),
            team_name: args.team_name.clone(),
            lead_session_id: args.lead_session_id.clone(),
            agent_type,
        },
        cwds,
    ))
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

/// Opens `path` only if it is a regular file, not a symlink: opening does
/// not follow a symlink or block on a FIFO, and the opened file is checked.
#[cfg(test)]
fn open_regular(path: &Path) -> Option<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    file.metadata().ok()?.is_file().then_some(file)
}

/// The first timestamped entry of `path`, or `None` when it has none or is
/// not a regular file.
#[cfg(test)]
pub(crate) fn read_header(path: &Path) -> Option<Header> {
    header_of(open_regular(path)?)
}

/// The first timestamped entry of a transcript, or `None` when there is none
/// yet (or it doesn't fit in `HEADER_LIMIT`).
fn header_of(file: impl Read) -> Option<Header> {
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

/// The stamp of an open regular file, from one `fstat` (`statx` on Linux,
/// which also gives the birth time where the file system keeps it).
#[cfg(unix)]
fn stamp(file: &std::fs::File) -> Option<Stamp> {
    use std::os::unix::fs::MetadataExt;
    let meta = file.metadata().ok()?;
    meta.is_file().then(|| Stamp {
        dev: meta.dev(),
        ino: meta.ino(),
        birth: meta.created().ok(),
        changed_ns: i128::from(meta.ctime()) * 1_000_000_000 + i128::from(meta.ctime_nsec()),
        size: meta.len(),
    })
}

/// The [`TranscriptFile`] of an open file: its identity and its header,
/// from the one descriptor.
#[cfg(unix)]
pub(crate) fn transcript_file_of(file: &std::fs::File) -> Option<TranscriptFile> {
    let stamp = stamp(file)?;
    Some(TranscriptFile {
        dev: stamp.dev,
        ino: stamp.ino,
        birth: stamp.birth,
        header: header_of(file)?,
    })
}

/// Headers already read, each with the stamp of the file it was read from.
/// A cached header is used only while the path names a file with the same
/// stamp; a replaced file (even one given the old inode) is read again.
#[derive(Default)]
pub(crate) struct HeaderCache(HashMap<PathBuf, (Stamp, Header)>);

impl HeaderCache {
    /// The header of the file `open` returns for `path`, and that file's
    /// identity, both from the one descriptor.
    #[cfg(unix)]
    fn get(
        &mut self,
        path: &Path,
        open: impl FnOnce() -> Option<std::fs::File>,
    ) -> Option<TranscriptFile> {
        let file = open()?;
        let stamp = stamp(&file)?;
        let header = match self.0.get(path) {
            Some((cached, header)) if *cached == stamp => header.clone(),
            _ => {
                let header = header_of(&file)?;
                self.0.insert(path.to_path_buf(), (stamp, header.clone()));
                header
            }
        };
        Some(TranscriptFile {
            dev: stamp.dev,
            ino: stamp.ino,
            birth: stamp.birth,
            header,
        })
    }

    /// Drops the entries of files no lookup of the last pass saw.
    pub(crate) fn retain(&mut self, seen: &HashSet<PathBuf>) {
        self.0.retain(|path, _| seen.contains(path));
    }
}

/// A project directory opened once and read only through its descriptor,
/// so swapping the path for a symlink after the check changes nothing.
#[cfg(unix)]
struct ProjectDir {
    fd: std::os::fd::OwnedFd,
    /// Where the descriptor is, as the kernel reports it.
    path: PathBuf,
}

#[cfg(unix)]
impl ProjectDir {
    /// `project_dir`, when it is a directory (not a symlink) that is under
    /// `projects` (`~/.claude/projects`) as opened.
    fn open(project_dir: &Path, projects: &Path) -> Option<Self> {
        use std::os::fd::{FromRawFd, OwnedFd};
        use std::os::unix::ffi::OsStrExt;
        let root = projects.canonicalize().ok()?;
        let c = std::ffi::CString::new(project_dir.as_os_str().as_bytes()).ok()?;
        // SAFETY: `c` is a valid NUL-terminated path.
        let raw = unsafe {
            libc::open(
                c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if raw < 0 {
            return None;
        }
        // SAFETY: `raw` is a descriptor this function just opened.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let path = fd_path(&fd)?;
        (path.starts_with(&root) && path != root).then_some(Self { fd, path })
    }

    /// The regular files in the directory (symlinks, FIFOs and the rest
    /// left out), with their mtimes, read relative to the descriptor.
    fn files(&self) -> Vec<(std::ffi::OsString, SystemTime)> {
        use std::os::fd::AsRawFd;
        use std::os::unix::ffi::OsStrExt;
        let mut out = Vec::new();
        // SAFETY: `dup` of an open descriptor; `fdopendir` owns the copy.
        let dir = unsafe {
            let copy = libc::dup(self.fd.as_raw_fd());
            if copy < 0 {
                return out;
            }
            let dir = libc::fdopendir(copy);
            if dir.is_null() {
                libc::close(copy);
                return out;
            }
            libc::rewinddir(dir);
            dir
        };
        loop {
            // SAFETY: `dir` is an open directory stream.
            let entry = unsafe { libc::readdir(dir) };
            if entry.is_null() {
                break;
            }
            // SAFETY: `readdir` returned a valid entry with a NUL-terminated name.
            let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
            if matches!(name.to_bytes(), b"." | b"..") {
                continue;
            }
            if let Some(modified) = self.stat_regular(name) {
                out.push((
                    std::ffi::OsStr::from_bytes(name.to_bytes()).to_os_string(),
                    modified,
                ));
            }
        }
        // SAFETY: `dir` is open and not used after this.
        unsafe { libc::closedir(dir) };
        out
    }

    /// The mtime of `name`, when it is a regular file (not followed).
    fn stat_regular(&self, name: &std::ffi::CStr) -> Option<SystemTime> {
        use std::os::fd::AsRawFd;
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `st` is writable and `name` is NUL-terminated.
        let r = unsafe {
            libc::fstatat(
                self.fd.as_raw_fd(),
                name.as_ptr(),
                &mut st,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if r != 0 || (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
            return None;
        }
        let secs = u64::try_from(st.st_mtime).ok()?;
        Some(SystemTime::UNIX_EPOCH + Duration::new(secs, st.st_mtime_nsec as u32))
    }

    /// `name` in the directory, opened without following a symlink or
    /// blocking on a FIFO, when it is a regular file.
    fn open_file(&self, name: &std::ffi::OsStr) -> Option<std::fs::File> {
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(name.as_bytes()).ok()?;
        // SAFETY: `c` is NUL-terminated; the descriptor is open.
        let raw = unsafe {
            libc::openat(
                self.fd.as_raw_fd(),
                c.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if raw < 0 {
            return None;
        }
        // SAFETY: `raw` is a descriptor this function just opened.
        let file = unsafe { std::fs::File::from_raw_fd(raw) };
        file.metadata().ok()?.is_file().then_some(file)
    }
}

/// The path of an open descriptor.
#[cfg(target_os = "macos")]
fn fd_path(fd: &std::os::fd::OwnedFd) -> Option<PathBuf> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    let mut buf = vec![0u8; libc::PATH_MAX as usize];
    // SAFETY: F_GETPATH writes at most PATH_MAX bytes into `buf`.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETPATH, buf.as_mut_ptr()) } != 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0)?;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..end])))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn fd_path(fd: &std::os::fd::OwnedFd) -> Option<PathBuf> {
    use std::os::fd::AsRawFd;
    std::fs::read_link(format!("/proc/self/fd/{}", fd.as_raw_fd())).ok()
}

/// The session ID and file of the transcript `args` writes in `project_dir`: the
/// transcript whose first entry names this agent and team and was written no
/// earlier than the process started. Of several, the one that started last
/// (a `/clear` starts a new transcript in the same process). `None` when no
/// transcript matches, or the latest start is shared. `project_dir` must be
/// under `projects`, and only regular files are read. Each file looked at is
/// added to `seen`, for `HeaderCache::retain` after the pass.
#[cfg(unix)]
pub(crate) fn find_transcript(
    project_dir: &Path,
    projects: &Path,
    args: &TeammateArgs,
    process_start_secs: u64,
    cache: &mut HeaderCache,
    seen: &mut HashSet<PathBuf>,
) -> Option<(String, TranscriptFile)> {
    let dir = ProjectDir::open(project_dir, projects)?;
    let earliest_ms = (process_start_secs as i64 - START_SLACK_SECS) * 1000;
    let not_before = SystemTime::UNIX_EPOCH
        + Duration::from_secs(process_start_secs.saturating_sub(START_SLACK_SECS as u64));
    let mut best: Option<(i64, String, TranscriptFile)> = None;
    let mut tied = false;
    for (name, modified) in dir.files() {
        let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".jsonl")) else {
            continue;
        };
        // A transcript written to since the process started has an mtime
        // after it; older files can't be this process's.
        if modified < not_before {
            continue;
        }
        let path = dir.path.join(&name);
        seen.insert(path.clone());
        let Some(file) = cache.get(&path, || dir.open_file(&name)) else {
            continue;
        };
        let header = &file.header;
        let matches = header.agent_name.as_deref() == Some(args.agent_name.as_str())
            && header.team_name.as_deref() == Some(args.team_name.as_str())
            && header.session_id.as_deref() == Some(stem)
            && header.started_ms >= earliest_ms;
        if !matches {
            continue;
        }
        match &best {
            Some((started, ..)) if *started > header.started_ms => {}
            Some((started, ..)) if *started == header.started_ms => tied = true,
            _ => {
                best = Some((header.started_ms, stem.to_string(), file.clone()));
                tied = false;
            }
        }
    }
    if tied {
        return None;
    }
    best.map(|(_, id, file)| (id, file))
}

/// Teammates run in tmux panes; there are none to find elsewhere.
#[cfg(not(unix))]
pub(crate) fn find_transcript(
    _project_dir: &Path,
    _projects: &Path,
    _args: &TeammateArgs,
    _process_start_secs: u64,
    _cache: &mut HeaderCache,
    _seen: &mut HashSet<PathBuf>,
) -> Option<(String, TranscriptFile)> {
    None
}

/// `path` opened, when it still names `file`: a regular file (not a
/// symlink) whose directory is under `projects`. Opened without blocking
/// on a FIFO. Read the transcript only through the returned descriptor; a
/// later open by name could reach another file.
#[cfg(unix)]
pub(crate) fn still_names(
    path: &Path,
    projects: &Path,
    file: &TranscriptFile,
) -> Option<std::fs::File> {
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    let root = projects.canonicalize().ok()?;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `c` is a valid NUL-terminated path.
    let raw = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return None;
    }
    // SAFETY: `raw` is a descriptor this function just opened.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let dir = fd_path(&fd)?.parent()?.to_path_buf();
    let mut opened = std::fs::File::from(fd);
    if transcript_file_of(&opened)? != *file || !dir.starts_with(&root) || dir == root {
        return None;
    }
    // Reading the header moved the offset; hand the file back rewound.
    std::io::Seek::rewind(&mut opened).ok()?;
    Some(opened)
}

#[cfg(not(unix))]
pub(crate) fn still_names(
    _path: &Path,
    _projects: &Path,
    _file: &TranscriptFile,
) -> Option<std::fs::File> {
    None
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
        let (link, cwds) = link_for(home.path(), &args(), Some(Path::new("/repo"))).unwrap();
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
    fn no_link_without_a_team_config_that_lists_the_agent_under_its_lead() {
        let home = tempfile::tempdir().unwrap();
        write_team_config(home.path(), "00000000-0000-0000-0000-000000000000");
        assert!(link_for(home.path(), &args(), Some(Path::new("/repo"))).is_none());
        // No team config at all.
        let empty = tempfile::tempdir().unwrap();
        assert!(link_for(empty.path(), &args(), None).is_none());
        // The config lists other members only.
        write_team_config(home.path(), LEAD);
        let mut stranger = args();
        stranger.agent_name = "stranger".into();
        assert!(link_for(home.path(), &stranger, None).is_none());
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

    /// `find_transcript` for `args()` in `dir`, under its parent as the
    /// projects root.
    fn find(dir: &Path, cache: &mut HeaderCache) -> Option<String> {
        let root = dir.parent().unwrap();
        find_transcript(dir, root, &args(), START, cache, &mut HashSet::new()).map(|(id, _)| id)
    }

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
        let found = find(dir.path(), &mut HeaderCache::default());
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
        let found = find(dir.path(), &mut HeaderCache::default());
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
        assert_eq!(find(dir.path(), &mut HeaderCache::default()), None);
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
        assert_eq!(find(dir.path(), &mut HeaderCache::default()), None);
        // A missing project dir.
        let gone = dir.path().join("missing");
        assert_eq!(find(&gone, &mut HeaderCache::default()), None);
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
        assert_eq!(find(dir.path(), &mut HeaderCache::default()), None);
    }

    #[test]
    fn a_transcript_with_no_entry_yet_is_read_again_later() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("{}.jsonl", id(1)));
        std::fs::write(&path, "{\"type\":\"mode\",\"mode\":\"normal\"}\n").unwrap();
        let mut cache = HeaderCache::default();
        assert_eq!(find(dir.path(), &mut cache), None);
        transcript(
            dir.path(),
            &id(1),
            Some("p10-multi-cwd"),
            "session-52d94f40",
            START as i64 + 5,
        );
        assert_eq!(find(dir.path(), &mut cache), Some(id(1)));
    }

    #[cfg(unix)]
    #[test]
    fn only_regular_files_under_the_projects_root_are_read() {
        let root = tempfile::tempdir().unwrap();
        let projects = root.path().join("projects");
        let dir = projects.join("-repo");
        std::fs::create_dir_all(&dir).unwrap();
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let start = START as i64 + 5;
        transcript(
            &outside,
            &id(1),
            Some("p10-multi-cwd"),
            "session-52d94f40",
            start,
        );
        // A symlink to a matching transcript elsewhere.
        std::os::unix::fs::symlink(
            outside.join(format!("{}.jsonl", id(1))),
            dir.join(format!("{}.jsonl", id(1))),
        )
        .unwrap();
        // A FIFO with a transcript's name: opening it must not block.
        let fifo = dir.join(format!("{}.jsonl", id(2)));
        let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: `c` is a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert_eq!(read_header(&fifo), None);
        let lookup = |project_dir: &Path| {
            find_transcript(
                project_dir,
                &projects,
                &args(),
                START,
                &mut HeaderCache::default(),
                &mut HashSet::new(),
            )
            .map(|(id, _)| id)
        };
        assert_eq!(lookup(&dir), None);
        // A project dir that is a symlink out of the projects root.
        let escape = projects.join("-escape");
        std::os::unix::fs::symlink(&outside, &escape).unwrap();
        assert_eq!(lookup(&escape), None);
        // A project dir outside the root.
        assert_eq!(lookup(&outside), None);
        // The same transcript as a regular file in the project dir is found.
        std::fs::remove_file(dir.join(format!("{}.jsonl", id(1)))).unwrap();
        transcript(
            &dir,
            &id(1),
            Some("p10-multi-cwd"),
            "session-52d94f40",
            start,
        );
        assert_eq!(lookup(&dir), Some(id(1)));
    }

    #[test]
    fn the_cache_keeps_what_any_lookup_of_the_pass_saw() {
        let dir = tempfile::tempdir().unwrap();
        transcript(
            dir.path(),
            &id(1),
            Some("p10-multi-cwd"),
            "session-52d94f40",
            START as i64 + 5,
        );
        let mut cache = HeaderCache::default();
        let mut seen = HashSet::new();
        let found = find_transcript(
            dir.path(),
            dir.path().parent().unwrap(),
            &args(),
            START,
            &mut cache,
            &mut seen,
        )
        .map(|(id, _)| id);
        assert_eq!(found, Some(id(1)));
        // A later process's lookup skips the file by mtime; the pass still
        // saw it, so its header stays.
        let later = START + 3600 * 24 * 365 * 10;
        find_transcript(
            dir.path(),
            dir.path().parent().unwrap(),
            &args(),
            later,
            &mut cache,
            &mut seen,
        );
        cache.retain(&seen);
        assert_eq!(cache.0.len(), 1);
        cache.retain(&HashSet::new());
        assert!(cache.0.is_empty());
    }

    /// Codex r3: selection hands enrichment the file it matched, and
    /// enrichment reads by path only while the path still names that file.
    /// A dir swapped for a symlink out of the root, a replaced file and a
    /// FIFO all fail, and the check never blocks.
    #[cfg(unix)]
    #[test]
    fn enrichment_reads_only_the_file_selection_matched() {
        let root = tempfile::tempdir().unwrap();
        let projects = root.path().join("projects");
        let dir = projects.join("-repo");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let name = format!("{}.jsonl", id(1));
        transcript(
            &dir,
            &id(1),
            Some("p10-multi-cwd"),
            "session-52d94f40",
            START as i64 + 5,
        );
        let (found, file) = find_transcript(
            &dir,
            &projects,
            &args(),
            START,
            &mut HeaderCache::default(),
            &mut HashSet::new(),
        )
        .unwrap();
        assert_eq!(found, id(1));
        let path = dir.join(&name);
        assert!(still_names(&path, &projects, &file).is_some());
        // The dir swapped for a symlink to an outside copy.
        std::fs::copy(&path, outside.join(&name)).unwrap();
        std::fs::rename(&dir, projects.join("-moved")).unwrap();
        std::os::unix::fs::symlink(&outside, &dir).unwrap();
        assert!(still_names(&path, &projects, &file).is_none());
        // Even the original file, reached from outside the root.
        std::fs::remove_file(outside.join(&name)).unwrap();
        std::fs::hard_link(projects.join("-moved").join(&name), outside.join(&name)).unwrap();
        assert!(still_names(&path, &projects, &file).is_none());
        // Back in place: still the same file.
        std::fs::remove_file(&dir).unwrap();
        std::fs::rename(projects.join("-moved"), &dir).unwrap();
        assert!(still_names(&path, &projects, &file).is_some());
        // The file replaced by another under its name.
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, "{}").unwrap();
        assert!(still_names(&path, &projects, &file).is_none());
        // A FIFO under its name: refused without blocking.
        std::fs::remove_file(&path).unwrap();
        let c = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        // SAFETY: `c` is a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert!(still_names(&path, &projects, &file).is_none());
        assert!(crate::session::cache::open_regular(&path).is_err());
    }

    /// Codex r4: after the check, the transcript is read only through the
    /// descriptor it returned. Swapping the file for a symlink to an
    /// outside transcript changes nothing that is read, and a reopen by
    /// name refuses the symlink.
    #[cfg(unix)]
    #[test]
    fn reads_after_the_check_stay_on_the_checked_file() {
        use std::io::Read;
        let root = tempfile::tempdir().unwrap();
        let projects = root.path().join("projects");
        let dir = projects.join("-repo");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        transcript(
            &dir,
            &id(1),
            Some("p10-multi-cwd"),
            "session-52d94f40",
            START as i64 + 5,
        );
        let path = dir.join(format!("{}.jsonl", id(1)));
        let (_, file) = find_transcript(
            &dir,
            &projects,
            &args(),
            START,
            &mut HeaderCache::default(),
            &mut HashSet::new(),
        )
        .unwrap();
        let pinned = still_names(&path, &projects, &file).unwrap();
        let secret = outside.join("secret.jsonl");
        std::fs::write(&secret, "OUTSIDE-CONVERSATION\n").unwrap();
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&secret, &path).unwrap();
        let mut text = String::new();
        (&pinned).read_to_string(&mut text).unwrap();
        assert!(
            text.contains("p10-multi-cwd") && !text.contains("OUTSIDE"),
            "{text}"
        );
        assert!(crate::session::cache::open_regular(&path).is_err());
        assert!(still_names(&path, &projects, &file).is_none());
    }

    /// Codex r4: a cached header belongs to the file it was read from. A
    /// path atomically replaced by an unrelated transcript is read again,
    /// so the old header is never paired with the new file.
    #[cfg(unix)]
    #[test]
    fn a_replaced_file_is_read_again_not_served_from_the_cache() {
        let root = tempfile::tempdir().unwrap();
        let projects = root.path().join("projects");
        let dir = projects.join("-repo");
        std::fs::create_dir_all(&dir).unwrap();
        transcript(
            &dir,
            &id(1),
            Some("p10-multi-cwd"),
            "session-52d94f40",
            START as i64 + 5,
        );
        let mut cache = HeaderCache::default();
        let lookup = |cache: &mut HeaderCache| {
            find_transcript(&dir, &projects, &args(), START, cache, &mut HashSet::new())
        };
        let (found, before) = lookup(&mut cache).unwrap();
        assert_eq!(found, id(1));
        // Another agent's transcript renamed over the same name.
        let staging = root.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        transcript(
            &staging,
            &id(1),
            Some("someone-else"),
            "session-52d94f40",
            START as i64 + 6,
        );
        std::fs::rename(
            staging.join(format!("{}.jsonl", id(1))),
            dir.join(format!("{}.jsonl", id(1))),
        )
        .unwrap();
        assert_eq!(lookup(&mut cache), None);
        // A replacement that does match is found with its own identity.
        transcript(
            &staging,
            &id(1),
            Some("p10-multi-cwd"),
            "session-52d94f40",
            START as i64 + 7,
        );
        std::fs::rename(
            staging.join(format!("{}.jsonl", id(1))),
            dir.join(format!("{}.jsonl", id(1))),
        )
        .unwrap();
        // Read again, not served from the cache: the header is the new
        // file's, whatever inode the file system gave it.
        let (_, after) = lookup(&mut cache).unwrap();
        assert_eq!(after.header.started_ms, (START as i64 + 7) * 1000 + 155);
        assert_ne!(after.header, before.header);
    }

    /// The directory is read through the descriptor opened when it was
    /// checked: replacing the path with a symlink afterwards, or a file
    /// with a symlink, does not redirect a read.
    #[cfg(unix)]
    #[test]
    fn a_swap_after_the_check_does_not_redirect_reads() {
        let root = tempfile::tempdir().unwrap();
        let projects = root.path().join("projects");
        let dir = projects.join("-repo");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(dir.join("a.jsonl"), "inside").unwrap();
        std::fs::write(dir.join("b.jsonl"), "inside").unwrap();
        std::fs::write(outside.join("a.jsonl"), "outside").unwrap();
        let opened = ProjectDir::open(&dir, &projects).unwrap();
        let names: HashSet<_> = opened.files().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, HashSet::from(["a.jsonl".into(), "b.jsonl".into()]));
        // Swap the directory for a symlink out of the root.
        std::fs::rename(&dir, projects.join("-moved")).unwrap();
        std::os::unix::fs::symlink(&outside, &dir).unwrap();
        let mut text = String::new();
        opened
            .open_file("a.jsonl".as_ref())
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "inside");
        // And a file swapped for a symlink is not opened.
        let moved = projects.join("-moved");
        std::fs::remove_file(moved.join("b.jsonl")).unwrap();
        std::os::unix::fs::symlink(outside.join("a.jsonl"), moved.join("b.jsonl")).unwrap();
        assert!(opened.open_file("b.jsonl".as_ref()).is_none());
        // Opening the swapped path now fails: it is a symlink.
        assert!(ProjectDir::open(&dir, &projects).is_none());
    }
}
