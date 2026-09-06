//! Read-only subscription snapshots. Never infer quotas from token or cost totals.
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    process::Stdio,
    sync::OnceLock,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::Mutex,
};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    label: String,
    used_percent: f64,
    resets_at: Option<i64>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionUsage {
    provider: String,
    name: String,
    plan: Option<String>,
    windows: Vec<UsageWindow>,
    updated_at: Option<i64>,
    message: Option<String>,
}
fn unavailable(provider: &str, name: &str, message: &str) -> SubscriptionUsage {
    SubscriptionUsage {
        provider: provider.into(),
        name: name.into(),
        plan: None,
        windows: vec![],
        updated_at: None,
        message: Some(message.into()),
    }
}
fn parse_snapshot(value: &Value) -> SubscriptionUsage {
    let mut usage = unavailable(
        "codex",
        "Codex",
        "No subscription limits returned. Sign in to Codex with your ChatGPT account.",
    );
    let Some(snapshot) = value.get("rateLimits") else {
        return usage;
    };
    usage.plan = snapshot
        .get("planType")
        .and_then(Value::as_str)
        .map(str::to_owned);
    for (key, fallback) in [
        ("primary", "Primary window"),
        ("secondary", "Secondary window"),
    ] {
        let window = &snapshot[key];
        let Some(percent) = window["usedPercent"]
            .as_f64()
            .filter(|v| v.is_finite() && *v >= 0.0)
        else {
            continue;
        };
        let label = match window["windowDurationMins"].as_u64() {
            Some(10080) => "Weekly".into(),
            Some(mins) if mins > 0 && mins % 1440 == 0 => format!("{}-day", mins / 1440),
            Some(mins) if mins > 0 && mins % 60 == 0 => format!("{}-hour", mins / 60),
            Some(mins) if mins > 0 => format!("{mins}-minute"),
            _ => fallback.into(),
        };
        usage.windows.push(UsageWindow {
            label,
            used_percent: percent,
            resets_at: window["resetsAt"].as_i64(),
        });
    }
    if !usage.windows.is_empty() {
        usage.updated_at = Some(chrono::Utc::now().timestamp());
        usage.message = None;
    }
    usage
}
async fn read_codex() -> Result<SubscriptionUsage, String> {
    // GUI launches may have a minimal PATH. Prefer the user's installed CLI.
    let home = dirs::home_dir().ok_or("Home directory unavailable")?;
    let executable = [
        home.join(".local/bin/codex"),
        std::path::PathBuf::from("/opt/homebrew/bin/codex"),
        std::path::PathBuf::from("/usr/local/bin/codex"),
    ]
    .into_iter()
    .find(|p| p.is_file())
    .unwrap_or_else(|| "codex".into());
    let mut child = tokio::process::Command::new(executable)
        .arg("app-server")
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Could not start quota reader ({:?})", e.kind()))?;
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let mut stdin = child.stdin.take().ok_or("Quota reader stdin unavailable")?;
        let mut lines = BufReader::new(child.stdout.take().ok_or("Quota reader stdout unavailable")?).lines();
        let init = json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"c9watch_usage","version":"0.10.0"}}});
        stdin.write_all(format!("{init}\n").as_bytes()).await.map_err(|e| format!("Quota reader I/O failed ({e})"))?;
        while let Some(line) = lines.next_line().await.map_err(|e| format!("Quota reader I/O failed ({e})"))? {
            let Ok(message) = serde_json::from_str::<Value>(&line) else { continue; };
            match message["id"].as_u64() {
                Some(1) => {
                    if message.get("error").is_some() { return Err("Codex initialization rejected".into()); }
                    stdin.write_all(b"{\"method\":\"initialized\"}\n{\"id\":2,\"method\":\"account/rateLimits/read\"}\n").await.map_err(|e| format!("Quota reader I/O failed ({e})"))?;
                }
                Some(2) => return message.get("result").map(parse_snapshot).ok_or_else(|| format!("Codex quota request rejected (code {})", message["error"]["code"])),
                _ => {}
            }
        }
        Err("Codex reader exited before returning quota".into())
    }).await.unwrap_or_else(|_| Err("Codex quota request timed out".into()));
    let _ = child.kill().await;
    let _ = child.wait().await;
    result
}

