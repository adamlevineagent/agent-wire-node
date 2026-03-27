# Integration Wiring Plan: Local-First Pyramid Pipeline

## Context

All modules are built, tested, and audit-verified. Five modules (evidence_answering, reconciliation, publication, staleness, supersession) are dead code — nothing calls them. This plan wires them into the build pipeline with a local-first architecture.

**Audit status:** Stage 1 informed audit complete (2 independent auditors). All findings incorporated below.

## Architecture: Local Is Default, Wire Is Projection

The pyramid is a LOCAL artifact. It works fully offline in SQLite. Wire publication is an explicit, optional export step.

**Local namespace:**
- Source files → content-addressed `sha256:{first-12}` (deterministic, survives renames)
- Pyramid nodes → `L{depth}-{uuid}` (already done)
- Evidence links → local node IDs on both sides
- `derived_from` → local IDs only during build
- Everything in SQLite, everything works offline

**Publication boundary (optional, explicit):**
- User triggers "publish to Wire"
- Register source files as corpus documents → Wire corpus IDs
- Publish nodes bottom-up → Wire contribution IDs
- `pyramid_id_map` populated only at this step
- `derived_from` rewritten local→Wire at publication time

## What Changes

### Phase 0: Fix apex depth (question_compiler.rs)

**The problem:** Apex nodes are stored as `depth: 99` in the DB. The `question_compiler.rs:635` hardcodes `depth: Some(99)` as a placeholder. `assign_apex_storage_depth` tries to replace it but `apex_depth_for_question_set` misses converge-expanded steps.

**The fix:** Fix `apex_depth_for_question_set` to scan all compiled steps (including converge-expanded) for the real max depth, then set apex to `max + 1`. This fixes the legacy (non-evidence) path.

**Post-WS-AB note:** Under the new evidence pipeline, the apex is created by the evidence loop at the correct depth from the layer counter. The compiler fix is still needed for the legacy build path. The executor safety net (`highest_saved_depth`) is irrelevant in L0-only mode since it would be 0 — don't rely on it.

### Phase 1: Local Build Pipeline (evidence-weighted answering)

**The problem:** `run_decomposed_build` compiles the question tree into an ExecutionPlan, then hands it to `execute_plan`. The executor runs L0 extraction, clustering, and synthesis as IR steps. Upper layers use the old generic synthesis prompts, not the evidence-weighted answering system.

**The fix:** After `execute_plan` returns L0 nodes, add a post-execution loop in `run_decomposed_build` that drives L1+ through the evidence pipeline:

```
run_decomposed_build():
  1. characterize (existing)
  2. decompose question (existing)
  3. generate extraction schema (existing)
  4. compile to IR + patch L0 (existing)
  5. filter plan to depth=0 steps only, execute_plan for L0 ONLY
     - Validate filtered DAG after removing non-L0 steps
  --- NEW: evidence-weighted upper layer loop ---
  5b. load L0 nodes from SQLite (db::get_nodes_at_depth(conn, slug, 0))
  5c. build l0_results_summary string (concat headlines + distilled, truncated to token budget)
  6. generate_synthesis_prompts(question_tree, l0_results_summary, extraction_schema, llm_config)
     → Returns SynthesisPrompts { pre_mapping_prompt, answering_prompt, web_edge_prompt }
  6b. extract_layer_questions(question_tree) → HashMap<i64, Vec<LayerQuestion>>
     - NEW FUNCTION: walks tree, assigns depth from position, generates stable question IDs
  7. for layer in 1..=max_depth:
     - CHECK cancel.is_cancelled() at top of each iteration
     - layer_qs = layer_questions[layer]
     - lower_nodes = load nodes at (layer - 1) from SQLite
     a. pre_map_layer(layer_qs, lower_nodes, llm_config)
        → CandidateMap
     b. answer_questions(layer_qs, candidate_map, lower_nodes, synth.answering_prompt, llm_config)
        → Vec<AnsweredNode> (LLM calls only, NO DB writes)
     c. spawn_blocking: save answered nodes + evidence links + gaps to SQLite
     d. spawn_blocking: reconcile_layer(conn, slug, layer, answered_ids, lower_ids)
     e. send progress update via progress_tx
     f. if error at this layer: log, return partial result (L0..L(layer-1) are valid)
  7b. create apex node from final layer answers (depth = max_depth + 1 from loop counter)
  8. return build result with quality metrics
```

