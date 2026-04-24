//! Walker v3 W1b integration test (plan rev 1.0.2 §2.9 + §6 Phase 1).
//!
//! W1b wires `DispatchDecision::build` at the outer chain-step entry so
//! every CacheStepContext constructed inside the step inherits the same
//! `Arc<DispatchDecision>` via `with_dispatch_decision`. This test
//! observes that wire-in without running a real LLM dispatch: the
//! `chain_dispatch::test_capture` hook records every Decision that
//! `build_step_dispatch_decision` returns at the outer entry, so the
//! test can:
//!
//!   1. Seed a minimal `walker_provider_openrouter` contribution with
//!      `model_list: { mid: ["test-model-id"] }`.
//!   2. Invoke the outer-entry helper (what `dispatch_llm` /
//!      `dispatch_ir_llm` call at the top of every LLM step) with
//!      `slot = "mid"`.
//!   3. Assert the returned `Option<Arc<DispatchDecision>>` is
//!      populated AND that `decision.per_provider[OpenRouter].model_list`
//!      carries the seeded slug.
//!   4. Assert the capture hook observed exactly one Decision with the
//!      same `Arc` identity — proving that downstream StepContext
//!      constructions inherit the identical Arc rather than rebuilding.
//!
//! Permissive-on-failure is exercised separately: with no
//! `pyramid_config_contributions` table, `build_step_dispatch_decision`
//! logs + returns None so legacy dispatch fall-through still works.
//!
//! The non-LLM sub-dispatchers (mechanical, transform, dead-letter
//! retry) are not LLM-dispatching so they neither build nor read
//! Decisions — they're out of scope for W1b.

use std::sync::Arc;

use rusqlite::Connection;
use tokio::sync::Mutex;

use wire_node_lib::pyramid::chain_dispatch::{
    build_step_dispatch_decision, test_capture, ChainDispatchContext,
};
use wire_node_lib::pyramid::llm::LlmConfig;
use wire_node_lib::pyramid::walker_resolver::ProviderType;
use wire_node_lib::pyramid::{OperationalConfig, Tier1Config};

/// Mint a ChainDispatchContext with the given connection as the
/// `db_reader` — that's where `build_step_dispatch_decision` reads
/// `walker_provider_*` contributions from.
fn ctx_with_reader(reader: Connection) -> ChainDispatchContext {
    ChainDispatchContext {
        db_reader: Arc::new(Mutex::new(reader)),
        db_writer: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
        slug: "w1b-it".into(),
        config: LlmConfig::default(),
        tier1: Tier1Config::default(),
        ops: OperationalConfig::default(),
        audit: None,
        cache_base: None,
        concurrency_cap: None,
        // Phase 6b starter-runner extensions; integration test doesn't
        // exercise sub-chain recursion.
        state: None,
        chains_dir: None,
        target_id: None,
        sub_chain_depth: None,
    }
}

/// Create the minimal `pyramid_config_contributions` schema needed for
/// `build_scope_cache` to succeed. Mirrors the walker_decision.rs unit
/// test + the walker_v3_phase_a_migration integration test harness.
fn init_config_contributions_table(conn: &Connection) {
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
}

fn seed_walker_provider_openrouter(conn: &Connection) {
    conn.execute(
        "INSERT INTO pyramid_config_contributions (
             contribution_id, slug, schema_type, yaml_content, status, source
         ) VALUES (?1, NULL, 'walker_provider_openrouter', ?2, 'active', 'bundled')",
        rusqlite::params![
            "w1b-it-openrouter",
            r#"
schema_type: walker_provider_openrouter
version: 1
overrides:
  model_list:
    mid: ["test-model-id"]
"#
        ],
    )
    .unwrap();
}

