# Workstream: Phase 16 — Vine-of-Vines + Topical Vine Recipe

## Who you are

You are an implementer joining an active 17-phase initiative. Phases 0a, 0b, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15 are shipped. You are the implementer of Phase 16 — extending vine composition so vines can compose other vines (not just bedrocks), shipping the topical vine chain recipe YAML, and propagating updates through the vine hierarchy.

Phase 16 unlocks Phase 17 (recursive folder ingestion): a folder → a vine of (bedrock for files, sub-vine for subfolders) tree.

## Context

Current state:
- `pyramid_vine_compositions` table tracks `(vine_slug, bedrock_slug, position, status)` — bedrock children only, no way to reference a child vine.
- `vine.rs::run_build_pipeline` line ~599 explicitly rejects `ContentType::Vine`: "Vine build uses vine-specific pipeline, not run_build_pipeline".
- `vine_composition.rs::notify_vine_of_bedrock_completion` propagates bedrock apex updates to parent vines via `pyramid_evidence` cross-slug links. It handles one level of propagation (bedrock → vine); vine → parent-vine is not wired.
- Temporal vine recipe for conversation sessions exists implicitly via `conversation.yaml` / `conversation-episodic.yaml` but there's no dedicated "temporal vine" chain YAML.
- No topical vine chain YAML exists.
- No vine-of-vines propagation.

Phase 16 adds:
- `pyramid_vine_compositions.child_type` column (`'bedrock'` or `'vine'`).
- Allow `ContentType::Vine` to route through `run_build_pipeline` via a new branch that dispatches to the topical vine chain.
- New chain YAML: `chains/defaults/topical-vine.yaml`.
- New prompts: `chains/prompts/vine/topical_cluster.md`, `topical_synthesis.md`, `topical_apex.md`.
- Extended `notify_vine_of_bedrock_completion` to walk up through vine parents (bedrock → vine → vine-of-vine → ...).
- New `notify_vine_of_vine_completion` or unified handler for the recursive propagation.
- Cross-vine composition via chain `cross_build_input` primitive that can pull from both bedrocks and sub-vines.

## Required reading (in order)

### Spec docs

1. `docs/handoffs/handoff-2026-04-09-pyramid-folders-model-routing.md` — deviation protocol.
2. **`docs/specs/vine-of-vines-and-folder-ingestion.md` Part 1 (~lines 12-106)** — primary implementation contract for Phase 16. Phase 17 covers Part 2 separately.
3. `docs/specs/change-manifest-supersession.md` — scan the "Vine-Level Manifests" section. Phase 16 extends this pattern up the vine hierarchy.
4. `docs/plans/pyramid-folders-model-routing-full-pipeline-observability.md` — Phase 16 section.
5. `docs/plans/pyramid-folders-model-routing-implementation-log.md` — scan prior phase entries for vine-related changes.

### Code reading

6. **`src-tauri/src/pyramid/vine_composition.rs` in full** (~402 lines). Understand `notify_vine_of_bedrock_completion`, `enqueue_vine_manifest_mutations`, the locking pattern, and the event flow.
7. **`src-tauri/src/pyramid/vine.rs` lines 550-650** — the `run_build_pipeline` dispatch. Phase 16 replaces the vine rejection with a topical vine chain dispatch.
8. **`src-tauri/src/pyramid/db.rs` lines ~1405-1430** — `pyramid_vine_compositions` table definition. Phase 16 adds the `child_type` column.
9. **`src-tauri/src/pyramid/db.rs` lines ~11520-11650** — vine composition helpers (`insert_vine_composition`, `update_bedrock_apex`, `get_vines_for_bedrock`, `list_vine_compositions`). Extend each to handle the `child_type` column.
10. `src-tauri/src/pyramid/chain_executor.rs` — find `cross_build_input` primitive. Verify it can pull from vine children (not just bedrocks). If not, extend it.
11. `src-tauri/src/pyramid/chain_loader.rs` — understand how chain YAMLs are loaded and registered. Phase 16 registers `topical-vine.yaml` alongside existing recipes.
12. `src-tauri/src/pyramid/build_runner.rs` — understand how build_runner picks a chain for a slug based on content type.
13. `chains/defaults/conversation.yaml` — reference pattern for chain YAML structure.
14. `chains/prompts/` — reference patterns for prompt templates.

## What to build

### 1. Database schema + helpers

Add `child_type` column to `pyramid_vine_compositions`:

```rust
// In init_pyramid_db or a migration:
conn.execute_batch(
    "CREATE TABLE IF NOT EXISTS pyramid_vine_compositions (
         id INTEGER PRIMARY KEY AUTOINCREMENT,
         vine_slug TEXT NOT NULL,
         bedrock_slug TEXT NOT NULL,          -- reused as child_slug when child_type='vine'
         position INTEGER NOT NULL,
         bedrock_apex_node_id TEXT,            -- apex for vine children too
         status TEXT NOT NULL DEFAULT 'active',
         child_type TEXT NOT NULL DEFAULT 'bedrock',  -- 'bedrock' | 'vine'
         created_at TEXT NOT NULL DEFAULT (datetime('now')),
         updated_at TEXT NOT NULL DEFAULT (datetime('now')),
         UNIQUE(vine_slug, bedrock_slug)
     );"
)?;
```

**Idempotent ALTER:** for existing databases, the init code needs to detect whether `child_type` exists and add it via `ALTER TABLE`. Use `pragma_table_info` to check.

Rename the `bedrock_slug` column conceptually to "child_slug" in the struct fields and Rust code — the database column name stays for backwards compat (OR add a migration to rename; see deviations). Recommended: keep `bedrock_slug` as the column name, add a `child_type` sibling column, update the Rust struct to expose both `child_slug` and `child_type` fields that read from them.

Extend helpers:
- `insert_vine_composition(conn, vine_slug, child_slug, position, child_type) -> Result<()>`
- `list_vine_compositions(conn, vine_slug) -> Result<Vec<VineComposition>>` returns both bedrock and vine children with their types.
- `update_child_apex(conn, vine_slug, child_slug, apex_node_id) -> Result<()>` — renamed from `update_bedrock_apex` OR add a new helper; keep the old one as an alias for callers that only deal with bedrocks.
- `get_vines_for_child(conn, child_slug) -> Result<Vec<String>>` — returns all vines that include this slug as a child (regardless of type). Extends `get_vines_for_bedrock`.

Add a new helper to walk UP:
- `get_parent_vines_recursive(conn, child_slug) -> Result<Vec<String>>` — returns ALL ancestors (direct parents + grandparents + ...). Uses recursive CTE or iterative BFS.

### 2. Allow ContentType::Vine in run_build_pipeline

Replace the rejection at `vine.rs:599`:

```rust
ContentType::Vine => {
    // Phase 16: vines are built by dispatching the topical vine chain.
    // The chain's cross_build_input primitive pulls apex data from all
    // registered children (bedrocks + sub-vines).
    build::build_topical_vine(
        reader,
        &write_tx,
        llm_config,
        slug,
        cancel,
        &progress_tx,
    ).await
}
```

Add `build::build_topical_vine` to `build.rs`:

```rust
pub async fn build_topical_vine(
    reader: ReaderConn,
    writer: &WriterTxSender,
    llm_config: LlmConfig,
    slug: &str,
    cancel: CancelFlag,
    progress_tx: &mpsc::Sender<Progress>,
) -> Result<BuildOutcome> {
    // 1. Load the chain YAML for topical-vine (via chain_loader)
    // 2. Construct a chain_executor::StepContext with the slug and the
    //    cross_build_input primitive pointing at the vine's children
    //    (from pyramid_vine_compositions)
    // 3. Execute the chain via chain_executor::execute_chain_from
    // 4. Return the outcome
}
```

The topical vine chain YAML is the blueprint; the executor runs the steps.

### 3. Topical vine chain YAML

Ship `chains/defaults/topical-vine.yaml` per the spec (lines 50-105). Verify the primitives referenced (`cross_build_input`, `extract`, `web`, `recursive_pair`) all exist in the chain executor. If `cross_build_input` doesn't exist or doesn't support vine children, extend it.

### 4. Topical vine prompts

Ship three new prompt files under `chains/prompts/vine/`:

- **`topical_cluster.md`**: instructs the LLM to cluster child summaries by topic, entity overlap, or import graph signals. Output schema: `{ clusters: [{ name, child_slugs: [...], reason }] }`.
- **`topical_synthesis.md`**: per-cluster, synthesize a summary node from the cluster's children. Output: a node with headline, distilled, topics.
- **`topical_apex.md`**: recursive pairing prompt that reduces cluster nodes to apex. Output: a node.

Write each prompt in markdown with clear instructions. Reference existing conversation/code/document prompts for tone and structure.

### 5. Cross-build input primitive extension

The `cross_build_input` primitive (find it in `chain_executor.rs`) currently pulls apex data from bedrock slugs. Extend it to recognize when a `vine_slug` is requested and pull from vine slugs too. The lookup semantics are identical: fetch the slug's apex node(s) from `pyramid_nodes`.

