//! The event log and its cursor (D3, A:214-220).
//!
//! - **Cursor.** `machine:epoch:seq`. `seq` is the `events` rowid: global
//!   and gap-free within an epoch, since a rolled-back insert takes no
//!   rowid and retention never deletes the newest row (so `max(seq)` never
//!   goes back and SQLite never reuses a rowid).
//! - **Outbox.** An event row is inserted in the same transaction as the
//!   state change it reports (`Db::write`). After the commit only the new
//!   head `seq` is broadcast, on a `watch` channel: it never blocks and
//!   never queues, so a slow subscriber cannot hold up the writer.
//! - **Subscribe.** A subscriber keeps its own `seq` and reads every event
//!   after it from SQLite, in batches. The broadcast only wakes it. A
//!   subscriber that lags just reads more rows; no event is dropped.
//! - **Retention.** 24 h or 50k events, whichever comes first. A cursor
//!   below the low-water mark, or with another epoch or machine, gets
//!   `cursor_expired`; the client then calls `snapshot.get` and subscribes
//!   from the cursor it returns.

use rusqlite::{params, Connection};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

/// Events older than this are pruned.
pub const RETENTION_MS: i64 = 24 * 60 * 60 * 1000;
/// At most this many events are kept.
pub const RETENTION_EVENTS: i64 = 50_000;
/// Rows a subscriber reads per query.
pub const READ_BATCH: usize = 256;

/// A position in the event log: the client has seen every event up to and
/// including `seq`. `seq` 0 is the start of an epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    pub machine: String,
    pub epoch: String,
    pub seq: i64,
}

impl Cursor {
    /// Parse `machine:epoch:seq`.
    pub fn parse(s: &str) -> Result<Cursor, String> {
        let parts: Vec<&str> = s.split(':').collect();
        let bad = || format!("bad cursor {:?}, want machine:epoch:seq", s);
        let [machine, epoch, seq] = parts.as_slice() else {
            return Err(bad());
        };
        if machine.is_empty() || epoch.is_empty() {
            return Err(bad());
        }
        let seq: i64 = seq.parse().map_err(|_| bad())?;
        if seq < 0 {
            return Err(bad());
        }
        Ok(Cursor {
            machine: machine.to_string(),
            epoch: epoch.to_string(),
            seq,
        })
    }
}

impl std::fmt::Display for Cursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}:{}", self.machine, self.epoch, self.seq)
    }
}

/// An event before it is stored.
#[derive(Debug, Clone)]
pub struct NewEvent {
    pub topic: String,
    pub subject: Option<String>,
    /// The `origin` object, written by the engine (A:209).
    pub origin: Option<Value>,
    pub data: Value,
}

/// A stored event.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub seq: i64,
    pub topic: String,
    pub subject: Option<String>,
    pub origin: Option<Value>,
    pub data: Value,
    /// Unix ms.
    pub ts: i64,
}

/// Why reading from a cursor failed.
#[derive(Debug, Clone, PartialEq)]
pub enum ReadError {
    /// The cursor is from another epoch or machine, below the low-water
    /// mark, or ahead of the log. Maps to `cursor_expired`.
    Expired(String),
    /// A db failure.
    Db(String),
}

impl From<rusqlite::Error> for ReadError {
    fn from(e: rusqlite::Error) -> Self {
        ReadError::Db(e.to_string())
    }
}

/// Insert `e` at `ts`. Call it inside the transaction of the state change.
/// Returns its `seq`.
pub fn append(conn: &Connection, epoch: &str, ts: i64, e: &NewEvent) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO events (epoch, topic, subject, origin, payload, ts) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            epoch,
            e.topic,
            e.subject,
            e.origin.as_ref().map(Value::to_string),
            e.data.to_string(),
            ts
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// The newest `seq`, or 0 when the log is empty.
pub fn head(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("SELECT coalesce(max(seq), 0) FROM events", [], |r| r.get(0))
}

/// The oldest `seq` still stored.
fn oldest(conn: &Connection) -> rusqlite::Result<Option<i64>> {
    conn.query_row("SELECT min(seq) FROM events", [], |r| r.get(0))
}

/// Delete events older than `now - RETENTION_MS` and all but the newest
/// `RETENTION_EVENTS`. Returns how many went.
pub fn prune(conn: &Connection, now: i64) -> rusqlite::Result<usize> {
    prune_with(conn, now - RETENTION_MS, RETENTION_EVENTS)
}

/// `prune` with explicit limits. Deletes only a prefix of the log, up to
/// the newest event older than `cutoff_ts` or the `keep` limit: `ts` is
/// wall-clock time and can go back, and a hole in the middle would let a
/// subscriber below it skip events without `cursor_expired`. The newest row
/// always stays, so `seq` is never reused. Touches nothing but `events`
/// (audit has its own retention, `audit::prune`).
pub fn prune_with(conn: &Connection, cutoff_ts: i64, keep: i64) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM events WHERE seq < (SELECT max(seq) FROM events) \
         AND seq <= max( \
             (SELECT coalesce(max(seq), 0) FROM events WHERE ts < ?1), \
             (SELECT max(seq) FROM events) - ?2)",
        params![cutoff_ts, keep],
    )
}

