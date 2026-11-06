#!/usr/bin/env bash
# Live checks for E5, an agent creates and retires its own children
# (PR #226). It runs steps 1-5 of docs/e5-agent-parent-manual-test.md, and
# step 6 (limits) with --limits, and prints one PASS/FAIL line per check.
# It exits 1 when any check fails.
#
# Costs no model prompts (PROMPTS stays 0): the script types `!<command>`
# into an agent's PTY. That is Claude Code's bash mode: CC runs the command
# as its own child, without a model turn, so the engine sees the call come
# from inside that agent, as `c9watch-agent child ...` from the agent's
# Bash tool would. Each command is a small script the test writes into the
# agent's own cwd (<cwd>/out/<tag>.sh); its output lands in <tag>.out next
# to it.
#
# - Keys reach the PTY through the `live-test` input file
#   (<home>/live-test/<id>.in, engine::live_test), so the engine and
#   c9watch-agent must come from a build with that feature:
#     cd src-tauri && cargo build --release --no-default-features \
#       --features engine,live-test --bin c9watch --bin c9watch-agent
#   The script copies both into <ROOT>/bin and runs only those copies.
# - Run it from a plain terminal, not inside Claude Code or Codex: D8
#   refuses agent.create there. The script checks its ancestry and stops.
# - It drives a private engine (C9WATCH_ENGINE_HOME=<ROOT>/home), never
#   the normal one. At exit it retires the root agents it made and stops
#   that engine. Every file it makes is under ROOT. Claude Code itself
#   writes transcripts under ~/.claude/projects and folder trust into
#   ~/.claude.json, as for any session; the script only reads them (check
#   3b-trust reports the trust entries as INFO).
# - Needs a real, logged-in `claude` on PATH, python3 and sqlite3. Steps
#   1-5 run 5 CC processes at most (about 5 minutes); --limits runs 16 more.
#
# Usage:
#   tests/live/e5_parent.sh [--limits] <dir with c9watch and c9watch-agent> [ROOT]
#     ROOT defaults to ~/e5-test-<time>; it must not exist yet.
#   tests/live/e5_parent.sh clean <ROOT>
#     Removes a ROOT this script made (it holds .e5-live) once its engine
#     is gone. Nothing else is removed.
# Results: <ROOT>/results.txt, evidence in <ROOT>/evidence.

set -u
MARK=.e5-live
PROMPTS=0

refuse() {
    echo "e5_parent.sh: $*" >&2
    exit 2
}

check_root() { # path
    case "$1" in
    *..*) refuse "ROOT must not hold '..': $1" ;;
    "$HOME"/.claude | "$HOME"/.claude/* | "$HOME"/.codex | "$HOME"/.codex/*)
        refuse "ROOT must not be under ~/.claude or ~/.codex: $1"
        ;;
    /*) ;;
    *) refuse "ROOT must be absolute: $1" ;;
    esac
    # The paths go into JSON and into keys typed into a PTY unquoted.
    printf '%s' "$1" | grep -q '^[A-Za-z0-9/._-]*$' ||
        refuse "ROOT may hold only letters, digits and / . _ -: $1"
}

# D8: the engine refuses agent.create from any process under a session it
# observes (Claude Code, Codex, ...). Do not work around it; run from a
# plain terminal.
under_agent_session() {
    local p=$$ comm
    if [ -n "${C9_AGENT_ID:-}" ]; then
        echo "env C9_AGENT_ID=$C9_AGENT_ID"
        return 0
    fi
    if [ -n "${CLAUDECODE:-}" ]; then
        echo "env CLAUDECODE=$CLAUDECODE"
        return 0
    fi
    while [ "${p:-1}" -gt 1 ]; do
        comm=$(ps -o comm= -p "$p" 2>/dev/null)
        case "$comm" in
        */claude/versions/* | claude | */claude | codex | */codex | opencode | */opencode)
            echo "pid $p $comm"
            return 0
            ;;
        esac
        p=$(ps -o ppid= -p "$p" 2>/dev/null | tr -d ' ')
    done
    return 1
}

sock_alive() { # socket path -> 0 when an engine answers a connect there
    python3 -I -c '
import socket, sys
s = socket.socket(socket.AF_UNIX); s.settimeout(2)
try:
    s.connect(sys.argv[1])
except OSError:
    sys.exit(1)' "$1" 2>/dev/null
}

