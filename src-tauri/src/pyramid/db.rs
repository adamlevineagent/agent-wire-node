// pyramid/db.rs — SQLite schema, migrations, and CRUD operations for the Knowledge Pyramid
//
// Tables: pyramid_slugs, pyramid_batches, pyramid_nodes, pyramid_chunks, pyramid_pipeline_steps
// All JSON columns (topics, corrections, decisions, terms, dead_ends, children) are stored as
// JSON strings and parsed/serialized via serde_json on read/write.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::collections::HashMap;

use super::naming::{clean_headline, headline_for_node};
use super::types::*;

// ── Database Opening ─────────────────────────────────────────────────────────

/// Open (or create) the pyramid SQLite database at the given path, initialize
/// tables and pragmas, and return the connection.
pub fn open_pyramid_db(path: &std::path::Path) -> Result<Connection> {
    let conn = Connection::open(path)
        .with_context(|| format!("Failed to open pyramid DB at {}", path.display()))?;
    init_pyramid_db(&conn)?;
    Ok(conn)
}

// ── Schema Initialization ────────────────────────────────────────────────────

/// Initialize pyramid tables. Call on app startup.
///
/// Enables WAL mode and foreign keys, then creates all five tables with
/// proper constraints and indices.
pub fn init_pyramid_db(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_slugs (
            slug TEXT PRIMARY KEY,
            content_type TEXT NOT NULL CHECK(content_type IN ('code', 'conversation', 'document', 'vine')),
            source_path TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            last_built_at TEXT,
            node_count INTEGER NOT NULL DEFAULT 0,
            max_depth INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS pyramid_batches (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
            batch_type TEXT NOT NULL DEFAULT 'initial',
            source_path TEXT NOT NULL DEFAULT '',
            chunk_offset INTEGER NOT NULL DEFAULT 0,
            chunk_count INTEGER NOT NULL DEFAULT 0,
            metadata TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS pyramid_nodes (
            id TEXT NOT NULL,
            slug TEXT NOT NULL REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
            depth INTEGER NOT NULL,
            chunk_index INTEGER,
            headline TEXT NOT NULL DEFAULT '',
            distilled TEXT NOT NULL DEFAULT '',
            topics TEXT,
            corrections TEXT,
            decisions TEXT,
            terms TEXT,
            dead_ends TEXT,
            self_prompt TEXT,
            children TEXT,
            parent_id TEXT,
            build_version INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (slug, id)
        );

        CREATE INDEX IF NOT EXISTS idx_pyramid_nodes_depth ON pyramid_nodes(slug, depth);
        CREATE INDEX IF NOT EXISTS idx_pyramid_nodes_parent ON pyramid_nodes(slug, parent_id);

        CREATE TABLE IF NOT EXISTS pyramid_chunks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
            batch_id INTEGER REFERENCES pyramid_batches(id),
            chunk_index INTEGER NOT NULL,
            content TEXT NOT NULL,
            line_count INTEGER,
            char_count INTEGER,
            UNIQUE(slug, chunk_index)
        );

        CREATE TABLE IF NOT EXISTS pyramid_pipeline_steps (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
            step_type TEXT NOT NULL,
            chunk_index INTEGER NOT NULL DEFAULT -1,
            depth INTEGER NOT NULL DEFAULT -1,
            node_id TEXT NOT NULL DEFAULT '',
            output_json TEXT,
            model TEXT,
            elapsed_seconds REAL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(slug, step_type, chunk_index, depth, node_id)
        );
        ",
    )?;

    // ── Delta chain schema migrations ────────────────────────────────────────

    // Migration-safe column addition (ignore error if column already exists)
    let _ = conn.execute(
        "ALTER TABLE pyramid_nodes ADD COLUMN superseded_by TEXT DEFAULT NULL",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_nodes ADD COLUMN headline TEXT NOT NULL DEFAULT ''",
        [],
    );

    conn.execute_batch(
        "
        -- Live nodes view (non-superseded, built nodes)
        CREATE VIEW IF NOT EXISTS live_pyramid_nodes AS
        SELECT * FROM pyramid_nodes WHERE build_version > 0 AND superseded_by IS NULL;

        -- Thread identity table
        CREATE TABLE IF NOT EXISTS pyramid_threads (
            slug TEXT NOT NULL,
            thread_id TEXT NOT NULL,
            thread_name TEXT NOT NULL,
            current_canonical_id TEXT NOT NULL,
            depth INTEGER NOT NULL DEFAULT 2,
            delta_count INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (slug, thread_id),
            FOREIGN KEY (slug, current_canonical_id) REFERENCES pyramid_nodes(slug, id)
        );

        -- Delta chain table
        CREATE TABLE IF NOT EXISTS pyramid_deltas (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL,
            thread_id TEXT NOT NULL,
            sequence INTEGER NOT NULL,
            content TEXT NOT NULL,
            relevance TEXT NOT NULL DEFAULT 'medium',
            source_node_id TEXT,
            flag TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(slug, thread_id, sequence),
            FOREIGN KEY (slug, thread_id) REFERENCES pyramid_threads(slug, thread_id)
        );
        CREATE INDEX IF NOT EXISTS idx_deltas_thread ON pyramid_deltas(slug, thread_id, sequence);

        -- Cumulative distillation table
        CREATE TABLE IF NOT EXISTS pyramid_distillations (
            slug TEXT NOT NULL,
            thread_id TEXT NOT NULL,
            content TEXT NOT NULL DEFAULT '',
            delta_count INTEGER NOT NULL DEFAULT 0,
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (slug, thread_id),
            FOREIGN KEY (slug, thread_id) REFERENCES pyramid_threads(slug, thread_id)
        );

        -- Collapse events log
        CREATE TABLE IF NOT EXISTS pyramid_collapse_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL,
            thread_id TEXT NOT NULL,
            old_canonical_id TEXT NOT NULL,
            new_canonical_id TEXT NOT NULL,
            deltas_absorbed INTEGER NOT NULL,
            model_used TEXT NOT NULL,
            elapsed_seconds REAL NOT NULL DEFAULT 0.0,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            FOREIGN KEY (slug, thread_id) REFERENCES pyramid_threads(slug, thread_id)
        );

        -- Web edges table
        CREATE TABLE IF NOT EXISTS pyramid_web_edges (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL,
            thread_a_id TEXT NOT NULL,
            thread_b_id TEXT NOT NULL,
            relationship TEXT NOT NULL DEFAULT '',
            relevance REAL NOT NULL DEFAULT 1.0,
            delta_count INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(slug, thread_a_id, thread_b_id),
            CHECK(thread_a_id < thread_b_id),
            FOREIGN KEY (slug, thread_a_id) REFERENCES pyramid_threads(slug, thread_id),
            FOREIGN KEY (slug, thread_b_id) REFERENCES pyramid_threads(slug, thread_id)
        );

        -- Web edge deltas
        CREATE TABLE IF NOT EXISTS pyramid_web_edge_deltas (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            edge_id INTEGER NOT NULL,
            content TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            FOREIGN KEY (edge_id) REFERENCES pyramid_web_edges(id) ON DELETE CASCADE
        );

        -- Annotations table
        CREATE TABLE IF NOT EXISTS pyramid_annotations (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL,
            node_id TEXT NOT NULL,
            annotation_type TEXT NOT NULL DEFAULT 'observation',
            content TEXT NOT NULL,
            question_context TEXT,
            author TEXT NOT NULL DEFAULT 'system',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            FOREIGN KEY (slug, node_id) REFERENCES pyramid_nodes(slug, id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_annotations_node ON pyramid_annotations(slug, node_id);

        -- Cost monitoring table
        CREATE TABLE IF NOT EXISTS pyramid_cost_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
            operation TEXT NOT NULL,
            model TEXT NOT NULL,
            input_tokens INTEGER NOT NULL DEFAULT 0,
            output_tokens INTEGER NOT NULL DEFAULT 0,
            estimated_cost REAL NOT NULL DEFAULT 0.0,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        ",
    )?;

    // ── FAQ nodes table ────────────────────────────────────────────────────────
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_faq_nodes (
            id TEXT PRIMARY KEY,
            slug TEXT NOT NULL REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
            question TEXT NOT NULL,
            answer TEXT NOT NULL,
            related_node_ids TEXT NOT NULL DEFAULT '[]',
            annotation_ids TEXT NOT NULL DEFAULT '[]',
            hit_count INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_faq_slug ON pyramid_faq_nodes(slug);
        ",
    )?;

    // ── FAQ category table ────────────────────────────────────────────────────
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_faq_categories (
            id TEXT PRIMARY KEY,
            slug TEXT NOT NULL REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
            name TEXT NOT NULL,
            distilled_summary TEXT NOT NULL DEFAULT '',
            faq_ids TEXT NOT NULL DEFAULT '[]',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_faq_cat_slug ON pyramid_faq_categories(slug);
        ",
    )?;

    // ── Usage log table ──────────────────────────────────────────────────────
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_usage_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
            query_type TEXT NOT NULL,
            query_params TEXT NOT NULL DEFAULT '{}',
            result_node_ids TEXT NOT NULL DEFAULT '[]',
            agent_id TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_usage_slug ON pyramid_usage_log(slug);
        CREATE INDEX IF NOT EXISTS idx_usage_type ON pyramid_usage_log(slug, query_type);
        ",
    )?;

    // ── v4.2 new tables ───────────────────────────────────────────────────────

    conn.execute_batch(
        "
        -- WAL for crash recovery of pending mutations
        CREATE TABLE IF NOT EXISTS pyramid_pending_mutations (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
            layer INTEGER NOT NULL,
            mutation_type TEXT NOT NULL,
            target_ref TEXT NOT NULL,
            detail TEXT,
            cascade_depth INTEGER NOT NULL DEFAULT 0,
            detected_at TEXT NOT NULL DEFAULT (datetime('now')),
            processed INTEGER NOT NULL DEFAULT 0,
            batch_id TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_pending_unprocessed ON pyramid_pending_mutations(slug, processed, layer);

        -- File hash tracking
        CREATE TABLE IF NOT EXISTS pyramid_file_hashes (
            slug TEXT NOT NULL REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
            file_path TEXT NOT NULL,
            hash TEXT NOT NULL,
            chunk_count INTEGER NOT NULL DEFAULT 0,
            node_ids TEXT NOT NULL DEFAULT '[]',
            last_ingested_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (slug, file_path)
        );

        -- Per-pyramid auto-update settings
        CREATE TABLE IF NOT EXISTS pyramid_auto_update_config (
            slug TEXT PRIMARY KEY REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
            auto_update INTEGER NOT NULL DEFAULT 0,
            debounce_minutes INTEGER NOT NULL DEFAULT 5 CHECK(debounce_minutes >= 1),
            min_changed_files INTEGER NOT NULL DEFAULT 1 CHECK(min_changed_files >= 1),
            runaway_threshold REAL NOT NULL DEFAULT 0.5 CHECK(runaway_threshold > 0.0 AND runaway_threshold <= 1.0),
            breaker_tripped INTEGER NOT NULL DEFAULT 0,
            breaker_tripped_at TEXT,
            frozen INTEGER NOT NULL DEFAULT 0,
            frozen_at TEXT
        );

        -- Stale-check audit trail
        CREATE TABLE IF NOT EXISTS pyramid_stale_check_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
            batch_id TEXT NOT NULL,
            layer INTEGER NOT NULL,
            target_id TEXT NOT NULL,
            stale INTEGER NOT NULL DEFAULT 0,
            reason TEXT NOT NULL DEFAULT '',
            checker_index INTEGER NOT NULL DEFAULT 0,
            checker_batch_size INTEGER NOT NULL DEFAULT 1,
            checked_at TEXT NOT NULL DEFAULT (datetime('now')),
            cost_tokens INTEGER,
            cost_usd REAL
        );
        CREATE INDEX IF NOT EXISTS idx_stale_check_slug ON pyramid_stale_check_log(slug, batch_id);
        CREATE INDEX IF NOT EXISTS idx_stale_check_target ON pyramid_stale_check_log(slug, target_id);

        -- Connection carryforward decisions
        CREATE TABLE IF NOT EXISTS pyramid_connection_check_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
            supersession_node_id TEXT NOT NULL,
            new_node_id TEXT NOT NULL,
            connection_type TEXT NOT NULL,
            connection_id TEXT NOT NULL,
            still_valid INTEGER NOT NULL DEFAULT 1,
            reason TEXT NOT NULL DEFAULT '',
            checked_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_conn_check_slug ON pyramid_connection_check_log(slug, supersession_node_id);
        ",
    )?;

    // ── v4.2 ALTER TABLE migrations (ignore-error pattern for existing columns) ──

    let _ = conn.execute(
        "ALTER TABLE pyramid_cost_log ADD COLUMN source TEXT DEFAULT 'manual'",
        [],
    );
    let _ = conn.execute("ALTER TABLE pyramid_cost_log ADD COLUMN layer INTEGER", []);
    let _ = conn.execute(
        "ALTER TABLE pyramid_cost_log ADD COLUMN check_type TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_faq_nodes ADD COLUMN match_triggers TEXT DEFAULT '[]'",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_auto_update_config ADD COLUMN ingested_extensions TEXT DEFAULT '[]'",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_auto_update_config ADD COLUMN ingested_config_files TEXT DEFAULT '[]'",
        [],
    );

    // ── Compensating DELETE triggers for FK CASCADE on existing DBs ──
    // (SQLite cannot ALTER FK constraints, so these triggers handle cascading
    //  deletes for tables created before CASCADE was added)
    let _ = conn.execute_batch(
        "
        CREATE TRIGGER IF NOT EXISTS fk_cascade_faq_on_slug_delete
        AFTER DELETE ON pyramid_slugs
        FOR EACH ROW BEGIN
            DELETE FROM pyramid_faq_nodes WHERE slug = OLD.slug;
        END;

        CREATE TRIGGER IF NOT EXISTS fk_cascade_cost_on_slug_delete
        AFTER DELETE ON pyramid_slugs
        FOR EACH ROW BEGIN
            DELETE FROM pyramid_cost_log WHERE slug = OLD.slug;
        END;

        CREATE TRIGGER IF NOT EXISTS fk_cascade_usage_on_slug_delete
        AFTER DELETE ON pyramid_slugs
        FOR EACH ROW BEGIN
            DELETE FROM pyramid_usage_log WHERE slug = OLD.slug;
        END;

        CREATE TRIGGER IF NOT EXISTS fk_cascade_annotations_on_node_delete
        AFTER DELETE ON pyramid_nodes
        FOR EACH ROW BEGIN
            DELETE FROM pyramid_annotations WHERE slug = OLD.slug AND node_id = OLD.id;
        END;
        ",
    );

    backfill_missing_headlines(conn)?;

    // Migrate existing L2+ nodes into threads
    migrate_existing_threads(conn)?;

    // ── Vine tables ────────────────────────────────────────────────────────────
    conn.execute_batch(
        "
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
            penultimate_node_ids TEXT,
            status          TEXT NOT NULL DEFAULT 'pending',
            metadata        TEXT,
            created_at      TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at      TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(vine_slug, bunch_slug)
        );
        CREATE INDEX IF NOT EXISTS idx_vine_bunches_vine ON vine_bunches(vine_slug);
        CREATE INDEX IF NOT EXISTS idx_vine_bunches_order ON vine_bunches(vine_slug, bunch_index);
        ",
    )?;

    // ── Migrate CHECK constraint to include 'vine' ────────────────────────────
    migrate_slugs_check_constraint(conn)?;

    Ok(())
}

