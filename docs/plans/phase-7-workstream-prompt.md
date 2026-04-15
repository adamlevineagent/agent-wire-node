# Workstream: Phase 7 — Cache Warming on Import

## Who you are

You are an implementer joining an active 17-phase initiative. Phases 0a, 0b, 1, 2, 3, 4, 5, 6 are shipped. You are the implementer of Phase 7, which builds the import-side cache warming flow: when a user pulls a pyramid from Wire, the source node's cache manifest is downloaded and populated into the local `pyramid_step_cache` so unchanged nodes cache-hit on the first build instead of re-running all the LLM calls.

Phase 7 is substantial but bounded. It's backend + publish/pull plumbing + staleness logic. Frontend is Phase 10's scope.

## Context

Phase 6 shipped the `pyramid_step_cache` content-addressable cache. Phase 6's wanderer fix wired it into the production chain executor. Phase 7 now populates that cache from imported pyramids. The insight is simple: cache keys are content-addressable, so a cache entry produced by another node is valid for this node as long as the referenced source file content matches (same SHA-256).

This matters because Wire's whole point is shared intelligence — a 112-L0 pyramid with full upper-layer synthesis is hundreds of LLM calls and dollars of spend. Forcing every importer to redo that work would make Wire cooperation pointless. With Phase 7, importing becomes "copy the cache, verify L0 sources match, walk the evidence graph to propagate staleness, insert the valid entries" — the first build after import is a near-zero-cost reconstruction for the matching subset.

## Required reading (in order, in full unless noted)

### Handoff + spec docs

1. `docs/handoffs/handoff-2026-04-09-pyramid-folders-model-routing.md` — deviation protocol.
2. **`docs/specs/cache-warming-and-import.md` — read in full (521 lines).** This is the primary implementation contract. Particular attention to: Import Flow (~line 49), Staleness Check Algorithm (~line 80), Cache Manifest Format (~line 151), Publication Side (~line 223), Privacy Consideration (~line 270), Import Resumability (~line 297), DADBEAR Integration (~line 349), IPC Contract (~line 374).
3. `docs/specs/llm-output-cache.md` — re-read the "Cache Warming on Pyramid Import" section (~line 314). Phase 6's spec references Phase 7's work.
4. `docs/plans/pyramid-folders-model-routing-full-pipeline-observability.md` — Phase 7 section.
5. `docs/plans/pyramid-folders-model-routing-implementation-log.md` — scan Phase 4 (contribution creation path), Phase 5 (Wire publish infrastructure), Phase 6 (pyramid_step_cache) to understand the foundations you're building on.
6. `docs/plans/pyramid-folders-model-routing-friction-log.md` — scan for any Phase 5 publish-related friction and the Phase 6 wanderer fix notes so you don't trip over known issues.

### Code reading

7. **`src-tauri/src/pyramid/db.rs`** — targeted. Find `pyramid_step_cache` table definition (Phase 6) — your `check_cache` / `store_cache` helpers are what the import populates. Also find existing `pyramid_file_hashes` (used for file → node_id lookups) and `pyramid_evidence` (the evidence graph). You'll add `pyramid_import_state` table here.
8. **`src-tauri/src/pyramid/wire_publish.rs`** — find `PyramidPublisher` struct and its existing publish methods. Phase 5 added `publish_contribution_with_metadata` + `dry_run_publish`. Phase 7 adds `export_cache_manifest` that exports rows from `pyramid_step_cache` into the manifest JSON format.
9. **`src-tauri/src/pyramid/wire_import.rs`** — check if this file exists. If yes, it's the existing import scaffold and you extend it. If no, you create a new `pyramid_import.rs` (or similar) that implements the import logic.
10. `src-tauri/src/pyramid/config_contributions.rs` — Phase 4's contribution CRUD. Your DADBEAR auto-enable post-import should create a `dadbear_policy` contribution via `create_config_contribution` and call `sync_config_to_operational` so the imported pyramid's DADBEAR setup gets the same contribution-based provenance as any user-created config. Do NOT write directly to `pyramid_dadbear_config` — that's a Phase 4 anti-pattern from the wanderer findings.
11. `src-tauri/src/pyramid/step_context.rs` — Phase 6's module. You don't modify it but you reference `CacheEntry` when inserting imported rows.
12. `src-tauri/src/main.rs` — find the existing IPC command block for patterns. You'll register 3 new commands (`pyramid_import_pyramid`, `pyramid_import_progress`, `pyramid_import_cancel`).
13. **Read enough of `routes.rs` to find existing HTTP patterns** if the spec's IPC preference is HTTP routes; the spec says "`routes.rs` — `handle_import_pyramid()`, ..." so the IPC is HTTP not Tauri invoke. Match the existing HTTP handler pattern (see Phase 5's `pyramid_publish_to_wire` for reference).
14. Any existing evidence graph walker — grep for `dependents_of` or `walk_evidence_graph` or similar. If none exists, you'll write a simple BFS over `pyramid_evidence` rows. Don't over-engineer — O(nodes) is the right complexity and the pyramid counts are small.

