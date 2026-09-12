//! In-memory owner-transport interaction tracking. Never reconstruct actionable requests
//! from transcripts. Private controls validate decisions against the live request.
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{mpsc, oneshot},
};
use tokio_tungstenite::{tungstenite::Message, WebSocketStream};

const MAX_WIRE: usize = 8 * 1024 * 1024;
const MAX_CONTROL: usize = 8 * 1024 * 1024;
#[path = "codex_interaction_actions.rs"]
mod actions;
const MAX_PENDING: usize = 64;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    pub id: String,
    pub header: String,
    pub question: String,
    #[serde(default)]
    pub options: Vec<OptionLabel>,
    #[serde(default)]
    pub is_other: bool,
    #[serde(default)]
    pub is_secret: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OptionLabel {
    pub label: String,
    #[serde(default)]
    pub description: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pending {
    pub token: String,
    pub thread_id: String,
    pub turn_id: String,
    pub kind: String,
    pub summary: String,
    pub questions: Vec<Question>,
    pub submitted: bool,
    pub answerable: bool,
    #[serde(default)]
    pub details: Value,
    #[serde(default)]
    pub actions: Vec<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub endpoint: String,
    pub connected: bool,
    pub pending: Vec<Pending>,
    pub statuses: BTreeMap<String, String>,
    pub overflow: bool,
    #[serde(default)]
    pub turns: BTreeMap<String, TurnProgress>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnProgress {
    pub turn_id: String,
    pub status: String,
    pub stopping: bool,
    pub plan: Value,
    pub explanation: String,
    pub diff: String,
    pub diff_truncated: bool,
}
struct Entry {
    params: Value,
    id: Value,
    view: Pending,
}
#[derive(Default)]
struct Registry {
    entries: Vec<Entry>,
    statuses: BTreeMap<String, String>,
    turns: BTreeMap<String, TurnProgress>,
    items: BTreeMap<(String, String, String), Value>,
    // c9watch-claimed IDs remain tombstones for this connection; drop late Desktop answers.
    answered: VecDeque<Value>,
    overflow: bool,
}
fn text(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or_default().to_owned()
}
impl Registry {
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            connected: true,
            pending: self
                .entries
                .iter()
                .map(|e| {
                    let mut view = e.view.clone();
                    view.answerable &= self.answered.len() < 1024;
                    if self.answered.len() >= 1024 {
                        view.actions.clear();
                    }
                    view
                })
                .collect(),
            statuses: self.statuses.clone(),
            turns: self.turns.clone(),
            overflow: self.overflow,
            ..Default::default()
        }
    }
    fn remember(&mut self, id: Value) {
        if !self.answered.contains(&id) {
            self.answered.push_back(id);
        }
    }
    fn clear(&mut self, thread: &str, turn: Option<&str>) {
        self.entries
            .retain(|e| !(e.view.thread_id == thread && turn.is_none_or(|t| e.view.turn_id == t)));
    }
    fn observe(&mut self, v: &Value) {
        let method = v["method"].as_str().unwrap_or_default();
        let p = &v["params"];
        let thread = text(p, "threadId");
        if self.observe_progress(method, p) {
            return;
        }
        match method {
            "serverRequest/resolved" => {
                let id = &p["requestId"];
                self.entries
                    .retain(|e| !(e.id == *id && e.view.thread_id == thread));
                return;
            }
            "turn/started" => {
                if self.statuses.len() < 512 || self.statuses.contains_key(&thread) {
                    self.statuses.insert(thread, "active".into());
                }
                return;
            }
            "turn/completed" => {
                self.clear(&thread, p["turn"]["id"].as_str());
                if self
                    .turns
                    .get(&thread)
                    .is_some_and(|t| Some(t.turn_id.as_str()) != p["turn"]["id"].as_str())
                {
                    return;
                }
                if self.statuses.len() < 512 || self.statuses.contains_key(&thread) {
                    self.statuses.insert(thread, "idle".into());
                }
                return;
            }
            "thread/closed" | "thread/archived" => {
                self.clear(&thread, None);
                self.statuses.remove(&thread);
                self.turns.remove(&thread);
                self.items.retain(|(t, _, _), _| t != &thread);
                return;
            }
            "thread/status/changed" => {
                if self.statuses.len() >= 512 && !self.statuses.contains_key(&thread) {
                    return;
                }
                let status = &p["status"];
                if status["type"] == "notLoaded" {
                    self.clear(&thread, None);
                    self.turns.remove(&thread);
                    self.items.retain(|(t, _, _), _| t != &thread);
                }
                let flags = status["activeFlags"].as_array();
                let label = if flags.is_some_and(|f| {
                    f.iter()
                        .any(|s| s == "waitingOnApproval" || s == "waitingOnUserInput")
                }) {
                    "waiting"
                } else {
                    status["type"].as_str().unwrap_or("unknown")
                };
                self.statuses.insert(thread, label.into());
                return;
            }
            _ => (),
        }
        let kind = match method {
            "item/tool/requestUserInput" => "question",
            "item/commandExecution/requestApproval" => "command",
            "item/fileChange/requestApproval" => "file",
            "item/permissions/requestApproval" => "permission",
            "mcpServer/elicitation/request" => "form",
            _ => return,
        };
        let Some(id) = v.get("id").filter(|id| id.is_string() || id.is_i64()) else {
            return;
        };
        if thread.is_empty() || self.entries.iter().any(|e| e.id == *id) {
            return;
        }
        self.answered.retain(|old| old != id);
        if self.entries.len() >= MAX_PENDING || v.to_string().len() > 32 * 1024 {
            self.overflow = true;
            if self.statuses.len() < 512 || self.statuses.contains_key(&thread) {
                self.statuses.insert(thread, "waiting".into());
            }
            return;
        }
        let questions = p["questions"]
            .as_array()
            .and_then(|qs| {
                if qs.is_empty() || qs.len() > 8 {
                    return None;
                }
                let mut result = Vec::new();
                for q in qs {
                    let mut q = q.clone();
                    if q["options"].is_null() {
                        q["options"] = json!([]);
                    }
                    let parsed: Question = serde_json::from_value(q).ok()?;
                    if parsed.id.is_empty() || result.iter().any(|x: &Question| x.id == parsed.id) {
                        return None;
                    }
                    result.push(parsed);
                }
                Some(result)
            })
            .unwrap_or_default();
        let summary = if kind == "question" {
            questions
                .first()
                .map(|q| q.question.clone())
                .unwrap_or_else(|| "Answer in Codex (unsupported question format)".into())
        } else {
            let reason = text(p, "reason");
            if !reason.is_empty() {
                reason
            } else {
                match kind {
                    "command" => text(p, "command"),
                    "file" => "File changes need approval in Codex".into(),
                    "permission" => "Permissions need approval in Codex".into(),
                    _ => "Complete this form in Codex".into(),
                }
            }
        };
        // Secret questions stay in the owner UI. Their answers must never enter UI stores/logs.
        let answerable =
            kind == "question" && !questions.is_empty() && !questions.iter().any(|q| q.is_secret);
        let item_key = (thread.clone(), text(p, "turnId"), text(p, "itemId"));
        let details = actions::details(kind, p, self.items.get(&item_key));
        let available_actions = actions::available(kind, p, &details);
        self.entries.push(Entry {
            params: p.clone(),
            id: id.clone(),
            view: Pending {
                token: uuid::Uuid::new_v4().to_string(),
                thread_id: thread,
                turn_id: text(p, "turnId"),
                kind: kind.into(),
                summary,
                questions,
                answerable,
                submitted: false,
                details,
                actions: available_actions,
            },
        });
    }
    fn observe_progress(&mut self, method: &str, p: &Value) -> bool {
        let thread = text(p, "threadId");
        let turn = p["turnId"]
            .as_str()
            .or_else(|| p["turn"]["id"].as_str())
            .unwrap_or_default()
            .to_owned();
        if thread.is_empty() {
            return false;
        }
        if matches!(method, "item/started" | "item/completed") {
            let item = &p["item"];
            let key = (thread.clone(), turn.clone(), text(item, "id"));
            if matches!(
                item["type"].as_str(),
                Some("fileChange" | "commandExecution")
            ) {
                if self.items.len() >= 64 && !self.items.contains_key(&key) {
                    self.items.pop_first();
                }
                // Do not retain command output; file diffs are bounded without truncating approvals.
                let cached = if item["type"] == "commandExecution" {
                    json!({"command":item["command"],"cwd":item["cwd"]})
                } else {
                    json!({"changes":item["changes"]})
                };
                if cached.to_string().len() <= 32 * 1024 {
                    self.items.insert(key.clone(), cached);
                } else {
                    self.items.remove(&key);
                }
                for entry in self.entries.iter_mut().filter(|e| {
                    e.view.thread_id == thread
                        && e.view.turn_id == turn
                        && e.params["itemId"] == item["id"]
                        && !e.view.submitted
                }) {
                    let details =
                        actions::details(&entry.view.kind, &entry.params, self.items.get(&key));
                    if details != entry.view.details {
                        entry.view.details = details;
                        entry.view.actions = actions::available(
                            &entry.view.kind,
                            &entry.params,
                            &entry.view.details,
                        );
                        // A different proposal needs a fresh UI decision.
                        entry.view.token = uuid::Uuid::new_v4().to_string();
                    }
                }
                if method == "item/completed" {
                    self.items.remove(&key);
                    self.entries.retain(|e| {
                        !(e.view.thread_id == thread
                            && e.view.turn_id == turn
                            && e.params["itemId"] == item["id"])
                    });
                }
            }
            return true;
        }
        if !matches!(
            method,
            "turn/started" | "turn/completed" | "turn/plan/updated" | "turn/diff/updated"
        ) || turn.is_empty()
        {
            return false;
        }
        if self.turns.len() >= 32 && !self.turns.contains_key(&thread) {
            if let Some(old) = self
                .turns
                .iter()
                .find(|(_, t)| t.status != "inProgress")
                .map(|(id, _)| id.clone())
            {
                self.turns.remove(&old);
            } else {
                return false;
            }
        }
        if method == "turn/started" {
            self.items.retain(|(t, _, _), _| t != &thread);
            self.turns.insert(
                thread.clone(),
                TurnProgress {
                    turn_id: turn.clone(),
                    status: "inProgress".into(),
                    ..Default::default()
                },
            );
        }
        let Some(progress) = self.turns.get_mut(&thread).filter(|t| t.turn_id == turn) else {
            return false;
        };
        match method {
            "turn/completed" => {
                progress.status = p["turn"]["status"].as_str().unwrap_or("completed").into();
                progress.stopping = false;
                self.items
                    .retain(|(t, tr, _), _| t != &thread || tr != &turn);
            }
            "turn/plan/updated" => {
                if p["plan"].as_array().is_some_and(|v| v.len() <= 64)
                    && p.to_string().len() <= 16 * 1024
                {
                    progress.plan = p["plan"].clone();
                    progress.explanation = text(p, "explanation");
                }
            }
            "turn/diff/updated" => {
                let diff = text(p, "diff");
                progress.diff_truncated = diff.len() > 32 * 1024;
                progress.diff = diff.chars().take(8 * 1024).collect();
                progress.diff_truncated |= progress.diff.len() < diff.len();
            }
            _ => (),
        }
        // Lifecycle events also update pending/status state below.
        matches!(method, "turn/plan/updated" | "turn/diff/updated")
    }
    fn decide(
        &mut self,
        token: &str,
        thread: &str,
        action: &str,
        input: &Value,
    ) -> Result<Value, String> {
        if self.answered.len() >= 1024 {
            return Err("Decision limit reached. Continue in Codex.".into());
        }
        let entry = self
            .entries
            .iter_mut()
            .find(|e| e.view.token == token && e.view.thread_id == thread)
            .ok_or("This request changed or was cleared. Review the current card.")?;
        if entry.view.submitted {
            return Err("A decision was already submitted".into());
        }
        let result = actions::response(
            &entry.view.kind,
            &entry.params,
            &entry.view.details,
            action,
            input,
        )?;
        entry.view.submitted = true;
        let id = entry.id.clone();
        self.remember(id.clone());
        Ok(json!({"id":id,"result":result}))
    }
    fn interrupt(&mut self, thread: &str, turn: &str) -> Result<Value, String> {
        let progress = self
            .turns
            .get_mut(thread)
            .filter(|p| p.turn_id == turn && p.status == "inProgress" && !p.stopping)
            .ok_or("This turn is no longer active, or a stop is already pending")?;
        progress.stopping = true;
        Ok(
            json!({"id":format!("c9watch-stop-{}",uuid::Uuid::new_v4()),"method":"turn/interrupt","params":{"threadId":thread,"turnId":turn}}),
        )
    }
    fn owner_response(&mut self, v: &Value) -> bool {
        if v.get("method").is_some() {
            return true;
        }
        let Some(id) = v.get("id") else {
            return true;
        };
        if self.answered.contains(id) {
            return false;
        }
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == *id) {
            if entry.view.submitted {
                return false;
            }
            entry.view.submitted = true;
        }
        true
    }
    fn answer(&mut self, token: &str, thread: &str, answers: &Value) -> Result<Value, String> {
        if self.answered.len() >= 1024 {
            return Err(
                "Interaction answer limit reached. Continue in Codex until its next restart."
                    .into(),
            );
        }
        let entry = self
            .entries
            .iter_mut()
            .find(|e| e.view.token == token && e.view.thread_id == thread)
            .ok_or("This question has expired or was already cleared.")?;
        if !entry.view.answerable || entry.view.submitted {
            return Err("Answer in Codex or wait for the submitted response to resolve.".into());
        }
        let map = answers.as_object().ok_or("Invalid answers")?;
        if map.len() != entry.view.questions.len() || answers.to_string().len() > 32 * 1024 {
            return Err("Answer every question (up to 32 KiB total).".into());
        }
        for q in &entry.view.questions {
            let list = map
                .get(&q.id)
                .and_then(|v| v["answers"].as_array())
                .ok_or("Answer every question")?;
            if list.len() != 1 {
                return Err("Choose one answer for each question.".into());
            }
            let value = list[0]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .ok_or("Answers cannot be empty")?;
            if !q.options.is_empty() && !q.is_other && !q.options.iter().any(|o| o.label == value) {
                return Err("Choose an available option.".into());
            }
        }
        entry.view.submitted = true;
        let id = entry.id.clone();
        self.remember(id.clone());
        Ok(json!({"id":id,"result":{"answers":answers}}))
    }
}