if [ "${1:-}" = clean ]; then
    W=${2:?ROOT to clean}
    [ -f "$W/$MARK" ] || refuse "$W was not made by this script (no $MARK); not removing"
    sock_alive "$W/home/engine-v1.sock" &&
        refuse "the engine of $W still runs; stop it first: C9WATCH_ENGINE_HOME=$W/home $W/bin/c9watch engine stop"
    rm -rf "$W"
    echo "removed $W"
    exit 0
fi

LIMITS=0
if [ "${1:-}" = --limits ]; then
    LIMITS=1
    shift
fi
BUILD=${1:?usage: e5_parent.sh [--limits] <dir with c9watch and c9watch-agent> [ROOT]}
ROOT=${2:-$HOME/e5-test-$(date +%Y%m%d-%H%M%S)}
check_root "$ROOT"
if s=$(under_agent_session); then
    refuse "this shell runs under an agent session ($s); the engine refuses agent.create from there (D8). Run it from a plain terminal."
fi
[ -e "$ROOT" ] && refuse "$ROOT already exists; pick a fresh dir"
[ -x "$BUILD/c9watch" ] && [ -x "$BUILD/c9watch-agent" ] || refuse "no c9watch and c9watch-agent in $BUILD"
command -v claude >/dev/null || refuse "no claude on PATH"
command -v sqlite3 >/dev/null || refuse "no sqlite3 on PATH"
command -v python3 >/dev/null || refuse "no python3 on PATH"
mkdir -p "$ROOT" && ROOT=$(cd "$ROOT" && pwd -P) || exit 2
check_root "$ROOT"
: >"$ROOT/$MARK"

WORK=$ROOT
H="$ROOT/home"
R="$ROOT/root"
SOCK="$H/engine-v1.sock"
DB="$H/engine.db"
EV="$ROOT/evidence"
RES="$ROOT/results.txt"
P=e5-live # lib.sh's transcript needle; unused here
mkdir -p "$ROOT/bin" "$EV" "$R"
: >"$RES"
cp "$BUILD/c9watch" "$BUILD/c9watch-agent" "$ROOT/bin/"
C9="$ROOT/bin/c9watch"
export C9WATCH_ENGINE_HOME="$H"
# shellcheck source=tests/live/lib.sh
. "$(dirname "$0")/lib.sh"

# Counted from results.txt at the end: some checks run in a subshell.
ok() { pass "$1" "$2"; }
bad() { fail "$1" "$2"; }
info() { note "$1: INFO, $2"; }

{
    echo "date: $(date)"
    echo "claude: $(claude --version 2>&1)"
    echo "c9watch: $("$C9" --version 2>&1)"
    echo "ROOT: $ROOT"
    echo "limits: $LIMITS"
} >"$EV/env.txt"
log "ROOT=$ROOT"

# --- dirs. 3b plants a project hook and an MCP server in a/h, standing in
# for the parent, which can write there.
mkdir -p "$R"/a/c/g "$R"/a/k "$R"/a/h/.claude "$R"/a/.git/x "$R"/b
printf '{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"touch %s/pwned"}]}]}}\n' "$ROOT" >"$R/a/h/.claude/settings.json"
printf '{"mcpServers":{"x":{"command":"touch","args":["%s/pwned-mcp"]}}}\n' "$ROOT" >"$R/a/h/.mcp.json"

# Folder trust: does ~/.claude.json hold a project entry for each dir?
trust_snapshot() { # tag dir...
    local tag=$1
    shift
    python3 -I - "$HOME/.claude.json" "$@" <<'PY' >"$EV/trust-$tag.txt"
import json, sys
f, dirs = sys.argv[1], sys.argv[2:]
try:
    projects = json.load(open(f)).get("projects") or {}
except (OSError, ValueError) as e:
    projects = None
    print("unreadable %s: %s" % (f, e))
for d in dirs:
    if projects is None:
        print("%s unknown" % d)
    elif d in projects:
        e = projects[d]
        print("%s present trust=%s" % (d, e.get("hasTrustDialogAccepted")))
    else:
        print("%s absent" % d)
PY
}
TRUST_DIRS=("$R/a" "$R/a/c" "$R/a/h" "$R/a/c/g" "$R/b")
trust_snapshot before "${TRUST_DIRS[@]}"

