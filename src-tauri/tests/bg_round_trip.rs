//! End-to-end smoke test for the `claude --bg` + `control.sock` JSON-RPC stack.
//!
//! Requires `claude >= 2.1.150` on PATH. Runs only via:
//!     cargo test --test bg_round_trip -- --ignored --nocapture
//!
//! Validates: spawn → subscribe → snapshot → turn-1 settled → reply →
//! turn-2 settled → claude stop → claude rm → no residue.
//!
//! Safety: spawns in /tmp, names sessions `c9watch-bg-poc-<pid>`, and a
//! Drop guard always runs `claude stop` + `claude rm` even on panic.

use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use regex::Regex;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::{timeout, Instant};

const SETTLE_TIMEOUT: Duration = Duration::from_secs(25);
const TOTAL_BUDGET: Duration = Duration::from_secs(55);
const QUIET_WINDOW: Duration = Duration::from_millis(1500);

/// RAII guard: stop + rm the bg session on Drop unless disarmed.
struct StopGuard {
    short: String,
    disarmed: bool,
}
impl StopGuard {
    fn new(short: String) -> Self {
        Self {
            short,
            disarmed: false,
        }
    }
    fn disarm(&mut self) {
        self.disarmed = true;
    }
}
impl Drop for StopGuard {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }
        eprintln!("[guard] cleaning up {}", self.short);
        let _ = Command::new("claude").arg("stop").arg(&self.short).status();
        let _ = Command::new("claude").arg("rm").arg(&self.short).status();
    }
}

/// End-to-end smoke test for the `claude --bg` + control.sock JSON-RPC stack.
///
/// Requires `claude >= 2.1.150` on PATH. Runs only via:
///     cargo test --test bg_round_trip -- --ignored --nocapture
///
/// Validates: spawn → subscribe → snapshot → turn-1 settled → reply →
/// turn-2 settled → claude stop → claude rm → no residue.
#[tokio::test]
#[ignore]
async fn bg_round_trip_end_to_end() -> anyhow::Result<()> {
    let started = std::time::Instant::now();
    tokio::time::timeout(TOTAL_BUDGET, bg_round_trip_inner())
        .await
        .map_err(|_| anyhow::anyhow!("{:?} budget exceeded", TOTAL_BUDGET))??;
    println!("[done] total {:.2}s", started.elapsed().as_secs_f32());
    Ok(())
}

async fn bg_round_trip_inner() -> Result<()> {
    let socket = resolve_socket().context("step 1: resolve daemon socket")?;
    println!("[socket] {}", socket);

    let name = format!("c9watch-bg-poc-{}", std::process::id());
    let (short, model) = spawn_bg(&name).context("step 2: spawn `claude --bg`")?;
    println!("[spawn] short={} name={} model={}", short, name, model);

    assert!(short.len() == 8, "expected 8-hex short, got {}", short);
    assert!(
        short.chars().all(|c| c.is_ascii_hexdigit()),
        "short must be hex, got {}",
        short
    );

    let mut guard = StopGuard::new(short.clone());

    let stream = UnixStream::connect(&socket)
        .await
        .with_context(|| format!("step 3: connect to {}", socket))?;
    let (rd, mut wr) = stream.into_split();
    let mut reader = BufReader::new(rd).lines();
    println!("[connect] unix stream open");

    write_line(&mut wr, &json!({"proto":1,"op":"subscribe","short":short}))
        .await
        .context("step 4: write subscribe")?;
    println!("[subscribe] sent");

    drain_initial_snapshot(&mut reader)
        .await
        .context("step 4a: drain snapshot")?;

    // Turn 1: the prompt we passed at spawn. Subscribing here catches the
    // mid-flight active→idle state patch reliably (verified manually).
    let d1 = wait_for_idle(&mut reader, &short, "turn-1")
        .await
        .context("step 5: wait for turn-1 idle")?;
    println!("[turn-1 detail] {:?}", d1);

    // Turn 2: send via the `reply` verb. Field is `text` (not message/prompt).
    write_line(
        &mut wr,
        &json!({
            "proto":1, "op":"reply", "short":short,
            "text":"now say DONE in all caps and nothing else"
        }),
    )
    .await
    .context("step 6: write turn-2 reply")?;
    println!("[send] turn-2 reply sent");

    let d2 = wait_for_idle(&mut reader, &short, "turn-2")
        .await
        .context("step 7: wait for turn-2 idle")?;
    println!("[turn-2 detail] {:?}", d2);

    let stop = Command::new("claude")
        .arg("stop")
        .arg(&short)
        .status()
        .context("step 8a: `claude stop`")?;
    println!("[stop] exit={}", stop.code().unwrap_or(-1));
    assert_eq!(stop.code().unwrap_or(-1), 0, "claude stop failed");

    let rm = Command::new("claude")
        .arg("rm")
        .arg(&short)
        .status()
        .context("step 8b: `claude rm`")?;
    println!("[rm] exit={}", rm.code().unwrap_or(-1));
    assert_eq!(rm.code().unwrap_or(-1), 0, "claude rm failed");

    if let Some(s) = current_status(&short)? {
        bail!("step 9: session {} still listed with status={}", short, s);
    }
    guard.disarm();
    println!("[verify] session gone");
    Ok(())
}

