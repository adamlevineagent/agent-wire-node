// pyramid/db.rs — SQLite schema, migrations, and CRUD operations for the Knowledge Pyramid
//
// Tables: pyramid_slugs, pyramid_batches, pyramid_nodes, pyramid_chunks, pyramid_pipeline_steps
// All JSON columns (topics, corrections, decisions, terms, dead_ends, children) are stored as
// JSON strings and parsed/serialized via serde_json on read/write.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
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

/// Open a pyramid DB connection with WAL, FK pragmas, and busy_timeout.
///
/// Unlike `open_pyramid_db`, this does NOT run schema initialization — it only
/// sets connection pragmas. Use this for stale engine and helper code where the
/// DB is already initialized at startup.
pub fn open_pyramid_connection(path: &std::path::Path) -> Result<Connection> {
    let conn = Connection::open(path)
        .with_context(|| format!("Failed to open pyramid connection at {}", path.display()))?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=10000;",
    )?;
    Ok(conn)
}

// ── Schema Initialization ────────────────────────────────────────────────────

/// Initialize pyramid tables. Call on app startup.
///
/// Enables WAL mode and foreign keys, then creates all five tables with
/// proper constraints and indices.
pub fn init_pyramid_db(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=10000;")?;

    // 11-R: CASCADE DELETEs on FK constraints below only fire when a slug row is
    // physically DELETEd, which only happens via admin-only `purge_slug`.
    // Normal workflow uses `archive_slug` (sets archived_at), which never triggers cascades.
    // The cascades are intentional for purge: removing a slug should clean up all its data.
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_slugs (
            slug TEXT PRIMARY KEY,
            content_type TEXT NOT NULL CHECK(content_type IN ('code', 'conversation', 'document', 'vine', 'question')),
            source_path TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            last_built_at TEXT,
            node_count INTEGER NOT NULL DEFAULT 0,
            max_depth INTEGER NOT NULL DEFAULT 0,
            archived_at TEXT DEFAULT NULL
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
        CREATE INDEX IF NOT EXISTS idx_web_edges_slug_a ON pyramid_web_edges(slug, thread_a_id);
        CREATE INDEX IF NOT EXISTS idx_web_edges_slug_b ON pyramid_web_edges(slug, thread_b_id);

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

        -- Annotation reactions (voting)
        CREATE TABLE IF NOT EXISTS pyramid_annotation_reactions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            annotation_id INTEGER NOT NULL REFERENCES pyramid_annotations(id) ON DELETE CASCADE,
            reaction TEXT NOT NULL CHECK(reaction IN ('up', 'down')),
            agent_id TEXT NOT NULL DEFAULT 'anonymous',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(annotation_id, agent_id)
        );

        -- Agent session tracking
        CREATE TABLE IF NOT EXISTS pyramid_agent_sessions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL,
            agent_id TEXT NOT NULL,
            started_at TEXT NOT NULL DEFAULT (datetime('now')),
            last_activity TEXT NOT NULL DEFAULT (datetime('now')),
            actions_count INTEGER NOT NULL DEFAULT 0,
            summary TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_agent_sessions_slug ON pyramid_agent_sessions(slug);

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

    // ── P1.5 cost observatory columns (nullable, old rows stay valid) ──
    let _ = conn.execute("ALTER TABLE pyramid_cost_log ADD COLUMN chain_id TEXT", []);
    let _ = conn.execute("ALTER TABLE pyramid_cost_log ADD COLUMN step_name TEXT", []);
    let _ = conn.execute("ALTER TABLE pyramid_cost_log ADD COLUMN tier TEXT", []);
    let _ = conn.execute(
        "ALTER TABLE pyramid_cost_log ADD COLUMN latency_ms INTEGER",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_cost_log ADD COLUMN generation_id TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_cost_log ADD COLUMN estimated_cost_usd REAL",
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

    // ── WS3: build_id columns for contribution model ──
    let _ = conn.execute(
        "ALTER TABLE pyramid_nodes ADD COLUMN build_id TEXT DEFAULT NULL",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_threads ADD COLUMN build_id TEXT DEFAULT NULL",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_pipeline_steps ADD COLUMN build_id TEXT DEFAULT NULL",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_distillations ADD COLUMN build_id TEXT DEFAULT NULL",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_deltas ADD COLUMN build_id TEXT DEFAULT NULL",
        [],
    );

    // ── WS4: build_id scoping for question decomposition tables ──
    let _ = conn.execute(
        "ALTER TABLE pyramid_question_nodes ADD COLUMN build_id TEXT DEFAULT NULL",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_question_tree ADD COLUMN build_id TEXT DEFAULT NULL",
        [],
    );

    // ── WS-SCHEMA-V2 (§15.2, §15.7): PyramidNode schema v2 + per-contribution
    //    supersession chain via an append-only pyramid_node_versions table.
    //
    //    All column additions are nullable-or-defaulted so existing pyramids
    //    load unchanged. Append NEW migrations BELOW this block so later
    //    Phase 1 workstreams (FTS5, DEADLETTER, COST-MODEL) can also append
    //    without conflict.
    let _ = conn.execute(
        "ALTER TABLE pyramid_nodes ADD COLUMN time_range_start TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_nodes ADD COLUMN time_range_end TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_nodes ADD COLUMN weight REAL DEFAULT 1.0",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_nodes ADD COLUMN provisional INTEGER DEFAULT 0",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_nodes ADD COLUMN promoted_from TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_nodes ADD COLUMN narrative_json TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_nodes ADD COLUMN entities_json TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_nodes ADD COLUMN key_quotes_json TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_nodes ADD COLUMN transitions_json TEXT",
        [],
    );
    // Per-contribution version chain pointer. Distinct from the legacy
    // `build_version` column (build-sweep counter). See §15.7 "build_version
    // vs current_version".
    let _ = conn.execute(
        "ALTER TABLE pyramid_nodes ADD COLUMN current_version INTEGER NOT NULL DEFAULT 1",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_nodes ADD COLUMN current_version_chain_phase TEXT",
        [],
    );

    let _ = conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_pyramid_nodes_provisional \
             ON pyramid_nodes(slug, provisional)",
        [],
    );
    let _ = conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_pyramid_nodes_time_range \
             ON pyramid_nodes(slug, time_range_start)",
        [],
    );

    // Append-only per-contribution version history. See §15.7. The FK is on
    // slug only — node-level FK would complicate the snapshot-then-update
    // transaction and isn't needed because we write via a SAVEPOINT.
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_node_versions (
            slug TEXT NOT NULL REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
            node_id TEXT NOT NULL,
            version INTEGER NOT NULL,
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
            time_range_start TEXT,
            time_range_end TEXT,
            weight REAL,
            narrative_json TEXT,
            entities_json TEXT,
            key_quotes_json TEXT,
            transitions_json TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            chain_phase TEXT,
            build_id TEXT,
            superseded_by_version INTEGER,
            supersession_reason TEXT,
            PRIMARY KEY (slug, node_id, version)
        );
        CREATE INDEX IF NOT EXISTS idx_node_versions_node
            ON pyramid_node_versions(slug, node_id, version DESC);
        CREATE INDEX IF NOT EXISTS idx_node_versions_build
            ON pyramid_node_versions(slug, build_id);
        ",
    )?;
    // ── end WS-SCHEMA-V2 migration block ──────────────────────────────────

    // ── Compensating DELETE triggers for FK CASCADE on existing DBs ──
    // (SQLite cannot ALTER FK constraints, so these triggers handle cascading
    //  deletes for tables created before CASCADE was added)
    // NOTE: fk_cascade_annotations_on_node_delete deliberately removed —
    // supersession replaces deletion, annotations survive on superseded nodes.
    let _ = conn.execute_batch(
        "
        DROP TRIGGER IF EXISTS fk_cascade_annotations_on_node_delete;

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

    // ── Chain registry table ─────────────────────────────────────────────────
    super::chain_registry::init_chain_tables(conn)?;

    // ── Event bus tables (P3.2) ──────────────────────────────────────────────
    super::event_chain::init_event_tables(conn)?;

    // ── Wire import tables (P4.2) ────────────────────────────────────────────
    super::wire_import::init_import_tables(conn)?;

    // ── Wire publication ID mapping table (P4.3) ──────────────────────────────
    super::wire_publish::init_id_map_table(conn)?;

    // ── Phase 1: Question Pyramid Evidence System tables ─────────────────────

    conn.execute_batch(
        "
        -- Many-to-many weighted evidence links between nodes
        CREATE TABLE IF NOT EXISTS pyramid_evidence (
            slug TEXT NOT NULL,
            build_id TEXT NOT NULL DEFAULT '',
            source_node_id TEXT NOT NULL,
            target_node_id TEXT NOT NULL,
            verdict TEXT NOT NULL CHECK(verdict IN ('KEEP', 'DISCONNECT', 'MISSING')),
            weight REAL,
            reason TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (slug, build_id, source_node_id, target_node_id)
        );
        CREATE INDEX IF NOT EXISTS idx_evidence_target ON pyramid_evidence(slug, target_node_id);
        CREATE INDEX IF NOT EXISTS idx_evidence_source ON pyramid_evidence(slug, source_node_id);
        -- NOTE: idx_evidence_build is created by migrate_evidence_pk AFTER build_id column exists

        -- Question decomposition tree per slug (stored as JSON blob)
        CREATE TABLE IF NOT EXISTS pyramid_question_tree (
            slug TEXT NOT NULL,
            build_id TEXT NOT NULL DEFAULT '',
            tree TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (slug, build_id)
        );

        -- Individual question decomposition nodes for incremental/resumable builds
        CREATE TABLE IF NOT EXISTS pyramid_question_nodes (
            slug TEXT NOT NULL,
            question_id TEXT NOT NULL,
            parent_id TEXT,
            depth INTEGER NOT NULL,
            question TEXT NOT NULL,
            about TEXT NOT NULL DEFAULT '',
            creates TEXT NOT NULL DEFAULT '',
            prompt_hint TEXT NOT NULL DEFAULT '',
            is_leaf INTEGER NOT NULL DEFAULT 0,
            children_json TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY(slug, question_id)
        );
        CREATE INDEX IF NOT EXISTS idx_question_nodes_parent ON pyramid_question_nodes(slug, parent_id);
        CREATE INDEX IF NOT EXISTS idx_question_nodes_depth ON pyramid_question_nodes(slug, depth);

        -- Missing evidence gap reports
        CREATE TABLE IF NOT EXISTS pyramid_gaps (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL,
            question_id TEXT NOT NULL,
            description TEXT NOT NULL,
            layer INTEGER NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(slug, question_id, description)
        );
        CREATE INDEX IF NOT EXISTS idx_gaps_slug ON pyramid_gaps(slug);
        CREATE INDEX IF NOT EXISTS idx_gaps_question ON pyramid_gaps(slug, question_id);

        -- Per-file change log for crystallization (NOT thread-level pyramid_deltas)
        CREATE TABLE IF NOT EXISTS pyramid_source_deltas (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL,
            file_path TEXT NOT NULL,
            change_type TEXT NOT NULL,
            diff_summary TEXT,
            processed INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_source_deltas_unprocessed ON pyramid_source_deltas(slug, processed);

        -- Belief correction audit trail
        CREATE TABLE IF NOT EXISTS pyramid_supersessions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL,
            node_id TEXT NOT NULL,
            superseded_claim TEXT NOT NULL,
            corrected_to TEXT NOT NULL,
            source_node TEXT,
            channel TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_supersessions_slug ON pyramid_supersessions(slug);
        CREATE INDEX IF NOT EXISTS idx_supersessions_node ON pyramid_supersessions(slug, node_id);

        -- Pending re-answer work items
        CREATE TABLE IF NOT EXISTS pyramid_staleness_queue (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL,
            question_id TEXT NOT NULL,
            reason TEXT NOT NULL,
            channel TEXT NOT NULL,
            priority REAL NOT NULL DEFAULT 0.0,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(slug, question_id)
        );
        CREATE INDEX IF NOT EXISTS idx_staleness_queue_slug ON pyramid_staleness_queue(slug, priority DESC);

        -- Build metadata
        CREATE TABLE IF NOT EXISTS pyramid_builds (
            slug TEXT NOT NULL,
            build_id TEXT NOT NULL,
            question TEXT NOT NULL,
            started_at TEXT NOT NULL DEFAULT (datetime('now')),
            completed_at TEXT,
            status TEXT NOT NULL DEFAULT 'running',
            layers_completed INTEGER DEFAULT 0,
            total_layers INTEGER DEFAULT 0,
            l0_node_count INTEGER DEFAULT 0,
            total_node_count INTEGER DEFAULT 0,
            quality_score REAL,
            error_message TEXT,
            PRIMARY KEY (slug, build_id)
        );
        CREATE INDEX IF NOT EXISTS idx_builds_slug ON pyramid_builds(slug);
        ",
    )?;

    // Migrate pyramid_id_map: add wire_handle_path column if missing
    let _ = conn.execute(
        "ALTER TABLE pyramid_id_map ADD COLUMN wire_handle_path TEXT DEFAULT ''",
        [],
    );

    // Migrate pyramid_staleness_queue: add UNIQUE(slug, question_id) if missing
    migrate_staleness_queue_unique(conn)?;

    // Migrate pyramid_gaps: add UNIQUE(slug, question_id, description) and question index if missing
    migrate_gaps_unique(conn)?;

    // Backfill pyramid_evidence from existing pyramid_nodes.children arrays
    backfill_evidence_from_children(conn)?;

    // ── WS3: live_pyramid_evidence view (joins against live nodes on both sides) ──
    conn.execute_batch(
        "
        CREATE VIEW IF NOT EXISTS live_pyramid_evidence AS
        SELECT e.* FROM pyramid_evidence e
        INNER JOIN live_pyramid_nodes s ON e.source_node_id = s.id AND e.slug = s.slug
        INNER JOIN live_pyramid_nodes t ON e.target_node_id = t.id AND e.slug = t.slug;
        ",
    )?;

    // ── WS8-A: Multi-reference answer pyramid schema ─────────────────────────

    // Junction table for slug cross-references (NO CASCADE DELETE on either FK)
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_slug_references (
            slug TEXT NOT NULL REFERENCES pyramid_slugs(slug),
            referenced_slug TEXT NOT NULL REFERENCES pyramid_slugs(slug),
            reference_type TEXT NOT NULL DEFAULT 'base',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (slug, referenced_slug)
        );
        ",
    )?;

    // Slug archival column
    let _ = conn.execute(
        "ALTER TABLE pyramid_slugs ADD COLUMN archived_at TEXT DEFAULT NULL",
        [],
    );

    // Evidence build_id column (added before PK rebuild migration)
    let _ = conn.execute(
        "ALTER TABLE pyramid_evidence ADD COLUMN build_id TEXT DEFAULT NULL",
        [],
    );

    // Original question on builds table
    let _ = conn.execute(
        "ALTER TABLE pyramid_builds ADD COLUMN original_question TEXT DEFAULT NULL",
        [],
    );

    // Gap report build_id
    let _ = conn.execute(
        "ALTER TABLE pyramid_gaps ADD COLUMN build_id TEXT DEFAULT NULL",
        [],
    );

    // Migrate CHECK constraint to include 'question' content type
    migrate_slugs_check_question(conn)?;

    // Rebuild evidence table PK to include build_id
    migrate_evidence_pk(conn)?;

    // Rebuild question_tree PK to (slug, build_id)
    migrate_question_tree_pk(conn)?;

    // Create evidence build_id index — safe to run after migration ensures column exists
    // For fresh DBs: column exists from CREATE TABLE. For existing: migration added it.
    let _ = conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_evidence_build ON pyramid_evidence(slug, build_id);",
    );

    // ── Understanding Web: targeted L0 index + gap resolution ──
    let _ = conn.execute(
        "ALTER TABLE pyramid_gaps ADD COLUMN resolved INTEGER NOT NULL DEFAULT 0",
        [],
    );
    let _ = conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_nodes_targeted_l0
         ON pyramid_nodes(slug, depth)
         WHERE depth = 0 AND self_prompt != '';",
    );

    // Migrate pyramid_gaps: add resolution_confidence column
    let _ = conn.execute(
        "ALTER TABLE pyramid_gaps ADD COLUMN resolution_confidence REAL NOT NULL DEFAULT 0.0",
        [],
    );
    // Backfill: existing resolved=1 gaps get confidence=1.0
    let _ = conn.execute(
        "UPDATE pyramid_gaps SET resolution_confidence = 1.0 WHERE resolved = 1 AND resolution_confidence = 0.0",
        [],
    );

    // ── Live Pyramid Theatre: LLM audit trail ──────────────────────────────
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_llm_audit (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL,
            build_id TEXT NOT NULL,
            node_id TEXT,
            step_name TEXT NOT NULL,
            call_purpose TEXT NOT NULL,
            depth INTEGER,
            model TEXT NOT NULL,
            system_prompt TEXT NOT NULL,
            user_prompt TEXT NOT NULL,
            raw_response TEXT,
            parsed_ok INTEGER DEFAULT 0,
            prompt_tokens INTEGER DEFAULT 0,
            completion_tokens INTEGER DEFAULT 0,
            latency_ms INTEGER,
            generation_id TEXT,
            status TEXT NOT NULL DEFAULT 'pending',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            completed_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_llm_audit_slug_build ON pyramid_llm_audit(slug, build_id);
        CREATE INDEX IF NOT EXISTS idx_llm_audit_node ON pyramid_llm_audit(slug, node_id);

        -- Prompt deduplication: system prompts repeat across nodes in a build.
        -- Store unique prompts by hash, audit rows reference the hash.
        CREATE TABLE IF NOT EXISTS pyramid_prompt_store (
            hash TEXT PRIMARY KEY,
            content TEXT NOT NULL,
            char_count INTEGER NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        ",
    )?;

    // ── Schema Prep Migration for Wire Online push ──────────────────────────
    migrate_online_push_columns(conn)?;

    // ── Phase 0.5: post-agents-retro web surface skeleton ──────────────────
    // (a) web_sessions — v3.3 A2 + B14
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS web_sessions (
            token TEXT PRIMARY KEY,
            supabase_user_id TEXT NOT NULL,
            email TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            expires_at TEXT NOT NULL,
            last_seen_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_web_sessions_expires ON web_sessions(expires_at);
        ",
    )?;

    // (b) pyramid_ascii_art — v3.3 C1 supersession-aware
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_ascii_art (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL,
            kind TEXT NOT NULL,
            source_hash TEXT NOT NULL,
            art_text TEXT NOT NULL,
            model TEXT NOT NULL,
            superseded_by INTEGER REFERENCES pyramid_ascii_art(id),
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_ascii_art_slug_kind_head
            ON pyramid_ascii_art(slug, kind) WHERE superseded_by IS NULL;
        ",
    )?;

    // (c) pyramid_slugs.updated_at — add if absent (idempotent; ignores
    // duplicate-column error on existing DBs).
    let _ = conn.execute(
        "ALTER TABLE pyramid_slugs ADD COLUMN updated_at TEXT NOT NULL DEFAULT (datetime('now'))",
        [],
    );

    // WS-COST-MODEL: transparency + preview-gate cost estimation per chain phase.
    // Cold-start seeding + observation-based recompute are handled by
    // `pyramid::cost_model::{apply_seed, recompute_from_audit}`. This block only
    // ensures the table exists.
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_chain_cost_model (
            chain_phase TEXT NOT NULL,
            model TEXT NOT NULL,
            avg_input_tokens REAL NOT NULL DEFAULT 0,
            avg_output_tokens REAL NOT NULL DEFAULT 0,
            calls_per_conversation REAL NOT NULL DEFAULT 0,
            usd_per_call REAL NOT NULL DEFAULT 0,
            usd_per_conversation REAL NOT NULL DEFAULT 0,
            is_heuristic INTEGER NOT NULL DEFAULT 1,
            sample_count INTEGER NOT NULL DEFAULT 0,
            updated_at INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (chain_phase, model)
        );
        CREATE INDEX IF NOT EXISTS idx_chain_cost_model_phase
            ON pyramid_chain_cost_model(chain_phase);
        ",
    )?;

    // WS-DEADLETTER (§15.18): persistent record of chain steps that
    // exhausted retries. Operator surface via HTTP routes. `status` is a
    // simple state machine: 'open' -> 'resolved' | 'skipped' (terminal).
    // Snapshots (step_snapshot, system_prompt, defaults_snapshot,
    // input_snapshot) let us re-dispatch without reloading chain YAML.
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_dead_letter (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL,
            chain_id TEXT,
            step_name TEXT NOT NULL,
            step_primitive TEXT NOT NULL,
            chunk_index INTEGER,
            input_snapshot TEXT,
            step_snapshot TEXT,
            system_prompt TEXT,
            defaults_snapshot TEXT,
            error_text TEXT NOT NULL,
            error_kind TEXT NOT NULL,
            retry_count INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL DEFAULT 'open',
            note TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            last_seen_at TEXT NOT NULL DEFAULT (datetime('now')),
            resolved_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_dead_letter_slug_status
            ON pyramid_dead_letter(slug, status);
        ",
    )?;

    // WS-INGEST-PRIMITIVE (Phase 1.5): Track what has been ingested, when,
    // with what signature, and what state it's in. DADBEAR uses this to
    // detect, debounce, and trigger pyramid construction from source files.
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_ingest_records (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL,
            source_path TEXT NOT NULL,
            content_type TEXT NOT NULL,
            ingest_signature TEXT NOT NULL,
            file_hash TEXT,
            file_mtime TEXT,
            status TEXT NOT NULL DEFAULT 'pending',
            build_id TEXT,
            error_message TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(slug, source_path, ingest_signature)
        );
        CREATE INDEX IF NOT EXISTS idx_ingest_records_slug_status
            ON pyramid_ingest_records(slug, status);
        CREATE INDEX IF NOT EXISTS idx_ingest_records_slug_sig
            ON pyramid_ingest_records(slug, ingest_signature);
        ",
    )?;

    // WS-PROVISIONAL (Phase 2b): Live-session provisional node lifecycle.
    // Tracks which provisional nodes were created in each live session so they
    // can be batch-promoted when the canonical build completes.
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_provisional_sessions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL,
            source_path TEXT NOT NULL,
            session_id TEXT NOT NULL UNIQUE,
            status TEXT NOT NULL DEFAULT 'active',
            provisional_node_ids TEXT,
            canonical_build_id TEXT,
            file_mtime TEXT,
            last_chunk_processed INTEGER DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_prov_sessions_slug_status
            ON pyramid_provisional_sessions(slug, status);
        CREATE INDEX IF NOT EXISTS idx_prov_sessions_session_id
            ON pyramid_provisional_sessions(session_id);
        ",
    )?;

    Ok(())
}

/// Wire Online Push — Schema Prep Migration.
///
/// Adds all columns needed by WS-ONLINE-A through WS-ONLINE-G to `pyramid_slugs`,
/// adds build_id/archived_at to `pyramid_web_edges`, creates `pyramid_remote_web_edges`,
/// and backfills web edge build_ids. Idempotent: uses ALTER TABLE with error suppression
/// and CREATE TABLE/INDEX IF NOT EXISTS.
fn migrate_online_push_columns(conn: &Connection) -> Result<()> {
    // ── pyramid_slugs: publication tracking (WS-ONLINE-A) ──
    let _ = conn.execute(
        "ALTER TABLE pyramid_slugs ADD COLUMN last_published_build_id TEXT DEFAULT NULL",
        [],
    );

    // ── pyramid_slugs: pinning (WS-ONLINE-D) ──
    let _ = conn.execute(
        "ALTER TABLE pyramid_slugs ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_slugs ADD COLUMN source_tunnel_url TEXT DEFAULT NULL",
        [],
    );

    // ── pyramid_slugs: access tiers (WS-ONLINE-E) ──
    let _ = conn.execute(
        "ALTER TABLE pyramid_slugs ADD COLUMN access_tier TEXT NOT NULL DEFAULT 'public'",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_slugs ADD COLUMN access_price INTEGER DEFAULT NULL",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_slugs ADD COLUMN allowed_circles TEXT DEFAULT NULL",
        [],
    );

    // ── pyramid_slugs: discovery metadata tracking (WS-ONLINE-B) ──
    let _ = conn.execute(
        "ALTER TABLE pyramid_slugs ADD COLUMN metadata_contribution_id TEXT DEFAULT NULL",
        [],
    );

    // ── pyramid_slugs: absorption config (WS-ONLINE-G) ──
    let _ = conn.execute(
        "ALTER TABLE pyramid_slugs ADD COLUMN absorption_mode TEXT NOT NULL DEFAULT 'open'",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_slugs ADD COLUMN absorption_chain_id TEXT DEFAULT NULL",
        [],
    );

    // ── pyramid_slugs: emergent pricing cache (WS-ONLINE-E) ──
    let _ = conn.execute(
        "ALTER TABLE pyramid_slugs ADD COLUMN cached_emergent_price INTEGER DEFAULT NULL",
        [],
    );

    // ── pyramid_web_edges: build_id scoping (WS-ONLINE-S3) ──
    let _ = conn.execute(
        "ALTER TABLE pyramid_web_edges ADD COLUMN build_id TEXT DEFAULT NULL",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_web_edges ADD COLUMN archived_at TEXT DEFAULT NULL",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_web_edges ADD COLUMN last_confirmed_at TEXT DEFAULT NULL",
        [],
    );

    // ── pyramid_web_edge_deltas: build_id scoping (WS-ONLINE-S3) ──
    let _ = conn.execute(
        "ALTER TABLE pyramid_web_edge_deltas ADD COLUMN build_id TEXT DEFAULT NULL",
        [],
    );

    // ── pyramid_remote_web_edges table (WS-ONLINE-F) ──
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_remote_web_edges (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL,
            local_thread_id TEXT NOT NULL,
            remote_handle_path TEXT NOT NULL,
            remote_tunnel_url TEXT NOT NULL,
            relationship TEXT NOT NULL DEFAULT '',
            relevance REAL NOT NULL DEFAULT 1.0,
            delta_count INTEGER NOT NULL DEFAULT 0,
            build_id TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(slug, local_thread_id, remote_handle_path, build_id),
            FOREIGN KEY (slug, local_thread_id) REFERENCES pyramid_threads(slug, thread_id)
        );
        CREATE INDEX IF NOT EXISTS idx_remote_web_edges_slug ON pyramid_remote_web_edges(slug);
        ",
    )?;

    // ── Backfill: set build_id on existing web edges from latest node build_id ──
    let _ = conn.execute(
        "UPDATE pyramid_web_edges SET build_id = (
            SELECT MAX(build_id) FROM pyramid_nodes
            WHERE pyramid_nodes.slug = pyramid_web_edges.slug
        ) WHERE build_id IS NULL",
        [],
    );

    // ── pyramid_unredeemed_tokens: payment retry queue (WS-ONLINE-H) ──
    //
    // When a serving node executes a query but fails to redeem the payment
    // token (e.g., Wire server unavailable), the token is stored here for
    // retry with exponential backoff (up to 5 attempts). Tokens auto-expire
    // after TTL (60s) — the Wire server is the authority on expiration.
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_unredeemed_tokens (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            nonce TEXT NOT NULL UNIQUE,
            payment_token TEXT NOT NULL,
            querier_operator_id TEXT NOT NULL,
            slug TEXT NOT NULL,
            query_type TEXT NOT NULL,
            stamp_amount INTEGER NOT NULL DEFAULT 1,
            access_amount INTEGER NOT NULL DEFAULT 0,
            total_amount INTEGER NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            expires_at TEXT NOT NULL,
            retry_count INTEGER NOT NULL DEFAULT 0,
            last_retry_at TEXT DEFAULT NULL,
            redeemed_at TEXT DEFAULT NULL,
            status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending', 'redeemed', 'expired', 'failed'))
        );
        CREATE INDEX IF NOT EXISTS idx_unredeemed_tokens_status
            ON pyramid_unredeemed_tokens(status) WHERE status = 'pending';
        CREATE INDEX IF NOT EXISTS idx_unredeemed_tokens_expires
            ON pyramid_unredeemed_tokens(expires_at) WHERE status = 'pending';
        ",
    )?;

    // ── WS-2: faq_synthesis_pass column on pyramid_annotations ──
    // Tracks which FAQ synthesis pass processed each annotation.
    // NULL = unprocessed, 'ACUTE' = acute FAQ path, 'PASS-{uuid}' = passive pass.
    let _ = conn.execute(
        "ALTER TABLE pyramid_annotations ADD COLUMN faq_synthesis_pass TEXT DEFAULT NULL",
        [],
    );
    // Backfill: existing annotations with question_context were created by the acute FAQ path.
    let _ = conn.execute(
        "UPDATE pyramid_annotations SET faq_synthesis_pass = 'ACUTE' WHERE question_context IS NOT NULL AND faq_synthesis_pass IS NULL",
        [],
    );

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

