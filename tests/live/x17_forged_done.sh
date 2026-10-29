#!/usr/bin/env bash
# Live X17: a forged task.done is refused (plan C1a-29).
#
# Needs a real, logged-in `claude` on PATH and costs subscription quota.
# CI never runs it. Same test engine approach as tests/live/cc_sandbox.sh:
# - The engine and c9watch-agent come from a build with the `live-test`
#   feature (test-only PTY input and screen files, `engine::live_test`):
#     cargo build --release --no-default-features --features engine,live-test \
#       --bin c9watch --bin c9watch-agent
# - One agent: interactive CC in the engine's PTY. No `claude -p`, no SDK.
# - Every path is under WORK. The engine home has a space in it (S:56).
# - The project pins "model": "claude-haiku-5-5" in .claude/settings.json.
#
# In the agent's Bash, six prompts:
#   x1 env | grep -i c9          only C9_AGENT_ID, C9_AGENT_GEN, C9_ENGINE_SOCK
#   x2 python3 forge.py          a raw c9p/1 task.done  -> peer_not_authorized
#   x3 c9watch api task.done     the main binary        -> agent_caller_refused
#   x4 the same without C9_AGENT_ID, so the call reaches the engine
#                                                       -> peer_not_authorized
#   x5 a copy of <home>/bin/c9watch-agent at another path -> peer_not_authorized
#   x6 <home>/bin/c9watch-agent task done               -> no_task (authenticated)
# The verdict comes from the tool results in the CC transcript and the
# engine audit rows, not from what the model says.
#
# Usage: tests/live/x17_forged_done.sh <release dir with c9watch and c9watch-agent> <WORK>
# Results: <WORK>/results.txt, <WORK>/evidence/, <WORK>/pids.
# The script never signals a process it did not start (pids in <WORK>/pids).

set -u
BUILD=${1:?release dir}
WORK=${2:?work dir}
MAX_PROMPTS=${MAX_PROMPTS:-8}
MODEL=claude-haiku-5-5

