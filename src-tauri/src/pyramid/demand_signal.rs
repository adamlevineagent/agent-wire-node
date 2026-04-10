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

    // Phase 12 verifier fix: on-demand reactivation of deferred
    // questions. After the signal is recorded, check whether any
    // deferred question with `check_interval IN ('never','on_demand')`
    // targets the leaf node. For each, re-run triage against the
    // current policy — if it now returns `Answer`, remove the
    // deferred row so the next build picks up the question.
    let pending = list_on_demand_deferred_for_node(conn, slug, node_id).unwrap_or_default();
    if !pending.is_empty() {
        use super::triage::{resolve_decision, TriageDecision, TriageFacts};
        use super::types::LayerQuestion;
        for (qid, qjson) in pending {
            let question: LayerQuestion = match serde_json::from_str(&qjson) {
                Ok(q) => q,
                Err(_) => continue,
            };
            // has_demand_signals is true by construction — we just
            // recorded one.
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
                    let _ = db::remove_deferred(conn, slug, &qid);
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
            record_demand_signal(&conn, "s", "leaf", "user_drill", Some("user"), &policy)
                .unwrap();
        }

        let leaf_total =
            db::sum_demand_weight(&conn, "s", "leaf", "user_drill", "-1 day").unwrap();
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
}