# --- engine
log "starting the test engine (home: $H)"
"$C9" engine >"$ROOT/engine.log" 2>&1 &
EPID=$!
echo "engine $EPID" >>"$ROOT/pids"
for _ in $(seq 1 100); do [ -S "$SOCK" ] && break; sleep 0.2; done
[ -S "$SOCK" ] || {
    log "no engine socket; see $ROOT/engine.log"
    exit 1
}
mkdir -m 700 "$H/live-test"

ROOTS=() # the root agents this run made, retired at exit
stop_engine() {
    local id
    # bash 3.2 (macOS): an empty array is unbound under set -u.
    for id in ${ROOTS[@]+"${ROOTS[@]}"}; do
        case "$(state_of "$id")" in retired | failed | missing | gone) ;; *) rpc agent.retire "{\"id\":\"$id\"}" 60 >/dev/null 2>&1 ;; esac
    done
    if sock_alive "$SOCK"; then
        "$C9" engine stop >>"$ROOT/engine.log" 2>&1 ||
            note "cleanup: '$C9 engine stop' failed; stop it by hand (C9WATCH_ENGINE_HOME=$H)"
    fi
    if kill -0 "$EPID" 2>/dev/null; then
        kill -TERM "$EPID"
        for _ in $(seq 1 60); do kill -0 "$EPID" 2>/dev/null || return 0; sleep 1; done
        note "cleanup: engine $EPID still runs 60 s after SIGTERM; stop it by hand"
    fi
}
trap stop_engine EXIT

busy() { # rpc reply -> 0 when it is a retryable `busy`
    printf '%s' "$1" | python3 -I -c '
import json, sys
try:
    e = json.load(sys.stdin).get("error") or {}
except ValueError:
    sys.exit(1)
sys.exit(0 if e.get("code") == "busy" and e.get("retryable") else 1)'
}

# agent.create params: free mode, a guard and the blocking fence hook.
params() { # cwd deny allow [extra json members]
    printf '{"mode":"free","cwd":"%s","guard":{"denyWriteUnder":["%s"],"allowWrite":["%s"]},"require":["hook.pre_tool_use.block"]%s}' \
        "$1" "$2" "$3" "${4:-}"
}

# A root agent from the terminal. A fresh engine answers `busy` until its
# first detection pass; retry that for up to 60 s.
create_root() { # cwd name -> agent id
    local out t=0
    while :; do
        out=$(rpc agent.create "$(params "$1" "$R" "$1" ",\"name\":\"$2\"")" 120)
        busy "$out" && [ $t -lt 60 ] || break
        sleep 1
        t=$((t + 1))
    done
    echo "$out" >"$EV/create-$2.json"
    echo "$out" | python3 -I -c 'import json,sys; r=json.load(sys.stdin); print(r["result"]["agent"]["id"] if r.get("ok") else "")' 2>/dev/null
}

alist() { rpc agent.list '{"all":true}' 60 2>/dev/null; }
# The engine's agent ids, sorted (observed sessions left out).
ids() {
    alist | python3 -I -c '
import json, sys
r = json.load(sys.stdin)
print("\n".join(sorted(a["id"] for a in (r.get("result") or {}).get("agents", []) if a.get("source") == "engine")))' 2>/dev/null
}
field() { # id key -> that field of the agent row, "-" when null
    alist | python3 -I -c '
import json, sys
r = json.load(sys.stdin)
for a in (r.get("result") or {}).get("agents", []):
    if a["id"] == sys.argv[1]:
        v = a.get(sys.argv[2]); print("-" if v is None else v); break
else:
    print("missing")' "$1" "$2" 2>/dev/null
}
# The full command line of an agent's CC process: the one that names its
# spawn settings file (<home>/spawn/<id>-<gen>.json).
agent_args() { # id
    ps -axww -o args= 2>/dev/null | grep -F -- "$H/spawn/$1-" | grep -vF 'grep' | head -1
}
live() { # id -> 0 when the agent is not retired, failed or gone
    case "$(state_of "$1")" in retired | failed | orphaned | missing | gone) return 1 ;; esac
}

