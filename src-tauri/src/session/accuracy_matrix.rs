//! Status accuracy matrix: source × scenario → what the dashboard shows.
//!
//! Each cell builds a tempdir fixture for one source in one scenario, runs it
//! through the real detector and `enrich_detected_sessions`, and checks the
//! session the scenario is about: listed with a [`SessionStatus`], or not
//! listed. Process tables (`claude agents --json` output, pid liveness, start
//! times, Pi's process probe) are injected; nothing reads the real
//! `~/.claude`, `~/.codex`, `~/.cursor` or `~/.pi` session data. Timestamps
//! are written relative to the real clock, since the status heuristics read
//! `Utc::now()` themselves.
//!
//! `MATRIX` is the single source of truth. `docs/accuracy-matrix.md` holds a
//! rendering of it, and `docs_table_matches_matrix` fails when they drift.
//!
//! # Adding or changing a cell
//!
//! 1. Edit the `Cell` in `MATRIX` (`expect` is the behaviour we want, not
//!    what the code does today).
//! 2. If the code can't do it yet, set `gap` to the reason and mark the
//!    cell's test `#[ignore = "route1-B: <same reason>"]`. To close a gap,
//!    set `gap: None` and remove the `#[ignore]`.
//! 3. Make `observe` build the fixture for that source and scenario.
//! 4. Add (or un-ignore) the one-line `#[test]` for the cell at the bottom.
//! 5. Run `cargo test docs_table_matches_matrix`; it prints the table to
//!    paste into `docs/accuracy-matrix.md` when they differ.
//!
//! A new source or scenario is a new enum variant plus one cell per
//! scenario or source; `matrix_has_one_cell_per_source_and_scenario` checks
//! the grid is complete.

use super::agent_liveness::{OpenFileEvidence, OpenFileProbe};
use super::detector::encode_path_for_matching;
use super::enrichment::enrich_detected_sessions;
use super::pi_liveness::{PiProcessEvidence, PiProcessProbe};
use super::source::{AgentKind, DetectedSession, DetectionDiagnostics, SessionSource};
use super::SessionStatus;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    ClaudeCli,
    ClaudeLegacy,
    Codex,
    Cursor,
    Pi,
}

const SOURCES: [Source; 5] = [
    Source::ClaudeCli,
    Source::ClaudeLegacy,
    Source::Codex,
    Source::Cursor,
    Source::Pi,
];

