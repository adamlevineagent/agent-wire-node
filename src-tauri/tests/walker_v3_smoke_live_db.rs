//! Walker v3 Phase 0a-1 commit 5 + Phase 0a-2 WS5 smoke against a real
//! dev DB snapshot.
//!
//! Run with:
//!   PYRAMID_DB=/tmp/walker-v3-smoke-XXXXX/pyramid.db \
//!   cargo test --test walker_v3_smoke_live_db -- --nocapture --ignored
//!
//! Validates: migration runs cleanly on the copy, creates uq_config_contrib_active
//! + _pre_v3_dedup_snapshot, preserves 155 active rows (0 dups = 0 moved),
//! and is idempotent on a second call. Second test (Phase 0a-2 WS5) exercises
//! `run_walker_cache_boot` against the copy and asserts Booting → Ready.

use rusqlite::Connection;
use std::sync::Arc;
use std::time::Duration;

use wire_node_lib::app_mode::{new_app_mode, AppMode};
use wire_node_lib::boot::{run_walker_cache_boot, BootResult};
use wire_node_lib::pyramid::event_bus::BuildEventBus;

#[test]
#[ignore]
fn migration_runs_cleanly_on_live_db_copy() {
    let path = std::env::var("PYRAMID_DB")
        .expect("set PYRAMID_DB=/tmp/walker-v3-smoke-XXXXX/pyramid.db");
    let conn = Connection::open(&path).expect("open db copy");

    let pre_active: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pyramid_config_contributions WHERE status='active'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let pre_total: i64 = conn
        .query_row("SELECT COUNT(*) FROM pyramid_config_contributions", [], |r| r.get(0))
        .unwrap();
    let pre_dup: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM (
               SELECT COALESCE(slug,'__global__') k, schema_type, COUNT(*) c
               FROM pyramid_config_contributions WHERE status='active'
               GROUP BY k, schema_type HAVING c > 1
             )",
            [],
            |r| r.get(0),
        )
        .unwrap();
    println!(
        "BASELINE  active={}  total={}  dup_active_pairs={}",
        pre_active, pre_total, pre_dup
    );

    // First migration call.
    let t0 = std::time::Instant::now();
    wire_node_lib::pyramid::config_contributions::ensure_config_contrib_active_unique_index(&conn)
        .expect("first migration call");
    let dur1 = t0.elapsed();
    println!("FIRST CALL took {:?}", dur1);

    let post_active: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pyramid_config_contributions WHERE status='active'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let index_exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='uq_config_contrib_active'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let snapshot_exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='_pre_v3_dedup_snapshot'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let snapshot_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM _pre_v3_dedup_snapshot", [], |r| r.get(0))
        .unwrap_or(-1);

    println!(
        "POST-1    active={}  index_exists={}  snapshot_exists={}  snapshot_rows={}",
        post_active, index_exists, snapshot_exists, snapshot_rows
    );

    assert_eq!(pre_active, post_active, "no active rows should move with 0 dup pairs");
    assert_eq!(index_exists, 1, "unique index must exist after migration");
    assert_eq!(snapshot_exists, 1, "snapshot table must exist");
    assert_eq!(snapshot_rows, 0, "snapshot must be empty (no dups to record)");

    // Idempotency: second call must not blow up.
    let t1 = std::time::Instant::now();
    wire_node_lib::pyramid::config_contributions::ensure_config_contrib_active_unique_index(&conn)
        .expect("second migration call (idempotent)");
    println!("SECOND CALL (idempotent) took {:?}", t1.elapsed());

    // Try a read via the shim-backed path: load an active row that definitely exists.
    let any_schema_type: String = conn
        .query_row(
            "SELECT schema_type FROM pyramid_config_contributions WHERE status='active' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    println!("SAMPLE active schema_type: {}", any_schema_type);

    // Verify the index actually enforces: try to INSERT a duplicate-active row.
    // We do this in a tx and roll it back so we don't dirty the copy.
    let tx = conn.unchecked_transaction().unwrap();
    let dup_insert: rusqlite::Result<usize> = tx.execute(
        "INSERT INTO pyramid_config_contributions (
            contribution_id, slug, schema_type, yaml_content, wire_native_metadata_json,
            wire_publication_state_json, supersedes_id, superseded_by_id, triggering_note,
            status, source, wire_contribution_id, created_at, accepted_at, created_by
        )
        SELECT 'smoke-dup-probe', slug, schema_type, yaml_content, '{}', '{}',
               NULL, NULL, 'smoke probe', 'active', 'bundled', NULL,
               datetime('now'), datetime('now'), NULL
        FROM pyramid_config_contributions WHERE status='active' LIMIT 1",
        [],
    );
    tx.rollback().unwrap();
    println!("DUP INSERT (expected SQLITE_CONSTRAINT): {:?}", dup_insert);
    assert!(
        matches!(
            dup_insert,
            Err(rusqlite::Error::SqliteFailure(_, _))
        ),
        "duplicate-active insert must be rejected by unique index"
    );

    println!("SMOKE: migration clean, index enforced, baseline preserved.");
}

