//! The single db writer (A:340, D12): one OS thread owns the only writable
//! connection and runs jobs in the order they arrive. Callers never hold a
//! connection across an await, and two writes never race for the lock.
//!
//! A job is a closure on `&mut Connection`, so a caller can put a state
//! change and its event row in one transaction (A:216-217).
//!
//! Backpressure (D11): the job queue is bounded (`QUEUE_CAPACITY`). When
//! it is full, `call_blocking` blocks and `call` waits (without blocking
//! the runtime) until the writer takes a job, so a stuck or slow disk
//! slows the producers down instead of growing memory without limit.
//!
//! Why the bound adds no deadlock: every caller already waits for its
//! job's reply, so a caller that waits on a full queue would otherwise
//! wait on its reply; any lock it holds is held for the same span either
//! way. There is no fire-and-forget send. The one new trap would be a job
//! that calls the writer itself (it waits on a queue only it can drain);
//! that already hung on its reply before, and `send` now refuses it with
//! an error instead.

use rusqlite::Connection;
use std::sync::{mpsc, Mutex};
use std::thread::{JoinHandle, ThreadId};
use std::time::Duration;

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

/// Error text when the writer thread is gone or a job panicked.
const WRITER_STOPPED: &str = "db writer stopped";

/// Error text when a job calls its own writer.
const WRITER_REENTRANT: &str = "db writer called from one of its own jobs";

/// Jobs that may wait in the queue. Each caller waits for its own reply,
/// so the queue depth is about the number of concurrent writers (detection,
/// one supervisor per agent, API requests): tens in practice. 256 leaves
/// ample room for bursts; a full queue means the writer is stuck, and then
/// producers should wait rather than pile up closures.
pub const QUEUE_CAPACITY: usize = 256;

/// How long `call` sleeps between tries while the queue is full.
const FULL_BACKOFF_MAX: Duration = Duration::from_millis(10);

pub struct Writer {
    tx: Mutex<Option<mpsc::SyncSender<Job>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    thread_id: ThreadId,
}

impl Writer {
    /// Move `conn` to a new writer thread.
    pub fn spawn(conn: Connection) -> Result<Self, String> {
        Self::spawn_with_capacity(conn, QUEUE_CAPACITY)
    }