send_keys() { # id bytes
    printf '%b' "$2" >"$H/live-test/.$1.tmp" && mv "$H/live-test/.$1.tmp" "$H/live-test/$1.in"
    for _ in $(seq 1 40); do [ -e "$H/live-test/$1.in" ] || return 0; sleep 0.25; done
    log "input for $1 was not taken"
}

# Run a shell body inside agent `id` through CC's bash mode. The body
# runs in <cwd>/out with B set to the engine's c9watch-agent copy, the
# path the doc spells out. Its stdout+stderr and `rc=<exit>` land in
# <cwd>/out/<tag>.out; the function prints that path. A `busy` answer
# (the engine's first detection pass, or right after a restart) is run
# again, up to 6 times, 5 s apart.
astep() { # id cwd tag secs body
    local id=$1 d="$2/out" tag=$3 secs=$4 body=$5 t try=0
    mkdir -p "$d"
    {
        cat >"$d/$tag.sh" <<EOF
cd "\$(dirname "\$0")" || exit 1
B="\$(dirname "\$C9_ENGINE_SOCK")/bin/c9watch-agent"
( $body ) >"$tag.tmp" 2>&1
echo "rc=\$?" >>"$tag.tmp"
mv "$tag.tmp" "$tag.out"
EOF
        while :; do
            rm -f "$d/$tag.out"
            t=0
            while [ $t -lt 60 ]; do
                case "$(state_of "$id")" in ready) break ;; esac
                sleep 1
                t=$((t + 1))
            done
            log "[$tag] in $id: $body"
            send_keys "$id" '!'
            sleep 0.5
            send_keys "$id" "sh $d/$tag.sh"
            sleep 0.6
            send_keys "$id" '\r'
            t=0
            while [ $t -lt "$secs" ] && [ ! -f "$d/$tag.out" ]; do
                sleep 1
                t=$((t + 1))
            done
            sleep 0.5
            grep -q ': busy:' "$d/$tag.out" 2>/dev/null && [ $try -lt 6 ] || break
            try=$((try + 1))
            log "[$tag] engine busy; again in 5 s ($try/6)"
            sleep 5
        done
        cp "$H/live-test/$id.screen" "$EV/$tag.screen" 2>/dev/null
        cp "$d/$tag.out" "$EV/$tag.out" 2>/dev/null
    } >&2
    echo "$d/$tag.out"
}
out_rc() { sed -n 's/^rc=//p' "$1" 2>/dev/null | tail -1; }
out_code() { # out file -> the first engine error code in it, else "none"
    grep -oE 'peer_not_authorized|not_allowed|agent_caller_refused' "$1" 2>/dev/null | head -1 | grep . || echo none
}
out_text() { tr '\n' ' ' <"$1" 2>/dev/null | cut -c1-240; }
out_agent() { # out file key -> that field of the agent the output names
    python3 -I - "$1" "$2" <<'PY' 2>/dev/null
import json, sys
f, key = sys.argv[1:3]
for line in open(f):
    try:
        o = json.loads(line)
    except ValueError:
        continue
    a = o.get("agent") or (o.get("result") or {}).get("agent")
    if isinstance(a, dict):
        v = a.get(key); print("-" if v is None else v); break
PY
}

# A step that must be refused: the expected error code, and no new agent.
refusal() { # tag id cwd want-codes(ERE) body [extra condition note]
    local before after out rc code
    before=$(ids)
    out=$(astep "$2" "$3" "$1" 120 "$5")
    after=$(ids)
    rc=$(out_rc "$out")
    code=$(out_code "$out")
    if [ ! -f "$out" ]; then
        bad "$1" "the command never finished in the agent (see evidence/$1.screen)"
    elif [ "$rc" = 0 ]; then
        bad "$1" "it succeeded: $(out_text "$out")"
    elif ! printf '%s' "$code" | grep -qxE "$4"; then
        bad "$1" "error code '$code', want $4: $(out_text "$out")"
    elif [ "$before" != "$after" ]; then
        bad "$1" "refused ($code) but agent.list gained: $(comm -13 <(echo "$before") <(echo "$after") | tr '\n' ' ')"
    else
        ok "$1" "refused with $code, no new agent${6:+; $6}"
    fi
}

