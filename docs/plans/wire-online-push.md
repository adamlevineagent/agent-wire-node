# Wire Online Push -- Implementation Plan

**Date:** 2026-03-28
**Scope:** Everything needed to get pyramids live on the Wire, EXCEPT escrow/transfer/for-sale features.
**Execution order:** A (publication) -> B (discovery) -> C (remote query) -> then D/E/F/G/H in parallel.

---

## WS-ONLINE-A: Pyramid Publication to Wire

**Goal:** Push pyramid nodes as Wire contributions using the existing publication pipeline, triggered automatically through the Node page sync mechanism.

### Mechanism

The publication pipeline already works (`publication.rs` + `wire_publish.rs`). What's missing is automatic triggering and Node page integration. The corpus sync system in `sync.rs` provides the pattern: linked folders with direction, auto-sync interval, diff-based push/pull. Pyramids piggyback on this.

A pyramid slug gets a "publication link" in the Node page, analogous to a folder link for corpus sync. The sync loop checks for unpublished or changed nodes (via build_id comparison against last-published build_id) and triggers `publish_pyramid_bottom_up`.

Source document registration must be unwired first. `register_corpus_document` in `publication.rs` is stubbed with placeholder UUIDs. The Wire corpus API at `/api/v1/wire/corpora/{slug}/documents` already accepts document uploads (used by `sync.rs` for corpus push). Wire the actual HTTP call so L0 derived_from entries carry real Wire UUIDs instead of v5 placeholders.

### Files to Modify

**agent-wire-node (Rust):**
- `src-tauri/src/pyramid/publication.rs` -- Replace `register_corpus_document` stub with actual HTTP call to Wire corpus API. Use `PyramidPublisher`'s HTTP client and auth token.
- `src-tauri/src/sync.rs` -- Add `PyramidPublicationLink` struct (slug, direction=Upload, auto_publish enabled/interval). Add `linked_pyramids: HashMap<String, PyramidPublicationLink>` to `SyncState`. Add `pyramid_sync_tick()` that checks for unpublished nodes and calls the publication orchestrator.
- `src-tauri/src/pyramid/db.rs` -- Add `get_last_published_build_id(conn, slug)` to compare against current latest build_id. Add `count_unpublished_nodes(conn, slug)` for sync status display.
- `src-tauri/src/lib.rs` -- Wire pyramid sync tick into the existing auto-sync timer loop (same interval, or independent config).

**agent-wire-node (Frontend):**
- `src/components/modes/NodeMode.tsx` -- Add a "Pyramids" sub-tab alongside "Sync", "Market", "Logs". This tab lists pyramid slugs with publication status.
- `src/components/PyramidPublicationStatus.tsx` (new) -- Shows each slug's publication state: total nodes, published count, last publish time, auto-publish toggle. "Publish Now" button for manual trigger.
- `src/components/SyncStatus.tsx` -- No changes needed; pyramid publication gets its own tab.

### Dependencies
- None (this is the starting point).

