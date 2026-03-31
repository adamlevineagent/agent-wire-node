# Sprint 3 Server-Side Fixes Handoff

These issues were found during the Sprint 3 pre-implementation audit. The TOCTOU race, query types, and stale comments are being fixed now. The items below need separate server-side work.

## 1. Serving Node Payment-Redeem Call (MAJOR)

**Problem:** The serving node has `validate_payment_token()` in routes.rs that validates payment JWTs, but **no code ever calls `POST /api/v1/wire/payment-redeem`** to actually collect payment. The enforcement points are marked with `### WS-ONLINE-H ENFORCEMENT POINT ###` comments (routes.rs lines 510-516) but the actual HTTP call to the Wire server doesn't exist. The `pyramid_unredeemed_tokens` SQLite table and CRUD functions exist in db.rs, but nothing populates or retries them.

**What needs building:**
1. After serving a paid pyramid query, call `POST /api/v1/wire/payment-redeem` with the payment token
2. On success: log the redemption
3. On failure (network error, Wire server down): store the token in `pyramid_unredeemed_tokens` for retry
4. Background task that periodically retries unredeemed tokens (the table exists, the retry logic doesn't)

**Files:**
- `src-tauri/src/pyramid/routes.rs` — add redeem call after query execution at enforcement points
- `src-tauri/src/pyramid/db.rs` — unredeemed token CRUD already exists, verify it works
- `src-tauri/src/main.rs` — add background task for retrying unredeemed tokens (similar to existing heartbeat/sync tasks)

## 2. Export Endpoint Skips Access Tier Enforcement (MODERATE)

**Problem:** The `/pyramid/:slug/export` endpoint in routes.rs does not check access tiers. An embargoed pyramid's data could be exported by anyone with a valid JWT, bypassing the access tier system that protects apex/drill/search endpoints.

**Fix:** Add `with_slug_read_auth()` filter to the export endpoint, same as other read endpoints.

**File:** `src-tauri/src/pyramid/routes.rs` — find the export route and add the auth filter

## 3. Handle-Path Format Mismatch (MODERATE)

**Problem:** `resolveContributionFromHandlePath` in `payment-escrow.ts` expects either a raw UUID or `@handle/contribution-slug` (2 segments). But Pillar 14 handle-paths are `{handle}/{epoch-day}/{sequence}` (3 segments). The `parts.slice(1).join('/')` call joins `epoch-day/sequence` as the slug, which fails lookup.

**Fix:** Update `resolveContributionFromHandlePath` to handle Pillar 14 format: split on `/`, if 3+ segments, query by `handle_path = full_path` instead of trying to extract a slug.

**File:** `GoodNewsEveryone/src/lib/server/payment-escrow.ts`

## 4. walkChainAndPay Agent Resolution (MINOR)

**Problem:** For multi-agent operators, `walkChainAndPay` resolves the creator's pseudo_id to pay them, but if the operator has multiple agents, the resolution is non-deterministic (could pick any agent). The payment goes to the right operator pool (Pillar 15), but the attribution to a specific pseudo_id may be wrong.

**This is cosmetic** — credits pool at operator level regardless. But the ledger entry might reference the wrong pseudo_id.

## 5. Float Pool Dependency (MINOR)

**Problem:** `walkChainAndPay` uses `payFromFloat` to pay creators. If the float pool is empty, paid queries fail even though the querier's credits are locked. Credits get released back, but the serving node doesn't get paid.

**This is a systemic issue** — the float pool needs to be funded. Not a Sprint 3 fix, but worth tracking.

---

Priority: Items 1 and 2 should be fixed before Sprint 3 ships. Items 3-5 are quality improvements.
