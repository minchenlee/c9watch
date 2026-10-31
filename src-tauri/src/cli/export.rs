//! L0 exports use c9p only. No engine database or provider files are read here.

use super::api_schema;
use super::engine_client::{current_exe, ensure_engine, error_line, EngineConn};
use crate::engine::db::outbox::Cursor;
use crate::engine::home::{engine_home, sock_path};
use crate::engine::proto::ErrorCode;
use clap::{Args, ValueEnum};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq)]
pub enum What {
    Sessions,
    Cost,
    History,
    Events,
    Annotations,
}
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq)]
pub enum Format {
    Ndjson,
    Csv,
}

#[derive(Args)]
pub struct ExportArgs {
    #[arg(value_enum)]
    pub what: What,
    #[arg(long, value_enum, default_value = "ndjson")]
    pub format: Format,
    /// Event cursor, RFC 3339 timestamp, or YYYY-MM-DD (UTC)
    #[arg(long)]
    pub since: Option<String>,
    /// Exclusive upper time bound, RFC 3339 or YYYY-MM-DD (UTC)
    #[arg(long)]
    pub until: Option<String>,
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Include prompt, title and message fields in history and events
    #[arg(long)]
    pub include_conversation: bool,
}

fn time(s: &str) -> Result<i64, String> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.timestamp_millis())
        .or_else(|_| {
            chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis())
        })
        .map_err(|_| format!("invalid time {s:?}; use RFC 3339 or YYYY-MM-DD"))
}

fn supported(conn: &EngineConn, method: &str) -> Result<(), String> {
    if conn.hello.breaking_since.iter().any(|m| m == method) {
        return Err(format!(
            "breaking preview change to {method}; update this CLI"
        ));
    }
    if !conn.hello.methods.contains_key(method) {
        return Err(format!(
            "unsupported: this core does not advertise {method}; update core"
        ));
    }
    Ok(())
}

fn call(conn: &mut EngineConn, method: &str, params: Value) -> Result<Value, String> {
    supported(conn, method)?;
    let r = conn.call(method, params)?;
    if r.ok {
        Ok(r.result.unwrap_or(Value::Null))
    } else {
        Err(error_line(&r))
    }
}

/// Schema-driven projection. Unknown object fields are always dropped. A
/// classified free-form value is retained only at its field's allowed class.
fn project(value: &Value, shape: &Value, schema: &Value, conversation: bool) -> Value {
    if let Some(reference) = shape["$ref"].as_str() {
        return project(
            value,
            &schema["$defs"][reference.trim_start_matches("#/$defs/")],
            schema,
            conversation,
        );
    }
    if value.is_null() {
        return Value::Null;
    }
    if let Some(alts) = shape["anyOf"]
        .as_array()
        .or_else(|| shape["oneOf"].as_array())
    {
        if let Some(s) = alts.iter().find(|s| s["type"] != "null") {
            return project(value, s, schema, conversation);
        }
    }
    match value {
        Value::Object(o) if shape["properties"].is_object() => {
            let mut out = serde_json::Map::new();
            for (k, v) in o {
                let s = &shape["properties"][k];
                let class = s["x-sensitivity"].as_str();
                if matches!(class, Some("public" | "metadata"))
                    || (conversation && class == Some("conversation"))
                {
                    out.insert(k.clone(), project(v, s, schema, conversation));
                }
            }
            out.into()
        }
        Value::Array(a) if shape.get("items").is_some() => a
            .iter()
            .map(|v| project(v, &shape["items"], schema, conversation))
            .collect(),
        _ => value.clone(),
    }
}

fn rows(v: Value, field: &str) -> Result<Vec<Value>, String> {
    v[field]
        .as_array()
        .cloned()
        .ok_or_else(|| format!("bad c9p result: expected {field} array"))
}

fn row_time(row: &Value, what: What) -> Option<i64> {
    match what {
        What::History => row["timestamp"].as_i64(),
        What::Sessions => row["modified"].as_str().and_then(|s| time(s).ok()),
        What::Events => row["ts"].as_i64(),
        What::Cost => row["timestamp"].as_str().and_then(|s| time(s).ok()),
        What::Annotations => row["updatedAt"]
            .as_i64()
            .or_else(|| row["createdAt"].as_i64()),
    }
}

