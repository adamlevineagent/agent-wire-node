# Wire Online Push -- Implementation Plan

**Date:** 2026-03-28
**Scope:** Everything needed to get pyramids live on the Wire, EXCEPT escrow/transfer/for-sale features.
**Execution order:** S (security hardening) + Schema Prep Migration (parallel) -> I (type registration) -> A (publication) -> B (discovery) -> C (remote query) -> then D/E/F/G/H/V in parallel (with noted dependency constraints).

---

## WS-ONLINE-S: Security Hardening (Prerequisite)

**Goal:** Harden the existing HTTP API surface before exposing it to remote access. Must complete before WS-ONLINE-C opens the tunnel to Wire JWT auth.

### S1: ALL Mutation Endpoints — Tauri IPC Only

Once Wire JWT auth opens the node to remote access (WS-ONLINE-C), every HTTP mutation endpoint becomes remotely exploitable. The maximal solution: **remote agents can only READ. All mutations are desktop-app-only via Tauri IPC.**

**Principle:** The HTTP API exposed through the tunnel is READ-ONLY for remote agents. Wire JWT auth gates read access. All state-changing operations require the local auth_token (which is never exposed remotely) or Tauri IPC.

**Endpoints to remove from HTTP (move to Tauri IPC only):**
- `POST /pyramid/config` — auth token rotation, credential theft
- `POST /pyramid/:slug/auto-update/config` — auto-update reconfiguration
- `POST /pyramid/:slug/auto-update/freeze` — disable staleness detection
- `POST /pyramid/:slug/auto-update/unfreeze` — re-enable
- `POST /pyramid/:slug/auto-update/l0-sweep` — triggers sweep
- `POST /pyramid/:slug/auto-update/breaker/resume` — circuit breaker
- `POST /pyramid/:slug/auto-update/breaker/build-new` — force rebuild
- `POST /pyramid/:slug/archive` — freeze a pyramid
- `DELETE /pyramid/:slug/purge` — CASCADE DELETE (most dangerous)
- `POST /pyramid/:slug/build` — triggers compute-consuming LLM calls
- `POST /pyramid/:slug/build/question` — same
- `POST /pyramid/:slug/build/preview` — triggers LLM decomposition
- `POST /pyramid/:slug/characterize` — triggers LLM characterization
- `POST /pyramid/:slug/ingest` — file ingestion
- `POST /pyramid/:slug/meta` — triggers LLM meta passes
- `POST /pyramid/:slug/crystallize` — triggers crystallization
- `POST /pyramid/vine/build` — triggers vine build
- `POST /pyramid/:slug/vine/rebuild-upper` — triggers vine rebuild
- `POST /pyramid/:slug/vine/integrity` — triggers integrity check
- `POST /pyramid/:slug/publish` — publishes to Wire
- `POST /pyramid/:slug/publish/question-set` — publishes question set
- `POST /pyramid/:slug/check-staleness` — triggers staleness pipeline
- `POST /pyramid/chain/import` — imports chain from Wire
- `POST /pyramid/slugs` — creates new slugs
- `POST /partner/message` — triggers LLM call (Partner/Dennis)
- `POST /partner/session/new` — creates session state
- `POST /auth/complete` — **CRITICAL: overwrites node auth state, accepts null Origin, must be IPC-only or removed entirely**

**Endpoints to auth-gate (currently unauthenticated, must require local auth_token):**
- `GET /stats` — leaks financial data
- `GET /tunnel-status` — leaks tunnel URL and metadata

**Endpoints that STAY on HTTP (read-only, Wire JWT accessible):**

**Endpoints that STAY on HTTP (read-only, Wire JWT accessible):**
- `GET /pyramid/:slug/apex` — read
- `GET /pyramid/:slug/drill/:node_id` — read
- `GET /pyramid/:slug/search` — read
- `GET /pyramid/:slug/entities` — read
- `GET /pyramid/:slug/export` — read (gated, rate limited)
- `GET /pyramid/:slug/query-cost` — read (cost preview)
- `GET /pyramid/:slug/absorption-config` — read-only config visibility
- `POST /trace/openrouter` — webhook ingestion (signature auth, not Wire JWT)
- `POST /pyramid/remote-query` — local-auth-only proxy for Vibesmithy
- `GET /health` — health check

**Files:**
- `src-tauri/src/pyramid/routes.rs` -- Remove all mutation routes from warp. Keep read-only routes. Add explicit route-level comments marking each as `// REMOTE-SAFE: read-only` or `// LOCAL-ONLY: Tauri IPC`.
- `src-tauri/src/main.rs` -- Add Tauri commands for all removed endpoints: `pyramid_update_config`, `pyramid_archive_slug`, `pyramid_purge_slug`, `pyramid_trigger_build`, `pyramid_trigger_question_build`, `pyramid_ingest`, `pyramid_auto_update_config`, `pyramid_auto_update_freeze`, `pyramid_auto_update_unfreeze`, `pyramid_breaker_resume`, `pyramid_breaker_build_new`.
- `src/components/modes/NodeMode.tsx` -- Verify all UI paths use Tauri invoke, not HTTP.
- `vibesmithy/src/lib/node-client.ts` -- Remove `deleteSlug` HTTP DELETE method (line 182). Once S1 moves mutations to IPC and S2 removes DELETE from CORS, this will break. Vibesmithy slug deletion should go through the local node's Tauri IPC, not HTTP.

### S2: CORS Tightening

The existing CORS config at `server.rs:288` uses `allow_any_origin()`. Once Wire JWT auth is added, any web page can make authenticated cross-origin requests using a leaked JWT. This is not acceptable for production.

**Maximal solution:** Replace `allow_any_origin()` with a configurable allowlist defaulting to `["http://localhost:*", "https://localhost:*"]`. Add the node's own Vibesmithy origin and the Wire server origin. The allowlist is stored in `pyramid_config.json` and editable only via Tauri IPC (per S1).

**Files:**
- `src-tauri/src/server.rs` -- Replace `allow_any_origin()` with `.allow_origins(config.cors_allowed_origins)`. Ensure `Authorization` header and `X-Payment-Token` header are in `.allow_headers(...)`. Restrict `.allow_methods(...)` to `GET, POST, OPTIONS` only (remove DELETE). **Also fix hardcoded `Access-Control-Allow-Origin: *` in document serve response headers (server.rs:438, 450)** — these bypass the CORS middleware and must use the same allowlist.
- `src-tauri/src/pyramid/mod.rs` -- Add `cors_allowed_origins: Vec<String>` to `PyramidConfig` with localhost defaults.

### S4: Request Body Size Limits

No `content_length_limit` exists on any warp endpoint. An attacker could POST arbitrarily large JSON bodies to consume node memory.

**Fix:** Add `warp::body::content_length_limit(1_048_576)` (1MB) to all POST endpoints as a default. Export responses may exceed this for large pyramids — the limit is on incoming request bodies only, not responses.

### S3: Existing Web Edge DELETEs (Pillar 1 + Pillar 38)

`db.rs:2120` has `delete_web_edges_for_depth` (hard DELETE) and `db.rs:2262` has `decay_web_edges` (DELETE below relevance threshold). Both violate Pillar 1. Per Pillar 38, fix all bugs when found.

**Fix:**
- `delete_web_edges_for_depth` → scope by build_id. New builds write new edges; old edges persist.
- `decay_web_edges` → mark edges with `archived_at` timestamp instead of deleting. Query filters exclude archived edges. Edge data preserved as historical record. **Also add `last_confirmed_at` guard** (prior audit finding m-07): edges should only decay if they haven't been confirmed by a recent build. Without this, valid edges on quiet pyramids decay to zero in ~20 cycles.
- **Backfill existing rows:** After migration adds `build_id` column (nullable for ALTER TABLE compatibility), run: `UPDATE pyramid_web_edges SET build_id = (SELECT MAX(build_id) FROM pyramid_nodes WHERE pyramid_nodes.slug = pyramid_web_edges.slug) WHERE build_id IS NULL;` This assigns existing edges to the latest build. All new code writes build_id on every insert (NOT NULL at application layer even though the column is nullable for migration reasons).
- **Rename misleading variable:** `webbing.rs:265` uses `archived` for what is currently a DELETE count. Rename to `deleted_count` until S3 lands, then rename to `archived_count` when archive-marking is implemented.
- **Update tests:** Replace the DELETE behavior test at `db.rs:4800-4874` with tests for build_id scoping and archive-marking.
- **`delete_web_edge_deltas`** (db.rs:2201) — also scope by build_id alongside the web edges fix (same pattern, same migration).

**Files:**
- `src-tauri/src/pyramid/db.rs` -- Add `build_id` and `archived_at` columns to `pyramid_web_edges` (in prep migration). Replace DELETE with build_id scoping. Replace decay DELETE with archive update. Run backfill. Scope `delete_web_edge_deltas` by build_id. Update tests.
- `src-tauri/src/pyramid/webbing.rs` -- Rename misleading `archived` variable.

