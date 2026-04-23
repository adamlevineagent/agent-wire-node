//! Walker v3 Phase A migration integration test (plan rev 1.0.2 §5.3, W1a).
//!
//! Seeds a minimal-but-realistic legacy state (pyramid_tier_routing rows
//! across openrouter + local + market, an active dispatch_policy
//! contribution with routing_rules.route_to, a migration_marker at v2),
//! then runs `run_v3_phase_a_migration` and asserts the post-migration
//! state is what §5.3 Phase A promises:
//!
//!   * Walker_provider_{openrouter,local,market} rows present and
//!     carrying the right model_list + context_limit entries.
//!   * Walker_call_order row present with order matching
//!     dispatch_policy.routing_rules.route_to (first rule).
//!   * Migration_marker superseded from `v2` to
//!     `v3-db-migrated-config-pending`.
//!   * _pre_v3_snapshot_pyramid_tier_routing has the expected rows.
//!   * `build_scope_cache` returns a chain populated from the migrated
//!     contributions (round-trip sanity: resolver sees what migration
//!     wrote).
//!
//! Uses only `pyramid_config_contributions` + `pyramid_tier_routing` +
//! `pyramid_builds` tables — not the full init_pyramid_db surface. That
//! keeps this test decoupled from unrelated schema churn while still
//! exercising the production envelope-writer path (same code path the
//! unit tests exercise, but with more surface seeded).

use rusqlite::Connection;
use tempfile::TempDir;

use wire_node_lib::pyramid::v3_migration::{run_v3_phase_a_migration, V3MigrationError};
use wire_node_lib::pyramid::walker_resolver::{
    build_scope_cache, resolve_context_limit, resolve_model_list, ProviderType,
};

fn make_integration_db() -> (TempDir, String) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("walker_v3_phase_a_it.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE pyramid_config_contributions (
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
            accepted_at TEXT
        );

        CREATE TABLE pyramid_tier_routing (
            tier_name TEXT PRIMARY KEY,
            provider_id TEXT NOT NULL,
            model_id TEXT NOT NULL,
            context_limit INTEGER,
            max_completion_tokens INTEGER,
            pricing_json TEXT NOT NULL DEFAULT '{}',
            supported_parameters_json TEXT,
            notes TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE pyramid_builds (
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
        "#,
    )
    .unwrap();
    (dir, path.to_string_lossy().to_string())
}

fn insert_marker(conn: &Connection, body: &str) {
    let id = uuid::Uuid::new_v4().to_string();
    let yaml = format!("schema_type: migration_marker\nbody: \"{}\"\n", body);
    conn.execute(
        "INSERT INTO pyramid_config_contributions \
           (contribution_id, schema_type, yaml_content, status, source) \
         VALUES (?1, 'migration_marker', ?2, 'active', 'bundled')",
        rusqlite::params![id, yaml],
    )
    .unwrap();
}

fn seed_realistic_legacy_state(conn: &Connection) {
    insert_marker(conn, "v2");

    // Routing: four tier rows across three provider types.
    conn.execute_batch(
        r#"
        INSERT INTO pyramid_tier_routing
          (tier_name, provider_id, model_id, context_limit, max_completion_tokens,
           pricing_json, supported_parameters_json, notes)
        VALUES
          ('mid', 'openrouter', 'inception/mercury-2', 200000, NULL,
           '{"prompt":"0.000002","completion":"0.000008"}',
           '["tools","response_format"]', 'operator pinned this tier'),
          ('max', 'openrouter', 'x-ai/grok-4.20-beta', 1000000, NULL,
           '{}', NULL, NULL),
          ('extractor', 'ollama-local', 'qwen2.5:14b-instruct', 128000, 4096,
           '{}', NULL, NULL),
          ('synth_heavy', 'market', 'moonshotai/kimi-k2.6', NULL, NULL,
           '{}', NULL, NULL);
        "#,
    )
    .unwrap();

    // dispatch_policy: route_to with a budget cap on fleet.
    let dp_yaml = r#"
version: 1
routing_rules:
  - name: build
    match_config: {}
    route_to:
      - provider_id: market
        max_budget_credits: 1000
      - provider_id: ollama-local
      - provider_id: openrouter
      - provider_id: fleet
"#;
    conn.execute(
        "INSERT INTO pyramid_config_contributions \
           (contribution_id, schema_type, yaml_content, status, source) \
         VALUES (?1, 'dispatch_policy', ?2, 'active', 'bundled')",
        rusqlite::params!["dp-integration", dp_yaml],
    )
    .unwrap();
}