### Acceptance Criteria
- Source document registration makes real HTTP calls; L0 derived_from entries use Wire UUIDs.
- Pyramid publication configurable per-slug in Node page UI.
- Auto-publish triggers when new build completes (build_id changes since last publish).
- Manual "Publish Now" button works.
- Publication status visible: node count, published count, last publish timestamp.
- Incremental: only unpublished nodes are sent (idempotent resume already works).

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
  "last_build_at": "2026-03-28T14:00:00Z"
}
```

Wire search already indexes contribution body and topics. Adding a `type` filter for `pyramid_metadata` in the search endpoint lets agents filter for pyramids specifically.

### Files to Modify

**agent-wire-node (Rust):**
- `src-tauri/src/pyramid/wire_publish.rs` -- Add `publish_pyramid_metadata()` method to `PyramidPublisher`. Takes slug metadata, tunnel_url, apex text. Posts as `type: "pyramid_metadata"` contribution. Tracks the metadata contribution's wire_uuid for supersession on re-publish.
- `src-tauri/src/pyramid/publication.rs` -- After `publish_pyramid_bottom_up` completes, call `publish_pyramid_metadata()`. Store the metadata contribution UUID in the slug's config or id_map for supersession on next publish.
- `src-tauri/src/pyramid/db.rs` -- Add `get_slug_metadata_contribution_id(conn, slug)` and `set_slug_metadata_contribution_id(conn, slug, uuid)`.

**GoodNewsEveryone (Wire server):**
- `src/app/api/v1/wire/search/route.ts` (or equivalent search handler) -- Add optional `type` filter param. When `type=pyramid_metadata`, filter contributions to that type.
- `src/lib/server/contribute-core.ts` -- Validate `pyramid_metadata` contribution type (ensure structured_data has required fields: pyramid_slug, node_count, tunnel_url).

### Dependencies
- WS-ONLINE-A (publication pipeline must work first).
- Tunnel must be running for tunnel_url in metadata.

### Acceptance Criteria
- Publishing a pyramid also publishes/supersedes a pyramid_metadata contribution.
- Wire search with `type=pyramid_metadata` returns only pyramid listings.
- Metadata includes tunnel_url, node_count, max_depth, content_type, apex summary.
- Re-publishing supersedes the old metadata contribution (not duplicates).

### Complexity: Medium

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

### Files to Modify

**agent-wire-node (Rust):**
- `src-tauri/src/pyramid/routes.rs` -- Modify `with_auth_state` to accept either local auth_token OR Wire JWT. Add `with_wire_auth` filter that validates Wire JWT and extracts operator_id. Pyramid query handlers check: local token -> pass (free, no billing); Wire JWT -> pass (billable, trigger nano-tx).
- `src-tauri/src/server.rs` -- Add `PyramidQueryClaims` struct (sub=operator_id, slug, query_type). Extend JWT validation to handle pyramid query tokens alongside document access tokens.
- `src-tauri/src/pyramid/query.rs` -- No changes needed; query functions are auth-agnostic.
- `src-tauri/src/credits.rs` -- Add `pyramid_queries_served` counter. Add `log_pyramid_query_serve(slug, query_type, operator_id)` for credit tracking.

**agent-wire-node (Rust, client side):**
- `src-tauri/src/pyramid/wire_import.rs` -- Add `RemotePyramidClient` struct. Takes tunnel_url + Wire auth token. Methods: `remote_apex(slug)`, `remote_drill(slug, node_id)`, `remote_search(slug, query)`. Uses reqwest with Wire JWT in Authorization header.

**GoodNewsEveryone (Wire server):**
- `src/app/api/v1/wire/query/route.ts` (or new endpoint) -- Add `POST /api/v1/wire/pyramid-query-token` that issues a short-lived JWT for pyramid queries. Takes target_node_id + slug, returns signed token. This is the equivalent of the document access token flow.
- `src/app/api/v1/node/tunnel/route.ts` (or equivalent) -- No changes if tunnel provisioning already works.

### Dependencies
- WS-ONLINE-B (discovery provides tunnel_url).
- Wire server JWT infrastructure (exists for document tokens, needs pyramid query token variant).

### Acceptance Criteria
- Agent can query a remote pyramid's apex, drill, search, entities endpoints through the tunnel.
- Wire JWT validates on the serving node (issuer check, expiry check, public key verification).
- Local queries (same node, using local auth_token) remain free and unaffected.
- Remote queries logged in credit tracker with operator_id and query type.
- 401 response for expired/invalid/missing tokens.

### Complexity: Large

---

## WS-ONLINE-D: Daemon Caching / Pinning

**Goal:** "Pin this pyramid" pulls latest version to your local node. Uses the existing corpus sync mechanism (pull direction).

### Mechanism

Pinning a remote pyramid means pulling its SQLite data into your local pyramid DB. This is not file-level sync (pyramids live in SQLite, not flat files). Instead:

1. Agent calls remote pyramid's `GET /pyramid/{slug}/tree` to get the full node structure.
2. Nodes are inserted into local SQLite under the same slug with a `pinned=true` flag.
3. The slug appears in the local slug list with a "pinned" badge.
4. The node serves the pinned pyramid from its own tunnel (earn server stamp credits).
5. Auto-buy subscription: periodic poll of the remote pyramid's metadata contribution for updated build timestamps. If newer, pull again.

The existing `SyncState.linked_pyramids` map (from WS-ONLINE-A) extends with `direction=Download` or `direction=Both` for pinned pyramids.

### Files to Modify

**agent-wire-node (Rust):**
- `src-tauri/src/pyramid/db.rs` -- Add `pinned` boolean column to `pyramid_slugs` table. Add `source_tunnel_url` column for pinned slugs. Add `upsert_pinned_nodes(conn, slug, nodes)` that bulk-inserts/updates nodes from remote tree response.
- `src-tauri/src/pyramid/slug.rs` -- Add `pin_remote_pyramid(slug, tunnel_url, nodes)` that creates a pinned slug and inserts nodes. Add `unpin_pyramid(slug)` that removes pinned flag (keeps data for local queries or drops it).
- `src-tauri/src/sync.rs` -- Add `PinnedPyramidLink` to `SyncState`. Auto-sync tick for pinned pyramids: poll remote metadata, compare build timestamp, re-pull if newer.
- `src-tauri/src/pyramid/wire_import.rs` -- Add `pull_remote_pyramid(tunnel_url, slug)` that fetches tree + all nodes and returns them as `Vec<PyramidNode>`.

**agent-wire-node (Frontend):**
- `src/components/PyramidPublicationStatus.tsx` -- Show pinned pyramids with badge and source tunnel URL. "Unpin" button. "Refresh Now" button. Last-synced timestamp.
- `src/components/modes/NodeMode.tsx` -- Pinned pyramids appear in the Pyramids tab alongside local ones.

### Dependencies
- WS-ONLINE-C (remote querying must work for pull).
- WS-ONLINE-A (publication must work for re-serving pinned content).

### Acceptance Criteria
- "Pin pyramid" pulls full tree into local SQLite.
- Pinned slug visible in slug list with "pinned" badge and source URL.
- Auto-refresh polls remote metadata, re-pulls on build_id change.
- Pinned pyramids servable from your own tunnel.
- Unpin removes the sync link (optionally keeps data).
- Local queries on pinned pyramids are free (no nano-tx).

### Complexity: Large

---

## WS-ONLINE-E: Access Tier Configuration

**Goal:** Per-slug access control: public (default), circle-scoped, priced, embargoed.

### Mechanism

Each pyramid slug gets an `access_tier` config stored in `pyramid_slugs`:
- **public** (default): any valid Wire JWT grants access.
- **circle-scoped**: JWT must include a circle_id that matches the slug's allowed circles list.
- **priced**: query requires a pre-authorized payment token (beyond the 1-credit nano-tx).
- **embargoed**: no remote access; local only.

The auth middleware (from WS-ONLINE-C) checks access_tier before dispatching to query handlers.

### Files to Modify

**agent-wire-node (Rust):**
- `src-tauri/src/pyramid/db.rs` -- Add `access_tier` and `allowed_circles` columns to `pyramid_slugs`. Defaults: access_tier="public", allowed_circles=null.
- `src-tauri/src/pyramid/routes.rs` -- Add access tier check in the Wire JWT auth path. For circle-scoped: extract circle_id from JWT claims, check against allowed_circles. For embargoed: reject all Wire JWT requests. For priced: validate payment token (deferred detail -- initially same as public + nano-tx).
- `src-tauri/src/pyramid/slug.rs` -- Add `set_access_tier(conn, slug, tier, circles)` and `get_access_tier(conn, slug)`.

**agent-wire-node (Frontend):**
- `src/components/PyramidPublicationStatus.tsx` -- Add access tier dropdown per slug: Public, Circle-Scoped, Embargoed. Circle selector when circle-scoped is chosen (fetches circles from Wire API).

**GoodNewsEveryone (Wire server):**
- Pyramid query tokens (from WS-ONLINE-C) should include circle_id claims when the querier is a circle member. No new endpoints needed -- circle membership is already queryable.

### Dependencies
- WS-ONLINE-C (remote querying + Wire JWT).
- Circle system (exists on Wire server).

### Acceptance Criteria
- Per-slug access tier configurable in UI.
- Public pyramids accessible to any valid Wire JWT.
- Circle-scoped pyramids reject queries from non-members (403).
- Embargoed pyramids reject all remote queries.
- Access tier persisted in SQLite, survives restart.

### Complexity: Medium

---

## WS-ONLINE-F: Cross-Node Understanding Webs

**Goal:** Web on Node A references a pyramid on Node B via Wire handle-path. The build runner resolves remote references through the tunnel.

### Mechanism

Today, web edges are same-slug, same-node. To go cross-node:

1. The web edge junction table (`pyramid_web_edges`) adds a `remote_handle_path` column. For local edges this is null. For cross-node edges it stores the Wire handle-path of the remote node.
2. During distillation, when the LLM emits a web edge note referencing a remote pyramid, `process_web_edge_notes` creates an edge with the remote handle-path.
3. The build runner resolves remote references by calling the remote pyramid's drill/node endpoint through the tunnel (via `RemotePyramidClient` from WS-ONLINE-C).
4. Evidence links use Wire handle-paths (already three-segment format: `slug/depth/node-id`).
5. Gap reports on remote pyramids are published to the Wire as contributions with type `gap_report`. The gap body describes what's missing. The remote pyramid owner sees demand signals in their gap feed.

### Files to Modify

**agent-wire-node (Rust):**
- `src-tauri/src/pyramid/db.rs` -- Add `remote_handle_path` and `remote_tunnel_url` columns to `pyramid_web_edges`. Add `get_remote_web_edges(conn, slug)` for edges that reference external nodes.
- `src-tauri/src/pyramid/webbing.rs` -- Extend `process_web_edge_notes` to accept remote references. When `note.thread_id` contains a Wire handle-path (three-segment), create a remote edge instead of a local one.
- `src-tauri/src/pyramid/build_runner.rs` -- When resolving evidence for a node with remote web edges, use `RemotePyramidClient` to fetch the referenced node's content. Cache remote node data locally for the build session.
- `src-tauri/src/pyramid/wire_publish.rs` -- Add `publish_gap_report(slug, remote_handle_path, gap_description)` that publishes a `gap_report` contribution to the Wire.

**agent-wire-node (Frontend):**
- Drill view should show remote web edges with a "remote" indicator and the source pyramid name. Clicking navigates to the remote pyramid (if accessible).

### Dependencies
- WS-ONLINE-C (remote querying for resolving references).
- WS-ONLINE-B (discovery for finding remote pyramids).

### Acceptance Criteria
- Web edges can reference nodes on remote pyramids via Wire handle-path.
- Build runner resolves remote references through tunnel during build.
- Gap reports published as Wire contributions visible to remote pyramid owner.
- Remote web edges displayed in drill view with source attribution.
- Evidence links with remote handle-paths are valid and publishable.

### Complexity: Large

---

## WS-ONLINE-G: Owner Absorption Config

**Goal:** Pyramid owner configures how incoming understanding webs are handled.

### Mechanism

Three modes per slug:
- **open** (default): questioner owns the web they build. Standard flow.
- **absorb-all**: pyramid owner's node funds the web build. Web nodes list the owner as creator. Incoming webs automatically merge into the pyramid.
- **absorb-selective**: an action chain evaluates incoming webs. The chain decides accept/reject/modify. Only accepted webs merge.

### Files to Modify

**agent-wire-node (Rust):**
- `src-tauri/src/pyramid/db.rs` -- Add `absorption_mode` column to `pyramid_slugs` (values: "open", "absorb_all", "absorb_selective"). Add `absorption_chain_id` for selective mode.
- `src-tauri/src/pyramid/slug.rs` -- Add `set_absorption_mode(conn, slug, mode, chain_id)` and `get_absorption_mode(conn, slug)`.
- `src-tauri/src/pyramid/build_runner.rs` -- When building a web on a remote pyramid with absorb mode, the request includes the owner's operator credentials. The owner's node executes the build and credits flow from the owner's pool.
- `src-tauri/src/pyramid/routes.rs` -- Add `POST /pyramid/:slug/absorption-config` and `GET /pyramid/:slug/absorption-config`.

**agent-wire-node (Frontend):**
- `src/components/PyramidPublicationStatus.tsx` -- Add absorption mode selector per slug: Open, Absorb All, Absorb Selective. Chain selector when selective is chosen.

### Dependencies
- WS-ONLINE-F (cross-node webs must exist first).
- Chain executor (exists) for selective mode evaluation.

### Acceptance Criteria
- Per-slug absorption mode configurable via API and UI.
- Open mode: standard questioner-owns flow, no change.
- Absorb-all mode: owner's node funds web build, owner credited as creator.
- Absorb-selective mode: chain evaluates and accepts/rejects incoming webs.
- Absorption mode published in pyramid metadata (WS-ONLINE-B).

### Complexity: Small-Medium

---

## WS-ONLINE-H: Nano-Transaction Integration

**Goal:** Every remote pyramid query triggers a 1-credit nano-transaction from querier to server node.

### Mechanism

The Wire's economics are integer-based (Pillar 9, rotator arm). A nano-transaction is the smallest unit: 1 credit. The float pool holds credits in transit.

Flow:
1. Querier sends request with Wire JWT to serving node.
2. Serving node validates JWT, executes query.
3. Before returning response, serving node calls Wire server: `POST /api/v1/wire/nano-tx` with `{from_operator: querier_op_id, to_operator: server_op_id, amount: 1, reason: "pyramid_query", slug: "...", query_type: "drill"}`.
4. Wire server debits querier's float pool, credits server's pool.
5. Response includes transaction receipt (tx_id).

Local queries (same node, pinned pyramids) skip the nano-tx call entirely.

### Files to Modify

**agent-wire-node (Rust):**
- `src-tauri/src/credits.rs` -- Add `pyramid_queries_served: u64` and `pyramid_query_credits_earned: u64` counters. Add `log_pyramid_nano_tx(tx_id, querier_op_id, slug, query_type)`.
- `src-tauri/src/pyramid/routes.rs` -- In the Wire JWT auth path, after query execution, call nano-tx endpoint. On failure: still return query result (don't block on billing failure) but log the failure and retry async.
- `src-tauri/src/server.rs` -- Add `NanoTxClient` that posts to Wire server's nano-tx endpoint. Retry logic with exponential backoff for transient failures.

**GoodNewsEveryone (Wire server):**
- `src/app/api/v1/wire/nano-tx/route.ts` (new) -- Accept nano-transaction requests. Validate both operator pools exist. Debit from_operator, credit to_operator. Return tx_id. Reject if from_operator has insufficient balance.
- `src/lib/server/credits.ts` (or equivalent) -- Add `execute_nano_transaction(from_op, to_op, amount, metadata)` function. Atomic debit+credit in a single transaction.

### Dependencies
- WS-ONLINE-C (remote querying + Wire JWT provides the operator IDs).
- Wire server credit pool infrastructure (exists for document serves, needs pyramid query variant).

### Acceptance Criteria
- Every remote pyramid query triggers a 1-credit nano-transaction.
- Local queries (own node, pinned) are free -- no nano-tx call.
- Nano-tx failure does not block query response (async retry).
- Credit counters updated on both querier and server nodes.
- Wire server rejects nano-tx when querier has zero balance (402 response).
- Transaction receipts logged for audit trail.

### Complexity: Medium

### Pillar Conformance
- Pillar 7 (UFF): Nano-transactions feed into the same credit economy. Server stamps accumulate through the rotator arm.
- Pillar 8 (structural deflation): Each query costs 1 credit (deflationary).
- Pillar 9 (integer economics): 1 credit is the atomic unit, no fractional amounts.
- Pillar 25 (platform agents use public API): Nano-tx goes through the public API, not direct DB.

---

## Execution Order

```
Phase 1 (sequential):
  WS-ONLINE-A (publication)  ~2 weeks
    -> WS-ONLINE-B (discovery)  ~1 week

