//! Real loopback HTTP fixtures; no user transcripts or external endpoints.
use super::*;
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::atomic::AtomicUsize;
use std::thread;

struct Reply {
    code: u16,
    body: String,
    headers: String,
    delay: Duration,
    chunked: bool,
}

impl Reply {
    fn json(value: Value) -> Self {
        Self::body(value.to_string())
    }
    fn body(body: String) -> Self {
        Self {
            code: 200,
            body,
            headers: String::new(),
            delay: Duration::ZERO,
            chunked: false,
        }
    }
}

struct Fixture {
    connection: Connection,
    requests: Arc<Mutex<Vec<String>>>,
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    server: Option<thread::JoinHandle<()>>,
}

impl Fixture {
    fn new(handler: impl Fn(&str) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let connection = Connection::parse(
            &format!("http://{}", listener.local_addr().unwrap()),
            "opencode".into(),
            None,
        )
        .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (server_stop, seen, count, maximum) =
            (stop.clone(), requests.clone(), active.clone(), peak.clone());
        let handler = Arc::new(handler);
        let server = thread::spawn(move || {
            let mut jobs = Vec::new();
            while !server_stop.load(Ordering::Acquire) {
                let Ok((mut socket, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(1));
                    continue;
                };
                let (seen, handler, count, maximum) = (
                    seen.clone(),
                    handler.clone(),
                    count.clone(),
                    maximum.clone(),
                );
                jobs.push(thread::spawn(move || {
                    socket.set_nonblocking(false).unwrap();
                    socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                    socket.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
                    let mut reader = BufReader::new(socket.try_clone().unwrap());
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 { return; }
                    let path = line.split_whitespace().nth(1).unwrap().to_string();
                    loop {
                        line.clear();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" { break; }
                    }
                    seen.lock().unwrap().push(path.clone());
                    let n = count.fetch_add(1, Ordering::AcqRel) + 1;
                    maximum.fetch_max(n, Ordering::AcqRel);
                    let reply = handler(&path);
                    thread::sleep(reply.delay);
                    if reply.chunked {
                        let _ = write!(socket, "HTTP/1.1 {} Test\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n{}\r\n{:X}\r\n{}\r\n0\r\n\r\n", reply.code, reply.headers, reply.body.len(), reply.body);
                    } else {
                        let _ = write!(socket, "HTTP/1.1 {} Test\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n{}", reply.code, reply.body.len(), reply.headers, reply.body);
                    }
                    count.fetch_sub(1, Ordering::AcqRel);
                }));
            }
            for job in jobs {
                job.join().unwrap();
            }
        });
        Self {
            connection,
            requests,
            active,
            peak,
            stop,
            server: Some(server),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.server.take().unwrap().join().unwrap();
        assert_eq!(self.active.load(Ordering::Acquire), 0);
    }
}

fn remote(id: &str, directory: &str) -> Value {
    json!({"id":id,"directory":directory,"title":id,"time":{"created":1,"updated":chrono::Utc::now().timestamp_millis()}})
}

fn message(text: &str, created: i64) -> Value {
    json!({"info":{"role":"assistant","time":{"created":created}},"parts":[{"type":"text","text":text}]})
}

#[test]
fn identical_remote_ids_in_two_directories_survive_enrichment_and_reads_are_scoped() {
    let fixture = Fixture::new(|path| match path {
        "/global/health" => Reply::json(json!({"healthy":true,"version":"fixture"})),
        "/session?limit=513" => Reply::json(json!([remote("same", "/a"), remote("same", "/中文")])),
        "/session/status?directory=%2Fa" => Reply::json(json!({"same":{"type":"busy"}})),
        "/session/status?directory=%2F%E4%B8%AD%E6%96%87" => Reply::json(json!({})),
        "/session/same?directory=%2Fa" => Reply::json(remote("same", "/a")),
        "/session/same/message?limit=20&directory=%2Fa" => {
            Reply::json(json!([message("English A", 1)]))
        }
        "/session/same?directory=%2F%E4%B8%AD%E6%96%87" => Reply::json(remote("same", "/中文")),
        "/session/same/message?limit=20&directory=%2F%E4%B8%AD%E6%96%87" => {
            Reply::json(json!([message("中文 B", 2)]))
        }
        _ => panic!("unexpected {path}"),
    });
    let detected = snapshot(&fixture.connection, &client().unwrap()).unwrap();
    let sessions = super::super::enrichment::enrich_detected_sessions(detected, Default::default())
        .unwrap()
        .0;
    assert_eq!(sessions.len(), 2);
    assert_ne!(sessions[0].session_key, sessions[1].session_key);
    assert_eq!(sessions[0].status, SessionStatus::Working);
    assert_eq!(sessions[1].status, SessionStatus::WaitingForInput);
    let mut reader = ConversationReader::default();
    for (s, text) in sessions.iter().zip(["English A", "中文 B"]) {
        let conv = reader.load(&fixture.connection, &s.id, false).unwrap();
        assert_eq!(conv.session_id, s.id);
        assert_eq!(conv.messages[0].content, text);
    }
}

#[test]
fn detail_children_deleted_archived_and_wrong_directory_are_validated() {
    let fixture = Fixture::new(|path| {
        if path == "/session/root/children?directory=%2Fa" {
            let mut child = remote("child", "/a");
            child["parentID"] = json!("root");
            return Reply::json(json!([child]));
        }
        if path.contains("missing") {
            let mut reply = Reply::body("private body".into());
            reply.code = 404;
            return reply;
        }
        let mut detail = remote(
            if path.contains("archived") {
                "archived"
            } else {
                "root"
            },
            "/a",
        );
        if path.contains("archived") {
            detail["time"]["archived"] = json!(123);
        }
        Reply::json(detail)
    });
    let children = read_children(
        &fixture.connection,
        &client().unwrap(),
        &scoped_id("root", "/a"),
    )
    .unwrap();
    assert_eq!(children[0].parent_thread_id, scoped_id("root", "/a"));
    assert_eq!(children[0].session_id, scoped_id("child", "/a"));
    let mut reader = ConversationReader::default();
    for (id, expected) in [
        ("missing", "404"),
        ("archived", "archived"),
        ("root?directory=%2Fb", "identity"),
    ] {
        assert!(reader
            .load(&fixture.connection, id, false)
            .unwrap_err()
            .contains(expected));
    }
}

#[test]
fn health_auth_server_malformed_and_recovery_replace_or_retain_snapshot() {
    let mode = Arc::new(AtomicUsize::new(0));
    let state = mode.clone();
    let fixture = Fixture::new(move |path| {
        if path == "/global/health" {
            let mode = state.load(Ordering::Acquire);
            if mode == 6 {
                return Reply::body("not json".into());
            }
            if mode == 7 {
                return Reply::json(json!({"healthy":false,"version":"fixture"}));
            }
            if mode > 0 && mode < 6 {
                let mut r = Reply::body("secret diagnostics".into());
                r.code = [0, 401, 403, 404, 500, 503][mode];
                return r;
            }
            return Reply::json(json!({"healthy":true,"version":"fixture"}));
        }
        if path == "/session?limit=513" {
            return Reply::json(json!([remote("root", "/a")]));
        }
        Reply::json(json!({}))
    });
    let client = client().unwrap();
    let mut cache = Cache {
        started: true,
        generation: 1,
        connection: None,
        sessions: vec![],
        error: None,
        connected: false,
    };
    apply_result(&mut cache, snapshot(&fixture.connection, &client));
    assert_eq!(cache.sessions.len(), 1);
    for mode_value in 1..=7 {
        mode.store(mode_value, Ordering::Release);
        apply_result(&mut cache, snapshot(&fixture.connection, &client));
        assert!(!cache.connected);
        assert_eq!(cache.sessions.len(), 1);
        assert_eq!(
            cache.sessions[0].opencode_summary.as_ref().unwrap().status,
            SessionStatus::Connecting
        );
        assert!(!cache.error.as_ref().unwrap().contains("secret"));
        mode.store(0, Ordering::Release);
        apply_result(&mut cache, snapshot(&fixture.connection, &client));
        assert!(cache.connected && cache.error.is_none());
    }
    apply_generation_result(&mut cache, 0, Ok(vec![]));
    assert_eq!(
        cache.sessions.len(),
        1,
        "late generation cannot clear replacement snapshot"
    );
    apply_generation_result(&mut cache, 1, Ok(vec![]));
    assert!(
        cache.sessions.is_empty(),
        "deletion/expiry removes prior snapshot"
    );
}

#[test]
fn bounded_pages_bytes_messages_and_malformed_parts_do_not_silently_truncate() {
    let fixture = Fixture::new(|_| Reply::json(json!([message(&"a".repeat(256), 1)])));
    let mut budget = Budget::new(2, 128);
    assert!(read_conversation_bounded(
        &fixture.connection,
        &client().unwrap(),
        "root",
        false,
        &mut budget
    )
    .unwrap_err()
    .contains("total response"));
    let fixture = Fixture::new(|_| {
        let mut r = Reply::json(json!([]));
        r.headers = format!("X-Next-Cursor: {}\r\n", uuid::Uuid::new_v4());
        r
    });
    assert!(
        read_conversation(&fixture.connection, &client().unwrap(), "root", false)
            .unwrap_err()
            .contains("128 pages")
    );
    assert_eq!(fixture.requests.lock().unwrap().len(), 128);
    let fixture =
        Fixture::new(|_| Reply::json(json!((0..21).map(|_| message("x", 1)).collect::<Vec<_>>())));
    assert!(
        read_conversation(&fixture.connection, &client().unwrap(), "root", false)
            .unwrap_err()
            .contains("ignored message page limit")
    );
    let fixture = Fixture::new(|_| {
        let mut row = message("x", 1);
        row["parts"] = json!((0..10_001)
            .map(|_| json!({"type":"text","text":"x"}))
            .collect::<Vec<_>>());
        Reply::json(json!([row]))
    });
    assert!(
        read_conversation(&fixture.connection, &client().unwrap(), "root", false)
            .unwrap_err()
            .contains("10000")
    );
    assert!(parse_conversation("x", vec![json!({"info":{"role":"assistant","time":{"created":1}},"parts":[{"type":"text","text":123}]})], false).is_err());
}

#[test]
fn chunked_oversized_page_shrinks_and_cursor_cycles_are_detected() {
    let fixture = Fixture::new(|path| {
        let mut reply = if path.contains("limit=20") {
            Reply::body(" ".repeat(MAX_RESPONSE as usize + 1))
        } else {
            Reply::json(json!([message("small page", 1)]))
        };
        reply.chunked = true;
        reply
    });
    let conv = read_conversation(&fixture.connection, &client().unwrap(), "root", false).unwrap();
    assert_eq!(conv.messages[0].content, "small page");
    assert_eq!(fixture.requests.lock().unwrap().len(), 2);
    let fixture = Fixture::new(|path| {
        let mut reply = Reply::json(json!([]));
        reply.headers = format!(
            "X-Next-Cursor: {}\r\n",
            if path.contains("before=a") { "b" } else { "a" }
        );
        reply
    });
    assert!(
        read_conversation(&fixture.connection, &client().unwrap(), "root", false)
            .unwrap_err()
            .contains("repeated")
    );
    assert_eq!(fixture.requests.lock().unwrap().len(), 3);
}

#[test]
fn deadline_and_disconnect_bound_http_work_and_discard_late_data() {
    let fixture = Fixture::new(|_| {
        let mut r = Reply::json(json!([]));
        r.delay = Duration::from_millis(150);
        r
    });
    let start = Instant::now();
    let mut budget = Budget {
        deadline: Instant::now() + Duration::from_millis(40),
        remaining: MAX_RESPONSE,
    };
    assert!(read_conversation_bounded(
        &fixture.connection,
        &client().unwrap(),
        "root",
        false,
        &mut budget
    )
    .is_err());
    assert!(start.elapsed() < Duration::from_millis(500));
    let connection = fixture.connection.clone();
    let job =
        thread::spawn(move || read_conversation(&connection, &client().unwrap(), "root", false));
    let wait_start = Instant::now();
    while fixture.requests.lock().unwrap().len() < 2 {
        assert!(wait_start.elapsed() < Duration::from_secs(2));
        thread::sleep(Duration::from_millis(1));
    }
    fixture.connection.cancelled.store(true, Ordering::Release);
    assert!(job.join().unwrap().unwrap_err().contains("disconnected"));
    let n = fixture.requests.lock().unwrap().len();
    assert!(read_conversation(&fixture.connection, &client().unwrap(), "root", false).is_err());
    assert_eq!(
        fixture.requests.lock().unwrap().len(),
        n,
        "disconnect must not start another HTTP request"
    );
}

#[test]
fn cache_is_one_entry_short_lived_connection_scoped_and_fail_fast() {
    let fixture = Fixture::new(|path| {
        if path.contains("/message?") {
            Reply::json(json!([message("cached", 1)]))
        } else {
            Reply::json(remote("root", "/a"))
        }
    });
    let reader = Arc::new(Mutex::new(ConversationReader::default()));
    let mut guard = reader.lock().unwrap();
    let value = guard.load(&fixture.connection, "root", false).unwrap();
    for _ in 0..100 {
        assert_eq!(
            guard
                .load(&fixture.connection, "root", false)
                .unwrap()
                .messages
                .len(),
            value.messages.len()
        );
    }
    assert_eq!(fixture.requests.lock().unwrap().len(), 2);
    let other = reader.clone();
    assert!(thread::spawn(move || other.try_lock().is_err())
        .join()
        .unwrap());
    guard.cached.as_mut().unwrap().loaded -= Duration::from_secs(2);
    guard.load(&fixture.connection, "root", false).unwrap();
    assert_eq!(fixture.requests.lock().unwrap().len(), 4);
    fixture.connection.cancelled.store(true, Ordering::Release);
    assert!(guard.load(&fixture.connection, "root", false).is_err());
    assert_eq!(fixture.peak.load(Ordering::Acquire), 1);
}

#[test]
fn actual_32_mib_total_cap_and_single_8_mib_page_cap_are_enforced() {
    let page = json!([message(&"x".repeat(5 * 1024 * 1024), 1)]).to_string();
    let fixture = Fixture::new(move |_| {
        let mut reply = Reply::body(page.clone());
        reply.headers = format!("X-Next-Cursor: {}\r\n", uuid::Uuid::new_v4());
        reply
    });
    assert!(
        read_conversation(&fixture.connection, &client().unwrap(), "root", false)
            .unwrap_err()
            .contains("total response")
    );
    assert_eq!(fixture.requests.lock().unwrap().len(), 7);
    let fixture = Fixture::new(|_| Reply::body("x".repeat(MAX_RESPONSE as usize + 1)));
    let mut budget = Budget::new(3, MAX_RESPONSE + 2);
    let error = fixture
        .connection
        .get_page_bounded::<Value>(
            &client().unwrap(),
            &["session", "root", "message"],
            &[("limit", "1")],
            &mut budget,
        )
        .unwrap_err();
    assert!(error.contains("exceeds 8 MiB"));
    assert_eq!(fixture.requests.lock().unwrap().len(), 1);
}

#[test]
fn discovery_size_and_directory_fanout_are_bounded() {
    for directories in [false, true] {
        let fixture = Fixture::new(move |path| {
            if path == "/global/health" {
                return Reply::json(json!({"healthy":true,"version":"fixture"}));
            }
            if path == "/session?limit=513" {
                return Reply::json(json!((0..if directories { 33 } else { 513 })
                    .map(|i| remote(
                        &format!("id{i}"),
                        &format!("/dir{}", if directories { i } else { 0 })
                    ))
                    .collect::<Vec<_>>()));
            }
            Reply::json(json!({}))
        });
        let error = snapshot(&fixture.connection, &client().unwrap()).unwrap_err();
        assert!(error.contains(if directories {
            "32 directories"
        } else {
            "512 sessions"
        }));
        assert!(fixture.requests.lock().unwrap().len() <= 34);
    }
}

#[test]
fn scoped_ids_keep_legacy_collision_guards_and_direct_cli_references() {
    let id = "ses_f835ea1f8ffelnS2uzHgkM9qUq";
    let scoped = scoped_id(id, "/中文?folder&space value");
    assert!(is_full_session_reference(&scoped));
    assert!(matches_session_reference(&scoped, id));
    assert!(matches_session_reference(&scoped, &scoped));
    assert!(!matches_session_reference(
        &scoped,
        &scoped_id(id, "/other")
    ));
    assert!(!matches_session_reference(&scoped, "unrelated"));
    assert!(!is_full_session_reference(&format!("{id}?directory=")));
    assert_eq!(
        split_id(&scoped).unwrap(),
        (id, Some("/中文?folder&space value".into()))
    );
}

#[test]
#[ignore = "explicit production reader benchmark including detail and cache copies"]
fn benchmark_opencode_reader_cache_and_refresh() {
    for (name, count, width) in [("small", 24usize, 128usize), ("large", 1200, 8192)] {
        let rows: Vec<_> = (0..count)
            .map(|i| message(&format!("{i:05} {}", "x".repeat(width)), i as i64 + 1))
            .collect();
        let fixture = Fixture::new(move |path| {
            if !path.contains("/message?") {
                return Reply::json(remote("root", "/a"));
            }
            let url = reqwest::Url::parse(&format!("http://fixture{path}")).unwrap();
            let before = url
                .query_pairs()
                .find(|(key, _)| key == "before")
                .map(|(_, value)| value.parse::<usize>().unwrap())
                .unwrap_or(count);
            let start = before.saturating_sub(20);
            let mut reply = Reply::json(json!(rows[start..before]));
            if start > 0 {
                reply.headers = format!("X-Next-Cursor: {start}\r\n");
            }
            reply
        });
        let mut reader = ConversationReader::default();
        let mut times = Vec::new();
        let mut cached_times = Vec::new();
        for _ in 0..11 {
            if let Some(cached) = &mut reader.cached {
                cached.loaded -= Duration::from_secs(2);
            }
            let start = Instant::now();
            let value = reader.load(&fixture.connection, "root", false).unwrap();
            assert_eq!(value.messages.len(), count);
            let _serialized = serde_json::to_vec(&value).unwrap();
            times.push(start.elapsed().as_secs_f64() * 1000.0);
            let requests = fixture.requests.lock().unwrap().len();
            let start = Instant::now();
            let cached = reader.load(&fixture.connection, "root", false).unwrap();
            let _serialized = serde_json::to_vec(&cached).unwrap();
            cached_times.push(start.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(fixture.requests.lock().unwrap().len(), requests);
        }
        let cold = times.remove(0);
        times.sort_by(f64::total_cmp);
        cached_times.sort_by(f64::total_cmp);
        assert_eq!(
            fixture.requests.lock().unwrap().len(),
            11 * (1 + count.div_ceil(20))
        );
        println!(
            "OPENCODE_READER_PERF {}",
            json!({
                "case":name,"messages":count,"cold_ms":cold,
                "refresh_median_ms":times[5],"refresh_p95_ms":times[9],
                "cache_hit_median_ms":cached_times[5],"cache_hit_http_requests":0,
                "total_http_requests":fixture.requests.lock().unwrap().len(),
                "includes":"session detail, page reads, cache copy, response serialization"
            })
        );
    }
}

#[test]
#[ignore = "isolated global service stress; run explicitly with --test-threads=1"]
fn concurrent_conversation_entry_has_one_http_load_and_recovers_after_disconnect() {
    let fixture = Fixture::new(|path| {
        if path.contains("/message?") {
            let mut reply = Reply::json(json!([message("one load", 1)]));
            reply.delay = Duration::from_millis(250);
            reply
        } else {
            Reply::json(remote("root", "/a"))
        }
    });
    CACHE.lock().unwrap().connection = Some(fixture.connection.clone());
    let barrier = Arc::new(std::sync::Barrier::new(17));
    let jobs: Vec<_> = (0..16)
        .map(|_| {
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                conversation("root", false)
            })
        })
        .collect();
    barrier.wait();
    let results: Vec<_> = jobs.into_iter().map(|job| job.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert!(results
        .iter()
        .filter_map(|r| r.as_ref().err())
        .all(|e| e.contains("already in progress")));
    assert_eq!(fixture.requests.lock().unwrap().len(), 2);
    assert_eq!(fixture.peak.load(Ordering::Acquire), 1);
    fixture.connection.cancelled.store(true, Ordering::Release);
    assert!(conversation("root", false)
        .unwrap_err()
        .contains("disconnected"));
    let replacement =
        Connection::parse(fixture.connection.url.as_str(), "opencode".into(), None).unwrap();
    CACHE.lock().unwrap().connection = Some(replacement);
    assert_eq!(
        conversation("root", false).unwrap().messages[0].content,
        "one load"
    );
    assert_eq!(fixture.requests.lock().unwrap().len(), 4);
    CACHE.lock().unwrap().connection = None;
    *CONVERSATION_READER.lock().unwrap() = ConversationReader::default();
    println!("OPENCODE_CONCURRENCY callers=16 accepted=1 rejected=15 peak_http=1 recovery=ok");
}
