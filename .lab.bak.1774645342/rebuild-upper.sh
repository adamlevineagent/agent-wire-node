#!/bin/bash
# Rebuild L1+ only (skips L0 extraction) — 4x faster for prompt iteration
# Usage: ./rebuild-upper.sh <slug>
# Requires: slug already exists with L0 nodes built

set -euo pipefail

SLUG="${1:?Usage: ./rebuild-upper.sh <slug>}"
AUTH="Authorization: Bearer vibesmithy-test-token"
BASE="http://localhost:8765/pyramid"
CLI="/Users/adamlevine/AI Project Files/agent-wire-node/mcp-server/dist/cli.js"
DB="/Users/adamlevine/Library/Application Support/wire-node/pyramid.db"
REPO_CHAINS="/Users/adamlevine/AI Project Files/agent-wire-node/chains"
DATA_CHAINS="/Users/adamlevine/Library/Application Support/wire-node/chains"
TIMEOUT=600  # 10 minutes (no L0 = much faster)
START=$(date +%s)

# Sync chain files
echo "[0] Syncing chain files..."
cp "$REPO_CHAINS/defaults/code.yaml" "$DATA_CHAINS/defaults/code.yaml"
cp "$REPO_CHAINS/prompts/code/"*.md "$DATA_CHAINS/prompts/code/" 2>/dev/null || true
cp "$REPO_CHAINS/prompts/conversation/"*.md "$DATA_CHAINS/prompts/conversation/" 2>/dev/null || true

# Check L0 nodes exist
L0_COUNT=$(sqlite3 "$DB" "SELECT COUNT(*) FROM pyramid_nodes WHERE slug='$SLUG' AND depth=0;")
echo "  L0 nodes: $L0_COUNT"
if [ "$L0_COUNT" -eq 0 ]; then
  echo "ERROR: No L0 nodes for slug '$SLUG'. Run a full build first."
  exit 1
fi

# Cancel any running build
curl -s -H "$AUTH" -X POST "$BASE/$SLUG/build/cancel" 2>/dev/null || true
sleep 1

# Build from depth 1
echo "[1] Building from depth 1..."
BUILD=$(curl -s -H "$AUTH" -X POST "$BASE/$SLUG/build?from_depth=1")
echo "  Build: $BUILD"

# Poll
echo "[2] Polling..."
SAW_RUNNING=false
while true; do
  NOW=$(date +%s)
  ELAPSED=$((NOW - START))

  if [ $ELAPSED -gt $TIMEOUT ]; then
    echo "TIMEOUT after ${ELAPSED}s"
    exit 1
  fi

  STATUS=$(curl -s -H "$AUTH" "$BASE/$SLUG/build/status" 2>/dev/null || echo '{"status":"error"}')
  STATE=$(echo "$STATUS" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('status','unknown'))" 2>/dev/null || echo "parse_error")
  PROGRESS=$(echo "$STATUS" | python3 -c "import sys,json; d=json.load(sys.stdin); p=d.get('progress',{}); print(f\"{p.get('done',0)}/{p.get('total',0)}\")" 2>/dev/null || echo "?/?")
  FAILURES=$(echo "$STATUS" | python3 -c "import sys,json; print(json.load(sys.stdin).get('failures',0))" 2>/dev/null || echo "0")

  if [ "$STATE" = "running" ]; then SAW_RUNNING=true; fi

  if [ "$STATE" = "complete" ] || [ "$STATE" = "complete_with_errors" ]; then
    echo "Build complete in ${ELAPSED}s (failures: $FAILURES)"
    break
  elif [ "$STATE" = "idle" ] && [ "$SAW_RUNNING" = true ]; then
    echo "Build finished in ${ELAPSED}s"
    break
  elif [ "$STATE" = "failed" ] || [ "$STATE" = "cancelled" ]; then
    echo "FAILED after ${ELAPSED}s: $STATUS"
    exit 1
  fi

  echo "  [$PROGRESS] $STATE (${ELAPSED}s, failures: $FAILURES)"
  sleep 5
done

# Apex check
echo ""
echo "=== Apex Check ==="
APEX=$(node "$CLI" apex "$SLUG" 2>/dev/null || echo "APEX_FAILED")
if echo "$APEX" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('distilled')" 2>/dev/null; then
  echo "APEX: VALID"
else
  echo "APEX: INVALID"
  echo "Raw: $(echo "$APEX" | head -c 200)"
  exit 1
fi

# Structure
echo ""
echo "=== Structure ==="
sqlite3 "$DB" "SELECT depth, COUNT(*) as nodes FROM pyramid_nodes WHERE slug='$SLUG' GROUP BY depth ORDER BY depth;"
MAX_DEPTH=$(sqlite3 "$DB" "SELECT MAX(depth) FROM pyramid_nodes WHERE slug='$SLUG';")
TOTAL=$(sqlite3 "$DB" "SELECT COUNT(*) FROM pyramid_nodes WHERE slug='$SLUG';")
END=$(date +%s)
DURATION=$((END - START))
echo "Max depth: $MAX_DEPTH | Total: $TOTAL | Duration: ${DURATION}s"