/// Phase 0a-2 WS5 boot-coordinator smoke: point `run_walker_cache_boot`
/// at the live DB copy and assert AppMode walks Booting → Ready in a
/// reasonable wall-clock. Also confirms the returned handles are live
/// (reloader, mode_relay, ConfigSynced bridge all still polling).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn boot_coordinator_runs_clean_against_live_db_copy() {
    let path = std::env::var("PYRAMID_DB")
        .expect("set PYRAMID_DB=/tmp/walker-v3-smoke-XXXXX/pyramid.db");

    let app_mode = new_app_mode();
    assert_eq!(
        *app_mode.read().await,
        AppMode::Booting,
        "fresh app_mode handle must start at Booting"
    );

    let bus = Arc::new(BuildEventBus::new());

    let t0 = std::time::Instant::now();
    let result = run_walker_cache_boot(path.clone(), app_mode.clone(), bus.clone()).await;
    let dur = t0.elapsed();
    println!("BOOT took {:?}", dur);

    let handles = match result {
        BootResult::Ok(h) => h,
        BootResult::Aborted(reason) => panic!("boot must not abort on live DB copy: {reason}"),
    };

    assert_eq!(
        *app_mode.read().await,
        AppMode::Ready,
        "post-boot AppMode must be Ready"
    );
    assert!(
        dur < Duration::from_secs(5),
        "boot on live DB copy must complete in <5s (got {:?})",
        dur
    );
    assert!(
        !handles.reloader_handle.is_finished(),
        "reloader must be live"
    );
    assert!(
        !handles.mode_relay_handle.is_finished(),
        "mode_relay must be live"
    );
    assert!(
        !handles.config_sync_bridge_handle.is_finished(),
        "config_sync_bridge must be live"
    );
    println!("SMOKE: boot coordinator Booting → Ready on live DB copy in {:?}", dur);
}

