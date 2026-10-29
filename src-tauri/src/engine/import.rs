//! Import of the legacy PM files (plan C1a-18, A:496).
//!
//! Copy first, then parse. At every boot the engine copies
//! `<legacy>/{workers/*/meta.json, inbox/**, adoptions/*}` into
//! `<home>/import/<ms>/` and parses only that copy:
//! - Every open uses `O_NOFOLLOW`, relative to the fd of its parent dir, so
//!   a symlink anywhere below the legacy root is skipped, never followed.
//!   Only regular files are copied, each one at most `MAX_FILE_BYTES`, and
//!   one import at most `MAX_FILES` files and `MAX_TOTAL_BYTES`.
//! - The legacy tree is only read. In particular this never calls
//!   `cli::adoption::read_filter`, which rewrites or deletes adoption files
//!   while it reads them. C1a neither freezes nor changes the legacy dirs.
//!
//! What the copy becomes, in one transaction:
//! - `WorkerMeta` (`cli/pm_fs.rs`): one `role=worker`, `control=none`,
//!   `source=legacy_import` row per worker session, with a `session_alias`
//!   row. A re-import updates that row; a change emits `agent.upsert`.
//! - Legacy rows are history, never live: `gone_at` is the worker's
//!   `stoppedAt`, or the import time when it has none (the daemon never
//!   writes one). A legacy worker that still runs is listed by detection
//!   as its `source=detected` row, so the default `agent.list` shows it
//!   once.
//! - `AdoptionRecord` (`cli/adoption.rs`): the worker's `parent_id`. The
//!   adopting PM gets a `role=orch` legacy row if it has none. A record that
//!   names a worker with no row (a dangling adoption) is skipped.
//! - `InboxEvent` (`cli/pm_inbox.rs`): one `legacy.inbox` event each, once
//!   per `eventId`. `legacy_events` remembers the ids, so an event that
//!   retention pruned is not imported again. The `messages` table is C1b.
//!
//! The legacy types live behind `pm-orchestration`, so the copy is parsed
//! into the minimal mirrors below, which ignore unknown fields.

use super::db::outbox::NewEvent;
use super::db::{agent_row, new_agent_id, now_ms, Db, Tx, AGENT_COLUMNS};
use super::fsutil::ensure_private_dir;
use super::proto::AgentRow;
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::ffi::{CStr, CString, OsStr, OsString};
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// The snapshot copies inside the engine home.
pub const IMPORT_DIR: &str = "import";
/// Snapshot copies kept after an import; older ones are removed.
pub const KEEP_SNAPSHOTS: usize = 3;
/// A larger legacy file is skipped. Meta, inbox and adoption files are a
/// few KiB.
pub const MAX_FILE_BYTES: u64 = 1 << 20;
/// One import copies at most this much...
pub const MAX_TOTAL_BYTES: u64 = 64 << 20;
/// ...and at most this many files.
pub const MAX_FILES: usize = 20_000;
/// `inbox/**` is followed at most this deep (the layout is
/// `inbox/<worker>/<event>.json`).
const MAX_INBOX_DEPTH: usize = 3;

/// The provider of every legacy worker: the PM daemon spawned Claude Code.
const PROVIDER: &str = "claudeCode";

/// The legacy daemon's dir, `~/.claude/c9watch`.
pub fn legacy_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("c9watch"))
}

/// `<home>/import`
pub fn import_dir(home: &Path) -> PathBuf {
    home.join(IMPORT_DIR)
}

/// What one import did.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Report {
    /// The snapshot copy; `None` when the legacy root does not exist.
    pub snapshot: Option<PathBuf>,
    pub copied: usize,
    /// Symlinks met in the legacy tree, none of them followed.
    pub symlinks: usize,
    /// Files that are not regular, too large, or over the import caps.
    pub skipped: usize,
    /// Copied files that did not parse.
    pub bad: usize,
    /// Legacy rows created (workers and adopting PMs).
    pub created: Vec<String>,
    /// Legacy rows that changed.
    pub updated: Vec<String>,
    /// Adoption entries naming a worker with no row.
    pub dangling: usize,
    /// Inbox events emitted.
    pub events: usize,
}

impl Report {
    /// Anything worth a log line.
    pub fn is_empty(&self) -> bool {
        self.copied == 0 && self.symlinks == 0 && self.skipped == 0 && self.bad == 0
    }
}

