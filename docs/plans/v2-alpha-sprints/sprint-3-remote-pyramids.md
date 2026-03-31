# Sprint 3 — Remote Pyramid Access

## Context

Sprints 1-2 make the intent bar intelligent and close the chain-as-contribution loop. But everything runs locally — the user must have a Wire Node with source material and LLM keys. Sprint 3 enables querying pyramids hosted on OTHER operators' nodes through the Wire network.

This is the prerequisite for Vibesmithy standalone (Sprint 5) and auto-fulfill with helpers (Sprint 4).

## What Already Exists (verified by audit)

The Wire Online push (phases C, H, V) built most of the infrastructure. Sprint 3 is primarily an **integration and frontend sprint**, not a greenfield build.

### Fully Built
- **Wire server: `POST /api/v1/wire/pyramid-query-token`** — issues JWT with `aud: "pyramid-query"`, contains slug/query_type/target_node_id. Does NOT check credits or return tunnel URL (those are separate concerns).
- **Wire server: `POST /api/v1/wire/payment-intent`** — locks credits, splits stamp (1 credit) + access price, returns JWT with `aud: "payment"`. Rate limited 100/min per operator.
- **Wire server: `POST /api/v1/wire/payment-redeem`** — verifies payment JWT, transfers credits to serving node.
- **Wire Node: JWT verification** — `verify_pyramid_query_jwt()` and `verify_payment_token()` in server.rs. Two separate JWT types, two separate verifiers.
- **Wire Node: Dual-auth on pyramid routes** — `with_dual_auth()` accepts either local auth_token OR Wire JWT on all read-only endpoints. Rate limited 100/min per operator for JWT auth. Access tier checking (public/circle/priced/embargoed).
- **Wire Node: Remote query proxy** — `POST /pyramid/remote-query` in routes.rs. Accepts `{ tunnel_url, slug, action, params }`, forwards to remote node. Local-auth-only, rate limited 60/min.
- **Wire Node: Tunnel** — Cloudflare tunnel provisioning and management, exposes localhost:8765.
- **Wire Node: Publication** — `pyramid_publish` and `pyramid_pin_remote` Tauri commands.
- **Payment escrow** — `payment_tokens` table, lock/release/redeem/expire functions in payment-escrow.ts.

### Partially Built (needs wiring)
- **Payment enforcement points** — Marked with `### WS-ONLINE-H ENFORCEMENT POINT ###` comments in routes.rs but NOT activated. `validate_payment_token` filter exists but has `#[allow(dead_code)]`. Sprint 3 activates these.
- **Remote query proxy auth** — Currently sends the local `config.auth_token` as the "Wire JWT" to remote nodes (BUG — this is a meaningless token). Must call `pyramid-query-token` to get a real Wire JWT.

### Not Built
- **Tunnel URL discovery** — The pyramid-query-token JWT does NOT contain the tunnel URL. The querier must discover it from Wire search (pyramid metadata contribution contains tunnel_url).
- **Frontend integration** — Search tab doesn't show published pyramids. Understanding tab doesn't show remote pyramids. Planner doesn't know about remote pyramids.

## Two-Token Architecture (critical to understand)

Remote pyramid queries use TWO separate JWTs:

1. **Pyramid Query JWT** (`aud: "pyramid-query"`) — Authentication. Proves the querier is authorized to access this pyramid. Obtained from `POST /api/v1/wire/pyramid-query-token`. Contains: slug, query_type, operator_id.

2. **Payment JWT** (`aud: "payment"`) — Economics. Locks credits and authorizes billing. Obtained from `POST /api/v1/wire/payment-intent`. Contains: amount, serving_node_operator_id, contribution_handle_path.

The serving node verifies BOTH tokens. The query JWT goes in the Authorization header. The payment JWT goes in the `X-Payment-Token` header.

## What's Actually Missing (Sprint 3 scope)

1. **Fix remote query proxy auth** — Replace local auth_token with real Wire JWT from pyramid-query-token endpoint
2. **Wire payment-intent into remote query proxy** — Call payment-intent, pass payment token via X-Payment-Token header
3. **Activate payment enforcement** — Enable the existing enforcement points in routes.rs
4. **Tunnel URL discovery** — Look up tunnel URL from Wire search pyramid metadata, not from JWT
5. **Frontend: Search discovers pyramids** — Show published pyramids in Search results
6. **Frontend: Understanding shows remote pyramids** — Cache query results, show "remote" badge
7. **Frontend: Planner awareness** — Planner knows about remote pyramids, can suggest querying them
8. **Frontend: Cost disclosure** — Show stamp + access price before remote query (Pillar 23)

---

## Phase 1: Fix Remote Query Proxy (Rust)

The existing `POST /pyramid/remote-query` proxy in routes.rs has a critical auth bug. Fix it:

1. **Before forwarding to remote node:**
   - Call `POST /api/v1/wire/pyramid-query-token` with `{ slug, query_type, target_node_id }` → get pyramid-query JWT
   - Call `POST /api/v1/wire/payment-intent` with `{ amount, serving_node_id, contribution_handle_path, query_type, slug }` → get payment JWT
   - Send both JWTs to remote node (Authorization + X-Payment-Token headers)

2. **After receiving response:**
   - The remote node calls `POST /api/v1/wire/payment-redeem` to collect payment
   - (This happens on the serving side, not the querier side)