/// Delete the `session.view` events `seqs`: a newer view of the same
/// session (or its `session.gone`) supersedes each. `session.view` is a
/// latest-state topic: a subscriber reading on from any cursor still gets
/// the newest view of every live session, but not every view between.
/// The oldest stored event stays, so compaction never moves the
/// `cursor_expired` line (only `prune` does). Returns how many went.
pub fn compact_views(conn: &Connection, seqs: &[i64]) -> rusqlite::Result<usize> {
    let mut stmt = conn.prepare_cached(
        "DELETE FROM events WHERE seq = ?1 AND topic = 'session.view' \
         AND seq > (SELECT min(seq) FROM events)",
    )?;
    let mut n = 0;
    for seq in seqs {
        n += stmt.execute(params![seq])?;
    }
    Ok(n)
}

/// Delete every `session.view` event before `seq` but the oldest stored
/// event: the views an earlier engine run wrote, superseded by the first
/// tick of this one.
pub fn compact_views_before(conn: &Connection, seq: i64) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM events WHERE topic = 'session.view' AND seq < ?1 \
         AND seq > (SELECT min(seq) FROM events)",
        params![seq],
    )
}

/// Check that a client that has seen up to `after` can read on without a
/// gap: `after` is not ahead of the log and nothing after it was pruned.
fn check_after(conn: &Connection, after: i64) -> Result<(), ReadError> {
    let head = head(conn)?;
    if after > head {
        return Err(ReadError::Expired(format!(
            "cursor seq {} is ahead of the event log (head {})",
            after, head
        )));
    }
    if let Some(oldest) = oldest(conn)? {
        if after + 1 < oldest {
            return Err(ReadError::Expired(format!(
                "events after seq {} were pruned (oldest kept is {})",
                after, oldest
            )));
        }
    }
    Ok(())
}

/// Check `since` against this log: same machine, same epoch, and readable
/// without a gap. Returns its `seq`.
pub fn check_cursor(
    conn: &Connection,
    since: &Cursor,
    machine: &str,
    epoch: &str,
) -> Result<i64, ReadError> {
    if since.machine != machine {
        return Err(ReadError::Expired(format!(
            "cursor is from machine {}, this is {}",
            since.machine, machine
        )));
    }
    if since.epoch != epoch {
        return Err(ReadError::Expired(format!(
            "cursor is from epoch {}, the db is now at epoch {}",
            since.epoch, epoch
        )));
    }
    check_after(conn, since.seq)?;
    Ok(since.seq)
}

/// Up to `limit` events after `after`, oldest first. Fails with `Expired`
/// when events after `after` were already pruned.
pub fn read_after(conn: &Connection, after: i64, limit: usize) -> Result<Vec<Event>, ReadError> {
    // One read transaction, so a prune cannot land between the check and
    // the read.
    conn.execute_batch("BEGIN")?;
    let out = check_after(conn, after).and_then(|_| read_rows(conn, after, limit));
    conn.execute_batch("COMMIT")?;
    out
}

fn read_rows(conn: &Connection, after: i64, limit: usize) -> Result<Vec<Event>, ReadError> {
    let mut st = conn.prepare_cached(
        "SELECT seq, topic, subject, origin, payload, ts FROM events \
         WHERE seq > ?1 ORDER BY seq LIMIT ?2",
    )?;
    let rows = st.query_map(params![after, limit as i64], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, i64>(5)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (seq, topic, subject, origin, payload, ts) = row?;
        out.push(Event {
            seq,
            topic,
            subject,
            origin: origin.as_deref().map(parse_json).transpose()?,
            data: payload.as_deref().map_or(Ok(Value::Null), parse_json)?,
            ts,
        });
    }
    Ok(out)
}

fn parse_json(s: &str) -> Result<Value, ReadError> {
    serde_json::from_str(s).map_err(|e| ReadError::Db(format!("bad JSON in events: {}", e)))
}

/// One subscriber's position in the log. Every event comes from SQLite; the
/// head broadcast only says when to read again.
pub struct Subscription {
    reader: Arc<Mutex<Connection>>,
    head: watch::Receiver<i64>,
    after: i64,
    batch: usize,
}

impl Subscription {
    /// Read events after `after` from `reader`, woken by `head`. Create the
    /// receiver before the first read, so no commit is missed.
    pub fn new(reader: Connection, head: watch::Receiver<i64>, after: i64, batch: usize) -> Self {
        Subscription {
            reader: Arc::new(Mutex::new(reader)),
            head,
            after,
            batch: batch.max(1),
        }
    }