/// Copy `source` into a new snapshot in `home`, import the copy, and drop
/// old snapshots. A missing `source` imports nothing and creates nothing,
/// and a failed import removes its snapshot. Blocking.
pub fn import(db: &Db, home: &Path, source: &Path) -> Result<Report, String> {
    let mut report = Report::default();
    let root = match open_nofollow(None, &cstr(source.as_os_str())?, DIR_FLAGS) {
        Ok(fd) => fd,
        Err(e) if e.raw_os_error() == Some(libc::ENOENT) => return Ok(report),
        Err(e) if is_link_err(&e) => {
            let link = std::fs::symlink_metadata(source).is_ok_and(|m| m.file_type().is_symlink());
            if !link {
                return Err(format!("{:?} is not a directory", source));
            }
            report.symlinks += 1;
            return Ok(report);
        }
        Err(e) => return Err(format!("Failed to open {:?}: {}", source, e)),
    };

    let base = import_dir(home);
    ensure_private_dir(&base)?;
    let snap = new_snapshot_dir(&base)?;
    if let Err(e) = copy_and_apply(db, &root, &snap, &mut report) {
        // Each failed boot would otherwise leave one more copy behind.
        if let Err(rm) = std::fs::remove_dir_all(&snap) {
            eprintln!("[engine] Failed to remove import {:?}: {}", snap, rm);
        }
        return Err(e);
    }
    report.snapshot = Some(snap.clone());
    prune_snapshots(&base, &snap);
    Ok(report)
}

fn copy_and_apply(db: &Db, root: &OwnedFd, snap: &Path, report: &mut Report) -> Result<(), String> {
    copy_legacy(root, snap, report)?;
    let legacy = parse(snap, report);
    let machine = db.machine_id().to_string();
    let (applied, _) = db.write_blocking(move |tx| apply(tx, &machine, &legacy, now_ms()))?;
    report.created = applied.created;
    report.updated = applied.updated;
    report.dangling = applied.dangling;
    report.events = applied.events;
    Ok(())
}

// ── Copy ──────────────────────────────────────────────────────────────

const DIR_FLAGS: libc::c_int = libc::O_RDONLY | libc::O_DIRECTORY;
// O_NONBLOCK: opening a FIFO planted in the tree must not hang the boot.
const FILE_FLAGS: libc::c_int = libc::O_RDONLY | libc::O_NONBLOCK;

/// Copy the three legacy subtrees of `root` into `snap`.
fn copy_legacy(root: &OwnedFd, snap: &Path, report: &mut Report) -> Result<(), String> {
    let mut budget = Budget::default();
    if let Some(workers) = open_child_dir(root, OsStr::new("workers"), report)? {
        for name in list_dir(&workers)? {
            let Some(w) = open_child_dir(&workers, &name, report)? else {
                continue;
            };
            let dest = snap.join("workers").join(&name);
            copy_file(&w, OsStr::new("meta.json"), &dest, report, &mut budget)?;
        }
    }
    if let Some(inbox) = open_child_dir(root, OsStr::new("inbox"), report)? {
        copy_tree(&inbox, &snap.join("inbox"), 1, report, &mut budget)?;
    }
    if let Some(adoptions) = open_child_dir(root, OsStr::new("adoptions"), report)? {
        for name in list_dir(&adoptions)? {
            let dest = snap.join("adoptions");
            copy_file(&adoptions, &name, &dest, report, &mut budget)?;
        }
    }
    Ok(())
}

#[derive(Default)]
struct Budget {
    files: usize,
    bytes: u64,
}

/// Copy every regular file under `dir`, down to `MAX_INBOX_DEPTH`.
fn copy_tree(
    dir: &OwnedFd,
    dest: &Path,
    depth: usize,
    report: &mut Report,
    budget: &mut Budget,
) -> Result<(), String> {
    for name in list_dir(dir)? {
        let c = cstr(&name)?;
        let st = match lstat_at(dir, &c) {
            Ok(st) => st,
            // Gone since the listing.
            Err(_) => continue,
        };
        match st.st_mode & libc::S_IFMT {
            libc::S_IFLNK => report.symlinks += 1,
            libc::S_IFDIR if depth < MAX_INBOX_DEPTH => {
                if let Some(sub) = open_child_dir(dir, &name, report)? {
                    copy_tree(&sub, &dest.join(&name), depth + 1, report, budget)?;
                }
            }
            libc::S_IFREG => copy_file(dir, &name, dest, report, budget)?,
            _ => report.skipped += 1,
        }
    }
    Ok(())
}

/// Open the dir `name` in `parent`. `None` when it is missing, a symlink
/// (counted) or not a dir (counted).
fn open_child_dir(
    parent: &OwnedFd,
    name: &OsStr,
    report: &mut Report,
) -> Result<Option<OwnedFd>, String> {
    let c = cstr(name)?;
    match open_nofollow(Some(parent), &c, DIR_FLAGS) {
        Ok(fd) => Ok(Some(fd)),
        Err(e) if e.raw_os_error() == Some(libc::ENOENT) => Ok(None),
        Err(e) if is_link_err(&e) => {
            let link =
                lstat_at(parent, &c).is_ok_and(|st| st.st_mode & libc::S_IFMT == libc::S_IFLNK);
            if link {
                report.symlinks += 1;
            } else {
                report.skipped += 1;
            }
            Ok(None)
        }
        Err(e) => Err(format!("Failed to open legacy dir {:?}: {}", name, e)),
    }
}

