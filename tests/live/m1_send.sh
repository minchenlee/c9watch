#!/usr/bin/env bash
# Live C1b-M1-14: `c9watch send` mails real CC agents (plan-m1 M1-14).
#
# Needs a real, logged-in `claude` on PATH and costs subscription quota.
# CI never runs it. It drives a private test engine only:
# - The engine, the CLI and c9watch-agent come from an `engine` build (no
#   `live-test`: input goes through msg.send):
#     cargo build --no-default-features --features engine \
#       --bin c9watch --bin c9watch-agent
# - Agents A and B are interactive CC in the engine's PTY, both in one cwd
#   (the engine allows one cwd at a time). No `claude -p`, no SDK.
# - Every path is under WORK. The engine home has a space in it (S:56).
# - The project pins "model": "claude-haiku-5-5" in .claude/settings.json.
#
# Steps (each checks the transcript and the messages table, not what the
# model says):
#   s3 terminal -> A, --wait: A answers the nonce; detail prompt_hook; the
#      prompt CC got holds the fence with the msg id
#   s4 a > 3 KiB multi-line body (CC collapses it): still submitted, the
#      prompt holds the msg id
#   s5 A runs `c9watch send B` in its Bash, no --wait: B answers
#   s6 mail to B while B works stays queued, goes after B is ready
#   s7 a 32 KiB body: delivering -> delivered time (PASTE_TIMEOUT, Q6)
#   s8 mail sent while the engine drains stays queued, is delivered after
#      the restart resumes A
#
# Usage: tests/live/m1_send.sh <dir with c9watch and c9watch-agent> <WORK>
#   ONLY=s7 runs one step.
# Results: <WORK>/results.txt, <WORK>/evidence/, <WORK>/pids.
# The script never signals a process it did not start (pids in <WORK>/pids).

set -u
BUILD=${1:?build dir}
WORK=${2:?work dir}
MODEL=claude-haiku-5-5
NONCE=$(date +%s)
ONLY=${ONLY:-}
want() { [ -z "$ONLY" ] || [ "$ONLY" = "$1" ]; }

check_work() { # path
    case "$1" in
    *..* | /private/tmp/claude-* | /private/tmp/claude)
        echo "WORK must not hold '..' or be under /private/tmp/claude-*: $1" >&2
        exit 2
        ;;
    /private/tmp/?*) ;;
    *)
        echo "WORK must be under /private/tmp: $1" >&2
        exit 2
        ;;
    esac
}
check_work "$WORK"
if [ -e "$WORK/engine home" ]; then
    echo "$WORK already used; pick a fresh dir" >&2
    exit 2
fi
mkdir -p "$WORK" && WORK=$(cd "$WORK" && pwd -P) || exit 2
check_work "$WORK"

mkdir -p "$WORK"/{bin,evidence}
H="$WORK/engine home"
SOCK="$H/engine-v1.sock"
EV="$WORK/evidence"
RES="$WORK/results.txt"
DB="$H/engine.db"
: >"$RES"
cp "$BUILD/c9watch" "$BUILD/c9watch-agent" "$WORK/bin/"
C9="$WORK/bin/c9watch"
export C9WATCH_ENGINE_HOME="$H"

# Shared RPC, ledger, transcript and result helpers.
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

P="$WORK/proj"
mkdir -p "$P/.claude"
printf '{\n  "model": "%s"\n}\n' "$MODEL" >"$P/.claude/settings.json"
printf '# Scratch project for the c9watch C1b-M1-14 live test.\n' >"$P/CLAUDE.md"

start_engine() { # log name
    "$C9" engine >"$WORK/$1.log" 2>&1 &
    EPID=$!
    echo "$1 $EPID" >>"$WORK/pids"
    for _ in $(seq 1 150); do rpc agent.list '{}' >/dev/null 2>&1 && return 0; sleep 0.2; done
    log "no engine socket"
    exit 1
}
create() { # name -> agent id
    local out
    out=$(rpc agent.create "{\"mode\":\"free\",\"cwd\":\"$P\",\"name\":\"$1\"}" 120)
    echo "$out" >"$EV/create-$1.json"
    echo "$out" | python3 -I -c 'import json,sys; r=json.load(sys.stdin); print(r["result"]["agent"]["id"] if r.get("ok") else "")'
}
send() { # tag to [send args...]: stdout and stderr to evidence, exit code in $RC
    local tag=$1
    shift
    log "send [$tag] $*" | cut -c1-200
    "$C9" send "$@" >"$EV/$tag.out" 2>"$EV/$tag.err"
    RC=$?
}

