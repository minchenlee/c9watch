//! Dev-only shadow diff (plan C1a-17): each GUI poll compares the sessions
//! the GUI detected with the engine's `session.list`, to measure X1 during
//! dogfood. Compiled only with `cfg(c9_channel_dev)`, so a release build has
//! none of it.
//!
//! - The diff never starts the engine. A tick that cannot read the engine
//!   counts as `unreachable` and compares nothing.
//! - Both sides are aligned by `SessionIdentity::key()` and compared on
//!   presence and status. The GUI and the engine poll on their own clocks,
//!   so a diff gets one tick of slack: it is unexplained only when the same
//!   diff is still there on the next compared tick.
//! - Each unexplained diff is appended once to `<home>/shadow-diff.ndjson`
//!   (key and statuses only, no conversation text).
//! - `<home>/shadow-coverage.ndjson` holds one row per local day:
//!   `{day, gui_ticks, compared, unreachable, mismatches, unexplained}`.
//!   `mismatches` counts compared ticks with any diff, `unexplained` the
//!   compared ticks with a diff that outlived its slack.
//!   `c9watch engine shadow-report` prints these rows.
//!
//! `<home>` is the engine home of this build's channel (`engine-dev` for a
//! dev build). The GUI never creates it: until the engine has made it, the
//! counters stay in memory.

use super::client::{self, ClientError};
use super::home::{engine_home, sock_path};
use super::wire::AgentRow;
use crate::session::enrichment::Session;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Unexplained diffs, one JSON object per line.
pub const DIFF_FILE: &str = "shadow-diff.ndjson";
/// Daily coverage rows, one JSON object per line.
pub const COVERAGE_FILE: &str = "shadow-coverage.ndjson";

/// How long one GUI tick waits for the engine.
const ENGINE_TIMEOUT: Duration = Duration::from_millis(500);
/// How often the coverage row is written while the day goes on.
const FLUSH_EVERY: Duration = Duration::from_secs(60);
/// `shadow-diff.ndjson` stops growing past this size.
const MAX_DIFF_FILE: u64 = 16 << 20;
/// The coverage file keeps this many days.
const MAX_DAYS: usize = 366;

/// One day of coverage.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Coverage {
    /// Local date, `YYYY-MM-DD`.
    pub day: String,
    /// GUI ticks that detected sessions.
    pub gui_ticks: u64,
    /// Ticks that read the engine and compared.
    pub compared: u64,
    /// Ticks that could not read the engine.
    pub unreachable: u64,
    /// Compared ticks with any diff.
    pub mismatches: u64,
    /// Compared ticks with a diff that was also there on the tick before.
    pub unexplained: u64,
}

/// One session as one side sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Side {
    /// `SessionIdentity::key()`.
    pub key: String,
    /// `SessionStatus::wire_name()`.
    pub status: String,
}

impl Side {
    pub fn from_session(s: &Session) -> Self {
        Side {
            key: s.session_key.clone(),
            status: s.status.wire_name().to_string(),
        }
    }

    /// `None` for a row with no session key (not matched to a session).
    pub fn from_row(r: &AgentRow) -> Option<Self> {
        Some(Side {
            key: r.session_key.clone()?,
            status: r.status.clone().unwrap_or_default(),
        })
    }
}

/// One difference between the two sides.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Diff {
    /// The GUI sees the session, the engine does not.
    OnlyGui { key: String },
    /// The engine lists the session, the GUI does not see it.
    OnlyEngine { key: String },
    /// Both see the session with another status.
    Status {
        key: String,
        gui: String,
        engine: String,
    },
}