### Dependencies
- None. This is independent prerequisite work.

### Acceptance Criteria
- Config changes only possible via Tauri IPC. HTTP `POST /pyramid/config` returns 404.
- CORS restricted to configured allowlist. `allow_any_origin` removed.
- No production DELETE on web edges. All scoped by build_id or archived.
- Existing functionality unaffected (config still works from desktop app UI).

### Complexity: Small-Medium

### Pillar Conformance
- Pillar 1 (everything is a contribution): Web edges no longer deleted.
- Pillar 38 (fix all bugs when found): Security and Pillar 1 violations addressed immediately.

---

## WS-ONLINE-I: Wire Server Type Registration

**Goal:** Register `pyramid_metadata`, `gap_report`, and `economic_parameter` as valid contribution types on the Wire server, and add a `type` filter to the query endpoint. This is a prerequisite for B, F, and the settlement layer.

### Mechanism

The Wire server's `contribute-core.ts` maintains a `VALID_TYPES` array that gates contribution ingest. Neither `pyramid_metadata` nor `gap_report` are currently in that list -- attempts to publish them will fail validation with a 400. Both must be added. The Wire query endpoint (`/api/v1/wire/query/route.ts`) has a `KNOWN_PARAMS` set that rejects unknown query parameters; `type` must be added so agents can filter contributions by type.

### Files to Modify

**GoodNewsEveryone (Wire server):**
- `src/lib/server/contribute-core.ts` -- Add `'pyramid_metadata'`, `'gap_report'`, and `'economic_parameter'` to the `VALID_TYPES` array. Add type-specific structured_data validation in `validateBody()`: `pyramid_metadata` requires `pyramid_slug`, `node_count`, `tunnel_url`; `gap_report` requires `target_handle_path`, `gap_description`; `economic_parameter` requires `parameter_name`, `value`, `effective_date`.
- `src/app/api/v1/contribute/route.ts` -- **NOTE: `contribute-core.ts` is currently dead code — nothing imports it.** The dedup is actually a refactor: route.ts has its own complete validation pipeline parallel to contribute-core's `processContribution()`. The fix: wire route.ts to USE contribute-core's `processContribution()` function (not just import the types array), making contribute-core the single canonical contribution processing path. Then add the three new types to contribute-core only.
- `src/app/api/v1/wire/query/route.ts` -- Add `'type'` to the `KNOWN_PARAMS` set. When `type` param is present, add a `.eq('type', type)` filter to the Supabase query.

### Dependencies
- None (this is independent server-side work).

### Acceptance Criteria
- `pyramid_metadata`, `gap_report`, and `economic_parameter` contributions accepted by the Wire contribute endpoint.
- Wire query endpoint accepts `?type=pyramid_metadata` and filters results accordingly.
- Invalid types still rejected with 400 and the updated `VALID_TYPES` list in the error message.
- `route.ts` imports VALID_TYPES from `contribute-core.ts` (single source of truth, no duplication).
- `validateBody()` has explicit `structured_data` validation branches for all three new types (following existing `pyramid_node` pattern). Malformed contributions rejected with 400.

### Complexity: Small

### Pillar Conformance
- Pillar 1 (everything is a contribution): pyramid metadata and gap reports are contributions, not special endpoints.

---

## WS-ONLINE-A: Pyramid Publication to Wire

**Goal:** Push pyramid nodes as Wire contributions using the existing publication pipeline, triggered automatically through a new sync timer and Node page integration.

### Mechanism

The publication pipeline already works (`publication.rs` + `wire_publish.rs`). What's missing is automatic triggering, the timer loop to drive it, a `last_published_build_id` column to track what's been published, and Node page integration.

A pyramid slug gets a "publication link" in the Node page, analogous to a folder link for corpus sync. A new `tokio::time::interval` timer (independent of the corpus sync timer) ticks on a configurable interval and calls `pyramid_sync_tick()`. The tick checks for unpublished or changed nodes by comparing the slug's current build_id against `last_published_build_id` in `pyramid_slugs`. If they differ, it triggers `publish_pyramid_bottom_up`. After successful publication, the tick writes the build_id to `last_published_build_id`.

Pyramid sync state is tracked in a new `PyramidSyncState` struct, kept separate from the corpus `SyncState` to avoid coupling concerns that are fundamentally different (corpus sync is file-level, pyramid sync is SQLite-level).

Source document registration must be unwired first. `register_corpus_document` in `publication.rs` is stubbed with placeholder UUIDs. The Wire corpus API at `/api/v1/wire/corpora/{slug}/documents` already accepts document uploads (used by `sync.rs` for corpus push). Wire the actual HTTP call so L0 derived_from entries carry real Wire UUIDs instead of v5 placeholders.

### Schema Changes

`last_published_build_id` column is added by the Phase 2.5 consolidated migration (runs before any workstream). No standalone migration needed here. `pyramid_sync_tick()` compares `last_published_build_id` against the current latest build_id from `pyramid_nodes` to determine if publication is needed. `publish_pyramid_bottom_up` writes the build_id here after success.

### Files to Modify

**agent-wire-node (Rust):**
- `src-tauri/src/pyramid/publication.rs` -- Replace `register_corpus_document` stub with actual HTTP call to Wire corpus API. Use `PyramidPublisher`'s HTTP client and auth token. After `publish_pyramid_bottom_up` completes, write the build_id to `last_published_build_id` via `set_last_published_build_id(conn, slug, build_id)`.
- `src-tauri/src/pyramid/sync.rs` (NEW) -- New file for pyramid-specific sync. Contains `PyramidSyncState` struct (linked_pyramids: HashMap<String, PyramidPublicationLink>, last_tick: Option<Instant>). Contains `pyramid_sync_tick()` that iterates linked pyramids, checks for unpublished nodes (build_id != last_published_build_id), and calls the publication orchestrator. Separate from corpus `SyncState` in `sync.rs`.
- `src-tauri/src/pyramid/db.rs` -- Add `last_published_build_id` column migration. Add `get_last_published_build_id(conn, slug)` and `set_last_published_build_id(conn, slug, build_id)` to compare/update against current latest build_id. Add `count_unpublished_nodes(conn, slug)` for sync status display.
- `src-tauri/src/lib.rs` -- Add a new `tokio::time::interval` timer loop for pyramid sync. This is NEW work -- no existing timer loop exists to piggyback on. The timer spawns a tokio task that ticks at a configurable interval (default 60s) and calls `pyramid_sync_tick()`. Configurable via pyramid settings.
- `src-tauri/src/pyramid/mod.rs` -- Add `pub mod sync;` declaration.

**agent-wire-node (Frontend):**
- `src/components/modes/NodeMode.tsx` -- Add a "Pyramids" sub-tab alongside "Sync", "Market", "Logs". This tab lists pyramid slugs with publication status.
- `src/components/PyramidPublicationStatus.tsx` (new) -- Shows each slug's publication state: total nodes, published count, last publish time, auto-publish toggle. "Publish Now" button for manual trigger.
- `src/components/SyncStatus.tsx` -- No changes needed; pyramid publication gets its own tab.

### Dependencies
- WS-ONLINE-I (Wire server must accept `pyramid_metadata` type before metadata can be published in B).

### Acceptance Criteria
- Source document registration makes real HTTP calls; L0 derived_from entries use Wire UUIDs.
- Pyramid publication configurable per-slug in Node page UI.
- Auto-publish triggers via the new tokio timer when new build completes (build_id differs from last_published_build_id).
- Manual "Publish Now" button works.
- Publication status visible: node count, published count, last publish timestamp.
- Incremental: only unpublished nodes are sent (idempotent resume already works).
- `last_published_build_id` persisted in `pyramid_slugs`, survives restart.
- Publication timer checks `build_status` before publishing — if a build is `in_progress`, skip the tick (prevents publishing incomplete pyramid state). Only publish when latest build is `completed` and its build_id differs from `last_published_build_id`.

### Complexity: Medium

### Pillar Conformance
- Pillar 1 (everything is a contribution): pyramid nodes publish as contributions.
- Pillar 3 (strict derived_from): evidence-weighted derived_from already implemented.
- Pillar 5 (immutability + supersession): republished nodes create new contributions via supersession.
- Pillar 31 (local is local, Wire is Wire): publication is explicit sync, not implicit coupling.
- Pillar 42 (always include frontend): Node page pyramid tab.

---

## WS-ONLINE-B: Discovery

**Goal:** Publish pyramid metadata as a Wire contribution so agents can discover pyramids alongside regular contributions.

### Mechanism