/// Cursor's installed dashboard client uses this read-only Connect RPC.
/// Credentials stay in the backend and are sent only to Cursor's fixed HTTPS origin.
async fn read_cursor() -> Result<SubscriptionUsage, String> {
    let (token, plan) = tokio::task::spawn_blocking(|| {
        let home = dirs::home_dir().ok_or("Home directory unavailable")?;
        #[cfg(target_os = "macos")]
        let database =
            home.join("Library/Application Support/Cursor/User/globalStorage/state.vscdb");
        #[cfg(target_os = "linux")]
        let database = home.join(".config/Cursor/User/globalStorage/state.vscdb");
        #[cfg(target_os = "windows")]
        let database = dirs::config_dir()
            .ok_or("Config directory unavailable")?
            .join("Cursor/User/globalStorage/state.vscdb");
        let db = rusqlite::Connection::open_with_flags(
            database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|e| format!("Quota reader I/O failed ({e})"))?;
        db.busy_timeout(Duration::from_millis(500))
            .map_err(|e| format!("Quota reader I/O failed ({e})"))?;
        let token: String = db
            .query_row(
                "SELECT value FROM ItemTable WHERE key = 'cursorAuth/accessToken'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| format!("Quota reader I/O failed ({e})"))?;
        let plan: Option<String> = db
            .query_row(
                "SELECT value FROM ItemTable WHERE key = 'cursorAuth/stripeMembershipType'",
                [],
                |r| r.get(0),
            )
            .ok();
        Ok::<_, String>((token, plan))
    })
    .await
    .map_err(|e| format!("Quota reader I/O failed ({e})"))??;
    // Reject config/header injection; Cursor access tokens use the JWT alphabet.
    if token.is_empty()
        || !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err("Cursor login token is unavailable or invalid".into());
    }
    let mut child = tokio::process::Command::new(if cfg!(target_os = "macos") {
        "/usr/bin/curl"
    } else {
        "curl"
    })
    .args([
        "--disable",
        "--config",
        "-",
        "--silent",
        "--write-out",
        "\n%{http_code}",
        "--max-time",
        "12",
        "--max-filesize",
        "1048576",
        "--proto",
        "=https",
        "--request",
        "POST",
        "--data",
        "{}",
        "https://api2.cursor.sh/aiserver.v1.DashboardService/GetCurrentPeriodUsage",
    ])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .kill_on_drop(true)
    .spawn()
    .map_err(|e| format!("Could not start quota reader ({:?})", e.kind()))?;
    let mut stdin = child.stdin.take().ok_or("Quota reader stdin unavailable")?;
    let config = format!("header = \"Authorization: Bearer {token}\"\nheader = \"Content-Type: application/json\"\nheader = \"Connect-Protocol-Version: 1\"\n");
    stdin
        .write_all(config.as_bytes())
        .await
        .map_err(|e| format!("Quota reader I/O failed ({e})"))?;
    drop(stdin);
    let output = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
        .await
        .map_err(|e| format!("Quota reader I/O failed ({e})"))?
        .map_err(|e| format!("Quota reader I/O failed ({e})"))?;
    if !output.status.success() {
        return Err(format!(
            "Cursor connection failed (curl exit {})",
            output.status.code().unwrap_or(-1)
        ));
    }
    let body =
        std::str::from_utf8(&output.stdout).map_err(|_| "Cursor response was not valid text")?;
    let (body, status) = body
        .rsplit_once('\n')
        .ok_or("Cursor HTTP status unavailable")?;
    if status != "200" {
        return Err(match status {
            "401" | "403" => {
                "Cursor sign-in expired or access denied. Open Cursor to sign in again.".into()
            }
            "429" => "Cursor temporarily rate-limited quota checks. Retrying automatically.".into(),
            _ => format!("Cursor quota service returned HTTP {status}"),
        });
    }
    let value: Value =
        serde_json::from_str(body).map_err(|_| "Cursor quota response format changed")?;
    Ok(parse_cursor_snapshot(&value, plan))
}