**Key decisions:**
- L0 runs through the IR executor (file-type dispatch, parallelism, structured outputs). L1+ through evidence pipeline.
- `answer_questions` returns results WITHOUT writing to DB. The caller persists in `spawn_blocking`. This solves the `&Connection` / `!Send` / `Arc<Mutex>` problem.
- Cancellation checked at each layer boundary.
- Progress reporting: after execute_plan returns, reset counter for evidence phase.
- Per-layer error handling: catch per-layer, don't abort the whole build.

**How to stop execute_plan after L0:** Filter the plan to only include depth=0 steps before passing to execute_plan. After filtering, validate the DAG has no broken dependency references.

### Phase 2: Local Storage (save locally, no Wire)

**New function: `save_pyramid_locally()`** in a new `local_store.rs` module.

After the evidence loop completes, persist the full pyramid state:
- All nodes (L0 through apex) → `pyramid_nodes` (L0 via executor, L1+ via evidence loop)
- Evidence links → `pyramid_evidence` (saved per-layer in step 7c)
- Question tree → `pyramid_question_tree` (already saved by decompose)
- Gaps → `pyramid_gaps` (saved per-layer in step 7c, from answer_questions missing reports)
- Build metadata → new `pyramid_builds` table (question, timestamp, quality score, layers)

Note: L0 nodes have no evidence links (they come from source extraction, not evidence answering). This is by design — L0 evidence would be "this chunk came from this file" which is already tracked by `pyramid_file_hashes`. Document this asymmetry.

### Phase 3: Wire Publication (explicit, separate)

**New route: `POST /pyramid/:slug/publish`**

Reads the local pyramid state, translates IDs, publishes to Wire:
1. Register source files as corpus documents via Wire API (`POST /api/v1/wire/corpora/[slug]/documents` — endpoint exists on Wire side)
2. Publish L0 nodes with `source_type: "source_document"`, `derived_from` citing corpus IDs
3. Publish L1+ nodes with `source_type: "contribution"`, `derived_from` citing Wire contribution IDs
4. Populate `pyramid_id_map` with local→Wire mappings
5. Return publication manifest

`publication.rs` already does most of this. The change is:
- Remove Wire publication from the build loop
- Add corpus registration step (new client function in `wire_publish.rs` or `publication.rs`)
- Make it a standalone operation triggered by the user
- Continue using `make_placeholder_uuid` as fallback until corpus registration is fully wired

### Phase 4: Staleness + Supersession (file watcher integration)

**New route: `POST /pyramid/:slug/check-staleness`**

**IMPORTANT: Two staleness systems exist and must be bridged:**
- **DADBEAR (existing, wired):** `watcher.rs` → `write_mutation` → `pyramid_pending_mutations` → stale engine → `StaleCheckResult`
- **Crystallization (dead code):** `staleness.rs` → `detect_source_changes` → `pyramid_source_deltas` → `propagate_staleness` → `pyramid_staleness_queue`

**Bridge approach:** After DADBEAR's stale engine processes mutations, it calls into the crystallization pipeline. This avoids duplicate event handling:
1. DADBEAR detects file changes (existing)
2. DADBEAR stale engine processes mutations (existing)
3. **NEW:** Bridge calls `detect_source_changes()` with the same file list
4. `propagate_staleness()` — propagate scores through evidence graph
5. `detect_contradictions()` — LLM check for belief supersession
6. `trace_supersession()` + `record_supersession()` — trace + record
7. `process_staleness_queue()` — dequeue items for re-answering
8. Re-run evidence answering for affected questions only

**Debounce:** File watcher events must be debounced (2-3 seconds) before triggering the crystallization pipeline. DADBEAR already has a 5-minute debounce; the bridge should use this same window.

## Workstream Breakdown

