//! Walker v3 Phase 2 integration test — LocalReadiness + Ollama probe
//! cache end-to-end against `DispatchDecision::build`.
//!
//! Plan rev 1.0.2 §2.6 + §3 Phase 2. Phase 0a-1 landed a trivial
//! `LocalReadinessStub` that returned Ready unconditionally. Phase 2
//! replaces it with a real impl reading the
//! `walker_ollama_probe` cache. This test exercises both directions of
//! the gate:
//!
//!   * Given a walker_provider_local contribution declaring
//!     `model_list: { mid: [gemma3:27b] }` AND a probe cache entry
//!     reporting `reachable=true` + `models=[gemma3:27b]`, the Decision
//!     built for slot "mid" MUST include Local in `effective_call_order`
//!     AND populate `per_provider[Local]` with the declared model_list.
//!
//!   * With the same contribution but NO probe cache entry (offline /
//!     pre-probe), the Decision MUST drop Local from the order while
//!     still building (OpenRouter/Fleet/Market stubs stay Ready).
//!
//!   * With a probe cache entry reporting `reachable=true` but NO
//!     overlap with the declared model_list (operator declared
//!     `gemma3:27b` but Ollama has only `llama3.2`), Local MUST drop
//!     with OllamaOffline.
//!
//! The integration test bypasses the background probe task (which
//! requires a live tokio runtime + working Ollama). It writes into the
//! shared probe cache directly via `walker_ollama_probe::write_cached_probe`,
//! mirroring what the background task would do in production. This is
//! the same pattern used by the lib tests in `walker_readiness.rs`.

use rusqlite::Connection;
use tempfile::TempDir;

use wire_node_lib::pyramid::walker_decision::DispatchDecision;
use wire_node_lib::pyramid::walker_ollama_probe::{
    invalidate_cached_probe, write_cached_probe, CachedProbe,
};
use wire_node_lib::pyramid::walker_resolver::ProviderType;

/// Create the minimal `pyramid_config_contributions` schema used by
/// `build_scope_cache` (and therefore by `DispatchDecision::build`).
fn make_it_db() -> (TempDir, Connection) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("walker_v3_local_readiness_it.db");
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

/// Insert an active `walker_provider_local` contribution declaring
/// both `ollama_base_url` (so tests don't collide on the SYSTEM_DEFAULT
/// URL) and a `model_list[slot]` at the requested slot.
fn seed_walker_provider_local(
    conn: &Connection,
    contribution_id: &str,
    base_url: &str,
    slot: &str,
    models: &[&str],
) {
    let models_yaml = models
        .iter()
        .map(|m| format!("\"{m}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let yaml = format!(
        r#"
schema_type: walker_provider_local
version: 1
overrides:
  active: true
  ollama_base_url: {base_url}
  model_list:
    {slot}: [{models_yaml}]
"#
    );
    conn.execute(
        "INSERT INTO pyramid_config_contributions (
             contribution_id, slug, schema_type, yaml_content, status, source
         ) VALUES (?1, NULL, 'walker_provider_local', ?2, 'active', 'bundled')",
        rusqlite::params![contribution_id, yaml],
    )
    .unwrap();
}

#[test]
fn phase2_decision_includes_local_when_ollama_up_and_model_installed() {
    let (_dir, conn) = make_it_db();
    let base_url = "http://test-phase2-it-ready.invalid:11434/v1";
    seed_walker_provider_local(&conn, "c-phase2-ready", base_url, "mid", &["gemma3:27b"]);

    // Simulate the background probe task having observed a healthy
    // Ollama with the declared model installed.
    write_cached_probe(
        base_url,
        CachedProbe {
            reachable: true,
            models: vec!["gemma3:27b".into(), "llama3.2:latest".into()],
            at: std::time::Instant::now(),
        },
    );

    let d = DispatchDecision::build("mid", &conn)
        .expect("Decision must build with all providers ready");
    assert!(
        d.effective_call_order.contains(&ProviderType::Local),
        "Local MUST be in effective_call_order; got {:?}",
        d.effective_call_order
    );
    let local = d
        .per_provider
        .get(&ProviderType::Local)
        .expect("Local params must be present");
    assert_eq!(
        local.model_list,
        Some(vec!["gemma3:27b".to_string()]),
        "Local.model_list must carry the seeded slug"
    );

    invalidate_cached_probe(base_url);
}

#[test]
fn phase2_decision_drops_local_when_ollama_probe_missing() {
    let (_dir, conn) = make_it_db();
    let base_url = "http://test-phase2-it-offline.invalid:11434/v1";
    // No cache entry for this URL — simulates "background probe task
    // hasn't yet observed this base_url" or "Ollama is down".
    invalidate_cached_probe(base_url);
    seed_walker_provider_local(&conn, "c-phase2-offline", base_url, "mid", &["gemma3:27b"]);

    let d = DispatchDecision::build("mid", &conn)
        .expect("non-Local providers (Ready stubs) still build the Decision");
    assert!(
        !d.effective_call_order.contains(&ProviderType::Local),
        "Local MUST drop when probe cache is unseeded; got {:?}",
        d.effective_call_order
    );
    assert!(
        !d.per_provider.contains_key(&ProviderType::Local),
        "Local params must NOT be present when Local was dropped"
    );
}

#[test]
fn phase2_decision_drops_local_when_declared_model_not_installed() {
    let (_dir, conn) = make_it_db();
    let base_url = "http://test-phase2-it-nomatch.invalid:11434/v1";
    seed_walker_provider_local(&conn, "c-phase2-nomatch", base_url, "mid", &["gemma3:27b"]);

    // Ollama is up but has a different model installed.
    write_cached_probe(
        base_url,
        CachedProbe {
            reachable: true,
            models: vec!["llama3.2:latest".into()],
            at: std::time::Instant::now(),
        },
    );

    let d = DispatchDecision::build("mid", &conn)
        .expect("non-Local providers still build the Decision");
    assert!(
        !d.effective_call_order.contains(&ProviderType::Local),
        "Local MUST drop when declared model is not installed; got {:?}",
        d.effective_call_order
    );

    invalidate_cached_probe(base_url);
}

#[test]
fn phase2_decision_drops_local_when_reachable_false() {
    let (_dir, conn) = make_it_db();
    let base_url = "http://test-phase2-it-unreachable.invalid:11434/v1";
    seed_walker_provider_local(
        &conn,
        "c-phase2-unreachable",
        base_url,
        "mid",
        &["gemma3:27b"],
    );

    // Cache entry reports "probe ran but failed" — readiness treats
    // this as OllamaOffline just like a missing entry.
    write_cached_probe(
        base_url,
        CachedProbe {
            reachable: false,
            models: vec![],
            at: std::time::Instant::now(),
        },
    );

    let d = DispatchDecision::build("mid", &conn)
        .expect("non-Local providers still build the Decision");
    assert!(
        !d.effective_call_order.contains(&ProviderType::Local),
        "Local MUST drop when probe reports reachable=false; got {:?}",
        d.effective_call_order
    );

    invalidate_cached_probe(base_url);
}