/// Migrate `pyramid_slugs` CHECK constraint to include 'vine' content type.
/// Idempotent: skips if CHECK already includes 'vine'.
fn migrate_slugs_check_constraint(conn: &Connection) -> Result<()> {
    // Check if the table's CHECK constraint already includes 'vine'
    // by reading the table's SQL definition from sqlite_master
    let table_sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='pyramid_slugs'",
            [],
            |row| row.get(0),
        )
        .ok();

    let needs_migration = match &table_sql {
        Some(sql) => sql.contains("CHECK") && !sql.contains("vine"),
        None => false, // Table doesn't exist yet (will be created with vine on next startup)
    };

    if !needs_migration {
        return Ok(());
    }

    tracing::info!("Migrating pyramid_slugs CHECK constraint to include 'vine'...");

    // Must disable FK checks during table rebuild.
    // CRITICAL: Always re-enable FK checks, even on error.
    conn.execute_batch("PRAGMA foreign_keys=OFF;")?;

    let result = (|| -> Result<()> {
        let tx = conn.unchecked_transaction()?;

        tx.execute_batch(
            "
            CREATE TABLE pyramid_slugs_new (
                slug TEXT PRIMARY KEY,
                content_type TEXT NOT NULL CHECK(content_type IN ('code', 'conversation', 'document', 'vine')),
                source_path TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                last_built_at TEXT,
                node_count INTEGER NOT NULL DEFAULT 0,
                max_depth INTEGER NOT NULL DEFAULT 0
            );
            INSERT INTO pyramid_slugs_new SELECT * FROM pyramid_slugs;
            DROP TABLE pyramid_slugs;
            ALTER TABLE pyramid_slugs_new RENAME TO pyramid_slugs;
            ",
        )?;

        tx.commit()?;
        Ok(())
    })();

    // Always re-enable FK enforcement, regardless of success or failure
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;

    match result {
        Ok(()) => {
            tracing::info!("pyramid_slugs CHECK constraint migrated successfully.");
            Ok(())
        }
        Err(e) => {
            tracing::error!("pyramid_slugs migration failed (FK re-enabled): {e}");
            Err(e)
        }
    }
}

// ── Slug CRUD ────────────────────────────────────────────────────────────────

/// Create a new slug entry. Returns the created SlugInfo.
pub fn create_slug(
    conn: &Connection,
    slug: &str,
    content_type: &ContentType,
    source_path: &str,
) -> Result<SlugInfo> {
    conn.execute(
        "INSERT INTO pyramid_slugs (slug, content_type, source_path) VALUES (?1, ?2, ?3)",
        rusqlite::params![slug, content_type.as_str(), source_path],
    )
    .with_context(|| format!("Failed to create slug '{slug}'"))?;

    // Read back the row to get server-generated defaults (created_at)
    get_slug(conn, slug)?.ok_or_else(|| anyhow::anyhow!("Slug '{slug}' not found after insert"))
}

/// Fetch a slug by name. Returns None if not found.
pub fn get_slug(conn: &Connection, slug: &str) -> Result<Option<SlugInfo>> {
    let mut stmt = conn.prepare(
        "SELECT slug, content_type, source_path, node_count, max_depth, last_built_at, created_at
         FROM pyramid_slugs WHERE slug = ?1",
    )?;

    let result = stmt.query_row(rusqlite::params![slug], |row| {
        let ct_str: String = row.get(1)?;
        let content_type = ContentType::from_str(&ct_str).unwrap_or_else(|| {
            tracing::warn!("Unknown content_type '{ct_str}' for slug, defaulting to Document");
            ContentType::Document
        });
        Ok(SlugInfo {
            slug: row.get(0)?,
            content_type,
            source_path: row.get(2)?,
            node_count: row.get(3)?,
            max_depth: row.get(4)?,
            last_built_at: row.get(5)?,
            created_at: row.get(6)?,
        })
    });

    match result {
        Ok(info) => Ok(Some(info)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// List all slugs, ordered by created_at descending.
/// Optionally excludes bunch slugs (those containing "--bunch-") from the listing.
pub fn list_slugs(conn: &Connection) -> Result<Vec<SlugInfo>> {
    list_slugs_filtered(conn, true)
}

/// List slugs with optional bunch filtering.
pub fn list_slugs_filtered(conn: &Connection, exclude_bunches: bool) -> Result<Vec<SlugInfo>> {
    let sql = if exclude_bunches {
        "SELECT slug, content_type, source_path, node_count, max_depth, last_built_at, created_at
         FROM pyramid_slugs WHERE slug NOT LIKE '%--bunch-%' ORDER BY created_at DESC"
    } else {
        "SELECT slug, content_type, source_path, node_count, max_depth, last_built_at, created_at
         FROM pyramid_slugs ORDER BY created_at DESC"
    };
    let mut stmt = conn.prepare(sql)?;

    let rows = stmt.query_map([], |row| {
        let ct_str: String = row.get(1)?;
        let content_type = ContentType::from_str(&ct_str).unwrap_or_else(|| {
            tracing::warn!("Unknown content_type '{ct_str}' in list_slugs, defaulting to Document");
            ContentType::Document
        });
        Ok(SlugInfo {
            slug: row.get(0)?,
            content_type,
            source_path: row.get(2)?,
            node_count: row.get(3)?,
            max_depth: row.get(4)?,
            last_built_at: row.get(5)?,
            created_at: row.get(6)?,
        })
    })?;

    let mut slugs = Vec::new();
    for row in rows {
        slugs.push(row?);
    }
    Ok(slugs)
}

/// Delete a slug and all associated data (cascades to nodes, chunks, batches, steps).
pub fn delete_slug(conn: &Connection, slug: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM pyramid_slugs WHERE slug = ?1",
        rusqlite::params![slug],
    )?;
    Ok(())
}

/// Delete pipeline steps above a given depth for a slug.
/// Needed for force-rebuild: step_exists() would skip work if old steps remain.
pub fn delete_steps_above_depth(conn: &Connection, slug: &str, depth: i64) -> Result<i64> {
    let count = conn.execute(
        "DELETE FROM pyramid_pipeline_steps WHERE slug = ?1 AND depth > ?2",
        rusqlite::params![slug, depth],
    )?;
    Ok(count as i64)
}

/// Recompute node_count, max_depth, and last_built_at from the nodes table.
pub fn update_slug_stats(conn: &Connection, slug: &str) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_slugs SET
            node_count = (SELECT COUNT(*) FROM live_pyramid_nodes WHERE slug = ?1),
            max_depth = COALESCE((SELECT MAX(depth) FROM live_pyramid_nodes WHERE slug = ?1), 0),
            last_built_at = datetime('now')
         WHERE slug = ?1",
        rusqlite::params![slug],
    )?;
    Ok(())
}