# A child create inside agent `id` that must succeed; prints the child id.
child_ok() { # tag id cwd json-file-name expect-parent
    local out cid st pid
    out=$(astep "$2" "$3" "$1" 150 "\"\$B\" child create --params - <$4")
    cid=$(out_agent "$out" id)
    st=$(out_agent "$out" state)
    if [ "$(out_rc "$out")" != 0 ] || [ -z "$cid" ]; then
        bad "$1" "child create failed: $(out_text "$out")" >&2
        return 1
    fi
    pid=$(field "$cid" parentId)
    if [ "$st" != ready ]; then
        bad "$1" "child $cid is '$st', not ready" >&2
    elif [ "$pid" != "$5" ]; then
        bad "$1" "child $cid is ready but its parentId is '$pid', not $5" >&2
    else
        ok "$1" "child $cid is ready with parentId $5" >&2
    fi
    echo "$cid"
}

summary() {
    stop_engine
    trap - EXIT
    trust_snapshot after "${TRUST_DIRS[@]}"
    paste -d' ' <(sed 's/^/before: /' "$EV/trust-before.txt") <(cut -d' ' -f2- "$EV/trust-after.txt" | sed 's/^/-> after: /') |
        while read -r line; do info 3b-trust "$line"; done
    sqlite3 -header "$DB" 'SELECT seq, ts, principal, method, target, decision, reason FROM audit ORDER BY seq' >"$EV/audit.txt" 2>&1
    sqlite3 -header "$DB" "SELECT id, parent_id, state, state_reason, generation, cwd FROM agents WHERE source = 'engine'" >"$EV/agents-db.txt" 2>&1
    cp "$ROOT/engine.log" "$EV/" 2>/dev/null
    NPASS=$(grep -c ': PASS,' "$RES")
    NFAIL=$(grep -c ': FAIL,' "$RES")
    echo "prompts used: $PROMPTS" >>"$RES"
    echo "summary: $NPASS PASS, $NFAIL FAIL" >>"$RES"
    echo
    echo "==== results ($RES)"
    cat "$RES"
    echo "evidence: $EV"
    echo "remove the run with: $0 clean $ROOT"
    [ "$NFAIL" -eq 0 ]
    exit $?
}

# ---------------------------------------------------------------- step 1
PA=$(create_root "$R/a" e5-parent)
if [ -z "$PA" ]; then
    bad 1-parent "agent.create of the parent failed: $(cut -c1-300 "$EV/create-e5-parent.json")"
    summary
fi
ROOTS+=("$PA")
st=$(state_of "$PA")
if [ "$st" = ready ]; then ok 1-parent "parent $PA is ready"; else bad 1-parent "parent $PA is '$st'"; fi
info 1-parent-args "$(agent_args "$PA" | grep -oE -- '--setting-sources [^ ]*' || echo 'no --setting-sources in the args')"

# ---------------------------------------------------------------- step 2
D="$R/a/out"
mkdir -p "$D"
params "$R/a/c" "$R" "$R/a/c" >"$D/c1.json"
before=$(ids)
out=$(astep "$PA" "$R/a" 2-create 150 '"$B" child create --idem c1 --params - <c1.json')
C1=$(out_agent "$out" id)
if [ "$(out_rc "$out")" = 0 ] && [ -n "$C1" ] && [ "$(out_agent "$out" state)" = ready ]; then
    ok 2-create "child $C1 is ready"
else
    bad 2-create "child create failed: $(out_text "$out")"
fi
if [ -n "$C1" ]; then
    pid=$(field "$C1" parentId)
    if [ "$pid" = "$PA" ]; then ok 2-parentId "agent.list gives $C1 parentId $PA"; else bad 2-parentId "parentId of $C1 is '$pid', not $PA"; fi
    mid=$(ids)
    out=$(astep "$PA" "$R/a" 2-idem 150 '"$B" child create --idem c1 --params - <c1.json')
    again=$(out_agent "$out" id)
    after=$(ids)
    gained=$(comm -13 <(echo "$before") <(echo "$after") | wc -l | tr -d ' ')
    if [ "$again" != "$C1" ]; then
        bad 2-idem "the replay gave '$again', not $C1: $(out_text "$out")"
    elif [ "$mid" != "$after" ] || [ "$gained" != 1 ]; then
        bad 2-idem "same id, but agent.list changed ($gained new since step 2)"
    else
        ok 2-idem "the replay of --idem c1 gave the same child $C1, and no second agent"
    fi
