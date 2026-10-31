//! L1 store. Namespace admission is called by the central dispatch pipeline.
//! State, resulting-map events, sequence tombstones and successful-write audit
//! commit in one transaction. No value is copied into audit.
use super::api::internal;
use super::db::audit::{self, AuditEntry};
use super::db::outbox::NewEvent;
use super::db::{now_ms, Db, Tx};
use super::principal::Principal;
use super::proto::*;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_TTL_MS: u64 = 86_400_000;
pub const TARGET_GRACE_MS: i64 = 30 * 86_400_000;
// Reserve bounded source slots, including sequence tombstones. Evicting an
// accepted watermark would let an old request resurrect a cleared/expired key.
const MAX_SOURCES_PER_TARGET: i64 = 128;
const MAX_TARGETS_PER_SOURCE: i64 = 2000;
pub fn is_write(method: &str) -> bool {
    matches!(
        method,
        "annotation.set" | "annotation.delete" | "annotation.patch"
    )
}
fn bad(msg: &str) -> ApiError {
    ApiError::new(ErrorCode::BadRequest, msg)
}
fn limit(msg: &str) -> ApiError {
    ApiError::new(ErrorCode::LimitExceeded, msg)
}
fn component(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
}
fn key(s: &str) -> Result<(&str, &str), ApiError> {
    let (ns, name) = s
        .split_once('/')
        .ok_or_else(|| bad("key must be namespace/name"))?;
    if !component(ns, 40) || !component(name, 32) {
        return Err(bad("invalid annotation key"));
    }
    Ok((ns, name))
}
fn target(t: &AnnotationTarget) -> Result<(), ApiError> {
    if t.kind != "session"
        || t.session_key.is_empty()
        || t.session_key.len() > 256
        || t.session_key.chars().any(char::is_control)
    {
        return Err(bad("only a nonempty session target is supported"));
    }
    Ok(())
}
/// Mutating admission: validate/force source before any handler runs. Foreign
/// explicit namespaces are rejected, rather than silently rewriting the key.
pub fn admit(method: &str, params: &mut Value, who: &Principal) -> Result<(), ApiError> {
    let supplied = if method == "annotation.patch" {
        match params.get("source") {
            Some(Value::String(s)) => Some(s.as_str()),
            Some(Value::Null) | None => None,
            _ => return Err(bad("source must be a namespace")),
        }
    } else {
        Some(
            key(params["key"]
                .as_str()
                .ok_or_else(|| bad("key is required"))?)?
            .0,
        )
    };
    let forced = match who {
        Principal::Agent { id, .. } => Some(format!("agent.{id}")),
        Principal::Client { id } => Some(format!("client.{id}")),
        Principal::LocalUnverified => None,
    };
    let ns = if let Some(ref forced) = forced {
        if supplied.is_some_and(|s| s != forced) {
            return Err(ApiError::new(
                ErrorCode::NotAllowed,
                "annotation namespace belongs to another principal",
            ));
        }
        forced.as_str()
    } else {
        let ns = supplied.ok_or_else(|| bad("local patches require source"))?;
        if ns.starts_with("agent.") || ns.starts_with("client.") || ns.starts_with("c9watch") {
            return Err(ApiError::new(
                ErrorCode::NotAllowed,
                "reserved annotation namespace",
            ));
        }
        ns
    };
    if !component(ns, 40) {
        return Err(bad("invalid annotation namespace"));
    }
    if method == "annotation.patch" {
        params["source"] = ns.to_string().into();
    }
    Ok(())
}
fn text(s: &str, max_chars: usize, max_bytes: usize) -> String {
    let trimmed = s.trim();
    let mut out = String::new();
    for c in trimmed.chars().filter(|c| !c.is_control()).take(max_chars) {
        if out.len() + c.len_utf8() > max_bytes {
            break;
        }
        out.push(c);
    }
    out.trim().to_string()
}
fn normalize(v: &mut AnnotationValue) -> Result<(), ApiError> {
    match v {
        AnnotationValue::Number { v } if !v.is_finite() => {
            return Err(bad("number must be finite"))
        }
        AnnotationValue::String { v } => *v = text(v, 80, 256),
        AnnotationValue::Money { amount, currency } => {
            let n = amount.strip_prefix('-').unwrap_or(amount);
            let parts: Vec<_> = n.split('.').collect();
            if amount.len() > 128
                || parts.len() > 2
                || parts
                    .iter()
                    .any(|s| s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()))
            {
                return Err(bad("money amount must be a decimal string"));
            }
            if !CURRENCIES.split_whitespace().any(|c| c == currency) {
                return Err(bad("currency must be an ISO 4217 code"));
            }
        }
        _ => {}
    }
    Ok(())
}
fn value_info(v: &AnnotationValue) -> (&'static str, &'static str) {
    match v {
        AnnotationValue::Number { .. } => ("number", "metadata"),
        AnnotationValue::String { .. } => ("string", "conversation"),
        AnnotationValue::Bool { .. } => ("bool", "metadata"),
        AnnotationValue::Duration { .. } => ("duration", "metadata"),
        AnnotationValue::Money { .. } => ("money", "metadata"),
    }
}
struct Patch {
    target: AnnotationTarget,
    source: String,
    values: BTreeMap<String, Option<AnnotationValue>>,
    ttl: Option<u64>,
    seq: Option<i64>,
    display: Option<(Option<String>, Option<String>, Option<String>)>,
}
fn parse(method: &str, params: Value) -> Result<Patch, ApiError> {
    let mut patch = match method {
        "annotation.patch" => {
            let p: AnnotationPatchParams = serde_json::from_value(params)
                .map_err(|_| bad("invalid annotation.patch params"))?;
            Patch {
                target: p.target,
                source: p.source.ok_or_else(|| bad("source is required"))?,
                values: p.values,
                ttl: p.ttl_ms,
                seq: p
                    .seq
                    .map(|s| i64::try_from(s).map_err(|_| bad("seq exceeds signed 64-bit range")))
                    .transpose()?,
                display: None,
            }
        }
        "annotation.set" => {
            let p: AnnotationSetParams =
                serde_json::from_value(params).map_err(|_| bad("invalid annotation.set params"))?;
            let (ns, name) = key(&p.key)?;
            if p.ttl_seconds.is_some_and(|s| !(60..=86_400).contains(&s)) {
                return Err(bad("ttlSeconds must be 60 through 86400"));
            }
            if p.unit
                .as_ref()
                .is_some_and(|u| u.chars().count() > 12 || u.chars().any(char::is_control))
                || p.format.as_ref().is_some_and(|f| !component(f, 32))
            {
                return Err(bad("invalid unit or format"));
            }
            Patch {
                target: p.target,
                source: ns.into(),
                values: [(name.into(), Some(p.value))].into(),
                ttl: p.ttl_seconds.map(|s| s * 1000),
                seq: None,
                display: Some((p.label.map(|s| text(&s, 32, 128)), p.unit, p.format)),
            }
        }
        _ => {
            let p: AnnotationDeleteParams = serde_json::from_value(params)
                .map_err(|_| bad("invalid annotation.delete params"))?;
            let (ns, name) = key(&p.key)?;
            Patch {
                target: p.target,
                source: ns.into(),
                values: [(name.into(), None)].into(),
                ttl: None,
                seq: None,
                display: None,
            }
        }
    };
    target(&patch.target)?;
    if patch.values.is_empty() || patch.values.len() > 32 {
        return Err(limit("patch requires 1 through 32 keys"));
    }
    if patch.ttl.is_some_and(|t| !(1..=MAX_TTL_MS).contains(&t)) {
        return Err(bad("ttlMs must be 1 through 86400000"));
    }
    for (name, v) in &mut patch.values {
        if !component(name, 32) {
            return Err(bad("invalid annotation name"));
        }
        if let Some(v) = v {
            normalize(v)?;
        }
    }
    Ok(patch)
}
const COLUMNS: &str = "target_kind,target_id,namespace,name,value_json,label,unit,format,sensitivity,writer_principal,created_at,updated_at,expires_at";
fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Annotation> {
    let raw: String = r.get(4)?;
    let value = serde_json::from_str(&raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(Annotation {
        target: AnnotationTarget {
            kind: r.get(0)?,
            session_key: r.get(1)?,
        },
        key: format!("{}/{}", r.get::<_, String>(2)?, r.get::<_, String>(3)?),
        value: Some(value),
        label: r.get(5)?,
        unit: r.get(6)?,
        format: r.get(7)?,
        sensitivity: r.get(8)?,
        writer: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
        expires_at: r.get(12)?,
        redacted: false,
    })
}
fn source_map(
    c: &Connection,
    t: &AnnotationTarget,
    source: &str,
    now: i64,
) -> rusqlite::Result<BTreeMap<String, Annotation>> {
    let mut s=c.prepare(&format!("SELECT {COLUMNS} FROM annotations WHERE target_kind=?1 AND target_id=?2 AND namespace=?3 AND (expires_at IS NULL OR expires_at>?4) ORDER BY name"))?;
    let rows = s.query_map(params![t.kind, t.session_key, source, now], row)?;
    rows.map(|r| r.map(|a| (a.key.split_once('/').unwrap().1.to_string(), a)))
        .collect()
}
/// A list and its cursor are read on the writer, between transactions, so
/// clients can subscribe from the snapshot cursor without missing a patch.
pub async fn list(db: &Db, params: Value) -> Result<Value, ApiError> {
    let p: AnnotationListParams =
        serde_json::from_value(params).map_err(|_| bad("invalid annotation.list params"))?;
    if let Some(t) = &p.target {
        target(t)?;
    }
    if p.namespace.as_ref().is_some_and(|s| !component(s, 40)) {
        return Err(bad("invalid namespace"));
    }
    let ((annotations,seq),_)=db.write(move |tx| {
        let mut s=tx.prepare(&format!("SELECT {COLUMNS} FROM annotations WHERE (?1 IS NULL OR target_id=?1) AND (?2 IS NULL OR namespace=?2) AND (expires_at IS NULL OR expires_at>?3) ORDER BY target_kind,target_id,namespace,name"))?;
        let rows=s.query_map(params![p.target.map(|t|t.session_key),p.namespace,now_ms()],row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        let seq=tx.query_row("SELECT coalesce(max(seq),0) FROM events",[],|r|r.get::<_,i64>(0))?;
        Ok((rows,seq))
    }).await.map_err(internal)?;
    Ok(serde_json::to_value(AnnotationListResult {
        annotations,
        cursor: db.cursor_at(seq).to_string(),
    })
    .unwrap())
}
fn emit_changed(tx: &mut Tx, t: &AnnotationTarget, source: &str, now: i64) -> rusqlite::Result<()> {
    let event = AnnotationChangedEvent {
        target: t.clone(),
        source: source.into(),
        values: source_map(tx, t, source, now)?,
    };
    tx.emit(&NewEvent {
        topic: "annotation.changed".into(),
        subject: Some(t.session_key.clone()),
        origin: None,
        data: serde_json::to_value(event).unwrap(),
    })?;
    Ok(())
}
fn take_rate(tx: &Tx, principal: &str, now: i64) -> rusqlite::Result<bool> {
    let old = tx
        .query_row(
            "SELECT tokens,updated_at FROM annotation_rates WHERE principal=?1",
            [principal],
            |r| Ok((r.get::<_, f64>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?;
    let (tokens, at) = old.unwrap_or((20.0, now));
    let tokens = (tokens + (now - at).max(0) as f64 / 100.0).min(20.0);
    let allowed = tokens >= 1.0;
    tx.execute("INSERT INTO annotation_rates(principal,tokens,updated_at) VALUES (?1,?2,?3) ON CONFLICT(principal) DO UPDATE SET tokens=excluded.tokens,updated_at=excluded.updated_at",params![principal,if allowed {tokens-1.0}else{tokens},now.max(at)])?;
    Ok(allowed)
}
/// Called only after central authz and namespace/peer admission.
pub async fn write(
    db: &Db,
    method: &str,
    params: Value,
    who: &Principal,
) -> Result<Value, ApiError> {
    let patch = parse(method, params.clone())?;
    let principal = who.to_string();
    let method = method.to_string();
    let (result, _) = db
        .write(move |tx| {
            let now = now_ms();
            let result = apply(tx, &patch, &principal, &method, now)?;
            if result.is_ok() {
                audit_request(tx, &principal, &method, &params, "allow", None, now)?;
            }
            Ok(result)
        })
        .await
        .map_err(internal)?;
    result
}
fn apply(
    tx: &mut Tx,
    p: &Patch,
    principal: &str,
    method: &str,
    now: i64,
) -> rusqlite::Result<Result<Value, ApiError>> {
    if !take_rate(tx, principal, now)? {
        return Ok(Err(limit(
            "annotation write rate exceeded (10/s, burst 20)",
        )));
    }
    let exists = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM agents WHERE session_key=?1 AND gone_at IS NULL)",
        [&p.target.session_key],
        |r| r.get::<_, bool>(0),
    )?;
    if !exists {
        return Ok(Err(ApiError::new(
            ErrorCode::UnknownSession,
            "target is not a live session",
        )));
    }
    let source_seq=tx.query_row("SELECT last_seq FROM annotation_sources WHERE target_kind=?1 AND target_id=?2 AND namespace=?3",params![p.target.kind,p.target.session_key,p.source],|r|r.get::<_,Option<i64>>(0)).optional()?;
    let previous = source_seq.flatten();
    let before = source_map(tx, &p.target, &p.source, now)?;
    if p.seq.zip(previous).is_some_and(|(n, old)| n <= old) {
        return Ok(Ok(json!(AnnotationPatchResult {
            applied: false,
            target: p.target.clone(),
            source: p.source.clone(),
            values: before
        })));
    }
    let mut names: BTreeSet<_> = before.keys().cloned().collect();
    let mut delta = 0i64;
    for (name, v) in &p.values {
        let had = names.contains(name);
        if v.is_some() {
            names.insert(name.clone());
            if !had {
                delta += 1;
            }
        } else if names.remove(name) {
            delta -= 1;
        }
    }
    let count_target:i64=tx.query_row("SELECT count(*) FROM annotations WHERE target_kind=?1 AND target_id=?2 AND (expires_at IS NULL OR expires_at>?3)",params![p.target.kind,p.target.session_key,now],|r|r.get(0))?;
    let count_source:i64=tx.query_row("SELECT count(*) FROM annotations WHERE namespace=?1 AND (expires_at IS NULL OR expires_at>?2)",params![p.source,now],|r|r.get(0))?;
    if names.len() > 32 || count_target + delta > 128 || count_source + delta > 2000 {
        return Ok(Err(limit(
            "annotation key count exceeded (32/source/target, 128/target, 2000/source)",
        )));
    }
    let keep_source = !names.is_empty() || p.seq.is_some() || previous.is_some();
    if keep_source && source_seq.is_none() {
        let target_sources: i64 = tx.query_row(
            "SELECT count(*) FROM annotation_sources WHERE target_kind=?1 AND target_id=?2",
            params![p.target.kind, p.target.session_key],
            |r| r.get(0),
        )?;
        let source_targets: i64 = tx.query_row(
            "SELECT count(*) FROM annotation_sources WHERE namespace=?1",
            [&p.source],
            |r| r.get(0),
        )?;
        if target_sources >= MAX_SOURCES_PER_TARGET || source_targets >= MAX_TARGETS_PER_SOURCE {
            return Ok(Err(limit(
                "annotation source record count exceeded (128/target, 2000/source)",
            )));
        }
    }
    if keep_source {
        tx.execute("INSERT INTO annotation_sources(target_kind,target_id,namespace,last_seq) VALUES (?1,?2,?3,?4) ON CONFLICT(target_kind,target_id,namespace) DO UPDATE SET last_seq=coalesce(excluded.last_seq,last_seq),gone_at=NULL",params![p.target.kind,p.target.session_key,p.source,p.seq])?;
    } else {
        tx.execute("DELETE FROM annotation_sources WHERE target_kind=?1 AND target_id=?2 AND namespace=?3 AND last_seq IS NULL",params![p.target.kind,p.target.session_key,p.source])?;
    }
    for (name, v) in &p.values {
        if let Some(v) = v {
            let (ty, class) = value_info(v);
            let (label, unit, format) = p.display.clone().unwrap_or_else(|| {
                before
                    .get(name)
                    .map(|a| (a.label.clone(), a.unit.clone(), a.format.clone()))
                    .unwrap_or_default()
            });
            tx.execute("INSERT INTO annotations(target_kind,target_id,namespace,name,value_type,value_json,label,unit,format,sensitivity,writer_principal,created_at,updated_at,expires_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?12,?13) ON CONFLICT(target_kind,target_id,namespace,name) DO UPDATE SET value_type=excluded.value_type,value_json=excluded.value_json,label=excluded.label,unit=excluded.unit,format=excluded.format,sensitivity=excluded.sensitivity,writer_principal=excluded.writer_principal,updated_at=excluded.updated_at,expires_at=excluded.expires_at",params![p.target.kind,p.target.session_key,p.source,name,ty,serde_json::to_string(v).unwrap(),label,unit,format,class,principal,now,p.ttl.map(|t|now+t as i64)])?;
        } else {
            tx.execute("DELETE FROM annotations WHERE target_kind=?1 AND target_id=?2 AND namespace=?3 AND name=?4",params![p.target.kind,p.target.session_key,p.source,name])?;
        }
    }
    emit_changed(tx, &p.target, &p.source, now)?;
    let values = source_map(tx, &p.target, &p.source, now)?;
    let result = match method {
        "annotation.set" => json!(AnnotationSetResult {
            annotation: values[p.values.keys().next().unwrap()].clone()
        }),
        "annotation.delete" => json!(AnnotationDeleteResult {
            deleted: before.contains_key(p.values.keys().next().unwrap())
        }),
        _ => json!(AnnotationPatchResult {
            applied: true,
            target: p.target.clone(),
            source: p.source.clone(),
            values
        }),
    };
    Ok(Ok(result))
}
/// Audit includes only validated key names and known session IDs; even malformed
/// requests cannot smuggle a value into a text column. Admitted params are hashed.
pub fn audit_request(
    tx: &Tx,
    principal: &str,
    method: &str,
    params: &Value,
    decision: &str,
    reason: Option<&str>,
    now: i64,
) -> rusqlite::Result<()> {
    let supplied = params["target"]["sessionKey"].as_str().unwrap_or_default();
    let known = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM agents WHERE session_key=?1)",
        [supplied],
        |r| r.get::<_, bool>(0),
    )?;
    let keys: Vec<String> = if let Some(k) = params["key"].as_str().filter(|s| key(s).is_ok()) {
        vec![k.into()]
    } else {
        let forced = principal
            .strip_prefix("agent:")
            .map(|id| format!("agent.{id}"))
            .or_else(|| {
                principal
                    .strip_prefix("client:")
                    .map(|id| format!("client.{id}"))
            });
        let ns = params["source"]
            .as_str()
            .or(forced.as_deref())
            .filter(|s| component(s, 40));
        params["values"]
            .as_object()
            .into_iter()
            .flat_map(|o| o.keys())
            .filter(|n| component(n, 32))
            .filter_map(|n| ns.map(|s| format!("{s}/{n}")))
            .collect()
    };
    let hash = super::mail::sha256_hex(&serde_json::to_vec(params).unwrap());
    let seq = audit::append(
        tx,
        now,
        &AuditEntry {
            principal,
            method,
            target: known.then_some(supplied),
            decision,
            reason,
            payload_hash: Some(&hash),
            ..Default::default()
        },
    )?;
    tx.execute(
        "UPDATE audit SET annotation_keys=?1 WHERE seq=?2",
        params![serde_json::to_string(&keys).unwrap(), seq],
    )?;
    Ok(())
}
/// Avoid a writer transaction on idle ticks. Check every kind of sweep work,
/// including sequence tombstones and rate rows with no remaining annotations.
/// The writer rechecks the live rows; this read never decides what to delete.
pub fn sweep_if_due(db: &Db, now: i64) -> Result<bool, String> {
    let due: bool = db
        .reader()?
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM annotations WHERE expires_at<=?1)
         OR EXISTS(SELECT 1 FROM annotation_sources WHERE gone_at<=?2)
         OR EXISTS(SELECT 1 FROM annotation_sources s WHERE last_seq IS NULL
             AND NOT EXISTS(SELECT 1 FROM annotations a WHERE a.target_kind=s.target_kind
                 AND a.target_id=s.target_id AND a.namespace=s.namespace))
         OR EXISTS(SELECT 1 FROM annotation_rates WHERE updated_at<?3)",
            params![now, now - TARGET_GRACE_MS, now - MAX_TTL_MS as i64],
            |r| r.get(0),
        )
        .map_err(|e| format!("Failed to check annotation expiry: {e}"))?;
    if due {
        db.write_blocking(move |tx| sweep(tx, now))?;
    }
    Ok(due)
}

