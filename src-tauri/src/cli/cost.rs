//! `c9watch cost`: whole-dataset aggregation and per-session breakdowns.

use super::{print_json, CostSortDir, CostSortField};
use crate::session;

/// Minimum length for `--session` values when treated as a prefix. Use
/// `--session-prefix` for anything shorter.
const SESSION_MIN_LEN: usize = 8;

/// How a session lookup value was provided — controls min-length enforcement.
#[derive(Clone, Copy, Debug, PartialEq)]
enum SessionLookupKind {
    /// `--session`: full id or ≥8-char prefix. Exact matches win outright.
    Session,
    /// `--session-prefix`: any length; must match exactly one id.
    Prefix,
}

/// Resolve a session identifier (full id or prefix) against all known sessions.
///
/// Returns the full `(session_id, provider)` identity, or an error if the input
/// is empty, too short for the chosen kind, or matches zero/multiple sessions.
fn resolve_cost_session_identity(
    sessions: &[session::cost::SessionCostRecord],
    needle: &str,
    kind: SessionLookupKind,
) -> Result<(String, String), String> {
    let (provider_filter, raw_needle) = parse_provider_scoped_cost_reference(needle);
    if raw_needle.is_empty() {
        return Err("Session id must not be empty".to_string());
    }
    if kind == SessionLookupKind::Session && raw_needle.len() < SESSION_MIN_LEN {
        return Err(format!(
            "--session value '{}' is too short — must be at least {} characters, or use --session-prefix for shorter prefixes",
            needle, SESSION_MIN_LEN
        ));
    }

    let mut matches = std::collections::BTreeSet::new();
    for s in sessions {
        if provider_filter.is_some_and(|provider| s.provider != provider) {
            continue;
        }
        if s.session_id.starts_with(raw_needle) {
            // Cost data spans providers; keep the provider namespace in the
            // resolver so identical opaque IDs cannot be aggregated together.
            matches.insert((s.session_id.clone(), s.provider.clone()));
        }
    }

    // For `--session`, a full-id exact match wins even if other sessions share
    // the same prefix, but only when exactly one provider owns that ID.
    if kind == SessionLookupKind::Session {
        let exact: Vec<_> = matches.iter().filter(|(id, _)| id == raw_needle).collect();
        if exact.len() == 1 {
            let (_, provider) = exact[0];
            return Ok((raw_needle.to_string(), provider.clone()));
        }
    }

    match matches.len() {
        0 => Err(format!("No session matching '{}'", needle)),
        1 => {
            let (id, provider) = matches.into_iter().next().unwrap();
            Ok((id, provider))
        }
        n => {
            let ids: Vec<String> = matches.into_iter().map(|(id, _)| id).collect();
            let shown = ids.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
            let suffix = if n > 5 {
                format!(" (and {} more)", n - 5)
            } else {
                String::new()
            };
            Err(format!(
                "Ambiguous '{}' matches {} sessions: {}{}",
                needle, n, shown, suffix
            ))
        }
    }
}

fn parse_provider_scoped_cost_reference(reference: &str) -> (Option<&str>, &str) {
    let Some((provider, raw_id)) = reference.split_once(':') else {
        return (None, reference);
    };
    if raw_id.is_empty() || !matches!(provider, "claudeCode" | "codex" | "cursor" | "pi") {
        return (None, reference);
    }
    (Some(provider), raw_id)
}

#[cfg(test)]
fn resolve_cost_session_id(
    sessions: &[session::cost::SessionCostRecord],
    needle: &str,
    kind: SessionLookupKind,
) -> Result<String, String> {
    resolve_cost_session_identity(sessions, needle, kind).map(|(session_id, _)| session_id)
}

/// Parse the `--since YYYY-MM-DD` flag. `cost` and `cost --session` share
/// this, so they accept the same dates and print the same error text.
fn parse_since(since: Option<&str>) -> Result<Option<chrono::NaiveDate>, String> {
    since
        .map(|s| {
            chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map_err(|e| format!("Invalid --since date '{}': {}", s, e))
        })
        .transpose()
}

/// True when the record's `YYYY-MM-DD` date is on or after `since`. A record
/// whose date does not parse never matches.
fn on_or_after(record: &session::cost::SessionCostRecord, since: chrono::NaiveDate) -> bool {
    chrono::NaiveDate::parse_from_str(&record.date, "%Y-%m-%d")
        .map(|d| d >= since)
        .unwrap_or(false)
}

