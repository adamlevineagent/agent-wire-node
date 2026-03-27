# Question Pyramid Builder v3 — Conductor Implementation Plan

> **Status:** Ready for implementation
> **Canonical spec:** `docs/pyramid-builder-v3.md`
> **Repos:** agent-wire-node (primary), GoodNewsEveryone (Wire enhancements)
> **Branch:** `research/chain-optimization`

---

## Delta: What's Built vs What's Needed

### Already Built (functional)
- Question decomposition (`question_decomposition.rs` — 1,591 lines)
- Question compiler to IR (`question_compiler.rs` — 1,582 lines)
- Question YAML v3.0 types + loader
- IR executor with converge expansion (`chain_executor.rs` — 10,321 lines)
- Defaults adapter (YAML → IR)
- Build runner with decomposed build path
- HTTP routes: `/pyramid/:slug/build/question`, `/build/preview`
- Wire server accepts `pyramid_node` contributions
- Rotator arm with weight-to-slot conversion
- Cross-layer web edges system

### Not Built (the plan requires)
1. **Characterize step** — material profiling + user checkpoint before build
2. **Dynamic extraction schema** — the meta-prompt that generates question-shaped prompts (currently generic, scored 3.5/10)
3. **Evidence-weighted answering** — KEEP/DISCONNECT/MISSING verdicts with weights
4. **Horizontal pre-mapping** — LLM-driven candidate assignment before answering
5. **Reconciliation engine** — orphan detection, gap reports, central node identification
6. **Wire publication pipeline** — bottom-up publish with derived_from, handle-path mapping, idempotency
7. **7 local SQLite tables** — evidence, question_tree, gaps, id_map, deltas, supersessions, staleness_queue
8. **Crystallization** — staleness + supersession propagation channels
9. **4 code fixes** — evidence weights, source_type, idempotency, locking
10. **Wire enhancements** — batch publication or annotation enrichment

---

## Phase 1: Foundation (3 parallel workstreams)

All three are independent — no cross-workstream dependencies.

### WS1-A: Local Schema Migration
**Repo:** agent-wire-node
**Files:** `src-tauri/src/db.rs` (or schema module), migration SQL

Create 7 new SQLite tables per the plan:

```sql
-- pyramid_evidence: many-to-many weighted evidence links
CREATE TABLE pyramid_evidence (
  id INTEGER PRIMARY KEY,
  slug TEXT NOT NULL,
  source_node_id TEXT NOT NULL,      -- child node (evidence provider)
  target_node_id TEXT NOT NULL,      -- parent node (question answerer)
  verdict TEXT NOT NULL,             -- KEEP, DISCONNECT, MISSING
  weight REAL,                       -- 0.0-1.0, NULL for DISCONNECT/MISSING
  reason TEXT,
  created_at TEXT DEFAULT (datetime('now')),
  UNIQUE(slug, source_node_id, target_node_id)
);

-- pyramid_question_tree: decomposition tree per slug
CREATE TABLE pyramid_question_tree (
  id INTEGER PRIMARY KEY,
  slug TEXT NOT NULL,
  question_id TEXT NOT NULL,
  parent_question_id TEXT,           -- NULL for apex question
  question_text TEXT NOT NULL,
  is_leaf INTEGER NOT NULL DEFAULT 0,
  depth INTEGER NOT NULL DEFAULT 0,
  scope TEXT,                        -- about: value
  creates TEXT,                      -- creates: value
  created_at TEXT DEFAULT (datetime('now')),
  UNIQUE(slug, question_id)
);

-- pyramid_gaps: MISSING evidence reports
CREATE TABLE pyramid_gaps (
  id INTEGER PRIMARY KEY,
  slug TEXT NOT NULL,
  question_id TEXT NOT NULL,
  description TEXT NOT NULL,
  layer INTEGER NOT NULL,
  bounty_id TEXT,                    -- Wire contribution ID if bounty created
  resolved INTEGER NOT NULL DEFAULT 0,
  created_at TEXT DEFAULT (datetime('now'))
);

-- pyramid_id_map: local ID → Wire handle-path
CREATE TABLE pyramid_id_map (
  id INTEGER PRIMARY KEY,
  slug TEXT NOT NULL,
  local_id TEXT NOT NULL,
  wire_handle_path TEXT NOT NULL,
  wire_uuid TEXT,
  published_at TEXT DEFAULT (datetime('now')),
  UNIQUE(slug, local_id)
);

-- pyramid_deltas: per-file change log for crystallization
CREATE TABLE pyramid_deltas (
  id INTEGER PRIMARY KEY,
  slug TEXT NOT NULL,
  file_path TEXT NOT NULL,
  change_type TEXT NOT NULL,         -- ADDITION, MODIFICATION, SUPERSESSION
  diff_summary TEXT,
  detected_at TEXT DEFAULT (datetime('now')),
  processed INTEGER NOT NULL DEFAULT 0
);

-- pyramid_supersessions: belief correction audit trail
CREATE TABLE pyramid_supersessions (
  id INTEGER PRIMARY KEY,
  slug TEXT NOT NULL,
  node_id TEXT NOT NULL,
  superseded_claim TEXT NOT NULL,
  corrected_to TEXT NOT NULL,
  source_node TEXT,                  -- node that revealed the correction
  channel TEXT NOT NULL,             -- weight_staleness or belief_supersession
  created_at TEXT DEFAULT (datetime('now'))
);

-- pyramid_staleness_queue: pending re-answer work items
CREATE TABLE pyramid_staleness_queue (
  id INTEGER PRIMARY KEY,
  slug TEXT NOT NULL,
  question_id TEXT NOT NULL,
  reason TEXT NOT NULL,
  channel TEXT NOT NULL,             -- weight_staleness or belief_supersession
  priority REAL NOT NULL DEFAULT 0,  -- higher = more urgent
  enqueued_at TEXT DEFAULT (datetime('now')),
  processed_at TEXT
);
```

Backfill: migrate existing `children` JSON arrays in `pyramid_nodes` → `pyramid_evidence` rows (verdict=KEEP, weight=1.0). Add indexes on (slug, source_node_id) and (slug, target_node_id).

**Acceptance:** All tables created. Existing pyramids still queryable. pyramid_evidence backfilled from children arrays.

### WS1-B: Characterize Step
**Repo:** agent-wire-node
**Files:** new `src-tauri/src/characterize.rs`, modify `build_runner.rs`

Implement Step 1.1 from the plan:

1. **`characterize()`** function:
   - Input: apex question + source folder path
   - Reads folder map (file names, extensions, directory structure — NO file content)
   - One LLM call: "Given this question and these files, what kind of material is this? What is the user really asking? Who is the audience? What tone?"
   - Output: `CharacterizationResult { material_profile, interpreted_question, audience, tone }`
   - Model: use "max" tier (this is a judgment call, not extraction)

2. **User checkpoint** (two-chain pattern):
   - After characterize returns, build pauses
   - Result stored in build state
   - New endpoint: `POST /pyramid/:slug/build/confirm` resumes with (optionally corrected) characterization
   - Alternative: `POST /pyramid/:slug/build/question` accepts optional `characterization` override to skip auto-characterize

3. Wire into `run_decomposed_build()` in build_runner.rs:
   - Characterize → return to caller → wait for confirm → decompose → compile → execute

**Acceptance:** Characterize endpoint works. Build pauses for confirmation. User can override interpretation.

### WS1-C: Wire Publish Fixes
**Repo:** agent-wire-node
**Files:** `src-tauri/src/wire_publish.rs`, `src-tauri/src/crystallization.rs`

Fix the 4 code bugs from the plan:

1. **Evidence weights** — `derived_from` weights currently hardcoded to 1.0. Change to pass actual evidence weights from `pyramid_evidence` table. Normalize KEEP weights to sum=1.0.

2. **Source document citation** — L0 nodes hardcode `source_type: "contribution"`. Change to `source_type: "source_document"` when citing source files, with `ref` set to corpus path.

