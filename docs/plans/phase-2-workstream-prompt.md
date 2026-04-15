# Workstream: Phase 2 — Change-Manifest Supersession

## Who you are

You are an implementer joining an active 17-phase initiative to turn Wire Node into a Wire-native application. Phase 0a (clippy), Phase 0b (Pipeline B chain dispatch), and Phase 1 (DADBEAR in-flight lock, hoisted to PyramidState in a fix pass) are all shipped on their feature branches. You are the implementer of Phase 2.

Phase 2 is the fix for the viz orphaning bug — stale-check-driven supersession currently creates new upper-layer node IDs, which breaks `get_tree()`'s evidence-link-based `children_by_parent` lookup and renders the pyramid DAG as an orphaned apex. The fix is to rewrite the stale-update path to produce **change manifests** that update nodes in place with stable IDs and bumped `build_version`.

## Context: the viz orphaning bug in one paragraph

`stale_helpers_upper.rs::execute_supersession` (line 1387) currently:

1. Resolves the live canonical node ID (e.g., `L3-000`)
2. Generates a NEW node ID (e.g., `L3-S000`) via some pattern
3. INSERTs a new node with the new ID and sets `superseded_by` on the old node
4. The evidence links in `pyramid_evidence` still point at the OLD ID (`L3-000`)
5. `live_pyramid_nodes` view filters out `superseded_by IS NOT NULL`, so the old node is hidden
6. `get_tree()` in `query.rs` (lines 395-633) builds the parent-child graph from evidence links: `children_by_parent.get("L3-S000")` returns empty because no evidence points at `L3-S000`
7. The DAG renders a lone apex with no visible children — repeatedly, reliably, and visibly broken for Adam

The fix: instead of creating a new ID and regenerating, ask the LLM "given these children changed, what needs to change in this node's synthesis?" The LLM produces a targeted **change manifest** (topics to add/update/remove, optional new distilled text, optional headline change, children_swapped list). Apply the manifest in-place on the existing node: same ID, bumped `build_version`, snapshot the prior version to `pyramid_node_versions`, update evidence links to reflect children_swapped. Identity-change case (rare) is the escape hatch for actual wholesale node reorganization.

## Required reading (in order, in full unless noted)

### Handoff + spec docs

1. `docs/handoffs/handoff-2026-04-09-pyramid-folders-model-routing.md` — original handoff, deviation protocol, implementation log protocol.
2. `docs/handoffs/handoff-2026-04-09-pyramid-folders-model-routing-addendum-01.md` — **read the "Phase 2's scope boundary is now explicit" section in full.** This is critical: `supersede_nodes_above()` has three callers, and Phase 2 only modifies ONE of them (`stale_helpers_upper.rs::execute_supersession`). The two wholesale-rebuild callers (`vine.rs:3381` and `chain_executor.rs:4821`) are CORRECT as-is and must NOT be touched.
3. **`docs/specs/change-manifest-supersession.md` — this is your implementation contract. Read it in full, end-to-end.** Every field semantic, every validation rule, the LLM prompt, the in-place update flow, vine-level manifests, manifest supersession chains, reroll-with-notes scope — all in there.
4. `docs/plans/pyramid-folders-model-routing-full-pipeline-observability.md` — the master plan. Phase 2 section is short; read it to see how it connects to Phase 1 (done) and Phase 13 (which will use `pyramid_reroll_node` for the reroll-with-notes UI — Phase 2 only provides the manifest storage + supersession chain infrastructure, not the IPC command).
5. `docs/plans/pyramid-folders-model-routing-implementation-log.md` — scan the Phase 0b and Phase 1 entries so you understand the patterns previous phases established for cargo verification, tests, logs.

### Code reading (read in full for files you'll change, targeted for others)

**Files you'll change (read in full):**