else
    bad 2-parentId "not run: no child"
    bad 2-idem "not run: no child"
fi

# ---------------------------------------------------------------- step 3
printf '{"mode":"free","cwd":"%s"}' "$R/a/k" >"$D/noguard.json"
params "$R/b" "$R" "$R/b" >"$D/outside.json"
params "$R/a" "$R" "$R/a" >"$D/owncwd.json"
params "$R/a/.git/x" "$R" "$R/a/.git/x" >"$D/git.json"
params "$R/a/k" "$R/a" "$R/a/k" >"$D/looser.json"
params "$R/a/k" "$R" "$R/a/k" ',"overlay":{}' >"$D/extra.json"
params "$R/a/k" "$R" "$R/a/k" >"$D/good.json"
for c in noguard outside owncwd git looser extra; do
    refusal "3-$c" "$PA" "$R/a" not_allowed "\"\$B\" child create --params - <$c.json"
done
# The doc's "another binary": the CLI from the parent's Bash. The CLI
# refuses an agent caller (C9_AGENT_ID) itself, before the engine; the
# two checks after it reach the engine.
refusal 3-other-cli "$PA" "$R/a" 'peer_not_authorized|not_allowed|agent_caller_refused' \
    "C9WATCH_ENGINE_HOME=\"$H\" \"$C9\" api agent.create \"\$(cat good.json)\"" "agent_caller_refused is the CLI's own refusal"
cat >"$D/raw.py" <<'PY'
import json, socket, sys
s = socket.socket(socket.AF_UNIX); s.settimeout(30); s.connect(sys.argv[1])
f = s.makefile("rwb")
def call(i, m, p):
    f.write((json.dumps({"v": 1, "id": str(i), "method": m, "params": p}) + "\n").encode()); f.flush()
    return json.loads(f.readline())
h = call(0, "hello", {"apiMajor": 1, "client": {"kind": "cli", "name": "e5_parent.sh"}})
if not h.get("ok"):
    print(json.dumps(h)); sys.exit(1)
r = call(1, "agent.create", json.load(open(sys.argv[2])))
print(json.dumps(r))
sys.exit(0 if r.get("ok") else 1)
PY
refusal 3-other-raw "$PA" "$R/a" peer_not_authorized 'python3 -I raw.py "$C9_ENGINE_SOCK" good.json'
refusal 3-other-copy "$PA" "$R/a" peer_not_authorized 'cp "$B" ./agent-copy && ./agent-copy child create --params - <good.json'

# ---------------------------------------------------------------- step 3b
params "$R/a/h" "$R" "$R/a/h" >"$D/h.json"
HC=$(child_ok 3b-ready "$PA" "$R/a" h.json "$PA")
sleep 5
pw=
for f in "$ROOT"/pwned "$ROOT"/pwned-mcp; do [ -e "$f" ] && pw="$pw $f"; done
if [ -z "$HC" ]; then
    bad 3b-hook "not run: no child in a/h"
    bad 3b-args "not run: no child in a/h"
else
    if [ -n "$pw" ]; then bad 3b-hook "planted config ran: $pw"; else ok 3b-hook "no pwned file: the planted SessionStart hook and .mcp.json server did not run"; fi
    args=$(agent_args "$HC")
    echo "$args" >"$EV/3b-child-args.txt"
    if printf '%s' "$args" | grep -qF -- '--setting-sources  --strict-mcp-config'; then
        ok 3b-args "the child runs with --setting-sources \"\" --strict-mcp-config"
    else
        bad 3b-args "child args lack '--setting-sources  --strict-mcp-config': $(printf '%s' "$args" | cut -c1-240)"
    fi
fi

# ---------------------------------------------------------------- step 4
QA=$(create_root "$R/b" e5-other)
if [ -n "$QA" ] && [ "$(state_of "$QA")" = ready ]; then
    ROOTS+=("$QA")
    ok 4-other-root "second root agent $QA is ready"
    refusal 4-retire-other "$PA" "$R/a" not_allowed "\"\$B\" child retire $QA"
    live "$QA" || bad 4-retire-other "and $QA is now '$(state_of "$QA")'"