/// The diffs between `gui` and `engine`, in a stable order.
pub fn compare(gui: &[Side], engine: &[Side]) -> BTreeSet<Diff> {
    let g: HashMap<&str, &str> = gui
        .iter()
        .map(|s| (s.key.as_str(), s.status.as_str()))
        .collect();
    let e: HashMap<&str, &str> = engine
        .iter()
        .map(|s| (s.key.as_str(), s.status.as_str()))
        .collect();
    let mut out = BTreeSet::new();
    for (key, gs) in &g {
        match e.get(key) {
            None => {
                out.insert(Diff::OnlyGui {
                    key: key.to_string(),
                });
            }
            Some(es) if es != gs => {
                out.insert(Diff::Status {
                    key: key.to_string(),
                    gui: gs.to_string(),
                    engine: es.to_string(),
                });
            }
            Some(_) => {}
        }
    }
    for key in e.keys().filter(|k| !g.contains_key(*k)) {
        out.insert(Diff::OnlyEngine {
            key: key.to_string(),
        });
    }
    out
}

/// What one tick found.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Unreachable,
    /// `new_unexplained` are the diffs written to the diff file this tick.
    Compared {
        diffs: usize,
        new_unexplained: Vec<Diff>,
    },
}

/// The shadow diff state of one GUI process.
pub struct ShadowDiff {
    home: PathBuf,
    today: Option<Coverage>,
    /// Diffs of the last compared tick.
    prev: HashSet<Diff>,
    /// Unexplained diffs already written, while they last.
    reported: HashSet<Diff>,
    last_flush: Option<Instant>,
    last_error: Option<String>,
}

impl ShadowDiff {
    /// The shadow diff for this build's engine home, or `None` without one.
    pub fn for_gui() -> Option<Self> {
        match engine_home() {
            Ok(home) => Some(Self::new(home)),
            Err(e) => {
                crate::debug_log::log_warn(&format!("Shadow diff off: {}", e));
                None
            }
        }
    }

    pub fn new(home: PathBuf) -> Self {
        Self {
            home,
            today: None,
            prev: HashSet::new(),
            reported: HashSet::new(),
            last_flush: None,
            last_error: None,
        }
    }

    /// One GUI poll: read the engine, compare, and write the coverage row
    /// when it is due.
    pub fn tick(&mut self, sessions: &[Session]) {
        let gui: Vec<Side> = sessions.iter().map(Side::from_session).collect();
        let engine = client::session_list(&sock_path(&self.home), "gui-shadow", ENGINE_TIMEOUT)
            .map(|rows| rows.iter().filter_map(Side::from_row).collect::<Vec<_>>());
        self.note_error(engine.as_ref().err());
        let now = chrono::Local::now();
        let day = now.format("%Y-%m-%d").to_string();
        self.record(&day, now.timestamp_millis(), &gui, engine.ok());
        if self.last_flush.is_none_or(|t| t.elapsed() >= FLUSH_EVERY) {
            self.flush();
        }
    }

    /// Log when the reason the engine cannot be read changes, not every
    /// tick.
    fn note_error(&mut self, err: Option<&ClientError>) {
        let err = err.map(|e| e.to_string());
        if err != self.last_error {
            match &err {
                Some(e) => crate::debug_log::log_info(&format!("Shadow diff: engine {}", e)),
                None => crate::debug_log::log_info("Shadow diff: engine reachable"),
            }
            self.last_error = err;
        }
    }

    /// Count one tick. `engine` is `None` when the engine could not be read.
    pub fn record(
        &mut self,
        day: &str,
        ts_ms: i64,
        gui: &[Side],
        engine: Option<Vec<Side>>,
    ) -> Outcome {
        if self.today.as_ref().is_some_and(|c| c.day != day) {
            // A new day: write the finished row first.
            self.flush();
            self.today = None;
        }
        let home = self.home.clone();
        let today = self.today.get_or_insert_with(|| {
            // Continue today's row after a GUI restart.
            read_coverage(&home)
                .ok()
                .and_then(|rows| rows.into_iter().find(|r| r.day == day))
                .unwrap_or_else(|| Coverage {
                    day: day.to_string(),
                    ..Coverage::default()
                })
        });
        today.gui_ticks += 1;

        let Some(engine) = engine else {
            today.unreachable += 1;
            // No slack across a gap: a diff must show on two compared ticks
            // in a row.
            self.prev.clear();
            self.reported.clear();
            return Outcome::Unreachable;
        };

        let diffs = compare(gui, &engine);
        today.compared += 1;
        if !diffs.is_empty() {
            today.mismatches += 1;
        }
        let persisting: Vec<&Diff> = diffs.iter().filter(|d| self.prev.contains(d)).collect();
        if !persisting.is_empty() {
            today.unexplained += 1;
        }
        let new_unexplained: Vec<Diff> = persisting
            .into_iter()
            .filter(|d| !self.reported.contains(*d))
            .cloned()
            .collect();
        for d in &new_unexplained {
            self.append_diff(day, ts_ms, d);
            self.reported.insert(d.clone());
        }
        self.reported.retain(|d| diffs.contains(d));
        let n = diffs.len();
        self.prev = diffs.into_iter().collect();
        Outcome::Compared {
            diffs: n,
            new_unexplained,
        }
    }