/// Migrate `pyramid_slugs` CHECK constraint to include 'question' content type.
/// Idempotent: skips if CHECK already includes 'question'.
fn migrate_slugs_check_question(conn: &Connection) -> Result<()> {
    let table_sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='pyramid_slugs'",
            [],
            |row| row.get(0),
        )
        .ok();

    let needs_migration = match &table_sql {
        Some(sql) => sql.contains("CHECK") && !sql.contains("question"),
        None => false,
    };

    if !needs_migration {
        return Ok(());
    }

    tracing::info!("Migrating pyramid_slugs CHECK constraint to include 'question'...");

    conn.execute_batch("PRAGMA foreign_keys=OFF;")?;

    let result = (|| -> Result<()> {
        let tx = conn.unchecked_transaction()?;

        tx.execute_batch(
            "
            CREATE TABLE pyramid_slugs_new (
                slug TEXT PRIMARY KEY,
                content_type TEXT NOT NULL CHECK(content_type IN ('code', 'conversation', 'document', 'vine', 'question')),
                source_path TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                last_built_at TEXT,
                node_count INTEGER NOT NULL DEFAULT 0,
                max_depth INTEGER NOT NULL DEFAULT 0,
                archived_at TEXT DEFAULT NULL
            );
            INSERT INTO pyramid_slugs_new (slug, content_type, source_path, created_at, last_built_at, node_count, max_depth, archived_at)
                SELECT slug, content_type, source_path, created_at, last_built_at, node_count, max_depth, archived_at
                FROM pyramid_slugs;
            DROP TABLE pyramid_slugs;
            ALTER TABLE pyramid_slugs_new RENAME TO pyramid_slugs;
            ",
        )?;

        tx.commit()?;
        Ok(())
    })();

    conn.execute_batch("PRAGMA foreign_keys=ON;")?;

    match result {
        Ok(()) => {
            tracing::info!("pyramid_slugs CHECK constraint migrated to include 'question'.");
            Ok(())
        }
        Err(e) => {
            tracing::error!("pyramid_slugs question migration failed (FK re-enabled): {e}");
            Err(e)
        }
    }
}

/// Migrate `pyramid_evidence` PK from `(slug, source_node_id, target_node_id)` to
/// `(slug, build_id, source_node_id, target_node_id)`.
/// Idempotent: skips if PK already includes `build_id`.
fn migrate_evidence_pk(conn: &Connection) -> Result<()> {
    let table_sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='pyramid_evidence'",
            [],
            |row| row.get(0),
        )
        .ok();

    let needs_migration = match &table_sql {
        Some(sql) => {
            // Check if build_id is in the PRIMARY KEY clause specifically.
            // ALTER TABLE adds build_id to the SQL but doesn't change the PK.
            // We need to check if the PK definition itself includes build_id.
            if let Some(pk_start) = sql.find("PRIMARY KEY") {
                let pk_section = &sql[pk_start..];
                // Find the closing paren of the PK definition
                if let Some(pk_end) = pk_section.find(')') {
                    let pk_def = &pk_section[..pk_end + 1];
                    !pk_def.contains("build_id")
                } else {
                    true // malformed PK, try migration
                }
            } else {
                false // no PK found, skip
            }
        }
        None => false,
    };

    if !needs_migration {
        return Ok(());
    }

    tracing::info!("Migrating pyramid_evidence PK to include build_id...");

    // Must drop the view that depends on this table first
    let _ = conn.execute_batch("DROP VIEW IF EXISTS live_pyramid_evidence;");

    conn.execute_batch("PRAGMA foreign_keys=OFF;")?;

    let result = (|| -> Result<()> {
        let tx = conn.unchecked_transaction()?;

        tx.execute_batch(
            "
            CREATE TABLE pyramid_evidence_new (
                slug TEXT NOT NULL,
                build_id TEXT NOT NULL DEFAULT '',
                source_node_id TEXT NOT NULL,
                target_node_id TEXT NOT NULL,
                verdict TEXT NOT NULL CHECK(verdict IN ('KEEP', 'DISCONNECT', 'MISSING')),
                weight REAL,
                reason TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                PRIMARY KEY (slug, build_id, source_node_id, target_node_id)
            );
            INSERT INTO pyramid_evidence_new (slug, build_id, source_node_id, target_node_id, verdict, weight, reason, created_at)
                SELECT slug, COALESCE(build_id, ''), source_node_id, target_node_id, verdict, weight, reason, created_at
                FROM pyramid_evidence;
            DROP TABLE pyramid_evidence;
            ALTER TABLE pyramid_evidence_new RENAME TO pyramid_evidence;
            CREATE INDEX IF NOT EXISTS idx_evidence_target ON pyramid_evidence(slug, target_node_id);
            CREATE INDEX IF NOT EXISTS idx_evidence_source ON pyramid_evidence(slug, source_node_id);
            CREATE INDEX IF NOT EXISTS idx_evidence_build ON pyramid_evidence(slug, build_id);
            ",
        )?;

        tx.commit()?;
        Ok(())
    })();

    conn.execute_batch("PRAGMA foreign_keys=ON;")?;

    // Recreate the live view
    let _ = conn.execute_batch(
        "
        CREATE VIEW IF NOT EXISTS live_pyramid_evidence AS
        SELECT e.* FROM pyramid_evidence e
        INNER JOIN live_pyramid_nodes s ON e.source_node_id = s.id AND e.slug = s.slug
        INNER JOIN live_pyramid_nodes t ON e.target_node_id = t.id AND e.slug = t.slug;
        ",
    );

    match result {
        Ok(()) => {
            tracing::info!("pyramid_evidence PK migrated to include build_id.");
            Ok(())
        }
        Err(e) => {
            tracing::error!("pyramid_evidence PK migration failed (FK re-enabled): {e}");
            Err(e)
        }
    }
}

/// Migrate `pyramid_question_tree` PK from `(slug)` to `(slug, build_id)`.
/// Idempotent: skips if PK already includes `build_id`.
fn migrate_question_tree_pk(conn: &Connection) -> Result<()> {
    let table_sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='pyramid_question_tree'",
            [],
            |row| row.get(0),
        )
        .ok();

    let needs_migration = match &table_sql {
        Some(sql) => {
            // The original table has `slug TEXT PRIMARY KEY` — no build_id in PK
            // After migration it will have `PRIMARY KEY (slug, build_id)`
            !sql.contains("build_id")
                || (sql.contains("build_id") && sql.contains("slug TEXT PRIMARY KEY"))
        }
        None => false,
    };

    if !needs_migration {
        return Ok(());
    }

    tracing::info!("Migrating pyramid_question_tree PK to (slug, build_id)...");

    conn.execute_batch("PRAGMA foreign_keys=OFF;")?;

    let result = (|| -> Result<()> {
        let tx = conn.unchecked_transaction()?;

        tx.execute_batch(
            "
            CREATE TABLE pyramid_question_tree_new (
                slug TEXT NOT NULL,
                build_id TEXT NOT NULL DEFAULT '',
                tree TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                PRIMARY KEY (slug, build_id)
            );
            INSERT INTO pyramid_question_tree_new (slug, build_id, tree, created_at, updated_at)
                SELECT slug, COALESCE(build_id, ''), tree, created_at, updated_at
                FROM pyramid_question_tree;
            DROP TABLE pyramid_question_tree;
            ALTER TABLE pyramid_question_tree_new RENAME TO pyramid_question_tree;
            ",
        )?;

        tx.commit()?;
        Ok(())
    })();

    conn.execute_batch("PRAGMA foreign_keys=ON;")?;

    match result {
        Ok(()) => {
            tracing::info!("pyramid_question_tree PK migrated to (slug, build_id).");
            Ok(())
        }
        Err(e) => {
            tracing::error!("pyramid_question_tree PK migration failed (FK re-enabled): {e}");
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
        "SELECT slug, content_type, source_path, node_count, max_depth, last_built_at, created_at, archived_at
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
            archived_at: row.get(7)?,
            referenced_slugs: Vec::new(),
            referencing_slugs: Vec::new(),
        })
    });

    match result {
        Ok(mut info) => {
            info.referenced_slugs = get_slug_references(conn, &info.slug).unwrap_or_default();
            info.referencing_slugs = get_slug_referrers(conn, &info.slug).unwrap_or_default();
            Ok(Some(info))
        }
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
/// Filters out archived slugs (archived_at IS NULL).
pub fn list_slugs_filtered(conn: &Connection, exclude_bunches: bool) -> Result<Vec<SlugInfo>> {
    let sql = if exclude_bunches {
        "SELECT slug, content_type, source_path, node_count, max_depth, last_built_at, created_at, archived_at
         FROM pyramid_slugs WHERE slug NOT LIKE '%--bunch-%' AND archived_at IS NULL ORDER BY created_at DESC"
    } else {
        "SELECT slug, content_type, source_path, node_count, max_depth, last_built_at, created_at, archived_at
         FROM pyramid_slugs WHERE archived_at IS NULL ORDER BY created_at DESC"
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
            archived_at: row.get(7)?,
            referenced_slugs: Vec::new(),
            referencing_slugs: Vec::new(),
        })
    })?;

    let mut slugs = Vec::new();
    for row in rows {
        let mut info = row?;
        info.referenced_slugs = get_slug_references(conn, &info.slug).unwrap_or_default();
        info.referencing_slugs = get_slug_referrers(conn, &info.slug).unwrap_or_default();
        slugs.push(info);
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

// ── Slug References (WS8-A) ──────────────────────────────────────────────────

/// Bulk-insert slug references: records that `slug` reads from each of `referenced_slugs`.
/// Uses INSERT OR IGNORE to skip existing pairs.
pub fn save_slug_references(
    conn: &Connection,
    slug: &str,
    referenced_slugs: &[String],
) -> Result<()> {
    let mut stmt = conn.prepare(
        "INSERT OR IGNORE INTO pyramid_slug_references (slug, referenced_slug) VALUES (?1, ?2)",
    )?;
    for ref_slug in referenced_slugs {
        stmt.execute(rusqlite::params![slug, ref_slug])?;
    }
    Ok(())
}

/// What does this slug read from? Returns referenced slugs.
pub fn get_slug_references(conn: &Connection, slug: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT referenced_slug FROM pyramid_slug_references WHERE slug = ?1 ORDER BY created_at",
    )?;
    let rows = stmt.query_map(rusqlite::params![slug], |row| row.get(0))?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Who references this slug? Returns slugs that reference the given slug.
pub fn get_slug_referrers(conn: &Connection, slug: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT slug FROM pyramid_slug_references WHERE referenced_slug = ?1 ORDER BY created_at",
    )?;
    let rows = stmt.query_map(rusqlite::params![slug], |row| row.get(0))?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Return all question pyramids that reference `source_slug`. Used by the
/// public web surface to render the "Questions asked" section on a source
/// pyramid's home page. Newest first.
pub fn get_questions_referencing(
    conn: &Connection,
    source_slug: &str,
) -> Result<Vec<SlugInfo>> {
    let mut stmt = conn.prepare(
        "SELECT s.slug, s.content_type, s.source_path, s.node_count, s.max_depth,
                s.last_built_at, s.created_at, s.archived_at
         FROM pyramid_slugs s
         JOIN pyramid_slug_references r ON r.slug = s.slug
         WHERE r.referenced_slug = ?1
           AND s.content_type = 'question'
           AND s.archived_at IS NULL
         ORDER BY s.created_at DESC",
    )?;
    let rows = stmt.query_map(rusqlite::params![source_slug], |row| {
        let ct_str: String = row.get(1)?;
        let content_type = ContentType::from_str(&ct_str).unwrap_or(ContentType::Question);
        Ok(SlugInfo {
            slug: row.get(0)?,
            content_type,
            source_path: row.get(2)?,
            node_count: row.get(3)?,
            max_depth: row.get(4)?,
            last_built_at: row.get(5)?,
            created_at: row.get(6)?,
            archived_at: row.get(7)?,
            referenced_slugs: Vec::new(),
            referencing_slugs: Vec::new(),
        })
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Returns true if any other slug references this one.
pub fn has_slug_referrers(conn: &Connection, slug: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pyramid_slug_references WHERE referenced_slug = ?1",
        rusqlite::params![slug],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Archive a slug — sets `archived_at` timestamp. Does NOT delete.
pub fn archive_slug(conn: &Connection, slug: &str) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_slugs SET archived_at = datetime('now') WHERE slug = ?1",
        rusqlite::params![slug],
    )?;
    // Disable DADBEAR for archived slugs — prevents stale engine from monitoring them
    conn.execute(
        "UPDATE pyramid_auto_update_config SET auto_update = 0, frozen = 1, frozen_at = datetime('now') WHERE slug = ?1",
        rusqlite::params![slug],
    )?;
    Ok(())
}

/// Admin-only hard delete of a slug and all associated data (like parity.rs exemption).
/// Unlike `delete_slug`, this also removes slug reference entries in both directions.
pub fn purge_slug(conn: &Connection, slug: &str) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM pyramid_slug_references WHERE slug = ?1 OR referenced_slug = ?1",
        rusqlite::params![slug],
    )?;
    tx.execute(
        "DELETE FROM pyramid_slugs WHERE slug = ?1",
        rusqlite::params![slug],
    )?;
    tx.commit()?;
    Ok(())
}

// ── Handle Path Utilities (WS8-A) ───────────────────────────────────────────

/// Parse a handle path like "vibe-ev8/0/L0-003" into (slug, depth, node_id).
/// Returns None for bare IDs that contain no '/'.
pub fn parse_handle_path(id: &str) -> Option<(&str, i64, &str)> {
    let parts: Vec<&str> = id.splitn(3, '/').collect();
    if parts.len() != 3 {
        return None;
    }
    let depth: i64 = parts[1].parse().ok()?;
    Some((parts[0], depth, parts[2]))
}

/// Format a handle path from components.
pub fn format_handle_path(slug: &str, depth: i64, node_id: &str) -> String {
    format!("{}/{}/{}", slug, depth, node_id)
}

/// 11-A: Supersede pipeline steps above a given depth for a slug + build_id.
/// Scoped by build_id instead of bare DELETE. When build_id is None, scopes to
/// steps with NULL build_id (legacy data).
pub fn delete_steps_above_depth(
    conn: &Connection,
    slug: &str,
    depth: i64,
    build_id: Option<&str>,
) -> Result<i64> {
    let count = match build_id {
        Some(bid) => conn.execute(
            "DELETE FROM pyramid_pipeline_steps WHERE slug = ?1 AND depth > ?2 AND build_id = ?3",
            rusqlite::params![slug, depth, bid],
        )?,
        None => conn.execute(
            "DELETE FROM pyramid_pipeline_steps WHERE slug = ?1 AND depth > ?2 AND build_id IS NULL",
            rusqlite::params![slug, depth],
        )?,
    };
    Ok(count as i64)
}

/// Recompute node_count, max_depth, and last_built_at from the nodes table.
pub fn update_slug_stats(conn: &Connection, slug: &str) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_slugs SET
            node_count = (SELECT COUNT(*) FROM live_pyramid_nodes WHERE slug = ?1),
            max_depth = COALESCE((SELECT MAX(depth) FROM live_pyramid_nodes WHERE slug = ?1), 0),
            last_built_at = datetime('now'),
            updated_at = datetime('now')
         WHERE slug = ?1",
        rusqlite::params![slug],
    )?;
    Ok(())
}

/// Bump `pyramid_slugs.updated_at` for cache-busting purposes (the public web
/// surface ETags pyramid pages on this column). Best-effort: failures are
/// non-fatal — the cache just stays slightly fresh longer than necessary.
/// Call from any site that mutates a slug's visible content (banner replace,
/// access tier change, absorption mode change, contribution writes, etc.).
pub fn touch_slug(conn: &Connection, slug: &str) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_slugs SET updated_at = datetime('now') WHERE slug = ?1",
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

/// Load just the first 200 bytes of a chunk's content (for file path header extraction).
/// Avoids loading full 50KB+ content when only the `## FILE: path` header is needed.
pub fn get_chunk_header(conn: &Connection, slug: &str, chunk_index: i64) -> Result<Option<String>> {
    let result = conn.query_row(
        "SELECT SUBSTR(content, 1, 200) FROM pyramid_chunks WHERE slug = ?1 AND chunk_index = ?2",
        rusqlite::params![slug, chunk_index],
        |row| row.get::<_, String>(0),
    );
    match result {
        Ok(header) => Ok(Some(header)),
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

/// Delete all chunks for a slug. Used before re-ingestion to prevent duplicates.
pub fn clear_chunks(conn: &Connection, slug: &str) -> Result<i64> {
    let deleted = conn.execute(
        "DELETE FROM pyramid_chunks WHERE slug = ?1",
        rusqlite::params![slug],
    )?;
    // Also reset batch chunk_counts so they don't drift
    conn.execute(
        "UPDATE pyramid_batches SET chunk_count = 0 WHERE slug = ?1",
        rusqlite::params![slug],
    )?;
    Ok(deleted as i64)
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
        "SELECT {NODE_SELECT_COLS} FROM live_pyramid_nodes
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
        build_id: row.get::<_, Option<String>>("build_id").unwrap_or(None),
        created_at: row.get::<_, String>("created_at").unwrap_or_default(),

        // ── WS-SCHEMA-V2 (§15.2) new columns ──
        // All reads are tolerant: if the SELECT list doesn't include a column
        // (e.g. `get_node_summary` which uses a hand-written list), or the
        // JSON is NULL, we fall back to Default so existing pyramids and
        // lightweight queries round-trip cleanly.
        time_range: {
            let start = row
                .get::<_, Option<String>>("time_range_start")
                .ok()
                .flatten();
            let end = row.get::<_, Option<String>>("time_range_end").ok().flatten();
            if start.is_some() || end.is_some() {
                Some(TimeRange { start, end })
            } else {
                None
            }
        },
        weight: row
            .get::<_, Option<f64>>("weight")
            .ok()
            .flatten()
            .unwrap_or(0.0),
        provisional: row
            .get::<_, Option<i64>>("provisional")
            .ok()
            .flatten()
            .map(|v| v != 0)
            .unwrap_or(false),
        promoted_from: row
            .get::<_, Option<String>>("promoted_from")
            .ok()
            .flatten(),
        narrative: row
            .get::<_, Option<String>>("narrative_json")
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default(),
        entities: row
            .get::<_, Option<String>>("entities_json")
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default(),
        key_quotes: row
            .get::<_, Option<String>>("key_quotes_json")
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default(),
        transitions: row
            .get::<_, Option<String>>("transitions_json")
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default(),
        current_version: row
            .get::<_, Option<i64>>("current_version")
            .ok()
            .flatten()
            .unwrap_or(1),
        current_version_chain_phase: row
            .get::<_, Option<String>>("current_version_chain_phase")
            .ok()
            .flatten(),
    })
}

const NODE_SELECT_COLS: &str =
    "id, slug, depth, chunk_index, headline, distilled, topics, corrections, decisions, \
     terms, dead_ends, self_prompt, children, parent_id, superseded_by, build_id, created_at, \
     time_range_start, time_range_end, weight, provisional, promoted_from, \
     narrative_json, entities_json, key_quotes_json, transitions_json, \
     current_version, current_version_chain_phase";

/// Save (upsert) a PyramidNode. Serializes all Vec fields to JSON strings.
///
/// The optional `topics_json` parameter allows passing a pre-serialized topics
/// string (useful when the build pipeline already has the raw JSON). If None,
/// topics are serialized from `node.topics`.
pub fn save_node(conn: &Connection, node: &PyramidNode, topics_json: Option<&str>) -> Result<()> {
    // WS-SCHEMA-V2 (§15.7 "Unified write path"): if a row already exists for
    // (slug, id), route the write through apply_supersession so the prior
    // content is snapshotted into pyramid_node_versions before the UPDATE.
    // Only the first write of a (slug, id) pair takes the bare INSERT path
    // below. Provisional rows mutate in place via mutate_provisional_node.
    let row_exists: bool = conn
        .query_row(
            "SELECT 1 FROM pyramid_nodes WHERE slug = ?1 AND id = ?2",
            rusqlite::params![node.slug, node.id],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);

    if row_exists {
        let (is_provisional, existing_depth): (bool, i64) = conn
            .query_row(
                "SELECT COALESCE(provisional, 0), depth FROM pyramid_nodes WHERE slug = ?1 AND id = ?2",
                rusqlite::params![node.slug, node.id],
                |r| Ok((r.get::<_, i64>(0)? != 0, r.get::<_, i64>(1)?)),
            )?;
        if is_provisional {
            mutate_provisional_node(conn, &node.slug, &node.id, node)?;
        } else {
            // WS-IMMUTABILITY-ENFORCE: bedrock L0/L1 nodes (depth <= 1) are
            // permanently immutable once written canonically. Only provisional
            // nodes at these depths may be mutated (handled above).
            if existing_depth <= 1 {
                return Err(anyhow::anyhow!(
                    "Cannot mutate immutable bedrock node {} at depth {}",
                    node.id,
                    existing_depth
                ));
            }
            // Default rebuild phase/reason. Callers that need a different
            // reason (delta, collapse, promotion, agent_writeback,
            // stale_refresh) should invoke apply_supersession directly.
            apply_supersession(conn, &node.slug, &node.id, node, "rebuild", "rebuild", "")?;
        }
        return Ok(());
    }

    // First write: plain INSERT path.
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

    let (time_range_start, time_range_end) = match &node.time_range {
        Some(tr) => (tr.start.clone(), tr.end.clone()),
        None => (None, None),
    };
    let narrative_json = serde_json::to_string(&node.narrative)?;
    let entities_json = serde_json::to_string(&node.entities)?;
    let key_quotes_json = serde_json::to_string(&node.key_quotes)?;
    let transitions_json = serde_json::to_string(&node.transitions)?;
    let provisional_i: i64 = if node.provisional { 1 } else { 0 };
    let weight = if node.weight == 0.0 { 1.0 } else { node.weight };

    conn.execute(
        "INSERT INTO pyramid_nodes
            (id, slug, depth, chunk_index, headline, distilled, topics, corrections, decisions,
             terms, dead_ends, self_prompt, children, parent_id, superseded_by, build_id,
             time_range_start, time_range_end, weight, provisional, promoted_from,
             narrative_json, entities_json, key_quotes_json, transitions_json,
             current_version, current_version_chain_phase)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                 ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27)",
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
            node.build_id,
            time_range_start,
            time_range_end,
            weight,
            provisional_i,
            node.promoted_from,
            narrative_json,
            entities_json,
            key_quotes_json,
            transitions_json,
            1_i64,
            node.current_version_chain_phase,
        ],
    )?;

    Ok(())
}