impl Source {
    fn label(self) -> &'static str {
        match self {
            Source::ClaudeCli => "Claude Code (CLI backend)",
            Source::ClaudeLegacy => "Claude Code (legacy backend)",
            Source::Codex => "Codex",
            Source::Cursor => "Cursor",
            Source::Pi => "Pi",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scenario {
    /// The agent is mid-turn.
    Working,
    /// The agent waits on the user: a permission prompt, or the CLI's
    /// `waiting` status.
    Waiting,
    /// The turn ended; the agent waits for the next prompt.
    Idle,
    /// The process is gone (hard kill) but its files remain.
    Killed,
    /// The process is gone and a newer process now holds its pid (or, for
    /// sources without pids, its place).
    PidReuse,
    /// A subagent spawned by a working root session. The cell checks the
    /// subagent's own row; the root must be listed in every case.
    Subagent,
}

const SCENARIOS: [Scenario; 6] = [
    Scenario::Working,
    Scenario::Waiting,
    Scenario::Idle,
    Scenario::Killed,
    Scenario::PidReuse,
    Scenario::Subagent,
];

impl Scenario {
    fn label(self) -> &'static str {
        match self {
            Scenario::Working => "working",
            Scenario::Waiting => "waiting",
            Scenario::Idle => "idle",
            Scenario::Killed => "killed",
            Scenario::PidReuse => "pid reuse",
            Scenario::Subagent => "subagent",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Expect {
    Listed(SessionStatus),
    /// No row for the session. For a Claude Code subagent this is correct:
    /// it shows in its parent's subagent panel, not as its own row.
    Hidden,
    /// The source has no such situation; the cell has no test.
    NotApplicable(&'static str),
}

struct Cell {
    source: Source,
    scenario: Scenario,
    expect: Expect,
    /// Why the code can't meet `expect` yet; the cell's test is ignored.
    gap: Option<&'static str>,
    /// The cell passes, but the detection is a heuristic with these known
    /// errors.
    heuristic: Option<&'static str>,
}

const fn cell(source: Source, scenario: Scenario, expect: Expect) -> Cell {
    Cell {
        source,
        scenario,
        expect,
        gap: None,
        heuristic: None,
    }
}

const fn gap(source: Source, scenario: Scenario, expect: Expect, why: &'static str) -> Cell {
    Cell {
        source,
        scenario,
        expect,
        gap: Some(why),
        heuristic: None,
    }
}

const fn heuristic(
    source: Source,
    scenario: Scenario,
    expect: Expect,
    errors: &'static str,
) -> Cell {
    Cell {
        source,
        scenario,
        expect,
        gap: None,
        heuristic: Some(errors),
    }
}

const fn not_applicable(source: Source, scenario: Scenario, why: &'static str) -> Cell {
    Cell {
        source,
        scenario,
        expect: Expect::NotApplicable(why),
        gap: None,
        heuristic: None,
    }
}

use Expect::{Hidden, Listed};
use Scenario as Sc;
use SessionStatus::{NeedsAttention, WaitingForInput, Working};
use Source as So;

const CODEX_APPROVAL_HEURISTIC: &str = "an escalated tool call with no later record for 35 s \
(`CODEX_ESCALATION_APPROVAL_DELAY_MS`) counts as waiting for approval. Known errors: (a) after \
the user approves, approval delay + the command's 30 s yield can pass 35 s and show \
NeedsAttention for a short time; (b) approvals without a marker (`apply_patch` outside the \
writable roots, untrusted-mode prompts) stay Working. `request_user_input` is exact. See \
docs/research/codex-approval-signal-2026-10.md";

const MATRIX: &[Cell] = &[
    cell(So::ClaudeCli, Sc::Working, Listed(Working)),
    cell(So::ClaudeCli, Sc::Waiting, Listed(NeedsAttention)),
    cell(So::ClaudeCli, Sc::Idle, Listed(WaitingForInput)),
    cell(So::ClaudeCli, Sc::Killed, Hidden),
    cell(So::ClaudeCli, Sc::PidReuse, Hidden),
    cell(So::ClaudeCli, Sc::Subagent, Hidden),
    cell(So::ClaudeLegacy, Sc::Working, Listed(Working)),
    cell(So::ClaudeLegacy, Sc::Waiting, Listed(NeedsAttention)),
    cell(So::ClaudeLegacy, Sc::Idle, Listed(WaitingForInput)),
    cell(So::ClaudeLegacy, Sc::Killed, Hidden),
    cell(So::ClaudeLegacy, Sc::PidReuse, Hidden),
    cell(So::ClaudeLegacy, Sc::Subagent, Hidden),
    cell(So::Codex, Sc::Working, Listed(Working)),
    heuristic(
        So::Codex,
        Sc::Waiting,
        Listed(NeedsAttention),
        CODEX_APPROVAL_HEURISTIC,
    ),
    cell(So::Codex, Sc::Idle, Listed(WaitingForInput)),
    cell(So::Codex, Sc::Killed, Hidden),
    cell(So::Codex, Sc::PidReuse, Hidden),
    cell(So::Codex, Sc::Subagent, Listed(Working)),
    cell(So::Cursor, Sc::Working, Listed(Working)),
    gap(
        So::Cursor,
        Sc::Waiting,
        Listed(NeedsAttention),
        "no approval signal is known in the Cursor transcript (records are role messages and \
`turn_ended`); not yet checked whether a pending `tool_use` is written before the prompt",
    ),
    cell(So::Cursor, Sc::Idle, Listed(WaitingForInput)),
    cell(So::Cursor, Sc::Killed, Hidden),
    cell(So::Cursor, Sc::PidReuse, Hidden),
    cell(So::Cursor, Sc::Subagent, Listed(Working)),
    cell(So::Pi, Sc::Working, Listed(Working)),
    not_applicable(
        So::Pi,
        Sc::Waiting,
        "Pi 0.87.1 has no approval step: tools run with the process's OS permissions \
(pi-coding-agent docs/security.md:33, docs/how-pi-works.md:49), and the transcript has only \
user, assistant and toolResult messages (pi.rs:8-10). Only an extension can ask the user \
(docs/extensions.md:7); c9watch does not model extensions",
    ),
    cell(So::Pi, Sc::Idle, Listed(WaitingForInput)),
    cell(So::Pi, Sc::Killed, Hidden),
    cell(So::Pi, Sc::PidReuse, Hidden),
    not_applicable(
        So::Pi,
        Sc::Subagent,
        "Pi 0.87.1 has no built-in subagents (none in its docs); every Pi session is a root \
(pi.rs:317)",
    ),
];

fn lookup(source: Source, scenario: Scenario) -> &'static Cell {
    MATRIX
        .iter()
        .find(|c| c.source == source && c.scenario == scenario)
        .unwrap_or_else(|| panic!("no cell for {source:?} × {scenario:?}"))
}

/// Runs one cell and asserts its expectation.
fn check(source: Source, scenario: Scenario) {
    let cell = lookup(source, scenario);
    assert!(
        !matches!(cell.expect, Expect::NotApplicable(_)),
        "{source:?} × {scenario:?} is not applicable; it has no test"
    );
    let observed = observe(source, scenario);
    assert_eq!(
        observed,
        cell.expect,
        "{} × {}: dashboard shows {observed:?}, expected {:?}",
        source.label(),
        scenario.label(),
        cell.expect
    );
}

/// What the dashboard shows for the scenario's session.
fn observe(source: Source, scenario: Scenario) -> Expect {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = match source {
        So::ClaudeCli => claude_cli(tmp.path(), scenario),
        So::ClaudeLegacy => claude_legacy(tmp.path(), scenario),
        So::Codex => codex(tmp.path(), scenario),
        So::Cursor => cursor(tmp.path(), scenario),
        So::Pi => pi(tmp.path(), scenario),
    };
    let (sessions, _) =
        enrich_detected_sessions(fixture.detected, DetectionDiagnostics::default()).unwrap();
    if let Some(root) = &fixture.root_id {
        assert!(
            sessions.iter().any(|s| &s.id == root),
            "{} × {}: the subagent's root session must stay listed",
            source.label(),
            scenario.label()
        );
    }
    match sessions.iter().find(|s| s.id == fixture.subject_id) {
        Some(session) => {
            if scenario == Sc::Subagent {
                assert_eq!(session.agent_kind, AgentKind::Subagent);
            }
            Listed(session.status.clone())
        }
        None => Hidden,
    }
}

struct Fixture {
    detected: Vec<DetectedSession>,
    /// The session the scenario is about.
    subject_id: String,
    /// For `Subagent`: the root session that must stay listed.
    root_id: Option<String>,
}

fn now_rfc3339(ago_secs: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::seconds(ago_secs)).to_rfc3339()
}

fn set_age(path: &Path, age: Duration) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::now() - age)
        .unwrap();
}

fn write_lines(path: &Path, lines: &[String]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, lines.join("\n") + "\n").unwrap();
}