When a pyramid is published (WS-ONLINE-A), also publish (or supersede) a single "pyramid_metadata" contribution. Body is the apex node's distilled text. Structured_data contains:

```json
{
  "pyramid_slug": "opt-025",
  "node_count": 347,
  "max_depth": 4,
  "content_type": "code",
  "quality_score": 0.87,
  "tunnel_url": "https://abc123.cfargotunnel.com",
  "api_base": "/pyramid/opt-025",
  "apex_headline": "...",
  "topics": ["rust", "tauri", "knowledge-pyramid"],
  "last_build_at": "2026-03-28T14:00:00Z",
  "access_tier": "public",
  "access_price": null,
  "absorption_mode": "open"
}
```

The metadata includes access tier and pricing so queriers know the cost structure BEFORE connecting (Pillar 23 at the discovery level). Re-publish metadata when access tier or pricing changes.

Wire search already indexes contribution body and topics. The `type` filter added in WS-ONLINE-I lets agents filter for `pyramid_metadata` specifically via `/wire/query?type=pyramid_metadata`.

### Files to Modify

**agent-wire-node (Rust):**
- `src-tauri/src/pyramid/wire_publish.rs` -- Add `publish_pyramid_metadata()` method to `PyramidPublisher`. Takes slug metadata, tunnel_url, apex text. Posts as `type: "pyramid_metadata"` contribution. Tracks the metadata contribution's wire_uuid for supersession on re-publish.
- `src-tauri/src/pyramid/publication.rs` -- After `publish_pyramid_bottom_up` completes, call `publish_pyramid_metadata()`. Store the metadata contribution UUID in the slug's config or id_map for supersession on next publish.
- `src-tauri/src/pyramid/db.rs` -- Add `get_slug_metadata_contribution_id(conn, slug)` and `set_slug_metadata_contribution_id(conn, slug, uuid)`.

### Dependencies
- WS-ONLINE-A (publication pipeline must work first).
- WS-ONLINE-I (Wire server must accept `pyramid_metadata` type).
- Tunnel must be running for tunnel_url in metadata.

### Acceptance Criteria
- Publishing a pyramid also publishes/supersedes a pyramid_metadata contribution.
- Wire search with `type=pyramid_metadata` returns only pyramid listings.
- Metadata includes tunnel_url, node_count, max_depth, content_type, apex summary.
- Re-publishing supersedes the old metadata contribution (not duplicates).

### Complexity: Medium

### Pillar Conformance
- Pillar 1 (everything is a contribution): metadata is a contribution, not a special registry.
- Pillar 5 (immutability + supersession): re-publication supersedes, never overwrites.

---

## Schema Prep Migration (before Phase 1)

**Goal:** Add ALL new columns to `pyramid_slugs` in one migration before parallel workstreams start. Prevents migration collisions when D/E/F/G run in parallel.

### Schema Changes

```sql
-- Publication tracking (WS-ONLINE-A)
ALTER TABLE pyramid_slugs ADD COLUMN last_published_build_id TEXT DEFAULT NULL;

-- Pinning (WS-ONLINE-D)
ALTER TABLE pyramid_slugs ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;
ALTER TABLE pyramid_slugs ADD COLUMN source_tunnel_url TEXT DEFAULT NULL;

-- Access tiers (WS-ONLINE-E)
ALTER TABLE pyramid_slugs ADD COLUMN access_tier TEXT NOT NULL DEFAULT 'public';
ALTER TABLE pyramid_slugs ADD COLUMN access_price INTEGER DEFAULT NULL;
ALTER TABLE pyramid_slugs ADD COLUMN allowed_circles TEXT DEFAULT NULL;
-- NOTE: allowed_circles format is JSON array: '["circle-uuid-1","circle-uuid-2"]'

-- Discovery metadata tracking (WS-ONLINE-B)
ALTER TABLE pyramid_slugs ADD COLUMN metadata_contribution_id TEXT DEFAULT NULL;

-- Absorption config (WS-ONLINE-G)
ALTER TABLE pyramid_slugs ADD COLUMN absorption_mode TEXT NOT NULL DEFAULT 'open';
ALTER TABLE pyramid_slugs ADD COLUMN absorption_chain_id TEXT DEFAULT NULL;

-- Emergent pricing cache (WS-ONLINE-E)
ALTER TABLE pyramid_slugs ADD COLUMN cached_emergent_price INTEGER DEFAULT NULL;

-- Web edge build_id scoping (WS-ONLINE-S3)
ALTER TABLE pyramid_web_edges ADD COLUMN build_id TEXT DEFAULT NULL;
ALTER TABLE pyramid_web_edges ADD COLUMN archived_at TEXT DEFAULT NULL;

-- Remote web edges table (WS-ONLINE-F)
CREATE TABLE IF NOT EXISTS pyramid_remote_web_edges (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    slug TEXT NOT NULL,
    local_thread_id TEXT NOT NULL,
    remote_handle_path TEXT NOT NULL,
    remote_tunnel_url TEXT NOT NULL,
    relationship TEXT NOT NULL DEFAULT '',
    relevance REAL NOT NULL DEFAULT 1.0,
    delta_count INTEGER NOT NULL DEFAULT 0,
    build_id TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(slug, local_thread_id, remote_handle_path, build_id),
    FOREIGN KEY (slug, local_thread_id) REFERENCES pyramid_threads(slug, thread_id)
);
CREATE INDEX IF NOT EXISTS idx_remote_web_edges_slug ON pyramid_remote_web_edges(slug);
```

### Mechanism

A single idempotent migration function (`migrate_online_push_columns`) runs during `init_pyramid_db`. Uses `ALTER TABLE ... ADD COLUMN` with error suppression for "column already exists" and `CREATE TABLE IF NOT EXISTS` (standard SQLite migration patterns already used in the codebase). All columns have defaults, so existing rows are unaffected.

### Files to Modify

**agent-wire-node (Rust):**
- `src-tauri/src/pyramid/db.rs` -- Add `migrate_online_push_columns(conn)` function. Called from `init_pyramid_db()` after existing migrations.

### Dependencies
- Runs BEFORE Phase 1 (all workstreams depend on these columns/tables existing). Idempotent, safe to run on any DB state.

### Acceptance Criteria
- All columns present in `pyramid_slugs` after migration.
- Existing data unaffected (all columns have safe defaults).
- Migration is idempotent (safe to run multiple times).

### Complexity: Small

---

## WS-ONLINE-C: Remote Pyramid Querying

**Goal:** Agent discovers a pyramid on the Wire, connects via tunnel URL, queries the HTTP API.

### Mechanism

Agent flow:
1. Search Wire for `type=pyramid_metadata`, find a pyramid with tunnel_url.
2. Call `GET {tunnel_url}/pyramid/{slug}/apex` (or drill, search, etc.).
3. Auth: request includes a Wire identity token (JWT signed by Wire server). The serving node validates the JWT's issuer and checks the querier has credits.
4. Each query triggers a nano-transaction: 1 credit from querier to server node.

The pyramid API auth currently uses a local `auth_token` from `pyramid_config.json`. For remote access, the middleware needs a second auth path: Wire identity tokens validated via the Wire server's public key (already stored in `ServerState.jwt_public_key`).

The server already validates JWTs for document access tokens (`DocumentClaims` in `server.rs`). The pattern extends to pyramid queries.

CORS middleware must be added to the warp routes for pyramid endpoints to allow cross-origin requests from Vibesmithy and other web-based consumers.

### Files to Modify

**agent-wire-node (Rust):**
- `src-tauri/src/pyramid/routes.rs` -- Modify `with_auth_state` to accept either local auth_token OR Wire JWT. Add `with_wire_auth` filter that validates Wire JWT and extracts operator_id. Pyramid query handlers check: local token -> pass (free, no billing); Wire JWT -> pass (billable, trigger nano-tx). Add CORS middleware (`warp::cors()`) to all pyramid route filters: allow `Authorization` header, `Content-Type`, configurable allowed origins.
- `src-tauri/src/server.rs` -- Add `PyramidQueryClaims` struct (sub=operator_id, slug, query_type). **JWT audience-based dispatch:** All Wire-issued JWTs include an `aud` (audience) claim: `"document"` for document access, `"pyramid-query"` for pyramid queries, `"payment"` for payment tokens. The `verify_jwt` function reads `aud` first and routes to the correct claims struct (`DocumentClaims`, `PyramidQueryClaims`, or `PaymentTokenClaims`). Reject tokens with mismatched audience. This prevents document tokens from authenticating pyramid queries or vice versa. **Wire server protocol change:** The Wire server's token issuance endpoints must include the `aud` claim in all JWTs.
- `src-tauri/src/pyramid/query.rs` -- No changes needed; query functions are auth-agnostic.
- `src-tauri/src/credits.rs` -- Add `pyramid_queries_served` counter. Add `log_pyramid_query_serve(slug, query_type, operator_id)` for credit tracking.