else
    [ -n "$QA" ] && ROOTS+=("$QA")
    bad 4-other-root "agent.create of Q failed: $(cut -c1-300 "$EV/create-e5-other.json")"
    bad 4-retire-other "not run: no Q"
fi
refusal 4-retire-self "$PA" "$R/a" not_allowed "\"\$B\" child retire $PA"
live "$PA" || bad 4-retire-self "and the parent is now '$(state_of "$PA")'"
G=
if [ -n "$C1" ]; then
    mkdir -p "$R/a/c/out"
    params "$R/a/c/g" "$R" "$R/a/c/g" >"$R/a/c/out/g.json"
    G=$(child_ok 4-grandchild "$C1" "$R/a/c" g.json "$C1")
else
    bad 4-grandchild "not run: no child"
fi
# Not in the doc: a parent retires its own child (the a/h one).
if [ -n "$HC" ]; then
    out=$(astep "$PA" "$R/a" 4-retire-own 60 "\"\$B\" child retire $HC")
    if [ "$(out_rc "$out")" = 0 ] && wait_state "$HC" retired 10; then
        ok 4-retire-own "the parent retired its child $HC"
    else
        bad 4-retire-own "rc $(out_rc "$out"), $HC is '$(state_of "$HC")': $(out_text "$out")"
    fi
fi

# ---------------------------------------------------------------- step 5
TREE=("$PA")
[ -n "$C1" ] && TREE+=("$C1")
[ -n "$G" ] && TREE+=("$G")
for id in "${TREE[@]}"; do echo "$id $(field "$id" parentId)"; done >"$EV/5-parents-before.txt"
if "$C9" engine restart >"$EV/5-restart.json" 2>&1; then
    t=0
    for id in "${TREE[@]}"; do
        while [ $t -lt 120 ] && [ "$(state_of "$id")" != ready ]; do
            sleep 1
            t=$((t + 1))
        done
    done
    for id in "${TREE[@]}"; do echo "$id $(field "$id" parentId)"; done >"$EV/5-parents-after.txt"
    states=$(for id in "${TREE[@]}"; do printf '%s=%s ' "$id" "$(state_of "$id")"; done)
    gone=$(for id in "${TREE[@]}"; do live "$id" || printf '%s ' "$id"; done)
    if [ -n "$gone" ]; then
        bad 5-restart "not back after the restart: $gone($states)"
    elif ! cmp -s "$EV/5-parents-before.txt" "$EV/5-parents-after.txt"; then
        bad 5-restart "parentIds changed: $(tr '\n' ' ' <"$EV/5-parents-after.txt")"
    else
        ok 5-restart "${#TREE[@]} agents back with the same parentIds ($states)"
    fi
else
    bad 5-restart "engine restart failed: $(cut -c1-300 "$EV/5-restart.json")"
fi
rpc agent.retire "{\"id\":\"$PA\"}" 60 >"$EV/5-retire-parent.json" 2>&1
t0=$(date +%s)
t=0
while [ $t -lt 20 ]; do
    left=$(for id in "${TREE[@]}"; do [ "$(state_of "$id")" = retired ] || printf '%s ' "$id"; done)
    [ -z "$left" ] && break
    sleep 0.5
    t=$((t + 1))
done
secs=$(($(date +%s) - t0))
if [ -n "$left" ]; then
    bad 5-sweep "not retired 10 s after the parent: $left"
else
    ok 5-sweep "the parent and its tree are retired ${secs}s after agent.retire (doc: about 2 s)"