/// Find the oldest readable position through subscriptions. Retention may
/// move while we probe; a later cursor_expired fails the export, never silently
/// skips records. No subscription is left open by a probe.
fn oldest(sock: &std::path::Path, head: &Cursor) -> Result<Cursor, String> {
    let mut low = 0;
    let mut high = head.seq;
    while low < high {
        let mid = low + (high - low) / 2;
        let cursor = Cursor {
            seq: mid,
            ..head.clone()
        };
        let mut conn = EngineConn::connect(sock).map_err(|e| e.into_msg())?;
        supported(&conn, "events.subscribe")?;
        let r = conn.call("events.subscribe", json!({"since": cursor.to_string()}))?;
        if r.ok {
            high = mid;
        } else if r
            .error
            .as_ref()
            .is_some_and(|e| e.code == ErrorCode::CursorExpired)
        {
            low = mid + 1;
        } else {
            return Err(error_line(&r));
        }
    }
    Ok(Cursor {
        seq: low,
        ..head.clone()
    })
}

fn events(
    mut conn: EngineConn,
    sock: &std::path::Path,
    since: Option<&str>,
    schema: &Value,
    conversation: bool,
) -> Result<Vec<Value>, String> {
    supported(&conn, "events.subscribe")?;
    let head = Cursor::parse(
        conn.hello
            .cursor
            .as_deref()
            .ok_or("core returned no cursor")?,
    )?;
    let start = match since {
        Some(s) if Cursor::parse(s).is_ok() => Cursor::parse(s)?,
        _ => oldest(sock, &head)?,
    };
    // Validate even an empty export, including another machine/epoch.
    call(
        &mut conn,
        "events.subscribe",
        json!({"since": start.to_string()}),
    )?;
    if start == head {
        return Ok(Vec::new());
    }
    let frames = conn.into_frames()?;
    let mut out = Vec::new();
    loop {
        let line = frames
            .lines
            .recv_timeout(Duration::from_secs(10))
            .map_err(|e| format!("event export did not reach its captured head: {e}"))?;
        let mut event: Value =
            serde_json::from_str(&line).map_err(|e| format!("invalid event: {e}"))?;
        if event["ok"] == false {
            return Err(format!("event stream failed: {}", event["error"]));
        }
        if event["type"] != "event" {
            return Err("unexpected event stream frame".into());
        }
        let cursor = Cursor::parse(event["cursor"].as_str().ok_or("event has no cursor")?)?;
        if cursor.machine != head.machine || cursor.epoch != head.epoch {
            return Err("event epoch changed".into());
        }
        let topic = event["topic"].as_str().ok_or("event has no topic")?;
        if cursor.seq <= head.seq && schema["topics"].get(topic).is_some() {
            let data = project(
                &event["data"],
                &schema["topics"][topic]["data"],
                schema,
                conversation,
            );
            // Project the envelope separately; its generic data field has no type.
            event["data"] = Value::Null;
            let mut event = project(&event, &schema["$defs"]["EventFrame"], schema, false);
            event["data"] = data;
            // origin is an untyped provenance object; do not export arbitrary payloads.
            event.as_object_mut().unwrap().remove("origin");
            out.push(event);
        }
        if cursor.seq >= head.seq {
            return Ok(out);
        }
    }
}

