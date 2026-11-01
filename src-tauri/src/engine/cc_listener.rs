//! Stable engine peer sockets. Only authenticated current engine PTY children
//! can submit receipts. SendMessage replies are audited and redirected to CLI.
use super::{
    cc_inbox,
    db::{
        audit::{self, AuditEntry},
        Db, Tx,
    },
    mail,
    merge::MergeTable,
    peer,
};
use rusqlite::{params, OptionalExtension};
use serde_json::Value;
use std::collections::HashMap;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;

pub struct Sockets {
    db: Arc<Db>,
    merge: Arc<MergeTable>,
    runtime: tokio::runtime::Handle,
    paths: Mutex<HashMap<PathBuf, (tokio::task::JoinHandle<()>, u64)>>,
    spawner: Mutex<Weak<super::create::Spawner>>,
    slots: Arc<tokio::sync::Semaphore>,
    rates: Mutex<HashMap<libc::pid_t, (Instant, usize)>>,
}
impl Sockets {
    pub fn new(db: Arc<Db>, merge: Arc<MergeTable>) -> Arc<Self> {
        Arc::new(Self {
            db,
            merge,
            runtime: tokio::runtime::Handle::current(),
            paths: Mutex::new(HashMap::new()),
            spawner: Mutex::new(Weak::new()),
            slots: Arc::new(tokio::sync::Semaphore::new(16)),
            rates: Mutex::new(HashMap::new()),
        })
    }
    pub fn set_spawner(&self, s: &Arc<super::create::Spawner>) {
        *self.spawner.lock().unwrap() = Arc::downgrade(s);
    }
    pub fn bind(self: &Arc<Self>, dir: &Path) -> Result<PathBuf, String> {
        if !cc_inbox::enabled() {
            return Err("CC mail socket is disabled".into());
        }
        cc_inbox::check_namespace(dir)?;
        if !dir.try_exists().map_err(|e| e.to_string())? {
            use std::os::unix::fs::DirBuilderExt;
            match std::fs::DirBuilder::new().mode(0o700).create(dir) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.to_string()),
            }
        }
        cc_inbox::check_dir(dir)?;
        let machine = self.db.machine_id();
        let path = dir.join(format!(
            "c9w-{}.sock",
            machine.chars().take(8).collect::<String>()
        ));
        cc_inbox::address(&path)?;
        let mut paths = self.paths.lock().unwrap();
        if paths.contains_key(&path) {
            return Ok(path);
        }
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_socket() && m.uid() == peer::engine_uid() => {
                match std::os::unix::net::UnixStream::connect(&path) {
                    Ok(_) => return Err("another engine owns the CC namespace".into()),
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                        ) =>
                    {
                        std::fs::remove_file(&path).map_err(|e| e.to_string())?
                    }
                    Err(e) => return Err(e.to_string()),
                }
            }
            Ok(_) => return Err("refusing non-socket or foreign engine endpoint".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        let listener = std::os::unix::net::UnixListener::bind(&path).map_err(|e| e.to_string())?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
        let inode = std::fs::symlink_metadata(&path)
            .map_err(|e| e.to_string())?
            .ino();
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let _enter = self.runtime.enter();
        let listener = tokio::net::UnixListener::from_std(listener).map_err(|e| e.to_string())?;
        let weak = Arc::downgrade(self);
        let task = self.runtime.spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let Some(this) = weak.upgrade() else {
                    break;
                };
                let Ok(slot) = this.slots.clone().try_acquire_owned() else {
                    eprintln!("[engine] CC inbound connection limit");
                    continue;
                };
                tokio::spawn(async move {
                    let _slot = slot;
                    if let Err(e) = this.receive(stream).await {
                        eprintln!("[engine] CC inbound: {}", e);
                    }
                });
            }
        });
        paths.insert(path.clone(), (task, inode));
        Ok(path)
    }
    async fn receive(&self, mut stream: tokio::net::UnixStream) -> Result<(), String> {
        if !cc_inbox::enabled() {
            return Err("CC mail socket is disabled".into());
        }
        let cred = peer::peer_cred(stream.as_raw_fd())?;
        peer::check_peer_uid(&cred, peer::engine_uid())?;
        {
            let mut rates = self.rates.lock().unwrap();
            rates.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(60));
            let entry = rates.entry(cred.pid).or_insert((Instant::now(), 0));
            entry.1 += 1;
            if entry.1 > 30 {
                return Err("CC inbound pid rate limit".into());
            }
        }
        let peer_identity = {
            let c = self.db.reader()?;
            identity(&c, cred.pid).map_err(|e| e.to_string())?
        };
        // Read through EOF, bounded by bytes and time; close only after commit.
        let bytes = tokio::time::timeout(Duration::from_secs(30), async {
            let mut out = Vec::new();
            let mut b = [0; 8192];
            loop {
                let n = stream.read(&mut b).await?;
                if n == 0 {
                    break;
                }
                out.extend_from_slice(&b[..n]);
                if out.len() > 1024 * 1024 {
                    return Err(std::io::Error::other("CC line over 1 MiB"));
                }
            }
            Ok::<_, std::io::Error>(out)
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
        let Some((agent, gen)) = peer_identity else {
            let pid = cred.pid;
            self.db
                .write(move |tx| {
                    audit::append(
                        tx,
                        super::db::now_ms(),
                        &AuditEntry {
                            principal: "local-unverified",
                            method: "cc.inbound",
                            decision: "deny",
                            reason: Some(&format!("unknown peer pid={}", pid)),
                            ..Default::default()
                        },
                    )
                })
                .await?;
            return Ok(());
        };
        let frame: Value =
            serde_json::from_slice(bytes.split(|b| *b == b'\n').next().unwrap_or(&[]))
                .map_err(|e| e.to_string())?;
        let (machine, who, merge) = (
            self.db.machine_id().to_string(),
            agent.clone(),
            self.merge.clone(),
        );
        let pid = cred.pid;
        self.db
            .write(move |tx| {
                // Revalidate after I/O: generation/pid may have changed while reading.
                if identity(tx, pid)? != Some((who.clone(), gen)) {
                    return Ok(());
                }
                apply(tx, &machine, &who, gen, &frame, &merge, super::db::now_ms())
            })
            .await?;
        if let Some(s) = self.spawner.lock().unwrap().upgrade() {
            s.nudge_mail(&agent);
        }
        drop(stream);
        Ok(())
    }
    pub fn close(&self) {
        for (p, (task, inode)) in self.paths.lock().unwrap().drain() {
            task.abort();
            if std::fs::symlink_metadata(&p).is_ok_and(|m| {
                m.file_type().is_socket() && m.ino() == inode && m.uid() == peer::engine_uid()
            }) {
                let _ = std::fs::remove_file(p);
            }
        }
    }
}
/// Ensures all boot-error exits close sockets even while an inbound task holds an Arc.
pub struct SocketCleanup(pub Arc<Sockets>);
impl Drop for SocketCleanup {
    fn drop(&mut self) {
        self.0.close();
    }
}