/// Phase 0b WS-E live-DB smoke: drive `walker_resolver::build_scope_cache`
/// directly against a real pyramid DB. Two phases:
///
/// 1. Pre-manifest: with no `walker_*` schema_type contributions present
///    yet (first v3 binary on pre-walker-v3 state), return a valid-but-
///    empty cache fast (<100ms). Exposed as a timing sanity check for
///    the fallback-on-error path the boot coordinator takes in step 3.
///
/// 2. Post-manifest: after `walk_bundled_contributions_manifest` inserts
///    the six walker_* defaults, re-building the cache produces a
///    populated `ScopeChain`. Asserts tier_set_from_chain unions the
///    per-provider model_list keys, resolve_patience_secs for
///    (mid, Market) returns SYSTEM_DEFAULT (3600) because the bundled
///    walker_provider_market does NOT override patience_secs, and a
///    runtime `DispatchDecision::build("mid", conn)` produces
///    effective_call_order matching the bundled `walker_call_order`
///    seed (market, local, openrouter, fleet). All four readiness stubs
///    return Ready so every provider is present in per_provider.
///
/// Run with:
///   PYRAMID_DB=/tmp/walker-v3-smoke-XXXXX/pyramid.db \
///   cargo test --test walker_v3_smoke_live_db \
///     phase_0b_scope_cache_populates_from_live_db \
///     -- --nocapture --ignored
#[test]
#[ignore]
fn phase_0b_scope_cache_populates_from_live_db() {
    use wire_node_lib::pyramid::walker_decision::DispatchDecision;
    use wire_node_lib::pyramid::walker_resolver::{
        build_scope_cache, resolve_patience_secs, tier_set_from_chain, ProviderType,
        DEFAULT_CALL_ORDER, PATIENCE_SECS_DEFAULT,
    };
    use wire_node_lib::pyramid::wire_migration::walk_bundled_contributions_manifest;

    let path = std::env::var("PYRAMID_DB")
        .expect("set PYRAMID_DB=/tmp/walker-v3-smoke-XXXXX/pyramid.db");
    let conn = Connection::open(&path).expect("open db copy");

    // ── Phase 1: pre-manifest baseline ──────────────────────────────
    let t0 = std::time::Instant::now();
    let cache_pre = build_scope_cache(&conn).expect("build_scope_cache on live DB copy (pre)");
    let dur_pre = t0.elapsed();

    println!(
        "[pre-manifest]  build_scope_cache took {:?}; source_contribution_ids = {}",
        dur_pre,
        cache_pre.source_contribution_ids.len()
    );
    assert!(
        dur_pre.as_millis() < 100,
        "pre-manifest build_scope_cache should be fast on a real DB (got {:?})",
        dur_pre
    );

    // ── Phase 2: seed the bundled manifest into the live DB copy ───
    //
    // Walks the JSON manifest and inserts walker_* defaults via the
    // envelope writer. INSERT OR IGNORE means repeat runs are no-ops.
    let t_mani = std::time::Instant::now();
    let report = walk_bundled_contributions_manifest(&conn)
        .expect("bundled manifest walk must succeed on live DB copy");
    println!(
        "[manifest-walk] took {:?}; inserted={} skipped={} failed={}",
        t_mani.elapsed(),
        report.inserted,
        report.skipped_already_present,
        report.failed
    );
    assert_eq!(report.failed, 0, "manifest walk had failures");

    // ── Phase 3: post-manifest rebuild + resolver assertions ────────
    let t1 = std::time::Instant::now();
    let cache = build_scope_cache(&conn).expect("build_scope_cache on live DB copy (post)");
    let dur = t1.elapsed();

    println!(
        "[post-manifest] build_scope_cache took {:?}; source_contribution_ids = {}",
        dur,
        cache.source_contribution_ids.len()
    );
    assert!(
        dur.as_millis() < 100,
        "post-manifest build_scope_cache should be fast on a real DB (got {:?})",
        dur
    );
    // All six walker_* seeds (4 providers + call_order + slot_policy)
    // should contribute a source_contribution_id except slot_policy
    // whose bundled body is `slots: {}` (empty) — that row still gets
    // inserted as an active config so its contribution_id is pushed.
    assert!(
        cache.source_contribution_ids.len() >= 4,
        "expected at least 4 walker_* source contributions, got {:?}",
        cache.source_contribution_ids
    );

    // (a) tier_set_from_chain pulls keys from the per-provider
    //     model_list maps at scopes 3 + 4. Bundled openrouter declares
    //     max/high/mid/extractor; market declares mid/high/extractor.
    let tiers = tier_set_from_chain(&cache.scope_chain);
    println!("[post-manifest] tier_set = {:?}", tiers);
    for required in ["max", "high", "mid", "extractor"] {
        assert!(
            tiers.contains(required),
            "expected tier `{required}` in tier_set, got {:?}",
            tiers
        );
    }

    // (b) resolve_patience_secs(slot=mid, Market) = SYSTEM_DEFAULT
    //     (bundled walker_provider_market does not override patience_secs)
    let ps = resolve_patience_secs(&cache.scope_chain, "mid", ProviderType::Market);
    assert_eq!(
        ps, PATIENCE_SECS_DEFAULT,
        "market mid patience_secs must fall through to SYSTEM_DEFAULT"
    );

    // (c) DispatchDecision::build yields the bundled call_order.
    //     All four readiness stubs return Ready today, so
    //     effective_call_order matches the bundled seed's order.
    let t_d = std::time::Instant::now();
    let decision =
        DispatchDecision::build("mid", &conn).expect("DispatchDecision::build must succeed");
    println!(
        "[post-manifest] DispatchDecision::build took {:?}; effective_call_order = {:?}",
        t_d.elapsed(),
        decision
            .effective_call_order
            .iter()
            .map(|p| p.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        decision.effective_call_order,
        DEFAULT_CALL_ORDER.to_vec(),
        "bundled walker_call_order must yield DEFAULT_CALL_ORDER"
    );
    for pt in DEFAULT_CALL_ORDER {
        assert!(
            decision.per_provider.contains_key(&pt),
            "per_provider must include {pt:?}"
        );
    }
    // Market carries active=false per bundled seed.
    let mkt = decision
        .per_provider
        .get(&ProviderType::Market)
        .expect("market present");
    assert!(
        !mkt.active,
        "bundled walker_provider_market ships active=false"
    );
    // OpenRouter mid model_list is [inception/mercury-2].
    let or = decision
        .per_provider
        .get(&ProviderType::OpenRouter)
        .expect("openrouter present");
    assert_eq!(
        or.model_list.as_deref(),
        Some(&["inception/mercury-2".to_string()][..]),
        "openrouter mid model_list from bundled seed"
    );
}

/// W4 live-DB smoke: drives the full Phase A + Phase B migration
/// against a copy of the real pyramid DB + pyramid_config.json. Tests
/// that:
///   * the two-phase sequence completes inside 500ms on real data,
///   * pyramid_config.json ends without the legacy model fields,
///   * migration_marker ends at body `v3`.
///
/// Run with:
///   PYRAMID_DB=/tmp/walker-v3-smoke-XXXXX/pyramid.db \
///   PYRAMID_CONFIG_JSON=/tmp/walker-v3-smoke-XXXXX/pyramid_config.json \
///   cargo test --test walker_v3_smoke_live_db \
///     phase_b_rewrites_live_config_json -- --nocapture --ignored
///
/// NOTE: copy the real files to a tempdir FIRST. This test rewrites
/// the config file referenced by `PYRAMID_CONFIG_JSON` and mutates
/// the DB referenced by `PYRAMID_DB` — pointing either at a live file
/// will corrupt operator state.
#[test]
#[ignore]
fn phase_b_rewrites_live_config_json() {
    use wire_node_lib::pyramid::v3_migration::{
        run_v3_phase_a_migration, run_v3_phase_b_migration, should_run_phase_a,
        should_run_phase_b, V3MigrationError,
    };

    let db_path = std::env::var("PYRAMID_DB")
        .expect("set PYRAMID_DB=/tmp/walker-v3-smoke-XXXXX/pyramid.db");
    let config_path_str = std::env::var("PYRAMID_CONFIG_JSON")
        .expect("set PYRAMID_CONFIG_JSON=/tmp/walker-v3-smoke-XXXXX/pyramid_config.json");
    let config_path = std::path::PathBuf::from(&config_path_str);
    let data_dir = config_path
        .parent()
        .expect("PYRAMID_CONFIG_JSON must have a parent dir")
        .to_path_buf();

    let bytes_before_smoke = std::fs::read_to_string(&config_path)
        .expect("read seeded pyramid_config.json copy")
        .len();

    let mut conn = Connection::open(&db_path).expect("open db copy");

    // ── Phase A (if needed) ─────────────────────────────────────────
    let marker_before_a = should_run_phase_a(&conn).expect("marker probe");
    let t_a = std::time::Instant::now();
    let phase_a_ran = match run_v3_phase_a_migration(&mut conn, Some(&data_dir)) {
        Ok(report) => {
            println!(
                "[phase_a] committed; marker {} -> {}; routing_snapshot_rows={}",
                report.marker_transitioned_from,
                report.marker_transitioned_to,
                report.snapshot_rows_dumped
            );
            true
        }
        Err(V3MigrationError::AlreadyMigrated { body }) => {
            println!("[phase_a] already migrated (marker body = {body})");
            false
        }
        Err(e) => panic!("Phase A must not hard-fail on smoke copy: {e:?}"),
    };
    let dur_a = t_a.elapsed();
    println!(
        "[phase_a] took {:?}; marker_before_a = {:?}",
        dur_a, marker_before_a
    );

    // ── Phase B ─────────────────────────────────────────────────────
    let marker_before_b = should_run_phase_b(&conn).expect("marker probe");
    let t_b = std::time::Instant::now();
    let phase_b_result = run_v3_phase_b_migration(&mut conn, &data_dir);
    let dur_b = t_b.elapsed();

    match phase_b_result {
        Ok(report) => {
            println!(
                "[phase_b] committed; bytes_before={} bytes_after={} marker_transition={}",
                report.bytes_before, report.bytes_after, report.marker_transition
            );
            assert!(report.bytes_after > 0, "rewrite must land a non-empty file");
            assert!(
                report.bytes_after <= report.bytes_before,
                "stripped config must not grow"
            );
        }
        Err(wire_node_lib::pyramid::v3_migration::V3PhaseBError::AlreadyMigrated { body }) => {
            println!("[phase_b] already migrated (marker body = {body})");
        }
        Err(other) => panic!("Phase B must not hard-fail on smoke copy: {other:?}"),
    }
    println!(
        "[phase_b] took {:?}; marker_before_b = {:?}; phase_a_ran_this_pass = {}",
        dur_b, marker_before_b, phase_a_ran
    );

    // ── Assertions on final on-disk state ──────────────────────────
    let rewritten: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&config_path).expect("read rewritten config"),
    )
    .expect("parse rewritten config");
    for legacy_key in [
        "primary_model",
        "fallback_model_1",
        "fallback_model_2",
        "primary_context_limit",
        "fallback_1_context_limit",
    ] {
        assert!(
            rewritten.get(legacy_key).is_none(),
            "legacy key `{legacy_key}` must be absent after Phase B"
        );
    }

    // Marker is at `v3`.
    let active_marker: String = conn
        .query_row(
            "SELECT yaml_content FROM pyramid_config_contributions \
             WHERE schema_type = 'migration_marker' AND status = 'active' \
               AND superseded_by_id IS NULL LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("read active marker");
    assert!(
        active_marker.contains("\"v3\"")
            && !active_marker.contains("v3-db-migrated-config-pending"),
        "marker must be `v3` post-Phase-B: {active_marker}"
    );

    // Phase A + B cumulative budget: <500ms on the 227MB real DB.
    // Only enforce when Phase A actually ran (idempotent re-runs
    // should be well under this).
    let cumulative = dur_a + dur_b;
    println!(
        "[smoke] phase_a + phase_b cumulative = {:?}; bytes_before_smoke = {}",
        cumulative, bytes_before_smoke
    );
    assert!(
        cumulative < Duration::from_millis(500),
        "Phase A + B must complete in <500ms on real DB (got {cumulative:?})"
    );
}
