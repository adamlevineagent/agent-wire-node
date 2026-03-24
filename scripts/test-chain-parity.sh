#!/usr/bin/env bash
# test-chain-parity.sh — Quick parity test: legacy vs chain engine builds
#
# Builds the same content with both legacy and chain engine paths,
# then compares node counts, max depth, and step counts.
#
# Usage: ./scripts/test-chain-parity.sh [JSONL_PATH]
#
# If JSONL_PATH is omitted, uses a default test file.

set -euo pipefail

# ── Configuration ──────────────────────────────────────────────────────────

AUTH="Authorization: Bearer vibesmithy-test-token"
BASE="http://localhost:8765"
DB="$HOME/Library/Application Support/wire-node/pyramid.db"
DATA_DIR="$HOME/Library/Application Support/wire-node"
CONFIG_FILE="$DATA_DIR/pyramid_config.json"

SLUG_LEGACY="parity-legacy-$$"
SLUG_CHAIN="parity-chain-$$"

# Use provided JSONL or find a small one
JSONL_PATH="${1:-}"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

PASS_COUNT=0
FAIL_COUNT=0

# ── Helper functions ───────────────────────────────────────────────────────

log()  { echo -e "${YELLOW}[parity]${NC} $*"; }
pass() { echo -e "${GREEN}[PASS]${NC} $*"; PASS_COUNT=$((PASS_COUNT + 1)); }
fail() { echo -e "${RED}[FAIL]${NC} $*"; FAIL_COUNT=$((FAIL_COUNT + 1)); }

cleanup() {
    log "Cleaning up test slugs..."
    # Delete test slugs (ignore errors)
    curl -s -H "$AUTH" -X DELETE "$BASE/pyramid/$SLUG_LEGACY" >/dev/null 2>&1 || true
    curl -s -H "$AUTH" -X DELETE "$BASE/pyramid/$SLUG_CHAIN"  >/dev/null 2>&1 || true

    # Also clean from DB directly in case API delete doesn't exist
    sqlite3 "$DB" "DELETE FROM pyramid_nodes WHERE slug='$SLUG_LEGACY';" 2>/dev/null || true
    sqlite3 "$DB" "DELETE FROM pyramid_nodes WHERE slug='$SLUG_CHAIN';" 2>/dev/null || true
    sqlite3 "$DB" "DELETE FROM pyramid_pipeline_steps WHERE slug='$SLUG_LEGACY';" 2>/dev/null || true
    sqlite3 "$DB" "DELETE FROM pyramid_pipeline_steps WHERE slug='$SLUG_CHAIN';" 2>/dev/null || true
    sqlite3 "$DB" "DELETE FROM pyramid_slugs WHERE slug='$SLUG_LEGACY';" 2>/dev/null || true
    sqlite3 "$DB" "DELETE FROM pyramid_slugs WHERE slug='$SLUG_CHAIN';" 2>/dev/null || true
}

set_chain_engine() {
    local enabled="$1"
    python3 -c "
import json, pathlib
p = pathlib.Path('$CONFIG_FILE')
c = json.loads(p.read_text())
c['use_chain_engine'] = $enabled
p.write_text(json.dumps(c, indent=2))
"
    log "use_chain_engine set to $enabled"
}

wait_for_build() {
    local slug="$1"
    local timeout="${2:-300}"
    local elapsed=0

    log "Waiting for $slug build to complete (timeout: ${timeout}s)..."

    while [ $elapsed -lt $timeout ]; do
        local response
        response=$(curl -s -H "$AUTH" "$BASE/pyramid/$slug/build/status" 2>/dev/null || echo '{"status":"error"}')

        local status
        status=$(echo "$response" | python3 -c "import sys,json; print(json.load(sys.stdin).get('status','unknown'))" 2>/dev/null || echo "unknown")

        case "$status" in
            complete|idle)
                log "$slug build completed."
                return 0
                ;;
            failed|error)
                log "$slug build FAILED."
                return 1
                ;;
            *)
                sleep 5
                elapsed=$((elapsed + 5))
                ;;
        esac
    done

    log "$slug build timed out after ${timeout}s."
    return 1
}

query_db() {
    sqlite3 "$DB" "$1" 2>/dev/null
}

# ── Preflight checks ──────────────────────────────────────────────────────

log "=== Chain Parity Test ==="
log ""

# Check server health
log "Checking server health..."
HEALTH=$(curl -s -o /dev/null -w "%{http_code}" "$BASE/health" 2>/dev/null || echo "000")
if [ "$HEALTH" = "000" ]; then
    fail "Server not reachable at $BASE. Start the server first."
    echo ""
    echo "  Start with: cd agent-wire-node && cargo tauri dev"
    echo "  Or: node mcp-server/dist/index.js"
    echo ""
    exit 1
fi
pass "Server is running (HTTP $HEALTH)"