struct Control {
    value: Value,
    reply: oneshot::Sender<Value>,
}

// A stalled owner pipe or WebSocket must not indefinitely hold the single
// interaction writer. Closing the bridge leaves uncertain responses uncertain.
async fn write_with_deadline<T, E: std::fmt::Display>(
    operation: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, String> {
    tokio::time::timeout(Duration::from_secs(3), operation)
        .await
        .map_err(|_| "Codex transport write timed out; peer is not reading".to_string())?
        .map_err(|error| error.to_string())
}
async fn control_listener(listener: UnixListener, tx: mpsc::Sender<Control>) {
    let slots = std::sync::Arc::new(tokio::sync::Semaphore::new(8));
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            break;
        };
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            continue;
        };
        let tx = tx.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _ = tokio::time::timeout(Duration::from_secs(4), async {
                let mut stream = BufReader::new(stream);
                let mut raw = Vec::new();
                (&mut stream)
                    .take(64 * 1024)
                    .read_until(b'\n', &mut raw)
                    .await
                    .ok()?;
                if !raw.ends_with(b"\n") {
                    return None;
                }
                let value = serde_json::from_slice(&raw).ok()?;
                let (reply, result) = oneshot::channel();
                tx.send(Control { value, reply }).await.ok()?;
                let value = result.await.ok()?;
                stream
                    .get_mut()
                    .write_all(format!("{value}\n").as_bytes())
                    .await
                    .ok()?;
                Some(())
            })
            .await;
        });
    }
}

