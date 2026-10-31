#!/usr/bin/env bash
# Manual, quota-consuming CC acceptance. NEVER run in CI or without owner OK.
# Usage: m2_socket.sh <engine bindir> </private/tmp/fresh-work>; ONLY=sN.
# Synthetic checks are src-tauri/tests/engine_cli.rs, not this script.
set -u
BUILD=${1:?build dir}; WORK=${2:?fresh work dir}; ONLY=${ONLY:-}
case "$WORK" in *..*|/private/tmp/claude*|/private/tmp/) exit 2;; /private/tmp/?*) ;; *) exit 2;; esac
[ ! -e "$WORK" ] || { echo 'Choose a fresh WORK directory' >&2; exit 2; }
mkdir -p "$WORK"/{bin,evidence,proj/.claude}
WORK=$(cd "$WORK" && pwd -P)
cp "$BUILD/c9watch" "$BUILD/c9watch-agent" "$WORK/bin/"
C9="$WORK/bin/c9watch"; P="$WORK/proj"; H="$WORK/engine home"
SOCK="$H/engine-v1.sock"; DB="$H/engine.db"; EV="$WORK/evidence"; RES="$WORK/results.txt"
: >"$RES"; : >"$WORK/pids"
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"
export C9WATCH_ENGINE_HOME="$H" C9_MAIL_SOCKET=on
printf '{"model":"claude-haiku-5-5"}\n' >"$P/.claude/settings.json"
printf '# Isolated c9watch peer-mail acceptance project.\n' >"$P/CLAUDE.md"
want() { [ -z "$ONLY" ] || [ "$ONLY" = "$1" ]; }
start() {
    "$C9" engine >"$WORK/engine-$1.log" 2>&1 & EPID=$!
    echo "engine $EPID" >>"$WORK/pids"
    for _ in $(seq 1 150); do rpc agent.list '{}' >/dev/null 2>&1 && return; sleep .2; done
    exit 1
}
create() {
    rpc agent.create "{\"mode\":\"free\",\"cwd\":\"$P\",\"name\":\"$1\"}" 120 | tee "$EV/create-$1.json" | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["result"]["agent"]["id"])'
}
send() { local tag=$1; shift; "$C9" send "$@" >"$EV/$tag.out" 2>"$EV/$tag.err"; }
agent_send() { # tag sender target body
    send "$1" "$2" "Use Bash to run exactly once: \"$C9\" send $3 \"$4\". Then end your turn." --wait --timeout 180
}
last_peer() { q "SELECT id FROM messages WHERE from_agent='$1' AND to_agent='$2' ORDER BY created_at DESC,rowid DESC LIMIT 1"; }
peer_seen() { # Checks JSONL, not a model claim.
    python3 -I - "$DB" "$1" <<'PY'
import json,sqlite3,sys
c=sqlite3.connect(sys.argv[1]); wire,target=c.execute('SELECT wire_id,to_agent FROM messages WHERE id=?',(sys.argv[2],)).fetchone()
paths=c.execute('SELECT transcript_path FROM session_alias WHERE agent_id=? AND transcript_path IS NOT NULL',(target,))
for (path,) in paths:
    try:
        for line in open(path):
            try: e=json.loads(line)
            except ValueError: continue
            if e.get('turnOrigin')=='peer' and e.get('origin',{}).get('kind')=='peer' and e['origin'].get('msg_id')==wire: sys.exit(0)
    except OSError: pass
sys.exit(1)
PY
}
check_peer() { [ -n "$2" ] && wait_msg "$2" delivered 180 && [ "$(q "SELECT via FROM messages WHERE id='$2'")" = socket ] && peer_seen "$2" && pass "$1" "$2 peer evidence" || fail "$1" "no peer evidence for ${2:-missing}"; }
cleanup() {
    for id in "${AID:-}" "${BID:-}"; do [ -z "$id" ] || rpc agent.retire "{\"id\":\"$id\"}" >"$EV/retire-$id.json" 2>/dev/null; done
    if [ -n "${EPID:-}" ]; then kill -TERM "$EPID" 2>/dev/null || true; wait "$EPID" 2>/dev/null || true; fi
    # The engine removes only its owned return socket. Never glob-delete CC sockets.
}
trap cleanup EXIT
start on
AID=$(create m2-a); BID=$(create m2-b)
echo "agent-a $AID" >>"$WORK/pids"; echo "agent-b $BID" >>"$WORK/pids"
if want s1; then
    python3 -I - "$DB" <<'PY' >"$EV/gate.txt"
