# Wire Online Push — Testing Handoff

**Date:** 2026-03-28
**Branch:** `research/chain-optimization`
**What was built:** Complete Wire Online push implementation across 3 repos, 18 commits, ~8,500 lines. Phases 0-3 of the combined build plan.
**Status:** Compiles clean. Zero runtime testing done. Serial verifiers caught and fixed a critical JWT claim mismatch.

---

## Step 0: Build the Node App

The Rust backend has significant changes. Rebuild from the current branch.

```bash
cd "/Users/adamlevine/AI Project Files/agent-wire-node"
git checkout research/chain-optimization
git pull

# Build the Tauri app
cd src-tauri
cargo build 2>&1 | tail -20
# Should complete with warnings only, no errors

# Or build the full Tauri desktop app
cd ..
npm run tauri build
# Or for dev mode:
npm run tauri dev
```

If `cargo build` fails, capture the full error output — the most likely cause is a missing dependency or a type mismatch that `cargo check` missed (check vs build can differ for procedural macros).

---

## Step 1: Schema Migration Verification

The migration runs automatically on app startup (`init_pyramid_db` → `migrate_online_push_columns`).

**Start the app.** If it starts without crashing, the migration succeeded.

**Verify columns exist:**
```bash
# Find your pyramid DB path (usually in the Tauri data dir)
sqlite3 /path/to/pyramid.db ".schema pyramid_slugs" | grep -E "pinned|access_tier|absorption_mode|cached_emergent_price|metadata_contribution_id|last_published_build_id"
```

**Expected:** All 10 new columns present with correct defaults. Also check:
```bash
sqlite3 /path/to/pyramid.db ".schema pyramid_web_edges" | grep -E "build_id|archived_at|last_confirmed_at"
sqlite3 /path/to/pyramid.db ".schema pyramid_remote_web_edges"
sqlite3 /path/to/pyramid.db ".schema pyramid_unredeemed_tokens"
```