The spec says:
> cross_build_input primitive that can pull from both bedrocks and sub-vines

So the primitive needs to load children via `list_vine_compositions(conn, vine_slug)` and fetch apex summaries for each child, regardless of child_type.

### 6. Vine-of-vines propagation

Extend `notify_vine_of_bedrock_completion` OR add a new `notify_vine_of_vine_completion` that handles the recursive walk:

```rust
pub async fn notify_vine_of_child_completion(
    state: &PyramidState,
    child_slug: &str,
    child_build_id: &str,
    apex_node_id: &str,
) -> Result<Vec<String>>
```

Implementation:
1. Look up direct parent vines via `get_vines_for_child(child_slug)`.
2. For each parent vine, run the existing per-vine update path (update_child_apex + enqueue_vine_manifest_mutations + DeltaLanded event).
3. **New: recursively call `notify_vine_of_child_completion(parent_vine_slug, parent_build_id, parent_apex_node_id)`**. The parent vine's apex is its own apex (not the child's). If the parent vine has no apex yet (never built), skip the recursion — propagation waits for the next actual build.
4. **Cycle guard**: track visited slugs to prevent infinite loops on cyclic vine-of-vine references.

Rename the existing `notify_vine_of_bedrock_completion` to `notify_vine_of_child_completion` with an alias for callers using the old name, OR keep both (recommend rename + alias for migration safety).

**Critical invariant**: the propagation walk is FIRE-AND-FORGET at each level. Don't await the full recursive chain synchronously — that would block the calling DADBEAR tick loop. Spawn a tokio task for each level OR return the list of newly-notified vines and let the stale engine pick them up on its next cycle.

Recommend: SYNCHRONOUS update at each level (write the composition table + enqueue pending mutations), ASYNCHRONOUS chain execution (the stale engine picks up the mutations).

### 7. Chain loader registration

Register `topical-vine.yaml` in `chain_loader.rs` as a recognized chain recipe. It loads via the existing mechanism that reads from `chains/defaults/`. Confirm the load path handles new files automatically, or add an explicit entry if there's a manifest.

### 8. Tests

Rust tests:
- `db.rs` phase16_tests:
  - `test_vine_compositions_schema_includes_child_type`
  - `test_insert_vine_composition_with_child_type_vine`
  - `test_list_vine_compositions_returns_both_bedrock_and_vine_children`
  - `test_get_vines_for_child_returns_parents_regardless_of_type`
  - `test_get_parent_vines_recursive_walks_multi_level_hierarchy`
  - `test_get_parent_vines_recursive_cycle_guard`
- `vine_composition.rs` tests:
  - `test_notify_vine_of_bedrock_completion_propagates_to_vine_of_vine`
  - `test_notify_vine_of_child_completion_idempotent_with_visited_set`
  - `test_notify_vine_of_child_completion_skips_vines_with_no_apex`
- `vine.rs` tests (if the build_topical_vine flow is testable):
  - `test_run_build_pipeline_accepts_content_type_vine`
  - `test_build_topical_vine_loads_topical_chain`
- `chain_executor.rs` tests:
  - `test_cross_build_input_pulls_from_vine_child`
  - `test_cross_build_input_pulls_from_bedrock_child`

Mock chains/prompts where needed. The full LLM path doesn't need to run — just verify the structural plumbing.

### 9. Frontend (minimal)

Phase 16 is mostly backend. Frontend touches:
- No new components.
- The existing vine display (if any in PyramidBuildViz or the dashboard) should now handle vine children correctly. If the frontend queries `list_vine_compositions` anywhere, make sure it doesn't break when `child_type` is `'vine'`.

Document any frontend touches in the log.

## Scope boundaries

**In scope:**
- `pyramid_vine_compositions.child_type` column + idempotent migration
- Updated helpers (`insert_vine_composition`, `list_vine_compositions`, `update_child_apex`, `get_vines_for_child`, `get_parent_vines_recursive`)
- `ContentType::Vine` branch in `run_build_pipeline` → `build_topical_vine`
- `chains/defaults/topical-vine.yaml`
- `chains/prompts/vine/topical_cluster.md`, `topical_synthesis.md`, `topical_apex.md`
- `cross_build_input` primitive extension for vine children
- Recursive `notify_vine_of_child_completion` with cycle guard
- Rust tests for all of the above