### WS-0: Fix apex depth (question_compiler.rs)
- Fix `apex_depth_for_question_set` to scan compiled steps (including converge-expanded) for real max depth
- Kept for legacy build path correctness; evidence path uses loop counter
- ~30 lines changed

### WS-AB: L0 filter + evidence loop (build_runner.rs, question_decomposition.rs)
**Combined workstream** (WS-A and WS-B were incorrectly marked parallel — they modify the same function at the same insertion point)

- Filter ExecutionPlan to depth=0 steps, validate DAG after filtering (~40 lines)
- NEW: Add stable `id` field to `QuestionNode` (content-hash of `question + about + depth`) so IDs survive re-decomposition and staleness can track by question_id
- NEW: `extract_layer_questions(tree: &QuestionTree) -> HashMap<i64, Vec<LayerQuestion>>` in question_decomposition.rs (~50 lines)
- NEW: `build_l0_summary(nodes: &[PyramidNode]) -> String` helper (~15 lines)
- Add token budget guard to `pre_map_layer`: if combined prompt exceeds ~80K chars, batch into multiple LLM calls or use headlines-only mode (~20 lines)
- Evidence loop in run_decomposed_build: per-layer pre_map → answer → spawn_blocking save → reconcile (~100 lines)
- Refactor `answer_questions` to return `Vec<AnsweredNode>` WITHOUT DB writes; caller persists in spawn_blocking (~20 lines refactor)
- Cancellation checks at layer boundaries (~5 lines)
- Progress reporting: reset counter after L0, increment per-layer (~10 lines)
- Apex creation at correct depth from loop counter (~15 lines)
- Per-layer error handling with partial-result return (~15 lines)
- Remove `EvidenceVerdict::Missing` from evidence links path (keep in enum for type completeness, but document it is never stored — gaps use `pyramid_gaps` table instead)
- Rename `delta.rs::propagate_staleness` to `propagate_staleness_parent_chain` to avoid name collision with `staleness.rs::propagate_staleness`
- **Total: ~300 lines** (new + refactored)

### WS-C: Local store module (local_store.rs)
- `save_build_metadata()` — save build question, timestamp, quality metrics
- `get_build_summary()` — retrieve latest build state
- `pyramid_builds` table in db.rs init
- ~80 lines

### WS-D: Publication route (routes.rs + publication.rs refactor)
- New `POST /pyramid/:slug/publish` route handler
- Fix `publication.rs::publish_layer` async/Connection issue: split into sync DB-read phase + async publish phase, drop conn before first `.await`
- Replace `make_placeholder_uuid`'s `DefaultHasher` with `Uuid::new_v5` (stable across Rust versions)
- New corpus registration client function (calls Wire API `POST /api/v1/wire/corpora/{slug}/documents`)
- Remove Wire concerns from build pipeline
- Fallback: continue using placeholder UUIDs until corpus registration fully wired
- ~170 lines

### WS-E: Staleness route + DADBEAR bridge (routes.rs + staleness integration)
- New `POST /pyramid/:slug/check-staleness` route handler
- **Bridge**: Hook DADBEAR stale engine output into `staleness.rs` + `supersession.rs` (the direct function paths, NOT the `crystallization.rs` chain-template system — those are separate)
- Re-answer loop for queued items
- Debounce integration with DADBEAR's existing 5-minute window
- ~180 lines (higher than original estimate due to bridge work)

### WS-F: Frontend (if applicable)
- Build status UI showing evidence quality per layer
- "Publish to Wire" button (separate from build)
- Staleness indicators on pyramid view

## Execution Order

0. **WS-0** — Fix apex depth. Quick, unblocks visual testing.
1. **WS-AB** — Core pipeline wiring. Single workstream, sequential. Makes question pyramids produce evidence-grounded output.
2. **WS-C** — Local storage. Quick, adds build metadata.
3. **WS-D** — Publication. Separate Wire export with corpus registration.
4. **WS-E** — Staleness + DADBEAR bridge. Background crystallization loop.
5. **WS-F** — Frontend. Adam tests by feel.

## Acceptance Criteria

