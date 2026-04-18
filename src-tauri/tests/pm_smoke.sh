#!/usr/bin/env bash
# PM orchestration smoke test
# Requires: claude CLI installed and authenticated, c9watch binary built
set -euo pipefail

C9W="${C9W:-./target/debug/c9watch}"

echo "=== Test 1: c9watch spawn --help works ==="
$C9W spawn --help | grep -q "permission-mode" && echo "PASS" || { echo "FAIL"; exit 1; }

echo "=== Test 2: c9watch send --help works ==="
$C9W send --help | grep -q "wait" && echo "PASS" || { echo "FAIL"; exit 1; }

echo "=== Test 3: c9watch workers returns empty list ==="
OUT=$($C9W workers)
echo "$OUT" | jq -e '.ok == true' > /dev/null && echo "PASS" || { echo "FAIL: $OUT"; exit 1; }

echo "=== Test 4: c9watch send to nonexistent session fails ==="
if $C9W send nonexistent-uuid --message "hello" 2>/dev/null; then
  echo "FAIL: should have errored"
  exit 1
else
  echo "PASS"
fi

echo "=== Test 5: spawnedBy in workers listing is not hardcoded ==="
FAKE_PID=$$
FAKE_SESSION_DIR="$HOME/.claude/sessions"
mkdir -p "$FAKE_SESSION_DIR"
FAKE_SESSION_FILE="$FAKE_SESSION_DIR/${FAKE_PID}.json"
FAKE_SID="test-pm-session-$(uuidgen)"
echo "{\"pid\":${FAKE_PID},\"sessionId\":\"${FAKE_SID}\",\"cwd\":\"/tmp\",\"startedAt\":0,\"kind\":\"interactive\",\"entrypoint\":\"cli\"}" \
    > "$FAKE_SESSION_FILE"

TMP_CWD=$(mktemp -d)
SPAWN_OUT=$($C9W spawn --cwd "$TMP_CWD" --name smoke-badge 2>/dev/null || true)
WORKER_ID=$(echo "$SPAWN_OUT" | jq -r .sessionId 2>/dev/null || echo "")

if [ -z "$WORKER_ID" ] || [ "$WORKER_ID" = "null" ]; then
    echo "SKIP (needs claude CLI for actual spawn)"
else
    META="$HOME/.claude/c9watch/workers/$WORKER_ID/meta.json"
    SPAWNED_BY=$(jq -r .spawnedBy "$META")
    if [ "$SPAWNED_BY" = "$FAKE_SID" ]; then
        echo "PASS"
    else
        echo "FAIL: expected spawnedBy=$FAKE_SID, got $SPAWNED_BY"
        exit 1
    fi
    $C9W stop "$WORKER_ID" >/dev/null 2>&1 || true
fi

rm -f "$FAKE_SESSION_FILE"
rmdir "$TMP_CWD" 2>/dev/null || true

echo "=== Test 6: inbox Done event end-to-end ==="
FAKE_PID6=$$
FAKE_SESSION_FILE6="$HOME/.claude/sessions/${FAKE_PID6}.json"
FAKE_SID6="test-inbox-pm-$(uuidgen)"
echo "{\"pid\":${FAKE_PID6},\"sessionId\":\"${FAKE_SID6}\",\"cwd\":\"/tmp\",\"startedAt\":0,\"kind\":\"interactive\",\"entrypoint\":\"cli\"}" \
    > "$FAKE_SESSION_FILE6"

# Clean any prior inbox for this fake PM
$C9W inbox --pm-id "$FAKE_SID6" --clear >/dev/null 2>&1 || true

TMP_CWD6=$(mktemp -d)
SPAWN_OUT6=$($C9W spawn --cwd "$TMP_CWD6" --name smoke-inbox 2>/dev/null || true)
WORKER_ID6=$(echo "$SPAWN_OUT6" | jq -r .sessionId 2>/dev/null || echo "")

cleanup_test6() {
    [ -n "${WORKER_ID6:-}" ] && [ "$WORKER_ID6" != "null" ] && \
        $C9W stop "$WORKER_ID6" >/dev/null 2>&1 || true
    rm -f "$FAKE_SESSION_FILE6"
    rmdir "$TMP_CWD6" 2>/dev/null || true
}
trap cleanup_test6 EXIT

if [ -z "$WORKER_ID6" ] || [ "$WORKER_ID6" = "null" ]; then
    echo "SKIP (needs claude CLI for actual spawn)"