/// Copy the regular file `name` in `dir` to `dest_dir/name`, within the
/// caps. Anything else is counted and skipped.
fn copy_file(
    dir: &OwnedFd,
    name: &OsStr,
    dest_dir: &Path,
    report: &mut Report,
    budget: &mut Budget,
) -> Result<(), String> {
    let fd = match open_nofollow(Some(dir), &cstr(name)?, FILE_FLAGS) {
        Ok(fd) => fd,
        Err(e) if e.raw_os_error() == Some(libc::ENOENT) => return Ok(()),
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
            report.symlinks += 1;
            return Ok(());
        }
        Err(_) => {
            report.skipped += 1;
            return Ok(());
        }
    };
    let mut file = std::fs::File::from(fd);
    let md = file
        .metadata()
        .map_err(|e| format!("Failed to stat legacy file {:?}: {}", name, e))?;
    let size = md.len();
    if !md.is_file()
        || size > MAX_FILE_BYTES
        || budget.files >= MAX_FILES
        || budget.bytes + size > MAX_TOTAL_BYTES
    {
        report.skipped += 1;
        return Ok(());
    }
    let mut bytes = Vec::with_capacity(size as usize);
    // One byte past the cap tells a file that grew since the stat.
    if (&mut file)
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() as u64 > MAX_FILE_BYTES
    {
        report.skipped += 1;
        return Ok(());
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dest_dir)
        .map_err(|e| format!("Failed to create {:?}: {}", dest_dir, e))?;
    let dest = dest_dir.join(name);
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&dest)
        .map_err(|e| format!("Failed to create {:?}: {}", dest, e))?;
    std::io::Write::write_all(&mut out, &bytes)
        .map_err(|e| format!("Failed to write {:?}: {}", dest, e))?;
    budget.files += 1;
    budget.bytes += bytes.len() as u64;
    report.copied += 1;
    Ok(())
}

/// `<base>/<ms>`, created private. Waits out a clash with a snapshot made
/// in the same ms.
fn new_snapshot_dir(base: &Path) -> Result<PathBuf, String> {
    let mut ms = now_ms();
    loop {
        let dir = base.join(ms.to_string());
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => ms += 1,
            Err(e) => return Err(format!("Failed to create {:?}: {}", dir, e)),
        }
    }
}

/// Remove all but the newest `KEEP_SNAPSHOTS` snapshots. Never `keep`.
/// Only dirs named by a ms timestamp are touched.
fn prune_snapshots(base: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    let mut snaps: Vec<(i64, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| {
            let ms = e.file_name().to_str()?.parse::<i64>().ok()?;
            Some((ms, e.path()))
        })
        .collect();
    snaps.sort();
    let old = snaps.len().saturating_sub(KEEP_SNAPSHOTS);
    for (_, dir) in snaps.into_iter().take(old) {
        if dir != keep {
            if let Err(e) = std::fs::remove_dir_all(&dir) {
                eprintln!("[engine] Failed to remove old import {:?}: {}", dir, e);
            }
        }
    }
}

fn cstr(s: &OsStr) -> Result<CString, String> {
    CString::new(s.as_bytes()).map_err(|_| format!("{:?} contains a NUL byte", s))
}

/// What `O_NOFOLLOW` on a symlink fails with: `ELOOP`, or `ENOTDIR` when
/// `O_DIRECTORY` is set too (Linux). `ENOTDIR` also means "not a dir", so
/// callers that pass `O_DIRECTORY` lstat to tell the two apart.
fn is_link_err(e: &std::io::Error) -> bool {
    matches!(e.raw_os_error(), Some(libc::ELOOP) | Some(libc::ENOTDIR))
}

/// `openat(parent, name, flags|O_NOFOLLOW|O_CLOEXEC)`; `None` resolves an
/// absolute `name`.
fn open_nofollow(
    parent: Option<&OwnedFd>,
    name: &CString,
    flags: libc::c_int,
) -> std::io::Result<OwnedFd> {
    let dirfd = parent.map_or(libc::AT_FDCWD, |p| p.as_raw_fd());
    let flags = flags | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let fd = unsafe { libc::openat(dirfd, name.as_ptr(), flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn lstat_at(parent: &OwnedFd, name: &CString) -> std::io::Result<libc::stat> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(st)
}

/// The entry names of the open dir `dir`, without `.` and `..`, sorted.
fn list_dir(dir: &OwnedFd) -> Result<Vec<OsString>, String> {
    let fail = |e: std::io::Error| format!("Failed to list a legacy dir: {}", e);
    // `closedir` closes the fd it was given, so it gets a dup.
    let dup = unsafe { libc::dup(dir.as_raw_fd()) };
    if dup < 0 {
        return Err(fail(std::io::Error::last_os_error()));
    }
    let d = unsafe { libc::fdopendir(dup) };
    if d.is_null() {
        let e = std::io::Error::last_os_error();
        unsafe { libc::close(dup) };
        return Err(fail(e));
    }
    let mut names = Vec::new();
    loop {
        let ent = unsafe { libc::readdir(d) };
        if ent.is_null() {
            break;
        }
        let name = unsafe { CStr::from_ptr((*ent).d_name.as_ptr()) };
        let name = OsStr::from_bytes(name.to_bytes());
        if name != "." && name != ".." {
            names.push(name.to_os_string());
        }
    }
    unsafe { libc::closedir(d) };
    names.sort();
    Ok(names)
}

// ── Parse (the copy only) ─────────────────────────────────────────────

/// The fields of `cli::pm_fs::WorkerMeta` the import uses.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyWorker {
    session_id: String,
    #[serde(default)]
    pid: Option<i64>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    spawned_at: Option<String>,
    #[serde(default)]
    stopped_at: Option<String>,
}

/// `cli::adoption::AdoptionRecord`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyAdoption {
    pm_session_id: String,
    #[serde(default)]
    adopted_at: String,
    worker_ids: Vec<String>,
}