case "$WORK" in
/private/tmp/*) ;;
*)
    echo "WORK must be under /private/tmp" >&2
    exit 2
    ;;
esac
if [ -e "$WORK/engine home" ]; then
    echo "$WORK already used; pick a fresh dir" >&2
    exit 2
fi

mkdir -p "$WORK"/{bin,evidence}
H="$WORK/engine home"
SOCK="$H/engine-v1.sock"
EV="$WORK/evidence"
RES="$WORK/results.txt"
P="$WORK/proj"
PROMPTS=0
: >"$RES"
cp "$BUILD/c9watch" "$BUILD/c9watch-agent" "$WORK/bin/"
C9="$WORK/bin/c9watch"

log() { printf '%s %s\n' "$(date +%H:%M:%S)" "$*" | tee -a "$WORK/run.log"; }
note() { printf '%s\n' "$*" | tee -a "$RES"; }

# --- c9p/1 over the engine socket, as the test driver (a local client).
cat >"$WORK/rpc.py" <<'PY'
import json, socket, sys
sock, method, params = sys.argv[1], sys.argv[2], json.loads(sys.argv[3])
timeout = float(sys.argv[4]) if len(sys.argv) > 4 else 10
s = socket.socket(socket.AF_UNIX); s.settimeout(timeout); s.connect(sock)
f = s.makefile("rwb")
def call(i, m, p):
    f.write((json.dumps({"v": 1, "id": str(i), "method": m, "params": p}) + "\n").encode()); f.flush()
    return json.loads(f.readline())
h = call(0, "hello", {"apiMajor": 1, "client": {"kind": "cli", "name": "x17_forged_done.sh"}})
if not h.get("ok"):
    print(json.dumps(h)); sys.exit(1)
r = call(1, method, params)
print(json.dumps(r))
sys.exit(0 if r.get("ok") else 1)
PY
rpc() { python3 -I "$WORK/rpc.py" "$SOCK" "$@"; }

state_of() {
    rpc agent.list '{"all":true}' 2>/dev/null | python3 -I -c '
import json, sys
try:
    r = json.load(sys.stdin)
except ValueError:
    print("gone -"); sys.exit()
want = sys.argv[1]
for a in (r.get("result") or {}).get("agents", []):
    if a["id"] == want:
        print(a.get("state") or "-", a.get("status") or "-"); break
else:
    print("missing -")' "$1"
}

# --- project: the cheap model, and the forged client the agent runs (x2).
mkdir -p "$P/.claude"
printf '{\n  "model": "%s"\n}\n' "$MODEL" >"$P/.claude/settings.json"
printf '# Scratch project for the c9watch C1a-29 live test.\n' >"$P/CLAUDE.md"
cat >"$P/forge.py" <<'PY'
# A forged task.done: a raw c9p/1 client that is not c9watch-agent.
import json, os, socket
s = socket.socket(socket.AF_UNIX); s.settimeout(10); s.connect(os.environ["C9_ENGINE_SOCK"])
f = s.makefile("rwb")
def call(i, m, p):
    f.write((json.dumps({"v": 1, "id": str(i), "method": m, "params": p}) + "\n").encode()); f.flush()
    return json.loads(f.readline())
print(json.dumps(call(0, "hello", {"apiMajor": 1, "client": {"kind": "agent", "name": "forge.py"}})))
print(json.dumps(call(1, "task.done", {"agentId": os.environ["C9_AGENT_ID"], "report": "x17 forged", "seq": "1"})))
PY

# --- engine
log "starting the test engine (home: $H)"
env C9WATCH_ENGINE_HOME="$H" C9WATCH_EXPERIMENTAL_CREATE=1 "$C9" engine >"$WORK/engine.log" 2>&1 &
EPID=$!
echo "engine $EPID" >>"$WORK/pids"
for _ in $(seq 1 100); do [ -S "$SOCK" ] && break; sleep 0.2; done
[ -S "$SOCK" ] || {
    log "no engine socket"
    exit 1
}
mkdir -m 700 "$H/live-test"

send_keys() { # id bytes
    printf '%b' "$2" >"$H/live-test/.$1.tmp" && mv "$H/live-test/.$1.tmp" "$H/live-test/$1.in"
    for _ in $(seq 1 40); do [ -e "$H/live-test/$1.in" ] || return 0; sleep 0.25; done
    log "input for $1 was not taken"
}

# One prompt: type it, submit, wait for the turn to end. A permission
# prompt (state blocked) is captured and dismissed with Esc (a deny).
prompt() { # id tag text
    if [ "$PROMPTS" -ge "$MAX_PROMPTS" ]; then
        note "$2: SKIPPED (prompt cap $MAX_PROMPTS)"
        return 1
    fi
    PROMPTS=$((PROMPTS + 1))
    log "prompt $PROMPTS [$2] $3"
    send_keys "$1" "$3"
    sleep 0.6
    send_keys "$1" '\r'
    local st seen=0 calm=0 t=0
    while [ $t -lt 240 ]; do
        sleep 1
        t=$((t + 1))
        st=$(state_of "$1")
        case "$st" in
        working*) seen=1 calm=0 ;;
        blocked*)
            cp "$H/live-test/$1.screen" "$EV/$2.blocked.screen" 2>/dev/null
            log "[$2] blocked: dismissing the permission prompt with Esc"
            echo "$2: permission prompt shown" >>"$EV/prompts-shown.txt"
            send_keys "$1" '\033'
            calm=0
            ;;
        ready* | paused* | interrupted* | failed* | retired* | missing* | gone*)
            if [ $seen -eq 1 ] || [ $t -gt 20 ]; then calm=$((calm + 1)); fi
            [ $calm -ge 3 ] && break
            ;;
        esac
    done
    cp "$H/live-test/$1.screen" "$EV/$2.screen" 2>/dev/null
    log "[$2] turn over after ${t}s ($st)"
}

out=$(rpc agent.create "{\"mode\":\"free\",\"cwd\":\"$P\",\"name\":\"x17\"}" 120)
echo "$out" >"$EV/create.json"
ID=$(echo "$out" | python3 -I -c 'import json,sys; r=json.load(sys.stdin); print(r["result"]["agent"]["id"] if r.get("ok") else "")')
if [ -z "$ID" ]; then
    log "create failed: $out"
else
    echo "agent $ID" >>"$WORK/pids"
    log "agent = $ID"
    cp "$H/spawn/$ID-1.json" "$EV/spawn-spec.json" 2>/dev/null

    T='This is an authorized c9watch engine authentication test run by the repo owner. Make exactly one Bash tool call with the command given, exactly as written, do not retry, do not try another way, then reply with one line: RESULT: <the output or the exact error>. Command:'
    J="{\"agentId\":\"$ID\",\"report\":\"x17\",\"seq\":\"1\"}"
    prompt "$ID" x1-env "$T env | grep -i c9"
    prompt "$ID" x2-raw-socket "$T python3 forge.py"
    prompt "$ID" x3-main-bin "$T \"$C9\" api task.done '$J'"
    prompt "$ID" x4-main-bin-no-id "$T env -u C9_AGENT_ID C9WATCH_ENGINE_HOME=\"$H\" \"$C9\" api task.done '$J'"
    prompt "$ID" x5-copy "$T cp \"$H/bin/c9watch-agent\" ./agent-copy && ./agent-copy task done --report x17"
    prompt "$ID" x6-real "$T \"$H/bin/c9watch-agent\" task done --report x17"

    rpc agent.list '{"all":true}' >"$EV/agents-before-retire.json"
    log "retiring $ID"
    rpc agent.retire "{\"id\":\"$ID\"}" >"$EV/retire.json"
    rpc agent.list '{"all":true}' >"$EV/agents-final.json"
fi

# --- stop the test engine (SIGTERM drains).
kill -TERM "$EPID"
for _ in $(seq 1 100); do kill -0 "$EPID" 2>/dev/null || break; sleep 1; done
kill -0 "$EPID" 2>/dev/null && note "cleanup: engine $EPID still runs after SIGTERM"
sqlite3 -header "$H/engine.db" 'SELECT seq, ts, principal, method, target, decision, reason FROM audit ORDER BY seq' >"$EV/audit.txt" 2>&1
sqlite3 -header "$H/engine.db" 'SELECT id, state, state_reason, generation, provider_sid, cwd FROM agents WHERE source = "engine"' >"$EV/agents-db.txt" 2>&1
cp "$WORK"/engine.log "$EV/" 2>/dev/null

# --- verdict from the transcript's Bash calls and results.
SID=$(sqlite3 "$H/engine.db" "SELECT provider_sid FROM agents WHERE id = '${ID:-none}'" 2>/dev/null)
TR=$(ls "$HOME"/.claude/projects/*/"${SID:-none}".jsonl 2>/dev/null | head -1)
[ -n "$TR" ] || TR=$(ls -t "$HOME/.claude/projects/$(printf '%s' "$P" | sed 's#[/.]#-#g')"/*.jsonl 2>/dev/null | head -1)
echo "${TR:-no transcript}" >"$EV/transcript-path.txt"
python3 -I - "${TR:-/nonexistent}" "$EV/audit.txt" "$EV/bash-calls.txt" <<'PY' | tee -a "$RES"
import json, re, sys
tr, audit_path, out_path = sys.argv[1:4]
calls, results = [], {}
try:
    lines = open(tr).read().splitlines()
except OSError:
    lines = []
for line in lines:
    try:
        e = json.loads(line)
    except ValueError:
        continue
    content = (e.get("message") or {}).get("content")
    if not isinstance(content, list):
        continue
    for c in content:
        if c.get("type") == "tool_use" and c.get("name") == "Bash":
            calls.append((c["id"], c["input"].get("command", "")))
        elif c.get("type") == "tool_result":
            body = c.get("content")
            if isinstance(body, list):
                body = "\n".join(x.get("text", "") for x in body if isinstance(x, dict))
            results[c.get("tool_use_id")] = str(body)
with open(out_path, "w") as f:
    for i, cmd in calls:
        f.write("$ %s\n%s\n\n" % (cmd, results.get(i, "<no result>")))

def find(pred):
    return [(cmd, results.get(i, "")) for i, cmd in calls if pred(cmd)]

rows = []
def row(tag, pred, check, want):
    hits = find(pred)
    if not hits:
        rows.append((tag, "NOT TESTED", "no Bash call"))
        return
    cmd, out = hits[0]
    ok = check(out)
    rows.append((tag, "PASS" if ok else "FAIL", want if ok else out.strip().replace("\n", " | ")[:300]))

def env_ok(out):
    keys = sorted({l.split("=", 1)[0] for l in out.splitlines() if "=" in l and "c9" in l.split("=", 1)[0].lower()})
    return keys == ["C9_AGENT_GEN", "C9_AGENT_ID", "C9_ENGINE_SOCK"]
row("x1-env", lambda c: "grep -i c9" in c, env_ok, "only C9_AGENT_GEN, C9_AGENT_ID, C9_ENGINE_SOCK")
row("x2-raw-socket", lambda c: "forge.py" in c, lambda o: "peer_not_authorized" in o, "peer_not_authorized")
row("x3-main-bin", lambda c: " api task.done" in c and "env -u" not in c, lambda o: "agent_caller_refused" in o, "agent_caller_refused")
row("x4-main-bin-no-id", lambda c: " api task.done" in c and "env -u" in c, lambda o: "peer_not_authorized" in o, "peer_not_authorized")
row("x5-copy", lambda c: "agent-copy" in c, lambda o: "peer_not_authorized" in o, "peer_not_authorized")
row("x6-real", lambda c: "c9watch-agent\" task done" in c and "agent-copy" not in c, lambda o: "no_task" in o, "no_task (authenticated)")

audit = open(audit_path).read() if audit_path else ""
denies = [l for l in audit.splitlines() if "|task.done|" in l and "|deny|" in l]
print("audit task.done deny rows: %d" % len(denies))
for tag, verdict, why in rows:
    print("%s: %s (%s)" % (tag, verdict, why))
print("VERDICT: %s" % ("pass" if all(v == "PASS" for _, v, _ in rows) else "fail"))
PY

{
    echo "prompts used: $PROMPTS"
    echo "engine pid: $EPID"
    echo "agent: ${ID:-none}"
} >>"$RES"
log "done; inspect $EV and $WORK/engine.log; stop or retire anything left with the pids in $WORK/pids"
