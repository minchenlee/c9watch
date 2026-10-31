//! Message queries and human mailbox controls. Reads never mean model `read`.
use super::engine_client::error_line;
use super::engine_wait::{connect, target};
use crate::engine_proto::wire::{MsgListResult, MsgReadResult, MsgSummary};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(clap::Args, Debug)]
pub struct MailArgs {
    #[command(subcommand)]
    pub action: MailAction,
}
#[derive(clap::Subcommand, Debug)]
pub enum MailAction {
    /// List mail (newest first), optionally scoped to an agent or thread
    List {
        agent: Option<String>,
        #[arg(long)]
        thread: Option<String>,
        #[arg(long)]
        before: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Read the full body and state history, without changing any state
    Read { msg_id: String },
    /// Cancel a queued message (human only)
    Cancel { msg_id: String },
    /// Pause agent and system mail; human mail continues (human only)
    Pause { agent: String },
    /// Resume agent and system mail (human only)
    Resume { agent: String },
}
pub fn request(a: &MailAction) -> (&'static str, Value) {
    match a {
        MailAction::List {
            agent,
            thread,
            before,
            limit,
        } => (
            "msg.list",
            json!({"agent":agent,"thread":thread,"before":before,"limit":limit}),
        ),
        MailAction::Read { msg_id } => ("msg.read", json!({"msgId":msg_id})),
        MailAction::Cancel { msg_id } => ("msg.cancel", json!({"msgId":msg_id})),
        MailAction::Pause { agent } => ("mail.pause", json!({"agent":agent})),
        MailAction::Resume { agent } => ("mail.resume", json!({"agent":agent})),
    }
}
pub fn cmd_mail(args: &MailArgs, pretty: bool) -> Result<(), String> {
    let env = |k: &str| std::env::var_os(k);
    let t = target(&env)?;
    let mut conn = connect(&t, true)?;
    let (method, params) = request(&args.action);
    let resp = conn.call(method, params)?;
    if !resp.ok {
        return Err(error_line(&resp));
    }
    let result = resp.result.unwrap_or(Value::Null);
    if pretty && method == "msg.list" {
        println!(
            "{}",
            pretty_list(&serde_json::from_value(result).map_err(|e| e.to_string())?)
        );
    } else if pretty && method == "msg.read" {
        println!(
            "{}",
            pretty_read(&serde_json::from_value(result).map_err(|e| e.to_string())?)
        );
    } else {
        super::print_json(&result, pretty)?;
    }
    Ok(())
}
fn short(id: &str) -> &str {
    let start = id.len().saturating_sub(8);
    if id.is_char_boundary(start) {
        &id[start..]
    } else {
        id
    }
}
fn row(m: &MsgSummary) -> String {
    let from = match m.from.kind.as_str() {
        "human" => "[you]".into(),
        "engine" => "[system] c9watch engine".into(),
        _ => m
            .from
            .name
            .clone()
            .or(m.from.addr.clone())
            .unwrap_or_else(|| "agent".into()),
    };
    let to = m.to.name.as_deref().unwrap_or(&m.to.id);
    let via = match m.via.as_deref() {
        Some("socket") => "peer",
        Some("pty") => "paste",
        _ => "pending",
    };
    let reply = m
        .reply_to
        .as_deref()
        .map(|r| format!(" re: {}", short(r)))
        .unwrap_or_default();
    let at = chrono::DateTime::from_timestamp_millis(m.created_at)
        .map(|d| d.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| m.created_at.to_string());
    let detail = m
        .detail
        .as_deref()
        .map(|d| format!(" ({})", d))
        .unwrap_or_default();
    format!(
        "{}  {} → {}  {}{} [{}]{}  {}\n  {}",
        at,
        from,
        to,
        m.state,
        detail,
        via,
        reply,
        m.msg_id,
        m.preview.replace('\n', "\n  ")
    )
}
pub fn pretty_list(result: &MsgListResult) -> String {
    let mut threads = BTreeMap::<String, Vec<&MsgSummary>>::new();
    for item in &result.items {
        threads
            .entry(
                item.thread_id
                    .clone()
                    .unwrap_or_else(|| item.msg_id.clone()),
            )
            .or_default()
            .push(item);
    }
    let mut out = vec![];
    for (thread, mut items) in threads {
        out.push(format!("Thread {}", thread));
        items.sort_by_key(|m| m.created_at);
        for item in items {
            out.push(row(item));
        }
    }
    if let Some(next) = &result.next {
        out.push(format!("More: --before {}", next));
    }
    if out.is_empty() {
        "No mail.".into()
    } else {
        out.join("\n")
    }
}
pub fn pretty_read(result: &MsgReadResult) -> String {
    let mut out = format!(
        "{}\n\n{}\n{}\n\nHistory ({} attempts):",
        row(&result.summary),
        result.provenance,
        result.body,
        result.attempts
    );
    for status in &result.status {
        out.push_str(&format!(
            "\n  {} {}{}",
            status.at,
            status.state,
            status
                .detail
                .as_deref()
                .map(|d| format!(": {}", d))
                .unwrap_or_default()
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_proto::wire::{MsgAddress, MsgFrom, MsgStatus};
    fn summary() -> MsgSummary {
        MsgSummary {
            msg_id: "msg_example".into(),
            from: MsgFrom {
                kind: "agent".into(),
                addr: Some("m/agt_a".into()),
                name: Some("A".into()),
            },
            to: MsgAddress {
                id: "agt_b".into(),
                addr: "m/agt_b".into(),
                name: Some("B".into()),
            },
            kind: "info".into(),
            state: "delivered".into(),
            state_at: 1000,
            detail: Some("peer_jsonl".into()),
            via: Some("socket".into()),
            reply_to: Some("msg_original".into()),
            thread_id: Some("msg_original".into()),
            notice_id: None,
            hop: 1,
            created_at: 0,
            preview: "hello".into(),
        }
    }
    #[test]
    fn pretty_mail_goldens() {
        let s = summary();
        let listed = pretty_list(&MsgListResult {
            items: vec![s.clone()],
            next: None,
        });
        assert_eq!(
            listed,
            include_str!("../../tests/fixtures/cli-golden/mail-list.txt").trim_end()
        );
        let read = pretty_read(&MsgReadResult {
            summary: s,
            body: "hello\nworld".into(),
            provenance: "[from m/agt_a (free) for user]".into(),
            attempts: 1,
            status: vec![
                MsgStatus {
                    state: "queued".into(),
                    at: 0,
                    detail: None,
                },
                MsgStatus {
                    state: "delivered".into(),
                    at: 1000,
                    detail: Some("peer_jsonl".into()),
                },
            ],
        });
        assert_eq!(
            read,
            include_str!("../../tests/fixtures/cli-golden/mail-read.txt").trim_end()
        );
    }
}
