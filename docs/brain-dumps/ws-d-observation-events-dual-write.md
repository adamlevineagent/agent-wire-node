# WS-D Brain Dump: Observation Events Dual-Write

## What Was Done

### New Module: observation_events.rs
- Created `src-tauri/src/pyramid/observation_events.rs` with `write_observation_event()` helper
- Registered as `pub mod observation_events` in `pyramid/mod.rs`
- Function takes: conn, slug, source, event_type, source_path, file_path, content_hash, previous_hash, target_node_id, layer, metadata_json
- Returns `Result<i64>` (the new event ID)
- Uses `Utc::now()` for `detected_at` timestamp, matching the WAL pattern

### All 15 WAL Write Sites Now Dual-Write

Each site adds a call to `write_observation_event()` immediately after the existing `INSERT INTO pyramid_pending_mutations`. The existing INSERT is unchanged. Dual-write failures are silently ignored (`let _ =`) to avoid breaking the existing mutation pipeline.

| # | File | Line (approx) | Old mutation_type | New event_type | Source |
|---|------|---------------|-------------------|----------------|--------|
| 1 | watcher.rs | 643 | file_change/new_file/deleted_file/rename_candidate | file_modified/file_created/file_deleted/file_renamed | watcher |
| 2 | stale_helpers.rs | 113 | confirmed_stale | cascade_stale | cascade |
| 3 | stale_helpers_upper.rs | 1425 | confirmed_stale (cross-thread) | cascade_stale | cascade |
| 4 | stale_helpers_upper.rs | 2827 | confirmed_stale (in-place) | cascade_stale | cascade |
| 5 | stale_helpers_upper.rs | 2853 | edge_stale (in-place) | edge_stale | cascade |
| 6 | stale_helpers_upper.rs | 3312 | confirmed_stale (supersession) | cascade_stale | cascade |
| 7 | stale_helpers_upper.rs | 3337 | edge_stale (supersession) | edge_stale | cascade |
| 8 | chain_executor.rs | 6158 | evidence_set_growth | evidence_growth | evidence |
| 9 | build_runner.rs | 421 | confirmed_stale (vine bedrock) | vine_stale | vine |
| 10 | vine_composition.rs | 377 | confirmed_stale (vine node) | vine_stale | vine |
| 11 | stale_engine.rs | 1068 | targeted_l0_stale | targeted_stale | cascade |
| 12 | stale_engine.rs | 1679 | confirmed_stale (propagation) | cascade_stale | cascade |
| 13 | routes.rs | 4933 | deleted_file (unfreeze rescan) | file_deleted | rescan |
| 14 | routes.rs | 4945 | file_change (unfreeze rescan) | file_modified | rescan |
| 15 | routes.rs | 5007 | file_change/deleted_file (full sweep) | file_modified/file_deleted | rescan |

### staleness_bridge.rs Rewritten
- `auto_detect_changed_files()` now reads from `dadbear_observation_events` first using a cursor
- Interim cursor stored as `last_bridge_observation_id` column on `pyramid_build_metadata` (ALTER TABLE IF NOT EXISTS)
- Helper functions: `ensure_bridge_cursor_column()`, `get_bridge_cursor()`, `advance_bridge_cursor()`
- Falls back to old `pyramid_pending_mutations` CTE if no observation events found
- Old CTE logic preserved in `auto_detect_changed_files_from_wal()` private function

### Backfill Migration
- Added to `init_pyramid_db()` after the `pyramid_build_metadata` population
- One-time: only runs if `dadbear_observation_events` is empty
- Copies all rows from `pyramid_pending_mutations` with source='migration'

## Decisions Made

1. **Silent failure on dual-write**: Used `let _ =` on all observation event writes so a failure in the new table never breaks the existing WAL pipeline. The old system must keep working identically.

2. **Watcher mutation_type mapping**: The watcher's `write_mutation()` function receives the mutation_type as a string parameter. Added a match block to translate old vocabulary to new vocabulary inline.

3. **Bridge cursor uses UPSERT**: `advance_bridge_cursor` uses `INSERT ... ON CONFLICT ... DO UPDATE` so it works even if the slug doesn't yet exist in `pyramid_build_metadata`.

4. **Content hash not available at WAL write time**: The watcher's `write_mutation()` doesn't have the content hash in scope (it's computed earlier in the call chain). Passed `None` for content_hash/previous_hash at that site. The routes.rs unfreeze rescan sites DO have both hashes available and pass them through.

5. **`detail` field mapped to `metadata_json`**: For most sites, the existing `detail` string is passed as `metadata_json` since it contains the contextual information about why the mutation happened.

## Type Issues Resolved
- `stale_helpers_upper.rs` in-place update function: `conn` is an owned `Connection` (from `open_pyramid_connection`), not `&Connection`. Had to pass `&conn` to the helper.
- `stale_helpers_upper.rs` child-to-parent supersession function: `conn` comes from `spawn_blocking` owned scope, same pattern, used `&conn`.
- `stale_engine.rs`: `conn` is `&Connection` from function param, passed directly.
- `chain_executor.rs`: `c` is a `MutexGuard<Connection>`, auto-derefs to `&Connection`.

## Known Gaps / Future Work
- content_hash and previous_hash are not populated at the watcher WAL site (need to thread them through from the caller)
- source_path is not populated for watcher events (could be derived from the watcher's watch root)
- The bridge cursor column migration (`ALTER TABLE`) runs on every `auto_detect_changed_files` call; the pragma check makes it cheap but a proper migration would be cleaner
- The backfill maps `mutation_type` directly as `event_type` (old vocabulary like 'file_change' rather than new 'file_modified'); consumers of the observation events table should handle both vocabularies during transition

## Compilation Status
- `cargo check` passes (both `--lib` and default target including main.rs)
- Only pre-existing warnings remain (deprecated functions, private interfaces)