    /// `spawn` with a queue of `capacity` jobs (tests use a small one).
    fn spawn_with_capacity(mut conn: Connection, capacity: usize) -> Result<Self, String> {
        let (tx, rx) = mpsc::sync_channel::<Job>(capacity);
        let thread = std::thread::Builder::new()
            .name("c9-engine-db-writer".into())
            .spawn(move || {
                for job in rx {
                    // A panicking job drops its reply sender, so its caller
                    // gets an error; an open transaction rolls back on drop.
                    // The writer itself keeps going.
                    let conn = &mut conn;
                    let _ =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || job(conn)));
                }
            })
            .map_err(|e| format!("Failed to start the db writer thread: {}", e))?;
        Ok(Writer {
            tx: Mutex::new(Some(tx)),
            thread_id: thread.thread().id(),
            thread: Mutex::new(Some(thread)),
        })
    }

    /// A sender for one job. Cloned out of the lock, so a producer that
    /// waits on a full queue never holds up `close` or other producers.
    fn sender(&self) -> Result<mpsc::SyncSender<Job>, String> {
        if std::thread::current().id() == self.thread_id {
            return Err(WRITER_REENTRANT.to_string());
        }
        self.tx
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .ok_or_else(|| WRITER_STOPPED.to_string())
    }

    /// Queue `job`, blocking this thread while the queue is full.
    fn send_blocking(&self, job: Job) -> Result<(), String> {
        self.sender()?
            .send(job)
            .map_err(|_| WRITER_STOPPED.to_string())
    }

    /// Queue `job`, sleeping (not blocking the runtime) while the queue is
    /// full.
    async fn send_async(&self, mut job: Job) -> Result<(), String> {
        let tx = self.sender()?;
        let mut backoff = Duration::from_millis(1);
        loop {
            match tx.try_send(job) {
                Ok(()) => return Ok(()),
                Err(mpsc::TrySendError::Disconnected(_)) => return Err(WRITER_STOPPED.to_string()),
                Err(mpsc::TrySendError::Full(j)) => {
                    job = j;
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(FULL_BACKOFF_MAX);
                }
            }
        }
    }

    /// Finish the queued jobs, then close the connection (which checkpoints
    /// the WAL) before returning, so the caller can release the engine lock
    /// knowing the db is closed. Later calls fail with `db writer stopped`.
    /// Idempotent; `Drop` calls it too.
    pub fn close(&self) {
        drop(self.tx.lock().unwrap_or_else(|p| p.into_inner()).take());
        let thread = self.thread.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(t) = thread {
            let _ = t.join();
        }
    }

    /// Run `f` on the writer thread and wait for it without blocking the
    /// async runtime.
    pub async fn call<R, F>(&self, f: F) -> Result<R, String>
    where
        F: FnOnce(&mut Connection) -> R + Send + 'static,
        R: Send + 'static,
    {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_async(Box::new(move |c| {
            let _ = reply.send(f(c));
        }))
        .await?;
        rx.await.map_err(|_| WRITER_STOPPED.to_string())
    }

    /// `call` for code outside the async runtime (boot, tests).
    pub fn call_blocking<R, F>(&self, f: F) -> Result<R, String>
    where
        F: FnOnce(&mut Connection) -> R + Send + 'static,
        R: Send + 'static,
    {
        let (reply, rx) = mpsc::sync_channel(1);
        self.send_blocking(Box::new(move |c| {
            let _ = reply.send(f(c));
        }))?;
        rx.recv().map_err(|_| WRITER_STOPPED.to_string())
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writer() -> Writer {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (x INTEGER)").unwrap();
        Writer::spawn(conn).unwrap()
    }

    #[test]
    fn jobs_run_in_order_on_one_thread() {
        let w = writer();
        let me = std::thread::current().id();
        let mut threads = Vec::new();
        for i in 0..20 {
            threads.push(
                w.call_blocking(move |c| {
                    c.execute("INSERT INTO t (x) VALUES (?1)", [i]).unwrap();
                    std::thread::current().id()
                })
                .unwrap(),
            );
        }
        assert!(threads.iter().all(|t| *t == threads[0] && *t != me));
        let xs: Vec<i64> = w
            .call_blocking(|c| {
                let mut st = c.prepare("SELECT x FROM t ORDER BY rowid").unwrap();
                let rows = st.query_map([], |r| r.get(0)).unwrap();
                rows.map(Result::unwrap).collect()
            })
            .unwrap();
        assert_eq!(xs, (0..20).collect::<Vec<_>>());
    }

    #[test]
    fn panicking_job_errors_and_rolls_back_but_writer_survives() {
        let w = writer();
        let err = w
            .call_blocking(|c| {
                let tx = c.transaction().unwrap();
                tx.execute("INSERT INTO t (x) VALUES (1)", []).unwrap();
                panic!("boom");
            })
            .map(|_: ()| ())
            .unwrap_err();
        assert_eq!(err, WRITER_STOPPED);
        let n: i64 = w
            .call_blocking(|c| {
                c.query_row("SELECT count(*) FROM t", [], |r| r.get(0))
                    .unwrap()
            })
            .unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn async_call_returns_the_result() {
        let w = writer();
        let n = w
            .call(|c| c.execute("INSERT INTO t (x) VALUES (7)", []).unwrap())
            .await
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn close_finishes_queued_jobs_then_refuses_new_ones() {
        let w = writer();
        w.call_blocking(|c| c.execute("INSERT INTO t (x) VALUES (1)", []).unwrap())
            .unwrap();
        w.close();
        w.close();
        let err = w.call_blocking(|_| ()).unwrap_err();
        assert_eq!(err, WRITER_STOPPED);
    }

    /// A writer with a 1-job queue whose thread is parked in a job until
    /// the returned sender fires, with one more job filling the queue.
    fn stuck_writer() -> (std::sync::Arc<Writer>, mpsc::Sender<()>) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (x INTEGER)").unwrap();
        let w = std::sync::Arc::new(Writer::spawn_with_capacity(conn, 1).unwrap());
        let (release, gate) = mpsc::channel::<()>();
        let (started_tx, started) = mpsc::channel::<()>();
        // Job 1 runs and parks; job 2 fills the queue.
        w.send_blocking(Box::new(move |_| {
            started_tx.send(()).unwrap();
            gate.recv().unwrap();
        }))
        .unwrap();
        started.recv().unwrap();
        w.send_blocking(Box::new(|c| {
            c.execute("INSERT INTO t (x) VALUES (1)", []).unwrap();
        }))
        .unwrap();
        (w, release)
    }

    #[test]
    fn a_full_queue_blocks_the_producer_until_the_writer_catches_up() {
        let (w, release) = stuck_writer();
        let (done_tx, done) = mpsc::channel();
        let w2 = w.clone();
        std::thread::spawn(move || {
            let r = w2.call_blocking(|c| {
                c.query_row("SELECT count(*) FROM t", [], |r| r.get::<_, i64>(0))
                    .unwrap()
            });
            done_tx.send(r).unwrap();
        });
        // The queue is full: the producer must still be waiting.
        assert!(done.recv_timeout(Duration::from_millis(200)).is_err());
        release.send(()).unwrap();
        let n = done.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
        assert_eq!(n, 1, "jobs keep their order under backpressure");
    }

    #[tokio::test]
    async fn async_call_waits_on_a_full_queue_without_blocking_the_runtime() {
        let (w, release) = stuck_writer();
        let w2 = w.clone();
        let call = tokio::spawn(async move { w2.call(|_| 5).await });
        // The runtime (one thread here) still runs other tasks.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!call.is_finished());
        release.send(()).unwrap();
        assert_eq!(call.await.unwrap().unwrap(), 5);
    }

    #[test]
    fn a_job_that_calls_its_own_writer_gets_an_error_not_a_hang() {
        let w = std::sync::Arc::new(writer());
        let inner = w.clone();
        let r = w
            .call_blocking(move |_| inner.call_blocking(|_| ()).unwrap_err())
            .unwrap();
        assert_eq!(r, WRITER_REENTRANT);
    }
}