/// Rows returned by filtering cost data down to a single session.
#[derive(Debug)]
struct SessionCostBreakdown {
    session_id: String,
    provider: String,
    session_name: String,
    project: String,
    project_name: String,
    total_cost: f64,
    total_tokens: u64,
    models: Vec<String>,
    earliest: String,
    latest: String,
    daily: Vec<DailyRow>,
}

#[derive(Debug)]
struct DailyRow {
    date: String,
    cost: f64,
    total_tokens: u64,
    model: String,
}

/// Build a per-session breakdown from raw session records. Factored out so
/// tests can exercise filtering + aggregation without touching disk.
fn build_session_breakdown(
    all_sessions: &[session::cost::SessionCostRecord],
    session_id: &str,
    provider: &str,
    since_date: Option<chrono::NaiveDate>,
) -> Result<SessionCostBreakdown, String> {
    let mut records: Vec<&session::cost::SessionCostRecord> = all_sessions
        .iter()
        .filter(|s| s.session_id == session_id)
        .filter(|s| s.provider == provider)
        .filter(|s| match since_date {
            Some(since_d) => on_or_after(s, since_d),
            None => true,
        })
        .collect();

    if records.is_empty() {
        return Err(format!(
            "No cost data for session '{}' in the given date range",
            session_id
        ));
    }

    records.sort_by(|a, b| a.date.cmp(&b.date).then(a.timestamp.cmp(&b.timestamp)));

    let total_cost: f64 = records.iter().map(|s| s.cost).sum();
    let total_tokens: u64 = records.iter().map(|s| s.total_tokens).sum();

    let mut seen_models = std::collections::HashSet::new();
    let mut models: Vec<String> = Vec::new();
    for s in &records {
        if !s.model.is_empty() && seen_models.insert(s.model.clone()) {
            models.push(s.model.clone());
        }
    }

    let earliest = records
        .iter()
        .map(|s| &s.timestamp)
        .min()
        .cloned()
        .unwrap_or_default();
    let latest = records
        .iter()
        .map(|s| &s.timestamp)
        .max()
        .cloned()
        .unwrap_or_default();

    let daily = records
        .iter()
        .map(|s| DailyRow {
            date: s.date.clone(),
            cost: s.cost,
            total_tokens: s.total_tokens,
            model: s.model.clone(),
        })
        .collect();

    let first = records.first().unwrap();
    Ok(SessionCostBreakdown {
        session_id: session_id.to_string(),
        provider: provider.to_string(),
        session_name: first.session_name.clone(),
        project: first.project.clone(),
        project_name: first.project_name.clone(),
        total_cost,
        total_tokens,
        models,
        earliest,
        latest,
        daily,
    })
}

fn breakdown_to_json(b: &SessionCostBreakdown) -> serde_json::Value {
    let by_day: Vec<serde_json::Value> = b
        .daily
        .iter()
        .map(|d| {
            serde_json::json!({
                "date": d.date,
                "cost": d.cost,
                "totalTokens": d.total_tokens,
                "model": d.model,
            })
        })
        .collect();
    serde_json::json!({
        "sessionId": b.session_id,
        "provider": b.provider,
        "sessionName": b.session_name,
        "project": b.project,
        "projectName": b.project_name,
        "totalCost": b.total_cost,
        "totalTokens": b.total_tokens,
        "turns": b.daily.len(),
        "models": b.models,
        "earliest": b.earliest,
        "latest": b.latest,
        "byDay": by_day,
    })
}