// ── Batch CRUD ───────────────────────────────────────────────────────────────

/// Create a batch record. Returns the new batch ID.
pub fn create_batch(
    conn: &Connection,
    slug: &str,
    batch_type: &str,
    source_path: &str,
    chunk_offset: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO pyramid_batches (slug, batch_type, source_path, chunk_offset)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![slug, batch_type, source_path, chunk_offset],
    )?;
    Ok(conn.last_insert_rowid())
}

// ── Chunk CRUD ───────────────────────────────────────────────────────────────

/// Insert a chunk. Computes line_count and char_count automatically.
pub fn insert_chunk(
    conn: &Connection,
    slug: &str,
    batch_id: i64,
    chunk_index: i64,
    content: &str,
) -> Result<()> {
    let line_count = content.matches('\n').count() as i64 + 1;
    let char_count = content.len() as i64;

    conn.execute(
        "INSERT INTO pyramid_chunks (slug, batch_id, chunk_index, content, line_count, char_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![slug, batch_id, chunk_index, content, line_count, char_count],
    )?;

    // Update batch chunk_count
    conn.execute(
        "UPDATE pyramid_batches SET chunk_count = chunk_count + 1 WHERE id = ?1",
        rusqlite::params![batch_id],
    )?;

    Ok(())
}

