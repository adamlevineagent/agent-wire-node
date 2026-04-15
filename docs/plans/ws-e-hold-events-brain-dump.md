# WS-E Brain Dump: Hold Events + Projection + auto_update_ops Rewrite

## What was done

### 1. auto_update_ops.rs — Full rewrite

**File:** `src-tauri/src/pyramid/auto_update_ops.rs`

Every hold mutation now writes three things atomically:
1. **Append-only hold event** -> `dadbear_hold_events`
2. **Materialized projection** -> `dadbear_holds_projection` (INSERT OR REPLACE on place, DELETE on clear)
3. **Old table dual-write** -> `pyramid_auto_update_config.frozen` / `breaker_tripped` (transition period)

After each mutation, TWO events are emitted:
- `DadbearHoldsChanged` (new canonical event)
- `AutoUpdateStateChanged` (existing event for backward compatibility)

Functions rewritten:
- `freeze()` — hold event (frozen, placed) + projection INSERT + old table frozen=1
- `unfreeze()` — hold event (frozen, cleared) + projection DELETE + old table frozen=0
- `trip_breaker()` — hold event (breaker, placed) + projection INSERT + old table breaker_tripped=1
- `resume_breaker()` — hold event (breaker, cleared) + projection DELETE + old table breaker_tripped=0
- `freeze_all()` — per-slug loop: hold event + projection + old table. Events only on actual change.
- `unfreeze_all()` — per-slug loop: hold event + projection + old table. Events only on actual change.
- `count_freeze_scope()` — unchanged interface, delegates to updated `resolve_scope_slugs`

New utility functions:
- `is_held(conn, slug) -> bool` — SELECT EXISTS from holds projection (any hold type)
- `get_holds(conn, slug) -> Vec<Hold>` — SELECT * from holds projection WHERE slug

`resolve_scope_slugs` fully rewritten to use `dadbear_holds_projection` for frozen-state filtering:
- `"all"` scope: frozen slugs from projection, unfrozen via NOT EXISTS against projection
- `"slug"` scope: check projection for presence/absence of frozen hold
- `"folder"` scope: JOIN pyramid_dadbear_config with EXISTS/NOT EXISTS on projection

### 2. db.rs — Master gate rewrite

**File:** `src-tauri/src/pyramid/db.rs`, function `get_enabled_dadbear_configs()`

Old query:
```sql
FROM pyramid_dadbear_config d
JOIN pyramid_auto_update_config a ON d.slug = a.slug
WHERE d.enabled = 1
  AND a.auto_update = 1 AND a.frozen = 0 AND a.breaker_tripped = 0
```

New query:
```sql
FROM pyramid_dadbear_config d
WHERE d.enabled = 1
  AND NOT EXISTS (SELECT 1 FROM dadbear_holds_projection h WHERE h.slug = d.slug)
  AND EXISTS (SELECT 1 FROM pyramid_auto_update_config a WHERE a.slug = d.slug AND a.auto_update = 1)
```

The holds anti-join is now the sole authority for frozen/breaker state. The `auto_update = 1` check is transitional (removed in Phase 7). The `d.enabled = 1` check is also transitional.

### 3. Migration seeding

**File:** `src-tauri/src/pyramid/db.rs`, in `init_pyramid_db()` after table creation

Seeds `dadbear_hold_events` and `dadbear_holds_projection` from current `pyramid_auto_update_config` state. Uses `INSERT OR IGNORE` for idempotency. Seeds both frozen and breaker holds with the original timestamps from frozen_at / breaker_tripped_at (falls back to datetime('now') if NULL).

## Compilation status

`cargo check` passes for all code in this workstream. 2 pre-existing errors remain in `stale_helpers_upper.rs` (borrow issue from WS-D observation_events work, not this workstream). 2 pre-existing warnings (deprecated function use, unused variable) also unrelated.

## What downstream consumers need to know

- All existing callers of `freeze/unfreeze/trip_breaker/resume_breaker/freeze_all/unfreeze_all/count_freeze_scope` continue to work with identical signatures.
- New `is_held` and `get_holds` are available for any consumer that wants to check holds.
- The `Hold` struct is exported from `auto_update_ops`.
- The master gate (`get_enabled_dadbear_configs`) now excludes ANY hold type, not just frozen/breaker. Future hold types (e.g., `cost_limit`) will automatically block dispatch when added to the projection.

## Phase 7 removal targets

When Phase 7 drops the old table:
1. Remove all `conn.execute("UPDATE pyramid_auto_update_config ...")` dual-writes from auto_update_ops.rs
2. Remove the `emit_state_changed` function and all calls to it
3. Remove the `AND EXISTS (SELECT 1 FROM pyramid_auto_update_config ...)` clause from the master gate
4. Remove the migration seeding SQL (projection will be the only source of truth)