/// WS-SCHEMA-V2 (§15.7): Apply a per-contribution supersession.
///
/// Snapshots the CURRENT row of `pyramid_nodes(slug, id)` into
/// `pyramid_node_versions` at its current version, then UPDATEs the live row
/// in place with the new content and bumps `current_version`. Runs inside a
/// single SAVEPOINT so the snapshot+update is atomic and nests safely inside
/// an outer transaction.
///
/// Distinct from the legacy `supersede_node` (per-build-sweep tombstoning).
/// Both coexist: this helper is per-contribution; the legacy one is per-build.
///
/// `supersession_reason` is one of: "delta" | "collapse" | "rebuild" |
/// "promotion" | "agent_writeback" | "stale_refresh".
///
/// Bedrock immutability enforcement (§15.7) is a per-chain policy applied
/// by callers (composition delta) based on chain configuration before
/// invoking this helper. The helper itself is the mechanical snapshot-then-
/// update primitive.
///
/// Returns the NEW current_version number assigned to the live row.
pub fn apply_supersession(
    conn: &Connection,
    slug: &str,
    node_id: &str,
    new_node: &PyramidNode,
    chain_phase: &str,
    supersession_reason: &str,
    _requesting_chain_id: &str,
) -> Result<i64> {
    // WS-IMMUTABILITY-ENFORCE: bedrock L0/L1 nodes (depth <= 1) are permanently
    // immutable once written canonically. Reject any supersession attempt on
    // a canonical bedrock node. Provisional nodes at these depths are handled
    // by mutate_provisional_node, not this function.
    {
        let (existing_depth, is_provisional): (i64, bool) = conn
            .query_row(
                "SELECT depth, COALESCE(provisional, 0) FROM pyramid_nodes WHERE slug = ?1 AND id = ?2",
                rusqlite::params![slug, node_id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? != 0)),
            )
            .map_err(|e| anyhow::anyhow!("apply_supersession: lookup failed for ({slug}, {node_id}): {e}"))?;
        if existing_depth <= 1 && !is_provisional {
            return Err(anyhow::anyhow!(
                "Cannot mutate immutable bedrock node {} at depth {}",
                node_id,
                existing_depth
            ));
        }
    }

    let savepoint_name = "apply_supersession_sp";
    conn.execute_batch(&format!("SAVEPOINT {savepoint_name};"))?;

    let result: Result<i64> = (|| {
        // 1. Snapshot the current row into pyramid_node_versions, using its
        //    existing current_version as the snapshot version number.
        let snapshot_rows = conn.execute(
            "INSERT INTO pyramid_node_versions (
                slug, node_id, version,
                headline, distilled, topics, corrections, decisions,
                terms, dead_ends, self_prompt, children, parent_id,
                time_range_start, time_range_end, weight,
                narrative_json, entities_json, key_quotes_json, transitions_json,
                chain_phase, build_id, supersession_reason
             )
             SELECT
                slug, id, COALESCE(current_version, 1),
                headline, distilled, topics, corrections, decisions,
                terms, dead_ends, self_prompt, children, parent_id,
                time_range_start, time_range_end, weight,
                narrative_json, entities_json, key_quotes_json, transitions_json,
                COALESCE(current_version_chain_phase, ''), build_id, ?3
             FROM pyramid_nodes
             WHERE slug = ?1 AND id = ?2",
            rusqlite::params![slug, node_id, supersession_reason],
        )?;
        if snapshot_rows == 0 {
            return Err(anyhow::anyhow!(
                "apply_supersession: no row for ({slug}, {node_id})"
            ));
        }

        // 2. Serialize new content for the UPDATE.
        let topics = serde_json::to_string(&new_node.topics)?;
        let corrections = serde_json::to_string(&new_node.corrections)?;
        let decisions = serde_json::to_string(&new_node.decisions)?;
        let terms = serde_json::to_string(&new_node.terms)?;
        let dead_ends = serde_json::to_string(&new_node.dead_ends)?;
        let children = serde_json::to_string(&new_node.children)?;
        let headline = clean_headline(&new_node.headline)
            .unwrap_or_else(|| headline_for_node(new_node, None));
        let (time_range_start, time_range_end) = match &new_node.time_range {
            Some(tr) => (tr.start.clone(), tr.end.clone()),
            None => (None, None),
        };
        let narrative_json = serde_json::to_string(&new_node.narrative)?;
        let entities_json = serde_json::to_string(&new_node.entities)?;
        let key_quotes_json = serde_json::to_string(&new_node.key_quotes)?;
        let transitions_json = serde_json::to_string(&new_node.transitions)?;
        let weight = if new_node.weight == 0.0 { 1.0 } else { new_node.weight };

        // 3. UPDATE the live row. Clears the legacy `superseded_by` tombstone
        //    so a previously build-swept row doesn't ghost out of
        //    live_pyramid_nodes after this write.
        let updated = conn.execute(
            "UPDATE pyramid_nodes SET
                depth = ?3,
                chunk_index = ?4,
                headline = ?5,
                distilled = ?6,
                topics = ?7,
                corrections = ?8,
                decisions = ?9,
                terms = ?10,
                dead_ends = ?11,
                self_prompt = ?12,
                children = ?13,
                parent_id = ?14,
                build_id = COALESCE(?15, build_id),
                time_range_start = ?16,
                time_range_end = ?17,
                weight = ?18,
                promoted_from = COALESCE(?19, promoted_from),
                narrative_json = ?20,
                entities_json = ?21,
                key_quotes_json = ?22,
                transitions_json = ?23,
                current_version = COALESCE(current_version, 1) + 1,
                current_version_chain_phase = ?24,
                superseded_by = NULL,
                build_version = build_version + 1
             WHERE slug = ?1 AND id = ?2",
            rusqlite::params![
                slug,
                node_id,
                new_node.depth,
                new_node.chunk_index,
                headline,
                new_node.distilled,
                topics,
                corrections,
                decisions,
                terms,
                dead_ends,
                new_node.self_prompt,
                children,
                new_node.parent_id,
                new_node.build_id,
                time_range_start,
                time_range_end,
                weight,
                new_node.promoted_from,
                narrative_json,
                entities_json,
                key_quotes_json,
                transitions_json,
                chain_phase,
            ],
        )?;
        if updated == 0 {
            return Err(anyhow::anyhow!(
                "apply_supersession: UPDATE matched 0 rows for ({slug}, {node_id})"
            ));
        }

        // 4. Return the new current_version.
        let new_version: i64 = conn.query_row(
            "SELECT current_version FROM pyramid_nodes WHERE slug = ?1 AND id = ?2",
            rusqlite::params![slug, node_id],
            |r| r.get(0),
        )?;
        Ok(new_version)
    })();

    match result {
        Ok(v) => {
            conn.execute_batch(&format!("RELEASE SAVEPOINT {savepoint_name};"))?;
            Ok(v)
        }
        Err(e) => {
            let _ = conn.execute_batch(&format!(
                "ROLLBACK TO SAVEPOINT {savepoint_name}; RELEASE SAVEPOINT {savepoint_name};"
            ));
            Err(e)
        }
    }
}

/// WS-SCHEMA-V2 (§15.7): Mutate a provisional node in place WITHOUT writing
/// to the versions table. Provisional history isn't durable — only the
/// promotion-to-canonical transition is versioned, via `apply_supersession`
/// with `supersession_reason = "promotion"`.
///
/// Returns the number of rows updated (0 if the target row isn't
/// provisional, 1 on success).
pub fn mutate_provisional_node(
    conn: &Connection,
    slug: &str,
    node_id: &str,
    new_node: &PyramidNode,
) -> Result<usize> {
    let topics = serde_json::to_string(&new_node.topics)?;
    let corrections = serde_json::to_string(&new_node.corrections)?;
    let decisions = serde_json::to_string(&new_node.decisions)?;
    let terms = serde_json::to_string(&new_node.terms)?;
    let dead_ends = serde_json::to_string(&new_node.dead_ends)?;
    let children = serde_json::to_string(&new_node.children)?;
    let headline = clean_headline(&new_node.headline)
        .unwrap_or_else(|| headline_for_node(new_node, None));
    let (time_range_start, time_range_end) = match &new_node.time_range {
        Some(tr) => (tr.start.clone(), tr.end.clone()),
        None => (None, None),
    };
    let narrative_json = serde_json::to_string(&new_node.narrative)?;
    let entities_json = serde_json::to_string(&new_node.entities)?;
    let key_quotes_json = serde_json::to_string(&new_node.key_quotes)?;
    let transitions_json = serde_json::to_string(&new_node.transitions)?;
    let weight = if new_node.weight == 0.0 { 1.0 } else { new_node.weight };

    let updated = conn.execute(
        "UPDATE pyramid_nodes SET
            depth = ?3,
            chunk_index = ?4,
            headline = ?5,
            distilled = ?6,
            topics = ?7,
            corrections = ?8,
            decisions = ?9,
            terms = ?10,
            dead_ends = ?11,
            self_prompt = ?12,
            children = ?13,
            parent_id = ?14,
            time_range_start = ?15,
            time_range_end = ?16,
            weight = ?17,
            narrative_json = ?18,
            entities_json = ?19,
            key_quotes_json = ?20,
            transitions_json = ?21
         WHERE slug = ?1 AND id = ?2 AND provisional = 1",
        rusqlite::params![
            slug,
            node_id,
            new_node.depth,
            new_node.chunk_index,
            headline,
            new_node.distilled,
            topics,
            corrections,
            decisions,
            terms,
            dead_ends,
            new_node.self_prompt,
            children,
            new_node.parent_id,
            time_range_start,
            time_range_end,
            weight,
            narrative_json,
            entities_json,
            key_quotes_json,
            transitions_json,
        ],
    )?;
    Ok(updated)
}

/// WS-IMMUTABILITY-ENFORCE: Promote a provisional node to canonical status.
///
/// Sets `provisional = 0` on the node. After promotion, the node becomes
/// permanently immutable if it is at depth <= 1 (bedrock L0/L1 freeze).
///
/// Returns `true` if the node was provisional and got promoted, `false` if
/// the node was not provisional (already canonical or does not exist).
///
/// Does NOT emit events — WS-EVENTS owns event emission. The caller is
/// responsible for emitting `ProvisionalNodePromoted` after a successful
/// promotion.
pub fn promote_provisional_node(conn: &Connection, slug: &str, node_id: &str) -> Result<bool> {
    let updated = conn.execute(
        "UPDATE pyramid_nodes SET provisional = 0 WHERE slug = ?1 AND id = ?2 AND provisional = 1",
        rusqlite::params![slug, node_id],
    )?;
    Ok(updated > 0)
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

/// Lightweight node fetch — only the columns needed for drill child display.
/// Skips heavy JSON columns (topics, corrections, decisions, terms, dead_ends).
pub fn get_node_summary(
    conn: &Connection,
    slug: &str,
    node_id: &str,
) -> Result<Option<PyramidNode>> {
    let sql = "SELECT id, slug, depth, chunk_index, headline, distilled, \
               '[]' as topics, '[]' as corrections, '[]' as decisions, \
               '[]' as terms, '[]' as dead_ends, self_prompt, children, parent_id, superseded_by, build_id, created_at \
               FROM pyramid_nodes WHERE slug = ?1 AND id = ?2";
    let mut stmt = conn.prepare(sql)?;
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

/// Get ALL live (non-superseded) nodes for a slug, across all depths.
/// Used by cross-slug loading when a question slug references another question slug
/// and needs all answer nodes as source material.
pub fn get_all_live_nodes(conn: &Connection, slug: &str) -> Result<Vec<PyramidNode>> {
    let sql = format!(
        "SELECT {NODE_SELECT_COLS} FROM live_pyramid_nodes
         WHERE slug = ?1
         ORDER BY depth ASC, chunk_index ASC, id ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![slug], node_from_row)?;

    let mut nodes = Vec::new();
    for row in rows {
        nodes.push(row?);
    }
    Ok(nodes)
}

pub fn get_node_id_by_depth_and_chunk_index(
    conn: &Connection,
    slug: &str,
    depth: i64,
    chunk_index: i64,
) -> Result<Option<String>> {
    let result = conn.query_row(
        "SELECT id FROM live_pyramid_nodes
         WHERE slug = ?1 AND depth = ?2 AND chunk_index = ?3
         ORDER BY build_version DESC, id ASC
         LIMIT 1",
        rusqlite::params![slug, depth, chunk_index],
        |row| row.get::<_, String>(0),
    );

    match result {
        Ok(id) => Ok(Some(id)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn get_node_id_by_depth_and_headline(
    conn: &Connection,
    slug: &str,
    depth: i64,
    headline: &str,
) -> Result<Option<String>> {
    let result = conn.query_row(
        "SELECT id FROM live_pyramid_nodes
         WHERE slug = ?1 AND depth = ?2 AND headline = ?3
         ORDER BY build_version DESC, id ASC
         LIMIT 1",
        rusqlite::params![slug, depth, headline],
        |row| row.get::<_, String>(0),
    );

    match result {
        Ok(id) => Ok(Some(id)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
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
/// 11-M: Deprecated — use `supersede_nodes_above` instead. Retained for backward compat.
#[deprecated(
    note = "Use supersede_nodes_above instead — delete_nodes_above destroys contributions"
)]
pub fn delete_nodes_above(conn: &Connection, slug: &str, depth: i64) -> Result<i64> {
    let deleted = conn.execute(
        "DELETE FROM pyramid_nodes WHERE slug = ?1 AND depth > ?2",
        rusqlite::params![slug, depth],
    )?;
    Ok(deleted as i64)
}

// ── Supersession functions (WS3: Everything is a Contribution) ──────────────

/// Supersede all live nodes above a given depth by setting superseded_by = build_id.
/// Returns count of superseded nodes.
pub fn supersede_nodes_above(
    conn: &Connection,
    slug: &str,
    depth: i64,
    build_id: &str,
) -> Result<i64> {
    let count = conn.execute(
        "UPDATE pyramid_nodes SET superseded_by = ?3
         WHERE slug = ?1 AND depth > ?2 AND superseded_by IS NULL",
        rusqlite::params![slug, depth, build_id],
    )?;
    Ok(count as i64)
}

/// Supersede all live nodes at or above a given depth by setting superseded_by = build_id.
/// Returns count of superseded nodes.
pub fn supersede_nodes_at_and_above(
    conn: &Connection,
    slug: &str,
    depth: i64,
    build_id: &str,
) -> Result<i64> {
    let count = conn.execute(
        "UPDATE pyramid_nodes SET superseded_by = ?3
         WHERE slug = ?1 AND depth >= ?2 AND superseded_by IS NULL",
        rusqlite::params![slug, depth, build_id],
    )?;
    Ok(count as i64)
}

/// Supersede a single node by setting superseded_by = build_id.
pub fn supersede_node(conn: &Connection, slug: &str, node_id: &str, build_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_nodes SET superseded_by = ?3
         WHERE slug = ?1 AND id = ?2 AND superseded_by IS NULL",
        rusqlite::params![slug, node_id, build_id],
    )?;
    Ok(())
}

/// Supersede ALL live nodes for a slug (full rebuild / partial-fail cleanup).
/// Returns count of superseded nodes.
pub fn supersede_all_nodes(conn: &Connection, slug: &str, build_id: &str) -> Result<i64> {
    let count = conn.execute(
        "UPDATE pyramid_nodes SET superseded_by = ?2
         WHERE slug = ?1 AND superseded_by IS NULL",
        rusqlite::params![slug, build_id],
    )?;
    Ok(count as i64)
}

/// Supersede live nodes matching a headline pattern at a given depth.
/// Returns count of superseded nodes.
pub fn supersede_nodes_by_headline_pattern(
    conn: &Connection,
    slug: &str,
    depth: i64,
    pattern1: &str,
    pattern2: &str,
    build_id: &str,
) -> Result<i64> {
    let count = conn.execute(
        "UPDATE pyramid_nodes SET superseded_by = ?4
         WHERE slug = ?1 AND depth = ?2 AND superseded_by IS NULL
           AND (headline LIKE ?3 OR headline LIKE ?5)",
        rusqlite::params![slug, depth, pattern1, build_id, pattern2],
    )?;
    Ok(count as i64)
}

/// Get a live node (non-superseded) by slug and node ID. Returns None if not found or superseded.
pub fn get_live_node(conn: &Connection, slug: &str, node_id: &str) -> Result<Option<PyramidNode>> {
    let sql =
        format!("SELECT {NODE_SELECT_COLS} FROM live_pyramid_nodes WHERE slug = ?1 AND id = ?2");
    let mut stmt = conn.prepare(&sql)?;
    let result = stmt.query_row(rusqlite::params![slug, node_id], node_from_row);
    match result {
        Ok(node) => Ok(Some(node)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
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

/// Get the output_json for one exact step record.
pub fn get_step_output_exact(
    conn: &Connection,
    slug: &str,
    step_type: &str,
    chunk_index: i64,
    depth: i64,
    node_id: &str,
) -> Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT output_json FROM pyramid_pipeline_steps
         WHERE slug = ?1 AND step_type = ?2 AND chunk_index = ?3 AND depth = ?4 AND node_id = ?5
         LIMIT 1",
    )?;

    let result = stmt.query_row(
        rusqlite::params![slug, step_type, chunk_index, depth, node_id],
        |row| row.get::<_, Option<String>>(0),
    );

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

/// Get all active (non-archived) web edges for a slug.
pub fn get_web_edges(conn: &Connection, slug: &str) -> Result<Vec<WebEdge>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, thread_a_id, thread_b_id, relationship, relevance, delta_count, build_id, created_at, updated_at
         FROM pyramid_web_edges WHERE slug = ?1 AND archived_at IS NULL ORDER BY relevance DESC",
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
            build_id: row.get(7)?,
            created_at: row.get(8)?,
            updated_at: row.get(9)?,
        })
    })?;

    let mut edges = Vec::new();
    for row in rows {
        edges.push(row?);
    }
    Ok(edges)
}

/// Save (upsert) a web edge. Returns the row ID.
/// Writes build_id and last_confirmed_at for contribution-model scoping.
pub fn save_web_edge(conn: &Connection, edge: &WebEdge) -> Result<i64> {
    conn.execute(
        "INSERT INTO pyramid_web_edges (slug, thread_a_id, thread_b_id, relationship, relevance, delta_count, build_id, last_confirmed_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'), datetime('now'))
         ON CONFLICT(slug, thread_a_id, thread_b_id) DO UPDATE SET
            relationship = excluded.relationship,
            relevance = excluded.relevance,
            delta_count = excluded.delta_count,
            build_id = excluded.build_id,
            last_confirmed_at = datetime('now'),
            archived_at = NULL,
            updated_at = excluded.updated_at",
        rusqlite::params![
            edge.slug,
            edge.thread_a_id,
            edge.thread_b_id,
            edge.relationship,
            edge.relevance,
            edge.delta_count,
            edge.build_id,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// No-op: web edges are now scoped by build_id (WS-ONLINE-S3).
///
/// Previously this deleted all web edges for a given depth before re-inserting.
/// With the contribution model, new edges are upserted with the current build_id
/// via `save_web_edge`. Old edges with stale build_ids persist as historical
/// records and are eventually archived by `decay_web_edges`.
///
/// Retained with original signature to avoid breaking callers during transition.
pub fn delete_web_edges_for_depth(_conn: &Connection, _slug: &str, _depth: i64) -> Result<usize> {
    Ok(0)
}

/// Get a single active (non-archived) web edge between two threads (normalized order: a < b).
pub fn get_web_edge_between(
    conn: &Connection,
    slug: &str,
    thread_a_id: &str,
    thread_b_id: &str,
) -> Result<Option<WebEdge>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, thread_a_id, thread_b_id, relationship, relevance, delta_count, build_id, created_at, updated_at
         FROM pyramid_web_edges WHERE slug = ?1 AND thread_a_id = ?2 AND thread_b_id = ?3 AND archived_at IS NULL",
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
            build_id: row.get(7)?,
            created_at: row.get(8)?,
            updated_at: row.get(9)?,
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

/// No-op: web edge deltas are now scoped by build_id (WS-ONLINE-S3).
///
/// Previously this deleted all deltas for an edge after collapse absorbed them.
/// With the contribution model, deltas persist as historical records. The
/// edge's `delta_count` is reset to 0 by `update_web_edge` during collapse,
/// which is sufficient for collapse-threshold tracking. Retained with original
/// signature to avoid breaking callers.
pub fn delete_web_edge_deltas(_conn: &Connection, _edge_id: i64) -> Result<usize> {
    Ok(0)
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

/// Get a web edge by its ID (includes archived edges — used by collapse logic).
pub fn get_web_edge(conn: &Connection, edge_id: i64) -> Result<Option<WebEdge>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, thread_a_id, thread_b_id, relationship, relevance, delta_count, build_id, created_at, updated_at
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
            build_id: row.get(7)?,
            created_at: row.get(8)?,
            updated_at: row.get(9)?,
        })
    })?;

    match rows.next() {
        Some(Ok(edge)) => Ok(Some(edge)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

/// Decay all active web edges for a slug by reducing relevance. Returns count of archived edges.
///
/// Edges that drop below 0.1 relevance are archived (not deleted) by setting `archived_at`.
/// The `last_confirmed_at` guard prevents valid edges on quiet pyramids from decaying to
/// zero: edges confirmed within the last 7 days are exempt from archival even if their
/// relevance has decayed below the threshold.
pub fn decay_web_edges(conn: &Connection, slug: &str, decay_rate: f64) -> Result<usize> {
    // Reduce relevance on all active (non-archived) edges
    conn.execute(
        "UPDATE pyramid_web_edges SET relevance = relevance - ?1, updated_at = datetime('now')
         WHERE slug = ?2 AND archived_at IS NULL",
        rusqlite::params![decay_rate, slug],
    )?;

    // Archive edges that dropped below threshold AND haven't been confirmed recently
    let archived_count = conn.execute(
        "UPDATE pyramid_web_edges SET archived_at = datetime('now')
         WHERE slug = ?1 AND archived_at IS NULL AND relevance < 0.1
           AND (last_confirmed_at IS NULL OR last_confirmed_at < datetime('now', '-7 days'))",
        rusqlite::params![slug],
    )?;

    Ok(archived_count)
}

/// Get active (non-archived) web edges above a minimum relevance threshold.
pub fn get_active_edges(conn: &Connection, slug: &str, min_relevance: f64) -> Result<Vec<WebEdge>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, thread_a_id, thread_b_id, relationship, relevance, delta_count, build_id, created_at, updated_at
         FROM pyramid_web_edges WHERE slug = ?1 AND relevance >= ?2 AND archived_at IS NULL ORDER BY relevance DESC",
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
            build_id: row.get(7)?,
            created_at: row.get(8)?,
            updated_at: row.get(9)?,
        })
    })?;

    let mut edges = Vec::new();
    for row in rows {
        edges.push(row?);
    }
    Ok(edges)
}

// ── Remote Web Edge CRUD (WS-ONLINE-F) ──────────────────────────────────────

/// Save (upsert) a remote web edge. Returns the row ID.
///
/// Remote web edges reference nodes on other pyramids via Wire handle-paths.
/// Scoped by build_id — each build writes its own edges.
pub fn save_remote_web_edge(conn: &Connection, edge: &RemoteWebEdge) -> Result<i64> {
    conn.execute(
        "INSERT INTO pyramid_remote_web_edges (slug, local_thread_id, remote_handle_path, remote_tunnel_url, relationship, relevance, delta_count, build_id, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, datetime('now'))
         ON CONFLICT(slug, local_thread_id, remote_handle_path, build_id) DO UPDATE SET
            remote_tunnel_url = excluded.remote_tunnel_url,
            relationship = excluded.relationship,
            relevance = excluded.relevance,
            delta_count = excluded.delta_count,
            updated_at = datetime('now')",
        rusqlite::params![
            edge.slug,
            edge.local_thread_id,
            edge.remote_handle_path,
            edge.remote_tunnel_url,
            edge.relationship,
            edge.relevance,
            edge.delta_count,
            edge.build_id,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Get remote web edges for a specific slug and build_id.
pub fn get_remote_web_edges(
    conn: &Connection,
    slug: &str,
    build_id: &str,
) -> Result<Vec<RemoteWebEdge>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, local_thread_id, remote_handle_path, remote_tunnel_url, relationship, relevance, delta_count, build_id, created_at, updated_at
         FROM pyramid_remote_web_edges WHERE slug = ?1 AND build_id = ?2 ORDER BY relevance DESC",
    )?;

    let rows = stmt.query_map(rusqlite::params![slug, build_id], |row| {
        Ok(RemoteWebEdge {
            id: row.get(0)?,
            slug: row.get(1)?,
            local_thread_id: row.get(2)?,
            remote_handle_path: row.get(3)?,
            remote_tunnel_url: row.get(4)?,
            relationship: row.get(5)?,
            relevance: row.get(6)?,
            delta_count: row.get(7)?,
            build_id: row.get(8)?,
            created_at: row.get(9)?,
            updated_at: row.get(10)?,
        })
    })?;

    let mut edges = Vec::new();
    for row in rows {
        edges.push(row?);
    }
    Ok(edges)
}

/// Get all remote web edges for a slug across all builds (for display).
pub fn get_all_remote_web_edges(conn: &Connection, slug: &str) -> Result<Vec<RemoteWebEdge>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, local_thread_id, remote_handle_path, remote_tunnel_url, relationship, relevance, delta_count, build_id, created_at, updated_at
         FROM pyramid_remote_web_edges WHERE slug = ?1 ORDER BY build_id DESC, relevance DESC",
    )?;

    let rows = stmt.query_map(rusqlite::params![slug], |row| {
        Ok(RemoteWebEdge {
            id: row.get(0)?,
            slug: row.get(1)?,
            local_thread_id: row.get(2)?,
            remote_handle_path: row.get(3)?,
            remote_tunnel_url: row.get(4)?,
            relationship: row.get(5)?,
            relevance: row.get(6)?,
            delta_count: row.get(7)?,
            build_id: row.get(8)?,
            created_at: row.get(9)?,
            updated_at: row.get(10)?,
        })
    })?;

    let mut edges = Vec::new();
    for row in rows {
        edges.push(row?);
    }
    Ok(edges)
}

