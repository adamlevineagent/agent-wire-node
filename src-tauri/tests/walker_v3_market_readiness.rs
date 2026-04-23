//! Walker v3 Phase 3 integration test — MarketReadiness + market-probe
//! cache end-to-end against `DispatchDecision::build`.
//!
//! Plan rev 1.0.2 §2.6 + §3 Phase 3. Phase 0a-1 landed a trivial
//! `MarketReadinessStub` that returned Ready unconditionally. Phase 3
//! replaces it with a real impl reading the `walker_market_probe`
//! cache + node-state snapshot (credit balance, network reachability,
//! self-handles). This test exercises the five scenarios named in the
//! Phase 3 scope:
//!
//!   * happy path — offers with headroom → Market in effective_call_order
//!   * all offers saturated → AllOffersSaturatedForModel, dropped
//!   * no offers for model (catalog reports active_offers == 0) →
//!     NoMarketOffersForSlot, dropped
//!   * self-dealing (all cached offers belong to this node) → SelfDealing,
//!     dropped
//!   * insufficient credit (balance < max_budget_credits) →
//!     InsufficientCredit, dropped
//!
//! The integration test bypasses the background market-probe task
//! (which requires a live tokio runtime + a populated MarketSurfaceCache).
//! It writes into the shared probe cache directly via
//! `walker_market_probe::write_cached_model` + `set_credit_balance` /
//! `set_self_handles`, mirroring what the background task would do in
//! production.

use rusqlite::Connection;
use tempfile::TempDir;

use wire_node_lib::pyramid::walker_decision::DispatchDecision;
use wire_node_lib::pyramid::walker_market_probe::{
    clear_model_cache_for_tests, clear_node_state_for_tests, invalidate_cached_model,
    node_state_test_lock, set_credit_balance, set_self_handles, write_cached_model,
    CachedMarketModel, CachedOffer,
};
use wire_node_lib::pyramid::walker_resolver::ProviderType;

/// Serialize tests that mutate the global walker_market_probe
/// node-state cache (credit balance, self-handles). Routed through
/// the module-level `node_state_test_lock` so sibling unit tests in
/// walker_readiness + walker_decision + walker_market_probe see the
/// same ordering.
fn market_it_lock() -> &'static std::sync::Mutex<()> {
    node_state_test_lock()
}

/// Create the minimal `pyramid_config_contributions` schema used by
/// `build_scope_cache` (and therefore by `DispatchDecision::build`).
fn make_it_db() -> (TempDir, Connection) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("walker_v3_market_readiness_it.db");
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

/// Seed a `walker_provider_market` contribution declaring `active=true`
/// + `model_list[slot] = [slug]`, optionally with a max_budget_credits
/// cap. Slot-scoped so the test scoping-chain resolution picks it up.
fn seed_walker_provider_market(
    conn: &Connection,
    contribution_id: &str,
    slot: &str,
    slugs: &[&str],
    max_budget_credits: Option<i64>,
) {
    let slugs_yaml = slugs
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let budget_line = match max_budget_credits {
        Some(n) => format!("  max_budget_credits: {}\n", n),
        None => String::new(),
    };
    let yaml = format!(
        r#"
schema_type: walker_provider_market
version: 1
overrides:
  active: true
{budget_line}  model_list:
    {slot}: [{slugs_yaml}]
"#
    );
    conn.execute(
        "INSERT INTO pyramid_config_contributions (
             contribution_id, slug, schema_type, yaml_content, status, source
         ) VALUES (?1, NULL, 'walker_provider_market', ?2, 'active', 'bundled')",
        rusqlite::params![contribution_id, yaml],
    )
    .unwrap();
}