pub fn run(args: ExportArgs) -> Result<(), String> {
    let since_cursor = args.since.as_deref().filter(|s| Cursor::parse(s).is_ok());
    if since_cursor.is_some() && args.what != What::Events {
        return Err(
            "--since cursor is supported only for events; use a time for this export".into(),
        );
    }
    let lower = args
        .since
        .as_deref()
        .filter(|_| since_cursor.is_none())
        .map(time)
        .transpose()?;
    let upper = args.until.as_deref().map(time).transpose()?;
    if lower.zip(upper).is_some_and(|(a, b)| a >= b) {
        return Err("--since must be before --until".into());
    }
    // Annotation timestamps are not part of today's public contract.
    if args.what == What::Annotations && (lower.is_some() || upper.is_some()) {
        return Err("annotation time filtering is unsupported by this API".into());
    }
    let home = engine_home()?;
    ensure_engine(&home, &current_exe()?)?;
    let sock = sock_path(&home);
    let mut conn = EngineConn::connect(&sock).map_err(|e| e.into_msg())?;
    let schema = api_schema::public(api_schema::embedded());
    let mut data = match args.what {
        What::Sessions => rows(call(&mut conn, "session.views", json!({}))?, "sessions")?,
        What::Cost => rows(call(&mut conn, "cost.get", json!({}))?, "dailyCosts")?
            .into_iter()
            .flat_map(|d| d["sessions"].as_array().cloned().unwrap_or_default())
            .collect(),
        What::Annotations => rows(
            call(&mut conn, "annotation.list", json!({}))?,
            "annotations",
        )?,
        What::History => {
            let mut out = Vec::new();
            let mut offset = 0;
            loop {
                let page = call(
                    &mut conn,
                    "history.list",
                    json!({"page": {"offset": offset}}),
                )?;
                out.extend(rows(page.clone(), "entries")?);
                match page["nextOffset"].as_u64() {
                    None if page["nextOffset"].is_null() => break,
                    Some(next) if next > offset => offset = next,
                    _ => return Err("history paging did not advance".into()),
                }
            }
            out.into_iter()
                .map(|v| {
                    project(
                        &v,
                        &schema["$defs"]["HistoryEntry"],
                        &schema,
                        args.include_conversation,
                    )
                })
                .collect()
        }
        What::Events => events(
            conn,
            &sock,
            args.since.as_deref(),
            &schema,
            args.include_conversation,
        )?,
    };
    if lower.is_some() || upper.is_some() {
        // Missing/invalid timestamps cannot establish membership in a time range.
        data.retain(|r| {
            row_time(r, args.what)
                .is_some_and(|t| lower.is_none_or(|x| t >= x) && upper.is_none_or(|x| t < x))
        });
    }
    let bytes = render(&data, args.format)?;
    if let Some(path) = args.out {
        use std::os::unix::fs::OpenOptionsExt;
        // Do not overwrite an existing export or follow a symlink.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
    } else {
        std::io::stdout()
            .lock()
            .write_all(&bytes)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn render(rows: &[Value], format: Format) -> Result<Vec<u8>, String> {
    let mut out = String::new();
    if format == Format::Ndjson {
        for row in rows {
            out.push_str(&serde_json::to_string(row).map_err(|e| e.to_string())?);
            out.push('\n');
        }
    } else {
        let columns: BTreeSet<&str> = rows
            .iter()
            .filter_map(Value::as_object)
            .flat_map(|o| o.keys().map(String::as_str))
            .collect();
        fn cell(s: &str) -> String {
            format!("\"{}\"", s.replace('"', "\"\""))
        }
        out.push_str(
            &columns
                .iter()
                .map(|k| cell(k))
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push('\n');
        for row in rows {
            out.push_str(
                &columns
                    .iter()
                    .map(|k| match &row[*k] {
                        Value::Null => cell(""),
                        Value::String(s) => cell(s),
                        v => cell(&v.to_string()),
                    })
                    .collect::<Vec<_>>()
                    .join(","),
            );
            out.push('\n');
        }
    }
    Ok(out.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn redaction_is_recursive_and_drops_unknown_and_private_fields() {
        let s = api_schema::public(api_schema::embedded());
        let h = json!({"sessionId":"x", "display":"secret", "futureField":"secret", "customTitle":"secret", "timestamp":1});
        let r = project(&h, &s["$defs"]["HistoryEntry"], &s, false);
        assert_eq!(r, json!({"sessionId":"x", "timestamp":1}));
        assert_eq!(
            project(&h, &s["$defs"]["HistoryEntry"], &s, true)["display"],
            "secret"
        );
        let row = json!({"id":"a", "resumeCount":8});
        assert_eq!(
            project(&row, &s["$defs"]["AgentRow"], &s, true),
            json!({"id":"a"})
        );
        let session =
            json!({"pendingToolInput":{"secret":"anything"}, "firstPrompt":"secret", "id":"x"});
        assert_eq!(
            project(&session, &s["$defs"]["SessionView"], &s, false),
            json!({"id":"x"})
        );
    }
    #[test]
    fn csv_quotes_newlines_commas_and_nested_json() {
        let csv = String::from_utf8(
            render(&[json!({"a":"x,\n\"z", "nested":{"n":1}})], Format::Csv).unwrap(),
        )
        .unwrap();
        assert_eq!(csv, "\"a\",\"nested\"\n\"x,\n\"\"z\",\"{\"\"n\"\":1}\"\n");
        assert_eq!(
            time("2026-11-01").unwrap(),
            time("2026-11-01T00:00:00Z").unwrap()
        );
        assert!(time("nonsense").is_err());
    }
}
