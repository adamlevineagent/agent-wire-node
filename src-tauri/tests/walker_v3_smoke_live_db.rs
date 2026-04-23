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