**Out of scope:**
- Folder ingestion UI (Phase 17)
- Folder walk algorithm (Phase 17)
- Temporal vine chain YAML (exists implicitly via conversation-episodic.yaml; don't refactor)
- Per-vine cost rollup UI (Phase 15 covered cross-pyramid rollup)
- Wire-publishing vine compositions (could be a follow-up)
- Frontend vine tree visualization (nice-to-have; defer)
- The 7 pre-existing unrelated Rust test failures

## Verification criteria

1. **Rust clean:** `cargo check --lib`, `cargo build --lib` from `src-tauri/` — zero new warnings.
2. **Test count:** `cargo test --lib pyramid` — Phase 15 count (1183) + new Phase 16 tests. Same 7 pre-existing failures.
3. **Frontend build:** `npm run build` — clean.
4. **Schema verification:** manual step — dump `pyramid_vine_compositions` schema via SQLite inspect, confirm `child_type` column exists with default `'bedrock'`.
5. **Chain YAML loads:** manual step — launch dev, confirm `chains/defaults/topical-vine.yaml` loads via chain_loader without error.
6. **Vine-of-vine manual verification path** documented in the log:
   - Create two bedrock pyramids A and B
   - Create a vine V1 that includes A as a bedrock child
   - Create a vine V2 that includes V1 as a vine child AND B as a bedrock child
   - Trigger a rebuild on A
   - Verify V1 receives a DeltaLanded event and V2 receives one too (propagation walks the hierarchy)

## Deviation protocol

Standard. Most likely deviations:

- **`cross_build_input` primitive doesn't support vine children**: extend it or write a new `cross_vine_input` primitive. Document which path you chose.
- **`child_type` column already exists** (unlikely but possible from a prior manual test): the idempotent migration handles it silently.
- **`build_topical_vine` needs the chain executor's full StepContext shape** — thread through the existing patterns from `build_conversation` / `build_code` / `build_docs`. If the shape differs (e.g., chain needs a pyramid_slug + child slugs), adapt the call site.
- **Recursive propagation performance**: if a vine-of-vine-of-vine hierarchy has hundreds of nodes, the recursive walk could be slow. Add a max-depth guard (e.g., 10 levels) as a safety net. Document.
- **Prompt content for topical clustering**: the spec mentions "entity overlap" and "import graph signals" as heuristics. The import graph isn't computed at this layer — simplify to "cluster by shared entity names in the distilled text" and document the simplification.
- **Existing `notify_vine_of_bedrock_completion` callers**: if you rename the function, keep the old name as a thin alias so Phase 13/14 callers don't break.

## Implementation log protocol

Append Phase 16 entry to `docs/plans/pyramid-folders-model-routing-implementation-log.md`. Include:
1. Schema migration details
2. Helper function changes
3. Chain YAML + prompts added
4. `build_topical_vine` flow
5. Recursive propagation design + cycle guard
6. Tests added
7. Manual verification steps
8. Deviations with rationale
9. Status: `awaiting-verification`

## Mandate

- **Phase 16 is mostly backend.** Frontend touches are minimal.
- **No hardcoded LLM-constraining numbers.** Chain YAMLs are contributions conceptually; the topical vine recipe ships as a file that's later migrated into a contribution (Phase 5 pattern).
- **Cycle guard is mandatory.** A vine referencing itself (directly or transitively) must NOT cause an infinite loop.
- **Fire-and-forget at each propagation level.** The DADBEAR tick loop must not block on the full recursive walk.
- **Match existing backend conventions.** `build_topical_vine` follows the shape of `build_conversation` / `build_code` / `build_docs`.
- **Commit when done.** Single commit with message `phase-16: vine-of-vines + topical vine recipe`. Body: 5-8 lines summarizing the child_type column, ContentType::Vine dispatch, topical vine YAML + prompts, cross_build_input extension, recursive propagation with cycle guard. Do not amend. Do not push.

## End state

Phase 16 is complete when:

1. `pyramid_vine_compositions.child_type` column exists via idempotent migration.
2. `insert_vine_composition` + `list_vine_compositions` + `get_vines_for_child` + `get_parent_vines_recursive` all handle child_type.
3. `ContentType::Vine` routes through `run_build_pipeline` → `build_topical_vine`.
4. `chains/defaults/topical-vine.yaml` + three vine prompts exist and load correctly.
5. `cross_build_input` primitive can pull from both bedrock and vine children.
6. `notify_vine_of_child_completion` walks up the hierarchy with a cycle guard.
7. `cargo check --lib` + `cargo build --lib` + `npm run build` clean.
8. `cargo test --lib pyramid` at Phase 15 count + new Phase 16 tests.
9. Implementation log Phase 16 entry complete.
10. Single commit on branch `phase-16-vine-of-vines`.

Begin with the spec + existing vine_composition.rs + vine.rs. Then wire.

Good luck. Build carefully.
