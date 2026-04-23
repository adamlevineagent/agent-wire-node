// pyramid/demand_signal.rs — Phase 12 demand signal tracking + propagation
//
// Demand signals are fire-and-forget records of agent queries, user
// drills, and search hits that resolve to pyramid nodes. They feed the
// triage DSL's `has_demand_signals` condition and drive on-demand
// reactivation of deferred evidence questions.
//
// This module owns the propagation BFS (with attenuation, floor, and
// max_depth guards) and the reactivation hook for deferred questions
// whose `check_interval` is "never" or "on_demand".
//
// See `docs/specs/evidence-triage-and-dadbear.md` Part 2 §Propagation.

use std::collections::{HashSet, VecDeque};

use anyhow::Result;
use rusqlite::Connection;

use super::db::{self, DemandSignalAttenuationYaml, EvidencePolicy};

/// Record a demand signal at `node_id` and propagate it upward
/// through `pyramid_evidence` KEEP links with attenuation.
///
/// Each propagated row stores the attenuated weight AND
/// `source_node_id = original leaf` so we can trace parent demand
/// back to the leaf event that produced it.
///
/// This function is synchronous (DB-bound). Callers that want
/// fire-and-forget behavior should wrap it in `tokio::task::spawn_blocking`.
///
/// After propagation, Phase 12 reactivates any `"never"`/`"on_demand"`
/// deferred questions on `node_id` whose triage now returns `Answer`
/// against the current policy — this is the demand-driven reactivation
/// path from the spec's §7.
pub fn record_demand_signal(
    conn: &Connection,
    slug: &str,
    node_id: &str,
    signal_type: &str,
    source: Option<&str>,
    policy: &EvidencePolicy,
) -> Result<()> {
    let attenuation = &policy.demand_signal_attenuation;

    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, f64, u32)> = VecDeque::new();
    queue.push_back((node_id.to_string(), 1.0, 0));

    let source_leaf = node_id.to_string();

    while let Some((current_node, current_weight, depth)) = queue.pop_front() {
        if !visited.insert(current_node.clone()) {
            continue;
        }

        if depth > attenuation.max_depth {
            continue;
        }

        if current_weight < attenuation.floor {
            continue;
        }

        // Record the (possibly attenuated) signal at this node.
        db::insert_demand_signal(
            conn,
            slug,
            &current_node,
            signal_type,
            source,
            current_weight,
            Some(&source_leaf),
        )?;

        // Walk up via evidence graph — parents are nodes that cite
        // `current_node` as a KEEP source.
        let parents = db::load_parents_via_evidence(conn, slug, &current_node)?;

        // Attenuation with short-circuit on factor == 0.0 (disabled).
        if attenuation.factor <= 0.0 {
            continue;
        }

        let next_weight = current_weight * attenuation.factor;
        let next_depth = depth + 1;
        for parent_id in parents {
            if !visited.contains(&parent_id) {
                queue.push_back((parent_id, next_weight, next_depth));
            }
        }
    }

    // Phase 12 wanderer fix: on-demand reactivation of deferred
    // questions, slug-scoped.
    //
    // The original spec text said "query `pyramid_deferred_questions`
    // for `(slug, node_id)` rows where `check_interval IN ('never',
    // 'on_demand')`". That mapping is impossible with the current
    // schema: a deferred question's `question_id` column is a
    // `q-{sha256}` hash (`make_question_id` in
    // question_decomposition.rs) while the drill signal's `node_id`
    // is the answered pyramid node's `L{layer}-{seq}` id. The two
    // ID spaces never overlap and the prior `list_deferred_by_question_target`
    // join returned zero rows for every real drill event — the
    // reactivation hook was dead code.
    //
    // Correct semantics inside the schema we have: "ANY demand
    // signal arriving on the slug is 'demand arriving' for every
    // slug-scoped `on_demand`/`never` deferred question". The
    // triage DSL then decides which ones to reactivate. This
    // matches the spec's intent ("demand drives re-check") while
    // staying sound in the only ID space we have at both sides of
    // the join. When the pyramid grows a persistent q-hash → L-id
    // map (Phase 13+), the tighter per-node reactivation can return.
    let pending = db::list_on_demand_deferred_for_slug(conn, slug).unwrap_or_default();
    if !pending.is_empty() {
        use super::triage::{resolve_decision, TriageDecision, TriageFacts};
        use super::types::LayerQuestion;
        for row in pending {
            let question: LayerQuestion = match serde_json::from_str(&row.question_json) {
                Ok(q) => q,
                Err(_) => continue,
            };
            // has_demand_signals is true by construction — we just
            // recorded one within the triage window on this slug.
            let facts = TriageFacts {
                question: &question,
                target_node_distilled: None,
                target_node_depth: Some(question.layer),
                is_first_build: false,
                is_stale_check: false,
                has_demand_signals: true,
                evidence_question_trivial: None,
                evidence_question_high_value: None,
            };
            match resolve_decision(policy, &facts) {
                Ok(TriageDecision::Answer { .. }) => {
                    let _ = db::remove_deferred(conn, slug, &row.question_id);
                }
                _ => {
                    // Still deferred or skipped — leave as-is.
                }
            }
        }
    }

    Ok(())
}