/// One `cli::pm_inbox::InboxEvent`, kept as it was written.
#[derive(Debug, Clone)]
struct LegacyEvent {
    event_id: String,
    session_id: String,
    raw: Value,
}

#[derive(Debug, Default)]
struct Legacy {
    workers: Vec<LegacyWorker>,
    adoptions: Vec<LegacyAdoption>,
    events: Vec<LegacyEvent>,
}

/// The rule of `cli::pm_fs::validate_session_id`.
fn valid_sid(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Every regular file under `dir`, recursively, sorted.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in entries.filter_map(Result::ok) {
        match e.file_type() {
            Ok(t) if t.is_dir() => out.extend(files_under(&e.path())),
            Ok(t) if t.is_file() => out.push(e.path()),
            _ => {}
        }
    }
    out.sort();
    out
}

fn parse(snap: &Path, report: &mut Report) -> Legacy {
    let mut legacy = Legacy::default();
    for path in files_under(&snap.join("workers")) {
        match read_json::<LegacyWorker>(&path) {
            Some(w) if valid_sid(&w.session_id) => legacy.workers.push(w),
            _ => report.bad += 1,
        }
    }
    for path in files_under(&snap.join("adoptions")) {
        match read_json::<LegacyAdoption>(&path) {
            Some(a) if valid_sid(&a.pm_session_id) => legacy.adoptions.push(a),
            _ => report.bad += 1,
        }
    }
    for path in files_under(&snap.join("inbox")) {
        let raw: Option<Value> = read_json(&path);
        let ev = raw.and_then(|raw| {
            let event_id = raw.get("eventId")?.as_str()?.to_string();
            let session_id = raw.get("sessionId")?.as_str()?.to_string();
            let ok = !event_id.is_empty() && event_id.len() <= 256 && valid_sid(&session_id);
            ok.then_some(LegacyEvent {
                event_id,
                session_id,
                raw,
            })
        });
        match ev {
            Some(ev) => legacy.events.push(ev),
            None => report.bad += 1,
        }
    }
    legacy
        .workers
        .sort_by(|a, b| a.session_id.cmp(&b.session_id));
    // Oldest first, so the newest adoption of a worker wins.
    legacy
        .adoptions
        .sort_by(|a, b| a.adopted_at.cmp(&b.adopted_at));
    legacy.events.sort_by(|a, b| a.event_id.cmp(&b.event_id));
    legacy
}

/// An ISO-8601 timestamp as unix ms.
fn iso_ms(s: Option<&str>) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s?)
        .ok()
        .map(|t| t.timestamp_millis())
}

// ── Apply ─────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct Applied {
    created: Vec<String>,
    updated: Vec<String>,
    dangling: usize,
    events: usize,
}

fn subject(machine: &str, id: &str) -> String {
    format!("{}/{}", machine, id)
}

fn read_row(tx: &Tx, id: &str) -> rusqlite::Result<AgentRow> {
    tx.query_row(
        &format!("SELECT {} FROM agents WHERE id = ?1", AGENT_COLUMNS),
        [id],
        agent_row,
    )
}

/// The legacy row of `sid`.
fn legacy_row(tx: &Tx, sid: &str) -> rusqlite::Result<Option<AgentRow>> {
    tx.query_row(
        &format!(
            "SELECT {} FROM agents WHERE source = 'legacy_import' AND provider = ?1 \
             AND provider_sid = ?2",
            AGENT_COLUMNS
        ),
        params![PROVIDER, sid],
        agent_row,
    )
    .optional()
}

fn emit_upsert(tx: &mut Tx, machine: &str, row: &AgentRow) -> rusqlite::Result<()> {
    tx.emit(&NewEvent {
        topic: "agent.upsert".into(),
        subject: Some(subject(machine, &row.id)),
        origin: None,
        data: serde_json::to_value(row).expect("AgentRow serializes"),
    })?;
    Ok(())
}

