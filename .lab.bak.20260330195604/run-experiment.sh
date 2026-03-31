#!/bin/bash
# Run a single code pyramid experiment
# Usage: ./run-experiment.sh [slug]
# Default slug: opt-test

set -euo pipefail

SLUG="${1:-opt-test}"
# Auth token from ~/Library/Application Support/wire-node/pyramid_config.json → "auth_token" field
# Set via desktop app Settings → API Key, or manually in the JSON file
AUTH="Authorization: Bearer vibesmithy-test-token"
BASE="http://localhost:8765/pyramid"
CLI="/Users/adamlevine/AI Project Files/agent-wire-node/mcp-server/dist/cli.js"
DB="/Users/adamlevine/Library/Application Support/wire-node/pyramid.db"
REPO_CHAINS="/Users/adamlevine/AI Project Files/agent-wire-node/chains"
DATA_CHAINS="/Users/adamlevine/Library/Application Support/wire-node/chains"
TIMEOUT=1200  # 20 minutes (large chunks need time for LLM retries)
START=$(date +%s)

# Sync chain YAML + prompts from repo to data dir before every run
echo "[0/6] Syncing chain files to data dir..."
cp "$REPO_CHAINS/defaults/code.yaml" "$DATA_CHAINS/defaults/code.yaml"
cp "$REPO_CHAINS/prompts/code/"*.md "$DATA_CHAINS/prompts/code/" 2>/dev/null || true
cp "$REPO_CHAINS/prompts/conversation/"*.md "$DATA_CHAINS/prompts/conversation/" 2>/dev/null || true
echo "  Synced."

echo "=== Experiment: $SLUG ==="
echo "Started: $(date)"

# 1. Clean slate — cancel, delete slug via API, then scrub DB
echo "[1/6] Cleaning up old slug..."
curl -s -H "$AUTH" -X POST "$BASE/$SLUG/build/cancel" 2>/dev/null || true
sleep 2
DEL_RESULT=$(curl -s -H "$AUTH" -X DELETE "$BASE/$SLUG" 2>/dev/null || echo '{"error":"not found"}')
echo "  API delete: $DEL_RESULT"
# Belt-and-suspenders: scrub any residual rows (cascade may miss pipeline_steps)
sqlite3 "$DB" "DELETE FROM pyramid_pipeline_steps WHERE slug='$SLUG';" 2>/dev/null || true
sqlite3 "$DB" "DELETE FROM pyramid_nodes WHERE slug='$SLUG';" 2>/dev/null || true
sqlite3 "$DB" "DELETE FROM pyramid_chunks WHERE slug='$SLUG';" 2>/dev/null || true
sqlite3 "$DB" "DELETE FROM pyramid_slugs WHERE slug='$SLUG';" 2>/dev/null || true
echo "  DB scrubbed."

# 2. Create slug (code type, source = agent-wire-node)
echo "[2/5] Creating slug..."
CREATE=$(curl -s -H "$AUTH" -H "Content-Type: application/json" \
  -X POST "$BASE/slugs" \
  -d "{\"slug\":\"$SLUG\",\"content_type\":\"code\",\"source_path\":\"/Users/adamlevine/AI Project Files/agent-wire-node\"}")
echo "  Create: $CREATE"

# 3. Ingest
echo "[3/5] Ingesting..."
INGEST=$(curl -s -H "$AUTH" -X POST "$BASE/$SLUG/ingest")
echo "  Ingest: $INGEST"

# 4. Build
echo "[4/5] Building..."
BUILD=$(curl -s -H "$AUTH" -X POST "$BASE/$SLUG/build")
echo "  Build: $BUILD"

# 5. Poll until done
echo "[5/5] Polling status..."
SAW_RUNNING=false
while true; do
  NOW=$(date +%s)
  ELAPSED=$((NOW - START))

  if [ $ELAPSED -gt $TIMEOUT ]; then
    echo "TIMEOUT after ${ELAPSED}s"
    echo "RESULT: timeout"
    exit 1
  fi

  STATUS=$(curl -s -H "$AUTH" "$BASE/$SLUG/build/status" 2>/dev/null || echo '{"status":"error"}')
  STATE=$(echo "$STATUS" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('status','unknown'))" 2>/dev/null || echo "parse_error")
  PROGRESS=$(echo "$STATUS" | python3 -c "import sys,json; d=json.load(sys.stdin); p=d.get('progress',{}); print(f\"{p.get('done',0)}/{p.get('total',0)}\")" 2>/dev/null || echo "?/?")
  FAILURES=$(echo "$STATUS" | python3 -c "import sys,json; print(json.load(sys.stdin).get('failures',0))" 2>/dev/null || echo "0")

  if [ "$STATE" = "running" ]; then
    SAW_RUNNING=true
  fi

  if [ "$STATE" = "complete" ] || [ "$STATE" = "complete_with_errors" ]; then
    echo "Build complete in ${ELAPSED}s (failures: $FAILURES)"
    break
  elif [ "$STATE" = "idle" ] && [ "$SAW_RUNNING" = true ]; then
    echo "Build finished (idle after running) in ${ELAPSED}s"
    break
  elif [ "$STATE" = "failed" ] || [ "$STATE" = "cancelled" ]; then
    echo "Build FAILED/CANCELLED after ${ELAPSED}s"
    echo "Status: $STATUS"
    echo "RESULT: failed"
    exit 1
  fi

  echo "  [$PROGRESS] $STATE (${ELAPSED}s, failures: $FAILURES)"
  sleep 5
done

# 6. Check apex
echo ""
echo "=== Apex Check ==="
APEX=$(node "$CLI" apex "$SLUG" 2>/dev/null || echo "APEX_FAILED")
if echo "$APEX" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('distilled')" 2>/dev/null; then
  echo "APEX: VALID"
else
  echo "APEX: INVALID or missing"
  echo "Raw: $APEX"
  echo "RESULT: no_apex"
  exit 1
fi

# 7. Structure check
echo ""
echo "=== Pyramid Structure ==="
sqlite3 "$DB" "SELECT depth, COUNT(*) as nodes FROM pyramid_nodes WHERE slug='$SLUG' GROUP BY depth ORDER BY depth;"

MAX_DEPTH=$(sqlite3 "$DB" "SELECT MAX(depth) FROM pyramid_nodes WHERE slug='$SLUG';")
APEX_COUNT=$(sqlite3 "$DB" "SELECT COUNT(*) FROM pyramid_nodes WHERE slug='$SLUG' AND depth=$MAX_DEPTH;")
TOTAL_NODES=$(sqlite3 "$DB" "SELECT COUNT(*) FROM pyramid_nodes WHERE slug='$SLUG';")

echo "Max depth: $MAX_DEPTH | Apex nodes: $APEX_COUNT | Total: $TOTAL_NODES"

if [ "$APEX_COUNT" != "1" ]; then
  echo "WARNING: Expected 1 apex node, got $APEX_COUNT"
fi
if [ "$MAX_DEPTH" -lt 4 ]; then
  echo "WARNING: Max depth $MAX_DEPTH < 4 — pyramid may be truncated"
fi

END=$(date +%s)
DURATION=$((END - START))
echo ""
echo "=== RESULT: success (${DURATION}s) ==="
