#!/usr/bin/env bash
# Shared live-mail helpers; source only after WORK, SOCK, DB, RES and P are set.
log() { printf '%s %s\n' "$(date +%H:%M:%S)" "$*" | tee -a "$WORK/run.log"; }
note() { printf '%s\n' "$*" | tee -a "$RES"; }
pass() { note "$1: PASS, $2"; }
fail() { note "$1: FAIL, $2"; }

cat >"$WORK/rpc.py" <<'PY'
import json, socket, sys
sock, method, params = sys.argv[1], sys.argv[2], json.loads(sys.argv[3])
timeout = float(sys.argv[4]) if len(sys.argv) > 4 else 10
s = socket.socket(socket.AF_UNIX); s.settimeout(timeout); s.connect(sock)
f = s.makefile("rwb")
def call(i, m, p):
    f.write((json.dumps({"v": 1, "id": str(i), "method": m, "params": p}) + "\n").encode()); f.flush()
    return json.loads(f.readline())
h = call(0, "hello", {"apiMajor": 1, "client": {"kind": "cli", "name": "m1_send.sh"}})
if not h.get("ok"):
    print(json.dumps(h)); sys.exit(1)
r = call(1, method, params)
print(json.dumps(r))
sys.exit(0 if r.get("ok") else 1)
PY
rpc() { python3 -I "$WORK/rpc.py" "$SOCK" "$@"; }

state_of() { # agent id -> its state
    rpc agent.list '{"all":true}' 2>/dev/null | python3 -I -c '
import json, sys
try:
    r = json.load(sys.stdin)
except ValueError:
    print("gone"); sys.exit()
for a in (r.get("result") or {}).get("agents", []):
    if a["id"] == sys.argv[1]:
        print(a.get("state") or "-"); break
else:
    print("missing")' "$1"
}
wait_state() { # id state secs
    local t=0
    while [ $t -lt "$3" ]; do
        [ "$(state_of "$1")" = "$2" ] && return 0
        sleep 1
        t=$((t + 1))
    done
    return 1
}
q() { sqlite3 "$DB" "$1" 2>/dev/null; }
msg_state() { q "SELECT state FROM messages WHERE id = '$1'"; }
msg_detail() { # id state -> the detail of its first row in that state
    q "SELECT detail FROM message_status WHERE msg_id = '$1' AND state = '$2' ORDER BY seq LIMIT 1"
}
msg_id() { python3 -I -c 'import json,sys; print(json.loads(sys.stdin.readline())["msg"]["msgId"])' <"$1" 2>/dev/null; }
wait_msg() { # id state secs
    local t=0
    while [ $t -lt "$3" ]; do
        [ "$(msg_state "$1")" = "$2" ] && return 0
        sleep 1
        t=$((t + 1))
    done
    return 1
}

# The transcripts of the project (the agents share one cwd).
transcripts() { ls "$HOME"/.claude/projects/*/*.jsonl 2>/dev/null | xargs grep -l "$P" 2>/dev/null; }
# A user prompt that holds $1, and an assistant text that holds $2.
said() { # prompt-needle answer-needle
    transcripts | xargs python3 -I -c '
import json, sys
need_p, need_a = sys.argv[1], sys.argv[2]
seen_p = seen_a = False
for tr in sys.argv[3:]:
    for line in open(tr, errors="replace"):
        try:
            e = json.loads(line)
        except ValueError:
            continue
        m = e.get("message") or {}
        c = m.get("content")
        text = c if isinstance(c, str) else "\n".join(
            x.get("text", "") for x in (c or []) if isinstance(x, dict))
        if e.get("type") == "user" and need_p in text:
            seen_p = True
        if e.get("type") == "assistant" and need_a in text:
            seen_a = True
print("prompt=%s answer=%s" % (seen_p, seen_a))
sys.exit(0 if seen_p and seen_a else 1)' "$1" "$2"
}