/// TTL expires on each detection tick, including a failed observation. The
/// source map is emitted once per affected source, even if many keys expire.
pub fn sweep(tx: &mut Tx, now: i64) -> rusqlite::Result<()> {
    let expired: Vec<(String, String)> = {
        let mut s=tx.prepare("SELECT DISTINCT target_id,namespace FROM annotations WHERE expires_at<=?1 ORDER BY target_id,namespace")?;
        let rows = s
            .query_map([now], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };
    tx.execute("DELETE FROM annotations WHERE expires_at<=?1", [now])?;
    for (id, source) in expired {
        emit_changed(
            tx,
            &AnnotationTarget {
                kind: "session".into(),
                session_key: id,
            },
            &source,
            now,
        )?;
    }
    let gone: Vec<Annotation> = {
        let mut s=tx.prepare(&format!("SELECT {} FROM annotations a WHERE EXISTS(SELECT 1 FROM annotation_sources s WHERE s.target_kind=a.target_kind AND s.target_id=a.target_id AND s.namespace=a.namespace AND s.gone_at<=?1) ORDER BY target_id,namespace,name",COLUMNS.split(',').map(|s|format!("a.{s}")).collect::<Vec<_>>().join(",")))?;
        let rows = s
            .query_map([now - TARGET_GRACE_MS], row)?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };
    for a in gone {
        let (ns, name) = a.key.split_once('/').unwrap();
        tx.execute("DELETE FROM annotations WHERE target_kind=?1 AND target_id=?2 AND namespace=?3 AND name=?4",params![a.target.kind,a.target.session_key,ns,name])?;
        tx.emit(&NewEvent {
            topic: "annotation.removed".into(),
            subject: Some(a.target.session_key.clone()),
            origin: None,
            data: json!(AnnotationRemovedEvent {
                target: a.target,
                key: a.key,
                reason: "target_gone".into()
            }),
        })?;
    }
    tx.execute(
        "DELETE FROM annotation_sources WHERE gone_at<=?1",
        [now - TARGET_GRACE_MS],
    )?;
    // No sequence to preserve: release slots after expiry or legacy no-op
    // deletes, even while the target remains live.
    tx.execute("DELETE FROM annotation_sources WHERE last_seq IS NULL AND NOT EXISTS(SELECT 1 FROM annotations a WHERE a.target_kind=annotation_sources.target_kind AND a.target_id=annotation_sources.target_id AND a.namespace=annotation_sources.namespace)", [])?;
    tx.execute(
        "DELETE FROM annotation_rates WHERE updated_at<?1",
        [now - MAX_TTL_MS as i64],
    )?;
    Ok(())
}
/// Called after detection has updated its agents, including the first pass
/// after restart. Preserve a first disappearance time until the target returns.
pub fn track_targets(tx: &Tx, now: i64) -> rusqlite::Result<()> {
    tx.execute("UPDATE annotation_sources SET gone_at=CASE WHEN EXISTS(SELECT 1 FROM agents WHERE session_key=target_id AND gone_at IS NULL) THEN NULL ELSE coalesce(gone_at,?1) END",[now])?;
    Ok(())
}

// SIX ISO 4217 Maintenance Agency, list-one.xml, published 2026-09-17.
// https://www.six-group.com/dam/download/financial-information/data-center/iso-currrency/lists/list-one.xml
const CURRENCIES: &str = "AED AFN ALL AMD AOA ARS AUD AWG AZN BAM BBD BDT BHD BIF BMD BND BOB BOV BRL BSD BTN BWP BYN BZD CAD CDF CHE CHF CHW CLF CLP CNY COP COU CRC CUP CVE CZK DJF DKK DOP DZD EGP ERN ETB EUR FJD FKP GBP GEL GHS GIP GMD GNF GTQ GYD HKD HNL HTG HUF IDR ILS INR IQD IRR ISK JMD JOD JPY KES KGS KHR KMF KPW KRW KWD KYD KZT LAK LBP LKR LRD LSL LYD MAD MDL MGA MKD MMK MNT MOP MRU MUR MVR MWK MXN MXV MYR MZN NAD NGN NIO NOK NPR NZD OMR PAB PEN PGK PHP PKR PLN PYG QAR RON RSD RUB RWF SAR SBD SCR SDG SEK SGD SHP SLE SOS SRD SSP STN SVC SYP SZL THB TJS TMT TND TOP TRY TTD TWD TZS UAH UGX USD USN UYI UYU UYW UZS VED VES VND VUV WST XAD XAF XAG XAU XBA XBB XBC XBD XCD XCG XDR XOF XPD XPF XPT XSU XTS XUA XXX YER ZAR ZMW ZWG";

#[cfg(test)]
mod tests {
    use super::*;
    fn db() -> (tempfile::TempDir, Db) {
        let t = tempfile::tempdir().unwrap();
        let db = super::super::db::open(&t.path().join("engine"), "m").unwrap();
        db.write_blocking(|tx| {
            seed(tx, "claudeCode:s")?;
            Ok(())
        })
        .unwrap();
        (t, db)
    }
    fn seed(tx: &Tx, key: &str) -> rusqlite::Result<()> {
        tx.execute("INSERT INTO agents(id,role,provider,source,session_key) VALUES (?1,'observed','claudeCode','detected',?1)",[key])?;
        Ok(())
    }
    fn request(ns: &str, values: Value, seq: Option<u64>, ttl: Option<u64>) -> Value {
        json!({"target":{"kind":"session","sessionKey":"claudeCode:s"},"source":ns,"values":values,"seq":seq,"ttlMs":ttl})
    }
    fn patch(v: Value) -> Patch {
        parse("annotation.patch", v).unwrap()
    }
    fn apply_at(db: &Db, p: Patch, now: i64) -> Result<Value, ApiError> {
        db.write_blocking(move |tx| apply(tx, &p, "local-unverified", "annotation.patch", now))
            .unwrap()
            .0
    }
    #[test]
    fn namespaces_are_forced_and_reserved_ones_are_refused() {
        let agent = Principal::Agent {
            id: "a".into(),
            gen: 1,
        };
        let client = Principal::Client { id: "phone".into() };
        for (who, own, foreign) in [
            (agent, "agent.a", "agent.b"),
            (client, "client.phone", "client.other"),
        ] {
            let mut p = request(own, json!({"x":{"type":"bool","v":true}}), None, None);
            p.as_object_mut().unwrap().remove("source");
            admit("annotation.patch", &mut p, &who).unwrap();
            assert_eq!(p["source"], own);
            p["source"] = foreign.into();
            assert_eq!(
                admit("annotation.patch", &mut p, &who).unwrap_err().code,
                ErrorCode::NotAllowed
            );
            let mut set = json!({"key":format!("{foreign}/x")});
            assert_eq!(
                admit("annotation.set", &mut set, &who).unwrap_err().code,
                ErrorCode::NotAllowed
            );
        }
        for ns in ["c9watch", "c9watch.extra", "agent.a", "client.phone"] {
            let mut set = json!({"key":format!("{ns}/x")});
            assert_eq!(
                admit("annotation.set", &mut set, &Principal::LocalUnverified)
                    .unwrap_err()
                    .code,
                ErrorCode::NotAllowed
            );
        }
        // Current gateway token IDs are hyphenated UUIDs: 7 + 36 > 40.
        // Keep the namespace cap fail closed until the owner settles that
        // contract mismatch; never silently shorten an authenticated ID.
        let long_client = Principal::Client {
            id: "00000000-0000-4000-8000-000000000001".into(),
        };
        let mut p = request("ci", json!({"x":null}), None, None);
        p.as_object_mut().unwrap().remove("source");
        assert_eq!(
            admit("annotation.patch", &mut p, &long_client)
                .unwrap_err()
                .code,
            ErrorCode::BadRequest
        );
        let client = Principal::Client { id: "phone".into() };
        for method in ["annotation.set", "annotation.delete", "annotation.patch"] {
            assert!(!super::super::authz::can(method, &client, None).allowed);
            let access = super::super::authz::Access {
                scopes: ["annotation:write".into()].into(),
                ..Default::default()
            };
            assert!(super::super::authz::can(method, &client, Some(&access)).allowed);
        }
    }
    #[test]
    fn validation_normalizes_text_and_rejects_invalid_types_and_ttls() {
        let p = patch(request(
            "ci",
            json!({"x":{"type":"string","v":format!(" \u{0000}{}\n ","😀".repeat(100))}}),
            None,
            Some(1),
        ));
        let AnnotationValue::String { v } = p.values["x"].as_ref().unwrap() else {
            panic!()
        };
        assert_eq!(v.len(), 256);
        let p = patch(request(
            "ci",
            json!({"x":{"type":"string","v":"a".repeat(300)}}),
            None,
            None,
        ));
        assert!(
            matches!(p.values["x"].as_ref().unwrap(),AnnotationValue::String{v} if v.len()==80)
        );
        for value in [
            json!({"type":"duration","ms":-1}),
            json!({"type":"number","v":"NaN"}),
            json!({"type":"money","amount":"1e9","currency":"USD"}),
            json!({"type":"money","amount":"3.1","currency":"ZZZ"}),
            json!({"type":"bool","v":"yes"}),
        ] {
            assert!(parse(
                "annotation.patch",
                request("ci", json!({"x":value}), None, None)
            )
            .is_err());
        }
        for ttl in [0, MAX_TTL_MS + 1] {
            assert!(parse(
                "annotation.patch",
                request("ci", json!({"x":{"type":"bool","v":true}}), None, Some(ttl))
            )
            .is_err());
        }
        assert!(normalize(&mut AnnotationValue::Number { v: f64::INFINITY }).is_err());
        for ttl in [1, 59, 86_401] {
            assert!(parse("annotation.set",json!({"target":{"kind":"session","sessionKey":"s"},"key":"ci/x","value":{"type":"bool","v":true},"ttlSeconds":ttl})).is_err());
        }
    }
    #[test]
    fn patches_keep_omitted_keys_and_stale_sequences_never_emit_or_revert() {
        let (_t, db) = db();
        let one = patch(request(
            "ci",
            json!({"a":{"type":"number","v":1},"b":{"type":"bool","v":true}}),
            Some(2),
            None,
        ));
        apply_at(&db, one, 1000).unwrap();
        let head = db.head_cursor();
        for seq in [1, 2] {
            let r = apply_at(
                &db,
                patch(request("ci", json!({"a":null}), Some(seq), None)),
                1100,
            )
            .unwrap();
            assert_eq!(r["applied"], false);
            assert_eq!(db.head_cursor(), head);
        }
        let r = apply_at(
            &db,
            patch(request(
                "ci",
                json!({"a":null,"c":{"type":"duration","ms":50}}),
                Some(3),
                None,
            )),
            1200,
        )
        .unwrap();
        assert!(r["values"].get("a").is_none());
        assert!(r["values"].get("b").is_some());
        assert!(r["values"].get("c").is_some());
        let c = db.reader().unwrap();
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM events WHERE topic='annotation.changed'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            2
        );
    }
    #[test]
    fn empty_unsequenced_sources_do_not_accumulate_and_expiry_releases_them() {
        let (_t, db) = db();
        for i in 0..256 {
            let source = format!("source{i}");
            let at = 1000 + i * 300;
            let absent = parse(
                "annotation.delete",
                json!({
                    "target":{"kind":"session","sessionKey":"claudeCode:s"},
                    "key":format!("{source}/x")
                }),
            )
            .unwrap();
            db.write_blocking(move |tx| {
                apply(tx, &absent, "local-unverified", "annotation.delete", at)
            })
            .unwrap()
            .0
            .unwrap();
            assert_eq!(
                db.reader()
                    .unwrap()
                    .query_row("SELECT count(*) FROM annotation_sources", [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
            apply_at(
                &db,
                patch(request(
                    &source,
                    json!({"x":{"type":"bool","v":true}}),
                    None,
                    None,
                )),
                at + 100,
            )
            .unwrap();
            apply_at(
                &db,
                patch(request(&source, json!({"x":null}), None, None)),
                at + 200,
            )
            .unwrap();
        }
        db.write_blocking(|tx| {
            assert_eq!(tx.query_row("SELECT count(*) FROM annotation_sources", [], |r| r.get::<_, i64>(0))?, 0);
            // Also collect records left by the old absent-delete implementation.
            tx.execute("INSERT INTO annotation_sources(target_kind,target_id,namespace) VALUES ('session','claudeCode:s','legacy')", [])?;
            Ok(())
        }).unwrap();
        apply_at(
            &db,
            patch(request(
                "ttl",
                json!({"x":{"type":"bool","v":true}}),
                None,
                Some(1),
            )),
            100_000,
        )
        .unwrap();
        db.write_blocking(|tx| {
            sweep(tx, 100_001)?;
            assert_eq!(
                tx.query_row("SELECT count(*) FROM annotation_sources", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .unwrap();
    }
    #[test]
    fn sequence_tombstones_are_bounded_without_evicting_accepted_watermarks() {
        let (t, db) = db();
        // Reserve one slot via a value, then expiry. Reserve the others with
        // sequenced clears against absent keys; none may disappear on a sweep.
        apply_at(
            &db,
            patch(request(
                "source0",
                json!({"x":{"type":"bool","v":true}}),
                Some(5),
                Some(1),
            )),
            1000,
        )
        .unwrap();
        for i in 1..MAX_SOURCES_PER_TARGET {
            apply_at(
                &db,
                patch(request(
                    &format!("source{i}"),
                    json!({"x":null}),
                    Some(5),
                    None,
                )),
                1000 + i * 100,
            )
            .unwrap();
        }
        db.write_blocking(|tx| sweep(tx, 20_000)).unwrap();
        let head = db.head_cursor();
        let err = apply_at(
            &db,
            patch(request("overflow", json!({"x":null}), Some(1), None)),
            20_100,
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::LimitExceeded);
        assert_eq!(db.head_cursor(), head);
        let err = apply_at(
            &db,
            patch(request(
                "overflow",
                json!({"x":{"type":"bool","v":true}}),
                None,
                None,
            )),
            20_200,
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::LimitExceeded);
        // No-op unsequenced deletes remain harmless at capacity.
        apply_at(
            &db,
            patch(request("overflow", json!({"x":null}), None, None)),
            20_300,
        )
        .unwrap();
        drop(db);
        let db = super::super::db::open(&t.path().join("engine"), "m").unwrap();
        let stale = apply_at(
            &db,
            patch(request(
                "source0",
                json!({"x":{"type":"bool","v":true}}),
                Some(5),
                None,
            )),
            20_400,
        )
        .unwrap();
        assert_eq!(stale["applied"], false);
        assert!(stale["values"].as_object().unwrap().is_empty());
        // Existing source operations still work at capacity. An unsequenced
        // clear must preserve the earlier sequenced watermark.
        apply_at(
            &db,
            patch(request(
                "source0",
                json!({"x":{"type":"bool","v":true}}),
                Some(6),
                None,
            )),
            20_500,
        )
        .unwrap();
        apply_at(
            &db,
            patch(request("source0", json!({"x":null}), None, None)),
            20_600,
        )
        .unwrap();
        drop(db);
        let db = super::super::db::open(&t.path().join("engine"), "m").unwrap();
        let stale = apply_at(
            &db,
            patch(request(
                "source0",
                json!({"x":{"type":"bool","v":true}}),
                Some(6),
                None,
            )),
            20_700,
        )
        .unwrap();
        assert_eq!(stale["applied"], false);
        db.write_blocking(|tx| {
            assert_eq!(
                tx.query_row("SELECT count(*) FROM annotation_sources", [], |r| r
                    .get::<_, i64>(0))?,
                MAX_SOURCES_PER_TARGET
            );
            tx.execute("UPDATE agents SET gone_at=21000", [])?;
            track_targets(tx, 21_000)?;
            sweep(tx, 21_000 + TARGET_GRACE_MS)?;
            assert_eq!(
                tx.query_row("SELECT count(*) FROM annotation_sources", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .unwrap();
    }
    #[test]
    fn thirty_third_key_is_refused_atomically_but_replace_at_capacity_works() {
        let (_t, db) = db();
        let values: serde_json::Map<String, Value> = (0..32)
            .map(|i| (format!("k{i}"), json!({"type":"number","v":i})))
            .collect();
        apply_at(&db, patch(request("ci", values.into(), None, None)), 1000).unwrap();
        let head = db.head_cursor();
        let e = apply_at(
            &db,
            patch(request(
                "ci",
                json!({"k0":null,"k32":{"type":"bool","v":true},"k33":{"type":"bool","v":true}}),
                None,
                None,
            )),
            1100,
        )
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::LimitExceeded);
        assert_eq!(db.head_cursor(), head);
        let r = apply_at(
            &db,
            patch(request(
                "ci",
                json!({"k0":null,"k32":{"type":"bool","v":true}}),
                None,
                None,
            )),
            1200,
        )
        .unwrap();
        assert_eq!(r["values"].as_object().unwrap().len(), 32);
    }
    #[test]
    fn target_and_global_namespace_limits_are_enforced() {
        let (_t, db) = db();
        db.write_blocking(|tx| {
            let values: serde_json::Map<String, Value> = (0..32)
                .map(|i| (format!("k{i}"), json!({"type":"number","v":i})))
                .collect();
            for n in 0..4 {
                let p = patch(request(
                    &format!("ns{n}"),
                    values.clone().into(),
                    None,
                    None,
                ));
                apply(
                    tx,
                    &p,
                    "local-unverified",
                    "annotation.patch",
                    1000 + n * 100,
                )?
                .unwrap();
            }
            let p = patch(request(
                "fifth",
                json!({"x":{"type":"bool","v":true}}),
                None,
                None,
            ));
            assert_eq!(
                apply(tx, &p, "local-unverified", "annotation.patch", 1500)?
                    .unwrap_err()
                    .code,
                ErrorCode::LimitExceeded
            );
            // 62 full targets and 16 keys on the last = 2000 in one namespace.
            for n in 0..63 {
                let id = format!("claudeCode:global{n}");
                seed(tx, &id)?;
                let mut v = request("global", values.clone().into(), None, None);
                v["target"]["sessionKey"] = id.into();
                if n == 62 {
                    v["values"]
                        .as_object_mut()
                        .unwrap()
                        .retain(|k, _| k[1..].parse::<i32>().unwrap() < 16);
                }
                apply(
                    tx,
                    &patch(v),
                    "local-unverified",
                    "annotation.patch",
                    2000 + n * 100,
                )?
                .unwrap();
            }
            let mut v = request(
                "global",
                json!({"extra":{"type":"bool","v":true}}),
                None,
                None,
            );
            v["target"]["sessionKey"] = "claudeCode:global62".into();
            assert_eq!(
                apply(tx, &patch(v), "local-unverified", "annotation.patch", 9000)?
                    .unwrap_err()
                    .code,
                ErrorCode::LimitExceeded
            );
            Ok(())
        })
        .unwrap();
    }
    #[test]
    fn principal_budget_is_shared_by_namespaces_and_refills_at_ten_per_second() {
        let (_t, db) = db();
        for i in 0..20 {
            apply_at(
                &db,
                patch(request(
                    &format!("ns{i}"),
                    json!({"x":{"type":"bool","v":true}}),
                    None,
                    None,
                )),
                1000,
            )
            .unwrap();
        }
        for at in [1000, 1099] {
            assert_eq!(
                apply_at(
                    &db,
                    patch(request("other", json!({"x":null}), None, None)),
                    at
                )
                .unwrap_err()
                .code,
                ErrorCode::LimitExceeded
            );
        }
        apply_at(
            &db,
            patch(request("other", json!({"x":null}), None, None)),
            1100,
        )
        .unwrap();
    }
    #[tokio::test]
    async fn expiry_is_per_key_emits_one_map_and_list_excludes_expired_values() {
        let (_t, db) = db();
        let at = now_ms() - 1000;
        apply_at(
            &db,
            patch(request(
                "ci",
                json!({"short":{"type":"number","v":1}}),
                Some(1),
                Some(1),
            )),
            at,
        )
        .unwrap();
        apply_at(
            &db,
            patch(request(
                "ci",
                json!({"long":{"type":"bool","v":true}}),
                Some(2),
                Some(MAX_TTL_MS),
            )),
            at,
        )
        .unwrap();
        let listed = list(&db, json!({})).await.unwrap();
        assert_eq!(listed["annotations"].as_array().unwrap().len(), 1);
        assert_eq!(listed["annotations"][0]["key"], "ci/long");
        db.write_blocking(move |tx| sweep(tx, at + 1)).unwrap();
        let c = db.reader().unwrap();
        let payload:String=c.query_row("SELECT payload FROM events WHERE topic='annotation.changed' ORDER BY seq DESC LIMIT 1",[],|r|r.get(0)).unwrap();
        let event: Value = serde_json::from_str(&payload).unwrap();
        assert!(event["values"].get("short").is_none());
        assert!(event["values"].get("long").is_some());
        assert_eq!(
            c.query_row("SELECT count(*) FROM annotations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            c.query_row("SELECT last_seq FROM annotation_sources", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
    }
    #[test]
    fn ttl_is_swept_before_observation_even_when_observation_fails() {
        use crate::engine::detect::{Detector, Observation, Observe};
        use std::sync::Arc;
        struct CheckExpiry(Arc<Db>);
        impl Observe for CheckExpiry {
            fn observe(&mut self) -> Result<Observation, String> {
                let c = self.0.reader().unwrap();
                let remaining: i64 = c
                    .query_row("SELECT count(*) FROM annotations", [], |r| r.get(0))
                    .unwrap();
                assert_eq!(remaining, 0, "expiry must commit before the observer runs");
                let payload: String = c.query_row("SELECT payload FROM events WHERE topic='annotation.changed' ORDER BY seq DESC LIMIT 1", [], |r| r.get(0)).unwrap();
                let event: serde_json::Value = serde_json::from_str(&payload).unwrap();
                assert_eq!(event["values"], serde_json::json!({}));
                Err("detect failed".into())
            }
        }
        let (_t, db) = db();
        let db = Arc::new(db);
        let params = parse(
            "annotation.patch",
            serde_json::json!({
                "target":{"kind":"session","sessionKey":"claudeCode:s"}, "source":"ci",
                "values":{"x":{"type":"bool","v":true}}, "ttlMs":1, "seq":1
            }),
        )
        .unwrap();
        db.write_blocking(move |tx| {
            apply(tx, &params, "local-unverified", "annotation.patch", 1000)
        })
        .unwrap()
        .0
        .unwrap();
        let mut detector = Detector::new(Box::new(CheckExpiry(db.clone())), "m".into());
        assert_eq!(detector.tick(&db).unwrap_err(), "detect failed");
    }

    #[test]
    fn sweep_skips_idle_rows_and_cleans_up_each_kind_of_due_work() {
        let (_t, db) = db();
        assert!(!sweep_if_due(&db, 1000).unwrap());
        apply_at(
            &db,
            patch(request(
                "ci",
                json!({"x":{"type":"bool","v":true}}),
                Some(1),
                Some(1000),
            )),
            1000,
        )
        .unwrap();
        assert!(!sweep_if_due(&db, 1999).unwrap());
        assert!(sweep_if_due(&db, 2000).unwrap());
        assert!(
            !sweep_if_due(&db, 2000).unwrap(),
            "a live sequence tombstone must remain"
        );
        db.write_blocking(|tx| {
            tx.execute("UPDATE annotation_sources SET gone_at=2000", [])?;
            Ok(())
        })
        .unwrap();
        // Remove the old rate budget first; then only target grace is due.
        assert!(sweep_if_due(&db, 1001 + MAX_TTL_MS as i64).unwrap());
        assert!(!sweep_if_due(&db, 2000 + TARGET_GRACE_MS - 1).unwrap());
        assert!(sweep_if_due(&db, 2000 + TARGET_GRACE_MS).unwrap());
        assert!(!sweep_if_due(&db, 2000 + TARGET_GRACE_MS).unwrap());
        db.write_blocking(|tx| {
            tx.execute("INSERT INTO annotation_sources VALUES ('session','claudeCode:s','legacy',NULL,NULL)", [])?;
            Ok(())
        }).unwrap();
        assert!(sweep_if_due(&db, 2000 + TARGET_GRACE_MS).unwrap());
        assert!(!sweep_if_due(&db, 2000 + TARGET_GRACE_MS).unwrap());
    }

    #[test]
    fn target_disappearance_grace_survives_restart_and_return_cancels_it() {
        let (t, db) = db();
        apply_at(
            &db,
            patch(request(
                "ci",
                json!({"x":{"type":"number","v":1}}),
                Some(5),
                None,
            )),
            1000,
        )
        .unwrap();
        db.write_blocking(|tx| {
            tx.execute("UPDATE agents SET gone_at=2000", [])?;
            track_targets(tx, 2000)?;
            sweep(tx, 2000 + TARGET_GRACE_MS - 1)
        })
        .unwrap();
        drop(db);
        let db = super::super::db::open(&t.path().join("engine"), "m").unwrap();
        db.write_blocking(|tx| {
            tx.execute("UPDATE agents SET gone_at=NULL", [])?;
            track_targets(tx, 3000)?;
            sweep(tx, 2000 + TARGET_GRACE_MS)?;
            assert_eq!(
                source_map(
                    tx,
                    &AnnotationTarget {
                        kind: "session".into(),
                        session_key: "claudeCode:s".into()
                    },
                    "ci",
                    i64::MAX
                )?
                .len(),
                1
            );
            Ok(())
        })
        .unwrap();
        db.write_blocking(|tx| {
            tx.execute("UPDATE agents SET gone_at=4000", [])?;
            track_targets(tx, 4000)?;
            sweep(tx, 4000 + TARGET_GRACE_MS)
        })
        .unwrap();
        let c = db.reader().unwrap();
        assert_eq!(
            c.query_row("SELECT count(*) FROM annotations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let payload: String = c
            .query_row(
                "SELECT payload FROM events WHERE topic='annotation.removed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&payload).unwrap()["reason"],
            "target_gone"
        );
    }
    #[tokio::test]
    async fn successful_writes_have_one_hash_only_audit_each_and_stamped_writer() {
        let (_t, db) = db();
        let p = json!({"target":{"kind":"session","sessionKey":"claudeCode:s"},"key":"ci/note","value":{"type":"string","v":"secret prompt"},"label":" \n tests \u{0000} "});
        let r = write(&db, "annotation.set", p, &Principal::LocalUnverified)
            .await
            .unwrap();
        assert_eq!(r["annotation"]["writer"], "local-unverified");
        assert_eq!(r["annotation"]["label"], "tests");
        write(
            &db,
            "annotation.delete",
            json!({"target":{"kind":"session","sessionKey":"claudeCode:s"},"key":"ci/note"}),
            &Principal::LocalUnverified,
        )
        .await
        .unwrap();
        let c = db.reader().unwrap();
        let rows: Vec<(String, String, String)> = c
            .prepare("SELECT payload_hash,annotation_keys,coalesce(reason,'') FROM audit WHERE method IN ('annotation.set','annotation.delete')")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(rows.len(), 2);
        for (hash, keys, reason) in rows {
            assert_eq!(hash.len(), 64);
            assert_eq!(keys, "[\"ci/note\"]");
            assert!(!format!("{hash}{keys}{reason}").contains("secret"));
        }
    }
    #[test]
    fn schema_redacts_string_values_but_keeps_numbers_in_results_and_source_maps() {
        use crate::gateway::redaction::{project, Sensitivity};
        let schema = super::super::api::schema();
        let a = |value: Value| json!({"target":{"kind":"session","sessionKey":"claudeCode:s"},"key":"ci/x","value":value,"writer":"local-unverified","sensitivity":"conversation","createdAt":1,"updatedAt":1,"expiresAt":null});
        for (value, visible) in [
            (json!({"type":"string","v":"secret"}), false),
            (json!({"type":"number","v":42}), true),
            (
                json!({"type":"money","amount":"4.20","currency":"USD"}),
                true,
            ),
            (json!({"type":"future","v":"secret"}), false),
        ] {
            let input = a(value);
            let shape = &schema["methods"]["annotation.set"]["result"];
            let out = project(
                &json!({"annotation":input.clone()}),
                shape,
                &schema,
                Sensitivity::Metadata,
            );
            assert_eq!(out["annotation"].get("value").is_some(), visible, "{out}");
            if !visible {
                assert_eq!(out["annotation"]["redacted"], true);
                assert!(!out.to_string().contains("secret"));
            }
            let data = json!({"target":input["target"],"source":"ci","values":{"x":input}});
            let out = project(
                &data,
                &schema["topics"]["annotation.changed"]["data"],
                &schema,
                Sensitivity::Metadata,
            );
            assert_eq!(out["values"]["x"].get("value").is_some(), visible, "{out}");
            assert!(!out.to_string().contains("secret"));
        }
    }
}