// ── Claude Code ─────────────────────────────────────────────────────

const CLAUDE_SID: &str = "a11ce000-0000-4000-8000-00000000c1a0";
const CLAUDE_SUBAGENT_ID: &str = "agent-a11ce0000000c1a1";
const CLAUDE_PID: u32 = 4_100_041;
/// The Claude Code process started this long ago.
const CLAUDE_UPTIME_SECS: i64 = 2 * 60 * 60;

fn claude_cwd(scenario: Scenario) -> PathBuf {
    PathBuf::from(format!(
        "/fixture/accuracy-matrix/{}",
        scenario.label().replace(' ', "-")
    ))
}

/// Writes the session transcript under `claude_dir/projects/` and returns it.
fn write_claude_transcript(claude_dir: &Path, cwd: &Path, scenario: Scenario) -> PathBuf {
    let project = claude_dir
        .join("projects")
        .join(encode_path_for_matching(&cwd.to_string_lossy()));
    let transcript = project.join(format!("{CLAUDE_SID}.jsonl"));
    let user = |ago: i64, text: &str| {
        serde_json::json!({
            "type": "user", "uuid": format!("u{ago}"), "timestamp": now_rfc3339(ago),
            "sessionId": CLAUDE_SID,
            "message": {"role": "user", "content": text},
        })
        .to_string()
    };
    let lines = match scenario {
        Sc::Waiting => vec![
            user(60, "run the fixture tool"),
            serde_json::json!({
                "type": "assistant", "uuid": "a1", "timestamp": now_rfc3339(50),
                "sessionId": CLAUDE_SID,
                "message": {"role": "assistant", "model": "m", "id": "msg_wait",
                    "stop_reason": "tool_use", "content": [{
                    // No user setting can allow a tool by this name, so the
                    // call waits for a permission prompt.
                    "type": "tool_use", "id": "toolu_matrix", "name": "AccuracyMatrixFixtureTool",
                    "input": {},
                }]},
            })
            .to_string(),
        ],
        Sc::Idle => vec![
            user(3600, "say hi"),
            serde_json::json!({
                "type": "assistant", "uuid": "a1", "timestamp": now_rfc3339(3590),
                "sessionId": CLAUDE_SID,
                "message": {"role": "assistant", "model": "m", "id": "msg_idle",
                    "stop_reason": "end_turn",
                    "content": [{"type": "text", "text": "hi"}]},
            })
            .to_string(),
        ],
        _ => vec![user(1, "keep working")],
    };
    write_lines(&transcript, &lines);
    if scenario == Sc::Idle {
        set_age(&transcript, Duration::from_secs(3600));
    }
    if scenario == Sc::Subagent {
        write_lines(
            &project
                .join(CLAUDE_SID)
                .join("subagents")
                .join(format!("{CLAUDE_SUBAGENT_ID}.jsonl")),
            &[user(1, "explore the codebase")],
        );
    }
    transcript
}

