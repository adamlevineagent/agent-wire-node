//! Walker v3 Phase 4 integration test — FleetReadiness + fleet-probe
//! cache end-to-end against `DispatchDecision::build`.
//!
//! Plan rev 1.0.2 §2.6 + §3 Phase 4. Phase 0a-1 landed a trivial
//! `FleetReadinessStub` that returned Ready unconditionally. Phase 4
//! replaces it with a real impl reading the `walker_fleet_probe`
//! sync cache (populated from the async `FleetRoster` by boot).
//!
//! The integration test bypasses the background fleet roster refresh
//! (which requires a live tokio runtime + a populated `FleetRoster`).
//! It writes into the shared fleet probe cache directly via
//! `walker_fleet_probe::write_cached_peer`, mirroring what the
//! refresh task would do in production.

use rusqlite::Connection;
use tempfile::TempDir;

use wire_node_lib::pyramid::walker_decision::DispatchDecision;
use wire_node_lib::pyramid::walker_fleet_probe::{
    clear_fleet_cache_for_tests, fleet_probe_test_lock, write_cached_peer, CachedFleetPeer,
};
use wire_node_lib::pyramid::walker_market_probe::{
    clear_node_state_for_tests, node_state_test_lock,
};
use wire_node_lib::pyramid::walker_resolver::ProviderType;

/// Serialize tests that mutate the global walker_fleet_probe cache.
fn fleet_it_lock() -> &'static std::sync::Mutex<()> {
    fleet_probe_test_lock()
}

/// Create the minimal `pyramid_config_contributions` schema used by
/// `build_scope_cache`.
fn make_it_db() -> (TempDir, Connection) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("walker_v3_fleet_readiness_it.db");
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

fn seed_walker_provider_fleet(
    conn: &Connection,
    contribution_id: &str,
    slot: &str,
    slugs: &[&str],
) {
    let slugs_yaml = slugs
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let yaml = format!(
        r#"
schema_type: walker_provider_fleet
version: 1
overrides:
  active: true
  model_list:
    {slot}: [{slugs_yaml}]
"#
    );
    conn.execute(
        "INSERT INTO pyramid_config_contributions (
             contribution_id, slug, schema_type, yaml_content, status, source
         ) VALUES (?1, NULL, 'walker_provider_fleet', ?2, 'active', 'bundled')",
        rusqlite::params![contribution_id, yaml],
    )
    .unwrap();
}

fn make_peer(
    node_id: &str,
    models: &[&str],
    last_seen: chrono::DateTime<chrono::Utc>,
    is_v1: bool,
) -> CachedFleetPeer {
    CachedFleetPeer {
        node_id: node_id.to_string(),
        node_handle: Some(format!("@op/{node_id}")),
        announced_models: models.iter().map(|s| s.to_string()).collect(),
        last_seen_at: last_seen,
        is_v1_announcer: is_v1,
    }
}

#[test]
fn phase4_decision_includes_fleet_when_matching_v2_peer_is_fresh() {
    let _g = fleet_it_lock().lock().unwrap_or_else(|p| p.into_inner());
    clear_fleet_cache_for_tests();
    let slug = "phase4-fleet-readiness-happy:latest";
    write_cached_peer(make_peer(
        "peer-happy",
        &[slug],
        chrono::Utc::now(),
        false,
    ));
    let (_dir, conn) = make_it_db();
    seed_walker_provider_fleet(&conn, "c-wpf-happy", "mid", &[slug]);

    let d = DispatchDecision::build("mid", &conn)
        .expect("Decision must build with fleet ready");
    assert!(
        d.effective_call_order.contains(&ProviderType::Fleet),
        "Fleet MUST be in effective_call_order; got {:?}",
        d.effective_call_order
    );
    let f = d
        .per_provider
        .get(&ProviderType::Fleet)
        .expect("Fleet params must be present");
    assert_eq!(f.model_list.as_deref(), Some(&[slug.to_string()][..]));
    // Phase 4: fleet_peer_min_staleness_secs + fleet_prefer_cached ride
    // along on per_provider[Fleet] via the Decision builder.
    assert!(f.fleet_peer_min_staleness_secs.is_some());
    assert!(f.fleet_prefer_cached.is_some());
    clear_fleet_cache_for_tests();
}

#[test]
fn phase4_decision_drops_fleet_when_cache_empty() {
    let _g = fleet_it_lock().lock().unwrap_or_else(|p| p.into_inner());
    clear_fleet_cache_for_tests();
    let slug = "phase4-fleet-readiness-no-peers:latest";
    let (_dir, conn) = make_it_db();
    seed_walker_provider_fleet(&conn, "c-wpf-no-peers", "mid", &[slug]);

    let d = DispatchDecision::build("mid", &conn)
        .expect("non-Fleet providers still build the Decision");
    assert!(
        !d.effective_call_order.contains(&ProviderType::Fleet),
        "Fleet MUST drop when roster is empty; got {:?}",
        d.effective_call_order
    );
}