/// Get chunk content by slug and chunk_index.
pub fn get_chunk(conn: &Connection, slug: &str, chunk_index: i64) -> Result<Option<String>> {
    let mut stmt =
        conn.prepare("SELECT content FROM pyramid_chunks WHERE slug = ?1 AND chunk_index = ?2")?;

    let result = stmt.query_row(rusqlite::params![slug, chunk_index], |row| {
        row.get::<_, String>(0)
    });

    match result {
        Ok(content) => Ok(Some(content)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Count total chunks for a slug.
pub fn count_chunks(conn: &Connection, slug: &str) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pyramid_chunks WHERE slug = ?1",
        rusqlite::params![slug],
        |row| row.get(0),
    )?;
    Ok(count)
}

// ── Node CRUD ────────────────────────────────────────────────────────────────

/// Parse a JSON string into a Vec<T>, returning an empty vec on null/empty/error.
fn parse_json_vec<T: serde::de::DeserializeOwned>(json: &str) -> Vec<T> {
    if json.is_empty() || json == "null" {
        return Vec::new();
    }
    serde_json::from_str(json).unwrap_or_default()
}

fn load_source_paths_by_node_id(conn: &Connection) -> Result<HashMap<String, String>> {
    let mut stmt =
        conn.prepare("SELECT file_path, node_ids FROM pyramid_file_hashes ORDER BY file_path")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    let mut source_path_by_node_id = HashMap::new();
    for row in rows {
        let (file_path, node_ids_json) = row?;
        let node_ids: Vec<String> = serde_json::from_str(&node_ids_json).unwrap_or_default();
        for node_id in node_ids {
            source_path_by_node_id
                .entry(node_id)
                .or_insert_with(|| file_path.clone());
        }
    }

    Ok(source_path_by_node_id)
}

fn backfill_missing_headlines(conn: &Connection) -> Result<()> {
    let source_path_by_node_id = load_source_paths_by_node_id(conn)?;
    let sql = format!(
        "SELECT {NODE_SELECT_COLS} FROM pyramid_nodes
         WHERE headline IS NULL OR TRIM(headline) = ''"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], node_from_row)?;

    for row in rows {
        let node = row?;
        let headline = headline_for_node(
            &node,
            source_path_by_node_id
                .get(&node.id)
                .map(|path| path.as_str()),
        );
        conn.execute(
            "UPDATE pyramid_nodes SET headline = ?1 WHERE slug = ?2 AND id = ?3",
            rusqlite::params![headline, node.slug, node.id],
        )?;
    }

    Ok(())
}

/// Build a PyramidNode from a rusqlite Row using named column access.
///
/// Uses named columns for robustness against schema column reordering.
/// Works with both `SELECT *` and `SELECT {NODE_SELECT_COLS}` queries.
pub fn node_from_row(row: &rusqlite::Row) -> rusqlite::Result<PyramidNode> {
    let topics_json: String = row.get::<_, String>("topics").unwrap_or_default();
    let corrections_json: String = row.get::<_, String>("corrections").unwrap_or_default();
    let decisions_json: String = row.get::<_, String>("decisions").unwrap_or_default();
    let terms_json: String = row.get::<_, String>("terms").unwrap_or_default();
    let dead_ends_json: String = row.get::<_, String>("dead_ends").unwrap_or_default();
    let children_json: String = row.get::<_, String>("children").unwrap_or_default();

    Ok(PyramidNode {
        id: row.get("id")?,
        slug: row.get("slug")?,
        depth: row.get("depth")?,
        chunk_index: row.get("chunk_index").ok(),
        headline: row.get::<_, String>("headline").unwrap_or_default(),
        distilled: row.get("distilled")?,
        topics: parse_json_vec(&topics_json),
        corrections: parse_json_vec(&corrections_json),
        decisions: parse_json_vec(&decisions_json),
        terms: parse_json_vec(&terms_json),
        dead_ends: parse_json_vec(&dead_ends_json),
        self_prompt: row.get::<_, String>("self_prompt").unwrap_or_default(),
        children: parse_json_vec(&children_json),
        parent_id: row.get("parent_id").ok().and_then(
            |v: String| {
                if v.is_empty() {
                    None
                } else {
                    Some(v)
                }
            },
        ),
        superseded_by: row
            .get::<_, Option<String>>("superseded_by")
            .unwrap_or(None),
        created_at: row.get::<_, String>("created_at").unwrap_or_default(),
    })
}

const NODE_SELECT_COLS: &str =
    "id, slug, depth, chunk_index, headline, distilled, topics, corrections, decisions, \
     terms, dead_ends, self_prompt, children, parent_id, superseded_by, created_at";

/// Save (upsert) a PyramidNode. Serializes all Vec fields to JSON strings.
///
/// The optional `topics_json` parameter allows passing a pre-serialized topics
/// string (useful when the build pipeline already has the raw JSON). If None,
/// topics are serialized from `node.topics`.
pub fn save_node(conn: &Connection, node: &PyramidNode, topics_json: Option<&str>) -> Result<()> {
    let topics = match topics_json {
        Some(s) => s.to_string(),
        None => serde_json::to_string(&node.topics)?,
    };
    let corrections = serde_json::to_string(&node.corrections)?;
    let decisions = serde_json::to_string(&node.decisions)?;
    let terms = serde_json::to_string(&node.terms)?;
    let dead_ends = serde_json::to_string(&node.dead_ends)?;
    let children = serde_json::to_string(&node.children)?;
    let headline = clean_headline(&node.headline).unwrap_or_else(|| headline_for_node(node, None));

    conn.execute(
        "INSERT INTO pyramid_nodes
            (id, slug, depth, chunk_index, headline, distilled, topics, corrections, decisions,
             terms, dead_ends, self_prompt, children, parent_id, superseded_by)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT(slug, id) DO UPDATE SET
            depth = excluded.depth,
            chunk_index = excluded.chunk_index,
            headline = excluded.headline,
            distilled = excluded.distilled,
            topics = excluded.topics,
            corrections = excluded.corrections,
            decisions = excluded.decisions,
            terms = excluded.terms,
            dead_ends = excluded.dead_ends,
            self_prompt = excluded.self_prompt,
            children = excluded.children,
            parent_id = excluded.parent_id,
            superseded_by = excluded.superseded_by,
            build_version = build_version + 1",
        rusqlite::params![
            node.id,
            node.slug,
            node.depth,
            node.chunk_index,
            headline,
            node.distilled,
            topics,
            corrections,
            decisions,
            terms,
            dead_ends,
            node.self_prompt,
            children,
            node.parent_id,
            node.superseded_by,
        ],
    )?;

    Ok(())
}

/// Get a single node by slug and node ID.
pub fn get_node(conn: &Connection, slug: &str, node_id: &str) -> Result<Option<PyramidNode>> {
    let sql = format!("SELECT {NODE_SELECT_COLS} FROM pyramid_nodes WHERE slug = ?1 AND id = ?2");
    let mut stmt = conn.prepare(&sql)?;

    let result = stmt.query_row(rusqlite::params![slug, node_id], node_from_row);

    match result {
        Ok(node) => Ok(Some(node)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Get all nodes at a given depth for a slug, ordered by chunk_index.
pub fn get_nodes_at_depth(conn: &Connection, slug: &str, depth: i64) -> Result<Vec<PyramidNode>> {
    let sql = format!(
        "SELECT {NODE_SELECT_COLS} FROM live_pyramid_nodes
         WHERE slug = ?1 AND depth = ?2
         ORDER BY chunk_index ASC, id ASC"
    );
    let mut stmt = conn.prepare(&sql)?;

    let rows = stmt.query_map(rusqlite::params![slug, depth], node_from_row)?;

    let mut nodes = Vec::new();
    for row in rows {
        nodes.push(row?);
    }
    Ok(nodes)
}

/// Count nodes at a given depth for a slug.
pub fn count_nodes_at_depth(conn: &Connection, slug: &str, depth: i64) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM live_pyramid_nodes WHERE slug = ?1 AND depth = ?2",
        rusqlite::params![slug, depth],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Delete all nodes with depth > the given depth. Returns count of deleted rows.
/// Used when rebuilding upper layers of the pyramid.
pub fn delete_nodes_above(conn: &Connection, slug: &str, depth: i64) -> Result<i64> {
    let deleted = conn.execute(
        "DELETE FROM pyramid_nodes WHERE slug = ?1 AND depth > ?2",
        rusqlite::params![slug, depth],
    )?;
    Ok(deleted as i64)
}

/// Update a node's parent_id.
pub fn update_parent(conn: &Connection, slug: &str, node_id: &str, parent_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_nodes SET parent_id = ?3 WHERE slug = ?1 AND id = ?2",
        rusqlite::params![slug, node_id, parent_id],
    )?;
    Ok(())
}

// ── Pipeline Step Tracking ───────────────────────────────────────────────────

/// Save a pipeline step record (for resumability).
pub fn save_step(
    conn: &Connection,
    slug: &str,
    step_type: &str,
    chunk_index: i64,
    depth: i64,
    node_id: &str,
    output_json: &str,
    model: &str,
    elapsed: f64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_pipeline_steps
            (slug, step_type, chunk_index, depth, node_id, output_json, model, elapsed_seconds)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(slug, step_type, chunk_index, depth, node_id) DO UPDATE SET
            output_json = excluded.output_json,
            model = excluded.model,
            elapsed_seconds = excluded.elapsed_seconds",
        rusqlite::params![
            slug,
            step_type,
            chunk_index,
            depth,
            node_id,
            output_json,
            model,
            elapsed
        ],
    )?;
    Ok(())
}

/// Check whether a specific pipeline step has already been completed.
pub fn step_exists(
    conn: &Connection,
    slug: &str,
    step_type: &str,
    chunk_index: i64,
    depth: i64,
    node_id: &str,
) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pyramid_pipeline_steps
         WHERE slug = ?1 AND step_type = ?2 AND chunk_index = ?3 AND depth = ?4 AND node_id = ?5",
        rusqlite::params![slug, step_type, chunk_index, depth, node_id],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Get the output_json for a step, looked up by slug + step_type + chunk_index.
/// Returns the most recent match (by id DESC) if multiple exist at different depths.
pub fn get_step_output(
    conn: &Connection,
    slug: &str,
    step_type: &str,
    chunk_index: i64,
) -> Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT output_json FROM pyramid_pipeline_steps
         WHERE slug = ?1 AND step_type = ?2 AND chunk_index = ?3
         ORDER BY id DESC LIMIT 1",
    )?;

    let result = stmt.query_row(rusqlite::params![slug, step_type, chunk_index], |row| {
        row.get::<_, Option<String>>(0)
    });

    match result {
        Ok(json) => Ok(json),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Delete all pipeline steps of a given type for a slug.
/// Used when re-running a specific pipeline phase.
pub fn delete_steps(conn: &Connection, slug: &str, step_type: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM pyramid_pipeline_steps WHERE slug = ?1 AND step_type = ?2",
        rusqlite::params![slug, step_type],
    )?;
    Ok(())
}

// ── Thread Migration ─────────────────────────────────────────────────────────

/// Backfill missing thread/distillation rows for live L1+ canonical nodes.
/// Safe to call multiple times — only inserts rows that are still missing.
pub fn migrate_existing_threads(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        INSERT OR IGNORE INTO pyramid_threads (slug, thread_id, thread_name, current_canonical_id, depth)
        SELECT pn.slug, pn.id, COALESCE(NULLIF(TRIM(pn.headline), ''), json_extract(pn.topics, '$[0].name'), 'Untitled-' || pn.id), pn.id, pn.depth
        FROM pyramid_nodes pn
        WHERE pn.depth >= 1
          AND pn.build_version > 0
          AND pn.superseded_by IS NULL
          AND NOT EXISTS (
              SELECT 1
              FROM pyramid_threads pt
              WHERE pt.slug = pn.slug
                AND pt.current_canonical_id = pn.id
          );

        INSERT OR IGNORE INTO pyramid_distillations (slug, thread_id, content, delta_count)
        SELECT slug, thread_id, '', 0
        FROM pyramid_threads;
        ",
    )?;

    Ok(())
}

// ── Thread CRUD ──────────────────────────────────────────────────────────────

/// Get all threads for a slug, ordered by thread_name.
pub fn get_threads(conn: &Connection, slug: &str) -> Result<Vec<PyramidThread>> {
    let mut stmt = conn.prepare(
        "SELECT slug, thread_id, thread_name, current_canonical_id, depth, delta_count, created_at, updated_at
         FROM pyramid_threads WHERE slug = ?1 ORDER BY thread_name ASC",
    )?;

    let rows = stmt.query_map(rusqlite::params![slug], |row| {
        Ok(PyramidThread {
            slug: row.get(0)?,
            thread_id: row.get(1)?,
            thread_name: row.get(2)?,
            current_canonical_id: row.get(3)?,
            depth: row.get(4)?,
            delta_count: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    })?;

    let mut threads = Vec::new();
    for row in rows {
        threads.push(row?);
    }
    Ok(threads)
}

/// Get a single thread by slug and thread_id.
pub fn get_thread(conn: &Connection, slug: &str, thread_id: &str) -> Result<Option<PyramidThread>> {
    let mut stmt = conn.prepare(
        "SELECT slug, thread_id, thread_name, current_canonical_id, depth, delta_count, created_at, updated_at
         FROM pyramid_threads WHERE slug = ?1 AND thread_id = ?2",
    )?;

    let result = stmt.query_row(rusqlite::params![slug, thread_id], |row| {
        Ok(PyramidThread {
            slug: row.get(0)?,
            thread_id: row.get(1)?,
            thread_name: row.get(2)?,
            current_canonical_id: row.get(3)?,
            depth: row.get(4)?,
            delta_count: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    });

    match result {
        Ok(thread) => Ok(Some(thread)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Save (upsert) a thread.
pub fn save_thread(conn: &Connection, thread: &PyramidThread) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_threads (slug, thread_id, thread_name, current_canonical_id, depth, delta_count, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(slug, thread_id) DO UPDATE SET
            thread_name = excluded.thread_name,
            current_canonical_id = excluded.current_canonical_id,
            depth = excluded.depth,
            delta_count = excluded.delta_count,
            updated_at = excluded.updated_at",
        rusqlite::params![
            thread.slug,
            thread.thread_id,
            thread.thread_name,
            thread.current_canonical_id,
            thread.depth,
            thread.delta_count,
            thread.created_at,
            thread.updated_at,
        ],
    )?;
    Ok(())
}

// ── Distillation CRUD ────────────────────────────────────────────────────────

/// Get the cumulative distillation for a thread.
pub fn get_distillation(
    conn: &Connection,
    slug: &str,
    thread_id: &str,
) -> Result<Option<CumulativeDistillation>> {
    let mut stmt = conn.prepare(
        "SELECT slug, thread_id, content, delta_count, updated_at
         FROM pyramid_distillations WHERE slug = ?1 AND thread_id = ?2",
    )?;

    let result = stmt.query_row(rusqlite::params![slug, thread_id], |row| {
        Ok(CumulativeDistillation {
            slug: row.get(0)?,
            thread_id: row.get(1)?,
            content: row.get(2)?,
            delta_count: row.get(3)?,
            updated_at: row.get(4)?,
        })
    });

    match result {
        Ok(d) => Ok(Some(d)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Save (upsert) a cumulative distillation.
pub fn save_distillation(conn: &Connection, distillation: &CumulativeDistillation) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_distillations (slug, thread_id, content, delta_count, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(slug, thread_id) DO UPDATE SET
            content = excluded.content,
            delta_count = excluded.delta_count,
            updated_at = excluded.updated_at",
        rusqlite::params![
            distillation.slug,
            distillation.thread_id,
            distillation.content,
            distillation.delta_count,
            distillation.updated_at,
        ],
    )?;
    Ok(())
}

// ── Delta CRUD ───────────────────────────────────────────────────────────────

/// Get deltas for a thread, ordered by sequence. Optional limit.
pub fn get_deltas(
    conn: &Connection,
    slug: &str,
    thread_id: &str,
    limit: Option<i64>,
) -> Result<Vec<Delta>> {
    let sql = match limit {
        Some(n) => format!(
            "SELECT id, slug, thread_id, sequence, content, relevance, source_node_id, flag, created_at
             FROM pyramid_deltas WHERE slug = ?1 AND thread_id = ?2
             ORDER BY sequence ASC LIMIT {n}"
        ),
        None => "SELECT id, slug, thread_id, sequence, content, relevance, source_node_id, flag, created_at
                 FROM pyramid_deltas WHERE slug = ?1 AND thread_id = ?2
                 ORDER BY sequence ASC".to_string(),
    };

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![slug, thread_id], |row| {
        let relevance_str: String = row.get(5)?;
        Ok(Delta {
            id: row.get(0)?,
            slug: row.get(1)?,
            thread_id: row.get(2)?,
            sequence: row.get(3)?,
            content: row.get(4)?,
            relevance: DeltaRelevance::from_str(&relevance_str),
            source_node_id: row.get(6)?,
            flag: row.get(7)?,
            created_at: row.get(8)?,
        })
    })?;

    let mut deltas = Vec::new();
    for row in rows {
        deltas.push(row?);
    }
    Ok(deltas)
}

/// Save a delta. Returns the new row ID.
pub fn save_delta(conn: &Connection, delta: &Delta) -> Result<i64> {
    conn.execute(
        "INSERT INTO pyramid_deltas (slug, thread_id, sequence, content, relevance, source_node_id, flag)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            delta.slug,
            delta.thread_id,
            delta.sequence,
            delta.content,
            delta.relevance.as_str(),
            delta.source_node_id,
            delta.flag,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

// ── Web Edge CRUD ────────────────────────────────────────────────────────────

/// Get all web edges for a slug.
pub fn get_web_edges(conn: &Connection, slug: &str) -> Result<Vec<WebEdge>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, thread_a_id, thread_b_id, relationship, relevance, delta_count, created_at, updated_at
         FROM pyramid_web_edges WHERE slug = ?1 ORDER BY relevance DESC",
    )?;

    let rows = stmt.query_map(rusqlite::params![slug], |row| {
        Ok(WebEdge {
            id: row.get(0)?,
            slug: row.get(1)?,
            thread_a_id: row.get(2)?,
            thread_b_id: row.get(3)?,
            relationship: row.get(4)?,
            relevance: row.get(5)?,
            delta_count: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
        })
    })?;

    let mut edges = Vec::new();
    for row in rows {
        edges.push(row?);
    }
    Ok(edges)
}

/// Save (upsert) a web edge. Returns the row ID.
pub fn save_web_edge(conn: &Connection, edge: &WebEdge) -> Result<i64> {
    conn.execute(
        "INSERT INTO pyramid_web_edges (slug, thread_a_id, thread_b_id, relationship, relevance, delta_count, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))
         ON CONFLICT(slug, thread_a_id, thread_b_id) DO UPDATE SET
            relationship = excluded.relationship,
            relevance = excluded.relevance,
            delta_count = excluded.delta_count,
            updated_at = excluded.updated_at",
        rusqlite::params![
            edge.slug,
            edge.thread_a_id,
            edge.thread_b_id,
            edge.relationship,
            edge.relevance,
            edge.delta_count,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Get a single web edge between two threads (normalized order: a < b).
pub fn get_web_edge_between(
    conn: &Connection,
    slug: &str,
    thread_a_id: &str,
    thread_b_id: &str,
) -> Result<Option<WebEdge>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, thread_a_id, thread_b_id, relationship, relevance, delta_count, created_at, updated_at
         FROM pyramid_web_edges WHERE slug = ?1 AND thread_a_id = ?2 AND thread_b_id = ?3",
    )?;

    let mut rows = stmt.query_map(rusqlite::params![slug, thread_a_id, thread_b_id], |row| {
        Ok(WebEdge {
            id: row.get(0)?,
            slug: row.get(1)?,
            thread_a_id: row.get(2)?,
            thread_b_id: row.get(3)?,
            relationship: row.get(4)?,
            relevance: row.get(5)?,
            delta_count: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
        })
    })?;

    match rows.next() {
        Some(Ok(edge)) => Ok(Some(edge)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

/// Save a web edge delta. Returns the new row ID.
pub fn save_web_edge_delta(conn: &Connection, delta: &WebEdgeDelta) -> Result<i64> {
    conn.execute(
        "INSERT INTO pyramid_web_edge_deltas (edge_id, content) VALUES (?1, ?2)",
        rusqlite::params![delta.edge_id, delta.content],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Get all deltas for a web edge, ordered by creation time.
pub fn get_web_edge_deltas(conn: &Connection, edge_id: i64) -> Result<Vec<WebEdgeDelta>> {
    let mut stmt = conn.prepare(
        "SELECT id, edge_id, content, created_at
         FROM pyramid_web_edge_deltas WHERE edge_id = ?1 ORDER BY id ASC",
    )?;

    let rows = stmt.query_map(rusqlite::params![edge_id], |row| {
        Ok(WebEdgeDelta {
            id: row.get(0)?,
            edge_id: row.get(1)?,
            content: row.get(2)?,
            created_at: row.get(3)?,
        })
    })?;

    let mut deltas = Vec::new();
    for row in rows {
        deltas.push(row?);
    }
    Ok(deltas)
}

/// Delete web edge deltas by edge ID. Returns the number of deleted rows.
pub fn delete_web_edge_deltas(conn: &Connection, edge_id: i64) -> Result<usize> {
    let count = conn.execute(
        "DELETE FROM pyramid_web_edge_deltas WHERE edge_id = ?1",
        rusqlite::params![edge_id],
    )?;
    Ok(count)
}

/// Update a web edge's relationship, relevance, and delta_count.
pub fn update_web_edge(
    conn: &Connection,
    edge_id: i64,
    relationship: &str,
    relevance: f64,
    delta_count: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_web_edges SET relationship = ?1, relevance = ?2, delta_count = ?3, updated_at = datetime('now') WHERE id = ?4",
        rusqlite::params![relationship, relevance, delta_count, edge_id],
    )?;
    Ok(())
}

/// Get a web edge by its ID.
pub fn get_web_edge(conn: &Connection, edge_id: i64) -> Result<Option<WebEdge>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, thread_a_id, thread_b_id, relationship, relevance, delta_count, created_at, updated_at
         FROM pyramid_web_edges WHERE id = ?1",
    )?;

    let mut rows = stmt.query_map(rusqlite::params![edge_id], |row| {
        Ok(WebEdge {
            id: row.get(0)?,
            slug: row.get(1)?,
            thread_a_id: row.get(2)?,
            thread_b_id: row.get(3)?,
            relationship: row.get(4)?,
            relevance: row.get(5)?,
            delta_count: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
        })
    })?;

    match rows.next() {
        Some(Ok(edge)) => Ok(Some(edge)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

/// Decay all web edges for a slug by reducing relevance. Returns count of decayed edges.
pub fn decay_web_edges(conn: &Connection, slug: &str, decay_rate: f64) -> Result<usize> {
    // Reduce relevance
    conn.execute(
        "UPDATE pyramid_web_edges SET relevance = relevance - ?1, updated_at = datetime('now') WHERE slug = ?2",
        rusqlite::params![decay_rate, slug],
    )?;

    // Delete edges that dropped below threshold
    let archived = conn.execute(
        "DELETE FROM pyramid_web_edges WHERE slug = ?1 AND relevance < 0.1",
        rusqlite::params![slug],
    )?;

    Ok(archived)
}

/// Get active web edges above a minimum relevance threshold.
pub fn get_active_edges(conn: &Connection, slug: &str, min_relevance: f64) -> Result<Vec<WebEdge>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, thread_a_id, thread_b_id, relationship, relevance, delta_count, created_at, updated_at
         FROM pyramid_web_edges WHERE slug = ?1 AND relevance >= ?2 ORDER BY relevance DESC",
    )?;

    let rows = stmt.query_map(rusqlite::params![slug, min_relevance], |row| {
        Ok(WebEdge {
            id: row.get(0)?,
            slug: row.get(1)?,
            thread_a_id: row.get(2)?,
            thread_b_id: row.get(3)?,
            relationship: row.get(4)?,
            relevance: row.get(5)?,
            delta_count: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
        })
    })?;

    let mut edges = Vec::new();
    for row in rows {
        edges.push(row?);
    }
    Ok(edges)
}

// ── Collapse Event CRUD ──────────────────────────────────────────────────────

/// Save a collapse event. Returns the new row ID.
pub fn save_collapse_event(conn: &Connection, event: &CollapseEvent) -> Result<i64> {
    conn.execute(
        "INSERT INTO pyramid_collapse_events (slug, thread_id, old_canonical_id, new_canonical_id, deltas_absorbed, model_used, elapsed_seconds)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            event.slug,
            event.thread_id,
            event.old_canonical_id,
            event.new_canonical_id,
            event.deltas_absorbed,
            event.model_used,
            event.elapsed_seconds,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

// ── Annotation CRUD ──────────────────────────────────────────────────────────

/// Get annotations for a node.
pub fn get_annotations(
    conn: &Connection,
    slug: &str,
    node_id: &str,
) -> Result<Vec<PyramidAnnotation>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, node_id, annotation_type, content, question_context, author, created_at
         FROM pyramid_annotations WHERE slug = ?1 AND node_id = ?2
         ORDER BY created_at DESC",
    )?;

    let rows = stmt.query_map(rusqlite::params![slug, node_id], |row| {
        let at_str: String = row.get(3)?;
        Ok(PyramidAnnotation {
            id: row.get(0)?,
            slug: row.get(1)?,
            node_id: row.get(2)?,
            annotation_type: AnnotationType::from_str(&at_str),
            content: row.get(4)?,
            question_context: row.get(5)?,
            author: row.get(6)?,
            created_at: row.get(7)?,
        })
    })?;

    let mut annotations = Vec::new();
    for row in rows {
        annotations.push(row?);
    }
    Ok(annotations)
}

/// Get all annotations for a slug (across all nodes).
pub fn get_all_annotations(conn: &Connection, slug: &str) -> Result<Vec<PyramidAnnotation>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, node_id, annotation_type, content, question_context, author, created_at
         FROM pyramid_annotations WHERE slug = ?1
         ORDER BY created_at DESC",
    )?;

    let rows = stmt.query_map(rusqlite::params![slug], |row| {
        let at_str: String = row.get(3)?;
        Ok(PyramidAnnotation {
            id: row.get(0)?,
            slug: row.get(1)?,
            node_id: row.get(2)?,
            annotation_type: AnnotationType::from_str(&at_str),
            content: row.get(4)?,
            question_context: row.get(5)?,
            author: row.get(6)?,
            created_at: row.get(7)?,
        })
    })?;

    let mut annotations = Vec::new();
    for row in rows {
        annotations.push(row?);
    }
    Ok(annotations)
}

/// Save an annotation. Returns the new row ID.
pub fn save_annotation(
    conn: &Connection,
    annotation: &PyramidAnnotation,
) -> Result<PyramidAnnotation> {
    conn.execute(
        "INSERT INTO pyramid_annotations (slug, node_id, annotation_type, content, question_context, author)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            annotation.slug,
            annotation.node_id,
            annotation.annotation_type.as_str(),
            annotation.content,
            annotation.question_context,
            annotation.author,
        ],
    )?;
    let id = conn.last_insert_rowid();
    // Re-read to get server-populated created_at
    let saved = conn.query_row(
        "SELECT id, slug, node_id, annotation_type, content, question_context, author, created_at FROM pyramid_annotations WHERE id = ?1",
        rusqlite::params![id],
        |row| {
            Ok(PyramidAnnotation {
                id: row.get(0)?,
                slug: row.get(1)?,
                node_id: row.get(2)?,
                annotation_type: AnnotationType::from_str(row.get::<_, String>(3)?.as_str()),
                content: row.get(4)?,
                question_context: row.get(5)?,
                author: row.get(6)?,
                created_at: row.get(7)?,
            })
        },
    )?;
    Ok(saved)
}

// ── FAQ CRUD ─────────────────────────────────────────────────────────────────

/// Upsert a FAQ node by id.
pub fn save_faq_node(conn: &Connection, faq: &FaqNode) -> Result<()> {
    let related_json = serde_json::to_string(&faq.related_node_ids)?;
    let annotation_json = serde_json::to_string(&faq.annotation_ids)?;
    let triggers_json = serde_json::to_string(&faq.match_triggers)?;
    conn.execute(
        "INSERT INTO pyramid_faq_nodes (id, slug, question, answer, related_node_ids, annotation_ids, hit_count, match_triggers, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(id) DO UPDATE SET
            question = excluded.question,
            answer = excluded.answer,
            related_node_ids = excluded.related_node_ids,
            annotation_ids = excluded.annotation_ids,
            hit_count = excluded.hit_count,
            match_triggers = excluded.match_triggers,
            updated_at = datetime('now')",
        rusqlite::params![
            faq.id,
            faq.slug,
            faq.question,
            faq.answer,
            related_json,
            annotation_json,
            faq.hit_count,
            triggers_json,
            faq.created_at,
            faq.updated_at,
        ],
    )?;
    Ok(())
}

/// Get all FAQ nodes for a slug.
pub fn get_faq_nodes(conn: &Connection, slug: &str) -> Result<Vec<FaqNode>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, question, answer, related_node_ids, annotation_ids, hit_count, match_triggers, created_at, updated_at
         FROM pyramid_faq_nodes WHERE slug = ?1
         ORDER BY hit_count DESC, updated_at DESC",
    )?;

    let rows = stmt.query_map(rusqlite::params![slug], |row| {
        let related_str: String = row.get(4)?;
        let annotation_str: String = row.get(5)?;
        let triggers_str: String = row.get::<_, String>(7).unwrap_or_else(|_| "[]".to_string());
        Ok(FaqNode {
            id: row.get(0)?,
            slug: row.get(1)?,
            question: row.get(2)?,
            answer: row.get(3)?,
            related_node_ids: serde_json::from_str(&related_str).unwrap_or_default(),
            annotation_ids: serde_json::from_str(&annotation_str).unwrap_or_default(),
            hit_count: row.get(6)?,
            match_triggers: serde_json::from_str(&triggers_str).unwrap_or_default(),
            created_at: row.get(8)?,
            updated_at: row.get(9)?,
        })
    })?;

    let mut faqs = Vec::new();
    for row in rows {
        faqs.push(row?);
    }
    Ok(faqs)
}

/// Get a single FAQ node by id.
pub fn get_faq_node(conn: &Connection, id: &str) -> Result<Option<FaqNode>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, question, answer, related_node_ids, annotation_ids, hit_count, match_triggers, created_at, updated_at
         FROM pyramid_faq_nodes WHERE id = ?1",
    )?;

    let mut rows = stmt.query_map(rusqlite::params![id], |row| {
        let related_str: String = row.get(4)?;
        let annotation_str: String = row.get(5)?;
        let triggers_str: String = row.get::<_, String>(7).unwrap_or_else(|_| "[]".to_string());
        Ok(FaqNode {
            id: row.get(0)?,
            slug: row.get(1)?,
            question: row.get(2)?,
            answer: row.get(3)?,
            related_node_ids: serde_json::from_str(&related_str).unwrap_or_default(),
            annotation_ids: serde_json::from_str(&annotation_str).unwrap_or_default(),
            hit_count: row.get(6)?,
            match_triggers: serde_json::from_str(&triggers_str).unwrap_or_default(),
            created_at: row.get(8)?,
            updated_at: row.get(9)?,
        })
    })?;

    match rows.next() {
        Some(Ok(faq)) => Ok(Some(faq)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

/// Increment the hit_count on a FAQ node.
pub fn increment_faq_hit(conn: &Connection, id: &str) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_faq_nodes SET hit_count = hit_count + 1, updated_at = datetime('now') WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

/// Delete a single FAQ node by id.
pub fn delete_faq_node(conn: &Connection, id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM pyramid_faq_nodes WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

// ── FAQ Category CRUD ────────────────────────────────────────────────────────

use super::types::FaqCategory;

/// Save (upsert) a FAQ category.
pub fn save_faq_category(conn: &Connection, cat: &FaqCategory) -> Result<()> {
    let faq_ids_json = serde_json::to_string(&cat.faq_ids).unwrap_or_else(|_| "[]".to_string());
    conn.execute(
        "INSERT INTO pyramid_faq_categories (id, slug, name, distilled_summary, faq_ids, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(id) DO UPDATE SET
            name = excluded.name,
            distilled_summary = excluded.distilled_summary,
            faq_ids = excluded.faq_ids,
            updated_at = excluded.updated_at",
        rusqlite::params![
            cat.id, cat.slug, cat.name, cat.distilled_summary, faq_ids_json,
            cat.created_at, cat.updated_at
        ],
    )?;
    Ok(())
}

/// Get all FAQ categories for a slug.
pub fn get_faq_categories(conn: &Connection, slug: &str) -> Result<Vec<FaqCategory>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, name, distilled_summary, faq_ids, created_at, updated_at
         FROM pyramid_faq_categories WHERE slug = ?1 ORDER BY name ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![slug], |row| {
        let faq_ids_str: String = row.get(4)?;
        let faq_ids: Vec<String> = serde_json::from_str(&faq_ids_str).unwrap_or_default();
        Ok(FaqCategory {
            id: row.get(0)?,
            slug: row.get(1)?,
            name: row.get(2)?,
            distilled_summary: row.get(3)?,
            faq_ids,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
        })
    })?;
    let mut result = Vec::new();
    for r in rows {
        result.push(r?);
    }
    Ok(result)
}

/// Get a single FAQ category by id.
pub fn get_faq_category(conn: &Connection, id: &str) -> Result<Option<FaqCategory>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, name, distilled_summary, faq_ids, created_at, updated_at
         FROM pyramid_faq_categories WHERE id = ?1",
    )?;
    let mut rows = stmt.query_map(rusqlite::params![id], |row| {
        let faq_ids_str: String = row.get(4)?;
        let faq_ids: Vec<String> = serde_json::from_str(&faq_ids_str).unwrap_or_default();
        Ok(FaqCategory {
            id: row.get(0)?,
            slug: row.get(1)?,
            name: row.get(2)?,
            distilled_summary: row.get(3)?,
            faq_ids,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
        })
    })?;
    match rows.next() {
        Some(Ok(cat)) => Ok(Some(cat)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

/// Delete all FAQ categories for a slug (used before regenerating).
pub fn delete_faq_categories(conn: &Connection, slug: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM pyramid_faq_categories WHERE slug = ?1",
        rusqlite::params![slug],
    )?;
    Ok(())
}

// ── Usage Log CRUD ───────────────────────────────────────────────────────────

/// Insert a usage log entry. Returns the auto-generated id.
pub fn log_usage(conn: &Connection, entry: &UsageLogEntry) -> Result<i64> {
    conn.execute(
        "INSERT INTO pyramid_usage_log (slug, query_type, query_params, result_node_ids, agent_id)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            entry.slug,
            entry.query_type,
            entry.query_params,
            entry.result_node_ids,
            entry.agent_id,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Get the most recent N usage log entries for a slug.
pub fn get_usage_log(conn: &Connection, slug: &str, limit: i64) -> Result<Vec<UsageLogEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, query_type, query_params, result_node_ids, agent_id, created_at
         FROM pyramid_usage_log WHERE slug = ?1
         ORDER BY created_at DESC LIMIT ?2",
    )?;

    let rows = stmt.query_map(rusqlite::params![slug, limit], |row| {
        Ok(UsageLogEntry {
            id: row.get(0)?,
            slug: row.get(1)?,
            query_type: row.get(2)?,
            query_params: row.get(3)?,
            result_node_ids: row.get(4)?,
            agent_id: row.get(5)?,
            created_at: row.get(6)?,
        })
    })?;

    let mut entries = Vec::new();
    for row in rows {
        entries.push(row?);
    }
    Ok(entries)
}

/// Get usage counts grouped by query_type for a slug.
pub fn get_usage_stats(conn: &Connection, slug: &str) -> Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT query_type, COUNT(*) as cnt
         FROM pyramid_usage_log WHERE slug = ?1
         GROUP BY query_type ORDER BY cnt DESC",
    )?;

    let rows = stmt.query_map(rusqlite::params![slug], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;

    let mut stats = Vec::new();
    for row in rows {
        stats.push(row?);
    }
    Ok(stats)
}

/// Get the most accessed node IDs for a slug, ranked by access count.
pub fn get_most_accessed_nodes(
    conn: &Connection,
    slug: &str,
    limit: i64,
) -> Result<Vec<(String, i64)>> {
    // result_node_ids is a JSON array, so we use json_each to unnest
    let mut stmt = conn.prepare(
        "SELECT j.value as node_id, COUNT(*) as cnt
         FROM pyramid_usage_log u, json_each(u.result_node_ids) j
         WHERE u.slug = ?1
         GROUP BY j.value ORDER BY cnt DESC LIMIT ?2",
    )?;

    let rows = stmt.query_map(rusqlite::params![slug, limit], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;

    let mut nodes = Vec::new();
    for row in rows {
        nodes.push(row?);
    }
    Ok(nodes)
}

// ── Watcher Cache Helpers ─────────────────────────────────────────────────────

/// Get all tracked file paths for a slug from pyramid_file_hashes.
pub fn get_tracked_paths(
    conn: &Connection,
    slug: &str,
) -> Result<std::collections::HashSet<String>> {
    let mut stmt = conn.prepare("SELECT file_path FROM pyramid_file_hashes WHERE slug = ?1")?;
    let rows = stmt.query_map(rusqlite::params![slug], |row| row.get::<_, String>(0))?;
    let mut paths = std::collections::HashSet::new();
    for row in rows {
        if let Ok(p) = row {
            paths.insert(p);
        }
    }
    Ok(paths)
}

/// Get ingested extensions for a slug from pyramid_auto_update_config.
/// Returns empty Vec if no config exists or column is missing.
pub fn get_ingested_extensions(conn: &Connection, slug: &str) -> Result<Vec<String>> {
    let json_str: String = conn
        .query_row(
            "SELECT ingested_extensions FROM pyramid_auto_update_config WHERE slug = ?1",
            rusqlite::params![slug],
            |row| row.get(0),
        )
        .unwrap_or_else(|_| "[]".to_string());
    let exts: Vec<String> = serde_json::from_str(&json_str).unwrap_or_default();
    Ok(exts)
}

/// Get ingested config filenames for a slug from pyramid_auto_update_config.
/// Returns empty Vec if no config exists or column is missing.
pub fn get_ingested_config_files(conn: &Connection, slug: &str) -> Result<Vec<String>> {
    let json_str: String = conn
        .query_row(
            "SELECT ingested_config_files FROM pyramid_auto_update_config WHERE slug = ?1",
            rusqlite::params![slug],
            |row| row.get(0),
        )
        .unwrap_or_else(|_| "[]".to_string());
    let configs: Vec<String> = serde_json::from_str(&json_str).unwrap_or_default();
    Ok(configs)
}

// ── Build Pipeline Seeding Helpers ───────────────────────────────────────────

/// Insert default auto_update_config for a slug with ingested extensions and config files.
/// Uses INSERT OR IGNORE so it won't overwrite an existing config.
pub fn insert_auto_update_config_defaults(
    conn: &Connection,
    slug: &str,
    extensions_json: &str,
    config_files_json: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO pyramid_auto_update_config
         (slug, auto_update, debounce_minutes, min_changed_files, runaway_threshold,
          breaker_tripped, frozen, ingested_extensions, ingested_config_files)
         VALUES (?1, 1, 5, 1, 0.5, 0, 0, ?2, ?3)",
        rusqlite::params![slug, extensions_json, config_files_json],
    )?;
    Ok(())
}

/// Upsert a file hash into pyramid_file_hashes.
pub fn upsert_file_hash(
    conn: &Connection,
    slug: &str,
    file_path: &str,
    hash: &str,
    chunk_count: i32,
    node_ids_json: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_file_hashes (slug, file_path, hash, chunk_count, node_ids, last_ingested_at)
         VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))
         ON CONFLICT(slug, file_path) DO UPDATE SET
            hash = excluded.hash,
            chunk_count = excluded.chunk_count,
            node_ids = excluded.node_ids,
            last_ingested_at = excluded.last_ingested_at",
        rusqlite::params![slug, file_path, hash, chunk_count, node_ids_json],
    )?;
    Ok(())
}

// ── Shared Query Functions (used by both HTTP routes and Tauri IPC commands) ──

/// Load auto-update config for a slug. Returns None if not found.
pub fn get_auto_update_config(
    conn: &Connection,
    slug: &str,
) -> Option<super::types::AutoUpdateConfig> {
    conn.query_row(
        "SELECT slug, auto_update, debounce_minutes, min_changed_files,
                runaway_threshold, breaker_tripped, breaker_tripped_at, frozen, frozen_at
         FROM pyramid_auto_update_config WHERE slug = ?1",
        rusqlite::params![slug],
        |row| {
            Ok(super::types::AutoUpdateConfig {
                slug: row.get(0)?,
                auto_update: row.get::<_, i32>(1)? != 0,
                debounce_minutes: row.get(2)?,
                min_changed_files: row.get(3)?,
                runaway_threshold: row.get(4)?,
                breaker_tripped: row.get::<_, i32>(5)? != 0,
                breaker_tripped_at: row.get(6)?,
                frozen: row.get::<_, i32>(7)? != 0,
                frozen_at: row.get(8)?,
            })
        },
    )
    .ok()
}

/// Get auto-update status for a slug (config + pending mutations + last check time).
pub fn get_auto_update_status(conn: &Connection, slug: &str) -> Result<Option<serde_json::Value>> {
    let config = match get_auto_update_config(conn, slug) {
        Some(c) => c,
        None => return Ok(None),
    };

    let mut pending_by_layer = std::collections::HashMap::new();
    for layer in 0..=3 {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_pending_mutations
                 WHERE processed = 0 AND slug = ?1 AND layer = ?2",
                rusqlite::params![slug, layer],
                |row| row.get(0),
            )
            .unwrap_or(0);
        pending_by_layer.insert(layer, count);
    }

    let last_check_at: Option<String> = conn
        .query_row(
            "SELECT MAX(checked_at) FROM pyramid_stale_check_log WHERE slug = ?1",
            rusqlite::params![slug],
            |row| row.get(0),
        )
        .ok()
        .flatten();

    Ok(Some(serde_json::json!({
        "auto_update": config.auto_update,
        "frozen": config.frozen,
        "breaker_tripped": config.breaker_tripped,
        "pending_mutations_by_layer": pending_by_layer,
        "last_check_at": last_check_at,
    })))
}

/// Query stale check log entries.
pub fn get_stale_log(
    conn: &Connection,
    slug: &str,
    layer: Option<i32>,
    stale: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Vec<serde_json::Value>> {
    let mut sql = String::from(
        "SELECT id, slug, batch_id, layer, target_id, stale, reason,
                checker_index, checker_batch_size, checked_at, cost_tokens, cost_usd
         FROM pyramid_stale_check_log WHERE slug = ?1",
    );
    let mut param_vals: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    param_vals.push(Box::new(slug.to_string()));

    if let Some(layer_val) = layer {
        param_vals.push(Box::new(layer_val));
        sql.push_str(&format!(" AND layer = ?{}", param_vals.len()));
    }
    if let Some(stale_str) = stale {
        let stale_val: i32 = if stale_str == "yes" || stale_str == "true" || stale_str == "1" {
            1
        } else {
            0
        };
        param_vals.push(Box::new(stale_val));
        sql.push_str(&format!(" AND stale = ?{}", param_vals.len()));
    }

    param_vals.push(Box::new(limit));
    sql.push_str(&format!(
        " ORDER BY checked_at DESC LIMIT ?{}",
        param_vals.len()
    ));
    param_vals.push(Box::new(offset));
    sql.push_str(&format!(" OFFSET ?{}", param_vals.len()));

    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        param_vals.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;

    let rows: Vec<serde_json::Value> = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, i64>(0)?,
                "slug": row.get::<_, String>(1)?,
                "batch_id": row.get::<_, String>(2)?,
                "layer": row.get::<_, i32>(3)?,
                "target_id": row.get::<_, String>(4)?,
                "stale": if row.get::<_, i32>(5)? == 1 { "yes" } else { "no" },
                "reason": row.get::<_, String>(6)?,
                "checker_index": row.get::<_, i32>(7)?,
                "checker_batch_size": row.get::<_, i32>(8)?,
                "checked_at": row.get::<_, String>(9)?,
                "cost_tokens": row.get::<_, Option<i64>>(10)?,
                "cost_usd": row.get::<_, Option<f64>>(11)?,
            }))
        })
        .map(|iter| iter.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();

    Ok(rows)
}