**agent-wire-node (Rust, client side):**
- `src-tauri/src/pyramid/wire_import.rs` -- Add `RemotePyramidClient` struct. Takes tunnel_url + Wire auth token + Wire server URL. Methods: `remote_apex(slug)`, `remote_drill(slug, node_id)`, `remote_search(slug, query)`, `remote_export(slug)`. Each method integrates the payment flow: (1) call cost preview, (2) call Wire server payment-intent, (3) attach payment token to query, (4) return result. Uses `PaymentClient` from `server.rs` for Wire server calls and reqwest with Wire JWT + `X-Payment-Token` for serving node calls.

**GoodNewsEveryone (Wire server):**
- `src/app/api/v1/wire/query/route.ts` (or new endpoint) -- Add `POST /api/v1/wire/pyramid-query-token` that issues a short-lived JWT for pyramid queries. Takes target_node_id + slug, returns signed token. This is the equivalent of the document access token flow.
- `src/app/api/v1/node/tunnel/route.ts` (or equivalent) -- No changes if tunnel provisioning already works.

**agent-wire-node (Frontend — Pillar 42):**
- `src/components/modes/NodeMode.tsx` -- Add "Remote Connections" section to the Node page showing: Wire JWT status (valid/expired/missing), tunnel URL, remote query count, last remote query timestamp. Manual tunnel URL input field for testing remote queries before discovery (WS-ONLINE-B) lands.
- `src/components/RemoteConnectionStatus.tsx` (new) -- Compact status indicator: Wire identity connected (green/red), tunnel active (green/red), queries served count, queries made count.

### Dependencies
- WS-ONLINE-B (discovery provides tunnel_url).
- Wire server JWT infrastructure (exists for document tokens, needs pyramid query token variant).

### Acceptance Criteria
- Agent can query a remote pyramid's apex, drill, search, entities endpoints through the tunnel.
- Wire JWT validates on the serving node (issuer check, expiry check, public key verification).
- Local queries (same node, using local auth_token) remain free and unaffected.
- Remote queries logged in credit tracker with operator_id and query type.
- 401 response for expired/invalid/missing tokens.
- CORS headers present on all pyramid endpoint responses.
- Remote connection status visible in Node page UI (Pillar 42).
- Manual tunnel URL input available for testing before discovery lands.

### Complexity: Large

### Pillar Conformance
- Pillar 25 (platform agents use public API): remote queries go through the same HTTP API as local ones.
- Pillar 31 (local is local, Wire is Wire): auth path distinguishes local vs remote cleanly.
- Pillar 42 (always include frontend): remote connection status and manual tunnel input in Node page.

---

## WS-ONLINE-D: Daemon Caching / Pinning

**Goal:** "Pin this pyramid" pulls latest version to your local node. Uses a dedicated export endpoint for full tree retrieval.

### Mechanism

Pinning a remote pyramid means pulling its SQLite data into your local pyramid DB. This is not file-level sync (pyramids live in SQLite, not flat files). Instead:

1. Agent calls remote pyramid's `GET /pyramid/{slug}/export` to get the full node data (all nodes, edges, metadata in one response). This dedicated endpoint exists specifically for pinning -- the regular drill/search endpoints are for querying, not bulk export.
2. Nodes are inserted into local SQLite under the same slug with `pinned=true` flag.
3. The slug appears in the local slug list with a "pinned" badge.
4. The node serves the pinned pyramid from its own tunnel (earn server stamp credits).
5. Auto-buy subscription: periodic poll of the remote pyramid's metadata contribution for updated build timestamps. If newer, pull again.

The `PyramidSyncState` (from WS-ONLINE-A) extends with download-direction links for pinned pyramids.

### Export Endpoint

`GET /pyramid/:slug/export` returns the full node tree with all node data (id, depth, headline, distilled_text, children, topics, etc.). Gated behind Wire JWT auth (only authenticated Wire agents can export). Response is a JSON array of all nodes, sufficient for local reconstruction.

### Files to Modify

**agent-wire-node (Rust):**
- `src-tauri/src/pyramid/db.rs` -- Use `pinned` and `source_tunnel_url` columns from Phase 2.5 migration. Add `upsert_pinned_nodes(conn, slug, nodes)` that bulk-inserts/updates nodes from remote export response.
- `src-tauri/src/pyramid/slug.rs` -- Add `pin_remote_pyramid(slug, tunnel_url, nodes)` that creates a pinned slug and inserts nodes. Add `unpin_pyramid(slug)` that clears the `pinned` flag and `source_tunnel_url` but NEVER deletes node data (Pillar 1 — pinned data may have been queried, cited, or used as evidence; it persists as historical record).
- `src-tauri/src/pyramid/sync.rs` -- Add `PinnedPyramidLink` to `PyramidSyncState`. Auto-sync tick for pinned pyramids: poll remote metadata, compare build timestamp, re-pull if newer.
- `src-tauri/src/pyramid/routes.rs` -- Add `GET /pyramid/:slug/export` endpoint. Returns all node data for the slug. Gated behind Wire JWT auth.
- `src-tauri/src/pyramid/wire_import.rs` -- Add `pull_remote_pyramid(tunnel_url, slug)` that fetches export endpoint and returns nodes as `Vec<PyramidNode>`.

**agent-wire-node (Frontend):**
- `src/components/PyramidPublicationStatus.tsx` -- Show pinned pyramids with badge and source tunnel URL. "Unpin" button. "Refresh Now" button. Last-synced timestamp.
- `src/components/modes/NodeMode.tsx` -- Pinned pyramids appear in the Pyramids tab alongside local ones.

### Dependencies
- WS-ONLINE-C (remote querying must work for pull, CORS must be in place).
- WS-ONLINE-A (publication must work for re-serving pinned content).

### Acceptance Criteria
- `GET /pyramid/:slug/export` returns full node data, gated behind Wire JWT.
- "Pin pyramid" pulls full tree into local SQLite via the export endpoint.
- Pinned slug visible in slug list with "pinned" badge and source URL.
- Auto-refresh polls remote metadata, re-pulls on build_id change.
- Pinned pyramids servable from your own tunnel.
- Unpin clears pinned flag and sync link; node data always persists (Pillar 1).
- Local queries on pinned pyramids are free (no nano-tx).

### Complexity: Large

### Pillar Conformance
- Pillar 31 (local is local, Wire is Wire): pinning is explicit user action, not automatic coupling.
- Pillar 25 (re-serving attribution): pinned content credits original creator through rotator arm.

---

## WS-ONLINE-E: Access Tier Configuration

**Goal:** Per-slug access control: public (default), circle-scoped, priced, embargoed.

### Mechanism

Each pyramid slug gets an `access_tier` config stored in `pyramid_slugs` (columns added in Phase 2.5):
- **public** (default): any valid Wire JWT grants access. Cost = stamp only (1 credit p2p to server, no rotator arm, no UFF). This is the floor price for any remote action.
- **circle-scoped**: JWT must include a circle_id that matches the slug's `allowed_circles` JSON array. Same stamp cost.
- **priced**: stamp + additional access price routed through rotator arm with UFF splits. Defaults to **emergent pricing**: `cached_emergent_price` = count of unique citations across ALL nodes in the pyramid, computed and cached during build (NOT per-query — stored in `pyramid_slugs.cached_emergent_price`). Owner can override with explicit pricing curves. When no override is set, the cached citation count determines value. The cost preview (from WS-ONLINE-H) reads the cached value.
- **embargoed**: no remote access; local only.