3. **Tunnel URL discovery:**
   - The frontend provides `tunnel_url` from Wire search results (pyramid metadata contains it)
   - The proxy does NOT need to discover it — the frontend already has it

4. **Add `pyramid_remote_query` Tauri command** — wraps the HTTP proxy for desktop app IPC

**Files:**
- `src-tauri/src/pyramid/routes.rs` — fix handle_remote_query auth flow
- `src-tauri/src/main.rs` — add pyramid_remote_query Tauri command + register in generate_handler![]

### Phase 1b: Activate Payment Enforcement

Enable the existing enforcement points:
- Remove `#[allow(dead_code)]` from `validate_payment_token` filter
- Wire the filter into paid pyramid query routes
- The enforcement points are already marked with `### WS-ONLINE-H ENFORCEMENT POINT ###`

**Files:**
- `src-tauri/src/pyramid/routes.rs` — activate enforcement at marked points

---

## Phase 2: Frontend — Pyramid Discovery in Search

Show published pyramids in Wire Search results:

1. Wire query already supports `type` filter — use `type=pyramid` or `topics=pyramid`
2. Search results include pyramid metadata (slug, node_count, tunnel_url, access tier, price)
3. Each pyramid result shows: title, node count, access tier (public/priced), serving operator
4. **Cost disclosure (Pillar 23):** Show stamp (1 credit) + access price before query. User confirms before any credits are spent.
5. "Query this pyramid" button → triggers remote query flow

**Files:**
- `src/components/modes/SearchMode.tsx` — add pyramid discovery results
- `src/components/planner/PlanWidgets.tsx` — cost disclosure widget for remote queries

---

## Phase 3: Frontend — Remote Pyramids in Understanding

When a user queries a remote pyramid, cache the results locally and display them:

1. Remote pyramid appears in Understanding with "Remote" badge
2. Cached apex, drill, search results available offline
3. Show serving operator and last-queried timestamp
4. Re-query button (costs another stamp + access)

**Files:**
- `src/components/PyramidDashboard.tsx` — remote pyramid display with badge

---

## Phase 4: Planner Awareness

Update the planner to suggest remote pyramids:

1. During context gathering, include discovered remote pyramids from recent searches
2. Planner can suggest: "A battery chemistry pyramid exists on the Wire. Query cost: dynamic (governor-adjusted). Build your own: ~200 credits."
3. New vocabulary command: `query_remote_pyramid` with params `{ tunnel_url, slug, action, params }`

**Files:**
- `chains/prompts/planner/planner-system.md` — add remote pyramid awareness to context section
- `chains/vocabulary_yaml/pyramid_explore.yaml` — add query_remote_pyramid command

---

## Reconciliation Notes

- **Query type vocabulary:** Wire server allows `apex/drill/search/export/entities`. Node allows `apex/drill/search/entities/export/tree`. Reconcile to a single source of truth.
- **JWT TTL:** Both tokens have 60-second TTL. If user confirmation is required (Pillar 23), this may be too short. Consider extending to 120s or implementing a re-acquire flow.
- **CORS for Sprint 5:** localhost-only CORS in server.rs will block browser-direct access from Vibesmithy standalone. Flag as Sprint 5 dependency, not Sprint 3.
- **Rotator arm verification:** Verify `payment-escrow.ts:redeemToken()` routes through the 80-slot rotator arm per Pillar 9, not a direct credit transfer.

---

## Implementation Order

| Phase | What | Size | Depends on |
|-------|------|------|-----------|
| 1 | Fix remote query proxy auth + payment wiring | Medium | None |
| 1b | Activate payment enforcement | Small | Phase 1 |
| 2 | Search discovers pyramids + cost disclosure | Medium | Phase 1 |
| 3 | Understanding shows remote pyramids | Small | Phase 1 |
| 4 | Planner awareness | Small | Phase 2 |

---

## Verification

1. User A publishes a pyramid → appears in User B's Wire Search with access tier and price
2. User B sees cost disclosure (stamp + access price) → confirms → query executes → results display
3. User B's Understanding tab shows the remote pyramid with "Remote" badge
4. Payment: User B's credits decrease, User A's credits increase (via payment-redeem)
5. Planner suggests remote pyramid when local one doesn't exist
6. Auth flow: real Wire JWT used (not local auth_token)
7. Payment enforcement: unpaid queries to priced pyramids are rejected (402)
8. `cargo check` + `npx tsc --noEmit` pass

## Audit Trail

**Stage 1 pre-implementation audit (2 auditors, ground-truth verification):**
- Phase 1 mischaracterized pyramid-query-token → rewritten with accurate description
- Phase 3 (JWT on pyramid routes) already fully implemented → removed, replaced with payment enforcement activation
- Phase 4 partially built → rescoped to fix auth bug + payment wiring only
- Two-token architecture → explicit section added explaining both JWTs
- auth_token bug → documented as critical fix in Phase 1
- Payment enforcement disabled → Phase 1b activates existing enforcement points
- Wrong planner prompt path → corrected
- Pillar 9 rotator arm → flagged for verification
- Pillar 23 preview-then-commit → cost disclosure added to Phase 2
- Pillar 42 frontend/UX → UX integrated into each phase, not deferred