#[test]
fn phase_a_migration_end_to_end() {
    let (_dir, path) = make_integration_db();
    let mut conn = Connection::open(&path).unwrap();
    seed_realistic_legacy_state(&conn);

    let report = run_v3_phase_a_migration(&mut conn, None).expect("Phase A must succeed");

    // Three provider-type groups → three walker_provider_* rows.
    assert!(
        report.walker_provider_contributions_written.len() >= 3,
        "expected ≥3 walker_provider_* writes, got {}",
        report.walker_provider_contributions_written.len()
    );
    assert!(report.walker_call_order_written.is_some());
    assert_eq!(report.snapshot_rows_dumped, 4);
    assert_eq!(report.marker_transitioned_from, "v2");
    assert_eq!(
        report.marker_transitioned_to,
        "v3-db-migrated-config-pending"
    );

    // ── Resolver round-trip: build_scope_cache must see the data that
    //    migration wrote. ────────────────────────────────────────────
    let cache = build_scope_cache(&conn).expect("build_scope_cache must succeed");
    let chain = cache.scope_chain.clone();

    // walker_provider_openrouter.overrides.model_list[mid] = [inception/mercury-2]
    let or_mid = resolve_model_list(&chain, "mid", ProviderType::OpenRouter);
    assert_eq!(
        or_mid,
        Some(vec!["inception/mercury-2".to_string()]),
        "openrouter mid model_list must carry the migrated slug"
    );
    let or_max = resolve_model_list(&chain, "max", ProviderType::OpenRouter);
    assert_eq!(or_max, Some(vec!["x-ai/grok-4.20-beta".to_string()]));

    // context_limit per tier.
    assert_eq!(
        resolve_context_limit(&chain, "mid", ProviderType::OpenRouter),
        Some(200_000)
    );
    assert_eq!(
        resolve_context_limit(&chain, "max", ProviderType::OpenRouter),
        Some(1_000_000)
    );

    // Local (ollama-local → local).
    let local_ex = resolve_model_list(&chain, "extractor", ProviderType::Local);
    assert_eq!(
        local_ex,
        Some(vec!["qwen2.5:14b-instruct".to_string()]),
        "local extractor model_list must carry the migrated slug"
    );

    // Market.
    let mkt_synth = resolve_model_list(&chain, "synth_heavy", ProviderType::Market);
    assert_eq!(
        mkt_synth,
        Some(vec!["moonshotai/kimi-k2.6".to_string()]),
        "market synth_heavy model_list must carry the migrated slug"
    );

    // walker_call_order: order should match dispatch_policy.route_to's
    // first-rule provider sequence (market, local, openrouter, fleet).
    assert_eq!(
        chain.call_order,
        vec![
            ProviderType::Market,
            ProviderType::Local,
            ProviderType::OpenRouter,
            ProviderType::Fleet,
        ]
    );

    // Migration marker transitioned.
    let marker_body: String = conn
        .query_row(
            "SELECT yaml_content FROM pyramid_config_contributions \
             WHERE schema_type = 'migration_marker' AND status = 'active' \
               AND superseded_by_id IS NULL LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        marker_body.contains("v3-db-migrated-config-pending"),
        "marker body after migration: {marker_body}"
    );

    // Snapshot rows present.
    let snap_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM _pre_v3_snapshot_pyramid_tier_routing",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(snap_rows, 4);

    // `_notes` preserved in the openrouter body (underscore-prefix metadata).
    let or_body: String = conn
        .query_row(
            "SELECT yaml_content FROM pyramid_config_contributions \
             WHERE schema_type = 'walker_provider_openrouter' AND status = 'active' \
               AND superseded_by_id IS NULL LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        or_body.contains("_notes:") && or_body.contains("operator pinned this tier"),
        "openrouter body should preserve _notes: {or_body}"
    );
}

#[test]
fn phase_a_migration_idempotent_on_rerun() {
    let (_dir, path) = make_integration_db();
    let mut conn = Connection::open(&path).unwrap();
    insert_marker(&conn, "v2");

    let _ = run_v3_phase_a_migration(&mut conn, None).expect("first run must succeed");
    // Rerun must return AlreadyMigrated.
    let err = run_v3_phase_a_migration(&mut conn, None).unwrap_err();
    match err {
        V3MigrationError::AlreadyMigrated { body } => {
            assert_eq!(body, "v3-db-migrated-config-pending");
        }
        other => panic!("expected AlreadyMigrated, got {:?}", other),
    }
}

#[test]
fn phase_a_migration_blocks_on_in_progress_builds() {
    let (_dir, path) = make_integration_db();
    let mut conn = Connection::open(&path).unwrap();
    insert_marker(&conn, "v2");
    conn.execute(
        "INSERT INTO pyramid_builds (slug, build_id, question, status) \
         VALUES ('hot-pyramid', 'build-42', 'why', 'running')",
        [],
    )
    .unwrap();

    let err = run_v3_phase_a_migration(&mut conn, None).unwrap_err();
    match err {
        V3MigrationError::InProgressBuildsBlock(slugs) => {
            assert_eq!(slugs, vec!["hot-pyramid".to_string()]);
        }
        other => panic!("expected InProgressBuildsBlock, got {:?}", other),
    }
}

#[test]
fn phase_a_migration_hard_fails_on_unknown_provider_id() {
    let (_dir, path) = make_integration_db();
    let mut conn = Connection::open(&path).unwrap();
    insert_marker(&conn, "v2");
    conn.execute(
        "INSERT INTO pyramid_tier_routing \
           (tier_name, provider_id, model_id, pricing_json) \
         VALUES ('mid', 'wild-card', 'some-model', '{}')",
        [],
    )
    .unwrap();

    let err = run_v3_phase_a_migration(&mut conn, None).unwrap_err();
    match err {
        V3MigrationError::UnknownProviderIds { ids } => {
            assert_eq!(ids, vec!["wild-card".to_string()]);
        }
        other => panic!("expected UnknownProviderIds, got {:?}", other),
    }

    // Transaction rolled back: no walker_provider_* rows, marker still v2.
    let walker_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pyramid_config_contributions \
             WHERE schema_type LIKE 'walker_provider_%' AND status = 'active'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(walker_count, 0, "no walker_provider_* rows on rollback");
}