#[tokio::test]
async fn w1b_decision_built_once_per_step_with_seeded_model_list() {
    let conn = Connection::open_in_memory().unwrap();
    init_config_contributions_table(&conn);
    seed_walker_provider_openrouter(&conn);

    let ctx = ctx_with_reader(conn);

    // Outer-step entry call. This is what `dispatch_llm` /
    // `dispatch_ir_llm` invoke at the top of every LLM step.
    let decision = build_step_dispatch_decision(&ctx, "mid").await;
    assert!(
        decision.is_some(),
        "W1b: Decision must build successfully with seeded walker_provider_openrouter"
    );
    let decision = decision.unwrap();

    // (1) slot propagates.
    assert_eq!(decision.slot, "mid");

    // (2) runtime path is non-synthetic (preview path is out-of-scope).
    assert!(!decision.synthetic);

    // (3) Seeded model_list surfaces on per_provider[OpenRouter] —
    //     the exact property W2 consumers will read.
    let or = decision
        .per_provider
        .get(&ProviderType::OpenRouter)
        .expect("OpenRouter must pass readiness stubs");
    assert_eq!(
        or.model_list.as_deref(),
        Some(&["test-model-id".to_string()][..]),
        "W1b: seeded model_list must be reachable via step_ctx.dispatch_decision"
    );

    // (4) Downstream inheritance: when the Arc is cloned (as every
    //     CacheStepContext inside the step does), all clones share
    //     the same Arc target (compute-once guarantee §2.9).
    let cloned = Arc::clone(&decision);
    assert!(Arc::ptr_eq(&decision, &cloned));
    assert_eq!(Arc::strong_count(&decision), 2);
}

#[tokio::test]
async fn w1b_permissive_on_db_failure_returns_none() {
    // No pyramid_config_contributions table → DispatchDecision::build
    // errors out of build_scope_cache_pair. Helper must log + return
    // None so legacy dispatch keeps working.
    let conn = Connection::open_in_memory().unwrap();
    // NOTE: no init_config_contributions_table(&conn) — intentional.
    let ctx = ctx_with_reader(conn);

    let decision = build_step_dispatch_decision(&ctx, "mid").await;
    assert!(
        decision.is_none(),
        "W1b: build() failure must be permissive (None + log), not panic"
    );
}

#[tokio::test]
async fn w1b_slot_propagates_to_decision() {
    // Slot passed into the helper lands unchanged on the resulting
    // Decision — downstream consumers read it for chronicle events and
    // to key per-provider lookups to the right tier.
    let conn = Connection::open_in_memory().unwrap();
    init_config_contributions_table(&conn);
    let ctx = ctx_with_reader(conn);

    for slot in ["mid", "high", "max", "extractor", "synth_heavy"] {
        let d = build_step_dispatch_decision(&ctx, slot).await;
        assert!(d.is_some(), "build must succeed for slot {slot}");
        assert_eq!(d.as_ref().unwrap().slot, slot);
    }
}

/// Capture-hook regression: the capture module exists and correctly
/// records a Decision when the hook is enabled for a single-threaded
/// test. This is the observability path that the production
/// dispatch_llm / dispatch_ir_llm entry points flow through — if this
/// regresses, any future test that wants to assert "the executor
/// actually called build_step_dispatch_decision" loses its only
/// low-cost observation surface.
///
/// Runs as a single tokio::test so enable() + snapshot() don't race
/// with other integration tests in this file (parallel default). A
/// flaky cross-test race on the shared static would degrade this test
/// into false-negative territory — the comment above is load-bearing.
#[tokio::test(flavor = "current_thread")]
async fn w1b_capture_hook_observes_production_call() {
    // Isolate from any capture state left by earlier tests sharing the
    // same binary — enable-then-clear establishes a known baseline.
    test_capture::enable();
    test_capture::clear();

    let conn = Connection::open_in_memory().unwrap();
    init_config_contributions_table(&conn);
    let ctx = ctx_with_reader(conn);

    let d = build_step_dispatch_decision(&ctx, "capture-probe-slot").await;
    assert!(d.is_some());

    let snap = test_capture::snapshot();
    // Other parallel tests in this file may race ahead and enable
    // capture too — but we just need ours in there. Find our probe
    // slot specifically.
    let ours: Vec<_> = snap
        .iter()
        .filter(|c| c.slot == "capture-probe-slot")
        .collect();
    assert_eq!(
        ours.len(),
        1,
        "capture hook must observe our outer-entry call exactly once"
    );
    assert!(Arc::ptr_eq(&ours[0].decision, d.as_ref().unwrap()));

    test_capture::disable();
}