/// Get cost summary for a slug within an optional time window.
/// Note: The actual column in pyramid_cost_log is `estimated_cost`, not `cost_usd`.
pub fn get_cost_summary(
    conn: &Connection,
    slug: &str,
    window: Option<&str>,
) -> Result<serde_json::Value> {
    let window_clause = match window {
        Some("24h") => "AND created_at >= datetime('now', '-1 day')",
        Some("7d") => "AND created_at >= datetime('now', '-7 days')",
        Some("30d") => "AND created_at >= datetime('now', '-30 days')",
        _ => "",
    };

    let (total_spend, total_calls): (f64, i64) = conn
        .query_row(
            &format!(
                "SELECT COALESCE(SUM(estimated_cost), 0.0), COUNT(*) FROM pyramid_cost_log
                 WHERE slug = ?1 {}",
                window_clause
            ),
            rusqlite::params![slug],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap_or((0.0, 0));

    let by_source = {
        let mut stmt = conn.prepare(&format!(
            "SELECT COALESCE(source, 'manual'), COALESCE(SUM(estimated_cost), 0.0), COUNT(*)
             FROM pyramid_cost_log WHERE slug = ?1 {}
             GROUP BY COALESCE(source, 'manual')",
            window_clause
        ))?;
        let rows: Vec<serde_json::Value> = stmt
            .query_map(rusqlite::params![slug], |row| {
                Ok(serde_json::json!({
                    "source": row.get::<_, String>(0)?,
                    "spend": row.get::<_, f64>(1)?,
                    "calls": row.get::<_, i64>(2)?,
                }))
            })
            .map(|iter| iter.filter_map(|r| r.ok()).collect())
            .unwrap_or_default();
        rows
    };

    let by_check_type = {
        let mut stmt = conn.prepare(&format!(
            "SELECT COALESCE(check_type, 'unknown'), COALESCE(SUM(estimated_cost), 0.0), COUNT(*)
             FROM pyramid_cost_log WHERE slug = ?1 {}
             GROUP BY COALESCE(check_type, 'unknown')",
            window_clause
        ))?;
        let rows: Vec<serde_json::Value> = stmt
            .query_map(rusqlite::params![slug], |row| {
                Ok(serde_json::json!({
                    "check_type": row.get::<_, String>(0)?,
                    "spend": row.get::<_, f64>(1)?,
                    "calls": row.get::<_, i64>(2)?,
                }))
            })
            .map(|iter| iter.filter_map(|r| r.ok()).collect())
            .unwrap_or_default();
        rows
    };

    let by_layer = {
        let mut stmt = conn.prepare(&format!(
            "SELECT COALESCE(layer, -1), COALESCE(SUM(estimated_cost), 0.0), COUNT(*)
             FROM pyramid_cost_log WHERE slug = ?1 {}
             GROUP BY COALESCE(layer, -1)",
            window_clause
        ))?;
        let rows: Vec<serde_json::Value> = stmt
            .query_map(rusqlite::params![slug], |row| {
                Ok(serde_json::json!({
                    "layer": row.get::<_, i32>(0)?,
                    "spend": row.get::<_, f64>(1)?,
                    "calls": row.get::<_, i64>(2)?,
                }))
            })
            .map(|iter| iter.filter_map(|r| r.ok()).collect())
            .unwrap_or_default();
        rows
    };

    let recent_calls = {
        let mut stmt = conn.prepare(&format!(
            "SELECT id, operation, model, input_tokens, output_tokens, estimated_cost,
                    COALESCE(source, 'manual'), layer, check_type, created_at
             FROM pyramid_cost_log WHERE slug = ?1 {}
             ORDER BY created_at DESC LIMIT 20",
            window_clause
        ))?;
        let rows: Vec<serde_json::Value> = stmt
            .query_map(rusqlite::params![slug], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, i64>(0)?,
                    "operation": row.get::<_, String>(1)?,
                    "model": row.get::<_, String>(2)?,
                    "input_tokens": row.get::<_, i64>(3)?,
                    "output_tokens": row.get::<_, i64>(4)?,
                    "cost_usd": row.get::<_, f64>(5)?,
                    "source": row.get::<_, String>(6)?,
                    "layer": row.get::<_, Option<i32>>(7)?,
                    "check_type": row.get::<_, Option<String>>(8)?,
                    "created_at": row.get::<_, String>(9)?,
                }))
            })
            .map(|iter| iter.filter_map(|r| r.ok()).collect())
            .unwrap_or_default();
        rows
    };

    Ok(serde_json::json!({
        "slug": slug,
        "total_spend": total_spend,
        "total_calls": total_calls,
        "by_source": by_source,
        "by_check_type": by_check_type,
        "by_layer": by_layer,
        "recent_calls": recent_calls,
    }))
}

