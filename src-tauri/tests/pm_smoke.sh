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