**If the app crashes on startup:** The migration likely hit an issue. Check the logs for SQLite errors. Common causes:
- Column already exists with different type (shouldn't happen — we use error suppression)
- Table creation failure on `pyramid_remote_web_edges` (check FK constraint — needs `pyramid_threads` table to exist)

---

## Step 2: Security Hardening (S1-S4)

### S1: Mutation endpoints return 410

With the app running, test that mutation endpoints are blocked:

```bash
PORT=8787  # or whatever your node runs on
TOKEN="your-local-auth-token"

# Config write — should be 410
curl -s -X POST http://localhost:$PORT/pyramid/config \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"primary_model":"test"}' | jq .

# Build trigger — should be 410
curl -s -X POST http://localhost:$PORT/pyramid/test-slug/build \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{}' | jq .

# Purge — should be 410
curl -s -X DELETE http://localhost:$PORT/pyramid/test-slug/purge \
  -H "Authorization: Bearer $TOKEN" | jq .

# Partner message — should be 410
curl -s -X POST http://localhost:$PORT/partner/message \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"session_id":"x","content":"test"}' | jq .
```

**Expected:** Each returns `{"error":"moved_to_ipc","command":"pyramid_set_config"}` (or similar) with HTTP 410.

**Read endpoints should still work:**
```bash
# Slug list — should return data
curl -s http://localhost:$PORT/pyramid/slugs \
  -H "Authorization: Bearer $TOKEN" | jq '.[:2]'

# Apex — should return data
curl -s http://localhost:$PORT/pyramid/opt-025/apex \
  -H "Authorization: Bearer $TOKEN" | jq .headline
```

### S2: CORS restricted

```bash
# Bad origin — should NOT get Access-Control-Allow-Origin header
curl -s -I -X OPTIONS http://localhost:$PORT/pyramid/slugs \
  -H "Origin: https://evil.com" \
  -H "Access-Control-Request-Method: GET" 2>&1 | grep -i "access-control"

# Good origin — should get Access-Control-Allow-Origin: http://localhost:1420
curl -s -I -X OPTIONS http://localhost:$PORT/pyramid/slugs \
  -H "Origin: http://localhost:1420" \
  -H "Access-Control-Request-Method: GET" 2>&1 | grep -i "access-control"
```

### S3: Web edge archival

Build a pyramid that has web edges (code type works). Then check:
```bash
# Edges should have build_id and last_confirmed_at set
sqlite3 /path/to/pyramid.db "SELECT slug, build_id, last_confirmed_at, archived_at FROM pyramid_web_edges LIMIT 5"
```

After a rebuild on the same slug, old edges should persist with the old build_id, new edges have the new build_id.

### S4: Body size limits

```bash
# Generate a 2MB payload
python3 -c "print('{\"data\":\"' + 'x'*2000000 + '\"}')" > /tmp/big.json

# Should be rejected (payload too large)
curl -s -X POST http://localhost:$PORT/pyramid/opt-025/annotate \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d @/tmp/big.json | jq .
```

**Expected:** 413 Payload Too Large or similar rejection.

---

## Step 3: Desktop App UI Testing

Open the desktop app and check:

### Pyramids Tab (Node page)
- [ ] "Pyramids" tab visible alongside Sync, Market, Logs
- [ ] Each slug shows publication status (published/pending/never)
- [ ] "Publish Now" button visible and clickable
- [ ] Auto-publish toggle present

### Access Tier Controls
- [ ] Expand a slug's config panel
- [ ] Access tier dropdown shows: Public, Circle-Scoped, Priced, Embargoed
- [ ] Changing to "Priced" shows price override field + cached emergent price
- [ ] Changing to "Circle-Scoped" shows circles input
- [ ] Save button works (check SQLite to confirm)

### Absorption Controls
- [ ] Absorption mode dropdown: Open, Absorb All, Absorb Selective
- [ ] Absorb All shows rate limit + daily cap inputs
- [ ] Absorb Selective shows chain ID input

### Pinned Pyramids
- [ ] If any pyramids are pinned, they show a "pinned" badge
- [ ] "Unpin" and "Refresh Now" buttons visible for pinned slugs

### Remote Tab
- [ ] "Remote" tab visible in Node page
- [ ] Shows Wire identity status, tunnel status
- [ ] Manual tunnel URL input field present

---

## Step 4: Publication Pipeline

This tests WS-ONLINE-A and WS-ONLINE-B together.

**Prerequisites:** App running with a built pyramid and tunnel active.

1. Enable auto-publish on a slug via the Pyramids tab
2. Watch logs for sync timer messages (should tick every 60s)
3. After a tick, check:
   ```bash
   sqlite3 /path/to/pyramid.db "SELECT slug, last_published_build_id FROM pyramid_slugs WHERE last_published_build_id IS NOT NULL"
   ```
4. If Wire server is deployed, check that `pyramid_metadata` contribution appears:
   ```bash
   curl "https://your-wire-server/api/v1/wire/query?type=pyramid_metadata" | jq '.contributions[:2]'
   ```

**Common failure points:**
- Tunnel not running → metadata published without tunnel_url
- Wire server not reachable → publication of nodes may fail (check logs for HTTP errors)
- Auth token issues → Wire publish fails with 401

---

## Step 5: Wire Server Deployment

The GoodNewsEveryone repo has changes that need deploying:

### New files to deploy:
- `src/app/api/v1/wire/pyramid-query-token/route.ts`
- `src/app/api/v1/wire/payment-intent/route.ts`
- `src/app/api/v1/wire/payment-redeem/route.ts`
- `src/lib/server/payment-escrow.ts`

### Migration to apply:
```bash
# Apply the payment escrow migration
# File: supabase/migrations/20260328000000_payment_escrow.sql
# This adds held_credits to wire_operators, creates payment_tokens table, and 4 RPCs
```

### Modified files:
- `src/lib/server/contribute-core.ts` (3 new types + validation)
- `src/app/api/v1/contribute/route.ts` (imports VALID_TYPES from contribute-core)
- `src/app/api/v1/wire/query/route.ts` (type filter)

### Verify after deploy:
```bash
# Type registration
curl -s "https://your-wire-server/api/v1/wire/query?type=pyramid_metadata" | jq .

# Payment-intent endpoint exists (should return 401, not 404)
curl -s -X POST "https://your-wire-server/api/v1/wire/payment-intent" | jq .status
```

---

## Step 6: Remote Querying (Two-Node Test)

Needs two nodes with tunnels, or one node querying itself via tunnel.

**Self-test (one node):**
```bash
# Get your tunnel URL
TUNNEL_URL=$(curl -s http://localhost:$PORT/tunnel-status | jq -r .tunnel_url)

# Get a pyramid query token from Wire server
QUERY_TOKEN=$(curl -s -X POST "https://your-wire-server/api/v1/wire/pyramid-query-token" \
  -H "Authorization: Bearer $WIRE_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"slug":"opt-025","query_type":"apex"}' | jq -r .token)

# Query your own pyramid via tunnel with Wire JWT
curl -s "$TUNNEL_URL/pyramid/opt-025/apex" \
  -H "Authorization: Bearer $QUERY_TOKEN" | jq .headline
```

**Expected:** Returns the apex headline. If it fails:
- 401 → JWT validation issue (check public key, audience claim)
- 403 → Access tier blocking (check slug's access_tier)
- 451 → Slug is embargoed
- 429 → Rate limited

**Test rate limiting:**
```bash
# Rapid-fire 110 requests (should get 429 after 100)
for i in $(seq 1 110); do
  STATUS=$(curl -s -o /dev/null -w "%{http_code}" "$TUNNEL_URL/pyramid/opt-025/apex" \
    -H "Authorization: Bearer $QUERY_TOKEN")
  echo "$i: $STATUS"
done
```

---

## Step 7: Query Cost + Payment Flow

**Test query-cost endpoint:**
```bash
curl -s "$TUNNEL_URL/pyramid/opt-025/query-cost?query_type=drill&node_id=L2-001" \
  -H "Authorization: Bearer $QUERY_TOKEN" | jq .
```

**Expected:** `{ stamp: 1, access_price: 0, total: 1, slug: "opt-025", serving_node_id: "..." }`

**Test payment-intent (Wire server):**
```bash
curl -s -X POST "https://your-wire-server/api/v1/wire/payment-intent" \
  -H "Authorization: Bearer $WIRE_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"amount":1,"serving_node_id":"NODE_OPERATOR_ID","contribution_handle_path":"opt-025/3/APEX-ID","query_type":"apex","slug":"opt-025"}' | jq .
```

**Expected:** `{ payment_token: "ey...", expires_at: "...", nonce: "..." }`
**402:** Insufficient credits.
**404/500:** Endpoint not deployed or migration not applied.

---

## Step 8: Vibesmithy

```bash
cd "/Users/adamlevine/AI Project Files/vibesmithy"
npm run dev
```

- [ ] Navigate to `/explore` — search page loads
- [ ] Manual tunnel URL input works
- [ ] Entering a tunnel URL and clicking "Explore" navigates to space view
- [ ] Remote pyramid browsable (apex, drill)
- [ ] Settings page shows Wire Connection section
- [ ] Sidebar has "Explore" nav item

---

## What To Report Back

For each layer tested, report:
1. **PASS** or **FAIL** with the specific error
2. For failures: the curl output, log messages, or UI screenshot
3. Any unexpected behavior even if not a crash

**Priority order:** Build → Schema → S1/S2 → UI → Publication → Wire server deploy → Remote querying → Payment → Vibesmithy

The most likely first failure is the Rust build itself (procedural macros, feature flags) or the JWT validation when real Wire tokens hit the dual-auth filter for the first time.