// ── Vine DB Helpers ──────────────────────────────────────────────────────────

/// List all vine bunches for a given vine slug.
pub fn list_vine_bunches(conn: &Connection, vine_slug: &str) -> Result<Vec<VineBunch>> {
    let mut stmt = conn.prepare(
        "SELECT id, vine_slug, bunch_slug, session_id, jsonl_path, bunch_index,
                first_ts, last_ts, message_count, chunk_count, apex_node_id,
                penultimate_node_ids, status, metadata, created_at, updated_at
         FROM vine_bunches WHERE vine_slug = ?1 ORDER BY bunch_index ASC",
    )?;

    let rows = stmt.query_map(rusqlite::params![vine_slug], |row| {
        let pen_json: String = row
            .get::<_, String>(11)
            .unwrap_or_else(|_| "[]".to_string());
        let pen_ids: Vec<String> = serde_json::from_str(&pen_json).unwrap_or_default();
        let meta_json: Option<String> = row.get(13).ok();
        let metadata: Option<VineBunchMetadata> =
            meta_json.and_then(|s| serde_json::from_str(&s).ok());

        Ok(VineBunch {
            id: row.get(0)?,
            vine_slug: row.get(1)?,
            bunch_slug: row.get(2)?,
            session_id: row.get(3)?,
            jsonl_path: row.get(4)?,
            bunch_index: row.get(5)?,
            first_ts: row.get(6)?,
            last_ts: row.get(7)?,
            message_count: row.get(8)?,
            chunk_count: row.get(9)?,
            apex_node_id: row.get(10)?,
            penultimate_node_ids: pen_ids,
            status: row.get(12)?,
            metadata,
        })
    })?;

    let bunches: Vec<VineBunch> = rows.filter_map(|r| r.ok()).collect();
    Ok(bunches)
}