import json,os,sqlite3,sys,stat
c=sqlite3.connect(sys.argv[1])
for pid,sid in c.execute('SELECT st.pty_child_pid,a.provider_sid FROM agents a JOIN agent_state st ON st.agent_id=a.id'):
    p=os.path.expanduser('~/.claude/sessions/%s.json'%pid); v=json.load(open(p)); assert v['sessionId']==sid and v['peerProtocol']==1
    machine=c.execute('SELECT id FROM machines WHERE is_self=1').fetchone()[0]
    ret=os.path.join(os.path.dirname(v['messagingSocketPath']),'c9w-'+machine[:8]+'.sock')
    assert stat.S_ISSOCK(os.lstat(ret).st_mode)
    print(pid,sid,v['messagingSocketPath'],ret)
PY
    [ $? -eq 0 ] && pass s1 'matching pid.json protocol 1; boot return socket recorded in engine log' || fail s1 'gate mismatch'
fi
if want s2; then
    send s2 "$AID" 'Acknowledge and end your turn.' --wait --timeout 180
    M=$(msg_id "$EV/s2.out")
    [ "$(q "SELECT via FROM messages WHERE id='$M'")" = pty ] && [ "$(msg_detail "$M" delivered)" = prompt_hook ] && pass s2 "$M paste prompt_hook" || fail s2 'human route/evidence'
fi
# s3 is also a prerequisite for s5.
if want s3 || want s5; then
    agent_send s3 "$AID" "$BID" 'Acknowledge this peer mail and end your turn.'
    M3=$(last_peer "$AID" "$BID"); check_peer s3 "$M3"
    q "SELECT * FROM message_status WHERE msg_id='$M3'" >"$EV/s3-status.txt"
fi
if want s4; then
    send s4-busy "$BID" 'Use Bash to run sleep 20, then end your turn.'
    wait_state "$BID" working 60 || fail s4 'B never working'
    agent_send s4 "$AID" "$BID" 'Acknowledge this mid-turn peer mail.'
    M4=$(last_peer "$AID" "$BID"); check_peer s4 "$M4"
    q "SELECT ts,payload FROM events WHERE topic='agent.turn_end' AND subject LIKE '%/$BID' ORDER BY seq" >"$EV/s4-turn-ends.txt"
    q "SELECT * FROM message_status WHERE msg_id='$M4'" >"$EV/s4-status.txt"
    wait_state "$BID" ready 180 || true
    EARLY=$(q "SELECT count(*) FROM messages m WHERE m.id='$M4' AND m.state_at < (SELECT min(ts) FROM events WHERE topic='agent.turn_end' AND subject LIKE '%/$BID' AND ts>=m.created_at)")
    [ "$EARLY" = 1 ] || fail s4 'peer evidence was not before the target turn end' 
fi
if want s5; then
    send s5 "$BID" "Use Bash to run once: \"$C9\" send $AID --reply-to $M3 \"Explicit CLI reply\". Then end your turn." --wait --timeout 180
    M5=$(last_peer "$BID" "$AID"); check_peer s5 "$M5"
    [ "$(q "SELECT reply_to||':'||hop FROM messages WHERE id='$M5'")" = "$M3:1" ] || fail s5 'reply link/hop mismatch'
fi
if want s6; then
    agent_send s6 "$AID" "$BID" 'Use SendMessage to reply to this peer sender once.'
    sleep 10
    [ "$(q "SELECT count(*) FROM audit WHERE method='cc.inbound' AND reason='use_cli'")" -ge 1 ] && [ "$(q "SELECT count(*) FROM messages WHERE kind='system' AND to_agent='$BID'")" -ge 1 ] && pass s6 'use_cli audit and system notice' || fail s6 'no corrective evidence'