/// `~/.claude/sessions/<pid>.json` as current Claude Code writes it.
fn write_claude_registry(claude_dir: &Path, cwd: &Path, status: &str) {
    let start = chrono::Utc::now() - chrono::Duration::seconds(CLAUDE_UPTIME_SECS);
    let sessions = claude_dir.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join(format!("{CLAUDE_PID}.json")),
        serde_json::json!({
            "pid": CLAUDE_PID, "sessionId": CLAUDE_SID, "cwd": cwd,
            "startedAt": start.timestamp_millis() + 2000,
            "procStart": start.format("%a %b %e %H:%M:%S %Y").to_string(),
            "pidDomain": "darwin", "kind": "interactive", "entrypoint": "cli",
            "version": "2.1.299", "status": status,
        })
        .to_string(),
    )
    .unwrap();
}

/// Start time of the process now holding `CLAUDE_PID`: the original Claude
/// Code, or for `PidReuse` a process started a minute ago.
fn claude_actual_start(scenario: Scenario) -> u64 {
    let ago = if scenario == Sc::PidReuse {
        60
    } else {
        CLAUDE_UPTIME_SECS
    };
    (chrono::Utc::now().timestamp() - ago) as u64
}

fn claude_subject(scenario: Scenario) -> (String, Option<String>) {
    if scenario == Sc::Subagent {
        (CLAUDE_SUBAGENT_ID.into(), Some(CLAUDE_SID.into()))
    } else {
        (CLAUDE_SID.into(), None)
    }
}

#[cfg(unix)]
fn claude_cli(tmp: &Path, scenario: Scenario) -> Fixture {
    use super::detector_cli::test_support::{fixture_cli, with_process_table};
    let home = tmp.join("home");
    let claude_dir = home.join(".claude");
    let cwd = claude_cwd(scenario);
    let status = match scenario {
        Sc::Waiting => "waiting",
        Sc::Idle => "idle",
        _ => "busy",
    };
    write_claude_transcript(&claude_dir, &cwd, scenario);
    write_claude_registry(&claude_dir, &cwd, status);
    let started_at = (chrono::Utc::now().timestamp() - CLAUDE_UPTIME_SECS) * 1000 + 2000;
    std::fs::write(
        tmp.join("cli-output.json"),
        serde_json::json!([{
            "pid": CLAUDE_PID, "cwd": cwd, "kind": "interactive", "startedAt": started_at,
            "sessionId": CLAUDE_SID, "status": status,
        }])
        .to_string()
            + "\n",
    )
    .unwrap();
    let alive: fn(u32) -> bool = if scenario == Sc::Killed {
        |_| false
    } else {
        |_| true
    };
    let actual_start = claude_actual_start(scenario);
    let mut source = with_process_table(fixture_cli(tmp), home, alive, move |pids| {
        pids.iter().map(|&pid| (pid, actual_start)).collect()
    });
    let (detected, _) = source.detect().unwrap();
    let (subject_id, root_id) = claude_subject(scenario);
    Fixture {
        detected,
        subject_id,
        root_id,
    }
}