/// Insert a legacy row and its `session_alias`; emit `agent.upsert`.
fn insert_legacy(
    tx: &mut Tx,
    machine: &str,
    role: &str,
    sid: &str,
    w: Option<&LegacyWorker>,
    now: i64,
) -> rusqlite::Result<String> {
    let id = new_agent_id();
    let started = w.and_then(|w| iso_ms(w.spawned_at.as_deref()));
    tx.execute(
        "INSERT INTO agents (id, role, control, provider, provider_sid, source, machine_id, \
         cwd, name, pid, started_at, created_at, gone_at) \
         VALUES (?1, ?2, 'none', ?3, ?4, 'legacy_import', ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            id,
            role,
            PROVIDER,
            sid,
            machine,
            w.and_then(|w| w.cwd.as_deref()),
            w.and_then(|w| w.name.as_deref()),
            w.and_then(|w| w.pid),
            started,
            now,
            w.and_then(|w| iso_ms(w.stopped_at.as_deref()))
                .unwrap_or(now),
        ],
    )?;
    tx.execute(
        "INSERT INTO session_alias (agent_id, generation, provider_sid, bound_at) \
         VALUES (?1, 0, ?2, ?3)",
        params![id, sid, started.unwrap_or(now)],
    )?;
    let row = read_row(tx, &id)?;
    emit_upsert(tx, machine, &row)?;
    Ok(id)
}