    /// The `seq` of the last event returned.
    pub fn after(&self) -> i64 {
        self.after
    }

    /// The next events after the cursor, waiting until there is at least
    /// one. `Ok(None)` when the store is gone.
    pub async fn next(&mut self) -> Result<Option<Vec<Event>>, ReadError> {
        loop {
            let (reader, after, batch) = (self.reader.clone(), self.after, self.batch);
            let got = tokio::task::spawn_blocking(move || {
                let conn = reader.lock().unwrap_or_else(|p| p.into_inner());
                read_after(&conn, after, batch)
            })
            .await
            .map_err(|e| ReadError::Db(format!("event reader failed: {}", e)))??;
            if let Some(last) = got.last() {
                self.after = last.seq;
                return Ok(Some(got));
            }
            // Nothing new. A commit since the last wake marks the channel
            // changed, so this returns at once if one landed during the read.
            if self.head.changed().await.is_err() {
                return Ok(None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cursor_round_trips_and_rejects_junk() {
        let c = Cursor::parse("ab12:ep:42").unwrap();
        assert_eq!(
            c,
            Cursor {
                machine: "ab12".into(),
                epoch: "ep".into(),
                seq: 42
            }
        );
        assert_eq!(c.to_string(), "ab12:ep:42");
        for bad in ["", "a:b", "a:b:c:1", "a:b:x", "a:b:-1", ":b:1", "a::1"] {
            assert!(Cursor::parse(bad).is_err(), "{:?}", bad);
        }
    }

    fn mem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(include_str!("migrations/0001_init.sql"))
            .unwrap();
        c
    }

    fn ev(n: i64) -> NewEvent {
        NewEvent {
            topic: "t".into(),
            subject: Some(format!("m/{}", n)),
            origin: Some(json!({"kind": "system"})),
            data: json!({"n": n}),
        }
    }

    fn seqs(conn: &Connection) -> Vec<i64> {
        conn.prepare("SELECT seq FROM events ORDER BY seq")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn events_round_trip() {
        let c = mem();
        assert_eq!(head(&c).unwrap(), 0);
        assert_eq!(append(&c, "e", 5, &ev(1)).unwrap(), 1);
        let got = read_after(&c, 0, 10).unwrap();
        assert_eq!(
            got,
            [Event {
                seq: 1,
                topic: "t".into(),
                subject: Some("m/1".into()),
                origin: Some(json!({"kind": "system"})),
                data: json!({"n": 1}),
                ts: 5,
            }]
        );
        assert!(read_after(&c, 1, 10).unwrap().is_empty());
    }

    #[test]
    fn retention_keeps_24h_or_50k_and_always_the_newest() {
        let c = mem();
        for i in 1..=10 {
            append(&c, "e", i * 100, &ev(i)).unwrap();
        }
        // Count limit: keep the newest 4.
        assert_eq!(prune_with(&c, 0, 4).unwrap(), 6);
        assert_eq!(seqs(&c), [7, 8, 9, 10]);
        // Age limit: everything before ts 900 goes.
        assert_eq!(prune_with(&c, 900, 50).unwrap(), 2);
        assert_eq!(seqs(&c), [9, 10]);
        // All too old: the newest row stays, so seq is never reused.
        prune_with(&c, i64::MAX, 50).unwrap();
        assert_eq!(seqs(&c), [10]);
        assert_eq!(append(&c, "e", 1, &ev(11)).unwrap(), 11);

        // The real limits: a day old goes, a fresh one stays.
        let now = 10 * RETENTION_MS;
        append(&c, "e", now - RETENTION_MS - 1, &ev(12)).unwrap();
        append(&c, "e", now, &ev(13)).unwrap();
        prune(&c, now).unwrap();
        assert_eq!(seqs(&c), [13]);
    }

    #[test]
    fn retention_by_age_deletes_only_a_prefix_when_the_clock_goes_back() {
        let c = mem();
        // The clock stepped back between seq 2 and 3.
        for (i, ts) in [100, 500, 110, 120, 600].into_iter().enumerate() {
            append(&c, "e", ts, &ev(i as i64 + 1)).unwrap();
        }
        // Seq 4 is older than the cutoff, so everything up to it goes,
        // seq 2 included; nothing after a kept row is deleted.
        assert_eq!(prune_with(&c, 200, 50).unwrap(), 4);
        assert_eq!(seqs(&c), [5]);

        let c = mem();
        for (i, ts) in [100, 500, 110, 900].into_iter().enumerate() {
            append(&c, "e", ts, &ev(i as i64 + 1)).unwrap();
        }
        // Only seq 1 and 3 are old; 3 drags 2 with it. A subscriber at 2
        // must not pass the gap check and then miss 3.
        prune_with(&c, 200, 50).unwrap();
        assert_eq!(seqs(&c), [4]);
        assert!(expired(read_after(&c, 2, 10)).contains("pruned"));
        assert_eq!(read_after(&c, 3, 10).unwrap()[0].seq, 4);
    }

    #[test]
    fn retention_never_touches_audit() {
        let c = mem();
        for i in 0..5 {
            super::super::audit::append(
                &c,
                i,
                &super::super::audit::AuditEntry {
                    principal: "local-unverified",
                    method: "agent.create",
                    decision: "allow",
                    ..Default::default()
                },
            )
            .unwrap();
            append(&c, "e", i, &ev(i)).unwrap();
        }
        prune_with(&c, i64::MAX, 0).unwrap();
        let n: i64 = c
            .query_row("SELECT count(*) FROM audit", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 5);
    }

    fn expired(r: Result<impl std::fmt::Debug, ReadError>) -> String {
        match r {
            Err(ReadError::Expired(m)) => m,
            other => panic!("want Expired, got {:?}", other),
        }
    }

    #[test]
    fn old_epoch_other_machine_pruned_or_ahead_cursors_expire() {
        let c = mem();
        for i in 1..=5 {
            append(&c, "e1", i, &ev(i)).unwrap();
        }
        let cur = |m: &str, e: &str, s: i64| Cursor {
            machine: m.into(),
            epoch: e.into(),
            seq: s,
        };
        assert_eq!(check_cursor(&c, &cur("m", "e1", 0), "m", "e1"), Ok(0));
        assert_eq!(check_cursor(&c, &cur("m", "e1", 5), "m", "e1"), Ok(5));
        assert!(expired(check_cursor(&c, &cur("m", "e0", 3), "m", "e1")).contains("epoch"));
        assert!(expired(check_cursor(&c, &cur("x", "e1", 3), "m", "e1")).contains("machine"));
        assert!(expired(check_cursor(&c, &cur("m", "e1", 6), "m", "e1")).contains("ahead"));

        prune_with(&c, 0, 3).unwrap();
        // 3, 4 and 5 are kept: 2 reads on from 3, but 1 lost event 2.
        assert_eq!(check_cursor(&c, &cur("m", "e1", 2), "m", "e1"), Ok(2));
        assert!(expired(check_cursor(&c, &cur("m", "e1", 1), "m", "e1")).contains("pruned"));
        assert!(expired(read_after(&c, 0, 10)).contains("pruned"));
    }

    /// A file db, so a writer and a reader can share it.
    fn file_db() -> (tempfile::TempDir, Connection, Connection) {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("e.db");
        let w = Connection::open(&path).unwrap();
        w.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))
            .unwrap();
        w.execute_batch(include_str!("migrations/0001_init.sql"))
            .unwrap();
        let r = Connection::open(&path).unwrap();
        (t, w, r)
    }

    #[tokio::test]
    async fn subscription_catches_up_in_batches_then_waits_for_new_events() {
        let (_t, w, r) = file_db();
        for i in 1..=7 {
            append(&w, "e", i, &ev(i)).unwrap();
        }
        let (tx, rx) = watch::channel(head(&w).unwrap());
        let mut sub = Subscription::new(r, rx, 0, 3);
        let mut got = Vec::new();
        while got.len() < 7 {
            got.extend(
                sub.next()
                    .await
                    .unwrap()
                    .unwrap()
                    .into_iter()
                    .map(|e| e.seq),
            );
        }
        assert_eq!(got, (1..=7).collect::<Vec<_>>());

        // Caught up: `next` waits until a commit is broadcast.
        let waiting = tokio::spawn(async move {
            let batch = sub.next().await.unwrap().unwrap();
            (batch.iter().map(|e| e.seq).collect::<Vec<_>>(), sub)
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!waiting.is_finished());
        append(&w, "e", 8, &ev(8)).unwrap();
        tx.send_replace(8);
        let (batch, sub) = waiting.await.unwrap();
        assert_eq!(batch, [8]);
        assert_eq!(sub.after(), 8);

        // The store going away ends the subscription.
        drop(tx);
        let mut sub = sub;
        assert_eq!(sub.next().await, Ok(None));
    }

    #[tokio::test]
    async fn a_subscriber_that_lags_past_retention_gets_expired() {
        let (_t, w, r) = file_db();
        for i in 1..=6 {
            append(&w, "e", i, &ev(i)).unwrap();
        }
        let (_tx, rx) = watch::channel(head(&w).unwrap());
        let mut sub = Subscription::new(r, rx, 0, 2);
        assert_eq!(sub.next().await.unwrap().unwrap().len(), 2);
        // Retention runs while the subscriber is slow: 3 and 4 are lost.
        prune_with(&w, 0, 2).unwrap();
        assert!(expired(sub.next().await).contains("pruned"));
    }
}
