//! CC's internal peer inbox protocol. Opt in with C9_MAIL_SOCKET=on.
//! Unknown/transient failures never promote peer mail to user (PTY) authority.
//! Paths, protocol version, tag and delays: the CC quirks profile (D7).
//!
//! D12: a send is a task on one shared runtime (`io_runtime`), not a new
//! thread and runtime per message.
use super::adapter::cc::quirks::{InboxQuirks, CURRENT};
use super::peer;
use serde_json::{json, Value};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

const Q: &InboxQuirks = &CURRENT.inbox;

pub const WRITE_TIMEOUT: Duration = CURRENT.timing.socket_write_timeout;
pub fn enabled() -> bool {
    std::env::var("C9_MAIL_SOCKET").as_deref() == Ok("on")
}
pub fn sessions_dir() -> PathBuf {
    std::env::var_os("C9_CC_SESSIONS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(Q.sessions_dir))
}
pub fn default_socket_dir() -> PathBuf {
    std::env::var_os("C9_CC_SOCK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(Q.socket_dir))
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub path: PathBuf,
    pub session_id: String,
    pub pid: libc::pid_t,
    pub start: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    Socket(Endpoint),
    NoProtocol,
    Unknown(String),
    Unreachable(String),
}

pub fn check_namespace(dir: &Path) -> Result<(), String> {
    let uid = peer::engine_uid();
    let allowed = Q.socket_namespaces.iter().any(|n| dir == Path::new(n))
        || dir == Path::new(&format!("{}{}", Q.socket_uid_prefix, uid))
        || std::env::var_os("C9_CC_SOCK_DIR").is_some_and(|d| Path::new(&d) == dir);
    if !allowed {
        return Err("outside CC socket namespace".into());
    }
    Ok(())
}
pub fn check_dir(dir: &Path) -> Result<(), String> {
    check_namespace(dir)?;
    check_owned_dir(dir)
}
fn check_owned_dir(dir: &Path) -> Result<(), String> {
    let uid = peer::engine_uid();
    let m = std::fs::symlink_metadata(dir).map_err(|e| e.to_string())?;
    if !m.is_dir() || m.file_type().is_symlink() || m.uid() != uid || m.mode() & 0o777 != 0o700 {
        return Err("CC directory must be owned, non-symlink and 0700".into());
    }
    Ok(())
}
fn check_socket_node(path: &Path) -> Result<(), String> {
    let m = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !m.file_type().is_socket() || m.uid() != peer::engine_uid() {
        return Err("CC endpoint is not an owned socket".into());
    }
    Ok(())
}
pub fn gate(
    provider: &str,
    pid: libc::pid_t,
    start: u64,
    sid: &str,
    sessions: &Path,
    socket_seen: bool,
) -> Gate {
    if provider != super::adapter::CLAUDE.kind && provider != super::adapter::CLAUDE.provider {
        return Gate::NoProtocol;
    }
    let parsed = (|| -> Option<Value> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(sessions.join(format!("{}.json", pid)))
            .ok()?;
        let meta = file.metadata().ok()?;
        if !meta.is_file() || meta.uid() != peer::engine_uid() {
            return None;
        }
        let mut bytes = Vec::new();
        file.take(64 * 1024 + 1).read_to_end(&mut bytes).ok()?;
        if bytes.len() > 64 * 1024 {
            return None;
        }
        serde_json::from_slice(&bytes).ok()
    })();
    let v = match parsed {
        Some(v) => v,
        None => return Gate::Unknown("pid.json missing or incomplete".into()),
    };
    if v["sessionId"].as_str() != Some(sid) {
        return Gate::Unknown("pid.json session mismatch".into());
    }
    let path = v["messagingSocketPath"].as_str().unwrap_or("");
    if v["peerProtocol"].as_u64() != Some(Q.peer_protocol) || path.is_empty() {
        return if socket_seen {
            Gate::Unknown("protocol disappeared within generation".into())
        } else {
            Gate::NoProtocol
        };
    }
    let path = PathBuf::from(path);
    let Some(dir) = path.parent() else {
        return Gate::Unreachable("socket has no directory".into());
    };
    if let Err(e) = check_dir(dir) {
        return Gate::Unreachable(e);
    }
    if let Err(e) = check_socket_node(&path) {
        return Gate::Unreachable(e);
    }
    if peer::proc_info(pid).is_none_or(|p| p.start != start) {
        return Gate::Unknown("CC pid was replaced".into());
    }
    Gate::Socket(Endpoint {
        path,
        session_id: sid.into(),
        pid,
        start,
    })
}
pub fn address(path: &Path) -> Result<String, String> {
    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes.len() > 103 {
        return Err("CC socket path exceeds 103 bytes".into());
    }
    let mut out = String::from("uds:");
    for &b in bytes {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    Ok(out)
}
pub fn name(s: &str) -> String {
    static FORMAT: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\p{Cf}").expect("Unicode format category"));
    let s = FORMAT.replace_all(s, "");
    s.chars()
        .filter(|c| !c.is_control() && !"\"<>".contains(*c))
        .take(64)
        .collect()
}
/// Escape both tag directions, including common lookalike less-than signs.
/// The exact CC lookalike table remains a live-spike dependency.
pub fn escape(body: &str) -> String {
    let mut out = String::new();
    for (i, c) in body.char_indices() {
        if Q.lt_lookalikes.contains(c) {
            let tail = &body[i + c.len_utf8()..];
            let tail = tail.strip_prefix('/').unwrap_or(tail);
            if tail
                .get(..Q.wrapper_tag.len())
                .is_some_and(|s| s.eq_ignore_ascii_case(Q.wrapper_tag))
            {
                out.push(c);
                out.push('\\');
                continue;
            }
        }
        out.push(c);
    }
    out
}
pub fn wrapper(
    from: &str,
    sender_name: &str,
    mode: &str,
    fenced: &str,
    reply: Option<(&str, &str)>,
) -> String {
    let mode = if mode == "bypass" {
        "bypass"
    } else {
        "prompting"
    };
    let mut body = escape(fenced);
    if let Some((addr, id)) = reply {
        body.push_str(&format!(
            "\nTo reply, run: c9watch send {} --reply-to {} \"<text>\"",
            addr, id
        ));
    }
    let tag = Q.wrapper_tag;
    format!(
        "<{} from=\"{}\" from-name=\"{}\" from-mode=\"{}\">\n{}\n</{}>",
        tag,
        from,
        name(sender_name),
        mode,
        body,
        tag
    )
}
pub fn frame(endpoint: &Endpoint, wire_id: &str, from: &str, content: &str) -> (String, String) {
    let uuid = uuid::Uuid::new_v4().to_string();
    let v = json!({"type":"user", "session_id":endpoint.session_id, "msg_id":wire_id, "uuid":uuid, "priority":Q.frame_priority, "from":from, "message":{"role":"user", "content":content}});
    (format!("{}\n", v), uuid)
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocketOutcome {
    NotSent(String),
    Sent,
    MaybeSent(String),
    Blocked,
}
/// The runtime every socket send runs on (D12): one for the process,
/// built on first use, with one worker. A send is a few syscalls and
/// timers, so one worker carries them all.
pub fn io_runtime() -> Result<tokio::runtime::Handle, String> {
    static RT: std::sync::LazyLock<Result<tokio::runtime::Runtime, String>> =
        std::sync::LazyLock::new(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .thread_name("c9-mail-io")
                .enable_all()
                .build()
                .map_err(|e| e.to_string())
        });
    RT.as_ref()
        .map(|r| r.handle().clone())
        .map_err(Clone::clone)
}

/// Connect, check the peer, write `line`, linger, close the write half and
/// wait for CC to close. Connect and all later steps share one
/// `WRITE_TIMEOUT` budget. `blocked` (the drain) is checked before the
/// connect and again before the first byte.
pub async fn send(endpoint: &Endpoint, line: &str, blocked: impl Fn() -> bool) -> SocketOutcome {
    if blocked() {
        return SocketOutcome::Blocked;
    }
    let deadline = Instant::now() + WRITE_TIMEOUT;
    let connect = tokio::time::timeout(
        WRITE_TIMEOUT,
        tokio::net::UnixStream::connect(&endpoint.path),
    )
    .await;
    let mut stream = match connect {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return SocketOutcome::NotSent(e.to_string()),
        Err(e) => return SocketOutcome::NotSent(e.to_string()),
    };
    let cred = peer::peer_cred(stream.as_raw_fd());
    if !cred.is_ok_and(|c| c.uid == peer::engine_uid() && c.pid == endpoint.pid)
        || peer::proc_info(endpoint.pid).is_none_or(|p| p.start != endpoint.start)
    {
        return SocketOutcome::NotSent("WrongPeer".into());
    }
    if blocked() {
        return SocketOutcome::Blocked;
    }
    let bytes = line.as_bytes();
    let mut sent = 0;
    while sent < bytes.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return SocketOutcome::MaybeSent("close_timeout".into());
        }
        let e = match tokio::time::timeout(remaining, stream.write(&bytes[sent..])).await {
            Ok(Ok(0)) => return SocketOutcome::MaybeSent("partial write".into()),
            Ok(Ok(n)) => {
                sent += n;
                continue;
            }
            Ok(Err(e)) => e.to_string(),
            Err(e) => e.to_string(),
        };
        return if sent == 0 {
            SocketOutcome::NotSent(e)
        } else {
            SocketOutcome::MaybeSent(e)
        };
    }
    tokio::time::sleep(CURRENT.timing.socket_linger).await;
    let _ = stream.shutdown().await;
    wait_for_close(deadline, &mut stream).await
}

