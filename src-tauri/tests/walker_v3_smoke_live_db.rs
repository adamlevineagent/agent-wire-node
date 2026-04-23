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
