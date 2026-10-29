//! Standing orders (plan C1a-25, A:545-551, D28 Q5=A): what the engine
//! puts in front of every CC agent through the SessionStart hook's
//! `additionalContext`, not a paste (D24.8: CC wraps a paste in
//! `<pasted_content>`, so the model reads it as user text).
//!
//! - The text lives in `<home>/standing-orders.md`. The human edits it; the
//!   spawn profile denies the agent writes to it (C1a-23). On the first
//!   start (no file yet) the engine writes the three default items and
//!   never rewrites the file after that. `{{agent_id}}` in it becomes the
//!   agent's id.
//! - Every SessionStart source gets the orders: `startup`, `resume`,
//!   `compact` (T4, A:550: a compaction drops the earlier context) and
//!   `clear`.
//! - After an interrupt (C1a-27 sets `interrupted_note_pending`), the next
//!   generation's first SessionStart also gets `INTERRUPTED_NOTE`, and the
//!   flag is cleared in the same write transaction as the hook's intake
//!   (A:551). Only a `startup` or `resume` takes it: a new generation
//!   always starts with one of those, so a `compact` of the interrupted
//!   generation itself, or one after the resume, never repeats or eats it.
//!   The flag clears when the engine takes the event, and CC never runs
//!   the hook again, so `hook session-start` waits
//!   `cli::task::SESSION_START_TIMEOUT` (4 s), not 0.5 s, for the reply: a
//!   busy writer must not cost the note (reviews).
//! - The file is read outside the transaction (I/O never holds the single
//!   writer) and with `O_NOFOLLOW`. It is capped at `MAX_ORDERS` bytes.

use super::db::Tx;
use super::hooks::{self, DropReason, HookEvent, Outcome};
use super::merge::MergeTable;
use rusqlite::params;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// `<home>/standing-orders.md`
pub const STANDING_ORDERS_NAME: &str = "standing-orders.md";

/// Replaced by the agent's id in the orders.
pub const AGENT_PLACEHOLDER: &str = "{{agent_id}}";

/// The most of the file the engine sends. CC keeps hook context short;
/// a longer file is cut at a char boundary and says so.
pub const MAX_ORDERS: usize = 8 * 1024;

/// The one-time line after an interrupted generation (A:551).
pub const INTERRUPTED_NOTE: &str = "You were interrupted; check `git status` first.";

/// What the engine writes on the first start: the agent's name, the
/// `c9watch` read commands and the engine socket (D28 Q5=A), nothing else.
pub const DEFAULT_ORDERS: &str = "\
# Standing orders

- You are c9watch agent {{agent_id}}.
- To see the other agent sessions on this machine, use the c9watch CLI read \
commands: `c9watch list`, `c9watch status`, `c9watch view <id> --last <n>`, \
`c9watch history` and `c9watch search <query>`.
- The c9watch engine socket (`$C9_ENGINE_SOCK`) and the `c9watch-agent` hooks \
that talk to it are part of your task. Do not block, remove or work around them.
";

/// `<home>/standing-orders.md`
pub fn path(home: &Path) -> PathBuf {
    home.join(STANDING_ORDERS_NAME)
}

/// Write `DEFAULT_ORDERS` to `<home>/standing-orders.md`, 0600, unless the
/// file (or a link in its place) is already there. Returns whether it
/// wrote. `home` must already be the engine's private directory.
pub fn ensure(home: &Path) -> Result<bool, String> {
    let p = path(home);
    let mut f = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&p)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(e) => return Err(format!("Failed to create {:?}: {}", p, e)),
    };
    f.write_all(DEFAULT_ORDERS.as_bytes())
        .and_then(|_| f.sync_all())
        .map_err(|e| format!("Failed to write {:?}: {}", p, e))?;
    Ok(true)
}