6. `src-tauri/src/pyramid/stale_helpers_upper.rs` — the whole file (~3000+ lines). Your main target is `execute_supersession` at line 1387. Understand what it currently does: resolves live canonical node ID → gathers node data → generates new ID → INSERTs new node → sets `superseded_by`. Your rewrite keeps the ID stable, calls a new `generate_change_manifest` function to get the LLM to produce a targeted delta, validates the manifest, and calls the new `update_node_in_place` helper (which you'll add to `db.rs`). Also pay attention to how this function is called — from `stale_engine.rs` via some dispatch path.

7. `src-tauri/src/pyramid/db.rs` — **do NOT read all of it** (it's huge). Instead: grep for `pyramid_change_manifests` (should not exist yet), grep for `pyramid_node_versions` (already exists — see collapse.rs + recovery.rs for its existing schema and callers), grep for `init_pyramid_db` (the function that creates schema on first run), grep for `fn supersede_nodes_above` (around line 2839 — the function whose callers are the three sites from the addendum), grep for `fn save_node` (the current INSERT path that execute_supersession uses). Read each of those targeted regions in full. Your work adds: (a) `pyramid_change_manifests` table in `init_pyramid_db`, (b) `build_version INTEGER DEFAULT 1` column on `pyramid_nodes`, (c) new `update_node_in_place()` function, (d) manifest CRUD functions.

8. `src-tauri/src/pyramid/vine_composition.rs` — read in full. Look for `notify_vine_of_bedrock_completion` (or similar). The spec's "Vine-Level Manifests" section says this is the integration point where a bedrock apex update triggers a change manifest on the vine node(s) that compose it.

9. `src-tauri/src/pyramid/stale_engine.rs` — **targeted read**. Grep for `execute_supersession` to find the dispatch path that calls it. You need to understand what result your rewrite returns (the spec says the function returns the updated node ID, which stays the same except in the rare identity-change case).

**Files you'll reference but NOT change:**

10. `src-tauri/src/pyramid/query.rs` around `get_tree()` (line 395-633) — understand how the tree walk uses evidence links. Your fix should make the "children point at old ID" problem disappear because the ID doesn't change on stale updates. Do NOT simplify `get_tree()` in this phase — the spec explicitly says "may not need `live_pyramid_nodes` view filter for superseded upper nodes" is a follow-up concern, not Phase 2.

11. `src-tauri/src/pyramid/collapse.rs` and `src-tauri/src/pyramid/recovery.rs` — read the sections that touch `pyramid_node_versions`. You need to understand its existing schema (`slug, node_id, version, headline, distilled, supersession_reason, created_at`) so your `update_node_in_place` uses the correct columns.

12. **`chains/prompts/shared/` directory** — check if it exists. The spec says to create `chains/prompts/shared/change_manifest.md` with the LLM prompt. If the `shared/` subdirectory doesn't exist, create it.

### Canonical Wire reference (for context only, not for Phase 2 directly)

13. You do NOT need to read `GoodNewsEveryone/docs/wire-native-documents.md` etc. for Phase 2. Those are Phase 5's concern (wire-contribution-mapping).

## What to build

### 1. Schema changes (in `db.rs`)

**New table `pyramid_change_manifests`:**

```sql
CREATE TABLE IF NOT EXISTS pyramid_change_manifests (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    slug TEXT NOT NULL,
    node_id TEXT NOT NULL,
    build_version INTEGER NOT NULL,
    manifest_json TEXT NOT NULL,
    note TEXT,  -- user-provided note for reroll-with-notes; NULL for automated stale-check manifests
    supersedes_manifest_id INTEGER REFERENCES pyramid_change_manifests(id),  -- prior manifest this one corrects
    applied_at TEXT DEFAULT (datetime('now')),
    UNIQUE(slug, node_id, build_version)
);
CREATE INDEX IF NOT EXISTS idx_change_manifests_node ON pyramid_change_manifests(slug, node_id);
```

Add the creation SQL to `init_pyramid_db`. Also add a `CREATE INDEX IF NOT EXISTS` on `(slug, node_id)` for manifest-chain lookups.

**New column on `pyramid_nodes`:**

```sql
ALTER TABLE pyramid_nodes ADD COLUMN build_version INTEGER NOT NULL DEFAULT 1;
```

Handle the migration correctly: `CREATE TABLE ... IF NOT EXISTS` for the new table is idempotent, but `ALTER TABLE ADD COLUMN` is NOT — you need a schema version check or a try-and-ignore-on-duplicate pattern. Look at how other recent column additions in `init_pyramid_db` handle this (grep for `ALTER TABLE pyramid_nodes ADD COLUMN` — there should be prior examples). Follow the existing pattern exactly.

### 2. Manifest CRUD helpers (in `db.rs`)

Add these functions to `db.rs`, following the existing naming and style conventions (e.g., the functions around `save_node`, `save_ingest_record`):

- `save_change_manifest(conn: &Connection, slug: &str, node_id: &str, build_version: i64, manifest_json: &str, note: Option<&str>, supersedes_manifest_id: Option<i64>) -> Result<i64>` — inserts a row and returns the new id
- `get_change_manifests_for_node(conn: &Connection, slug: &str, node_id: &str) -> Result<Vec<ChangeManifestRecord>>` — returns all manifests for a node ordered by `applied_at` ascending
- `get_latest_manifest_for_node(conn: &Connection, slug: &str, node_id: &str) -> Result<Option<ChangeManifestRecord>>` — returns the most recent manifest row

Define a `ChangeManifestRecord` struct in `types.rs` with the appropriate fields. Match existing record struct conventions.

### 3. `update_node_in_place` helper (in `db.rs`)

New function:

```rust
pub fn update_node_in_place(
    conn: &Connection,
    slug: &str,
    node_id: &str,
    updates: &ContentUpdates,
    children_swapped: &[(String, String)],
    build_id: &str,
) -> Result<i64>  // returns new build_version
```

It must:

1. Load the current node from `pyramid_nodes` (BEGIN IMMEDIATE transaction for atomicity).
2. Insert a row into `pyramid_node_versions` capturing the pre-update state (follow the existing schema — check `collapse.rs` and `recovery.rs` for the column list).
3. Apply `updates.distilled`, `updates.headline`, `updates.topics`, `updates.terms`, `updates.decisions`, `updates.dead_ends` to the existing row (only non-null fields). Topics/terms/decisions arrays apply in add/update/remove fashion per the spec's field semantics.
4. Bump `build_version` by 1.
5. Update the node's `children` JSON array: replace each `old` in `children_swapped` with its `new`.
6. Update `pyramid_evidence` rows: `UPDATE pyramid_evidence SET source_node_id = ?new WHERE source_node_id = ?old AND target_node_id = ?node_id AND slug = ?slug` for each children_swapped entry.
7. COMMIT. Return the new `build_version`.

Define `ContentUpdates` in `types.rs` with optional fields matching the spec's manifest JSON shape.

### 4. Manifest validation (in `stale_helpers_upper.rs` or a new `change_manifest.rs` module)

Per the spec's "Manifest Validation" section, every change manifest is validated before it is applied. Implement `validate_change_manifest` with all six checks (target exists, children_swapped references, identity_changed semantics, content_updates field-level, reason non-empty, build_version contiguous). Invalid manifests surface via `ManifestValidationError` enum — do not silently discard. Log WARN-level with the full manifest + error details.

### 5. LLM prompt file (`chains/prompts/shared/change_manifest.md`)

Create this file with the prompt from the spec's "LLM Prompt: Change Manifest Generation" section. It takes the current node's state and changed children deltas and produces a JSON manifest. Follow the existing prompt file style (look at any prompt in `chains/prompts/` for the format).

### 6. `generate_change_manifest` function (in `stale_helpers_upper.rs`)

New async function that:
- Takes the current node state (headline, distilled, topics, terms, decisions) and a list of changed children (old summary vs new summary)
- Loads the `change_manifest.md` prompt
- Calls the LLM via the existing chain_executor-adjacent paths (or via `llm::call_model_unified` if that's the right entry point — check the existing LLM call sites in `stale_helpers_upper.rs` for the pattern)
- Parses the JSON response into a `ChangeManifest` struct
- Returns `Result<ChangeManifest>`

Define `ChangeManifest` in `types.rs` matching the spec's JSON schema.

### 7. Rewrite `execute_supersession`

Replace the current body (new-ID-based supersession) with:

1. Resolve the live canonical node ID (same as today)
2. Load the current node state
3. Load the changed children's old vs new summaries (from the stale-check context — you'll need to thread this through, it's what the existing code's stale-check comparator produces)
4. Call `generate_change_manifest(...)` to get the LLM's targeted delta
5. Validate the manifest via `validate_change_manifest`
6. If validation fails, log WARN and return an error — the stale check is NOT retried automatically per the spec. Save the failed manifest to `pyramid_change_manifests` so the DADBEAR oversight page (Phase 15) can surface it.
7. If `identity_changed == true` (rare case), fall back to the existing new-ID flow
8. Otherwise, call `update_node_in_place(...)` and then `save_change_manifest(...)` (note = None for automated stale-check manifests, supersedes_manifest_id = None)
9. Return the node ID (same as input in the normal case)

### 8. Vine-level manifest integration (in `vine_composition.rs`)

Per the spec's "Vine-Level Manifests" section, modify `notify_vine_of_bedrock_completion` (or the equivalent function — grep for it) to: after a bedrock completes, look up which vines include it and for each affected vine node, call `generate_change_manifest` with the bedrock apex as the changed child. Use the same `update_node_in_place` pathway.

The `children_swapped` entries in vine-level manifests use a `bedrock-slug:node-id` prefix format (e.g., `{old: "bedrock-x:L3-000", new: "bedrock-x:L3-S001"}`) so the manifest tracks which bedrock's apex changed.

### 9. Tests

Add tests to `stale_helpers_upper.rs` (or a new `change_manifest_tests.rs` if that's cleaner):

- `test_update_node_in_place_normal_case` — insert a node, call `update_node_in_place` with a topic update + children_swapped, assert the node ID is unchanged, `build_version` is 2, `pyramid_node_versions` has a snapshot, evidence links are updated.
- `test_update_node_in_place_identity_changed` — the rare escape-hatch path; verify new ID is created when `identity_changed=true`.
- `test_validate_change_manifest_all_errors` — exercise each `ManifestValidationError` variant (missing target, missing children, invalid topic op, empty reason, non-contiguous version, etc).
- `test_manifest_supersession_chain` — two manifests on the same node with `supersedes_manifest_id` pointing at the first; assert `get_latest_manifest_for_node` returns the second.
- `test_execute_supersession_stable_id` — end-to-end-ish: insert a node with evidence links, call `execute_supersession` with a mock stale change, assert the node ID stays the same and evidence links are still valid. This may require mocking the LLM call — use a local fixture or test-mode shortcut.

All existing tests must continue to pass. Post-Phase-1 there are 15 tests in `dadbear_extend`. There are also tests in `stale_helpers_upper.rs` (grep for `#[test]` or `#[tokio::test]` in that file) and tests scattered across other pyramid modules. Run `cargo test --lib pyramid` and confirm no regressions in the full pyramid test suite.

## Scope boundaries

**In scope:**
- Schema: `pyramid_change_manifests` table, `build_version` column
- New functions: `update_node_in_place`, manifest CRUD, `generate_change_manifest`, `validate_change_manifest`
- Rewrite: `execute_supersession` to use change manifests
- Integration: `notify_vine_of_bedrock_completion` in `vine_composition.rs`
- LLM prompt file: `chains/prompts/shared/change_manifest.md`
- Types: `ChangeManifest`, `ChangeManifestRecord`, `ContentUpdates`, `ManifestValidationError`
- Tests for every new function and the rewrite

**Out of scope:**
- **`vine.rs:3381` — DO NOT TOUCH.** This is `handle_vine_rebuild_upper` (or similar) calling `supersede_nodes_above(&conn, vine_slug, 1, &rebuild_build_id)` — it's an explicit wholesale L2+ rebuild, not a stale-update. Correct as-is.
- **`chain_executor.rs:4821` — DO NOT TOUCH.** This is the fresh-path build inside `build_lifecycle` clearing leftover L1+ overlay nodes from a prior attempt. Correct as-is.
- Simplifications to `get_tree()` or `live_pyramid_nodes` view filter (follow-up concern)
- `pyramid_reroll_node` IPC command (Phase 13's scope — `build-viz-expansion.md`)
- DADBEAR oversight page (Phase 15's scope)
- Manifest batching optimization (open question in the spec, not required for Phase 2)
- Any changes to Pipeline B (`dadbear_extend.rs`, `fire_ingest_chain`) — Phase 2 is purely about the stale-update path, not Pipeline B's creation path

## Verification criteria

1. **`cargo check`, `cargo build`** from `src-tauri/` — clean, zero new warnings in files you touched.
2. **`cargo test --lib pyramid`** — all existing pyramid tests plus your new Phase 2 tests pass. Note: there are currently 7 pre-existing unrelated test failures in `pyramid::db::tests::test_evidence_pk_cross_slug_coexistence`, `pyramid::defaults_adapter::tests::real_yaml_thread_clustering_preserves_response_schema`, and 5 `pyramid::staleness::tests::*`. Confirm these are pre-existing by stashing your changes briefly if needed; your work should NOT introduce new failures or fix these unrelated ones.
3. **Manual viz verification** (document as pending human-run): describe the steps for Adam to verify the viz orphaning fix — build a pyramid, trigger a DADBEAR stale check that supersedes an upper node, assert the DAG still shows children under the updated apex.

## Deviation protocol

Same as every phase. The addendum's scope boundary is load-bearing — if you find yourself wanting to modify `vine.rs:3381` or `chain_executor.rs:4821`, STOP and flag it. Those are NOT Phase 2's scope and changing them breaks two working systems. Any other deviation follows the standard friction log + `> [For the planner]` block pattern.

## Implementation log protocol

Append / update the Phase 2 entry in `docs/plans/pyramid-folders-model-routing-implementation-log.md`. Use the format defined at the top of that file. Minimum fields: Started / Completed timestamps, files touched with brief descriptions, spec adherence per section (schema, helpers, validation, prompt, rewrite, vine integration, tests), verification results, Status: `awaiting-verification`. Do NOT mark yourself verified.

## Mandate

- **Correct before fast.** Phase 2 is substantial. Don't skip tests, don't hand-wave validation.
- **Right before complete.** The in-place update flow has subtle ordering requirements (snapshot → apply → bump → update evidence) — get them right.
- **Scope discipline.** The "only execute_supersession" rule is the spec's explicit boundary. Do not widen.
- **Fix all bugs found.** If you encounter a bug in adjacent code (not in the don't-touch list), fix it per the repo convention and note in the friction log.
- **Pillar 37 watch.** If you introduce any LLM-output-constraining number (temperature, max_tokens, max_retries hardcoded in the manifest generation path) — it's wrong. Configuration flows through existing tier-routing paths and the chain YAML. No hardcoded numbers.
- **Commit when done.** Single commit with message `phase-2: change-manifest supersession`. Body: summary of (a) schema changes, (b) new helpers, (c) rewritten execute_supersession, (d) vine integration, (e) tests added. Do not amend. Do not push.

## End state

Phase 2 is complete when:

1. `pyramid_change_manifests` table and `build_version` column exist in `init_pyramid_db`.
2. `update_node_in_place`, manifest CRUD, `generate_change_manifest`, `validate_change_manifest` all exist and are tested.
3. `execute_supersession` in `stale_helpers_upper.rs` uses change manifests for the normal path, with identity-change as the escape hatch.
4. `notify_vine_of_bedrock_completion` in `vine_composition.rs` calls `generate_change_manifest` for affected vine nodes.
5. `chains/prompts/shared/change_manifest.md` exists with the prompt from the spec.
6. `vine.rs:3381` and `chain_executor.rs:4821` are UNCHANGED — verified by grep or `git diff --stat`.
7. `cargo check`, `cargo build`, `cargo test --lib pyramid` all pass, no new failures.
8. Implementation log Phase 2 entry is complete with spec adherence and verification results.
9. Single commit on branch `phase-2-change-manifest-supersession`.

Begin with the reading. The spec is your implementation contract — read it end-to-end first. Then the code. Then write.

Good luck. Build carefully.