## What to build

### 1. `pyramid_import_state` table (in `db.rs`)

Add to `init_pyramid_db`:

```sql
CREATE TABLE IF NOT EXISTS pyramid_import_state (
    target_slug TEXT PRIMARY KEY,
    wire_pyramid_id TEXT NOT NULL,
    source_path TEXT NOT NULL,
    status TEXT NOT NULL,
    nodes_total INTEGER,
    nodes_processed INTEGER DEFAULT 0,
    cache_entries_total INTEGER,
    cache_entries_validated INTEGER DEFAULT 0,
    cache_entries_inserted INTEGER DEFAULT 0,
    last_node_id_processed TEXT,
    error_message TEXT,
    started_at TEXT DEFAULT (datetime('now')),
    updated_at TEXT DEFAULT (datetime('now'))
);
```

CRUD helpers alongside:
- `create_import_state(conn, target_slug, wire_pyramid_id, source_path) -> Result<()>`
- `load_import_state(conn, target_slug) -> Result<Option<ImportState>>`
- `update_import_state(conn, target_slug, status, progress_fields) -> Result<()>`
- `delete_import_state(conn, target_slug) -> Result<()>`

### 2. Cache manifest types (in `types.rs` or the new import module)

Rust types matching the spec's JSON shape:

```rust
pub struct CacheManifest {
    pub manifest_version: u32,
    pub source_pyramid_id: String,
    pub exported_at: String,
    pub nodes: Vec<ImportNodeEntry>,
}

pub struct ImportNodeEntry {
    pub node_id: String,
    pub layer: i64,
    pub source_path: Option<String>,        // L0 only
    pub source_hash: Option<String>,        // L0 only
    pub source_size_bytes: Option<u64>,     // L0 only
    pub derived_from: Vec<String>,          // upper layers only
    pub cache_entries: Vec<ImportedCacheEntry>,
}

pub struct ImportedCacheEntry {
    pub step_name: String,
    pub chunk_index: Option<i64>,
    pub depth: Option<i64>,
    pub cache_key: String,
    pub inputs_hash: String,
    pub prompt_hash: String,
    pub model_id: String,
    pub output_json: String,
    pub token_usage_json: Option<String>,
    pub cost_usd: Option<f64>,
    pub latency_ms: Option<i64>,
    pub created_at: Option<String>,
}
```

Use `serde` for JSON (de)serialization. The manifest can be large (5-40 MB); serde_json handles it fine.

### 3. Staleness check algorithm (in `pyramid_import.rs`)

Implement `populate_from_import(conn, manifest, target_slug, local_source_root)` exactly per the spec's three-pass algorithm:

1. **Pass 1 (L0 staleness):** for each L0 node, resolve `local_source_root.join(source_path)`, SHA-256 the file, compare to `node.source_hash`. Mark node stale if file missing or hash mismatch. Otherwise, insert the node's cache entries into `pyramid_step_cache` via Phase 6's `store_cache` helper (or direct `INSERT OR IGNORE` since the spec says idempotency matters for resumability).
2. **Pass 2 (upward propagation):** BFS over the evidence graph from the stale L0 set. The manifest carries `derived_from` on upper nodes, so build an in-memory dependency graph: `dependents: HashMap<String, Vec<String>>` where `dependents[parent] = [child1, child2, ...]`. Walk from the stale L0 frontier outward, marking every downstream node stale.
3. **Pass 3 (upper layer cache):** for every upper-layer node NOT in the stale set, insert its cache entries into `pyramid_step_cache`. Nodes in the stale set are skipped — their cache entries are dropped.

Return an `ImportReport`:

```rust
pub struct ImportReport {
    pub cache_entries_valid: u64,
    pub cache_entries_stale: u64,
    pub nodes_needing_rebuild: u64,
    pub nodes_with_valid_cache: u64,
}
```

**Idempotency:** use `INSERT OR IGNORE` (not `INSERT OR REPLACE`) when populating the cache. Resumption re-runs the loop from the last cursor; already-inserted entries should be no-ops, not overwrites. Overwriting could clobber a local cache entry that had force-fresh set between import attempts.

### 4. `pyramid_import.rs` main entry point

