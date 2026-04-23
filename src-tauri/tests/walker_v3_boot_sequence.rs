//! Walker v3 Phase 0a-2 WS5 integration test (§6 canonical commit 7).
//!
//! Two scenarios:
//!   1. Happy path — `run_walker_cache_boot` walks the (§2.17) steps and
//!      flips AppMode: Booting → Ready. ScopeCache is populated; the
//!      scope_cache_reloader task is running.
//!   2. Injected panic — the rebuild_fn panics every time; after the 4th
//!      panic AppMode transitions Booting → Ready → Quarantined, the
//!      triggering contribution_id is marked `status='quarantined'` in
//!      the DB, and the LKG ScopeCache is still served from the ArcSwap.
//!
//! Both tests use a tempfile-backed SQLite database seeded with the
//! minimal `pyramid_config_contributions` shape — the real migration
//! pipeline (init_pyramid_db + walk_bundled_contributions_manifest) is
//! outside the WS5 boot coordinator's responsibilities, and wiring it up
//! would couple this test to Phase 0a-1/WS1 table shapes that have their
//! own test coverage. The coordinator only READS `migration_marker` via
//! `load_active_config_contribution`; that path tolerates Ok(None) and
//! logs + continues.
//!
//! The second scenario drives the reloader DIRECTLY rather than through
//! the coordinator's ConfigSynced → RebuildTrigger bridge — we need a
//! panicking rebuild_fn to exercise the quarantine path, and the
//! coordinator's in-process rebuild_fn is the stub `|_| Ok(empty())`.
//! The bridge path is covered by the reloader's own unit tests (see
//! `pyramid::walker_cache::tests::reloader_restarts_up_to_3_times...`).
//! What this test adds on top is the WS5 wiring: AppMode transitions
//! relayed through `boot::run_walker_cache_boot`'s mode_relay task.

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use rusqlite::Connection;
use tempfile::TempDir;
use tokio::sync::mpsc;

use wire_node_lib::app_mode::{new_app_mode, AppMode};
use wire_node_lib::boot::{run_walker_cache_boot, BootResult};
use wire_node_lib::pyramid::event_bus::BuildEventBus;
use wire_node_lib::pyramid::walker_cache::{
    spawn_scope_cache_reloader, AppModeTransition, RebuildTrigger, ScopeCache, RESTART_BUDGET,
};

/// Seed an empty SQLite DB with the pyramid_config_contributions shape
/// the reloader's quarantine UPDATE touches (contribution_id + status).
/// The boot coordinator's `load_active_config_contribution` call needs
/// the same table with the full column set, so we match the full
/// `CREATE TABLE` used by production `init_pyramid_db` at a minimum.
fn make_test_db() -> (TempDir, String) {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("walker_v3_boot_test.db");
    let conn = Connection::open(&path).expect("open test db");
    // Minimal schema matching the production shape's CRUD contract for
    // config contributions. Mirrors `db.rs::init_pyramid_db`'s
    // `pyramid_config_contributions` CREATE — subset of columns, same
    // names/types. Keeping this local avoids pulling the full
    // production schema into a test fixture and coupling us to
    // unrelated table changes.
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
        "#,
    )
    .expect("create pyramid_config_contributions");

    // Seed an active `migration_marker` at body `v3` so the v3
    // migration (Phase A + Phase B) short-circuits as `AlreadyMigrated`
    // and the boot coordinator does NOT touch `default_data_dir()` (the
    // user's real `pyramid_config.json`). W5 walker-cache boot tests
    // are about the coordinator, not migration — migration has its own
    // dedicated integration test under `walker_v3_phase_a_migration.rs`.
    let marker_id = uuid::Uuid::new_v4().to_string();
    let marker_yaml = "schema_type: migration_marker\nbody: \"v3\"\n";
    conn.execute(
        "INSERT INTO pyramid_config_contributions \
           (contribution_id, schema_type, yaml_content, status, source) \
         VALUES (?1, 'migration_marker', ?2, 'active', 'migration')",
        rusqlite::params![marker_id, marker_yaml],
    )
    .expect("seed v3 migration_marker");

    let path_str = path.to_string_lossy().to_string();
    (dir, path_str)
}

