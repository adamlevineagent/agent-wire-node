#!/bin/bash
# Bootstrap Episodic Memory Vine — ingest and build all conversations
# Usage: ./bootstrap-episodic-vine.sh [source_dir] [vine_slug]
#
# Defaults:
#   source_dir: ~/.claude/projects/-Users-adamlevine-AI-Project-Files-agent-wire-node/
#   vine_slug: episodic-memory-vine
#
# This script:
# 1. Scans for .jsonl files > 50KB (main conversations, not subagent)
# 2. Creates a slug per conversation
# 3. Ingests each
# 4. Triggers build with conversation-episodic chain
# 5. Creates a vine slug and registers all bedrocks
# 6. Reports progress
#
# Prerequisites:
# - Pyramid server running on localhost:8765
# - Auth token: test
# - Chains synced to runtime location

set -euo pipefail

AUTH="Authorization: Bearer test"
BASE="http://localhost:8765"
SOURCE_DIR="${1:-/Users/adamlevine/.claude/projects/-Users-adamlevine-AI-Project-Files-agent-wire-node/}"
VINE_SLUG="${2:-episodic-memory-vine}"
MIN_SIZE=50k
MAX_PARALLEL=3

echo "=== Episodic Memory Vine Bootstrap ==="
echo "Source: $SOURCE_DIR"
echo "Vine: $VINE_SLUG"
echo ""

# Step 1: Find all eligible .jsonl files
FILES=$(find "$SOURCE_DIR" -maxdepth 1 -name "*.jsonl" -size +$MIN_SIZE | sort)
FILE_COUNT=$(echo "$FILES" | wc -l | tr -d ' ')
echo "Found $FILE_COUNT conversation files > $MIN_SIZE"
echo ""

# Step 2: Create vine slug
echo "Creating vine slug: $VINE_SLUG"
curl -s -X POST "$BASE/pyramid/slugs" \
  -H "$AUTH" -H "Content-Type: application/json" \
  -d "{\"slug\":\"$VINE_SLUG\",\"content_type\":\"vine\",\"source_path\":\"$SOURCE_DIR\"}" 2>/dev/null || true
echo ""

# Step 3: Process each conversation
POSITION=0
BUILT=0
FAILED=0

for FILE in $FILES; do
  BASENAME=$(basename "$FILE" .jsonl)
  SLUG="em-${BASENAME:0:30}"  # Truncate long UUIDs
  
  echo "[$((POSITION+1))/$FILE_COUNT] Processing $BASENAME..."
  
  # Create slug
  RESULT=$(curl -s -X POST "$BASE/pyramid/slugs" \
    -H "$AUTH" -H "Content-Type: application/json" \
    -d "{\"slug\":\"$SLUG\",\"content_type\":\"conversation\",\"source_path\":\"$FILE\"}" 2>/dev/null)
  
  if echo "$RESULT" | grep -q "error"; then
    echo "  SKIP: slug already exists or error"
  fi
  
  # Ingest
  curl -s -X POST "$BASE/pyramid/$SLUG/ingest" -H "$AUTH" >/dev/null 2>&1
  
  # Build
  BUILD_RESULT=$(curl -s -X POST "$BASE/pyramid/$SLUG/build" -H "$AUTH" 2>/dev/null)
  
  if echo "$BUILD_RESULT" | grep -q "running"; then
    echo "  BUILD STARTED"
    BUILT=$((BUILT + 1))
  else
    echo "  BUILD ISSUE: $BUILD_RESULT"
    FAILED=$((FAILED + 1))
  fi
  
  POSITION=$((POSITION + 1))
  
  # Throttle: wait for some builds to complete before starting more
  if [ $((POSITION % MAX_PARALLEL)) -eq 0 ]; then
    echo "  Waiting 30s for batch to progress..."
    sleep 30
  fi
done

echo ""
echo "=== Bootstrap Summary ==="
echo "Files processed: $FILE_COUNT"
echo "Builds started: $BUILT"
echo "Failed: $FAILED"
echo ""
echo "Monitor progress:"
echo "  curl -s -H '$AUTH' $BASE/pyramid/slugs | python3 -c 'import sys,json; [print(f\"{s[\"slug\"]:50s} nodes={s.get(\"node_count\",0)}\") for s in json.load(sys.stdin) if s[\"slug\"].startswith(\"em-\")]'"
echo ""
echo "When builds complete, register bedrocks to vine $VINE_SLUG manually or via the vine API."
