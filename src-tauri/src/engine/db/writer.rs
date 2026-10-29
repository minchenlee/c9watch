//! The single db writer (A:340, D12): one OS thread owns the only writable
//! connection and runs jobs in the order they arrive. Callers never hold a
//! connection across an await, and two writes never race for the lock.
//!
//! A job is a closure on `&mut Connection`, so a caller can put a state
//! change and its event row in one transaction (A:216-217).

use rusqlite::Connection;
use std::sync::{mpsc, Mutex};
use std::thread::JoinHandle;

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

/// Error text when the writer thread is gone or a job panicked.
const WRITER_STOPPED: &str = "db writer stopped";

pub struct Writer {
    tx: Mutex<Option<mpsc::Sender<Job>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Writer {
    /// Move `conn` to a new writer thread.
    pub fn spawn(mut conn: Connection) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel::<Job>();
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
            thread: Mutex::new(Some(thread)),
        })
    }

    fn send(&self, job: Job) -> Result<(), String> {
        self.tx
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .and_then(|tx| tx.send(job).ok())
            .ok_or_else(|| WRITER_STOPPED.to_string())
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
        self.send(Box::new(move |c| {
            let _ = reply.send(f(c));
        }))?;
        rx.await.map_err(|_| WRITER_STOPPED.to_string())
    }

    /// `call` for code outside the async runtime (boot, tests).
    pub fn call_blocking<R, F>(&self, f: F) -> Result<R, String>
    where
        F: FnOnce(&mut Connection) -> R + Send + 'static,
        R: Send + 'static,
    {
        let (reply, rx) = mpsc::sync_channel(1);
        self.send(Box::new(move |c| {
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
}
