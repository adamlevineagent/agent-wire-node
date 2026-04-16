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
        -- Phase 8 (DADBEAR decommission): drop legacy WAL.
        -- The canonical supervisor reads from dadbear_observation_events.
        DROP TABLE IF EXISTS pyramid_pending_mutations;

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

        -- Phase 8 (DADBEAR decommission): drop legacy auto-update config.
        -- The holds projection is the sole authority for frozen/breaker state.
        -- Contribution existence in pyramid_dadbear_config is the enable gate.
        DROP TABLE IF EXISTS pyramid_auto_update_config;

        -- Phase 7: Drop legacy tables. Any code still referencing these will crash
        -- loudly, which is how we find consumers that need migration.
        DROP TABLE IF EXISTS pyramid_stale_check_log;
        DROP TABLE IF EXISTS pyramid_connection_check_log;
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

    // ── Phase 11: Broadcast reconciliation + synchronous cost persistence ──
    //
    // Per `docs/specs/evidence-triage-and-dadbear.md` Parts 3 & 4, the
    // synchronous cost path (`usage.cost` from the OpenRouter response)
    // populates `actual_cost` + `reconciliation_status = 'synchronous'`
    // immediately after the response is parsed. The Broadcast webhook
    // receiver (Phase 11) then populates `broadcast_confirmed_at` +
    // `broadcast_cost_usd` + `broadcast_discrepancy_ratio` when the trace
    // arrives. The leak detection sweep transitions stale synchronous rows
    // to `reconciliation_status = 'broadcast_missing'` after the grace
    // period. NO auto-correction — discrepancies flip status to
    // `'discrepancy'` and fire loud events; actual_cost is never silently
    // rewritten.
    let _ = conn.execute(
        "ALTER TABLE pyramid_cost_log ADD COLUMN actual_cost REAL",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_cost_log ADD COLUMN actual_tokens_in INTEGER",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_cost_log ADD COLUMN actual_tokens_out INTEGER",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_cost_log ADD COLUMN reconciled_at TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_cost_log ADD COLUMN reconciliation_status TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_cost_log ADD COLUMN provider_id TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_cost_log ADD COLUMN broadcast_confirmed_at TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_cost_log ADD COLUMN broadcast_payload_json TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_cost_log ADD COLUMN broadcast_cost_usd REAL",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_cost_log ADD COLUMN broadcast_discrepancy_ratio REAL",
        [],
    );
    // Indexes for the correlation + leak sweep hot paths.
    let _ = conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_cost_log_reconciliation \
         ON pyramid_cost_log(reconciliation_status, created_at)",
        [],
    );
    let _ = conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_cost_log_broadcast \
         ON pyramid_cost_log(broadcast_confirmed_at)",
        [],
    );

    // Provider health ALTERs are moved BELOW the
    // pyramid_providers CREATE TABLE so they run after the table
    // exists on fresh installs. For existing databases they apply
    // as idempotent column additions.

    // Orphan broadcasts landing zone — broadcast traces that arrive with
    // a metadata shape no local pyramid_cost_log row expects. This is the
    // primary indicator of credential exfiltration: someone else is
    // making calls with the user's OpenRouter API key.
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_orphan_broadcasts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            received_at TEXT NOT NULL DEFAULT (datetime('now')),
            provider_id TEXT,
            generation_id TEXT,
            session_id TEXT,
            pyramid_slug TEXT,
            build_id TEXT,
            step_name TEXT,
            model TEXT,
            cost_usd REAL,
            tokens_in INTEGER,
            tokens_out INTEGER,
            payload_json TEXT NOT NULL,
            acknowledged_at TEXT,
            acknowledgment_reason TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_orphan_broadcasts_generation
            ON pyramid_orphan_broadcasts(generation_id);
        CREATE INDEX IF NOT EXISTS idx_orphan_broadcasts_received
            ON pyramid_orphan_broadcasts(received_at);
        CREATE INDEX IF NOT EXISTS idx_orphan_broadcasts_unreviewed
            ON pyramid_orphan_broadcasts(acknowledged_at);
        ",
    )?;

    // Phase 11 wanderer fix: provider error log for the state
    // machine's threshold-based HTTP 5xx degrade. Each observation of a
    // provider-side error is recorded here so `record_provider_error`
    // can count recent occurrences within the policy window and only
    // flip the provider health flag when the spec's 3-in-window
    // threshold is crossed. Without this table the state machine
    // degrades on the first 5xx, which is strictly more aggressive
    // than the spec permits. Cost discrepancies continue to use
    // `pyramid_cost_log.reconciliation_status = 'discrepancy'` as
    // their counter surface — this table is HTTP-specific.
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_provider_error_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            provider_id TEXT NOT NULL,
            error_kind TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_provider_error_log_recent
            ON pyramid_provider_error_log(provider_id, error_kind, created_at);
        ",
    )?;

    let _ = conn.execute(
        "ALTER TABLE pyramid_faq_nodes ADD COLUMN match_triggers TEXT DEFAULT '[]'",
        [],
    );
    // Phase 8: ALTER TABLEs for pyramid_auto_update_config removed — table dropped.

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

    // ── Phase 2: Change-Manifest Supersession ─────────────────────────────
    //
    // `pyramid_change_manifests` stores the LLM-produced targeted deltas
    // applied during stale-check-driven supersession and user-initiated
    // reroll-with-notes operations. Spec: docs/specs/change-manifest-supersession.md.
    //
    // NOTE: the existing `build_version` column on `pyramid_nodes` (created
    // with the base schema at line ~91) is what this table indexes against.
    // Phase 2 does NOT introduce a new column — it ties into the existing
    // counter that `apply_supersession` already bumps.
    //
    // RETAINED (Phase 7): pyramid_change_manifests is used by the build
    // pipeline (reroll, supersession), not just DADBEAR. It is NOT deprecated
    // — it remains a live table for the build system. dadbear_result_applications
    // is the DADBEAR-specific canonical table for applied results.
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_change_manifests (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
            node_id TEXT NOT NULL,
            build_version INTEGER NOT NULL,
            manifest_json TEXT NOT NULL,
            note TEXT,
            supersedes_manifest_id INTEGER REFERENCES pyramid_change_manifests(id),
            applied_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(slug, node_id, build_version)
        );
        CREATE INDEX IF NOT EXISTS idx_change_manifests_node
            ON pyramid_change_manifests(slug, node_id);
        CREATE INDEX IF NOT EXISTS idx_change_manifests_supersedes
            ON pyramid_change_manifests(supersedes_manifest_id);
        ",
    )?;
    // ── end Phase 2 migration block ──────────────────────────────────────

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

    // ── WS-MULTI-CHAIN-OVERLAY: Chain overlay tracking table ─────────────────
    super::multi_chain_overlay::init_overlay_table(conn)?;

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
            completed_at TEXT,
            cache_hit INTEGER NOT NULL DEFAULT 0
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

    // Phase 18b: idempotent migration adding the `cache_hit` distinction to
    // pyramid_llm_audit. Pre-Phase-18b rows default to `0` (treated as
    // wire-served). Audited cache hits write a row with `cache_hit = 1` so
    // the audit trail / DADBEAR Oversight page / cost reconciliation can
    // distinguish "served from cache" from "served by HTTP call to model X".
    {
        let has_cache_hit: bool = conn
            .prepare(
                "SELECT 1 FROM pragma_table_info('pyramid_llm_audit') WHERE name = 'cache_hit'",
            )?
            .exists([])?;
        if !has_cache_hit {
            conn.execute(
                "ALTER TABLE pyramid_llm_audit ADD COLUMN cache_hit INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
    }

    // ── Phase 6: LLM output cache (pyramid_step_cache) ─────────────────────
    //
    // Content-addressable cache for LLM outputs, keyed on
    // `(slug, cache_key)` where `cache_key = sha256(inputs_hash, prompt_hash,
    // model_id)`. Every successful LLM call writes a row here; every call
    // checks this table BEFORE hitting the wire. The unique constraint means
    // INSERT OR REPLACE on a duplicate is semantically an update.
    //
    // See `docs/specs/llm-output-cache.md` for the full specification and
    // `pyramid::step_context` for the hash helpers + verification gate.
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_step_cache (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL,
            build_id TEXT NOT NULL,
            step_name TEXT NOT NULL,
            chunk_index INTEGER DEFAULT -1,
            depth INTEGER DEFAULT 0,
            cache_key TEXT NOT NULL,
            inputs_hash TEXT NOT NULL,
            prompt_hash TEXT NOT NULL,
            model_id TEXT NOT NULL,
            output_json TEXT NOT NULL,
            token_usage_json TEXT,
            cost_usd REAL,
            latency_ms INTEGER,
            created_at TEXT DEFAULT (datetime('now')),
            force_fresh INTEGER DEFAULT 0,
            supersedes_cache_id INTEGER,
            UNIQUE(slug, cache_key)
        );
        CREATE INDEX IF NOT EXISTS idx_step_cache_lookup
            ON pyramid_step_cache(slug, step_name, chunk_index, depth);
        CREATE INDEX IF NOT EXISTS idx_step_cache_key
            ON pyramid_step_cache(cache_key);
        ",
    )?;

    // ── Phase 13: Reroll + downstream invalidation columns ──────────
    //
    // `note` — user-provided rationale captured during reroll. Empty
    // on non-reroll writes. Populated when the reroll IPC writes a
    // supersession row. The UI shows this as a tooltip on the
    // rerolled step row.
    //
    // `invalidated_by` — set when the downstream cache invalidation
    // walker marks this row stale. Carries the originating cache_key
    // so operators can trace back to the root cause. Cache lookup
    // treats any non-NULL `invalidated_by` as a forced miss.
    let _ = conn.execute(
        "ALTER TABLE pyramid_step_cache ADD COLUMN note TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_step_cache ADD COLUMN invalidated_by TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_step_cache ADD COLUMN invalidated_at TEXT",
        [],
    );
    let _ = conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_step_cache_build_id
            ON pyramid_step_cache(slug, build_id)",
        [],
    );

    // ── Phase 7: Cache warming on pyramid import (pyramid_import_state) ───────
    //
    // Tracks the in-flight state of a `pyramid_import_pyramid` call so a
    // partially completed import (network drop, crash, user-cancel) can be
    // resumed from the cursor on the next attempt without re-validating the
    // already-inserted entries.
    //
    // See `docs/specs/cache-warming-and-import.md` ("Import Resumability"
    // section ~line 297) for the column-by-column rationale.
    conn.execute_batch(
        "
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
        CREATE INDEX IF NOT EXISTS idx_pyramid_import_state_status
            ON pyramid_import_state(status);
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

    // WS-DADBEAR-EXTEND (Phase 2b): DADBEAR source folder watch configuration.
    // Each row is a watched source path for a pyramid slug. DADBEAR's tick loop
    // iterates over enabled configs, scans their source directories, and dispatches
    // ingest records through the build pipeline.
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_dadbear_config (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL,
            source_path TEXT NOT NULL,
            content_type TEXT NOT NULL CHECK(content_type IN ('code', 'conversation', 'document')),
            scan_interval_secs INTEGER NOT NULL DEFAULT 10,
            debounce_secs INTEGER NOT NULL DEFAULT 30,
            session_timeout_secs INTEGER NOT NULL DEFAULT 1800,
            batch_size INTEGER NOT NULL DEFAULT 1,
            enabled INTEGER NOT NULL DEFAULT 1,
            last_scan_at TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(slug, source_path)
        );
        CREATE INDEX IF NOT EXISTS idx_dadbear_config_slug
            ON pyramid_dadbear_config(slug);
        CREATE INDEX IF NOT EXISTS idx_dadbear_config_enabled
            ON pyramid_dadbear_config(slug, enabled);
        ",
    )?;

    // ── WS-DEMAND-GEN (Phase 3): Demand-driven L0 generation job tracking ──────
    // Tracks async demand-gen jobs fired when retrieval encounters questions
    // whose answers don't exist in the pyramid.
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_demand_gen_jobs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            job_id TEXT NOT NULL UNIQUE,
            slug TEXT NOT NULL,
            question TEXT NOT NULL,
            sub_questions TEXT,
            status TEXT NOT NULL DEFAULT 'queued',
            result_node_ids TEXT,
            error_message TEXT,
            requested_at TEXT NOT NULL DEFAULT (datetime('now')),
            started_at TEXT,
            completed_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_demand_gen_slug_status
            ON pyramid_demand_gen_jobs(slug, status);
        CREATE INDEX IF NOT EXISTS idx_demand_gen_job_id
            ON pyramid_demand_gen_jobs(job_id);
        ",
    )?;

    // ── WS-MANIFEST-API (Phase 3): Manifest provenance log ──────────────────────
    // Tracks every manifest execution for audit and metrics. Each entry records
    // the full set of operations and their results, keyed by provenance_id.
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_manifest_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            provenance_id TEXT NOT NULL UNIQUE,
            slug TEXT NOT NULL,
            session_id TEXT,
            operations TEXT NOT NULL,
            results TEXT,
            executed_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_manifest_log_slug
            ON pyramid_manifest_log(slug, executed_at);
        CREATE INDEX IF NOT EXISTS idx_manifest_log_provenance
            ON pyramid_manifest_log(provenance_id);
        ",
    )?;

    // ── WS-VINE-UNIFY (Phase 2b): Vine composition table ──────────────────────
    // Tracks which child pyramids compose each vine pyramid, their ordering,
    // and the current apex reference for each child.
    //
    // Phase 16 (vine-of-vines): added `child_type` column so a single vine
    // composition row can reference either a bedrock or a sub-vine. The
    // `bedrock_slug` column is retained as the child slug for backwards
    // compatibility with existing rows and helpers.
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_vine_compositions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            vine_slug TEXT NOT NULL,
            bedrock_slug TEXT NOT NULL,
            position INTEGER NOT NULL,
            bedrock_apex_node_id TEXT,
            status TEXT NOT NULL DEFAULT 'active',
            child_type TEXT NOT NULL DEFAULT 'bedrock',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(vine_slug, bedrock_slug)
        );
        CREATE INDEX IF NOT EXISTS idx_vine_comp_vine
            ON pyramid_vine_compositions(vine_slug, status);
        CREATE INDEX IF NOT EXISTS idx_vine_comp_bedrock
            ON pyramid_vine_compositions(bedrock_slug);
        ",
    )?;

    // Phase 16: idempotent `child_type` column addition for databases that
    // pre-date the vine-of-vines schema. We check pragma_table_info first so
    // we don't rely on the migration-safe `let _ =` idiom for a NOT NULL
    // column (SQLite's ALTER TABLE ADD COLUMN can accept a NOT NULL default,
    // but skipping the ALTER entirely when the column already exists keeps
    // the migration explicit and auditable).
    {
        let has_child_type: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('pyramid_vine_compositions') WHERE name = 'child_type'")?
            .exists([])?;
        if !has_child_type {
            conn.execute(
                "ALTER TABLE pyramid_vine_compositions
                 ADD COLUMN child_type TEXT NOT NULL DEFAULT 'bedrock'",
                [],
            )?;
        }
    }

    // ── WS-CHAIN-PUBLISH (Phase 3): Chain publication metadata ──────────────────
    // Tracks chain configurations published to the Wire contribution graph.
    // Supports versioning, fork lineage, and Wire publication state.
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_chain_publications (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            chain_id TEXT NOT NULL,
            version INTEGER NOT NULL DEFAULT 1,
            wire_handle_path TEXT,
            wire_uuid TEXT,
            published_at TEXT,
            description TEXT,
            author TEXT,
            forked_from TEXT,
            status TEXT NOT NULL DEFAULT 'local',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(chain_id, version)
        );
        CREATE INDEX IF NOT EXISTS idx_chain_pub_chain_id
            ON pyramid_chain_publications(chain_id);
        CREATE INDEX IF NOT EXISTS idx_chain_pub_status
            ON pyramid_chain_publications(chain_id, status);
        ",
    )?;

    // ── WS-CHAIN-PROPOSAL (Phase 3): Agent-proposed chain updates ─────────────
    // Agents propose updates to chain configurations based on what they learn
    // during sessions. Proposals surface to the operator for review.
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_chain_proposals (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            proposal_id TEXT NOT NULL UNIQUE,
            chain_id TEXT NOT NULL,
            proposer TEXT NOT NULL,
            proposal_type TEXT NOT NULL,
            description TEXT NOT NULL,
            reasoning TEXT NOT NULL,
            patch TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending',
            operator_notes TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            reviewed_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_chain_proposal_chain_id
            ON pyramid_chain_proposals(chain_id);
        CREATE INDEX IF NOT EXISTS idx_chain_proposal_status
            ON pyramid_chain_proposals(status);
        ",
    )?;

    // ── WS-COLLAPSE-EXTEND: collapse tracking table ──────────────────────────
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_collapse_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL,
            node_id TEXT NOT NULL,
            versions_before INTEGER NOT NULL,
            versions_after INTEGER NOT NULL,
            preserved BOOLEAN NOT NULL DEFAULT 1,
            collapsed_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_collapse_log_slug
            ON pyramid_collapse_log(slug);
        CREATE INDEX IF NOT EXISTS idx_collapse_log_slug_node
            ON pyramid_collapse_log(slug, node_id);
        ",
    )?;

    // ── Phase 3: Provider registry, tier routing, per-step overrides ─────────
    //
    // Per `docs/specs/provider-registry.md`. Replaces the hardcoded
    // OpenRouter URL + model cascade in `llm.rs` with a pluggable
    // provider registry. Rows here are populated via the IPC surface
    // in `main.rs` or seeded on first run via `seed_default_provider_registry`.
    //
    // `pyramid_providers.api_key_ref` stores a CREDENTIAL VARIABLE NAME
    // (e.g. "OPENROUTER_KEY") rather than the literal key — the actual
    // secret lives in the `.credentials` file (see credentials-and-secrets.md).
    // `pyramid_tier_routing.pricing_json` follows OpenRouter's shape where
    // individual fields are STRING-encoded numbers.
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_providers (
            id TEXT PRIMARY KEY,
            display_name TEXT NOT NULL,
            provider_type TEXT NOT NULL CHECK(provider_type IN ('openrouter', 'openai_compat')),
            base_url TEXT NOT NULL,
            api_key_ref TEXT,
            auto_detect_context INTEGER NOT NULL DEFAULT 0,
            supports_broadcast INTEGER NOT NULL DEFAULT 0,
            broadcast_config_json TEXT,
            config_json TEXT NOT NULL DEFAULT '{}',
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            -- Phase 11: provider health state machine columns. Carried
            -- in-line here for fresh installs; the ALTER TABLE block
            -- above also adds them to existing databases as a
            -- separate upgrade path.
            provider_health TEXT NOT NULL DEFAULT 'healthy',
            health_reason TEXT,
            health_since TEXT,
            health_acknowledged_at TEXT
        );

        CREATE TABLE IF NOT EXISTS pyramid_tier_routing (
            tier_name TEXT PRIMARY KEY,
            provider_id TEXT NOT NULL REFERENCES pyramid_providers(id) ON DELETE CASCADE,
            model_id TEXT NOT NULL,
            context_limit INTEGER,
            max_completion_tokens INTEGER,
            pricing_json TEXT NOT NULL DEFAULT '{}',
            supported_parameters_json TEXT,
            notes TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_tier_routing_provider
            ON pyramid_tier_routing(provider_id);

        CREATE TABLE IF NOT EXISTS pyramid_step_overrides (
            slug TEXT NOT NULL,
            chain_id TEXT NOT NULL,
            step_name TEXT NOT NULL,
            field_name TEXT NOT NULL,
            value_json TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (slug, chain_id, step_name, field_name)
        );
        ",
    )?;

    // Phase 11: provider health state machine columns. Added as
    // idempotent ALTERs after the CREATE TABLE so existing
    // databases from before Phase 11 pick them up on next boot.
    // Fresh installs get the columns directly from the CREATE TABLE
    // above, so these ALTERs become no-ops (silently ignored via
    // the try-and-ignore pattern).
    //
    // Health is a SIGNAL — it does NOT drive automatic failover or
    // reroute traffic. Providers in a non-healthy state just emit a
    // WARN log on every resolution until an admin acknowledges them.
    let _ = conn.execute(
        "ALTER TABLE pyramid_providers \
         ADD COLUMN provider_health TEXT NOT NULL DEFAULT 'healthy'",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_providers ADD COLUMN health_reason TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_providers ADD COLUMN health_since TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_providers ADD COLUMN health_acknowledged_at TEXT",
        [],
    );

    // First-run seeding. Only fires when pyramid_providers is empty so
    // we don't clobber user-customized rows on subsequent boots.
    seed_default_provider_registry(conn)?;
    ensure_standard_tiers_exist(conn)?;

    // ── Phase 18a: Local Mode state row ───────────────────────────────────────
    //
    // Per `docs/specs/provider-registry.md` §382-395, the Local LLM
    // (Ollama) toggle is a single conceptual switch. The state behind
    // it lives here so toggle-off can restore the prior tier_routing
    // and build_strategy contributions verbatim. The table is
    // single-row (id = 1 PRIMARY KEY); existing installs see the row
    // appear with `enabled = 0` on next boot.
    //
    // Two restore columns because Phase 18a supersedes BOTH the active
    // tier_routing contribution (route every tier through Ollama) AND
    // the active build_strategy contribution (set concurrency to 1).
    // Disable must restore both, otherwise toggling off would leave
    // the user pinned at concurrency 1 against OpenRouter — slow and
    // expensive.
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_local_mode_state (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            enabled INTEGER NOT NULL DEFAULT 0,
            ollama_base_url TEXT,
            ollama_model TEXT,
            detected_context_limit INTEGER,
            restore_from_contribution_id TEXT,
            restore_build_strategy_contribution_id TEXT,
            context_override INTEGER,
            concurrency_override INTEGER,
            restore_dispatch_policy_contribution_id TEXT,
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        ",
    )?;
    // Idempotent: insert the singleton row if missing.
    conn.execute(
        "INSERT OR IGNORE INTO pyramid_local_mode_state (id, enabled) VALUES (1, 0)",
        [],
    )?;
    // Migration-safe column additions for existing databases (Phase 1 daemon control plane).
    let _ = conn.execute(
        "ALTER TABLE pyramid_local_mode_state ADD COLUMN context_override INTEGER",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_local_mode_state ADD COLUMN concurrency_override INTEGER",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pyramid_local_mode_state ADD COLUMN restore_dispatch_policy_contribution_id TEXT",
        [],
    );

    // ── Phase 4: Config Contribution Foundation ───────────────────────────────
    //
    // Per `docs/specs/config-contribution-and-wire-sharing.md`. Every
    // behavioral configuration in Wire Node is a contribution: not a
    // separate table, but a row in `pyramid_config_contributions` with a
    // supersession chain, a triggering note, and Wire shareability. The
    // existing operational tables (`pyramid_dadbear_config`,
    // `pyramid_tier_routing`, `pyramid_step_overrides`, and the four new
    // tables below) remain as runtime caches — fast lookup for the
    // executor's hot path, populated by
    // `config_contributions::sync_config_to_operational()` whenever a
    // contribution is activated.
    //
    // `wire_native_metadata_json` and `wire_publication_state_json` are
    // stored as opaque JSON strings in Phase 4. Phase 5 introduces the
    // canonical `WireNativeMetadata` struct and validates the JSON
    // against it; Phase 4 just initializes them to `"{}"` on every new
    // contribution.
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_config_contributions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            contribution_id TEXT NOT NULL UNIQUE,
            slug TEXT,
            schema_type TEXT NOT NULL,
            yaml_content TEXT NOT NULL,
            wire_native_metadata_json TEXT NOT NULL DEFAULT '{}',
            wire_publication_state_json TEXT NOT NULL DEFAULT '{}',
            supersedes_id TEXT,
            superseded_by_id TEXT,
            triggering_note TEXT,
            status TEXT NOT NULL DEFAULT 'active',
            source TEXT NOT NULL DEFAULT 'local',
            wire_contribution_id TEXT,
            created_by TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            accepted_at TEXT,
            FOREIGN KEY (supersedes_id) REFERENCES pyramid_config_contributions(contribution_id)
        );

        CREATE INDEX IF NOT EXISTS idx_config_contrib_slug_type
            ON pyramid_config_contributions(slug, schema_type);
        CREATE INDEX IF NOT EXISTS idx_config_contrib_active
            ON pyramid_config_contributions(slug, schema_type, status)
            WHERE status = 'active';
        CREATE INDEX IF NOT EXISTS idx_config_contrib_supersedes
            ON pyramid_config_contributions(supersedes_id);
        CREATE INDEX IF NOT EXISTS idx_config_contrib_wire
            ON pyramid_config_contributions(wire_contribution_id);

        -- ── Phase 4 operational tables ─────────────────────────────────────
        -- These are runtime caches populated by sync_config_to_operational().
        -- They are NOT written directly; every row carries a contribution_id
        -- FK back to pyramid_config_contributions.contribution_id so the
        -- executor can always resolve an operational value to the
        -- contribution that produced it.

        CREATE TABLE IF NOT EXISTS pyramid_evidence_policy (
            slug TEXT,
            triage_rules_json TEXT NOT NULL DEFAULT '[]',
            demand_signals_json TEXT NOT NULL DEFAULT '[]',
            budget_json TEXT NOT NULL DEFAULT '{}',
            contribution_id TEXT NOT NULL
                REFERENCES pyramid_config_contributions(contribution_id),
            updated_at TEXT DEFAULT (datetime('now')),
            PRIMARY KEY (slug)
        );

        CREATE TABLE IF NOT EXISTS pyramid_build_strategy (
            slug TEXT,
            initial_build_json TEXT NOT NULL DEFAULT '{}',
            maintenance_json TEXT NOT NULL DEFAULT '{}',
            quality_json TEXT NOT NULL DEFAULT '{}',
            contribution_id TEXT NOT NULL
                REFERENCES pyramid_config_contributions(contribution_id),
            updated_at TEXT DEFAULT (datetime('now')),
            PRIMARY KEY (slug)
        );

        CREATE TABLE IF NOT EXISTS pyramid_custom_prompts (
            slug TEXT,
            extraction_focus TEXT,
            synthesis_style TEXT,
            vocabulary_priority_json TEXT,
            ignore_patterns_json TEXT,
            contribution_id TEXT NOT NULL
                REFERENCES pyramid_config_contributions(contribution_id),
            updated_at TEXT DEFAULT (datetime('now')),
            PRIMARY KEY (slug)
        );

        CREATE TABLE IF NOT EXISTS pyramid_folder_ingestion_heuristics (
            slug TEXT,
            min_files_for_pyramid INTEGER NOT NULL DEFAULT 3,
            max_file_size_bytes INTEGER NOT NULL DEFAULT 10485760,
            max_recursion_depth INTEGER NOT NULL DEFAULT 10,
            content_type_rules_json TEXT NOT NULL DEFAULT '[]',
            ignore_patterns_json TEXT NOT NULL DEFAULT '[]',
            respect_gitignore INTEGER NOT NULL DEFAULT 1,
            respect_pyramid_ignore INTEGER NOT NULL DEFAULT 1,
            vine_collapse_single_child INTEGER NOT NULL DEFAULT 1,
            -- Phase 17: code/document extension lists, DADBEAR default scan
            -- interval, and Claude Code auto-include knobs.
            default_scan_interval_secs INTEGER NOT NULL DEFAULT 30,
            code_extensions_json TEXT NOT NULL DEFAULT '[]',
            document_extensions_json TEXT NOT NULL DEFAULT '[]',
            claude_code_auto_include INTEGER NOT NULL DEFAULT 1,
            claude_code_conversation_path TEXT NOT NULL DEFAULT '~/.claude/projects',
            contribution_id TEXT NOT NULL
                REFERENCES pyramid_config_contributions(contribution_id),
            updated_at TEXT DEFAULT (datetime('now')),
            PRIMARY KEY (slug)
        );

        -- ── Phase 12 evidence triage tables ────────────────────────────────
        --
        -- pyramid_demand_signals: fire-and-forget log of agent queries,
        -- user drills, and search hits that resolve to pyramid nodes.
        -- Read at triage time (sum(weight) over a window) and propagated
        -- upward via evidence KEEP links with attenuation.
        CREATE TABLE IF NOT EXISTS pyramid_demand_signals (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL,
            node_id TEXT NOT NULL,
            signal_type TEXT NOT NULL,
            source TEXT,
            weight REAL NOT NULL DEFAULT 1.0,
            source_node_id TEXT,
            created_at TEXT DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_demand_signals
            ON pyramid_demand_signals(slug, node_id, signal_type, created_at);

        -- pyramid_deferred_questions: evidence questions routed to
        -- the defer branch by triage. The DADBEAR tick scans this
        -- table for expired rows and re-runs triage. Demand-signal
        -- handlers reactivate rows with check_interval IN (never, on_demand).
        CREATE TABLE IF NOT EXISTS pyramid_deferred_questions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL,
            question_id TEXT NOT NULL,
            question_json TEXT NOT NULL,
            deferred_at TEXT NOT NULL DEFAULT (datetime('now')),
            next_check_at TEXT NOT NULL,
            check_interval TEXT NOT NULL,
            triage_reason TEXT,
            contribution_id TEXT,
            UNIQUE(slug, question_id)
        );
        CREATE INDEX IF NOT EXISTS idx_deferred_questions_next
            ON pyramid_deferred_questions(slug, next_check_at);
        CREATE INDEX IF NOT EXISTS idx_deferred_questions_interval
            ON pyramid_deferred_questions(check_interval);

        -- ── Phase 14: Wire discovery update cache ────────────────────────
        --
        -- pyramid_wire_update_cache: caches the results of the periodic
        -- supersession-check run by `WireUpdatePoller`. The UI reads
        -- from this table to render 'Update available' badges on
        -- contributions with newer versions on the Wire, without having
        -- to round-trip to the Wire on every render.
        --
        -- Per `wire-discovery-ranking.md` §Storage (line 297). Entries
        -- are deleted when the user pulls the latest; `acknowledged_at`
        -- is set when the user dismisses a badge (the poller preserves
        -- the row so the badge doesn't re-appear on every sweep, but
        -- clears it when an even-newer version arrives).
        CREATE TABLE IF NOT EXISTS pyramid_wire_update_cache (
            local_contribution_id TEXT PRIMARY KEY
                REFERENCES pyramid_config_contributions(contribution_id),
            latest_wire_contribution_id TEXT NOT NULL,
            chain_length_delta INTEGER NOT NULL,
            changes_summary TEXT,
            author_handles_json TEXT,
            checked_at TEXT NOT NULL DEFAULT (datetime('now')),
            acknowledged_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_wire_update_cache_ack
            ON pyramid_wire_update_cache(acknowledged_at);
        ",
    )?;

    // Phase 17: idempotent ALTER TABLE for folder_ingestion_heuristics to add
    // the new Claude Code / extensions / scan-interval columns for databases
    // that pre-date Phase 17. The CREATE TABLE above already ships the Phase 17
    // shape, but an older DB with the Phase 4 shape needs ALTERs run before
    // any Phase 17 reader touches the table.
    {
        let check_and_add = |col: &str, ddl: &str| -> Result<()> {
            let exists: bool = conn
                .prepare(
                    "SELECT 1 FROM pragma_table_info('pyramid_folder_ingestion_heuristics') WHERE name = ?1",
                )?
                .exists(rusqlite::params![col])?;
            if !exists {
                conn.execute(ddl, [])?;
            }
            Ok(())
        };
        check_and_add(
            "default_scan_interval_secs",
            "ALTER TABLE pyramid_folder_ingestion_heuristics
             ADD COLUMN default_scan_interval_secs INTEGER NOT NULL DEFAULT 30",
        )?;
        check_and_add(
            "code_extensions_json",
            "ALTER TABLE pyramid_folder_ingestion_heuristics
             ADD COLUMN code_extensions_json TEXT NOT NULL DEFAULT '[]'",
        )?;
        check_and_add(
            "document_extensions_json",
            "ALTER TABLE pyramid_folder_ingestion_heuristics
             ADD COLUMN document_extensions_json TEXT NOT NULL DEFAULT '[]'",
        )?;
        check_and_add(
            "claude_code_auto_include",
            "ALTER TABLE pyramid_folder_ingestion_heuristics
             ADD COLUMN claude_code_auto_include INTEGER NOT NULL DEFAULT 1",
        )?;
        check_and_add(
            "claude_code_conversation_path",
            "ALTER TABLE pyramid_folder_ingestion_heuristics
             ADD COLUMN claude_code_conversation_path TEXT NOT NULL DEFAULT '~/.claude/projects'",
        )?;
    }

    // Idempotent column addition — adds `contribution_id` to the existing
    // `pyramid_dadbear_config` rows so DADBEAR gains the same provenance
    // link as the new operational tables above. Phase 4 bootstrap
    // migration populates this column for legacy rows. FK-to-text column
    // intentionally: rusqlite's ALTER TABLE can't add a REFERENCES
    // clause on an existing column, and SQLite treats the check at
    // runtime anyway if `foreign_keys=ON`. We document the intended
    // reference in the comment and rely on the migration path to keep
    // it consistent. The column is nullable because SQLite ALTER TABLE
    // does not support a REFERENCES clause on ADD COLUMN without a
    // NULL default, and legacy rows start as NULL before migration.
    let _ = conn.execute(
        "ALTER TABLE pyramid_dadbear_config ADD COLUMN contribution_id TEXT DEFAULT NULL",
        [],
    );

    // ── Phase 9 migration: needs_migration flag ──────────────────────
    //
    // Phase 9's schema_definition supersession flow flags downstream
    // config rows as "needs migration" so ToolsMode can surface a
    // Migrate button and the user can run an LLM-assisted refinement
    // into the new schema shape. The flag lives on
    // `pyramid_config_contributions` as an integer column, defaulting
    // to 0. Idempotent ALTER — the error is ignored when the column
    // already exists, matching the `pyramid_dadbear_config` precedent
    // above.
    let _ = conn.execute(
        "ALTER TABLE pyramid_config_contributions ADD COLUMN needs_migration INTEGER NOT NULL DEFAULT 0",
        [],
    );

    // Bootstrap migration: convert legacy pyramid_dadbear_config rows
    // to pyramid_config_contributions. Idempotent — the migration
    // checks the `_migration_marker` contribution before running, and
    // individual DADBEAR rows are only migrated if their
    // `contribution_id` column is still NULL.
    migrate_legacy_dadbear_to_contributions(conn)?;

    // Phase 8: ghost-engine backfill and migrate_legacy_auto_update_to_contributions
    // removed — pyramid_auto_update_config table has been dropped. The one-time
    // migration already ran on all existing installations before this decommission.

    // Phase 0 (DADBEAR Canonical Architecture): split dadbear_policy into
    // watch_root + dadbear_norms, and absorb auto_update_policy fields.
    // Must run AFTER the legacy migrations above (they create the source
    // contributions that these migrations split/merge).
    crate::pyramid::config_contributions::migrate_dadbear_policy_to_split(conn)?;
    crate::pyramid::config_contributions::migrate_auto_update_into_norms(conn)?;

    // Dispatch policy operational table (stores active YAML for hot-reload).
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS pyramid_dispatch_policy (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            yaml_content TEXT NOT NULL DEFAULT '',
            contribution_id TEXT,
            updated_at TEXT DEFAULT (datetime('now'))
        )"
    )?;

    // Fleet delivery policy operational table — async fleet dispatch.
    // Singleton (id=1). Mirrors `pyramid_dispatch_policy` exactly; see
    // `pyramid::fleet_delivery_policy` for read/upsert helpers and
    // `docs/plans/async-fleet-dispatch.md` § "Operational Policy" for
    // field semantics.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS pyramid_fleet_delivery_policy (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            yaml_content TEXT NOT NULL DEFAULT '',
            contribution_id TEXT,
            updated_at TEXT DEFAULT (datetime('now'))
        )"
    )?;

    // ── DADBEAR Canonical State Model tables ────────────────────────────────
    //
    // Per `docs/plans/dadbear-canonical-state-model.md`. These are WALs,
    // immutable audit logs, operational state, and read-through caches —
    // NOT user-facing data tables. Law 3 exception: explicitly allowed
    // outside the contribution store.
    conn.execute_batch(
        "
        -- 1. Append-only observation stream
        CREATE TABLE IF NOT EXISTS dadbear_observation_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL,
            source TEXT NOT NULL,
            source_path TEXT,
            event_type TEXT NOT NULL,
            file_path TEXT,
            content_hash TEXT,
            previous_hash TEXT,
            target_node_id TEXT,
            layer INTEGER,
            detected_at TEXT NOT NULL,
            metadata_json TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_obs_slug ON dadbear_observation_events(slug, detected_at);
        CREATE INDEX IF NOT EXISTS idx_obs_cursor ON dadbear_observation_events(slug, id);

        -- 2. Append-only hold stream
        CREATE TABLE IF NOT EXISTS dadbear_hold_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL,
            hold TEXT NOT NULL,
            action TEXT NOT NULL,
            reason TEXT,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_hold_slug ON dadbear_hold_events(slug, created_at);

        -- 3. Materialized active holds (fast-path projection of hold_events)
        CREATE TABLE IF NOT EXISTS dadbear_holds_projection (
            slug TEXT NOT NULL,
            hold TEXT NOT NULL,
            held_since TEXT NOT NULL,
            reason TEXT,
            PRIMARY KEY (slug, hold)
        );

        -- 4. Durable work items (the compiler output)
        -- IDs are semantic paths, not UUIDs. Parse with splitn(5, colon).
        --   id:       slug:epoch_short:primitive:layer:target_id
        --   batch_id: slug:epoch_short:batch-cursor_position
        --   epoch_id: slug:recipe_short:norms_short:timestamp
        -- target_id uses / for composites (edge/L2-003/L2-007)
        -- _short = first 8 hex of contribution UUID. timestamp = uniqueness.
        CREATE TABLE IF NOT EXISTS dadbear_work_items (
            id TEXT PRIMARY KEY,
            slug TEXT NOT NULL,
            batch_id TEXT NOT NULL,
            epoch_id TEXT NOT NULL,
            recipe_contribution_id TEXT,
            step_name TEXT NOT NULL,
            primitive TEXT NOT NULL,
            layer INTEGER NOT NULL,
            target_id TEXT,
            system_prompt TEXT NOT NULL,
            user_prompt TEXT NOT NULL,
            model_tier TEXT NOT NULL,
            resolved_model_id TEXT,
            resolved_provider_id TEXT,
            temperature REAL,
            max_tokens INTEGER,
            response_format_json TEXT,
            build_id TEXT,
            chunk_index INTEGER,
            prompt_hash TEXT,
            force_fresh INTEGER DEFAULT 0,
            observation_event_ids TEXT,
            compiled_at TEXT NOT NULL,
            state TEXT NOT NULL DEFAULT 'compiled',
            state_changed_at TEXT NOT NULL,
            blocked_from TEXT,
            preview_id TEXT,
            result_json TEXT,
            result_cost_usd REAL,
            result_tokens_in INTEGER,
            result_tokens_out INTEGER,
            result_latency_ms INTEGER,
            completed_at TEXT,
            applied_at TEXT,
            application_contribution_id TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_wi_slug_state ON dadbear_work_items(slug, state);
        CREATE INDEX IF NOT EXISTS idx_wi_batch ON dadbear_work_items(batch_id);
        CREATE INDEX IF NOT EXISTS idx_wi_epoch ON dadbear_work_items(slug, epoch_id);

        -- 5. Dependency DAG between work items
        CREATE TABLE IF NOT EXISTS dadbear_work_item_deps (
            work_item_id TEXT NOT NULL REFERENCES dadbear_work_items(id),
            depends_on_id TEXT NOT NULL REFERENCES dadbear_work_items(id),
            PRIMARY KEY (work_item_id, depends_on_id)
        );
        CREATE INDEX IF NOT EXISTS idx_deps_upstream ON dadbear_work_item_deps(depends_on_id);

        -- 6. Batch-level commit contracts
        -- ID is semantic path: {slug}:{batch_id}:{policy_hash_short}
        CREATE TABLE IF NOT EXISTS dadbear_dispatch_previews (
            id TEXT PRIMARY KEY,
            slug TEXT NOT NULL,
            batch_id TEXT NOT NULL,
            policy_hash TEXT NOT NULL,
            norms_hash TEXT NOT NULL,
            item_count INTEGER NOT NULL,
            total_cost_usd REAL NOT NULL,
            total_wall_time_secs REAL,
            enforcement_cost_usd REAL,
            enforcement_level TEXT,
            routing_summary_json TEXT,
            expires_at TEXT NOT NULL,
            committed_at TEXT,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_preview_batch ON dadbear_dispatch_previews(batch_id);

        -- 7. Per-slug epoch-versioned compilation cursor
        -- epoch_id is semantic path: {slug}:{recipe_id_short}:{norms_id_short}:{timestamp}
        CREATE TABLE IF NOT EXISTS dadbear_compilation_state (
            slug TEXT PRIMARY KEY,
            epoch_id TEXT NOT NULL,
            recipe_contribution_id TEXT,
            norms_contribution_id TEXT,
            last_compiled_observation_id INTEGER,
            epoch_start_observation_id INTEGER,
            epoch_started_at TEXT NOT NULL
        );

        -- 8. Per-dispatch attempt log
        -- ID is semantic path: {work_item_id}:a{attempt_number}
        CREATE TABLE IF NOT EXISTS dadbear_work_attempts (
            id TEXT PRIMARY KEY,
            work_item_id TEXT NOT NULL REFERENCES dadbear_work_items(id),
            attempt_number INTEGER NOT NULL,
            dispatched_at TEXT NOT NULL,
            model_id TEXT NOT NULL,
            routing TEXT NOT NULL,
            result_json TEXT,
            cost_usd REAL,
            tokens_in INTEGER,
            tokens_out INTEGER,
            latency_ms INTEGER,
            status TEXT NOT NULL DEFAULT 'pending',
            review_status TEXT NOT NULL DEFAULT 'none',
            cost_log_id TEXT,
            completed_at TEXT,
            error TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_attempts_wi ON dadbear_work_attempts(work_item_id);

        -- 9. Idempotent result application log
        CREATE TABLE IF NOT EXISTS dadbear_result_applications (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            work_item_id TEXT NOT NULL REFERENCES dadbear_work_items(id),
            slug TEXT NOT NULL,
            target_id TEXT NOT NULL,
            action TEXT NOT NULL,
            old_contribution_id TEXT,
            new_contribution_id TEXT,
            applied_at TEXT NOT NULL,
            UNIQUE(work_item_id, target_id)
        );

        -- 10. Per-slug build-derived facts
        CREATE TABLE IF NOT EXISTS pyramid_build_metadata (
            slug TEXT PRIMARY KEY,
            ingested_extensions TEXT DEFAULT '[]',
            ingested_config_files TEXT DEFAULT '[]',
            updated_at TEXT
        );
        ",
    )?;

    // Phase 8: Hold migration, build_metadata population, and observation event
    // backfill from pyramid_auto_update_config / pyramid_pending_mutations removed.
    // Both source tables have been dropped. These one-time migrations already ran
    // on all existing installations before this decommission.

    // ── end DADBEAR Canonical State Model tables ────────────────────────────

    // ── Compute Chronicle: persistent compute observability ────────────────
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_compute_events (
            id             INTEGER PRIMARY KEY AUTOINCREMENT,
            job_path       TEXT NOT NULL,
            event_type     TEXT NOT NULL,
            timestamp      TEXT NOT NULL,
            model_id       TEXT,
            source         TEXT NOT NULL,
            slug           TEXT,
            build_id       TEXT,
            chain_name     TEXT,
            content_type   TEXT,
            step_name      TEXT,
            primitive      TEXT,
            depth          INTEGER,
            task_label     TEXT,
            metadata       TEXT,
            work_item_id   TEXT,
            attempt_id     TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_compute_events_pyramid
            ON pyramid_compute_events(slug, build_id, timestamp);

        CREATE INDEX IF NOT EXISTS idx_compute_events_source_type
            ON pyramid_compute_events(source, event_type, timestamp);

        CREATE INDEX IF NOT EXISTS idx_compute_events_model
            ON pyramid_compute_events(model_id, timestamp);

        CREATE INDEX IF NOT EXISTS idx_compute_events_layer
            ON pyramid_compute_events(chain_name, depth, timestamp);

        CREATE INDEX IF NOT EXISTS idx_compute_events_job_path
            ON pyramid_compute_events(job_path);

        CREATE INDEX IF NOT EXISTS idx_compute_events_work_item
            ON pyramid_compute_events(work_item_id)
            WHERE work_item_id IS NOT NULL;
        ",
    )?;

    // ── Chronicle views (lazy, computed on query) ──────────────────────────
    conn.execute_batch(
        "
        CREATE VIEW IF NOT EXISTS v_compute_hourly_by_model AS
        SELECT
            strftime('%Y-%m-%d %H:00:00', timestamp) AS hour,
            model_id,
            source,
            COUNT(CASE WHEN event_type = 'completed' THEN 1 END) AS completed,
            COUNT(CASE WHEN event_type = 'failed' THEN 1 END) AS failed,
            AVG(CASE WHEN event_type = 'completed' THEN CAST(json_extract(metadata, '$.latency_ms') AS REAL) END) AS avg_latency_ms,
            SUM(CASE WHEN event_type = 'completed' THEN CAST(json_extract(metadata, '$.tokens_prompt') AS INTEGER) ELSE 0 END) AS total_tokens_in,
            SUM(CASE WHEN event_type = 'completed' THEN CAST(json_extract(metadata, '$.tokens_completion') AS INTEGER) ELSE 0 END) AS total_tokens_out,
            SUM(CASE WHEN event_type IN ('completed', 'cloud_returned') THEN CAST(json_extract(metadata, '$.cost_usd') AS REAL) ELSE 0.0 END) AS total_cost_usd
        FROM pyramid_compute_events
        GROUP BY hour, model_id, source;

        CREATE VIEW IF NOT EXISTS v_compute_by_build AS
        SELECT
            slug,
            build_id,
            chain_name,
            content_type,
            COUNT(CASE WHEN event_type = 'completed' THEN 1 END) AS total_calls,
            SUM(CASE WHEN event_type = 'completed' THEN CAST(json_extract(metadata, '$.latency_ms') AS INTEGER) ELSE 0 END) AS total_gpu_ms,
            AVG(CASE WHEN event_type = 'started' THEN CAST(json_extract(metadata, '$.queue_wait_ms') AS REAL) END) AS avg_queue_wait_ms,
            COUNT(CASE WHEN source = 'fleet' THEN 1 END) AS fleet_steps,
            COUNT(CASE WHEN source = 'local' THEN 1 END) AS local_steps,
            COUNT(CASE WHEN source = 'cloud' THEN 1 END) AS cloud_steps,
            GROUP_CONCAT(DISTINCT model_id) AS models_used,
            SUM(CASE WHEN event_type IN ('completed', 'cloud_returned') THEN CAST(json_extract(metadata, '$.cost_usd') AS REAL) ELSE 0.0 END) AS total_cost_usd,
            MIN(timestamp) AS started_at,
            MAX(timestamp) AS finished_at
        FROM pyramid_compute_events
        WHERE slug IS NOT NULL AND build_id IS NOT NULL
        GROUP BY slug, build_id, chain_name, content_type;

        CREATE VIEW IF NOT EXISTS v_compute_by_depth AS
        SELECT
            slug,
            build_id,
            depth,
            primitive,
            COUNT(CASE WHEN event_type = 'completed' THEN 1 END) AS step_count,
            SUM(CASE WHEN event_type = 'completed' THEN CAST(json_extract(metadata, '$.latency_ms') AS INTEGER) ELSE 0 END) AS total_gpu_ms,
            AVG(CASE WHEN event_type = 'completed' THEN CAST(json_extract(metadata, '$.latency_ms') AS REAL) END) AS avg_latency_ms,
            SUM(CASE WHEN event_type = 'completed' THEN CAST(json_extract(metadata, '$.tokens_completion') AS INTEGER) ELSE 0 END) AS total_tokens_out,
            GROUP_CONCAT(DISTINCT source) AS sources_used
        FROM pyramid_compute_events
        WHERE slug IS NOT NULL AND depth IS NOT NULL
        GROUP BY slug, build_id, depth, primitive;

        DROP VIEW IF EXISTS v_compute_fleet_peers;
        CREATE VIEW IF NOT EXISTS v_compute_fleet_peers AS
        SELECT
            json_extract(metadata, '$.peer_id') AS peer_id,
            COUNT(CASE WHEN event_type = 'fleet_dispatched_async' THEN 1 END) AS dispatch_count,
            COUNT(CASE WHEN event_type = 'fleet_result_received' THEN 1 END) AS success_count,
            COUNT(CASE WHEN event_type IN ('fleet_dispatch_failed', 'fleet_dispatch_timeout', 'fleet_peer_overloaded') THEN 1 END) AS failed_count,
            ROUND(
                CAST(COUNT(CASE WHEN event_type = 'fleet_result_received' THEN 1 END) AS REAL) /
                NULLIF(COUNT(CASE WHEN event_type = 'fleet_dispatched_async' THEN 1 END), 0) * 100,
                1
            ) AS success_rate_pct,
            AVG(CASE WHEN event_type = 'fleet_result_received' THEN CAST(json_extract(metadata, '$.latency_ms') AS REAL) END) AS avg_round_trip_ms,
            GROUP_CONCAT(DISTINCT model_id) AS models_served
        FROM pyramid_compute_events
        WHERE source = 'fleet'
        GROUP BY peer_id;

        DROP VIEW IF EXISTS v_compute_by_source;
        CREATE VIEW IF NOT EXISTS v_compute_by_source AS
        SELECT
            source,
            COUNT(CASE WHEN event_type IN ('completed', 'fleet_result_received', 'cloud_returned') THEN 1 END) AS total_completed,
            SUM(CASE WHEN event_type IN ('completed', 'cloud_returned') THEN CAST(json_extract(metadata, '$.cost_usd') AS REAL) ELSE 0.0 END) AS total_cost_usd,
            AVG(CASE WHEN event_type IN ('completed', 'fleet_result_received', 'cloud_returned') THEN CAST(json_extract(metadata, '$.latency_ms') AS REAL) END) AS avg_latency_ms,
            SUM(CASE WHEN event_type IN ('completed', 'fleet_result_received', 'cloud_returned') THEN CAST(json_extract(metadata, '$.tokens_prompt') AS INTEGER) ELSE 0 END) AS total_tokens_in,
            SUM(CASE WHEN event_type IN ('completed', 'fleet_result_received', 'cloud_returned') THEN CAST(json_extract(metadata, '$.tokens_completion') AS INTEGER) ELSE 0 END) AS total_tokens_out
        FROM pyramid_compute_events
        GROUP BY source;
        ",
    )?;

    // ── Fleet async dispatch: result outbox ────────────────────────────────
    //
    // Peer-side durable outbox for async fleet dispatch AND compute/storage
    // market dispatch (per architecture §VIII.6 DD-D / DD-Q). Compound PK
    // prevents cross-dispatcher hijacking; unique index on job_id alone
    // detects any cross-dispatcher UUID reuse. `expires_at` drives all
    // sweep-time state transitions (pending/ready/delivered/failed).
    // `worker_heartbeat_at` is distinct from `expires_at` only for
    // observability.
    //
    // `callback_kind` discriminates Fleet (roster-validated dispatcher) vs
    // MarketStandard / Relay (JWT-gated, any-HTTPS). Sweep helpers read this
    // column to reconstruct CallbackKind for revalidation.
    //
    // For market dispatches, dispatcher_node_id = `fleet::WIRE_PLATFORM_DISPATCHER`
    // (the sentinel "wire-platform"). The Wire is not a peer; the sentinel
    // lets the compound PK work uniformly across markets.
    //
    // See docs/plans/async-fleet-dispatch.md "Outbox Schema" for the full
    // state-machine spec, and compute-market-architecture.md §VIII.6
    // DD-D/DD-Q for the market reuse decision.
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS fleet_result_outbox (
            dispatcher_node_id TEXT NOT NULL,
            job_id TEXT NOT NULL,
            callback_url TEXT NOT NULL,
            callback_kind TEXT NOT NULL DEFAULT 'Fleet',
            status TEXT NOT NULL,
            result_json TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            ready_at TEXT,
            delivered_at TEXT,
            expires_at TEXT NOT NULL,
            worker_heartbeat_at TEXT,
            delivery_attempts INTEGER NOT NULL DEFAULT 0,
            last_attempt_at TEXT,
            last_error TEXT,
            PRIMARY KEY (dispatcher_node_id, job_id)
        );
        CREATE INDEX IF NOT EXISTS idx_fleet_outbox_expires ON fleet_result_outbox (expires_at);
        CREATE INDEX IF NOT EXISTS idx_fleet_outbox_status_attempts ON fleet_result_outbox (status, last_attempt_at);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_fleet_outbox_job_id ON fleet_result_outbox (job_id);
        CREATE INDEX IF NOT EXISTS idx_fleet_outbox_callback_kind ON fleet_result_outbox (callback_kind);
        ",
    )?;

    // ── Phase 2 Workstream 0 (DD-Q): add callback_kind to existing DBs ─────
    //
    // The CREATE TABLE above handles fresh DBs (callback_kind DEFAULT 'Fleet'
    // is in-line). For DBs that predate Phase 2, the column doesn't exist;
    // we ALTER once. SQLite has no `ADD COLUMN IF NOT EXISTS`, so we check
    // via PRAGMA table_info first.
    {
        let mut stmt = conn.prepare("PRAGMA table_info(fleet_result_outbox)")?;
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();
        if !cols.iter().any(|c| c == "callback_kind") {
            conn.execute_batch(
                "ALTER TABLE fleet_result_outbox ADD COLUMN callback_kind TEXT NOT NULL DEFAULT 'Fleet';
                 CREATE INDEX IF NOT EXISTS idx_fleet_outbox_callback_kind ON fleet_result_outbox (callback_kind);",
            )?;
        }
    }

    // ── Market delivery policy singleton ───────────────────────────────────
    //
    // Per architecture §VIII.6 DD-E + DD-Q: shape-parallel to
    // pyramid_fleet_delivery_policy. Holds the current YAML of the
    // market_delivery_policy contribution. Loaded at boot; hot-reloaded on
    // ConfigSynced via config_contributions::sync_config_to_operational_with_registry.
    //
    // Columns match pyramid_fleet_delivery_policy exactly (yaml_content +
    // contribution_id + updated_at) so the DB helpers in
    // pyramid/market_delivery_policy.rs mirror the fleet helpers shape-for-shape.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS pyramid_market_delivery_policy (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            yaml_content TEXT NOT NULL DEFAULT '',
            contribution_id TEXT,
            updated_at TEXT DEFAULT (datetime('now'))
        )",
    )?;

    Ok(())
}

// ── Fleet Result Outbox: CAS helpers ────────────────────────────────────────
//
// Peer-side durable result storage for async fleet dispatch. All UPDATEs are
// compare-and-swap on `status`; callers MUST check the returned rowcount to
// know whether the CAS won or lost (rowcount=1 => won, rowcount=0 => lost).
//
// These helpers are synchronous rusqlite calls, designed to be invoked from
// `tokio::task::spawn_blocking` contexts in the delivery sweep and worker
// paths. See docs/plans/async-fleet-dispatch.md for the full state machine.

/// Status constants — canonical spellings persisted in the `status` column.
pub const FLEET_STATUS_PENDING: &str = "pending";
pub const FLEET_STATUS_READY: &str = "ready";
pub const FLEET_STATUS_DELIVERED: &str = "delivered";
pub const FLEET_STATUS_FAILED: &str = "failed";

/// Row shape returned by sweep helpers (`fleet_outbox_sweep_expired`,
/// `fleet_outbox_retry_candidates`, etc.). Carries enough fields for both
/// Predicate A's state-transition branching and Predicate B's retry filtering.
#[derive(Debug, Clone)]
pub struct OutboxRow {
    pub dispatcher_node_id: String,
    pub job_id: String,
    pub status: String,
    pub callback_url: String,
    pub result_json: Option<String>,
    pub delivery_attempts: i64,
    pub last_attempt_at: Option<String>,
    pub expires_at: String,
}

/// Insert a new outbox row in `pending` state. Uses INSERT OR IGNORE so repeat
/// calls with the same PK are a no-op. Returns `conn.changes()` rowcount —
/// `1` means the row was freshly inserted, `0` means a row with that PK
/// already existed. Callers use this to disambiguate fresh insert from
/// legitimate retry (see `handle_fleet_dispatch` step 6).
pub fn fleet_outbox_insert_or_ignore(
    conn: &Connection,
    dispatcher_node_id: &str,
    job_id: &str,
    callback_url: &str,
    expires_at: &str,
) -> Result<usize> {
    let n = conn.execute(
        "INSERT OR IGNORE INTO fleet_result_outbox
            (dispatcher_node_id, job_id, callback_url, status, expires_at)
         VALUES (?1, ?2, ?3, 'pending', ?4)",
        rusqlite::params![dispatcher_node_id, job_id, callback_url, expires_at],
    )?;
    Ok(n)
}

/// Row shape returned by [`fleet_outbox_lookup`]. Carries the fields the
/// `handle_fleet_dispatch` step-6 branch table needs:
///
/// - `dispatcher_node_id` — stored dispatcher identity for 409 collision checks.
/// - `status` — terminal-state branching (`pending`/`ready`/`delivered`/`failed`).
/// - `delivery_attempts` — retry counter exposed to observability and metrics.
/// - `last_error` — populated into the 410 Gone body when a row is already
///   `failed` so the dispatcher sees the terminal failure reason.
#[derive(Debug, Clone)]
pub struct OutboxLookup {
    pub dispatcher_node_id: String,
    pub status: String,
    pub delivery_attempts: u32,
    pub last_error: Option<String>,
}

/// Look up an outbox row by `job_id` alone (NOT by compound PK). Keys on
/// `job_id` so cross-dispatcher UUID collisions are detectable — the caller
/// compares the returned `dispatcher_node_id` against its own identity and
/// rejects with 409 Conflict on mismatch.
///
/// Returns an [`OutboxLookup`] or `None`. The `last_error` field is populated
/// when the row's `last_error` column is non-NULL so the Phase 3 handler can
/// include it in the 410 Gone body for already-failed rows.
pub fn fleet_outbox_lookup(
    conn: &Connection,
    job_id: &str,
) -> Result<Option<OutboxLookup>> {
    let row = conn
        .query_row(
            "SELECT dispatcher_node_id, status, delivery_attempts, last_error
             FROM fleet_result_outbox
             WHERE job_id = ?1",
            rusqlite::params![job_id],
            |r| {
                let did: String = r.get(0)?;
                let status: String = r.get(1)?;
                let attempts: i64 = r.get(2)?;
                let last_error: Option<String> = r.get(3)?;
                Ok(OutboxLookup {
                    dispatcher_node_id: did,
                    status,
                    delivery_attempts: attempts as u32,
                    last_error,
                })
            },
        )
        .optional()?;
    Ok(row)
}

/// Admission counter: number of in-flight (`pending` or `ready`) rows in the
/// outbox, EXCLUDING the caller's just-inserted row identified by
/// `(dispatcher_node_id, job_id)`. Used to honor `max_inflight_jobs` as the
/// actual limit rather than `max − 1` — the freshly inserted row is already
/// counted by SELECT, so we subtract it here.
pub fn fleet_outbox_count_inflight_excluding(
    conn: &Connection,
    dispatcher_node_id: &str,
    job_id: &str,
) -> Result<u64> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM fleet_result_outbox
         WHERE status IN ('pending','ready')
           AND NOT (dispatcher_node_id = ?1 AND job_id = ?2)",
        rusqlite::params![dispatcher_node_id, job_id],
        |r| r.get(0),
    )?;
    Ok(n as u64)
}

/// Unconditional delete by compound PK. Returns rowcount (0 if PK missing —
/// not an error; the row is simply gone). Used by admission rollback
/// (INSERT then DELETE on rejection) and by Predicate A's `delivered`/`failed`
/// transitions.
pub fn fleet_outbox_delete(
    conn: &Connection,
    dispatcher_node_id: &str,
    job_id: &str,
) -> Result<usize> {
    let n = conn.execute(
        "DELETE FROM fleet_result_outbox
         WHERE dispatcher_node_id = ?1 AND job_id = ?2",
        rusqlite::params![dispatcher_node_id, job_id],
    )?;
    Ok(n)
}

/// Alias for `fleet_outbox_delete` — kept under the spec-named symbol for
/// call sites written against Predicate A's DELETE transitions.
pub fn fleet_outbox_delete_row(
    conn: &Connection,
    dispatcher_node_id: &str,
    job_id: &str,
) -> Result<usize> {
    fleet_outbox_delete(conn, dispatcher_node_id, job_id)
}

/// Worker heartbeat tick: CAS update `expires_at` (and observability
/// `worker_heartbeat_at`) only if the row is still `pending`. Returns
/// rowcount — `1` means the heartbeat bumped the row, `0` means the sweep
/// already promoted us to `ready` (or the row was deleted) and the worker
/// should exit the heartbeat loop.
pub fn fleet_outbox_update_heartbeat_if_pending(
    conn: &Connection,
    dispatcher_node_id: &str,
    job_id: &str,
    new_expires_at: &str,
) -> Result<usize> {
    let n = conn.execute(
        "UPDATE fleet_result_outbox
            SET expires_at = ?3,
                worker_heartbeat_at = datetime('now')
          WHERE dispatcher_node_id = ?1 AND job_id = ?2 AND status = 'pending'",
        rusqlite::params![dispatcher_node_id, job_id, new_expires_at],
    )?;
    Ok(n)
}

/// Worker completion CAS: promote `pending → ready` and stamp the serialized
/// result. Writes `ready_at = now` and `expires_at = now + ready_retention_secs`.
/// Returns rowcount — `1` means the worker won the race; `0` means the sweep
/// already promoted the row to `ready` with a synthesized Error payload and
/// the worker's result should be dropped.
pub fn fleet_outbox_promote_ready_if_pending(
    conn: &Connection,
    dispatcher_node_id: &str,
    job_id: &str,
    result_json: &str,
    ready_retention_secs: u64,
) -> Result<usize> {
    // `'+' || ? || ' seconds'` composes a SQLite datetime modifier at bind time.
    let modifier = format!("+{} seconds", ready_retention_secs);
    let n = conn.execute(
        "UPDATE fleet_result_outbox
            SET status = 'ready',
                result_json = ?3,
                ready_at = datetime('now'),
                expires_at = datetime('now', ?4)
          WHERE dispatcher_node_id = ?1 AND job_id = ?2 AND status = 'pending'",
        rusqlite::params![dispatcher_node_id, job_id, result_json, modifier],
    )?;
    Ok(n)
}

/// Delivery success CAS: promote `ready → delivered`. Stamps `delivered_at`
/// and resets `expires_at` to `now + delivered_retention_secs`. Returns
/// rowcount — `0` means Predicate A concurrently promoted the row
/// `ready → failed` (retries exhausted). The callback already succeeded
/// (dispatcher received 2xx); discard the CAS failure.
pub fn fleet_outbox_mark_delivered_if_ready(
    conn: &Connection,
    dispatcher_node_id: &str,
    job_id: &str,
    delivered_retention_secs: u64,
) -> Result<usize> {
    let modifier = format!("+{} seconds", delivered_retention_secs);
    let n = conn.execute(
        "UPDATE fleet_result_outbox
            SET status = 'delivered',
                delivered_at = datetime('now'),
                expires_at = datetime('now', ?3)
          WHERE dispatcher_node_id = ?1 AND job_id = ?2 AND status = 'ready'",
        rusqlite::params![dispatcher_node_id, job_id, modifier],
    )?;
    Ok(n)
}

/// Terminal failure CAS: promote `ready → failed`. Stamps `expires_at =
/// now + failed_retention_secs` so the row is kept for post-mortem visibility
/// before final cleanup. Returns rowcount — `0` means the row was already
/// delivered, deleted, or in another state.
pub fn fleet_outbox_mark_failed_if_ready(
    conn: &Connection,
    dispatcher_node_id: &str,
    job_id: &str,
    failed_retention_secs: u64,
) -> Result<usize> {
    let modifier = format!("+{} seconds", failed_retention_secs);
    let n = conn.execute(
        "UPDATE fleet_result_outbox
            SET status = 'failed',
                expires_at = datetime('now', ?3)
          WHERE dispatcher_node_id = ?1 AND job_id = ?2 AND status = 'ready'",
        rusqlite::params![dispatcher_node_id, job_id, modifier],
    )?;
    Ok(n)
}

/// Record a failed delivery attempt: bump `delivery_attempts`, stamp
/// `last_attempt_at`, and capture `last_error`. Gated on `status = 'ready'`
/// so a concurrent `ready → delivered` or `ready → failed` doesn't get
/// the attempt counter polluted.
pub fn fleet_outbox_bump_delivery_attempt(
    conn: &Connection,
    dispatcher_node_id: &str,
    job_id: &str,
    last_error: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE fleet_result_outbox
            SET last_attempt_at = datetime('now'),
                last_error = ?3,
                delivery_attempts = delivery_attempts + 1
          WHERE dispatcher_node_id = ?1 AND job_id = ?2 AND status = 'ready'",
        rusqlite::params![dispatcher_node_id, job_id, last_error],
    )?;
    Ok(())
}

/// Two-step `ready → failed` promotion used by Predicate B for the
/// retries-exhausted path. Rather than writing `failed` directly (which
/// would duplicate the terminal-state-write machinery), this helper bumps
/// `expires_at` to one second in the past; Predicate A on its next tick
/// picks up the row and transitions it via the standard CAS path.
/// Returns number of rows pushed into the past.
pub fn fleet_outbox_expire_exhausted(conn: &Connection, max_attempts: u64) -> Result<usize> {
    // SQLite integers are signed 64-bit; `FleetDeliveryPolicy.max_delivery_attempts`
    // is `u64`. Cap at `i64::MAX` before the cast — a policy value above that
    // would saturate rather than wrap. In practice `max_delivery_attempts` is
    // a small retry budget (default 20), so saturation here is a belt-and-
    // suspenders guard, not a real operating condition.
    let max_as_i64: i64 = max_attempts.min(i64::MAX as u64) as i64;
    let n = conn.execute(
        "UPDATE fleet_result_outbox
            SET expires_at = datetime('now', '-1 second')
          WHERE status = 'ready' AND delivery_attempts >= ?1",
        rusqlite::params![max_as_i64],
    )?;
    Ok(n)
}

/// Predicate A candidate select: all rows whose `expires_at` is at or before
/// `now`. Caller branches on `status` for each returned row — `pending` →
/// synthesize Error and promote to `ready`, `ready` → terminal `failed`,
/// `delivered`/`failed` → DELETE.
pub fn fleet_outbox_sweep_expired(conn: &Connection) -> Result<Vec<OutboxRow>> {
    // Filter to `callback_kind = 'Fleet'`: market/relay rows are owned by
    // compute market Phase 2+ delivery paths (not this sweeper).
    // See architecture §VIII.6 DD-D / DD-Q for the outbox reuse decision.
    let mut stmt = conn.prepare(
        "SELECT dispatcher_node_id, job_id, status, callback_url, result_json,
                delivery_attempts, last_attempt_at, expires_at
           FROM fleet_result_outbox
          WHERE expires_at <= datetime('now')
            AND callback_kind = 'Fleet'",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(OutboxRow {
                dispatcher_node_id: r.get(0)?,
                job_id: r.get(1)?,
                status: r.get(2)?,
                callback_url: r.get(3)?,
                result_json: r.get(4)?,
                delivery_attempts: r.get(5)?,
                last_attempt_at: r.get(6)?,
                expires_at: r.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Predicate B candidate select: all `ready` rows. The caller computes the
/// per-row backoff in Rust against `delivery_attempts` and
/// `last_attempt_at` (exponential backoff formula lives in the sweep loop,
/// not in SQL).
pub fn fleet_outbox_retry_candidates(conn: &Connection) -> Result<Vec<OutboxRow>> {
    // Filter to `callback_kind = 'Fleet'`: market/relay rows are owned by
    // compute market Phase 2+ delivery paths (not this sweeper).
    let mut stmt = conn.prepare(
        "SELECT dispatcher_node_id, job_id, status, callback_url, result_json,
                delivery_attempts, last_attempt_at, expires_at
           FROM fleet_result_outbox
          WHERE status = 'ready'
            AND callback_kind = 'Fleet'",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(OutboxRow {
                dispatcher_node_id: r.get(0)?,
                job_id: r.get(1)?,
                status: r.get(2)?,
                callback_url: r.get(3)?,
                result_json: r.get(4)?,
                delivery_attempts: r.get(5)?,
                last_attempt_at: r.get(6)?,
                expires_at: r.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Synthesizes a `FleetAsyncResult::Error(msg)` payload as JSON, matching the
/// `#[serde(tag = "kind", content = "data")]` contract on the Phase 2
/// `FleetAsyncResult` enum. Single source of truth so startup recovery and
/// the Phase 3 heartbeat-lost sweep stay byte-aligned with the enum's
/// serialization shape. The message itself is JSON-encoded via `serde_json`
/// so quotes, backslashes, and other control characters are escaped
/// correctly.
pub fn synthesize_worker_error_json(msg: &str) -> String {
    // `serde_json::to_string` on a `&str` yields the properly-escaped,
    // quoted JSON string literal — we splice that directly into the object
    // body to avoid paying for a full `json!` macro round-trip.
    let escaped = serde_json::to_string(msg).expect("string is always JSON-serializable");
    format!(r#"{{"kind":"Error","data":{}}}"#, escaped)
}

/// Startup recovery: convert every `pending` row to `ready` with a synthesized
/// `FleetAsyncResult::Error` payload so the dispatcher hears about the
/// worker crash via the normal callback path. `ready` rows are left alone.
/// Returns number of rows recovered.
pub fn fleet_outbox_startup_recovery(
    conn: &Connection,
    ready_retention_secs: u64,
) -> Result<usize> {
    let modifier = format!("+{} seconds", ready_retention_secs);
    let synth_payload =
        synthesize_worker_error_json("worker crashed before completion (node restarted)");
    // Fleet rows only: market/relay rows are recovered by their own path
    // (compute market Phase 2+). See architecture §VIII.6 DD-D / DD-Q.
    let n = conn.execute(
        "UPDATE fleet_result_outbox
            SET status = 'ready',
                result_json = ?2,
                ready_at = datetime('now'),
                expires_at = datetime('now', ?1),
                last_error = 'startup recovery'
          WHERE status = 'pending'
            AND callback_kind = 'Fleet'",
        rusqlite::params![modifier, synth_payload],
    )?;
    Ok(n)
}

/// Phase 4 bootstrap: convert every legacy `pyramid_dadbear_config` row
/// to a `pyramid_config_contributions` row with `schema_type =
/// 'dadbear_policy'`, `source = 'migration'`, `status = 'active'`, and
/// `triggering_note = 'Migrated from legacy pyramid_dadbear_config'`.
///
/// Idempotent via two guards:
///
/// 1. A sentinel row with `schema_type = '_migration_marker'` is
///    inserted on first run. Subsequent runs short-circuit on its
///    presence.
/// 2. Per-row: only DADBEAR rows whose `contribution_id` column is
///    still NULL are migrated. Once migrated, the column points at
///    the new contribution row so re-runs skip them.
///
/// Used by `init_pyramid_db` — callers should not invoke this directly
/// unless they've already initialized the contribution table schema.
pub fn migrate_legacy_dadbear_to_contributions(conn: &Connection) -> Result<()> {
    // Guard 1: short-circuit if the migration marker already exists.
    let marker_exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pyramid_config_contributions
         WHERE schema_type = '_migration_marker'
           AND source = 'migration'
           AND created_by = 'dadbear_bootstrap'",
        [],
        |row| row.get(0),
    )?;
    if marker_exists > 0 {
        return Ok(());
    }

    // Collect every DADBEAR row still missing a contribution_id.
    let sql = format!(
        "SELECT {DADBEAR_CONFIG_COLUMNS} FROM pyramid_dadbear_config
         WHERE contribution_id IS NULL
         ORDER BY id ASC"
    );
    let rows: Vec<DadbearWatchConfig> = {
        let mut stmt = conn.prepare(&sql)?;
        let iter = stmt.query_map([], parse_dadbear_config)?;
        let mut out = Vec::new();
        for row in iter {
            out.push(row?);
        }
        out
    };

    for cfg in &rows {
        // Serialize to a dadbear_policy YAML document. Schema shape:
        //   source_path: string
        //   content_type: string
        //   scan_interval_secs: int
        //   debounce_secs: int
        //   session_timeout_secs: int
        //   batch_size: int
        //   enabled: bool
        //
        // Note: `id`, `slug`, `created_at`, and `updated_at` are
        // operational metadata, not policy. They don't belong in the
        // contribution YAML (slug lives on the contribution row itself).
        let yaml = format!(
            "source_path: {:?}\ncontent_type: {:?}\nscan_interval_secs: {}\ndebounce_secs: {}\nsession_timeout_secs: {}\nbatch_size: {}\nenabled: {}\n",
            cfg.source_path,
            cfg.content_type,
            cfg.scan_interval_secs,
            cfg.debounce_secs,
            cfg.session_timeout_secs,
            cfg.batch_size,
            cfg.enabled,
        );

        // Phase 5 wanderer fix: write canonical metadata instead of '{}'
        // per `docs/specs/wire-contribution-mapping.md` Creation-Time
        // Capture table, which says:
        //   "Bootstrap migration from legacy tables | Empty defaults.
        //    maturity = canon. description via prepare LLM on first publish."
        // Phase 5's original pass missed this direct-insert path — the
        // config_contributions.rs helpers now write canonical
        // metadata, but this low-level bootstrap migration still used
        // the `'{}'` stub. Without the fix, every legacy DADBEAR row
        // lands with default metadata (maturity=Draft) which means
        // `pyramid_publish_to_wire` refuses to publish them until the
        // user manually supersedes the row.
        let mut metadata = crate::pyramid::wire_native_metadata::default_wire_native_metadata(
            "dadbear_policy",
            Some(cfg.slug.as_str()),
        );
        metadata.maturity = crate::pyramid::wire_native_metadata::WireMaturity::Canon;
        let metadata_json = metadata
            .to_json()
            .unwrap_or_else(|_| "{}".to_string());

        let contribution_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO pyramid_config_contributions (
                contribution_id, slug, schema_type, yaml_content,
                wire_native_metadata_json, wire_publication_state_json,
                supersedes_id, superseded_by_id, triggering_note,
                status, source, wire_contribution_id, created_by, accepted_at
             ) VALUES (
                ?1, ?2, 'dadbear_policy', ?3,
                ?4, '{}',
                NULL, NULL, 'Migrated from legacy pyramid_dadbear_config',
                'active', 'migration', NULL, 'dadbear_bootstrap', datetime('now')
             )",
            rusqlite::params![contribution_id, cfg.slug, yaml, metadata_json],
        )?;
        conn.execute(
            "UPDATE pyramid_dadbear_config SET contribution_id = ?1 WHERE id = ?2",
            rusqlite::params![contribution_id, cfg.id],
        )?;
    }

    // Guard 1 writeback: insert the sentinel row so subsequent runs
    // short-circuit. The sentinel has NULL slug + empty yaml; it is
    // identified by the composite of (_migration_marker, migration,
    // dadbear_bootstrap).
    let marker_id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO pyramid_config_contributions (
            contribution_id, slug, schema_type, yaml_content,
            status, source, created_by, accepted_at
         ) VALUES (
            ?1, NULL, '_migration_marker', '',
            'active', 'migration', 'dadbear_bootstrap', datetime('now')
         )",
        rusqlite::params![marker_id],
    )?;

    Ok(())
}

/// Ghost-engine fix: convert every legacy `pyramid_auto_update_config` row
/// to a `pyramid_config_contributions` row with `schema_type =
/// 'auto_update_policy'`, `source = 'migration'`, `status = 'active'`.
///
/// Idempotent via the same two-guard pattern as the DADBEAR migration:
/// 1. Sentinel row with `created_by = 'auto_update_bootstrap'`
/// 2. Per-row: only rows whose `contribution_id` column is still NULL
///
/// Serializes only policy fields (auto_update, debounce_minutes,
/// min_changed_files, runaway_threshold). Excludes runtime/derived state
/// (frozen, breaker_tripped, timestamps, ingested_*).
pub fn migrate_legacy_auto_update_to_contributions(conn: &Connection) -> Result<()> {
    // Guard 1: short-circuit if the migration marker already exists.
    let marker_exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pyramid_config_contributions
         WHERE schema_type = '_migration_marker'
           AND source = 'migration'
           AND created_by = 'auto_update_bootstrap'",
        [],
        |row| row.get(0),
    )?;
    if marker_exists > 0 {
        return Ok(());
    }

    // Collect every auto_update_config row still missing a contribution_id.
    let mut stmt = conn.prepare(
        "SELECT slug, auto_update, debounce_minutes, min_changed_files, runaway_threshold
         FROM pyramid_auto_update_config
         WHERE contribution_id IS NULL
         ORDER BY slug ASC",
    )?;
    struct LegacyRow {
        slug: String,
        auto_update: bool,
        debounce_minutes: i64,
        min_changed_files: i64,
        runaway_threshold: f64,
    }
    let rows: Vec<LegacyRow> = {
        let iter = stmt.query_map([], |row| {
            Ok(LegacyRow {
                slug: row.get(0)?,
                auto_update: row.get::<_, i32>(1)? != 0,
                debounce_minutes: row.get(2)?,
                min_changed_files: row.get(3)?,
                runaway_threshold: row.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for row in iter {
            out.push(row?);
        }
        out
    };

    for cfg in &rows {
        // Serialize using serde_yaml for round-trip safety with the
        // dispatcher's serde_yaml::from_str() deserialization.
        let yaml_struct = AutoUpdatePolicyYaml {
            auto_update: cfg.auto_update,
            debounce_minutes: cfg.debounce_minutes,
            min_changed_files: cfg.min_changed_files,
            runaway_threshold: cfg.runaway_threshold,
        };
        let yaml = serde_yaml::to_string(&yaml_struct)
            .unwrap_or_else(|_| format!(
                "auto_update: {}\ndebounce_minutes: {}\nmin_changed_files: {}\nrunaway_threshold: {}\n",
                cfg.auto_update, cfg.debounce_minutes, cfg.min_changed_files, cfg.runaway_threshold,
            ));

        // Write canonical metadata with maturity=Canon (matches DADBEAR migration pattern).
        let mut metadata = crate::pyramid::wire_native_metadata::default_wire_native_metadata(
            "auto_update_policy",
            Some(cfg.slug.as_str()),
        );
        metadata.maturity = crate::pyramid::wire_native_metadata::WireMaturity::Canon;
        let metadata_json = metadata
            .to_json()
            .unwrap_or_else(|_| "{}".to_string());

        let contribution_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO pyramid_config_contributions (
                contribution_id, slug, schema_type, yaml_content,
                wire_native_metadata_json, wire_publication_state_json,
                supersedes_id, superseded_by_id, triggering_note,
                status, source, wire_contribution_id, created_by, accepted_at
             ) VALUES (
                ?1, ?2, 'auto_update_policy', ?3,
                ?4, '{}',
                NULL, NULL, 'Migrated from legacy pyramid_auto_update_config',
                'active', 'migration', NULL, 'auto_update_bootstrap', datetime('now')
             )",
            rusqlite::params![contribution_id, cfg.slug, yaml, metadata_json],
        )?;
        conn.execute(
            "UPDATE pyramid_auto_update_config SET contribution_id = ?1 WHERE slug = ?2",
            rusqlite::params![contribution_id, cfg.slug],
        )?;
    }

    // Sentinel row so subsequent runs short-circuit.
    let marker_id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO pyramid_config_contributions (
            contribution_id, slug, schema_type, yaml_content,
            status, source, created_by, accepted_at
         ) VALUES (
            ?1, NULL, '_migration_marker', '',
            'active', 'migration', 'auto_update_bootstrap', datetime('now')
         )",
        rusqlite::params![marker_id],
    )?;

    Ok(())
}

// ── Phase 14: pyramid_wire_update_cache CRUD helpers ─────────────────────────
//
// Per `docs/specs/wire-discovery-ranking.md` §Storage (line 297). These
// helpers back the `WireUpdatePoller` and the
// `pyramid_wire_update_available` / `pyramid_wire_acknowledge_update` /
// `pyramid_wire_pull_latest` IPCs. The table is a cache: the source of
// truth is the Wire's supersession state, and entries are expected to
// expire naturally as the user pulls or dismisses them.

/// A row from `pyramid_wire_update_cache`. One per pending update.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WireUpdateCacheEntry {
    pub local_contribution_id: String,
    pub latest_wire_contribution_id: String,
    pub chain_length_delta: i64,
    pub changes_summary: Option<String>,
    pub author_handles_json: Option<String>,
    pub checked_at: String,
    pub acknowledged_at: Option<String>,
}

/// Insert or update a row in `pyramid_wire_update_cache`. Idempotent:
/// on PRIMARY KEY conflict (same `local_contribution_id`), the existing
/// row is replaced. `checked_at` is refreshed to NOW on every upsert.
///
/// The caller (`WireUpdatePoller::run_once`) holds the writer lock so
/// this function uses a single `INSERT OR REPLACE`.
pub fn upsert_wire_update_cache(
    conn: &Connection,
    local_contribution_id: &str,
    latest_wire_contribution_id: &str,
    chain_length_delta: i64,
    changes_summary: Option<&str>,
    author_handles_json: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO pyramid_wire_update_cache (
            local_contribution_id, latest_wire_contribution_id,
            chain_length_delta, changes_summary, author_handles_json,
            checked_at, acknowledged_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'), NULL)",
        rusqlite::params![
            local_contribution_id,
            latest_wire_contribution_id,
            chain_length_delta,
            changes_summary,
            author_handles_json,
        ],
    )?;
    Ok(())
}

/// List pending Wire updates (rows with `acknowledged_at IS NULL`).
/// When `slug` is `Some(...)`, filters to updates for contributions
/// matching that slug via a join against `pyramid_config_contributions`.
/// When `None`, returns all pending updates across every slug.
pub fn list_pending_wire_updates(
    conn: &Connection,
    slug: Option<&str>,
) -> Result<Vec<WireUpdateCacheEntry>> {
    let mut rows: Vec<WireUpdateCacheEntry> = Vec::new();

    let row_mapper = |row: &rusqlite::Row| -> rusqlite::Result<WireUpdateCacheEntry> {
        Ok(WireUpdateCacheEntry {
            local_contribution_id: row.get("local_contribution_id")?,
            latest_wire_contribution_id: row.get("latest_wire_contribution_id")?,
            chain_length_delta: row.get("chain_length_delta")?,
            changes_summary: row.get("changes_summary")?,
            author_handles_json: row.get("author_handles_json")?,
            checked_at: row.get("checked_at")?,
            acknowledged_at: row.get("acknowledged_at")?,
        })
    };

    if let Some(slug_val) = slug {
        let sql = "SELECT wuc.local_contribution_id, wuc.latest_wire_contribution_id,
                          wuc.chain_length_delta, wuc.changes_summary,
                          wuc.author_handles_json, wuc.checked_at, wuc.acknowledged_at
                   FROM pyramid_wire_update_cache wuc
                   JOIN pyramid_config_contributions pcc
                     ON pcc.contribution_id = wuc.local_contribution_id
                   WHERE wuc.acknowledged_at IS NULL
                     AND (pcc.slug = ?1 OR pcc.slug IS NULL)
                   ORDER BY wuc.checked_at DESC";
        let mut stmt = conn.prepare(sql)?;
        let iter = stmt.query_map(rusqlite::params![slug_val], row_mapper)?;
        for r in iter {
            rows.push(r?);
        }
    } else {
        let sql = "SELECT local_contribution_id, latest_wire_contribution_id,
                          chain_length_delta, changes_summary,
                          author_handles_json, checked_at, acknowledged_at
                   FROM pyramid_wire_update_cache
                   WHERE acknowledged_at IS NULL
                   ORDER BY checked_at DESC";
        let mut stmt = conn.prepare(sql)?;
        let iter = stmt.query_map([], row_mapper)?;
        for r in iter {
            rows.push(r?);
        }
    }

    Ok(rows)
}

/// Mark a Wire update notification as acknowledged (the user dismissed
/// the badge). Sets `acknowledged_at` to NOW. The row is preserved so
/// the next poller sweep can detect if a newer version arrives and
/// re-trigger the badge; it's not deleted.
pub fn acknowledge_wire_update(
    conn: &Connection,
    local_contribution_id: &str,
) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE pyramid_wire_update_cache
         SET acknowledged_at = datetime('now')
         WHERE local_contribution_id = ?1",
        rusqlite::params![local_contribution_id],
    )?;
    Ok(changed > 0)
}

/// Delete a row from `pyramid_wire_update_cache`. Called when the user
/// pulls the update — after the pull flow writes the new local
/// contribution, the cache entry is no longer relevant.
pub fn delete_wire_update_cache(
    conn: &Connection,
    local_contribution_id: &str,
) -> Result<bool> {
    let changed = conn.execute(
        "DELETE FROM pyramid_wire_update_cache WHERE local_contribution_id = ?1",
        rusqlite::params![local_contribution_id],
    )?;
    Ok(changed > 0)
}

/// List contributions that are tracked on the Wire (have a non-null
/// `wire_contribution_id`) and are currently active. Used by the
/// update poller to build its supersession-check payload.
pub fn list_wire_tracked_contributions(
    conn: &Connection,
) -> Result<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT contribution_id, wire_contribution_id, schema_type
         FROM pyramid_config_contributions
         WHERE wire_contribution_id IS NOT NULL
           AND status = 'active'",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
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

    // ── WS-VOCAB (Phase 3): Vocabulary catalog persistence ────────────────────
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_vocabulary_catalog (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL,
            entry_name TEXT NOT NULL,
            entry_type TEXT NOT NULL CHECK(entry_type IN ('topic', 'entity', 'decision', 'term', 'practice')),
            category TEXT,
            importance REAL,
            liveness TEXT NOT NULL DEFAULT 'live',
            detail TEXT,
            source_node_id TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(slug, entry_name, entry_type)
        );
        CREATE INDEX IF NOT EXISTS idx_vocab_catalog_slug ON pyramid_vocabulary_catalog(slug);
        CREATE INDEX IF NOT EXISTS idx_vocab_catalog_type ON pyramid_vocabulary_catalog(slug, entry_type);
        CREATE INDEX IF NOT EXISTS idx_vocab_catalog_liveness ON pyramid_vocabulary_catalog(slug, liveness);
        CREATE INDEX IF NOT EXISTS idx_vocab_catalog_updated ON pyramid_vocabulary_catalog(slug, updated_at);
        ",
    )?;

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

    // Phase 8: pyramid_auto_update_config INSERT removed — table dropped.
    // Contribution existence in pyramid_dadbear_config is the enable gate.

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
///
/// Accepts an optional event bus so the frozen-state change routes through
/// `auto_update_ops::freeze` (DB write + event emission) instead of a bare
/// UPDATE, satisfying the ghost-engine contract.
pub fn archive_slug(
    conn: &Connection,
    slug: &str,
    event_bus: Option<&std::sync::Arc<crate::pyramid::event_bus::BuildEventBus>>,
) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_slugs SET archived_at = datetime('now') WHERE slug = ?1",
        rusqlite::params![slug],
    )?;
    // Freeze via auto_update_ops so the event bus is notified and dispatch is blocked.
    // Phase 7: holds projection is the sole authority — no dual-write to old table.
    if let Some(bus) = event_bus {
        if let Err(e) = crate::pyramid::auto_update_ops::freeze(conn, bus, slug) {
            tracing::warn!(slug = %slug, "archive_slug: auto_update_ops::freeze failed: {e}");
        }
    } else {
        // Fallback when no event bus available (e.g. tests). Write hold event + projection
        // directly. No bus events emitted since there's no bus, but the DB is correct.
        conn.execute(
            "INSERT INTO dadbear_hold_events (slug, hold, action, reason, created_at)
             VALUES (?1, 'frozen', 'placed', 'archive_slug fallback (no bus)', datetime('now'))",
            rusqlite::params![slug],
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO dadbear_holds_projection (slug, hold, held_since, reason)
             VALUES (?1, 'frozen', datetime('now'), 'archive_slug fallback (no bus)')",
            rusqlite::params![slug],
        )?;
    }
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

// ── Phase 2: Change-Manifest In-Place Updates ────────────────────────────────
//
// `update_node_in_place` + manifest CRUD helpers implement the stale-check
// side of the change-manifest flow. The key property: the node ID stays the
// same and the live `pyramid_nodes` row is mutated in place. All evidence
// links and parent-child references stay valid because no new ID is created.
//
// The prior content is snapshotted into `pyramid_node_versions` using the
// existing WS-SCHEMA-V2 versioning infrastructure (the snapshot format that
// `apply_supersession` already writes), so the delta history is durable.
//
// This path is distinct from `apply_supersession` in two ways:
//   1. It takes a ContentUpdates struct (field-level add/update/remove
//      operations) rather than a complete new PyramidNode, so the LLM is
//      asked to produce targeted edits only.
//   2. It explicitly updates `pyramid_evidence.source_node_id` rows for
//      children_swapped entries, so the evidence graph stays consistent with
//      the new children list on the updated node.
//
// NOTE on immutability enforcement: `apply_supersession` enforces
// WS-IMMUTABILITY-ENFORCE (depth <= 1 && !provisional rejects mutation).
// `update_node_in_place` deliberately does NOT apply that check — the
// immutability invariant exists for Wire publication (the pyramid is a
// snapshot at publish time), not for local refresh. DADBEAR-driven
// file_change supersession (the primary depth==0 use case) needs to mutate
// local L0 nodes in place so files stay in sync with the live filesystem
// as the user edits. Dropping the guard here is the correct semantic for
// the local-node use case.

/// Apply a LLM-produced change manifest to a live pyramid node in place,
/// without creating a new node ID. Same ID, bumped `build_version`, prior
/// state snapshotted into `pyramid_node_versions` for durability.
///
/// Arguments:
/// - `conn` — a DB connection. The update runs inside a `BEGIN IMMEDIATE`
///   transaction so snapshot + in-place update + evidence rewrite are
///   atomic and roll back together on failure.
/// - `slug` / `node_id` — target node (must exist in `pyramid_nodes`).
/// - `updates` — field-level content updates (any None field is untouched).
/// - `children_swapped` — pairs of `(old_child_id, new_child_id)`. For each
///   pair the function:
///     - replaces `old` with `new` in the node's `children` JSON array
///     - UPDATEs `pyramid_evidence` rows where `source_node_id = old AND
///       target_node_id = node_id` to point at `new`.
/// - `supersession_reason` — e.g. "stale_refresh", "reroll". Recorded on
///   the pyramid_node_versions snapshot row.
///
/// Returns the new `build_version` after the bump.
pub fn update_node_in_place(
    conn: &Connection,
    slug: &str,
    node_id: &str,
    updates: &super::types::ContentUpdates,
    children_swapped: &[(String, String)],
    supersession_reason: &str,
) -> Result<i64> {
    use super::types::ContentUpdates;

    // BEGIN IMMEDIATE for writer-serialization. If we're nested inside an
    // outer transaction, use a SAVEPOINT instead so we can roll back just
    // this in-place update without torching the outer tx. We detect nesting
    // by attempting a BEGIN IMMEDIATE and falling back to a SAVEPOINT.
    let using_savepoint = conn
        .execute_batch("BEGIN IMMEDIATE;")
        .is_err();
    if using_savepoint {
        conn.execute_batch("SAVEPOINT update_node_in_place_sp;")?;
    }

    let result: Result<i64> = (|| {
        // 1. Load the current node row. We need the raw
        //    topics/terms/decisions/dead_ends JSON strings to apply per-entry
        //    operations, and the current headline/distilled for fallback
        //    when the manifest leaves those fields null.
        //
        //    `depth` and `build_version` are selected but not read directly
        //    — they're in the SELECT list so the snapshot row layout matches
        //    the existing pyramid_node_versions schema when we INSERT the
        //    snapshot row. The bump is done by the UPDATE statement
        //    (`COALESCE(build_version, 1) + 1`) so Rust never needs them.
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Snapshot {
            depth: i64,
            headline: String,
            distilled: String,
            topics: String,
            corrections: String,
            decisions: String,
            terms: String,
            dead_ends: String,
            self_prompt: Option<String>,
            children: String,
            parent_id: Option<String>,
            current_version: i64,
            build_version: i64,
            current_version_chain_phase: Option<String>,
            build_id: Option<String>,
            time_range_start: Option<String>,
            time_range_end: Option<String>,
            weight: Option<f64>,
            narrative_json: Option<String>,
            entities_json: Option<String>,
            key_quotes_json: Option<String>,
            transitions_json: Option<String>,
        }

        let snap: Snapshot = conn
            .query_row(
                "SELECT depth, headline, distilled,
                        COALESCE(topics, '[]'), COALESCE(corrections, '[]'),
                        COALESCE(decisions, '[]'), COALESCE(terms, '[]'),
                        COALESCE(dead_ends, '[]'), self_prompt,
                        COALESCE(children, '[]'), parent_id,
                        COALESCE(current_version, 1),
                        COALESCE(build_version, 1),
                        current_version_chain_phase, build_id,
                        time_range_start, time_range_end, weight,
                        narrative_json, entities_json, key_quotes_json, transitions_json
                 FROM pyramid_nodes
                 WHERE slug = ?1 AND id = ?2",
                rusqlite::params![slug, node_id],
                |row| {
                    Ok(Snapshot {
                        depth: row.get(0)?,
                        headline: row.get(1)?,
                        distilled: row.get(2)?,
                        topics: row.get(3)?,
                        corrections: row.get(4)?,
                        decisions: row.get(5)?,
                        terms: row.get(6)?,
                        dead_ends: row.get(7)?,
                        self_prompt: row.get(8)?,
                        children: row.get(9)?,
                        parent_id: row.get(10)?,
                        current_version: row.get(11)?,
                        build_version: row.get(12)?,
                        current_version_chain_phase: row.get(13)?,
                        build_id: row.get(14)?,
                        time_range_start: row.get(15)?,
                        time_range_end: row.get(16)?,
                        weight: row.get(17)?,
                        narrative_json: row.get(18)?,
                        entities_json: row.get(19)?,
                        key_quotes_json: row.get(20)?,
                        transitions_json: row.get(21)?,
                    })
                },
            )
            .map_err(|e| {
                anyhow::anyhow!(
                    "update_node_in_place: load of ({slug}, {node_id}) failed: {e}"
                )
            })?;

        // 2. Snapshot prior state into pyramid_node_versions BEFORE applying
        //    the updates. This matches the format `apply_supersession` uses
        //    so downstream tooling (collapse.rs, recovery.rs) finds a uniform
        //    version history shape regardless of the write path.
        conn.execute(
            "INSERT INTO pyramid_node_versions (
                slug, node_id, version,
                headline, distilled, topics, corrections, decisions,
                terms, dead_ends, self_prompt, children, parent_id,
                time_range_start, time_range_end, weight,
                narrative_json, entities_json, key_quotes_json, transitions_json,
                chain_phase, build_id, supersession_reason
             ) VALUES (
                ?1, ?2, ?3,
                ?4, ?5, ?6, ?7, ?8,
                ?9, ?10, ?11, ?12, ?13,
                ?14, ?15, ?16,
                ?17, ?18, ?19, ?20,
                ?21, ?22, ?23
             )",
            rusqlite::params![
                slug,
                node_id,
                snap.current_version,
                snap.headline,
                snap.distilled,
                snap.topics,
                snap.corrections,
                snap.decisions,
                snap.terms,
                snap.dead_ends,
                snap.self_prompt.clone().unwrap_or_default(),
                snap.children,
                snap.parent_id.clone().unwrap_or_default(),
                snap.time_range_start,
                snap.time_range_end,
                snap.weight,
                snap.narrative_json,
                snap.entities_json,
                snap.key_quotes_json,
                snap.transitions_json,
                snap.current_version_chain_phase.clone().unwrap_or_default(),
                snap.build_id.clone(),
                supersession_reason,
            ],
        )?;

        // 3. Apply per-entry operations to topics, terms, decisions, dead_ends.
        let new_topics_json = apply_topic_ops(&snap.topics, updates.topics.as_deref())?;
        let new_terms_json = apply_term_ops(&snap.terms, updates.terms.as_deref())?;
        let new_decisions_json =
            apply_decision_ops(&snap.decisions, updates.decisions.as_deref())?;
        let new_dead_ends_json =
            apply_dead_end_ops(&snap.dead_ends, updates.dead_ends.as_deref())?;

        // 4. Apply wholesale replacements where present.
        let new_distilled = updates
            .distilled
            .clone()
            .unwrap_or(snap.distilled.clone());
        let new_headline = updates
            .headline
            .clone()
            .unwrap_or(snap.headline.clone());

        // 5. Apply children_swapped to the children JSON array.
        let mut children_vec: Vec<String> =
            serde_json::from_str(&snap.children).unwrap_or_default();
        if !children_swapped.is_empty() {
            for (old, new) in children_swapped {
                for existing in children_vec.iter_mut() {
                    if existing == old {
                        *existing = new.clone();
                    }
                }
            }
        }
        let new_children_json =
            serde_json::to_string(&children_vec).unwrap_or_else(|_| "[]".to_string());

        // 6. In-place UPDATE on the live row. Bumps build_version and clears
        //    any stale `superseded_by` pointer so the row remains visible in
        //    the live_pyramid_nodes view.
        let rows_affected = conn.execute(
            "UPDATE pyramid_nodes SET
                headline = ?3,
                distilled = ?4,
                topics = ?5,
                terms = ?6,
                decisions = ?7,
                dead_ends = ?8,
                children = ?9,
                build_version = COALESCE(build_version, 1) + 1,
                current_version = COALESCE(current_version, 1) + 1,
                superseded_by = NULL
             WHERE slug = ?1 AND id = ?2",
            rusqlite::params![
                slug,
                node_id,
                new_headline,
                new_distilled,
                new_topics_json,
                new_terms_json,
                new_decisions_json,
                new_dead_ends_json,
                new_children_json,
            ],
        )?;
        if rows_affected == 0 {
            return Err(anyhow::anyhow!(
                "update_node_in_place: no row updated for ({slug}, {node_id})"
            ));
        }

        // 7. Rewrite evidence links for swapped children. For each pair,
        //    move KEEP and DISCONNECT rows from (old_child -> node) to
        //    (new_child -> node). This is the load-bearing step that keeps
        //    get_tree()'s children_by_parent lookup coherent after the
        //    update.
        //
        //    `pyramid_evidence` PK is (slug, build_id, source_node_id,
        //    target_node_id) so we handle potential conflicts by deleting
        //    any existing row at the destination first.
        for (old_child, new_child) in children_swapped {
            if old_child == new_child {
                continue;
            }
            // Collect the PK columns of the rows that need rewriting.
            let mut select_stmt = conn.prepare(
                "SELECT build_id FROM pyramid_evidence
                 WHERE slug = ?1 AND source_node_id = ?2 AND target_node_id = ?3",
            )?;
            let existing_build_ids: Vec<String> = select_stmt
                .query_map(rusqlite::params![slug, old_child, node_id], |row| {
                    row.get::<_, String>(0)
                })?
                .filter_map(|r| r.ok())
                .collect();
            drop(select_stmt);

            for ev_build_id in existing_build_ids {
                // Delete any (slug, build_id, new_child, node) row that
                // already exists to avoid PK conflict on UPDATE.
                conn.execute(
                    "DELETE FROM pyramid_evidence
                     WHERE slug = ?1 AND build_id = ?2
                       AND source_node_id = ?3 AND target_node_id = ?4",
                    rusqlite::params![slug, ev_build_id, new_child, node_id],
                )?;
                // Rewrite the old row's source to the new id.
                conn.execute(
                    "UPDATE pyramid_evidence
                     SET source_node_id = ?1
                     WHERE slug = ?2 AND build_id = ?3
                       AND source_node_id = ?4 AND target_node_id = ?5",
                    rusqlite::params![new_child, slug, ev_build_id, old_child, node_id],
                )?;
            }
        }

        // 8. Return the new build_version.
        let new_bv: i64 = conn.query_row(
            "SELECT build_version FROM pyramid_nodes WHERE slug = ?1 AND id = ?2",
            rusqlite::params![slug, node_id],
            |r| r.get(0),
        )?;
        // Quiet the unused-import warning for ContentUpdates on older rustc
        // bodies that inline the match.
        let _ = std::mem::size_of::<ContentUpdates>();
        Ok(new_bv)
    })();

    match result {
        Ok(v) => {
            if using_savepoint {
                conn.execute_batch("RELEASE SAVEPOINT update_node_in_place_sp;")?;
            } else {
                conn.execute_batch("COMMIT;")?;
            }
            Ok(v)
        }
        Err(e) => {
            if using_savepoint {
                let _ = conn.execute_batch(
                    "ROLLBACK TO SAVEPOINT update_node_in_place_sp; \
                     RELEASE SAVEPOINT update_node_in_place_sp;",
                );
            } else {
                let _ = conn.execute_batch("ROLLBACK;");
            }
            Err(e)
        }
    }
}

// ── Helpers for applying per-entry content updates ───────────────────────────
//
// Each of these takes the current JSON string stored on the node and an
// Option<&[Op]> of updates, and returns the new JSON string. The helpers are
// forgiving about malformed stored JSON — an unparseable current value is
// treated as an empty array so the manifest can still rebuild from scratch.

fn apply_topic_ops(
    current_json: &str,
    ops: Option<&[super::types::TopicOp]>,
) -> Result<String> {
    use super::types::Topic;
    let ops = match ops {
        Some(ops) => ops,
        None => return Ok(current_json.to_string()),
    };

    let mut topics: Vec<Topic> =
        serde_json::from_str(current_json).unwrap_or_default();

    for op in ops {
        match op.action.as_str() {
            "add" => {
                topics.push(Topic {
                    name: op.name.clone(),
                    current: op.current.clone(),
                    entities: Vec::new(),
                    corrections: Vec::new(),
                    decisions: Vec::new(),
                    extra: Default::default(),
                });
            }
            "update" => {
                if let Some(existing) = topics.iter_mut().find(|t| t.name == op.name) {
                    existing.current = op.current.clone();
                } else {
                    // Treat update-of-missing as an add so the LLM's intent
                    // is preserved even if the node drifted.
                    topics.push(Topic {
                        name: op.name.clone(),
                        current: op.current.clone(),
                        entities: Vec::new(),
                        corrections: Vec::new(),
                        decisions: Vec::new(),
                        extra: Default::default(),
                    });
                }
            }
            "remove" => {
                topics.retain(|t| t.name != op.name);
            }
            _ => {}
        }
    }

    Ok(serde_json::to_string(&topics)?)
}

fn apply_term_ops(
    current_json: &str,
    ops: Option<&[super::types::TermOp]>,
) -> Result<String> {
    use super::types::Term;
    let ops = match ops {
        Some(ops) => ops,
        None => return Ok(current_json.to_string()),
    };

    let mut terms: Vec<Term> = serde_json::from_str(current_json).unwrap_or_default();

    for op in ops {
        match op.action.as_str() {
            "add" => {
                terms.push(Term {
                    term: op.term.clone(),
                    definition: op.definition.clone(),
                });
            }
            "update" => {
                if let Some(existing) = terms.iter_mut().find(|t| t.term == op.term) {
                    existing.definition = op.definition.clone();
                } else {
                    terms.push(Term {
                        term: op.term.clone(),
                        definition: op.definition.clone(),
                    });
                }
            }
            "remove" => {
                terms.retain(|t| t.term != op.term);
            }
            _ => {}
        }
    }

    Ok(serde_json::to_string(&terms)?)
}

fn apply_decision_ops(
    current_json: &str,
    ops: Option<&[super::types::DecisionOp]>,
) -> Result<String> {
    use super::types::Decision;
    let ops = match ops {
        Some(ops) => ops,
        None => return Ok(current_json.to_string()),
    };

    let mut decisions: Vec<Decision> =
        serde_json::from_str(current_json).unwrap_or_default();

    for op in ops {
        match op.action.as_str() {
            "add" => {
                decisions.push(Decision {
                    decided: op.decided.clone(),
                    why: op.why.clone(),
                    rejected: String::new(),
                    stance: if op.stance.is_empty() {
                        "open".to_string()
                    } else {
                        op.stance.clone()
                    },
                    importance: 0.0,
                    related: Vec::new(),
                });
            }
            "update" => {
                if let Some(existing) =
                    decisions.iter_mut().find(|d| d.decided == op.decided)
                {
                    if !op.why.is_empty() {
                        existing.why = op.why.clone();
                    }
                    if !op.stance.is_empty() {
                        existing.stance = op.stance.clone();
                    }
                } else {
                    decisions.push(Decision {
                        decided: op.decided.clone(),
                        why: op.why.clone(),
                        rejected: String::new(),
                        stance: if op.stance.is_empty() {
                            "open".to_string()
                        } else {
                            op.stance.clone()
                        },
                        importance: 0.0,
                        related: Vec::new(),
                    });
                }
            }
            "remove" => {
                decisions.retain(|d| d.decided != op.decided);
            }
            _ => {}
        }
    }

    Ok(serde_json::to_string(&decisions)?)
}

fn apply_dead_end_ops(
    current_json: &str,
    ops: Option<&[super::types::DeadEndOp]>,
) -> Result<String> {
    let ops = match ops {
        Some(ops) => ops,
        None => return Ok(current_json.to_string()),
    };

    let mut dead_ends: Vec<String> =
        serde_json::from_str(current_json).unwrap_or_default();

    for op in ops {
        match op.action.as_str() {
            "add" => {
                if !dead_ends.iter().any(|d| d == &op.value) {
                    dead_ends.push(op.value.clone());
                }
            }
            "remove" => {
                dead_ends.retain(|d| d != &op.value);
            }
            _ => {}
        }
    }

    Ok(serde_json::to_string(&dead_ends)?)
}

// ── Manifest CRUD helpers ───────────────────────────────────────────────────

/// Insert a `pyramid_change_manifests` row and return its new id.
pub fn save_change_manifest(
    conn: &Connection,
    slug: &str,
    node_id: &str,
    build_version: i64,
    manifest_json: &str,
    note: Option<&str>,
    supersedes_manifest_id: Option<i64>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO pyramid_change_manifests (
            slug, node_id, build_version, manifest_json, note, supersedes_manifest_id
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            slug,
            node_id,
            build_version,
            manifest_json,
            note,
            supersedes_manifest_id,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Load all change manifests for a (slug, node_id), ordered by applied_at
/// ascending (oldest first). Use this for audit views and supersession-chain
/// walking.
pub fn get_change_manifests_for_node(
    conn: &Connection,
    slug: &str,
    node_id: &str,
) -> Result<Vec<super::types::ChangeManifestRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, node_id, build_version, manifest_json, note,
                supersedes_manifest_id, applied_at
         FROM pyramid_change_manifests
         WHERE slug = ?1 AND node_id = ?2
         ORDER BY applied_at ASC, id ASC",
    )?;

    let rows = stmt.query_map(rusqlite::params![slug, node_id], |row| {
        Ok(super::types::ChangeManifestRecord {
            id: row.get(0)?,
            slug: row.get(1)?,
            node_id: row.get(2)?,
            build_version: row.get(3)?,
            manifest_json: row.get(4)?,
            note: row.get(5)?,
            supersedes_manifest_id: row.get(6)?,
            applied_at: row.get(7)?,
        })
    })?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Load the most recent change manifest for a (slug, node_id). Returns None
/// if no manifests have been applied. "Most recent" is determined by
/// `applied_at DESC, id DESC` so the latest row wins even with equal
/// timestamps in the same second.
pub fn get_latest_manifest_for_node(
    conn: &Connection,
    slug: &str,
    node_id: &str,
) -> Result<Option<super::types::ChangeManifestRecord>> {
    let result = conn.query_row(
        "SELECT id, slug, node_id, build_version, manifest_json, note,
                supersedes_manifest_id, applied_at
         FROM pyramid_change_manifests
         WHERE slug = ?1 AND node_id = ?2
         ORDER BY applied_at DESC, id DESC
         LIMIT 1",
        rusqlite::params![slug, node_id],
        |row| {
            Ok(super::types::ChangeManifestRecord {
                id: row.get(0)?,
                slug: row.get(1)?,
                node_id: row.get(2)?,
                build_version: row.get(3)?,
                manifest_json: row.get(4)?,
                note: row.get(5)?,
                supersedes_manifest_id: row.get(6)?,
                applied_at: row.get(7)?,
            })
        },
    );
    match result {
        Ok(rec) => Ok(Some(rec)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

// ── end Phase 2 change-manifest helpers ─────────────────────────────────────

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

/// Get ingested extensions for a slug from pyramid_build_metadata (canonical source).
/// Returns empty Vec if no metadata exists.
pub fn get_ingested_extensions(conn: &Connection, slug: &str) -> Result<Vec<String>> {
    let json_str: String = conn
        .query_row(
            "SELECT ingested_extensions FROM pyramid_build_metadata WHERE slug = ?1",
            rusqlite::params![slug],
            |row| row.get(0),
        )
        .unwrap_or_else(|_| "[]".to_string());
    let exts: Vec<String> = serde_json::from_str(&json_str).unwrap_or_default();
    Ok(exts)
}

/// Get ingested config filenames for a slug from pyramid_build_metadata (canonical source).
/// Returns empty Vec if no metadata exists.
pub fn get_ingested_config_files(conn: &Connection, slug: &str) -> Result<Vec<String>> {
    let json_str: String = conn
        .query_row(
            "SELECT ingested_config_files FROM pyramid_build_metadata WHERE slug = ?1",
            rusqlite::params![slug],
            |row| row.get(0),
        )
        .unwrap_or_else(|_| "[]".to_string());
    let configs: Vec<String> = serde_json::from_str(&json_str).unwrap_or_default();
    Ok(configs)
}

// ── Build Pipeline Seeding Helpers ───────────────────────────────────────────

/// Insert build metadata defaults for a slug with ingested extensions and config files.
/// Uses INSERT OR IGNORE so it won't overwrite existing metadata.
///
/// DECOMMISSIONED: No longer writes to pyramid_auto_update_config (table dropped).
/// Writes to pyramid_build_metadata instead.
pub fn insert_auto_update_config_defaults(
    conn: &Connection,
    slug: &str,
    extensions_json: &str,
    config_files_json: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_build_metadata (slug, ingested_extensions, ingested_config_files, updated_at)
         VALUES (?1, ?2, ?3, datetime('now'))
         ON CONFLICT(slug) DO UPDATE SET
            ingested_extensions = CASE WHEN pyramid_build_metadata.ingested_extensions = '[]'
                                       THEN excluded.ingested_extensions
                                       ELSE pyramid_build_metadata.ingested_extensions END,
            ingested_config_files = CASE WHEN pyramid_build_metadata.ingested_config_files = '[]'
                                         THEN excluded.ingested_config_files
                                         ELSE pyramid_build_metadata.ingested_config_files END,
            updated_at = datetime('now')",
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

/// Load auto-update config for a slug from canonical sources.
///
/// DECOMMISSIONED: No longer reads from pyramid_auto_update_config (table dropped).
/// Synthesizes from dadbear_norms contributions (for policy fields) and the
/// holds projection (for frozen/breaker state). Returns None if no DADBEAR
/// config exists for this slug.
pub fn get_auto_update_config(
    conn: &Connection,
    slug: &str,
) -> Option<super::types::AutoUpdateConfig> {
    // Check if slug has a dadbear config (enable gate)
    let has_config: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pyramid_dadbear_config WHERE slug = ?1)",
            rusqlite::params![slug],
            |row| row.get(0),
        )
        .unwrap_or(false);

    if !has_config {
        return None;
    }

    // Load policy from dadbear_norms contribution (if any)
    let (debounce_minutes, min_changed_files, runaway_threshold) = conn
        .query_row(
            "SELECT yaml_content FROM pyramid_config_contributions
             WHERE slug = ?1 AND schema_type = 'dadbear_norms' AND status = 'active'
             ORDER BY accepted_at DESC LIMIT 1",
            rusqlite::params![slug],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .and_then(|yaml| {
            serde_yaml::from_str::<serde_json::Value>(&yaml).ok()
        })
        .map(|v| {
            let debounce = v.get("debounce_minutes").and_then(|d| d.as_i64()).unwrap_or(5) as i32;
            let min_changed = v.get("min_changed_files").and_then(|m| m.as_i64()).unwrap_or(1) as i32;
            let threshold = v.get("runaway_threshold").and_then(|t| t.as_f64()).unwrap_or(0.5);
            (debounce, min_changed, threshold)
        })
        .unwrap_or((5, 1, 0.5));

    // Load holds from the canonical projection
    let frozen: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM dadbear_holds_projection WHERE slug = ?1 AND hold = 'frozen')",
            rusqlite::params![slug],
            |row| row.get(0),
        )
        .unwrap_or(false);

    let frozen_at: Option<String> = if frozen {
        conn.query_row(
            "SELECT held_since FROM dadbear_holds_projection WHERE slug = ?1 AND hold = 'frozen'",
            rusqlite::params![slug],
            |row| row.get(0),
        )
        .ok()
    } else {
        None
    };

    let breaker_tripped: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM dadbear_holds_projection WHERE slug = ?1 AND hold = 'breaker')",
            rusqlite::params![slug],
            |row| row.get(0),
        )
        .unwrap_or(false);

    let breaker_tripped_at: Option<String> = if breaker_tripped {
        conn.query_row(
            "SELECT held_since FROM dadbear_holds_projection WHERE slug = ?1 AND hold = 'breaker'",
            rusqlite::params![slug],
            |row| row.get(0),
        )
        .ok()
    } else {
        None
    };

    Some(super::types::AutoUpdateConfig {
        slug: slug.to_string(),
        auto_update: true, // If dadbear_config exists, it's enabled
        debounce_minutes,
        min_changed_files,
        runaway_threshold,
        breaker_tripped,
        breaker_tripped_at,
        frozen,
        frozen_at,
    })
}

/// Get auto-update status for a slug (config + pending observation events + last check time).
pub fn get_auto_update_status(conn: &Connection, slug: &str) -> Result<Option<serde_json::Value>> {
    let config = match get_auto_update_config(conn, slug) {
        Some(c) => c,
        None => return Ok(None),
    };

    let mut pending_by_layer = std::collections::HashMap::new();
    for layer in 0..=3 {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dadbear_observation_events
                 WHERE slug = ?1 AND layer = ?2
                   AND id > COALESCE(
                       (SELECT last_bridge_observation_id FROM pyramid_build_metadata WHERE slug = ?1),
                       0
                   )",
                rusqlite::params![slug, layer],
                |row| row.get(0),
            )
            .unwrap_or(0);
        pending_by_layer.insert(layer, count);
    }

    let last_check_at: Option<String> = conn
        .query_row(
            "SELECT MAX(a.dispatched_at) FROM dadbear_work_attempts a
             JOIN dadbear_work_items wi ON a.work_item_id = wi.id
             WHERE wi.slug = ?1",
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

/// Query stale check log entries from dadbear_work_items (canonical source).
pub fn get_stale_log(
    conn: &Connection,
    slug: &str,
    layer: Option<i32>,
    stale: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Vec<serde_json::Value>> {
    // Read from dadbear_work_items + dadbear_work_attempts (canonical).
    // Map the old stale filter values to work_item states.
    let mut sql = String::from(
        "SELECT wi.id, wi.slug, wi.batch_id, wi.layer, wi.target_id,
                wi.state, COALESCE(wi.result_json, ''),
                wi.chunk_index, 1,
                COALESCE(wi.completed_at, wi.compiled_at),
                wi.result_tokens_in, wi.result_cost_usd
         FROM dadbear_work_items wi WHERE wi.slug = ?1",
    );
    let mut param_vals: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    param_vals.push(Box::new(slug.to_string()));

    if let Some(layer_val) = layer {
        param_vals.push(Box::new(layer_val));
        sql.push_str(&format!(" AND wi.layer = ?{}", param_vals.len()));
    }
    if let Some(stale_str) = stale {
        // Map old stale filter to work_item state filter
        let state_filter: &str = match stale_str {
            "yes" | "true" | "1" => "applied",
            "no" | "false" | "0" => "completed",
            _ => "applied",
        };
        param_vals.push(Box::new(state_filter.to_string()));
        sql.push_str(&format!(" AND wi.state = ?{}", param_vals.len()));
    }

    param_vals.push(Box::new(limit));
    sql.push_str(&format!(
        " ORDER BY COALESCE(wi.completed_at, wi.compiled_at) DESC LIMIT ?{}",
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
                "id": row.get::<_, String>(0)?,
                "slug": row.get::<_, String>(1)?,
                "batch_id": row.get::<_, String>(2)?,
                "layer": row.get::<_, i32>(3)?,
                "target_id": row.get::<_, Option<String>>(4)?,
                "stale": match row.get::<_, String>(5)?.as_str() {
                    "applied" => "yes",
                    "completed" => "no",
                    "failed" => "skipped",
                    _ => "unknown",
                },
                "reason": row.get::<_, String>(6)?,
                "checker_index": row.get::<_, Option<i32>>(7)?,
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

/// Phase 18b: insert a fully-formed audit row stamped as a cache hit in
/// a single statement. Cache hits don't go through the pending → complete
/// dance because there is no LLM call to "fail mid-flight" — the cached
/// result is read in microseconds. The row carries `parsed_ok = true`
/// (the cached output already parsed once when it was stored) and
/// `cache_hit = 1` so the audit trail can distinguish it from wire calls.
///
/// Returns the inserted audit row id.
#[allow(clippy::too_many_arguments)]
pub fn insert_llm_audit_cache_hit(
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
    raw_response: &str,
    prompt_tokens: i64,
    completion_tokens: i64,
    latency_ms: i64,
    generation_id: Option<&str>,
) -> Result<i64> {
    // Deduplicate system prompt via hash, matching insert_llm_audit_pending.
    let sys_hash = prompt_hash(system_prompt);
    let _ = conn.execute(
        "INSERT OR IGNORE INTO pyramid_prompt_store (hash, content, char_count)
         VALUES (?1, ?2, ?3)",
        rusqlite::params![sys_hash, system_prompt, system_prompt.len() as i64],
    );
    let sys_ref = format!("@@hash:{}", sys_hash);

    conn.execute(
        "INSERT INTO pyramid_llm_audit
         (slug, build_id, node_id, step_name, call_purpose, depth, model,
          system_prompt, user_prompt, raw_response, parsed_ok,
          prompt_tokens, completion_tokens, latency_ms, generation_id,
          status, completed_at, cache_hit)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1,
                 ?11, ?12, ?13, ?14, 'complete', datetime('now'), 1)",
        rusqlite::params![
            slug, build_id, node_id, step_name, call_purpose, depth,
            model, sys_ref, user_prompt, raw_response,
            prompt_tokens, completion_tokens, latency_ms, generation_id,
        ],
    )?;
    Ok(conn.last_insert_rowid())
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
                status, created_at, completed_at, cache_hit
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
                status, created_at, completed_at, cache_hit
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
/// Clean up audit records for a slug, preserving the LATEST audit record
/// per node. Only deletes older duplicates where a node has been re-processed
/// in a newer build. Nodes that were not re-processed retain their original
/// audit records from whichever build created them.
pub fn cleanup_old_audit_records(conn: &Connection, slug: &str) -> Result<i64> {
    // Delete audit records where a NEWER record exists for the same slug + node_id.
    // This preserves the latest audit per node regardless of build_id.
    let deleted = conn.execute(
        "DELETE FROM pyramid_llm_audit WHERE id IN (
            SELECT a.id FROM pyramid_llm_audit a
            INNER JOIN pyramid_llm_audit b
                ON a.slug = b.slug AND a.node_id = b.node_id AND a.id < b.id
            WHERE a.slug = ?1
        )",
        rusqlite::params![slug],
    )?;
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
        // Phase 18b: cache_hit is the 20th column. Default to false on
        // pre-Phase-18b rows that haven't been touched by the migration
        // (the migration is idempotent + run on init, but defensively
        // handle row.get errors with `unwrap_or(0)`).
        cache_hit: row.get::<_, i32>(19).unwrap_or(0) != 0,
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

// ── Phase 6: LLM Output Cache CRUD (pyramid_step_cache) ───────────────────
//
// Content-addressable cache for LLM outputs. The table is keyed on
// `(slug, cache_key)` where `cache_key = sha256(inputs_hash, prompt_hash,
// model_id)` computed by `pyramid::step_context::compute_cache_key`.
//
// Writes are INSERT OR REPLACE — identical content addressing produces
// an update (latest wins). Force-fresh writes (reroll) link back to the
// prior row via `supersedes_cache_id` and retain their own unique row
// for version history.

use super::step_context::{CacheEntry, CachedStepOutput};

fn cached_step_output_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CachedStepOutput> {
    Ok(CachedStepOutput {
        id: row.get(0)?,
        slug: row.get(1)?,
        build_id: row.get(2)?,
        step_name: row.get(3)?,
        chunk_index: row.get(4)?,
        depth: row.get(5)?,
        cache_key: row.get(6)?,
        inputs_hash: row.get(7)?,
        prompt_hash: row.get(8)?,
        model_id: row.get(9)?,
        output_json: row.get(10)?,
        token_usage_json: row.get(11)?,
        cost_usd: row.get(12)?,
        latency_ms: row.get(13)?,
        created_at: row.get(14)?,
        force_fresh: row.get::<_, i64>(15)? != 0,
        supersedes_cache_id: row.get(16)?,
        note: row.get::<_, Option<String>>(17).unwrap_or(None),
        invalidated_by: row.get::<_, Option<String>>(18).unwrap_or(None),
    })
}

/// Look up a cached LLM output by content-addressable key. Returns
/// `Ok(None)` if no row matches — callers MUST treat this as a miss and
/// fall through to the HTTP path.
///
/// Returns the most recent matching row when multiple exist (e.g. a
/// force-fresh write superseded an earlier entry under the same cache
/// key). Multiple rows under the same `(slug, cache_key)` are prevented
/// by the UNIQUE constraint; this ORDER BY is a defensive tie-break.
///
/// Phase 13: rows with a non-null `invalidated_by` column are treated
/// as forced cache misses — the downstream walker set this flag to
/// mark the row as orphaned by an upstream reroll, and callers that
/// return the row would serve stale content. We filter at the query
/// level so the lookup is a single roundtrip.
pub fn check_cache(
    conn: &Connection,
    slug: &str,
    cache_key: &str,
) -> Result<Option<CachedStepOutput>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, build_id, step_name, chunk_index, depth, cache_key,
                inputs_hash, prompt_hash, model_id, output_json, token_usage_json,
                cost_usd, latency_ms, created_at, force_fresh, supersedes_cache_id,
                note, invalidated_by
         FROM pyramid_step_cache
         WHERE slug = ?1 AND cache_key = ?2 AND invalidated_by IS NULL
         ORDER BY id DESC
         LIMIT 1",
    )?;
    let row = stmt
        .query_row(rusqlite::params![slug, cache_key], cached_step_output_from_row)
        .optional()?;
    Ok(row)
}

/// Phase 13: fetch a cache entry including rows that were marked
/// invalidated. The reroll IPC uses this to look up the prior
/// content that the user is rerolling against, even if it has been
/// orphaned.
pub fn check_cache_including_invalidated(
    conn: &Connection,
    slug: &str,
    cache_key: &str,
) -> Result<Option<CachedStepOutput>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, build_id, step_name, chunk_index, depth, cache_key,
                inputs_hash, prompt_hash, model_id, output_json, token_usage_json,
                cost_usd, latency_ms, created_at, force_fresh, supersedes_cache_id,
                note, invalidated_by
         FROM pyramid_step_cache
         WHERE slug = ?1 AND cache_key = ?2
         ORDER BY id DESC
         LIMIT 1",
    )?;
    let row = stmt
        .query_row(rusqlite::params![slug, cache_key], cached_step_output_from_row)
        .optional()?;
    Ok(row)
}

// ── Phase 13: build viz pre-population and reroll support ────────────

/// Phase 13: compact summary of a cache entry used by the build viz
/// pre-population query (`pyramid_step_cache_for_build` IPC). Only
/// the fields the UI renders — `output_json` is excluded because the
/// timeline preview doesn't show content. Callers fetch the full row
/// via `check_cache*` when they need content.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CacheEntrySummary {
    pub id: i64,
    pub slug: String,
    pub build_id: String,
    pub step_name: String,
    pub chunk_index: i64,
    pub depth: i64,
    pub cache_key: String,
    pub model_id: String,
    pub cost_usd: Option<f64>,
    pub latency_ms: Option<i64>,
    pub created_at: String,
    pub force_fresh: bool,
    pub supersedes_cache_id: Option<i64>,
    pub note: Option<String>,
    pub invalidated_by: Option<String>,
}

/// Phase 13: find the most-recent build_id for a slug by scanning
/// `pyramid_step_cache` for the newest row. Used by
/// `list_cache_entries_for_latest_build` when the caller has no
/// explicit build_id (e.g. the PyramidBuildViz pre-populate path
/// where the UI only has a slug).
///
/// Phase 13 verifier fix: the initial implementation called the
/// `pyramid_step_cache_for_build` IPC with `(slug, slug)` as the
/// `(slug, build_id)` pair, which never matched any row since real
/// build_ids are strings like `chain-<uuid>`. This helper unblocks
/// the pre-populate path for the common "open the viz on the
/// current/latest build" flow.
pub fn find_latest_build_id_for_slug(conn: &Connection, slug: &str) -> Result<Option<String>> {
    let row: Option<String> = conn
        .query_row(
            "SELECT build_id FROM pyramid_step_cache
              WHERE slug = ?1
              ORDER BY id DESC
              LIMIT 1",
            rusqlite::params![slug],
            |row| row.get(0),
        )
        .optional()?;
    Ok(row)
}

/// Phase 13 verifier fix: list cache entries for the latest build of
/// a slug. Frontend callers that only have a slug (PyramidBuildViz
/// pre-populate path) use this so the step timeline seeds with real
/// data even though the UI doesn't know the build_id at mount time.
pub fn list_cache_entries_for_latest_build(
    conn: &Connection,
    slug: &str,
) -> Result<Vec<CacheEntrySummary>> {
    let Some(build_id) = find_latest_build_id_for_slug(conn, slug)? else {
        return Ok(Vec::new());
    };
    list_cache_entries_for_build(conn, slug, &build_id)
}

/// Phase 13: fetch every cache entry written during the given build,
/// newest-first. Used by `pyramid_step_cache_for_build` to seed the
/// step timeline on mount / resume so already-completed steps render
/// immediately.
pub fn list_cache_entries_for_build(
    conn: &Connection,
    slug: &str,
    build_id: &str,
) -> Result<Vec<CacheEntrySummary>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, build_id, step_name, chunk_index, depth, cache_key,
                model_id, cost_usd, latency_ms, created_at, force_fresh,
                supersedes_cache_id, note, invalidated_by
         FROM pyramid_step_cache
         WHERE slug = ?1 AND build_id = ?2
         ORDER BY id DESC",
    )?;
    let rows = stmt.query_map(rusqlite::params![slug, build_id], |row| {
        Ok(CacheEntrySummary {
            id: row.get(0)?,
            slug: row.get(1)?,
            build_id: row.get(2)?,
            step_name: row.get(3)?,
            chunk_index: row.get(4)?,
            depth: row.get(5)?,
            cache_key: row.get(6)?,
            model_id: row.get(7)?,
            cost_usd: row.get(8)?,
            latency_ms: row.get(9)?,
            created_at: row.get(10)?,
            force_fresh: row.get::<_, i64>(11)? != 0,
            supersedes_cache_id: row.get(12)?,
            note: row.get::<_, Option<String>>(13).unwrap_or(None),
            invalidated_by: row.get::<_, Option<String>>(14).unwrap_or(None),
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Phase 13: mark a set of cache entries as invalidated by a reroll.
/// `reason` is stored in `invalidated_by` (typically the originating
/// cache_key) so operators can trace the dependency chain.
///
/// Returns the number of rows flipped. Already-invalidated rows are
/// left untouched (idempotent). Rows superseded via
/// `supersede_cache_entry` are left alone — they're already archival.
pub fn invalidate_cache_entries(
    conn: &Connection,
    slug: &str,
    cache_keys: &[String],
    reason: &str,
) -> Result<usize> {
    if cache_keys.is_empty() {
        return Ok(0);
    }
    let mut total: usize = 0;
    for ck in cache_keys {
        let affected = conn.execute(
            "UPDATE pyramid_step_cache
                SET invalidated_by = ?1, invalidated_at = datetime('now')
              WHERE slug = ?2 AND cache_key = ?3 AND invalidated_by IS NULL",
            rusqlite::params![reason, slug, ck],
        )?;
        total += affected;
    }
    Ok(total)
}

/// Phase 13 verifier fix: invalidate cache entries and return the
/// exact set of cache keys that actually flipped (as opposed to
/// just a count). The reroll path emits `CacheInvalidated` events
/// per flipped key; without this helper it was emitting events for
/// the first N items of the input list, which may not correspond
/// to the actually-flipped rows when some entries were already
/// stale.
pub fn invalidate_cache_entries_returning_flipped(
    conn: &Connection,
    slug: &str,
    cache_keys: &[String],
    reason: &str,
) -> Result<Vec<String>> {
    let mut flipped: Vec<String> = Vec::new();
    for ck in cache_keys {
        let affected = conn.execute(
            "UPDATE pyramid_step_cache
                SET invalidated_by = ?1, invalidated_at = datetime('now')
              WHERE slug = ?2 AND cache_key = ?3 AND invalidated_by IS NULL",
            rusqlite::params![reason, slug, ck],
        )?;
        if affected > 0 {
            flipped.push(ck.clone());
        }
    }
    Ok(flipped)
}

/// Phase 13: find cache entries whose step_name + depth chain points
/// into a downstream consumer of the given `(step_name, chunk_index,
/// depth)` tuple. This is the single-level walker the spec asks for:
/// when a step at depth D is rerolled, any entry at depth D+1 whose
/// step consumes that depth's outputs is marked stale.
///
/// The MVP walker uses a simple heuristic: any entry at `depth + 1`
/// (independent of step_name) is considered a downstream dependent.
/// This over-invalidates by design — the spec allows for over-
/// invalidation and a transitive walker can refine this later.
/// Returns the list of affected cache_keys so the caller can emit
/// `CacheInvalidated` events.
pub fn find_downstream_cache_keys(
    conn: &Connection,
    slug: &str,
    depth: i64,
) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT cache_key
           FROM pyramid_step_cache
          WHERE slug = ?1 AND depth > ?2 AND invalidated_by IS NULL
          ORDER BY depth ASC, id ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![slug, depth], |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Phase 13: count how many supersession rows exist for a given step
/// slot in the last 10 minutes. Used by the reroll IPC to produce the
/// anti-slot-machine warning (the UI shows a banner after 3+ rerolls
/// but the backend does NOT hard-block).
pub fn count_recent_rerolls(
    conn: &Connection,
    slug: &str,
    step_name: &str,
    chunk_index: i64,
    depth: i64,
) -> Result<i64> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pyramid_step_cache
              WHERE slug = ?1 AND step_name = ?2 AND chunk_index = ?3 AND depth = ?4
                AND created_at > datetime('now', '-10 minutes')
                AND supersedes_cache_id IS NOT NULL",
            rusqlite::params![slug, step_name, chunk_index, depth],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0);
    Ok(count)
}

/// Insert or replace a cache entry. Uses the `(slug, cache_key)` unique
/// constraint as the conflict target so identical content-addressed
/// writes produce an update (most recent wins, keeping `created_at`
/// fresh).
///
/// Force-fresh entries should instead go through
/// `supersede_cache_entry` so the supersession link is preserved.
pub fn store_cache(conn: &Connection, entry: &CacheEntry) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_step_cache
            (slug, build_id, step_name, chunk_index, depth, cache_key,
             inputs_hash, prompt_hash, model_id, output_json, token_usage_json,
             cost_usd, latency_ms, created_at, force_fresh, supersedes_cache_id, note)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, datetime('now'), ?14, ?15, ?16)
         ON CONFLICT(slug, cache_key) DO UPDATE SET
            build_id = excluded.build_id,
            step_name = excluded.step_name,
            chunk_index = excluded.chunk_index,
            depth = excluded.depth,
            inputs_hash = excluded.inputs_hash,
            prompt_hash = excluded.prompt_hash,
            model_id = excluded.model_id,
            output_json = excluded.output_json,
            token_usage_json = excluded.token_usage_json,
            cost_usd = excluded.cost_usd,
            latency_ms = excluded.latency_ms,
            created_at = datetime('now'),
            force_fresh = excluded.force_fresh,
            supersedes_cache_id = excluded.supersedes_cache_id,
            note = excluded.note,
            invalidated_by = NULL,
            invalidated_at = NULL",
        rusqlite::params![
            entry.slug,
            entry.build_id,
            entry.step_name,
            entry.chunk_index,
            entry.depth,
            entry.cache_key,
            entry.inputs_hash,
            entry.prompt_hash,
            entry.model_id,
            entry.output_json,
            entry.token_usage_json,
            entry.cost_usd,
            entry.latency_ms,
            if entry.force_fresh { 1_i64 } else { 0_i64 },
            entry.supersedes_cache_id,
            entry.note,
        ],
    )
    .with_context(|| format!("store_cache(slug={}, cache_key={})", entry.slug, entry.cache_key))?;
    Ok(())
}

/// Phase 7: idempotent cache insert for `populate_from_import`.
///
/// Uses `INSERT ... ON CONFLICT(slug, cache_key) DO NOTHING`, which is the
/// SQLite equivalent of `INSERT OR IGNORE` on a conflict. Unlike
/// [`store_cache`], this helper will NEVER overwrite an existing row — it
/// silently leaves pre-existing rows untouched and reports whether the new
/// row landed.
///
/// This is the exact semantic `docs/specs/cache-warming-and-import.md`
/// mandates for the import path (see "Idempotency" section ~line 341):
///
/// > Cache entries are content-addressable. Re-importing the same entry is
/// > a no-op because `pyramid_step_cache` has `UNIQUE(slug, cache_key)` and
/// > the import uses `INSERT OR IGNORE`. A partial import's inserted
/// > entries are not duplicated on resume.
///
/// The reason we need `DO NOTHING` specifically (and not `DO UPDATE`) is
/// the reroll + resume scenario: if a user imports a pyramid, then
/// force-rerolls a step locally (which writes a fresh row through
/// `supersede_cache_entry` at the same content-addressable key with
/// `force_fresh = 1` and a `supersedes_cache_id` pointing at the archival
/// row), and then for any reason re-runs the import (network failure,
/// crash recovery, explicit resume), `store_cache`'s `DO UPDATE` branch
/// would clobber the rerolled row: the `output_json`, `force_fresh` flag,
/// and `supersedes_cache_id` link would all be overwritten by the
/// imported values. That silently undoes the user's reroll.
///
/// With `DO NOTHING`, the rerolled row is preserved and the re-imported
/// row is simply skipped. Returns `true` if the row was actually inserted,
/// `false` if a prior row occupied the slot.
///
/// Force-fresh writes should NEVER go through this helper — they go
/// through `supersede_cache_entry` which preserves supersession history.
pub fn store_cache_if_absent(conn: &Connection, entry: &CacheEntry) -> Result<bool> {
    let affected = conn
        .execute(
            "INSERT INTO pyramid_step_cache
                (slug, build_id, step_name, chunk_index, depth, cache_key,
                 inputs_hash, prompt_hash, model_id, output_json, token_usage_json,
                 cost_usd, latency_ms, created_at, force_fresh, supersedes_cache_id, note)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                     datetime('now'), ?14, ?15, ?16)
             ON CONFLICT(slug, cache_key) DO NOTHING",
            rusqlite::params![
                entry.slug,
                entry.build_id,
                entry.step_name,
                entry.chunk_index,
                entry.depth,
                entry.cache_key,
                entry.inputs_hash,
                entry.prompt_hash,
                entry.model_id,
                entry.output_json,
                entry.token_usage_json,
                entry.cost_usd,
                entry.latency_ms,
                if entry.force_fresh { 1_i64 } else { 0_i64 },
                entry.supersedes_cache_id,
                entry.note,
            ],
        )
        .with_context(|| {
            format!(
                "store_cache_if_absent(slug={}, cache_key={})",
                entry.slug, entry.cache_key
            )
        })?;
    // SQLite returns the number of rows actually affected. DO NOTHING
    // means a conflict yields 0 rows affected.
    Ok(affected == 1)
}

/// Delete a cache row by its content-addressable key. Used by the
/// verification-failure path in `call_model_unified_with_options` — when
/// `verify_cache_hit` returns `Mismatch*` or `CorruptedOutput`, the
/// caller deletes the stale row and falls through to HTTP.
pub fn delete_cache_entry(conn: &Connection, slug: &str, cache_key: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM pyramid_step_cache WHERE slug = ?1 AND cache_key = ?2",
        rusqlite::params![slug, cache_key],
    )?;
    Ok(())
}

/// Insert a new cache entry that supersedes a prior row for the same
/// `(slug, cache_key)`. Used by the force-fresh (reroll) path: the prior
/// row is retained (for version history) and the new row carries
/// `supersedes_cache_id` pointing at it.
///
/// Because `pyramid_step_cache` has `UNIQUE(slug, cache_key)`, we cannot
/// insert a second row with the same pair directly. The supersession
/// pattern is:
///   1. Save the prior row's id.
///   2. Soft-move the prior row to a unique-but-distinct cache_key by
///      appending the id so it stays queryable but no longer matches
///      content-addressable lookups.
///   3. Insert the new row under the original cache_key with
///      `supersedes_cache_id` pointing at the moved prior row.
///
/// This preserves the invariant "at most one active row per content
/// address" while retaining version history readable from
/// `pyramid_step_cache` directly.
pub fn supersede_cache_entry(
    conn: &Connection,
    slug: &str,
    prior_cache_key: &str,
    new_entry: &CacheEntry,
) -> Result<()> {
    let prior_row: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, cache_key FROM pyramid_step_cache
             WHERE slug = ?1 AND cache_key = ?2
             ORDER BY id DESC LIMIT 1",
            rusqlite::params![slug, prior_cache_key],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;

    let (prior_id, archival_cache_key) = if let Some((id, orig_key)) = prior_row {
        // Move the prior row to an archival cache_key so the new row can
        // claim the content-addressable slot. The archival key embeds
        // the prior id so it remains locatable but cannot collide with
        // any future content-addressable lookup (a real cache_key is a
        // 64-char SHA-256 hex and never starts with "archived:").
        let archival_key = format!("archived:{}:{}", id, orig_key);
        conn.execute(
            "UPDATE pyramid_step_cache SET cache_key = ?1 WHERE id = ?2",
            rusqlite::params![archival_key, id],
        )?;
        (Some(id), Some(archival_key))
    } else {
        (None, None)
    };

    // Link the new entry to the prior row (regardless of whether the
    // caller already supplied a supersedes_cache_id).
    let mut entry = new_entry.clone();
    if let Some(id) = prior_id {
        entry.supersedes_cache_id = Some(id);
    }
    entry.force_fresh = true;

    if let Err(e) = store_cache(conn, &entry) {
        // If the store failed, restore the prior row's cache_key so it
        // doesn't dangle in the archival state.
        if let (Some(id), Some(_)) = (prior_id, archival_cache_key) {
            let _ = conn.execute(
                "UPDATE pyramid_step_cache SET cache_key = ?1 WHERE id = ?2",
                rusqlite::params![prior_cache_key, id],
            );
        }
        return Err(e);
    }

    Ok(())
}

// ── Phase 7: Cache warming on import — pyramid_import_state CRUD ─────────────
//
// In-flight tracking for `pyramid_import_pyramid` calls. The row is the
// resumption cursor: a fresh import inserts a row with status
// `downloading_manifest`, the staleness pass updates progress fields and
// `last_node_id_processed`, and a successful run flips status to `complete`.
// On crash or user-cancel the row is left behind so a subsequent call can
// either resume from the cursor or be explicitly cancelled.
//
// See `docs/specs/cache-warming-and-import.md` "Import Resumability" section.

/// In-memory representation of a `pyramid_import_state` row.
///
/// Optional `nodes_total` / `cache_entries_total` fields reflect the column
/// nullability — both are initially NULL and only populated after the manifest
/// download phase has counted them.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportState {
    pub target_slug: String,
    pub wire_pyramid_id: String,
    pub source_path: String,
    pub status: String,
    pub nodes_total: Option<i64>,
    pub nodes_processed: i64,
    pub cache_entries_total: Option<i64>,
    pub cache_entries_validated: i64,
    pub cache_entries_inserted: i64,
    pub last_node_id_processed: Option<String>,
    pub error_message: Option<String>,
    pub started_at: Option<String>,
    pub updated_at: Option<String>,
}

fn import_state_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ImportState> {
    Ok(ImportState {
        target_slug: row.get(0)?,
        wire_pyramid_id: row.get(1)?,
        source_path: row.get(2)?,
        status: row.get(3)?,
        nodes_total: row.get(4)?,
        nodes_processed: row.get(5)?,
        cache_entries_total: row.get(6)?,
        cache_entries_validated: row.get(7)?,
        cache_entries_inserted: row.get(8)?,
        last_node_id_processed: row.get(9)?,
        error_message: row.get(10)?,
        started_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

/// Insert a fresh import state row. Fails if a row already exists for the
/// `target_slug` — callers MUST first call `load_import_state` and decide
/// whether to resume or `delete_import_state` before re-creating.
pub fn create_import_state(
    conn: &Connection,
    target_slug: &str,
    wire_pyramid_id: &str,
    source_path: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_import_state (
            target_slug, wire_pyramid_id, source_path, status,
            nodes_total, nodes_processed,
            cache_entries_total, cache_entries_validated, cache_entries_inserted,
            last_node_id_processed, error_message, started_at, updated_at
         ) VALUES (?1, ?2, ?3, 'downloading_manifest', NULL, 0, NULL, 0, 0,
                   NULL, NULL, datetime('now'), datetime('now'))",
        rusqlite::params![target_slug, wire_pyramid_id, source_path],
    )
    .with_context(|| {
        format!(
            "create_import_state(target_slug={target_slug}, wire_pyramid_id={wire_pyramid_id})"
        )
    })?;
    Ok(())
}

/// Load the import state for a given target slug. Returns `None` if no row
/// exists (i.e. no in-flight or completed import for this slug).
pub fn load_import_state(
    conn: &Connection,
    target_slug: &str,
) -> Result<Option<ImportState>> {
    let mut stmt = conn.prepare(
        "SELECT target_slug, wire_pyramid_id, source_path, status,
                nodes_total, nodes_processed,
                cache_entries_total, cache_entries_validated, cache_entries_inserted,
                last_node_id_processed, error_message, started_at, updated_at
         FROM pyramid_import_state
         WHERE target_slug = ?1",
    )?;
    let row = stmt
        .query_row(rusqlite::params![target_slug], import_state_from_row)
        .optional()?;
    Ok(row)
}

/// Progress fields the resumable import passes update on each tick.
#[derive(Debug, Clone, Default)]
pub struct ImportStateProgress {
    pub status: Option<String>,
    pub nodes_total: Option<i64>,
    pub nodes_processed: Option<i64>,
    pub cache_entries_total: Option<i64>,
    pub cache_entries_validated: Option<i64>,
    pub cache_entries_inserted: Option<i64>,
    pub last_node_id_processed: Option<String>,
    pub error_message: Option<String>,
}

/// Update the progress fields on an existing import state row. Only the
/// supplied fields are written; the rest are left untouched. Always bumps
/// `updated_at`.
pub fn update_import_state(
    conn: &Connection,
    target_slug: &str,
    progress: &ImportStateProgress,
) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_import_state SET
            status = COALESCE(?2, status),
            nodes_total = COALESCE(?3, nodes_total),
            nodes_processed = COALESCE(?4, nodes_processed),
            cache_entries_total = COALESCE(?5, cache_entries_total),
            cache_entries_validated = COALESCE(?6, cache_entries_validated),
            cache_entries_inserted = COALESCE(?7, cache_entries_inserted),
            last_node_id_processed = COALESCE(?8, last_node_id_processed),
            error_message = COALESCE(?9, error_message),
            updated_at = datetime('now')
         WHERE target_slug = ?1",
        rusqlite::params![
            target_slug,
            progress.status,
            progress.nodes_total,
            progress.nodes_processed,
            progress.cache_entries_total,
            progress.cache_entries_validated,
            progress.cache_entries_inserted,
            progress.last_node_id_processed,
            progress.error_message,
        ],
    )?;
    Ok(())
}

/// Delete the import state row for a target slug. Used by `cancel` and by
/// `complete` cleanup paths. Idempotent — deleting a missing row is a no-op.
pub fn delete_import_state(conn: &Connection, target_slug: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM pyramid_import_state WHERE target_slug = ?1",
        rusqlite::params![target_slug],
    )?;
    Ok(())
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

/// Mark an unredeemed token as permanently failed (WS-ONLINE-H).
///
/// Called when a redeem attempt returns a permanent error (400, 401, 409)
/// indicating the token is invalid, already redeemed, or expired on the server.
/// Unlike `increment_unredeemed_retry`, this immediately marks the token as
/// 'failed' without incrementing retry_count — no point retrying permanent errors.
pub fn mark_unredeemed_failed(conn: &Connection, nonce: &str) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_unredeemed_tokens
         SET status = 'failed', last_retry_at = datetime('now')
         WHERE nonce = ?1 AND status = 'pending'",
        rusqlite::params![nonce],
    )?;
    Ok(())
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
                speaker: "Adam".to_string(),
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

// ── WS-DADBEAR-EXTEND: Session helper updates flagged by WS-PROVISIONAL ──────

/// Update the file_mtime on a provisional session. Called by the DADBEAR tick
/// loop when it observes a new mtime for the watched source file.
pub fn update_session_mtime(conn: &Connection, session_id: &str, mtime: &str) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_provisional_sessions
         SET file_mtime = ?2, updated_at = datetime('now')
         WHERE session_id = ?1",
        rusqlite::params![session_id, mtime],
    )?;
    Ok(())
}

/// Update the last_chunk_processed index on a provisional session. Called after
/// a provisional build processes through chunk N so the next tick picks up from
/// chunk N+1.
pub fn update_session_chunk_progress(
    conn: &Connection,
    session_id: &str,
    chunk_index: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_provisional_sessions
         SET last_chunk_processed = ?2, updated_at = datetime('now')
         WHERE session_id = ?1",
        rusqlite::params![session_id, chunk_index],
    )?;
    Ok(())
}

// ── WS-DADBEAR-EXTEND: DADBEAR watch config CRUD ─────────────────────────────

/// Column list for SELECT queries on pyramid_dadbear_config.
/// Note: last_scan_at (col 9) is read but not stored on DadbearWatchConfig;
/// it's accessed separately by the status endpoint.
const DADBEAR_CONFIG_COLUMNS: &str =
    "id, slug, source_path, content_type, scan_interval_secs, debounce_secs, session_timeout_secs, batch_size, enabled, last_scan_at, created_at, updated_at";

/// Parse a row from `pyramid_dadbear_config` into a `DadbearWatchConfig`.
fn parse_dadbear_config(row: &rusqlite::Row) -> rusqlite::Result<DadbearWatchConfig> {
    let last_scan_at: Option<String> = row.get(9)?;
    Ok(DadbearWatchConfig {
        id: row.get(0)?,
        slug: row.get(1)?,
        source_path: row.get(2)?,
        content_type: row.get(3)?,
        scan_interval_secs: row.get::<_, i64>(4)? as u64,
        debounce_secs: row.get::<_, i64>(5)? as u64,
        session_timeout_secs: row.get::<_, i64>(6)? as u64,
        batch_size: row.get::<_, i32>(7)? as u32,
        enabled: row.get::<_, i32>(8)? != 0,
        last_scan_at,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

/// Insert or update a DADBEAR watch config. Upserts on (slug, source_path).
pub fn save_dadbear_config(conn: &Connection, config: &DadbearWatchConfig) -> Result<i64> {
    conn.execute(
        "INSERT INTO pyramid_dadbear_config
            (slug, source_path, content_type, scan_interval_secs, debounce_secs,
             session_timeout_secs, batch_size, enabled)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(slug, source_path) DO UPDATE SET
            content_type = excluded.content_type,
            scan_interval_secs = excluded.scan_interval_secs,
            debounce_secs = excluded.debounce_secs,
            session_timeout_secs = excluded.session_timeout_secs,
            batch_size = excluded.batch_size,
            enabled = excluded.enabled,
            updated_at = datetime('now')",
        rusqlite::params![
            config.slug,
            config.source_path,
            config.content_type,
            config.scan_interval_secs as i64,
            config.debounce_secs as i64,
            config.session_timeout_secs as i64,
            config.batch_size as i32,
            config.enabled as i32,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Insert or update the operational DADBEAR row *and* ensure backing
/// `watch_root` + `dadbear_norms` contributions exist. Idempotent: if
/// contributions already exist for this (slug, source_path) they are
/// not duplicated.
///
/// Production callers that create new watch configs should use this
/// instead of bare `save_dadbear_config` so the contribution store
/// stays in sync with the operational table.
pub fn save_dadbear_config_with_contributions(
    conn: &Connection,
    config: &DadbearWatchConfig,
) -> Result<i64> {
    use super::config_contributions::{create_config_contribution, load_active_config_contribution};

    // 1. Operational write (upsert).
    let rowid = save_dadbear_config(conn, config)?;

    // 2. Ensure watch_root contribution exists for (slug, source_path).
    //    Check if there's already an active watch_root whose yaml
    //    contains this source_path. If not, create one.
    let existing_wr = load_active_config_contribution(conn, "watch_root", Some(&config.slug))?;
    let wr_id = if let Some(ref wr) = existing_wr {
        // Check if the source_path matches. A slug can have multiple
        // watch_roots for different source_paths, so we search by yaml.
        let matches_source: bool = wr.yaml_content.contains(&config.source_path);
        if matches_source {
            wr.contribution_id.clone()
        } else {
            let watch_root = WatchRootYaml {
                source_path: config.source_path.clone(),
                content_type: config.content_type.clone(),
            };
            let yaml_str = serde_yaml::to_string(&watch_root)
                .unwrap_or_else(|_| format!(
                    "source_path: {:?}\ncontent_type: {:?}\n",
                    watch_root.source_path, watch_root.content_type,
                ));
            create_config_contribution(
                conn,
                "watch_root",
                Some(&config.slug),
                &yaml_str,
                Some("Auto-created from post-build seed"),
                "local",
                Some("post_build_seed"),
                "active",
            )?
        }
    } else {
        let watch_root = WatchRootYaml {
            source_path: config.source_path.clone(),
            content_type: config.content_type.clone(),
        };
        let yaml_str = serde_yaml::to_string(&watch_root)
            .unwrap_or_else(|_| format!(
                "source_path: {:?}\ncontent_type: {:?}\n",
                watch_root.source_path, watch_root.content_type,
            ));
        create_config_contribution(
            conn,
            "watch_root",
            Some(&config.slug),
            &yaml_str,
            Some("Auto-created from post-build seed"),
            "local",
            Some("post_build_seed"),
            "active",
        )?
    };

    // 3. Ensure dadbear_norms contribution exists for this slug.
    if load_active_config_contribution(conn, "dadbear_norms", Some(&config.slug))?.is_none() {
        let norms = DadbearNormsYaml {
            scan_interval_secs: config.scan_interval_secs as i64,
            debounce_secs: config.debounce_secs as i64,
            session_timeout_secs: config.session_timeout_secs as i64,
            batch_size: config.batch_size as i64,
            min_changed_files: 1,
            runaway_threshold: 0.5,
            retention_window_days: 30,
        };
        let norms_yaml = serde_yaml::to_string(&norms)
            .unwrap_or_else(|_| "scan_interval_secs: 10\ndebounce_secs: 30\n".to_string());
        create_config_contribution(
            conn,
            "dadbear_norms",
            Some(&config.slug),
            &norms_yaml,
            Some("Auto-created from post-build seed"),
            "local",
            Some("post_build_seed"),
            "active",
        )?;
    }

    // 4. Sync contribution_id FK on the operational row.
    conn.execute(
        "UPDATE pyramid_dadbear_config SET contribution_id = ?1
         WHERE slug = ?2 AND source_path = ?3",
        rusqlite::params![wr_id, config.slug, config.source_path],
    )?;

    Ok(rowid)
}

/// Get all DADBEAR watch configs for a slug.
pub fn get_dadbear_configs(conn: &Connection, slug: &str) -> Result<Vec<DadbearWatchConfig>> {
    let sql = format!(
        "SELECT {DADBEAR_CONFIG_COLUMNS} FROM pyramid_dadbear_config
         WHERE slug = ?1 ORDER BY source_path ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![slug], parse_dadbear_config)?;
    let mut configs = Vec::new();
    for row in rows {
        configs.push(row?);
    }
    Ok(configs)
}

/// Get all enabled DADBEAR watch configs (across all slugs).
///
/// **Phase 7 (DADBEAR canonical architecture — legacy cleanup complete):**
///
/// Enable gate: having a row in `pyramid_dadbear_config` is the enable gate.
/// Contribution existence is what matters — no `d.enabled` column check,
/// no `pyramid_auto_update_config.auto_update` subquery.
///
/// Dispatch gate: holds projection anti-join. A slug with ANY active hold
/// in `dadbear_holds_projection` is excluded from dispatch.
///
/// Column order matches `DADBEAR_CONFIG_COLUMNS` for `parse_dadbear_config()`.
pub fn get_enabled_dadbear_configs(conn: &Connection) -> Result<Vec<DadbearWatchConfig>> {
    let sql =
        "SELECT d.id, d.slug, d.source_path, d.content_type, d.scan_interval_secs,
                d.debounce_secs, d.session_timeout_secs, d.batch_size, d.enabled,
                d.last_scan_at, d.created_at, d.updated_at
         FROM pyramid_dadbear_config d
         WHERE NOT EXISTS (SELECT 1 FROM dadbear_holds_projection h WHERE h.slug = d.slug)
         ORDER BY d.slug ASC, d.source_path ASC";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], parse_dadbear_config)?;
    let mut configs = Vec::new();
    for row in rows {
        configs.push(row?);
    }
    Ok(configs)
}


// ── Phase 18c (L9): Scoped pause/resume for DADBEAR ──────────────────────────
//
// Phase 13 shipped only `scope: "all"`. Phase 18c adds the folder scope
// per `cross-pyramid-observability.md` "Pause-All Semantics" section
// (~line 286). The circle scope referenced in the spec depends on a
// `pyramid_metadata.circle_id` table that does not currently exist in
// the local DB schema (circle membership lives only in the Wire JWT
// claim layer, not in the local pyramid DB), so the circle scope is
// deferred to a later phase. See `deferral-ledger.md` for the deferral
// note.
//
// Folder canonicalization strategy: lexical only. The input folder
// has any trailing slash stripped before matching, then SQL uses
// `(source_path = ?1 OR source_path LIKE ?1 || '/%')` to match both
// the exact path and any descendant. No filesystem resolution, no
// symlink handling — the DB stores whatever the user originally
// configured, and we match against that text. Trailing-slash
// equivalence is preserved by the input normalization.

/// Strip a single trailing slash from a folder path so the LIKE match
/// works consistently for inputs like `/a/` vs `/a`. The root path `/`
/// is preserved as-is so it doesn't collapse to an empty string.
fn normalize_dadbear_folder(folder: &str) -> String {
    if folder.len() > 1 && folder.ends_with('/') {
        folder[..folder.len() - 1].to_string()
    } else {
        folder.to_string()
    }
}


/// Phase 18c: distinct list of `source_path` values across all
/// DADBEAR configs. Used by the frontend scope picker to populate the
/// folder dropdown. Sorted alphabetically; duplicates removed at the
/// SQL level via `DISTINCT`.
pub fn list_dadbear_source_paths(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT source_path FROM pyramid_dadbear_config
         ORDER BY source_path ASC",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Phase 18c + Phase 7: count slugs that WOULD be flipped by a pause-all
/// (freeze) or resume-all (unfreeze) call with the given scope. Used by
/// the frontend scope picker to show the live count as the user changes scope.
///
/// `target_state = true` means "would-pause" (count currently unfrozen slugs).
/// `target_state = false` means "would-resume" (count currently frozen slugs).
///
/// Phase 7: now reads from the holds projection instead of the `enabled` column.
pub fn count_dadbear_scope(
    conn: &Connection,
    scope: &str,
    scope_value: Option<&str>,
    target_state: bool,
) -> Result<usize> {
    match scope {
        "all" => {
            if target_state {
                // Would-pause: count distinct slugs that do NOT have a 'frozen' hold
                let count: i64 = conn.query_row(
                    "SELECT COUNT(DISTINCT d.slug) FROM pyramid_dadbear_config d
                     WHERE NOT EXISTS (
                         SELECT 1 FROM dadbear_holds_projection h
                         WHERE h.slug = d.slug AND h.hold = 'frozen'
                     )",
                    [],
                    |r| r.get(0),
                )?;
                Ok(count as usize)
            } else {
                // Would-resume: count distinct slugs that HAVE a 'frozen' hold
                let count: i64 = conn.query_row(
                    "SELECT COUNT(DISTINCT slug) FROM dadbear_holds_projection WHERE hold = 'frozen'",
                    [],
                    |r| r.get(0),
                )?;
                Ok(count as usize)
            }
        }
        "folder" => {
            let folder = match scope_value {
                Some(f) if !f.is_empty() => normalize_dadbear_folder(f),
                _ => return Ok(0),
            };
            if target_state {
                // Would-pause: configs under folder without a 'frozen' hold
                let count: i64 = conn.query_row(
                    "SELECT COUNT(DISTINCT d.slug) FROM pyramid_dadbear_config d
                     WHERE (d.source_path = ?1 OR d.source_path LIKE ?1 || '/%')
                       AND NOT EXISTS (
                           SELECT 1 FROM dadbear_holds_projection h
                           WHERE h.slug = d.slug AND h.hold = 'frozen'
                       )",
                    rusqlite::params![folder],
                    |r| r.get(0),
                )?;
                Ok(count as usize)
            } else {
                // Would-resume: configs under folder with a 'frozen' hold
                let count: i64 = conn.query_row(
                    "SELECT COUNT(DISTINCT d.slug) FROM pyramid_dadbear_config d
                     WHERE (d.source_path = ?1 OR d.source_path LIKE ?1 || '/%')
                       AND EXISTS (
                           SELECT 1 FROM dadbear_holds_projection h
                           WHERE h.slug = d.slug AND h.hold = 'frozen'
                       )",
                    rusqlite::params![folder],
                    |r| r.get(0),
                )?;
                Ok(count as usize)
            }
        }
        // "circle" is deferred — no schema to query. Return 0 so the
        // IPC layer can show "Circle scoping not yet available".
        "circle" => Ok(0),
        _ => Ok(0),
    }
}

/// Phase 13: Cost-rollup helper. Aggregates `pyramid_cost_log` across
/// all slugs within the given ISO date range, grouped by
/// `(slug, provider, operation)`. Callers pivot the result client-side
/// into per-pyramid / per-provider / per-operation views.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CostRollupBucket {
    pub slug: String,
    pub provider: Option<String>,
    pub operation: String,
    pub estimated: f64,
    pub actual: f64,
    pub call_count: i64,
}

pub fn cost_rollup(
    conn: &Connection,
    from_iso: &str,
    to_iso: &str,
) -> Result<Vec<CostRollupBucket>> {
    let mut stmt = conn.prepare(
        "SELECT
            slug,
            provider_id,
            operation,
            COALESCE(SUM(COALESCE(estimated_cost_usd, estimated_cost)), 0) AS estimated,
            COALESCE(SUM(COALESCE(broadcast_cost_usd, actual_cost, 0)), 0) AS actual,
            COUNT(*) AS call_count
         FROM pyramid_cost_log
         WHERE created_at >= ?1 AND created_at < ?2
         GROUP BY slug, provider_id, operation
         ORDER BY slug, operation",
    )?;
    let rows = stmt.query_map(rusqlite::params![from_iso, to_iso], |row| {
        Ok(CostRollupBucket {
            slug: row.get(0)?,
            provider: row.get(1)?,
            operation: row.get(2)?,
            estimated: row.get::<_, f64>(3).unwrap_or(0.0),
            actual: row.get::<_, f64>(4).unwrap_or(0.0),
            call_count: row.get(5)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

// ── Phase 15: DADBEAR Oversight Page helpers (test-only) ─────────────────────
//
// `build_dadbear_overview_rows` has no production callers — the v2 overview
// handler reads from canonical work-item tables. Retained under #[cfg(test)]
// so existing coverage tests keep compiling.

/// Phase 15: per-slug row for the DADBEAR Oversight page (test-only).
#[cfg(test)]
#[derive(Debug, Clone, serde::Serialize)]
pub struct DadbearOverviewRowDb {
    pub slug: String,
    pub config_ids: Vec<i64>,
    pub enabled: bool,
    pub scan_interval_secs: i64,
    pub debounce_secs: i64,
    pub last_scan_at: Option<String>,
    pub pending_mutations_count: i64,
    pub deferred_questions_count: i64,
    pub demand_signals_24h: i64,
    pub cost_24h_estimated_usd: f64,
    pub cost_24h_actual_usd: f64,
    pub cost_reconciliation_status: String,
    pub recent_manifest_count: i64,
    pub frozen: bool,
    pub breaker_tripped: bool,
    pub auto_update: bool,
}

/// Phase 15: build every per-slug overview row (test-only, no production callers).
#[cfg(test)]
pub fn build_dadbear_overview_rows(
    conn: &Connection,
) -> Result<Vec<DadbearOverviewRowDb>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, enabled, scan_interval_secs, debounce_secs, last_scan_at
         FROM pyramid_dadbear_config
         ORDER BY slug ASC, id ASC",
    )?;
    #[allow(clippy::type_complexity)]
    let raw: Vec<(i64, String, bool, i64, i64, Option<String>)> = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i32>(2)? != 0,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);

    #[derive(Default)]
    struct SlugBucket {
        config_ids: Vec<i64>,
        enabled_any: bool,
        min_scan_interval: Option<i64>,
        min_debounce: Option<i64>,
        latest_scan: Option<String>,
    }
    let mut buckets: std::collections::BTreeMap<String, SlugBucket> =
        std::collections::BTreeMap::new();
    for (id, slug, enabled, scan_iv, debounce, last_scan_at) in raw {
        let bucket = buckets.entry(slug).or_default();
        bucket.config_ids.push(id);
        if enabled {
            bucket.enabled_any = true;
        }
        bucket.min_scan_interval = Some(
            bucket
                .min_scan_interval
                .map_or(scan_iv, |cur| cur.min(scan_iv)),
        );
        bucket.min_debounce = Some(
            bucket
                .min_debounce
                .map_or(debounce, |cur| cur.min(debounce)),
        );
        if let Some(ts) = last_scan_at {
            bucket.latest_scan = match &bucket.latest_scan {
                Some(cur) if cur >= &ts => Some(cur.clone()),
                _ => Some(ts),
            };
        }
    }

    let mut rows: Vec<DadbearOverviewRowDb> = Vec::with_capacity(buckets.len());
    for (slug, bucket) in buckets {
        let pending_mutations_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dadbear_observation_events
                 WHERE slug = ?1
                   AND id > COALESCE(
                       (SELECT last_bridge_observation_id FROM pyramid_build_metadata WHERE slug = ?1),
                       0
                   )",
                rusqlite::params![slug],
                |r| r.get(0),
            )
            .unwrap_or(0);

        let deferred_questions_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_deferred_questions WHERE slug = ?1",
                rusqlite::params![slug],
                |r| r.get(0),
            )
            .unwrap_or(0);

        let demand_signals_24h: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_demand_signals
                 WHERE slug = ?1 AND created_at > datetime('now', '-24 hours')",
                rusqlite::params![slug],
                |r| r.get(0),
            )
            .unwrap_or(0);

        let (cost_24h_estimated_usd, cost_24h_actual_usd): (f64, f64) = conn
            .query_row(
                "SELECT
                    COALESCE(SUM(COALESCE(estimated_cost_usd, estimated_cost, 0)), 0),
                    COALESCE(SUM(COALESCE(broadcast_cost_usd, actual_cost, 0)), 0)
                 FROM pyramid_cost_log
                 WHERE slug = ?1 AND created_at > datetime('now', '-24 hours')",
                rusqlite::params![slug],
                |r| Ok((r.get::<_, f64>(0)?, r.get::<_, f64>(1)?)),
            )
            .unwrap_or((0.0, 0.0));

        // Severity-ordered reconciliation aggregation.
        let worst_discrepancy: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_cost_log
                 WHERE slug = ?1
                   AND reconciliation_status = 'discrepancy'
                   AND created_at > datetime('now', '-24 hours')",
                rusqlite::params![slug],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let worst_missing: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_cost_log
                 WHERE slug = ?1
                   AND reconciliation_status = 'broadcast_missing'
                   AND created_at > datetime('now', '-24 hours')",
                rusqlite::params![slug],
                |r| r.get(0),
            )
            .unwrap_or(0);
        // Rows are "pending confirmation" when a broadcast was
        // expected but has not yet landed. The `reconciliation_status`
        // stays at `'synchronous'` even AFTER a healthy broadcast
        // arrives (see `record_broadcast_confirmation` — it only
        // flips on discrepancy; success leaves the status alone and
        // stamps `broadcast_confirmed_at` instead). So the right
        // signal for "still waiting" is `broadcast_confirmed_at IS
        // NULL`, not the status value alone. `'synchronous_local'`
        // rows are zero-cost/local calls with nothing to reconcile —
        // they never pend.
        let pending: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_cost_log
                 WHERE slug = ?1
                   AND broadcast_confirmed_at IS NULL
                   AND reconciliation_status != 'synchronous_local'
                   AND reconciliation_status != 'broadcast_missing'
                   AND reconciliation_status != 'discrepancy'
                   AND reconciliation_status != 'broadcast'
                   AND created_at > datetime('now', '-24 hours')",
                rusqlite::params![slug],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let total_rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_cost_log
                 WHERE slug = ?1
                   AND created_at > datetime('now', '-24 hours')",
                rusqlite::params![slug],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let cost_reconciliation_status = if worst_discrepancy > 0 {
            "discrepancy".to_string()
        } else if worst_missing > 0 {
            "broadcast_missing".to_string()
        } else if total_rows == 0 {
            "healthy".to_string()
        } else if pending > 0 {
            "pending".to_string()
        } else {
            "healthy".to_string()
        };

        let recent_manifest_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_change_manifests
                 WHERE slug = ?1 AND applied_at > datetime('now', '-24 hours')",
                rusqlite::params![slug],
                |r| r.get(0),
            )
            .unwrap_or(0);

        let frozen: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM dadbear_holds_projection WHERE slug = ?1 AND hold = 'frozen')",
            rusqlite::params![slug],
            |row| row.get(0),
        ).unwrap_or(false);
        let breaker_tripped: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM dadbear_holds_projection WHERE slug = ?1 AND hold = 'breaker')",
            rusqlite::params![slug],
            |row| row.get(0),
        ).unwrap_or(false);
        let auto_update_enabled: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pyramid_dadbear_config WHERE slug = ?1)",
            rusqlite::params![slug],
            |row| row.get(0),
        ).unwrap_or(false);

        rows.push(DadbearOverviewRowDb {
            slug,
            config_ids: bucket.config_ids,
            enabled: bucket.enabled_any,
            scan_interval_secs: bucket.min_scan_interval.unwrap_or(10),
            debounce_secs: bucket.min_debounce.unwrap_or(30),
            last_scan_at: bucket.latest_scan,
            pending_mutations_count,
            deferred_questions_count,
            demand_signals_24h,
            cost_24h_estimated_usd,
            cost_24h_actual_usd,
            cost_reconciliation_status,
            recent_manifest_count,
            frozen,
            breaker_tripped,
            auto_update: auto_update_enabled,
        });
    }

    Ok(rows)
}

/// Phase 13: active-builds query. Returns one summary row per
/// active (running/idle) slug. The current schema doesn't have a
/// `pyramid_build_runs` lifecycle table — the spec assumed one —
/// so we derive the active set from `active_build` runtime state
/// and supplement it with cost / cache data computed from
/// `pyramid_step_cache` and `pyramid_cost_log`.
///
/// The runtime state is passed as a parameter because it lives in
/// `PyramidState.active_build` (a `RwLock<HashMap<_, BuildHandle>>`)
/// rather than in the DB. The caller (IPC handler) reads that map
/// and hands us the slugs that currently have live `BuildHandle`s.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ActiveBuildRow {
    pub slug: String,
    pub build_id: String,
    pub status: String,
    pub started_at: String,
    pub completed_steps: i64,
    pub total_steps: i64,
    pub current_step: Option<String>,
    pub cost_so_far_usd: f64,
    pub cache_hit_rate: f64,
}

/// Build a single `ActiveBuildRow` for the given slug.
///
/// `completed_steps` / `total_steps` are the caller's responsibility — they
/// come from the live `BuildHandle`'s progress channel (main.rs:3917-3922
/// maintains `handle.status.progress` via `progress_rx.recv()`). The
/// pyramid surface drawer reads the same source via
/// `pyramid_build_progress_v2`, which is why it shows accurate per-step
/// counts (e.g. "source_extract 7/21") while this function should just
/// mirror the same numbers. An earlier revision tried to aggregate
/// `dadbear_work_items` directly; that was wrong because DADBEAR work
/// items are created per-stage and reset between stages, so total
/// collapsed to the current stage's fan-out rather than the cumulative
/// build progress.
///
/// Cost and cache hit rate still come from `pyramid_step_cache`, which
/// the chain executor writes for every step regardless of whether the
/// step was compiled through DADBEAR. We aggregate by slug only —
/// `BuildHandle` has no durable build_id and the placeholder semantics
/// in the legacy code were fragile. Slug-scoped aggregation is
/// "cost so far on this pyramid," which is what the UI wants.
pub fn build_active_build_summary(
    conn: &Connection,
    slug: &str,
    build_id: &str,
    status: &str,
    started_at: &str,
    current_step: Option<&str>,
    completed_steps: i64,
    total_steps: i64,
) -> Result<ActiveBuildRow> {
    let cost_so_far_usd: f64 = conn
        .query_row(
            "SELECT COALESCE(SUM(cost_usd), 0.0) FROM pyramid_step_cache
              WHERE slug = ?1",
            rusqlite::params![slug],
            |row| row.get(0),
        )
        .unwrap_or(0.0);

    let (hits, total): (i64, i64) = conn
        .query_row(
            "SELECT
                COALESCE(SUM(CASE WHEN force_fresh = 0 THEN 1 ELSE 0 END), 0),
                COUNT(*)
             FROM pyramid_step_cache
             WHERE slug = ?1",
            rusqlite::params![slug],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap_or((0, 0));

    let cache_hit_rate = if total > 0 {
        (hits as f64) / (total as f64)
    } else {
        0.0
    };

    Ok(ActiveBuildRow {
        slug: slug.to_string(),
        build_id: build_id.to_string(),
        status: status.to_string(),
        started_at: started_at.to_string(),
        completed_steps,
        total_steps,
        current_step: current_step.map(|s| s.to_string()),
        cost_so_far_usd,
        cache_hit_rate,
    })
}

/// Update the last_scan_at timestamp for a DADBEAR config row.
pub fn touch_dadbear_last_scan(conn: &Connection, config_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE pyramid_dadbear_config SET last_scan_at = datetime('now'), updated_at = datetime('now') WHERE id = ?1",
        rusqlite::params![config_id],
    )?;
    Ok(())
}

/// Get a specific DADBEAR config by (slug, source_path).
pub fn get_dadbear_config(
    conn: &Connection,
    slug: &str,
    source_path: &str,
) -> Result<Option<DadbearWatchConfig>> {
    let sql = format!(
        "SELECT {DADBEAR_CONFIG_COLUMNS} FROM pyramid_dadbear_config
         WHERE slug = ?1 AND source_path = ?2"
    );
    let result = conn
        .query_row(&sql, rusqlite::params![slug, source_path], parse_dadbear_config)
        .optional()?;
    Ok(result)
}

/// Delete a specific DADBEAR config by (slug, source_path).
pub fn delete_dadbear_config(conn: &Connection, slug: &str, source_path: &str) -> Result<bool> {
    let count = conn.execute(
        "DELETE FROM pyramid_dadbear_config WHERE slug = ?1 AND source_path = ?2",
        rusqlite::params![slug, source_path],
    )?;
    Ok(count > 0)
}

// ── WS-VINE-UNIFY: Vine composition DB helpers ────────────────────────────────

/// Column list for SELECT queries on pyramid_vine_compositions.
const VINE_COMP_COLUMNS: &str =
    "id, vine_slug, bedrock_slug, position, bedrock_apex_node_id, status, child_type, created_at, updated_at";

/// Parse a row from `pyramid_vine_compositions` into a `VineComposition`.
/// Phase 16: `child_type` is COALESCE'd to `'bedrock'` at query time so rows
/// from pre-migration databases always yield a valid value.
fn parse_vine_composition(row: &rusqlite::Row<'_>) -> rusqlite::Result<VineComposition> {
    Ok(VineComposition {
        id: row.get(0)?,
        vine_slug: row.get(1)?,
        bedrock_slug: row.get(2)?,
        position: row.get(3)?,
        bedrock_apex_node_id: row.get(4)?,
        status: row.get(5)?,
        child_type: row
            .get::<_, Option<String>>(6)?
            .unwrap_or_else(|| "bedrock".to_string()),
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

/// Add a bedrock pyramid to a vine at a given position. Inserts or updates
/// on conflict (reactivates a previously removed bedrock).
///
/// Phase 16 retains this signature as a thin alias for
/// `insert_vine_composition(conn, vine_slug, bedrock_slug, position, "bedrock")`
/// so Phase 2 / Phase 13 callers continue to work unchanged.
pub fn add_bedrock_to_vine(
    conn: &Connection,
    vine_slug: &str,
    bedrock_slug: &str,
    position: i32,
) -> Result<()> {
    insert_vine_composition(conn, vine_slug, bedrock_slug, position, "bedrock")
}

/// Phase 16: add a child (bedrock OR sub-vine) to a vine at a given position.
///
/// `child_type` must be either `"bedrock"` or `"vine"`. On conflict with an
/// existing row for the same (vine_slug, child_slug) pair, this reactivates
/// the row and updates position + child_type.
pub fn insert_vine_composition(
    conn: &Connection,
    vine_slug: &str,
    child_slug: &str,
    position: i32,
    child_type: &str,
) -> Result<()> {
    if child_type != "bedrock" && child_type != "vine" {
        return Err(anyhow::anyhow!(
            "invalid child_type '{}': must be 'bedrock' or 'vine'",
            child_type
        ));
    }
    conn.execute(
        "INSERT INTO pyramid_vine_compositions (vine_slug, bedrock_slug, position, status, child_type)
         VALUES (?1, ?2, ?3, 'active', ?4)
         ON CONFLICT(vine_slug, bedrock_slug) DO UPDATE SET
            position = excluded.position,
            status = 'active',
            child_type = excluded.child_type,
            updated_at = datetime('now')",
        rusqlite::params![vine_slug, child_slug, position, child_type],
    )?;
    Ok(())
}

/// Get all bedrocks for a vine, ordered by position ascending. Only returns
/// active entries by default.
///
/// Phase 16: this returns all children (including vine children) because the
/// existing callers (vine composition propagation) treat bedrocks and sub-vines
/// the same way at this layer. If a caller needs only bedrock-typed rows, it
/// can filter on `composition.is_vine_child()`. A strict bedrock-only variant
/// can be added via `list_vine_compositions` with explicit filtering.
pub fn get_vine_bedrocks(conn: &Connection, vine_slug: &str) -> Result<Vec<VineComposition>> {
    list_vine_compositions(conn, vine_slug)
}

/// Phase 16: list all children (bedrock + vine) for a vine, ordered by
/// position. Active entries only.
pub fn list_vine_compositions(
    conn: &Connection,
    vine_slug: &str,
) -> Result<Vec<VineComposition>> {
    let sql = format!(
        "SELECT {VINE_COMP_COLUMNS} FROM pyramid_vine_compositions
         WHERE vine_slug = ?1 AND status = 'active'
         ORDER BY position ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params![vine_slug], parse_vine_composition)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Update the apex node id for a specific bedrock within a vine.
///
/// Phase 16 retains this as a thin alias for `update_child_apex` so
/// existing Phase 2 callers continue to work unchanged. New code should
/// prefer `update_child_apex` which reads more naturally for vine children.
pub fn update_bedrock_apex(
    conn: &Connection,
    vine_slug: &str,
    bedrock_slug: &str,
    apex_node_id: &str,
) -> Result<()> {
    update_child_apex(conn, vine_slug, bedrock_slug, apex_node_id)
}

/// Phase 16: update the apex node id for a specific child (bedrock OR
/// sub-vine) within a vine.
pub fn update_child_apex(
    conn: &Connection,
    vine_slug: &str,
    child_slug: &str,
    apex_node_id: &str,
) -> Result<()> {
    let count = conn.execute(
        "UPDATE pyramid_vine_compositions
         SET bedrock_apex_node_id = ?3, updated_at = datetime('now')
         WHERE vine_slug = ?1 AND bedrock_slug = ?2",
        rusqlite::params![vine_slug, child_slug, apex_node_id],
    )?;
    if count == 0 {
        return Err(anyhow::anyhow!(
            "No vine composition found for vine={vine_slug}, child={child_slug}"
        ));
    }
    Ok(())
}

/// Soft-remove a bedrock from a vine (sets status = 'removed').
pub fn remove_bedrock_from_vine(
    conn: &Connection,
    vine_slug: &str,
    bedrock_slug: &str,
) -> Result<()> {
    let count = conn.execute(
        "UPDATE pyramid_vine_compositions
         SET status = 'removed', updated_at = datetime('now')
         WHERE vine_slug = ?1 AND bedrock_slug = ?2",
        rusqlite::params![vine_slug, bedrock_slug],
    )?;
    if count == 0 {
        return Err(anyhow::anyhow!(
            "No vine composition found for vine={vine_slug}, bedrock={bedrock_slug}"
        ));
    }
    Ok(())
}

/// Get all vine slugs that include a given bedrock (active compositions only).
///
/// Phase 16 retains this signature; new code should prefer
/// `get_vines_for_child` which covers sub-vine parents too. Because the
/// bedrock_slug column is reused as child_slug, this also returns vines that
/// include the slug as a sub-vine child — callers that need bedrock-only
/// parents should filter on `child_type`.
pub fn get_vines_for_bedrock(conn: &Connection, bedrock_slug: &str) -> Result<Vec<String>> {
    get_vines_for_child(conn, bedrock_slug)
}

/// Phase 16: get all vine slugs that include a given slug as a child — bedrock
/// OR sub-vine. Active compositions only.
pub fn get_vines_for_child(conn: &Connection, child_slug: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT vine_slug FROM pyramid_vine_compositions
         WHERE bedrock_slug = ?1 AND status = 'active'",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![child_slug], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(rows)
}

/// Phase 16: get ALL ancestor vines (direct parents + grandparents + ...)
/// for a given slug via iterative BFS. Includes a cycle guard so a vine
/// referencing itself directly or transitively cannot cause infinite
/// recursion. Includes a max-depth safety net of 32 levels so a very deep
/// hierarchy eventually terminates cleanly; deeper hierarchies log a warning
/// and return the ancestors walked so far.
///
/// Ordering is BFS — nearer ancestors come before distant ancestors. The
/// starting `child_slug` is NOT included in the result.
pub fn get_parent_vines_recursive(
    conn: &Connection,
    child_slug: &str,
) -> Result<Vec<String>> {
    const MAX_DEPTH: usize = 32;
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut order: Vec<String> = Vec::new();
    let mut frontier: std::collections::VecDeque<String> =
        std::collections::VecDeque::new();

    visited.insert(child_slug.to_string());
    frontier.push_back(child_slug.to_string());

    let mut depth = 0usize;
    while let Some(current) = frontier.pop_front() {
        depth = depth.saturating_add(1);
        if depth > MAX_DEPTH * 8 {
            // Very defensive upper bound on total iterations to shield against
            // pathological fan-out even inside the cycle guard.
            tracing::warn!(
                child_slug,
                visited_count = visited.len(),
                "get_parent_vines_recursive: hit iteration cap, returning partial walk"
            );
            break;
        }
        let parents = get_vines_for_child(conn, &current)?;
        for parent in parents {
            if visited.insert(parent.clone()) {
                order.push(parent.clone());
                frontier.push_back(parent);
            }
        }
        // Max-depth guard: BFS breadth expands frontier entries; the
        // iteration cap above bounds total work, but we also shortcut out
        // when the known ancestor set gets unreasonably large. 2*MAX_DEPTH
        // vines in a single ancestor chain indicates either a runaway graph
        // or a misuse.
        if order.len() >= MAX_DEPTH * 2 {
            tracing::warn!(
                child_slug,
                ancestors = order.len(),
                "get_parent_vines_recursive: hit ancestor count cap, returning partial walk"
            );
            break;
        }
    }

    Ok(order)
}

// ── WS-DEMAND-GEN (Phase 3): Demand generation job DB helpers ────────────────

use super::types::DemandGenJob;

fn map_demand_gen_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DemandGenJob> {
    let sub_q_json: Option<String> = row.get(4)?;
    let result_json: Option<String> = row.get(6)?;
    Ok(DemandGenJob {
        id: row.get(0)?,
        job_id: row.get(1)?,
        slug: row.get(2)?,
        question: row.get(3)?,
        sub_questions: sub_q_json
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default(),
        status: row.get(5)?,
        result_node_ids: result_json
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default(),
        error_message: row.get(7)?,
        requested_at: row.get(8)?,
        started_at: row.get(9)?,
        completed_at: row.get(10)?,
    })
}

const DEMAND_GEN_COLUMNS: &str =
    "id, job_id, slug, question, sub_questions, status, result_node_ids, error_message, requested_at, started_at, completed_at";

/// Insert a new demand-gen job. The caller must provide the `job_id` (UUID)
/// and the `question`. Sub-questions are optional at creation time and can
/// be populated when the executor decomposes the question.
pub fn create_demand_gen_job(conn: &Connection, job: &DemandGenJob) -> Result<()> {
    let sub_q = serde_json::to_string(&job.sub_questions)?;
    conn.execute(
        "INSERT INTO pyramid_demand_gen_jobs (job_id, slug, question, sub_questions, status, requested_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            job.job_id,
            job.slug,
            job.question,
            sub_q,
            job.status,
            job.requested_at,
        ],
    )?;
    Ok(())
}

/// Fetch a single demand-gen job by job_id.
pub fn get_demand_gen_job(conn: &Connection, job_id: &str) -> Result<Option<DemandGenJob>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {DEMAND_GEN_COLUMNS} FROM pyramid_demand_gen_jobs WHERE job_id = ?1"
    ))?;
    let mut rows = stmt.query_map(rusqlite::params![job_id], map_demand_gen_row)?;
    match rows.next() {
        Some(Ok(job)) => Ok(Some(job)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

/// Transition a demand-gen job to "running" and set started_at.
pub fn mark_demand_gen_running(conn: &Connection, job_id: &str) -> Result<()> {
    let count = conn.execute(
        "UPDATE pyramid_demand_gen_jobs
         SET status = 'running', started_at = datetime('now')
         WHERE job_id = ?1 AND status = 'queued'",
        rusqlite::params![job_id],
    )?;
    if count == 0 {
        return Err(anyhow::anyhow!(
            "demand-gen job {job_id} not found or not in queued state"
        ));
    }
    Ok(())
}

/// Mark a demand-gen job complete with the generated node IDs.
pub fn mark_demand_gen_complete(conn: &Connection, job_id: &str, node_ids: &[String]) -> Result<()> {
    let ids_json = serde_json::to_string(node_ids)?;
    let count = conn.execute(
        "UPDATE pyramid_demand_gen_jobs
         SET status = 'complete', result_node_ids = ?1, completed_at = datetime('now')
         WHERE job_id = ?2 AND status = 'running'",
        rusqlite::params![ids_json, job_id],
    )?;
    if count == 0 {
        return Err(anyhow::anyhow!(
            "demand-gen job {job_id} not found or not in running state"
        ));
    }
    Ok(())
}

/// Mark a demand-gen job as failed with an error message.
pub fn mark_demand_gen_failed(conn: &Connection, job_id: &str, error: &str) -> Result<()> {
    let count = conn.execute(
        "UPDATE pyramid_demand_gen_jobs
         SET status = 'failed', error_message = ?1, completed_at = datetime('now')
         WHERE job_id = ?2 AND (status = 'queued' OR status = 'running')",
        rusqlite::params![error, job_id],
    )?;
    if count == 0 {
        return Err(anyhow::anyhow!(
            "demand-gen job {job_id} not found or already in terminal state"
        ));
    }
    Ok(())
}

/// List pending (queued or running) demand-gen jobs for a slug.
pub fn get_pending_demand_gen_jobs(conn: &Connection, slug: &str) -> Result<Vec<DemandGenJob>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {DEMAND_GEN_COLUMNS} FROM pyramid_demand_gen_jobs
         WHERE slug = ?1 AND status IN ('queued', 'running')
         ORDER BY requested_at ASC"
    ))?;
    let rows = stmt
        .query_map(rusqlite::params![slug], map_demand_gen_row)?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

/// List recent demand-gen jobs for a slug (all statuses, most recent first).
pub fn list_demand_gen_jobs(conn: &Connection, slug: &str, limit: i64) -> Result<Vec<DemandGenJob>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {DEMAND_GEN_COLUMNS} FROM pyramid_demand_gen_jobs
         WHERE slug = ?1
         ORDER BY requested_at DESC
         LIMIT ?2"
    ))?;
    let rows = stmt
        .query_map(rusqlite::params![slug, limit], map_demand_gen_row)?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

/// Compact positions for active bedrocks in a vine after a removal.
/// Reassigns positions 0, 1, 2, ... in current order.
pub fn reorder_vine_bedrocks(conn: &Connection, vine_slug: &str) -> Result<()> {
    // Read current active bedrocks in position order
    let bedrocks: Vec<(i64, String)> = {
        let mut stmt = conn.prepare(
            "SELECT id, bedrock_slug FROM pyramid_vine_compositions
             WHERE vine_slug = ?1 AND status = 'active'
             ORDER BY position ASC",
        )?;
        let mapped = stmt.query_map(rusqlite::params![vine_slug], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        let collected = mapped.collect::<rusqlite::Result<Vec<_>>>()?;
        collected
    };
    // Reassign sequential positions
    for (new_pos, (row_id, _)) in bedrocks.iter().enumerate() {
        conn.execute(
            "UPDATE pyramid_vine_compositions SET position = ?1, updated_at = datetime('now') WHERE id = ?2",
            rusqlite::params![new_pos as i32, row_id],
        )?;
    }
    Ok(())
}

// ── WS-CHAIN-PUBLISH: Chain publication DB helpers ─────────────────────────────

/// Column list for SELECT queries on pyramid_chain_publications.
const CHAIN_PUB_COLUMNS: &str =
    "id, chain_id, version, wire_handle_path, wire_uuid, published_at, description, author, forked_from, status, created_at, updated_at";

/// Parse a row from `pyramid_chain_publications` into a `ChainPublication`.
fn map_chain_publication_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<super::types::ChainPublication> {
    Ok(super::types::ChainPublication {
        id: row.get(0)?,
        chain_id: row.get(1)?,
        version: row.get(2)?,
        wire_handle_path: row.get(3)?,
        wire_uuid: row.get(4)?,
        published_at: row.get(5)?,
        description: row.get(6)?,
        author: row.get(7)?,
        forked_from: row.get(8)?,
        status: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

/// Save (insert or update) a chain publication record.
pub fn save_chain_publication(conn: &Connection, pub_record: &super::types::ChainPublication) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_chain_publications
            (chain_id, version, wire_handle_path, wire_uuid, published_at, description, author, forked_from, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(chain_id, version) DO UPDATE SET
            wire_handle_path = excluded.wire_handle_path,
            wire_uuid = excluded.wire_uuid,
            published_at = excluded.published_at,
            description = excluded.description,
            author = excluded.author,
            forked_from = excluded.forked_from,
            status = excluded.status,
            updated_at = datetime('now')",
        rusqlite::params![
            pub_record.chain_id,
            pub_record.version,
            pub_record.wire_handle_path,
            pub_record.wire_uuid,
            pub_record.published_at,
            pub_record.description,
            pub_record.author,
            pub_record.forked_from,
            pub_record.status,
        ],
    )?;
    Ok(())
}

/// Get the latest chain publication for a given chain_id (highest version).
pub fn get_chain_publication(conn: &Connection, chain_id: &str) -> Result<Option<super::types::ChainPublication>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {CHAIN_PUB_COLUMNS} FROM pyramid_chain_publications
         WHERE chain_id = ?1
         ORDER BY version DESC
         LIMIT 1"
    ))?;
    let mut rows = stmt.query_map(rusqlite::params![chain_id], map_chain_publication_row)?;
    match rows.next() {
        Some(Ok(record)) => Ok(Some(record)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

/// Get a specific version of a chain publication.
pub fn get_chain_publication_by_version(
    conn: &Connection,
    chain_id: &str,
    version: i32,
) -> Result<Option<super::types::ChainPublication>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {CHAIN_PUB_COLUMNS} FROM pyramid_chain_publications
         WHERE chain_id = ?1 AND version = ?2"
    ))?;
    let mut rows = stmt.query_map(rusqlite::params![chain_id, version], map_chain_publication_row)?;
    match rows.next() {
        Some(Ok(record)) => Ok(Some(record)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

/// List all chain publications (latest version per chain_id).
pub fn list_chain_publications(conn: &Connection) -> Result<Vec<super::types::ChainPublication>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {CHAIN_PUB_COLUMNS} FROM pyramid_chain_publications cp1
         WHERE version = (
             SELECT MAX(version) FROM pyramid_chain_publications cp2
             WHERE cp2.chain_id = cp1.chain_id
         )
         ORDER BY chain_id ASC"
    ))?;
    let rows = stmt.query_map([], map_chain_publication_row)?;
    let collected = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(collected)
}

/// Mark a chain as published, recording the Wire handle-path and UUID.
pub fn mark_chain_published(
    conn: &Connection,
    chain_id: &str,
    wire_handle_path: &str,
    wire_uuid: &str,
) -> Result<()> {
    let affected = conn.execute(
        "UPDATE pyramid_chain_publications
         SET wire_handle_path = ?2,
             wire_uuid = ?3,
             published_at = datetime('now'),
             status = 'published',
             updated_at = datetime('now')
         WHERE chain_id = ?1 AND version = (
             SELECT MAX(version) FROM pyramid_chain_publications WHERE chain_id = ?1
         )",
        rusqlite::params![chain_id, wire_handle_path, wire_uuid],
    )?;
    if affected == 0 {
        anyhow::bail!("no chain publication found for chain_id '{}'", chain_id);
    }
    Ok(())
}

/// Increment the version for a chain and return the new version number.
/// Creates a new row by copying the latest version's metadata with
/// version = max + 1 and status = 'local'.
pub fn increment_chain_version(conn: &Connection, chain_id: &str) -> Result<i32> {
    let current = get_chain_publication(conn, chain_id)?;
    match current {
        Some(pub_record) => {
            let new_version = pub_record.version + 1;
            conn.execute(
                "INSERT INTO pyramid_chain_publications
                    (chain_id, version, description, author, forked_from, status)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'local')",
                rusqlite::params![
                    chain_id,
                    new_version,
                    pub_record.description,
                    pub_record.author,
                    pub_record.forked_from,
                ],
            )?;
            Ok(new_version)
        }
        None => {
            anyhow::bail!("no chain publication found for chain_id '{}'", chain_id);
        }
    }
}

/// Create a fork record: inserts a new chain publication for `new_chain_id`
/// with version 1, `forked_from` pointing to the source, and status 'local'.
pub fn fork_chain_publication(
    conn: &Connection,
    source_chain_id: &str,
    new_chain_id: &str,
    author: &str,
) -> Result<()> {
    let source = get_chain_publication(conn, source_chain_id)?;
    let description = source.as_ref().and_then(|s| s.description.clone());
    conn.execute(
        "INSERT INTO pyramid_chain_publications
            (chain_id, version, description, author, forked_from, status)
         VALUES (?1, 1, ?2, ?3, ?4, 'local')",
        rusqlite::params![new_chain_id, description, author, source_chain_id],
    )?;
    Ok(())
}

// ── WS-CHAIN-PROPOSAL: Chain proposal DB helpers ─────────────────────────────

/// Column list for SELECT queries on pyramid_chain_proposals.
const CHAIN_PROPOSAL_COLUMNS: &str =
    "id, proposal_id, chain_id, proposer, proposal_type, description, reasoning, patch, status, operator_notes, created_at, reviewed_at";

/// Parse a row from `pyramid_chain_proposals` into a `ChainProposal`.
fn map_chain_proposal_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<super::types::ChainProposal> {
    let patch_str: String = row.get(7)?;
    let patch: serde_json::Value = serde_json::from_str(&patch_str)
        .unwrap_or(serde_json::Value::Null);
    Ok(super::types::ChainProposal {
        id: row.get(0)?,
        proposal_id: row.get(1)?,
        chain_id: row.get(2)?,
        proposer: row.get(3)?,
        proposal_type: row.get(4)?,
        description: row.get(5)?,
        reasoning: row.get(6)?,
        patch,
        status: row.get(8)?,
        operator_notes: row.get(9)?,
        created_at: row.get(10)?,
        reviewed_at: row.get(11)?,
    })
}

/// Insert a new chain proposal. Returns the database row ID.
pub fn create_chain_proposal(conn: &Connection, proposal: &super::types::ChainProposal) -> Result<i64> {
    let patch_str = serde_json::to_string(&proposal.patch)
        .unwrap_or_else(|_| "{}".to_string());
    conn.execute(
        "INSERT INTO pyramid_chain_proposals
            (proposal_id, chain_id, proposer, proposal_type, description, reasoning, patch, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            proposal.proposal_id,
            proposal.chain_id,
            proposal.proposer,
            proposal.proposal_type,
            proposal.description,
            proposal.reasoning,
            patch_str,
            proposal.status,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Get a single chain proposal by its text proposal_id.
pub fn get_chain_proposal(conn: &Connection, proposal_id: &str) -> Result<Option<super::types::ChainProposal>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {CHAIN_PROPOSAL_COLUMNS} FROM pyramid_chain_proposals
         WHERE proposal_id = ?1"
    ))?;
    let mut rows = stmt.query_map(rusqlite::params![proposal_id], map_chain_proposal_row)?;
    match rows.next() {
        Some(Ok(record)) => Ok(Some(record)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

/// List chain proposals with optional filters on chain_id and status.
pub fn list_chain_proposals(
    conn: &Connection,
    chain_id: Option<&str>,
    status: Option<&str>,
) -> Result<Vec<super::types::ChainProposal>> {
    let mut where_clauses = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut idx = 1;

    if let Some(cid) = chain_id {
        where_clauses.push(format!("chain_id = ?{idx}"));
        params.push(Box::new(cid.to_string()));
        idx += 1;
    }
    if let Some(st) = status {
        where_clauses.push(format!("status = ?{idx}"));
        params.push(Box::new(st.to_string()));
    }

    let where_sql = if where_clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", where_clauses.join(" AND "))
    };

    let sql = format!(
        "SELECT {CHAIN_PROPOSAL_COLUMNS} FROM pyramid_chain_proposals{where_sql} ORDER BY created_at DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let rows = stmt.query_map(param_refs.as_slice(), map_chain_proposal_row)?;
    let collected = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(collected)
}

/// Accept a chain proposal: set status to 'accepted' and reviewed_at to now.
pub fn accept_chain_proposal(conn: &Connection, proposal_id: &str, operator_notes: Option<&str>) -> Result<()> {
    let affected = conn.execute(
        "UPDATE pyramid_chain_proposals
         SET status = 'accepted', operator_notes = ?2, reviewed_at = datetime('now')
         WHERE proposal_id = ?1 AND status = 'pending'",
        rusqlite::params![proposal_id, operator_notes],
    )?;
    if affected == 0 {
        // Check if it exists at all
        let exists: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM pyramid_chain_proposals WHERE proposal_id = ?1",
            rusqlite::params![proposal_id],
            |row| row.get(0),
        )?;
        if !exists {
            anyhow::bail!("chain proposal '{}' not found", proposal_id);
        }
        // Already reviewed — not an error, but nothing changed
    }
    Ok(())
}

/// Reject a chain proposal: set status to 'rejected' and reviewed_at to now.
/// Idempotent: re-rejecting an already-rejected proposal is a no-op success.
pub fn reject_chain_proposal(conn: &Connection, proposal_id: &str, operator_notes: Option<&str>) -> Result<()> {
    let affected = conn.execute(
        "UPDATE pyramid_chain_proposals
         SET status = 'rejected', operator_notes = COALESCE(?2, operator_notes), reviewed_at = COALESCE(reviewed_at, datetime('now'))
         WHERE proposal_id = ?1 AND status IN ('pending', 'rejected')",
        rusqlite::params![proposal_id, operator_notes],
    )?;
    if affected == 0 {
        let exists: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM pyramid_chain_proposals WHERE proposal_id = ?1",
            rusqlite::params![proposal_id],
            |row| row.get(0),
        )?;
        if !exists {
            anyhow::bail!("chain proposal '{}' not found", proposal_id);
        }
        // Already in a non-pending/non-rejected state — accepted or deferred; no-op
    }
    Ok(())
}

/// Defer a chain proposal: set status to 'deferred' and reviewed_at to now.
pub fn defer_chain_proposal(conn: &Connection, proposal_id: &str, operator_notes: Option<&str>) -> Result<()> {
    let affected = conn.execute(
        "UPDATE pyramid_chain_proposals
         SET status = 'deferred', operator_notes = ?2, reviewed_at = datetime('now')
         WHERE proposal_id = ?1 AND status = 'pending'",
        rusqlite::params![proposal_id, operator_notes],
    )?;
    if affected == 0 {
        let exists: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM pyramid_chain_proposals WHERE proposal_id = ?1",
            rusqlite::params![proposal_id],
            |row| row.get(0),
        )?;
        if !exists {
            anyhow::bail!("chain proposal '{}' not found", proposal_id);
        }
    }
    Ok(())
}

// ── Phase 3: Provider registry CRUD helpers ──────────────────────────────────
//
// These helpers back `pyramid::provider::ProviderRegistry` and the IPC
// commands in `main.rs`. All reads return domain types from
// `pyramid::provider`. All writes upsert and bump `updated_at`.

use super::provider::{Provider, ProviderType, StepOverride, TierRoutingEntry};

fn provider_from_row(row: &rusqlite::Row) -> rusqlite::Result<Provider> {
    let provider_type_str: String = row.get("provider_type")?;
    let provider_type = ProviderType::from_str(&provider_type_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())),
        )
    })?;
    let auto_detect: i64 = row.get("auto_detect_context")?;
    let supports_broadcast: i64 = row.get("supports_broadcast")?;
    let enabled: i64 = row.get("enabled")?;
    Ok(Provider {
        id: row.get("id")?,
        display_name: row.get("display_name")?,
        provider_type,
        base_url: row.get("base_url")?,
        api_key_ref: row.get("api_key_ref")?,
        auto_detect_context: auto_detect != 0,
        supports_broadcast: supports_broadcast != 0,
        broadcast_config_json: row.get("broadcast_config_json")?,
        config_json: row.get("config_json")?,
        enabled: enabled != 0,
    })
}

/// Get a single provider row by id.
pub fn get_provider(conn: &Connection, id: &str) -> Result<Option<Provider>> {
    let mut stmt = conn.prepare(
        "SELECT id, display_name, provider_type, base_url, api_key_ref,
                auto_detect_context, supports_broadcast, broadcast_config_json,
                config_json, enabled
         FROM pyramid_providers
         WHERE id = ?1",
    )?;
    let row = stmt
        .query_row(rusqlite::params![id], provider_from_row)
        .optional()
        .context("get_provider query_row")?;
    Ok(row)
}

/// List every provider row in display-name order.
pub fn list_providers(conn: &Connection) -> Result<Vec<Provider>> {
    let mut stmt = conn.prepare(
        "SELECT id, display_name, provider_type, base_url, api_key_ref,
                auto_detect_context, supports_broadcast, broadcast_config_json,
                config_json, enabled
         FROM pyramid_providers
         ORDER BY display_name",
    )?;
    let rows = stmt.query_map([], provider_from_row)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Insert or update a provider row. Idempotent via ON CONFLICT.
pub fn save_provider(conn: &Connection, provider: &Provider) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_providers (
            id, display_name, provider_type, base_url, api_key_ref,
            auto_detect_context, supports_broadcast, broadcast_config_json,
            config_json, enabled, created_at, updated_at
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5,
            ?6, ?7, ?8,
            ?9, ?10, datetime('now'), datetime('now')
         )
         ON CONFLICT(id) DO UPDATE SET
            display_name = excluded.display_name,
            provider_type = excluded.provider_type,
            base_url = excluded.base_url,
            api_key_ref = excluded.api_key_ref,
            auto_detect_context = excluded.auto_detect_context,
            supports_broadcast = excluded.supports_broadcast,
            broadcast_config_json = excluded.broadcast_config_json,
            config_json = excluded.config_json,
            enabled = excluded.enabled,
            updated_at = datetime('now')
        ",
        rusqlite::params![
            provider.id,
            provider.display_name,
            provider.provider_type.as_str(),
            provider.base_url,
            provider.api_key_ref,
            provider.auto_detect_context as i64,
            provider.supports_broadcast as i64,
            provider.broadcast_config_json,
            provider.config_json,
            provider.enabled as i64,
        ],
    )?;
    Ok(())
}

/// Delete a provider row by id. Cascades to tier routing rows via the
/// ON DELETE CASCADE constraint.
pub fn delete_provider(conn: &Connection, id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM pyramid_providers WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

fn tier_routing_from_row(row: &rusqlite::Row) -> rusqlite::Result<TierRoutingEntry> {
    let context_limit: Option<i64> = row.get("context_limit")?;
    let max_completion_tokens: Option<i64> = row.get("max_completion_tokens")?;
    Ok(TierRoutingEntry {
        tier_name: row.get("tier_name")?,
        provider_id: row.get("provider_id")?,
        model_id: row.get("model_id")?,
        context_limit: context_limit.map(|n| n as usize),
        max_completion_tokens: max_completion_tokens.map(|n| n as usize),
        pricing_json: row.get("pricing_json")?,
        supported_parameters_json: row.get("supported_parameters_json")?,
        notes: row.get("notes")?,
    })
}

/// Return every tier routing entry keyed by tier_name.
pub fn get_tier_routing(conn: &Connection) -> Result<HashMap<String, TierRoutingEntry>> {
    let mut stmt = conn.prepare(
        "SELECT tier_name, provider_id, model_id, context_limit, max_completion_tokens,
                pricing_json, supported_parameters_json, notes
         FROM pyramid_tier_routing
         ORDER BY tier_name",
    )?;
    let rows = stmt.query_map([], tier_routing_from_row)?;
    let mut out = HashMap::new();
    for row in rows {
        let entry = row?;
        out.insert(entry.tier_name.clone(), entry);
    }
    Ok(out)
}

/// Upsert a tier routing entry.
pub fn save_tier_routing(conn: &Connection, entry: &TierRoutingEntry) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_tier_routing (
            tier_name, provider_id, model_id, context_limit, max_completion_tokens,
            pricing_json, supported_parameters_json, notes, created_at, updated_at
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5,
            ?6, ?7, ?8, datetime('now'), datetime('now')
         )
         ON CONFLICT(tier_name) DO UPDATE SET
            provider_id = excluded.provider_id,
            model_id = excluded.model_id,
            context_limit = excluded.context_limit,
            max_completion_tokens = excluded.max_completion_tokens,
            pricing_json = excluded.pricing_json,
            supported_parameters_json = excluded.supported_parameters_json,
            notes = excluded.notes,
            updated_at = datetime('now')
        ",
        rusqlite::params![
            entry.tier_name,
            entry.provider_id,
            entry.model_id,
            entry.context_limit.map(|n| n as i64),
            entry.max_completion_tokens.map(|n| n as i64),
            entry.pricing_json,
            entry.supported_parameters_json,
            entry.notes,
        ],
    )?;
    Ok(())
}

/// Delete a tier routing entry.
pub fn delete_tier_routing(conn: &Connection, tier_name: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM pyramid_tier_routing WHERE tier_name = ?1",
        rusqlite::params![tier_name],
    )?;
    Ok(())
}

fn step_override_from_row(row: &rusqlite::Row) -> rusqlite::Result<StepOverride> {
    Ok(StepOverride {
        slug: row.get("slug")?,
        chain_id: row.get("chain_id")?,
        step_name: row.get("step_name")?,
        field_name: row.get("field_name")?,
        value_json: row.get("value_json")?,
    })
}

/// Return every step override row.
pub fn list_step_overrides(conn: &Connection) -> Result<Vec<StepOverride>> {
    let mut stmt = conn.prepare(
        "SELECT slug, chain_id, step_name, field_name, value_json
         FROM pyramid_step_overrides
         ORDER BY slug, chain_id, step_name, field_name",
    )?;
    let rows = stmt.query_map([], step_override_from_row)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Return step overrides for a specific (slug, chain_id) pair.
pub fn get_step_overrides_for_chain(
    conn: &Connection,
    slug: &str,
    chain_id: &str,
) -> Result<Vec<StepOverride>> {
    let mut stmt = conn.prepare(
        "SELECT slug, chain_id, step_name, field_name, value_json
         FROM pyramid_step_overrides
         WHERE slug = ?1 AND chain_id = ?2
         ORDER BY step_name, field_name",
    )?;
    let rows = stmt.query_map(rusqlite::params![slug, chain_id], step_override_from_row)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Look up a single step override by the full composite key.
pub fn get_step_override(
    conn: &Connection,
    slug: &str,
    chain_id: &str,
    step_name: &str,
    field_name: &str,
) -> Result<Option<StepOverride>> {
    let mut stmt = conn.prepare(
        "SELECT slug, chain_id, step_name, field_name, value_json
         FROM pyramid_step_overrides
         WHERE slug = ?1 AND chain_id = ?2 AND step_name = ?3 AND field_name = ?4",
    )?;
    let row = stmt
        .query_row(
            rusqlite::params![slug, chain_id, step_name, field_name],
            step_override_from_row,
        )
        .optional()?;
    Ok(row)
}

/// Upsert a step override.
pub fn save_step_override(conn: &Connection, override_row: &StepOverride) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_step_overrides (
            slug, chain_id, step_name, field_name, value_json, created_at, updated_at
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, datetime('now'), datetime('now')
         )
         ON CONFLICT(slug, chain_id, step_name, field_name) DO UPDATE SET
            value_json = excluded.value_json,
            updated_at = datetime('now')
        ",
        rusqlite::params![
            override_row.slug,
            override_row.chain_id,
            override_row.step_name,
            override_row.field_name,
            override_row.value_json,
        ],
    )?;
    Ok(())
}

/// Delete a step override.
pub fn delete_step_override(
    conn: &Connection,
    slug: &str,
    chain_id: &str,
    step_name: &str,
    field_name: &str,
) -> Result<()> {
    conn.execute(
        "DELETE FROM pyramid_step_overrides
         WHERE slug = ?1 AND chain_id = ?2 AND step_name = ?3 AND field_name = ?4",
        rusqlite::params![slug, chain_id, step_name, field_name],
    )?;
    Ok(())
}

/// First-run seeding of the provider registry + tier routing table.
///
/// Adam's default model slugs (provided explicitly; do NOT verify
/// against `/api/v1/models` at seed time — they are pinned here):
///
/// | Tier          | Provider    | Model slug                |
/// | ------------- | ----------- | ------------------------- |
/// | `fast_extract`| openrouter  | `inception/mercury-2`      |
/// | `web`         | openrouter  | `x-ai/grok-4.1-fast` (2M)  |
/// | `synth_heavy` | openrouter  | `minimax/minimax-m2.7`     |
/// | `stale_remote`| openrouter  | `minimax/minimax-m2.7`     |
///
/// `stale_local` is NOT seeded — it only exists once a user registers
/// a local provider (Ollama). Do not insert a row pointing at a
/// placeholder; the absence is deliberate per Adam's decision.
///
/// Idempotent: the seed only fires when `pyramid_providers` is empty
/// so existing rows are never overwritten.
pub fn seed_default_provider_registry(conn: &Connection) -> Result<()> {
    let existing: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pyramid_providers",
        [],
        |row| row.get(0),
    )?;
    if existing > 0 {
        return Ok(());
    }

    let openrouter = Provider {
        id: "openrouter".into(),
        display_name: "OpenRouter".into(),
        provider_type: ProviderType::Openrouter,
        base_url: "https://openrouter.ai/api/v1".into(),
        api_key_ref: Some("OPENROUTER_KEY".into()),
        auto_detect_context: false,
        supports_broadcast: true,
        broadcast_config_json: None,
        config_json: "{}".into(),
        enabled: true,
    };
    save_provider(conn, &openrouter)?;

    // Seed the four tiers Adam specified. Pricing is left as an empty
    // object — Phase 14 will prefetch live pricing from
    // `GET /api/v1/models`. The context limits reflect each model's
    // published window.
    let empty_pricing = "{}".to_string();
    let seed_tier = |tier_name: &str,
                     model_id: &str,
                     context_limit: Option<usize>,
                     notes: &str|
     -> TierRoutingEntry {
        TierRoutingEntry {
            tier_name: tier_name.to_string(),
            provider_id: "openrouter".into(),
            model_id: model_id.to_string(),
            context_limit,
            max_completion_tokens: None,
            pricing_json: empty_pricing.clone(),
            supported_parameters_json: None,
            notes: Some(notes.to_string()),
        }
    };

    save_tier_routing(
        conn,
        &seed_tier(
            "fast_extract",
            "inception/mercury-2",
            Some(120_000),
            "Very fast, very cheap, smart enough for most extraction (Adam's default)",
        ),
    )?;
    save_tier_routing(
        conn,
        &seed_tier(
            "web",
            "x-ai/grok-4.1-fast",
            Some(2_000_000),
            "2M context window for whole-array relational work (Adam's default)",
        ),
    )?;
    save_tier_routing(
        conn,
        &seed_tier(
            "synth_heavy",
            "minimax/minimax-m2.7",
            Some(200_000),
            "Near-frontier (very smart), slow (40 tps), very inexpensive (Adam's default)",
        ),
    )?;
    save_tier_routing(
        conn,
        &seed_tier(
            "stale_remote",
            "minimax/minimax-m2.7",
            Some(200_000),
            "Same quality profile for upper-layer stale checks (Adam's default)",
        ),
    )?;

    // `stale_local` is intentionally NOT seeded — the tier materializes
    // when a local provider (Ollama) is added. Do not insert a
    // placeholder row here.

    // Seed `mid` and `extractor` for fresh installs — these are used by
    // code/document and conversation chain YAMLs respectively.
    save_tier_routing(
        conn,
        &seed_tier(
            "mid",
            "inception/mercury-2",
            Some(120_000),
            "Default tier for code/document chains — same model as fast_extract (Adam's default)",
        ),
    )?;
    save_tier_routing(
        conn,
        &seed_tier(
            "extractor",
            "inception/mercury-2",
            Some(120_000),
            "Conversation extraction tier — same model as fast_extract (Adam's default)",
        ),
    )?;
    save_tier_routing(
        conn,
        &seed_tier(
            "high",
            "qwen/qwen3.5-flash-02-23",
            Some(900_000),
            "Large-context fallback tier for cascade (Adam's default)",
        ),
    )?;
    save_tier_routing(
        conn,
        &seed_tier(
            "max",
            "x-ai/grok-4.20-beta",
            Some(1_000_000),
            "Maximum-context fallback tier for cascade (Adam's default)",
        ),
    )?;

    Ok(())
}

/// Ensure all standard tiers exist in the tier_routing table. Uses INSERT
/// OR IGNORE so existing rows (including Ollama-routed ones) are never
/// overwritten. Called on every boot after seed_default_provider_registry.
pub fn ensure_standard_tiers_exist(conn: &Connection) -> Result<()> {
    let standard_tiers: &[(&str, &str, i64, &str)] = &[
        ("fast_extract", "inception/mercury-2", 120_000, "Default extraction tier"),
        ("web", "x-ai/grok-4.1-fast", 2_000_000, "2M context for relational work"),
        ("synth_heavy", "minimax/minimax-m2.7", 200_000, "Near-frontier synthesis"),
        ("stale_remote", "minimax/minimax-m2.7", 200_000, "Upper-layer stale checks"),
        ("mid", "inception/mercury-2", 120_000, "Default tier for code/document chains"),
        ("extractor", "inception/mercury-2", 120_000, "Conversation extraction tier"),
        ("high", "qwen/qwen3.5-flash-02-23", 900_000, "Large-context fallback tier for cascade"),
        ("max", "x-ai/grok-4.20-beta", 1_000_000, "Maximum-context fallback tier for cascade"),
    ];
    for &(tier_name, model_id, context_limit, notes) in standard_tiers {
        conn.execute(
            "INSERT OR IGNORE INTO pyramid_tier_routing
                (tier_name, provider_id, model_id, context_limit, notes, created_at, updated_at)
             VALUES (?1, 'openrouter', ?2, ?3, ?4, datetime('now'), datetime('now'))",
            rusqlite::params![tier_name, model_id, context_limit, notes],
        )?;
    }
    Ok(())
}

// ── Phase 18a: Local Mode state row helpers ───────────────────────────────────

/// Snapshot of `pyramid_local_mode_state`. Mirrors the table 1:1 and
/// is consumed by the IPC handlers in `main.rs`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LocalModeStateRow {
    pub enabled: bool,
    pub ollama_base_url: Option<String>,
    pub ollama_model: Option<String>,
    pub detected_context_limit: Option<i64>,
    pub restore_from_contribution_id: Option<String>,
    pub restore_build_strategy_contribution_id: Option<String>,
    /// Phase 1 daemon control plane: user-set context override (None = auto-detect).
    pub context_override: Option<i64>,
    /// Phase 1 daemon control plane: user-set concurrency override (None = default 1).
    pub concurrency_override: Option<i64>,
    /// Phase 1 daemon control plane: prior dispatch_policy contribution_id to restore on disable.
    pub restore_dispatch_policy_contribution_id: Option<String>,
    pub updated_at: String,
}

/// Read the singleton `pyramid_local_mode_state` row. Returns a row
/// initialized with the table defaults if the singleton was never
/// inserted (defensive — `init_pyramid_db` always inserts it).
pub fn load_local_mode_state(conn: &Connection) -> Result<LocalModeStateRow> {
    let row = conn
        .query_row(
            "SELECT enabled, ollama_base_url, ollama_model,
                    detected_context_limit,
                    restore_from_contribution_id,
                    restore_build_strategy_contribution_id,
                    context_override,
                    concurrency_override,
                    restore_dispatch_policy_contribution_id,
                    updated_at
             FROM pyramid_local_mode_state
             WHERE id = 1",
            [],
            |row| {
                Ok(LocalModeStateRow {
                    enabled: row.get::<_, i64>(0)? != 0,
                    ollama_base_url: row.get(1)?,
                    ollama_model: row.get(2)?,
                    detected_context_limit: row.get(3)?,
                    restore_from_contribution_id: row.get(4)?,
                    restore_build_strategy_contribution_id: row.get(5)?,
                    context_override: row.get(6)?,
                    concurrency_override: row.get(7)?,
                    restore_dispatch_policy_contribution_id: row.get(8)?,
                    updated_at: row.get(9)?,
                })
            },
        )
        .optional()?;
    Ok(row.unwrap_or_else(|| LocalModeStateRow {
        enabled: false,
        ollama_base_url: None,
        ollama_model: None,
        detected_context_limit: None,
        restore_from_contribution_id: None,
        restore_build_strategy_contribution_id: None,
        context_override: None,
        concurrency_override: None,
        restore_dispatch_policy_contribution_id: None,
        updated_at: String::new(),
    }))
}

/// Write the singleton `pyramid_local_mode_state` row. The `id = 1`
/// row is created on first call (or by `init_pyramid_db`'s
/// `INSERT OR IGNORE`).
pub fn save_local_mode_state(conn: &Connection, state: &LocalModeStateRow) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_local_mode_state (
            id, enabled, ollama_base_url, ollama_model,
            detected_context_limit,
            restore_from_contribution_id,
            restore_build_strategy_contribution_id,
            context_override,
            concurrency_override,
            restore_dispatch_policy_contribution_id,
            updated_at
         ) VALUES (
            1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, datetime('now')
         )
         ON CONFLICT(id) DO UPDATE SET
            enabled = excluded.enabled,
            ollama_base_url = excluded.ollama_base_url,
            ollama_model = excluded.ollama_model,
            detected_context_limit = excluded.detected_context_limit,
            restore_from_contribution_id = excluded.restore_from_contribution_id,
            restore_build_strategy_contribution_id = excluded.restore_build_strategy_contribution_id,
            context_override = excluded.context_override,
            concurrency_override = excluded.concurrency_override,
            restore_dispatch_policy_contribution_id = excluded.restore_dispatch_policy_contribution_id,
            updated_at = datetime('now')
        ",
        rusqlite::params![
            state.enabled as i64,
            state.ollama_base_url,
            state.ollama_model,
            state.detected_context_limit,
            state.restore_from_contribution_id,
            state.restore_build_strategy_contribution_id,
            state.context_override,
            state.concurrency_override,
            state.restore_dispatch_policy_contribution_id,
        ],
    )?;
    Ok(())
}

// ── Phase 4: Operational table upsert helpers ─────────────────────────────────
//
// Each schema_type whose `sync_config_to_operational()` writes to a dedicated
// operational table has an upsert helper here. The YAML struct definitions
// are minimal — enough to deserialize a valid YAML document and write it into
// the operational row. Full schema definitions (JSON Schema validation, every
// field) live in future phases (Phase 9 generative config seeds).
//
// Each upsert helper:
// 1. Runs inside a single transaction (or an implicit transaction on the
//    passed-in Connection).
// 2. UPSERTs the row keyed on `slug` (NULL = global).
// 3. Records the `contribution_id` FK back to pyramid_config_contributions.
//
// Notes:
// - `slug` is `Option<String>` because global configs use NULL slug. SQLite
//   treats NULL ≠ NULL in UNIQUE constraints, so the PK on (slug) combined
//   with a single global row means we DELETE-then-INSERT to simulate an
//   UPSERT that handles the NULL case. `INSERT OR REPLACE` works because
//   the PK is (slug) alone.

/// Phase 12 triage rule. Part of the `evidence_policy` YAML schema.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TriageRuleYaml {
    #[serde(default)]
    pub condition: String,
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub model_tier: Option<String>,
    #[serde(default)]
    pub check_interval: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
}

/// Phase 12 demand signal policy rule.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DemandSignalRuleYaml {
    #[serde(rename = "type", default)]
    pub r#type: String,
    #[serde(default)]
    pub threshold: f64,
    #[serde(default)]
    pub window: String,
}

/// Phase 12 policy budget (triage model tier + max concurrency).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PolicyBudgetYaml {
    #[serde(default)]
    pub maintenance_model_tier: Option<String>,
    #[serde(default)]
    pub initial_build_model_tier: Option<String>,
    #[serde(default)]
    pub triage_model_tier: Option<String>,
    #[serde(default)]
    pub max_concurrent_evidence: Option<usize>,
    #[serde(default)]
    pub triage_batch_size: Option<usize>,
}

/// Phase 12 demand signal attenuation parameters (BFS propagation).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DemandSignalAttenuationYaml {
    #[serde(default = "default_attenuation_factor")]
    pub factor: f64,
    #[serde(default = "default_attenuation_floor")]
    pub floor: f64,
    #[serde(default = "default_attenuation_max_depth")]
    pub max_depth: u32,
}

fn default_attenuation_factor() -> f64 {
    0.5
}
fn default_attenuation_floor() -> f64 {
    0.1
}
fn default_attenuation_max_depth() -> u32 {
    6
}

impl Default for DemandSignalAttenuationYaml {
    fn default() -> Self {
        Self {
            factor: default_attenuation_factor(),
            floor: default_attenuation_floor(),
            max_depth: default_attenuation_max_depth(),
        }
    }
}

/// Minimal evidence policy YAML struct. Extended in Phase 9/11/12
/// (evidence triage spec). All new fields carry `#[serde(default)]` so
/// pre-Phase-12 YAML still deserializes cleanly.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct EvidencePolicyYaml {
    #[serde(default)]
    pub triage_rules: Option<Vec<TriageRuleYaml>>,
    #[serde(default)]
    pub demand_signals: Option<Vec<DemandSignalRuleYaml>>,
    #[serde(default)]
    pub budget: Option<PolicyBudgetYaml>,
    #[serde(default)]
    pub demand_signal_attenuation: Option<DemandSignalAttenuationYaml>,
}

/// Runtime representation of an evidence policy with defaults filled
/// in. Returned by `load_active_evidence_policy`. Callers use this to
/// evaluate triage conditions without unwrapping `Option`s repeatedly.
#[derive(Debug, Clone)]
pub struct EvidencePolicy {
    pub slug: Option<String>,
    pub contribution_id: Option<String>,
    pub triage_rules: Vec<TriageRuleYaml>,
    pub demand_signals: Vec<DemandSignalRuleYaml>,
    pub budget: PolicyBudgetYaml,
    pub demand_signal_attenuation: DemandSignalAttenuationYaml,
    /// SHA-256 hex of the source YAML; used as part of the triage
    /// cache inputs_hash so policy changes invalidate cached triage
    /// decisions.
    pub policy_yaml_hash: String,
}

impl EvidencePolicy {
    /// Convenience: return the triage_batch_size with a reasonable
    /// default (15 per the spec) if unset.
    pub fn triage_batch_size(&self) -> usize {
        self.budget.triage_batch_size.unwrap_or(15)
    }
}

/// UPSERT an evidence policy row keyed on slug.
pub fn upsert_evidence_policy(
    conn: &Connection,
    slug: &Option<String>,
    yaml: &EvidencePolicyYaml,
    contribution_id: &str,
) -> Result<()> {
    let triage_json = serde_json::to_string(&yaml.triage_rules).unwrap_or_else(|_| "[]".into());
    let demand_json = serde_json::to_string(&yaml.demand_signals).unwrap_or_else(|_| "[]".into());
    let budget_json = serde_json::to_string(&yaml.budget).unwrap_or_else(|_| "{}".into());

    // INSERT OR REPLACE keyed on slug (PK). Handles both per-slug and
    // global (NULL slug) rows — SQLite PRIMARY KEY treats a single-NULL
    // row as a distinct key.
    conn.execute(
        "INSERT OR REPLACE INTO pyramid_evidence_policy (
            slug, triage_rules_json, demand_signals_json, budget_json,
            contribution_id, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
        rusqlite::params![slug, triage_json, demand_json, budget_json, contribution_id],
    )?;
    Ok(())
}

/// Phase 12: load the active evidence policy for a slug (or the
/// global default if slug is None). Returns a fully-populated
/// `EvidencePolicy` with defaults filled in for any missing fields.
///
/// Resolution order:
/// 1. Try `slug`-specific row in `pyramid_evidence_policy`.
/// 2. Fall back to the global (`slug IS NULL`) row.
/// 3. If neither exists, return a policy with empty rules and
///    defaults — behaves as "triage disabled, all questions answered".
pub fn load_active_evidence_policy(
    conn: &Connection,
    slug: Option<&str>,
) -> Result<EvidencePolicy> {
    use crate::pyramid::step_context::sha256_hex;

    // Try slug-specific row first, then global.
    let row: Option<(Option<String>, String, String, String, String)> = match slug {
        Some(s) => conn
            .query_row(
                "SELECT slug, triage_rules_json, demand_signals_json, budget_json, contribution_id
                 FROM pyramid_evidence_policy WHERE slug = ?1",
                rusqlite::params![s],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .ok(),
        None => None,
    };
    let row = match row {
        Some(r) => Some(r),
        None => conn
            .query_row(
                "SELECT slug, triage_rules_json, demand_signals_json, budget_json, contribution_id
                 FROM pyramid_evidence_policy WHERE slug IS NULL",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .ok(),
    };

    let (row_slug, triage_json, demand_json, budget_json, contribution_id) = match row {
        Some(r) => r,
        None => {
            return Ok(EvidencePolicy {
                slug: slug.map(|s| s.to_string()),
                contribution_id: None,
                triage_rules: Vec::new(),
                demand_signals: Vec::new(),
                budget: PolicyBudgetYaml::default(),
                demand_signal_attenuation: DemandSignalAttenuationYaml::default(),
                policy_yaml_hash: String::new(),
            });
        }
    };

    // Re-parse JSON blobs into typed structs. `triage_rules_json` is
    // an `Option<Vec<TriageRuleYaml>>` in `EvidencePolicyYaml`, so the
    // stored JSON looks like `null`, `[]`, or `[...]`.
    let triage_rules: Vec<TriageRuleYaml> = serde_json::from_str::<
        Option<Vec<TriageRuleYaml>>,
    >(&triage_json)
    .ok()
    .flatten()
    .unwrap_or_default();
    let demand_signals: Vec<DemandSignalRuleYaml> = serde_json::from_str::<
        Option<Vec<DemandSignalRuleYaml>>,
    >(&demand_json)
    .ok()
    .flatten()
    .unwrap_or_default();
    // Budget JSON may be `null`, `{}`, or a full object.
    let budget_raw: Option<PolicyBudgetYaml> = serde_json::from_str::<
        Option<PolicyBudgetYaml>,
    >(&budget_json)
    .ok()
    .flatten();
    let budget = budget_raw.unwrap_or_default();

    // demand_signal_attenuation isn't stored in a dedicated column —
    // we persist the whole `EvidencePolicyYaml` via its JSON columns
    // only (triage_rules_json, demand_signals_json, budget_json). For
    // Phase 12, attenuation parameters default to spec values unless
    // the user adds an `demand_signal_attenuation` row via the
    // budget_json envelope (we'll accept it under budget_json as a
    // subobject in a future extension). For now: defaults.
    let demand_signal_attenuation = DemandSignalAttenuationYaml::default();

    // Hash the concatenation of all three JSON blobs — policy_yaml_hash
    // only needs to change when the policy changes.
    let combined = format!("{}|{}|{}", triage_json, demand_json, budget_json);
    let policy_yaml_hash = sha256_hex(combined.as_bytes());

    Ok(EvidencePolicy {
        slug: row_slug,
        contribution_id: Some(contribution_id),
        triage_rules,
        demand_signals,
        budget,
        demand_signal_attenuation,
        policy_yaml_hash,
    })
}

// ── Phase 12 demand signal helpers ──────────────────────────────────────────
//
// Demand signals are fire-and-forget INSERTs recorded every time an
// agent query, user drill, or search hit resolves to a pyramid node.
// They drive the `has_demand_signals` condition in the triage DSL and
// support the on-demand reactivation of deferred questions.

/// Insert a single demand signal row. The caller chooses `weight`
/// (1.0 at the leaf, attenuated values on parents). Writes via the
/// passed connection — propagation is handled by
/// `demand_signal::record_demand_signal`.
pub fn insert_demand_signal(
    conn: &Connection,
    slug: &str,
    node_id: &str,
    signal_type: &str,
    source: Option<&str>,
    weight: f64,
    source_node_id: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_demand_signals (
            slug, node_id, signal_type, source, weight, source_node_id, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))",
        rusqlite::params![slug, node_id, signal_type, source, weight, source_node_id],
    )?;
    Ok(())
}

/// Sum the weights of demand signals of a given type for a node
/// within a time window. `window_modifier` is a SQLite datetime
/// modifier such as `"-14 days"` or `"-7 days"`.
///
/// Returns 0.0 if no signals match (not an error).
pub fn sum_demand_weight(
    conn: &Connection,
    slug: &str,
    node_id: &str,
    signal_type: &str,
    window_modifier: &str,
) -> Result<f64> {
    let total: Option<f64> = conn
        .query_row(
            "SELECT SUM(weight) FROM pyramid_demand_signals
             WHERE slug = ?1 AND node_id = ?2 AND signal_type = ?3
               AND created_at > datetime('now', ?4)",
            rusqlite::params![slug, node_id, signal_type, window_modifier],
            |r| r.get(0),
        )
        .ok();
    Ok(total.unwrap_or(0.0))
}

/// Phase 12 wanderer fix: sum the weights of demand signals of a
/// given type across an ENTIRE slug within a time window. This is
/// the helper the triage DSL's `has_demand_signals` condition uses —
/// the per-node variant (`sum_demand_weight`) can't be used for
/// deferred evidence questions because a `LayerQuestion.question_id`
/// is a `q-{sha256}` hash, not the L{layer}-{seq} id that demand
/// signals are recorded under. The two ID spaces never meet.
///
/// Aggregating per-slug matches the spec's intent ("drive re-check
/// by demand") while staying correct in the only ID space we
/// actually have at both sides of the join. When the pyramid grows
/// a persistent q-hash → node-id map (Phase 13+), the per-node
/// variant can be brought back for spatial precision.
///
/// Returns 0.0 if no signals match (not an error).
pub fn sum_slug_demand_weight(
    conn: &Connection,
    slug: &str,
    signal_type: &str,
    window_modifier: &str,
) -> Result<f64> {
    let total: Option<f64> = conn
        .query_row(
            "SELECT SUM(weight) FROM pyramid_demand_signals
             WHERE slug = ?1 AND signal_type = ?2
               AND created_at > datetime('now', ?3)",
            rusqlite::params![slug, signal_type, window_modifier],
            |r| r.get(0),
        )
        .ok();
    Ok(total.unwrap_or(0.0))
}

/// Phase 12 wanderer fix: list every distinct slug that has at
/// least one row in `pyramid_deferred_questions`. Used by the
/// global-policy re-evaluation path so a supersession of a global
/// `evidence_policy` (contribution with `slug = NULL`) can walk
/// every affected slug instead of silently matching zero rows on
/// `slug = ''`.
pub fn list_slugs_with_deferred_questions(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT slug FROM pyramid_deferred_questions ORDER BY slug ASC",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Phase 12 wanderer fix: list every deferred row for a slug with
/// `check_interval IN ('never', 'on_demand')`. Used by the
/// `record_demand_signal` on-demand reactivation hook. The previous
/// `list_deferred_by_question_target` helper tried to join by
/// question_id = node_id which never matches (q-hash vs L{}-{}),
/// so the reactivation hook was a no-op. This helper drops the
/// node_id filter and returns all slug-level `on_demand`/`never`
/// rows — the demand signal handler then re-triages each with
/// `has_demand_signals=true` and reactivates the ones whose
/// decision flips to Answer.
pub fn list_on_demand_deferred_for_slug(
    conn: &Connection,
    slug: &str,
) -> Result<Vec<DeferredQuestion>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, question_id, question_json, deferred_at, next_check_at,
                check_interval, triage_reason, contribution_id
         FROM pyramid_deferred_questions
         WHERE slug = ?1 AND check_interval IN ('never', 'on_demand')
         ORDER BY deferred_at ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![slug], |r| {
        Ok(DeferredQuestion {
            id: r.get(0)?,
            slug: r.get(1)?,
            question_id: r.get(2)?,
            question_json: r.get(3)?,
            deferred_at: r.get(4)?,
            next_check_at: r.get(5)?,
            check_interval: r.get(6)?,
            triage_reason: r.get(7)?,
            contribution_id: r.get(8)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Walk `pyramid_evidence` to find every parent node (target_node_id)
/// linked via a KEEP edge from the given node_id. Used by the demand
/// signal propagation BFS.
///
/// `pyramid_evidence` stores verdicts as uppercase strings (`KEEP`,
/// `DISCONNECT`, `MISSING`) enforced by a CHECK constraint. The
/// `live_pyramid_evidence` view adds liveness filtering by joining
/// against the nodes table, but for propagation we want all
/// historically-linked parents — the signal graph follows all
/// edges regardless of whether the node is currently superseded.
pub fn load_parents_via_evidence(
    conn: &Connection,
    slug: &str,
    node_id: &str,
) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT target_node_id FROM pyramid_evidence
         WHERE slug = ?1 AND source_node_id = ?2 AND verdict = 'KEEP'",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![slug, node_id], |r| r.get::<_, String>(0))?;
    let mut parents = Vec::new();
    for row in rows {
        parents.push(row?);
    }
    Ok(parents)
}

// ── Phase 12 deferred questions helpers ─────────────────────────────────────
//
// Deferred questions are evidence questions that the triage step
// routed to "defer" instead of "answer" or "skip". They are stored
// with a `next_check_at` deadline and re-evaluated by the DADBEAR
// tick loop or by demand-signal reactivation.

/// Runtime representation of a deferred question row.
#[derive(Debug, Clone)]
pub struct DeferredQuestion {
    pub id: i64,
    pub slug: String,
    pub question_id: String,
    pub question_json: String,
    pub deferred_at: String,
    pub next_check_at: String,
    pub check_interval: String,
    pub triage_reason: Option<String>,
    pub contribution_id: Option<String>,
}

/// Parse a `check_interval` string into a SQLite datetime modifier and
/// compute a `next_check_at` value as an ISO timestamp. Supports
/// "Nd" (days), "Nh" (hours), "Nw" (weeks), "never", "on_demand".
/// Unrecognized intervals default to 30 days.
pub fn parse_check_interval_to_next_check_at(interval: &str) -> String {
    let trimmed = interval.trim().to_lowercase();
    if trimmed == "never" || trimmed == "on_demand" {
        return "9999-12-31 00:00:00".to_string();
    }
    // Parse "Nd", "Nh", "Nw" forms.
    let (num_part, unit_part) = trimmed
        .chars()
        .partition::<String, _>(|c| c.is_ascii_digit() || *c == '-');
    let num: i64 = num_part.parse().unwrap_or(30);
    let unit = unit_part.as_str();
    let modifier = match unit {
        "d" => format!("+{} days", num),
        "h" => format!("+{} hours", num),
        "w" => format!("+{} days", num * 7),
        "m" => format!("+{} minutes", num), // minutes (test convenience)
        _ => format!("+{} days", num),
    };
    modifier
}

/// UPSERT a deferred question. `question_id` is the canonical id from
/// `LayerQuestion.question_id`. `question_json` is the full serialized
/// payload (so the tick can re-run triage without needing the live
/// question graph). `check_interval` is stored verbatim; the
/// `next_check_at` column is computed via SQLite's datetime function.
pub fn defer_question(
    conn: &Connection,
    slug: &str,
    question_id: &str,
    question_json: &str,
    check_interval: &str,
    triage_reason: Option<&str>,
    contribution_id: Option<&str>,
) -> Result<()> {
    let trimmed = check_interval.trim().to_lowercase();
    if trimmed == "never" || trimmed == "on_demand" {
        conn.execute(
            "INSERT INTO pyramid_deferred_questions (
                slug, question_id, question_json, deferred_at, next_check_at,
                check_interval, triage_reason, contribution_id
             ) VALUES (?1, ?2, ?3, datetime('now'), '9999-12-31 00:00:00',
                 ?4, ?5, ?6)
             ON CONFLICT(slug, question_id) DO UPDATE SET
                 question_json = excluded.question_json,
                 deferred_at = datetime('now'),
                 next_check_at = '9999-12-31 00:00:00',
                 check_interval = excluded.check_interval,
                 triage_reason = excluded.triage_reason,
                 contribution_id = excluded.contribution_id",
            rusqlite::params![
                slug,
                question_id,
                question_json,
                check_interval,
                triage_reason,
                contribution_id,
            ],
        )?;
    } else {
        let modifier = parse_check_interval_to_next_check_at(check_interval);
        conn.execute(
            "INSERT INTO pyramid_deferred_questions (
                slug, question_id, question_json, deferred_at, next_check_at,
                check_interval, triage_reason, contribution_id
             ) VALUES (?1, ?2, ?3, datetime('now'), datetime('now', ?4),
                 ?5, ?6, ?7)
             ON CONFLICT(slug, question_id) DO UPDATE SET
                 question_json = excluded.question_json,
                 deferred_at = datetime('now'),
                 next_check_at = datetime('now', ?4),
                 check_interval = excluded.check_interval,
                 triage_reason = excluded.triage_reason,
                 contribution_id = excluded.contribution_id",
            rusqlite::params![
                slug,
                question_id,
                question_json,
                modifier,
                check_interval,
                triage_reason,
                contribution_id,
            ],
        )?;
    }
    Ok(())
}

/// List every deferred question whose `next_check_at` is on or before
/// the current time AND whose `check_interval` is not "never" or
/// "on_demand" (those are reactivated only by demand signals).
pub fn list_expired_deferred(
    conn: &Connection,
    slug: &str,
) -> Result<Vec<DeferredQuestion>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, question_id, question_json, deferred_at, next_check_at,
                check_interval, triage_reason, contribution_id
         FROM pyramid_deferred_questions
         WHERE slug = ?1
           AND next_check_at <= datetime('now')
           AND check_interval NOT IN ('never', 'on_demand')
         ORDER BY next_check_at ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![slug], |r| {
        Ok(DeferredQuestion {
            id: r.get(0)?,
            slug: r.get(1)?,
            question_id: r.get(2)?,
            question_json: r.get(3)?,
            deferred_at: r.get(4)?,
            next_check_at: r.get(5)?,
            check_interval: r.get(6)?,
            triage_reason: r.get(7)?,
            contribution_id: r.get(8)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// List ALL deferred questions for a slug, regardless of next_check_at
/// or interval. Used by the policy-change re-evaluation flow.
pub fn list_all_deferred(
    conn: &Connection,
    slug: &str,
) -> Result<Vec<DeferredQuestion>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, question_id, question_json, deferred_at, next_check_at,
                check_interval, triage_reason, contribution_id
         FROM pyramid_deferred_questions
         WHERE slug = ?1
         ORDER BY deferred_at ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![slug], |r| {
        Ok(DeferredQuestion {
            id: r.get(0)?,
            slug: r.get(1)?,
            question_id: r.get(2)?,
            question_json: r.get(3)?,
            deferred_at: r.get(4)?,
            next_check_at: r.get(5)?,
            check_interval: r.get(6)?,
            triage_reason: r.get(7)?,
            contribution_id: r.get(8)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// List every deferred question for a node. Used by the demand-signal
/// reactivation path: when a signal lands on (slug, node_id) we check
/// if any deferred questions target that node and re-run triage on
/// them.
pub fn list_deferred_by_question_target(
    conn: &Connection,
    slug: &str,
    target_node_id: &str,
) -> Result<Vec<DeferredQuestion>> {
    // Phase 12 verifier fix: the `LayerQuestion` type does not carry
    // an explicit `target_node_id` field — evidence questions are
    // identified by their `question_id`, which by convention is
    // derived from (and matches) the target node id in the question
    // compiler's output (see `question_decomposition::extract_layer_questions`).
    // Match on `question_id` column directly, with a belt-and-suspenders
    // JSON LIKE on the `question_id` payload field in case the
    // column and payload diverge (which they shouldn't).
    //
    // The previous implementation matched on `"target_node_id":"..."`
    // which never appears in the serialized LayerQuestion — so the
    // query always returned zero rows and the on-demand reactivation
    // hook in `demand_signal::record_demand_signal` was dead.
    let payload_pattern = format!("%\"question_id\":\"{}\"%", target_node_id);
    let mut stmt = conn.prepare(
        "SELECT id, slug, question_id, question_json, deferred_at, next_check_at,
                check_interval, triage_reason, contribution_id
         FROM pyramid_deferred_questions
         WHERE slug = ?1 AND (question_id = ?2 OR question_json LIKE ?3)
         ORDER BY deferred_at ASC",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![slug, target_node_id, payload_pattern],
        |r| {
            Ok(DeferredQuestion {
                id: r.get(0)?,
                slug: r.get(1)?,
                question_id: r.get(2)?,
                question_json: r.get(3)?,
                deferred_at: r.get(4)?,
                next_check_at: r.get(5)?,
                check_interval: r.get(6)?,
                triage_reason: r.get(7)?,
                contribution_id: r.get(8)?,
            })
        },
    )?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Remove a deferred question (called when triage decides to answer
/// or skip it on re-evaluation).
pub fn remove_deferred(
    conn: &Connection,
    slug: &str,
    question_id: &str,
) -> Result<()> {
    conn.execute(
        "DELETE FROM pyramid_deferred_questions WHERE slug = ?1 AND question_id = ?2",
        rusqlite::params![slug, question_id],
    )?;
    Ok(())
}

/// Update the `next_check_at` + `contribution_id` for an existing
/// deferred question. Called when triage on re-evaluation returns
/// `Defer` again but the interval (or the triggering policy) has
/// changed.
pub fn update_deferred_next_check(
    conn: &Connection,
    slug: &str,
    question_id: &str,
    check_interval: &str,
    contribution_id: Option<&str>,
) -> Result<()> {
    let trimmed = check_interval.trim().to_lowercase();
    if trimmed == "never" || trimmed == "on_demand" {
        conn.execute(
            "UPDATE pyramid_deferred_questions SET
                next_check_at = '9999-12-31 00:00:00',
                check_interval = ?1,
                contribution_id = ?2
             WHERE slug = ?3 AND question_id = ?4",
            rusqlite::params![check_interval, contribution_id, slug, question_id],
        )?;
    } else {
        let modifier = parse_check_interval_to_next_check_at(check_interval);
        conn.execute(
            "UPDATE pyramid_deferred_questions SET
                next_check_at = datetime('now', ?1),
                check_interval = ?2,
                contribution_id = ?3
             WHERE slug = ?4 AND question_id = ?5",
            rusqlite::params![modifier, check_interval, contribution_id, slug, question_id],
        )?;
    }
    Ok(())
}

/// Minimal build strategy YAML struct. Extended in Phase 9.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct BuildStrategyYaml {
    #[serde(default)]
    pub initial_build: Option<serde_yaml::Value>,
    #[serde(default)]
    pub maintenance: Option<serde_yaml::Value>,
    #[serde(default)]
    pub quality: Option<serde_yaml::Value>,
}

/// UPSERT a build strategy row keyed on slug.
pub fn upsert_build_strategy(
    conn: &Connection,
    slug: &Option<String>,
    yaml: &BuildStrategyYaml,
    contribution_id: &str,
) -> Result<()> {
    let initial_json =
        serde_json::to_string(&yaml.initial_build).unwrap_or_else(|_| "{}".into());
    let maintenance_json =
        serde_json::to_string(&yaml.maintenance).unwrap_or_else(|_| "{}".into());
    let quality_json = serde_json::to_string(&yaml.quality).unwrap_or_else(|_| "{}".into());

    conn.execute(
        "INSERT OR REPLACE INTO pyramid_build_strategy (
            slug, initial_build_json, maintenance_json, quality_json,
            contribution_id, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
        rusqlite::params![
            slug,
            initial_json,
            maintenance_json,
            quality_json,
            contribution_id
        ],
    )?;
    Ok(())
}

/// Read the global build strategy's initial_build concurrency cap.
/// Returns `None` if no strategy is set or if concurrency is absent.
/// Used by the chain executor to cap step concurrency against the
/// build_strategy contribution (e.g., local mode sets concurrency=1).
pub fn read_build_strategy_concurrency(conn: &Connection) -> Result<Option<usize>> {
    let result: Option<String> = conn
        .query_row(
            "SELECT initial_build_json FROM pyramid_build_strategy WHERE slug IS NULL ORDER BY updated_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(json_str) = result {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&json_str) {
            if let Some(c) = val.get("concurrency").and_then(|v| v.as_u64()) {
                // Defense-in-depth: clamp against MAX_CONCURRENCY so a
                // direct YAML edit can't exceed the hard ceiling (AD-5).
                let clamped = (c as usize).min(super::local_mode::MAX_CONCURRENCY);
                return Ok(Some(clamped));
            }
        }
    }
    Ok(None)
}

/// Minimal custom prompts YAML struct. Extended in Phase 9.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CustomPromptsYaml {
    #[serde(default)]
    pub extraction_focus: Option<String>,
    #[serde(default)]
    pub synthesis_style: Option<String>,
    #[serde(default)]
    pub vocabulary_priority: Option<serde_yaml::Value>,
    #[serde(default)]
    pub ignore_patterns: Option<serde_yaml::Value>,
}

/// UPSERT a custom prompts row keyed on slug.
pub fn upsert_custom_prompts(
    conn: &Connection,
    slug: &Option<String>,
    yaml: &CustomPromptsYaml,
    contribution_id: &str,
) -> Result<()> {
    let vocab_json = yaml
        .vocabulary_priority
        .as_ref()
        .and_then(|v| serde_json::to_string(v).ok());
    let ignore_json = yaml
        .ignore_patterns
        .as_ref()
        .and_then(|v| serde_json::to_string(v).ok());

    conn.execute(
        "INSERT OR REPLACE INTO pyramid_custom_prompts (
            slug, extraction_focus, synthesis_style,
            vocabulary_priority_json, ignore_patterns_json,
            contribution_id, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))",
        rusqlite::params![
            slug,
            yaml.extraction_focus,
            yaml.synthesis_style,
            vocab_json,
            ignore_json,
            contribution_id,
        ],
    )?;
    Ok(())
}

/// Folder ingestion heuristics YAML struct.
///
/// Phase 4 introduced the core fields. Phase 17 extends it with the
/// Claude Code auto-include toggle, extension lists, and the DADBEAR
/// default scan interval. Every field is optional so a minimal YAML
/// (only `schema_type`) validates cleanly and picks up the loader
/// defaults.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct FolderIngestionHeuristicsYaml {
    #[serde(default = "default_min_files_for_pyramid")]
    pub min_files_for_pyramid: i64,
    #[serde(default = "default_max_file_size_bytes")]
    pub max_file_size_bytes: i64,
    #[serde(default = "default_max_recursion_depth")]
    pub max_recursion_depth: i64,
    #[serde(default)]
    pub content_type_rules: Option<serde_yaml::Value>,
    #[serde(default)]
    pub ignore_patterns: Option<serde_yaml::Value>,
    #[serde(default = "default_true")]
    pub respect_gitignore: bool,
    #[serde(default = "default_true")]
    pub respect_pyramid_ignore: bool,
    #[serde(default = "default_true")]
    pub vine_collapse_single_child: bool,
    // ── Phase 17 extensions ──
    #[serde(default = "default_scan_interval_secs")]
    pub default_scan_interval_secs: i64,
    #[serde(default)]
    pub code_extensions: Option<Vec<String>>,
    #[serde(default)]
    pub document_extensions: Option<Vec<String>>,
    #[serde(default = "default_true")]
    pub claude_code_auto_include: bool,
    #[serde(default = "default_claude_code_conversation_path")]
    pub claude_code_conversation_path: String,
}

fn default_min_files_for_pyramid() -> i64 {
    3
}
fn default_max_file_size_bytes() -> i64 {
    10_485_760
}
fn default_max_recursion_depth() -> i64 {
    10
}
fn default_true() -> bool {
    true
}
fn default_scan_interval_secs() -> i64 {
    30
}
fn default_claude_code_conversation_path() -> String {
    "~/.claude/projects".to_string()
}

/// Phase 17: fully-resolved folder ingestion config, with every field
/// defaulted. Returned by `load_active_folder_ingestion_heuristics`.
#[derive(Debug, Clone)]
pub struct FolderIngestionConfig {
    pub min_files_for_pyramid: usize,
    pub max_file_size_bytes: u64,
    pub max_recursion_depth: usize,
    pub default_scan_interval_secs: u64,
    pub code_extensions: Vec<String>,
    pub document_extensions: Vec<String>,
    pub ignore_patterns: Vec<String>,
    pub respect_gitignore: bool,
    pub respect_pyramid_ignore: bool,
    pub vine_collapse_single_child: bool,
    pub claude_code_auto_include: bool,
    pub claude_code_conversation_path: String,
}

impl Default for FolderIngestionConfig {
    fn default() -> Self {
        Self {
            min_files_for_pyramid: 3,
            max_file_size_bytes: 10_485_760,
            max_recursion_depth: 10,
            default_scan_interval_secs: 30,
            code_extensions: default_code_extensions(),
            document_extensions: default_document_extensions(),
            ignore_patterns: default_ignore_patterns(),
            respect_gitignore: true,
            respect_pyramid_ignore: true,
            vine_collapse_single_child: true,
            claude_code_auto_include: true,
            claude_code_conversation_path: "~/.claude/projects".to_string(),
        }
    }
}

/// Phase 17: seed list of code file extensions (lowercase, with leading dot).
pub fn default_code_extensions() -> Vec<String> {
    [
        ".rs", ".ts", ".tsx", ".py", ".go", ".js", ".jsx", ".java", ".rb",
        ".c", ".cpp", ".h", ".hpp", ".cs", ".swift", ".kt",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Phase 17: seed list of document file extensions (lowercase, with leading dot).
pub fn default_document_extensions() -> Vec<String> {
    [".md", ".txt", ".pdf", ".doc", ".docx", ".rst", ".org"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Phase 17: seed list of bundled ignore patterns.
///
/// These supplement (but do NOT replace) the scanner's `.gitignore` /
/// `.pyramid-ignore` walker. They exist for two reasons:
///
/// 1. Defence-in-depth: when a folder isn't inside a git repo and
///    has no ignore file, these still keep common noise out.
/// 2. Catching known-junk paths that escape the walker: workspace
///    backups, AI-session state directories, and filesystem oddities
///    like a literal `~/` dir from shell-escape mishaps. These are
///    rarely in a `.gitignore` but pollute pyramid builds.
///
/// Pattern semantics (see `path_matches_any_ignore` in
/// `pyramid/folder_ingestion.rs`):
/// - `name/` → matches any path component named `name` at any depth
/// - `*.ext` → case-insensitive basename suffix match
/// - bare token → exact basename, exact component, OR substring on
///    the full path (the substring fallback is what catches
///    timestamped backups like `.lab.bak.1774645342/`).
pub fn default_ignore_patterns() -> Vec<String> {
    [
        // ── Build artifacts and vendored deps ──
        "node_modules/",
        "target/",
        ".git/",
        "dist/",
        "build/",
        "out/",
        ".next/",
        ".nuxt/",
        ".turbo/",
        "coverage/",
        ".nyc_output/",
        // ── Language-specific caches ──
        "__pycache__/",
        ".pytest_cache/",
        ".mypy_cache/",
        ".ruff_cache/",
        ".venv/",
        "venv/",
        // ── Editor / IDE state ──
        ".idea/",
        ".vscode/",
        // ── Alternate VCS ──
        ".svn/",
        ".hg/",
        // ── AI agent session state (handled separately via
        //    claude_code_conversation_path, not as source docs) ──
        ".claude/",
        // ── Filesystem oddities ──
        //
        // Substring match for timestamped backup directories
        // (`.lab.bak.1774645342/`, `.lab.bak.20260402200504/`, etc.).
        // Users accumulate these when running experiment snapshots,
        // and they're never knowledge the pyramid should index.
        ".lab.bak.",
        // A literal `~/` directory at the repo root — usually a
        // shell-escape mishap (`mv foo ~/bar` without expansion).
        "~/",
        // ── Files ──
        "*.lock",
        "*.bin",
        "*.exe",
        "*.dylib",
        ".DS_Store",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Phase 17: load the active `folder_ingestion_heuristics` contribution and
/// return a `FolderIngestionConfig` with every field resolved.
///
/// Resolution order:
/// 1. Row in `pyramid_folder_ingestion_heuristics` for `slug = NULL` (the
///    global default written on contribution sync).
/// 2. If absent, returns `FolderIngestionConfig::default()` so the folder
///    ingester always has a workable config even on a pristine DB.
pub fn load_active_folder_ingestion_heuristics(conn: &Connection) -> Result<FolderIngestionConfig> {
    let row: Option<(
        i64, i64, i64, String, String, i64, i64, i64, i64, String, String, String,
    )> = conn
        .query_row(
            "SELECT min_files_for_pyramid, max_file_size_bytes, max_recursion_depth,
                    code_extensions_json, document_extensions_json,
                    default_scan_interval_secs, respect_gitignore, respect_pyramid_ignore,
                    vine_collapse_single_child, ignore_patterns_json,
                    claude_code_conversation_path, '' || claude_code_auto_include
             FROM pyramid_folder_ingestion_heuristics
             WHERE slug IS NULL",
            [],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, i64>(8)?,
                    r.get::<_, String>(9)?,
                    r.get::<_, String>(10)?,
                    r.get::<_, String>(11)?,
                ))
            },
        )
        .optional()?;

    let defaults = FolderIngestionConfig::default();
    let Some((
        min_files,
        max_size,
        max_depth,
        code_ext_json,
        doc_ext_json,
        scan_interval,
        respect_git,
        respect_pyramid,
        collapse_single,
        ignore_json,
        cc_path,
        cc_auto,
    )) = row
    else {
        return Ok(defaults);
    };

    let code_extensions: Vec<String> = match serde_json::from_str::<Vec<String>>(&code_ext_json) {
        Ok(v) if !v.is_empty() => v,
        _ => defaults.code_extensions,
    };
    let document_extensions: Vec<String> = match serde_json::from_str::<Vec<String>>(&doc_ext_json)
    {
        Ok(v) if !v.is_empty() => v,
        _ => defaults.document_extensions,
    };
    let ignore_patterns: Vec<String> = match serde_json::from_str::<Vec<String>>(&ignore_json) {
        Ok(v) if !v.is_empty() => v,
        _ => defaults.ignore_patterns,
    };

    Ok(FolderIngestionConfig {
        min_files_for_pyramid: min_files.max(1) as usize,
        max_file_size_bytes: max_size.max(1) as u64,
        max_recursion_depth: max_depth.max(1) as usize,
        default_scan_interval_secs: scan_interval.max(1) as u64,
        code_extensions,
        document_extensions,
        ignore_patterns,
        respect_gitignore: respect_git != 0,
        respect_pyramid_ignore: respect_pyramid != 0,
        vine_collapse_single_child: collapse_single != 0,
        claude_code_auto_include: cc_auto.trim() != "0",
        claude_code_conversation_path: if cc_path.is_empty() {
            defaults.claude_code_conversation_path
        } else {
            cc_path
        },
    })
}

/// UPSERT a folder ingestion heuristics row keyed on slug.
///
/// Phase 17: writes the extended columns (code/document extension lists,
/// DADBEAR scan interval, Claude Code auto-include toggle + path).
pub fn upsert_folder_ingestion_heuristics(
    conn: &Connection,
    slug: &Option<String>,
    yaml: &FolderIngestionHeuristicsYaml,
    contribution_id: &str,
) -> Result<()> {
    let content_type_rules_json = yaml
        .content_type_rules
        .as_ref()
        .and_then(|v| serde_json::to_string(v).ok())
        .unwrap_or_else(|| "[]".into());
    let ignore_patterns_json = yaml
        .ignore_patterns
        .as_ref()
        .and_then(|v| serde_json::to_string(v).ok())
        .unwrap_or_else(|| "[]".into());
    let code_extensions_json = yaml
        .code_extensions
        .as_ref()
        .and_then(|v| serde_json::to_string(v).ok())
        .unwrap_or_else(|| "[]".into());
    let document_extensions_json = yaml
        .document_extensions
        .as_ref()
        .and_then(|v| serde_json::to_string(v).ok())
        .unwrap_or_else(|| "[]".into());

    conn.execute(
        "INSERT OR REPLACE INTO pyramid_folder_ingestion_heuristics (
            slug, min_files_for_pyramid, max_file_size_bytes, max_recursion_depth,
            content_type_rules_json, ignore_patterns_json,
            respect_gitignore, respect_pyramid_ignore, vine_collapse_single_child,
            default_scan_interval_secs, code_extensions_json, document_extensions_json,
            claude_code_auto_include, claude_code_conversation_path,
            contribution_id, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, datetime('now'))",
        rusqlite::params![
            slug,
            yaml.min_files_for_pyramid,
            yaml.max_file_size_bytes,
            yaml.max_recursion_depth,
            content_type_rules_json,
            ignore_patterns_json,
            yaml.respect_gitignore as i64,
            yaml.respect_pyramid_ignore as i64,
            yaml.vine_collapse_single_child as i64,
            yaml.default_scan_interval_secs,
            code_extensions_json,
            document_extensions_json,
            yaml.claude_code_auto_include as i64,
            yaml.claude_code_conversation_path,
            contribution_id,
        ],
    )?;
    Ok(())
}

/// Minimal DADBEAR policy YAML struct — mirrors the columns on the
/// existing `pyramid_dadbear_config` table that actually represent
/// policy (as opposed to operational metadata like `id`, `created_at`,
/// etc.). The contribution sync path writes INTO the existing DADBEAR
/// table rather than creating a new one, per the spec's
/// "pyramid_dadbear_config (existing)" section.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DadbearPolicyYaml {
    pub source_path: String,
    pub content_type: String,
    #[serde(default = "dadbear_default_scan_interval")]
    pub scan_interval_secs: i64,
    #[serde(default = "dadbear_default_debounce")]
    pub debounce_secs: i64,
    #[serde(default = "dadbear_default_session_timeout")]
    pub session_timeout_secs: i64,
    #[serde(default = "dadbear_default_batch_size")]
    pub batch_size: i64,
    /// DEPRECATED (Phase 7): `enabled` is no longer read by the master gate.
    /// Contribution existence is the enable gate; holds projection is the
    /// dispatch gate. Retained with `serde(default)` so old YAMLs with this
    /// field still deserialize without error.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Phase 11: cost reconciliation thresholds. Persisted back
    /// through the contribution for Wire sharing + versioning but
    /// not yet consumed by the runtime — `openrouter_webhook::
    /// process_trace` uses `CostReconciliationPolicy::default()`.
    /// Phase 12/15 will wire this into the live runtime policy so
    /// users can tune the thresholds without a rebuild.
    #[serde(default)]
    pub cost_reconciliation: Option<DadbearCostReconciliationYaml>,
}

/// Phase 11: cost reconciliation policy surfaced on `dadbear_policy`.
/// Mirrors `CostReconciliationPolicy` in `provider_health.rs` — the
/// YAML is the persistent form, the struct is the runtime form.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DadbearCostReconciliationYaml {
    #[serde(default = "default_discrepancy_ratio")]
    pub discrepancy_ratio: f64,
    #[serde(default = "default_provider_degrade_count")]
    pub provider_degrade_count: i64,
    #[serde(default = "default_provider_degrade_window_secs")]
    pub provider_degrade_window_secs: i64,
    #[serde(default = "default_true")]
    pub broadcast_required: bool,
    #[serde(default = "default_broadcast_grace_period_secs")]
    pub broadcast_grace_period_secs: i64,
    #[serde(default = "default_broadcast_audit_interval_secs")]
    pub broadcast_audit_interval_secs: i64,
}

impl Default for DadbearCostReconciliationYaml {
    fn default() -> Self {
        Self {
            discrepancy_ratio: default_discrepancy_ratio(),
            provider_degrade_count: default_provider_degrade_count(),
            provider_degrade_window_secs: default_provider_degrade_window_secs(),
            broadcast_required: true,
            broadcast_grace_period_secs: default_broadcast_grace_period_secs(),
            broadcast_audit_interval_secs: default_broadcast_audit_interval_secs(),
        }
    }
}

fn default_discrepancy_ratio() -> f64 {
    0.10
}
fn default_provider_degrade_count() -> i64 {
    3
}
fn default_provider_degrade_window_secs() -> i64 {
    600
}
fn default_broadcast_grace_period_secs() -> i64 {
    600
}
fn default_broadcast_audit_interval_secs() -> i64 {
    900
}

fn dadbear_default_scan_interval() -> i64 {
    10
}
fn dadbear_default_debounce() -> i64 {
    30
}
fn dadbear_default_session_timeout() -> i64 {
    1800
}
fn dadbear_default_batch_size() -> i64 {
    1
}

// ── DADBEAR Canonical Architecture: split contribution types ─────────────────
//
// Phase 0 of `docs/plans/dadbear-canonical-state-model.md` splits the
// monolithic `dadbear_policy` into two focused contribution types:
//
//   - `watch_root`: per-source-path identity (slug, source_path, content_type)
//   - `dadbear_norms`: per-slug or global timing/threshold norms
//
// Contribution existence IS the enable gate — no `enabled` bool needed.
// If no `watch_root` contribution exists for a slug, DADBEAR doesn't watch it.

fn default_scan_interval() -> i64 {
    10
}
fn default_debounce() -> i64 {
    30
}
fn default_session_timeout() -> i64 {
    1800
}
fn default_batch_size() -> i64 {
    1
}
fn default_min_changed() -> i64 {
    1
}
fn default_runaway() -> f64 {
    0.5
}
fn default_retention() -> i64 {
    30
}

/// Identity contribution for a single watched source path. One per
/// (slug, source_path) pair. Contribution existence IS the enable gate.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct WatchRootYaml {
    pub source_path: String,
    pub content_type: String,
}

/// Per-slug or global timing/threshold norms. One per slug (or slug=NULL
/// for global defaults). The layered resolver merges global + per-slug
/// at read time; the dispatcher writes resolved norms into the
/// operational table on each sync.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DadbearNormsYaml {
    #[serde(default = "default_scan_interval")]
    pub scan_interval_secs: i64,
    #[serde(default = "default_debounce")]
    pub debounce_secs: i64,
    #[serde(default = "default_session_timeout")]
    pub session_timeout_secs: i64,
    #[serde(default = "default_batch_size")]
    pub batch_size: i64,
    #[serde(default = "default_min_changed")]
    pub min_changed_files: i64,
    #[serde(default = "default_runaway")]
    pub runaway_threshold: f64,
    #[serde(default = "default_retention")]
    pub retention_window_days: i64,
}

impl Default for DadbearNormsYaml {
    fn default() -> Self {
        Self {
            scan_interval_secs: default_scan_interval(),
            debounce_secs: default_debounce(),
            session_timeout_secs: default_session_timeout(),
            batch_size: default_batch_size(),
            min_changed_files: default_min_changed(),
            runaway_threshold: default_runaway(),
            retention_window_days: default_retention(),
        }
    }
}

/// UPSERT a `watch_root` contribution into the operational
/// `pyramid_dadbear_config` table. Takes the resolved norms (from the
/// layered resolver) as a parameter so the norms columns are populated
/// alongside identity columns in a single atomic write.
///
/// When `slug` is `None`, this is invalid — `watch_root` is always
/// per-slug (a watched path must belong to a pyramid). Returns Ok as a
/// no-op for global watch_root contributions (shouldn't exist, but
/// defensive).
pub fn upsert_watch_root(
    conn: &Connection,
    slug: &Option<String>,
    yaml: &WatchRootYaml,
    resolved_norms: &DadbearNormsYaml,
    contribution_id: &str,
) -> Result<()> {
    let Some(slug_str) = slug.as_deref() else {
        // Global watch_root makes no sense — watch roots are per-slug.
        // Defensive no-op rather than error (matches dadbear_policy pattern).
        return Ok(());
    };

    conn.execute(
        "INSERT INTO pyramid_dadbear_config (
            slug, source_path, content_type, scan_interval_secs, debounce_secs,
            session_timeout_secs, batch_size, enabled, contribution_id
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8)
         ON CONFLICT(slug, source_path) DO UPDATE SET
            content_type = excluded.content_type,
            scan_interval_secs = excluded.scan_interval_secs,
            debounce_secs = excluded.debounce_secs,
            session_timeout_secs = excluded.session_timeout_secs,
            batch_size = excluded.batch_size,
            enabled = 1,
            contribution_id = excluded.contribution_id,
            updated_at = datetime('now')",
        rusqlite::params![
            slug_str,
            yaml.source_path,
            yaml.content_type,
            resolved_norms.scan_interval_secs,
            resolved_norms.debounce_secs,
            resolved_norms.session_timeout_secs,
            resolved_norms.batch_size,
            contribution_id,
        ],
    )?;
    Ok(())
}

/// UPSERT a DADBEAR policy contribution's payload into the existing
/// `pyramid_dadbear_config` table. Writes into the existing columns
/// (`scan_interval_secs`, `debounce_secs`, etc.) per the spec, and
/// records the `contribution_id` FK on the existing row. A per-slug
/// DADBEAR row is identified by (slug, source_path) — the same
/// composite key the existing CRUD uses.
///
/// Phase 15 wanderer: when `slug` is `None`, this is a global-defaults
/// contribution (from the DADBEAR Oversight "Set Default Norms"
/// button). The `pyramid_dadbear_config` operational table has no
/// global row to update (its `slug` column is NOT NULL with a FK to
/// `pyramid_slugs`), so we treat the operational-table write as a
/// no-op and leave the contribution itself as the source of truth in
/// `pyramid_config_contributions`. A future phase can introduce a
/// layered resolver that merges the active global `dadbear_policy`
/// contribution with per-slug rows at read time, giving users a way
/// to see and edit defaults without a sentinel `pyramid_slugs` row.
pub fn upsert_dadbear_policy(
    conn: &Connection,
    slug: &Option<String>,
    yaml: &DadbearPolicyYaml,
    contribution_id: &str,
) -> Result<()> {
    let Some(slug_str) = slug.as_deref() else {
        // Global defaults: contribution already landed via
        // `accept_config_draft`'s transaction — nothing to mirror in
        // the operational table. Return Ok so the sync dispatcher
        // doesn't error and leak an orphaned active contribution.
        return Ok(());
    };

    conn.execute(
        "INSERT INTO pyramid_dadbear_config (
            slug, source_path, content_type, scan_interval_secs, debounce_secs,
            session_timeout_secs, batch_size, enabled, contribution_id
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(slug, source_path) DO UPDATE SET
            content_type = excluded.content_type,
            scan_interval_secs = excluded.scan_interval_secs,
            debounce_secs = excluded.debounce_secs,
            session_timeout_secs = excluded.session_timeout_secs,
            batch_size = excluded.batch_size,
            enabled = excluded.enabled,
            contribution_id = excluded.contribution_id,
            updated_at = datetime('now')",
        rusqlite::params![
            slug_str,
            yaml.source_path,
            yaml.content_type,
            yaml.scan_interval_secs,
            yaml.debounce_secs,
            yaml.session_timeout_secs,
            yaml.batch_size,
            yaml.enabled as i64,
            contribution_id,
        ],
    )?;
    Ok(())
}

// ── Auto-Update Policy (ghost-engine fix) ────────────────────────────────────

fn auto_update_default_debounce() -> i64 {
    5
}
fn auto_update_default_min_files() -> i64 {
    1
}
fn auto_update_default_threshold() -> f64 {
    0.5
}

/// YAML struct for the `auto_update_policy` contribution schema type.
///
/// Governs the per-pyramid stale engine behavior: debounce, thresholds,
/// and the master enable flag. This is NOT `wire_auto_update_settings`
/// (which controls Wire discovery polling) — this governs the local
/// stale engine's file-watching behavior.
///
/// **Excluded from YAML (runtime/derived state):** `frozen`,
/// `breaker_tripped`, `*_at` timestamps, `ingested_extensions`,
/// `ingested_config_files`. Operational state is managed by
/// `auto_update_ops.rs`; ingested_* are build-derived.
///
/// **Debounce limitation:** `debounce_minutes` is baked into
/// `LayerTimer` at engine construction time. Changes via contribution
/// take effect on next engine restart (toggle auto_update off/on, or
/// app restart). All other fields are re-read per drain cycle.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AutoUpdatePolicyYaml {
    pub auto_update: bool,
    #[serde(default = "auto_update_default_debounce")]
    pub debounce_minutes: i64,
    #[serde(default = "auto_update_default_min_files")]
    pub min_changed_files: i64,
    #[serde(default = "auto_update_default_threshold")]
    pub runaway_threshold: f64,
}

/// UPSERT an auto-update policy contribution's payload.
///
/// DECOMMISSIONED: pyramid_auto_update_config table has been dropped.
/// Policy fields are now read directly from contributions (dadbear_norms).
/// This function is a no-op retained to avoid breaking callers during
/// the transition.
pub fn upsert_auto_update_policy(
    _conn: &Connection,
    _slug: &Option<String>,
    _yaml: &AutoUpdatePolicyYaml,
    _contribution_id: &str,
) -> Result<()> {
    // No-op: policy is read directly from contributions.
    Ok(())
}

/// Store the active dispatch policy YAML. Singleton table (id=1).
pub fn upsert_dispatch_policy(
    conn: &Connection,
    _slug: &Option<String>,
    yaml_content: &str,
    contribution_id: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_dispatch_policy (id, yaml_content, contribution_id, updated_at)
         VALUES (1, ?1, ?2, datetime('now'))
         ON CONFLICT(id) DO UPDATE SET
            yaml_content = excluded.yaml_content,
            contribution_id = excluded.contribution_id,
            updated_at = datetime('now')",
        rusqlite::params![yaml_content, contribution_id],
    )?;
    Ok(())
}

/// Read the active dispatch policy YAML. Returns None if no policy is set.
pub fn read_dispatch_policy(conn: &Connection) -> Result<Option<String>> {
    let result: Option<String> = conn.query_row(
        "SELECT yaml_content FROM pyramid_dispatch_policy WHERE id = 1",
        [],
        |row| row.get(0),
    ).optional()?;
    Ok(result.filter(|s| !s.is_empty()))
}

/// Minimal tier routing YAML struct — a list of tier entries. Used by
/// the `tier_routing` schema_type sync dispatcher. The real
/// `TierRoutingEntry` lives in `provider.rs` with richer fields; this
/// struct is the YAML-serializable surface.
///
/// **Phase 18a fix:** the canonical field name is `entries:` per the
/// bundled `tier_routing` JSON Schema (see
/// `assets/bundled_contributions.json` →
/// `bundled-schema_definition-tier_routing-v1`) and the bundled
/// default tier_routing seed. Phase 4 originally declared this struct
/// as `tiers:`, so the bundled seed parsed silently into an empty
/// list and never reached the operational `pyramid_tier_routing`
/// table during contribution-driven supersessions. The struct now
/// uses `entries:` (matching the schema_definition + the seed) and
/// accepts the legacy `tiers:` alias so any in-flight contributions
/// written against the broken shape still deserialize cleanly.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TierRoutingYaml {
    #[serde(default, alias = "tiers")]
    pub entries: Vec<TierRoutingYamlEntry>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TierRoutingYamlEntry {
    pub tier_name: String,
    pub provider_id: String,
    pub model_id: String,
    #[serde(default)]
    pub context_limit: Option<i64>,
    #[serde(default)]
    pub max_completion_tokens: Option<i64>,
    #[serde(default)]
    pub pricing_json: Option<String>,
    #[serde(default)]
    pub supported_parameters_json: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    /// Phase 18a: bundled `tier_routing` schema_definition exposes a
    /// per-entry `priority` integer for fallback ordering inside a
    /// single contribution. Phase 18a does not yet thread priority
    /// into the operational table — Phase 19 will. Carrying it on
    /// the YAML struct lets bundled + pulled-from-Wire contributions
    /// parse without an unknown-field error.
    #[serde(default)]
    pub priority: Option<i64>,
    /// Phase 18a: bundled `tier_routing` schema accepts pricing as
    /// flat per-token numbers in addition to the structured
    /// `pricing_json` blob. We accept both — flat numbers are
    /// synthesized into a `pricing_json` blob in the upsert.
    #[serde(default)]
    pub prompt_price_per_token: Option<f64>,
    #[serde(default)]
    pub completion_price_per_token: Option<f64>,
}

/// UPSERT a bundle of tier routing entries from a contribution.
/// Delegates to the existing `save_tier_routing` helper per entry so
/// the Phase 3 data model stays authoritative.
///
/// **Phase 18a:** also DELETEs any existing `pyramid_tier_routing`
/// rows whose `tier_name` is NOT in the new contribution. Without
/// this, an active local-mode contribution listing only
/// `fast_extract`/`synth_heavy`/etc. would leave a stale `web` row
/// pointing at OpenRouter, and a chain step asking for the `web`
/// tier would silently route to OpenRouter even though local mode is
/// on. The DELETE matches the rest of the dispatcher's
/// "contribution is the source of truth" model.
pub fn upsert_tier_routing_from_contribution(
    conn: &Connection,
    yaml: &TierRoutingYaml,
    _contribution_id: &str,
) -> Result<()> {
    // Phase 18a: drop tiers not present in the incoming contribution.
    let incoming_names: Vec<String> =
        yaml.entries.iter().map(|e| e.tier_name.clone()).collect();
    if incoming_names.is_empty() {
        // Empty contribution = wipe the table.
        conn.execute("DELETE FROM pyramid_tier_routing", [])?;
    } else {
        let placeholders = incoming_names
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "DELETE FROM pyramid_tier_routing WHERE tier_name NOT IN ({placeholders})"
        );
        let params: Vec<&dyn rusqlite::ToSql> = incoming_names
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();
        conn.execute(&sql, params.as_slice())?;
    }

    // Phase 4: we don't record contribution_id on individual
    // tier_routing rows — the existing schema doesn't have that column.
    // The contribution→tier linkage lives on
    // pyramid_config_contributions itself. Phase 14 can add a back-ref
    // column if the executor needs to trace tier → contribution.
    for entry in &yaml.entries {
        // Phase 18a: synthesize a pricing_json blob from the flat
        // per-token rate fields when the contribution didn't supply
        // its own pricing_json. Keeps the canonical bundled schema
        // (which expresses pricing as flat numbers) and the
        // structured-blob form interoperable.
        let pricing_json = entry
            .pricing_json
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| {
                if entry.prompt_price_per_token.is_some()
                    || entry.completion_price_per_token.is_some()
                {
                    let prompt = entry.prompt_price_per_token.unwrap_or(0.0);
                    let completion = entry.completion_price_per_token.unwrap_or(0.0);
                    serde_json::json!({
                        "prompt": prompt.to_string(),
                        "completion": completion.to_string(),
                        "request": "0"
                    })
                    .to_string()
                } else {
                    "{}".to_string()
                }
            });
        let tier = TierRoutingEntry {
            tier_name: entry.tier_name.clone(),
            provider_id: entry.provider_id.clone(),
            model_id: entry.model_id.clone(),
            context_limit: entry.context_limit.map(|n| n as usize),
            max_completion_tokens: entry.max_completion_tokens.map(|n| n as usize),
            pricing_json,
            supported_parameters_json: entry.supported_parameters_json.clone(),
            notes: entry.notes.clone(),
        };
        save_tier_routing(conn, &tier)?;
    }
    Ok(())
}

/// Minimal step overrides bundle YAML struct.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct StepOverridesBundleYaml {
    pub slug: String,
    pub chain_id: String,
    pub overrides: Vec<StepOverrideYamlEntry>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StepOverrideYamlEntry {
    pub step_name: String,
    pub field_name: String,
    pub value_json: String,
}

/// DELETE all existing step overrides for a (slug, chain_id) pair,
/// then INSERT one row per entry in the bundle. The whole bundle is
/// accepted/superseded as a unit per the spec. contribution_id is not
/// recorded on individual rows — the linkage lives on
/// pyramid_config_contributions (same rationale as tier_routing).
pub fn replace_step_overrides_bundle(
    conn: &Connection,
    bundle: &StepOverridesBundleYaml,
    _contribution_id: &str,
) -> Result<()> {
    // DELETE-then-INSERT under the caller's transaction context.
    conn.execute(
        "DELETE FROM pyramid_step_overrides WHERE slug = ?1 AND chain_id = ?2",
        rusqlite::params![bundle.slug, bundle.chain_id],
    )?;
    for entry in &bundle.overrides {
        let row = StepOverride {
            slug: bundle.slug.clone(),
            chain_id: bundle.chain_id.clone(),
            step_name: entry.step_name.clone(),
            field_name: entry.field_name.clone(),
            value_json: entry.value_json.clone(),
        };
        save_step_override(conn, &row)?;
    }
    Ok(())
}

// ── Chain assignment / defaults YAML structs ─────────────────────────────────
//
// Deserialized from `chain_assignment` and `chain_defaults` contributions
// in `sync_config_to_operational`. The operational tables
// (`pyramid_chain_assignments`, `pyramid_chain_defaults`) are caches;
// the contribution is the source of truth.

/// Per-pyramid chain override. The `chain_id` field names the chain YAML's
/// `id` field (e.g. `"conversation-episodic-fast"`). The special value
/// `"default"` means "remove any override, fall back to content-type defaults."
#[derive(Debug, serde::Deserialize)]
pub struct ChainAssignmentYaml {
    pub chain_id: String,
}

/// Global content-type → chain_id mapping. Ships as a bundled contribution,
/// updatable via Wire, supersedable locally. Replaces the former hardcoded
/// `default_chain_id()` / `default_chain_id_for_mode()` functions.
#[derive(Debug, serde::Deserialize)]
pub struct ChainDefaultsYaml {
    pub mappings: Vec<ChainDefaultMapping>,
}

/// A single (content_type, evidence_mode) → chain_id entry. When
/// `evidence_mode` is omitted or empty, it defaults to `"*"` (wildcard,
/// matches any mode).
#[derive(Debug, serde::Deserialize)]
pub struct ChainDefaultMapping {
    pub content_type: String,
    #[serde(default = "default_wildcard")]
    pub evidence_mode: String,
    pub chain_id: String,
}

fn default_wildcard() -> String {
    "*".to_string()
}

// ── Phase 11: OpenRouter Broadcast + Cost Reconciliation ────────────────────
//
// These helpers implement the database side of `docs/specs/
// evidence-triage-and-dadbear.md` Parts 3 and 4. The invariants they
// maintain, load-bearing for the leak-detection contract:
//
// 1. `insert_cost_log_pending` is the synchronous primary path. It
//    INSERTs a row with `reconciliation_status = 'synchronous'` (or
//    `'synchronous_local'` for zero-cost local calls) and populates
//    `actual_cost` / `actual_tokens_*` directly from the response
//    body. The caller must have already parsed the response so there
//    is always either an authoritative number or `None`.
//
// 2. Correlation is keyed first on `generation_id` (OpenRouter's
//    `gen-xxx`) and falls back to `(slug, step_name, model)` when the
//    generation_id is missing. The fallback only returns the oldest
//    still-unconfirmed row to avoid double-confirming the same
//    broadcast.
//
// 3. `record_broadcast_confirmation` NEVER rewrites `actual_cost`.
//    Even when the broadcast disagrees, we store the broadcast's cost
//    in `broadcast_cost_usd` and flip `reconciliation_status` to
//    `'discrepancy'` — the synchronous ledger is preserved so the user
//    can audit the disagreement.
//
// 4. `sweep_broadcast_missing` is the leak detection pass. It
//    transitions `synchronous` rows past the grace period to
//    `broadcast_missing` so the oversight page can surface them as a
//    red alert.
//
// 5. Orphan broadcasts are inserted when correlation finds no matching
//    row. These are the primary credential-exfiltration indicator.

/// Row returned from correlation queries. `id` is the primary key of
/// the matched `pyramid_cost_log` row; `actual_cost` carries the
/// synchronous value for discrepancy comparison.
#[derive(Debug, Clone)]
pub struct CorrelatedCostLogRow {
    pub id: i64,
    pub slug: String,
    pub step_name: Option<String>,
    pub model: String,
    pub actual_cost: Option<f64>,
    pub provider_id: Option<String>,
    pub reconciliation_status: Option<String>,
    pub broadcast_confirmed_at: Option<String>,
}

/// Insert a synchronous cost log row populated directly from the
/// provider's response body. Called by the LLM call path immediately
/// after parsing a successful response, so `actual_cost` and
/// `reconciliation_status` are authoritative on row creation.
///
/// For Ollama and other local providers that report no cost, pass
/// `actual_cost = Some(0.0)` and status = `"synchronous_local"`.
#[allow(clippy::too_many_arguments)]
pub fn insert_cost_log_synchronous(
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
    actual_cost: Option<f64>,
    actual_tokens_in: Option<i64>,
    actual_tokens_out: Option<i64>,
    provider_id: Option<&str>,
    reconciliation_status: &str,
) -> Result<i64> {
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    conn.execute(
        "INSERT INTO pyramid_cost_log (
             slug, operation, model, input_tokens, output_tokens,
             estimated_cost, source, layer, check_type, created_at,
             chain_id, step_name, tier, latency_ms, generation_id,
             estimated_cost_usd,
             actual_cost, actual_tokens_in, actual_tokens_out,
             provider_id, reconciliation_status, reconciled_at
         ) VALUES (
             ?1, ?2, ?3, ?4, ?5,
             ?6, ?7, ?8, ?9, ?10,
             ?11, ?12, ?13, ?14, ?15,
             ?16,
             ?17, ?18, ?19,
             ?20, ?21, ?22
         )",
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
            actual_cost,
            actual_tokens_in,
            actual_tokens_out,
            provider_id,
            reconciliation_status,
            now,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Find the single `pyramid_cost_log` row a broadcast trace should
/// confirm. Primary correlation is by `generation_id` (the OpenRouter
/// response.id we stored on row creation). Fallback correlation is by
/// `(slug, step_name, model)` taking the oldest unconfirmed row.
///
/// Returns `None` when no match is found — the caller treats this as an
/// orphan broadcast and writes a row into `pyramid_orphan_broadcasts`.
pub fn correlate_broadcast_to_cost_log(
    conn: &Connection,
    generation_id: Option<&str>,
    session_slug: Option<&str>,
    step_name: Option<&str>,
    model: Option<&str>,
) -> Result<Option<CorrelatedCostLogRow>> {
    // Path 1: generation_id exact match (authoritative).
    if let Some(gid) = generation_id {
        if !gid.is_empty() {
            let mut stmt = conn.prepare(
                "SELECT id, slug, step_name, model, actual_cost, provider_id,
                        reconciliation_status, broadcast_confirmed_at
                 FROM pyramid_cost_log
                 WHERE generation_id = ?1
                 ORDER BY id DESC
                 LIMIT 1",
            )?;
            let mut rows = stmt.query(rusqlite::params![gid])?;
            if let Some(row) = rows.next()? {
                return Ok(Some(CorrelatedCostLogRow {
                    id: row.get(0)?,
                    slug: row.get(1)?,
                    step_name: row.get(2)?,
                    model: row.get(3)?,
                    actual_cost: row.get(4)?,
                    provider_id: row.get(5)?,
                    reconciliation_status: row.get(6)?,
                    broadcast_confirmed_at: row.get(7)?,
                }));
            }
        }
    }

    // Path 2: session_id + step_name + model fallback. Returns the
    // oldest still-unconfirmed row so repeated broadcasts for the same
    // (slug, step) pair don't double-confirm.
    let Some(slug) = session_slug else {
        return Ok(None);
    };
    let Some(step) = step_name else {
        return Ok(None);
    };
    let model_filter = model.unwrap_or("");
    let mut stmt = conn.prepare(
        "SELECT id, slug, step_name, model, actual_cost, provider_id,
                reconciliation_status, broadcast_confirmed_at
         FROM pyramid_cost_log
         WHERE slug = ?1
           AND step_name = ?2
           AND (?3 = '' OR model = ?3)
           AND broadcast_confirmed_at IS NULL
         ORDER BY created_at ASC
         LIMIT 1",
    )?;
    let mut rows = stmt.query(rusqlite::params![slug, step, model_filter])?;
    if let Some(row) = rows.next()? {
        return Ok(Some(CorrelatedCostLogRow {
            id: row.get(0)?,
            slug: row.get(1)?,
            step_name: row.get(2)?,
            model: row.get(3)?,
            actual_cost: row.get(4)?,
            provider_id: row.get(5)?,
            reconciliation_status: row.get(6)?,
            broadcast_confirmed_at: row.get(7)?,
        }));
    }
    Ok(None)
}

/// Write a broadcast confirmation onto a matched `pyramid_cost_log`
/// row. If the ratio exceeds the caller's threshold, the row is
/// transitioned to `'discrepancy'` instead of kept at `'synchronous'`.
/// `actual_cost` is NEVER rewritten — the broadcast cost lives in the
/// separate `broadcast_cost_usd` column so the audit trail preserves
/// both sides of a disagreement.
pub fn record_broadcast_confirmation(
    conn: &Connection,
    cost_log_id: i64,
    broadcast_cost_usd: Option<f64>,
    payload_json: &str,
    discrepancy_ratio: Option<f64>,
    flag_discrepancy: bool,
) -> Result<()> {
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    if flag_discrepancy {
        conn.execute(
            "UPDATE pyramid_cost_log
             SET broadcast_confirmed_at = ?1,
                 broadcast_payload_json = ?2,
                 broadcast_cost_usd = ?3,
                 broadcast_discrepancy_ratio = ?4,
                 reconciliation_status = 'discrepancy'
             WHERE id = ?5",
            rusqlite::params![now, payload_json, broadcast_cost_usd, discrepancy_ratio, cost_log_id],
        )?;
    } else {
        conn.execute(
            "UPDATE pyramid_cost_log
             SET broadcast_confirmed_at = ?1,
                 broadcast_payload_json = ?2,
                 broadcast_cost_usd = ?3,
                 broadcast_discrepancy_ratio = ?4
             WHERE id = ?5
               AND reconciliation_status != 'discrepancy'",
            rusqlite::params![now, payload_json, broadcast_cost_usd, discrepancy_ratio, cost_log_id],
        )?;
    }
    Ok(())
}

/// Recovery path: the synchronous primary path failed (parse error,
/// connection drop mid-body) but the broadcast arrived afterwards with
/// authoritative values. Populate `actual_cost` and flip status from
/// `'estimated'` to `'broadcast'`. This is the ONLY path where the
/// broadcast is allowed to set `actual_cost`, and only when the
/// synchronous path explicitly failed.
pub fn record_broadcast_recovery(
    conn: &Connection,
    cost_log_id: i64,
    actual_cost: f64,
    actual_tokens_in: Option<i64>,
    actual_tokens_out: Option<i64>,
    payload_json: &str,
) -> Result<()> {
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    conn.execute(
        "UPDATE pyramid_cost_log
         SET actual_cost = ?1,
             actual_tokens_in = COALESCE(?2, actual_tokens_in),
             actual_tokens_out = COALESCE(?3, actual_tokens_out),
             broadcast_confirmed_at = ?4,
             broadcast_payload_json = ?5,
             broadcast_cost_usd = ?1,
             reconciled_at = ?4,
             reconciliation_status = 'broadcast'
         WHERE id = ?6
           AND reconciliation_status = 'estimated'",
        rusqlite::params![
            actual_cost,
            actual_tokens_in,
            actual_tokens_out,
            now,
            payload_json,
            cost_log_id,
        ],
    )?;
    Ok(())
}

/// Insert an orphan broadcast row. Orphans are broadcasts that arrived
/// with no matching local cost_log row — the primary indicator of
/// credential exfiltration.
#[allow(clippy::too_many_arguments)]
pub fn insert_orphan_broadcast(
    conn: &Connection,
    provider_id: Option<&str>,
    generation_id: Option<&str>,
    session_id: Option<&str>,
    pyramid_slug: Option<&str>,
    build_id: Option<&str>,
    step_name: Option<&str>,
    model: Option<&str>,
    cost_usd: Option<f64>,
    tokens_in: Option<i64>,
    tokens_out: Option<i64>,
    payload_json: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO pyramid_orphan_broadcasts (
             provider_id, generation_id, session_id, pyramid_slug, build_id,
             step_name, model, cost_usd, tokens_in, tokens_out, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        rusqlite::params![
            provider_id,
            generation_id,
            session_id,
            pyramid_slug,
            build_id,
            step_name,
            model,
            cost_usd,
            tokens_in,
            tokens_out,
            payload_json,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Leak detection sweep. Flips `synchronous` rows whose broadcast
/// confirmation never arrived within the grace period to
/// `broadcast_missing`. Skipped entirely when the user opts out of
/// broadcast confirmation via `broadcast_required: false`.
///
/// Returns the number of rows flipped.
pub fn sweep_broadcast_missing(
    conn: &Connection,
    grace_period_secs: i64,
) -> Result<usize> {
    // SQLite datetime modifier format. The column was written with
    // `datetime('now')` (UTC) so we compare against the same epoch.
    let modifier = format!("-{} seconds", grace_period_secs.max(0));
    let affected = conn.execute(
        "UPDATE pyramid_cost_log
         SET reconciliation_status = 'broadcast_missing'
         WHERE reconciliation_status = 'synchronous'
           AND broadcast_confirmed_at IS NULL
           AND created_at < datetime('now', ?1)",
        rusqlite::params![modifier],
    )?;
    Ok(affected)
}

// ── Phase 11: Provider Health State Machine ──────────────────────────

/// Provider health classification. Stored as a lowercase string in
/// `pyramid_providers.provider_health`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderHealth {
    Healthy,
    Degraded,
    Down,
}

impl ProviderHealth {
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderHealth::Healthy => "healthy",
            ProviderHealth::Degraded => "degraded",
            ProviderHealth::Down => "down",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "degraded" => ProviderHealth::Degraded,
            "down" => ProviderHealth::Down,
            _ => ProviderHealth::Healthy,
        }
    }
}

/// Snapshot of a provider's health state for the IPC surface.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProviderHealthEntry {
    pub provider_id: String,
    pub display_name: String,
    pub provider_type: String,
    pub health: String,
    pub reason: Option<String>,
    pub since: Option<String>,
    pub acknowledged_at: Option<String>,
    pub recent_discrepancies: i64,
    pub recent_broadcast_missing: i64,
    pub recent_orphans: i64,
}

/// Set a provider's health state and record the reason. Called by
/// `record_provider_error` when enough signals accumulate to degrade
/// the provider. Does NOT auto-clear — admin must acknowledge via
/// `acknowledge_provider_health`.
pub fn set_provider_health(
    conn: &Connection,
    provider_id: &str,
    health: ProviderHealth,
    reason: &str,
) -> Result<()> {
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    conn.execute(
        "UPDATE pyramid_providers
         SET provider_health = ?1,
             health_reason = ?2,
             health_since = ?3
         WHERE id = ?4",
        rusqlite::params![health.as_str(), reason, now, provider_id],
    )?;
    Ok(())
}

/// Acknowledge a provider health alert. Resets state to `"healthy"`
/// and stamps `health_acknowledged_at` with the current time. Keeps
/// `health_reason` populated for the audit trail.
pub fn acknowledge_provider_health(conn: &Connection, provider_id: &str) -> Result<()> {
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    conn.execute(
        "UPDATE pyramid_providers
         SET provider_health = 'healthy',
             health_acknowledged_at = ?1
         WHERE id = ?2",
        rusqlite::params![now, provider_id],
    )?;
    Ok(())
}

/// Read the current health state for a single provider.
pub fn get_provider_health(conn: &Connection, provider_id: &str) -> Result<Option<(String, Option<String>, Option<String>, Option<String>)>> {
    let mut stmt = conn.prepare(
        "SELECT provider_health, health_reason, health_since, health_acknowledged_at
         FROM pyramid_providers WHERE id = ?1",
    )?;
    let mut rows = stmt.query(rusqlite::params![provider_id])?;
    if let Some(row) = rows.next()? {
        Ok(Some((
            row.get::<_, Option<String>>(0)?.unwrap_or_else(|| "healthy".into()),
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
        )))
    } else {
        Ok(None)
    }
}

/// Full provider health snapshot for the IPC surface. Includes recent
/// discrepancy / broadcast_missing / orphan counts per provider so the
/// UI can render signal density without running separate queries.
pub fn list_provider_health(
    conn: &Connection,
    recent_window_secs: i64,
) -> Result<Vec<ProviderHealthEntry>> {
    let window_modifier = format!("-{} seconds", recent_window_secs.max(0));
    let mut stmt = conn.prepare(
        "SELECT id, display_name, provider_type, provider_health,
                health_reason, health_since, health_acknowledged_at
         FROM pyramid_providers
         ORDER BY id",
    )?;
    let entries = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?.unwrap_or_else(|| "healthy".into()),
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?
        .filter_map(|r| r.ok())
        .collect::<Vec<_>>();

    let mut out = Vec::with_capacity(entries.len());
    for (provider_id, display_name, provider_type, health, reason, since, acked) in entries {
        let recent_discrepancies: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_cost_log
                 WHERE provider_id = ?1
                   AND reconciliation_status = 'discrepancy'
                   AND created_at > datetime('now', ?2)",
                rusqlite::params![provider_id, window_modifier],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let recent_broadcast_missing: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_cost_log
                 WHERE provider_id = ?1
                   AND reconciliation_status = 'broadcast_missing'
                   AND created_at > datetime('now', ?2)",
                rusqlite::params![provider_id, window_modifier],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let recent_orphans: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_orphan_broadcasts
                 WHERE provider_id = ?1
                   AND received_at > datetime('now', ?2)
                   AND acknowledged_at IS NULL",
                rusqlite::params![provider_id, window_modifier],
                |r| r.get(0),
            )
            .unwrap_or(0);

        out.push(ProviderHealthEntry {
            provider_id,
            display_name,
            provider_type,
            health,
            reason,
            since,
            acknowledged_at: acked,
            recent_discrepancies,
            recent_broadcast_missing,
            recent_orphans,
        });
    }
    Ok(out)
}

/// Count discrepancies recorded for a provider inside a window.
/// Called by the provider health state machine to decide whether to
/// degrade after an N-in-M-seconds trigger.
pub fn count_recent_cost_discrepancies(
    conn: &Connection,
    provider_id: &str,
    window_secs: i64,
) -> Result<i64> {
    let modifier = format!("-{} seconds", window_secs.max(0));
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pyramid_cost_log
             WHERE provider_id = ?1
               AND reconciliation_status = 'discrepancy'
               AND created_at > datetime('now', ?2)",
            rusqlite::params![provider_id, modifier],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Ok(count)
}

/// Phase 11 wanderer fix: record a single provider error observation
/// into the rolling event log. Called by `provider_health::
/// record_provider_error` for HTTP 5xx events before the threshold
/// check runs. Connection failures and cost discrepancies continue to
/// use their own single-occurrence / cost_log-based paths and do NOT
/// write here — we only record errors whose spec behavior depends on
/// a rolling count.
pub fn record_provider_error_event(
    conn: &Connection,
    provider_id: &str,
    error_kind: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_provider_error_log (provider_id, error_kind)
         VALUES (?1, ?2)",
        rusqlite::params![provider_id, error_kind],
    )?;
    Ok(())
}

/// Phase 11 wanderer fix: count recent provider error events of a
/// given kind within a rolling window. Used by the HTTP 5xx degrade
/// threshold so the state machine only flips `provider_health` after
/// the spec's 3-in-window signal, not on the first occurrence.
pub fn count_recent_provider_errors(
    conn: &Connection,
    provider_id: &str,
    error_kind: &str,
    window_secs: i64,
) -> Result<i64> {
    let modifier = format!("-{} seconds", window_secs.max(0));
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pyramid_provider_error_log
             WHERE provider_id = ?1
               AND error_kind = ?2
               AND created_at > datetime('now', ?3)",
            rusqlite::params![provider_id, error_kind, modifier],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Ok(count)
}

#[cfg(test)]
mod provider_registry_tests {
    use super::*;

    fn mem_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        conn
    }

    #[test]
    fn init_seeds_default_providers_on_empty_db() {
        let conn = mem_conn();
        let providers = list_providers(&conn).unwrap();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].id, "openrouter");
        assert_eq!(providers[0].api_key_ref.as_deref(), Some("OPENROUTER_KEY"));
    }

    #[test]
    fn init_seeds_four_tiers_but_not_stale_local() {
        let conn = mem_conn();
        let tiers = get_tier_routing(&conn).unwrap();
        assert!(tiers.contains_key("fast_extract"));
        assert!(tiers.contains_key("web"));
        assert!(tiers.contains_key("synth_heavy"));
        assert!(tiers.contains_key("stale_remote"));
        assert!(
            !tiers.contains_key("stale_local"),
            "stale_local must NOT be seeded — only exists after a local provider is registered"
        );
        assert_eq!(tiers.len(), 4);
    }

    #[test]
    fn seed_tiers_use_adams_model_slugs() {
        let conn = mem_conn();
        let tiers = get_tier_routing(&conn).unwrap();
        assert_eq!(tiers["fast_extract"].model_id, "inception/mercury-2");
        assert_eq!(tiers["web"].model_id, "x-ai/grok-4.1-fast");
        assert_eq!(tiers["synth_heavy"].model_id, "minimax/minimax-m2.7");
        assert_eq!(tiers["stale_remote"].model_id, "minimax/minimax-m2.7");
        assert_eq!(tiers["web"].context_limit, Some(2_000_000));
    }

    #[test]
    fn init_does_not_reseed_populated_db() {
        let conn = mem_conn();
        // Overwrite a field to prove reseed doesn't clobber it.
        conn.execute(
            "UPDATE pyramid_providers SET display_name = 'User-customized' WHERE id = 'openrouter'",
            [],
        )
        .unwrap();
        seed_default_provider_registry(&conn).unwrap();
        let providers = list_providers(&conn).unwrap();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].display_name, "User-customized");
    }

    #[test]
    fn save_and_get_provider_round_trip() {
        let conn = mem_conn();
        let provider = Provider {
            id: "ollama-local".into(),
            display_name: "Ollama Local".into(),
            provider_type: ProviderType::OpenaiCompat,
            base_url: "http://localhost:11434/v1".into(),
            api_key_ref: None,
            auto_detect_context: true,
            supports_broadcast: false,
            broadcast_config_json: None,
            config_json: r#"{"extra_headers":{}}"#.into(),
            enabled: true,
        };
        save_provider(&conn, &provider).unwrap();
        let loaded = get_provider(&conn, "ollama-local").unwrap().unwrap();
        assert_eq!(loaded.provider_type, ProviderType::OpenaiCompat);
        assert_eq!(loaded.base_url, "http://localhost:11434/v1");
        assert!(loaded.api_key_ref.is_none());
        assert!(loaded.auto_detect_context);
    }

    #[test]
    fn save_and_get_tier_routing_round_trip() {
        let conn = mem_conn();
        let entry = TierRoutingEntry {
            tier_name: "custom_tier".into(),
            provider_id: "openrouter".into(),
            model_id: "anthropic/claude-sonnet-4-5".into(),
            context_limit: Some(200_000),
            max_completion_tokens: Some(64_000),
            pricing_json: r#"{"prompt":"0.000003","completion":"0.000015"}"#.into(),
            supported_parameters_json: Some(r#"["tools","response_format"]"#.into()),
            notes: Some("quality tier".into()),
        };
        save_tier_routing(&conn, &entry).unwrap();
        let tiers = get_tier_routing(&conn).unwrap();
        let loaded = tiers.get("custom_tier").unwrap();
        assert_eq!(loaded.model_id, "anthropic/claude-sonnet-4-5");
        assert_eq!(loaded.context_limit, Some(200_000));
        assert_eq!(loaded.max_completion_tokens, Some(64_000));
    }

    #[test]
    fn save_and_get_step_override_round_trip() {
        let conn = mem_conn();
        let override_row = StepOverride {
            slug: "my-slug".into(),
            chain_id: "code_pyramid".into(),
            step_name: "deep_synthesis".into(),
            field_name: "model_tier".into(),
            value_json: r#""synth_heavy""#.into(),
        };
        save_step_override(&conn, &override_row).unwrap();
        let loaded = get_step_override(
            &conn,
            "my-slug",
            "code_pyramid",
            "deep_synthesis",
            "model_tier",
        )
        .unwrap()
        .unwrap();
        assert_eq!(loaded.value_json, r#""synth_heavy""#);
    }

    #[test]
    fn delete_provider_cascades_to_tier_routing() {
        let conn = mem_conn();
        // The seeded openrouter provider + seeded tiers are linked.
        delete_provider(&conn, "openrouter").unwrap();
        let tiers = get_tier_routing(&conn).unwrap();
        assert!(
            tiers.is_empty(),
            "deleting a provider should cascade to tier routing rows via FK"
        );
    }

    // ── Phase 5 wanderer fix: DADBEAR migration writes canonical metadata ──
    //
    // Per `docs/specs/wire-contribution-mapping.md` Creation-Time
    // Capture table, bootstrap migrations from legacy tables must
    // write canonical metadata (not `'{}'`) with `maturity = canon`.
    // Phase 5's original pass updated `config_contributions.rs` helpers
    // to populate canonical metadata but missed this low-level direct
    // INSERT inside `migrate_legacy_dadbear_to_contributions`. This
    // test inserts a legacy DADBEAR row, runs the migration, and
    // verifies the resulting contribution row has real canonical
    // metadata with `maturity: canon`, not the `'{}'` stub.
    #[test]
    fn phase5_dadbear_migration_writes_canonical_metadata_not_empty_json() {
        // Build a bare sqlite connection and initialize the schema.
        // Can't use `mem_conn()` here because we need to insert into
        // `pyramid_dadbear_config` BEFORE `init_pyramid_db` runs the
        // migration, which only happens on a completely fresh DB.
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();

        // The migration should have already run during init_pyramid_db.
        // Since the DADBEAR table was empty at init time, no rows
        // were migrated. We need to:
        //   1. Clear the sentinel so the migration can run again
        //   2. Insert a legacy DADBEAR row
        //   3. Re-run the migration
        //   4. Verify the resulting contribution has canonical metadata
        conn.execute(
            "DELETE FROM pyramid_config_contributions WHERE schema_type = '_migration_marker'",
            [],
        )
        .unwrap();

        // Insert a legacy DADBEAR row directly (bypassing the Phase 4
        // helper so it lands without a contribution_id).
        conn.execute(
            "INSERT INTO pyramid_dadbear_config (
                slug, source_path, content_type, scan_interval_secs,
                debounce_secs, session_timeout_secs, batch_size, enabled,
                created_at, updated_at
             ) VALUES (
                'test-slug', '/tmp/test-source', 'code', 10,
                30, 1800, 5, 1,
                datetime('now'), datetime('now')
             )",
            [],
        )
        .unwrap();

        // Re-run the migration.
        migrate_legacy_dadbear_to_contributions(&conn).unwrap();

        // Load the migrated contribution and check its metadata.
        let (metadata_json, schema_type, source): (String, String, String) = conn
            .query_row(
                "SELECT wire_native_metadata_json, schema_type, source
                 FROM pyramid_config_contributions
                 WHERE schema_type = 'dadbear_policy' AND source = 'migration'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();

        assert_eq!(schema_type, "dadbear_policy");
        assert_eq!(source, "migration");
        assert_ne!(
            metadata_json, "{}",
            "Phase 5 wanderer fix: DADBEAR bootstrap migration must write canonical metadata, not the '{{}}' stub"
        );

        // Parse the metadata and verify maturity = Canon (per spec).
        let metadata =
            crate::pyramid::wire_native_metadata::WireNativeMetadata::from_json(&metadata_json)
                .unwrap();
        assert!(
            matches!(
                metadata.maturity,
                crate::pyramid::wire_native_metadata::WireMaturity::Canon
            ),
            "Phase 5 spec says bootstrap migration writes maturity=canon; got {:?}",
            metadata.maturity
        );
        // The contribution_type should be Template (from the mapping
        // table) since dadbear_policy maps to a template contribution.
        assert!(
            matches!(
                metadata.contribution_type,
                crate::pyramid::wire_native_metadata::WireContributionType::Template
            ),
            "dadbear_policy should map to contribution_type=template per mapping table; got {:?}",
            metadata.contribution_type
        );
    }
}

// ── Phase 6: pyramid_step_cache CRUD tests ─────────────────────────────────

#[cfg(test)]
mod step_cache_tests {
    use super::*;
    use crate::pyramid::step_context::{
        compute_cache_key, compute_inputs_hash, verify_cache_hit, CacheEntry, CacheHitResult,
    };

    fn mem_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        conn
    }

    fn make_entry(slug: &str, key_seed: &str) -> CacheEntry {
        let inputs_hash = compute_inputs_hash("system", &format!("user-{key_seed}"));
        let prompt_hash = "phash-1".to_string();
        let model_id = "openrouter/test-1".to_string();
        let cache_key = compute_cache_key(&inputs_hash, &prompt_hash, &model_id);
        CacheEntry {
            slug: slug.into(),
            build_id: "build-1".into(),
            step_name: "step-a".into(),
            chunk_index: -1,
            depth: 0,
            cache_key,
            inputs_hash,
            prompt_hash,
            model_id,
            output_json: serde_json::json!({"content":"hello","usage":{"prompt_tokens":1,"completion_tokens":2}})
                .to_string(),
            token_usage_json: Some("{\"prompt_tokens\":1,\"completion_tokens\":2}".into()),
            cost_usd: None,
            latency_ms: Some(42),
            force_fresh: false,
            supersedes_cache_id: None,
            note: None,
        }
    }

    #[test]
    fn test_table_initialized_on_init_pyramid_db() {
        let conn = mem_conn();
        // Schema check: SELECT against the table should not error.
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_step_cache",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_store_and_check_round_trip() {
        let conn = mem_conn();
        let entry = make_entry("test-slug", "round-trip");
        store_cache(&conn, &entry).unwrap();

        let fetched = check_cache(&conn, "test-slug", &entry.cache_key)
            .unwrap()
            .expect("entry should exist after store");
        assert_eq!(fetched.slug, "test-slug");
        assert_eq!(fetched.cache_key, entry.cache_key);
        assert_eq!(fetched.inputs_hash, entry.inputs_hash);
        assert_eq!(fetched.prompt_hash, entry.prompt_hash);
        assert_eq!(fetched.model_id, entry.model_id);
        assert_eq!(fetched.output_json, entry.output_json);
        assert_eq!(fetched.latency_ms, Some(42));
        assert!(!fetched.force_fresh);
        assert_eq!(fetched.supersedes_cache_id, None);
    }

    #[test]
    fn test_check_cache_returns_none_on_miss() {
        let conn = mem_conn();
        let result = check_cache(&conn, "nope", "no-such-key").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_unique_constraint_on_slug_cache_key_replaces() {
        // Inserting twice under the same (slug, cache_key) must update,
        // not duplicate (the ON CONFLICT clause is the spec's INSERT OR
        // REPLACE semantics).
        let conn = mem_conn();
        let mut entry = make_entry("ts", "dup");
        store_cache(&conn, &entry).unwrap();
        // Mutate the output and store again under the same cache_key.
        entry.output_json = serde_json::json!({"content":"updated","usage":{"prompt_tokens":3,"completion_tokens":4}}).to_string();
        store_cache(&conn, &entry).unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_step_cache WHERE slug = ?1 AND cache_key = ?2",
                rusqlite::params!["ts", entry.cache_key],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "ON CONFLICT must update, not insert a duplicate");

        let fetched = check_cache(&conn, "ts", &entry.cache_key).unwrap().unwrap();
        assert!(fetched.output_json.contains("updated"));
    }

    #[test]
    fn test_delete_cache_entry() {
        let conn = mem_conn();
        let entry = make_entry("ts", "delete");
        store_cache(&conn, &entry).unwrap();
        delete_cache_entry(&conn, "ts", &entry.cache_key).unwrap();
        let fetched = check_cache(&conn, "ts", &entry.cache_key).unwrap();
        assert!(fetched.is_none(), "row should be gone after delete");
    }

    #[test]
    fn test_check_cache_hit_and_verify_valid() {
        let conn = mem_conn();
        let entry = make_entry("ts", "verify");
        store_cache(&conn, &entry).unwrap();

        let fetched = check_cache(&conn, "ts", &entry.cache_key).unwrap().unwrap();
        let verdict = verify_cache_hit(
            &fetched,
            &entry.inputs_hash,
            &entry.prompt_hash,
            &entry.model_id,
        );
        assert_eq!(verdict, CacheHitResult::Valid);
    }

    #[test]
    fn test_cache_hit_verification_rejects_input_mismatch() {
        let conn = mem_conn();
        let entry = make_entry("ts", "mismatch_inputs");
        store_cache(&conn, &entry).unwrap();
        let fetched = check_cache(&conn, "ts", &entry.cache_key).unwrap().unwrap();
        // Different inputs hash → mismatch.
        let verdict =
            verify_cache_hit(&fetched, "different-inputs", &entry.prompt_hash, &entry.model_id);
        assert_eq!(verdict, CacheHitResult::MismatchInputs);
    }

    #[test]
    fn test_cache_hit_verification_rejects_prompt_mismatch() {
        let conn = mem_conn();
        let entry = make_entry("ts", "mismatch_prompt");
        store_cache(&conn, &entry).unwrap();
        let fetched = check_cache(&conn, "ts", &entry.cache_key).unwrap().unwrap();
        let verdict =
            verify_cache_hit(&fetched, &entry.inputs_hash, "different-prompt", &entry.model_id);
        assert_eq!(verdict, CacheHitResult::MismatchPrompt);
    }

    #[test]
    fn test_cache_hit_verification_rejects_model_mismatch() {
        let conn = mem_conn();
        let entry = make_entry("ts", "mismatch_model");
        store_cache(&conn, &entry).unwrap();
        let fetched = check_cache(&conn, "ts", &entry.cache_key).unwrap().unwrap();
        let verdict = verify_cache_hit(
            &fetched,
            &entry.inputs_hash,
            &entry.prompt_hash,
            "openrouter/different-model",
        );
        assert_eq!(verdict, CacheHitResult::MismatchModel);
    }

    #[test]
    fn test_cache_hit_verification_rejects_corrupted_output() {
        // Construct a row with malformed output_json directly via the
        // store helper. The verifier should flag it as corruption.
        let conn = mem_conn();
        let mut entry = make_entry("ts", "corrupted");
        entry.output_json = "this is not json {{{".to_string();
        store_cache(&conn, &entry).unwrap();
        let fetched = check_cache(&conn, "ts", &entry.cache_key).unwrap().unwrap();
        let verdict = verify_cache_hit(
            &fetched,
            &entry.inputs_hash,
            &entry.prompt_hash,
            &entry.model_id,
        );
        assert_eq!(verdict, CacheHitResult::CorruptedOutput);
    }

    #[test]
    fn test_supersede_cache_entry_links_back() {
        // Force-fresh path: an existing entry is superseded by a new
        // entry under the same cache_key. The supersession helper moves
        // the prior row to an archival key, retains it, and writes the
        // new row with `supersedes_cache_id` pointing at it.
        let conn = mem_conn();
        let prior = make_entry("ts", "supersede");
        store_cache(&conn, &prior).unwrap();
        let prior_id: i64 = conn
            .query_row(
                "SELECT id FROM pyramid_step_cache WHERE slug = ?1 AND cache_key = ?2",
                rusqlite::params!["ts", prior.cache_key],
                |row| row.get(0),
            )
            .unwrap();

        // The "fresh" entry has the same content-addressable cache key
        // (same inputs/prompt/model) but represents a reroll.
        let mut new_entry = prior.clone();
        new_entry.output_json =
            serde_json::json!({"content":"reroll","usage":{"prompt_tokens":5,"completion_tokens":6}})
                .to_string();
        supersede_cache_entry(&conn, "ts", &prior.cache_key, &new_entry).unwrap();

        // The new row is the active content-addressable lookup target.
        let active = check_cache(&conn, "ts", &prior.cache_key).unwrap().unwrap();
        assert!(active.output_json.contains("reroll"));
        assert!(active.force_fresh, "force_fresh flag should be set");
        assert_eq!(
            active.supersedes_cache_id,
            Some(prior_id),
            "new entry should link back to the prior id"
        );

        // The prior row still exists under the archival cache_key.
        let archival_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_step_cache WHERE slug = ?1 AND id = ?2 AND cache_key LIKE 'archived:%'",
                rusqlite::params!["ts", prior_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            archival_count, 1,
            "prior row should be retained under archival cache_key"
        );
    }

    #[test]
    fn test_supersede_with_no_prior_row_just_inserts() {
        // If no prior row exists, supersede_cache_entry is equivalent
        // to a force_fresh store_cache. The new row stands alone with
        // supersedes_cache_id = None.
        let conn = mem_conn();
        let entry = make_entry("ts", "first_reroll");
        supersede_cache_entry(&conn, "ts", &entry.cache_key, &entry).unwrap();
        let fetched = check_cache(&conn, "ts", &entry.cache_key).unwrap().unwrap();
        assert!(fetched.force_fresh);
        assert_eq!(fetched.supersedes_cache_id, None);
    }

    #[test]
    fn test_check_cache_returns_most_recent_row() {
        // Defensive ORDER BY id DESC: ensure the lookup returns the
        // most recent entry. (The unique constraint guarantees at most
        // one row per content-addressable key, but the ORDER BY is the
        // canonical tie-break.)
        let conn = mem_conn();
        let entry = make_entry("ts", "ordering");
        store_cache(&conn, &entry).unwrap();
        let fetched = check_cache(&conn, "ts", &entry.cache_key).unwrap().unwrap();
        // Just confirm the fields round-trip — the assertion above is
        // enough; this test exists to lock down the SELECT shape.
        assert_eq!(fetched.cache_key, entry.cache_key);
    }

    #[test]
    fn test_store_cache_if_absent_returns_true_on_fresh_insert() {
        // `store_cache_if_absent` with no prior row should return `true`
        // (row was actually inserted) and the row should be present in
        // the table afterward.
        let conn = mem_conn();
        let entry = make_entry("ts", "fresh");
        let inserted = store_cache_if_absent(&conn, &entry).unwrap();
        assert!(inserted, "expected true for a fresh insert");

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_step_cache
                 WHERE slug = 'ts' AND cache_key = ?1",
                [&entry.cache_key],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_store_cache_if_absent_returns_false_on_conflict_and_preserves_row() {
        // Phase 7 contract (spec "Idempotency" section ~line 341):
        // `INSERT OR IGNORE` semantics must leave pre-existing rows
        // untouched on conflict. This is the spec mandate the Phase 7
        // import flow relies on to protect local rerolls during resume.
        let conn = mem_conn();

        // Plant a row that looks like a local force-reroll — same
        // cache_key as what the import will bring, but with `force_fresh`
        // set and a distinct output_json.
        let mut rerolled = make_entry("ts", "conflict");
        rerolled.output_json =
            serde_json::json!({"content":"LOCAL_REROLL","usage":{}}).to_string();
        rerolled.force_fresh = true;
        rerolled.build_id = "local-reroll".into();
        store_cache(&conn, &rerolled).unwrap();

        // Now attempt to import a row at the same cache_key with the
        // imported-style payload. Under `INSERT OR IGNORE` semantics
        // this must NOT clobber the rerolled row.
        let mut imported = make_entry("ts", "conflict");
        imported.output_json =
            serde_json::json!({"content":"IMPORTED","usage":{}}).to_string();
        imported.force_fresh = false;
        imported.build_id = "import:wire:p1".into();
        let inserted = store_cache_if_absent(&conn, &imported).unwrap();
        assert!(!inserted, "expected false because a prior row exists");

        // Row count is still 1 — no duplicate.
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_step_cache
                 WHERE slug = 'ts' AND cache_key = ?1",
                [&rerolled.cache_key],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        // The active row is STILL the local reroll, not the import.
        let (active_output, active_force_fresh, active_build_id): (
            String,
            i64,
            String,
        ) = conn
            .query_row(
                "SELECT output_json, force_fresh, build_id FROM pyramid_step_cache
                 WHERE slug = 'ts' AND cache_key = ?1",
                [&rerolled.cache_key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert!(
            active_output.contains("LOCAL_REROLL"),
            "store_cache_if_absent clobbered the local reroll output_json: {active_output}"
        );
        assert_eq!(active_force_fresh, 1, "force_fresh flag was cleared");
        assert_eq!(
            active_build_id, "local-reroll",
            "build_id was overwritten"
        );
    }
}

// ── Phase 7: pyramid_import_state CRUD tests ────────────────────────────────

#[cfg(test)]
mod import_state_tests {
    use super::*;

    fn mem_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        conn
    }

    #[test]
    fn test_create_and_load_import_state() {
        let conn = mem_conn();
        create_import_state(&conn, "test-slug", "wire:pyr-abc", "/tmp/src").unwrap();
        let state = load_import_state(&conn, "test-slug").unwrap().unwrap();
        assert_eq!(state.target_slug, "test-slug");
        assert_eq!(state.wire_pyramid_id, "wire:pyr-abc");
        assert_eq!(state.source_path, "/tmp/src");
        assert_eq!(state.status, "downloading_manifest");
        assert_eq!(state.nodes_total, None);
        assert_eq!(state.nodes_processed, 0);
        assert_eq!(state.cache_entries_inserted, 0);
    }

    #[test]
    fn test_load_missing_import_state_returns_none() {
        let conn = mem_conn();
        let state = load_import_state(&conn, "missing").unwrap();
        assert!(state.is_none());
    }

    #[test]
    fn test_create_duplicate_import_state_fails() {
        let conn = mem_conn();
        create_import_state(&conn, "dup", "wire:a", "/tmp/a").unwrap();
        let err = create_import_state(&conn, "dup", "wire:a", "/tmp/a");
        assert!(err.is_err(), "second create should fail on PRIMARY KEY");
    }

    #[test]
    fn test_update_import_state_coalesces_nulls() {
        let conn = mem_conn();
        create_import_state(&conn, "upd", "wire:x", "/tmp").unwrap();

        // Update only the status — other fields should be left alone.
        update_import_state(
            &conn,
            "upd",
            &ImportStateProgress {
                status: Some("validating_sources".into()),
                nodes_total: Some(10),
                cache_entries_total: Some(50),
                ..Default::default()
            },
        )
        .unwrap();

        let state = load_import_state(&conn, "upd").unwrap().unwrap();
        assert_eq!(state.status, "validating_sources");
        assert_eq!(state.nodes_total, Some(10));
        assert_eq!(state.cache_entries_total, Some(50));
        // Unchanged fields stay at their defaults.
        assert_eq!(state.nodes_processed, 0);
        assert_eq!(state.cache_entries_inserted, 0);

        // Second update bumps counters without touching the totals.
        update_import_state(
            &conn,
            "upd",
            &ImportStateProgress {
                status: Some("populating_cache".into()),
                nodes_processed: Some(7),
                cache_entries_validated: Some(35),
                cache_entries_inserted: Some(30),
                ..Default::default()
            },
        )
        .unwrap();

        let state = load_import_state(&conn, "upd").unwrap().unwrap();
        assert_eq!(state.status, "populating_cache");
        // Totals were NOT in this progress → they stay.
        assert_eq!(state.nodes_total, Some(10));
        assert_eq!(state.cache_entries_total, Some(50));
        assert_eq!(state.nodes_processed, 7);
        assert_eq!(state.cache_entries_validated, 35);
        assert_eq!(state.cache_entries_inserted, 30);
    }

    #[test]
    fn test_delete_import_state_is_idempotent() {
        let conn = mem_conn();
        // Deleting a missing row is a no-op.
        delete_import_state(&conn, "gone").unwrap();

        // Create + delete + re-create should work.
        create_import_state(&conn, "roundtrip", "wire:a", "/tmp").unwrap();
        assert!(load_import_state(&conn, "roundtrip").unwrap().is_some());
        delete_import_state(&conn, "roundtrip").unwrap();
        assert!(load_import_state(&conn, "roundtrip").unwrap().is_none());
        create_import_state(&conn, "roundtrip", "wire:b", "/tmp").unwrap();
        assert!(load_import_state(&conn, "roundtrip").unwrap().is_some());
    }
}

// ── Phase 12: demand signal and deferred question tests ──────────────────

#[cfg(test)]
mod phase12_tests {
    use super::*;

    fn mem_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        conn
    }

    #[test]
    fn test_insert_demand_signal_and_sum() {
        let conn = mem_conn();
        insert_demand_signal(
            &conn,
            "test-slug",
            "node-1",
            "agent_query",
            Some("agent-alice"),
            1.0,
            Some("node-1"),
        )
        .unwrap();
        insert_demand_signal(
            &conn,
            "test-slug",
            "node-1",
            "agent_query",
            Some("agent-bob"),
            0.7,
            Some("node-1"),
        )
        .unwrap();

        let total =
            sum_demand_weight(&conn, "test-slug", "node-1", "agent_query", "-7 days").unwrap();
        assert!((total - 1.7).abs() < 1e-9);

        // Different signal type returns 0
        let user_total =
            sum_demand_weight(&conn, "test-slug", "node-1", "user_drill", "-7 days").unwrap();
        assert_eq!(user_total, 0.0);
    }

    #[test]
    fn test_load_parents_via_evidence_keeps_only_keep_verdicts() {
        let conn = mem_conn();
        // KEEP edge: child → parent
        conn.execute(
            "INSERT INTO pyramid_evidence
                (slug, build_id, source_node_id, target_node_id, verdict, weight, reason)
             VALUES (?1, '', 'child', 'parent', 'KEEP', 1.0, 'test')",
            rusqlite::params!["s"],
        )
        .unwrap();
        // DISCONNECT edge (should be filtered)
        conn.execute(
            "INSERT INTO pyramid_evidence
                (slug, build_id, source_node_id, target_node_id, verdict, weight, reason)
             VALUES (?1, '', 'child', 'other_parent', 'DISCONNECT', 0.0, 'test')",
            rusqlite::params!["s"],
        )
        .unwrap();

        let parents = load_parents_via_evidence(&conn, "s", "child").unwrap();
        assert_eq!(parents, vec!["parent".to_string()]);
    }

    #[test]
    fn test_defer_question_and_list_expired() {
        let conn = mem_conn();

        // Defer a question with a very short interval.
        defer_question(
            &conn,
            "s",
            "Q1",
            r#"{"question_id":"Q1","question_text":"?","layer":1,"about":"","creates":""}"#,
            "1m",
            Some("test"),
            Some("contrib-1"),
        )
        .unwrap();

        // Initially `next_check_at` is 1 minute in the future → not expired.
        let expired_now = list_expired_deferred(&conn, "s").unwrap();
        assert_eq!(expired_now.len(), 0);

        // Rewrite next_check_at to the past to simulate expiration.
        conn.execute(
            "UPDATE pyramid_deferred_questions SET next_check_at = '2020-01-01 00:00:00'
             WHERE slug = 's' AND question_id = 'Q1'",
            [],
        )
        .unwrap();

        let expired_later = list_expired_deferred(&conn, "s").unwrap();
        assert_eq!(expired_later.len(), 1);
        assert_eq!(expired_later[0].question_id, "Q1");
    }

    #[test]
    fn test_defer_question_never_is_excluded_from_expired() {
        let conn = mem_conn();
        defer_question(
            &conn,
            "s",
            "Q_NEVER",
            r#"{"question_id":"Q_NEVER","question_text":"?","layer":1,"about":"","creates":""}"#,
            "never",
            Some("skip"),
            None,
        )
        .unwrap();

        // Manually set next_check_at into the past.
        conn.execute(
            "UPDATE pyramid_deferred_questions SET next_check_at = '2020-01-01 00:00:00'
             WHERE slug = 's' AND question_id = 'Q_NEVER'",
            [],
        )
        .unwrap();

        // Expired-scan should NOT include 'never' rows.
        let expired = list_expired_deferred(&conn, "s").unwrap();
        assert_eq!(expired.len(), 0);

        // list_all_deferred SHOULD include it.
        let all = list_all_deferred(&conn, "s").unwrap();
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn test_remove_and_update_deferred() {
        let conn = mem_conn();
        defer_question(
            &conn,
            "s",
            "Q",
            r#"{"question_id":"Q","question_text":"?","layer":1,"about":"","creates":""}"#,
            "30d",
            Some("slow"),
            Some("c-1"),
        )
        .unwrap();

        // Update the interval + contribution id.
        update_deferred_next_check(&conn, "s", "Q", "7d", Some("c-2")).unwrap();

        let all = list_all_deferred(&conn, "s").unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].check_interval, "7d");
        assert_eq!(all[0].contribution_id.as_deref(), Some("c-2"));

        // Remove it.
        remove_deferred(&conn, "s", "Q").unwrap();
        let all = list_all_deferred(&conn, "s").unwrap();
        assert_eq!(all.len(), 0);
    }

    #[test]
    fn test_load_active_evidence_policy_fills_defaults_when_missing() {
        let conn = mem_conn();
        // No row → default policy.
        let policy = load_active_evidence_policy(&conn, Some("nonexistent-slug")).unwrap();
        assert!(policy.triage_rules.is_empty());
        assert!(policy.demand_signals.is_empty());
        assert_eq!(policy.demand_signal_attenuation.factor, 0.5);
        assert_eq!(policy.demand_signal_attenuation.floor, 0.1);
        assert_eq!(policy.demand_signal_attenuation.max_depth, 6);
    }

    #[test]
    fn test_load_active_evidence_policy_parses_stored_rules() {
        let conn = mem_conn();
        // Insert a contribution row so the FK is satisfied.
        conn.execute(
            "INSERT INTO pyramid_config_contributions
                (contribution_id, slug, schema_type, yaml_content, status)
             VALUES ('c-pol-1', 'test', 'evidence_policy', '', 'active')",
            [],
        )
        .unwrap();

        let yaml = EvidencePolicyYaml {
            triage_rules: Some(vec![TriageRuleYaml {
                condition: "first_build AND depth == 1".into(),
                action: "answer".into(),
                model_tier: Some("fast_extract".into()),
                check_interval: None,
                priority: None,
            }]),
            demand_signals: Some(vec![DemandSignalRuleYaml {
                r#type: "agent_query".into(),
                threshold: 2.0,
                window: "-14 days".into(),
            }]),
            budget: Some(PolicyBudgetYaml {
                maintenance_model_tier: Some("stale_local".into()),
                ..Default::default()
            }),
            demand_signal_attenuation: None,
        };
        upsert_evidence_policy(&conn, &Some("test".to_string()), &yaml, "c-pol-1").unwrap();

        let loaded = load_active_evidence_policy(&conn, Some("test")).unwrap();
        assert_eq!(loaded.triage_rules.len(), 1);
        assert_eq!(loaded.triage_rules[0].action, "answer");
        assert_eq!(loaded.demand_signals.len(), 1);
        assert_eq!(loaded.demand_signals[0].threshold, 2.0);
        assert_eq!(
            loaded.budget.maintenance_model_tier.as_deref(),
            Some("stale_local")
        );
        assert_eq!(loaded.contribution_id.as_deref(), Some("c-pol-1"));
        assert!(!loaded.policy_yaml_hash.is_empty());
    }

    #[test]
    fn test_parse_check_interval_never_and_on_demand() {
        assert_eq!(
            parse_check_interval_to_next_check_at("never"),
            "9999-12-31 00:00:00"
        );
        assert_eq!(
            parse_check_interval_to_next_check_at("on_demand"),
            "9999-12-31 00:00:00"
        );
    }

    #[test]
    fn test_parse_check_interval_short_forms() {
        assert_eq!(parse_check_interval_to_next_check_at("7d"), "+7 days");
        assert_eq!(parse_check_interval_to_next_check_at("30d"), "+30 days");
        assert_eq!(parse_check_interval_to_next_check_at("1h"), "+1 hours");
        assert_eq!(parse_check_interval_to_next_check_at("2w"), "+14 days");
    }
}

// ── Phase 13: cost rollup + pause-all + cache summary tests ──────────

#[cfg(test)]
mod phase13_tests {
    use super::*;
    use crate::pyramid::step_context::{compute_cache_key, compute_inputs_hash, CacheEntry};

    fn mem_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO pyramid_slugs (slug, content_type, source_path) VALUES ('p13-test', 'document', '/tmp/p13-test')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO pyramid_slugs (slug, content_type, source_path) VALUES ('p13-other', 'document', '/tmp/p13-other')",
            [],
        )
        .unwrap();
        conn
    }

    fn seed_cost_row(
        conn: &Connection,
        slug: &str,
        operation: &str,
        provider: Option<&str>,
        estimated: f64,
        actual: Option<f64>,
    ) {
        conn.execute(
            "INSERT INTO pyramid_cost_log
                (slug, operation, model, input_tokens, output_tokens, estimated_cost,
                 created_at, estimated_cost_usd, broadcast_cost_usd, provider_id)
             VALUES (?1, ?2, 'test-model', 10, 20, ?3, datetime('now'), ?3, ?4, ?5)",
            rusqlite::params![slug, operation, estimated, actual, provider],
        )
        .unwrap();
    }

    fn make_cache_entry(
        slug: &str,
        step: &str,
        chunk: i64,
        depth: i64,
        seed: &str,
    ) -> CacheEntry {
        let inputs_hash = compute_inputs_hash("sys", &format!("u-{seed}"));
        let prompt_hash = format!("phash-{seed}");
        let model_id = "m-1".to_string();
        let cache_key = compute_cache_key(&inputs_hash, &prompt_hash, &model_id);
        CacheEntry {
            slug: slug.into(),
            build_id: "p13-build".into(),
            step_name: step.into(),
            chunk_index: chunk,
            depth,
            cache_key,
            inputs_hash,
            prompt_hash,
            model_id,
            output_json: serde_json::json!({"c":"x"}).to_string(),
            token_usage_json: None,
            cost_usd: Some(0.01),
            latency_ms: Some(100),
            force_fresh: false,
            supersedes_cache_id: None,
            note: None,
        }
    }

    #[test]
    fn test_cost_rollup_groups_by_slug_provider_operation() {
        let conn = mem_conn();
        seed_cost_row(&conn, "p13-test", "build", Some("openrouter"), 1.23, Some(1.20));
        seed_cost_row(&conn, "p13-test", "build", Some("openrouter"), 0.50, Some(0.48));
        seed_cost_row(&conn, "p13-test", "stale_check", Some("openrouter"), 0.10, None);
        seed_cost_row(&conn, "p13-other", "build", Some("anthropic"), 2.00, Some(1.95));

        let from = "2020-01-01 00:00:00";
        let to = "2100-01-01 00:00:00";
        let buckets = cost_rollup(&conn, from, to).unwrap();

        // 3 distinct group keys: (p13-test, openrouter, build),
        // (p13-test, openrouter, stale_check), (p13-other, anthropic, build).
        assert_eq!(buckets.len(), 3);

        let build_opt = buckets
            .iter()
            .find(|b| b.slug == "p13-test" && b.operation == "build")
            .expect("p13-test build bucket");
        assert!((build_opt.estimated - 1.73).abs() < 1e-6);
        assert!((build_opt.actual - 1.68).abs() < 1e-6);
        assert_eq!(build_opt.call_count, 2);
    }

    // ── Phase 18c (L9) — Folder-scoped count + source paths ───────────────────

    /// Helper for the Phase 18c folder-scope tests: seeds a path
    /// hierarchy where /a, /a/b, and /a/b/c are all descendants
    /// of /a, plus /d as a sibling that should never be touched by
    /// /a-scoped operations.
    fn seed_dadbear_folder_hierarchy(conn: &Connection) {
        conn.execute(
            "INSERT INTO pyramid_dadbear_config (slug, source_path, content_type, enabled)
             VALUES ('p18c-a',     '/a',     'document', 1),
                    ('p18c-ab',    '/a/b',   'document', 1),
                    ('p18c-abc',   '/a/b/c', 'document', 1),
                    ('p18c-d',     '/d',     'document', 1)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn test_list_dadbear_source_paths_returns_distinct_sorted() {
        let conn = mem_conn();
        conn.execute(
            "INSERT INTO pyramid_dadbear_config (slug, source_path, content_type, enabled)
             VALUES ('p18c-a',  '/zeta',  'document', 1),
                    ('p18c-b',  '/alpha', 'document', 1),
                    ('p18c-c',  '/alpha', 'document', 0),
                    ('p18c-d',  '/mid',   'document', 1)",
            [],
        )
        .unwrap();

        let paths = list_dadbear_source_paths(&conn).unwrap();
        // /alpha appears twice in the table but DISTINCT collapses
        // to one entry. Result is alphabetically sorted.
        assert_eq!(
            paths,
            vec!["/alpha".to_string(), "/mid".to_string(), "/zeta".to_string()]
        );
    }

    #[test]
    fn test_count_dadbear_scope_all_uses_holds_projection() {
        let conn = mem_conn();
        seed_dadbear_folder_hierarchy(&conn);
        // Freeze one slug via holds projection.
        conn.execute(
            "INSERT INTO dadbear_holds_projection (slug, hold, source, acquired_at)
             VALUES ('p18c-d', 'frozen', 'test', datetime('now'))",
            [],
        )
        .unwrap();

        // target_state=true means "would-pause" — count slugs without a frozen hold.
        let pause_count = count_dadbear_scope(&conn, "all", None, true).unwrap();
        assert_eq!(pause_count, 3);

        // target_state=false means "would-resume" — count slugs with a frozen hold.
        let resume_count = count_dadbear_scope(&conn, "all", None, false).unwrap();
        assert_eq!(resume_count, 1);
    }

    #[test]
    fn test_count_dadbear_scope_folder_uses_holds_projection() {
        let conn = mem_conn();
        seed_dadbear_folder_hierarchy(&conn);

        // Preview should say 3 rows under /a (none frozen yet).
        let preview = count_dadbear_scope(&conn, "folder", Some("/a"), true).unwrap();
        assert_eq!(preview, 3);

        // Freeze the /a slugs via holds projection (canonical path).
        for slug in &["p18c-a", "p18c-ab", "p18c-abc"] {
            conn.execute(
                "INSERT INTO dadbear_holds_projection (slug, hold, source, acquired_at)
                 VALUES (?1, 'frozen', 'test', datetime('now'))",
                rusqlite::params![slug],
            ).unwrap();
        }
        let after = count_dadbear_scope(&conn, "folder", Some("/a"), true).unwrap();
        assert_eq!(after, 0);

        // Resume preview now sees the 3 frozen slugs.
        let resume_preview = count_dadbear_scope(&conn, "folder", Some("/a"), false).unwrap();
        assert_eq!(resume_preview, 3);
    }

    #[test]
    fn test_count_dadbear_scope_folder_handles_missing_or_empty_value() {
        let conn = mem_conn();
        seed_dadbear_folder_hierarchy(&conn);

        // Missing or empty scope_value returns 0 instead of matching
        // every row — this prevents UI from accidentally implying a
        // dangerous "everything" preview when the user hasn't typed
        // a path yet.
        assert_eq!(count_dadbear_scope(&conn, "folder", None, true).unwrap(), 0);
        assert_eq!(count_dadbear_scope(&conn, "folder", Some(""), true).unwrap(), 0);
    }

    #[test]
    fn test_count_dadbear_scope_circle_returns_zero_pending_schema() {
        let conn = mem_conn();
        seed_dadbear_folder_hierarchy(&conn);

        // The circle scope has no backing schema yet (deferred per
        // Phase 18c deviation note). The count helper returns 0 so
        // the UI can render "Circle scoping not yet available".
        let count =
            count_dadbear_scope(&conn, "circle", Some("any-circle"), true).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_list_cache_entries_for_build_returns_seeded_rows() {
        let conn = mem_conn();
        store_cache(&conn, &make_cache_entry("p13-test", "extract", 0, 0, "a")).unwrap();
        store_cache(&conn, &make_cache_entry("p13-test", "extract", 1, 0, "b")).unwrap();
        store_cache(&conn, &make_cache_entry("p13-test", "cluster", -1, 1, "c")).unwrap();

        let rows = list_cache_entries_for_build(&conn, "p13-test", "p13-build").unwrap();
        assert_eq!(rows.len(), 3);
        // Newest-first ordering.
        assert!(rows[0].id >= rows.last().unwrap().id);
    }

    #[test]
    fn test_find_downstream_cache_keys_returns_deeper_entries() {
        let conn = mem_conn();
        store_cache(&conn, &make_cache_entry("p13-test", "extract", 0, 0, "a")).unwrap();
        store_cache(&conn, &make_cache_entry("p13-test", "cluster", 0, 1, "b")).unwrap();
        store_cache(&conn, &make_cache_entry("p13-test", "synth", 0, 2, "c")).unwrap();

        let downstream = find_downstream_cache_keys(&conn, "p13-test", 0).unwrap();
        // Depth 1 + depth 2 entries should both be returned.
        assert_eq!(downstream.len(), 2);
    }

    #[test]
    fn test_invalidate_cache_entries_sets_invalidated_by() {
        let conn = mem_conn();
        let entry = make_cache_entry("p13-test", "cluster", 0, 1, "b");
        let cache_key = entry.cache_key.clone();
        store_cache(&conn, &entry).unwrap();

        let flipped = invalidate_cache_entries(
            &conn,
            "p13-test",
            &[cache_key.clone()],
            "upstream_reroll",
        )
        .unwrap();
        assert_eq!(flipped, 1);

        // check_cache should now return None because the row is
        // flagged invalidated.
        let hit = check_cache(&conn, "p13-test", &cache_key).unwrap();
        assert!(hit.is_none(), "invalidated row should be treated as miss");

        // check_cache_including_invalidated should still find it.
        let deep = check_cache_including_invalidated(&conn, "p13-test", &cache_key)
            .unwrap()
            .expect("row should still be in the table");
        assert_eq!(deep.invalidated_by.as_deref(), Some("upstream_reroll"));
    }

    #[test]
    fn test_count_recent_rerolls_counts_supersession_writes() {
        let conn = mem_conn();
        // Seed three supersedes-linked rows for the same step slot
        // within the last 10 minutes.
        for i in 0..3 {
            conn.execute(
                "INSERT INTO pyramid_step_cache
                    (slug, build_id, step_name, chunk_index, depth, cache_key,
                     inputs_hash, prompt_hash, model_id, output_json,
                     force_fresh, supersedes_cache_id, created_at)
                 VALUES ('p13-test', ?1, 'synth', -1, 0, ?2, 'i', 'p', 'm', '{}', 1, 99, datetime('now'))",
                rusqlite::params![format!("b{}", i), format!("ck{}", i)],
            )
            .unwrap();
        }

        let count = count_recent_rerolls(&conn, "p13-test", "synth", -1, 0).unwrap();
        assert!(count >= 3, "expected 3 recent rerolls, got {}", count);
    }

    // ── Phase 13 verifier fix tests ────────────────────────────────

    #[test]
    fn test_find_latest_build_id_for_slug_returns_most_recent() {
        let conn = mem_conn();
        // Seed entries with different build_ids using a raw insert so
        // we can control the build_id explicitly — make_cache_entry
        // hardcodes "p13-build".
        conn.execute(
            "INSERT INTO pyramid_step_cache
                (slug, build_id, step_name, chunk_index, depth, cache_key,
                 inputs_hash, prompt_hash, model_id, output_json,
                 force_fresh, supersedes_cache_id, created_at)
             VALUES ('slug-a', 'chain-001', 'extract', 0, 0, 'ck-a',
                     'i', 'p', 'm', '{}', 0, NULL, datetime('now', '-2 hours'))",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO pyramid_step_cache
                (slug, build_id, step_name, chunk_index, depth, cache_key,
                 inputs_hash, prompt_hash, model_id, output_json,
                 force_fresh, supersedes_cache_id, created_at)
             VALUES ('slug-a', 'chain-002', 'cluster', 0, 1, 'ck-b',
                     'i', 'p', 'm', '{}', 0, NULL, datetime('now'))",
            [],
        )
        .unwrap();

        let latest = find_latest_build_id_for_slug(&conn, "slug-a").unwrap();
        assert_eq!(latest.as_deref(), Some("chain-002"));

        // Non-existent slug returns None.
        let none = find_latest_build_id_for_slug(&conn, "nope").unwrap();
        assert!(none.is_none());
    }

    #[test]
    fn test_list_cache_entries_for_latest_build_resolves_on_slug() {
        let conn = mem_conn();
        // Two builds for the same slug. Latest one wins.
        conn.execute(
            "INSERT INTO pyramid_step_cache
                (slug, build_id, step_name, chunk_index, depth, cache_key,
                 inputs_hash, prompt_hash, model_id, output_json,
                 force_fresh, supersedes_cache_id, created_at)
             VALUES ('slug-b', 'chain-old', 'extract', 0, 0, 'ck-old',
                     'i', 'p', 'm', '{}', 0, NULL, datetime('now', '-1 day'))",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO pyramid_step_cache
                (slug, build_id, step_name, chunk_index, depth, cache_key,
                 inputs_hash, prompt_hash, model_id, output_json,
                 force_fresh, supersedes_cache_id, created_at)
             VALUES ('slug-b', 'chain-new', 'extract', 0, 0, 'ck-new',
                     'i', 'p', 'm', '{}', 0, NULL, datetime('now'))",
            [],
        )
        .unwrap();

        let rows = list_cache_entries_for_latest_build(&conn, "slug-b").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].build_id, "chain-new");
        assert_eq!(rows[0].cache_key, "ck-new");

        // Empty slug returns empty vec (not an error).
        let empty = list_cache_entries_for_latest_build(&conn, "nope").unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn test_invalidate_cache_entries_returning_flipped_matches_actual_writes() {
        let conn = mem_conn();
        let e1 = make_cache_entry("p13-test", "a", 0, 0, "1");
        let e2 = make_cache_entry("p13-test", "b", 0, 0, "2");
        let e3 = make_cache_entry("p13-test", "c", 0, 0, "3");
        let k1 = e1.cache_key.clone();
        let k2 = e2.cache_key.clone();
        let k3 = e3.cache_key.clone();
        store_cache(&conn, &e1).unwrap();
        store_cache(&conn, &e2).unwrap();
        store_cache(&conn, &e3).unwrap();

        // Pre-invalidate the middle row so the next call has to
        // report only k1 + k3 as freshly flipped.
        invalidate_cache_entries(&conn, "p13-test", &[k2.clone()], "setup").unwrap();

        let flipped = invalidate_cache_entries_returning_flipped(
            &conn,
            "p13-test",
            &[k1.clone(), k2.clone(), k3.clone()],
            "upstream_reroll",
        )
        .unwrap();
        assert_eq!(flipped.len(), 2);
        assert!(flipped.contains(&k1));
        assert!(!flipped.contains(&k2), "already-invalidated key should be skipped");
        assert!(flipped.contains(&k3));
    }

    #[test]
    fn test_build_active_build_summary_passes_through_progress() {
        let conn = mem_conn();
        // completed_steps and total_steps are caller-supplied (from
        // the live BuildHandle's progress channel). The function just
        // assembles the row and adds cost/cache from pyramid_step_cache.
        conn.execute(
            "INSERT INTO pyramid_step_cache
                (slug, build_id, step_name, chunk_index, depth, cache_key,
                 inputs_hash, prompt_hash, model_id, output_json, cost_usd,
                 force_fresh, supersedes_cache_id, created_at)
             VALUES ('summary-slug', 'chain-42', 'extract', 0, 0, 'ck-a',
                     'i', 'p', 'm', '{}', 0.0123, 1, NULL, datetime('now'))",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO pyramid_step_cache
                (slug, build_id, step_name, chunk_index, depth, cache_key,
                 inputs_hash, prompt_hash, model_id, output_json, cost_usd,
                 force_fresh, supersedes_cache_id, created_at)
             VALUES ('summary-slug', 'chain-42', 'cluster', 0, 1, 'ck-b',
                     'i', 'p', 'm', '{}', 0.0321, 0, NULL, datetime('now'))",
            [],
        )
        .unwrap();

        let row = build_active_build_summary(
            &conn,
            "summary-slug",
            "chain-42",
            "running",
            "3s ago",
            Some("source_extract"),
            7,
            21,
        )
        .unwrap();

        assert_eq!(row.build_id, "chain-42");
        assert_eq!(row.completed_steps, 7, "caller-supplied done");
        assert_eq!(row.total_steps, 21, "caller-supplied total");
        assert_eq!(row.current_step.as_deref(), Some("source_extract"));
        assert!(
            (row.cost_so_far_usd - 0.0444).abs() < 0.0001,
            "cost summed from pyramid_step_cache: {}",
            row.cost_so_far_usd
        );
        assert!(
            row.cache_hit_rate > 0.0 && row.cache_hit_rate < 1.0,
            "mixed hit/miss expected: {}",
            row.cache_hit_rate
        );
    }

    #[test]
    fn test_build_active_build_summary_zero_rows_for_slug() {
        let conn = mem_conn();
        // Corner case: a pyramid appears in the in-memory
        // active_build map but the chain executor hasn't written any
        // step_cache rows yet and progress is still zero.
        let row = build_active_build_summary(
            &conn,
            "empty-slug",
            "chain-1",
            "running",
            "0s ago",
            None,
            0,
            0,
        )
        .unwrap();
        assert_eq!(row.total_steps, 0);
        assert_eq!(row.completed_steps, 0);
        assert!((row.cost_so_far_usd - 0.0).abs() < 1e-9);
        assert_eq!(row.cache_hit_rate, 0.0);
        assert_eq!(row.build_id, "chain-1");
    }

    // ── Phase 14: pyramid_wire_update_cache helper tests ──────────────

    fn seed_wire_update_cache_config(conn: &Connection, contribution_id: &str) {
        conn.execute(
            "INSERT INTO pyramid_config_contributions (
                contribution_id, slug, schema_type, yaml_content,
                wire_native_metadata_json, wire_publication_state_json,
                status, source, wire_contribution_id, created_by, accepted_at
             ) VALUES (?1, NULL, 'custom_prompts', 'schema_type: custom_prompts\n',
                '{}', '{}', 'active', 'wire', 'w-orig', 'test', datetime('now'))",
            rusqlite::params![contribution_id],
        )
        .unwrap();
    }

    #[test]
    fn test_upsert_wire_update_cache_idempotent() {
        let conn = mem_conn();
        seed_wire_update_cache_config(&conn, "local-1");

        upsert_wire_update_cache(
            &conn,
            "local-1",
            "w-latest",
            2,
            Some("v2 changes"),
            Some("[\"alice\"]"),
        )
        .unwrap();
        // Upsert again with updated delta — should replace, not dupe.
        upsert_wire_update_cache(
            &conn,
            "local-1",
            "w-latest-2",
            3,
            Some("v3 changes"),
            Some("[\"alice\",\"bob\"]"),
        )
        .unwrap();

        let rows = list_pending_wire_updates(&conn, None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].latest_wire_contribution_id, "w-latest-2");
        assert_eq!(rows[0].chain_length_delta, 3);
        assert_eq!(rows[0].changes_summary.as_deref(), Some("v3 changes"));
    }

    #[test]
    fn test_list_pending_wire_updates_filters_acknowledged() {
        let conn = mem_conn();
        seed_wire_update_cache_config(&conn, "local-1");
        seed_wire_update_cache_config(&conn, "local-2");

        upsert_wire_update_cache(&conn, "local-1", "w-1", 1, None, None).unwrap();
        upsert_wire_update_cache(&conn, "local-2", "w-2", 1, None, None).unwrap();

        // Acknowledge local-1.
        let changed = acknowledge_wire_update(&conn, "local-1").unwrap();
        assert!(changed);

        let rows = list_pending_wire_updates(&conn, None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].local_contribution_id, "local-2");
    }

    #[test]
    fn test_delete_wire_update_cache() {
        let conn = mem_conn();
        seed_wire_update_cache_config(&conn, "local-1");

        upsert_wire_update_cache(&conn, "local-1", "w-1", 1, None, None).unwrap();
        assert_eq!(list_pending_wire_updates(&conn, None).unwrap().len(), 1);

        let deleted = delete_wire_update_cache(&conn, "local-1").unwrap();
        assert!(deleted);
        assert_eq!(list_pending_wire_updates(&conn, None).unwrap().len(), 0);

        // Deleting a non-existent row returns false.
        let deleted_again = delete_wire_update_cache(&conn, "local-1").unwrap();
        assert!(!deleted_again);
    }

    #[test]
    fn test_list_wire_tracked_contributions() {
        let conn = mem_conn();
        seed_wire_update_cache_config(&conn, "local-1");

        // Second contribution without a wire_contribution_id — should
        // NOT appear in the tracked list.
        conn.execute(
            "INSERT INTO pyramid_config_contributions (
                contribution_id, slug, schema_type, yaml_content,
                wire_native_metadata_json, wire_publication_state_json,
                status, source, wire_contribution_id, created_by, accepted_at
             ) VALUES ('local-2', NULL, 'custom_prompts',
                'schema_type: custom_prompts\n', '{}', '{}',
                'active', 'local', NULL, 'test', datetime('now'))",
            [],
        )
        .unwrap();

        let tracked = list_wire_tracked_contributions(&conn).unwrap();
        assert_eq!(tracked.len(), 1);
        assert_eq!(tracked[0].0, "local-1");
        assert_eq!(tracked[0].1, "w-orig");
        assert_eq!(tracked[0].2, "custom_prompts");
    }
}

// ── Phase 15 tests: DADBEAR Oversight aggregation ──────────────────────────
#[cfg(test)]
mod phase15_tests {
    use super::*;

    fn mem_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        // Seed a couple of slugs — the foreign key on
        // pyramid_dadbear_config.slug is relaxed in the schema but we
        // still want parent rows because cost_log, observation_events,
        // and change_manifests all reference pyramid_slugs ON DELETE
        // CASCADE.
        for s in ["p15-alpha", "p15-beta", "p15-gamma"] {
            conn.execute(
                "INSERT OR IGNORE INTO pyramid_slugs (slug, content_type, source_path)
                 VALUES (?1, 'document', '/tmp/p15')",
                rusqlite::params![s],
            )
            .unwrap();
        }
        conn
    }

    fn seed_dadbear_config(
        conn: &Connection,
        slug: &str,
        source_path: &str,
        enabled: bool,
        scan_interval: i64,
    ) -> i64 {
        conn.execute(
            "INSERT INTO pyramid_dadbear_config
                (slug, source_path, content_type, scan_interval_secs, debounce_secs, enabled)
             VALUES (?1, ?2, 'document', ?3, 30, ?4)",
            rusqlite::params![slug, source_path, scan_interval, enabled as i32],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn seed_cost_with_status(
        conn: &Connection,
        slug: &str,
        status: Option<&str>,
        estimated: f64,
        actual: Option<f64>,
    ) {
        // Production writers stamp `broadcast_confirmed_at` when a
        // broadcast lands healthy (status stays `'synchronous'`), and
        // leave it NULL otherwise. Default the seed to NULL; the
        // `seed_cost_row_confirmed` helper stamps it for the confirmed
        // case.
        conn.execute(
            "INSERT INTO pyramid_cost_log
                (slug, operation, model, input_tokens, output_tokens,
                 estimated_cost, estimated_cost_usd, broadcast_cost_usd,
                 reconciliation_status, created_at)
             VALUES (?1, 'stale_check', 'm-1', 10, 20, ?2, ?2, ?3, ?4, datetime('now'))",
            rusqlite::params![slug, estimated, actual, status],
        )
        .unwrap();
    }

    /// Seed a cost_log row matching the production "confirmed
    /// synchronous" state: status stays `'synchronous'` and
    /// `broadcast_confirmed_at` is stamped. This is the state
    /// `record_broadcast_confirmation` leaves a row in after a clean
    /// broadcast arrives (see `db::record_broadcast_confirmation`).
    fn seed_cost_row_confirmed(
        conn: &Connection,
        slug: &str,
        estimated: f64,
        actual: f64,
    ) {
        conn.execute(
            "INSERT INTO pyramid_cost_log
                (slug, operation, model, input_tokens, output_tokens,
                 estimated_cost, estimated_cost_usd, broadcast_cost_usd,
                 reconciliation_status, broadcast_confirmed_at, created_at)
             VALUES (?1, 'stale_check', 'm-1', 10, 20, ?2, ?2, ?3,
                     'synchronous', datetime('now'), datetime('now'))",
            rusqlite::params![slug, estimated, actual],
        )
        .unwrap();
    }

    #[test]
    fn test_overview_aggregates_single_slug() {
        let conn = mem_conn();
        seed_dadbear_config(&conn, "p15-alpha", "/tmp/a", true, 10);
        // Two unprocessed observation events (replaces old pyramid_pending_mutations inserts).
        conn.execute(
            "INSERT INTO dadbear_observation_events
                (slug, source, event_type, layer, detected_at)
             VALUES
                ('p15-alpha', 'test', 'file_modified', 0, datetime('now')),
                ('p15-alpha', 'test', 'file_modified', 0, datetime('now'))",
            [],
        )
        .unwrap();
        // Three demand signals.
        for _ in 0..3 {
            conn.execute(
                "INSERT INTO pyramid_demand_signals
                    (slug, node_id, signal_type, weight)
                 VALUES ('p15-alpha', 'n1', 'agent_query', 1.0)",
                [],
            )
            .unwrap();
        }
        // One deferred question.
        conn.execute(
            "INSERT INTO pyramid_deferred_questions
                (slug, question_id, question_json, next_check_at, check_interval)
             VALUES ('p15-alpha', 'q1', '{}', datetime('now', '+1 day'), 'daily')",
            [],
        )
        .unwrap();
        // Two cost rows — one synchronous-unconfirmed (pending
        // broadcast), one synchronous-confirmed (broadcast landed
        // healthy; status stays 'synchronous' per the production
        // contract in `record_broadcast_confirmation`).
        seed_cost_with_status(&conn, "p15-alpha", Some("synchronous"), 0.05, None);
        seed_cost_row_confirmed(&conn, "p15-alpha", 0.10, 0.09);
        // One change manifest.
        conn.execute(
            "INSERT INTO pyramid_change_manifests
                (slug, node_id, build_version, manifest_json, applied_at)
             VALUES ('p15-alpha', 'n1', 1, '{}', datetime('now'))",
            [],
        )
        .unwrap();

        let rows = build_dadbear_overview_rows(&conn).unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.slug, "p15-alpha");
        assert!(row.enabled);
        assert_eq!(row.config_ids.len(), 1);
        assert_eq!(row.pending_mutations_count, 2);
        assert_eq!(row.deferred_questions_count, 1);
        assert_eq!(row.demand_signals_24h, 3);
        assert!((row.cost_24h_estimated_usd - 0.15).abs() < 1e-9);
        assert!((row.cost_24h_actual_usd - 0.09).abs() < 1e-9);
        assert_eq!(row.recent_manifest_count, 1);
        // One synchronous row is unconfirmed (broadcast_confirmed_at
        // IS NULL) so overall status is "pending" — the confirmed
        // one doesn't count against us.
        assert_eq!(row.cost_reconciliation_status, "pending");
    }

    #[test]
    fn test_overview_reports_healthy_when_all_synchronous_confirmed() {
        let conn = mem_conn();
        seed_dadbear_config(&conn, "p15-alpha", "/tmp/a", true, 10);
        // Both cost rows are synchronous-confirmed — broadcast
        // landed healthy. The status should be "healthy", not
        // "pending". This is the bug the wanderer fixed.
        seed_cost_row_confirmed(&conn, "p15-alpha", 0.05, 0.05);
        seed_cost_row_confirmed(&conn, "p15-alpha", 0.10, 0.10);

        let rows = build_dadbear_overview_rows(&conn).unwrap();
        assert_eq!(rows[0].cost_reconciliation_status, "healthy");
    }

    #[test]
    fn test_overview_reports_healthy_when_only_synchronous_local() {
        let conn = mem_conn();
        seed_dadbear_config(&conn, "p15-alpha", "/tmp/a", true, 10);
        // Ollama / zero-cost local calls land as
        // `'synchronous_local'` with `broadcast_confirmed_at` NULL
        // — there's no broadcast to wait for, so the row must not
        // be counted as pending.
        seed_cost_with_status(&conn, "p15-alpha", Some("synchronous_local"), 0.0, Some(0.0));
        seed_cost_with_status(&conn, "p15-alpha", Some("synchronous_local"), 0.0, Some(0.0));

        let rows = build_dadbear_overview_rows(&conn).unwrap();
        assert_eq!(rows[0].cost_reconciliation_status, "healthy");
    }

    #[test]
    fn test_overview_reports_discrepancy_when_any_row_discrepant() {
        let conn = mem_conn();
        seed_dadbear_config(&conn, "p15-alpha", "/tmp/a", true, 10);
        seed_cost_row_confirmed(&conn, "p15-alpha", 0.10, 0.10);
        seed_cost_with_status(&conn, "p15-alpha", Some("discrepancy"), 0.05, Some(0.20));

        let rows = build_dadbear_overview_rows(&conn).unwrap();
        assert_eq!(rows[0].cost_reconciliation_status, "discrepancy");
    }

    #[test]
    fn test_overview_reports_broadcast_missing_when_no_discrepancy() {
        let conn = mem_conn();
        seed_dadbear_config(&conn, "p15-alpha", "/tmp/a", true, 10);
        seed_cost_with_status(&conn, "p15-alpha", Some("broadcast_missing"), 0.10, None);
        seed_cost_row_confirmed(&conn, "p15-alpha", 0.05, 0.05);

        let rows = build_dadbear_overview_rows(&conn).unwrap();
        assert_eq!(rows[0].cost_reconciliation_status, "broadcast_missing");
    }

    #[test]
    fn test_overview_reports_healthy_with_no_rows() {
        let conn = mem_conn();
        seed_dadbear_config(&conn, "p15-alpha", "/tmp/a", true, 10);
        let rows = build_dadbear_overview_rows(&conn).unwrap();
        assert_eq!(rows[0].cost_reconciliation_status, "healthy");
    }

    #[test]
    fn test_overview_groups_multi_config_per_slug() {
        let conn = mem_conn();
        // Two configs for the same slug — one enabled, one not.
        // The bucket should report enabled=true (any-enabled) and
        // carry both config_ids.
        let id_a = seed_dadbear_config(&conn, "p15-alpha", "/tmp/a", true, 10);
        let id_b = seed_dadbear_config(&conn, "p15-alpha", "/tmp/b", false, 20);

        let rows = build_dadbear_overview_rows(&conn).unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert!(row.enabled, "enabled should be true if any config is enabled");
        assert_eq!(row.config_ids.len(), 2);
        assert!(row.config_ids.contains(&id_a));
        assert!(row.config_ids.contains(&id_b));
        // scan_interval_secs should be the MIN across configs.
        assert_eq!(row.scan_interval_secs, 10);
    }

    #[test]
    fn test_overview_reports_all_paused_when_all_disabled() {
        let conn = mem_conn();
        seed_dadbear_config(&conn, "p15-alpha", "/tmp/a", false, 10);
        seed_dadbear_config(&conn, "p15-beta", "/tmp/b", false, 10);

        let rows = build_dadbear_overview_rows(&conn).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| !r.enabled));
    }

    #[test]
    fn test_overview_multi_slug_aggregates() {
        let conn = mem_conn();
        seed_dadbear_config(&conn, "p15-alpha", "/tmp/a", true, 10);
        seed_dadbear_config(&conn, "p15-beta", "/tmp/b", false, 20);
        seed_dadbear_config(&conn, "p15-gamma", "/tmp/c", true, 30);

        // Only alpha has a pending observation event (replaces old pyramid_pending_mutations insert).
        conn.execute(
            "INSERT INTO dadbear_observation_events
                (slug, source, event_type, layer, detected_at)
             VALUES ('p15-alpha', 'test', 'file_modified', 0, datetime('now'))",
            [],
        )
        .unwrap();
        // Beta has a deferred question.
        conn.execute(
            "INSERT INTO pyramid_deferred_questions
                (slug, question_id, question_json, next_check_at, check_interval)
             VALUES ('p15-beta', 'q1', '{}', datetime('now', '+1 day'), 'daily')",
            [],
        )
        .unwrap();
        // Gamma has a confirmed synchronous cost row.
        seed_cost_row_confirmed(&conn, "p15-gamma", 0.50, 0.48);

        let rows = build_dadbear_overview_rows(&conn).unwrap();
        assert_eq!(rows.len(), 3);

        let alpha = rows.iter().find(|r| r.slug == "p15-alpha").unwrap();
        let beta = rows.iter().find(|r| r.slug == "p15-beta").unwrap();
        let gamma = rows.iter().find(|r| r.slug == "p15-gamma").unwrap();

        assert_eq!(alpha.pending_mutations_count, 1);
        assert!(alpha.enabled);

        assert_eq!(beta.deferred_questions_count, 1);
        assert!(!beta.enabled);

        assert!((gamma.cost_24h_estimated_usd - 0.50).abs() < 1e-9);
        assert!((gamma.cost_24h_actual_usd - 0.48).abs() < 1e-9);
        assert!(gamma.enabled);
    }

    #[test]
    fn test_acknowledge_orphan_broadcast_updates_row() {
        let conn = mem_conn();
        // Seed one unacknowledged orphan.
        conn.execute(
            "INSERT INTO pyramid_orphan_broadcasts
                (provider_id, generation_id, session_id, pyramid_slug, build_id,
                 step_name, model, cost_usd, tokens_in, tokens_out, payload_json)
             VALUES ('openrouter', 'gen-1', 'p15-alpha/b1', 'p15-alpha', 'b1',
                     'extract', 'm-1', 0.01, 100, 50, '{}')",
            [],
        )
        .unwrap();
        let orphan_id: i64 = conn
            .query_row(
                "SELECT id FROM pyramid_orphan_broadcasts WHERE generation_id = 'gen-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();

        // Before acknowledgement, unacknowledged count = 1.
        let unack: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_orphan_broadcasts
                 WHERE acknowledged_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(unack, 1);

        // Simulate the IPC's UPDATE.
        let now = "2026-04-10 12:00:00";
        let affected = conn
            .execute(
                "UPDATE pyramid_orphan_broadcasts
                    SET acknowledged_at = ?1,
                        acknowledgment_reason = ?2
                  WHERE id = ?3 AND acknowledged_at IS NULL",
                rusqlite::params![now, "reviewed", orphan_id],
            )
            .unwrap();
        assert_eq!(affected, 1);

        // Re-ack should affect 0 rows (idempotent).
        let affected2 = conn
            .execute(
                "UPDATE pyramid_orphan_broadcasts
                    SET acknowledged_at = ?1,
                        acknowledgment_reason = ?2
                  WHERE id = ?3 AND acknowledged_at IS NULL",
                rusqlite::params![now, "reviewed again", orphan_id],
            )
            .unwrap();
        assert_eq!(affected2, 0);

        // Unacknowledged count now 0.
        let unack: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_orphan_broadcasts
                 WHERE acknowledged_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(unack, 0);
    }

    #[test]
    fn test_upsert_dadbear_policy_global_is_noop() {
        // Phase 15 wanderer: global (slug=None) dadbear_policy
        // contributions must not error and must not touch the
        // per-slug operational table. The contribution itself is
        // persisted in `pyramid_config_contributions` by the accept
        // flow; this helper is only responsible for the operational
        // mirror, which has no global row.
        let conn = mem_conn();
        let yaml = DadbearPolicyYaml {
            source_path: "/tmp/unused".to_string(),
            content_type: "document".to_string(),
            scan_interval_secs: 30,
            debounce_secs: 5,
            session_timeout_secs: 600,
            batch_size: 10,
            enabled: true,
            cost_reconciliation: None,
        };
        // Should succeed (no error) when slug is None.
        upsert_dadbear_policy(&conn, &None, &yaml, "contrib-global-1").unwrap();
        // And must not have inserted anything into the operational
        // table (the global contribution has nowhere to land there).
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_dadbear_config",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_upsert_dadbear_policy_per_slug_still_writes() {
        // Sanity check: per-slug contributions (the Configure-per-
        // pyramid path) still land in the operational table.
        let conn = mem_conn();
        // p15-alpha already exists in mem_conn; need a slug row.
        let yaml = DadbearPolicyYaml {
            source_path: "/tmp/alpha".to_string(),
            content_type: "document".to_string(),
            scan_interval_secs: 42,
            debounce_secs: 7,
            session_timeout_secs: 300,
            batch_size: 5,
            enabled: true,
            cost_reconciliation: None,
        };
        upsert_dadbear_policy(
            &conn,
            &Some("p15-alpha".to_string()),
            &yaml,
            "contrib-alpha-1",
        )
        .unwrap();
        let (scan, debounce): (i64, i64) = conn
            .query_row(
                "SELECT scan_interval_secs, debounce_secs
                 FROM pyramid_dadbear_config WHERE slug = 'p15-alpha'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(scan, 42);
        assert_eq!(debounce, 7);
    }
}

#[cfg(test)]
mod phase16_tests {
    //! Phase 16 tests: vine-of-vines composition + recursive propagation.
    //! Covers the `child_type` column migration, the extended DB helpers,
    //! and the recursive ancestor walk with cycle guard.

    use super::*;

    fn mem_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        conn
    }

    #[test]
    fn test_vine_compositions_schema_includes_child_type() {
        let conn = mem_conn();
        let has_child_type: bool = conn
            .prepare(
                "SELECT 1 FROM pragma_table_info('pyramid_vine_compositions')
                 WHERE name = 'child_type'",
            )
            .unwrap()
            .exists([])
            .unwrap();
        assert!(
            has_child_type,
            "pyramid_vine_compositions must include child_type column after Phase 16 migration"
        );
    }

    #[test]
    fn test_child_type_migration_is_idempotent() {
        // Running init_pyramid_db a second time on an already-migrated DB
        // must not error even though the migration is a no-op on the
        // pragma_table_info check.
        let conn = mem_conn();
        // Re-run init — should not fail.
        init_pyramid_db(&conn).unwrap();

        // Also verify the column is still present and insertable.
        insert_vine_composition(&conn, "v1", "b1", 0, "bedrock").unwrap();
        let comps = list_vine_compositions(&conn, "v1").unwrap();
        assert_eq!(comps.len(), 1);
        assert_eq!(comps[0].child_type, "bedrock");
    }

    #[test]
    fn test_insert_vine_composition_with_child_type_vine() {
        let conn = mem_conn();
        insert_vine_composition(&conn, "parent-vine", "child-vine", 0, "vine").unwrap();
        let comps = list_vine_compositions(&conn, "parent-vine").unwrap();
        assert_eq!(comps.len(), 1);
        assert_eq!(comps[0].bedrock_slug, "child-vine");
        assert_eq!(comps[0].child_type, "vine");
        assert!(comps[0].is_vine_child());
        assert_eq!(comps[0].child_slug(), "child-vine");
    }

    #[test]
    fn test_insert_vine_composition_rejects_invalid_child_type() {
        let conn = mem_conn();
        let result = insert_vine_composition(&conn, "v", "c", 0, "bogus");
        assert!(result.is_err());
    }

    #[test]
    fn test_list_vine_compositions_returns_both_bedrock_and_vine_children() {
        let conn = mem_conn();
        // Phase 16 vine composing two bedrocks and one sub-vine.
        insert_vine_composition(&conn, "top-vine", "bedrock-a", 0, "bedrock").unwrap();
        insert_vine_composition(&conn, "top-vine", "sub-vine", 1, "vine").unwrap();
        insert_vine_composition(&conn, "top-vine", "bedrock-b", 2, "bedrock").unwrap();

        let comps = list_vine_compositions(&conn, "top-vine").unwrap();
        assert_eq!(comps.len(), 3);
        assert_eq!(comps[0].bedrock_slug, "bedrock-a");
        assert_eq!(comps[0].child_type, "bedrock");
        assert_eq!(comps[1].bedrock_slug, "sub-vine");
        assert_eq!(comps[1].child_type, "vine");
        assert_eq!(comps[2].bedrock_slug, "bedrock-b");
        assert_eq!(comps[2].child_type, "bedrock");
    }

    #[test]
    fn test_add_bedrock_to_vine_backcompat_alias_defaults_to_bedrock() {
        let conn = mem_conn();
        // Phase 2/13 callers still use `add_bedrock_to_vine` and expect
        // child_type to default to 'bedrock'.
        add_bedrock_to_vine(&conn, "v", "b", 0).unwrap();
        let comps = list_vine_compositions(&conn, "v").unwrap();
        assert_eq!(comps.len(), 1);
        assert_eq!(comps[0].child_type, "bedrock");
        assert!(!comps[0].is_vine_child());
    }

    #[test]
    fn test_get_vines_for_child_returns_parents_regardless_of_type() {
        let conn = mem_conn();
        // A bedrock and a sub-vine both parented by multiple vines.
        insert_vine_composition(&conn, "vine-a", "bedrock-x", 0, "bedrock").unwrap();
        insert_vine_composition(&conn, "vine-b", "bedrock-x", 0, "bedrock").unwrap();
        insert_vine_composition(&conn, "vine-a", "sub-vine-y", 1, "vine").unwrap();
        insert_vine_composition(&conn, "vine-c", "sub-vine-y", 0, "vine").unwrap();

        let bedrock_parents = get_vines_for_child(&conn, "bedrock-x").unwrap();
        assert_eq!(bedrock_parents.len(), 2);
        assert!(bedrock_parents.contains(&"vine-a".to_string()));
        assert!(bedrock_parents.contains(&"vine-b".to_string()));

        let subvine_parents = get_vines_for_child(&conn, "sub-vine-y").unwrap();
        assert_eq!(subvine_parents.len(), 2);
        assert!(subvine_parents.contains(&"vine-a".to_string()));
        assert!(subvine_parents.contains(&"vine-c".to_string()));

        // The legacy alias returns the same result.
        let legacy = get_vines_for_bedrock(&conn, "bedrock-x").unwrap();
        assert_eq!(legacy.len(), 2);
    }

    #[test]
    fn test_get_parent_vines_recursive_walks_multi_level_hierarchy() {
        let conn = mem_conn();
        // Hierarchy:
        //   v3 composes v2 (as vine child) + b-top (as bedrock)
        //   v2 composes v1 (as vine child)
        //   v1 composes b-leaf (as bedrock)
        insert_vine_composition(&conn, "v1", "b-leaf", 0, "bedrock").unwrap();
        insert_vine_composition(&conn, "v2", "v1", 0, "vine").unwrap();
        insert_vine_composition(&conn, "v3", "v2", 0, "vine").unwrap();
        insert_vine_composition(&conn, "v3", "b-top", 1, "bedrock").unwrap();

        let ancestors = get_parent_vines_recursive(&conn, "b-leaf").unwrap();
        // BFS order: v1 (direct), then v2 (grandparent), then v3 (great-grandparent).
        assert_eq!(ancestors.len(), 3);
        assert_eq!(ancestors[0], "v1");
        assert!(ancestors.contains(&"v2".to_string()));
        assert!(ancestors.contains(&"v3".to_string()));

        // Starting from v1, we should get v2 and v3.
        let from_v1 = get_parent_vines_recursive(&conn, "v1").unwrap();
        assert_eq!(from_v1.len(), 2);
        assert!(from_v1.contains(&"v2".to_string()));
        assert!(from_v1.contains(&"v3".to_string()));

        // Starting from v3 (the top), no ancestors.
        let from_v3 = get_parent_vines_recursive(&conn, "v3").unwrap();
        assert!(from_v3.is_empty());
    }

    #[test]
    fn test_get_parent_vines_recursive_cycle_guard() {
        let conn = mem_conn();
        // Direct self-reference: v-self composes itself as a vine child.
        // A pathological but possible state; the cycle guard must
        // prevent infinite recursion.
        insert_vine_composition(&conn, "v-self", "v-self", 0, "vine").unwrap();

        let ancestors = get_parent_vines_recursive(&conn, "v-self").unwrap();
        // The walk visits v-self (starting set), sees its parent is also
        // v-self, recognizes it as already visited, and returns empty.
        assert!(
            ancestors.is_empty(),
            "self-referential vine must not produce any ancestors"
        );

        // Indirect cycle: v-a → v-b → v-a.
        insert_vine_composition(&conn, "v-a", "v-b", 0, "vine").unwrap();
        insert_vine_composition(&conn, "v-b", "v-a", 0, "vine").unwrap();

        let from_a = get_parent_vines_recursive(&conn, "v-a").unwrap();
        // Starting from v-a, v-b is a parent (since v-b composes v-a).
        // Then v-b's parent is v-a, which is already visited. Walk halts.
        assert_eq!(from_a.len(), 1);
        assert_eq!(from_a[0], "v-b");
    }

    #[test]
    fn test_update_child_apex_works_for_vine_children() {
        let conn = mem_conn();
        insert_vine_composition(&conn, "parent-vine", "sub-vine", 0, "vine").unwrap();
        update_child_apex(&conn, "parent-vine", "sub-vine", "node-apex-1").unwrap();

        let comps = list_vine_compositions(&conn, "parent-vine").unwrap();
        assert_eq!(comps.len(), 1);
        assert_eq!(
            comps[0].bedrock_apex_node_id.as_deref(),
            Some("node-apex-1")
        );
        assert_eq!(comps[0].child_type, "vine");

        // The legacy alias still works on vine children (the column name
        // is reused and the helper doesn't care about child_type).
        update_bedrock_apex(&conn, "parent-vine", "sub-vine", "node-apex-2").unwrap();
        let comps = list_vine_compositions(&conn, "parent-vine").unwrap();
        assert_eq!(
            comps[0].bedrock_apex_node_id.as_deref(),
            Some("node-apex-2")
        );
    }

    #[test]
    fn test_upsert_changes_child_type() {
        let conn = mem_conn();
        // Insert as bedrock first, then change to vine via re-insert. The
        // upsert path should update the child_type alongside the position.
        insert_vine_composition(&conn, "p", "c", 0, "bedrock").unwrap();
        let comps = list_vine_compositions(&conn, "p").unwrap();
        assert_eq!(comps[0].child_type, "bedrock");

        insert_vine_composition(&conn, "p", "c", 5, "vine").unwrap();
        let comps = list_vine_compositions(&conn, "p").unwrap();
        assert_eq!(comps.len(), 1);
        assert_eq!(comps[0].child_type, "vine");
        assert_eq!(comps[0].position, 5);
    }
}

#[cfg(test)]
mod phase17_tests {
    //! Phase 17 tests: extended folder_ingestion_heuristics schema
    //! + config loader.
    use super::*;

    fn mem_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        conn
    }

    fn seed_config_contribution(conn: &Connection, contribution_id: &str, schema_type: &str) {
        conn.execute(
            "INSERT INTO pyramid_config_contributions
                (contribution_id, schema_type, slug, yaml_content, wire_native_metadata_json,
                 source, status, created_at)
             VALUES (?1, ?2, NULL, 'schema_type: folder_ingestion_heuristics',
                 '{}', 'bundled', 'active', datetime('now'))",
            rusqlite::params![contribution_id, schema_type],
        )
        .unwrap();
    }

    #[test]
    fn test_folder_ingestion_heuristics_schema_has_new_columns() {
        let conn = mem_conn();
        for col in [
            "default_scan_interval_secs",
            "code_extensions_json",
            "document_extensions_json",
            "claude_code_auto_include",
            "claude_code_conversation_path",
        ] {
            let exists: bool = conn
                .prepare(
                    "SELECT 1 FROM pragma_table_info('pyramid_folder_ingestion_heuristics')
                     WHERE name = ?1",
                )
                .unwrap()
                .exists(rusqlite::params![col])
                .unwrap();
            assert!(exists, "missing column: {}", col);
        }
    }

    #[test]
    fn test_folder_ingestion_heuristics_yaml_roundtrip_with_new_fields() {
        let conn = mem_conn();
        seed_config_contribution(&conn, "cb-1", "folder_ingestion_heuristics");

        let yaml = FolderIngestionHeuristicsYaml {
            min_files_for_pyramid: 5,
            max_file_size_bytes: 20_000,
            max_recursion_depth: 4,
            content_type_rules: None,
            ignore_patterns: Some(serde_yaml::Value::Sequence(vec![
                serde_yaml::Value::String("node_modules/".to_string()),
                serde_yaml::Value::String("target/".to_string()),
            ])),
            respect_gitignore: true,
            respect_pyramid_ignore: true,
            vine_collapse_single_child: false,
            default_scan_interval_secs: 45,
            code_extensions: Some(vec![".rs".to_string(), ".ts".to_string()]),
            document_extensions: Some(vec![".md".to_string()]),
            claude_code_auto_include: false,
            claude_code_conversation_path: "/tmp/cc".to_string(),
        };

        upsert_folder_ingestion_heuristics(&conn, &None, &yaml, "cb-1").unwrap();

        let config = load_active_folder_ingestion_heuristics(&conn).unwrap();
        assert_eq!(config.min_files_for_pyramid, 5);
        assert_eq!(config.max_file_size_bytes, 20_000);
        assert_eq!(config.max_recursion_depth, 4);
        assert_eq!(config.default_scan_interval_secs, 45);
        assert_eq!(config.code_extensions, vec![".rs".to_string(), ".ts".to_string()]);
        assert_eq!(config.document_extensions, vec![".md".to_string()]);
        assert!(!config.claude_code_auto_include);
        assert_eq!(config.claude_code_conversation_path, "/tmp/cc");
        assert_eq!(
            config.ignore_patterns,
            vec!["node_modules/".to_string(), "target/".to_string()]
        );
    }

    #[test]
    fn test_load_active_folder_ingestion_heuristics_defaults_when_empty() {
        let conn = mem_conn();
        // No row in the table at all — loader should return the
        // bundled defaults instead of erroring.
        let config = load_active_folder_ingestion_heuristics(&conn).unwrap();
        assert_eq!(config.min_files_for_pyramid, 3);
        assert_eq!(config.max_recursion_depth, 10);
        assert_eq!(config.default_scan_interval_secs, 30);
        assert!(config.claude_code_auto_include);
        assert_eq!(
            config.claude_code_conversation_path,
            "~/.claude/projects".to_string()
        );
        assert!(config.code_extensions.contains(&".rs".to_string()));
        assert!(config.document_extensions.contains(&".md".to_string()));
        assert!(config.ignore_patterns.iter().any(|p| p == "node_modules/"));
    }

    #[test]
    fn test_load_active_folder_ingestion_heuristics_empty_lists_fall_back() {
        // If the stored row has empty JSON arrays, the loader should
        // populate the lists with the seed defaults so ingestion
        // never runs with a silently-empty extension set.
        let conn = mem_conn();
        seed_config_contribution(&conn, "cb-2", "folder_ingestion_heuristics");

        let yaml = FolderIngestionHeuristicsYaml {
            min_files_for_pyramid: 3,
            max_file_size_bytes: 100,
            max_recursion_depth: 10,
            content_type_rules: None,
            ignore_patterns: Some(serde_yaml::Value::Sequence(Vec::new())),
            respect_gitignore: true,
            respect_pyramid_ignore: true,
            vine_collapse_single_child: true,
            default_scan_interval_secs: 30,
            code_extensions: Some(Vec::new()),
            document_extensions: Some(Vec::new()),
            claude_code_auto_include: true,
            claude_code_conversation_path: String::new(),
        };
        upsert_folder_ingestion_heuristics(&conn, &None, &yaml, "cb-2").unwrap();

        let config = load_active_folder_ingestion_heuristics(&conn).unwrap();
        assert!(
            !config.code_extensions.is_empty(),
            "loader should hydrate empty code_extensions from defaults"
        );
        assert!(!config.document_extensions.is_empty());
        assert!(!config.ignore_patterns.is_empty());
        assert_eq!(
            config.claude_code_conversation_path,
            "~/.claude/projects".to_string()
        );
    }

    // ── Fleet Result Outbox tests ────────────────────────────────────────

    /// Build a fresh in-memory DB with the outbox table present.
    fn outbox_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        conn
    }

    /// Far-future ISO-8601 timestamp for rows that MUST NOT be swept in tests.
    fn future_expires() -> String {
        "9999-12-31 23:59:59".to_string()
    }

    #[test]
    fn test_fleet_outbox_table_creation_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        // Running init twice must not error — CREATE TABLE IF NOT EXISTS +
        // CREATE INDEX IF NOT EXISTS are idempotent.
        init_pyramid_db(&conn).unwrap();
        init_pyramid_db(&conn).unwrap();
        // And the table is queryable.
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM fleet_result_outbox", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn test_fleet_outbox_insert_or_ignore_fresh_vs_duplicate() {
        let conn = outbox_conn();
        let expires = future_expires();
        // Fresh insert returns 1.
        let n1 =
            fleet_outbox_insert_or_ignore(&conn, "dispA", "job-1", "https://x/cb", &expires)
                .unwrap();
        assert_eq!(n1, 1, "fresh insert should report rowcount=1");
        // Duplicate PK returns 0.
        let n2 =
            fleet_outbox_insert_or_ignore(&conn, "dispA", "job-1", "https://x/cb", &expires)
                .unwrap();
        assert_eq!(n2, 0, "duplicate PK should report rowcount=0");
    }

    #[test]
    fn test_fleet_outbox_lookup_present_and_missing() {
        let conn = outbox_conn();
        let expires = future_expires();
        fleet_outbox_insert_or_ignore(&conn, "dispA", "job-1", "https://x/cb", &expires).unwrap();

        let found = fleet_outbox_lookup(&conn, "job-1").unwrap();
        assert!(found.is_some());
        let lookup = found.unwrap();
        assert_eq!(lookup.dispatcher_node_id, "dispA");
        assert_eq!(lookup.status, FLEET_STATUS_PENDING);
        assert_eq!(lookup.delivery_attempts, 0);
        assert!(lookup.last_error.is_none());

        let missing = fleet_outbox_lookup(&conn, "nope").unwrap();
        assert!(missing.is_none());
    }

    #[test]
    fn test_fleet_outbox_count_inflight_excluding() {
        let conn = outbox_conn();
        let expires = future_expires();
        // Three in-flight rows, one delivered (should not count).
        fleet_outbox_insert_or_ignore(&conn, "dispA", "j1", "https://x/cb", &expires).unwrap();
        fleet_outbox_insert_or_ignore(&conn, "dispA", "j2", "https://x/cb", &expires).unwrap();
        fleet_outbox_insert_or_ignore(&conn, "dispB", "j3", "https://x/cb", &expires).unwrap();
        // Move j3 to delivered to confirm it's excluded from the count.
        fleet_outbox_promote_ready_if_pending(&conn, "dispB", "j3", "{}", 60).unwrap();
        fleet_outbox_mark_delivered_if_ready(&conn, "dispB", "j3", 3600).unwrap();

        // Count excluding j1 should be 1 (j2 pending; j3 delivered excluded by status).
        let n = fleet_outbox_count_inflight_excluding(&conn, "dispA", "j1").unwrap();
        assert_eq!(n, 1);

        // Count excluding j2 should be 1 (j1 pending; j3 delivered).
        let n = fleet_outbox_count_inflight_excluding(&conn, "dispA", "j2").unwrap();
        assert_eq!(n, 1);

        // Count excluding a nonexistent key returns all in-flight.
        let n =
            fleet_outbox_count_inflight_excluding(&conn, "dispA", "nonexistent").unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn test_fleet_outbox_promote_ready_if_pending_cas() {
        let conn = outbox_conn();
        let expires = future_expires();
        fleet_outbox_insert_or_ignore(&conn, "dispA", "job-1", "https://x/cb", &expires).unwrap();

        // First promotion: row is pending, CAS wins.
        let n =
            fleet_outbox_promote_ready_if_pending(&conn, "dispA", "job-1", "{\"kind\":\"Success\"}", 60)
                .unwrap();
        assert_eq!(n, 1);

        // Second promotion: row is now ready, CAS loses.
        let n =
            fleet_outbox_promote_ready_if_pending(&conn, "dispA", "job-1", "{\"kind\":\"Success\"}", 60)
                .unwrap();
        assert_eq!(n, 0);

        // Row missing entirely: CAS returns 0 (not an error).
        let n =
            fleet_outbox_promote_ready_if_pending(&conn, "dispA", "missing", "{}", 60).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn test_fleet_outbox_mark_delivered_if_ready_cas_loses_on_failed() {
        let conn = outbox_conn();
        let expires = future_expires();
        fleet_outbox_insert_or_ignore(&conn, "dispA", "job-1", "https://x/cb", &expires).unwrap();
        fleet_outbox_promote_ready_if_pending(&conn, "dispA", "job-1", "{}", 60).unwrap();
        // Force into failed state.
        fleet_outbox_mark_failed_if_ready(&conn, "dispA", "job-1", 3600).unwrap();

        // Delivery CAS must lose against failed status.
        let n =
            fleet_outbox_mark_delivered_if_ready(&conn, "dispA", "job-1", 3600).unwrap();
        assert_eq!(n, 0, "ready→delivered CAS must lose when row is failed");

        let lookup = fleet_outbox_lookup(&conn, "job-1").unwrap().unwrap();
        assert_eq!(lookup.status, FLEET_STATUS_FAILED);
    }

    #[test]
    fn test_fleet_outbox_heartbeat_cas_non_pending() {
        let conn = outbox_conn();
        let expires = future_expires();
        fleet_outbox_insert_or_ignore(&conn, "dispA", "job-1", "https://x/cb", &expires).unwrap();

        // Heartbeat on pending row: returns 1.
        let n = fleet_outbox_update_heartbeat_if_pending(
            &conn,
            "dispA",
            "job-1",
            "2099-01-01 00:00:00",
        )
        .unwrap();
        assert_eq!(n, 1);

        // Transition to ready; heartbeat must now CAS-lose.
        fleet_outbox_promote_ready_if_pending(&conn, "dispA", "job-1", "{}", 60).unwrap();
        let n = fleet_outbox_update_heartbeat_if_pending(
            &conn,
            "dispA",
            "job-1",
            "2099-01-01 00:00:00",
        )
        .unwrap();
        assert_eq!(n, 0);

        // Heartbeat on a missing row: 0, not an error.
        let n = fleet_outbox_update_heartbeat_if_pending(
            &conn,
            "dispA",
            "missing",
            "2099-01-01 00:00:00",
        )
        .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn test_fleet_outbox_startup_recovery_pending_only() {
        let conn = outbox_conn();
        let expires = future_expires();
        // Two pending rows, one ready row.
        fleet_outbox_insert_or_ignore(&conn, "dispA", "p1", "https://x/cb", &expires).unwrap();
        fleet_outbox_insert_or_ignore(&conn, "dispA", "p2", "https://x/cb", &expires).unwrap();
        fleet_outbox_insert_or_ignore(&conn, "dispA", "r1", "https://x/cb", &expires).unwrap();
        // Promote r1 to ready with a real result.
        fleet_outbox_promote_ready_if_pending(
            &conn,
            "dispA",
            "r1",
            "{\"kind\":\"Success\",\"data\":{\"content\":\"ok\"}}",
            60,
        )
        .unwrap();

        let recovered = fleet_outbox_startup_recovery(&conn, 1800).unwrap();
        assert_eq!(recovered, 2, "only the 2 pending rows should be recovered");

        // p1 and p2 should now be ready with synth Error JSON.
        for jid in &["p1", "p2"] {
            let row: (String, String, Option<String>) = conn
                .query_row(
                    "SELECT status, last_error, result_json FROM fleet_result_outbox WHERE job_id = ?1",
                    rusqlite::params![jid],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert_eq!(row.0, FLEET_STATUS_READY);
            assert_eq!(row.1, "startup recovery");
            let body = row.2.unwrap();
            assert!(body.contains("\"kind\":\"Error\""));
            assert!(body.contains("worker crashed before completion"));
        }

        // r1's result_json must still be the original Success payload.
        let r1_body: String = conn
            .query_row(
                "SELECT result_json FROM fleet_result_outbox WHERE job_id = 'r1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            r1_body.contains("\"kind\":\"Success\""),
            "ready rows must survive recovery unchanged"
        );
    }

    #[test]
    fn test_fleet_outbox_sweep_expired_returns_matching_rows() {
        let conn = outbox_conn();
        // One row already expired (past), one very much not expired (future).
        fleet_outbox_insert_or_ignore(
            &conn,
            "dispA",
            "expired",
            "https://x/cb",
            "1970-01-01 00:00:00",
        )
        .unwrap();
        fleet_outbox_insert_or_ignore(
            &conn,
            "dispA",
            "alive",
            "https://x/cb",
            "9999-12-31 23:59:59",
        )
        .unwrap();

        let rows = fleet_outbox_sweep_expired(&conn).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].job_id, "expired");
        assert_eq!(rows[0].status, FLEET_STATUS_PENDING);
    }

    #[test]
    fn test_fleet_outbox_unique_job_id_rejects_cross_dispatcher_reuse() {
        let conn = outbox_conn();
        let expires = future_expires();
        let n1 =
            fleet_outbox_insert_or_ignore(&conn, "dispA", "shared-uuid", "https://a/cb", &expires)
                .unwrap();
        assert_eq!(n1, 1);

        // Different dispatcher, same job_id. INSERT OR IGNORE turns the
        // unique-index conflict into a silent no-op, so the rowcount is 0
        // — the row is NOT created under dispB. The detection mechanism is
        // the subsequent lookup: callers compare the stored dispatcher_node_id
        // against their identity and reject with 409 Conflict on mismatch.
        let n2 = fleet_outbox_insert_or_ignore(
            &conn,
            "dispB",
            "shared-uuid",
            "https://b/cb",
            &expires,
        )
        .unwrap();
        assert_eq!(
            n2, 0,
            "unique index on job_id must prevent a second insert under a different dispatcher"
        );

        // Verify the stored row still belongs to dispA — the cross-dispatcher
        // reuse did NOT overwrite it.
        let stored = fleet_outbox_lookup(&conn, "shared-uuid").unwrap().unwrap();
        assert_eq!(stored.dispatcher_node_id, "dispA");

        // And there's exactly one row for this job_id.
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM fleet_result_outbox WHERE job_id = 'shared-uuid'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_fleet_outbox_bump_delivery_attempt_increments_counter() {
        let conn = outbox_conn();
        let expires = future_expires();
        fleet_outbox_insert_or_ignore(&conn, "dispA", "job-1", "https://x/cb", &expires).unwrap();
        fleet_outbox_promote_ready_if_pending(&conn, "dispA", "job-1", "{}", 60).unwrap();

        fleet_outbox_bump_delivery_attempt(&conn, "dispA", "job-1", "timeout").unwrap();
        fleet_outbox_bump_delivery_attempt(&conn, "dispA", "job-1", "5xx").unwrap();

        let lookup = fleet_outbox_lookup(&conn, "job-1").unwrap().unwrap();
        assert_eq!(lookup.delivery_attempts, 2);

        // And the gating on status=ready is live: a non-ready row doesn't
        // have its counter bumped.
        fleet_outbox_mark_delivered_if_ready(&conn, "dispA", "job-1", 3600).unwrap();
        fleet_outbox_bump_delivery_attempt(&conn, "dispA", "job-1", "nope").unwrap();
        let lookup_after_delivered =
            fleet_outbox_lookup(&conn, "job-1").unwrap().unwrap();
        assert_eq!(
            lookup_after_delivered.delivery_attempts, 2,
            "bump must not fire after ready transitions to delivered"
        );
    }

    #[test]
    fn test_synthesize_worker_error_json_escapes_quotes_and_backslashes() {
        // Plain message — round-trips cleanly.
        let plain = synthesize_worker_error_json("worker crashed");
        assert_eq!(plain, r#"{"kind":"Error","data":"worker crashed"}"#);

        // Message with embedded double quotes: serde_json escapes them
        // with backslash-quote. The final string must parse back as valid
        // JSON matching the kind/data contract.
        let with_quotes = synthesize_worker_error_json("hello \"world\"");
        assert_eq!(
            with_quotes,
            r#"{"kind":"Error","data":"hello \"world\""}"#,
            "quotes must be escaped, not copied raw"
        );

        // Round-trip through serde_json to prove the output is always
        // well-formed JSON that preserves the original message verbatim.
        let parsed: serde_json::Value =
            serde_json::from_str(&with_quotes).expect("synth output must be valid JSON");
        assert_eq!(parsed["kind"], "Error");
        assert_eq!(parsed["data"], "hello \"world\"");

        // Backslash, newline, tab — all common escape vectors.
        let tricky = synthesize_worker_error_json("a\\b\nc\td");
        let parsed_tricky: serde_json::Value =
            serde_json::from_str(&tricky).expect("synth output with escapes must be valid JSON");
        assert_eq!(parsed_tricky["data"], "a\\b\nc\td");
    }

    #[test]
    fn test_fleet_outbox_expire_exhausted_only_ready_rows() {
        let conn = outbox_conn();
        let expires = future_expires();
        // One ready row at max attempts, one ready row below, one pending
        // row (irrelevant; below is the key invariant).
        fleet_outbox_insert_or_ignore(&conn, "dispA", "exhausted", "https://x/cb", &expires)
            .unwrap();
        fleet_outbox_insert_or_ignore(&conn, "dispA", "below", "https://x/cb", &expires).unwrap();
        fleet_outbox_insert_or_ignore(&conn, "dispA", "pending", "https://x/cb", &expires).unwrap();

        fleet_outbox_promote_ready_if_pending(&conn, "dispA", "exhausted", "{}", 600).unwrap();
        fleet_outbox_promote_ready_if_pending(&conn, "dispA", "below", "{}", 600).unwrap();

        // Bump exhausted row to 5 attempts.
        for _ in 0..5 {
            fleet_outbox_bump_delivery_attempt(&conn, "dispA", "exhausted", "err").unwrap();
        }

        // max_attempts=5 should push only 'exhausted' into the past.
        let n = fleet_outbox_expire_exhausted(&conn, 5).unwrap();
        assert_eq!(n, 1);

        // Sweep now catches only the exhausted row.
        let rows = fleet_outbox_sweep_expired(&conn).unwrap();
        let job_ids: Vec<_> = rows.iter().map(|r| r.job_id.as_str()).collect();
        assert!(job_ids.contains(&"exhausted"));
        assert!(!job_ids.contains(&"below"));
        assert!(!job_ids.contains(&"pending"));
    }

    #[test]
    fn test_fleet_views_migration_uses_new_event_types() {
        let conn = Connection::open_in_memory().unwrap();
        // Running init twice verifies DROP VIEW IF EXISTS + CREATE runs cleanly
        // on upgrade: the first init installs the view, the second init drops
        // and recreates it without error.
        init_pyramid_db(&conn).unwrap();
        init_pyramid_db(&conn).unwrap();

        let peers_sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='view' AND name='v_compute_fleet_peers'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            peers_sql.contains("fleet_dispatched_async"),
            "v_compute_fleet_peers must reference the new event_type name"
        );
        assert!(
            peers_sql.contains("fleet_result_received"),
            "v_compute_fleet_peers must reference the new success event_type"
        );
        assert!(
            !peers_sql.contains("'fleet_dispatched'"),
            "v_compute_fleet_peers must not reference the old event_type 'fleet_dispatched'"
        );
        assert!(
            !peers_sql.contains("'fleet_returned'"),
            "v_compute_fleet_peers must not reference the old event_type 'fleet_returned'"
        );

        let by_source_sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='view' AND name='v_compute_by_source'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            by_source_sql.contains("fleet_result_received"),
            "v_compute_by_source must reference the new event_type name"
        );
        assert!(
            !by_source_sql.contains("'fleet_returned'"),
            "v_compute_by_source must not reference the old event_type 'fleet_returned'"
        );
    }
}
