// pyramid/reconciliation.rs — Mechanical reconciliation after evidence-weighted answering
//
// Step 3.3 of the pyramid builder v3 plan. PURELY MECHANICAL — no LLM calls.
// Aggregates evidence data produced by the answering step to detect orphans,
// gaps, central nodes, and build a weight map.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::Connection;

use super::db;
use super::types::{EvidenceLink, EvidenceVerdict, GapReport, ReconciliationResult};

/// Reconcile a layer after evidence-weighted answering.
///
/// Loads evidence links for all answered nodes, then mechanically computes:
/// - Orphan nodes (lower-layer nodes with zero evidence references)
/// - Gap reports (MISSING verdicts)
/// - Central nodes (cited by 3+ questions with avg weight > 0.5)
/// - Weight map (aggregate KEEP weight per source node)
pub fn reconcile_layer(
    conn: &Connection,
    slug: &str,
    layer: i64,
    answered_node_ids: &[String],
    lower_layer_node_ids: &[String],
) -> Result<ReconciliationResult> {
    // (a) Load all evidence links for answered nodes at this layer
    let mut all_evidence: Vec<EvidenceLink> = Vec::new();
    for target_id in answered_node_ids {
        let links = db::get_evidence_for_target(conn, slug, target_id)?;
        all_evidence.extend(links);
    }

    // (c) Orphan detection
    let orphans = find_orphans(lower_layer_node_ids, &all_evidence);

    // (d) Gap reports — evidence links with verdict=MISSING
    let gaps = extract_gaps(&all_evidence, layer);
    for gap in &gaps {
        db::save_gap(conn, slug, gap)?;
    }

    // (e) Central nodes
    let central_nodes = find_central_nodes(&all_evidence);

    // (f) Weight map
    let weight_map = build_weight_map(&all_evidence);

    Ok(ReconciliationResult {
        orphans,
        gaps,
        central_nodes,
        weight_map,
    })
}

/// Get all orphan nodes at a given layer — nodes from the layer below
/// that no question at this layer referenced (not even DISCONNECT).
pub fn find_orphans(
    lower_layer_node_ids: &[String],
    evidence_links: &[EvidenceLink],
) -> Vec<String> {
    // Collect all source_node_ids that appear in ANY evidence link
    let referenced: HashSet<&str> = evidence_links
        .iter()
        .map(|e| e.source_node_id.as_str())
        .collect();

    lower_layer_node_ids
        .iter()
        .filter(|id| !referenced.contains(id.as_str()))
        .cloned()
        .collect()
}

/// Find central nodes — cited by 3+ questions with avg weight > 0.5.
///
/// A "central node" is a source node that appears as KEEP evidence for
/// at least 3 different target (question) nodes, with an average weight
/// exceeding 0.5. These represent cross-cutting concerns.
pub fn find_central_nodes(evidence_links: &[EvidenceLink]) -> Vec<String> {
    // Group KEEP links by source_node_id → list of (target_node_id, weight)
    let mut source_citations: HashMap<&str, Vec<(&str, f64)>> = HashMap::new();

    for link in evidence_links {
        if link.verdict == EvidenceVerdict::Keep {
            let weight = link.weight.unwrap_or(0.0);
            source_citations
                .entry(link.source_node_id.as_str())
                .or_default()
                .push((link.target_node_id.as_str(), weight));
        }
    }

    let mut central: Vec<String> = Vec::new();
    for (source_id, citations) in &source_citations {
        // Deduplicate by target_node_id — a source might have multiple links
        // to the same target, but we count distinct questions
        let unique_targets: HashSet<&str> = citations.iter().map(|(t, _)| *t).collect();
        if unique_targets.len() >= 3 {
            let total_weight: f64 = citations.iter().map(|(_, w)| w).sum();
            let avg_weight = total_weight / citations.len() as f64;
            if avg_weight > 0.5 {
                central.push(source_id.to_string());
            }
        }
    }

    central.sort(); // deterministic ordering
    central
}

/// Build weight map — aggregate KEEP weights per source node.
///
/// For each lower-layer node, sums all KEEP citation weights across
/// every question that referenced it. Higher aggregate weight means
/// the node contributed more total evidence.
pub fn build_weight_map(evidence_links: &[EvidenceLink]) -> HashMap<String, f64> {
    let mut weights: HashMap<String, f64> = HashMap::new();

    for link in evidence_links {
        if link.verdict == EvidenceVerdict::Keep {
            let weight = link.weight.unwrap_or(0.0);
            *weights
                .entry(link.source_node_id.clone())
                .or_insert(0.0) += weight;
        }
    }

    weights
}