/// Seed a walker_provider_local entry + Ollama probe cache so Local
/// passes readiness without interfering with the Market-focused
/// assertions. Avoids the test-scaffolding flake where Local being
/// OllamaOffline + Market being inspected simultaneously leaves the
/// effective_call_order unexpectedly narrower than the markers the
/// assertion expects.
fn seed_local_ready(conn: &Connection, slug_suffix: &str) -> String {
    use wire_node_lib::pyramid::walker_ollama_probe::{write_cached_probe, CachedProbe};
    let base_url = format!("http://test-walker-v3-market-{slug_suffix}.invalid:11434/v1");
    write_cached_probe(
        &base_url,
        CachedProbe {
            reachable: true,
            models: vec!["gemma3:27b".into()],
            at: std::time::Instant::now(),
        },
    );
    let yaml = format!(
        r#"
schema_type: walker_provider_local
version: 1
overrides:
  active: true
  ollama_base_url: {base_url}
  model_list:
    mid: [gemma3:27b]
"#
    );
    conn.execute(
        "INSERT INTO pyramid_config_contributions (
             contribution_id, slug, schema_type, yaml_content, status, source
         ) VALUES (?1, NULL, 'walker_provider_local', ?2, 'active', 'bundled')",
        rusqlite::params![format!("c-wpl-{slug_suffix}"), yaml],
    )
    .unwrap();
    base_url
}

fn cached_model(active_offers: i64, all_saturated: bool, only_self: bool) -> CachedMarketModel {
    let offers = if active_offers > 0 {
        vec![CachedOffer {
            offer_id: "offer-1".into(),
            node_handle: if only_self {
                "me-node-id".into()
            } else {
                "other-node".into()
            },
            operator_handle: if only_self {
                "me-op".into()
            } else {
                "other-op".into()
            },
            typical_serve_ms_p50_7d: Some(1000.0),
            execution_concurrency: 1,
            current_queue_depth: if all_saturated { 5 } else { 0 },
            max_queue_depth: 5,
        }]
    } else {
        vec![]
    };
    CachedMarketModel {
        active_offers,
        all_offers_saturated: all_saturated,
        only_self_offers: only_self,
        model_typical_serve_ms_p50_7d: Some(1000.0),
        offers_detail: offers,
        at: std::time::Instant::now(),
    }
}

#[test]
fn phase3_decision_includes_market_when_offers_have_headroom() {
    let _g = market_it_lock().lock().unwrap_or_else(|p| p.into_inner());
    clear_model_cache_for_tests();
    clear_node_state_for_tests();
    let (_dir, conn) = make_it_db();
    let _ = seed_local_ready(&conn, "happy-path");
    let slug = "phase3-market-readiness/happy-path";
    write_cached_model(slug, cached_model(1, false, false));
    seed_walker_provider_market(&conn, "c-wpm-happy", "mid", &[slug], None);

    let d = DispatchDecision::build("mid", &conn).expect("Decision must build with market ready");
    assert!(
        d.effective_call_order.contains(&ProviderType::Market),
        "Market MUST be in effective_call_order; got {:?}",
        d.effective_call_order
    );
    let m = d
        .per_provider
        .get(&ProviderType::Market)
        .expect("Market params must be present");
    assert_eq!(m.model_list.as_deref(), Some(&[slug.to_string()][..]));
    invalidate_cached_model(slug);
}

#[test]
fn phase3_decision_drops_market_when_all_offers_saturated() {
    let _g = market_it_lock().lock().unwrap_or_else(|p| p.into_inner());
    clear_model_cache_for_tests();
    clear_node_state_for_tests();
    let (_dir, conn) = make_it_db();
    let _ = seed_local_ready(&conn, "saturated");
    let slug = "phase3-market-readiness/saturated";
    write_cached_model(slug, cached_model(1, true, false));
    seed_walker_provider_market(&conn, "c-wpm-saturated", "mid", &[slug], None);

    let d = DispatchDecision::build("mid", &conn)
        .expect("non-Market providers still build the Decision");
    assert!(
        !d.effective_call_order.contains(&ProviderType::Market),
        "Market MUST drop when all offers saturated; got {:?}",
        d.effective_call_order
    );
    invalidate_cached_model(slug);
}

