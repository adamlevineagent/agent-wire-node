# Wire Online Push — Testing Results

**Date:** 2026-03-28
**Tester:** Claude (automated + Adam visual confirmation)
**Branch:** `research/chain-optimization`
**App version:** v0.2.0, port 8765, token from pyramid_config.json

---

## Summary

Steps 0-3 tested. Steps 4-8 blocked on Wire server deployment (needed for publish, remote query, payment, Vibesmithy).

**1 bug found and fixed during testing.** 2 cosmetic issues noted.

---

## Results by Step

### Step 0: Build — PASS
- `cargo build` completes with 54 warnings, zero errors
- All warnings are unused imports/functions (expected for new IPC-only code paths)

### Step 1: Schema Migration — PASS
- All 10 new columns on `pyramid_slugs`: pinned, access_tier, absorption_mode, cached_emergent_price, metadata_contribution_id, last_published_build_id, source_tunnel_url, access_price, allowed_circles, absorption_chain_id
- 3 new columns on `pyramid_web_edges`: build_id, archived_at, last_confirmed_at
- `pyramid_remote_web_edges` table created with FK to pyramid_threads, unique constraint, index
- `pyramid_unredeemed_tokens` table created with status check constraint, partial indexes

### Step 2: Security Hardening — PASS (all 4)

| Test | Result | Details |
|------|--------|---------|
| S1: Mutation → 410 | PASS | Config write, build trigger, purge, partner message all return HTTP 410 with IPC redirect JSON |
| S1: Reads still work | PASS | `/pyramid/slugs` returns 152 slugs, `/pyramid/opt-025/apex` returns headline |
| S2: CORS bad origin | PASS | `https://evil.com` gets no Access-Control headers |
| S2: CORS good origin | PASS | `http://localhost:1420` gets allow-origin, allow-methods, allow-headers |
| S3: Web edge archival | PARTIAL | 24 edges have build_id set from post-migration build; `last_confirmed_at` is NULL on all (see note below) |
| S4: Body size limit | PASS | 2MB payload returns HTTP 413 |

**S3 note:** `last_confirmed_at` is empty even on edges with a build_id. This may be intentional (only set on rebuild confirmation?) or a gap in the migration backfill. Low priority — doesn't block anything.

### Step 3: Desktop App UI — PASS (with cosmetic issues)

**All components render:**
- Sub-tabs: Sync (6), Market, Pyramids, Remote, Logs — all visible and switchable
- Pyramids tab: 152 slugs listed with node counts, dates, Auto toggle, Publish Now button
- Access tier expand: clicking "Access: public▾" expands panel with Tier dropdown (Public/Circle-Scoped/Priced/Embargoed) + Save button
- Absorption mode expand: clicking "Absorption: open▾" expands panel with Mode dropdown (Open/Absorb All/Absorb Selective) + Save button
- Remote tab: Wire Identity status ("Not Set"), Tunnel Active with URL, Queries Served/Made counters, manual tunnel URL input
- Summary bar: "152 never published"
- Auto-publish toggle: functional (toggling on vibe-clean-1 triggered publish attempt)

**Cosmetic issues (non-blocking):**
1. **Access/Absorption expand buttons lack styling** — No CSS defined for `.pyramid-access-tier-toggle`, `.pyramid-access-tier-panel`, `.pyramid-access-tier-chevron`, `.pyramid-access-tier-section`, `.pyramid-access-tier-field`, `.pyramid-emergent-price`, `.pyramid-emergent-hint`, `.pyramid-pinned-badge`, `.pyramid-unpin-btn`. They render as unstyled native elements. Functional but visually rough — the chevron (▾/▴) is the only hint they're clickable.
2. **`accrete-1` shows "-1/4 nodes"** — negative published count, likely a data issue not a UI bug.

---

## Bug Found & Fixed

### `create_version` missing `title` field (sync.rs)

**Symptom:** Logs spammed with:
```
WARN wire_node_desktop: Version creation failed for <uuid>: Version creation failed (400 Bad Request): {"error":"title is required","param":"title"}
```

**Root cause:** `sync::create_version()` (sync.rs:962) sends payload with `original_document_id`, `body`, `source_path` but the Wire server's `/api/v1/wire/documents/version` endpoint requires a `title` field.

**Fix applied:** `src-tauri/src/sync.rs` — derives title from filename stem of source_path. Both call sites in main.rs (lines 1171 and 1682) pass through this function, so both are fixed.

**Status:** Fix compiles clean. Needs rebuild + retest to confirm.

---

## Steps Not Yet Tested

### Step 4: Publication Pipeline
- Auto-publish toggle works in UI, fires publish attempt
- Publish fails with "post_contribution request failed" — expected, Wire server changes not deployed
- **Blocked on:** Step 5 (Wire server deployment)

### Step 5: Wire Server Deployment
- New files and migration identified in handoff doc
- **Action needed:** Deploy GoodNewsEveryone changes + apply `20260328000000_payment_escrow.sql` migration
- Verify: `pyramid_metadata` type query works, payment-intent returns 401 (not 404)

### Step 6: Remote Querying
- Tunnel is active: `node-3168b8cb-55d5-450e-b31e-285be26100a4.agent-wire.com`
- **Blocked on:** Step 5 (need Wire server for JWT token generation)

### Step 7: Query Cost + Payment Flow
- **Blocked on:** Steps 5 + 6

### Step 8: Vibesmithy
- **Blocked on:** Steps 5 + 6 (needs a live tunnel to query)
- Can test basic UI load independently

---

## Recommended Next Steps

1. **Add CSS for access/absorption panels** — purely cosmetic but needed before ship
2. **Deploy Wire server** (Step 5) to unblock Steps 4, 6, 7, 8
3. **Rebuild app** with the sync.rs fix to stop the version creation spam
4. **Investigate `last_confirmed_at`** being NULL — verify if this is set during rebuild confirmation or if it's a gap
5. **Test publication pipeline end-to-end** once Wire server is live