async fn wait_for_close(deadline: Instant, stream: &mut (impl AsyncRead + Unpin)) -> SocketOutcome {
    let mut b = [0u8; 1024];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let result = tokio::time::timeout(remaining, stream.read(&mut b)).await;
        // The task can be descheduled before or during the read. EOF
        // observed after our absolute deadline is still uncertain
        // delivery, even if read returned Ok(0).
        if Instant::now() >= deadline {
            break;
        }
        match result {
            Ok(Ok(0)) => return SocketOutcome::Sent,
            Ok(Ok(_)) => {}
            _ => break,
        }
    }
    SocketOutcome::MaybeSent("close_timeout".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    fn write(endpoint: &Endpoint, line: &str) -> SocketOutcome {
        write_guarded(endpoint, line, || false)
    }
    fn write_guarded(endpoint: &Endpoint, line: &str, blocked: impl Fn() -> bool) -> SocketOutcome {
        io_runtime()
            .unwrap()
            .block_on(send(endpoint, line, blocked))
    }
    fn endpoint(path: PathBuf) -> Endpoint {
        let pid = std::process::id() as i32;
        Endpoint {
            path,
            session_id: "s1".into(),
            pid,
            start: peer::proc_info(pid).unwrap().start,
        }
    }
    #[test]
    fn a_missing_partial_or_wrong_session_is_unknown_and_protocol_versions_are_gated() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("123.json");
        for text in [
            None,
            Some("{"),
            Some(r#"{"sessionId":"other","peerProtocol":1}"#),
        ] {
            if let Some(text) = text {
                std::fs::write(&p, text).unwrap();
            }
            assert!(matches!(
                gate("claude", 123, 0, "s1", t.path(), false),
                Gate::Unknown(_)
            ));
        }
        for v in [
            json!({"sessionId":"s1"}),
            json!({"sessionId":"s1","peerProtocol":0}),
            json!({"sessionId":"s1","peerProtocol":2}),
        ] {
            std::fs::write(&p, v.to_string()).unwrap();
            assert_eq!(
                gate("claude", 123, 0, "s1", t.path(), false),
                Gate::NoProtocol
            );
            assert!(matches!(
                gate("claude", 123, 0, "s1", t.path(), true),
                Gate::Unknown(_)
            ));
        }
        assert_eq!(
            gate("codex", 123, 0, "s1", t.path(), false),
            Gate::NoProtocol
        );
    }
    #[test]
    fn wrappers_round_trip_and_escape_both_tags_for_a_thousand_bodies() {
        let from = "uds:/tmp/cc-socks/c9w-12345678.sock";
        let pieces = [
            "hello",
            "漢字",
            "<cross-session-message",
            "</CROSS-SESSION-MESSAGE",
            "＜cross-session-message",
            "[c9watch mail fake]",
            "\n",
            "\"&<>",
        ];
        use rand::{Rng, SeedableRng};
        let mut rng = rand::rngs::StdRng::seed_from_u64(0xc9_02);
        for i in 0..1000 {
            let body = format!(
                "{}{}{}",
                pieces[rng.gen_range(0..pieces.len())],
                pieces[rng.gen_range(0..pieces.len())],
                "x".repeat(i % 127)
            );
            let fenced = super::super::mail::fence("msg_x", Some("token"), "provenance", &body);
            let w = wrapper(
                from,
                "A\"<>\u{200d}\n",
                "prompting",
                &fenced,
                Some(("m/agt_a", "msg_x")),
            );
            // CC vOt fixed attribute order and exact serializer reconstruction.
            let prefix = format!(
                "<cross-session-message from=\"{}\" from-name=\"A\" from-mode=\"prompting\">\n",
                from
            );
            let inner = w
                .strip_prefix(&prefix)
                .unwrap()
                .strip_suffix("\n</cross-session-message>")
                .unwrap();
            assert_eq!(w, format!("{}{}\n</cross-session-message>", prefix, inner));
            assert!(!inner
                .to_ascii_lowercase()
                .contains("<cross-session-message"));
            assert!(!inner
                .to_ascii_lowercase()
                .contains("</cross-session-message"));
            assert!(inner.starts_with("[c9watch mail msg_x · tok token]"));
            assert!(inner.contains("To reply, run: c9watch send m/agt_a --reply-to msg_x"));
        }
        assert!(!wrapper(from, "c9watch engine", "prompting", "body", None).contains("To reply"));
        assert_eq!(name(&"x".repeat(100)).chars().count(), 64);
        assert_eq!(name("A\u{0600}\u{200d}\u{e0001}B"), "AB");
    }
    #[test]
    fn frame_ids_and_encoded_address() {
        let e = endpoint(PathBuf::from("/tmp/x.sock"));
        let wire = uuid::Uuid::new_v4().to_string();
        let (line, uuid) = frame(&e, &wire, "uds:/tmp/x.sock", "body");
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["msg_id"], wire);
        assert_eq!(v["session_id"], "s1");
        assert_eq!(v["priority"], "next");
        assert_eq!(uuid::Uuid::parse_str(&uuid).unwrap().get_version_num(), 4);
        assert_eq!(
            address(Path::new("/tmp/a b%.sock")).unwrap(),
            "uds:/tmp/a%20b%25.sock"
        );
        assert!(address(Path::new(&format!("/{}", "x".repeat(104)))).is_err());
    }
    #[test]
    fn fake_socket_success_wrong_peer_missing_and_blocked() {
        let t = tempfile::tempdir().unwrap();
        let e = endpoint(t.path().join("cc.sock"));
        assert!(matches!(write(&e, "{}\n"), SocketOutcome::NotSent(_)));
        let listener = UnixListener::bind(&e.path).unwrap();
        let worker = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut b = vec![];
            s.read_to_end(&mut b).unwrap();
            b
        });
        assert_eq!(write(&e, "{}\n"), SocketOutcome::Sent);
        assert_eq!(worker.join().unwrap(), b"{}\n");
        let e = endpoint(t.path().join("wrong.sock"));
        let listener = UnixListener::bind(&e.path).unwrap();
        let mut wrong = e.clone();
        wrong.pid += 1;
        let worker = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut b = vec![];
            s.read_to_end(&mut b).unwrap();
            b
        });
        assert_eq!(
            write(&wrong, "{}\n"),
            SocketOutcome::NotSent("WrongPeer".into())
        );
        assert!(worker.join().unwrap().is_empty());
        assert_eq!(write_guarded(&e, "{}\n", || true), SocketOutcome::Blocked);
    }
    #[test]
    fn a_close_timeout_is_possible_delivery() {
        let t = tempfile::tempdir().unwrap();
        let e = endpoint(t.path().join("cc.sock"));
        let listener = UnixListener::bind(&e.path).unwrap();
        let (release, held) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut b = vec![];
            s.read_to_end(&mut b).unwrap();
            // Hold the peer open until write has returned, even if the writer
            // is descheduled for longer than its timeout. A fixed sleep races
            // its read on a loaded runner.
            held.recv().unwrap();
            drop(s);
            b
        });
        let outcome = write(&e, "{}\n");
        release.send(()).unwrap();
        let received = worker.join().unwrap();
        assert_eq!(outcome, SocketOutcome::MaybeSent("close_timeout".into()));
        assert_eq!(received, b"{}\n");
    }
    #[test]
    fn eof_observed_after_the_deadline_is_possible_delivery() {
        use std::pin::Pin;
        use std::task::{Context, Poll};
        /// Models the task being descheduled inside the read: the read
        /// returns EOF at once, but only after the absolute deadline.
        struct LateEof(Instant);
        impl AsyncRead for LateEof {
            fn poll_read(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
                _: &mut tokio::io::ReadBuf<'_>,
            ) -> Poll<std::io::Result<()>> {
                std::thread::sleep(self.0.saturating_duration_since(Instant::now()));
                Poll::Ready(Ok(()))
            }
        }
        let deadline = Instant::now() + Duration::from_millis(50);
        let outcome = io_runtime()
            .unwrap()
            .block_on(wait_for_close(deadline, &mut LateEof(deadline)));
        assert_eq!(outcome, SocketOutcome::MaybeSent("close_timeout".into()));
    }
    #[test]
    fn every_send_runs_on_the_one_shared_runtime() {
        // D12: no thread and runtime per message.
        let name = io_runtime().unwrap().block_on(async {
            tokio::spawn(async { std::thread::current().name().map(String::from) })
                .await
                .unwrap()
        });
        assert_eq!(name.as_deref(), Some("c9-mail-io"));
    }
    #[test]
    fn unsafe_socket_nodes_and_directories_are_unreachable() {
        let t = tempfile::tempdir().unwrap();
        let pid = std::process::id() as i32;
        let start = peer::proc_info(pid).unwrap().start;
        // A recognized namespace that does not exist is unreachable, never old CC.
        let path = PathBuf::from(format!(
            "/tmp/cc-socks-{}/missing-m2.sock",
            peer::engine_uid()
        ));
        std::fs::write(
            t.path().join(format!("{}.json", pid)),
            json!({"sessionId":"s1","peerProtocol":1,"messagingSocketPath":path}).to_string(),
        )
        .unwrap();
        assert!(matches!(
            gate("claude", pid, start, "s1", t.path(), false),
            Gate::Unreachable(_)
        ));
        // Arbitrary owned private directories still cannot become CC namespaces.
        let path = t.path().join("cc.sock");
        let _socket = UnixListener::bind(&path).unwrap();
        std::fs::write(
            t.path().join(format!("{}.json", pid)),
            json!({"sessionId":"s1","peerProtocol":1,"messagingSocketPath":path}).to_string(),
        )
        .unwrap();
        assert!(matches!(
            gate("claude", pid, start, "s1", t.path(), false),
            Gate::Unreachable(_)
        ));
        assert!(check_dir(t.path()).is_err());
    }
    #[test]
    fn socket_and_directory_metadata_refuse_links_regular_files_and_bad_modes() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("node");
        assert!(check_socket_node(&p).is_err());
        std::fs::write(&p, "data").unwrap();
        assert!(check_socket_node(&p).is_err());
        let link = t.path().join("link");
        symlink(&p, &link).unwrap();
        assert!(check_socket_node(&link).is_err());
        let sock = t.path().join("cc.sock");
        let _server = UnixListener::bind(&sock).unwrap();
        assert!(check_socket_node(&sock).is_ok());
        let dir = t.path().join("dir");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(check_owned_dir(&dir).is_ok());
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(check_owned_dir(&dir).is_err());
        let linkdir = t.path().join("linkdir");
        symlink(&dir, &linkdir).unwrap();
        assert!(check_owned_dir(&linkdir).is_err());
    }
}