/// Read the orders file, at most `MAX_ORDERS` bytes. `None` when it is
/// missing, a symlink or unreadable (logged): the agent then starts without
/// orders rather than without its hook. Blocking.
pub fn read(path: &Path) -> Option<String> {
    let opened = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .and_then(|f| {
            if f.metadata()?.is_file() {
                Ok(f)
            } else {
                Err(std::io::Error::other("not a regular file"))
            }
        });
    let mut buf = Vec::new();
    let read = opened.and_then(|f| f.take(MAX_ORDERS as u64 + 1).read_to_end(&mut buf));
    if let Err(e) = read {
        eprintln!("[engine] No standing orders from {:?}: {}", path, e);
        return None;
    }
    let cut = buf.len() > MAX_ORDERS;
    buf.truncate(MAX_ORDERS);
    if cut {
        // Drop a char the cut split in two.
        if let Err(e) = std::str::from_utf8(&buf) {
            if e.error_len().is_none() {
                buf.truncate(e.valid_up_to());
            }
        }
    }
    let mut text = String::from_utf8_lossy(&buf).into_owned();
    if cut {
        eprintln!(
            "[engine] {:?} is longer than {} bytes; sending the first part",
            path, MAX_ORDERS
        );
        text.push_str("\n[standing orders cut at the engine's size limit]\n");
    }
    Some(text)
}

/// Clear `interrupted_note_pending` for `agent_id` when this SessionStart
/// starts a generation (`startup` or `resume`). Returns whether the flag
/// was set, that is, whether this reply carries the note.
pub fn take_interrupted_note(
    tx: &mut Tx,
    agent_id: &str,
    source: Option<&str>,
) -> rusqlite::Result<bool> {
    if !matches!(source, Some("startup" | "resume")) {
        return Ok(false);
    }
    let n = tx.execute(
        "UPDATE agents SET interrupted_note_pending = 0 \
         WHERE id = ?1 AND interrupted_note_pending = 1",
        params![agent_id],
    )?;
    Ok(n > 0)
}

/// The `additionalContext` text: the orders for `agent_id`, then the
/// interrupted note. `None` when there is neither.
pub fn context(orders: Option<&str>, agent_id: &str, note: bool) -> Option<String> {
    let mut out = orders
        .map(|o| o.replace(AGENT_PLACEHOLDER, agent_id))
        .filter(|o| !o.trim().is_empty())
        .unwrap_or_default();
    if note {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(INTERRUPTED_NOTE);
        out.push('\n');
    }
    (!out.is_empty()).then_some(out)
}

