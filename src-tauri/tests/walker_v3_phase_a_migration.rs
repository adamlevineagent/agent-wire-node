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

use wire_node_lib::pyramid::v3_migration::{
    run_v3_phase_a_migration, run_v3_phase_b_migration, V3MigrationError,
};
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

// ── Phase A + Phase B end-to-end ────────────────────────────────────

/// Drives the full two-phase migration against a test DB seeded with:
///   * legacy `pyramid_tier_routing` rows across openrouter + local + market
///   * active `dispatch_policy` contribution with `routing_rules.route_to`
///   * `pyramid_config.json` on disk containing the legacy model fields
///   * `migration_marker` body = `v2`
///
/// Asserts the final state after Phase A → Phase B:
///   (a) walker_provider_openrouter body carries the migrated routing
///       data AND the folded primary/fallback chain from the JSON.
///   (b) pyramid_config.json has the legacy keys stripped and other
///       keys preserved verbatim.
///   (c) migration_marker body is `"v3"` and exactly one active row.
///   (d) Both snapshots (`_pre_v3_snapshot_pyramid_tier_routing` and
///       `_pre_v3_snapshot_config`) hold pre-migration state.
#[test]
fn phase_a_then_phase_b_end_to_end() {
    let (_dir, path) = make_integration_db();
    // TempDir for pyramid_config.json location. Use a second TempDir
    // rather than reusing `_dir` so the DB and config file are side-by-
    // side as they would be in production (data_dir holds both).
    let data_dir = tempfile::TempDir::new().unwrap();
    let mut conn = Connection::open(&path).unwrap();
    seed_realistic_legacy_state(&conn);

    // Seed pyramid_config.json on disk with legacy fields + non-legacy
    // survivors.
    let original_config = serde_json::json!({
        "auth_token": "bearer-xyz",
        "openrouter_api_key": "sk-or-v1-test",
        "primary_model": "inception/mercury-2",
        "fallback_model_1": "x-ai/grok-4.20-beta",
        "fallback_model_2": "moonshotai/kimi-k2.6",
        "primary_context_limit": 200000,
        "fallback_1_context_limit": 1000000,
        "partner_model": "xiaomi/mimo-v2-pro",
        "operational": {},
    });
    let original_bytes = serde_json::to_string_pretty(&original_config).unwrap();
    std::fs::write(data_dir.path().join("pyramid_config.json"), &original_bytes).unwrap();

    // Phase A.
    let report_a = run_v3_phase_a_migration(&mut conn, Some(data_dir.path()))
        .expect("Phase A must succeed");
    assert_eq!(
        report_a.marker_transitioned_to,
        "v3-db-migrated-config-pending"
    );

    // Phase B.
    let report_b =
        run_v3_phase_b_migration(&mut conn, data_dir.path()).expect("Phase B must succeed");
    assert!(report_b.bytes_before > 0, "Phase B must see the seeded config");
    assert!(report_b.bytes_after > 0);
    assert!(
        report_b.bytes_after < report_b.bytes_before,
        "stripped config must be smaller (before={}, after={})",
        report_b.bytes_before,
        report_b.bytes_after
    );

    // (a) walker_provider_openrouter body has migrated data AND the
    // folded primary/fallback chain (via Phase A reading the JSON).
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
        or_body.contains("inception/mercury-2"),
        "openrouter body must carry migrated mid slug: {or_body}"
    );
    assert!(
        or_body.contains("x-ai/grok-4.20-beta"),
        "openrouter body must carry grok slug from routing table: {or_body}"
    );

    // (b) pyramid_config.json stripped of legacy keys, non-legacy preserved.
    let rewritten: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(data_dir.path().join("pyramid_config.json")).unwrap(),
    )
    .unwrap();
    for legacy_key in [
        "primary_model",
        "fallback_model_1",
        "fallback_model_2",
        "primary_context_limit",
        "fallback_1_context_limit",
    ] {
        assert!(
            rewritten.get(legacy_key).is_none(),
            "legacy key `{legacy_key}` must be stripped: {rewritten}"
        );
    }
    assert_eq!(
        rewritten.get("auth_token").and_then(|v| v.as_str()),
        Some("bearer-xyz")
    );
    assert_eq!(
        rewritten.get("partner_model").and_then(|v| v.as_str()),
        Some("xiaomi/mimo-v2-pro")
    );

    // (c) migration_marker body is `v3` and exactly one active row.
    let active_marker: String = conn
        .query_row(
            "SELECT yaml_content FROM pyramid_config_contributions \
             WHERE schema_type = 'migration_marker' AND status = 'active' \
               AND superseded_by_id IS NULL LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(active_marker.contains("\"v3\""), "marker: {active_marker}");
    assert!(
        !active_marker.contains("v3-db-migrated-config-pending"),
        "pending body must not remain active: {active_marker}"
    );
    let active_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pyramid_config_contributions \
             WHERE schema_type = 'migration_marker' AND status = 'active' \
               AND superseded_by_id IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(active_count, 1, "exactly one active marker row");

    // (d) Both snapshots populated.
    let routing_snap_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM _pre_v3_snapshot_pyramid_tier_routing",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(routing_snap_rows, 4, "routing snapshot row count");
    let config_snap_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM _pre_v3_snapshot_config",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(config_snap_rows, 1, "config snapshot must have one row");

    // Snapshot row carries the pre-rewrite body bytes verbatim.
    let snap_body: String = conn
        .query_row(
            "SELECT body FROM _pre_v3_snapshot_config LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(snap_body, original_bytes);
}

