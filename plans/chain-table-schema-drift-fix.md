# Fix: Chain Table Schema Drift

## Problem

`pyramid_chain_defaults` was created with `CREATE TABLE IF NOT EXISTS` which is a no-op on existing databases. The code added `evidence_mode` column and changed the primary key from `(content_type)` to `(content_type, evidence_mode)`, but existing installs keep the old schema forever. Boot sync fails with:

```
error=no such column: evidence_mode in SELECT chain_id FROM pyramid_chain_defaults
```

This blocks ALL new pyramid builds.

## Root Cause

`CREATE TABLE IF NOT EXISTS` never alters an existing table. The old table has schema:
```sql
CREATE TABLE pyramid_chain_defaults (
    content_type TEXT PRIMARY KEY,
    chain_id TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

The code expects:
```sql
CREATE TABLE pyramid_chain_defaults (
    content_type TEXT NOT NULL,
    evidence_mode TEXT NOT NULL DEFAULT '*',
    chain_id TEXT NOT NULL,
    contribution_id TEXT NOT NULL,
    PRIMARY KEY (content_type, evidence_mode)
);
```

## Critical Constraint: init_pyramid_db Is Called Three Times

`init_pyramid_db()` (and therefore `init_chain_tables()`) is called THREE times during boot:

1. `main.rs:9997` — `init_pyramid_db(&pyramid_writer)`
2. `main.rs:10002` — `init_pyramid_db(&pyramid_reader)`
3. `main.rs:10419` — `init_pyramid_db(&partner_pyramid_reader)`

The sync that populates these tables runs ONCE at the tail of `migrate_prompts_and_chains_to_contributions()` (called at `main.rs:10072`), which is between calls 2 and 3. An unconditional `DROP TABLE` in `init_chain_tables()` would wipe data on the third call.

**Therefore: the schema fix must be conditional — only fire when the old schema is detected.**

## Fix

### Change 1: Conditional schema migration in `init_chain_tables()` (`chain_registry.rs`)

Replace the `CREATE TABLE IF NOT EXISTS pyramid_chain_defaults` block with a schema check + conditional rebuild:

```rust
pub fn init_chain_tables(conn: &Connection) -> Result<()> {
    // ── pyramid_chain_assignments ──────────────────────────────────
    // Schema is stable; CREATE IF NOT EXISTS is fine here.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS pyramid_chain_assignments (
            slug TEXT PRIMARY KEY REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
            chain_id TEXT NOT NULL,
            assigned_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )?;

    // ── pyramid_chain_defaults ─────────────────────────────────────
    // The schema evolved: added evidence_mode column + compound PK.
    // CREATE IF NOT EXISTS is a no-op on existing tables, so we must
    // detect the old schema and rebuild. This table is an operational
    // cache — source of truth is the chain_defaults contribution in
    // pyramid_config_contributions. Boot sync repopulates it.
    //
    // The check must be idempotent because init_pyramid_db() is called
    // multiple times during boot (writer, reader, partner_reader).
    // We only DROP+CREATE when the old schema is actually present.
    let needs_rebuild = if table_exists(conn, "pyramid_chain_defaults")? {
        !column_exists(conn, "pyramid_chain_defaults", "evidence_mode")?
    } else {
        false // table doesn't exist; CREATE will handle it
    };

    if needs_rebuild {
        info!("pyramid_chain_defaults: migrating old schema (adding evidence_mode, compound PK)");
        conn.execute_batch("DROP TABLE pyramid_chain_defaults;")?;
    }

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS pyramid_chain_defaults (
            content_type TEXT NOT NULL,
            evidence_mode TEXT NOT NULL DEFAULT '*',
            chain_id TEXT NOT NULL,
            contribution_id TEXT NOT NULL,
            PRIMARY KEY (content_type, evidence_mode)
        );",
    )?;

    Ok(())
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        rusqlite::params![table],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", table))?;
    let has_col = stmt.query_map([], |row| row.get::<_, String>(1))?
        .any(|name| name.map(|n| n == column).unwrap_or(false));
    Ok(has_col)
}
```

**Why conditional:** `init_chain_tables()` is called 3 times during boot. The schema check is idempotent — on the first call it detects the old schema and rebuilds; on subsequent calls the column exists so it's a no-op `CREATE IF NOT EXISTS`.

**Why not ALTER TABLE:** The PK also changed from `(content_type)` to `(content_type, evidence_mode)`. SQLite doesn't support `ALTER TABLE ... DROP/ADD PRIMARY KEY`. The only way to change a PK is to recreate the table.

### Change 2: Add boot sync for `pyramid_chain_assignments` (`wire_migration.rs`)

Add `sync_chain_assignments_to_operational(conn)` right after the existing `sync_chain_defaults_to_operational(conn)` call at line 1110.

```rust
fn sync_chain_assignments_to_operational(conn: &Connection) {
    // Walk all active chain_assignment contributions and replay them.
    // ORDER BY accepted_at DESC so if duplicates exist for the same slug,
    // the most recent wins (assign_chain uses ON CONFLICT DO UPDATE).
    let rows: Vec<(String, String, String)> = conn
        .prepare(
            "SELECT contribution_id, slug, yaml_content
             FROM pyramid_config_contributions
             WHERE schema_type = 'chain_assignment'
               AND status = 'active'
             ORDER BY accepted_at DESC",
        )
        .and_then(|mut stmt| {
            stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        })
        .unwrap_or_else(|e| {
            warn!(error = %e, "boot sync: failed to query chain_assignment contributions");
            Vec::new()
        });

    for (contribution_id, slug, yaml_content) in &rows {
        match serde_yaml::from_str::<ChainAssignmentYaml>(yaml_content) {
            Ok(yaml) => {
                if yaml.chain_id == "default" {
                    // "default" means "no override" — skip on boot sync since
                    // the table starts empty (or was just rebuilt).
                    continue;
                }
                if let Err(e) = chain_registry::assign_chain(conn, slug, &yaml.chain_id) {
                    warn!(
                        error = %e,
                        slug = %slug,
                        "boot sync: failed to replay chain_assignment"
                    );
                }
            }
            Err(e) => {
                warn!(
                    error = %e,
                    contribution_id = %contribution_id,
                    "boot sync: failed to parse chain_assignment YAML"
                );
            }
        }
    }

    if !rows.is_empty() {
        debug!(count = rows.len(), "boot sync: chain_assignments replayed");
    }
}
```

Note: `chain_assignment` contributions have a `slug` field on the contribution row itself (required per `config_contributions.rs:734-737`), so we read it from the row, not from the YAML body.

**Required import** in `wire_migration.rs` (add alongside existing `ChainDefaultsYaml` import):
```rust
use crate::pyramid::db::ChainAssignmentYaml;
```

### Change 3: Remove stale comment in `chain_registry.rs`

The comment at lines 24-28 says:
```
-- The old schema had an extra `chain_file TEXT` column that no call
-- site ever read. We leave it in place on existing installs (harmless)
-- rather than DROP+CREATE, which would destroy any user-set per-slug
-- assignments on every boot.
```

This predates the contribution system. Both tables are now contribution-backed caches. Remove this comment entirely.

## Files Changed

| File | Change |
|------|--------|
| `src-tauri/src/pyramid/chain_registry.rs` | `init_chain_tables()`: conditional schema migration for `chain_defaults`, add `table_exists`/`column_exists` helpers, remove stale comment |
| `src-tauri/src/pyramid/wire_migration.rs` | Add `sync_chain_assignments_to_operational()`, call it at line ~1110, add `ChainAssignmentYaml` import |

## Boot Sequence After Fix

1. `init_pyramid_db(&pyramid_writer)` calls `init_chain_tables()` → detects old schema, DROP+CREATE `chain_defaults` with correct schema; `chain_assignments` created normally
2. `init_pyramid_db(&pyramid_reader)` calls `init_chain_tables()` → schema check passes (column exists), no-op
3. `migrate_prompts_and_chains_to_contributions(&pyramid_writer)` runs → writes contributions, then:
   - `sync_chain_defaults_to_operational()` → repopulates `pyramid_chain_defaults`
   - `sync_chain_assignments_to_operational()` → repopulates `pyramid_chain_assignments`
4. `init_pyramid_db(&partner_pyramid_reader)` calls `init_chain_tables()` → schema check passes (column exists), no-op. Data from step 3 is preserved.
5. Build runner calls `resolve_chain_for_slug()` → tier 1 and tier 2 both have correct data

## What This Does NOT Fix

- The ~70 other `CREATE TABLE IF NOT EXISTS` tables in `db.rs` — most are source-of-truth tables that need proper versioned migrations, not DROP+CREATE. That's a separate project.
- The `pyramid_chain_overlays` table in `multi_chain_overlay.rs` uses the same `CREATE TABLE IF NOT EXISTS` pattern but its cache/source-of-truth status is unverified.

## Verification

After applying, the user's `architecturegemma426btest` build should succeed. Test:
1. `cargo check` passes
2. Boot the app — no `evidence_mode` errors in logs
3. Start a new pyramid build — progresses past 0/0 steps