#[cfg(not(unix))]
fn claude_cli(_tmp: &Path, _scenario: Scenario) -> Fixture {
    unimplemented!("the fake `claude` binary is a shell script")
}

fn claude_legacy(tmp: &Path, scenario: Scenario) -> Fixture {
    let claude_dir = tmp.join(".claude");
    let cwd = claude_cwd(scenario);
    write_claude_transcript(&claude_dir, &cwd, scenario);
    write_claude_registry(&claude_dir, &cwd, "busy");
    let processes = if scenario == Sc::Killed {
        vec![]
    } else {
        vec![(CLAUDE_PID, cwd, claude_actual_start(scenario))]
    };
    let detected = super::detector::test_support::detect_with_processes(&claude_dir, &processes);
    let (subject_id, root_id) = claude_subject(scenario);
    Fixture {
        detected,
        subject_id,
        root_id,
    }
}

// ── Codex ───────────────────────────────────────────────────────────

const CODEX_ROOT: &str = "019f0000-0000-7000-8000-00000000c0de";
const CODEX_CHILD: &str = "019f0000-0000-7000-8000-00000000c0d1";

fn codex_event(event_type: &str, payload: serde_json::Value) -> String {
    serde_json::json!({"timestamp": now_rfc3339(5), "type": event_type, "payload": payload})
        .to_string()
}

fn codex_rollout(day: &Path, id: &str, meta: serde_json::Value, events: &[&str]) {
    let mut lines = vec![
        codex_event("session_meta", meta),
        codex_event(
            "event_msg",
            serde_json::json!({"type": "user_message", "message": "hello"}),
        ),
    ];
    lines.extend(
        events
            .iter()
            .map(|kind| codex_event("event_msg", serde_json::json!({"type": kind}))),
    );
    write_lines(
        &day.join(format!("rollout-2026-10-14T00-00-00-{id}.jsonl")),
        &lines,
    );
}

/// Open rollouts that injected `codex` processes hold.
struct FixedOpenFiles(OpenFileEvidence);

impl OpenFileProbe for FixedOpenFiles {
    fn scan(&mut self) -> OpenFileEvidence {
        self.0.clone()
    }
}

fn codex_rollout_name(id: &str) -> String {
    format!("rollout-2026-10-14T00-00-00-{id}.jsonl")
}