fn parse_cursor_snapshot(value: &Value, plan: Option<String>) -> SubscriptionUsage {
    let mut usage = unavailable(
        "cursor",
        "Cursor",
        "Cursor returned no subscription quota. Check your plan in Cursor settings.",
    );
    usage.plan = plan;
    // Cursor billing timestamps are milliseconds, unlike Codex's seconds.
    let resets_at = value["billingCycleEnd"]
        .as_i64()
        .or_else(|| value["billingCycleEnd"].as_str()?.parse::<i64>().ok())
        .filter(|n| *n > 0)
        .map(|n| n / 1000);
    let plan_usage = &value["planUsage"];
    let mut add = |label: &str, percent: Option<f64>| {
        if let Some(used_percent) = percent.filter(|v| v.is_finite() && *v >= 0.0) {
            usage.windows.push(UsageWindow {
                label: label.into(),
                used_percent,
                resets_at,
            });
        }
    };
    // Prefer provider percentages: legacy spend totals can include bonus usage
    // and do not describe the newer separate quota pools.
    let total = plan_usage["totalPercentUsed"].as_f64().or_else(|| {
        let limit = plan_usage["limit"].as_f64().filter(|v| *v > 0.0)?;
        Some(plan_usage["includedSpend"].as_f64().unwrap_or(0.0) / limit * 100.0)
    });
    add("Included usage", total);
    add("Auto", plan_usage["autoPercentUsed"].as_f64());
    add("API", plan_usage["apiPercentUsed"].as_f64());
    let on_demand = &value["spendLimitUsage"];
    if let Some(limit) = on_demand["individualLimit"].as_f64().filter(|v| *v > 0.0) {
        add(
            "On-demand spend",
            Some(on_demand["individualUsed"].as_f64().unwrap_or(0.0) / limit * 100.0),
        );
    }
    if !usage.windows.is_empty() {
        usage.updated_at = Some(chrono::Utc::now().timestamp());
        usage.message = None;
    }
    usage
}

fn resolve_snapshot(
    provider: &str,
    name: &str,
    result: Result<SubscriptionUsage, String>,
    previous: &[SubscriptionUsage],
) -> SubscriptionUsage {
    match result {
        Ok(snapshot) if !snapshot.windows.is_empty() || provider == "claudeCode" => snapshot,
        other => {
            let message = match other {
                Ok(snapshot) => snapshot
                    .message
                    .unwrap_or_else(|| "No quota returned".into()),
                Err(message) => message,
            };
            crate::debug_log::log_warn(&format!("[subscription-usage] {provider}: {message}"));
            if let Some(last) = previous
                .iter()
                .find(|p| p.provider == provider && !p.windows.is_empty())
            {
                let mut stale = last.clone();
                stale.message = Some(format!("Last known usage · {message}"));
                stale
            } else {
                unavailable(provider, name, &message)
            }
        }
    }
}

