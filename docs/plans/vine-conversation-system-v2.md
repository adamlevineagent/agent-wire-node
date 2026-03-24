# Vine Conversation System — Implementation Plan v2

> Status: Final, post-audit + MPS (Stage 1 + Stage 2 + MPS complete)
> Date: 2026-03-24
> Target: agent-wire-node pyramid engine (Rust)
> Supersedes: vine-conversation-system.md
> Audit: 4 auditors across 2 stages. All critical/major findings incorporated.

## The Grape Metaphor

- **Grape** = any single node in a conversation pyramid
- **Bunch** = one complete conversation pyramid (all grapes from one session, clustered in their pyramid shape)
- **Vine** = meta-pyramid where bunches connect at the top

The vine's L0 nodes are the **apex + one level down** from each bunch. If a conversation pyramid has an apex at L5 and two L4 nodes, the vine gets 3 L0 nodes from that bunch. The apex alone is too compressed (one sentence about a 12-hour session). The penultimate layer gives you the major topic clusters: "this session was about DADBEAR design, UX implementation, and audit methodology."

**Scale:** 71 conversations × ~3 nodes per bunch stem = ~213 vine L0 nodes. Medium pyramid. The vine clusters those into temporal/topical patterns, detects eras, tracks decision evolution — all using the same pyramid machinery.

**Everything is a contribution.** One new table (`vine_bunches`). ERAs, decisions, entities, thread continuity, corrections — all expressed as annotations, FAQ entries, and web edges on the vine's own pyramid. No parallel infrastructure.

---

## V1 Scope

- ✅ `vine_bunches` table + `vine` content type
- ✅ Bunch discovery + batch construction from JSONL files
- ✅ Vine L0 from apex + penultimate layer per bunch
- ✅ Vine L1/L2+/apex build with temporal-topic affinity clustering
- ✅ All six intelligence passes: ERAs, transitions, entity resolution, decisions, thread continuity, corrections
- ✅ Live Vine mode: direct notification after bunch rebuild, plus JSONL mtime polling for active conversations
- ⏸️ Cross-project vines (V2)
- ⏸️ Vine merging (coop mode, later)
- ⏸️ Auto-source discovery (later)

**Minimum viable vine:** Two bunches is sufficient.

---

## Phase 1: Database Schema

### 1a. Add `vine` content type

**Problem the audit found:** SQLite CHECK constraints are baked into the table at creation time. `CREATE TABLE IF NOT EXISTS` is a no-op on existing databases. Changing the CHECK in source code has zero effect on existing DBs — inserting `content_type='vine'` will fail.

**Solution:** Recreate the table with `'vine'` added to the CHECK, inside a transaction.

**File: `pyramid/types.rs`**
- Add `Vine` variant to `ContentType` enum
- Add `"vine"` arm to `as_str()` and `from_str()`
- Fix `ContentType::from_str()` catch-all: log warning for unknown types instead of silently returning `None`
- Fix `list_slugs()` in `db.rs`: change `unwrap_or(ContentType::Document)` to log warning for unknown types

**File: `pyramid/db.rs`**
- Add migration function `migrate_slugs_check_constraint()` that:
  1. `PRAGMA foreign_keys=OFF` (inside transaction)
  2. `CREATE TABLE pyramid_slugs_new` with `CHECK(content_type IN ('code','conversation','document','vine'))`
  3. `INSERT INTO pyramid_slugs_new SELECT * FROM pyramid_slugs`
  4. `DROP TABLE pyramid_slugs`
  5. `ALTER TABLE pyramid_slugs_new RENAME TO pyramid_slugs`
  6. Recreate indexes
  7. `PRAGMA foreign_keys=ON`
  8. All wrapped in a transaction. Idempotent: skip if CHECK already includes 'vine' (detect via `PRAGMA table_info`)
- Call from `init_pyramid_db()` after table creation

### 1b. New table: `vine_bunches`

**File: `pyramid/db.rs`** — add to `init_pyramid_db()`

```sql
CREATE TABLE IF NOT EXISTS vine_bunches (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    vine_slug       TEXT NOT NULL REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
    bunch_slug      TEXT NOT NULL REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
    session_id      TEXT NOT NULL,
    jsonl_path      TEXT NOT NULL,
    bunch_index     INTEGER NOT NULL,
    first_ts        TEXT,
    last_ts         TEXT,
    message_count   INTEGER,
    chunk_count     INTEGER,
    apex_node_id    TEXT,
    penultimate_node_ids TEXT,   -- JSON array of node IDs one level below apex
    status          TEXT NOT NULL DEFAULT 'pending',
    metadata        TEXT,        -- JSON: VineBunchMetadata
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at      TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(vine_slug, bunch_slug)   -- bunch_slug not session_id, to not block future cross-project vines
);
CREATE INDEX IF NOT EXISTS idx_vine_bunches_vine ON vine_bunches(vine_slug);
CREATE INDEX IF NOT EXISTS idx_vine_bunches_order ON vine_bunches(vine_slug, bunch_index);
```