Phase 2:
  WS-ONLINE-C (remote query)  ~2-3 weeks

Phase 3 (parallel, after C):
  WS-ONLINE-D (pinning)      ~2 weeks
  WS-ONLINE-E (access tiers) ~1 week
  WS-ONLINE-F (cross-node webs) ~2-3 weeks
  WS-ONLINE-G (absorption)   ~1 week
  WS-ONLINE-H (nano-tx)      ~1-2 weeks
```

Total estimated duration: 6-8 weeks with focused execution, workstreams D-H parallelized.

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
- `/src-tauri/src/pyramid/mod.rs` -- Module declarations and PyramidState
- `/src-tauri/src/sync.rs` -- Corpus sync engine (pattern to reuse)
- `/src-tauri/src/tunnel.rs` -- Cloudflare tunnel management
- `/src-tauri/src/server.rs` -- HTTP server state and JWT validation
- `/src-tauri/src/credits.rs` -- Credit tracking
- `/src-tauri/src/market.rs` -- Market daemon
- `/src-tauri/src/lib.rs` -- Tauri command wiring

**agent-wire-node (Frontend):**
- `/src/components/modes/NodeMode.tsx` -- Node page with sync/market/logs tabs
- `/src/components/SyncStatus.tsx` -- Corpus sync status UI
- `/src/components/PyramidPublicationStatus.tsx` (new) -- Pyramid publication and pinning UI

**GoodNewsEveryone (Wire server):**
- `/src/app/api/v1/contribute/route.ts` -- Contribution ingest
- `/src/lib/server/contribute-core.ts` -- Contribution validation and storage
- `/src/app/api/v1/wire/circles/` -- Circle system endpoints
- `/src/app/api/v1/wire/query/route.ts` -- Wire query endpoint
- `/src/app/api/v1/wire/nano-tx/route.ts` (new) -- Nano-transaction endpoint