/// Get all annotations of a specific type for a slug.
pub fn get_annotations_by_type(
    conn: &Connection,
    slug: &str,
    annotation_type: &str,
) -> Result<Vec<PyramidAnnotation>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, node_id, annotation_type, content, question_context, author, created_at
         FROM pyramid_annotations WHERE slug = ?1 AND annotation_type = ?2
         ORDER BY created_at DESC",
    )?;

    let rows = stmt.query_map(rusqlite::params![slug, annotation_type], |row| {
        let at_str: String = row.get(3)?;
        Ok(PyramidAnnotation {
            id: row.get(0)?,
            slug: row.get(1)?,
            node_id: row.get(2)?,
            annotation_type: serde_json::from_value(serde_json::Value::String(at_str.clone()))
                .unwrap_or(AnnotationType::Observation),
            content: row.get(4)?,
            question_context: row.get(5)?,
            author: row.get(6)?,
            created_at: row.get(7)?,
        })
    })?;

    let annotations: Vec<PyramidAnnotation> = rows.filter_map(|r| r.ok()).collect();
    Ok(annotations)
}

/// Get FAQ nodes filtered by ID prefix for a given slug.
pub fn get_faq_nodes_by_prefix(
    conn: &Connection,
    slug: &str,
    id_prefix: &str,
) -> Result<Vec<FaqNode>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, question, answer, related_node_ids, annotation_ids, hit_count, match_triggers, created_at, updated_at
         FROM pyramid_faq_nodes WHERE slug = ?1 AND id LIKE ?2
         ORDER BY hit_count DESC, updated_at DESC",
    )?;

    let like_pattern = format!("{}%", id_prefix);
    let rows = stmt.query_map(rusqlite::params![slug, like_pattern], |row| {
        let related_str: String = row.get(4)?;
        let annotation_str: String = row.get(5)?;
        let triggers_str: String = row.get::<_, String>(7).unwrap_or_else(|_| "[]".to_string());
        Ok(FaqNode {
            id: row.get(0)?,
            slug: row.get(1)?,
            question: row.get(2)?,
            answer: row.get(3)?,
            related_node_ids: serde_json::from_str(&related_str).unwrap_or_default(),
            annotation_ids: serde_json::from_str(&annotation_str).unwrap_or_default(),
            hit_count: row.get(6)?,
            match_triggers: serde_json::from_str(&triggers_str).unwrap_or_default(),
            created_at: row.get(8)?,
            updated_at: row.get(9)?,
        })
    })?;

    let faqs: Vec<FaqNode> = rows.filter_map(|r| r.ok()).collect();
    Ok(faqs)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        conn
    }

    #[test]
    fn test_init_creates_tables() {
        let conn = test_conn();
        // Verify tables exist by querying them
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM pyramid_slugs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_slug_crud() {
        let conn = test_conn();

        // Create
        let info = create_slug(&conn, "test-slug", &ContentType::Code, "/src").unwrap();
        assert_eq!(info.slug, "test-slug");
        assert_eq!(info.node_count, 0);

        // Get
        let got = get_slug(&conn, "test-slug").unwrap().unwrap();
        assert_eq!(got.source_path, "/src");

        // List
        let all = list_slugs(&conn).unwrap();
        assert_eq!(all.len(), 1);

        // Delete
        delete_slug(&conn, "test-slug").unwrap();
        assert!(get_slug(&conn, "test-slug").unwrap().is_none());
    }

    #[test]
    fn test_chunk_crud() {
        let conn = test_conn();
        create_slug(&conn, "s", &ContentType::Conversation, "").unwrap();
        let batch_id = create_batch(&conn, "s", "initial", "/path", 0).unwrap();

        insert_chunk(&conn, "s", batch_id, 0, "hello world\nsecond line").unwrap();
        insert_chunk(&conn, "s", batch_id, 1, "chunk two").unwrap();

        assert_eq!(count_chunks(&conn, "s").unwrap(), 2);

        let content = get_chunk(&conn, "s", 0).unwrap().unwrap();
        assert!(content.contains("hello world"));

        assert!(get_chunk(&conn, "s", 99).unwrap().is_none());
    }

    #[test]
    fn test_node_save_and_get() {
        let conn = test_conn();
        create_slug(&conn, "s", &ContentType::Code, "").unwrap();

        let node = PyramidNode {
            id: "n1".to_string(),
            slug: "s".to_string(),
            depth: 0,
            chunk_index: Some(0),
            headline: "Auth Node".to_string(),
            distilled: "Test distillation".to_string(),
            topics: vec![Topic {
                name: "Auth".to_string(),
                current: "JWT-based auth".to_string(),
                entities: vec!["AuthState".to_string()],
                corrections: vec![],
                decisions: vec![Decision {
                    decided: "Use JWT".to_string(),
                    why: "Standard".to_string(),
                    rejected: String::new(),
                }],
            }],
            corrections: vec![],
            decisions: vec![],
            terms: vec![Term {
                term: "JWT".to_string(),
                definition: "JSON Web Token".to_string(),
            }],
            dead_ends: vec!["OAuth considered".to_string()],
            self_prompt: "What auth mechanism?".to_string(),
            children: vec!["c1".to_string(), "c2".to_string()],
            parent_id: None,
            superseded_by: None,
            created_at: String::new(),
        };

        save_node(&conn, &node, None).unwrap();

        let got = get_node(&conn, "s", "n1").unwrap().unwrap();
        assert_eq!(got.distilled, "Test distillation");
        assert_eq!(got.topics.len(), 1);
        assert_eq!(got.topics[0].name, "Auth");
        assert_eq!(got.topics[0].decisions.len(), 1);
        assert_eq!(got.terms.len(), 1);
        assert_eq!(got.dead_ends.len(), 1);
        assert_eq!(got.children.len(), 2);
    }

    #[test]
    fn test_nodes_at_depth() {
        let conn = test_conn();
        create_slug(&conn, "s", &ContentType::Code, "").unwrap();

        for i in 0..3 {
            let node = PyramidNode {
                id: format!("d0-{i}"),
                slug: "s".to_string(),
                depth: 0,
                chunk_index: Some(i),
                headline: format!("Node {i}"),
                distilled: format!("Node {i}"),
                topics: vec![],
                corrections: vec![],
                decisions: vec![],
                terms: vec![],
                dead_ends: vec![],
                self_prompt: String::new(),
                children: vec![],
                parent_id: None,
                superseded_by: None,
                created_at: String::new(),
            };
            save_node(&conn, &node, None).unwrap();
        }

        let depth0 = get_nodes_at_depth(&conn, "s", 0).unwrap();
        assert_eq!(depth0.len(), 3);

        let depth1 = get_nodes_at_depth(&conn, "s", 1).unwrap();
        assert_eq!(depth1.len(), 0);

        assert_eq!(count_nodes_at_depth(&conn, "s", 0).unwrap(), 3);
    }

    #[test]
    fn test_delete_nodes_above() {
        let conn = test_conn();
        create_slug(&conn, "s", &ContentType::Code, "").unwrap();

        for depth in 0..4 {
            let node = PyramidNode {
                id: format!("d{depth}"),
                slug: "s".to_string(),
                depth,
                chunk_index: None,
                headline: format!("Depth {depth}"),
                distilled: format!("Depth {depth}"),
                topics: vec![],
                corrections: vec![],
                decisions: vec![],
                terms: vec![],
                dead_ends: vec![],
                self_prompt: String::new(),
                children: vec![],
                parent_id: None,
                superseded_by: None,
                created_at: String::new(),
            };
            save_node(&conn, &node, None).unwrap();
        }

        let deleted = delete_nodes_above(&conn, "s", 1).unwrap();
        assert_eq!(deleted, 2); // depth 2 and 3

        // depth 0 and 1 remain
        assert_eq!(count_nodes_at_depth(&conn, "s", 0).unwrap(), 1);
        assert_eq!(count_nodes_at_depth(&conn, "s", 1).unwrap(), 1);
        assert_eq!(count_nodes_at_depth(&conn, "s", 2).unwrap(), 0);
    }

    #[test]
    fn test_pipeline_steps() {
        let conn = test_conn();
        create_slug(&conn, "s", &ContentType::Code, "").unwrap();

        assert!(!step_exists(&conn, "s", "extract", 0, 0, "").unwrap());

        save_step(
            &conn,
            "s",
            "extract",
            0,
            0,
            "",
            r#"{"ok":true}"#,
            "gpt-4",
            1.5,
        )
        .unwrap();
        assert!(step_exists(&conn, "s", "extract", 0, 0, "").unwrap());

        let output = get_step_output(&conn, "s", "extract", 0).unwrap().unwrap();
        assert!(output.contains("ok"));

        // Upsert overwrites
        save_step(
            &conn,
            "s",
            "extract",
            0,
            0,
            "",
            r#"{"ok":false}"#,
            "gpt-4",
            2.0,
        )
        .unwrap();
        let output2 = get_step_output(&conn, "s", "extract", 0).unwrap().unwrap();
        assert!(output2.contains("false"));

        // Delete steps
        delete_steps(&conn, "s", "extract").unwrap();
        assert!(!step_exists(&conn, "s", "extract", 0, 0, "").unwrap());
    }

    #[test]
    fn test_update_slug_stats() {
        let conn = test_conn();
        create_slug(&conn, "s", &ContentType::Code, "").unwrap();

        // Add nodes at two depths
        for i in 0..3 {
            let node = PyramidNode {
                id: format!("d0-{i}"),
                slug: "s".to_string(),
                depth: 0,
                chunk_index: Some(i),
                headline: format!("Node {i}"),
                distilled: String::new(),
                topics: vec![],
                corrections: vec![],
                decisions: vec![],
                terms: vec![],
                dead_ends: vec![],
                self_prompt: String::new(),
                children: vec![],
                parent_id: None,
                superseded_by: None,
                created_at: String::new(),
            };
            save_node(&conn, &node, None).unwrap();
        }
        let apex = PyramidNode {
            id: "apex".to_string(),
            slug: "s".to_string(),
            depth: 1,
            chunk_index: None,
            headline: "Apex".to_string(),
            distilled: String::new(),
            topics: vec![],
            corrections: vec![],
            decisions: vec![],
            terms: vec![],
            dead_ends: vec![],
            self_prompt: String::new(),
            children: vec!["d0-0".into(), "d0-1".into(), "d0-2".into()],
            parent_id: None,
            superseded_by: None,
            created_at: String::new(),
        };
        save_node(&conn, &apex, None).unwrap();

        update_slug_stats(&conn, "s").unwrap();

        let info = get_slug(&conn, "s").unwrap().unwrap();
        assert_eq!(info.node_count, 4);
        assert_eq!(info.max_depth, 1);
        assert!(info.last_built_at.is_some());
    }

    #[test]
    fn test_node_upsert() {
        let conn = test_conn();
        create_slug(&conn, "s", &ContentType::Code, "").unwrap();

        let mut node = PyramidNode {
            id: "n1".to_string(),
            slug: "s".to_string(),
            depth: 0,
            chunk_index: Some(0),
            headline: "Versioned Node".to_string(),
            distilled: "Version 1".to_string(),
            topics: vec![],
            corrections: vec![],
            decisions: vec![],
            terms: vec![],
            dead_ends: vec![],
            self_prompt: String::new(),
            children: vec![],
            parent_id: None,
            superseded_by: None,
            created_at: String::new(),
        };
        save_node(&conn, &node, None).unwrap();

        // Upsert with new content
        node.distilled = "Version 2".to_string();
        save_node(&conn, &node, None).unwrap();

        let got = get_node(&conn, "s", "n1").unwrap().unwrap();
        assert_eq!(got.distilled, "Version 2");

        // Should still be 1 node, not 2
        assert_eq!(count_nodes_at_depth(&conn, "s", 0).unwrap(), 1);
    }
}
