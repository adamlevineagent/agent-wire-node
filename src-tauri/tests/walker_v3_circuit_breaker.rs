//! Walker v3 Phase 5 integration test — per-build circuit breaker.
//!
//! Plan rev 1.0.2 §2.16.6 + §3 `breaker_reset` + §E breaker
//! consultation in the Decision builder. End-to-end:
//!
//!   * Seed DB + build a Decision → record 3 failures → next Decision
//!     build drops the provider from `effective_call_order`.
//!   * `per_build` reset policy persists the trip across re-builds.
//!   * `time_secs:N` reset policy untrips after the interval.
//!   * `clear_build` drops all cells for the build_id (RAII guard
//!     simulated by explicit call).
//!
//! Phase 5 does NOT replace OpenRouterReadinessStub, so it's the
//! cleanest provider to pin an effective_call_order on in tests —
//! Ready stub returns Ready unconditionally, so the Decision's
//! `effective_call_order` hinges only on the breaker gate.

use rusqlite::Connection;
use tempfile::TempDir;

use wire_node_lib::pyramid::walker_breaker::{
    breaker_test_lock, clear_all_for_tests, clear_build, record_failure, TRIP_THRESHOLD,
};
use wire_node_lib::pyramid::walker_decision::DispatchDecision;
use wire_node_lib::pyramid::walker_resolver::{BreakerReset, ProviderType};

/// Create the minimal `pyramid_config_contributions` schema used by
/// `build_scope_cache` (and therefore by `DispatchDecision::build`).
fn make_it_db() -> (TempDir, Connection) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("walker_v3_circuit_breaker_it.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE pyramid_config_contributions (
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
         );",
    )
    .unwrap();
    (dir, conn)
}

fn insert_active(conn: &Connection, contribution_id: &str, schema_type: &str, yaml: &str) {
    conn.execute(
        "INSERT INTO pyramid_config_contributions (
             contribution_id, slug, schema_type, yaml_content, status, source
         ) VALUES (?1, NULL, ?2, ?3, 'active', 'bundled')",
        rusqlite::params![contribution_id, schema_type, yaml],
    )
    .unwrap();
}

/// Seed an OpenRouter-only call order at slot `mid` so the Decision
/// builder's effective_call_order is exactly `[OpenRouter]` when
/// readiness succeeds — easy to assert drop when the breaker trips.
fn seed_or_only(conn: &Connection) {
    insert_active(
        conn,
        "c-sp-or-only",
        "walker_slot_policy",
        r#"
schema_type: walker_slot_policy
version: 1
slots:
  mid:
    order: [openrouter]
"#,
    );
}

#[test]
fn trip_per_build_excludes_provider_from_next_decision() {
    let _g = breaker_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    clear_all_for_tests();
    let (_dir, conn) = make_it_db();
    seed_or_only(&conn);
    let bid = "integ-build-trip-per-build";
    // Sanity: before any failures, Decision must include OpenRouter.
    let d1 =
        DispatchDecision::build_with_build_id("mid", Some(bid), &conn).expect("baseline build");
    assert_eq!(d1.effective_call_order, vec![ProviderType::OpenRouter]);
    // Record TRIP_THRESHOLD failures.
    for _ in 0..TRIP_THRESHOLD {
        record_failure(bid, "mid", ProviderType::OpenRouter);
    }
    // Next Decision build for the same (build_id, slot, OpenRouter)
    // must drop OpenRouter → effective_call_order empty →
    // NoReadyProviders error.
    let err = DispatchDecision::build_with_build_id("mid", Some(bid), &conn)
        .expect_err("should error after breaker trip");
    let msg = format!("{err}");
    assert!(msg.contains("mid"));
    clear_all_for_tests();
}

#[test]
fn trip_on_one_build_does_not_affect_another_build() {
    let _g = breaker_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    clear_all_for_tests();
    let (_dir, conn) = make_it_db();
    seed_or_only(&conn);
    let bad = "integ-build-bad";
    let good = "integ-build-good";
    for _ in 0..TRIP_THRESHOLD {
        record_failure(bad, "mid", ProviderType::OpenRouter);
    }
    // The `good` build_id sees a fresh, empty breaker state.
    let d = DispatchDecision::build_with_build_id("mid", Some(good), &conn).unwrap();
    assert_eq!(d.effective_call_order, vec![ProviderType::OpenRouter]);
    // `bad` still trips.
    let err = DispatchDecision::build_with_build_id("mid", Some(bad), &conn).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("mid"));
    clear_all_for_tests();
}

#[test]
fn clear_build_resets_breaker_state() {
    let _g = breaker_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    clear_all_for_tests();
    let (_dir, conn) = make_it_db();
    seed_or_only(&conn);
    let bid = "integ-build-clear";
    for _ in 0..TRIP_THRESHOLD {
        record_failure(bid, "mid", ProviderType::OpenRouter);
    }
    // Tripped.
    assert!(DispatchDecision::build_with_build_id("mid", Some(bid), &conn).is_err());
    // Clear the build's breaker cells.
    clear_build(bid);
    // A fresh Decision for the same build_id sees Ready again.
    let d = DispatchDecision::build_with_build_id("mid", Some(bid), &conn).unwrap();
    assert_eq!(d.effective_call_order, vec![ProviderType::OpenRouter]);
    clear_all_for_tests();
}

#[test]
fn time_secs_reset_untrips_after_interval() {
    // Unit-scale — 1 second interval keeps the test fast while still
    // exercising the time-based reset logic end-to-end via is_tripped.
    use std::thread::sleep;
    use std::time::Duration;
    use wire_node_lib::pyramid::walker_breaker::is_tripped;
    let _g = breaker_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    clear_all_for_tests();
    let bid = "integ-build-timesecs";
    for _ in 0..TRIP_THRESHOLD {
        record_failure(bid, "mid", ProviderType::OpenRouter);
    }
    assert!(is_tripped(
        bid,
        "mid",
        ProviderType::OpenRouter,
        BreakerReset::TimeSecs { value: 1 },
    ));
    sleep(Duration::from_millis(1100));
    assert!(!is_tripped(
        bid,
        "mid",
        ProviderType::OpenRouter,
        BreakerReset::TimeSecs { value: 1 },
    ));
    clear_all_for_tests();
}

#[test]
fn build_without_build_id_bypasses_breaker() {
    // `DispatchDecision::build` (no build_id) skips the breaker gate.
    // Even with a tripped cell on some other build_id, the Decision
    // surfaces Ready providers as usual — preserves the pre-Phase-5
    // call-site contract for legacy paths that haven't been threaded
    // through `build_with_build_id`.
    let _g = breaker_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    clear_all_for_tests();
    let (_dir, conn) = make_it_db();
    seed_or_only(&conn);
    for _ in 0..TRIP_THRESHOLD {
        record_failure("some-other-build", "mid", ProviderType::OpenRouter);
    }
    let d = DispatchDecision::build("mid", &conn).expect("build without build_id");
    assert_eq!(d.effective_call_order, vec![ProviderType::OpenRouter]);
    clear_all_for_tests();
}