/// `hook.session_start`: the intake (`engine::hooks`), and in the same
/// transaction the interrupted note. Returns the outcome and the context
/// to send. An accepted event gets the orders; a duplicate (the hook's
/// retry of an answer it may have lost) gets them again, without the note;
/// any other drop gets nothing.
pub fn session_start(
    tx: &mut Tx,
    machine: &str,
    merge: &MergeTable,
    ev: &HookEvent,
    orders: Option<&str>,
    now_ms: i64,
) -> rusqlite::Result<(Outcome, Option<String>)> {
    let out = hooks::intake(tx, machine, merge, ev, now_ms)?;
    let id = ev.params.agent_id.as_str();
    let ctx = match out {
        Outcome::Accepted => {
            let source = ev.params.input["source"].as_str();
            let note = take_interrupted_note(tx, id, source)?;
            context(orders, id, note)
        }
        Outcome::Dropped(DropReason::Duplicate) => context(orders, id, false),
        Outcome::Dropped(_) => None,
    };
    Ok((out, ctx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::db::Db;
    use crate::engine::generation::tests::{db_with_agent, MACHINE};
    use crate::engine::generation::{begin_generation, GenReason};
    use crate::engine::proto::HookParams;
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    const ORDERS: &str = "Be agent {{agent_id}}.\n";

    /// A store with agent a1 at generation 1, state `ready`.
    fn setup() -> (tempfile::TempDir, Db, Arc<MergeTable>) {
        let (t, db) = db_with_agent("a1");
        db.write_blocking(|tx| {
            begin_generation(tx, MACHINE, "a1", GenReason::Spawn, "s1", None, 1)?;
            tx.execute("UPDATE agents SET state = 'ready' WHERE id = 'a1'", [])
        })
        .unwrap();
        (t, db, Arc::new(MergeTable::new()))
    }

    /// One `hook.session_start` with `source` and `seq`, as generation
    /// `gen` (also the live one).
    fn start(
        db: &Db,
        merge: &Arc<MergeTable>,
        gen: i64,
        source: &str,
        seq: &str,
    ) -> (Outcome, Option<String>) {
        let merge = merge.clone();
        let p = HookParams {
            agent_id: "a1".into(),
            kind: "claude".into(),
            seq: seq.into(),
            input: json!({"session_id": "s1", "source": source}),
            truncated: false,
        };
        db.write_blocking(move |tx| {
            let ev = HookEvent {
                method: "hook.session_start",
                principal: "agent:a1",
                gen,
                live_gen: Some(gen),
                params: &p,
            };
            session_start(tx, MACHINE, &merge, &ev, Some(ORDERS), 10)
        })
        .unwrap()
        .0
    }

    fn set_pending(db: &Db) {
        db.write_blocking(|tx| tx.execute("UPDATE agents SET interrupted_note_pending = 1", []))
            .unwrap();
    }

    fn pending(db: &Db) -> bool {
        let conn = db.reader().unwrap();
        conn.query_row("SELECT interrupted_note_pending FROM agents", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap()
            != 0
    }

    fn resume(db: &Db, gen: i64) {
        db.write_blocking(move |tx| {
            begin_generation(tx, MACHINE, "a1", GenReason::Resume, "s1", None, gen)
        })
        .unwrap();
    }

    #[test]
    fn the_first_start_writes_only_the_three_items_once() {
        let t = tempfile::tempdir().unwrap();
        assert!(ensure(t.path()).unwrap());
        let p = path(t.path());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), DEFAULT_ORDERS);
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let items: Vec<&str> = DEFAULT_ORDERS
            .lines()
            .filter(|l| l.starts_with("- "))
            .collect();
        assert_eq!(items.len(), 3, "{:?}", items);
        assert!(items[0].contains(AGENT_PLACEHOLDER));
        assert!(items[1].contains("`c9watch list`"));
        assert!(items[2].contains("engine socket"));
        // The human's edit survives the next start.
        std::fs::write(&p, "mine\n").unwrap();
        assert!(!ensure(t.path()).unwrap());
        assert_eq!(read(&p).as_deref(), Some("mine\n"));
    }

    #[test]
    fn a_link_or_a_missing_file_gives_no_orders() {
        let t = tempfile::tempdir().unwrap();
        let p = path(t.path());
        assert_eq!(read(&p), None);
        let other = t.path().join("other.md");
        std::fs::write(&other, "planted\n").unwrap();
        std::os::unix::fs::symlink(&other, &p).unwrap();
        assert_eq!(read(&p), None);
        // `ensure` does not write through the link either.
        assert!(!ensure(t.path()).unwrap());
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "planted\n");
    }

    #[test]
    fn a_long_file_is_cut_at_a_char_boundary() {
        let t = tempfile::tempdir().unwrap();
        let p = path(t.path());
        // A three-byte char straddles the limit.
        let body = format!("{}漢{}", "a".repeat(MAX_ORDERS - 1), "b".repeat(100));
        std::fs::write(&p, body).unwrap();
        let got = read(&p).unwrap();
        let (head, tail) = got.split_at(MAX_ORDERS - 1);
        assert_eq!(head, "a".repeat(MAX_ORDERS - 1));
        assert_eq!(tail, "\n[standing orders cut at the engine's size limit]\n");
    }

    #[test]
    fn every_source_gets_the_orders() {
        let (_t, db, merge) = setup();
        for (i, source) in ["startup", "resume", "compact", "clear"]
            .into_iter()
            .enumerate()
        {
            let (out, ctx) = start(&db, &merge, 1, source, &i.to_string());
            assert_eq!(out, Outcome::Accepted, "{}", source);
            assert_eq!(ctx.as_deref(), Some("Be agent a1.\n"), "{}", source);
        }
    }

    #[test]
    fn the_interrupted_note_goes_out_once() {
        let (_t, db, merge) = setup();
        // Generation 1 was interrupted; generation 2 resumes.
        set_pending(&db);
        resume(&db, 2);
        let (_, ctx) = start(&db, &merge, 2, "resume", "1");
        assert_eq!(
            ctx.as_deref(),
            Some(format!("Be agent a1.\n\n{}\n", INTERRUPTED_NOTE).as_str())
        );
        assert!(!pending(&db), "cleared with the reply");
        // The compaction after the resume gets the orders only.
        let (_, ctx) = start(&db, &merge, 2, "compact", "2");
        assert_eq!(ctx.as_deref(), Some("Be agent a1.\n"));
        // So does a second start event of the same generation.
        let (_, ctx) = start(&db, &merge, 2, "resume", "3");
        assert_eq!(ctx.as_deref(), Some("Be agent a1.\n"));
    }

    #[test]
    fn a_compact_does_not_take_a_pending_note() {
        let (_t, db, merge) = setup();
        // The flag is set while generation 1 still runs and compacts.
        set_pending(&db);
        let (_, ctx) = start(&db, &merge, 1, "compact", "1");
        assert_eq!(ctx.as_deref(), Some("Be agent a1.\n"));
        assert!(pending(&db));
        // The next generation's first start takes it.
        resume(&db, 2);
        let (_, ctx) = start(&db, &merge, 2, "startup", "2");
        assert!(ctx.unwrap().ends_with(&format!("{}\n", INTERRUPTED_NOTE)));
        assert!(!pending(&db));
    }

    #[test]
    fn drops_do_not_take_the_note() {
        let (_t, db, merge) = setup();
        set_pending(&db);
        resume(&db, 2);
        // A late event from generation 1: dropped, no context, flag kept.
        let merge2 = merge.clone();
        let p = HookParams {
            agent_id: "a1".into(),
            kind: "claude".into(),
            seq: "1".into(),
            input: json!({"source": "startup"}),
            truncated: false,
        };
        let (out, ctx) = db
            .write_blocking(move |tx| {
                let ev = HookEvent {
                    method: "hook.session_start",
                    principal: "agent:a1",
                    gen: 1,
                    live_gen: Some(2),
                    params: &p,
                };
                session_start(tx, MACHINE, &merge2, &ev, Some(ORDERS), 10)
            })
            .unwrap()
            .0;
        assert_eq!(out, Outcome::Dropped(DropReason::StaleGeneration));
        assert_eq!(ctx, None);
        assert!(pending(&db));
        // The real start takes it; its retry (same seq) gets the orders
        // again, without the note.
        let (_, ctx) = start(&db, &merge, 2, "resume", "2");
        assert!(ctx.unwrap().contains(INTERRUPTED_NOTE));
        let (out, ctx) = start(&db, &merge, 2, "resume", "2");
        assert_eq!(out, Outcome::Dropped(DropReason::Duplicate));
        assert_eq!(ctx.as_deref(), Some("Be agent a1.\n"));
    }

    #[test]
    fn context_without_orders() {
        assert_eq!(context(None, "a1", false), None);
        assert_eq!(context(Some("  \n"), "a1", false), None);
        assert_eq!(
            context(None, "a1", true).as_deref(),
            Some(format!("{}\n", INTERRUPTED_NOTE).as_str())
        );
        assert_eq!(
            context(Some("x"), "a1", true).as_deref(),
            Some(format!("x\n\n{}\n", INTERRUPTED_NOTE).as_str())
        );
    }
}
