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
    # Verify callbackInbox hint is present in spawn response
    CB_HINT=$(echo "$SPAWN_OUT6" | jq -r .callbackInbox)
    if [ "$CB_HINT" != "~/.claude/c9watch/inbox/${FAKE_SID6}/" ]; then
        echo "FAIL Test 6: callbackInbox hint wrong: got '$CB_HINT'"
        exit 1
    fi

    # Send a trivial message so the worker produces a result event
    $C9W send "$WORKER_ID6" --message "Say ready then stop." --wait --timeout 60 >/dev/null 2>&1 || true

    # Give the stdout tee a moment to flush the inbox file
    sleep 1

    INBOX_OUT=$($C9W inbox --pm-id "$FAKE_SID6")
    COUNT=$(echo "$INBOX_OUT" | jq -r .count)
    if [ "$COUNT" -lt 1 ]; then
        echo "FAIL Test 6: expected at least 1 inbox event, got count=$COUNT"
        echo "  inbox output: $INBOX_OUT"
        exit 1
    fi

    STATUS=$(echo "$INBOX_OUT" | jq -r '.events[0].status')
    EVENT_SID=$(echo "$INBOX_OUT" | jq -r '.events[0].sessionId')
    if [ "$STATUS" != "done" ]; then
        echo "FAIL Test 6: expected status=done, got '$STATUS'"
        exit 1
    fi
    if [ "$EVENT_SID" != "$WORKER_ID6" ]; then
        echo "FAIL Test 6: event sessionId '$EVENT_SID' != worker '$WORKER_ID6'"
        exit 1
    fi
    echo "PASS"

    echo "=== Test 7: inbox --consume clears the inbox ==="
    CONSUME_OUT=$($C9W inbox --pm-id "$FAKE_SID6" --consume)
    CONSUMED=$(echo "$CONSUME_OUT" | jq -r .consumed)
    if [ "$CONSUMED" != "true" ]; then
        echo "FAIL Test 7: response missing consumed=true"
        exit 1
    fi

    AFTER_OUT=$($C9W inbox --pm-id "$FAKE_SID6")
    AFTER_COUNT=$(echo "$AFTER_OUT" | jq -r .count)
    if [ "$AFTER_COUNT" != "0" ]; then
        echo "FAIL Test 7: after --consume, count=$AFTER_COUNT (expected 0)"
        exit 1
    fi
    echo "PASS"
fi

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