fn parse_claude_snapshot(value: &Value, now: i64) -> SubscriptionUsage {
    let mut usage = unavailable("claudeCode", "Claude Code", "No subscription limits reported. Requires an eligible Claude plan and a response in Claude Code.");
    let Some(updated) = value["updatedAt"].as_i64().filter(|t| *t > 0 && *t <= now + 60) else { return usage; };
    if value["schemaVersion"] != 1 { return usage; }
    usage.updated_at = Some(updated);
    let clean = crate::claude_usage::sanitize(value, now);
    for (key, label) in [("five_hour", "5-hour"), ("seven_day", "Weekly"), ("spend_limit", "Spend limit")] {
        let w = &clean["rate_limits"][key];
        if let Some(percent) = w["used_percentage"].as_f64() {
            usage.windows.push(UsageWindow { label: label.into(), used_percent: percent, resets_at: w["resets_at"].as_i64() });
        }
    }
    if !usage.windows.is_empty() {
        usage.message = if now - updated > 180 { Some("Last reported by Claude Code. Open a session to update usage.".into()) } else { None };
    }
    usage
}
async fn read_claude() -> Result<SubscriptionUsage, String> {
    use tokio::io::AsyncReadExt;
    let path = crate::claude_usage::cache_path()?;
    let file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(unavailable("claudeCode", "Claude Code", "Usage is not connected. Enable the Claude Code status-line bridge to show subscription limits.")),
        Err(_) => return Err("Cannot read Claude Code usage snapshot".into()),
    };
    let mut bytes = Vec::new();
    file.take(16_385).read_to_end(&mut bytes).await.map_err(|_| "Cannot read Claude usage")?;
    if bytes.len() > 16_384 { return Err("Claude usage snapshot is too large".into()); }
    let value = serde_json::from_slice(&bytes).map_err(|_| "Invalid Claude usage snapshot")?;
    Ok(parse_claude_snapshot(&value, chrono::Utc::now().timestamp()))
}