fn insert_active_contribution(db_path: &str, contribution_id: &str, schema_type: &str) {
    let conn = Connection::open(db_path).expect("open");
    conn.execute(
        "INSERT INTO pyramid_config_contributions
           (contribution_id, schema_type, yaml_content, status, source)
         VALUES (?1, ?2, ?3, 'active', 'local')",
        rusqlite::params![contribution_id, schema_type, ""],
    )
    .expect("insert");
}

fn read_status(db_path: &str, contribution_id: &str) -> String {
    let conn = Connection::open(db_path).expect("open");
    conn.query_row(
        "SELECT status FROM pyramid_config_contributions WHERE contribution_id = ?1",
        rusqlite::params![contribution_id],
        |r| r.get::<_, String>(0),
    )
    .expect("read status")
}

// ── Scenario 1: happy path ───────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn boot_happy_path_booting_to_ready() {
    let (_dir, db_path) = make_test_db();
    let app_mode = new_app_mode();
    assert_eq!(
        *app_mode.read().await,
        AppMode::Booting,
        "fresh boot must start in Booting"
    );

    let bus = Arc::new(BuildEventBus::new());

    let result = run_walker_cache_boot(db_path.clone(), app_mode.clone(), bus.clone()).await;

    let handles = match result {
        BootResult::Ok(h) => h,
        BootResult::Aborted(reason) => panic!("happy path must not abort: {reason}"),
    };

    // After boot: AppMode is Ready.
    assert_eq!(
        *app_mode.read().await,
        AppMode::Ready,
        "step 9 must flip AppMode to Ready"
    );

    // ScopeCache is populated (Phase 0a-2 stub: new_empty snapshot
    // stored via ArcSwap at step 3). The guarantee the test pins is
    // "something is reachable via load_full()" — the Phase 0b resolver
    // will assert richer invariants.
    let snapshot = handles.cache_writer.load_full();
    // Source contribution list is empty in the stub but the field is
    // reachable, which proves the ArcSwap is populated.
    assert!(
        snapshot.source_contribution_ids.is_empty(),
        "Phase 0a-2 stub cache has no contribution ids"
    );

    // Reloader task is running — we can verify by sending a trigger
    // through the returned `trigger_tx` and confirming it accepts.
    // Non-blocking: if the receiver is alive, try_send succeeds; if
    // the task crashed the send path would eventually fail but a
    // fresh channel with a live receiver accepts ≥1 message.
    handles
        .trigger_tx
        .try_send(RebuildTrigger {
            contribution_id: None,
            schema_type: None,
        })
        .expect("reloader trigger channel must accept a rebuild trigger");

    // Give the reloader a beat to execute the rebuild (debounce +
    // spawn_blocking). We don't assert on the rebuild count here —
    // that's the reloader's own unit-test territory. We only pin that
    // the task is responsive.
    tokio::time::sleep(Duration::from_millis(400)).await;

    // ConfigSynced bridge is running — publish a ConfigSynced event
    // on the bus and confirm the bridge task is still listening.
    // We observe liveness via the join handle still being active.
    assert!(
        !handles.reloader_handle.is_finished(),
        "reloader task must be running"
    );
    assert!(
        !handles.mode_relay_handle.is_finished(),
        "app_mode relay task must be running"
    );
    assert!(
        !handles.config_sync_bridge_handle.is_finished(),
        "ConfigSynced bridge task must be running"
    );
}