/// Extract gap reports from MISSING evidence links.
fn extract_gaps(evidence_links: &[EvidenceLink], layer: i64) -> Vec<GapReport> {
    evidence_links
        .iter()
        .filter(|link| link.verdict == EvidenceVerdict::Missing)
        .map(|link| GapReport {
            question_id: link.target_node_id.clone(),
            description: link
                .reason
                .clone()
                .unwrap_or_else(|| "Missing evidence (no reason provided)".to_string()),
            layer,
        })
        .collect()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_link(
        source: &str,
        target: &str,
        verdict: EvidenceVerdict,
        weight: Option<f64>,
        reason: Option<&str>,
    ) -> EvidenceLink {
        EvidenceLink {
            slug: "test".to_string(),
            source_node_id: source.to_string(),
            target_node_id: target.to_string(),
            verdict,
            weight,
            reason: reason.map(|s| s.to_string()),
        }
    }

    // ── find_orphans tests ───────────────────────────────────────────────

    #[test]
    fn test_find_orphans_no_orphans() {
        let lower = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let evidence = vec![
            make_link("a", "q1", EvidenceVerdict::Keep, Some(0.8), None),
            make_link("b", "q1", EvidenceVerdict::Disconnect, None, None),
            make_link("c", "q2", EvidenceVerdict::Keep, Some(0.5), None),
        ];

        let orphans = find_orphans(&lower, &evidence);
        assert!(orphans.is_empty());
    }

    #[test]
    fn test_find_orphans_some_orphans() {
        let lower = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let evidence = vec![
            make_link("a", "q1", EvidenceVerdict::Keep, Some(0.8), None),
            make_link("c", "q2", EvidenceVerdict::Disconnect, None, None),
        ];

        let orphans = find_orphans(&lower, &evidence);
        assert_eq!(orphans.len(), 2);
        assert!(orphans.contains(&"b".to_string()));
        assert!(orphans.contains(&"d".to_string()));
    }

    #[test]
    fn test_find_orphans_all_orphans() {
        let lower = vec!["a".to_string(), "b".to_string()];
        let evidence: Vec<EvidenceLink> = vec![];

        let orphans = find_orphans(&lower, &evidence);
        assert_eq!(orphans.len(), 2);
    }

    #[test]
    fn test_find_orphans_disconnect_not_orphan() {
        // A DISCONNECT link still means the node was referenced — not an orphan
        let lower = vec!["a".to_string()];
        let evidence = vec![make_link(
            "a",
            "q1",
            EvidenceVerdict::Disconnect,
            None,
            Some("not relevant"),
        )];

        let orphans = find_orphans(&lower, &evidence);
        assert!(orphans.is_empty());
    }

    // ── find_central_nodes tests ─────────────────────────────────────────

    #[test]
    fn test_find_central_nodes_basic() {
        let evidence = vec![
            make_link("src1", "q1", EvidenceVerdict::Keep, Some(0.8), None),
            make_link("src1", "q2", EvidenceVerdict::Keep, Some(0.7), None),
            make_link("src1", "q3", EvidenceVerdict::Keep, Some(0.6), None),
            make_link("src2", "q1", EvidenceVerdict::Keep, Some(0.9), None),
        ];

        let central = find_central_nodes(&evidence);
        assert_eq!(central, vec!["src1".to_string()]);
    }

    #[test]
    fn test_find_central_nodes_low_weight_excluded() {
        // 3+ citations but avg weight <= 0.5
        let evidence = vec![
            make_link("src1", "q1", EvidenceVerdict::Keep, Some(0.3), None),
            make_link("src1", "q2", EvidenceVerdict::Keep, Some(0.4), None),
            make_link("src1", "q3", EvidenceVerdict::Keep, Some(0.2), None),
        ];

        let central = find_central_nodes(&evidence);
        assert!(central.is_empty());
    }

    #[test]
    fn test_find_central_nodes_disconnect_ignored() {
        // DISCONNECT links don't count toward centrality
        let evidence = vec![
            make_link("src1", "q1", EvidenceVerdict::Keep, Some(0.8), None),
            make_link("src1", "q2", EvidenceVerdict::Keep, Some(0.7), None),
            make_link("src1", "q3", EvidenceVerdict::Disconnect, None, None),
        ];

        let central = find_central_nodes(&evidence);
        assert!(central.is_empty()); // only 2 KEEP citations
    }

    #[test]
    fn test_find_central_nodes_deduplicates_targets() {
        // Multiple links to same target count as one unique question
        let evidence = vec![
            make_link("src1", "q1", EvidenceVerdict::Keep, Some(0.8), None),
            make_link("src1", "q1", EvidenceVerdict::Keep, Some(0.9), None),
            make_link("src1", "q2", EvidenceVerdict::Keep, Some(0.7), None),
        ];

        let central = find_central_nodes(&evidence);
        assert!(central.is_empty()); // only 2 unique targets
    }

    #[test]
    fn test_find_central_nodes_multiple() {
        let evidence = vec![
            // src1 cited by q1, q2, q3
            make_link("src1", "q1", EvidenceVerdict::Keep, Some(0.8), None),
            make_link("src1", "q2", EvidenceVerdict::Keep, Some(0.7), None),
            make_link("src1", "q3", EvidenceVerdict::Keep, Some(0.6), None),
            // src2 cited by q1, q2, q3, q4
            make_link("src2", "q1", EvidenceVerdict::Keep, Some(0.9), None),
            make_link("src2", "q2", EvidenceVerdict::Keep, Some(0.8), None),
            make_link("src2", "q3", EvidenceVerdict::Keep, Some(0.7), None),
            make_link("src2", "q4", EvidenceVerdict::Keep, Some(0.6), None),
        ];

        let central = find_central_nodes(&evidence);
        assert_eq!(central.len(), 2);
        assert!(central.contains(&"src1".to_string()));
        assert!(central.contains(&"src2".to_string()));
    }

    // ── build_weight_map tests ───────────────────────────────────────────

    #[test]
    fn test_build_weight_map_basic() {
        let evidence = vec![
            make_link("a", "q1", EvidenceVerdict::Keep, Some(0.8), None),
            make_link("a", "q2", EvidenceVerdict::Keep, Some(0.6), None),
            make_link("b", "q1", EvidenceVerdict::Keep, Some(0.5), None),
        ];

        let wm = build_weight_map(&evidence);
        assert!((wm["a"] - 1.4).abs() < 1e-10);
        assert!((wm["b"] - 0.5).abs() < 1e-10);
    }

    #[test]
    fn test_build_weight_map_ignores_disconnect() {
        let evidence = vec![
            make_link("a", "q1", EvidenceVerdict::Keep, Some(0.8), None),
            make_link("a", "q2", EvidenceVerdict::Disconnect, None, None),
            make_link("b", "q1", EvidenceVerdict::Missing, None, Some("gap")),
        ];

        let wm = build_weight_map(&evidence);
        assert!((wm["a"] - 0.8).abs() < 1e-10);
        assert!(!wm.contains_key("b"));
    }

    #[test]
    fn test_build_weight_map_none_weight_treated_as_zero() {
        let evidence = vec![make_link(
            "a",
            "q1",
            EvidenceVerdict::Keep,
            None,
            None,
        )];

        let wm = build_weight_map(&evidence);
        assert!((wm["a"]).abs() < 1e-10);
    }

    #[test]
    fn test_build_weight_map_empty() {
        let wm = build_weight_map(&[]);
        assert!(wm.is_empty());
    }

    // ── extract_gaps tests ───────────────────────────────────────────────

    #[test]
    fn test_extract_gaps_basic() {
        let evidence = vec![
            make_link(
                "a",
                "q1",
                EvidenceVerdict::Missing,
                None,
                Some("need performance data"),
            ),
            make_link("b", "q1", EvidenceVerdict::Keep, Some(0.8), None),
            make_link(
                "c",
                "q2",
                EvidenceVerdict::Missing,
                None,
                Some("missing auth details"),
            ),
        ];

        let gaps = extract_gaps(&evidence, 2);
        assert_eq!(gaps.len(), 2);
        assert_eq!(gaps[0].question_id, "q1");
        assert_eq!(gaps[0].description, "need performance data");
        assert_eq!(gaps[0].layer, 2);
        assert_eq!(gaps[1].question_id, "q2");
        assert_eq!(gaps[1].description, "missing auth details");
    }

    #[test]
    fn test_extract_gaps_no_reason_gets_default() {
        let evidence = vec![make_link("a", "q1", EvidenceVerdict::Missing, None, None)];

        let gaps = extract_gaps(&evidence, 1);
        assert_eq!(gaps.len(), 1);
        assert_eq!(
            gaps[0].description,
            "Missing evidence (no reason provided)"
        );
    }

    #[test]
    fn test_extract_gaps_none_when_no_missing() {
        let evidence = vec![
            make_link("a", "q1", EvidenceVerdict::Keep, Some(0.8), None),
            make_link("b", "q1", EvidenceVerdict::Disconnect, None, None),
        ];

        let gaps = extract_gaps(&evidence, 1);
        assert!(gaps.is_empty());
    }
}