fn codex(tmp: &Path, scenario: Scenario) -> Fixture {
    let day = tmp.join(chrono::Local::now().format("%Y/%m/%d").to_string());
    let root_meta = serde_json::json!({
        "id": CODEX_ROOT, "cwd": "/tmp/accuracy-matrix-codex", "source": "cli",
        "originator": "codex-tui",
    });
    let events: &[&str] = match scenario {
        Sc::Idle => &["task_started", "task_complete"],
        // Killed / PidReuse: the rollout is left mid-turn.
        _ => &["task_started"],
    };
    codex_rollout(&day, CODEX_ROOT, root_meta, events);
    let root_path = day.join(codex_rollout_name(CODEX_ROOT));
    if scenario == Sc::Waiting {
        // An escalated command, written 60 s ago, with no output yet: the
        // approval dialog is open.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&root_path)
            .unwrap();
        // Codex writes `timestamp`, `type`, `payload` in this order, and the
        // monitor scanner relies on it (`json!` would sort the keys).
        let payload = serde_json::json!({"type": "function_call", "call_id": "call_matrix",
            "name": "exec_command",
            "arguments": r#"{"cmd":"touch probe.txt","sandbox_permissions":"require_escalated"}"#});
        let call = format!(
            r#"{{"timestamp":"{}","type":"response_item","payload":{payload}}}"#,
            now_rfc3339(60)
        );
        std::io::Write::write_all(&mut file, format!("{call}\n").as_bytes()).unwrap();
    }
    if scenario == Sc::Subagent {
        codex_rollout(
            &day,
            CODEX_CHILD,
            serde_json::json!({
                "id": CODEX_CHILD, "cwd": "/tmp/accuracy-matrix-codex",
                "originator": "codex-tui", "session_id": CODEX_ROOT,
                "source": {"subagent": {"thread_spawn": {
                    "parent_thread_id": CODEX_ROOT, "agent_nickname": "Ada",
                    "agent_role": "explorer",
                }}},
            }),
            &["task_started"],
        );
    }
    // The root rollout was last written a minute ago.
    set_age(&root_path, Duration::from_secs(60));
    let open = match scenario {
        // No `codex` process holds the rollout any more.
        Sc::Killed => HashSet::new(),
        // A new `codex` in the same cwd (possibly on the old pid) holds its
        // own rollout.
        Sc::PidReuse => HashSet::from([codex_rollout_name("019f0000-0000-7000-8000-0000000000ff")]),
        _ => HashSet::from([codex_rollout_name(CODEX_ROOT)]),
    };
    let mut source = super::codex::CodexSessionSource::at_root(tmp.to_path_buf())
        .with_process_probe(Box::new(FixedOpenFiles(OpenFileEvidence::Open(open))));
    let (detected, _) = source.detect().unwrap();
    subject_fixture(detected, scenario, CODEX_ROOT, CODEX_CHILD)
}

fn subject_fixture(
    detected: Vec<DetectedSession>,
    scenario: Scenario,
    root: &str,
    child: &str,
) -> Fixture {
    let (subject_id, root_id) = if scenario == Sc::Subagent {
        (child.to_string(), Some(root.to_string()))
    } else {
        (root.to_string(), None)
    };
    Fixture {
        detected,
        subject_id,
        root_id,
    }
}

// ── Cursor ──────────────────────────────────────────────────────────

const CURSOR_ROOT: &str = "c0000000-0000-4000-8000-00000000c0e5";
const CURSOR_CHILD: &str = "c0000000-0000-4000-8000-00000000c0e6";

fn cursor_user(text: &str) -> String {
    serde_json::json!({"role": "user", "message": {"content": [{"type": "text",
        "text": format!("<user_query>\n{text}\n</user_query>")}]}})
    .to_string()
}

fn cursor_assistant(text: &str) -> String {
    serde_json::json!({"role": "assistant", "message": {"content": [{"type": "text", "text": text}]}})
        .to_string()
}

fn cursor(tmp: &Path, scenario: Scenario) -> Fixture {
    let session_dir = tmp
        .join("accuracy-matrix-cursor")
        .join("agent-transcripts")
        .join(CURSOR_ROOT);
    let mut lines = vec![cursor_user("fix it"), cursor_assistant("on it")];
    match scenario {
        Sc::Idle => lines.push(r#"{"type":"turn_ended","status":"success"}"#.into()),
        Sc::Waiting => panic!("route1-B: no Cursor approval signal is known yet"),
        _ => {}
    }
    let transcript = session_dir.join(format!("{CURSOR_ROOT}.jsonl"));
    write_lines(&transcript, &lines);
    // The transcript was last written a minute ago.
    set_age(&transcript, Duration::from_secs(60));
    // An Agent CLI chat: it has a chat store, `chats/<md5(cwd)>/<id>/`.
    let chats = tmp.join("cli-chats");
    std::fs::create_dir_all(
        chats
            .join("0b5e5d5b0ec0f4f2f7f5a1c3b0d0c0de")
            .join(CURSOR_ROOT),
    )
    .unwrap();
    if scenario == Sc::Subagent {
        write_lines(
            &session_dir
                .join("subagents")
                .join(format!("{CURSOR_CHILD}.jsonl")),
            &[cursor_user("explore"), cursor_assistant("looking")],
        );
    }
    let open: HashSet<String> = match scenario {
        // No Agent CLI process holds the chat store any more.
        Sc::Killed => HashSet::new(),
        // A new Agent CLI in the same cwd (possibly on the old pid) holds
        // its own chat.
        Sc::PidReuse => HashSet::from(["c0000000-0000-4000-8000-0000000000ff".to_string()]),
        _ => HashSet::from([CURSOR_ROOT.to_string()]),
    };
    let mut source = super::cursor::CursorSessionSource::at_root(tmp.to_path_buf())
        .with_process_probe(
            Box::new(FixedOpenFiles(OpenFileEvidence::Open(open))),
            chats,
        );
    let (detected, _) = source.detect().unwrap();
    subject_fixture(detected, scenario, CURSOR_ROOT, CURSOR_CHILD)
}

// ── Pi ──────────────────────────────────────────────────────────────

const PI_ID: &str = "01a06659-0000-4000-8000-0000000000e1";
const PI_CWD: &str = "/tmp/accuracy-matrix-pi";

struct FixedProbe(PiProcessEvidence);

impl PiProcessProbe for FixedProbe {
    fn scan(&mut self) -> PiProcessEvidence {
        self.0.clone()
    }
}

fn pi(tmp: &Path, scenario: Scenario) -> Fixture {
    let root = tmp.join(".pi").join("agent").join("sessions");
    let now_ms = chrono::Utc::now().timestamp_millis();
    let header = serde_json::json!({"type": "session",
        "session": {"id": PI_ID, "timestamp": now_ms - 600_000, "cwd": PI_CWD}});
    let user = serde_json::json!({"id": "m1", "timestamp": now_ms - 590_000, "type": "message",
        "message": {"role": "user", "content": [{"type": "text", "text": "fix it"}]}});
    let reply = match scenario {
        Sc::Idle => serde_json::json!({"id": "m2", "timestamp": now_ms - 580_000,
            "type": "message", "message": {"role": "assistant", "model": "m",
            "content": [{"type": "text", "text": "Done."}], "stopReason": "stop"}}),
        Sc::Waiting | Sc::Subagent => unreachable!("not applicable to Pi"),
        // Mid-tool: the toolCall has no result yet.
        _ => serde_json::json!({"id": "m2", "timestamp": now_ms - 580_000,
            "type": "message", "message": {"role": "assistant", "model": "m",
            "content": [{"type": "toolCall", "id": "c1", "name": "bash", "arguments": {}}]}}),
    };
    let transcript = root
        .join("--tmp-accuracy-matrix-pi--")
        .join(format!("2026-10-14T00-00-00-000Z_{PI_ID}.jsonl"));
    write_lines(
        &transcript,
        &[header.to_string(), user.to_string(), reply.to_string()],
    );
    // The transcript was last written a minute ago.
    set_age(&transcript, Duration::from_secs(60));
    let live: Vec<Option<i64>> = match scenario {
        Sc::Killed => vec![],
        // A new pi in the same cwd, started after the last write, cannot
        // be the process that wrote it.
        Sc::PidReuse => vec![Some(now_ms - 5_000)],
        _ => vec![Some(now_ms - 3_600_000)],
    };
    let evidence = PiProcessEvidence::Live(HashMap::from([(PI_CWD.to_string(), live)]));
    let mut source = super::pi::PiSessionSource::at_root(root)
        .with_process_probe(Box::new(FixedProbe(evidence)));
    let (detected, _) = source.detect().unwrap();
    Fixture {
        detected,
        subject_id: PI_ID.into(),
        root_id: None,
    }
}

// ── Table rendering ─────────────────────────────────────────────────

fn render_cell(cell: &Cell) -> String {
    let expect = match &cell.expect {
        Listed(status) => format!("{status:?}"),
        Hidden => "hidden".to_string(),
        Expect::NotApplicable(_) => return "➖ n/a".to_string(),
    };
    match (cell.gap, cell.heuristic) {
        (Some(_), _) => format!("❌ gap (want {expect})"),
        (None, Some(_)) => format!("⚠️ {expect} (heuristic)"),
        (None, None) => format!("✅ {expect}"),
    }
}

fn render_table() -> String {
    let mut out = String::from("| Source |");
    for scenario in SCENARIOS {
        out += &format!(" {} |", scenario.label());
    }
    out += "\n|---|";
    out += &"---|".repeat(SCENARIOS.len());
    out += "\n";
    for source in SOURCES {
        out += &format!("| {} |", source.label());
        for scenario in SCENARIOS {
            out += &format!(" {} |", render_cell(lookup(source, scenario)));
        }
        out += "\n";
    }
    let section = |out: &mut String, title: &str, why: fn(&Cell) -> Option<&'static str>| {
        let cells: Vec<&Cell> = MATRIX.iter().filter(|c| why(c).is_some()).collect();
        if cells.is_empty() {
            return;
        }
        *out += &format!("\n{title}\n\n");
        for cell in cells {
            *out += &format!(
                "- {} × {}: {}\n",
                cell.source.label(),
                cell.scenario.label(),
                why(cell).unwrap()
            );
        }
    };
    section(
        &mut out,
        "Gaps (each is an `#[ignore = \"route1-B: …\"]` test):",
        |c| c.gap,
    );
    section(
        &mut out,
        "Heuristic cells (the test passes; known errors):",
        |c| c.heuristic,
    );
    section(&mut out, "Not applicable (no test):", |c| match c.expect {
        Expect::NotApplicable(why) => Some(why),
        _ => None,
    });
    out
}

const DOC_START: &str = "<!-- accuracy-matrix:start -->\n";
const DOC_END: &str = "<!-- accuracy-matrix:end -->";

#[test]
fn matrix_has_one_cell_per_source_and_scenario() {
    assert_eq!(MATRIX.len(), SOURCES.len() * SCENARIOS.len());
    for source in SOURCES {
        for scenario in SCENARIOS {
            let count = MATRIX
                .iter()
                .filter(|c| c.source == source && c.scenario == scenario)
                .count();
            assert_eq!(count, 1, "{source:?} × {scenario:?}");
        }
    }
}

#[test]
fn docs_table_matches_matrix() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/accuracy-matrix.md");
    let doc = std::fs::read_to_string(&path).unwrap();
    let start = doc.find(DOC_START).expect("start marker") + DOC_START.len();
    let end = doc.find(DOC_END).expect("end marker");
    let expected = render_table();
    assert!(
        doc[start..end] == expected,
        "docs/accuracy-matrix.md is out of date; replace the text between the markers with:\n\n{expected}"
    );
}

// ── Cells ───────────────────────────────────────────────────────────

#[test]
fn claude_cli_working() {
    check(So::ClaudeCli, Sc::Working);
}
#[test]
fn claude_cli_waiting() {
    check(So::ClaudeCli, Sc::Waiting);
}
#[test]
fn claude_cli_idle() {
    check(So::ClaudeCli, Sc::Idle);
}
#[test]
fn claude_cli_killed() {
    check(So::ClaudeCli, Sc::Killed);
}
#[test]
fn claude_cli_pid_reuse() {
    check(So::ClaudeCli, Sc::PidReuse);
}
#[test]
fn claude_cli_subagent() {
    check(So::ClaudeCli, Sc::Subagent);
}

#[test]
fn claude_legacy_working() {
    check(So::ClaudeLegacy, Sc::Working);
}
#[test]
fn claude_legacy_waiting() {
    check(So::ClaudeLegacy, Sc::Waiting);
}
#[test]
fn claude_legacy_idle() {
    check(So::ClaudeLegacy, Sc::Idle);
}
#[test]
fn claude_legacy_killed() {
    check(So::ClaudeLegacy, Sc::Killed);
}
#[test]
fn claude_legacy_pid_reuse() {
    check(So::ClaudeLegacy, Sc::PidReuse);
}
#[test]
fn claude_legacy_subagent() {
    check(So::ClaudeLegacy, Sc::Subagent);
}

#[test]
fn codex_working() {
    check(So::Codex, Sc::Working);
}
#[test]
fn codex_waiting() {
    check(So::Codex, Sc::Waiting);
}
#[test]
fn codex_idle() {
    check(So::Codex, Sc::Idle);
}
#[test]
fn codex_killed() {
    check(So::Codex, Sc::Killed);
}
#[test]
fn codex_pid_reuse() {
    check(So::Codex, Sc::PidReuse);
}
#[test]
fn codex_subagent() {
    check(So::Codex, Sc::Subagent);
}

#[test]
fn cursor_working() {
    check(So::Cursor, Sc::Working);
}
#[test]
#[ignore = "route1-B: no approval signal is known in the Cursor transcript (records are role messages and \
`turn_ended`); not yet checked whether a pending `tool_use` is written before the prompt"]
fn cursor_waiting() {
    check(So::Cursor, Sc::Waiting);
}
#[test]
fn cursor_idle() {
    check(So::Cursor, Sc::Idle);
}
#[test]
fn cursor_killed() {
    check(So::Cursor, Sc::Killed);
}
#[test]
fn cursor_pid_reuse() {
    check(So::Cursor, Sc::PidReuse);
}
#[test]
fn cursor_subagent() {
    check(So::Cursor, Sc::Subagent);
}

#[test]
fn pi_working() {
    check(So::Pi, Sc::Working);
}
#[test]
fn pi_idle() {
    check(So::Pi, Sc::Idle);
}
#[test]
fn pi_killed() {
    check(So::Pi, Sc::Killed);
}
#[test]
fn pi_pid_reuse() {
    check(So::Pi, Sc::PidReuse);
}