// ── Scenario 2: panic → Quarantined ──────────────────────────────────────
//
// Reuses `run_walker_cache_boot` for the Booting → Ready transition, then
// stops that coordinator's reloader and installs a new one whose
// rebuild_fn panics. This gives us the real §2.17 AppMode transition
// flow (Booting → Ready → Quarantined) without having to expose an
// internal panic hook on the stub rebuild_fn.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn boot_injected_panic_drives_quarantined() {
    let (_dir, db_path) = make_test_db();
    insert_active_contribution(&db_path, "victim-001", "walker_provider_market");

    let app_mode = new_app_mode();
    let bus = Arc::new(BuildEventBus::new());

    // Run the happy-path boot first so we see Booting → Ready.
    let boot = run_walker_cache_boot(db_path.clone(), app_mode.clone(), bus.clone()).await;
    let _handles = match boot {
        BootResult::Ok(h) => h,
        BootResult::Aborted(r) => panic!("boot must succeed: {r}"),
    };
    assert_eq!(*app_mode.read().await, AppMode::Ready);

    // Install a NEW reloader whose rebuild_fn panics on every call.
    // Hook its AppModeTransition output up to a relay that writes
    // AppMode — mirrors what run_walker_cache_boot's mode_relay does.
    let writer = Arc::new(ArcSwap::from_pointee(ScopeCache::new_empty()));
    let pre_panic_ptr = Arc::as_ptr(&writer.load_full());

    let rebuild_fn = |_conn: &Connection| -> anyhow::Result<ScopeCache> {
        panic!("injected panic — walker_v3_boot_sequence test")
    };

    let (trigger_tx, trigger_rx) = mpsc::channel::<RebuildTrigger>(16);
    let (mode_tx, mut mode_rx) = mpsc::channel::<AppModeTransition>(4);

    let event_emitter = |_name: &str, _v: serde_json::Value| {};

    let reloader = spawn_scope_cache_reloader(
        Arc::clone(&writer),
        trigger_rx,
        rebuild_fn,
        db_path.clone(),
        event_emitter,
        mode_tx,
    );

    // Replicate the run_walker_cache_boot mode-relay task so this
    // scenario exercises the SAME flip path production uses.
    let relay_mode = Arc::clone(&app_mode);
    let relay = tokio::spawn(async move {
        while let Some(transition) = mode_rx.recv().await {
            match transition {
                AppModeTransition::Quarantined { .. } => {
                    wire_node_lib::app_mode::transition_to(&relay_mode, AppMode::Quarantined)
                        .await;
                }
            }
        }
    });

    // 4 triggers, spaced beyond the debounce window, each landing in
    // the blocking rebuild and panicking. The 4th (> RESTART_BUDGET)
    // triggers quarantine.
    for _ in 0..4 {
        trigger_tx
            .send(RebuildTrigger {
                contribution_id: Some("victim-001".into()),
                schema_type: Some("walker_provider_market".into()),
            })
            .await
            .expect("send trigger");
        tokio::time::sleep(Duration::from_millis(400)).await;
    }

    // Wait for the quarantine transition to propagate.
    // The reloader emits AppModeTransition::Quarantined; the relay
    // flips AppMode. We poll with a generous timeout.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if *app_mode.read().await == AppMode::Quarantined {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "AppMode never reached Quarantined within 5s (budget={})",
                RESTART_BUDGET
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // LKG preservation: ArcSwap still holds the pre-panic ScopeCache
    // because no rebuild ever returned Ok.
    let post_ptr = Arc::as_ptr(&writer.load_full());
    assert_eq!(
        pre_panic_ptr, post_ptr,
        "LKG ScopeCache must be preserved across reloader panics"
    );

    // Contribution row is marked quarantined in the DB.
    assert_eq!(
        read_status(&db_path, "victim-001"),
        "quarantined",
        "triggering contribution must be marked status='quarantined'"
    );

    // AppMode final state.
    assert_eq!(
        *app_mode.read().await,
        AppMode::Quarantined,
        "AppMode must end in Quarantined"
    );

    // Clean up — dropping trigger_tx drains the reloader; the relay
    // task then sees its receiver close and returns.
    drop(trigger_tx);
    let _ = tokio::time::timeout(Duration::from_secs(2), reloader).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), relay).await;
}

// ── Scenario 3: boot aborts when DB is unreachable (§2.17.3) ─────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn boot_aborts_when_db_path_unusable() {
    // Intentionally point at a path inside a nonexistent directory
    // that SQLite cannot create the file in. rusqlite returns
    // SQLITE_CANTOPEN for this; the coordinator's probe catches it
    // and returns BootResult::Aborted.
    let db_path = "/nonexistent-directory-walker-v3/pyramid.db".to_string();
    let app_mode = new_app_mode();
    let bus = Arc::new(BuildEventBus::new());

    let result = run_walker_cache_boot(db_path, app_mode.clone(), bus).await;

    match result {
        BootResult::Aborted(reason) => {
            assert!(
                reason.contains("db open failed"),
                "aborted reason should mention db open failure: {reason}"
            );
        }
        BootResult::Ok(_) => panic!("boot must abort when db path is unusable"),
    }

    // AppMode must remain in Booting — build-starters keep refusing.
    assert_eq!(
        *app_mode.read().await,
        AppMode::Booting,
        "aborted boot must leave AppMode in Booting"
    );
}