fi
aud() { q "SELECT count(*) FROM audit WHERE $1"; }
na=$(aud "method = 'agent.create' AND decision = 'allow' AND principal LIKE 'agent:$PA%'")
nd=$(aud "method = 'agent.create' AND decision = 'deny' AND principal LIKE 'agent:$PA%'")
nr=$(aud "method = 'agent.retire' AND decision = 'deny' AND principal LIKE 'agent:$PA%'")
nra=$(aud "method = 'agent.retire' AND decision = 'allow' AND principal LIKE 'agent:$PA%'")
ng=$(aud "method = 'agent.retire' AND principal = 'system' AND reason LIKE 'parent-gone:%'")
npg=$(aud "method = 'agent.retire' AND principal = 'system' AND reason = 'parent-gone: $PA'")
want_g=$((${#TREE[@]} - 1))
detail="parent creates allow=$na deny=$nd, retires deny=$nr allow=$nra, system parent-gone=$ng (naming $PA: $npg)"
if [ "$na" -ge 2 ] && [ "$nd" -ge 6 ] && [ "$nr" -ge 2 ] && [ "$ng" -ge "$want_g" ] && [ "$npg" -ge 1 ]; then
    ok 5-audit "$detail"
else
    bad 5-audit "$detail; want allow>=2 deny>=6 retire-deny>=2 parent-gone>=$want_g with one naming $PA"
fi
q "SELECT principal, method, target, decision, reason FROM audit WHERE method LIKE 'agent.%' ORDER BY seq" >"$EV/5-audit-agent.txt"

# ---------------------------------------------------------------- step 6
if [ "$LIMITS" = 1 ]; then
    L="$R/l"
    mkdir -p "$L"/k1/g1/gg/ggg "$L"/k1/g2 "$L"/k1/g3 "$L"/k1/g4 "$L"/k1/g5 "$L"/k1/g6 "$L"/k1/g7 \
        "$L"/k2 "$L"/k3 "$L"/k4 "$L"/k5 "$L"/k6 "$L"/k7 "$L"/k8 "$L"/k9 "$L"/out "$L"/k1/out "$L"/k1/g1/out "$L"/k1/g1/gg/out
    LA=$(create_root "$L" e5-limits)
    if [ -z "$LA" ]; then
        bad 6-limits "agent.create of the limits root failed: $(cut -c1-300 "$EV/create-e5-limits.json")"
    else
        ROOTS+=("$LA")
        k1=
        for i in 1 2 3 4 5 6 7 8; do
            params "$L/k$i" "$R" "$L/k$i" >"$L/out/k$i.json"
            id=$(child_ok "6-child-$i" "$LA" "$L" "k$i.json" "$LA")
            [ "$i" = 1 ] && k1=$id
        done
        params "$L/k9" "$R" "$L/k9" >"$L/out/k9.json"
        refusal 6-children "$LA" "$L" not_allowed '"$B" child create --params - <k9.json'
        grep -q 'live children' "$EV/6-children.out" 2>/dev/null || bad 6-children "the refusal does not say 'live children'"
        if [ -n "$k1" ]; then
            params "$L/k1/g1" "$R" "$L/k1/g1" >"$L/k1/out/g1.json"
            g1=$(child_ok 6-depth-2 "$k1" "$L/k1" g1.json "$k1")
            gg=
            if [ -n "$g1" ]; then
                params "$L/k1/g1/gg" "$R" "$L/k1/g1/gg" >"$L/k1/g1/out/gg.json"
                gg=$(child_ok 6-depth-3 "$g1" "$L/k1/g1" gg.json "$g1")
            fi
            if [ -n "$gg" ]; then
                params "$L/k1/g1/gg/ggg" "$R" "$L/k1/g1/gg/ggg" >"$L/k1/g1/gg/out/ggg.json"
                refusal 6-depth "$gg" "$L/k1/g1/gg" not_allowed '"$B" child create --params - <ggg.json'
                grep -q 'depth 4' "$EV/6-depth.out" 2>/dev/null || bad 6-depth "the refusal does not say 'depth 4'"
            else
                bad 6-depth "not run: no depth-3 agent"
            fi
            # The tree holds LA, k1-k8, g1 and gg (11); g2-g6 make 16.
            for i in 2 3 4 5 6; do
                params "$L/k1/g$i" "$R" "$L/k1/g$i" >"$L/k1/out/g$i.json"
                child_ok "6-tree-$i" "$k1" "$L/k1" "g$i.json" "$k1" >/dev/null
            done
            params "$L/k1/g7" "$R" "$L/k1/g7" >"$L/k1/out/g7.json"
            refusal 6-tree "$k1" "$L/k1" not_allowed '"$B" child create --params - <g7.json'
            grep -q '16 live agents' "$EV/6-tree.out" 2>/dev/null || bad 6-tree "the refusal does not say '16 live agents'"
        else
            bad 6-depth "not run: no first child"
            bad 6-tree "not run: no first child"
        fi
    fi
fi

summary