    fn append_diff(&self, day: &str, ts_ms: i64, d: &Diff) {
        if !self.home.is_dir() {
            return;
        }
        let path = self.home.join(DIFF_FILE);
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.len() >= MAX_DIFF_FILE) {
            return;
        }
        let mut row = serde_json::to_value(d).unwrap_or_default();
        row["ts"] = ts_ms.into();
        row["day"] = day.into();
        let mut line = row.to_string();
        line.push('\n');
        let res = open_private(&path, OpenOptions::new().append(true).create(true))
            .and_then(|mut f| f.write_all(line.as_bytes()).map_err(|e| e.to_string()));
        if let Err(e) = res {
            crate::debug_log::log_warn(&format!("Shadow diff: cannot write {:?}: {}", path, e));
        }
    }

    /// Write today's coverage row, replacing an older row of the same day.
    /// Does nothing until the engine home exists.
    pub fn flush(&mut self) {
        self.last_flush = Some(Instant::now());
        let Some(today) = &self.today else { return };
        if !self.home.is_dir() {
            return;
        }
        if let Err(e) = write_coverage_row(&self.home, today) {
            crate::debug_log::log_warn(&format!("Shadow diff: cannot write coverage: {}", e));
        }
    }
}

/// Open `path` 0600 without following a symlink.
fn open_private(path: &Path, opts: &mut OpenOptions) -> Result<File, String> {
    opts.mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| e.to_string())
}

/// The coverage rows in `home`, oldest day first. No file is no rows.
pub fn read_coverage(home: &Path) -> Result<Vec<Coverage>, String> {
    let path = home.join(COVERAGE_FILE);
    let text = match open_private(&path, OpenOptions::new().read(true)) {
        Ok(mut f) => {
            let mut s = String::new();
            std::io::Read::read_to_string(&mut f, &mut s)
                .map_err(|e| format!("{:?}: {}", path, e))?;
            s
        }
        Err(_) if !path.exists() => return Ok(Vec::new()),
        Err(e) => return Err(format!("{:?}: {}", path, e)),
    };
    // A line we cannot parse is skipped, so one bad write does not hide
    // the other days.
    let mut rows: Vec<Coverage> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    rows.sort_by(|a, b| a.day.cmp(&b.day));
    Ok(rows)
}