/// Shared cache coalesces desktop/mobile polling and bounds subprocess creation.
pub async fn get_subscription_usage() -> Vec<SubscriptionUsage> {
    type Cache = Option<(Instant, Vec<SubscriptionUsage>)>;
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(|| Mutex::new(None)).lock().await;
    if let Some((at, data)) = cache.as_ref() {
        if at.elapsed() < Duration::from_secs(55) {
            return data.clone();
        }
    }
    let (claude, codex, cursor) = tokio::join!(read_claude(), read_codex(), read_cursor());
    let previous = cache
        .as_ref()
        .map(|(_, entries)| entries.as_slice())
        .unwrap_or(&[]);
    let data = [("claudeCode", "Claude Code", claude), ("codex", "Codex", codex), ("cursor", "Cursor", cursor)]
        .into_iter()
        .map(|(provider, name, result)| resolve_snapshot(provider, name, result, previous))
        .collect::<Vec<_>>();
    *cache = Some((Instant::now(), data.clone()));
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_windows_without_assuming_plan_limits() {
        let usage = parse_snapshot(
            &json!({"rateLimits":{"planType":"pro","primary":{"usedPercent":24.5,"windowDurationMins":300,"resetsAt":1800000000},"secondary":{"usedPercent":103,"windowDurationMins":10080}}}),
        );
        assert_eq!(usage.windows.len(), 2);
        assert_eq!(usage.windows[0].label, "5-hour");
        assert_eq!(usage.windows[0].used_percent, 24.5);
        assert_eq!(usage.windows[1].used_percent, 103.0);
        assert_eq!(usage.windows[1].label, "Weekly");
        assert!(usage.message.is_none());
    }
    #[test]
    fn missing_and_invalid_usage_are_never_zero_percent() {
        for value in [
            json!({}),
            json!({"rateLimits":{"primary":{"usedPercent":-1}}}),
            json!({"rateLimits":{"primary":null,"secondary":null}}),
        ] {
            let usage = parse_snapshot(&value);
            assert!(usage.windows.is_empty());
            assert!(usage.updated_at.is_none());
            assert!(usage.message.is_some());
        }
    }
    #[test]
    fn cursor_prefers_quota_pools_over_bonus_spend() {
        let usage = parse_cursor_snapshot(
            &json!({"billingCycleEnd":"1789396090000", "planUsage": {
            "includedSpend":2000, "bonusSpend":16000, "limit":2000,
            "totalPercentUsed":36.9, "autoPercentUsed":40.6, "apiPercentUsed":0
        }, "spendLimitUsage":{"individualLimit":500}}),
            Some("pro".into()),
        );
        assert_eq!(usage.windows.len(), 4);
        assert_eq!(usage.windows[0].used_percent, 36.9);
        assert_eq!(usage.windows[1].used_percent, 40.6);
        assert_eq!(usage.windows[2].used_percent, 0.0);
        assert_eq!(usage.windows[0].resets_at, Some(1789396090));
        assert_eq!(usage.windows[3].used_percent, 0.0);
    }
    #[test]
    fn cursor_legacy_and_unavailable_limits() {
        let usage = parse_cursor_snapshot(
            &json!({"planUsage":{"includedSpend":1000,"limit":2000}}),
            None,
        );
        assert_eq!(usage.windows[0].used_percent, 50.0);
        assert!(parse_cursor_snapshot(&json!({}), None).windows.is_empty());
        assert!(
            parse_cursor_snapshot(&json!({"planUsage":{"limit":0}}), None)
                .windows
                .is_empty()
        );
    }
    #[test]
    fn transient_failure_preserves_only_matching_last_known_usage() {
        let last = parse_cursor_snapshot(&json!({"planUsage":{"totalPercentUsed":41}}), None);
        let updated_at = last.updated_at;
        let stale = resolve_snapshot("cursor", "Cursor", Err("network timeout".into()), &[last]);
        assert_eq!(stale.windows[0].used_percent, 41.0);
        assert_eq!(stale.updated_at, updated_at);
        assert!(stale
            .message
            .as_deref()
            .unwrap()
            .contains("network timeout"));
        let codex = resolve_snapshot(
            "codex",
            "Codex",
            Err("reader exited".into()),
            &[stale.clone()],
        );
        assert!(codex.windows.is_empty());
        let fresh = parse_cursor_snapshot(&json!({"planUsage":{"totalPercentUsed":42}}), None);
        let recovered = resolve_snapshot("cursor", "Cursor", Ok(fresh), &[stale]);
        assert_eq!(recovered.windows[0].used_percent, 42.0);
        assert!(recovered.message.is_none());
    }
    #[test]
    fn claude_snapshot_handles_partial_stale_reset_and_unavailable_data() {
        let input = json!({"rate_limits":{"five_hour":{"used_percentage":0,"resets_at":2000},"seven_day":{"used_percentage":65.5,"resets_at":4000}}});
        let saved = crate::claude_usage::sanitize(&input, 1000);
        let fresh = parse_claude_snapshot(&saved, 1010);
        assert_eq!(fresh.windows.len(), 2);
        assert_eq!(fresh.windows[0].used_percent, 0.0);
        assert!(fresh.message.is_none());
        assert!(parse_claude_snapshot(&saved, 1300).message.is_some());
        let partial = parse_claude_snapshot(&saved, 2500);
        assert_eq!(partial.windows.len(), 1);
        assert_eq!(partial.windows[0].label, "Weekly");
        assert!(parse_claude_snapshot(&saved, 4500).windows.is_empty());
        assert!(parse_claude_snapshot(&saved, 10).windows.is_empty());
        assert!(parse_claude_snapshot(&json!({"schemaVersion":1,"updatedAt":1000,"rate_limits":{}}), 1010).windows.is_empty());
    }
    #[test]
    fn claude_authoritative_empty_snapshot_clears_previous_windows() {
        let saved = crate::claude_usage::sanitize(&json!({"rate_limits":{"five_hour":{"used_percentage":60,"resets_at":2000}}}), 1000);
        let previous = parse_claude_snapshot(&saved, 1010);
        let expired = parse_claude_snapshot(&saved, 2100);
        let resolved = resolve_snapshot("claudeCode", "Claude Code", Ok(expired), &[previous]);
        assert!(resolved.windows.is_empty());
    }
    #[tokio::test]
    #[ignore = "requires installed and authenticated Cursor"]
    async fn live_cursor_snapshot() {
        let usage = read_cursor().await.expect("Cursor quota request failed");
        assert!(!usage.windows.is_empty());
        println!("{} Cursor quota windows received", usage.windows.len());
    }
    #[tokio::test]
    #[ignore = "requires installed and authenticated Codex CLI"]
    async fn live_codex_snapshot() {
        let usage = read_codex()
            .await
            .expect("Codex account/rateLimits/read failed");
        assert!(!usage.windows.is_empty(), "No authenticated quota windows");
        println!("{} quota windows received", usage.windows.len());
    }
}