/// One writer serializes Desktop and c9watch answers, so a race cannot answer twice.
pub async fn relay(ws: WebSocketStream<UnixStream>, directory: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let path = directory.join("interactions.sock");
    let listener = UnixListener::bind(&path).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    let (tx, mut rx) = mpsc::channel::<Control>(8);
    let listener = control_listener(listener, tx);
    tokio::pin!(listener);
    let (mut sink, mut source) = ws.split();
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut registry = Registry::default();
    let mut stop_replies: BTreeMap<String, (oneshot::Sender<Value>, String, String)> =
        BTreeMap::new();
    let mut stop_ids = std::collections::BTreeSet::new();
    let mut pending = Vec::new();
    let mut chunk = [0u8; 65536];
    loop {
        stop_replies.retain(|_, (reply, _, _)| !reply.is_closed());
        tokio::select! {
            _ = &mut listener => return Err("Interaction listener closed".into()),
            frame = source.next() => {
                let Some(frame) = frame else { return Ok(()); };
                match frame.map_err(|e| e.to_string())? {
                    Message::Text(raw) => {
                        let value: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
                        if value.get("method").is_none() {
                            if let Some((reply,thread,turn))=value["id"].as_str().and_then(|id|stop_replies.remove(id)) {
                                let failed=value.get("error").is_some();
                                if failed {if let Some(t)=registry.turns.get_mut(&thread).filter(|t|t.turn_id==turn) {t.stopping=false;}}
                                let _=reply.send(json!({"status":if failed {"rejected"}else{"submitted"},"detail":if failed {"Codex rejected the stop request."}else{"Stop requested. Waiting for the turn to finish."}}));
                                continue;
                            }
                        }
                        if value.get("method").is_none() && value["id"].as_str().is_some_and(|id|stop_ids.contains(id)) {continue;}
                        registry.observe(&value);
                        let line = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
                        write_with_deadline(async {
                            stdout.write_all(&line).await?;
                            stdout.write_all(b"\n").await?;
                            stdout.flush().await
                        }).await?;
                    }
                    Message::Close(_) => return Ok(()),
                    Message::Binary(_) => return Err("Unexpected binary protocol frame".into()),
                    _ => ()
                }
            }
            n = stdin.read(&mut chunk) => {
                let n = n.map_err(|e| e.to_string())?;
                if n == 0 { return if pending.iter().all(u8::is_ascii_whitespace) { Ok(()) } else { Err("Incomplete JSONL record at EOF".into()) }; }
                pending.extend_from_slice(&chunk[..n]);
                while let Some(end) = pending.iter().position(|b| *b == b'\n') {
                    if end > MAX_WIRE { return Err("JSONL record exceeds 8 MiB".into()); }
                    let line: Vec<_> = pending.drain(..=end).collect();
                    if line.iter().all(u8::is_ascii_whitespace) { continue; }
                    let value: Value = serde_json::from_slice(&line).map_err(|e| e.to_string())?;
                    if registry.owner_response(&value) { write_with_deadline(sink.send(Message::Text(value.to_string()))).await?; }
                }
                if pending.len() > MAX_WIRE { return Err("JSONL record exceeds 8 MiB".into()); }
            }
            Some(control) = rx.recv() => {
                // If the requesting client disappeared, do not submit its queued answer.
                if control.reply.is_closed() { continue; }
                if control.value["op"]=="interrupt" {
                    if stop_replies.len()>=8 || stop_ids.len()>=1024 {let _=control.reply.send(json!({"status":"not_sent","detail":"Too many stop requests"}));continue;}
                    let thread=text(&control.value,"threadId");let turn=text(&control.value,"turnId");
                    match registry.interrupt(&thread,&turn) {
                        Err(error)=>{let _=control.reply.send(json!({"status":"not_sent","detail":error}));}
                        Ok(wire)=>{
                            if let Err(e)=write_with_deadline(sink.send(Message::Text(wire.to_string()))).await {let _=control.reply.send(json!({"status":"unknown","detail":"Stop delivery is unknown. Check Codex."}));return Err(e);}
                            stop_ids.insert(text(&wire,"id"));
                            stop_replies.insert(text(&wire,"id"),(control.reply,thread,turn));
                        }
                    }
                    continue;
                }
                let reply = match control.value["op"].as_str() {
                    Some("snapshot") => serde_json::to_value(registry.snapshot()).unwrap(),
                    Some("answer" | "decide") => match if control.value["op"]=="answer" {
                        registry.answer(&text(&control.value,"token"), &text(&control.value,"threadId"), &control.value["answers"])
                    } else {
                        registry.decide(&text(&control.value,"token"), &text(&control.value,"threadId"), &text(&control.value,"action"), &control.value["input"])
                    } {
                        Err(error) => json!({"status":"not_sent","detail":error}),
                        Ok(response) => {
                            if let Err(e) = write_with_deadline(sink.send(Message::Text(response.to_string()))).await {
                                let _ = control.reply.send(json!({"status":"unknown","detail":"Connection failed. Check Codex before answering again."}));
                                return Err(e.to_string());
                            }
                            json!({"status":"submitted","detail":"Response submitted. Waiting for Codex to clear the request."})
                        }
                    },
                    _ => json!({"status":"not_sent","detail":"Unsupported interaction operation"})
                };
                let _ = control.reply.send(reply);
            }
        }
    }
}