#[test]
fn phase3_decision_drops_market_when_no_offers_for_model() {
    let _g = market_it_lock().lock().unwrap_or_else(|p| p.into_inner());
    clear_model_cache_for_tests();
    clear_node_state_for_tests();
    let (_dir, conn) = make_it_db();
    let _ = seed_local_ready(&conn, "no-offers");
    let slug = "phase3-market-readiness/no-offers";
    write_cached_model(slug, cached_model(0, false, false));
    seed_walker_provider_market(&conn, "c-wpm-no-offers", "mid", &[slug], None);

    let d = DispatchDecision::build("mid", &conn)
        .expect("non-Market providers still build the Decision");
    assert!(
        !d.effective_call_order.contains(&ProviderType::Market),
        "Market MUST drop when no offers exist; got {:?}",
        d.effective_call_order
    );
    invalidate_cached_model(slug);
}

#[test]
fn phase3_decision_drops_market_when_offers_are_self_dealing() {
    let _g = market_it_lock().lock().unwrap_or_else(|p| p.into_inner());
    clear_model_cache_for_tests();
    clear_node_state_for_tests();
    set_self_handles("me-node-id", "me-op");
    let (_dir, conn) = make_it_db();
    let _ = seed_local_ready(&conn, "self-dealing");
    let slug = "phase3-market-readiness/self-dealing";
    write_cached_model(slug, cached_model(1, false, true));
    seed_walker_provider_market(&conn, "c-wpm-self", "mid", &[slug], None);

    let d = DispatchDecision::build("mid", &conn)
        .expect("non-Market providers still build the Decision");
    assert!(
        !d.effective_call_order.contains(&ProviderType::Market),
        "Market MUST drop when offers are all self-dealing; got {:?}",
        d.effective_call_order
    );
    clear_node_state_for_tests();
    invalidate_cached_model(slug);
}

#[test]
fn phase3_decision_drops_market_when_credit_balance_insufficient() {
    let _g = market_it_lock().lock().unwrap_or_else(|p| p.into_inner());
    clear_model_cache_for_tests();
    clear_node_state_for_tests();
    // Cache says offers have headroom, but credit balance is below
    // the Decision's max_budget_credits cap.
    set_credit_balance(Some(100));
    let (_dir, conn) = make_it_db();
    let _ = seed_local_ready(&conn, "insufficient");
    let slug = "phase3-market-readiness/insufficient-credit";
    write_cached_model(slug, cached_model(1, false, false));
    seed_walker_provider_market(&conn, "c-wpm-insufficient", "mid", &[slug], Some(1_000));

    let d = DispatchDecision::build("mid", &conn)
        .expect("non-Market providers still build the Decision");
    assert!(
        !d.effective_call_order.contains(&ProviderType::Market),
        "Market MUST drop when balance < max_budget_credits; got {:?}",
        d.effective_call_order
    );
    clear_node_state_for_tests();
    invalidate_cached_model(slug);
}

#[test]
fn phase3_decision_market_ready_when_budget_and_balance_both_high() {
    // Sanity: same setup as the insufficient-credit scenario but with
    // a balance comfortably above the cap — readiness must be Ready.
    let _g = market_it_lock().lock().unwrap_or_else(|p| p.into_inner());
    clear_model_cache_for_tests();
    clear_node_state_for_tests();
    set_credit_balance(Some(1_000_000));
    let (_dir, conn) = make_it_db();
    let _ = seed_local_ready(&conn, "sufficient");
    let slug = "phase3-market-readiness/sufficient-credit";
    write_cached_model(slug, cached_model(1, false, false));
    seed_walker_provider_market(&conn, "c-wpm-sufficient", "mid", &[slug], Some(1_000));

    let d = DispatchDecision::build("mid", &conn).expect("Decision must build");
    assert!(
        d.effective_call_order.contains(&ProviderType::Market),
        "Market MUST be ready with sufficient balance; got {:?}",
        d.effective_call_order
    );
    // max_budget_credits is a local_only / sensitive field so it
    // rides on ResolvedProviderParams but is redacted from the
    // chronicle view. Verify the cap made it into per_provider:
    let mkt = d.per_provider.get(&ProviderType::Market).unwrap();
    assert_eq!(mkt.max_budget_credits, Some(1_000));
    clear_node_state_for_tests();
    invalidate_cached_model(slug);
}
