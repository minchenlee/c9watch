//! Admission control before dispatching synchronous scans to the blocking pool.
use std::sync::{Arc, LazyLock};
use tokio::sync::Semaphore;

pub(crate) static DISCOVERY: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(1)));
// Conversation/archive/image I/O must not consume the single discovery slot.
// Native and WebSocket callers share this gate, not per-transport pools.
pub(crate) static SESSION_IO: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(4)));
pub(crate) static SUBAGENTS: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(1)));
pub(crate) static SUBAGENT_TRANSCRIPT: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(1)));

pub(crate) async fn scan<T: Send + 'static>(
    gate: &Arc<Semaphore>,
    job: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    // No unbounded queue of async waiters or already-spawned blocking jobs.
    let permit = gate
        .clone()
        .try_acquire_owned()
        .map_err(|_| "Scan already in progress; retry shortly")?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit; // Remains held if the caller is cancelled.
        job()
    })
    .await
    .map_err(|error| format!("Scan failed: {error}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test(flavor = "current_thread")]
    async fn blocking_scan_keeps_timers_running_and_rejects_overlap_after_cancellation() {
        let gate = Arc::new(Semaphore::new(1));
        let job_gate = gate.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = std::sync::mpsc::channel();
        let job = tokio::spawn(async move {
            scan(&job_gate, move || {
                started_tx.send(()).unwrap();
                finish_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                Ok(())
            })
            .await
        });
        started_rx.await.unwrap();
        tokio::time::timeout(
            Duration::from_millis(100),
            tokio::time::sleep(Duration::from_millis(5)),
        )
        .await
        .unwrap();
        assert!(scan(&gate, || Ok(()))
            .await
            .unwrap_err()
            .contains("in progress"));
        job.abort();
        assert!(
            scan(&gate, || Ok(())).await.is_err(),
            "cancelled callers must not release a running scan"
        );
        finish_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while gate.available_permits() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(scan(&gate, || Ok(())).await.is_ok());
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "explicit scheduler benchmark with a 40 MiB synthetic transcript"]
    async fn benchmark_transcript_scheduler() {
        use std::io::Write;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Instant;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let text = "x".repeat(2048);
        for i in 0..20_000 {
            writeln!(
                file,
                "{}",
                serde_json::json!({
                    "type":"assistant","uuid":format!("message-{i}"),"sessionId":"perf",
                "timestamp":"2026-09-12T00:00:00Z","message":{"id":format!("m-{i}"),"model":"fixture","role":"assistant",
                    "content":[{"type":"tool_use","id":format!("task-{i}"),"name":"Agent",
                    "input":{"description":text,"subagent_type":"fixture"}}]}
                })
            )
            .unwrap();
        }
        file.flush().unwrap();
        for inline in [true, false] {
            let running = Arc::new(AtomicBool::new(true));
            let running_in_timer = running.clone();
            let timer = tokio::spawn(async move {
                let mut previous = Instant::now();
                let mut max_gap = 0.0f64;
                let mut ticks = 0;
                while running_in_timer.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                    max_gap = max_gap.max(previous.elapsed().as_secs_f64() * 1000.0);
                    previous = Instant::now();
                    ticks += 1;
                }
                (ticks, max_gap)
            });
            tokio::task::yield_now().await;
            let path = file.path().to_path_buf();
            let work = move || {
                let entries = crate::session::parser::parse_all_entries(&path)?;
                let subagents = crate::session::active_subagents_for_path("perf", &path);
                Ok((entries.len(), subagents.len()))
            };
            let start = Instant::now();
            let counts = if inline {
                work().unwrap()
            } else {
                scan(&Arc::new(Semaphore::new(1)), work).await.unwrap()
            };
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            tokio::time::sleep(Duration::from_millis(5)).await;
            running.store(false, Ordering::Release);
            let (ticks, max_gap_ms) = timer.await.unwrap();
            assert_eq!(counts, (20_000, 20_000));
            println!(
                "SCAN_PERF {}",
                serde_json::json!({"mode":if inline {"inline_control"} else {"bounded_blocking"},
                "file_bytes":file.as_file().metadata().unwrap().len(),"elapsed_ms":elapsed,"timer_ticks":ticks,"max_timer_gap_ms":max_gap_ms})
            );
        }
    }
}