/// Get the tunnel URL for a pinned slug (if any).
pub fn get_slug_tunnel_url(conn: &Connection, slug: &str) -> Result<Option<String>> {
    let result = conn.query_row(
        "SELECT source_tunnel_url FROM pyramid_slugs WHERE slug = ?1",
        rusqlite::params![slug],
        |row| row.get::<_, Option<String>>(0),
    );

    match result {
        Ok(url) => Ok(url),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Get remote web edges for a specific local thread.
pub fn get_remote_web_edges_for_thread(
    conn: &Connection,
    slug: &str,
    local_thread_id: &str,
) -> Result<Vec<RemoteWebEdge>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, local_thread_id, remote_handle_path, remote_tunnel_url, relationship, relevance, delta_count, build_id, created_at, updated_at
         FROM pyramid_remote_web_edges WHERE slug = ?1 AND local_thread_id = ?2 ORDER BY relevance DESC",
    )?;

    let rows = stmt.query_map(rusqlite::params![slug, local_thread_id], |row| {
        Ok(RemoteWebEdge {
            id: row.get(0)?,
            slug: row.get(1)?,
            local_thread_id: row.get(2)?,
            remote_handle_path: row.get(3)?,
            remote_tunnel_url: row.get(4)?,
            relationship: row.get(5)?,
            relevance: row.get(6)?,
            delta_count: row.get(7)?,
            build_id: row.get(8)?,
            created_at: row.get(9)?,
            updated_at: row.get(10)?,
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
/// 11-D: Checks node liveness — blocks annotation on superseded nodes and returns
/// an error with the successor ID so callers can redirect.
pub fn save_annotation(
    conn: &Connection,
    annotation: &PyramidAnnotation,
) -> Result<PyramidAnnotation> {
    // 11-D: Check if the target node is superseded
    if !annotation.node_id.is_empty() {
        let superseded: Option<String> = conn
            .query_row(
                "SELECT superseded_by FROM pyramid_nodes WHERE slug = ?1 AND id = ?2",
                rusqlite::params![annotation.slug, annotation.node_id],
                |row| row.get(0),
            )
            .ok()
            .flatten();
        if let Some(successor_id) = superseded {
            return Err(anyhow::anyhow!(
                "Cannot annotate superseded node '{}'. Successor: '{}'",
                annotation.node_id,
                successor_id
            ));
        }
    }

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
        let stale_val: i32 = match stale_str {
            "yes" | "true" | "1" => 1,
            "no" | "false" | "0" => 0,
            "new" | "2" => 2,
            "deleted" | "3" => 3,
            "renamed" | "4" => 4,
            "skipped" | "5" => 5,
            _ => 0,
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
                "stale": match row.get::<_, i32>(5)? {
                    0 => "no",
                    1 => "yes",
                    2 => "new",
                    3 => "deleted",
                    4 => "renamed",
                    5 => "skipped",
                    _ => "unknown",
                },
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

/// Insert a cost log entry with full P1.5 observatory columns.
///
/// All new columns are optional — pass `None` for fields not available in
/// the current call context. This is the canonical write path for the chain
/// executor and any future callers that have chain/step/tier metadata.
pub fn insert_cost_log(
    conn: &Connection,
    slug: &str,
    operation: &str,
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
    estimated_cost: f64,
    source: &str,
    layer: Option<i32>,
    check_type: Option<&str>,
    chain_id: Option<&str>,
    step_name: Option<&str>,
    tier: Option<&str>,
    latency_ms: Option<i64>,
    generation_id: Option<&str>,
    estimated_cost_usd: Option<f64>,
) -> Result<()> {
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    conn.execute(
        "INSERT INTO pyramid_cost_log (slug, operation, model, input_tokens, output_tokens,
         estimated_cost, source, layer, check_type, created_at,
         chain_id, step_name, tier, latency_ms, generation_id, estimated_cost_usd)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        rusqlite::params![
            slug,
            operation,
            model,
            input_tokens,
            output_tokens,
            estimated_cost,
            source,
            layer,
            check_type,
            now,
            chain_id,
            step_name,
            tier,
            latency_ms,
            generation_id,
            estimated_cost_usd,
        ],
    )?;
    Ok(())
}

// ── LLM Audit Trail CRUD (Live Pyramid Theatre) ────────────────────────────

/// Insert a pending audit row BEFORE an LLM call. Returns the row id.
///
/// System prompts are deduplicated via `pyramid_prompt_store` — the audit row
/// stores a hash reference instead of the full text when the prompt already
/// exists. This saves significant space since system prompts repeat across
/// every node in a build.
pub fn insert_llm_audit_pending(
    conn: &Connection,
    slug: &str,
    build_id: &str,
    node_id: Option<&str>,
    step_name: &str,
    call_purpose: &str,
    depth: Option<i64>,
    model: &str,
    system_prompt: &str,
    user_prompt: &str,
) -> Result<i64> {
    // Deduplicate system prompt via hash
    let sys_hash = prompt_hash(system_prompt);
    let _ = conn.execute(
        "INSERT OR IGNORE INTO pyramid_prompt_store (hash, content, char_count)
         VALUES (?1, ?2, ?3)",
        rusqlite::params![sys_hash, system_prompt, system_prompt.len() as i64],
    );

    // Store hash reference as system_prompt in audit row
    let sys_ref = format!("@@hash:{}", sys_hash);
    conn.execute(
        "INSERT INTO pyramid_llm_audit
         (slug, build_id, node_id, step_name, call_purpose, depth, model,
          system_prompt, user_prompt, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending')",
        rusqlite::params![
            slug, build_id, node_id, step_name, call_purpose, depth,
            model, sys_ref, user_prompt,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// SHA-256 hash of a prompt string, returned as hex.
fn prompt_hash(text: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Complete an audit row AFTER the LLM call returns.
pub fn complete_llm_audit(
    conn: &Connection,
    audit_id: i64,
    raw_response: &str,
    parsed_ok: bool,
    prompt_tokens: i64,
    completion_tokens: i64,
    latency_ms: i64,
    generation_id: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_llm_audit SET
         raw_response = ?1, parsed_ok = ?2,
         prompt_tokens = ?3, completion_tokens = ?4,
         latency_ms = ?5, generation_id = ?6,
         status = 'complete', completed_at = datetime('now')
         WHERE id = ?7",
        rusqlite::params![
            raw_response, parsed_ok as i32,
            prompt_tokens, completion_tokens,
            latency_ms, generation_id, audit_id,
        ],
    )?;
    Ok(())
}

/// Mark an audit row as failed (LLM error).
pub fn fail_llm_audit(
    conn: &Connection,
    audit_id: i64,
    error_message: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_llm_audit SET
         raw_response = ?1, status = 'failed', completed_at = datetime('now')
         WHERE id = ?2",
        rusqlite::params![error_message, audit_id],
    )?;
    Ok(())
}

/// Get all audit records for a specific node in a build.
/// Hash-referenced system prompts are resolved to full text.
pub fn get_node_audit_records(
    conn: &Connection,
    slug: &str,
    node_id: &str,
) -> Result<Vec<LlmAuditRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, build_id, node_id, step_name, call_purpose, depth, model,
                system_prompt, user_prompt, raw_response, parsed_ok,
                prompt_tokens, completion_tokens, latency_ms, generation_id,
                status, created_at, completed_at
         FROM pyramid_llm_audit
         WHERE slug = ?1 AND node_id = ?2
         ORDER BY id ASC",
    )?;
    let mut rows: Vec<LlmAuditRecord> = stmt
        .query_map(rusqlite::params![slug, node_id], parse_llm_audit_row)?
        .filter_map(|r| r.ok())
        .collect();
    resolve_prompt_hashes(conn, &mut rows);
    Ok(rows)
}

/// Get a single audit record by id.
/// Hash-referenced system prompts are resolved to full text.
pub fn get_llm_audit_by_id(
    conn: &Connection,
    audit_id: i64,
) -> Result<Option<LlmAuditRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, build_id, node_id, step_name, call_purpose, depth, model,
                system_prompt, user_prompt, raw_response, parsed_ok,
                prompt_tokens, completion_tokens, latency_ms, generation_id,
                status, created_at, completed_at
         FROM pyramid_llm_audit WHERE id = ?1",
    )?;
    let mut rows: Vec<LlmAuditRecord> = stmt
        .query_map(rusqlite::params![audit_id], parse_llm_audit_row)?
        .filter_map(|r| r.ok())
        .collect();
    resolve_prompt_hashes(conn, &mut rows);
    Ok(rows.into_iter().next())
}

/// Get all live nodes for a build (for the Theatre's spatial view).
/// Returns nodes with parent_id for tree construction.
pub fn get_build_live_nodes(
    conn: &Connection,
    slug: &str,
    _build_id: &str,
) -> Result<Vec<LiveNodeInfo>> {
    let mut stmt = conn.prepare(
        "SELECT n.id, n.depth, n.headline, n.parent_id, n.children,
                CASE WHEN n.superseded_by IS NOT NULL THEN 'superseded'
                     WHEN n.distilled = '' THEN 'pending'
                     ELSE 'complete' END AS status
         FROM pyramid_nodes n
         WHERE n.slug = ?1 AND n.build_version > 0
         ORDER BY n.depth ASC, n.id ASC",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![slug], |row| {
            let children_json: String = row.get::<_, Option<String>>(4)?.unwrap_or_default();
            let children: Vec<String> = serde_json::from_str(&children_json).unwrap_or_default();
            Ok(LiveNodeInfo {
                node_id: row.get(0)?,
                depth: row.get(1)?,
                headline: row.get(2)?,
                parent_id: row.get::<_, Option<String>>(3)?,
                children,
                status: row.get(5)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

/// Delete audit records for all builds EXCEPT the latest for each slug.
pub fn cleanup_old_audit_records(conn: &Connection, slug: &str) -> Result<i64> {
    let latest_build_id: Option<String> = conn
        .query_row(
            "SELECT build_id FROM pyramid_llm_audit WHERE slug = ?1
             ORDER BY id DESC LIMIT 1",
            rusqlite::params![slug],
            |row| row.get(0),
        )
        .ok();
    let deleted = match latest_build_id {
        Some(bid) => conn.execute(
            "DELETE FROM pyramid_llm_audit WHERE slug = ?1 AND build_id != ?2",
            rusqlite::params![slug, bid],
        )?,
        None => 0,
    };
    Ok(deleted as i64)
}

fn parse_llm_audit_row(row: &rusqlite::Row) -> rusqlite::Result<LlmAuditRecord> {
    Ok(LlmAuditRecord {
        id: row.get(0)?,
        slug: row.get(1)?,
        build_id: row.get(2)?,
        node_id: row.get(3)?,
        step_name: row.get(4)?,
        call_purpose: row.get(5)?,
        depth: row.get(6)?,
        model: row.get(7)?,
        system_prompt: row.get(8)?, // may be "@@hash:..." — resolved at read time
        user_prompt: row.get(9)?,
        raw_response: row.get(10)?,
        parsed_ok: row.get::<_, i32>(11)? != 0,
        prompt_tokens: row.get(12)?,
        completion_tokens: row.get(13)?,
        latency_ms: row.get(14)?,
        generation_id: row.get(15)?,
        status: row.get(16)?,
        created_at: row.get(17)?,
        completed_at: row.get(18)?,
    })
}

/// Resolve hash references in audit records (system_prompt "@@hash:..." → full text).
fn resolve_prompt_hashes(conn: &Connection, records: &mut [LlmAuditRecord]) {
    for record in records.iter_mut() {
        if let Some(hash) = record.system_prompt.strip_prefix("@@hash:") {
            if let Ok(content) = conn.query_row(
                "SELECT content FROM pyramid_prompt_store WHERE hash = ?1",
                rusqlite::params![hash],
                |row| row.get::<_, String>(0),
            ) {
                record.system_prompt = content;
            }
        }
    }
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
                    COALESCE(source, 'manual'), layer, check_type, created_at,
                    chain_id, step_name, tier, latency_ms, generation_id, estimated_cost_usd
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
                    "chain_id": row.get::<_, Option<String>>(10)?,
                    "step_name": row.get::<_, Option<String>>(11)?,
                    "tier": row.get::<_, Option<String>>(12)?,
                    "latency_ms": row.get::<_, Option<i64>>(13)?,
                    "generation_id": row.get::<_, Option<String>>(14)?,
                    "estimated_cost_usd": row.get::<_, Option<f64>>(15)?,
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

// ── Evidence System CRUD (Phase 1) ────────────────────────────────────────────

/// Save an evidence link (upsert on slug + build_id + source + target).
pub fn save_evidence_link(conn: &Connection, link: &EvidenceLink) -> Result<()> {
    let build_id = link.build_id.as_deref().unwrap_or("");
    conn.execute(
        "INSERT INTO pyramid_evidence (slug, build_id, source_node_id, target_node_id, verdict, weight, reason)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(slug, build_id, source_node_id, target_node_id) DO UPDATE SET
           verdict = excluded.verdict,
           weight = excluded.weight,
           reason = excluded.reason",
        rusqlite::params![
            link.slug,
            build_id,
            link.source_node_id,
            link.target_node_id,
            link.verdict.as_str(),
            link.weight,
            link.reason,
        ],
    )?;
    Ok(())
}

#[deprecated(note = "Use get_evidence_for_target_cross for handle-path support")]
/// Get all evidence links pointing at a target node (i.e. its supporting evidence).
pub fn get_evidence_for_target(
    conn: &Connection,
    slug: &str,
    target_node_id: &str,
) -> Result<Vec<EvidenceLink>> {
    let mut stmt = conn.prepare(
        "SELECT slug, source_node_id, target_node_id, verdict, weight, reason, build_id
         FROM live_pyramid_evidence WHERE slug = ?1 AND target_node_id = ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![slug, target_node_id], evidence_from_row)?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

#[deprecated(note = "Use get_evidence_for_source_cross for handle-path support")]
/// Get all evidence links from a source node (i.e. what it supports).
pub fn get_evidence_for_source(
    conn: &Connection,
    slug: &str,
    source_node_id: &str,
) -> Result<Vec<EvidenceLink>> {
    let mut stmt = conn.prepare(
        "SELECT slug, source_node_id, target_node_id, verdict, weight, reason, build_id
         FROM live_pyramid_evidence WHERE slug = ?1 AND source_node_id = ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![slug, source_node_id], evidence_from_row)?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

#[deprecated(note = "Use get_keep_evidence_for_target_cross for handle-path support")]
/// Get only KEEP evidence links for a target node.
pub fn get_keep_evidence_for_target(
    conn: &Connection,
    slug: &str,
    target_node_id: &str,
) -> Result<Vec<EvidenceLink>> {
    let mut stmt = conn.prepare(
        "SELECT slug, source_node_id, target_node_id, verdict, weight, reason, build_id
         FROM live_pyramid_evidence WHERE slug = ?1 AND target_node_id = ?2 AND verdict = 'KEEP'",
    )?;
    let rows = stmt.query_map(rusqlite::params![slug, target_node_id], evidence_from_row)?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

fn evidence_from_row(row: &rusqlite::Row) -> rusqlite::Result<EvidenceLink> {
    let verdict_str: String = row.get(3)?;
    let build_id: String = row.get(6)?;
    Ok(EvidenceLink {
        slug: row.get(0)?,
        source_node_id: row.get(1)?,
        target_node_id: row.get(2)?,
        verdict: EvidenceVerdict::from_str(&verdict_str),
        weight: row.get(4)?,
        reason: row.get(5)?,
        build_id: if build_id.is_empty() {
            None
        } else {
            Some(build_id)
        },
        live: Some(true), // Default to live; cross-slug resolution comes in WS8-C
    })
}

/// Row mapper for the raw `pyramid_evidence` table (includes build_id column).
/// Unlike `evidence_from_row` which reads from the live view, this reads from the
/// raw table and leaves `live` as None — the caller resolves liveness in Rust.
fn evidence_from_raw_row(row: &rusqlite::Row) -> rusqlite::Result<EvidenceLink> {
    let verdict_str: String = row.get(3)?;
    let build_id: String = row.get(6)?;
    Ok(EvidenceLink {
        slug: row.get(0)?,
        source_node_id: row.get(1)?,
        target_node_id: row.get(2)?,
        verdict: EvidenceVerdict::from_str(&verdict_str),
        weight: row.get(4)?,
        reason: row.get(5)?,
        build_id: if build_id.is_empty() {
            None
        } else {
            Some(build_id)
        },
        live: None, // Caller resolves liveness via get_live_node
    })
}

// ── Cross-Slug Evidence Queries (WS8-C) ──────────────────────────────────────

/// Get all evidence links pointing at a target node, resolving cross-slug handle-paths.
///
/// Queries the RAW `pyramid_evidence` table (not the `live_pyramid_evidence` view),
/// then checks liveness of each source node in Rust. For handle-path source IDs,
/// the parsed slug is used for the liveness check; for bare IDs, the evidence link's
/// own slug is used.
///
/// Returns ALL links (both live and dead) annotated with `live: Some(true|false)`.
pub fn get_evidence_for_target_cross(
    conn: &Connection,
    slug: &str,
    target_node_id: &str,
) -> Result<Vec<EvidenceLink>> {
    let mut stmt = conn.prepare(
        "SELECT slug, source_node_id, target_node_id, verdict, weight, reason, build_id
         FROM pyramid_evidence WHERE slug = ?1 AND target_node_id = ?2",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![slug, target_node_id],
        evidence_from_raw_row,
    )?;
    let mut links: Vec<EvidenceLink> = rows.filter_map(|r| r.ok()).collect();

    // Resolve liveness for each source node
    for link in &mut links {
        if let Some((parsed_slug, _depth, parsed_node_id)) = parse_handle_path(&link.source_node_id)
        {
            // Cross-slug handle-path: check liveness in the source pyramid
            link.live = Some(get_live_node(conn, parsed_slug, parsed_node_id)?.is_some());
        } else {
            // Bare ID: same-slug, check in the evidence link's slug
            link.live = Some(get_live_node(conn, &link.slug, &link.source_node_id)?.is_some());
        }
    }

    Ok(links)
}

/// Get all evidence links from a source node across ALL slugs, resolving liveness.
///
/// Queries the RAW `pyramid_evidence` table with NO slug filter on `source_node_id`.
/// Used by supersession/staleness to find who cites a given source.
///
/// Returns ALL links annotated with `live: Some(true|false)` for the target nodes.
pub fn get_evidence_for_source_cross(
    conn: &Connection,
    source_node_id: &str,
) -> Result<Vec<EvidenceLink>> {
    let mut stmt = conn.prepare(
        "SELECT slug, source_node_id, target_node_id, verdict, weight, reason, build_id
         FROM pyramid_evidence WHERE source_node_id = ?1",
    )?;
    let rows = stmt.query_map(rusqlite::params![source_node_id], evidence_from_raw_row)?;
    let mut links: Vec<EvidenceLink> = rows.filter_map(|r| r.ok()).collect();

    // Resolve liveness for each target node
    for link in &mut links {
        link.live = Some(get_live_node(conn, &link.slug, &link.target_node_id)?.is_some());
    }

    Ok(links)
}

/// Get only KEEP evidence links for a target node, with cross-slug handle-path resolution.
///
/// Same as `get_evidence_for_target_cross` but filtered to `verdict = 'KEEP'`.
pub fn get_keep_evidence_for_target_cross(
    conn: &Connection,
    slug: &str,
    target_node_id: &str,
) -> Result<Vec<EvidenceLink>> {
    let mut stmt = conn.prepare(
        "SELECT slug, source_node_id, target_node_id, verdict, weight, reason, build_id
         FROM pyramid_evidence WHERE slug = ?1 AND target_node_id = ?2 AND verdict = 'KEEP'",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![slug, target_node_id],
        evidence_from_raw_row,
    )?;
    let mut links: Vec<EvidenceLink> = rows.filter_map(|r| r.ok()).collect();

    // Resolve liveness for each source node
    for link in &mut links {
        if let Some((parsed_slug, _depth, parsed_node_id)) = parse_handle_path(&link.source_node_id)
        {
            link.live = Some(get_live_node(conn, parsed_slug, parsed_node_id)?.is_some());
        } else {
            link.live = Some(get_live_node(conn, &link.slug, &link.source_node_id)?.is_some());
        }
    }

    Ok(links)
}

// ── Question Tree CRUD ───────────────────────────────────────────────────────

/// Save (upsert) a question decomposition tree for a slug.
pub fn save_question_tree(conn: &Connection, slug: &str, tree: &serde_json::Value) -> Result<()> {
    save_question_tree_with_build_id(conn, slug, tree, None)
}

/// Save question tree with optional build_id for overlay scoping.
pub fn save_question_tree_with_build_id(
    conn: &Connection,
    slug: &str,
    tree: &serde_json::Value,
    build_id: Option<&str>,
) -> Result<()> {
    let json_str = serde_json::to_string(tree)?;
    let build_id = build_id.unwrap_or("");
    conn.execute(
        "INSERT INTO pyramid_question_tree (slug, build_id, tree)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(slug, build_id) DO UPDATE SET
           tree = excluded.tree,
           updated_at = datetime('now')",
        rusqlite::params![slug, build_id, json_str],
    )?;
    Ok(())
}

/// Get the question tree for a slug.
pub fn get_question_tree(conn: &Connection, slug: &str) -> Result<Option<serde_json::Value>> {
    let mut stmt = conn.prepare("SELECT tree FROM pyramid_question_tree WHERE slug = ?1 ORDER BY updated_at DESC LIMIT 1")?;
    let result = stmt.query_row(rusqlite::params![slug], |row| {
        let json_str: String = row.get(0)?;
        Ok(json_str)
    });
    match result {
        Ok(json_str) => {
            let val: serde_json::Value = serde_json::from_str(&json_str)?;
            Ok(Some(val))
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

// ── Question Node CRUD (incremental decomposition) ──────────────────────────

/// Save (upsert) a single question decomposition node.
///
/// Used by the incremental decomposition flow: each node is persisted
/// immediately after the LLM returns its children, so progress survives
/// crashes and restarts.
pub fn save_question_node(
    conn: &Connection,
    slug: &str,
    node: &super::question_decomposition::QuestionNode,
    parent_id: Option<&str>,
    depth: u32,
) -> Result<()> {
    save_question_node_with_build_id(conn, slug, node, parent_id, depth, None)
}

/// Save a question node with an optional build_id for overlay scoping.
pub fn save_question_node_with_build_id(
    conn: &Connection,
    slug: &str,
    node: &super::question_decomposition::QuestionNode,
    parent_id: Option<&str>,
    depth: u32,
    build_id: Option<&str>,
) -> Result<()> {
    let children_json = if node.children.is_empty() {
        None
    } else {
        let ids: Vec<&str> = node.children.iter().map(|c| c.id.as_str()).collect();
        Some(serde_json::to_string(&ids)?)
    };

    conn.execute(
        "INSERT INTO pyramid_question_nodes (slug, question_id, parent_id, depth, question, about, creates, prompt_hint, is_leaf, children_json, build_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(slug, question_id) DO UPDATE SET
           parent_id = excluded.parent_id,
           depth = excluded.depth,
           question = excluded.question,
           about = excluded.about,
           creates = excluded.creates,
           prompt_hint = excluded.prompt_hint,
           is_leaf = excluded.is_leaf,
           children_json = excluded.children_json,
           build_id = excluded.build_id",
        rusqlite::params![
            slug,
            node.id,
            parent_id,
            depth,
            node.question,
            node.about,
            node.creates,
            node.prompt_hint,
            node.is_leaf as i32,
            children_json,
            build_id,
        ],
    )?;
    Ok(())
}

/// Load all question nodes for a slug and reconstruct the QuestionTree.
///
/// Returns None if no nodes exist for this slug. Reconstructs the tree
/// by assembling parent→child relationships from the flat node rows.
pub fn load_question_nodes_as_tree(
    conn: &Connection,
    slug: &str,
) -> Result<Option<Vec<QuestionNodeRow>>> {
    let mut stmt = conn.prepare(
        "SELECT question_id, parent_id, depth, question, about, creates, prompt_hint, is_leaf, children_json
         FROM pyramid_question_nodes
         WHERE slug = ?1
         ORDER BY depth ASC",
    )?;
    let rows: Vec<QuestionNodeRow> = stmt
        .query_map(rusqlite::params![slug], |row| {
            Ok(QuestionNodeRow {
                question_id: row.get(0)?,
                parent_id: row.get(1)?,
                depth: row.get(2)?,
                question: row.get(3)?,
                about: row.get(4)?,
                creates: row.get(5)?,
                prompt_hint: row.get(6)?,
                is_leaf: row.get::<_, i32>(7)? != 0,
                children_json: row.get(8)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    if rows.is_empty() {
        Ok(None)
    } else {
        Ok(Some(rows))
    }
}

/// A flat row from the pyramid_question_nodes table.
#[derive(Debug, Clone)]
pub struct QuestionNodeRow {
    pub question_id: String,
    pub parent_id: Option<String>,
    pub depth: u32,
    pub question: String,
    pub about: String,
    pub creates: String,
    pub prompt_hint: String,
    pub is_leaf: bool,
    pub children_json: Option<String>,
}

/// Get nodes that are branch nodes (is_leaf = 0) but haven't been decomposed yet
/// (children_json IS NULL). These are the nodes that need further LLM decomposition.
pub fn get_undecomposed_nodes(conn: &Connection, slug: &str) -> Result<Vec<QuestionNodeRow>> {
    let mut stmt = conn.prepare(
        "SELECT question_id, parent_id, depth, question, about, creates, prompt_hint, is_leaf, children_json
         FROM pyramid_question_nodes
         WHERE slug = ?1 AND is_leaf = 0 AND children_json IS NULL
         ORDER BY depth ASC",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![slug], |row| {
            Ok(QuestionNodeRow {
                question_id: row.get(0)?,
                parent_id: row.get(1)?,
                depth: row.get(2)?,
                question: row.get(3)?,
                about: row.get(4)?,
                creates: row.get(5)?,
                prompt_hint: row.get(6)?,
                is_leaf: row.get::<_, i32>(7)? != 0,
                children_json: row.get(8)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Count total question nodes for a slug, optionally scoped by build_id.
pub fn count_question_nodes(conn: &Connection, slug: &str) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pyramid_question_nodes WHERE slug = ?1",
        rusqlite::params![slug],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Count question nodes for a slug scoped to a specific build_id.
pub fn count_question_nodes_for_build(
    conn: &Connection,
    slug: &str,
    build_id: &str,
) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pyramid_question_nodes WHERE slug = ?1 AND build_id = ?2",
        rusqlite::params![slug, build_id],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Check if a slug has any live overlay nodes (L1+ with build_id LIKE 'qb-%')
/// AND has question nodes. Used by delta decomposition to decide fresh vs delta path.
pub fn has_existing_question_overlay(conn: &Connection, slug: &str) -> Result<bool> {
    let has_overlay_nodes: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM live_pyramid_nodes
            WHERE slug = ?1 AND depth > 0 AND build_id LIKE 'qb-%'
        )",
        rusqlite::params![slug],
        |row| row.get(0),
    )?;

    if !has_overlay_nodes {
        return Ok(false);
    }

    let has_question_nodes: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM pyramid_question_nodes WHERE slug = ?1
        )",
        rusqlite::params![slug],
        |row| row.get(0),
    )?;

    Ok(has_question_nodes)
}

/// Get all existing question overlay build_ids for a slug.
/// Returns (build_id, question, status, created_at) for each overlay.
pub fn list_question_overlays(conn: &Connection, slug: &str) -> Result<Vec<QuestionOverlayInfo>> {
    let mut stmt = conn.prepare(
        "SELECT build_id, question, status, started_at
         FROM pyramid_builds
         WHERE slug = ?1 AND build_id LIKE 'qb-%'
         ORDER BY started_at DESC",
    )?;
    let rows = stmt.query_map(rusqlite::params![slug], |row| {
        Ok(QuestionOverlayInfo {
            build_id: row.get(0)?,
            question: row.get(1)?,
            status: row.get(2)?,
            created_at: row.get(3)?,
        })
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Info about a question overlay build.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QuestionOverlayInfo {
    pub build_id: String,
    pub question: String,
    pub status: String,
    pub created_at: String,
}

/// Get existing overlay answer nodes and their question tree for delta decomposition.
/// Returns live L1+ nodes that belong to qb-* builds, plus their question tree context.
pub fn get_existing_overlay_answers(conn: &Connection, slug: &str) -> Result<Vec<PyramidNode>> {
    let sql = format!(
        "SELECT {NODE_SELECT_COLS} FROM live_pyramid_nodes
         WHERE slug = ?1 AND depth > 0 AND build_id LIKE 'qb-%'
         ORDER BY depth ASC, id ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![slug], node_from_row)?;
    let mut nodes = Vec::new();
    for row in rows {
        nodes.push(row?);
    }
    Ok(nodes)
}

/// 11-O: Clear question nodes for a slug scoped by build_id.
/// When build_id is None, clears nodes with NULL build_id (legacy data).
/// Question nodes are contributions — scoping by build_id preserves history.
pub fn clear_question_nodes(conn: &Connection, slug: &str, build_id: Option<&str>) -> Result<i64> {
    let deleted = match build_id {
        Some(bid) => conn.execute(
            "DELETE FROM pyramid_question_nodes WHERE slug = ?1 AND build_id = ?2",
            rusqlite::params![slug, bid],
        )?,
        None => conn.execute(
            "DELETE FROM pyramid_question_nodes WHERE slug = ?1 AND build_id IS NULL",
            rusqlite::params![slug],
        )?,
    };
    Ok(deleted as i64)
}

/// Reconstruct a QuestionTree from the flat node rows stored in pyramid_question_nodes.
///
/// Rebuilds the tree by finding the root (no parent_id) and recursively
/// attaching children based on children_json ordering.
pub fn reconstruct_question_tree(
    rows: &[QuestionNodeRow],
    config: &super::question_decomposition::DecompositionConfig,
) -> Result<super::question_decomposition::QuestionTree> {
    use super::question_decomposition::{QuestionNode, QuestionTree};

    if rows.is_empty() {
        return Err(anyhow::anyhow!("no nodes to reconstruct tree from"));
    }

    // Build a map: question_id → row
    let row_map: HashMap<String, &QuestionNodeRow> =
        rows.iter().map(|r| (r.question_id.clone(), r)).collect();

    // Find the root (no parent_id)
    let root_row = rows
        .iter()
        .find(|r| r.parent_id.is_none())
        .ok_or_else(|| anyhow::anyhow!("no root node found (all nodes have parent_id)"))?;

    fn build_node(
        row: &QuestionNodeRow,
        row_map: &HashMap<String, &QuestionNodeRow>,
    ) -> QuestionNode {
        let children = match &row.children_json {
            Some(json_str) => {
                let child_ids: Vec<String> = serde_json::from_str(json_str).unwrap_or_default();
                child_ids
                    .iter()
                    .filter_map(|id| row_map.get(id.as_str()))
                    .map(|child_row| build_node(child_row, row_map))
                    .collect()
            }
            None => vec![],
        };

        QuestionNode {
            id: row.question_id.clone(),
            question: row.question.clone(),
            about: row.about.clone(),
            creates: row.creates.clone(),
            prompt_hint: row.prompt_hint.clone(),
            children,
            is_leaf: row.is_leaf,
        }
    }

    let apex = build_node(root_row, &row_map);

    Ok(QuestionTree {
        apex,
        content_type: config.content_type.clone(),
        config: config.clone(),
        audience: None,
    })
}

// ── Gap Reports CRUD ─────────────────────────────────────────────────────────

/// Save a gap report for a slug.
/// 11-Z: Accepts optional build_id to scope gaps to a specific build.
/// Deduplicates on (slug, question_id, description) — upserts layer and build_id on re-runs.
pub fn save_gap(
    conn: &Connection,
    slug: &str,
    gap: &GapReport,
    build_id: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_gaps (slug, question_id, description, layer, build_id)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(slug, question_id, description) DO UPDATE SET layer = excluded.layer, build_id = excluded.build_id",
        rusqlite::params![slug, gap.question_id, gap.description, gap.layer, build_id],
    )?;
    Ok(())
}

/// Get all gap reports for a slug.
pub fn get_gaps_for_slug(conn: &Connection, slug: &str) -> Result<Vec<GapReport>> {
    let mut stmt = conn.prepare(
        "SELECT question_id, description, layer, COALESCE(resolved, 0), COALESCE(resolution_confidence, CASE WHEN COALESCE(resolved, 0) = 1 THEN 1.0 ELSE 0.0 END) FROM pyramid_gaps WHERE slug = ?1 ORDER BY id ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![slug], |row| {
        Ok(GapReport {
            question_id: row.get(0)?,
            description: row.get(1)?,
            layer: row.get(2)?,
            resolved: row.get::<_, i64>(3).unwrap_or(0) != 0,
            resolution_confidence: row.get::<_, f64>(4).unwrap_or(0.0),
        })
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Get unresolved gap reports for a slug (for gap processing pass).
pub fn get_unresolved_gaps_for_slug(conn: &Connection, slug: &str) -> Result<Vec<GapReport>> {
    let mut stmt = conn.prepare(
        "SELECT question_id, description, layer, 0, COALESCE(resolution_confidence, 0.0) FROM pyramid_gaps
         WHERE slug = ?1 AND COALESCE(resolution_confidence, CASE WHEN COALESCE(resolved, 0) = 1 THEN 1.0 ELSE 0.0 END) < 0.8 ORDER BY id ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![slug], |row| {
        Ok(GapReport {
            question_id: row.get(0)?,
            description: row.get(1)?,
            layer: row.get(2)?,
            resolved: row.get::<_, i64>(3).unwrap_or(0) != 0,
            resolution_confidence: row.get::<_, f64>(4).unwrap_or(0.0),
        })
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Get gap reports for a specific question node within a slug.
pub fn get_gaps_for_question(
    conn: &Connection,
    slug: &str,
    question_id: &str,
) -> Result<Vec<GapReport>> {
    let mut stmt = conn.prepare(
        "SELECT question_id, description, layer, COALESCE(resolved, 0), COALESCE(resolution_confidence, CASE WHEN COALESCE(resolved, 0) = 1 THEN 1.0 ELSE 0.0 END) FROM pyramid_gaps
         WHERE slug = ?1 AND question_id = ?2 ORDER BY id ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![slug, question_id], |row| {
        Ok(GapReport {
            question_id: row.get(0)?,
            description: row.get(1)?,
            layer: row.get(2)?,
            resolved: row.get::<_, i64>(3).unwrap_or(0) != 0,
            resolution_confidence: row.get::<_, f64>(4).unwrap_or(0.0),
        })
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Mark a gap as resolved after successful targeted re-examination.
pub fn mark_gap_resolved(
    conn: &Connection,
    slug: &str,
    question_id: &str,
    description: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_gaps SET resolved = 1, resolution_confidence = 1.0
         WHERE slug = ?1 AND question_id = ?2 AND description = ?3",
        rusqlite::params![slug, question_id, description],
    )?;
    Ok(())
}

// ── Sequential Node ID Generation ────────────────────────────────────────────

/// Generate the next sequential node ID for a given slug and depth.
///
/// Scans existing node IDs matching the pattern `L{depth}-{NNN}` (and variants
/// like `L0-TNNN`) to find the highest numeric suffix, then returns the next.
/// This avoids UUID-based IDs which LLMs cannot faithfully reproduce during
/// pre-mapping and evidence answering.
///
/// The `prefix` parameter allows callers to use a sub-prefix (e.g. "T" for
/// targeted extractions), producing IDs like `L0-T042`. Pass "" for standard
/// sequential IDs like `L1-003`.
pub fn next_sequential_node_id(
    conn: &Connection,
    slug: &str,
    depth: i64,
    prefix: &str,
) -> String {
    let pattern = format!("L{}-{}%", depth, prefix);
    let max_idx: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(CAST(SUBSTR(id, ?1) AS INTEGER)), -1)
             FROM pyramid_nodes
             WHERE slug = ?2 AND depth = ?3 AND id LIKE ?4",
            rusqlite::params![
                // offset into the id string: skip "L{depth}-{prefix}"
                format!("L{}-{}", depth, prefix).len() + 1,
                slug,
                depth,
                pattern,
            ],
            |row| row.get(0),
        )
        .unwrap_or(-1);
    format!("L{}-{}{:03}", depth, prefix, max_idx + 1)
}

// ── Evidence Set Queries ────────────────────────────────────────────────────

/// Get all evidence sets for a slug (targeted L0 nodes grouped by self_prompt).
/// Does NOT load member IDs — use get_evidence_set_member_ids() for that.
pub fn get_evidence_sets(conn: &Connection, slug: &str) -> Result<Vec<EvidenceSet>> {
    let mut stmt = conn.prepare(
        "SELECT n.self_prompt, COUNT(*) as member_count,
                (SELECT headline FROM pyramid_nodes
                 WHERE slug = ?1 AND depth = 0 AND id LIKE 'ES-%'
                   AND self_prompt = n.self_prompt AND superseded_by IS NULL
                 LIMIT 1) as index_headline
         FROM live_pyramid_nodes n
         WHERE n.slug = ?1 AND n.depth = 0 AND n.self_prompt != '' AND n.id NOT LIKE 'ES-%'
         GROUP BY n.self_prompt
         ORDER BY member_count DESC",
    )?;
    let rows = stmt.query_map(rusqlite::params![slug], |row| {
        Ok(EvidenceSet {
            self_prompt: row.get(0)?,
            member_count: row.get(1)?,
            index_headline: row.get(2)?,
        })
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Load member node IDs for a specific evidence set (by self_prompt).
pub fn get_evidence_set_member_ids(
    conn: &Connection,
    slug: &str,
    self_prompt: &str,
) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM live_pyramid_nodes
         WHERE slug = ?1 AND depth = 0 AND self_prompt = ?2 AND id NOT LIKE 'ES-%'
         ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![slug, self_prompt], |row| {
        row.get::<_, String>(0)
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Resolve a potentially relative file path to absolute using the slug's source directories.
/// If the path is already absolute, returns it unchanged.
/// If resolution fails, returns the original path.
pub fn resolve_to_absolute(conn: &Connection, slug: &str, path: &str) -> String {
    if path.starts_with('/') {
        return path.to_string();
    }
    let source_path: Option<String> = conn
        .query_row(
            "SELECT source_path FROM pyramid_slugs WHERE slug = ?1",
            rusqlite::params![slug],
            |row| row.get(0),
        )
        .ok();
    if let Some(sp) = source_path {
        let dirs: Vec<String> =
            serde_json::from_str(&sp).unwrap_or_else(|_| vec![sp]);
        for dir in &dirs {
            let candidate = std::path::Path::new(dir).join(path);
            if candidate.exists() {
                return candidate.to_string_lossy().to_string();
            }
        }
    }
    path.to_string()
}

/// Append a targeted L0 node ID to a file's node_ids JSON array in pyramid_file_hashes.
/// If no row exists for this file_path, creates one with the node_id.
/// File paths are normalized to absolute before storage.
pub fn append_node_id_to_file_hash(
    conn: &Connection,
    slug: &str,
    file_path: &str,
    node_id: &str,
) -> Result<()> {
    let abs_path = resolve_to_absolute(conn, slug, file_path);
    let rows = conn.execute(
        "UPDATE pyramid_file_hashes
         SET node_ids = COALESCE(json_insert(node_ids, '$[#]', ?3), json_array(?3))
         WHERE slug = ?1 AND file_path = ?2",
        rusqlite::params![slug, abs_path, node_id],
    )?;
    if rows == 0 {
        // No existing row — insert a new one
        conn.execute(
            "INSERT INTO pyramid_file_hashes (slug, file_path, hash, chunk_count, node_ids, last_ingested_at)
             VALUES (?1, ?2, '', 1, ?3, datetime('now'))",
            rusqlite::params![slug, abs_path, serde_json::json!([node_id]).to_string()],
        )?;
    }
    Ok(())
}

/// Find targeted L0 nodes linked to the same source files as the given canonical node IDs.
/// Lookup: canonical_node_id -> file_hashes rows containing that ID -> all node_ids -> filter targeted.
pub fn get_targeted_l0_for_canonical_nodes(
    conn: &Connection,
    slug: &str,
    canonical_node_ids: &[String],
) -> Result<Vec<String>> {
    let mut targeted = std::collections::BTreeSet::new();
    for canon_id in canonical_node_ids {
        // Find file_path rows whose node_ids JSON array contains this canonical ID
        let mut stmt = conn.prepare(
            "SELECT file_path, node_ids FROM pyramid_file_hashes
             WHERE slug = ?1 AND EXISTS (SELECT 1 FROM json_each(node_ids) WHERE value = ?2)",
        )?;
        let rows = stmt.query_map(rusqlite::params![slug, canon_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows.flatten() {
            let (_file_path, node_ids_json) = row;
            // Parse the JSON array and find targeted L0 nodes (non-empty self_prompt)
            if let Ok(ids) = serde_json::from_str::<Vec<String>>(&node_ids_json) {
                for nid in ids {
                    if nid != *canon_id {
                        // Check if this node is a targeted L0 (non-empty self_prompt)
                        let is_targeted: bool = conn
                            .query_row(
                                "SELECT self_prompt != '' FROM pyramid_nodes
                                 WHERE slug = ?1 AND id = ?2 AND superseded_by IS NULL",
                                rusqlite::params![slug, nid],
                                |row| row.get(0),
                            )
                            .unwrap_or(false);
                        if is_targeted {
                            targeted.insert(nid);
                        }
                    }
                }
            }
        }
    }
    Ok(targeted.into_iter().collect())
}

// ── ID Map Extensions (wire_handle_path) ─────────────────────────────────────

/// Save an ID mapping with wire_handle_path (extends existing pyramid_id_map).
pub fn save_id_mapping_extended(conn: &Connection, slug: &str, mapping: &IdMapping) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_id_map (slug, local_id, wire_uuid, wire_handle_path, published_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(slug, local_id) DO UPDATE SET
           wire_uuid = excluded.wire_uuid,
           wire_handle_path = excluded.wire_handle_path,
           published_at = excluded.published_at",
        rusqlite::params![
            slug,
            mapping.local_id,
            mapping.wire_uuid.as_deref().unwrap_or(""),
            mapping.wire_handle_path,
            mapping.published_at,
        ],
    )?;
    Ok(())
}

/// Save an ID mapping — contract-matching alias for `save_id_mapping_extended`.
pub fn save_id_mapping(conn: &Connection, slug: &str, mapping: &IdMapping) -> Result<()> {
    save_id_mapping_extended(conn, slug, mapping)
}

/// Get the wire handle-path for a local node ID.
pub fn get_wire_handle_path(
    conn: &Connection,
    slug: &str,
    local_id: &str,
) -> Result<Option<String>> {
    let mut stmt = conn
        .prepare("SELECT wire_handle_path FROM pyramid_id_map WHERE slug = ?1 AND local_id = ?2")?;
    let result = stmt.query_row(rusqlite::params![slug, local_id], |row| {
        row.get::<_, String>(0)
    });
    match result {
        Ok(path) if path.is_empty() => Ok(None),
        Ok(path) => Ok(Some(path)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Get all ID mappings for a slug as IdMapping structs.
pub fn get_all_id_mappings(conn: &Connection, slug: &str) -> Result<Vec<IdMapping>> {
    let mut stmt = conn.prepare(
        "SELECT local_id, wire_handle_path, wire_uuid, published_at
         FROM pyramid_id_map WHERE slug = ?1 ORDER BY local_id",
    )?;
    let rows = stmt.query_map(rusqlite::params![slug], |row| {
        let wire_uuid: String = row.get(2)?;
        Ok(IdMapping {
            local_id: row.get(0)?,
            wire_handle_path: row.get(1)?,
            wire_uuid: if wire_uuid.is_empty() {
                None
            } else {
                Some(wire_uuid)
            },
            published_at: row.get(3)?,
        })
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Check if a local node has been published to Wire.
pub fn is_already_published(conn: &Connection, slug: &str, local_id: &str) -> Result<bool> {
    let result =
        conn.prepare("SELECT 1 FROM pyramid_id_map WHERE slug = ?1 AND local_id = ?2 LIMIT 1");
    match result {
        Ok(mut stmt) => {
            let exists = stmt
                .query_row(rusqlite::params![slug, local_id], |_row| Ok(()))
                .is_ok();
            Ok(exists)
        }
        Err(e) => {
            // Gracefully handle "no such table" — table may not be created yet
            let msg = e.to_string();
            if msg.contains("no such table") {
                tracing::debug!(
                    slug = slug,
                    local_id = local_id,
                    "pyramid_id_map table not found, treating as not-yet-published"
                );
                Ok(false)
            } else {
                Err(e.into())
            }
        }
    }
}

// ── Publication Tracking ─────────────────────────────────────────────────────

/// Get the last published build_id for a slug.
///
/// Returns None if the slug has never been published (column is NULL).
pub fn get_last_published_build_id(conn: &Connection, slug: &str) -> Result<Option<String>> {
    let result = conn.query_row(
        "SELECT last_published_build_id FROM pyramid_slugs WHERE slug = ?1",
        rusqlite::params![slug],
        |row| row.get::<_, Option<String>>(0),
    );
    match result {
        Ok(val) => Ok(val),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Set the last published build_id for a slug after successful publication.
pub fn set_last_published_build_id(conn: &Connection, slug: &str, build_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_slugs SET last_published_build_id = ?1 WHERE slug = ?2",
        rusqlite::params![build_id, slug],
    )?;
    Ok(())
}

/// Get the current (latest) build_id for a slug from pyramid_nodes.
///
/// Returns the MAX(build_id) across all nodes in the slug, or None if no
/// nodes exist or all build_ids are NULL.
pub fn get_current_build_id(conn: &Connection, slug: &str) -> Result<Option<String>> {
    let result = conn.query_row(
        "SELECT MAX(build_id) FROM pyramid_nodes WHERE slug = ?1",
        rusqlite::params![slug],
        |row| row.get::<_, Option<String>>(0),
    );
    match result {
        Ok(val) => Ok(val),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Get the metadata contribution UUID for a slug (WS-ONLINE-B discovery).
///
/// Returns the Wire UUID of the most recently published `pyramid_metadata`
/// contribution for this slug, or None if never published.
pub fn get_slug_metadata_contribution_id(conn: &Connection, slug: &str) -> Result<Option<String>> {
    let result = conn.query_row(
        "SELECT metadata_contribution_id FROM pyramid_slugs WHERE slug = ?1",
        rusqlite::params![slug],
        |row| row.get::<_, Option<String>>(0),
    );
    match result {
        Ok(val) => Ok(val),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Read the access tier, access price, and absorption mode for a slug.
///
/// These columns were added by the WS-ONLINE prep migration and default to
/// 'public', NULL, and 'open' respectively.
pub fn get_slug_online_fields(
    conn: &Connection,
    slug: &str,
) -> Result<(String, Option<i64>, String)> {
    let result = conn.query_row(
        "SELECT access_tier, access_price, absorption_mode FROM pyramid_slugs WHERE slug = ?1",
        rusqlite::params![slug],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, String>(2)?,
            ))
        },
    );
    match result {
        Ok(val) => Ok(val),
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            Ok(("public".to_string(), None, "open".to_string()))
        }
        Err(e) => Err(e.into()),
    }
}

/// Read the absorption mode and chain ID for a slug (WS-ONLINE-G).
///
/// Returns `(mode, chain_id)` where mode is one of "open", "absorb-all",
/// "absorb-selective". Defaults to ("open", None) if the slug doesn't exist.
pub fn get_absorption_mode(conn: &Connection, slug: &str) -> Result<(String, Option<String>)> {
    let result = conn.query_row(
        "SELECT absorption_mode, absorption_chain_id FROM pyramid_slugs WHERE slug = ?1",
        rusqlite::params![slug],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
    );
    match result {
        Ok(val) => Ok(val),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(("open".to_string(), None)),
        Err(e) => Err(e.into()),
    }
}

/// Set the absorption mode for a slug (WS-ONLINE-G).
///
/// - `mode`: one of "open", "absorb-all", "absorb-selective"
/// - `chain_id`: required for absorb-selective (the action chain that evaluates incoming webs)
pub fn set_absorption_mode(
    conn: &Connection,
    slug: &str,
    mode: &str,
    chain_id: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_slugs SET absorption_mode = ?1, absorption_chain_id = ?2, updated_at = datetime('now') WHERE slug = ?3",
        rusqlite::params![mode, chain_id, slug],
    )?;
    Ok(())
}

/// Read the access tier config for a slug: (tier, price, allowed_circles JSON).
///
/// Returns the access_tier string, optional access_price override, and the
/// raw allowed_circles JSON string (a JSON array of circle UUIDs, or NULL).
pub fn get_access_tier(
    conn: &Connection,
    slug: &str,
) -> Result<(String, Option<i64>, Option<String>)> {
    let result = conn.query_row(
        "SELECT access_tier, access_price, allowed_circles FROM pyramid_slugs WHERE slug = ?1",
        rusqlite::params![slug],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        },
    );
    match result {
        Ok(val) => Ok(val),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(("public".to_string(), None, None)),
        Err(e) => Err(e.into()),
    }
}

/// Set the access tier config for a slug.
///
/// - `tier`: one of "public", "circle-scoped", "priced", "embargoed"
/// - `price`: explicit price override (None = use emergent pricing)
/// - `circles`: JSON array string of allowed circle UUIDs (only relevant for circle-scoped)
pub fn set_access_tier(
    conn: &Connection,
    slug: &str,
    tier: &str,
    price: Option<i64>,
    circles: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_slugs SET access_tier = ?1, access_price = ?2, allowed_circles = ?3, updated_at = datetime('now') WHERE slug = ?4",
        rusqlite::params![tier, price, circles, slug],
    )?;
    Ok(())
}

/// Compute the emergent price for a slug by counting unique source citations.
///
/// Counts unique `source_node_id` entries across all live evidence links for the slug.
/// This represents the breadth of source material the pyramid synthesizes, which
/// determines its emergent value.
pub fn compute_emergent_price(conn: &Connection, slug: &str) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT source_node_id) FROM live_pyramid_evidence WHERE slug = ?1",
        rusqlite::params![slug],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Compute and cache the emergent price for a slug.
///
/// Calls `compute_emergent_price` and writes the result to `cached_emergent_price`
/// in pyramid_slugs. Should be called after every successful build.
pub fn update_cached_emergent_price(conn: &Connection, slug: &str) -> Result<()> {
    let price = compute_emergent_price(conn, slug)?;
    conn.execute(
        "UPDATE pyramid_slugs SET cached_emergent_price = ?1 WHERE slug = ?2",
        rusqlite::params![price, slug],
    )?;
    tracing::debug!(slug = %slug, emergent_price = %price, "Cached emergent price updated");
    Ok(())
}

/// Read the cached emergent price for a slug (if computed).
pub fn get_cached_emergent_price(conn: &Connection, slug: &str) -> Result<Option<i64>> {
    let result = conn.query_row(
        "SELECT cached_emergent_price FROM pyramid_slugs WHERE slug = ?1",
        rusqlite::params![slug],
        |row| row.get::<_, Option<i64>>(0),
    );
    match result {
        Ok(val) => Ok(val),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Store the metadata contribution UUID for a slug after publishing discovery metadata.
pub fn set_slug_metadata_contribution_id(conn: &Connection, slug: &str, uuid: &str) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_slugs SET metadata_contribution_id = ?1 WHERE slug = ?2",
        rusqlite::params![uuid, slug],
    )?;
    Ok(())
}

/// Count nodes that exist in the pyramid but have not yet been published to Wire.
///
/// Returns the number of nodes in the slug that do NOT have a corresponding
/// entry in pyramid_id_map.
pub fn count_unpublished_nodes(conn: &Connection, slug: &str) -> Result<i64> {
    let result = conn.query_row(
        "SELECT COUNT(*) FROM pyramid_nodes n
         WHERE n.slug = ?1
         AND NOT EXISTS (
             SELECT 1 FROM pyramid_id_map m
             WHERE m.slug = n.slug AND m.local_id = n.id
         )",
        rusqlite::params![slug],
        |row| row.get::<_, i64>(0),
    );
    match result {
        Ok(count) => Ok(count),
        Err(e) => {
            // Gracefully handle "no such table" — pyramid_id_map may not exist yet
            let msg = e.to_string();
            if msg.contains("no such table") {
                // If id_map doesn't exist, all nodes are unpublished
                let total: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM pyramid_nodes WHERE slug = ?1",
                    rusqlite::params![slug],
                    |row| row.get(0),
                )?;
                Ok(total)
            } else {
                Err(e.into())
            }
        }
    }
}

// ── Source Deltas CRUD (file-level, NOT thread-level) ────────────────────────

/// Save a file-level source delta.
pub fn save_source_delta(
    conn: &Connection,
    slug: &str,
    file_path: &str,
    change_type: &str,
    diff_summary: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_source_deltas (slug, file_path, change_type, diff_summary)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![slug, file_path, change_type, diff_summary],
    )?;
    Ok(())
}

/// Get all unprocessed source deltas for a slug.
pub fn get_unprocessed_source_deltas(conn: &Connection, slug: &str) -> Result<Vec<SourceDelta>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, file_path, change_type, diff_summary, processed, created_at
         FROM pyramid_source_deltas WHERE slug = ?1 AND processed = 0 ORDER BY id ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![slug], |row| {
        Ok(SourceDelta {
            id: row.get(0)?,
            slug: row.get(1)?,
            file_path: row.get(2)?,
            change_type: row.get(3)?,
            diff_summary: row.get(4)?,
            processed: row.get::<_, i64>(5)? != 0,
            created_at: row.get(6)?,
        })
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Mark a source delta as processed.
pub fn mark_source_delta_processed(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_source_deltas SET processed = 1 WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

// ── Supersessions CRUD ───────────────────────────────────────────────────────

/// Record a belief correction (supersession).
pub fn save_supersession(
    conn: &Connection,
    slug: &str,
    node_id: &str,
    superseded_claim: &str,
    corrected_to: &str,
    source_node: Option<&str>,
    channel: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_supersessions (slug, node_id, superseded_claim, corrected_to, source_node, channel)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![slug, node_id, superseded_claim, corrected_to, source_node, channel],
    )?;
    Ok(())
}

// ── Staleness Queue CRUD ─────────────────────────────────────────────────────

/// Enqueue a question for re-answering due to staleness.
/// Deduplicates on (slug, question_id) — keeps the highest priority and latest channel.
pub fn enqueue_staleness(
    conn: &Connection,
    slug: &str,
    question_id: &str,
    reason: &str,
    channel: &str,
    priority: f64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_staleness_queue (slug, question_id, reason, channel, priority)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(slug, question_id) DO UPDATE SET
           priority = MAX(priority, excluded.priority),
           channel = excluded.channel,
           reason = excluded.reason",
        rusqlite::params![slug, question_id, reason, channel, priority],
    )?;
    Ok(())
}

/// Dequeue the highest-priority staleness items for a slug.
/// Returns up to `limit` items, deleting them from the queue.
/// SELECT + DELETE are wrapped in a transaction to prevent TOCTOU races.
pub fn dequeue_staleness(conn: &Connection, slug: &str, limit: u32) -> Result<Vec<StalenessItem>> {
    // Use IMMEDIATE transaction to prevent concurrent readers from seeing the
    // same rows before we delete them.
    conn.execute_batch("BEGIN IMMEDIATE")?;

    let result = (|| -> Result<Vec<StalenessItem>> {
        let mut stmt = conn.prepare(
            "SELECT id, slug, question_id, reason, channel, priority, created_at
             FROM pyramid_staleness_queue WHERE slug = ?1
             ORDER BY priority DESC, id ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![slug, limit], |row| {
            Ok(StalenessItem {
                id: row.get(0)?,
                slug: row.get(1)?,
                question_id: row.get(2)?,
                reason: row.get(3)?,
                channel: row.get(4)?,
                priority: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?;
        let items: Vec<StalenessItem> = rows.filter_map(|r| r.ok()).collect();

        // Delete the dequeued items using parameterized placeholders
        if !items.is_empty() {
            let ids: Vec<i64> = items.iter().map(|i| i.id).collect();
            let placeholders: String = (1..=ids.len())
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>()
                .join(",");
            conn.execute(
                &format!("DELETE FROM pyramid_staleness_queue WHERE id IN ({placeholders})"),
                rusqlite::params_from_iter(ids.iter()),
            )?;
        }

        Ok(items)
    })();

    match &result {
        Ok(_) => conn.execute_batch("COMMIT")?,
        Err(_) => {
            let _ = conn.execute_batch("ROLLBACK");
        }
    }

    result
}

// ── Table Migrations ─────────────────────────────────────────────────────────

/// Migrate `pyramid_staleness_queue` to add UNIQUE(slug, question_id) if missing.
/// Idempotent: skips if the constraint already exists.
fn migrate_staleness_queue_unique(conn: &Connection) -> Result<()> {
    let table_sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='pyramid_staleness_queue'",
            [],
            |row| row.get(0),
        )
        .ok();

    let needs_migration = match &table_sql {
        Some(sql) => !sql.contains("UNIQUE"),
        None => false, // Table doesn't exist yet (will be created fresh above)
    };

    if !needs_migration {
        return Ok(());
    }

    tracing::info!("Migrating pyramid_staleness_queue to add UNIQUE(slug, question_id)...");
    conn.execute_batch("PRAGMA foreign_keys=OFF;")?;
    let result = conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_staleness_queue_new (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL,
            question_id TEXT NOT NULL,
            reason TEXT NOT NULL,
            channel TEXT NOT NULL,
            priority REAL NOT NULL DEFAULT 0.0,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(slug, question_id)
        );
        INSERT OR REPLACE INTO pyramid_staleness_queue_new (id, slug, question_id, reason, channel, priority, created_at)
            SELECT id, slug, question_id, reason, channel, MAX(priority), created_at
            FROM pyramid_staleness_queue GROUP BY slug, question_id;
        DROP TABLE pyramid_staleness_queue;
        ALTER TABLE pyramid_staleness_queue_new RENAME TO pyramid_staleness_queue;
        CREATE INDEX IF NOT EXISTS idx_staleness_queue_slug ON pyramid_staleness_queue(slug, priority DESC);
        ",
    );
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;
    result?;
    Ok(())
}

/// Migrate `pyramid_gaps` to add UNIQUE(slug, question_id, description) and question index.
/// Idempotent: skips if the constraint already exists.
fn migrate_gaps_unique(conn: &Connection) -> Result<()> {
    let table_sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='pyramid_gaps'",
            [],
            |row| row.get(0),
        )
        .ok();

    let needs_migration = match &table_sql {
        Some(sql) => !sql.contains("UNIQUE"),
        None => false, // Table doesn't exist yet
    };

    if !needs_migration {
        // Still ensure the question index exists even if UNIQUE is already there
        let _ = conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_gaps_question ON pyramid_gaps(slug, question_id);",
        );
        return Ok(());
    }

    tracing::info!("Migrating pyramid_gaps to add UNIQUE(slug, question_id, description)...");
    conn.execute_batch("PRAGMA foreign_keys=OFF;")?;
    let result = conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_gaps_new (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL,
            question_id TEXT NOT NULL,
            description TEXT NOT NULL,
            layer INTEGER NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            build_id TEXT DEFAULT NULL,
            resolved INTEGER NOT NULL DEFAULT 0,
            UNIQUE(slug, question_id, description)
        );
        INSERT OR REPLACE INTO pyramid_gaps_new (id, slug, question_id, description, layer, created_at)
            SELECT id, slug, question_id, description, layer, created_at
            FROM pyramid_gaps GROUP BY slug, question_id, description;
        DROP TABLE pyramid_gaps;
        ALTER TABLE pyramid_gaps_new RENAME TO pyramid_gaps;
        CREATE INDEX IF NOT EXISTS idx_gaps_slug ON pyramid_gaps(slug);
        CREATE INDEX IF NOT EXISTS idx_gaps_question ON pyramid_gaps(slug, question_id);
        ",
    );
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;
    result?;
    Ok(())
}

// ── Evidence Backfill ────────────────────────────────────────────────────────

/// Migrate existing `pyramid_nodes.children` JSON arrays into `pyramid_evidence` rows.
/// Only runs if pyramid_evidence is empty but pyramid_nodes has children.
/// Creates evidence links with verdict=KEEP, weight=1.0, reason="legacy backfill".
fn backfill_evidence_from_children(conn: &Connection) -> Result<()> {
    // Check if pyramid_evidence already has data
    let evidence_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM pyramid_evidence", [], |row| {
            row.get(0)
        })?;
    if evidence_count > 0 {
        return Ok(());
    }

    // Check if any nodes have children
    let nodes_with_children: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pyramid_nodes WHERE children IS NOT NULL AND children != '[]' AND children != ''",
        [],
        |row| row.get(0),
    )?;
    if nodes_with_children == 0 {
        return Ok(());
    }

    tracing::info!(
        "Backfilling pyramid_evidence from {} nodes with children...",
        nodes_with_children
    );

    // 11-S: Read from live_pyramid_nodes (excludes superseded) instead of pyramid_nodes
    let node_rows: Vec<(String, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT slug, id, children FROM live_pyramid_nodes WHERE children IS NOT NULL AND children != '[]' AND children != ''",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        rows.filter_map(|r| r.ok()).collect()
    };

    // Wrap all inserts in a transaction to avoid partial backfill state on crash
    conn.execute_batch("BEGIN")?;
    let result = (|| -> Result<u64> {
        let mut count = 0u64;
        for (slug, parent_id, children_json) in &node_rows {
            let children: Vec<String> = match serde_json::from_str(children_json) {
                Ok(c) => c,
                Err(_) => continue,
            };
            for child_id in &children {
                // 11-K: Skip handle-path children (cross-slug references)
                // These should have proper evidence from the answering step
                if child_id.contains('/') {
                    continue;
                }
                let _ = conn.execute(
                    "INSERT OR IGNORE INTO pyramid_evidence (slug, source_node_id, target_node_id, verdict, weight, reason)
                     VALUES (?1, ?2, ?3, 'KEEP', 1.0, 'legacy backfill')",
                    rusqlite::params![slug, child_id, parent_id],
                );
                count += 1;
            }
        }
        Ok(count)
    })();

    match &result {
        Ok(_) => conn.execute_batch("COMMIT")?,
        Err(_) => {
            let _ = conn.execute_batch("ROLLBACK");
        }
    }

    let count = result?;
    if count > 0 {
        tracing::info!(
            "Backfilled {} evidence links from existing children.",
            count
        );
    }

    Ok(())
}

// ── Canonical L0 Helpers ─────────────────────────────────────────────────────

/// Check if canonical L0 exists for a slug (any live node matching C-L0-% pattern).
pub fn has_canonical_l0(conn: &Connection, slug: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM live_pyramid_nodes WHERE slug = ?1 AND id LIKE 'C-L0-%'",
        rusqlite::params![slug],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Get all canonical L0 nodes for a slug (live only).
pub fn get_canonical_l0_nodes(conn: &Connection, slug: &str) -> Result<Vec<PyramidNode>> {
    let sql = format!(
        "SELECT {NODE_SELECT_COLS} FROM live_pyramid_nodes
         WHERE slug = ?1 AND id LIKE 'C-L0-%'
         ORDER BY id ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![slug], node_from_row)?;
    let mut nodes = Vec::new();
    for row in rows {
        nodes.push(row?);
    }
    Ok(nodes)
}

/// Build a summary of canonical L0 for decomposition context (live only).
/// Returns: Vec of (node_id, headline, distilled_truncated_to_300_chars).
pub fn get_canonical_l0_summaries(
    conn: &Connection,
    slug: &str,
) -> Result<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT id, headline, distilled FROM live_pyramid_nodes
         WHERE slug = ?1 AND id LIKE 'C-L0-%'
         ORDER BY id ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![slug], |row| {
        let id: String = row.get(0)?;
        let headline: String = row.get(1)?;
        let distilled: String = row.get(2)?;
        Ok((id, headline, distilled))
    })?;
    let mut summaries = Vec::new();
    for row in rows {
        let (id, headline, distilled) = row?;
        // Truncate distilled to 300 chars
        let truncated = if distilled.len() > 300 {
            let mut end = 300;
            // Don't break in the middle of a UTF-8 character
            while !distilled.is_char_boundary(end) && end > 0 {
                end -= 1;
            }
            format!("{}...", &distilled[..end])
        } else {
            distilled
        };
        summaries.push((id, headline, truncated));
    }
    Ok(summaries)
}

/// Supersede canonical L0 nodes (for re-extraction when source files change).
pub fn supersede_canonical_l0(conn: &Connection, slug: &str, build_id: &str) -> Result<usize> {
    let count = conn.execute(
        "UPDATE pyramid_nodes SET superseded_by = ?2
         WHERE slug = ?1 AND id LIKE 'C-L0-%' AND superseded_by IS NULL",
        rusqlite::params![slug, build_id],
    )?;
    Ok(count)
}

/// Supersede question L0 nodes (for rebuild with different question).
pub fn supersede_question_l0(conn: &Connection, slug: &str, build_id: &str) -> Result<usize> {
    let count = conn.execute(
        "UPDATE pyramid_nodes SET superseded_by = ?2
         WHERE slug = ?1 AND id LIKE 'L0-%' AND id NOT LIKE 'C-L0-%' AND superseded_by IS NULL",
        rusqlite::params![slug, build_id],
    )?;
    Ok(count)
}

/// Load all chunk contents for a slug, ordered by chunk_index.
/// Returns Vec of (chunk_index, content).
pub fn get_all_chunks(conn: &Connection, slug: &str) -> Result<Vec<(i64, String)>> {
    let mut stmt = conn.prepare(
        "SELECT chunk_index, content FROM pyramid_chunks
         WHERE slug = ?1 ORDER BY chunk_index ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![slug], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut chunks = Vec::new();
    for row in rows {
        chunks.push(row?);
    }
    Ok(chunks)
}

// ── WS-ONLINE-D: Pinning / Daemon Caching ────────────────────────────────────

/// Pin a pyramid: set pinned=1 and store the source tunnel URL.
/// Creates the slug row if it doesn't exist (for remote pyramids being pinned for the first time).
pub fn pin_pyramid(conn: &Connection, slug: &str, tunnel_url: &str) -> Result<()> {
    // Try to update existing slug first
    let updated = conn.execute(
        "UPDATE pyramid_slugs SET pinned = 1, source_tunnel_url = ?2 WHERE slug = ?1",
        rusqlite::params![slug, tunnel_url],
    )?;

    if updated == 0 {
        // Slug doesn't exist yet — create it as a pinned remote pyramid
        conn.execute(
            "INSERT INTO pyramid_slugs (slug, content_type, source_path, pinned, source_tunnel_url)
             VALUES (?1, 'code', '', 1, ?2)",
            rusqlite::params![slug, tunnel_url],
        )?;
    }

    Ok(())
}

/// Unpin a pyramid: clear pinned flag and source_tunnel_url.
/// NEVER deletes node data (Pillar 1 — pinned data may have been queried,
/// cited, or used as evidence; it persists as historical record).
pub fn unpin_pyramid(conn: &Connection, slug: &str) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_slugs SET pinned = 0, source_tunnel_url = NULL WHERE slug = ?1",
        rusqlite::params![slug],
    )?;
    Ok(())
}

/// Bulk insert/update nodes from a remote export response.
/// Uses save_node under the hood for each node, preserving the same upsert semantics.
pub fn upsert_pinned_nodes(conn: &Connection, slug: &str, nodes: &[PyramidNode]) -> Result<usize> {
    let mut count = 0;
    for node in nodes {
        // Ensure the node's slug matches the target slug
        let mut pinned_node = node.clone();
        pinned_node.slug = slug.to_string();
        save_node(conn, &pinned_node, None)?;
        count += 1;
    }

    // Update slug stats (node_count, max_depth)
    let node_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM live_pyramid_nodes WHERE slug = ?1",
        rusqlite::params![slug],
        |row| row.get(0),
    )?;
    let max_depth: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(depth), 0) FROM live_pyramid_nodes WHERE slug = ?1",
            rusqlite::params![slug],
            |row| row.get(0),
        )
        .unwrap_or(0);

    conn.execute(
        "UPDATE pyramid_slugs SET node_count = ?2, max_depth = ?3 WHERE slug = ?1",
        rusqlite::params![slug, node_count, max_depth],
    )?;

    Ok(count)
}

/// Check whether a slug is pinned.
pub fn is_pinned(conn: &Connection, slug: &str) -> Result<bool> {
    let result = conn.query_row(
        "SELECT pinned FROM pyramid_slugs WHERE slug = ?1",
        rusqlite::params![slug],
        |row| row.get::<_, i64>(0),
    );
    match result {
        Ok(val) => Ok(val != 0),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Get the source tunnel URL for a pinned pyramid.
pub fn get_source_tunnel_url(conn: &Connection, slug: &str) -> Result<Option<String>> {
    let result = conn.query_row(
        "SELECT source_tunnel_url FROM pyramid_slugs WHERE slug = ?1",
        rusqlite::params![slug],
        |row| row.get::<_, Option<String>>(0),
    );
    match result {
        Ok(url) => Ok(url),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// List all pinned pyramids (slug, source_tunnel_url).
pub fn list_pinned_pyramids(conn: &Connection) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT slug, source_tunnel_url FROM pyramid_slugs
         WHERE pinned = 1 AND source_tunnel_url IS NOT NULL AND archived_at IS NULL",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut pinned = Vec::new();
    for row in rows {
        pinned.push(row?);
    }
    Ok(pinned)
}

/// Get all live nodes for export (used by the export endpoint).
/// Returns all non-superseded nodes for a slug, ordered by depth then chunk_index.
pub fn get_all_nodes_for_export(conn: &Connection, slug: &str) -> Result<Vec<PyramidNode>> {
    get_all_live_nodes(conn, slug)
}

// ── WS-ONLINE-H: Unredeemed token CRUD ──────────────────────────────────────

/// An unredeemed payment token awaiting retry (WS-ONLINE-H).
#[derive(Debug, Clone)]
pub struct UnredeemedToken {
    pub id: i64,
    pub nonce: String,
    pub payment_token: String,
    pub querier_operator_id: String,
    pub slug: String,
    pub query_type: String,
    pub stamp_amount: i64,
    pub access_amount: i64,
    pub total_amount: i64,
    pub created_at: String,
    pub expires_at: String,
    pub retry_count: i64,
    pub last_retry_at: Option<String>,
    pub redeemed_at: Option<String>,
    pub status: String,
}

/// Insert an unredeemed payment token for retry (WS-ONLINE-H).
///
/// Called when a serving node executes a query but the POST /api/v1/wire/payment-redeem
/// call fails (Wire server unavailable, network error, etc.). The token is stored for
/// retry with exponential backoff.
pub fn insert_unredeemed_token(
    conn: &Connection,
    nonce: &str,
    payment_token: &str,
    querier_operator_id: &str,
    slug: &str,
    query_type: &str,
    stamp_amount: i64,
    access_amount: i64,
    total_amount: i64,
    expires_at: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO pyramid_unredeemed_tokens
            (nonce, payment_token, querier_operator_id, slug, query_type,
             stamp_amount, access_amount, total_amount, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            nonce,
            payment_token,
            querier_operator_id,
            slug,
            query_type,
            stamp_amount,
            access_amount,
            total_amount,
            expires_at,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Get all unredeemed tokens that are still pending and have not expired.
///
/// Returns tokens ordered by created_at (oldest first) for FIFO retry.
/// Filters to status='pending' and retry_count < 5 (max retries).
pub fn get_unredeemed_tokens(conn: &Connection) -> Result<Vec<UnredeemedToken>> {
    let mut stmt = conn.prepare(
        "SELECT id, nonce, payment_token, querier_operator_id, slug, query_type,
                stamp_amount, access_amount, total_amount, created_at, expires_at,
                retry_count, last_retry_at, redeemed_at, status
         FROM pyramid_unredeemed_tokens
         WHERE status = 'pending' AND retry_count < 5
         ORDER BY created_at ASC",
    )?;

    let tokens = stmt
        .query_map([], |row| {
            Ok(UnredeemedToken {
                id: row.get(0)?,
                nonce: row.get(1)?,
                payment_token: row.get(2)?,
                querier_operator_id: row.get(3)?,
                slug: row.get(4)?,
                query_type: row.get(5)?,
                stamp_amount: row.get(6)?,
                access_amount: row.get(7)?,
                total_amount: row.get(8)?,
                created_at: row.get(9)?,
                expires_at: row.get(10)?,
                retry_count: row.get(11)?,
                last_retry_at: row.get(12)?,
                redeemed_at: row.get(13)?,
                status: row.get(14)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(tokens)
}

/// Mark an unredeemed token as redeemed (WS-ONLINE-H).
///
/// Called after a successful POST /api/v1/wire/payment-redeem response.
pub fn mark_redeemed(conn: &Connection, nonce: &str) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_unredeemed_tokens
         SET status = 'redeemed', redeemed_at = datetime('now')
         WHERE nonce = ?1 AND status = 'pending'",
        rusqlite::params![nonce],
    )?;
    Ok(())
}

/// Increment the retry count for an unredeemed token (WS-ONLINE-H).
///
/// Called after a failed redeem attempt. If retry_count reaches 5, the token
/// is automatically marked as 'failed'.
pub fn increment_unredeemed_retry(conn: &Connection, nonce: &str) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_unredeemed_tokens
         SET retry_count = retry_count + 1, last_retry_at = datetime('now')
         WHERE nonce = ?1 AND status = 'pending'",
        rusqlite::params![nonce],
    )?;

    // Auto-fail after 5 retries
    conn.execute(
        "UPDATE pyramid_unredeemed_tokens
         SET status = 'failed'
         WHERE nonce = ?1 AND retry_count >= 5 AND status = 'pending'",
        rusqlite::params![nonce],
    )?;

    Ok(())
}

/// Expire unredeemed tokens past their TTL (WS-ONLINE-H).
///
/// Should be called periodically (e.g., every 30 seconds) to clean up tokens
/// whose TTL has passed. Credits auto-unlock on the Wire server after TTL expiry.
pub fn expire_unredeemed_tokens(conn: &Connection) -> Result<usize> {
    let expired = conn.execute(
        "UPDATE pyramid_unredeemed_tokens
         SET status = 'expired'
         WHERE status = 'pending' AND expires_at < datetime('now')",
        [],
    )?;
    Ok(expired)
}

// ── Annotation Reactions & Agent Sessions ───────────────────────────────────

/// Save an annotation reaction (up/down vote). Uses INSERT OR REPLACE to allow changing votes.
pub fn save_annotation_reaction(
    conn: &Connection,
    annotation_id: i64,
    reaction: &str,
    agent_id: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO pyramid_annotation_reactions (annotation_id, reaction, agent_id) VALUES (?1, ?2, ?3)",
        rusqlite::params![annotation_id, reaction, agent_id],
    )?;
    Ok(())
}

/// Get reaction counts for an annotation.
pub fn get_annotation_reactions(conn: &Connection, annotation_id: i64) -> Result<(i64, i64)> {
    let up: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pyramid_annotation_reactions WHERE annotation_id = ?1 AND reaction = 'up'",
        rusqlite::params![annotation_id],
        |row| row.get(0),
    ).unwrap_or(0);
    let down: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pyramid_annotation_reactions WHERE annotation_id = ?1 AND reaction = 'down'",
        rusqlite::params![annotation_id],
        |row| row.get(0),
    ).unwrap_or(0);
    Ok((up, down))
}

/// Register an agent session.
pub fn register_agent_session(conn: &Connection, slug: &str, agent_id: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO pyramid_agent_sessions (slug, agent_id) VALUES (?1, ?2)",
        rusqlite::params![slug, agent_id],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Get recent agent sessions for a slug.
pub fn get_agent_sessions(conn: &Connection, slug: &str, limit: i64) -> Result<Vec<serde_json::Value>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, agent_id, started_at, last_activity, actions_count, summary
         FROM pyramid_agent_sessions WHERE slug = ?1
         ORDER BY last_activity DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![slug, limit], |row| {
        Ok(serde_json::json!({
            "id": row.get::<_, i64>(0)?,
            "slug": row.get::<_, String>(1)?,
            "agent_id": row.get::<_, String>(2)?,
            "started_at": row.get::<_, String>(3)?,
            "last_activity": row.get::<_, String>(4)?,
            "actions_count": row.get::<_, i64>(5)?,
            "summary": row.get::<_, Option<String>>(6)?,
        }))
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Bump session activity (fire-and-forget on each request with X-Agent-Id).
pub fn bump_agent_session(conn: &Connection, slug: &str, agent_id: &str) {
    let _ = conn.execute(
        "UPDATE pyramid_agent_sessions SET last_activity = datetime('now'), actions_count = actions_count + 1
         WHERE slug = ?1 AND agent_id = ?2 AND id = (SELECT MAX(id) FROM pyramid_agent_sessions WHERE slug = ?1 AND agent_id = ?2)",
        rusqlite::params![slug, agent_id],
    );
}

/// Set gap resolution confidence to a specific value.
pub fn set_gap_confidence(
    conn: &Connection,
    slug: &str,
    question_id: &str,
    description: &str,
    confidence: f64,
) -> Result<usize> {
    let rows = conn.execute(
        "UPDATE pyramid_gaps SET resolution_confidence = ?1, resolved = CASE WHEN ?1 >= 0.8 THEN 1 ELSE 0 END WHERE slug = ?2 AND question_id = ?3 AND description = ?4",
        rusqlite::params![confidence, slug, question_id, description],
    )?;
    Ok(rows)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::query::get_node_version as query_get_node_version;

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
                    ..Default::default()
                }],
                extra: serde_json::Map::new(),
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
            build_id: None,
            created_at: String::new(),
            ..Default::default()
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
                build_id: None,
                created_at: String::new(),
                ..Default::default()
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
    fn test_lookup_node_id_by_chunk_index_and_headline() {
        let conn = test_conn();
        create_slug(&conn, "s", &ContentType::Code, "").unwrap();

        let first = PyramidNode {
            id: "C-L0-000".to_string(),
            slug: "s".to_string(),
            depth: 0,
            chunk_index: Some(0),
            headline: "MCP Server Package Config".to_string(),
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
            build_id: None,
            created_at: String::new(),
            ..Default::default()
        };
        let second = PyramidNode {
            id: "C-L0-001".to_string(),
            slug: "s".to_string(),
            depth: 0,
            chunk_index: Some(1),
            headline: "mod.rs".to_string(),
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
            build_id: None,
            created_at: String::new(),
            ..Default::default()
        };

        save_node(&conn, &first, None).unwrap();
        save_node(&conn, &second, None).unwrap();

        assert_eq!(
            get_node_id_by_depth_and_chunk_index(&conn, "s", 0, 1).unwrap(),
            Some("C-L0-001".to_string())
        );
        assert_eq!(
            get_node_id_by_depth_and_headline(&conn, "s", 0, "MCP Server Package Config").unwrap(),
            Some("C-L0-000".to_string())
        );
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
                build_id: None,
                created_at: String::new(),
                ..Default::default()
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
    fn test_get_step_output_exact_disambiguates_by_depth_and_node() {
        let conn = test_conn();
        create_slug(&conn, "s", &ContentType::Code, "").unwrap();

        save_step(
            &conn,
            "s",
            "synth",
            -1,
            2,
            "L2-000",
            r#"{"node":"L2-000"}"#,
            "gpt-4",
            1.0,
        )
        .unwrap();
        save_step(
            &conn,
            "s",
            "synth",
            -1,
            3,
            "L3-000",
            r#"{"node":"L3-000"}"#,
            "gpt-4",
            1.0,
        )
        .unwrap();

        let l2 = get_step_output_exact(&conn, "s", "synth", -1, 2, "L2-000")
            .unwrap()
            .unwrap();
        let l3 = get_step_output_exact(&conn, "s", "synth", -1, 3, "L3-000")
            .unwrap()
            .unwrap();

        assert!(l2.contains("L2-000"));
        assert!(l3.contains("L3-000"));
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
                build_id: None,
                created_at: String::new(),
                ..Default::default()
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
            build_id: None,
            created_at: String::new(),
            ..Default::default()
        };
        save_node(&conn, &apex, None).unwrap();

        update_slug_stats(&conn, "s").unwrap();

        let info = get_slug(&conn, "s").unwrap().unwrap();
        assert_eq!(info.node_count, 4);
        assert_eq!(info.max_depth, 1);
        assert!(info.last_built_at.is_some());
    }

    #[test]
    fn test_touch_slug_bumps_updated_at() {
        let conn = test_conn();
        create_slug(&conn, "s", &ContentType::Code, "").unwrap();

        let read_updated = |c: &Connection| -> String {
            c.query_row(
                "SELECT updated_at FROM pyramid_slugs WHERE slug = ?1",
                rusqlite::params!["s"],
                |row| row.get::<_, String>(0),
            )
            .unwrap()
        };

        let before = read_updated(&conn);
        // sqlite datetime('now') has 1-second resolution; sleep just over 1s.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        touch_slug(&conn, "s").unwrap();
        let after = read_updated(&conn);
        assert_ne!(
            before, after,
            "touch_slug must bump pyramid_slugs.updated_at"
        );
    }

    #[test]
    fn test_node_upsert() {
        let conn = test_conn();
        create_slug(&conn, "s", &ContentType::Code, "").unwrap();

        // WS-IMMUTABILITY-ENFORCE: use depth >= 2 so the upsert path
        // (apply_supersession) is exercised without hitting the bedrock
        // immutability guard that rejects canonical L0/L1 updates.
        let mut node = PyramidNode {
            id: "n1".to_string(),
            slug: "s".to_string(),
            depth: 2,
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
            build_id: None,
            created_at: String::new(),
            ..Default::default()
        };
        save_node(&conn, &node, None).unwrap();

        // Upsert with new content
        node.distilled = "Version 2".to_string();
        save_node(&conn, &node, None).unwrap();

        let got = get_node(&conn, "s", "n1").unwrap().unwrap();
        assert_eq!(got.distilled, "Version 2");

        // Should still be 1 node, not 2
        assert_eq!(count_nodes_at_depth(&conn, "s", 2).unwrap(), 1);
    }

    /// WS-ONLINE-S3: delete_web_edges_for_depth is now a no-op (build_id scoping).
    /// This test verifies edges are preserved and that save_web_edge writes
    /// build_id + last_confirmed_at, and that decay archives instead of deleting.
    #[test]
    fn test_web_edge_build_id_scoping_and_archival() {
        let conn = test_conn();
        create_slug(&conn, "s", &ContentType::Code, "").unwrap();

        for (id, depth) in [("L1-000", 1), ("L1-001", 1), ("L2-000", 2), ("L2-001", 2)] {
            let node = PyramidNode {
                id: id.to_string(),
                slug: "s".to_string(),
                depth,
                chunk_index: None,
                headline: id.to_string(),
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
                build_id: None,
                created_at: String::new(),
                ..Default::default()
            };
            save_node(&conn, &node, None).unwrap();

            let thread = PyramidThread {
                slug: "s".into(),
                thread_id: id.to_string(),
                thread_name: id.to_string(),
                current_canonical_id: id.to_string(),
                depth,
                delta_count: 0,
                created_at: "now".into(),
                updated_at: "now".into(),
            };
            save_thread(&conn, &thread).unwrap();
        }

        // Save edges with build_id
        save_web_edge(
            &conn,
            &WebEdge {
                id: 0,
                slug: "s".into(),
                thread_a_id: "L1-000".into(),
                thread_b_id: "L1-001".into(),
                relationship: "L1 edge".into(),
                relevance: 0.8,
                delta_count: 0,
                build_id: Some("build-1".into()),
                created_at: String::new(),
                updated_at: String::new(),
            },
        )
        .unwrap();
        save_web_edge(
            &conn,
            &WebEdge {
                id: 0,
                slug: "s".into(),
                thread_a_id: "L2-000".into(),
                thread_b_id: "L2-001".into(),
                relationship: "L2 edge".into(),
                relevance: 0.9,
                delta_count: 0,
                build_id: Some("build-1".into()),
                created_at: String::new(),
                updated_at: String::new(),
            },
        )
        .unwrap();

        // delete_web_edges_for_depth is now a no-op — edges preserved
        let deleted = delete_web_edges_for_depth(&conn, "s", 1).unwrap();
        assert_eq!(deleted, 0);

        let all_edges = get_web_edges(&conn, "s").unwrap();
        assert_eq!(all_edges.len(), 2); // Both edges still present

        // Verify build_id was written
        assert_eq!(all_edges[0].build_id, Some("build-1".into()));

        // Verify last_confirmed_at was set (non-NULL)
        let has_confirmed: bool = conn
            .query_row(
                "SELECT last_confirmed_at IS NOT NULL FROM pyramid_web_edges WHERE slug = 's' LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(has_confirmed);

        // Upsert with new build_id updates the edge
        save_web_edge(
            &conn,
            &WebEdge {
                id: 0,
                slug: "s".into(),
                thread_a_id: "L1-000".into(),
                thread_b_id: "L1-001".into(),
                relationship: "L1 edge updated".into(),
                relevance: 0.05, // Below threshold
                delta_count: 0,
                build_id: Some("build-2".into()),
                created_at: String::new(),
                updated_at: String::new(),
            },
        )
        .unwrap();

        // Verify build_id updated
        let edge = get_web_edge_between(&conn, "s", "L1-000", "L1-001")
            .unwrap()
            .unwrap();
        assert_eq!(edge.build_id, Some("build-2".into()));
        assert_eq!(edge.relationship, "L1 edge updated");

        // Decay: edge at 0.05 is below 0.1 but last_confirmed_at is recent (just saved)
        // so the 7-day guard should protect it
        let archived = decay_web_edges(&conn, "s", 0.0).unwrap(); // decay_rate 0 to not change relevance
        assert_eq!(archived, 0); // Protected by last_confirmed_at guard

        // Backdate last_confirmed_at to bypass the guard
        conn.execute(
            "UPDATE pyramid_web_edges SET last_confirmed_at = datetime('now', '-8 days')
             WHERE slug = 's' AND thread_a_id = 'L1-000'",
            [],
        )
        .unwrap();

        // Now decay should archive the low-relevance edge
        let archived = decay_web_edges(&conn, "s", 0.0).unwrap();
        assert_eq!(archived, 1);

        // Archived edge excluded from get_web_edges
        let active = get_web_edges(&conn, "s").unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].thread_a_id, "L2-000");

        // Archived edge excluded from get_web_edge_between
        let gone = get_web_edge_between(&conn, "s", "L1-000", "L1-001").unwrap();
        assert!(gone.is_none());

        // But get_web_edge by ID still returns it (for collapse logic)
        let by_id = get_web_edge(&conn, edge.id).unwrap();
        assert!(by_id.is_some());

        // Re-saving an archived edge un-archives it (archived_at = NULL on upsert)
        save_web_edge(
            &conn,
            &WebEdge {
                id: 0,
                slug: "s".into(),
                thread_a_id: "L1-000".into(),
                thread_b_id: "L1-001".into(),
                relationship: "L1 edge revived".into(),
                relevance: 0.9,
                delta_count: 0,
                build_id: Some("build-3".into()),
                created_at: String::new(),
                updated_at: String::new(),
            },
        )
        .unwrap();

        let revived = get_web_edge_between(&conn, "s", "L1-000", "L1-001").unwrap();
        assert!(revived.is_some());
        assert_eq!(revived.unwrap().relationship, "L1 edge revived");
    }

    /// 11-J: Verify same-slug and cross-slug evidence can coexist with the same bare node ID.
    /// E.g., (qslug, "L0-003", Q1-001) and (qslug, "base/0/L0-003", Q1-001) are distinct rows.
    #[test]
    fn test_evidence_pk_cross_slug_coexistence() {
        let conn = test_conn();
        create_slug(&conn, "qslug", &ContentType::Code, "").unwrap();

        // Same-slug evidence: bare ID
        let link_same = EvidenceLink {
            slug: "qslug".to_string(),
            source_node_id: "L0-003".to_string(),
            target_node_id: "Q1-001".to_string(),
            verdict: EvidenceVerdict::Keep,
            weight: Some(0.8),
            reason: Some("direct match".to_string()),
            build_id: Some("b1".to_string()),
            live: None,
        };
        save_evidence_link(&conn, &link_same).unwrap();

        // Cross-slug evidence: handle-path ID (different source_node_id)
        let link_cross = EvidenceLink {
            slug: "qslug".to_string(),
            source_node_id: "base/0/L0-003".to_string(),
            target_node_id: "Q1-001".to_string(),
            verdict: EvidenceVerdict::Keep,
            weight: Some(0.9),
            reason: Some("cross-slug match".to_string()),
            build_id: Some("b1".to_string()),
            live: None,
        };
        save_evidence_link(&conn, &link_cross).unwrap();

        // Both should coexist — different source_node_id values
        let all = get_evidence_for_target(&conn, "qslug", "Q1-001").unwrap();
        assert_eq!(
            all.len(),
            2,
            "same-slug and cross-slug evidence must coexist"
        );

        // Verify both round-trip correctly
        let bare_link = all.iter().find(|l| l.source_node_id == "L0-003").unwrap();
        assert_eq!(bare_link.weight, Some(0.8));

        let handle_link = all
            .iter()
            .find(|l| l.source_node_id == "base/0/L0-003")
            .unwrap();
        assert_eq!(handle_link.weight, Some(0.9));
    }

    // ── WS-SCHEMA-V2 integration test ───────────────────────────────────
    #[test]
    fn ws_schema_v2_versioning_round_trip() {
        let conn = test_conn();
        create_slug(&conn, "vt", &ContentType::Code, "").unwrap();

        // Write v1 via save_node (first write — INSERT path).
        // WS-IMMUTABILITY-ENFORCE: use depth >= 2 so the versioning round-trip
        // (save_node -> apply_supersession) is exercised without hitting the
        // bedrock immutability guard that rejects canonical L0/L1 updates.
        let mut node = PyramidNode {
            id: "n-1".to_string(),
            slug: "vt".to_string(),
            depth: 2,
            headline: "initial".to_string(),
            distilled: "first version".to_string(),
            time_range: Some(TimeRange {
                start: Some("2026-04-08T00:00:00Z".to_string()),
                end: Some("2026-04-08T01:00:00Z".to_string()),
            }),
            weight: 0.75,
            narrative: NarrativeMultiZoom {
                levels: vec![NarrativeLevel {
                    zoom: 0,
                    text: "zoom-0 narrative".to_string(),
                }],
            },
            entities: vec![Entity {
                name: "Alice".to_string(),
                role: "person".to_string(),
                importance: 0.9,
                liveness: "live".to_string(),
            }],
            key_quotes: vec![KeyQuote {
                text: "hello".to_string(),
                speaker_role: "human".to_string(),
                importance: 0.5,
                chunk_ref: None,
            }],
            transitions: Transitions {
                prior: "start".to_string(),
                next: "next".to_string(),
            },
            current_version: 1,
            ..Default::default()
        };
        save_node(&conn, &node, None).unwrap();

        // Round-trip read: every new field survives INSERT + SELECT.
        let loaded = get_node(&conn, "vt", "n-1").unwrap().unwrap();
        assert_eq!(loaded.headline, "initial");
        assert_eq!(loaded.distilled, "first version");
        assert_eq!(loaded.current_version, 1);
        assert_eq!(loaded.weight, 0.75);
        assert!(loaded.time_range.is_some());
        assert_eq!(
            loaded.time_range.as_ref().unwrap().start.as_deref(),
            Some("2026-04-08T00:00:00Z")
        );
        assert_eq!(loaded.narrative.levels.len(), 1);
        assert_eq!(loaded.narrative.levels[0].text, "zoom-0 narrative");
        assert_eq!(loaded.entities.len(), 1);
        assert_eq!(loaded.entities[0].name, "Alice");
        assert_eq!(loaded.key_quotes.len(), 1);
        assert_eq!(loaded.transitions.prior, "start");

        // Second write via save_node routes through apply_supersession.
        node.headline = "second".to_string();
        node.distilled = "second version".to_string();
        node.narrative.levels[0].text = "zoom-0 revised".to_string();
        save_node(&conn, &node, None).unwrap();

        // Live row is the new content, current_version bumped.
        let live = get_node(&conn, "vt", "n-1").unwrap().unwrap();
        assert_eq!(live.headline, "second");
        assert_eq!(live.distilled, "second version");
        assert_eq!(live.current_version, 2);

        // pyramid_node_versions holds the prior snapshot at version 1.
        let prior = query_get_node_version(&conn, "vt", "n-1", 1)
            .unwrap()
            .expect("version 1 snapshot missing");
        assert_eq!(prior.headline, "initial");
        assert_eq!(prior.distilled, "first version");
        assert_eq!(prior.narrative.levels[0].text, "zoom-0 narrative");
        assert_eq!(prior.current_version, 1);

        // Non-existent version returns None.
        assert!(query_get_node_version(&conn, "vt", "n-1", 42)
            .unwrap()
            .is_none());

        // Third write: apply_supersession directly with a custom reason.
        let mut v3 = node.clone();
        v3.headline = "third".to_string();
        let new_version = apply_supersession(
            &conn, "vt", "n-1", &v3, "delta", "delta", "test-chain",
        )
        .unwrap();
        assert_eq!(new_version, 3);

        let live3 = get_node(&conn, "vt", "n-1").unwrap().unwrap();
        assert_eq!(live3.headline, "third");
        assert_eq!(live3.current_version, 3);

        // Version 2 now holds the "second" snapshot; version 1 is unchanged.
        let v1 = query_get_node_version(&conn, "vt", "n-1", 1)
            .unwrap()
            .unwrap();
        assert_eq!(v1.headline, "initial");
        let v2 = query_get_node_version(&conn, "vt", "n-1", 2)
            .unwrap()
            .unwrap();
        assert_eq!(v2.headline, "second");

        // mutate_provisional_node is a no-op on a canonical (provisional=0) row.
        let mut m = v3.clone();
        m.headline = "provisional change".to_string();
        let changed = mutate_provisional_node(&conn, "vt", "n-1", &m).unwrap();
        assert_eq!(changed, 0);
        let still = get_node(&conn, "vt", "n-1").unwrap().unwrap();
        assert_eq!(still.headline, "third"); // untouched

        // Flip the row to provisional and verify mutate_provisional_node works.
        conn.execute(
            "UPDATE pyramid_nodes SET provisional = 1 WHERE slug = ?1 AND id = ?2",
            rusqlite::params!["vt", "n-1"],
        )
        .unwrap();
        let changed2 = mutate_provisional_node(&conn, "vt", "n-1", &m).unwrap();
        assert_eq!(changed2, 1);
        let prov = get_node(&conn, "vt", "n-1").unwrap().unwrap();
        assert_eq!(prov.headline, "provisional change");
        // current_version must NOT change for provisional mutations.
        assert_eq!(prov.current_version, 3);
    }

    // ── WS-IMMUTABILITY-ENFORCE tests ───────────────────────────────────

    /// Test 1: Save a canonical L0 node, then attempt to update it via
    /// save_node — should fail with immutability error.
    #[test]
    fn test_immutability_canonical_l0_rejects_update() {
        let conn = test_conn();
        create_slug(&conn, "s", &ContentType::Code, "").unwrap();

        let node = PyramidNode {
            id: "imm-l0".to_string(),
            slug: "s".to_string(),
            depth: 0,
            headline: "Bedrock L0".to_string(),
            distilled: "original".to_string(),
            ..Default::default()
        };
        // First write succeeds (INSERT path).
        save_node(&conn, &node, None).unwrap();

        // Second write should fail — canonical L0 is immutable.
        let mut updated = node.clone();
        updated.distilled = "mutated".to_string();
        let err = save_node(&conn, &updated, None);
        assert!(err.is_err(), "Expected immutability error for canonical L0 update");
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("Cannot mutate immutable bedrock node"),
            "Error should mention immutability: {msg}"
        );

        // Verify the original content is untouched.
        let got = get_node(&conn, "s", "imm-l0").unwrap().unwrap();
        assert_eq!(got.distilled, "original");
    }

    /// Test 2: Save a provisional L0 node, update it via mutate_provisional_node
    /// — should succeed.
    #[test]
    fn test_immutability_provisional_l0_allows_mutation() {
        let conn = test_conn();
        create_slug(&conn, "s", &ContentType::Code, "").unwrap();

        let node = PyramidNode {
            id: "prov-l0".to_string(),
            slug: "s".to_string(),
            depth: 0,
            headline: "Provisional L0".to_string(),
            distilled: "original".to_string(),
            provisional: true,
            ..Default::default()
        };
        save_node(&conn, &node, None).unwrap();

        // Update via mutate_provisional_node should succeed.
        let mut updated = node.clone();
        updated.distilled = "mutated via provisional".to_string();
        let changed = mutate_provisional_node(&conn, "s", "prov-l0", &updated).unwrap();
        assert_eq!(changed, 1, "Provisional node mutation should succeed");

        let got = get_node(&conn, "s", "prov-l0").unwrap().unwrap();
        assert_eq!(got.distilled, "mutated via provisional");

        // Update via save_node should also succeed (routes through mutate_provisional_node).
        let mut updated2 = node.clone();
        updated2.distilled = "mutated again".to_string();
        save_node(&conn, &updated2, None).unwrap();

        let got2 = get_node(&conn, "s", "prov-l0").unwrap().unwrap();
        assert_eq!(got2.distilled, "mutated again");
    }

    /// Test 3: Promote a provisional L0 node, then attempt to update it —
    /// should fail with immutability error.
    #[test]
    fn test_immutability_promoted_l0_rejects_update() {
        let conn = test_conn();
        create_slug(&conn, "s", &ContentType::Code, "").unwrap();

        let node = PyramidNode {
            id: "prom-l0".to_string(),
            slug: "s".to_string(),
            depth: 0,
            headline: "Provisional L0".to_string(),
            distilled: "original".to_string(),
            provisional: true,
            ..Default::default()
        };
        save_node(&conn, &node, None).unwrap();

        // Promote: provisional -> canonical.
        let promoted = promote_provisional_node(&conn, "s", "prom-l0").unwrap();
        assert!(promoted, "Node should have been promoted");

        // Verify it's no longer provisional.
        let got = get_node(&conn, "s", "prom-l0").unwrap().unwrap();
        assert!(!got.provisional, "Node should be canonical after promotion");

        // Attempt to update via save_node — should fail.
        let mut updated = node.clone();
        updated.distilled = "mutated after promotion".to_string();
        updated.provisional = false; // reflect the promoted state
        let err = save_node(&conn, &updated, None);
        assert!(err.is_err(), "Expected immutability error after promotion");
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("Cannot mutate immutable bedrock node"),
            "Error should mention immutability: {msg}"
        );

        // Attempt to update via apply_supersession — should also fail.
        let err2 = apply_supersession(&conn, "s", "prom-l0", &updated, "rebuild", "rebuild", "");
        assert!(err2.is_err(), "apply_supersession should also reject immutable bedrock");
    }

    /// Test 4: Save an L2 node, update via apply_supersession — should succeed.
    #[test]
    fn test_immutability_l2_allows_supersession() {
        let conn = test_conn();
        create_slug(&conn, "s", &ContentType::Code, "").unwrap();

        let node = PyramidNode {
            id: "l2-node".to_string(),
            slug: "s".to_string(),
            depth: 2,
            headline: "L2 Node".to_string(),
            distilled: "original L2".to_string(),
            ..Default::default()
        };
        save_node(&conn, &node, None).unwrap();

        // Update via apply_supersession — L2 is mutable.
        let mut updated = node.clone();
        updated.distilled = "superseded L2".to_string();
        let new_version = apply_supersession(
            &conn, "s", "l2-node", &updated, "delta", "delta", "test-chain",
        )
        .unwrap();
        assert_eq!(new_version, 2, "L2 supersession should bump version");

        let got = get_node(&conn, "s", "l2-node").unwrap().unwrap();
        assert_eq!(got.distilled, "superseded L2");

        // Also test via save_node (routes through apply_supersession for non-provisional).
        let mut updated2 = node.clone();
        updated2.distilled = "superseded L2 again".to_string();
        save_node(&conn, &updated2, None).unwrap();

        let got2 = get_node(&conn, "s", "l2-node").unwrap().unwrap();
        assert_eq!(got2.distilled, "superseded L2 again");
    }

    /// Test 5: Promote a non-provisional node — should return false.
    #[test]
    fn test_promote_non_provisional_returns_false() {
        let conn = test_conn();
        create_slug(&conn, "s", &ContentType::Code, "").unwrap();

        // Save a canonical (non-provisional) node.
        let node = PyramidNode {
            id: "canon-l0".to_string(),
            slug: "s".to_string(),
            depth: 0,
            headline: "Canonical L0".to_string(),
            distilled: "already canonical".to_string(),
            provisional: false,
            ..Default::default()
        };
        save_node(&conn, &node, None).unwrap();

        // Attempt promotion on a non-provisional node.
        let result = promote_provisional_node(&conn, "s", "canon-l0").unwrap();
        assert!(!result, "Promoting a non-provisional node should return false");

        // Also test promoting a non-existent node.
        let result2 = promote_provisional_node(&conn, "s", "does-not-exist").unwrap();
        assert!(!result2, "Promoting a non-existent node should return false");
    }

    // ── WS-PROVISIONAL (Phase 2b): Provisional session lifecycle tests ──────

    /// Test 1: Create session, add provisional nodes, verify they're queryable.
    #[test]
    fn test_provisional_session_create_and_query() {
        let conn = test_conn();
        create_slug(&conn, "ps", &ContentType::Conversation, "/tmp/chat.jsonl").unwrap();

        let sid = "test-session-001";
        create_provisional_session(&conn, "ps", "/tmp/chat.jsonl", sid).unwrap();

        // Session should be active
        let session = get_provisional_session(&conn, sid).unwrap().unwrap();
        assert_eq!(session.status, "active");
        assert_eq!(session.slug, "ps");
        assert_eq!(session.source_path, "/tmp/chat.jsonl");
        assert!(session.provisional_node_ids.is_empty());

        // Add two provisional nodes to the session
        add_provisional_node_to_session(&conn, sid, "prov-n1").unwrap();
        add_provisional_node_to_session(&conn, sid, "prov-n2").unwrap();

        let node_ids = get_provisional_nodes_for_session(&conn, sid).unwrap();
        assert_eq!(node_ids, vec!["prov-n1", "prov-n2"]);

        // List active sessions
        let active = get_active_provisional_sessions(&conn, "ps").unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].session_id, sid);
        assert_eq!(active[0].provisional_node_ids, vec!["prov-n1", "prov-n2"]);
    }

    /// Test 2: Promote session — all nodes become non-provisional.
    #[test]
    fn test_provisional_session_promote_all_nodes() {
        let conn = test_conn();
        create_slug(&conn, "ps2", &ContentType::Conversation, "/tmp/chat.jsonl").unwrap();

        let sid = "test-session-002";
        create_provisional_session(&conn, "ps2", "/tmp/chat.jsonl", sid).unwrap();

        // Create two provisional nodes via standard save_node path
        let node1 = PyramidNode {
            id: "prov-a".to_string(),
            slug: "ps2".to_string(),
            depth: 0,
            headline: "Provisional Node A".to_string(),
            distilled: "content a".to_string(),
            provisional: true,
            ..Default::default()
        };
        save_node(&conn, &node1, None).unwrap();
        add_provisional_node_to_session(&conn, sid, "prov-a").unwrap();

        let node2 = PyramidNode {
            id: "prov-b".to_string(),
            slug: "ps2".to_string(),
            depth: 0,
            headline: "Provisional Node B".to_string(),
            distilled: "content b".to_string(),
            provisional: true,
            ..Default::default()
        };
        save_node(&conn, &node2, None).unwrap();
        add_provisional_node_to_session(&conn, sid, "prov-b").unwrap();

        // Verify nodes are provisional
        let loaded_a = get_node(&conn, "ps2", "prov-a").unwrap().unwrap();
        assert!(loaded_a.provisional, "Node A should be provisional");
        let loaded_b = get_node(&conn, "ps2", "prov-b").unwrap().unwrap();
        assert!(loaded_b.provisional, "Node B should be provisional");

        // Promote the session
        let count = promote_session(&conn, sid, "build-canonical-001", None).unwrap();
        assert_eq!(count, 2, "Should have promoted 2 nodes");

        // Verify nodes are now canonical (provisional=false)
        let after_a = get_node(&conn, "ps2", "prov-a").unwrap().unwrap();
        assert!(!after_a.provisional, "Node A should be canonical after promotion");
        let after_b = get_node(&conn, "ps2", "prov-b").unwrap().unwrap();
        assert!(!after_b.provisional, "Node B should be canonical after promotion");

        // Verify session status
        let session = get_provisional_session(&conn, sid).unwrap().unwrap();
        assert_eq!(session.status, "promoted");
        assert_eq!(session.canonical_build_id, Some("build-canonical-001".to_string()));
    }

    /// Test 3: Promoted provisional nodes reject subsequent mutations (immutability).
    #[test]
    fn test_promoted_provisional_rejects_mutation() {
        let conn = test_conn();
        create_slug(&conn, "ps3", &ContentType::Conversation, "").unwrap();

        let sid = "test-session-003";
        create_provisional_session(&conn, "ps3", "/tmp/chat.jsonl", sid).unwrap();

        // Create and save a provisional L0 node
        let node = PyramidNode {
            id: "prom-mut".to_string(),
            slug: "ps3".to_string(),
            depth: 0,
            headline: "Provisional L0".to_string(),
            distilled: "original content".to_string(),
            provisional: true,
            ..Default::default()
        };
        save_node(&conn, &node, None).unwrap();
        add_provisional_node_to_session(&conn, sid, "prom-mut").unwrap();

        // While provisional: mutation should work (via mutate_provisional_node)
        let mut updated = node.clone();
        updated.distilled = "mutated while provisional".to_string();
        save_node(&conn, &updated, None).unwrap();
        let got = get_node(&conn, "ps3", "prom-mut").unwrap().unwrap();
        assert_eq!(got.distilled, "mutated while provisional");

        // Promote the session
        promote_session(&conn, sid, "build-001", None).unwrap();

        // After promotion: mutation should fail (immutability guard)
        let mut post_promote = node.clone();
        post_promote.distilled = "should fail".to_string();
        post_promote.provisional = false;
        let err = save_node(&conn, &post_promote, None);
        assert!(
            err.is_err(),
            "Mutation of promoted L0 should fail with immutability error"
        );
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("Cannot mutate immutable bedrock node"),
            "Expected immutability error, got: {msg}"
        );
    }

    /// Test 4: Promote already-promoted session is idempotent.
    #[test]
    fn test_promote_already_promoted_is_idempotent() {
        let conn = test_conn();
        create_slug(&conn, "ps4", &ContentType::Conversation, "").unwrap();

        let sid = "test-session-004";
        create_provisional_session(&conn, "ps4", "/tmp/chat.jsonl", sid).unwrap();

        // Create one provisional node
        let node = PyramidNode {
            id: "prov-idem".to_string(),
            slug: "ps4".to_string(),
            depth: 0,
            headline: "Idem Node".to_string(),
            distilled: "idempotent test".to_string(),
            provisional: true,
            ..Default::default()
        };
        save_node(&conn, &node, None).unwrap();
        add_provisional_node_to_session(&conn, sid, "prov-idem").unwrap();

        // First promotion
        let count1 = promote_session(&conn, sid, "build-idem-001", None).unwrap();
        assert_eq!(count1, 1, "First promotion should promote 1 node");

        // Second promotion — should return 0 (idempotent)
        let count2 = promote_session(&conn, sid, "build-idem-002", None).unwrap();
        assert_eq!(count2, 0, "Second promotion should be idempotent (0 nodes)");

        // Session should still be in promoted state with the original build_id
        let session = get_provisional_session(&conn, sid).unwrap().unwrap();
        assert_eq!(session.status, "promoted");
        assert_eq!(
            session.canonical_build_id,
            Some("build-idem-001".to_string()),
            "canonical_build_id should not change on idempotent re-promote"
        );
    }

    /// Test 5: save_provisional_node creates node with provisional=true and
    /// tracks it in the session.
    #[test]
    fn test_save_provisional_node_convenience() {
        let conn = test_conn();
        create_slug(&conn, "ps5", &ContentType::Conversation, "").unwrap();

        let sid = "test-session-005";
        create_provisional_session(&conn, "ps5", "/tmp/chat.jsonl", sid).unwrap();

        let node = PyramidNode {
            id: "spn-1".to_string(),
            slug: "ps5".to_string(),
            depth: 0,
            headline: "Convenience Node".to_string(),
            distilled: "via save_provisional_node".to_string(),
            provisional: true,
            ..Default::default()
        };

        // Use the convenience function (no event bus for unit tests)
        save_provisional_node(&conn, &node, sid, None).unwrap();

        // Verify node was saved as provisional
        let loaded = get_node(&conn, "ps5", "spn-1").unwrap().unwrap();
        assert!(loaded.provisional, "Node should be provisional");
        assert_eq!(loaded.headline, "Convenience Node");

        // Verify it was tracked in the session
        let tracked = get_provisional_nodes_for_session(&conn, sid).unwrap();
        assert_eq!(tracked, vec!["spn-1"]);

        // Verify calling with provisional=false errors
        let bad_node = PyramidNode {
            id: "spn-bad".to_string(),
            slug: "ps5".to_string(),
            depth: 0,
            headline: "Bad Node".to_string(),
            provisional: false,
            ..Default::default()
        };
        let err = save_provisional_node(&conn, &bad_node, sid, None);
        assert!(err.is_err(), "save_provisional_node should reject non-provisional node");
    }
}

// ── WS-3: Evidence Density Statistics ───────────────────────────────────────

/// Returns evidence link density statistics for a pyramid slug.
///
/// Queries `live_pyramid_evidence` (excludes superseded links) joined to
/// `live_pyramid_nodes` for depth/headline metadata. Returns a JSON object
/// with `per_layer` (KEEP link counts grouped by target node depth) and
/// `top_nodes` (top 50 nodes by inbound KEEP links).
pub fn get_evidence_density(conn: &Connection, slug: &str) -> Result<serde_json::Value> {
    // Per layer: count KEEP links grouped by target node's depth
    let mut layer_stmt = conn.prepare(
        "SELECT pn.depth, COUNT(*) as keep_count
         FROM live_pyramid_evidence pe
         JOIN live_pyramid_nodes pn ON pe.target_node_id = pn.id AND pe.slug = pn.slug
         WHERE pe.slug = ?1
         GROUP BY pn.depth
         ORDER BY pn.depth ASC",
    )?;
    let per_layer: Vec<serde_json::Value> = layer_stmt
        .query_map(rusqlite::params![slug], |row| {
            let depth: i64 = row.get(0)?;
            let keep_count: i64 = row.get(1)?;
            Ok(serde_json::json!({
                "layer": depth,
                "keep_count": keep_count,
            }))
        })?
        .filter_map(|r| r.ok())
        .collect();

    // Top nodes by inbound KEEP links
    let mut top_stmt = conn.prepare(
        "SELECT pe.target_node_id, pn.headline, pn.depth, COUNT(*) as inbound_links
         FROM live_pyramid_evidence pe
         JOIN live_pyramid_nodes pn ON pe.target_node_id = pn.id AND pe.slug = pn.slug
         WHERE pe.slug = ?1
         GROUP BY pe.target_node_id
         ORDER BY inbound_links DESC
         LIMIT 50",
    )?;
    let top_nodes: Vec<serde_json::Value> = top_stmt
        .query_map(rusqlite::params![slug], |row| {
            let node_id: String = row.get(0)?;
            let headline: String = row.get(1)?;
            let depth: i64 = row.get(2)?;
            let inbound_links: i64 = row.get(3)?;
            Ok(serde_json::json!({
                "node_id": node_id,
                "headline": headline,
                "depth": depth,
                "inbound_links": inbound_links,
            }))
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(serde_json::json!({
        "per_layer": per_layer,
        "top_nodes": top_nodes,
    }))
}

// ── WS-DEADLETTER (§15.18) ─────────────────────────────────────────────
//
// Persistent queue of chain steps whose retry budget was exhausted. Helpers
// in this block do NOT acquire the per-slug write lock — callers must hold
// the lock (build_runner::run_build_from already holds it for in-build
// inserts; routes.rs handlers take it for operator-initiated skip/retry).
// This reflects the reentrancy constraint of tokio::sync::RwLock.

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DeadLetterEntry {
    pub id: i64,
    pub slug: String,
    pub chain_id: Option<String>,
    pub step_name: String,
    pub step_primitive: String,
    pub chunk_index: Option<i64>,
    pub input_snapshot: Option<String>,
    pub step_snapshot: Option<String>,
    pub system_prompt: Option<String>,
    pub defaults_snapshot: Option<String>,
    pub error_text: String,
    pub error_kind: String,
    pub retry_count: i64,
    pub status: String,
    pub note: Option<String>,
    pub created_at: String,
    pub last_seen_at: String,
    pub resolved_at: Option<String>,
}

/// Input payload for `insert_dead_letter`. Borrowing form so callers can
/// hand over already-computed snapshots without cloning.
#[derive(Debug)]
pub struct DeadLetterInsert<'a> {
    pub slug: &'a str,
    pub chain_id: Option<&'a str>,
    pub step_name: &'a str,
    pub step_primitive: &'a str,
    pub chunk_index: Option<i64>,
    pub input_snapshot: Option<&'a str>,
    pub step_snapshot: Option<&'a str>,
    pub system_prompt: Option<&'a str>,
    pub defaults_snapshot: Option<&'a str>,
    pub error_text: &'a str,
    pub error_kind: &'a str,
    pub retry_count: i64,
}

fn map_dead_letter_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeadLetterEntry> {
    Ok(DeadLetterEntry {
        id: row.get(0)?,
        slug: row.get(1)?,
        chain_id: row.get(2)?,
        step_name: row.get(3)?,
        step_primitive: row.get(4)?,
        chunk_index: row.get(5)?,
        input_snapshot: row.get(6)?,
        step_snapshot: row.get(7)?,
        system_prompt: row.get(8)?,
        defaults_snapshot: row.get(9)?,
        error_text: row.get(10)?,
        error_kind: row.get(11)?,
        retry_count: row.get(12)?,
        status: row.get(13)?,
        note: row.get(14)?,
        created_at: row.get(15)?,
        last_seen_at: row.get(16)?,
        resolved_at: row.get(17)?,
    })
}

const DEAD_LETTER_COLUMNS: &str = "id, slug, chain_id, step_name, step_primitive, \
    chunk_index, input_snapshot, step_snapshot, system_prompt, defaults_snapshot, \
    error_text, error_kind, retry_count, status, note, created_at, last_seen_at, \
    resolved_at";

/// Insert a new dead-letter row. Returns the new row id. Caller holds the
/// per-slug lock (see block comment above).
pub fn insert_dead_letter(conn: &Connection, e: &DeadLetterInsert<'_>) -> Result<i64> {
    conn.execute(
        "INSERT INTO pyramid_dead_letter (
            slug, chain_id, step_name, step_primitive, chunk_index,
            input_snapshot, step_snapshot, system_prompt, defaults_snapshot,
            error_text, error_kind, retry_count
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        rusqlite::params![
            e.slug,
            e.chain_id,
            e.step_name,
            e.step_primitive,
            e.chunk_index,
            e.input_snapshot,
            e.step_snapshot,
            e.system_prompt,
            e.defaults_snapshot,
            e.error_text,
            e.error_kind,
            e.retry_count,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// List dead-letter entries for a slug. `status_filter = None` returns all
/// statuses; otherwise filters to the given status (e.g. "open").
pub fn list_dead_letter(
    conn: &Connection,
    slug: &str,
    status_filter: Option<&str>,
) -> Result<Vec<DeadLetterEntry>> {
    let sql = if status_filter.is_some() {
        format!(
            "SELECT {DEAD_LETTER_COLUMNS} FROM pyramid_dead_letter \
             WHERE slug = ?1 AND status = ?2 ORDER BY id DESC"
        )
    } else {
        format!(
            "SELECT {DEAD_LETTER_COLUMNS} FROM pyramid_dead_letter \
             WHERE slug = ?1 ORDER BY id DESC"
        )
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<DeadLetterEntry> = if let Some(status) = status_filter {
        stmt.query_map(rusqlite::params![slug, status], map_dead_letter_row)?
            .filter_map(|r| r.ok())
            .collect()
    } else {
        stmt.query_map(rusqlite::params![slug], map_dead_letter_row)?
            .filter_map(|r| r.ok())
            .collect()
    };
    Ok(rows)
}

/// Fetch a single dead-letter entry by `(slug, id)`. Slug is part of the key
/// so a malicious/buggy caller can't inspect another slug's entries.
pub fn get_dead_letter(
    conn: &Connection,
    slug: &str,
    id: i64,
) -> Result<Option<DeadLetterEntry>> {
    let sql = format!(
        "SELECT {DEAD_LETTER_COLUMNS} FROM pyramid_dead_letter \
         WHERE slug = ?1 AND id = ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(rusqlite::params![slug, id])?;
    match rows.next()? {
        Some(row) => Ok(Some(map_dead_letter_row(row)?)),
        None => Ok(None),
    }
}

/// Transition a dead-letter entry to a new status. `resolved` and `skipped`
/// both update `resolved_at` to now. Caller is responsible for the state
/// machine guard (no transitions out of terminal states) — this helper
/// performs the write unconditionally.
pub fn update_dead_letter_status(
    conn: &Connection,
    slug: &str,
    id: i64,
    new_status: &str,
    note: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_dead_letter
         SET status = ?3,
             note = COALESCE(?4, note),
             resolved_at = datetime('now'),
             last_seen_at = datetime('now')
         WHERE slug = ?1 AND id = ?2",
        rusqlite::params![slug, id, new_status, note],
    )?;
    Ok(())
}

/// Increment `retry_count` and refresh `last_seen_at` before an operator
/// re-dispatch attempt, so we record the attempt even if the process dies
/// mid-retry.
pub fn bump_dead_letter_retry(conn: &Connection, slug: &str, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_dead_letter
         SET retry_count = retry_count + 1,
             last_seen_at = datetime('now')
         WHERE slug = ?1 AND id = ?2",
        rusqlite::params![slug, id],
    )?;
    Ok(())
}

// ── WS-INGEST-PRIMITIVE: Ingest record CRUD ─────────────────────────────────

/// Column list for SELECT queries on pyramid_ingest_records.
const INGEST_RECORD_COLUMNS: &str =
    "id, slug, source_path, content_type, ingest_signature, file_hash, file_mtime, status, build_id, error_message, created_at, updated_at";

/// Parse a row from `pyramid_ingest_records` into an `IngestRecord`.
fn parse_ingest_record(row: &rusqlite::Row) -> rusqlite::Result<IngestRecord> {
    Ok(IngestRecord {
        id: row.get(0)?,
        slug: row.get(1)?,
        source_path: row.get(2)?,
        content_type: row.get(3)?,
        ingest_signature: row.get(4)?,
        file_hash: row.get(5)?,
        file_mtime: row.get(6)?,
        status: row.get(7)?,
        build_id: row.get(8)?,
        error_message: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

/// Insert or update (upsert) an ingest record. On conflict (slug, source_path,
/// ingest_signature), updates the mutable fields.
pub fn save_ingest_record(conn: &Connection, record: &IngestRecord) -> Result<i64> {
    conn.execute(
        "INSERT INTO pyramid_ingest_records
            (slug, source_path, content_type, ingest_signature, file_hash, file_mtime, status, build_id, error_message)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(slug, source_path, ingest_signature) DO UPDATE SET
            file_hash = excluded.file_hash,
            file_mtime = excluded.file_mtime,
            status = excluded.status,
            build_id = excluded.build_id,
            error_message = excluded.error_message,
            updated_at = datetime('now')",
        rusqlite::params![
            record.slug,
            record.source_path,
            record.content_type,
            record.ingest_signature,
            record.file_hash,
            record.file_mtime,
            record.status,
            record.build_id,
            record.error_message,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Get a specific ingest record by (slug, source_path, ingest_signature).
pub fn get_ingest_record(
    conn: &Connection,
    slug: &str,
    source_path: &str,
    sig: &str,
) -> Result<Option<IngestRecord>> {
    let sql = format!(
        "SELECT {INGEST_RECORD_COLUMNS} FROM pyramid_ingest_records
         WHERE slug = ?1 AND source_path = ?2 AND ingest_signature = ?3"
    );
    let result = conn
        .query_row(&sql, rusqlite::params![slug, source_path, sig], parse_ingest_record)
        .optional()?;
    Ok(result)
}

/// Get all pending ingest records for a slug.
pub fn get_pending_ingests(conn: &Connection, slug: &str) -> Result<Vec<IngestRecord>> {
    let sql = format!(
        "SELECT {INGEST_RECORD_COLUMNS} FROM pyramid_ingest_records
         WHERE slug = ?1 AND status = 'pending'
         ORDER BY created_at ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![slug], parse_ingest_record)?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row?);
    }
    Ok(records)
}

/// Mark an ingest record as 'processing'.
pub fn mark_ingest_processing(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_ingest_records SET status = 'processing', updated_at = datetime('now') WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

/// Mark an ingest record as 'complete' and link it to a build.
pub fn mark_ingest_complete(conn: &Connection, id: i64, build_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_ingest_records SET status = 'complete', build_id = ?2, error_message = NULL, updated_at = datetime('now') WHERE id = ?1",
        rusqlite::params![id, build_id],
    )?;
    Ok(())
}

/// Mark an ingest record as 'failed' with an error message.
pub fn mark_ingest_failed(conn: &Connection, id: i64, error: &str) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_ingest_records SET status = 'failed', error_message = ?2, updated_at = datetime('now') WHERE id = ?1",
        rusqlite::params![id, error],
    )?;
    Ok(())
}

/// Mark all ingest records for a given slug + source_path as 'stale'.
pub fn mark_ingest_stale(conn: &Connection, slug: &str, source_path: &str) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_ingest_records SET status = 'stale', updated_at = datetime('now') WHERE slug = ?1 AND source_path = ?2",
        rusqlite::params![slug, source_path],
    )?;
    Ok(())
}

/// Get all ingest records for a slug (all statuses, all signatures).
pub fn get_ingest_records_for_slug(conn: &Connection, slug: &str) -> Result<Vec<IngestRecord>> {
    let sql = format!(
        "SELECT {INGEST_RECORD_COLUMNS} FROM pyramid_ingest_records
         WHERE slug = ?1
         ORDER BY source_path ASC, created_at ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![slug], parse_ingest_record)?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row?);
    }
    Ok(records)
}

// ── WS-PROVISIONAL (Phase 2b): Provisional session DB helpers ───────────────

/// Create a new provisional session for a slug + source file.
pub fn create_provisional_session(
    conn: &Connection,
    slug: &str,
    source_path: &str,
    session_id: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_provisional_sessions (slug, source_path, session_id, status)
         VALUES (?1, ?2, ?3, 'active')",
        rusqlite::params![slug, source_path, session_id],
    )?;
    Ok(())
}

/// Get all active provisional sessions for a slug.
pub fn get_active_provisional_sessions(
    conn: &Connection,
    slug: &str,
) -> Result<Vec<ProvisionalSession>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, source_path, session_id, status, provisional_node_ids,
                canonical_build_id, file_mtime, last_chunk_processed, created_at, updated_at
         FROM pyramid_provisional_sessions
         WHERE slug = ?1 AND status = 'active'
         ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![slug], parse_provisional_session)?;
    let mut sessions = Vec::new();
    for row in rows {
        sessions.push(row?);
    }
    Ok(sessions)
}

/// Get a specific provisional session by session_id.
pub fn get_provisional_session(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<ProvisionalSession>> {
    let result = conn
        .query_row(
            "SELECT id, slug, source_path, session_id, status, provisional_node_ids,
                    canonical_build_id, file_mtime, last_chunk_processed, created_at, updated_at
             FROM pyramid_provisional_sessions
             WHERE session_id = ?1",
            rusqlite::params![session_id],
            parse_provisional_session,
        )
        .optional()?;
    Ok(result)
}

/// Append a node ID to the provisional session's node list.
pub fn add_provisional_node_to_session(
    conn: &Connection,
    session_id: &str,
    node_id: &str,
) -> Result<()> {
    // Read current list, append, write back
    let current_json: Option<String> = conn
        .query_row(
            "SELECT provisional_node_ids FROM pyramid_provisional_sessions WHERE session_id = ?1",
            rusqlite::params![session_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();

    let mut ids: Vec<String> = current_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    ids.push(node_id.to_string());
    let new_json = serde_json::to_string(&ids)?;

    conn.execute(
        "UPDATE pyramid_provisional_sessions
         SET provisional_node_ids = ?2, updated_at = datetime('now')
         WHERE session_id = ?1",
        rusqlite::params![session_id, new_json],
    )?;
    Ok(())
}

/// Mark a provisional session as 'promoting' (transition from 'active').
pub fn mark_session_promoting(conn: &Connection, session_id: &str) -> Result<()> {
    let updated = conn.execute(
        "UPDATE pyramid_provisional_sessions
         SET status = 'promoting', updated_at = datetime('now')
         WHERE session_id = ?1 AND status = 'active'",
        rusqlite::params![session_id],
    )?;
    if updated == 0 {
        return Err(anyhow::anyhow!(
            "Session '{}' is not in 'active' status or does not exist",
            session_id
        ));
    }
    Ok(())
}

/// Mark a provisional session as 'promoted' and record the canonical build_id.
pub fn mark_session_promoted(
    conn: &Connection,
    session_id: &str,
    canonical_build_id: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_provisional_sessions
         SET status = 'promoted', canonical_build_id = ?2, updated_at = datetime('now')
         WHERE session_id = ?1",
        rusqlite::params![session_id, canonical_build_id],
    )?;
    Ok(())
}

/// Mark a provisional session as 'failed' with an error description.
pub fn mark_session_failed(
    conn: &Connection,
    session_id: &str,
    error_msg: &str,
) -> Result<()> {
    // Store the error in the canonical_build_id field (repurposed for error
    // text when status is 'failed') to avoid adding another column. The
    // canonical_build_id is meaningless for failed sessions.
    conn.execute(
        "UPDATE pyramid_provisional_sessions
         SET status = 'failed', canonical_build_id = ?2, updated_at = datetime('now')
         WHERE session_id = ?1",
        rusqlite::params![session_id, error_msg],
    )?;
    Ok(())
}

/// Get all provisional node IDs for a session.
pub fn get_provisional_nodes_for_session(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<String>> {
    let json: Option<String> = conn
        .query_row(
            "SELECT provisional_node_ids FROM pyramid_provisional_sessions WHERE session_id = ?1",
            rusqlite::params![session_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();

    let ids: Vec<String> = json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    Ok(ids)
}

/// WS-PROVISIONAL: Convenience function that creates a provisional PyramidNode,
/// saves it to the DB, adds it to the session tracking, and optionally emits
/// a `ProvisionalNodeAdded` event.
pub fn save_provisional_node(
    conn: &Connection,
    node: &PyramidNode,
    session_id: &str,
    bus: Option<&crate::pyramid::event_bus::BuildEventBus>,
) -> Result<()> {
    // Sanity: the node must be marked provisional
    if !node.provisional {
        return Err(anyhow::anyhow!(
            "save_provisional_node called with non-provisional node '{}'",
            node.id
        ));
    }

    // Save via the standard path (which will INSERT since it's new, with provisional=1)
    save_node(conn, node, None)?;

    // Track in the session
    add_provisional_node_to_session(conn, session_id, &node.id)?;

    // Emit event
    if let Some(bus) = bus {
        let _ = bus.tx.send(crate::pyramid::event_bus::TaggedBuildEvent {
            slug: node.slug.clone(),
            kind: crate::pyramid::event_bus::TaggedKind::ProvisionalNodeAdded {
                node_id: node.id.clone(),
            },
        });
    }

    Ok(())
}

/// WS-PROVISIONAL: Promote all provisional nodes in a session to canonical.
///
/// 1. Gets all provisional node IDs for the session
/// 2. Marks session as "promoting"
/// 3. For each node: calls `promote_provisional_node`
/// 4. Emits `ProvisionalPromoted` for each promoted node
/// 5. Marks session as "promoted" with the canonical build_id
/// 6. Returns count of promoted nodes
///
/// If the session is already promoted, returns 0 (idempotent).
pub fn promote_session(
    conn: &Connection,
    session_id: &str,
    canonical_build_id: &str,
    bus: Option<&crate::pyramid::event_bus::BuildEventBus>,
) -> Result<usize> {
    // Check session status first for idempotency
    let session = get_provisional_session(conn, session_id)?
        .ok_or_else(|| anyhow::anyhow!("Provisional session '{}' not found", session_id))?;

    if session.status == "promoted" {
        // Already promoted — idempotent success
        return Ok(0);
    }

    let node_ids = get_provisional_nodes_for_session(conn, session_id)?;

    // Transition to promoting (only from active)
    if session.status == "active" {
        mark_session_promoting(conn, session_id)?;
    }

    let slug = &session.slug;
    let mut promoted_count = 0;
    // Track which layers were promoted for SlopeChanged (WS-EVENTS §15.21 trigger #3)
    let mut affected_layers: Vec<i64> = Vec::new();

    for node_id in &node_ids {
        let was_provisional = promote_provisional_node(conn, slug, node_id)?;
        if was_provisional {
            promoted_count += 1;

            // Look up node depth for SlopeChanged trigger discipline
            if let Ok(Some(node)) = get_node(conn, slug, node_id) {
                if node.depth <= 1 && !affected_layers.contains(&node.depth) {
                    affected_layers.push(node.depth);
                }
            }

            // Emit ProvisionalPromoted event
            if let Some(bus) = bus {
                let _ = bus.tx.send(crate::pyramid::event_bus::TaggedBuildEvent {
                    slug: slug.clone(),
                    kind: crate::pyramid::event_bus::TaggedKind::ProvisionalPromoted {
                        provisional_id: node_id.clone(),
                        canonical_id: node_id.clone(),
                    },
                });
            }
        }
    }

    // WS-EVENTS §15.21 trigger #3: emit SlopeChanged when bedrock nodes promoted
    if !affected_layers.is_empty() {
        if let Some(bus) = bus {
            let _ = bus.tx.send(crate::pyramid::event_bus::TaggedBuildEvent {
                slug: slug.clone(),
                kind: crate::pyramid::event_bus::TaggedKind::SlopeChanged {
                    affected_layers,
                },
            });
        }
    }

    // Mark session as promoted
    mark_session_promoted(conn, session_id, canonical_build_id)?;

    Ok(promoted_count)
}

/// Parse a row from `pyramid_provisional_sessions` into a `ProvisionalSession`.
fn parse_provisional_session(row: &rusqlite::Row) -> rusqlite::Result<ProvisionalSession> {
    let node_ids_json: Option<String> = row.get("provisional_node_ids")?;
    let node_ids: Vec<String> = node_ids_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();

    Ok(ProvisionalSession {
        id: row.get("id")?,
        slug: row.get("slug")?,
        source_path: row.get("source_path")?,
        session_id: row.get("session_id")?,
        status: row.get("status")?,
        provisional_node_ids: node_ids,
        canonical_build_id: row.get("canonical_build_id")?,
        file_mtime: row.get("file_mtime")?,
        last_chunk_processed: row.get("last_chunk_processed")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}