async fn write_line<W>(w: &mut W, v: &Value) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    w.write_all(format!("{}\n", v).as_bytes()).await?;
    Ok(())
}

fn resolve_socket() -> Result<String> {
    let uid = unsafe { libc::getuid() };
    let parent = format!("/tmp/cc-daemon-{}", uid);
    let mut hits = Vec::new();
    for e in std::fs::read_dir(&parent)
        .with_context(|| format!("read_dir {} — is CC daemon running?", parent))?
        .flatten()
    {
        let p = e.path().join("control.sock");
        if p.exists() {
            hits.push(p.to_string_lossy().into_owned());
        }
    }
    match hits.len() {
        0 => bail!("no control.sock under {} (CC >= 2.1.145 required)", parent),
        1 => Ok(hits.remove(0)),
        n => bail!(
            "expected 1 host-id subdir under {}, found {}: {:?}",
            parent,
            n,
            hits
        ),
    }
}

/// Spawn `claude --bg --name <name> --model <m> "<prompt>"` in /tmp.
/// Tries haiku, then sonnet. Empirically (CC 2.1.150) spawning idle and
/// then sending the first `reply` over control.sock does NOT trigger the
/// worker reliably — spawning with a prompt gives a known turn-1.
fn spawn_bg(name: &str) -> Result<(String, String)> {
    let prompt = "respond with the single word: ok";
    for model in ["haiku", "sonnet"] {
        let out = Command::new("claude")
            .args(["--bg", "--name", name, "--model", model, prompt])
            .current_dir("/tmp")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| format!("spawn claude --bg --model {}", model))?;
        if !out.status.success() {
            eprintln!(
                "[spawn] model={} rejected: {}",
                model,
                String::from_utf8_lossy(&out.stderr).trim()
            );
            continue;
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        let short = parse_short(&stdout)
            .with_context(|| format!("parse short (model={}):\n{}", model, stdout))?;
        return Ok((short, model.to_string()));
    }
    bail!("no model accepted (tried haiku, sonnet)")
}

fn parse_short(stdout: &str) -> Result<String> {
    // "backgrounded · <8hex> [· name]" or with "(idle ...)" suffix.
    let re = Regex::new(r"backgrounded\s*[·•]\s*([0-9a-f]{8})").unwrap();
    stdout
        .lines()
        .find_map(|l| re.captures(l).map(|c| c[1].to_string()))
        .ok_or_else(|| anyhow!("no `backgrounded · <short>` line found"))
}

async fn drain_initial_snapshot<R>(reader: &mut tokio::io::Lines<BufReader<R>>) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let rem = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| anyhow!("snapshot not received within 5s"))?;
        let line = match timeout(rem, reader.next_line()).await {
            Ok(Ok(Some(l))) => l,
            Ok(Ok(None)) => bail!("EOF before snapshot"),
            Ok(Err(e)) => bail!("read error: {}", e),
            Err(_) => bail!("snapshot not received within 5s"),
        };
        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("type").and_then(|x| x.as_str()) == Some("snapshot") {
            let tempo = v
                .pointer("/record/tempo")
                .and_then(|x| x.as_str())
                .unwrap_or("?");
            let state = v
                .pointer("/record/state")
                .and_then(|x| x.as_str())
                .unwrap_or("?");
            println!("[event] initial snapshot tempo={} state={}", tempo, state);
            return Ok(());
        }
    }
}