- [ ] Apex node depth is `max_layer + 1`, not 99 (pyramid visualization shows correct hierarchy)
- [ ] `POST /pyramid/:slug/build/question` produces L0 through apex with evidence links
- [ ] Evidence links stored in `pyramid_evidence` with KEEP/DISCONNECT verdicts
- [ ] Gaps stored in `pyramid_gaps` with question references
- [ ] No Wire API calls during build (local-only)
- [ ] Build cancellation works during evidence loop (checked per-layer)
- [ ] Progress UI shows evidence loop progress, not frozen at L0 complete
- [ ] Partial builds are valid (if L2 fails, L0+L1 are queryable)
- [ ] `POST /pyramid/:slug/publish` publishes to Wire with correct derived_from
- [ ] Source files registered as corpus documents, not contributions
- [ ] Staleness propagation works when source files change (via DADBEAR bridge)
- [ ] `cargo check` clean, `cargo test` passing

## Risk Notes

- L0 prompt patching still uses the build_runner heuristic (documented, acceptable)
- Evidence answering quality depends on LLM prompt quality — the meta-prompt generates per-pyramid prompts
- Staleness re-answering reuses the same evidence pipeline (single code path, no divergence)
- `SynthesisPrompts.answering_prompt` is currently a single prompt for all layers — acceptable for v1, but per-layer prompts (calling `generate_synthesis_prompts` once per layer with actual lower-layer nodes) would improve quality for deep pyramids. Track as v2 enhancement.
- Corpus registration on Wire side exists (`/api/v1/wire/corpora/`) but client-side wiring is new work. Placeholder UUIDs are the fallback.
- `reconcile_layer`'s gap detection from evidence links is redundant — `answer_questions` already saves gaps directly from MISSING reports. The reconcile step still provides orphan detection and central node identification, so it's worth keeping.

## Audit Log

### Stage 1 Findings (incorporated above)
- **C1/I1**: No QuestionTree→LayerQuestion converter → Added `extract_layer_questions` to WS-AB
- **C2**: async/Connection !Send issue → answer_questions returns without DB writes, caller uses spawn_blocking
- **C3/I2**: generate_synthesis_prompts needs 4 args + L0 summary → Added L0 summary step, full signature in pseudocode
- **M1**: WS-A+B not parallel → Combined into WS-AB
- **M2**: Apex depth fix irrelevant post-WS-A → Documented, kept for legacy path
- **M3**: Single synthesis prompt for all layers → Documented as v2 enhancement
- **M4/I3**: reconcile_layer may read stale data → Same spawn_blocking block, same connection
- **I4**: No cancellation → Added cancel check per layer
- **I5**: No progress reporting → Added progress reset + per-layer updates
- **I6**: No error recovery → Added per-layer error handling with partial results
- **I9**: DADBEAR mutations ≠ staleness.rs → Added bridge approach, revised WS-E estimate
- **I10**: DB lock during LLM → Solved by answer_questions returning without writes

### Stage 2 Findings (incorporated above)
- **S2-A/C**: QuestionNode needs stable ID field → Added to WS-AB (content-hash ID at decomposition time)
- **S2-A/F + S2-B/7**: make_placeholder_uuid uses unstable DefaultHasher → Added Uuid::new_v5 fix to WS-D
- **S2-B/8**: pre_map_layer has no token budget → Added budget guard to WS-AB
- **S2-B/5**: publication.rs conn spans async boundary → Added split-phase fix to WS-D
- **S2-A/I**: Plan conflates crystallization with staleness functions → Clarified WS-E targets staleness.rs + supersession.rs, not crystallization.rs
- **S2-A/D**: Duplicate propagate_staleness in delta.rs → Added rename to WS-AB
- **S2-A/B + S2-B/2**: reconciliation extract_gaps always empty → Accepted, documented. Reconcile still provides orphan/central-node detection.
- **S2-B/13**: EvidenceVerdict::Missing never stored → Added cleanup note to WS-AB
- **S2-A/A + S2-B/1**: answer_questions still writes to DB → Expected, this IS the WS-AB refactor
- **S2-A/G + S2-B/6**: Recursive staleness propagation → Minor, accepted for v1 (practical depth 3-5)
- **S2-B/12**: Reconciliation ordering constraint implicit → Documented in plan step 7c-7d sequencing