### 1c. Add `Era` and `Transition` to `AnnotationType`

**Problem the audit found:** The existing `AnnotationType` enum only has Observation, Correction, Question, Friction, Idea. Storing `annotation_type = "era"` silently defaults to Observation on read-back.

**Solution:** Add `Era` and `Transition` variants to the enum, with corresponding `as_str()`/`from_str()` arms. Also fix the catch-all `_ => AnnotationType::Observation` in `from_str()` to log a warning when encountering unknown types, preventing silent data loss.

### 1d. Add `Vine` match arm to build dispatch

**File: `pyramid/routes.rs`** — the `handle_build` content_type match (line 942) needs a `ContentType::Vine` arm. Return an error: "Use POST /pyramid/vine/build for vine pyramids" since vine build has different parameters.

### 1e. New types

**File: `pyramid/types.rs`**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VineBunch {
    pub id: i64,
    pub vine_slug: String,
    pub bunch_slug: String,
    pub session_id: String,
    pub jsonl_path: String,
    pub bunch_index: i64,
    pub first_ts: Option<String>,
    pub last_ts: Option<String>,
    pub message_count: Option<i64>,
    pub chunk_count: Option<i64>,
    pub apex_node_id: Option<String>,
    pub penultimate_node_ids: Vec<String>,
    pub status: String,
    pub metadata: Option<VineBunchMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VineBunchMetadata {
    pub topics: Vec<String>,
    pub entities: Vec<String>,
    pub decisions: Vec<VineDecision>,
    pub corrections: Vec<VineCorrection>,
    pub open_questions: Vec<String>,
}

/// Decision with temporal context for cross-bunch evolution chains
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VineDecision {
    pub decision: Decision,
    pub bunch_index: i64,
    pub bunch_ts: String,       // first_ts of the bunch
}

/// Correction with temporal context for cross-bunch correction chains
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VineCorrection {
    pub correction: Correction,
    pub bunch_index: i64,
    pub bunch_ts: String,
}

#[derive(Debug, Clone)]
pub struct BunchDiscovery {
    pub session_id: String,
    pub jsonl_path: PathBuf,
    pub first_ts: String,
    pub last_ts: String,
    pub message_count: i64,
}
```

---

## Phase 2: Bunch Discovery & Construction

### New file: `src-tauri/src/pyramid/vine.rs`

### 2a. Bunch Discovery — `discover_bunches()`

```rust
pub fn discover_bunches(jsonl_dir: &Path) -> Result<Vec<BunchDiscovery>>
```

Synchronous (filesystem scan only):
- Scan `jsonl_dir` for `*.jsonl` files (top-level only — skip subdirectories containing subagent files)
- For each: read first `type: "user"` line → extract `timestamp`, `sessionId`
- Read last message line → `last_timestamp`
- Count user+assistant messages (skip all other types; skip `toolUseResult` entries)
- Skip files with < 3 user messages
- Sort by `first_timestamp` ascending, then `session_id` as tiebreaker (stable ordering when timestamps collide) → defines `bunch_index`

**Source:** `/Users/adamlevine/.claude/projects/-Users-adamlevine-AI-Project-Files-GoodNewsEveryone/*.jsonl` (71 files)

**Shared JSONL parser:** Extract a low-level `parse_jsonl_line()` helper that yields structured records (type, sessionId, timestamp, content). Both `ingest.rs::parse_conversation_messages` and `discover_bunches` consume it with different filters. This avoids maintaining two independent parsers for the same format.

### 2b. Build Pipeline Helper — `run_build_pipeline()`

**Problem the audit found:** `build_conversation()` requires a `WriteOp` mpsc channel + writer drain task + `BuildProgress` channel + `CancellationToken`. The plan cannot just "call it unchanged" — the channel setup from `routes.rs` (lines 894-964) must be replicated.

**Solution:** Extract the channel boilerplate into a shared helper:

```rust
pub async fn run_build_pipeline(
    reader: Arc<Mutex<Connection>>,
    writer: Arc<Mutex<Connection>>,
    llm_config: &LlmConfig,
    slug: &str,
    content_type: ContentType,
    cancel: &CancellationToken,
    progress_tx: Option<mpsc::Sender<BuildProgress>>,
) -> Result<i32>
```

This helper:
1. Creates `mpsc::channel::<WriteOp>(256)`
2. Spawns writer drain task (copies pattern from routes.rs:898-927)
3. Creates internal `mpsc::channel::<BuildProgress>(64)`, forwards to caller's `progress_tx` if provided
4. Dispatches by `content_type`: `Conversation → build_conversation()`, `Code → build_code()`, `Document → build_docs()`, `Vine → error`
5. Drops channels, awaits task handles
6. Returns failure count

Both `handle_build` in routes.rs and `build_bunch` in vine.rs call this helper. This eliminates the duplication.

**Concurrency:** The vine build **bypasses `active_build`** entirely. `active_build` is a global singleton (`Option<BuildHandle>`) that blocks all builds system-wide. The vine manages its own `CancellationToken` that propagates to each bunch build. The vine build does NOT register in `active_build`, so normal HTTP-triggered builds can still run concurrently (they operate on different slugs with independent writer operations). Pre-create bunch slugs before calling `ingest_conversation()` to avoid the unvalidated slug creation path in `ingest.rs`.

### 2c. Build Each Bunch

```rust
pub async fn build_bunch(
    state: &PyramidState,
    vine_slug: &str,
    bunch: &BunchDiscovery,
    bunch_index: i64,
    cancel: &CancellationToken,
) -> Result<VineBunch>
```

1. **Create bunch slug:** handle-path style naming: `{vine_slug}--bunch-{bunch_index:03}` with `content_type = "conversation"`
2. **Ingest:** Call `ingest::ingest_conversation()` — this is **synchronous**, takes `&Connection`. Must lock writer and call inside `spawn_blocking` or equivalent.
3. **Build:** Call `run_build_pipeline()` (the new helper from 2b)
4. **Read apex:** Call `query::get_apex(conn, &bunch_slug)` (note: `query::`, not `db::`)
5. **Read penultimate layer:** `SELECT * FROM live_pyramid_nodes WHERE slug=? AND depth=? ORDER BY id` where depth = apex.depth - 1
6. **Extract metadata** (2d below)
7. **Insert into `vine_bunches`**

### 2d. Metadata Extraction (Mechanical, No LLM)

```rust
fn extract_bunch_metadata(
    apex: &PyramidNode,
    penultimate_nodes: &[PyramidNode],
) -> VineBunchMetadata
```

Collect from both node-level AND topic-level on penultimate nodes:
- **topics:** all `topic.name` from penultimate nodes' `topics` vec
- **entities:** all `topic.entities` from penultimate nodes' topics
- **decisions:** union of `node.decisions` and `node.topics[*].decisions`, deduplicated by `decided` text, wrapped in `VineDecision` with `bunch_index` and `bunch_ts`
- **corrections:** union of `node.corrections` and `node.topics[*].corrections`, wrapped in `VineCorrection` with `bunch_index` and `bunch_ts`
- **open_questions:** from apex `self_prompt`

### 2e. Batch Build (Crash-Safe)

```rust
pub async fn build_all_bunches(
    state: &PyramidState,
    vine_slug: &str,
    jsonl_dir: &Path,
    cancel: &CancellationToken,
    progress_tx: Option<mpsc::Sender<(i64, i64)>>,  // (current_bunch, total_bunches)
) -> Result<Vec<VineBunch>>
```

- Sequential (LLM rate-limited)
- **State machine:** `pending → building → built | error`. On startup, any bunches in `building` state get cleaned up (cascade delete partial data via `db::delete_slug`) before retrying.
- Skip bunches with `status = 'built'` (crash-safe resume)
- On error: mark `status='error'`, continue to next bunch
- Progress: `"Bunch 14/71: building session from 2026-03-15..."`
- **Ingestion locking:** `ingest_conversation()` is sync and holds `&Connection`. Separate file read + chunking (no lock needed) from DB inserts (lock writer briefly). For V1, acceptable to hold writer lock during ingest since builds are sequential.

**Edge case:** Very small sessions (1-2 chunks) may produce a pyramid with no penultimate layer (apex IS L0). In that case, the vine gets 1 L0 node from that bunch (just the apex). The `penultimate_node_ids` array will be empty, and the vine L0 node uses only the apex content.

---

## Phase 3: Vine Construction

### 3a. Vine L0 — Apex + Penultimate Layer per Bunch

**Mechanical (no LLM).** For each bunch in temporal order:

For the **apex grape** — vine L0 node:
- **Node ID:** `L0-{global_index:03}` under vine slug
- **Content:** Apex distilled text + headline
- **Topics JSON:** from bunch metadata

For each **penultimate grape** — additional vine L0 nodes:
- **Node ID:** `L0-{global_index:03}` (continuing the sequence)
- **Content:** Penultimate node's distilled text + headline
- **Topics JSON:** from that node's topics

All vine L0 nodes from the same bunch share the same temporal metadata (timestamps, session reference). The vine L0 ordering is: all nodes from bunch 0 first, then all from bunch 1, etc. Within a bunch, apex first, then penultimate nodes.

**Expected count:** ~213 vine L0 nodes for 71 bunches.

### 3b. Vine L1 — LLM Clustering via `VINE_CLUSTER_PROMPT`

**LLM-only clustering** (no algorithmic split). Send all bunch topic/entity summaries to `VINE_CLUSTER_PROMPT` which receives:
- Each bunch's index, timestamp range, topic list, entity list
- Instruction to cluster bunches into temporal-topical neighborhoods

The LLM handles nuance that string-matching misses — sessions using different vocabulary for the same concept, tone/phase shifts that don't show up in entity overlap. Cost: ~$0.0002 per clustering call. Not worth optimizing away.

**Constraints the prompt enforces:**
- Max cluster size: 4 bunches (~12 vine L0 nodes)
- Prefer temporal contiguity (nearby sessions cluster together unless topics diverge)
- Singletons allowed
- All vine L0 nodes from the same bunch stay together (the prompt clusters bunches; their nodes follow)

Each cluster → one vine L1 node, distilled with existing `THREAD_NARRATIVE_PROMPT` with `[LATE — AUTHORITATIVE]` markers on more recent bunches.

### 3c. Vine L2+/Apex

**Problem the audit found:** `build_upper_layers()` is private to `build.rs` and has a different name than the plan assumed.

**Solution:** Either make it `pub` or reimplement the pair-and-distill loop in `vine.rs`. The pattern is simple:
1. Pair adjacent L1 nodes
2. Distill each pair with `DISTILL_PROMPT`
3. Recurse at next depth until apex

Reimplementing is cleaner than making an internal function public — keeps `build.rs` unchanged. The vine version uses the same `WriteOp` channel infrastructure via the helper. **Must include `step_exists()` checks before each LLM call** for crash-safe resumability, matching the pattern in `build.rs`.

---

## Phase 4: Intelligence Passes

All outputs are **contributions to the vine pyramid** using existing tables. No parallel infrastructure.

### 4a. ERA Detection — `detect_vine_eras()`

Three signals blended:

1. **Entity overlap sliding window** (mechanical): window=5 bunches, compute Jaccard overlap of entity sets. Boundary candidate when overlap < 0.3.
2. **LLM phase classifier** (ambiguous zone 0.3–0.6): new `VINE_PHASE_CHECK_PROMPT` — "same project phase or different?"
3. **Temporal gap reinforcement:** 3+ day gaps between bunches strengthen boundary signal.

**Output:** Annotations on vine L1/L2 nodes with `annotation_type = "era"` (using the new `Era` variant added in Phase 1c).

### 4b. Transition Classification — `classify_vine_transitions()`

For adjacent ERA pairs, new `VINE_TRANSITION_PROMPT` classifies:
- **pivot** / **evolution** / **expansion** / **refinement** / **return**

**Output:** Annotations with `annotation_type = "transition"` (using new `Transition` variant).

### 4c. Entity Resolution — `resolve_vine_entities()`

1. Collect all entities across all bunch metadata
2. Cluster by fuzzy match (Levenshtein < 3 or substring containment)
3. LLM picks canonical name per cluster
4. Store as FAQ entries in `pyramid_faq_nodes` under vine slug

### 4d. Decision Tracking — `track_vine_decisions()`

1. Collect decisions across bunches
2. Group by topic/entity overlap
3. Build evolution chains: proposed → decided → modified → replaced (with bunch references)
4. Store as FAQ entries under vine slug

### 4e. Thread Continuity — `map_vine_threads()`

**Problem the audit found:** `pyramid_web_edges` has FK constraints requiring `pyramid_threads` entries. Vine L0 nodes are not threads.

**Solution:** Create proper `pyramid_threads` entries under the vine slug for each cross-bunch narrative strand:
1. Match topic/thread names across bunches using entity resolution canonical names
2. Create a `pyramid_threads` entry for each cross-bunch strand (e.g., "DADBEAR design" thread spans bunches 40-47)
3. Create web edges between those threads
4. Track lifecycle: active / dormant / resolved / recurring

### 4f. Correction Chains — `trace_vine_corrections()`

Match corrections across bunches where a correction in bunch N fixes something stated in bunch M.
**Output:** Annotations with `annotation_type = "correction"` linking both bunches.

---

## Phase 5: Live Vine Mode

### 5a. No Cross-Slug Watching — Direct Notification Instead

**Problem the audit found:** `PyramidStaleEngine` is per-slug with no cross-slug monitoring. `PyramidFileWatcher` watches filesystem paths for source code changes, not pyramid node changes. The L1+ stale dispatch functions are placeholders.

**Solution:** Don't try to hook into DADBEAR for cross-slug watching. Use direct notification:

**After each bunch build completes** (in `build_all_bunches` or incremental build), directly call the vine update path:

```rust
async fn notify_vine_of_bunch_change(
    state: &PyramidState,
    vine_slug: &str,
    bunch: &VineBunch,
) -> Result<()>
```

1. Re-extract metadata from the bunch's updated pyramid
2. Compare against stored `vine_bunches.metadata`
3. If changed: update vine L0 nodes for this bunch, write mutation to vine's own `pyramid_pending_mutations`
4. Vine's own per-slug stale engine handles L1+ propagation (this IS standard DADBEAR, operating within the vine slug)

### 5b. Active Conversation Detection — JSONL Mtime Polling

New polling mechanism (NOT `PyramidFileWatcher` which is designed for source code):

```rust
pub struct VineJSONLWatcher {
    vine_slug: String,
    jsonl_dir: PathBuf,
    known_files: HashMap<PathBuf, (u64, SystemTime)>,  // path → (size, mtime)
    debounce_seconds: u64,
    pending: HashMap<PathBuf, Instant>,  // path → last_change_detected
}
```

- `tokio::time::interval` loop (60-second poll)
- Stat each JSONL file in directory
- Changed mtime → start debounce timer (5 minutes)
- Debounce expires → rebuild bunch via `ingest_continuation()` + rebuild pipeline
- New file → discover + build new bunch
- Feed results through `notify_vine_of_bunch_change`
- **JSONL race condition safety:** Claude Code appends to JSONL files concurrently. Read to last complete newline boundary (discard partial final line). Track byte offset rather than message count for `ingest_continuation`, so a previously-truncated line is re-read on the next poll rather than permanently lost.

### 5c. Staleness Trigger: Apex + Penultimate Changes

When a bunch rebuild completes:
- Read current apex + penultimate nodes
- Compare against stored `vine_bunches.metadata`
- **Apex changed → definitely stale** (update vine L0 nodes)
- **Only penultimate changed but apex didn't → still stale** (the penultimate carry the substance the apex compressed away)
- Update `vine_bunches.metadata` and `vine_bunches.penultimate_node_ids`

### 5d. Vine-Internal DADBEAR

The vine slug gets its own standard `PyramidStaleEngine` entry (same as any pyramid). When vine L0 nodes are updated (by `notify_vine_of_bunch_change`), this triggers vine-internal DADBEAR:
- Vine L0 change → mutation in vine's WAL
- Vine's debounce timer fires → evaluates vine L1 staleness
- Propagates through vine L2+/apex

This is standard DADBEAR operating within a single slug — no cross-slug plumbing needed.

### 5e. Annotation Cascade Protection

**Problem:** `pyramid_annotations` has `ON DELETE CASCADE` to `pyramid_nodes`. When vine L1/L2 nodes are rebuilt (superseded + replaced), cascade delete destroys all ERA/transition annotations attached to the old nodes.

**Solution:** Intelligence passes are cheap and rerunnable. After any vine L1+ rebuild, re-run the intelligence passes for the affected region. The passes operate on the vine's current nodes and produce fresh annotations. This is simpler and more robust than migrating annotations between node versions. Cost: ~$0.05 per incremental re-run.

Also add `rebuild_bunch()` function that: (1) deletes all nodes/chunks for the bunch slug (cascade), (2) re-ingests from scratch, (3) rebuilds, (4) updates vine_bunches metadata. Add `stale` status to the state machine for bunches that need full re-ingest (not just continuation).

### 5f. Force L2+ Rebuild

**Problem:** When a vine rebuild downstream reassigns children between sub-apex nodes, one sub-apex node can lose all its children and become a dead end — structurally present but navigationally useless. This is a general problem with higher-tier networking nodes in any pyramid.

**Solution:** `force_rebuild_vine_upper(state, vine_slug)` that:
1. Deletes all vine nodes at depth > 1 via `db::delete_nodes_above(conn, vine_slug, 1)`
2. Deletes pipeline_steps above depth 1 via new `delete_steps_above_depth(conn, slug, 1)` — needed because existing `delete_steps` filters by step_type not depth, and `step_exists()` would cause the rebuild to skip work if old steps remain
3. Re-runs `build_vine_upper()` from L1 up
4. Re-runs sub-apex directory wiring (5h) on the new sub-apex layer
5. Re-runs ERA detection and transition classification only (passes 4a and 4b) — entity resolution (4c), decision tracking (4d), thread continuity (4e), and correction chains (4f) produce FAQ entries and web edges referencing bunches/threads, not L2+ nodes, and are unaffected by L2+ deletion
6. Runs post-build integrity check (5g)

**Partial failure safety:** If step 3 fails midway, the vine has no L2+/apex but L0/L1 are intact. Recovery: re-run `rebuild-upper` again (step_exists crash-safety skips completed work). The CLI/route should return a clear error. This is the same recovery model as `build_vine` itself — crash-safe resume, not transactional rollback.

Available as both HTTP route (`POST /pyramid/:slug/vine/rebuild-upper`) and CLI command (`vine rebuild-upper <slug>`).

### 5g. Post-Build Integrity Check

After any vine build or rebuild, walk the tree and verify structural integrity:
- Find any non-leaf nodes (depth > 0) with empty children arrays → log as orphans
- Find any nodes whose parent_id points to a non-existent node → log as broken parent refs
- Find any L0 nodes not assigned to any L1 cluster → log as unclustered

**Storage:** Before writing results, delete any previous integrity annotations (identified by `annotation_type = "health_check"`). Then store as a `HealthCheck` annotation on the highest-depth node that exists (fallback to L1 if no apex). Add `HealthCheck` variant to `AnnotationType` enum (same pattern as Era/Transition).

If no apex exists, the absence IS the most critical finding — return the integrity results as the function return value and HTTP response body, and store the annotation on the highest node available.

**REST semantics:** `POST /pyramid/:slug/vine/integrity` (has side effects — writes annotation). `GET` not appropriate since it would accumulate annotations per request.

### 5h. Sub-Apex Directory Wiring

**Problem:** The sub-apex layer only has direct children (2-3 L2/L3 nodes). To find which L1 cluster covers "DADBEAR design," you have to drill through multiple layers. The sub-apex should act as a directory for all L1 clusters it transitively covers.

**Solution:** After `build_vine_upper` completes, run a directory wiring pass:
1. Delete all existing `Directory` annotations on sub-apex nodes (cleanup before re-wiring)
2. For each sub-apex node (depth = apex.depth - 1), walk its subtree down to L1
3. Collect all L1 node IDs + headlines + topic names
4. Store as a `Directory` annotation on the sub-apex node (add `Directory` variant to `AnnotationType`, same pattern as Era/Transition/HealthCheck):
   ```json
   {
     "l1_refs": [
       {"id": "L1-003", "headline": "DADBEAR Implementation Sprint", "topics": ["stale detection", "delta chains"]},
       {"id": "L1-007", "headline": "Pyramid Visualization", "topics": ["canvas rendering", "glow effects"]}
     ]
   }
   ```
5. The vine drill-down endpoint reads `Directory` annotations by type (clean type-based filtering, no JSON parsing needed to distinguish from regular observations)

Everything is a contribution — the directory is an annotation, queryable via existing `/annotations` endpoint with type filter.

**Scale note:** For V1, ~18 L1 references per sub-apex node (~2-3KB JSON). If vine grows to 500+ bunches, consider splitting by subtree depth.

---

## Phase 6: HTTP API Routes

### New routes in `routes.rs`

| Method | Path | Purpose |
|--------|------|---------|
| POST | `/pyramid/vine/build` | Build vine from JSONL directory |
| POST | `/pyramid/:slug/vine/rebuild-upper` | Force L2+ rebuild (clears L2+, rebuilds from L1, re-runs intelligence) |
| GET | `/pyramid/:slug/vine/bunches` | List all bunches with metadata |
| GET | `/pyramid/:slug/vine/eras` | List detected ERAs |
| GET | `/pyramid/:slug/vine/decisions` | Decision evolution chains |
| GET | `/pyramid/:slug/vine/entities` | Resolved entities with aliases |
| GET | `/pyramid/:slug/vine/threads` | Cross-bunch thread continuity |
| GET | `/pyramid/:slug/vine/timeline` | Timeline data for visualization |
| POST | `/pyramid/:slug/vine/integrity` | Run integrity check, store result, return findings |

**Existing routes work automatically** on the vine slug: `/apex`, `/search`, `/tree`, `/drill`, `/annotations`, `/faq/*`

### Vine-to-Bunch Drill-Down

The vine's killer feature: "show me the actual conversation where this decision was made."

**New query function:** `vine_drill(vine_slug, vine_node_id)` returns:
- The bunch this vine node came from (`bunch_slug`, `bunch_index`, timestamps)
- The source node IDs in the bunch pyramid (apex or penultimate node)
- The bunch pyramid's full tree (drillable via existing `/drill` endpoint)

**Navigation chain:** vine node → `vine_drill` → bunch_slug + source_node_id → `/drill bunch_slug source_node_id` → L0 chunks → raw JSONL offset

**Route:** `GET /pyramid/:vine_slug/vine/drill/:node_id` — returns `{ bunch: VineBunch, source_nodes: [PyramidNode], bunch_tree: TreeNode }`

### Vine CLI Commands

Add to MCP server CLI (`mcp-server/src/cli.ts`):

| Command | Purpose |
|---------|---------|
| `vine build <jsonl_dir>` | Build vine from JSONL directory |
| `vine rebuild-upper <vine_slug>` | Force L2+ rebuild + intelligence re-run |
| `vine integrity <vine_slug>` | Run integrity check |
| `vine status <vine_slug>` | Build progress, bunch count, staleness |
| `vine eras <vine_slug>` | List detected ERAs with date ranges |
| `vine decisions <vine_slug> [query]` | Search decision evolution chains |
| `vine timeline <vine_slug>` | Temporal overview of all bunches |
| `vine drill <vine_slug> <node_id>` | Drill from vine node to source bunch |

These are the primary interface — the vine is used from Claude Code sessions, not a browser.

### Slug Naming

Flat naming with `--` separator (passes existing `validate_slug()` which only allows `[a-z0-9-]`):
- Vine slug: `vine-gne`
- Bunch slugs: `vine-gne--bunch-000` through `vine-gne--bunch-070`

Add `list_slugs_filtered()` to `db.rs` (or a `hidden` column to `pyramid_slugs`) so bunch slugs are excluded from default listings. The `GET /pyramid/slugs` endpoint filters by `--bunch-` substring. Pre-create bunch slugs via `db::create_slug()` before calling `ingest_conversation()` to avoid the unvalidated slug creation path in `ingest.rs`.

---

## Files

### New
| File | Est. Lines | Purpose |
|------|-----------|---------|
| `pyramid/vine.rs` | 800-1000 | Core vine: discovery, bunch build, vine construction, intelligence, live mode, JSONL watcher |
| `pyramid/vine_prompts.rs` | 200 | `VINE_CLUSTER_PROMPT`, `VINE_PHASE_CHECK_PROMPT`, `VINE_TRANSITION_PROMPT`, `VINE_ENTITY_RESOLUTION_PROMPT` |

### Modified
| File | Change |
|------|--------|
| `pyramid/mod.rs` | Add `pub mod vine; pub mod vine_prompts;` |
| `pyramid/types.rs` | Add `Vine` to ContentType; add `Era`, `Transition` to AnnotationType; add VineBunch, VineBunchMetadata, BunchDiscovery structs |
| `pyramid/db.rs` | Add `vine_bunches` table; migrate `pyramid_slugs` to remove CHECK constraint; add vine CRUD functions |
| `pyramid/routes.rs` | Add `ContentType::Vine` match arm (error); add vine HTTP routes; extract `run_build_pipeline` helper; vine drill-down endpoint |
| `mcp-server/src/cli.ts` | Add vine CLI commands: `vine build`, `vine status`, `vine eras`, `vine decisions`, `vine timeline`, `vine drill` |

### Reused Unchanged
| File | What |
|------|------|
| `ingest.rs` | `ingest_conversation()`, `ingest_continuation()` |
| `build.rs` | `build_conversation()`, all prompts, `DISTILL_PROMPT` |
| `query.rs` | `get_apex()`, all query functions |
| `delta.rs` | Delta chains, supersession mechanics |
| `webbing.rs` | Web edges (used via `pyramid_threads` for thread continuity) |
| `faq.rs` | FAQ entries for decisions + entity aliases |
| `llm.rs` | `call_model()`, `extract_json()`, model cascade |

---

## Cost Estimates

| Operation | Per bunch | 71-bunch total |
|-----------|-----------|----------------|
| Bunch pyramid build (~3 chunks avg) | ~$0.02 | ~$1.42 |
| Bunch pyramid build (~10 chunks avg) | ~$0.08 | ~$5.68 |
| Metadata extraction | $0 (mechanical) | $0 |
| Vine L0 assembly | $0 (mechanical) | $0 |
| Vine L1 clustering + distillation | — | ~$0.15 |
| Vine L2+/apex | — | ~$0.08 |
| Intelligence passes (all six) | — | ~$0.30 |
| **Total (short sessions)** | | **~$1.95** |
| **Total (mixed sessions)** | | **~$6.21** |
| **Incremental per new bunch** | | **~$0.05-0.15** |

Note: Per-bunch cost scales linearly with chunk count. Short sessions (~3 chunks) cost ~$0.02; long sessions (~20 chunks) cost ~$0.15. The 71-conversation total depends on session length distribution.

---

## Verification

1. **Schema:** Create vine slug with `content_type='vine'`, verify INSERT works on both new and existing databases
2. **Discovery:** Run `discover_bunches()` on real JSONL dir, verify count + temporal ordering
3. **2-bunch MVP:** Build 2 small bunches + vine. Verify vine L0 has ~6 nodes (apex + penultimate from each), vine apex summarizes both sessions
4. **10-bunch intelligence:** Build 10 bunches, run all 6 intelligence passes. Verify ERA annotations, FAQ entries, thread web edges appear via existing endpoints
5. **Live mode:** Start JSONL watcher, append messages to a file. Verify debounce → bunch rebuild → vine update propagation
6. **Full build:** All 71 bunches. Verify vine apex is coherent project summary. Query "DADBEAR" → results point to correct bunches and eras
7. **Cost check:** Profile 5 representative sessions to calibrate per-bunch cost estimates

---

## Design Decisions

1. **Everything is a contribution** — ERAs, decisions, entities, thread continuity, corrections are annotations/FAQ/web edges on the vine pyramid. No parallel tables.

2. **Apex + penultimate = vine L0** — The apex alone is too compressed. One level down gives the major topic clusters. ~3 nodes per bunch, ~213 total for 71 conversations.

3. **Flat slug naming with `--` separator** — `vine-gne--bunch-000` style. Passes existing `validate_slug()` (`[a-z0-9-]` only). Filterable by `--bunch-` substring. No slashes (stripped by `slugify()`, rejected by `validate_slug()`, ambiguous in URLs).

4. **Direct notification, not cross-slug DADBEAR** — After bunch build, directly call vine update. Vine-internal DADBEAR handles L1+ propagation. No new cross-slug watching infrastructure.

5. **`run_build_pipeline` helper** — Extracts WriteOp channel boilerplate from routes.rs so both route handler and vine can call `build_conversation()` cleanly.

6. **Migrate CHECK constraint** — Recreate `pyramid_slugs` table with `'vine'` added to CHECK, in a transaction with `PRAGMA foreign_keys=OFF`. Idempotent (skip if already includes 'vine').

7. **New `VINE_CLUSTER_PROMPT`** — Can't reuse `THREAD_CLUSTER_PROMPT` (expects L1 topic JSON). Vine needs its own prompt for temporal-topic clustering.

8. **`pyramid_threads` for thread continuity** — Web edges require thread IDs, not node IDs. Create proper vine-level threads for cross-bunch narrative strands.

9. **New annotation types: `Era`, `Transition`, `HealthCheck`, `Directory`** — Extend the enum rather than overloading Observation. Each machine-generated contribution type gets its own variant for clean type-based filtering.

10. **JSONL mtime polling, not `PyramidFileWatcher`** — The existing watcher is for source code files. JSONL watching is a simpler `tokio::interval` poll with debounce.

11. **Vine bypasses `active_build` singleton** — `active_build` is global, one-at-a-time. Vine manages its own cancellation token. Normal HTTP builds can run concurrently on different slugs.

12. **Intelligence passes are rerunnable, not preserved** — When vine L1+ nodes are rebuilt, cascade delete removes attached annotations. Re-run intelligence passes instead of migrating annotations. Cheap (~$0.05) and produces fresh results from current state.

13. **`building` intermediate state for crash recovery** — `pending → building → built | error`. On startup, `building` bunches get cleaned up (cascade delete partial data) and retried.

14. **JSONL read safety** — Read to last complete newline boundary. Track byte offset for continuation, not message count. Prevents data loss from concurrent Claude Code writes.

15. **Shared JSONL line parser** — Extract `parse_jsonl_line()` helper used by both `ingest.rs` and `vine.rs`. One parser for the format, different consumers.

16. **LLM-only clustering** — No algorithmic/LLM split. LLM handles all clustering at ~$0.0002 per call. Catches semantic similarity that string-matching misses. One code path to implement, test, debug.

17. **`VineDecision`/`VineCorrection` wrapper types** — Decisions and corrections carry `bunch_index` and `bunch_ts` for cross-bunch evolution chains without round-trips to the bunch wrapper.

18. **Vine-to-bunch drill-down** — The killer query: vine node → bunch → chunk → raw conversation. Specified as a first-class endpoint and CLI command, not afterthought plumbing.

19. **Session ID tiebreaker** — Bunch ordering uses `first_ts` then `session_id` for deterministic ordering when timestamps collide.

20. **Force L2+ rebuild** — When sub-apex nodes lose children due to reassignment, `force_rebuild_vine_upper` clears L2+ and reconstructs from L1. Intelligence passes re-run automatically (cascade delete + rerun is the pattern, not migration).

21. **Post-build integrity check as contribution** — Orphan detection, broken parent refs, unclustered L0s — all stored as an Observation annotation on the vine apex. The vine knows its own health via the same contribution protocol.

22. **Sub-apex directory wiring as contribution** — Directory of all L1 clusters reachable from each sub-apex node, stored as Observation annotations with `type: "directory"`. Enables quick navigation without drilling through intermediate layers.