/// Look up every deferred question that targets `node_id` and has
/// `check_interval IN ('never', 'on_demand')`. These are the
/// on-demand reactivation candidates. Caller re-runs triage against
/// each and decides whether to activate, re-defer, or skip.
///
/// Returns the list of (question_id, question_json) pairs so the
/// caller can fetch the full question payloads without exposing the
/// DeferredQuestion row shape.
///
/// Phase 12 wanderer note: retained as a stable helper for any
/// future caller that has a real q-hash to match against. The
/// `record_demand_signal` reactivation hook no longer uses this
/// helper — it switched to `db::list_on_demand_deferred_for_slug`
/// (slug-scoped) because the drill event's `node_id` can never be
/// a `q-{sha256}` hash.
pub fn list_on_demand_deferred_for_node(
    conn: &Connection,
    slug: &str,
    node_id: &str,
) -> Result<Vec<(String, String)>> {
    let rows = db::list_deferred_by_question_target(conn, slug, node_id)?;
    Ok(rows
        .into_iter()
        .filter(|row| {
            let interval = row.check_interval.to_lowercase();
            interval == "never" || interval == "on_demand"
        })
        .map(|row| (row.question_id, row.question_json))
        .collect())
}

/// Return reasonable defaults for an attenuation config. Used by
/// tests and fallback-loader paths.
pub fn default_attenuation() -> DemandSignalAttenuationYaml {
    DemandSignalAttenuationYaml::default()
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::db::init_pyramid_db;
    use rusqlite::Connection;

    fn make_policy(factor: f64, floor: f64, max_depth: u32) -> EvidencePolicy {
        EvidencePolicy {
            slug: Some("test-slug".to_string()),
            contribution_id: None,
            triage_rules: Vec::new(),
            demand_signals: Vec::new(),
            budget: Default::default(),
            demand_signal_attenuation: DemandSignalAttenuationYaml {
                factor,
                floor,
                max_depth,
            },
            policy_yaml_hash: String::new(),
        }
    }

    /// Insert a KEEP evidence edge (source → target) with build_id = ''.
    /// Pass bypassing the normal build pipeline — we want direct control
    /// over the graph shape for propagation tests.
    fn insert_keep_edge(conn: &Connection, slug: &str, source: &str, target: &str) {
        conn.execute(
            "INSERT OR REPLACE INTO pyramid_evidence
                (slug, build_id, source_node_id, target_node_id, verdict, weight, reason)
             VALUES (?1, '', ?2, ?3, 'KEEP', 1.0, 'test')",
            rusqlite::params![slug, source, target],
        )
        .unwrap();
    }

    fn sum(conn: &Connection, slug: &str, node: &str) -> f64 {
        db::sum_demand_weight(conn, slug, node, "agent_query", "-1 day").unwrap()
    }

    fn count_rows(conn: &Connection, slug: &str, node: &str) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM pyramid_demand_signals
             WHERE slug = ?1 AND node_id = ?2",
            rusqlite::params![slug, node],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn test_propagate_respects_floor() {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        // Chain: leaf → p1 → p2 → p3 → p4 → p5 (five layers).
        insert_keep_edge(&conn, "s", "leaf", "p1");
        insert_keep_edge(&conn, "s", "p1", "p2");
        insert_keep_edge(&conn, "s", "p2", "p3");
        insert_keep_edge(&conn, "s", "p3", "p4");
        insert_keep_edge(&conn, "s", "p4", "p5");

        // factor 0.5, floor 0.2 → leaf=1, p1=0.5, p2=0.25 (still above),
        // p3=0.125 (below 0.2) → stop before p3 gets recorded.
        let policy = make_policy(0.5, 0.2, 100);
        record_demand_signal(&conn, "s", "leaf", "agent_query", Some("user"), &policy).unwrap();

        assert!(sum(&conn, "s", "leaf") > 0.99, "leaf has weight 1.0");
        assert!(sum(&conn, "s", "p1") > 0.49, "p1 has weight 0.5");
        assert!(sum(&conn, "s", "p2") > 0.24, "p2 has weight 0.25");
        assert_eq!(
            count_rows(&conn, "s", "p3"),
            0,
            "p3 below floor, should not be recorded"
        );
        assert_eq!(count_rows(&conn, "s", "p4"), 0);
        assert_eq!(count_rows(&conn, "s", "p5"), 0);
    }

    #[test]
    fn test_propagate_respects_max_depth() {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        // Chain: leaf → p1 → p2 → p3.
        insert_keep_edge(&conn, "s", "leaf", "p1");
        insert_keep_edge(&conn, "s", "p1", "p2");
        insert_keep_edge(&conn, "s", "p2", "p3");

        // max_depth 1 → only leaf + p1 get recorded (depth 0 + depth 1).
        let policy = make_policy(0.9, 0.001, 1);
        record_demand_signal(&conn, "s", "leaf", "agent_query", None, &policy).unwrap();

        assert_eq!(count_rows(&conn, "s", "leaf"), 1);
        assert_eq!(count_rows(&conn, "s", "p1"), 1);
        assert_eq!(
            count_rows(&conn, "s", "p2"),
            0,
            "p2 is at depth 2, exceeds max_depth=1"
        );
        assert_eq!(count_rows(&conn, "s", "p3"), 0);
    }

    #[test]
    fn test_propagate_cycle_guard() {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        // Cycle: A ↔ B.
        insert_keep_edge(&conn, "s", "A", "B");
        insert_keep_edge(&conn, "s", "B", "A");

        let policy = make_policy(0.9, 0.001, 100);
        record_demand_signal(&conn, "s", "A", "agent_query", None, &policy).unwrap();

        // Both nodes recorded exactly once — the visited set breaks
        // the cycle.
        assert_eq!(count_rows(&conn, "s", "A"), 1);
        assert_eq!(count_rows(&conn, "s", "B"), 1);
    }

    #[test]
    fn test_propagate_records_source_node_id() {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        insert_keep_edge(&conn, "s", "leaf", "p1");
        insert_keep_edge(&conn, "s", "p1", "p2");

        let policy = make_policy(0.5, 0.001, 100);
        record_demand_signal(&conn, "s", "leaf", "agent_query", None, &policy).unwrap();

        let rows: Vec<(String, Option<String>)> = conn
            .prepare("SELECT node_id, source_node_id FROM pyramid_demand_signals WHERE slug='s' ORDER BY node_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(!rows.is_empty());
        for (_, source_node) in rows {
            assert_eq!(source_node.as_deref(), Some("leaf"));
        }
    }

    #[test]
    fn test_propagate_disabled_when_attenuation_factor_zero() {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        insert_keep_edge(&conn, "s", "leaf", "p1");
        insert_keep_edge(&conn, "s", "p1", "p2");

        // factor 0.0 → propagation stops after the leaf itself.
        let policy = make_policy(0.0, 0.0, 100);
        record_demand_signal(&conn, "s", "leaf", "agent_query", None, &policy).unwrap();

        assert_eq!(count_rows(&conn, "s", "leaf"), 1);
        assert_eq!(count_rows(&conn, "s", "p1"), 0);
        assert_eq!(count_rows(&conn, "s", "p2"), 0);
    }

    #[test]
    fn test_sum_demand_weight_aggregates_multiple_signals() {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        insert_keep_edge(&conn, "s", "leaf", "parent");

        // Three separate user drill events on the leaf.
        let policy = make_policy(0.5, 0.0, 100);
        for _ in 0..3 {
            record_demand_signal(&conn, "s", "leaf", "user_drill", Some("user"), &policy).unwrap();
        }

        let leaf_total = db::sum_demand_weight(&conn, "s", "leaf", "user_drill", "-1 day").unwrap();
        assert!(
            (leaf_total - 3.0).abs() < 1e-9,
            "three 1.0 signals sum to 3.0"
        );

        let parent_total =
            db::sum_demand_weight(&conn, "s", "parent", "user_drill", "-1 day").unwrap();
        assert!(
            (parent_total - 1.5).abs() < 1e-9,
            "each leaf signal propagates to parent with 0.5 weight → 3×0.5 = 1.5"
        );
    }

    /// Phase 12 wanderer fix: `sum_slug_demand_weight` aggregates
    /// across the entire slug regardless of which node the signal
    /// landed on. This is the helper the triage DSL's
    /// `has_demand_signals` condition now uses, because the
    /// previous per-node path couldn't join a q-hash question_id
    /// to an L{}-{} drill node_id.
    #[test]
    fn test_sum_slug_demand_weight_aggregates_across_nodes() {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();

        // Drop signals on three distinct nodes on the same slug.
        // factor 0.0 → no propagation, each node gets exactly one row.
        let policy = make_policy(0.0, 0.0, 100);
        record_demand_signal(&conn, "s", "L1-001", "agent_query", Some("user"), &policy).unwrap();
        record_demand_signal(&conn, "s", "L1-002", "agent_query", Some("user"), &policy).unwrap();
        record_demand_signal(&conn, "s", "L2-003", "agent_query", Some("user"), &policy).unwrap();

        // Per-node lookup against a q-hash question id returns
        // zero (the old broken path).
        let per_node_miss =
            db::sum_demand_weight(&conn, "s", "q-abc123456789", "agent_query", "-1 day").unwrap();
        assert!(
            per_node_miss < 1e-9,
            "per-node lookup on q-hash can never match"
        );

        // Slug-level aggregation picks up all three signals.
        let slug_total = db::sum_slug_demand_weight(&conn, "s", "agent_query", "-1 day").unwrap();
        assert!(
            (slug_total - 3.0).abs() < 1e-9,
            "slug aggregate counts all three drill events"
        );

        // Different slug gets zero.
        let other_slug =
            db::sum_slug_demand_weight(&conn, "other", "agent_query", "-1 day").unwrap();
        assert!(other_slug < 1e-9, "slug filter is respected");

        // Different signal type gets zero.
        let wrong_type = db::sum_slug_demand_weight(&conn, "s", "user_drill", "-1 day").unwrap();
        assert!(wrong_type < 1e-9, "signal_type filter is respected");
    }

    /// Phase 12 wanderer fix: `list_on_demand_deferred_for_slug`
    /// returns every deferred row for a slug whose check_interval
    /// is `never` or `on_demand`, regardless of question_id. This
    /// replaces the broken `list_deferred_by_question_target` join
    /// used by the original reactivation hook.
    #[test]
    fn test_list_on_demand_deferred_for_slug() {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();

        // Seed a contribution row so FK-like wiring stays happy.
        conn.execute(
            "INSERT INTO pyramid_config_contributions
                (contribution_id, slug, schema_type, yaml_content, status)
             VALUES ('c-pol', 's', 'evidence_policy', '', 'active')",
            [],
        )
        .unwrap();

        // Three deferred rows: two on_demand/never, one 7d.
        db::defer_question(
            &conn,
            "s",
            "q-h1",
            r#"{"question_id":"q-h1","question_text":"?","layer":1,"about":"","creates":""}"#,
            "on_demand",
            Some("waiting for drill"),
            Some("c-pol"),
        )
        .unwrap();
        db::defer_question(
            &conn,
            "s",
            "q-h2",
            r#"{"question_id":"q-h2","question_text":"?","layer":1,"about":"","creates":""}"#,
            "never",
            None,
            None,
        )
        .unwrap();
        db::defer_question(
            &conn,
            "s",
            "q-h3",
            r#"{"question_id":"q-h3","question_text":"?","layer":1,"about":"","creates":""}"#,
            "7d",
            None,
            None,
        )
        .unwrap();

        let rows = db::list_on_demand_deferred_for_slug(&conn, "s").unwrap();
        let ids: Vec<String> = rows.into_iter().map(|r| r.question_id).collect();
        assert_eq!(ids.len(), 2, "only on_demand + never rows are returned");
        assert!(ids.contains(&"q-h1".to_string()));
        assert!(ids.contains(&"q-h2".to_string()));
        assert!(!ids.contains(&"q-h3".to_string()));
    }

    // ── Phase 18b L7: search_hit signal recording + propagation ─────────

    /// Helper that mirrors the production wiring used by
    /// `routes::handle_drill` and the `pyramid_drill` IPC: when a drill
    /// is launched from a search result, BOTH `user_drill` and
    /// `search_hit` are recorded for the same node. This test confirms
    /// the two-row outcome and that propagation works for `search_hit`
    /// the same way it does for the other signal types.
    #[test]
    fn test_phase18b_l7_search_hit_records_two_rows_per_drill() {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        // Mirror what handle_drill does on the from=search path:
        // emit `user_drill` then `search_hit` for the same node.
        let policy = make_policy(0.0, 0.0, 100); // factor=0 → no propagation
        record_demand_signal(&conn, "p18b", "L1-001", "user_drill", Some("user"), &policy).unwrap();
        record_demand_signal(&conn, "p18b", "L1-001", "search_hit", Some("user"), &policy).unwrap();

        // Both rows landed at the leaf node, with distinct signal types.
        let user_drill_total =
            db::sum_demand_weight(&conn, "p18b", "L1-001", "user_drill", "-1 day").unwrap();
        let search_hit_total =
            db::sum_demand_weight(&conn, "p18b", "L1-001", "search_hit", "-1 day").unwrap();
        assert!(
            (user_drill_total - 1.0).abs() < 1e-9,
            "exactly one user_drill row at weight 1.0"
        );
        assert!(
            (search_hit_total - 1.0).abs() < 1e-9,
            "exactly one search_hit row at weight 1.0 (the L7 fix)"
        );
    }

    /// Phase 18b L7: spot-check that `search_hit` propagation through
    /// the evidence graph behaves identically to the other signal
    /// types. The propagation engine is type-agnostic — the leaf
    /// signal type rides along through the BFS — so this is mostly
    /// regression coverage that the new signal_type doesn't get
    /// special-cased anywhere.
    #[test]
    fn test_phase18b_l7_search_hit_propagates_like_other_signals() {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        // Chain: leaf → p1 → p2.
        insert_keep_edge(&conn, "p18b", "leaf", "p1");
        insert_keep_edge(&conn, "p18b", "p1", "p2");

        // factor 0.5, floor 0.0 → leaf=1.0, p1=0.5, p2=0.25.
        let policy = make_policy(0.5, 0.0, 100);
        record_demand_signal(&conn, "p18b", "leaf", "search_hit", Some("user"), &policy).unwrap();

        let leaf = db::sum_demand_weight(&conn, "p18b", "leaf", "search_hit", "-1 day").unwrap();
        let p1 = db::sum_demand_weight(&conn, "p18b", "p1", "search_hit", "-1 day").unwrap();
        let p2 = db::sum_demand_weight(&conn, "p18b", "p2", "search_hit", "-1 day").unwrap();
        assert!((leaf - 1.0).abs() < 1e-9, "leaf at full weight");
        assert!((p1 - 0.5).abs() < 1e-9, "p1 at attenuated 0.5 weight");
        assert!((p2 - 0.25).abs() < 1e-9, "p2 at attenuated 0.25 weight");

        // The search_hit signal is type-isolated: a sum on a
        // different signal type returns 0 even though the same nodes
        // received search_hit weight.
        let user_drill_at_leaf =
            db::sum_demand_weight(&conn, "p18b", "leaf", "user_drill", "-1 day").unwrap();
        assert!(user_drill_at_leaf < 1e-9, "user_drill is a separate type");
    }
}