/// Write the parsed copy; see the module docs.
fn apply(tx: &mut Tx, machine: &str, legacy: &Legacy, now: i64) -> rusqlite::Result<Applied> {
    let mut applied = Applied::default();

    for w in &legacy.workers {
        match legacy_row(tx, &w.session_id)? {
            None => {
                let id = insert_legacy(tx, machine, "worker", &w.session_id, Some(w), now)?;
                applied.created.push(id);
            }
            Some(before) => {
                // A known stop time replaces the import time; the row
                // stays gone either way.
                tx.execute(
                    "UPDATE agents SET cwd = ?2, name = ?3, pid = ?4, started_at = ?5, \
                     gone_at = COALESCE(?6, gone_at, ?7) WHERE id = ?1",
                    params![
                        before.id,
                        w.cwd,
                        w.name,
                        w.pid,
                        iso_ms(w.spawned_at.as_deref()),
                        iso_ms(w.stopped_at.as_deref()),
                        now,
                    ],
                )?;
                let after = read_row(tx, &before.id)?;
                if after != before {
                    emit_upsert(tx, machine, &after)?;
                    applied.updated.push(after.id);
                }
            }
        }
    }

    // worker sid -> adopting PM sid; the newest adoption wins.
    let mut parent_of: BTreeMap<&str, &str> = BTreeMap::new();
    for a in &legacy.adoptions {
        for wid in &a.worker_ids {
            parent_of.insert(wid.as_str(), a.pm_session_id.as_str());
        }
    }
    for (wid, pm) in parent_of {
        let Some(worker) = legacy_row(tx, wid)? else {
            applied.dangling += 1;
            continue;
        };
        let pm_id = match legacy_row(tx, pm)? {
            Some(row) => row.id,
            None => {
                let id = insert_legacy(tx, machine, "orch", pm, None, now)?;
                applied.created.push(id.clone());
                id
            }
        };
        if worker.parent_id.as_deref() != Some(pm_id.as_str()) && worker.id != pm_id {
            tx.execute(
                "UPDATE agents SET parent_id = ?2 WHERE id = ?1",
                params![worker.id, pm_id],
            )?;
            let after = read_row(tx, &worker.id)?;
            emit_upsert(tx, machine, &after)?;
            if !applied.created.contains(&after.id) && !applied.updated.contains(&after.id) {
                applied.updated.push(after.id);
            }
        }
    }

    for ev in &legacy.events {
        let new = tx.execute(
            "INSERT OR IGNORE INTO legacy_events (event_id, imported_at) VALUES (?1, ?2)",
            params![ev.event_id, now],
        )?;
        if new == 0 {
            continue;
        }
        let worker = legacy_row(tx, &ev.session_id)?.map(|r| r.id);
        tx.emit(&NewEvent {
            topic: "legacy.inbox".into(),
            subject: worker.as_deref().map(|id| subject(machine, id)),
            origin: Some(json!({"kind": "system", "legacy": true})),
            data: json!({"legacy": true, "id": worker, "event": ev.raw}),
        })?;
        applied.events += 1;
    }
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::db::{self, AgentFilter};
    use std::fs;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::{symlink, MetadataExt};

    const M: &str = "m0";
    const W1: &str = "11111111-1111-1111-1111-111111111111";
    const W2: &str = "22222222-2222-2222-2222-222222222222";
    const GONE: &str = "99999999-9999-9999-9999-999999999999";
    const PM: &str = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";

    struct Fixture {
        _t: tempfile::TempDir,
        home: PathBuf,
        src: PathBuf,
        /// Outside both trees; symlinks point here.
        outside: PathBuf,
    }

    fn write(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn meta(sid: &str, name: &str, stopped: Option<&str>) -> String {
        json!({
            "sessionId": sid,
            "pid": 4242,
            "name": name,
            "cwd": "/repo",
            "spawnedAt": "2026-10-01T10:00:00Z",
            "spawnedBy": PM,
            "pmPid": 7,
            "spawnArgs": {"appendSystemPrompt": null, "permissionMode": "default",
                          "model": null, "addDirs": []},
            "stoppedAt": stopped,
        })
        .to_string()
    }

    fn event(eid: &str, sid: &str) -> String {
        json!({"eventId": eid, "sessionId": sid, "spawnedBy": PM, "status": "done",
               "finishedAt": "2026-10-01T11:00:00Z", "resultExcerpt": "ok"})
        .to_string()
    }

    /// Two workers, two inbox events, one adoption of W1 and of a worker
    /// that no longer exists.
    fn fixture() -> Fixture {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("legacy");
        let outside = t.path().join("outside");
        write(&outside.join("meta.json"), &meta(GONE, "outside", None));
        let w = src.join("workers");
        write(&w.join(W1).join("meta.json"), &meta(W1, "one", None));
        write(
            &w.join(W2).join("meta.json"),
            &meta(W2, "two", Some("2026-10-01T12:00:00Z")),
        );
        write(&w.join(W1).join("stdout.log"), "not copied");
        write(
            &src.join("inbox").join(W1).join("e1.json"),
            &event("20261001T110000.000Z-a", W1),
        );
        write(
            &src.join("inbox").join(W2).join("e2.json"),
            &event("20261001T110000.000Z-b", W2),
        );
        write(
            &src.join("adoptions").join(format!("{}.json", PM)),
            &json!({"pmSessionId": PM, "adoptedAt": "2026-10-01T10:30:00Z",
                    "workerIds": [W1, GONE]})
            .to_string(),
        );
        Fixture {
            home: t.path().join("engine"),
            src,
            outside,
            _t: t,
        }
    }

    /// Path, kind, link target or contents, mode and mtime of every entry.
    fn tree_hash(root: &Path) -> BTreeMap<PathBuf, (String, Vec<u8>, u32, i64)> {
        fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, (String, Vec<u8>, u32, i64)>) {
            for e in fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                let md = fs::symlink_metadata(&p).unwrap();
                let rel = p.strip_prefix(root).unwrap().to_path_buf();
                let ft = md.file_type();
                let (kind, body) = if ft.is_symlink() {
                    (
                        "link",
                        fs::read_link(&p).unwrap().into_os_string().into_vec(),
                    )
                } else if ft.is_dir() {
                    walk(root, &p, out);
                    ("dir", Vec::new())
                } else {
                    ("file", fs::read(&p).unwrap())
                };
                out.insert(
                    rel,
                    (
                        kind.into(),
                        body,
                        md.mode(),
                        md.mtime_nsec() + md.mtime() * 1_000_000_000,
                    ),
                );
            }
        }
        let mut out = BTreeMap::new();
        walk(root, root, &mut out);
        out
    }

    fn legacy_rows(db: &Db) -> Vec<AgentRow> {
        db.agents(AgentFilter::All)
            .unwrap()
            .into_iter()
            .filter(|r| r.source == "legacy_import")
            .collect()
    }

    fn by_sid<'a>(rows: &'a [AgentRow], sid: &str) -> &'a AgentRow {
        rows.iter()
            .find(|r| r.provider_sid.as_deref() == Some(sid))
            .unwrap()
    }

    fn legacy_topics(db: &Db) -> Vec<(String, Option<String>)> {
        db.reader()
            .unwrap()
            .prepare("SELECT topic, subject FROM events WHERE topic = 'legacy.inbox' ORDER BY seq")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn count(db: &Db, sql: &str) -> i64 {
        db.reader()
            .unwrap()
            .query_row(sql, [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn a_fixture_imports_workers_parents_and_events() {
        let f = fixture();
        let db = db::open(&f.home, M).unwrap();
        let r = import(&db, &f.home, &f.src).unwrap();
        // 2 metas, 2 events, 1 adoption; no stdout.log.
        assert_eq!(r.copied, 5);
        assert_eq!((r.bad, r.symlinks, r.skipped), (0, 0, 0));
        assert_eq!(r.events, 2);
        assert_eq!(r.dangling, 1);
        let snap = r.snapshot.clone().unwrap();
        assert!(snap.starts_with(import_dir(&f.home)));
        assert!(snap.join("workers").join(W1).join("meta.json").is_file());
        assert!(!snap.join("workers").join(W1).join("stdout.log").exists());

        let rows = legacy_rows(&db);
        assert_eq!(rows.len(), 3);
        let (w1, w2, pm) = (by_sid(&rows, W1), by_sid(&rows, W2), by_sid(&rows, PM));
        assert_eq!((w1.role.as_str(), w1.control.as_str()), ("worker", "none"));
        assert_eq!(w1.provider, PROVIDER);
        assert_eq!(w1.name.as_deref(), Some("one"));
        assert_eq!(w1.cwd.as_deref(), Some("/repo"));
        assert_eq!(w1.pid, Some(4242));
        assert_eq!(w1.machine_id.as_deref(), Some(M));
        assert_eq!(w1.started_at, iso_ms(Some("2026-10-01T10:00:00Z")));
        // No stoppedAt (the daemon never writes one): gone at import.
        assert!(w1.gone_at.is_some_and(|t| t >= w1.created_at.unwrap()));
        assert_eq!(w1.session_key, None);
        assert_eq!(w2.gone_at, iso_ms(Some("2026-10-01T12:00:00Z")));
        assert_eq!(pm.role, "orch");
        assert!(pm.gone_at.is_some());
        // History only: the default agent.list has none of them.
        assert!(db.agents(AgentFilter::Live).unwrap().is_empty());
        assert_eq!(w1.parent_id.as_deref(), Some(pm.id.as_str()));
        assert_eq!(w2.parent_id, None);
        // Not in session.list: no detected session.
        assert!(db.agents(AgentFilter::Sessions).unwrap().is_empty());
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM session_alias WHERE generation = 0"
            ),
            3
        );

        let topics = legacy_topics(&db);
        assert_eq!(
            topics,
            [
                ("legacy.inbox".into(), Some(format!("{}/{}", M, w1.id))),
                ("legacy.inbox".into(), Some(format!("{}/{}", M, w2.id))),
            ]
        );
        let (origin, payload): (String, String) = db
            .reader()
            .unwrap()
            .query_row(
                "SELECT origin, payload FROM events WHERE topic = 'legacy.inbox' LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&origin).unwrap()["legacy"],
            true
        );
        let payload: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(payload["legacy"], true);
        assert_eq!(payload["event"]["status"], "done");
    }

    #[test]
    fn a_running_legacy_worker_is_listed_once_by_its_detected_row() {
        let f = fixture();
        let db = db::open(&f.home, M).unwrap();
        import(&db, &f.home, &f.src).unwrap();
        let seen = vec![crate::engine::detect::Observed {
            key: format!("claudeCode:{}", W1),
            provider: PROVIDER.into(),
            sid: W1.into(),
            status: "working",
            cwd: "/repo".into(),
            name: "one".into(),
            pid: 4242,
            started_at: None,
        }];
        db.write_blocking(move |tx| {
            crate::engine::detect::apply(tx, M, &seen, now_ms(), &Default::default())
        })
        .unwrap();
        let live = db.agents(AgentFilter::Live).unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].source, "detected");
        assert_eq!(live[0].provider_sid.as_deref(), Some(W1));
    }

    #[test]
    fn a_reimport_writes_nothing_twice_and_picks_up_changes() {
        let f = fixture();
        let db = db::open(&f.home, M).unwrap();
        import(&db, &f.home, &f.src).unwrap();
        let rows = legacy_rows(&db);
        let events = count(&db, "SELECT count(*) FROM events");

        let again = import(&db, &f.home, &f.src).unwrap();
        assert!(again.created.is_empty() && again.updated.is_empty());
        assert_eq!(again.events, 0);
        assert_eq!(legacy_rows(&db), rows);
        assert_eq!(count(&db, "SELECT count(*) FROM events"), events);
        assert_eq!(count(&db, "SELECT count(*) FROM session_alias"), 3);

        // Retention pruned the events: the ledger still holds their ids.
        db.writer()
            .call_blocking(|c| c.execute("DELETE FROM events", []))
            .unwrap()
            .unwrap();
        assert_eq!(import(&db, &f.home, &f.src).unwrap().events, 0);

        // The worker stopped and a new event came: one update, one event.
        write(
            &f.src.join("workers").join(W1).join("meta.json"),
            &meta(W1, "one", Some("2026-10-02T00:00:00Z")),
        );
        write(
            &f.src.join("inbox").join(W1).join("e3.json"),
            &event("20261002T000000.000Z-c", W1),
        );
        let changed = import(&db, &f.home, &f.src).unwrap();
        let w1 = by_sid(&rows, W1).id.clone();
        assert_eq!(changed.updated, vec![w1.clone()]);
        assert_eq!(changed.events, 1);
        let now = legacy_rows(&db);
        assert_eq!(
            by_sid(&now, W1).gone_at,
            iso_ms(Some("2026-10-02T00:00:00Z"))
        );
        assert_eq!(now.len(), 3);

        // Only the newest snapshots are kept.
        let snaps = fs::read_dir(import_dir(&f.home)).unwrap().count();
        assert_eq!(snaps, KEEP_SNAPSHOTS);
    }

    #[test]
    fn the_source_tree_is_unchanged_even_with_a_dangling_adoption() {
        let f = fixture();
        // A sidecar whose only worker is gone: `read_filter` would delete it.
        write(
            &f.src.join("adoptions").join("bbbbbbbb.json"),
            &json!({"pmSessionId": "bbbbbbbb", "adoptedAt": "2026-10-01T10:00:00Z",
                    "workerIds": [GONE]})
            .to_string(),
        );
        let before = tree_hash(&f.src);
        let db = db::open(&f.home, M).unwrap();
        let r = import(&db, &f.home, &f.src).unwrap();
        import(&db, &f.home, &f.src).unwrap();
        assert_eq!(tree_hash(&f.src), before);
        assert_eq!(r.dangling, 1);
        // The PM of a dangling-only adoption gets no row.
        let rows = legacy_rows(&db);
        assert!(rows
            .iter()
            .all(|r| r.provider_sid.as_deref() != Some("bbbbbbbb")));
        assert!(rows.iter().all(|r| r.provider_sid.as_deref() != Some(GONE)));
    }

    #[test]
    fn symlinks_in_the_source_are_skipped() {
        let f = fixture();
        let w = f.src.join("workers");
        // A worker dir that is a link, and a meta.json that is a link.
        symlink(&f.outside, w.join("linked-dir")).unwrap();
        fs::create_dir_all(w.join("linked-meta")).unwrap();
        symlink(
            f.outside.join("meta.json"),
            w.join("linked-meta").join("meta.json"),
        )
        .unwrap();
        // Inbox: a linked event file and a linked worker dir.
        symlink(
            f.outside.join("meta.json"),
            f.src.join("inbox").join(W1).join("link.json"),
        )
        .unwrap();
        symlink(&f.outside, f.src.join("inbox").join("linked")).unwrap();
        symlink(
            f.outside.join("meta.json"),
            f.src.join("adoptions").join("link.json"),
        )
        .unwrap();
        let before = tree_hash(&f.src);

        let db = db::open(&f.home, M).unwrap();
        let r = import(&db, &f.home, &f.src).unwrap();
        assert_eq!(r.symlinks, 5);
        assert_eq!(r.copied, 5);
        assert_eq!(r.bad, 0);
        let rows = legacy_rows(&db);
        assert!(rows.iter().all(|r| r.provider_sid.as_deref() != Some(GONE)));
        assert_eq!(rows.len(), 3);
        // Nothing in the copy is a link or came through one.
        let snap = tree_hash(r.snapshot.as_ref().unwrap());
        assert!(snap.values().all(|(kind, ..)| kind != "link"));
        assert!(!snap.keys().any(|p| p.to_string_lossy().contains("link")));
        assert_eq!(tree_hash(&f.src), before);

        // A legacy root that is itself a link imports nothing.
        let t = tempfile::tempdir().unwrap();
        let linked_root = t.path().join("c9watch");
        symlink(&f.src, &linked_root).unwrap();
        let home2 = t.path().join("engine");
        let db2 = db::open(&home2, M).unwrap();
        let r = import(&db2, &home2, &linked_root).unwrap();
        assert_eq!((r.symlinks, r.copied, r.snapshot), (1, 0, None));
        assert!(legacy_rows(&db2).is_empty());
    }

    #[test]
    fn a_failed_import_removes_its_snapshot() {
        let f = fixture();
        let db = db::open(&f.home, M).unwrap();
        import(&db, &f.home, &f.src).unwrap();
        db.close();
        assert!(import(&db, &f.home, &f.src).is_err());
        assert!(import(&db, &f.home, &f.src).is_err());
        assert_eq!(fs::read_dir(import_dir(&f.home)).unwrap().count(), 1);
    }

    #[test]
    fn a_missing_source_creates_nothing_and_caps_hold() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("engine");
        let db = db::open(&home, M).unwrap();
        let r = import(&db, &home, &t.path().join("nope")).unwrap();
        assert_eq!(r, Report::default());
        assert!(!import_dir(&home).exists());

        // An oversized file, a FIFO and a corrupt meta are skipped or bad.
        let f = fixture();
        let big = "x".repeat(MAX_FILE_BYTES as usize + 1);
        write(&f.src.join("inbox").join(W1).join("big.json"), &big);
        let fifo = CString::new(
            f.src
                .join("inbox")
                .join(W1)
                .join("fifo.json")
                .as_os_str()
                .as_bytes(),
        )
        .unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        write(&f.src.join("workers").join("bad").join("meta.json"), "{");
        let db = db::open(&f.home, M).unwrap();
        let r = import(&db, &f.home, &f.src).unwrap();
        assert_eq!(r.skipped, 2);
        assert_eq!(r.bad, 1);
        assert_eq!(legacy_rows(&db).len(), 3);
    }
}