async fn exchange(path: &Path, request: Value) -> Result<Value, String> {
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut socket = UnixStream::connect(path).await.map_err(|e| e.to_string())?;
        socket
            .write_all(format!("{request}\n").as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        let mut raw = Vec::new();
        BufReader::new(socket)
            .take(MAX_CONTROL as u64 + 1)
            .read_until(b'\n', &mut raw)
            .await
            .map_err(|e| e.to_string())?;
        if raw.len() > MAX_CONTROL || !raw.ends_with(b"\n") {
            return Err("Invalid interaction response".into());
        }
        serde_json::from_slice(&raw).map_err(|e| e.to_string())
    })
    .await
    .map_err(|_| "Interaction connection timed out".to_string())?
}
fn controls() -> Vec<(String, PathBuf)> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    crate::codex_bridge::endpoints()
        .into_iter()
        .take(8)
        .filter_map(|path| {
            let dir = path.parent()?;
            let socket = dir.join("interactions.sock");
            let m = std::fs::symlink_metadata(&socket).ok()?;
            if !m.file_type().is_socket()
                || m.uid() != unsafe { libc::geteuid() }
                || m.mode() & 0o077 != 0
            {
                return None;
            }
            Some((dir.file_name()?.to_str()?.to_owned(), socket))
        })
        .collect()
}
#[tauri::command]
pub async fn codex_interaction_snapshots() -> Vec<Snapshot> {
    futures_util::future::join_all(controls().into_iter().map(|(endpoint, path)| async move {
        let mut snapshot = exchange(&path, json!({"op":"snapshot"}))
            .await
            .ok()
            .and_then(|v| serde_json::from_value::<Snapshot>(v).ok())
            .unwrap_or_default();
        snapshot.endpoint = endpoint;
        snapshot
    }))
    .await
}
#[tauri::command]
pub async fn answer_codex_question(
    endpoint: String,
    token: String,
    thread_id: String,
    answers: Value,
) -> Result<Value, String> {
    if answers.to_string().len() > 32 * 1024 {
        return Ok(json!({"status":"not_sent","detail":"Answers exceed 32 KiB"}));
    }
    let Some((_, path)) = controls().into_iter().find(|(id, _)| id == &endpoint) else {
        return Ok(
            json!({"status":"not_sent","detail":"Connection is no longer available. Reopen the question in Codex."}),
        );
    };
    // An exchange error is ambiguous: the owner may already have received the answer.
    exchange(
        &path,
        json!({"op":"answer","token":token,"threadId":thread_id,"answers":answers}),
    )
    .await
}