```rust
pub async fn import_pyramid(
    state: &PyramidState,
    wire_pyramid_id: &str,
    target_slug: &str,
    source_path: &str,
) -> Result<ImportReport>
```

High-level flow:
1. Validate: source_path is a directory, target_slug not already importing a different pyramid
2. Check for existing import state (resume vs fresh)
3. Download the cache manifest from the source Wire node (use existing `RemotePyramidClient` pattern from Phase 3/5's wire_publish/import infrastructure — grep for it)
4. Save progress (`status: "downloading_manifest"` → `"validating_sources"`)
5. Call `populate_from_import` with the manifest + local source root
6. After cache population: enable DADBEAR on the imported pyramid **via Phase 4's contribution path**, NOT by directly writing to `pyramid_dadbear_config`. Build a minimal `dadbear_policy` YAML (scan_interval_secs, source_path, etc.), call `create_config_contribution` with `schema_type = "dadbear_policy"`, `source = "import"`, then call `sync_config_to_operational` which handles the actual `pyramid_dadbear_config` write.
7. Mark import state as `"complete"`
8. Return the report

### 5. Publication side: `export_cache_manifest` in `wire_publish.rs`

Extend `PyramidPublisher` with:

```rust
pub async fn export_cache_manifest(
    &self,
    slug: &str,
    build_id: &str,
) -> Result<Option<CacheManifest>>
```

- Query `pyramid_step_cache` joined with `pyramid_pipeline_steps` for the given slug + build_id, per the SQL in the spec's "Publication Side" section (~line 240-260).
- Group rows by `node_id` and serialize to the `CacheManifest` struct.
- **Privacy gate:** check whether the pyramid is public-source (i.e., all L0 nodes reference corpus documents with `visibility = "public"`). The spec says all-or-nothing: if any node is private or circle-scoped, return `Ok(None)` and do NOT include the cache in the publish payload. For Phase 7, implement a SIMPLE version of this gate: default OFF for any pyramid (returns `Ok(None)`) unless the caller explicitly opts in via a parameter. This is the safer default — Phase 10's publish UI will add the opt-in checkbox with warnings. Document this narrower scope in a code comment referencing the spec's full privacy design.

Then extend the existing `publish_pyramid` / `publish_contribution_with_metadata` path to include the cache manifest in the upload payload when present.

### 6. Import IPC handlers

Add to `routes.rs` (HTTP handlers) OR `main.rs` (Tauri invoke) — match whichever the existing Phase 5/6 import-adjacent code uses. Grep for `pyramid_publish_to_wire` handler location to see which surface it's on.

Three endpoints per the spec:
- `pyramid_import_pyramid(wire_pyramid_id, target_slug, source_path)` — returns `ImportReport`
- `pyramid_import_progress(target_slug)` — returns status + progress percentage + counters
- `pyramid_import_cancel(target_slug)` — deletes import state, rolls back partial inserts

Progress is polled by the frontend; Phase 7 just provides the IPC surface. The spec's progress calculation is:

```
progress = (nodes_processed / nodes_total) * 0.5 + (cache_entries_validated / cache_entries_total) * 0.5
```

### 7. DADBEAR auto-enable via Phase 4 contributions

Post-import, create a `dadbear_policy` contribution:

```rust
let dadbear_yaml = format!(
    "slug: {target_slug}\nsource_path: {source_path}\nscan_interval_secs: 60\nenabled: true\n",
);

let contribution_id = create_config_contribution_with_metadata(
    &writer_conn,
    "dadbear_policy",
    Some(target_slug),
    &dadbear_yaml,
    Some("Imported pyramid — DADBEAR auto-enabled"),
    "import",  // source
    Some("pyramid_import"),  // created_by
    wire_native_metadata,
)?;

sync_config_to_operational(&writer_conn, &state.build_event_bus, &contribution)?;
```

This is the pattern that Phase 4's wanderer flagged as the correct route. The `contribution_id` is recorded on the new `pyramid_dadbear_config` row via the sync path.

### 8. Scope boundaries

**In scope:**
- `pyramid_import_state` table + CRUD
- Cache manifest types (shared between export and import)
- Staleness check algorithm with the three-pass flow
- `import_pyramid` main entry point
- `export_cache_manifest` extension to `PyramidPublisher` (returns None by default for privacy safety; explicit opt-in gate as a parameter)
- 3 IPC endpoints: import, progress, cancel
- DADBEAR auto-enable via Phase 4 contribution path
- Tests for each piece

**Out of scope:**
- ToolsMode frontend (Phase 10) — no React changes
- ImportPyramidWizard.tsx (Phase 10)
- Sidebar "Imported" badge (Phase 10)
- Privacy gate UI (Phase 10) — Phase 7 ships the safer default-off version
- Wire discovery ranking for cached pyramids (Phase 14)
- Real-time build viz cache hit display (Phase 13)
- Remote node download protocol — use whatever `RemotePyramidClient` from `wire_import.rs` or `wire_publish.rs` already exposes; do NOT write a new HTTP client from scratch
- The existing 7 pre-existing unrelated test failures

## Verification criteria

1. `cargo check --lib`, `cargo build --lib` — clean, zero new warnings.
2. `cargo test --lib pyramid::pyramid_import` (or wherever you put the tests) — all new tests passing.
3. `cargo test --lib pyramid` — 961 passing (Phase 6 count) + your Phase 7 tests. Same 7 pre-existing failures. No new ones.
4. **Integration test:** write a test that (a) builds a cache manifest in-memory with 3 L0 nodes and 2 upper-layer nodes, (b) creates a temp dir with 2 matching files + 1 mismatching file, (c) calls `populate_from_import`, (d) asserts the correct nodes are cache-hit and the one with the mismatched source propagates staleness to the upper layers that reference it.
5. **Idempotency test:** run `populate_from_import` twice with the same manifest, verify no duplicate rows in `pyramid_step_cache` (INSERT OR IGNORE behavior).
6. **DADBEAR-via-contribution test:** after import, verify `pyramid_config_contributions` has a new `dadbear_policy` row with `source = "import"` and `pyramid_dadbear_config` has the synced row with the contribution_id FK.

## Deviation protocol

Standard. Most likely deviations:
- **Missing `RemotePyramidClient` or similar** — if Phase 5's wire_publish path doesn't expose a reusable HTTP download primitive, you may need to add a minimal one. Flag it.
- **Evidence graph walker not reusable** — the manifest carries its own `derived_from` lists, so you can build the dependency graph in-memory from the manifest alone without touching the local `pyramid_evidence` table. Use this approach to avoid coupling to the local state during import.
- **Privacy gate is more complex than expected** — Phase 7's default-off safety net lets you ship without hitting the detection logic. Full detection lands in Phase 10.

## Implementation log protocol

Append Phase 7 entry to `docs/plans/pyramid-folders-model-routing-implementation-log.md`. Document the table, types, staleness algorithm, import entry point, export extension, IPC endpoints, DADBEAR-via-contribution path, tests, and verification results. Status: `awaiting-verification`.

## Mandate

- **Correct before fast.** The staleness check is the safety net — get the three passes right and the idempotency right. Cache poisoning is a silent correctness bug.
- **Use Phase 4's contribution path for DADBEAR enable.** Do NOT write directly to `pyramid_dadbear_config` — that's a regression to the pre-Phase-4 pattern the wanderers caught. `create_config_contribution` + `sync_config_to_operational` is the one canonical path.
- **Privacy-safe by default.** `export_cache_manifest` returns None unless explicitly opted in. Phase 10 adds the checkbox with warnings.
- **No new scope.** Frontend is Phase 10. Build viz integration is Phase 13. Full privacy UI is Phase 10.
- **Fix all bugs found.** Standard repo convention.
- **Commit when done.** Single commit with message `phase-7: cache warming on import`. Body: 5-7 lines summarizing table + manifest types + staleness algorithm + import entry + export extension + IPC + DADBEAR-via-contribution + tests. Do not amend. Do not push.

## End state

Phase 7 is complete when:

1. `pyramid_import_state` table exists with CRUD helpers.
2. `CacheManifest` / `ImportNodeEntry` / `ImportedCacheEntry` types exist and serialize to the spec's JSON shape.
3. `populate_from_import` implements the three-pass staleness check and inserts valid entries into `pyramid_step_cache`.
4. `import_pyramid` entry point orchestrates manifest download → staleness check → cache population → DADBEAR enable via Phase 4 contribution path.
5. `PyramidPublisher::export_cache_manifest` exists with the default-OFF privacy gate.
6. 3 IPC endpoints registered.
7. DADBEAR post-import setup goes through `create_config_contribution` + `sync_config_to_operational`, NOT direct writes.
8. Tests pass: unit tests for staleness + idempotency + contribution-path integration.
9. `cargo check`, `cargo build`, `cargo test --lib pyramid` all pass with 961+ passing and the same 7 pre-existing failures.
10. Implementation log Phase 7 entry complete.
11. Single commit on branch `phase-7-cache-warming-import`.

Begin with the spec (read in full). Then the relevant code patterns. Then write.

Good luck. Build carefully.