log "starting the test engine (home: $H)"
start_engine engine
AID=$(create m1-a)
BID=$(create m1-b)
[ -n "$AID" ] && [ -n "$BID" ] || {
    log "create failed: $(cat "$EV"/create-*.json)"
    exit 1
}
echo "agent-a $AID" >>"$WORK/pids"
echo "agent-b $BID" >>"$WORK/pids"
log "A=$AID B=$BID"

# --- s3: terminal -> A, --wait
if want s3; then
send s3 m1-a "Reply with exactly PONG-$NONCE and nothing else." --wait --timeout 180
M3=$(msg_id "$EV/s3.out")
D3=$(msg_detail "$M3" delivered)
if [ $RC -eq 0 ] && [ "$D3" = prompt_hook ] && said "[c9watch mail $M3" "PONG-$NONCE" >/dev/null; then
    pass s3 "rc 0, delivered($D3), the prompt holds the fence, A answered"
else
    fail s3 "rc $RC, delivered detail '$D3', $(said "[c9watch mail $M3" "PONG-$NONCE")"
fi
fi

# --- s4: > 3 KiB multi-line body
if want s4; then
{
    echo "Reply with exactly LINES-$NONCE and nothing else. The lines below are filler."
    for i in $(seq 1 80); do echo "filler line $i: the quick brown fox jumps over the lazy dog"; done
} >"$WORK/s4.txt"
send s4 m1-a --file "$WORK/s4.txt" --wait --timeout 180
M4=$(msg_id "$EV/s4.out")
D4=$(msg_detail "$M4" delivered)
if [ $RC -eq 0 ] && [ "$D4" = prompt_hook ] && said "[c9watch mail $M4" "LINES-$NONCE" >/dev/null; then
    pass s4 "$(wc -c <"$WORK/s4.txt") bytes, delivered($D4), the prompt holds the msg id"
else
    fail s4 "rc $RC, delivered detail '$D4', $(said "[c9watch mail $M4" "LINES-$NONCE")"
fi
fi

# --- s5: A mails B from its Bash
if want s5; then
send s5 m1-a "Use the Bash tool to run exactly this one command, once: \"$C9\" send m1-b \"Reply with exactly BETA-$NONCE and nothing else.\" Then end your turn with one line: SENT." --wait --timeout 180
MB=$(q "SELECT id FROM messages WHERE to_agent = '$BID' AND from_agent = '$AID' ORDER BY created_at LIMIT 1")
if [ -n "$MB" ] && wait_msg "$MB" delivered 120 && wait_state "$BID" ready 180 &&
    said "BETA-$NONCE" "BETA-$NONCE" >/dev/null; then
    pass s5 "A's mail $MB delivered($(msg_detail "$MB" delivered)), B answered"
else
    fail s5 "A's mail '${MB:-none}' state '$(msg_state "${MB:-x}")', B $(state_of "$BID")"
fi
fi

# --- s6: mail to B while B works
if want s6; then
send s6a m1-b "Use the Bash tool to run: sleep 25. Then reply with exactly SLEPT-$NONCE." --kind request
wait_state "$BID" working 60 || log "s6: B never showed working"
"$C9" send m1-b "Reply with exactly AFTER-$NONCE and nothing else." --wait --timeout 240 >"$EV/s6b.out" 2>"$EV/s6b.err" &
W6=$!
sleep 6
M6=$(q "SELECT id FROM messages WHERE to_agent = '$BID' AND body LIKE '%AFTER-$NONCE%'")
S6_EARLY=$(msg_state "$M6")
B6=$(state_of "$BID")
wait $W6
RC=$?
if [ "$S6_EARLY" = queued ] && [ $RC -eq 0 ] && said "AFTER-$NONCE" "AFTER-$NONCE" >/dev/null; then
    pass s6 "queued while B was $B6, delivered($(msg_detail "$M6" delivered)) after, B answered"
else
    fail s6 "6 s in: '$S6_EARLY' while B $B6; rc $RC"
fi
fi

# --- s7: 32 KiB body
if want s7; then
python3 -I - "$WORK/s7.txt" "$NONCE" <<'PY'
import sys
out, n = sys.argv[1], sys.argv[2]
lines = ["Reply with exactly BIG-%s and nothing else. The lines below are filler." % n]
i = 0
while True:
    i += 1
    # Every 4th line is CJK (wide, 3-byte UTF-8 characters).
    line = ("填充 %05d：敏捷的棕色狐狸跳過懶狗，寬字元測試" % i if i % 4 == 0 else
            "filler %05d: lorem ipsum dolor sit amet, consectetur adipiscing elit" % i)
    if sum(len(l.encode()) + 1 for l in lines + [line]) > 32768:
        break
    lines.append(line)
open(out, "w", encoding="utf-8").write("\n".join(lines) + "\n")
PY
send s7 m1-a --file "$WORK/s7.txt" --wait --timeout 240
M7=$(msg_id "$EV/s7.out")
T7=$(q "SELECT (SELECT at FROM message_status WHERE msg_id = '$M7' AND state = 'delivered' ORDER BY seq LIMIT 1) - (SELECT at FROM message_status WHERE msg_id = '$M7' AND state = 'delivering' ORDER BY seq LIMIT 1)")
if [ $RC -eq 0 ] && [ "$(msg_detail "$M7" delivered)" = prompt_hook ]; then
    pass s7 "$(wc -c <"$WORK/s7.txt") bytes, delivering -> delivered ${T7} ms (paste, settle, CR, hook)"
else
    fail s7 "rc $RC, state '$(msg_state "$M7")', ${T7:-?} ms"
fi
fi

# --- s8: mail during drain, delivered after the restart resumes A
if want s8; then
send s8a m1-a "Use the Bash tool to run: sleep 20. Then reply with exactly DRAINED-$NONCE." --kind request
wait_state "$AID" working 60 || log "s8: A never showed working"
log "SIGTERM the engine ($EPID): drain"
kill -TERM "$EPID"
sleep 1
send s8b m1-a "Reply with exactly RESUMED-$NONCE and nothing else."
S8_RC=$RC
M8=$(msg_id "$EV/s8b.out")
for _ in $(seq 1 120); do kill -0 "$EPID" 2>/dev/null || break; sleep 1; done
S8_STOPPED=$(msg_state "$M8")
log "engine stopped; s8 mail ${M8:-none} is '$S8_STOPPED'"
start_engine engine2
if [ $S8_RC -eq 0 ] && [ "$S8_STOPPED" = queued ] && wait_msg "$M8" delivered 240 &&
    wait_state "$AID" ready 240 && said "RESUMED-$NONCE" "RESUMED-$NONCE" >/dev/null; then
    pass s8 "accepted during drain, queued at stop, delivered($(msg_detail "$M8" delivered)) after resume"
else
    fail s8 "send rc $S8_RC, at stop '$S8_STOPPED', now '$(msg_state "${M8:-x}")', A $(state_of "$AID")"
fi
fi

# --- cleanup through the test engine: retire both, then SIGTERM.
rpc agent.retire "{\"id\":\"$AID\"}" >"$EV/retire-a.json"
rpc agent.retire "{\"id\":\"$BID\"}" >"$EV/retire-b.json"
kill -TERM "$EPID"
for _ in $(seq 1 100); do kill -0 "$EPID" 2>/dev/null || break; sleep 1; done
kill -0 "$EPID" 2>/dev/null && note "cleanup: engine $EPID still runs after SIGTERM"
q "SELECT m.id, m.to_agent, m.state, length(m.body), s.state, s.at, s.detail FROM messages m JOIN message_status s ON s.msg_id = m.id ORDER BY s.seq" >"$EV/messages.txt"
q "SELECT seq, ts, principal, method, target, decision, reason FROM audit ORDER BY seq" >"$EV/audit.txt"
transcripts >"$EV/transcripts.txt"
cp "$WORK"/engine*.log "$EV/" 2>/dev/null
log "done; see $RES and $EV"