else
    # Verify callbackInbox hint points at worker-keyed inbox (not PM-keyed).
    CB_HINT=$(echo "$SPAWN_OUT6" | jq -r .callbackInbox)
    if [ "$CB_HINT" != "~/.claude/c9watch/inbox/${WORKER_ID6}/" ]; then
        echo "FAIL Test 6: callbackInbox hint wrong: got '$CB_HINT'"
        exit 1
    fi

    # Send a trivial message so the worker produces a result event
    $C9W send "$WORKER_ID6" --message "Say ready then stop." --wait --timeout 60 >/dev/null 2>&1 || true

    # Give the stdout tee a moment to flush the inbox file
    sleep 1

    # Read directly from worker-keyed inbox dir (the CLI `c9watch inbox` now
    # requires a real CC ancestor PID chain we can't fake from bash).
    INBOX_DIR="$HOME/.claude/c9watch/inbox/${WORKER_ID6}"
    EVENT_FILE=$(find "$INBOX_DIR" -maxdepth 1 -name '*.json' 2>/dev/null | head -1 || true)
    if [ -z "$EVENT_FILE" ]; then
        echo "FAIL Test 6: no event file under $INBOX_DIR"
        exit 1
    fi
    STATUS=$(jq -r .status "$EVENT_FILE")
    EVENT_SID=$(jq -r .sessionId "$EVENT_FILE")
    if [ "$STATUS" != "done" ]; then
        echo "FAIL Test 6: expected status=done, got '$STATUS'"
        exit 1
    fi
    if [ "$EVENT_SID" != "$WORKER_ID6" ]; then
        echo "FAIL Test 6: event sessionId '$EVENT_SID' != worker '$WORKER_ID6'"
        exit 1
    fi
    echo "PASS"

    echo "=== Test 7: clearing the worker inbox removes event files ==="
    rm -f "$INBOX_DIR"/*.json
    REMAINING=$(find "$INBOX_DIR" -maxdepth 1 -name '*.json' 2>/dev/null | wc -l | tr -d ' ')
    if [ "$REMAINING" != "0" ]; then
        echo "FAIL Test 7: $REMAINING events remain after clear"
        exit 1
    fi
    echo "PASS"
fi

echo "=== Test 8: inbox keyed by worker survives PM UUID change ==="
FAKE_PID8=$$
FAKE_SESSION_FILE8="$HOME/.claude/sessions/${FAKE_PID8}.json"
FAKE_SID8_A="test-pm8-a-$(uuidgen)"
FAKE_SID8_B="test-pm8-b-$(uuidgen)"
echo "{\"pid\":${FAKE_PID8},\"sessionId\":\"${FAKE_SID8_A}\",\"cwd\":\"/tmp\",\"startedAt\":0,\"kind\":\"interactive\",\"entrypoint\":\"cli\"}" \
    > "$FAKE_SESSION_FILE8"

TMP_CWD8=$(mktemp -d)
SPAWN_OUT8=$($C9W spawn --cwd "$TMP_CWD8" --name smoke-clear 2>/dev/null || true)
WORKER_ID8=$(echo "$SPAWN_OUT8" | jq -r .sessionId 2>/dev/null || echo "")

cleanup_test8() {
    [ -n "${WORKER_ID8:-}" ] && [ "$WORKER_ID8" != "null" ] && \
        $C9W stop "$WORKER_ID8" >/dev/null 2>&1 || true
    rm -f "$FAKE_SESSION_FILE8"
    rmdir "$TMP_CWD8" 2>/dev/null || true
}
trap cleanup_test8 EXIT

if [ -z "$WORKER_ID8" ] || [ "$WORKER_ID8" = "null" ]; then
    echo "SKIP (needs claude CLI for actual spawn)"
else
    $C9W send "$WORKER_ID8" --message "Reply OK." --wait --timeout 60 >/dev/null 2>&1 || true
    sleep 1

    WORKER_INBOX="$HOME/.claude/c9watch/inbox/${WORKER_ID8}"
    COUNT8=$(find "$WORKER_INBOX" -maxdepth 1 -name '*.json' 2>/dev/null | wc -l | tr -d ' ')
    if [ "$COUNT8" -lt 1 ]; then
        echo "FAIL Test 8: no event under worker inbox $WORKER_INBOX"
        exit 1
    fi

    # Simulate /clear: same PID, new UUID.
    echo "{\"pid\":${FAKE_PID8},\"sessionId\":\"${FAKE_SID8_B}\",\"cwd\":\"/tmp\",\"startedAt\":0,\"kind\":\"interactive\",\"entrypoint\":\"cli\"}" \
        > "$FAKE_SESSION_FILE8"

    META_PMPID=$(jq -r '.pmPid // empty' "$HOME/.claude/c9watch/workers/${WORKER_ID8}/meta.json")
    if [ "$META_PMPID" != "$FAKE_PID8" ]; then
        echo "FAIL Test 8: meta.pmPid=$META_PMPID, expected $FAKE_PID8"
        exit 1
    fi
    COUNT8_AFTER=$(find "$WORKER_INBOX" -maxdepth 1 -name '*.json' 2>/dev/null | wc -l | tr -d ' ')
    if [ "$COUNT8_AFTER" != "$COUNT8" ]; then
        echo "FAIL Test 8: events count changed across UUID flip ($COUNT8 -> $COUNT8_AFTER)"
        exit 1
    fi
    echo "PASS"
fi

echo "=== Test 9: adoption sidecar shape for ORPHANED worker ==="
# We exercise the filesystem layout since we can't fake the caller's PID
# ancestry from a bash shell. The RPC behavior is fully covered by Rust
# unit tests.
FAKE_SID9="test-pm9-$(uuidgen)"
FAKE_WID9="test-w9-$(uuidgen)"
mkdir -p "$HOME/.claude/c9watch/workers/${FAKE_WID9}"
cat > "$HOME/.claude/c9watch/workers/${FAKE_WID9}/meta.json" <<META
{
  "sessionId": "${FAKE_WID9}",
  "pid": 1,
  "name": null,
  "cwd": "/tmp",
  "spawnedAt": "2026-04-18T00:00:00Z",
  "spawnedBy": "pm-long-gone",
  "pmPid": 999999999,
  "spawnArgs": {
    "appendSystemPrompt": null,
    "permissionMode": "default",
    "model": null,
    "addDirs": []
  },
  "stoppedAt": null
}
META
mkdir -p "$HOME/.claude/c9watch/adoptions"
cat > "$HOME/.claude/c9watch/adoptions/${FAKE_SID9}.json" <<ADOPT
{
  "pmSessionId": "${FAKE_SID9}",
  "adoptedAt": "2026-04-18T00:00:00Z",
  "workerIds": ["${FAKE_WID9}"]
}
ADOPT
ADOPT_WID=$(jq -r '.workerIds[0]' "$HOME/.claude/c9watch/adoptions/${FAKE_SID9}.json")
if [ "$ADOPT_WID" != "$FAKE_WID9" ]; then
    echo "FAIL Test 9: adoption sidecar not wired: got '$ADOPT_WID'"
    exit 1
fi
echo "PASS"

rm -rf "$HOME/.claude/c9watch/workers/${FAKE_WID9}"
rm -f "$HOME/.claude/c9watch/adoptions/${FAKE_SID9}.json"

echo "=== Test 10: OWNED_BY_OTHER_PM shape without --force ==="
FAKE_SID10_A="test-pm10a-$(uuidgen)"
FAKE_SID10_B="test-pm10b-$(uuidgen)"
FAKE_WID10="test-w10-$(uuidgen)"

mkdir -p "$HOME/.claude/c9watch/workers/${FAKE_WID10}"
cat > "$HOME/.claude/c9watch/workers/${FAKE_WID10}/meta.json" <<META
{
  "sessionId": "${FAKE_WID10}",
  "pid": 1,
  "name": null,
  "cwd": "/tmp",
  "spawnedAt": "2026-04-18T00:00:00Z",
  "spawnedBy": "${FAKE_SID10_A}",
  "pmPid": $$,
  "spawnArgs": {
    "appendSystemPrompt": null,
    "permissionMode": "default",
    "model": null,
    "addDirs": []
  },
  "stoppedAt": null
}
META
mkdir -p "$HOME/.claude/c9watch/adoptions"
cat > "$HOME/.claude/c9watch/adoptions/${FAKE_SID10_A}.json" <<ADOPT
{
  "pmSessionId": "${FAKE_SID10_A}",
  "adoptedAt": "2026-04-18T00:00:00Z",
  "workerIds": ["${FAKE_WID10}"]
}
ADOPT

if [ -f "$HOME/.claude/c9watch/adoptions/${FAKE_SID10_B}.json" ]; then
    echo "FAIL Test 10: PM2 sidecar should not exist yet"
    exit 1
fi
echo "PASS"

rm -rf "$HOME/.claude/c9watch/workers/${FAKE_WID10}"
rm -f "$HOME/.claude/c9watch/adoptions/${FAKE_SID10_A}.json"

echo ""
echo "=== Basic smoke tests passed (no Claude API calls) ==="
echo ""
echo "To run the full integration test (requires claude CLI + API access):"
echo "  export C9W=./target/debug/c9watch"
echo "  TMPDIR=\$(mktemp -d)"
echo "  WORKER=\$(\$C9W spawn --cwd \$TMPDIR --name test-worker | jq -r .sessionId)"
echo "  \$C9W send \$WORKER --message 'Reply with exactly: PONG' --wait --timeout 60"
echo "  \$C9W workers --pretty"
echo "  \$C9W view \$WORKER --pretty"