impl Drop for Sockets {
    fn drop(&mut self) {
        self.close();
    }
}
/// Detected sessions and reused PIDs can never authenticate a receipt.
pub fn identity(
    c: &rusqlite::Connection,
    pid: libc::pid_t,
) -> rusqlite::Result<Option<(String, i64)>> {
    let Some(proc) = peer::proc_info(pid) else {
        return Ok(None);
    };
    c.query_row("SELECT a.id, a.generation FROM agents a JOIN agent_state s ON s.agent_id = a.id AND s.gen = a.generation WHERE s.pty_child_pid = ?1 AND s.pty_child_start = ?2 AND a.source = 'engine' AND a.control = 'pty' AND a.state NOT IN ('paused','interrupted','retiring','retired','failed','orphaned')", params![pid, proc.start as i64], |r| Ok((r.get(0)?, r.get(1)?))).optional()
}
pub fn apply(
    tx: &mut Tx,
    machine: &str,
    agent: &str,
    gen: i64,
    frame: &Value,
    merge: &MergeTable,
    now: i64,
) -> rusqlite::Result<()> {
    if frame["type"] == "control" && frame["action"] == "peer_message_status" {
        let wire = frame["orig_msg_id"].as_str().unwrap_or("");
        let msg: Option<(String, String, i64)> = tx.query_row("SELECT id, state, attempts FROM messages WHERE wire_id = ?1 AND to_agent = ?2 AND via = 'socket'", params![wire, agent], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?;
        let Some((id, state, attempts)) = msg else {
            eprintln!("[engine] ignored CC receipt: unknown wire id or wrong target");
            return Ok(());
        };
        let status = frame["status"].as_str().unwrap_or("");
        let detail = frame["status_detail"].as_str().unwrap_or("");
        if state == "uncertain" {
            mail::note(
                tx,
                machine,
                &id,
                &format!("{} after uncertain: {}", status, detail),
                now,
            )?;
        } else if state == "delivering" {
            match status {
                "held" => {
                    mail::note(tx, machine, &id, &format!("held: {}", detail), now)?;
                }
                "delivered" => {
                    mail::note(tx, machine, &id, "cc accepted; awaiting evidence", now)?;
                }
                "denied" => {
                    mail::mark(
                        tx,
                        machine,
                        &id,
                        mail::MsgState::Rejected,
                        Some("denied by the user"),
                        now,
                    )?;
                }
                "expired" => {
                    mail::mark(tx, machine, &id, mail::MsgState::Expired, Some(detail), now)?;
                }
                "dropped" => {
                    let reason = frame["drop_reason"].as_str().unwrap_or(detail);
                    match reason {
                        "rate-limited" | "queue-full" if attempts < mail::SOCKET_MAX_ATTEMPTS => {
                            let delay = match attempts {
                                0 | 1 => 10_000,
                                2 => 30_000,
                                _ => 60_000,
                            };
                            mail::requeue_after(tx, machine, &id, reason, Some(now + delay), now)?;
                        }
                        "duplicate" => {
                            mail::mark(
                                tx,
                                machine,
                                &id,
                                mail::MsgState::Uncertain,
                                Some("cc duplicate"),
                                now,
                            )?;
                        }
                        "rate-limited" | "queue-full" | "hop-loop" | "hop-runaway" => {
                            mail::mark(
                                tx,
                                machine,
                                &id,
                                mail::MsgState::Failed,
                                Some(reason),
                                now,
                            )?;
                        }
                        _ => {
                            mail::note(
                                tx,
                                machine,
                                &id,
                                &format!("unknown drop: {}", reason),
                                now,
                            )?;
                        }
                    }
                }
                _ => {}
            }
        }
        if status == "held" && matches!(state.as_str(), "delivering" | "uncertain") {
            if let Some(((), staged)) = merge.stage_existing(agent, gen, |m| {
                m.attention(&format!("mail_held: {}", id), now)
            }) {
                merge.publish_on_commit(tx, staged);
            }
            tx.emit(&super::db::outbox::NewEvent { topic: "agent.attention".into(), subject: Some(format!("{}/{}", machine, agent)), origin: None, data: serde_json::json!({"id":agent,"generation":gen,"reason":"mail_held","msgId":id}) })?;
        }
    } else if frame["type"] == "user" {
        let body = frame["message"]["content"].as_str().unwrap_or("");
        let who = format!("agent:{}", agent);
        let recent: bool = tx.query_row("SELECT count(*) > 0 FROM audit WHERE method = 'cc.inbound' AND principal = ?1 AND reason = 'use_cli' AND ts > ?2", params![who, now - 600_000], |r| r.get(0))?;
        let system_reply: bool = {
            let mut st =
                tx.prepare("SELECT id FROM messages WHERE kind = 'system' AND to_agent = ?1")?;
            let ids = st
                .query_map([agent], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            ids.iter().any(|id| body.contains(id))
        };
        audit::append(
            tx,
            now,
            &AuditEntry {
                principal: &who,
                method: "cc.inbound",
                target: Some(agent),
                decision: "deny",
                reason: Some("use_cli"),
                payload_hash: Some(&mail::sha256_hex(body.as_bytes())),
                ..Default::default()
            },
        )?;
        if !recent && !system_reply {
            mail::system(tx, machine, agent, "Your SendMessage reply was not delivered. Use `c9watch send <addr> --reply-to <msg> \"<text>\"`. Do not reply to this message.", None, now, now)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{
        generation::tests::{db_with_agent, MACHINE},
        principal::Principal,
    };
    use crate::engine_proto::wire::MsgSendParams;
    use serde_json::json;
    // Keep gate settings in child processes: these tests run concurrently.
    fn socket_gate_child(name: &str, enabled: bool) -> bool {
        if std::env::var("C9_TEST_CC_GATE_CHILD").as_deref() == Ok(name) {
            return true;
        }
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            &format!("engine::cc_listener::tests::{name}"),
            "--nocapture",
        ])
        .env("C9_TEST_CC_GATE_CHILD", name);
        if enabled {
            cmd.env("C9_MAIL_SOCKET", "on");
        } else {
            cmd.env_remove("C9_MAIL_SOCKET");
        }
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        false
    }
    #[tokio::test]
    async fn m2fix_disabled_socket_rejects_inbound_without_side_effects() {
        if !socket_gate_child(
            "m2fix_disabled_socket_rejects_inbound_without_side_effects",
            false,
        ) {
            return;
        }
        use tokio::io::AsyncWriteExt;
        let (_t, db, _id, _wire) = setup();
        let pid = std::process::id() as i32;
        let start = peer::proc_info(pid).unwrap().start;
        db.write_blocking(move |tx| tx.execute("INSERT INTO agent_state(agent_id,gen,pty_child_pid,pty_child_start) VALUES ('b',1,?1,?2)", params![pid,start as i64])).unwrap();
        let db = Arc::new(db);
        let sockets = Sockets::new(db.clone(), Arc::new(MergeTable::new()));
        let namespace = tempfile::tempdir().unwrap();
        let disabled_dir = namespace.path().join("disabled");
        assert!(sockets
            .bind(&disabled_dir)
            .unwrap_err()
            .contains("disabled"));
        assert!(!disabled_dir.exists());
        let (server, mut client) = tokio::net::UnixStream::pair().unwrap();
        client
            .write_all(b"{\"type\":\"user\",\"message\":{\"content\":\"reply\"}}\n")
            .await
            .unwrap();
        client.shutdown().await.unwrap();
        assert!(sockets
            .receive(server)
            .await
            .unwrap_err()
            .contains("disabled"));
        let c = db.reader().unwrap();
        for sql in [
            "SELECT count(*) FROM audit WHERE method='cc.inbound'",
            "SELECT count(*) FROM messages WHERE kind='system'",
        ] {
            assert_eq!(c.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        }
    }
    fn setup() -> (tempfile::TempDir, Db, String, String) {
        let (t, db) = db_with_agent("a");
        db.write_blocking(|tx| { tx.execute("UPDATE agents SET state='ready', control='pty', generation=1 WHERE id='a'",[])?;tx.execute("INSERT INTO agents(id,name,role,provider,source,control,state,generation) VALUES ('b','b','worker','claudeCode','engine','pty','ready',1)",[])?;Ok(()) }).unwrap();
        let (id, wire) = db
            .write_blocking(|tx| {
                let sender = mail::sender(
                    tx,
                    &Principal::Agent {
                        id: "a".into(),
                        gen: 1,
                    },
                )?;
                let target = mail::resolve(tx, MACHINE, "b")?.unwrap();
                let sent = mail::enqueue(
                    tx,
                    MACHINE,
                    &sender,
                    &target,
                    &MsgSendParams {
                        to: "b".into(),
                        body: "hi".into(),
                        ..Default::default()
                    },
                    None,
                    "user",
                    1000,
                )?
                .unwrap();
                mail::take_next(tx, MACHINE, "b", 1000)?;
                mail::set_via(tx, &sent.msg_id, "socket")?;
                let wire = tx.query_row(
                    "SELECT wire_id FROM messages WHERE id=?1",
                    [&sent.msg_id],
                    |r| r.get::<_, String>(0),
                )?;
                Ok((sent.msg_id, wire))
            })
            .unwrap()
            .0;
        (t, db, id, wire)
    }
    fn receipt(db: &Db, who: &str, wire: &str, status: &str, reason: Option<&str>) {
        let (who, wire, status, reason) = (
            who.to_string(),
            wire.to_string(),
            status.to_string(),
            reason.map(str::to_string),
        );
        db.write_blocking(move |tx|apply(tx,MACHINE,&who,1,&json!({"type":"control","action":"peer_message_status","orig_msg_id":wire,"status":status,"drop_reason":reason}),&MergeTable::new(),2000)).unwrap();
    }
    #[test]
    fn receipts_are_target_bound_and_delivered_never_confirms_context() {
        let (_t, db, id, wire) = setup();
        receipt(&db, "a", &wire, "denied", None);
        assert_eq!(
            mail::state_of(&db.reader().unwrap(), &id).unwrap(),
            Some(mail::MsgState::Delivering)
        );
        receipt(&db, "b", &wire, "held", None);
        assert!(mail::latest_detail(&db.reader().unwrap(), &id)
            .unwrap()
            .unwrap()
            .starts_with("held:"));
        receipt(&db, "b", &wire, "delivered", None);
        assert_eq!(
            mail::state_of(&db.reader().unwrap(), &id).unwrap(),
            Some(mail::MsgState::Delivering)
        );
        receipt(&db, "b", &wire, "denied", None);
        assert_eq!(
            mail::state_of(&db.reader().unwrap(), &id).unwrap(),
            Some(mail::MsgState::Rejected)
        );
    }
    #[test]
    fn dropped_retries_are_delayed_duplicate_is_uncertain_and_late_receipts_are_notes() {
        let (_t, db, id, wire) = setup();
        receipt(&db, "b", &wire, "dropped", Some("rate-limited"));
        let c = db.reader().unwrap();
        let (state, after): (String, i64) = c
            .query_row(
                "SELECT state,not_before FROM messages WHERE id=?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "queued");
        assert_eq!(after, 12000);
        db.write_blocking(|tx| {
            assert!(mail::take_next(tx, MACHINE, "b", 11999)?.is_none());
            assert!(mail::take_next(tx, MACHINE, "b", 12000)?.is_some());
            Ok(())
        })
        .unwrap();
        receipt(&db, "b", &wire, "dropped", Some("duplicate"));
        receipt(&db, "b", &wire, "expired", None);
        assert_eq!(
            mail::state_of(&c, &id).unwrap(),
            Some(mail::MsgState::Uncertain)
        );
        assert!(mail::latest_detail(&c, &id)
            .unwrap()
            .unwrap()
            .contains("after uncertain"));
    }
    #[test]
    fn sendmessage_is_audited_once_not_imported_and_system_replies_do_not_loop() {
        let (_t, db, _id, _wire) = setup();
        for _ in 0..2 {
            db.write_blocking(|tx| {
                apply(
                    tx,
                    MACHINE,
                    "b",
                    1,
                    &json!({"type":"user","message":{"content":"reply"}}),
                    &MergeTable::new(),
                    2000,
                )
            })
            .unwrap();
        }
        let c = db.reader().unwrap();
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM audit WHERE method='cc.inbound' AND reason='use_cli'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            2
        );
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM messages WHERE kind='system'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            c.query_row("SELECT count(*) FROM messages", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
    }
    #[test]
    fn detected_stale_generation_and_reused_start_cannot_authenticate() {
        let (_t, db, _id, _wire) = setup();
        let pid = std::process::id() as i32;
        let start = peer::proc_info(pid).unwrap().start;
        db.write_blocking(move |tx|tx.execute("INSERT INTO agent_state(agent_id,gen,pty_child_pid,pty_child_start) VALUES ('b',1,?1,?2)",params![pid,start as i64])).unwrap();
        let c = db.reader().unwrap();
        assert_eq!(identity(&c, pid).unwrap(), Some(("b".into(), 1)));
        for sql in [
            "UPDATE agent_state SET pty_child_start=0",
            "UPDATE agent_state SET gen=99",
            "UPDATE agents SET source='detected' WHERE id='b'",
        ] {
            db.write_blocking(move |tx| {
                tx.execute(
                    "UPDATE agent_state SET gen=1,pty_child_start=?1",
                    [start as i64],
                )?;
                tx.execute("UPDATE agents SET source='engine' WHERE id='b'", [])
            })
            .unwrap();
            let sql = sql.to_string();
            db.write_blocking(move |tx| tx.execute(&sql, [])).unwrap();
            assert_eq!(identity(&c, pid).unwrap(), None);
        }
    }
    #[tokio::test]
    async fn eof_closes_after_the_sendmessage_audit_and_notice_commit() {
        if !socket_gate_child(
            "eof_closes_after_the_sendmessage_audit_and_notice_commit",
            true,
        ) {
            return;
        }
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (_t, db, _id, _wire) = setup();
        let pid = std::process::id() as i32;
        let start = peer::proc_info(pid).unwrap().start;
        db.write_blocking(move |tx|tx.execute("INSERT INTO agent_state(agent_id,gen,pty_child_pid,pty_child_start) VALUES ('b',1,?1,?2)",params![pid,start as i64])).unwrap();
        let db = Arc::new(db);
        let sockets = Sockets::new(db.clone(), Arc::new(MergeTable::new()));
        let (server, mut client) = tokio::net::UnixStream::pair().unwrap();
        let receiver = sockets.clone();
        let task = tokio::spawn(async move { receiver.receive(server).await });
        client
            .write_all(b"{\"type\":\"user\",\"message\":{\"content\":\"reply\"}}\n")
            .await
            .unwrap();
        client.shutdown().await.unwrap();
        let mut b = vec![];
        tokio::time::timeout(Duration::from_secs(3), client.read_to_end(&mut b))
            .await
            .unwrap()
            .unwrap();
        task.await.unwrap().unwrap();
        assert!(b.is_empty());
        let c = db.reader().unwrap();
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM audit WHERE method='cc.inbound' AND reason='use_cli'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM messages WHERE kind='system'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        let id: String = c
            .query_row("SELECT id FROM messages WHERE kind='system'", [], |r| {
                r.get(0)
            })
            .unwrap();
        db.write(move |tx| {
            apply(
                tx,
                MACHINE,
                "b",
                1,
                &json!({"type":"user","message":{"content":id}}),
                &MergeTable::new(),
                1_000_000,
            )
        })
        .await
        .unwrap();
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM messages WHERE kind='system'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }
    #[tokio::test]
    async fn oversized_lines_are_disconnected_and_only_sixteen_inbound_slots_exist() {
        if !socket_gate_child(
            "oversized_lines_are_disconnected_and_only_sixteen_inbound_slots_exist",
            true,
        ) {
            return;
        }
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (_t, db, _id, _wire) = setup();
        let sockets = Sockets::new(Arc::new(db), Arc::new(MergeTable::new()));
        let mut permits = vec![];
        for _ in 0..16 {
            permits.push(sockets.slots.clone().try_acquire_owned().unwrap());
        }
        assert!(sockets.slots.clone().try_acquire_owned().is_err());
        drop(permits);
        let (server, mut client) = tokio::net::UnixStream::pair().unwrap();
        let receiver = sockets.clone();
        let task = tokio::spawn(async move { receiver.receive(server).await });
        // write_all can observe the close before finishing the oversized body.
        let _ = client.write_all(&vec![b'x'; 1024 * 1024 + 1]).await;
        let _ = client.shutdown().await;
        let mut b = vec![];
        let _ = tokio::time::timeout(Duration::from_secs(3), client.read_to_end(&mut b))
            .await
            .unwrap();
        assert!(task.await.unwrap().unwrap_err().contains("1 MiB"));
    }
}