3. **Publication idempotency** — Before publishing, check `pyramid_id_map` for existing handle-path. If already published, skip (or update via supersede if content changed). Make re-runs safe.

4. **Crystallization locking** — Add per-node mutex in `crystallization.rs` so concurrent deltas don't drop corrections. Use a DashMap<String, Mutex<()>> keyed by node_id.

**Acceptance:** Evidence weights flow through to Wire. L0 nodes cite source_document. Re-running publish doesn't create duplicates. Concurrent crystallization deltas don't race.

---

## Phase 2: Question-Shaped Engine (3 parallel workstreams)

Depends on Phase 1 (schema tables must exist). All three Phase 2 workstreams are independent of each other.

### WS2-A: Dynamic Extraction Schema Generation
**Repo:** agent-wire-node
**Files:** new `src-tauri/src/extraction_schema.rs`, modify `question_decomposition.rs`, modify chain prompts

This is THE critical quality gap. Currently 3.5/10 on question-matched evaluation.

Implement Step 1.3 from the plan — the meta-prompt that generates all downstream prompts:

1. **`generate_extraction_schema()`** function:
   - Input: all leaf questions from decomposition + material_profile + audience + tone
   - One LLM call (max tier): "Given these leaf questions, generate:
     - An extraction prompt that tells L0 extraction EXACTLY what to look for (not 'list everything' — specifically what the downstream questions need)
     - A topic schema (what fields each node should have — varies by question domain)
     - Orientation guidance (how detailed, what tone)"
   - Output: `ExtractionSchema { extraction_prompt, topic_schema, orientation_guidance }`

2. **Store schema as first-class artifact:**
   - Save alongside pyramid in `pyramid_question_tree` metadata or new column
   - Referenced by every subsequent prompt in the build

3. **Generate synthesis prompts** (Step 5 from handoff — runs AFTER L0):
   - After L0 extraction completes, generate per-layer synthesis prompts
   - Input: question tree + actual L0 results + schema
   - Output: per-question answering instructions that reference actual extracted evidence

4. **Wire into decomposed build flow:**
   - After decompose, before L0: generate extraction schema
   - After L0, before L1: generate synthesis prompts
   - Each prompt is question-shaped, not generic

**Acceptance:** Build a test pyramid with a specific question (e.g., "How does the stale engine detect and propagate staleness?"). Extraction should pull ONLY stale-engine-relevant details. Score ≥7/10 on question-matched evaluation.

### WS2-B: Evidence-Weighted Answering
**Repo:** agent-wire-node
**Files:** new `src-tauri/src/evidence_answering.rs`, modify chain executor, new prompts

Implement Steps 3.1 + 3.2 from the plan:

1. **Horizontal Pre-Mapping** (Step 3.1):
   - New function: `pre_map_layer(questions, lower_layer_nodes) → CandidateMap`
   - One LLM call reads ALL questions for a layer + ALL nodes from below
   - Returns: `{ question_id → [candidate_node_ids] }` — intentionally over-inclusive
   - Model: mercury-2 (fast, this is classification not synthesis)

2. **Vertical Answering** (Step 3.2):
   - New function: `answer_question(question, candidates, synthesis_prompt) → AnsweredNode`
   - Parallel, 5x concurrency
   - Each question answered independently
   - Prompt instructs: "Answer this question using these candidates. For each, report KEEP(weight, reason), DISCONNECT(reason), or MISSING(what you wish you had)"
   - Output includes evidence array with verdicts
   - Save evidence to `pyramid_evidence` table
   - Save answered nodes to `pyramid_nodes`

3. **Wire into layer loop:**
   - Replace current clustering/synthesis flow with: pre-map → answer → (reconcile, next workstream)
   - Preserve converge expansion for when node count > threshold

**Acceptance:** L1 nodes have evidence arrays with weights. DISCONNECT entries exist. At least some MISSING entries appear. Evidence rows in pyramid_evidence table.

### WS2-C: Reconciliation Engine
**Repo:** agent-wire-node
**Files:** new `src-tauri/src/reconciliation.rs`

Implement Step 3.3 from the plan — purely mechanical, no LLM:

1. **`reconcile_layer()`** function:
   - Input: all answered questions at layer + all nodes from layer below + evidence links
   - Identifies:
     - **Orphan nodes**: lower-layer nodes never referenced by ANY question (not even DISCONNECT — completely overlooked)
     - **Gap reports**: questions reporting MISSING evidence → save to `pyramid_gaps`
     - **Central nodes**: nodes cited by 3+ questions with avg weight > 0.5 (cross-cutting concerns)
   - Output: `ReconciliationResult { orphans, gaps, central_nodes, weight_map }`

2. **Orphan handling:**
   - Orphan nodes NOT published (credits saved)
   - Logged for operator review
   - Could become future bounty targets

3. **Gap handling:**
   - Each gap saved to `pyramid_gaps` table
   - Optional: generate bounty contribution on Wire for each gap (deferred — just save for now)

4. **Wire into layer loop:**
   - After answering, before web edges: reconcile
   - Orphan list used by publication step (skip orphans)

**Acceptance:** After a build, `pyramid_gaps` has entries. Orphan nodes identified and excluded from publication list. Central nodes flagged.

---

## Phase 3: Publication & Wire (2 parallel workstreams)

Depends on Phase 2 (evidence answering must work, reconciliation must identify orphans).

### WS3-A: Full Publication Pipeline
**Repo:** agent-wire-node
**Files:** `src-tauri/src/wire_publish.rs`, new publication orchestrator

Implement Step 3.5 from the plan — bottom-up publication with handle-path tracking:

1. **Publication orchestrator:**
   - `publish_layer(slug, layer, non_orphan_nodes, evidence_links, web_edges) → Vec<HandlePath>`
   - For each non-orphan node at this layer:
     - Check `pyramid_id_map` — skip if already published (idempotency from WS1-C)
     - Build contribution payload:
       ```
       type: pyramid_node
       body: distilled orientation text
       structured_data: { depth, topics, entities, evidence_full, web_edges, question, gaps }
       derived_from: KEEP entries only, weights normalized, refs are handle-paths (from pyramid_id_map)
       ```
     - For L0: `derived_from` cites source files with `source_type: "source_document"`
     - For L1+: `derived_from` cites published lower-layer nodes by handle-path
   - After Wire returns handle-path → save to `pyramid_id_map`
   - Return all new handle-paths for next layer to reference

2. **Bottom-up ordering:**
   - Publish L0 (non-orphan) → get handle-paths → publish L1 citing L0 handle-paths → ... → apex
   - Each layer waits for previous layer's handle-paths

3. **Error recovery:**
   - If publish fails mid-layer, next run picks up from `pyramid_id_map` (skip already-published)
   - Log failed publications for retry

**Acceptance:** Full pyramid published to Wire. Each node has correct `derived_from` with handle-paths. `pyramid_id_map` populated. Re-run doesn't duplicate.

### WS3-B: Wire Server Enhancements
**Repo:** GoodNewsEveryone
**Files:** contribution routes, new batch endpoint or annotation endpoint

Implement the Wire enhancements from the plan (pick the simpler option for each):

1. **Batch publication** (preferred over handle-path reservation):
   - New endpoint: `POST /api/v1/contribute/batch`
   - Accepts array of contribution payloads
   - Processes in order, returns array of results (handle-paths)
   - Same validation as single contribute, but one HTTP round-trip
   - Transaction: all-or-nothing per batch (or partial with error array)

2. **Annotation enrichment** (for web edges post-publish):
   - New endpoint: `PATCH /api/v1/contributions/:id/structured_data`
   - Additive merge into existing `structured_data` JSONB
   - Only the contribution's creator can enrich
   - Core `body` stays immutable
   - Use case: add `web_edges` after initial publication