#[tauri::command]
pub async fn decide_codex_interaction(
    endpoint: String,
    token: String,
    thread_id: String,
    action: String,
    input: Value,
) -> Result<Value, String> {
    if input.to_string().len() > 32 * 1024 {
        return Ok(json!({"status":"not_sent","detail":"Response exceeds 32 KiB"}));
    }
    let Some((_, path)) = controls().into_iter().find(|(id, _)| id == &endpoint) else {
        return Ok(json!({"status":"not_sent","detail":"Connection unavailable. Check Codex."}));
    };
    exchange(
        &path,
        json!({"op":"decide","token":token,"threadId":thread_id,"action":action,"input":input}),
    )
    .await
}
#[tauri::command]
pub async fn interrupt_codex_turn(
    endpoint: String,
    thread_id: String,
    turn_id: String,
) -> Result<Value, String> {
    let Some((_, path)) = controls().into_iter().find(|(id, _)| id == &endpoint) else {
        return Ok(json!({"status":"not_sent","detail":"Connection unavailable. Check Codex."}));
    };
    exchange(
        &path,
        json!({"op":"interrupt","threadId":thread_id,"turnId":turn_id}),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    fn question(id: Value, thread: &str) -> Value {
        json!({"id":id,"method":"item/tool/requestUserInput","params":{"threadId":thread,"turnId":"turn-1","itemId":"item-1","isBlocking":true,"questions":[
            {"id":"choice","header":"Choice","question":"Which mode?","options":[{"label":"Local","description":"Local only"},{"label":"Remote","description":"Remote host"}],"isOther":false},
            {"id":"details","header":"Details","question":"Any details?","options":null}
        ]}})
    }
    fn answers() -> Value {
        json!({"choice":{"answers":["Local"]},"details":{"answers":["中文\nline two"]}})
    }
    #[test]
    fn answers_use_owner_request_id_and_resolve_only_the_matching_request() {
        let mut r = Registry::default();
        r.observe(&question(json!(12), "A"));
        r.observe(&question(json!(13), "B"));
        let token = r.entries[0].view.token.clone();
        assert!(r.answer(&token, "B", &answers()).is_err());
        let response = r.answer(&token, "A", &answers()).unwrap();
        assert_eq!(response, json!({"id":12,"result":{"answers":answers()}}));
        assert!(!r.owner_response(&response));
        assert!(r.answer(&token, "A", &answers()).is_err());
        r.observe(
            &json!({"method":"serverRequest/resolved","params":{"threadId":"A","requestId":12}}),
        );
        assert_eq!(r.snapshot().pending.len(), 1);
        assert!(!r.owner_response(&response)); // late Desktop answer after resolved
        assert!(r.answer(&token, "A", &answers()).is_err());
    }
    #[test]
    fn owner_wins_race_and_validation_never_claims_an_invalid_answer() {
        let mut r = Registry::default();
        r.observe(&question(json!("req"), "A"));
        let token = r.entries[0].view.token.clone();
        assert!(r.answer(&token, "A", &json!({})).is_err());
        let mut invalid = answers();
        invalid["choice"]["answers"] = json!(["invented"]);
        assert!(r.answer(&token, "A", &invalid).is_err());
        assert!(!r.entries[0].view.submitted);
        assert!(r.owner_response(&json!({"id":"req","result":{"answers":answers()}})));
        assert!(r.answer(&token, "A", &answers()).is_err());
        assert!(r.owner_response(&json!({"id":"req","method":"thread/read","params":{}})));
    }
    #[test]
    fn cancellation_clears_questions_but_does_not_clear_other_turns() {
        let mut r = Registry::default();
        r.observe(&question(json!(1), "A"));
        r.observe(
            &json!({"method":"turn/completed","params":{"threadId":"A","turn":{"id":"other"}}}),
        );
        assert_eq!(r.entries.len(), 1);
        r.observe(&json!({"method":"turn/completed","params":{"threadId":"A","turn":{"id":"turn-1","status":"interrupted"}}}));
        assert!(r.entries.is_empty());
    }
    #[test]
    fn approvals_secrets_and_oversized_requests_remain_owner_only() {
        let mut r = Registry::default();
        let mut q = question(json!(1), "A");
        q["params"]["questions"][0]["isSecret"] = json!(true);
        r.observe(&q);
        assert!(!r.entries[0].view.answerable);
        let token = r.entries[0].view.token.clone();
        assert!(r.answer(&token, "A", &answers()).is_err());
        r.observe(&json!({"id":2,"method":"item/commandExecution/requestApproval","params":{"threadId":"A","turnId":"turn-1","command":"echo hello"}}));
        assert_eq!(r.entries[1].view.kind, "command");
        assert!(!r.entries[1].view.answerable);
        for n in 3..100 {
            r.observe(&question(json!(n), "A"));
        }
        assert_eq!(r.entries.len(), MAX_PENDING);
        assert!(r.snapshot().overflow);
        assert!(r.owner_response(&json!({"id":99,"result":{"answers":answers()}})));
    }
    #[test]
    fn state_flags_and_other_answers_are_preserved() {
        let mut r = Registry::default();
        r.observe(&json!({"method":"thread/status/changed","params":{"threadId":"A","status":{"type":"active","activeFlags":["waitingOnUserInput"]}}}));
        assert_eq!(r.statuses["A"], "waiting");
        let mut q = question(json!(1), "A");
        q["params"]["questions"][0]["isOther"] = json!(true);
        r.observe(&q);
        let token = r.entries[0].view.token.clone();
        let mut a = answers();
        a["choice"]["answers"] = json!(["Custom mode"]);
        assert!(r.answer(&token, "A", &a).is_ok());
        r.observe(&json!({"method":"thread/closed","params":{"threadId":"A"}}));
        assert!(r.entries.is_empty());
        assert!(r.statuses.is_empty());
    }
}

#[cfg(test)]
mod action_registry_tests {
    use super::*;
    fn proposal(diff: &str) -> Value {
        json!({"method":"item/started","params":{"threadId":"A","turnId":"t1","item":{"type":"fileChange","id":"f1","changes":[{"path":"/tmp/a","kind":{"type":"update"},"diff":diff}]}}})
    }
    #[test]
    fn changed_file_proposal_invalidates_the_old_review_and_decision_is_once_only() {
        let mut r = Registry::default();
        r.observe(&proposal("-old\n+new"));
        r.observe(&json!({"id":1,"method":"item/fileChange/requestApproval","params":{"threadId":"A","turnId":"t1","itemId":"f1"}}));
        let old = r.entries[0].view.token.clone();
        assert!(r.entries[0].view.actions.contains(&"accept".into()));
        r.observe(&proposal("-old\n+different"));
        let current = r.entries[0].view.token.clone();
        assert_ne!(old, current);
        assert!(r
            .decide(&old, "A", "accept", &json!({"reviewed":true}))
            .is_err());
        let wire = r
            .decide(&current, "A", "accept", &json!({"reviewed":true}))
            .unwrap();
        assert_eq!(wire, json!({"id":1,"result":{"decision":"accept"}}));
        assert!(!r.owner_response(&wire));
        assert!(r.decide(&current, "A", "cancel", &json!({})).is_err());
    }
    #[test]
    fn stopping_targets_only_the_current_turn_and_completion_is_event_driven() {
        let mut r = Registry::default();
        r.observe(&json!({"method":"turn/started","params":{"threadId":"A","turn":{"id":"t1"}}}));
        r.observe(&json!({"method":"turn/plan/updated","params":{"threadId":"A","turnId":"t1","explanation":"Test plan","plan":[{"step":"Build","status":"inProgress"}]}}));
        r.observe(&json!({"method":"turn/diff/updated","params":{"threadId":"A","turnId":"t1","diff":"+test"}}));
        assert_eq!(r.turns["A"].plan[0]["step"], "Build");
        assert_eq!(r.turns["A"].diff, "+test");
        assert!(r.interrupt("A", "old-turn").is_err());
        let wire = r.interrupt("A", "t1").unwrap();
        assert_eq!(wire["method"], "turn/interrupt");
        assert_eq!(wire["params"], json!({"threadId":"A","turnId":"t1"}));
        assert!(r.interrupt("A", "t1").is_err());
        assert_eq!(r.turns["A"].status, "inProgress");
        r.observe(&json!({"method":"turn/completed","params":{"threadId":"A","turn":{"id":"t1","status":"interrupted"}}}));
        assert_eq!(r.turns["A"].status, "interrupted");
        assert!(!r.turns["A"].stopping);
        assert!(r.interrupt("A", "t1").is_err());
        r.observe(&json!({"method":"turn/started","params":{"threadId":"A","turn":{"id":"t2"}}}));
        r.observe(&json!({"method":"turn/diff/updated","params":{"threadId":"A","turnId":"t1","diff":"stale"}}));
        assert!(r.turns["A"].diff.is_empty());
    }
}