# Check DB exists
if [ ! -f "$DB" ]; then
    fail "Database not found at: $DB"
    exit 1
fi
pass "Database found"

# Check config file exists
if [ ! -f "$CONFIG_FILE" ]; then
    fail "Config file not found at: $CONFIG_FILE"
    exit 1
fi
pass "Config file found"

# Determine source path for test content
if [ -z "$JSONL_PATH" ]; then
    # Try to find an existing slug to reuse its source_path
    JSONL_PATH=$(query_db "SELECT source_path FROM pyramid_slugs WHERE content_type='conversation' LIMIT 1;" || echo "")
fi

if [ -z "$JSONL_PATH" ]; then
    fail "No JSONL_PATH provided and no existing conversation slugs found."
    echo "  Usage: $0 /path/to/test.jsonl"
    exit 1
fi
log "Source path: $JSONL_PATH"

# ── Phase 1: Legacy build ──────────────────────────────────────────────────

log ""
log "=== Phase 1: Legacy Build ($SLUG_LEGACY) ==="

set_chain_engine "False"

log "Creating slug $SLUG_LEGACY..."
curl -s -H "$AUTH" -H "Content-Type: application/json" \
    -X POST "$BASE/pyramid/slugs" \
    -d "{\"slug\":\"$SLUG_LEGACY\",\"content_type\":\"conversation\",\"source_path\":$JSONL_PATH}" \
    >/dev/null 2>&1 || true

log "Triggering legacy build..."
curl -s -H "$AUTH" -X POST "$BASE/pyramid/$SLUG_LEGACY/build" >/dev/null 2>&1

if ! wait_for_build "$SLUG_LEGACY" 600; then
    fail "Legacy build did not complete"
    cleanup
    exit 1
fi
pass "Legacy build completed"

# ── Phase 2: Chain engine build ────────────────────────────────────────────

log ""
log "=== Phase 2: Chain Engine Build ($SLUG_CHAIN) ==="

set_chain_engine "True"

log "Creating slug $SLUG_CHAIN..."
curl -s -H "$AUTH" -H "Content-Type: application/json" \
    -X POST "$BASE/pyramid/slugs" \
    -d "{\"slug\":\"$SLUG_CHAIN\",\"content_type\":\"conversation\",\"source_path\":$JSONL_PATH}" \
    >/dev/null 2>&1 || true

log "Triggering chain engine build..."
curl -s -H "$AUTH" -X POST "$BASE/pyramid/$SLUG_CHAIN/build" >/dev/null 2>&1

if ! wait_for_build "$SLUG_CHAIN" 600; then
    fail "Chain engine build did not complete"
    # Restore legacy mode before exiting
    set_chain_engine "False"
    cleanup
    exit 1
fi
pass "Chain engine build completed"

# Restore legacy mode as default
set_chain_engine "False"

# ── Phase 3: Comparison ───────────────────────────────────────────────────

log ""
log "=== Phase 3: Comparison ==="

# 3a. Node count per depth
log ""
log "--- Node count per depth ---"

LEGACY_DEPTHS=$(query_db "SELECT depth || ':' || COUNT(*) FROM pyramid_nodes WHERE slug='$SLUG_LEGACY' GROUP BY depth ORDER BY depth;")
CHAIN_DEPTHS=$(query_db "SELECT depth || ':' || COUNT(*) FROM pyramid_nodes WHERE slug='$SLUG_CHAIN' GROUP BY depth ORDER BY depth;")

echo "  Legacy: $LEGACY_DEPTHS"
echo "  Chain:  $CHAIN_DEPTHS"

if [ "$LEGACY_DEPTHS" = "$CHAIN_DEPTHS" ]; then
    pass "Node counts per depth match"
else
    fail "Node counts per depth DIFFER"
    echo "    Legacy depths: $LEGACY_DEPTHS"
    echo "    Chain depths:  $CHAIN_DEPTHS"
fi

# 3b. Max depth (apex level)
log ""
log "--- Max depth ---"

LEGACY_MAX=$(query_db "SELECT MAX(depth) FROM pyramid_nodes WHERE slug='$SLUG_LEGACY';")
CHAIN_MAX=$(query_db "SELECT MAX(depth) FROM pyramid_nodes WHERE slug='$SLUG_CHAIN';")

echo "  Legacy max depth: $LEGACY_MAX"
echo "  Chain max depth:  $CHAIN_MAX"

if [ "$LEGACY_MAX" = "$CHAIN_MAX" ]; then
    pass "Max depth matches ($LEGACY_MAX)"
else
    fail "Max depth DIFFERS (legacy=$LEGACY_MAX, chain=$CHAIN_MAX)"
fi

# 3c. Total node count
log ""
log "--- Total node count ---"