/// Regression test for the bundled-vs-migrated unique-index collision.
///
/// Production boot order: `walk_bundled_contributions_manifest` runs
/// first and inserts active `walker_provider_openrouter`,
/// `walker_call_order`, and friends (slug=NULL, source='bundled').
/// Then `run_walker_cache_boot` calls Phase A which also wants to
/// INSERT `status='active'` rows for those same schema_types. With the
/// `uq_config_contrib_active` partial unique index enforced on
/// `(COALESCE(slug,'__global__'), schema_type) WHERE status='active'`,
/// the second INSERT collides and SupersessionConflict bubbles up as
/// `V3MigrationError::Other` — boot aborts on any operator DB with
/// pyramid_tier_routing rows.
///
/// This test seeds bundled-style active rows BEFORE Phase A runs and
/// asserts Phase A completes successfully, leaving exactly one active
/// row per affected (schema_type, slug) pair.
#[test]
fn phase_a_coexists_with_bundled_walker_rows() {
    let (_dir, path) = make_integration_db();
    let mut conn = Connection::open(&path).unwrap();
    // Standard legacy state: marker=v2, 4 tier_routing rows (including
    // openrouter + ollama-local + market), an active dispatch_policy.
    seed_realistic_legacy_state(&conn);

    // Simulate the bundled manifest walk that runs at every boot before
    // Phase A. We only need the walker_* schema_types that Phase A
    // writes; schema_annotation + schema_definition + skill rows aren't
    // relevant to the collision.
    for (contrib_id, schema_type, body) in [
        (
            "bundled-walker_provider_openrouter-default-v1",
            "walker_provider_openrouter",
            "schema_type: walker_provider_openrouter\nversion: 1\noverrides:\n  model_list:\n    mid:\n      - \"inception/mercury-2\"\n",
        ),
        (
            "bundled-walker_provider_local-default-v1",
            "walker_provider_local",
            "schema_type: walker_provider_local\nversion: 1\noverrides:\n  model_list: {}\n",
        ),
        (
            "bundled-walker_provider_fleet-default-v1",
            "walker_provider_fleet",
            "schema_type: walker_provider_fleet\nversion: 1\noverrides:\n  model_list: {}\n",
        ),
        (
            "bundled-walker_provider_market-default-v1",
            "walker_provider_market",
            "schema_type: walker_provider_market\nversion: 1\noverrides:\n  active: false\n  model_list: {}\n",
        ),
        (
            "bundled-walker_call_order-default-v1",
            "walker_call_order",
            "schema_type: walker_call_order\nversion: 1\norder: [market, local, openrouter, fleet]\n",
        ),
    ] {
        conn.execute(
            "INSERT INTO pyramid_config_contributions \
               (contribution_id, schema_type, slug, yaml_content, status, source) \
             VALUES (?1, ?2, NULL, ?3, 'active', 'bundled')",
            rusqlite::params![contrib_id, schema_type, body],
        )
        .unwrap();
    }

    // Phase A must NOT hard-fail on uq_config_contrib_active even with
    // the bundled rows present.
    let report =
        run_v3_phase_a_migration(&mut conn, None).expect("Phase A must survive bundled seeds");
    assert_eq!(
        report.marker_transitioned_to,
        "v3-db-migrated-config-pending"
    );
    assert!(
        !report.walker_provider_contributions_written.is_empty(),
        "Phase A must have written at least one walker_provider_* row"
    );
    assert!(
        report.walker_call_order_written.is_some(),
        "Phase A must have written a walker_call_order row"
    );

    // Post-condition: exactly one ACTIVE row per walker_* schema_type
    // with slug=NULL. The prior bundled rows must be 'superseded'.
    for schema_type in [
        "walker_provider_openrouter",
        "walker_provider_local",
        "walker_provider_market",
        "walker_call_order",
    ] {
        let active_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions \
                 WHERE schema_type = ?1 \
                   AND slug IS NULL \
                   AND status = 'active' \
                   AND superseded_by_id IS NULL",
                rusqlite::params![schema_type],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            active_count, 1,
            "expected exactly one active {schema_type} row (slug=NULL); got {active_count}"
        );
    }

    // The new active walker_provider_openrouter row must point back at
    // the superseded bundled row via supersedes_id, and the bundled row
    // must carry superseded_by_id forward. This is the provenance
    // contract Phase 6 rollback relies on.
    let (new_supersedes, bundled_forward): (Option<String>, Option<String>) = {
        let new_supersedes: Option<String> = conn
            .query_row(
                "SELECT supersedes_id FROM pyramid_config_contributions \
                 WHERE schema_type = 'walker_provider_openrouter' \
                   AND slug IS NULL \
                   AND status = 'active' \
                   AND superseded_by_id IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let bundled_forward: Option<String> = conn
            .query_row(
                "SELECT superseded_by_id FROM pyramid_config_contributions \
                 WHERE contribution_id = 'bundled-walker_provider_openrouter-default-v1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        (new_supersedes, bundled_forward)
    };
    assert_eq!(
        new_supersedes.as_deref(),
        Some("bundled-walker_provider_openrouter-default-v1"),
        "new walker_provider_openrouter row must point supersedes_id at the bundled row"
    );
    assert!(
        bundled_forward.is_some(),
        "bundled walker_provider_openrouter row must have superseded_by_id backfilled"
    );
}