/// Race three turn-end signals; whichever fires first wins.
/// 1. `state` patch with done/blocked/idle (push subscription)
/// 2. Stream silence after PTY activity (also via subscription)
/// 3. `claude agents --json` poll: busy→idle transition (out-of-band)
async fn wait_for_idle<R>(
    reader: &mut tokio::io::Lines<BufReader<R>>,
    short: &str,
    label: &str,
) -> Result<Option<String>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    tokio::select! {
        r = stream_or_state(reader, label) => r,
        r = poll_idle(short, label) => r,
    }
}

/// Drives the reader: returns on settled state patch OR stream-quiet.
async fn stream_or_state<R>(
    reader: &mut tokio::io::Lines<BufReader<R>>,
    label: &str,
) -> Result<Option<String>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut saw_stream = false;
    let mut frames = 0u32;
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        if Instant::now() >= deadline {
            bail!("{}: stream_or_state timeout", label);
        }
        match timeout(QUIET_WINDOW, reader.next_line()).await {
            Ok(Ok(Some(line))) => {
                let v: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                match v.get("type").and_then(|x| x.as_str()) {
                    Some("stream") => {
                        if !saw_stream {
                            println!("[stream] {} first frame seen", label);
                        }
                        saw_stream = true;
                        frames += 1;
                    }
                    Some("state") => {
                        let p = v.get("patch").cloned().unwrap_or(Value::Null);
                        let st = p.get("state").and_then(|x| x.as_str());
                        let te = p.get("tempo").and_then(|x| x.as_str());
                        if matches!(st, Some("done") | Some("blocked"))
                            || matches!(te, Some("idle") | Some("blocked"))
                        {
                            let d = p
                                .get("detail")
                                .and_then(|x| x.as_str())
                                .map(|s| s.to_string());
                            println!(
                                "[{} complete via state] state={:?} tempo={:?}",
                                label, st, te
                            );
                            return Ok(d);
                        }
                    }
                    _ => {}
                }
            }
            Ok(Ok(None)) => bail!("{}: EOF on control.sock", label),
            Ok(Err(e)) => bail!("{}: read error: {}", label, e),
            Err(_) => {
                if saw_stream {
                    println!(
                        "[{} complete via stream-quiet] frames={} silence={:?}",
                        label, frames, QUIET_WINDOW
                    );
                    return Ok(None);
                }
            }
        }
    }
}

/// Poll `claude agents --json` every 250ms; require a busy→idle transition.
async fn poll_idle(short: &str, label: &str) -> Result<Option<String>> {
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    let mut last = String::new();
    let mut saw_busy = false;
    loop {
        if Instant::now() >= deadline {
            bail!("{}: poll_idle timeout (last={})", label, last);
        }
        let status = current_status(short)?;
        let disp = status.clone().unwrap_or_else(|| "<absent>".into());
        if disp != last {
            println!("[poll] {} status={}", label, disp);
            last = disp.clone();
        }
        match status.as_deref() {
            Some("busy") | Some("running") | Some("active") => saw_busy = true,
            Some("idle") | Some("blocked") | Some("done") if saw_busy => {
                println!("[{} complete via poll] status={}", label, disp);
                return Ok(None);
            }
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Returns `status` field of the agents entry whose sessionId starts with `short`.
fn current_status(short: &str) -> Result<Option<String>> {
    let out = Command::new("claude")
        .args(["agents", "--json"])
        .output()
        .context("run `claude agents --json`")?;
    if !out.status.success() {
        bail!(
            "claude agents --json failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let arr: Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).context("parse agents json")?;
    Ok(arr
        .as_array()
        .and_then(|a| {
            a.iter().find(|e| {
                e.get("sessionId")
                    .and_then(|x| x.as_str())
                    .map(|s| s.starts_with(short))
                    .unwrap_or(false)
            })
        })
        .and_then(|e| {
            e.get("status")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())
        }))
}