/// Per-session cost breakdown for `c9watch cost --session <id>` / `--session-prefix <prefix>`.
fn cmd_cost_session(
    needle: &str,
    kind: SessionLookupKind,
    since: Option<&str>,
    pretty: bool,
) -> Result<(), String> {
    let since_date = parse_since(since)?;

    let data = session::get_cost_data()?;
    let all_sessions: Vec<session::cost::SessionCostRecord> = data
        .project_costs
        .iter()
        .flat_map(|p| p.sessions.iter().cloned())
        .collect();

    let (session_id, provider) = resolve_cost_session_identity(&all_sessions, needle, kind)?;
    let breakdown = build_session_breakdown(&all_sessions, &session_id, &provider, since_date)?;

    print_json(&breakdown_to_json(&breakdown), pretty)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn cmd_cost(
    daily: bool,
    weekly: bool,
    project_filter: Option<&str>,
    since: Option<&str>,
    sort: CostSortField,
    sort_dir: CostSortDir,
    session: Option<&str>,
    session_prefix: Option<&str>,
    pretty: bool,
) -> Result<(), String> {
    // clap `conflicts_with_all` already blocks mixing these flags, but keep a
    // belt-and-braces check in case the function is called programmatically.
    if let Some(needle) = session {
        if daily || weekly || project_filter.is_some() || session_prefix.is_some() {
            return Err(
                "--session cannot be combined with --daily, --weekly, --project, or --session-prefix"
                    .to_string(),
            );
        }
        return cmd_cost_session(needle, SessionLookupKind::Session, since, pretty);
    }
    if let Some(prefix) = session_prefix {
        if daily || weekly || project_filter.is_some() {
            return Err(
                "--session-prefix cannot be combined with --daily, --weekly, or --project"
                    .to_string(),
            );
        }
        return cmd_cost_session(prefix, SessionLookupKind::Prefix, since, pretty);
    }

    if daily && weekly {
        return Err("--daily and --weekly are mutually exclusive".to_string());
    }

    let since_date = parse_since(since)?;

    let data = session::get_cost_data()?;

    // Flatten to sessions, then apply filters. project_costs covers all sessions.
    let mut sessions: Vec<session::cost::SessionCostRecord> = data
        .project_costs
        .iter()
        .flat_map(|p| p.sessions.iter().cloned())
        .collect();

    if let Some(proj) = project_filter {
        sessions.retain(|s| s.project == proj);
    }
    if let Some(since_d) = since_date {
        sessions.retain(|s| on_or_after(s, since_d));
    }

    // Helper: apply sort to a rows vec that has a date key and a cost key.
    // `date_key` is the JSON field name for the date string (ISO format, so
    // lexicographic order equals chronological order).
    let sort_rows = |rows: &mut Vec<serde_json::Value>, date_key: &str| {
        rows.sort_by(|a, b| {
            let ord = match sort {
                CostSortField::Date => a
                    .get(date_key)
                    .and_then(|v| v.as_str())
                    .cmp(&b.get(date_key).and_then(|v| v.as_str())),
                CostSortField::Cost => {
                    let ca = a.get("cost").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let cb = b.get("cost").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    ca.partial_cmp(&cb).unwrap_or(std::cmp::Ordering::Equal)
                }
            };
            // Flip for descending
            if sort_dir == CostSortDir::Desc {
                ord.reverse()
            } else {
                ord
            }
        });
    };

    if daily {
        let mut by_date: std::collections::HashMap<String, (f64, u64, u32)> =
            std::collections::HashMap::new();
        for s in &sessions {
            let e = by_date.entry(s.date.clone()).or_insert((0.0, 0, 0));
            e.0 += s.cost;
            e.1 += s.total_tokens;
            e.2 += 1;
        }
        let mut rows: Vec<_> = by_date
            .into_iter()
            .map(|(date, (cost, tokens, count))| {
                serde_json::json!({
                    "date": date,
                    "cost": cost,
                    "totalTokens": tokens,
                    "sessionCount": count,
                })
            })
            .collect();
        sort_rows(&mut rows, "date");
        return print_json(&rows, pretty);
    }

    if weekly {
        let mut by_week: std::collections::HashMap<String, (f64, u64, u32)> =
            std::collections::HashMap::new();
        for s in &sessions {
            let Ok(d) = chrono::NaiveDate::parse_from_str(&s.date, "%Y-%m-%d") else {
                continue;
            };
            use chrono::Datelike;
            let iso = d.iso_week();
            let week_start =
                chrono::NaiveDate::from_isoywd_opt(iso.year(), iso.week(), chrono::Weekday::Mon)
                    .unwrap_or(d);
            let key = week_start.format("%Y-%m-%d").to_string();
            let e = by_week.entry(key).or_insert((0.0, 0, 0));
            e.0 += s.cost;
            e.1 += s.total_tokens;
            e.2 += 1;
        }
        let mut rows: Vec<_> = by_week
            .into_iter()
            .map(|(week_start, (cost, tokens, count))| {
                serde_json::json!({
                    "weekStart": week_start,
                    "cost": cost,
                    "totalTokens": tokens,
                    "sessionCount": count,
                })
            })
            .collect();
        sort_rows(&mut rows, "weekStart");
        return print_json(&rows, pretty);
    }

    // Default: emit filtered CostData-shaped output if filters were applied,
    // otherwise emit the full CostData from the GUI.
    if project_filter.is_none() && since_date.is_none() {
        // --sort date --project is nonsensical (no single date per project);
        // silently fall back to cost sort in that case. Here there's no project
        // aggregation at all, so just return raw CostData unchanged.
        return print_json(&data, pretty);
    }

    // Project mode: --sort date is not meaningful (projects have no single date);
    // silently fall back to cost sort and note it in a comment.
    let effective_sort = if project_filter.is_some() && sort == CostSortField::Date {
        // date sort doesn't apply to project aggregations — use cost instead
        CostSortField::Cost
    } else {
        sort
    };

    // Rebuild CostData-ish aggregation from filtered sessions, sorting by session date or cost.
    let total_cost: f64 = sessions.iter().map(|s| s.cost).sum();
    let total_tokens: u64 = sessions.iter().map(|s| s.total_tokens).sum();

    // Sort individual sessions in the filtered output
    sessions.sort_by(|a, b| {
        let ord = match effective_sort {
            CostSortField::Date => a.date.cmp(&b.date),
            CostSortField::Cost => a
                .cost
                .partial_cmp(&b.cost)
                .unwrap_or(std::cmp::Ordering::Equal),
        };
        if sort_dir == CostSortDir::Desc {
            ord.reverse()
        } else {
            ord
        }
    });

    let output = serde_json::json!({
        "totalCost": total_cost,
        "totalTokens": total_tokens,
        "sessions": sessions,
    });
    print_json(&output, pretty)
}

#[cfg(test)]
mod cost_session_tests {
    use super::*;
    use crate::cli::Cli;
    use crate::session::cost::SessionCostRecord;

    fn rec(session_id: &str, date: &str, cost: f64, tokens: u64, model: &str) -> SessionCostRecord {
        SessionCostRecord {
            session_id: session_id.to_string(),
            project: "/tmp/proj".to_string(),
            project_name: "proj".to_string(),
            model: model.to_string(),
            provider: "claudeCode".to_string(),
            surface: None,
            agent_kind: None,
            cost,
            cost_available: true,
            input_tokens: tokens,
            cached_input_tokens: 0,
            output_tokens: 0,
            reasoning_output_tokens: 0,
            total_tokens: tokens,
            timestamp: format!("{}T00:00:00Z", date),
            date: date.to_string(),
            session_name: "sample session".to_string(),
        }
    }

    fn fixtures() -> Vec<SessionCostRecord> {
        vec![
            rec(
                "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee",
                "2026-04-10",
                1.25,
                1000,
                "sonnet",
            ),
            rec(
                "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee",
                "2026-04-11",
                0.75,
                500,
                "sonnet",
            ),
            rec(
                "aaaa2222-bbbb-cccc-dddd-ffffffffffff",
                "2026-04-10",
                0.50,
                200,
                "haiku",
            ),
            rec(
                "bbbb3333-cccc-dddd-eeee-000000000000",
                "2026-04-11",
                2.00,
                5000,
                "opus",
            ),
        ]
    }

    #[test]
    fn session_exact_full_id_match() {
        let sessions = fixtures();
        let full = "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee";
        let got = resolve_cost_session_id(&sessions, full, SessionLookupKind::Session).unwrap();
        assert_eq!(got, full);
    }

    #[test]
    fn cost_session_id_collision_across_providers_is_not_silently_merged() {
        let mut sessions = fixtures();
        let mut codex = sessions[0].clone();
        codex.provider = "codex".to_string();
        sessions.push(codex);

        let err = resolve_cost_session_id(
            &sessions,
            "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee",
            SessionLookupKind::Session,
        )
        .unwrap_err();
        assert!(err.contains("Ambiguous"));
    }

    #[test]
    fn provider_scoped_cost_key_selects_one_collision_namespace() {
        let mut sessions = fixtures();
        let mut codex = sessions[0].clone();
        codex.provider = "codex".to_string();
        sessions.push(codex);

        let (session_id, provider) = resolve_cost_session_identity(
            &sessions,
            "codex:aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee",
            SessionLookupKind::Session,
        )
        .unwrap();
        assert_eq!(session_id, "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee");
        assert_eq!(provider, "codex");
    }

    #[test]
    fn session_prefix_unambiguous() {
        let sessions = fixtures();
        let got =
            resolve_cost_session_id(&sessions, "bbbb3333", SessionLookupKind::Session).unwrap();
        assert_eq!(got, "bbbb3333-cccc-dddd-eeee-000000000000");
    }

    #[test]
    fn session_prefix_too_short_for_session_flag() {
        let sessions = fixtures();
        let err =
            resolve_cost_session_id(&sessions, "aaaa", SessionLookupKind::Session).unwrap_err();
        assert!(err.contains("too short"), "got: {}", err);
        assert!(err.contains("--session-prefix"), "got: {}", err);
    }

    #[test]
    fn session_prefix_kind_allows_short_unique() {
        let sessions = fixtures();
        let got = resolve_cost_session_id(&sessions, "bbbb", SessionLookupKind::Prefix).unwrap();
        assert_eq!(got, "bbbb3333-cccc-dddd-eeee-000000000000");
    }

    #[test]
    fn session_prefix_ambiguous_lists_candidates() {
        let sessions = fixtures();
        let err =
            resolve_cost_session_id(&sessions, "aaaa", SessionLookupKind::Prefix).unwrap_err();
        assert!(err.contains("Ambiguous"), "got: {}", err);
        assert!(err.contains("aaaa1111-"), "got: {}", err);
        assert!(err.contains("aaaa2222-"), "got: {}", err);
    }

    #[test]
    fn session_prefix_no_match_errors() {
        let sessions = fixtures();
        let err =
            resolve_cost_session_id(&sessions, "zzzzzzzz", SessionLookupKind::Prefix).unwrap_err();
        assert!(err.contains("No session matching"), "got: {}", err);
    }

    #[test]
    fn session_empty_errors() {
        let sessions = fixtures();
        let err = resolve_cost_session_id(&sessions, "", SessionLookupKind::Prefix).unwrap_err();
        assert!(err.contains("must not be empty"), "got: {}", err);
    }

    #[test]
    fn build_breakdown_sums_across_days() {
        let sessions = fixtures();
        let b = build_session_breakdown(
            &sessions,
            "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee",
            "claudeCode",
            None,
        )
        .unwrap();
        assert!((b.total_cost - 2.00).abs() < 1e-9);
        assert_eq!(b.total_tokens, 1500);
        assert_eq!(b.daily.len(), 2);
        assert_eq!(b.daily[0].date, "2026-04-10");
        assert_eq!(b.daily[1].date, "2026-04-11");
        assert_eq!(b.models, vec!["sonnet".to_string()]);
        assert_eq!(b.earliest, "2026-04-10T00:00:00Z");
        assert_eq!(b.latest, "2026-04-11T00:00:00Z");
    }

    #[test]
    fn build_breakdown_since_filter_drops_earlier_days() {
        let sessions = fixtures();
        let since = chrono::NaiveDate::parse_from_str("2026-04-11", "%Y-%m-%d").unwrap();
        let b = build_session_breakdown(
            &sessions,
            "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee",
            "claudeCode",
            Some(since),
        )
        .unwrap();
        assert_eq!(b.daily.len(), 1);
        assert_eq!(b.daily[0].date, "2026-04-11");
        assert!((b.total_cost - 0.75).abs() < 1e-9);
    }

    #[test]
    fn build_breakdown_empty_for_missing_session() {
        let sessions = fixtures();
        let err =
            build_session_breakdown(&sessions, "nonexistent", "claudeCode", None).unwrap_err();
        assert!(err.contains("No cost data"), "got: {}", err);
    }

    #[test]
    fn build_breakdown_keeps_provider_namespaces_separate() {
        let mut sessions = fixtures();
        let mut codex = sessions[0].clone();
        codex.provider = "codex".to_string();
        codex.cost = 99.0;
        codex.total_tokens = 99_000;
        sessions.push(codex);

        let claude = build_session_breakdown(
            &sessions,
            "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee",
            "claudeCode",
            None,
        )
        .unwrap();
        assert!((claude.total_cost - 2.0).abs() < 1e-9);
        assert_eq!(claude.total_tokens, 1_500);
        assert_eq!(claude.provider, "claudeCode");

        let codex = build_session_breakdown(
            &sessions,
            "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee",
            "codex",
            None,
        )
        .unwrap();
        assert!((codex.total_cost - 99.0).abs() < 1e-9);
        assert_eq!(codex.total_tokens, 99_000);
        assert_eq!(codex.provider, "codex");
    }

    #[test]
    fn json_output_shape_has_expected_fields() {
        let sessions = fixtures();
        let b = build_session_breakdown(
            &sessions,
            "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee",
            "claudeCode",
            None,
        )
        .unwrap();
        let v = breakdown_to_json(&b);
        for key in [
            "sessionId",
            "provider",
            "sessionName",
            "project",
            "projectName",
            "totalCost",
            "totalTokens",
            "turns",
            "models",
            "earliest",
            "latest",
            "byDay",
        ] {
            assert!(v.get(key).is_some(), "missing key {} in {}", key, v);
        }
        assert_eq!(v["sessionId"], "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee");
        assert_eq!(v["turns"], 2);
        let by_day = v["byDay"].as_array().unwrap();
        assert_eq!(by_day.len(), 2);
        for row in by_day {
            for key in ["date", "cost", "totalTokens", "model"] {
                assert!(row.get(key).is_some(), "missing {} in {}", key, row);
            }
        }
    }

    #[test]
    fn json_compact_is_single_line() {
        let sessions = fixtures();
        let b = build_session_breakdown(
            &sessions,
            "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee",
            "claudeCode",
            None,
        )
        .unwrap();
        let v = breakdown_to_json(&b);
        let compact = serde_json::to_string(&v).unwrap();
        assert!(
            !compact.contains('\n'),
            "compact output must be single-line: {}",
            compact
        );
        let parsed: serde_json::Value = serde_json::from_str(&compact).unwrap();
        assert_eq!(parsed["sessionId"], "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee");
    }

    #[test]
    fn json_pretty_is_indented_and_valid() {
        let sessions = fixtures();
        let b = build_session_breakdown(
            &sessions,
            "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee",
            "claudeCode",
            None,
        )
        .unwrap();
        let v = breakdown_to_json(&b);
        let pretty = serde_json::to_string_pretty(&v).unwrap();
        assert!(pretty.contains('\n'), "pretty output must be multi-line");
        assert!(pretty.contains("  "), "pretty output must be indented");
        let parsed: serde_json::Value = serde_json::from_str(&pretty).unwrap();
        assert_eq!(parsed["sessionId"], "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee");
        assert_eq!(parsed["byDay"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn clap_rejects_session_with_daily() {
        use clap::Parser;
        let res = Cli::try_parse_from(["c9watch", "cost", "--session", "aaaa1111-bbbb", "--daily"]);
        assert!(res.is_err(), "expected clap to reject --session + --daily");
    }

    #[test]
    fn clap_rejects_session_with_session_prefix() {
        use clap::Parser;
        let res = Cli::try_parse_from([
            "c9watch",
            "cost",
            "--session",
            "aaaa1111-bbbb",
            "--session-prefix",
            "aa",
        ]);
        assert!(
            res.is_err(),
            "expected clap to reject --session + --session-prefix"
        );
    }

    #[test]
    fn clap_accepts_session_alone() {
        use clap::Parser;
        let res = Cli::try_parse_from(["c9watch", "cost", "--session", "aaaa1111-bbbb"]);
        assert!(res.is_ok(), "parse failed: {:?}", res.err());
    }

    #[test]
    fn parse_since_accepts_none_and_iso_dates() {
        assert_eq!(parse_since(None).unwrap(), None);
        assert_eq!(
            parse_since(Some("2026-04-02")).unwrap(),
            Some(chrono::NaiveDate::from_ymd_opt(2026, 4, 2).unwrap())
        );
    }

    #[test]
    fn parse_since_keeps_the_cli_error_text() {
        let err = parse_since(Some("04/02/2026")).unwrap_err();
        let expected_cause = chrono::NaiveDate::parse_from_str("04/02/2026", "%Y-%m-%d")
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            format!("Invalid --since date '04/02/2026': {}", expected_cause)
        );
    }

    #[test]
    fn on_or_after_includes_the_boundary_and_rejects_bad_dates() {
        let since = chrono::NaiveDate::from_ymd_opt(2026, 4, 2).unwrap();
        assert!(!on_or_after(&rec("a", "2026-04-01", 0.0, 0, "m"), since));
        assert!(on_or_after(&rec("a", "2026-04-02", 0.0, 0, "m"), since));
        assert!(on_or_after(&rec("a", "2026-04-03", 0.0, 0, "m"), since));
        assert!(!on_or_after(&rec("a", "not-a-date", 0.0, 0, "m"), since));
    }
}