#[test]
fn phase4_decision_drops_fleet_when_all_peers_stale() {
    let _g = fleet_it_lock().lock().unwrap_or_else(|p| p.into_inner());
    clear_fleet_cache_for_tests();
    let slug = "phase4-fleet-readiness-stale:latest";
    let stale_when = chrono::Utc::now() - chrono::Duration::seconds(5000);
    write_cached_peer(make_peer("peer-stale", &[slug], stale_when, false));
    let (_dir, conn) = make_it_db();
    seed_walker_provider_fleet(&conn, "c-wpf-stale", "mid", &[slug]);

    let d = DispatchDecision::build("mid", &conn)
        .expect("non-Fleet providers still build the Decision");
    assert!(
        !d.effective_call_order.contains(&ProviderType::Fleet),
        "Fleet MUST drop when all peers are stale; got {:?}",
        d.effective_call_order
    );
    clear_fleet_cache_for_tests();
}

#[test]
fn phase4_decision_drops_fleet_when_only_v1_announcers_match() {
    let _g = fleet_it_lock().lock().unwrap_or_else(|p| p.into_inner());
    clear_fleet_cache_for_tests();
    let slug = "phase4-fleet-readiness-v1-only:latest";
    // Matching peer is v1 — reachable but strict-refused per §5.5.2.
    write_cached_peer(make_peer("peer-v1", &[slug], chrono::Utc::now(), true));
    // Extra v2 peer with NO matching model — must not flip the verdict.
    write_cached_peer(make_peer(
        "peer-v2-nomatch",
        &["someone-else:latest"],
        chrono::Utc::now(),
        false,
    ));
    let (_dir, conn) = make_it_db();
    seed_walker_provider_fleet(&conn, "c-wpf-v1", "mid", &[slug]);

    let d = DispatchDecision::build("mid", &conn)
        .expect("non-Fleet providers still build the Decision");
    assert!(
        !d.effective_call_order.contains(&ProviderType::Fleet),
        "Fleet MUST drop when only v1 announcers match; got {:?}",
        d.effective_call_order
    );
    clear_fleet_cache_for_tests();
}

#[test]
fn phase4_decision_drops_fleet_when_no_peer_announces_model() {
    let _g = fleet_it_lock().lock().unwrap_or_else(|p| p.into_inner());
    clear_fleet_cache_for_tests();
    let slug = "phase4-fleet-readiness-no-match:latest";
    // Peer reachable and v2 but announces a different model.
    write_cached_peer(make_peer(
        "peer-wrong-model",
        &["completely-different:latest"],
        chrono::Utc::now(),
        false,
    ));
    let (_dir, conn) = make_it_db();
    seed_walker_provider_fleet(&conn, "c-wpf-no-match", "mid", &[slug]);

    let d = DispatchDecision::build("mid", &conn)
        .expect("non-Fleet providers still build the Decision");
    assert!(
        !d.effective_call_order.contains(&ProviderType::Fleet),
        "Fleet MUST drop when no peer has the requested model; got {:?}",
        d.effective_call_order
    );
    clear_fleet_cache_for_tests();
}

#[test]
fn phase4_decision_chronicle_view_redacts_fleet_local_only_fields() {
    // Sanity: the chronicle redaction catalog already strips
    // fleet_peer_min_staleness_secs + fleet_prefer_cached even when
    // Fleet is in effective_call_order. Belt-and-suspenders with the
    // Phase 0b redaction test so Phase 4 doesn't regress it.
    let _g = fleet_it_lock().lock().unwrap_or_else(|p| p.into_inner());
    let _ng = node_state_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    clear_fleet_cache_for_tests();
    clear_node_state_for_tests();
    let slug = "phase4-fleet-readiness-redact:latest";
    write_cached_peer(make_peer(
        "peer-redact",
        &[slug],
        chrono::Utc::now(),
        false,
    ));
    let (_dir, conn) = make_it_db();
    seed_walker_provider_fleet(&conn, "c-wpf-redact", "mid", &[slug]);

    let d = DispatchDecision::build("mid", &conn).expect("build");
    let view = d.for_chronicle();
    let val = serde_json::to_value(&view).unwrap();
    let s = val.to_string();
    assert!(
        !s.contains("fleet_peer_min_staleness_secs"),
        "fleet_peer_min_staleness_secs must be redacted, got {s}"
    );
    assert!(
        !s.contains("fleet_prefer_cached"),
        "fleet_prefer_cached must be redacted, got {s}"
    );
    clear_fleet_cache_for_tests();
    clear_node_state_for_tests();
}