fn write_coverage_row(home: &Path, row: &Coverage) -> Result<(), String> {
    let mut rows = read_coverage(home)?;
    rows.retain(|r| r.day != row.day);
    rows.push(row.clone());
    rows.sort_by(|a, b| a.day.cmp(&b.day));
    let skip = rows.len().saturating_sub(MAX_DAYS);
    let mut text = String::new();
    for r in &rows[skip..] {
        text.push_str(&serde_json::to_string(r).map_err(|e| e.to_string())?);
        text.push('\n');
    }
    let path = home.join(COVERAGE_FILE);
    let tmp = home.join(format!("{}.tmp", COVERAGE_FILE));
    let mut f = open_private(
        &tmp,
        OpenOptions::new().write(true).create(true).truncate(true),
    )?;
    f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    drop(f);
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_proto::client::tests::{fake_engine, row};

    fn side(key: &str, status: &str) -> Side {
        Side {
            key: key.into(),
            status: status.into(),
        }
    }

    fn rows(home: &Path, file: &str) -> Vec<serde_json::Value> {
        std::fs::read_to_string(home.join(file))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    const DAY: &str = "2026-10-30";

    #[test]
    fn matching_sides_have_no_diff() {
        let dir = tempfile::tempdir().unwrap();
        let mut sd = ShadowDiff::new(dir.path().to_path_buf());
        let gui = [
            side("claudeCode:a", "working"),
            side("codex:b", "connecting"),
        ];
        let engine = vec![
            side("codex:b", "connecting"),
            side("claudeCode:a", "working"),
        ];
        for i in 0..3 {
            let out = sd.record(DAY, i, &gui, Some(engine.clone()));
            assert_eq!(
                out,
                Outcome::Compared {
                    diffs: 0,
                    new_unexplained: vec![]
                }
            );
        }
        sd.flush();
        let cov = read_coverage(dir.path()).unwrap();
        assert_eq!(
            cov,
            vec![Coverage {
                day: DAY.into(),
                gui_ticks: 3,
                compared: 3,
                unreachable: 0,
                mismatches: 0,
                unexplained: 0,
            }]
        );
        assert!(!dir.path().join(DIFF_FILE).exists());
    }

    #[test]
    fn a_diff_that_lasts_one_tick_is_explained() {
        let dir = tempfile::tempdir().unwrap();
        let mut sd = ShadowDiff::new(dir.path().to_path_buf());
        // The GUI sees a new session one tick before the engine does, and a
        // status change one tick after.
        sd.record(DAY, 1, &[side("k:a", "working")], Some(vec![]));
        sd.record(
            DAY,
            2,
            &[side("k:a", "working")],
            Some(vec![side("k:a", "waiting_for_input")]),
        );
        sd.record(
            DAY,
            3,
            &[side("k:a", "waiting_for_input")],
            Some(vec![side("k:a", "waiting_for_input")]),
        );
        sd.flush();
        let cov = &read_coverage(dir.path()).unwrap()[0];
        assert_eq!((cov.compared, cov.mismatches, cov.unexplained), (3, 2, 0));
        assert!(!dir.path().join(DIFF_FILE).exists());
    }

    #[test]
    fn a_persisting_mismatch_is_written_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut sd = ShadowDiff::new(dir.path().to_path_buf());
        let gui = [side("k:a", "working"), side("k:b", "needs_attention")];
        let engine = vec![side("k:b", "working"), side("k:c", "connecting")];
        let first = sd.record(DAY, 1, &gui, Some(engine.clone()));
        assert_eq!(
            first,
            Outcome::Compared {
                diffs: 3,
                new_unexplained: vec![]
            }
        );
        let second = sd.record(DAY, 2, &gui, Some(engine.clone()));
        let Outcome::Compared {
            new_unexplained, ..
        } = second
        else {
            panic!("{:?}", second)
        };
        assert_eq!(
            new_unexplained,
            vec![
                Diff::OnlyGui { key: "k:a".into() },
                Diff::OnlyEngine { key: "k:c".into() },
                Diff::Status {
                    key: "k:b".into(),
                    gui: "needs_attention".into(),
                    engine: "working".into()
                },
            ]
        );
        // Still there: counted as unexplained, but not written again.
        sd.record(DAY, 3, &gui, Some(engine));
        sd.flush();

        let diffs = rows(dir.path(), DIFF_FILE);
        assert_eq!(diffs.len(), 3);
        assert_eq!(diffs[0]["kind"], "only_gui");
        assert_eq!(diffs[0]["key"], "k:a");
        assert_eq!(diffs[0]["ts"], 2);
        assert_eq!(diffs[0]["day"], DAY);
        assert_eq!(diffs[2]["kind"], "status");
        assert_eq!(diffs[2]["gui"], "needs_attention");
        assert_eq!(diffs[2]["engine"], "working");

        let cov = &read_coverage(dir.path()).unwrap()[0];
        assert_eq!((cov.compared, cov.mismatches, cov.unexplained), (3, 3, 2));
    }

    #[test]
    fn unreachable_ticks_count_and_reset_the_slack() {
        let dir = tempfile::tempdir().unwrap();
        let mut sd = ShadowDiff::new(dir.path().to_path_buf());
        let gui = [side("k:a", "working")];
        sd.record(DAY, 1, &gui, Some(vec![]));
        assert_eq!(sd.record(DAY, 2, &gui, None), Outcome::Unreachable);
        // After the gap the diff starts its slack again.
        let out = sd.record(DAY, 3, &gui, Some(vec![]));
        assert_eq!(
            out,
            Outcome::Compared {
                diffs: 1,
                new_unexplained: vec![]
            }
        );
        sd.flush();
        let cov = &read_coverage(dir.path()).unwrap()[0];
        assert_eq!(
            (
                cov.gui_ticks,
                cov.compared,
                cov.unreachable,
                cov.unexplained
            ),
            (3, 2, 1, 0)
        );
    }

    #[test]
    fn tick_without_an_engine_is_unreachable_and_starts_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut sd = ShadowDiff::new(dir.path().to_path_buf());
        sd.tick(&[]);
        sd.flush();
        let cov = read_coverage(dir.path()).unwrap();
        assert_eq!((cov[0].gui_ticks, cov[0].unreachable), (1, 1));
        // Only the coverage file: no socket, lock or engine db appeared.
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec![COVERAGE_FILE.to_string()]);
    }

    #[test]
    fn tick_compares_with_a_running_engine() {
        let dir = tempfile::tempdir().unwrap();
        let (_sock, server) = fake_engine(dir.path(), vec![row("claudeCode:a", "working")], 1);
        let mut sd = ShadowDiff::new(dir.path().to_path_buf());
        sd.tick(&[]);
        server.join().unwrap();
        sd.flush();
        let cov = &read_coverage(dir.path()).unwrap()[0];
        assert_eq!((cov.compared, cov.unreachable, cov.mismatches), (1, 0, 1));
    }

    #[test]
    fn missing_home_keeps_counts_in_memory() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("engine-dev");
        let mut sd = ShadowDiff::new(home.clone());
        let gui = [side("k:a", "working")];
        sd.record(DAY, 1, &gui, Some(vec![]));
        sd.record(DAY, 2, &gui, Some(vec![]));
        sd.flush();
        assert!(!home.exists(), "the GUI must not create the engine home");
        // Once the engine made its home, the next flush writes the counts.
        std::fs::create_dir(&home).unwrap();
        sd.flush();
        assert_eq!(read_coverage(&home).unwrap()[0].compared, 2);
    }

    #[test]
    fn coverage_is_one_row_per_day_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let mut sd = ShadowDiff::new(home.clone());
        sd.record("2026-10-29", 1, &[], None);
        sd.record("2026-10-30", 2, &[], Some(vec![]));
        sd.flush();
        sd.flush();
        // A new GUI process continues today's row.
        let mut again = ShadowDiff::new(home.clone());
        again.record("2026-10-30", 3, &[], Some(vec![]));
        again.flush();
        let cov = read_coverage(&home).unwrap();
        let days: Vec<(&str, u64, u64, u64)> = cov
            .iter()
            .map(|c| (c.day.as_str(), c.gui_ticks, c.compared, c.unreachable))
            .collect();
        assert_eq!(days, vec![("2026-10-29", 1, 0, 1), ("2026-10-30", 2, 2, 0)]);
        // Snake-case keys, as the plan names them.
        let raw = rows(&home, COVERAGE_FILE);
        for key in [
            "day",
            "gui_ticks",
            "compared",
            "unreachable",
            "mismatches",
            "unexplained",
        ] {
            assert!(raw[0].get(key).is_some(), "missing {}", key);
        }
    }

    #[test]
    fn engine_rows_without_a_session_key_are_ignored() {
        let mut r = row("claudeCode:a", "working");
        assert_eq!(Side::from_row(&r), Some(side("claudeCode:a", "working")));
        r.session_key = None;
        assert_eq!(Side::from_row(&r), None);
    }
}