LEGACY_TOTAL=$(query_db "SELECT COUNT(*) FROM pyramid_nodes WHERE slug='$SLUG_LEGACY';")
CHAIN_TOTAL=$(query_db "SELECT COUNT(*) FROM pyramid_nodes WHERE slug='$SLUG_CHAIN';")

echo "  Legacy total nodes: $LEGACY_TOTAL"
echo "  Chain total nodes:  $CHAIN_TOTAL"

if [ "$LEGACY_TOTAL" = "$CHAIN_TOTAL" ]; then
    pass "Total node count matches ($LEGACY_TOTAL)"
else
    fail "Total node count DIFFERS (legacy=$LEGACY_TOTAL, chain=$CHAIN_TOTAL)"
fi

# 3d. Step count by type
log ""
log "--- Step count by type ---"

LEGACY_STEPS=$(query_db "SELECT step_type || ':' || COUNT(*) FROM pyramid_pipeline_steps WHERE slug='$SLUG_LEGACY' GROUP BY step_type ORDER BY step_type;")
CHAIN_STEPS=$(query_db "SELECT step_type || ':' || COUNT(*) FROM pyramid_pipeline_steps WHERE slug='$SLUG_CHAIN' GROUP BY step_type ORDER BY step_type;")

echo "  Legacy steps: $LEGACY_STEPS"
echo "  Chain steps:  $CHAIN_STEPS"

if [ "$LEGACY_STEPS" = "$CHAIN_STEPS" ]; then
    pass "Step counts by type match"
else
    fail "Step counts by type DIFFER"
fi

# 3e. Node IDs at each depth
log ""
log "--- Node ID comparison (L0 sample) ---"

LEGACY_L0_IDS=$(query_db "SELECT GROUP_CONCAT(id, ',') FROM (SELECT id FROM pyramid_nodes WHERE slug='$SLUG_LEGACY' AND depth=0 ORDER BY id);")
CHAIN_L0_IDS=$(query_db "SELECT GROUP_CONCAT(id, ',') FROM (SELECT id FROM pyramid_nodes WHERE slug='$SLUG_CHAIN' AND depth=0 ORDER BY id);")

echo "  Legacy L0 IDs: $LEGACY_L0_IDS"
echo "  Chain L0 IDs:  $CHAIN_L0_IDS"

if [ "$LEGACY_L0_IDS" = "$CHAIN_L0_IDS" ]; then
    pass "L0 node IDs match"
else
    fail "L0 node IDs DIFFER"
fi

# 3f. Parent-child topology check (count of nodes with parent_id set per depth)
log ""
log "--- Parent-child topology ---"

LEGACY_PARENTS=$(query_db "SELECT depth || ':' || COUNT(*) FROM pyramid_nodes WHERE slug='$SLUG_LEGACY' AND parent_id IS NOT NULL GROUP BY depth ORDER BY depth;")
CHAIN_PARENTS=$(query_db "SELECT depth || ':' || COUNT(*) FROM pyramid_nodes WHERE slug='$SLUG_CHAIN' AND parent_id IS NOT NULL GROUP BY depth ORDER BY depth;")

echo "  Legacy (nodes with parent): $LEGACY_PARENTS"
echo "  Chain (nodes with parent):  $CHAIN_PARENTS"

if [ "$LEGACY_PARENTS" = "$CHAIN_PARENTS" ]; then
    pass "Parent-child topology matches"
else
    fail "Parent-child topology DIFFERS"
fi

# ── Summary ───────────────────────────────────────────────────────────────

log ""
log "==============================="
log "  RESULTS: $PASS_COUNT passed, $FAIL_COUNT failed"
log "==============================="

if [ $FAIL_COUNT -eq 0 ]; then
    echo -e "${GREEN}ALL PARITY CHECKS PASSED${NC}"
else
    echo -e "${RED}$FAIL_COUNT PARITY CHECK(S) FAILED${NC}"
fi

# ── Cleanup prompt ────────────────────────────────────────────────────────

echo ""
read -r -p "Clean up test slugs? [y/N] " response
case "$response" in
    [yY][eE][sS]|[yY])
        cleanup
        log "Cleanup complete."
        ;;
    *)
        log "Test slugs retained: $SLUG_LEGACY, $SLUG_CHAIN"
        log "To clean up later:"
        echo "  sqlite3 \"$DB\" \"DELETE FROM pyramid_nodes WHERE slug IN ('$SLUG_LEGACY','$SLUG_CHAIN');\""
        echo "  sqlite3 \"$DB\" \"DELETE FROM pyramid_pipeline_steps WHERE slug IN ('$SLUG_LEGACY','$SLUG_CHAIN');\""
        echo "  sqlite3 \"$DB\" \"DELETE FROM pyramid_slugs WHERE slug IN ('$SLUG_LEGACY','$SLUG_CHAIN');\""
        ;;
esac

exit $FAIL_COUNT