fi
if want s7; then
    agent_send s7 "$AID" "$BID" 'Run sleep 20 before acknowledging.' & S7=$!
    sleep 1
    rpc agent.retire "{\"id\":\"$BID\"}" >"$EV/s7-retire.json"
    wait "$S7" || true
    M7=$(last_peer "$AID" "$BID")
    [ "$(msg_state "$M7")" = uncertain ] && pass s7 "$M7 uncertain; inspect delayed system notice" || fail s7 'retire race did not leave uncertain (rerun)'
    sleep 65
    q "SELECT body FROM messages WHERE kind='system' AND to_agent='$AID'" >"$EV/s7-notices.txt"
    grep -q 'may have arrived' "$EV/s7-notices.txt" || fail s7 'missing uncertainty notice'
    BID=$(create m2-b-next)
fi
if want s8; then
    python3 -I -c 'print("Acknowledge and end your turn.\n"+"x"*32700)' >"$WORK/large.txt"
    send s8 "$AID" "Use Bash once: \"$C9\" send $BID --file \"$WORK/large.txt\". Then end your turn." --wait --timeout 180
    M8=$(last_peer "$AID" "$BID"); check_peer s8 "$M8"
    q "SELECT at,detail FROM message_status WHERE msg_id='$M8'" >"$EV/s8-latency.txt"
fi
if want s9; then
    agent_send s9 "$AID" "$BID" 'Run sleep 20 before ending your turn.' & S9=$!
    sleep 1; kill -TERM "$EPID"; wait "$S9" || true; wait "$EPID" || true
    [ "$(q "SELECT state FROM agents WHERE id='$BID'")" = interrupted ] && pass s9 'drain interrupted in-flight peer mail' || fail s9 'inspect drain state; the race needs rerun if delivery already completed'
    q "SELECT ts,reason FROM audit WHERE method='drain.blocked_injection'" >"$EV/s9-injection.txt"
    start resume
fi
if want s10; then
    kill -TERM "$EPID"; wait "$EPID" || true; unset C9_MAIL_SOCKET; start default-off
    agent_send s10 "$AID" "$BID" 'Acknowledge the default-off mail.'
    M10=$(last_peer "$AID" "$BID")
    [ "$(q "SELECT via FROM messages WHERE id='$M10'")" = pty ] && pass s10 'default uses paste' || fail s10 'default route'
fi
if want s11; then
    "$C9" mail pause "$BID" >"$EV/s11-pause.json"
    agent_send s11-agent "$AID" "$BID" 'Queued while paused.'
    M11=$(last_peer "$AID" "$BID")
    send s11-human "$BID" 'Human input continues.' --wait --timeout 180
    [ "$(msg_state "$M11")" = queued ] || fail s11 'agent mail was not paused'
    "$C9" mail cancel "$M11" >"$EV/s11-cancel.json"
    "$C9" mail resume "$BID" >"$EV/s11-resume.json"
    [ "$(msg_state "$M11")" = cancelled ] && pass s11 'pause, human mail, cancel, resume' || fail s11 'control state'
fi
if want s12; then
    "$C9" mail list --pretty >"$EV/s12-pretty.txt"
    grep -q 'Thread msg_' "$EV/s12-pretty.txt" && pass s12 'pretty thread listing; owner must inspect evidence' || fail s12 'pretty listing'
fi
q 'SELECT id,from_agent,to_agent,state,via,wire_id,reply_to,thread_id,notice_id,hop FROM messages' >"$EV/messages.txt"
q 'SELECT * FROM message_status ORDER BY seq' >"$EV/status.txt"
q 'SELECT * FROM audit ORDER BY seq' >"$EV/audit.txt"
transcripts >"$EV/transcripts.txt"
grep -q ': FAIL' "$RES" && exit 1
exit 0
