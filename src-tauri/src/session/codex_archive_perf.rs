//! Reproducible, synthetic evidence for messaging/history contention. No user data.
use super::*;
use std::time::{Duration, Instant};

#[test]
fn conversation_finishes_while_archive_lock_is_still_held() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("sessions");
    std::fs::create_dir(&root).unwrap();
    let id = uuid::Uuid::from_u128(1).to_string();
    let path = root.join(format!("rollout-2026-09-12T00-00-00-{id}.jsonl"));
    let mut file = File::create(path).unwrap();
    writeln!(
        file,
        "{}",
        serde_json::json!({"type":"session_meta","payload":{"id":id,"cwd":"/tmp"}})
    )
    .unwrap();
    writeln!(file, "{}", serde_json::json!({"timestamp":"2026-09-12T00:00:00Z","type":"event_msg","payload":{"type":"user_message","message":"lock-free conversation"}})).unwrap();
    let guard = archive_state().lock().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        tx.send(
            super::super::codex::find_codex_conversation_under_with_progress(
                &root,
                &id,
                false,
                &mut |_, _| {},
            ),
        )
        .unwrap();
    });
    let result = rx.recv_timeout(Duration::from_secs(1));
    drop(guard);
    worker.join().unwrap();
    let messages = result
        .expect("conversation waited for an unrelated archive rebuild")
        .unwrap();
    assert_eq!(messages[0].content, "lock-free conversation");
}

// Run explicitly with --ignored --nocapture --test-threads=1. This is a measured
// diagnostic, not an acceptance assertion or a substitute for native RSS QA.
#[test]
#[ignore = "synthetic archive performance diagnostic; run explicitly"]
fn messaging_archive_measurements() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("sessions");
    std::fs::create_dir(&root).unwrap();
    let cache_path = temp.path().join("c9watch-archive-cache.json");
    let count = 96;
    let rows = 768;
    let mut fixture_bytes = 0;
    let target = uuid::Uuid::from_u128(1).to_string();
    for session in 1..=count {
        let id = uuid::Uuid::from_u128(session).to_string();
        let path = root.join(format!("rollout-2026-09-12T00-00-00-{id}.jsonl"));
        let mut out = BufWriter::new(File::create(&path).unwrap());
        writeln!(out, "{}", serde_json::json!({"type":"session_meta","payload":{"id":id,"cwd":"/tmp/synthetic","source":"cli"}})).unwrap();
        for ordinal in 0..rows {
            writeln!(out, "{}", serde_json::json!({"timestamp":"2026-09-12T00:00:00Z","type":"event_msg","payload":{"type":"user_message","message":format!("{ordinal}:{}", "x".repeat(1024))}})).unwrap();
        }
        out.flush().unwrap();
        fixture_bytes += std::fs::metadata(path).unwrap().len();
    }
    let started = Instant::now();
    let cold = load_listing_snapshots(&root, &cache_path);
    let cold_ms = started.elapsed().as_secs_f64() * 1000.0;
    let cache_version = std::fs::metadata(&cache_path).unwrap().modified().unwrap();
    let mut warm_ms = Vec::new();
    for _ in 0..20 {
        let started = Instant::now();
        assert_eq!(
            load_listing_snapshots(&root, &cache_path).len(),
            count as usize
        );
        warm_ms.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    assert_eq!(
        std::fs::metadata(&cache_path).unwrap().modified().unwrap(),
        cache_version
    );
    let retained_bytes: usize = archive_state().lock().unwrap()
        [&(root.clone(), cache_path.clone())]
        .cache
        .files
        .values()
        .map(|entry| entry.indexed_message_bytes)
        .sum();
    assert!(retained_bytes <= MAX_PROCESS_MESSAGE_BYTES);
    assert_eq!(cold.len(), count as usize);
    assert!(cold
        .iter()
        .all(|s| s.messages.is_empty() && s.token_events.is_empty()));

    let started = Instant::now();
    let messages = super::super::codex::find_codex_conversation_under_with_progress(
        &root,
        &target,
        false,
        &mut |_, _| {},
    )
    .unwrap();
    let first_load_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(messages.len(), rows);

    // Model a concurrent History/Cost rebuild holding the real global lock.
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = archive_state().lock().unwrap();
        locked_tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(500));
    });
    locked_rx.recv().unwrap();
    let started = Instant::now();
    let contended = super::super::codex::find_codex_conversation_under_with_progress(
        &root,
        &target,
        false,
        &mut |_, _| {},
    )
    .unwrap();
    let contended_load_ms = started.elapsed().as_secs_f64() * 1000.0;
    holder.join().unwrap();
    assert_eq!(contended.len(), rows);
    // Simulate a process restart without altering transcript/cache files.
    archive_state()
        .lock()
        .unwrap()
        .remove(&(root.clone(), cache_path.clone()));
    let started = Instant::now();
    assert_eq!(
        load_listing_snapshots(&root, &cache_path).len(),
        count as usize
    );
    let disk_cache_reload_ms = started.elapsed().as_secs_f64() * 1000.0;
    let reloaded_bytes: usize = archive_state().lock().unwrap()
        [&(root.clone(), cache_path.clone())]
        .cache
        .files
        .values()
        .map(|entry| entry.indexed_message_bytes)
        .sum();
    assert!(reloaded_bytes <= MAX_PROCESS_MESSAGE_BYTES);
    assert_eq!(
        std::fs::metadata(&cache_path).unwrap().modified().unwrap(),
        cache_version
    );
    warm_ms.sort_by(f64::total_cmp);
    println!(
        "{}",
        serde_json::json!({
            "fixtureBytes":fixture_bytes,"sessions":count,"rowsPerSession":rows,
            "coldArchiveMs":cold_ms,"warmArchiveMedianMs":warm_ms[10],"warmArchiveP95Ms":warm_ms[18],
            "retainedMessageBytes":retained_bytes,"retentionCapBytes":MAX_PROCESS_MESSAGE_BYTES,
            "firstConversationMs":first_load_ms,"contendedConversationMs":contended_load_ms,
            "diskCacheReloadMs":disk_cache_reload_ms,"reloadedMessageBytes":reloaded_bytes,
            "simulatedArchiveLockMs":500,"cacheFileBytes":std::fs::metadata(&cache_path).unwrap().len()
        })
    );
    archive_state().lock().unwrap().remove(&(root, cache_path));
}