3. **Corpus document citation verification:**
   - Verify `derived_from` accepts `source_type: "source_document"`
   - Verify rotator arm handles source document citations (no slots for source docs that aren't Wire contributions — all 28 slots go to actual Wire contribution sources, or 0 source slots if all citations are source docs)

**Acceptance:** Batch endpoint works for 10+ contributions. Annotation enrichment adds web_edges to existing contribution. Source document citations don't break rotator arm.

---

## Phase 4: Crystallization (2 parallel workstreams)

Depends on Phase 3 (publication pipeline must work). These two channels are independent.

### WS4-A: Weight-Based Staleness
**Repo:** agent-wire-node
**Files:** modify `src-tauri/src/crystallization.rs`, `pyramid_staleness_queue`

Implement Channel A from the plan:

1. **Delta detection:**
   - On DADBEAR source change: extract delta (ADDITION / MODIFICATION / SUPERSESSION)
   - Save to `pyramid_deltas`

2. **Weight propagation:**
   - For each changed file → find L0 node → trace evidence weights upward
   - L0 node with evidence weight 0.95 to L1 question → L1 is probably stale
   - Attenuation: multiply weights through layers (0.95 × 0.8 = 0.76 at L2)
   - Configurable threshold (e.g., 0.3) — questions above threshold → staleness queue

3. **Re-answer dispatch:**
   - Dequeue from `pyramid_staleness_queue` by priority
   - Re-run pre-mapping → answering → reconciliation for affected questions
   - New nodes supersede old Wire contributions
   - Bottom-up publication of replacements

**Acceptance:** Modify a source file → staleness detected → affected questions identified → re-answered → new nodes published with supersession.

### WS4-B: Belief Supersession
**Repo:** agent-wire-node
**Files:** modify `src-tauri/src/crystallization.rs`, `pyramid_supersessions`

Implement Channel B from the plan:

1. **Contradiction detection:**
   - Delta extraction identifies when a change CONTRADICTS a specific claim
   - E.g., "validate_token() now checks expiry" contradicts "validate_token() does not check expiry"
   - One LLM call per delta: "Does this change contradict any claims in these nodes?"

2. **Non-attenuating trace:**
   - Trace through EVERY node containing the superseded claim
   - Does NOT attenuate through layers (unlike Channel A)
   - Cannot be dismissed by operator

3. **Correction audit trail:**
   - Save to `pyramid_supersessions` table
   - Each affected node gets `structured_data.supersession_history` entry
   - Re-answer with forced correction

**Acceptance:** Introduce a contradiction in source → belief supersession detected → traces through all affected nodes → correction recorded → nodes re-published with supersession history.

---

## Phase 5: Integration & Cleanup (serial)

Depends on all previous phases.

### WS5-A: End-to-End Validation
**Repo:** agent-wire-node

1. Build a test pyramid with a specific question against agent-wire-node source
2. Verify: characterize → confirm → decompose → extract (question-shaped) → pre-map → answer with evidence → reconcile → web edges → publish → handle-paths tracked
3. Verify Wire contributions have correct structure
4. Verify rotator arm fires correctly on node access
5. Modify a source file → verify crystallization triggers

### WS5-B: Parity Validation & Legacy Cleanup
**Repo:** agent-wire-node

1. Run parity.rs: IR executor vs legacy executor on same input, diff outputs
2. Confirm IR produces equivalent or better results
3. Flip IR executor to default (feature flag)
4. Delete legacy executor code (~2,600 lines)
5. Delete defaults adapter (replaced by question compiler)
6. Delete content-type-specific prompt files (replaced by meta-prompt)

---

## Frontend / UX Workstreams (embedded in each phase)

Per Adam's preference — always include frontend alongside backend:

- **Phase 1:** Characterize result shown in Partner chat with confirm/edit UI
- **Phase 2:** Evidence verdicts visible in drill view (KEEP with weight, DISCONNECT with reason)
- **Phase 3:** Publication progress indicator, handle-path links in pyramid viewer
- **Phase 4:** Staleness indicators on nodes, supersession history in node detail view
- **Phase 5:** Toggle for IR vs legacy in settings UI (then remove toggle after cleanup)

---

## Risk Notes

- **Wire server batch endpoint** — New endpoint, needs its own audit cycle after implementation.
- **Crystallization LLM cost** — Belief supersession checks require LLM calls per delta. Could be expensive on large pyramids. Consider batching.