**Subtractive work on access-restricted pyramids (Pillar 10):** Circle-scoped pyramids can be flagged by any Wire agent (flagging bypasses access tiers -- quality patrol must have infinitely elastic supply). The flag goes to a challenge panel; the panel sees the content regardless of access tier. Embargoed pyramids cannot be flagged by external agents (they're invisible), but the owner's own subtractive agents can still flag internally.

The auth middleware (from WS-ONLINE-C) checks access_tier before dispatching to query handlers.

### Files to Modify

**agent-wire-node (Rust):**
- `src-tauri/src/pyramid/db.rs` -- Use `access_tier`, `access_price`, and `allowed_circles` columns from Phase 2.5 migration. Add `get_access_tier(conn, slug)` and `set_access_tier(conn, slug, tier, price, circles)`. Add `compute_emergent_price(conn, slug)` that counts unique citations across all nodes.
- `src-tauri/src/pyramid/routes.rs` -- Add access tier check in the Wire JWT auth path. For circle-scoped: extract circle_id from JWT claims, check against allowed_circles. For embargoed: reject all Wire JWT requests. For priced: validate payment token (deferred detail -- initially same as public + nano-tx).
- `src-tauri/src/pyramid/slug.rs` -- Add `set_access_tier(conn, slug, tier, circles)` and `get_access_tier(conn, slug)`.

**agent-wire-node (Frontend):**
- `src/components/PyramidPublicationStatus.tsx` -- Add access tier dropdown per slug: Public, Circle-Scoped, Priced, Embargoed. Circle selector when circle-scoped is chosen. Price override field when priced is chosen (blank = emergent).

**GoodNewsEveryone (Wire server):**
- Pyramid query tokens (from WS-ONLINE-C) should include circle_id claims when the querier is a circle member. No new endpoints needed -- circle membership is already queryable.

### Dependencies
- WS-ONLINE-C (remote querying + Wire JWT).
- Circle system (exists on Wire server).

### Acceptance Criteria
- Per-slug access tier configurable in UI.
- Public pyramids accessible to any valid Wire JWT.
- Circle-scoped pyramids reject queries from non-members (403).
- Priced pyramids show emergent price (unique citation count) or owner-set override.
- Embargoed pyramids reject all remote queries.
- Access tier persisted in SQLite, survives restart.

### Complexity: Medium

### Pillar Conformance
- Pillar 10 (subtractive work): flagging bypasses access tiers for quality patrol.
- Pillar 12 (emergent value): emergent price derived from citation graph.

---

## WS-ONLINE-F: Cross-Node Understanding Webs

**Goal:** Web on Node A references a pyramid on Node B via Wire handle-path. The build runner resolves remote references through the tunnel.

### Mechanism

Today, web edges are same-slug, same-node. The existing `pyramid_web_edges` table uses FOREIGN KEY constraints on `pyramid_threads(slug, thread_id)` for both endpoints -- it cannot store references to remote threads that don't exist locally.

To go cross-node, a separate `pyramid_remote_web_edges` table is needed (created in Schema Prep Migration — see that section for authoritative SQL with `build_id TEXT NOT NULL` and `UNIQUE(slug, local_thread_id, remote_handle_path, build_id)`).

This keeps local edges FK-constrained (safe) while allowing remote edges to reference handle-paths that only exist on other nodes. During distillation, when the LLM emits a web edge note referencing a remote pyramid, `process_web_edge_notes` creates a remote edge with the handle-path and tunnel URL. The build runner resolves remote references by calling the remote pyramid's drill/node endpoint through the tunnel (via `RemotePyramidClient` from WS-ONLINE-C).

Evidence links use Wire handle-paths (already three-segment format: `slug/depth/node-id`).

Gap reports on remote pyramids are published to the Wire as contributions with type `gap_report` (registered in WS-ONLINE-I). The gap body describes what's missing. The remote pyramid owner sees demand signals in their gap feed.

### Files to Modify

**agent-wire-node (Rust):**
- `src-tauri/src/pyramid/db.rs` -- Table `pyramid_remote_web_edges` already created by Schema Prep Migration (with `build_id TEXT NOT NULL`). Add `get_remote_web_edges(conn, slug, build_id)`, `save_remote_web_edge(conn, edge)`. Remote web edges scoped by build_id — new builds write new edges, old edges persist. No DELETE function (Pillar 1).
- `src-tauri/src/pyramid/webbing.rs` -- Extend `process_web_edge_notes` to accept remote references. When `note.thread_id` contains a Wire handle-path (three-segment), create a remote edge in `pyramid_remote_web_edges` instead of a local one in `pyramid_web_edges`.
- `src-tauri/src/pyramid/build_runner.rs` -- When resolving evidence for a node with remote web edges, use `RemotePyramidClient` to fetch the referenced node's content. Cache remote node data locally for the build session.
- `src-tauri/src/pyramid/wire_publish.rs` -- Add `publish_gap_report(slug, remote_handle_path, gap_description)` that publishes a `gap_report` contribution to the Wire.

**agent-wire-node (Frontend):**
- Drill view should show remote web edges with a "remote" indicator and the source pyramid name. Clicking navigates to the remote pyramid (if accessible).

### Dependencies
- WS-ONLINE-C (remote querying for resolving references).
- WS-ONLINE-B (discovery for finding remote pyramids).
- WS-ONLINE-I (Wire server must accept `gap_report` type).

### Acceptance Criteria
- Remote web edges stored in separate `pyramid_remote_web_edges` table (no FK constraint on remote handle-paths).
- Local web edges remain in `pyramid_web_edges` with full FK integrity.
- Build runner resolves remote references through tunnel during build.
- Gap reports published as Wire contributions visible to remote pyramid owner.
- Remote web edges displayed in drill view with source attribution.
- Evidence links with remote handle-paths are valid and publishable.

### Complexity: Large

### Pillar Conformance
- Pillar 1 (everything is a contribution): gap reports are contributions.
- Pillar 3 (strict derived_from): remote evidence still carries proper derived_from links.

---

## WS-ONLINE-G: Owner Absorption Config

**Goal:** Pyramid owner configures how incoming understanding webs are handled.

### Mechanism

Three modes per slug (columns added in Phase 2.5):
- **open** (default): questioner owns the web they build. Standard flow.
- **absorb-all**: pyramid owner's node funds the web build. Web nodes list the owner as creator. Incoming webs automatically merge into the pyramid.
- **absorb-selective**: an action chain evaluates incoming webs. The chain decides accept/reject/modify. Only accepted webs merge.

### Files to Modify

**agent-wire-node (Rust):**
- `src-tauri/src/pyramid/db.rs` -- Use `absorption_mode` and `absorption_chain_id` columns from Phase 2.5 migration. Add `set_absorption_mode(conn, slug, mode, chain_id)` and `get_absorption_mode(conn, slug)`.
- `src-tauri/src/pyramid/slug.rs` -- Add `set_absorption_mode(conn, slug, mode, chain_id)` and `get_absorption_mode(conn, slug)`.
- `src-tauri/src/pyramid/build_runner.rs` -- When building a web on a remote pyramid with absorb mode, the request includes the owner's operator credentials. The owner's node executes the build and credits flow from the owner's pool.
- `src-tauri/src/pyramid/routes.rs` -- Add `GET /pyramid/:slug/absorption-config` (read-only, accessible via Wire JWT so remote agents can see the mode). **Mutations are Tauri IPC only** (consistent with S1 — no remote config mutation via HTTP). Remove any `POST` absorption-config route.
- `src-tauri/src/main.rs` -- Add `pyramid_set_absorption_mode(slug, mode, chain_id)` Tauri command.

**agent-wire-node (Frontend):**
- `src/components/PyramidPublicationStatus.tsx` -- Add absorption mode selector per slug: Open, Absorb All, Absorb Selective. Chain selector when selective is chosen. Uses Tauri invoke, not HTTP.

### Rate Limiting for Absorb-All

Absorb-all mode is a financial attack vector — external agents can trigger builds that drain the owner's credits. Required protections:
- Configurable rate limit per external operator (e.g., N builds per hour, default 3)
- Daily spend cap for absorb-all builds (default: 100 credits/day, owner-configurable)
- Both stored in `pyramid_config.json`, editable via Tauri IPC only
- Enforced before build starts (reject with 429 + retry-after if exceeded)

### Dependencies
- WS-ONLINE-F (cross-node webs must exist first).
- Chain executor (exists) for selective mode evaluation.

### Acceptance Criteria
- Per-slug absorption mode configurable via Tauri IPC and desktop UI only.
- `GET /pyramid/:slug/absorption-config` available via HTTP (read-only) for remote agents.
- Open mode: standard questioner-owns flow, no change.
- Absorb-all mode: owner's node funds web build, owner credited as creator. Rate limited per operator + daily spend cap.
- Absorb-selective mode: chain evaluates and accepts/rejects incoming webs.
- Absorption mode published in pyramid metadata (WS-ONLINE-B).

### Complexity: Small-Medium

### Pillar Conformance
- Pillar 1 (everything is a contribution): absorbed webs become contributions.
- Pillar 7 (UFF): absorption mode determines who earns creator credit.

---

## WS-ONLINE-H: Nano-Transaction Integration

**Goal:** Every remote pyramid query incurs a stamp (1-credit p2p fee to the server) plus optional access pricing routed through the rotator arm. The Wire server mediates ALL payments — no party self-reports charges.

### Two Economic Events Per Query

1. **Stamp (1 credit):** A flat 1-credit transfer from querier to serving node, processed by the Wire server (the Wire is the counterparty in all transactions). Every remote action costs at least 1 credit (~1/10,000th of a dollar). This is the minimum economic signal. Stamps are NOT routed through the rotator arm — the full 1 credit goes to the server (no UFF rake). Stamps are how the hosting/storage market works: serving costs the server bandwidth/compute, and 1 credit is the floor price for any action. The Wire processes the transfer but takes no cut.

2. **Access price (N credits, optional):** For priced pyramids (WS-ONLINE-E), an additional access fee is routed through the rotator arm with UFF splits (Creator 60%, Source 35%, Wire 2.5%, Graph Fund 2.5%). Public pyramids have access_price = 0 (stamp only). Emergent pricing (from citation count) or explicit owner-set pricing determines N.

**Handle-path resolution per query type:** The access price is routed through the rotator arm for a specific contribution:
- `apex` → the apex node's contribution handle-path
- `drill` → the drilled node's contribution handle-path
- `search` → the top-ranked result's contribution handle-path
- `export` → the pyramid_metadata contribution handle-path

### Wire-Mediated Payment (Trust Model)

**The Wire server coordinates all payments. Neither party self-reports.**

The querier initiates the purchase through the Wire server, not the serving node. This prevents fabrication: a malicious serving node cannot claim queries happened that didn't, and cannot inflate charges.

**Preview-then-commit (Pillar 23):**
1. Querier asks the serving node for a cost preview: `GET /pyramid/:slug/query-cost?query_type=drill&node_id=L2-003`
2. Serving node responds: `{ stamp: 1, access_price: N, total: N+1, slug: "opt-025", serving_node_id: "node-abc" }`
3. Querier decides to proceed. Querier calls the **Wire server**: `POST /api/v1/wire/payment-intent` with `{ amount: N+1, serving_node_id: "node-abc", contribution_handle_path: "...", query_type: "drill", slug: "opt-025" }`
4. Wire server validates querier balance, locks the credits, and issues a `payment_token` (Wire-signed JWT, 60s TTL, single-use nonce).
5. Querier sends the actual query to the serving node with `Authorization: Wire JWT` + `X-Payment-Token: {payment_token}`
6. Serving node validates the payment token (Wire-signed, not self-signed — validated via Wire server's public key), executes the query, and calls `POST /api/v1/wire/payment-redeem` with the token to collect payment.
7. Wire server verifies token is valid and unredeemed, then executes: stamp (1 credit p2p to serving node) + access price (N credits through rotator arm for the contribution).
8. Response includes transaction receipt (tx_id). Both querier and server can query their transaction history.

**Why Wire-mediated:** The serving node never decides how much to charge. The Wire server issues the payment token with the locked amount. The serving node redeems it — it can only collect what the Wire already authorized. Fabrication is impossible because the querier initiated the payment intent.

**Rate limiting:** Per-operator rate limits on both the serving node (100 queries/minute per operator_id) and the Wire server payment-intent endpoint (prevents nonce exhaustion and credit-lock spam).

**Failure handling:**
- Wire server unavailable during payment-intent → querier cannot proceed (fail-closed, not fail-open). No query executed, no debt.
- Wire server unavailable during payment-redeem → serving node logs the unredeemed token locally. Retries with exponential backoff (5 attempts). Wire server tracks issued-but-unredeemed tokens and expires them after TTL. Credits auto-unlock if unredeemed within TTL. No debt model needed — the Wire server is the authority.
- Serving node goes offline after query but before redeeming → querier's credits stay locked until TTL expires, then auto-unlock. No permanent loss.

**Re-serving attribution (Pillar 25):** When a pinner re-serves cached content, the payment-intent references the ORIGINAL contribution's handle-path. The rotator arm routes the access price to the original creator, not the re-server. The re-server earns the stamp (1-credit p2p). This means: original creator earns the UFF-routed access fee regardless of who serves; the server earns the stamp for hosting/bandwidth.

Local queries (same node, own pyramids, pinned pyramids) skip the payment flow entirely — no stamp, no access fee.

### Files to Modify

**agent-wire-node (Rust):**
- `src-tauri/src/credits.rs` -- Add `pyramid_queries_served: u64` and `pyramid_stamps_earned: u64` counters. Add `log_pyramid_tx(tx_id, querier_op_id, slug, query_type)`.
- `src-tauri/src/pyramid/routes.rs` -- Add `GET /pyramid/:slug/query-cost` endpoint (cost preview). In the Wire JWT auth path, validate `X-Payment-Token` header (Wire-signed, not self-signed) before executing query. After execution, call `POST /api/v1/wire/payment-redeem` to collect. Rate limit: 100 queries/minute per operator_id.
- `src-tauri/src/pyramid/db.rs` -- Add `pyramid_unredeemed_tokens` table for tracking tokens that need retry. Add `insert_unredeemed_token()`, `get_unredeemed_tokens()`, `mark_redeemed()`.
- `src-tauri/src/server.rs` -- Add `PaymentClient` that calls Wire server's payment-intent and payment-redeem endpoints. Validate Wire-signed payment tokens via Wire server's public key (already stored in `ServerState.jwt_public_key`).
- `src-tauri/src/pyramid/routes.rs` -- Add `POST /trace/openrouter` webhook ingestion endpoint. Validates `X-Webhook-Signature` header (HMAC-SHA256 with per-node secret provisioned during onboarding). Accepts OTLP trace data. Writes to local `pyramid_cost_ledger` table. Excluded from Wire JWT auth (uses its own webhook signature auth). When tunnel URL changes and metadata is superseded (WS-ONLINE-B), also update the webhook destination via OpenRouter Management API.
- `src-tauri/src/pyramid/db.rs` -- Add `pyramid_cost_ledger` table: `(id, api_key_hash, model, prompt_tokens, completion_tokens, cost_usd, latency_ms, generation_id, received_at)`. Add fallback: periodic polling of `GET /api/v1/keys/{hash}` for usage data in case webhooks are delayed.

**GoodNewsEveryone (Wire server):**
- `src/app/api/v1/wire/payment-intent/route.ts` (new) -- Accept payment intent. Validate querier balance. Lock credits. Issue Wire-signed payment_token JWT (60s TTL, single-use nonce, amount, serving_node_id, contribution_handle_path). Rate limited per operator.
- `src/app/api/v1/wire/payment-redeem/route.ts` (new) -- Accept payment token from serving node. Validate: Wire-signed, not expired, not already redeemed, nonce unused, **AND authenticated operator_id matches `serving_node_id` in the token** (prevents interception/replay by third parties). Execute: stamp (1 credit p2p to serving_node operator) + access price (N credits through rotator arm). Return tx_id. Auto-expire unredeemed tokens after TTL and unlock credits.
- `src/lib/server/credits.ts` (or equivalent) -- Add `lock_credits(operator_id, amount)`, `release_credits(operator_id, amount)` (for TTL expiry), `execute_stamp(from_op, to_op, amount)`, `execute_access_payment(from_op, contribution_handle_path, amount)`. Stamp is p2p (no UFF). Access payment goes through `walkChainAndPay` / rotator arm.

**Wire Server Escrow Infrastructure (new sub-workstream within H):**

The payment-intent/redeem flow requires a mini-escrow system on the Wire server. This is NEW infrastructure — the existing `credit-engine.ts` has no lock/hold/release mechanism. Estimated ~1-2 weeks of Wire server work within H's timeline.

New Wire server schema (Postgres):
```sql
-- Held credits column on operators
ALTER TABLE operators ADD COLUMN held_credits INTEGER NOT NULL DEFAULT 0;

-- Payment tokens
CREATE TABLE payment_tokens (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    querier_operator_id UUID NOT NULL REFERENCES operators(id),
    serving_node_operator_id UUID NOT NULL REFERENCES operators(id),
    contribution_handle_path TEXT NOT NULL,
    query_type TEXT NOT NULL,
    slug TEXT NOT NULL,
    stamp_amount INTEGER NOT NULL DEFAULT 1,
    access_amount INTEGER NOT NULL DEFAULT 0,
    total_amount INTEGER NOT NULL,
    nonce TEXT NOT NULL UNIQUE,
    issued_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    redeemed_at TIMESTAMPTZ DEFAULT NULL,
    status TEXT NOT NULL DEFAULT 'active' CHECK(status IN ('active', 'redeemed', 'expired'))
);
CREATE INDEX idx_payment_tokens_querier ON payment_tokens(querier_operator_id, status);
CREATE INDEX idx_payment_tokens_expiry ON payment_tokens(expires_at) WHERE status = 'active';
```

**Endpoint schemas:**

`POST /api/v1/wire/payment-intent` — Querier calls this to lock credits and get a payment token.
- Auth: Querier's Wire JWT
- Request: `{ amount: integer, serving_node_id: string, contribution_handle_path: string, query_type: string, slug: string }`
- Response (200): `{ payment_token: string (Wire-signed JWT), expires_at: string (ISO 8601), nonce: string }`
- Response (402): `{ error: "insufficient_balance", available: integer, required: integer }`
- Response (429): `{ error: "rate_limited", retry_after: integer }`

`POST /api/v1/wire/payment-redeem` — Serving node calls this to collect payment.
- Auth: Serving node's Wire JWT (operator_id must match `serving_node_operator_id` in token)
- Request: `{ payment_token: string }`
- Response (200): `{ tx_id: string, stamp_credited: integer, access_credited: integer }`
- Response (400): `{ error: "token_expired" | "token_already_redeemed" | "redeemer_mismatch" }`

New Wire server functions:
- `lock_credits(operator_id, amount)` — atomic: `UPDATE operators SET credit_balance = credit_balance - amount, held_credits = held_credits + amount WHERE id = ? AND credit_balance >= amount`. Returns false if insufficient.
- `release_credits(operator_id, amount)` — atomic: `UPDATE operators SET credit_balance = credit_balance + amount, held_credits = held_credits - amount WHERE id = ?`. For TTL expiry.
- `redeem_token(token_id, redeemer_operator_id)` — verify redeemer == serving_node_id, execute stamp (p2p transfer from held to server) + access payment (from held through `walkChainAndPay` / rotator arm), mark token redeemed.
- Cleanup cron: expire active tokens past TTL, release held credits. Runs every 30 seconds.
- Nonce generation: UUID v4 (cryptographically random).

All payment JWT tokens include `aud: "payment"` claim for dispatch (see JWT dispatch in WS-ONLINE-C).

**Note on export queries:** Export queries may take longer than 60s TTL. Redeem the token BEFORE executing the query (payment collected upfront, then serve response). This is a deliberate trust decision: the querier pays first, the serving node delivers. If the serving node fails to deliver, the querier's recourse is through the challenge panel (Pillar 24).

### Dependencies
- WS-ONLINE-C (remote querying + Wire JWT provides the operator IDs).
- WS-ONLINE-E (access_price needed for cost preview — priced tiers use emergent or explicit pricing from E).
- Wire server credit pool infrastructure + rotator arm (exists for contribution access via `walkChainAndPay`).

### Acceptance Criteria
- Every remote pyramid query triggers a Wire-mediated payment (stamp + optional access price).
- Stamp: 1-credit p2p transfer to serving node. Access price: UFF-routed through rotator arm.
- Payment flow is Wire-mediated: querier initiates payment-intent, Wire locks credits, serving node redeems.
- No party self-reports charges. Wire server is the sole authority on amounts.
- Cost preview available before commitment (Pillar 23).
- Payment token is Wire-signed (not self-signed), validated via Wire server's public key.
- Re-served content: original creator earns access fee via rotator arm, re-server earns stamp only.
- Local queries (own node, pinned) are free — no payment flow.
- Unredeemed tokens auto-expire, credits auto-unlock after TTL.
- Rate limiting on both serving node (per operator_id) and Wire server (per operator payment-intent).
- Credit counters updated on both querier and server nodes.
- Wire server rejects payment-intent when querier has insufficient balance (402).
- Transaction receipts logged for audit trail.

### Complexity: Large (Wire-mediated payment adds round-trips but eliminates trust assumptions)

### Pillar Conformance
- Pillar 7 (UFF): Access price flows through the rotator arm with proper Creator/Source/Wire/GraphFund splits. Stamp is p2p (hosting market economics).
- Pillar 8 (structural deflation): Every query costs at least 1 credit (stamp). Deflationary.
- Pillar 9 (integer economics): Rotator arm routes whole credits to single recipients per slot. Stamps are whole credits.
- Pillar 23 (preview-then-commit): Cost preview → payment intent → execution → redeem.
- Pillar 25 (platform agents use public API + re-serving attribution): Original creator earns access fee regardless of who serves. Re-server earns stamp.
- Pillar 9 (integer economics): Rotator arm routes whole credits to single recipients per slot.
- Pillar 23 (preview-then-commit): Cost preview before query execution.
- Pillar 25 (platform agents use public API + re-serving attribution): Original creator earns regardless of who serves.

---

## WS-ONLINE-J: Complete Prompt Externalization

**Goal:** Externalize all remaining hardwired prompts to `.md` files (Pillar 28 — recipe is a contribution). After this, ZERO LLM prompts remain in Rust source code.

### Prompts to Externalize

| File | Lines | `.md` file | Template variables | Impact |
|------|-------|-----------|-------------------|--------|
| `extraction_schema.rs` | 66-96 | `question/extraction_schema.md` | `{{audience}}`, `{{content_type}}`, `{{question_tree_summary}}` | Shapes how L0 evidence is framed for questions |
| `extraction_schema.rs` | 187-209 | `question/synthesis_prompt.md` | `{{audience}}`, `{{content_type}}`, `{{l0_results_summary}}`, `{{extraction_schema}}` | Generates the `{{synthesis_prompt}}` variable used in `answer.md` |
| `question_decomposition.rs` | 274-300 | `question/decompose_delta.md` | `{{audience}}`, `{{existing_questions}}`, `{{existing_answers}}` | Controls question reuse in multi-question accretion |
| `characterize.rs` | (fallback) | `question/characterize.md` | `{{folder_map}}`, `{{content_type}}` | Already has load mechanism, just needs the `.md` file created |

### Pattern

Same as WS11-G/H: load from `chains_dir/prompts/question/{name}.md`, use `render_prompt_template()` with `{{variable}}` convention, fall back to inline Rust string if file missing. Log a warning on fallback.

### Files to Modify

- `src-tauri/src/pyramid/extraction_schema.rs` — Extract both prompts, add file loading with fallback
- `src-tauri/src/pyramid/question_decomposition.rs` — Extract delta decomposition prompt
- `chains/prompts/question/extraction_schema.md` (NEW)
- `chains/prompts/question/synthesis_prompt.md` (NEW)
- `chains/prompts/question/decompose_delta.md` (NEW)
- `chains/prompts/question/characterize.md` (NEW)

### Dependencies
- None — can run in parallel with everything.

### Acceptance Criteria
- Zero LLM prompt text hardwired in Rust source (all load from `.md` files)
- Each `.md` file uses `{{variable}}` template convention
- Inline fallback with `warn!` log when file missing
- Researcher can edit all prompts without Rust rebuild

### Complexity: Small

### Pillar Conformance
- Pillar 28 (recipe is a contribution): All prompts are versioned, forkable `.md` files.
- Pillar 37 (never prescribe outputs): Prompts describe goals, not quotas. Verified during extraction.

---

## WS-ONLINE-V: Vibesmithy Wire Integration

**Goal:** Vibesmithy becomes the human-facing explorer for pyramids on the Wire -- discovery, remote browsing, question-asking, and pinning. This is the consumer side of what WS-ONLINE-A through H build on the producer side.

**Precondition:** This workstream lifts the Vibesmithy freeze. Understanding webs are the first consumer the freeze was waiting for -- this work IS the reason to unfreeze. The freeze was pending the unified chain architecture; understanding webs built on remote pyramids via the Wire are the first concrete consumer of question pyramids, which is the deliverable the freeze was gating on.

### What Already Exists

Vibesmithy already has:
- Space canvas with 3D marbles (the visualization)
- Dennis chat interface (the question-asking surface)
- Configurable node URL in settings (`useNodeConnection` hook, `node-client.ts`)
- Drill/search/apex navigation via the node API
- Settings page for connection config (`src/app/settings/page.tsx`)
- Types for PyramidNode, DrillResult, SearchHit, etc.

### What Needs Adding

**Discovery UI:**
- New page or panel: "Explore the Wire" -- search published pyramids by topic, handle, content type
- Calls Wire server search API (not local node) to find published pyramid metadata contributions
- Results show: apex summary, node count, quality score, handle-path, access tier
- Click -> connects to that pyramid's tunnel URL

**Remote connection mode:**
- `node-client.ts` already supports configurable base URL -- extend to accept tunnel URLs from discovery
- Wire JWT auth: when connecting to a remote pyramid, include the user's Wire JWT (from the logged-in session) in requests
- Connection indicator: show whether you're browsing a local pyramid or a remote one (and the latency)

**Understanding web building from Vibesmithy:**
- "Ask Dennis" on a remote pyramid -> creates a question slug LOCALLY that references the remote pyramid
- The local Wire Node builds the web, querying the remote pyramid's L0 through the tunnel
- The web lives locally (your analysis, your contribution) -- the remote pyramid just serves evidence
- Dennis shows the build progress, then displays the web in the space view

**Pinning:**
- "Pin this pyramid" button on remote pyramids
- Calls the export endpoint (from WS-ONLINE-D) to pull full node data and saves locally
- Auto-buy toggle: re-pull when the remote pyramid updates (subscription model)
- Pinned pyramids appear in the local pyramid list with a "pinned" badge

### Files to Modify

**vibesmithy:**
- `src/lib/node-client.ts` -- Add `setRemoteTarget(tunnelUrl)` method. Vibesmithy ALWAYS talks to the local node only. For remote pyramids, calls `POST {localNodeUrl}/pyramid/remote-query` with `{ tunnel_url, slug, action, params }`. The local node proxies the request to the remote pyramid with its own Wire JWT. **The Wire JWT never reaches the browser.** No `wireJwt` parameter in any client-side function.
- `src/lib/types.ts` -- Add `PyramidMetadata` type (from Wire discovery results). Add `connection_type: "local" | "remote"` to relevant state.
- `src/app/explore/page.tsx` (NEW) -- Wire discovery search page. Search input, results grid with pyramid cards.
- `src/app/space/[slug]/page.tsx` -- Already works with any node URL. Just needs the connection source to switch between local and remote.
- `src/components/chat/ChatPanel.tsx` -- "Ask Dennis" triggers question slug creation on local node, referencing the remote pyramid. Needs to call local Wire Node API (not the remote pyramid) for the build.
- `src/app/settings/page.tsx` -- Add "Wire Connection" section alongside existing node URL config. Shows Wire login status, connected pyramids, pinned pyramids.
- `src/components/PyramidCard.tsx` (NEW or extend existing) -- Card for discovery results showing apex, stats, access tier, "Pin" and "Ask" buttons.

**agent-wire-node:**
- `src-tauri/src/pyramid/routes.rs` -- `POST /pyramid/:slug/pin` endpoint (uses the export endpoint from WS-ONLINE-D internally). Add `POST /pyramid/remote-query` proxy endpoint (see spec below). Authenticated via local auth_token only (not Wire JWT — local-node-only).
- `src-tauri/src/main.rs` -- Add `pyramid_pin_remote(tunnel_url, slug)` Tauri command.

**`POST /pyramid/remote-query` proxy specification:**

Request: `{ tunnel_url: string, slug: string, action: "apex"|"drill"|"search"|"entities"|"export", params: object }`
Auth: local auth_token only. Rate limited (configurable, default 60/minute — prevents accidental credit drain from bugs).

The proxy handles the full payment flow transparently:
1. Call `GET {tunnel_url}/pyramid/{slug}/query-cost?query_type={action}` for cost preview
2. If `access_price > 0` (priced pyramid) AND no `X-Confirm-Payment: true` header in request:
   - Return `402 Payment Required` with `{ stamp: 1, access_price: N, total: N+1, slug, serving_node_id }`
   - Vibesmithy shows cost to user, re-sends with `X-Confirm-Payment: true` to proceed
3. If confirmed (or public/stamp-only): call Wire server `POST /api/v1/wire/payment-intent` with amount + serving_node_id + contribution_handle_path
4. Wire server locks credits, returns payment_token
5. Forward query to `{tunnel_url}/pyramid/{slug}/{action}` with Wire JWT + `X-Payment-Token`
6. Return result to Vibesmithy
7. (Serving node redeems token with Wire server independently)

Handle-path resolution: `apex` → apex contribution, `drill` → drilled node, `search` → pyramid_metadata contribution (since top result unknown at intent time), `export` → pyramid_metadata contribution.

Error states returned to Vibesmithy:
- `402` — priced, needs confirmation
- `403` — circle-scoped, not a member
- `451` — embargoed
- `502` — tunnel unreachable (show last-known state if pinned)
- `503` — Wire server unavailable (payment-intent failed)

### Dependencies
- WS-ONLINE-C (remote querying -- provides the tunnel + JWT infrastructure)
- WS-ONLINE-B (discovery metadata -- provides what Vibesmithy searches for)
- WS-ONLINE-D (pin endpoint -- V's pin feature depends on D's export endpoint)
- Vibesmithy can START with manual tunnel URL entry (no discovery) and add discovery UI once WS-ONLINE-B lands

### Acceptance Criteria
- Connect to a remote pyramid by entering its tunnel URL
- Browse remote pyramid: drill, search, space view all work
- "Ask Dennis" on a remote pyramid builds a local understanding web
- Pin a remote pyramid locally (via the export endpoint from D)
- Wire discovery search finds published pyramids (once WS-ONLINE-B lands)
- Connection type indicator visible (local vs remote)

### Complexity: Medium-Large (spans both repos)

### Pillar Conformance
- Pillar 31 (Local is local, Wire is Wire): Local pyramids and remote pyramids coexist. Pinning is explicit. No automatic syncing without user action.
- Pillar 42 (Always include frontend): This IS the frontend. Vibesmithy is the human interface to the Wire's knowledge topology.

---

## Execution Order

```
Prerequisites (parallel):
  WS-ONLINE-S (security hardening)    ~1 week
  Schema Prep Migration               ~1 day

Phase 1 (sequential, after prerequisites):
  WS-ONLINE-I (type registration)  ~1 day
    -> WS-ONLINE-A (publication)  ~2 weeks
      -> WS-ONLINE-B (discovery)  ~1 week

Phase 2:
  WS-ONLINE-C (remote query + JWT)  ~2-3 weeks

Phase 3 (parallel, after C, with noted constraints):
  WS-ONLINE-D (pinning + export endpoint)  ~2 weeks
  WS-ONLINE-E (access tiers)               ~1 week
  WS-ONLINE-F (cross-node webs)            ~2-3 weeks
  WS-ONLINE-G (absorption)                 ~1 week  [after F]
  WS-ONLINE-H (nano-tx + settlement)       ~3-4 weeks  [after E -- needs access_price]
  WS-ONLINE-J (prompt externalization)     ~1-2 days  [no dependencies, can run anytime]
  WS-ONLINE-V (Vibesmithy)                 ~2-3 weeks  [pin feature after D's export endpoint]
    (can start with manual URL before B lands; lifts Vibesmithy freeze)
```

**Dependency graph (Phase 3):**
```
D (pinning) -----> V (Vibesmithy pin uses D's export endpoint)
E (access tiers) -> H (nano-tx needs access_price from E)
F (cross-node webs) -> G (absorption needs webs)
```

Total estimated duration: 6-8 weeks with focused execution, workstreams D-H+V parallelized where dependencies allow.

---

## File Index

Key files referenced across workstreams:

**agent-wire-node (Rust):**
- `/src-tauri/src/pyramid/publication.rs` -- Bottom-up publication orchestrator
- `/src-tauri/src/pyramid/wire_publish.rs` -- Wire HTTP publisher client
- `/src-tauri/src/pyramid/routes.rs` -- Pyramid HTTP API route handlers
- `/src-tauri/src/pyramid/db.rs` -- SQLite schema and CRUD
- `/src-tauri/src/pyramid/query.rs` -- Query functions (apex, drill, search)
- `/src-tauri/src/pyramid/webbing.rs` -- Web edge processing
- `/src-tauri/src/pyramid/wire_import.rs` -- Wire chain/question set import client
- `/src-tauri/src/pyramid/build_runner.rs` -- Build execution orchestrator
- `/src-tauri/src/pyramid/slug.rs` -- Slug management
- `/src-tauri/src/pyramid/sync.rs` (NEW) -- Pyramid-specific sync state and timer logic
- `/src-tauri/src/pyramid/mod.rs` -- Module declarations and PyramidState
- `/src-tauri/src/sync.rs` -- Corpus sync engine (pattern reference, not extended)
- `/src-tauri/src/tunnel.rs` -- Cloudflare tunnel management
- `/src-tauri/src/server.rs` -- HTTP server state and JWT validation
- `/src-tauri/src/credits.rs` -- Credit tracking (under pyramid/ conceptually, lives at crate root)
- `/src-tauri/src/market.rs` -- Market daemon
- `/src-tauri/src/lib.rs` -- Tauri command wiring + new tokio timer for pyramid sync

**vibesmithy:**
- `/src/lib/node-client.ts` -- Node API client (configurable URL)
- `/src/lib/types.ts` -- Shared TypeScript types
- `/src/app/space/[slug]/page.tsx` -- Space view (3D marble canvas)
- `/src/app/settings/page.tsx` -- Connection settings
- `/src/components/chat/ChatPanel.tsx` -- Dennis chat interface

**agent-wire-node (Frontend):**
- `/src/components/modes/NodeMode.tsx` -- Node page with sync/market/logs tabs
- `/src/components/SyncStatus.tsx` -- Corpus sync status UI
- `/src/components/PyramidPublicationStatus.tsx` (new) -- Pyramid publication and pinning UI

**GoodNewsEveryone (Wire server):**
- `/src/app/api/v1/contribute/route.ts` -- Contribution ingest
- `/src/lib/server/contribute-core.ts` -- Contribution validation and storage
- `/src/app/api/v1/wire/circles/` -- Circle system endpoints
- `/src/app/api/v1/wire/query/route.ts` -- Wire query endpoint (add `type` filter)
- `/src/app/api/v1/wire/payment-intent/route.ts` (new) -- Payment intent, credit locking
- `/src/app/api/v1/wire/payment-redeem/route.ts` (new) -- Payment redemption, stamp + access execution
