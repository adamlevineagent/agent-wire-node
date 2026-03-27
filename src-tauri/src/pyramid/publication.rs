// pyramid/publication.rs — Bottom-up Wire publication orchestrator
//
// Step 3.5 of the pyramid builder v3 plan.
// Publishes pyramid layers bottom-up (L0 → L1 → ... → apex) with:
//   - handle-path tracking via pyramid_id_map
//   - evidence-weighted derived_from entries
//   - orphan skipping
//   - idempotent resume (already-published nodes are skipped)
//
// NOTE: rusqlite::Connection is !Send. All DB reads happen synchronously
// before the async publish call, following the pattern from wire_publish.rs.

use std::collections::HashMap;

use anyhow::{Context, Result};

use super::db;
use super::types::{DerivedFromEntry, EvidenceLink, IdMapping, PublicationManifest, PyramidNode};
use super::wire_publish::PyramidPublisher;

/// Result of publishing a single layer.
#[derive(Debug, Clone)]
pub struct LayerPublishResult {
    pub layer: i64,
    pub published: Vec<IdMapping>,
    pub skipped_already_published: Vec<String>,
    pub skipped_orphans: Vec<String>,
    pub failed: Vec<(String, String)>, // (node_id, error_message)
}

/// Full result of a bottom-up pyramid publication.
#[derive(Debug, Clone)]
pub struct FullPublicationResult {
    pub slug: String,
    pub layers: Vec<LayerPublishResult>,
    pub total_published: usize,
    pub total_skipped: usize,
    pub total_failed: usize,
}

// ─── DerivedFrom Construction ────────────────────────────────────────────────

/// Build derived_from for an L0 node (cites source files).
///
/// L0 evidence links have source_node_id pointing to chunk/source identifiers.
/// For L0, source_type is "source_document" and ref_path is the source_node_id
/// (which represents the source file/chunk path).
///
/// If no evidence links exist (L0 nodes created by the chain executor don't
/// write to pyramid_evidence), we create a synthetic self-citation using the
/// node's ID as source reference with weight 1.0.
///
/// NOTE: The Wire validates `source_item_id` as UUID or handle-path format.
/// L0 `source_document` citations use local IDs (e.g., "C-L0-003") which may
/// not pass Wire validation. The Wire validates `source_item_id` as UUID or
/// handle-path format, so we use a deterministic UUID v5 placeholder derived
/// from the local ID. TODO: Wire should support source_document citations
/// without strict UUID validation, or we should register source documents
/// on the Wire first and use their real UUIDs.
fn build_l0_derived_from(
    node: &PyramidNode,
    evidence: &[EvidenceLink],
    id_map: &HashMap<String, String>, // local_id → wire_uuid (for source doc lookups)
) -> Vec<DerivedFromEntry> {
    if evidence.is_empty() {
        // L0 nodes created by the chain executor have no evidence links.
        // Use the Wire UUID from id_map if the source doc was registered,
        // otherwise generate a deterministic placeholder UUID.
        let ref_path = id_map
            .get(&node.id)
            .cloned()
            .unwrap_or_else(|| make_placeholder_uuid(&node.id));
        return vec![DerivedFromEntry {
            ref_path,
            source_type: "source_document".to_string(),
            weight: 1.0,
            justification: Some("L0 extraction from source file".to_string()),
        }];
    }

    evidence
        .iter()
        .map(|e| {
            // Use Wire UUID from id_map if available, else placeholder UUID
            let ref_path = id_map
                .get(&e.source_node_id)
                .cloned()
                .unwrap_or_else(|| make_placeholder_uuid(&e.source_node_id));
            DerivedFromEntry {
                ref_path,
                source_type: "source_document".to_string(),
                weight: e.weight.unwrap_or(1.0),
                justification: Some(e.reason.clone().unwrap_or_else(|| "Evidence-based citation".to_string())),
            }
        })
        .collect()
}

/// Generate a deterministic placeholder UUID (v5-style) from a local ID.
///
/// The Wire validates source_item_id as UUID or handle-path format. For L0
/// source document citations where we don't have a real Wire UUID, we produce
/// a UUID-formatted string so it passes Wire validation.
/// TODO: Replace with real Wire UUIDs once source document registration is implemented.
fn make_placeholder_uuid(local_id: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    local_id.hash(&mut hasher);
    let h = hasher.finish();
    // Format as UUID-like string: 00000000-0000-5000-8000-XXXXXXXXXXXX
    format!(
        "00000000-0000-5000-8000-{:012x}",
        h & 0xFFFF_FFFF_FFFF
    )
}

/// Build derived_from for L1+ node (cites published lower-layer nodes).
///
/// Looks up each evidence source_node_id in the id_map to get Wire handle-paths.
/// Sources without a handle-path are logged and skipped (the lower-layer node
/// may have been an orphan or failed to publish).
fn build_upper_derived_from(
    evidence: &[EvidenceLink],
    id_map: &HashMap<String, String>, // local_id → wire_uuid
) -> Vec<DerivedFromEntry> {
    let mut entries = Vec::new();
    for e in evidence {
        match id_map.get(&e.source_node_id) {
            Some(wire_uuid) => {
                entries.push(DerivedFromEntry {
                    ref_path: wire_uuid.clone(),
                    source_type: "contribution".to_string(),
                    weight: e.weight.unwrap_or(1.0),
                    justification: Some(e.reason.clone().unwrap_or_else(|| "Evidence-based citation".to_string())),
                });
            }
            None => {
                tracing::warn!(
                    source_node_id = %e.source_node_id,
                    "skipping derived_from entry: lower-layer node not in id_map (orphan or unpublished)"
                );
            }
        }
    }
    entries
}

/// Normalize weights in-place so they sum to 1.0.
///
/// If all weights are zero (or the slice is empty), leaves them as-is.
fn normalize_weights(entries: &mut [DerivedFromEntry]) {
    let sum: f64 = entries.iter().map(|e| e.weight).sum();
    if sum > 0.0 {
        for entry in entries.iter_mut() {
            entry.weight /= sum;
        }
    }
}

// ─── Layer Publication ───────────────────────────────────────────────────────

/// Pre-loaded data for a single node, gathered synchronously from SQLite
/// before the async publish loop.
struct NodePublishData {
    node: PyramidNode,
    derived_from: Vec<DerivedFromEntry>,
}

/// Publish all non-orphan nodes at a single layer.
///
/// All DB reads happen synchronously before the async publisher calls.
/// On publish failure for individual nodes, the error is logged and the node
/// is recorded in the failed list. Already-published nodes are skipped.
pub async fn publish_layer(
    publisher: &PyramidPublisher,
    conn: &rusqlite::Connection,
    slug: &str,
    layer: i64,
    node_ids: &[String],
    orphan_ids: &[String],
) -> Result<LayerPublishResult> {
    let orphan_set: std::collections::HashSet<&str> =
        orphan_ids.iter().map(|s| s.as_str()).collect();

    let mut result = LayerPublishResult {
        layer,
        published: Vec::new(),
        skipped_already_published: Vec::new(),
        skipped_orphans: Vec::new(),
        failed: Vec::new(),
    };

    // ── Phase 1: Synchronous DB reads ────────────────────────────────────
    // Gather all data we need before entering the async publish loop,
    // because rusqlite::Connection is !Send.

    // Build the current id_map from already-published entries.
    // Maps local_id → wire_uuid (Issue 1: use Wire UUID, not fabricated handle-path)
    let existing_mappings = db::get_all_id_mappings(conn, slug)
        .context("publication: failed to load existing id mappings")?;
    let mut id_map: HashMap<String, String> = existing_mappings
        .into_iter()
        .map(|m| {
            // Prefer wire_uuid; fall back to wire_handle_path for legacy entries
            let wire_ref = m.wire_uuid.clone().unwrap_or(m.wire_handle_path.clone());
            (m.local_id, wire_ref)
        })
        .collect();

    let mut nodes_to_publish: Vec<NodePublishData> = Vec::new();

    for node_id in node_ids {
        // Skip orphans
        if orphan_set.contains(node_id.as_str()) {
            result.skipped_orphans.push(node_id.clone());
            continue;
        }

        // Skip already-published (idempotency)
        match db::is_already_published(conn, slug, node_id) {
            Ok(true) => {
                tracing::info!(
                    slug = slug,
                    node_id = %node_id,
                    layer = layer,
                    "skipped already-published node"
                );
                result.skipped_already_published.push(node_id.clone());
                continue;
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(
                    slug = slug,
                    node_id = %node_id,
                    error = %e,
                    "failed to check publication status, will attempt publish"
                );
            }
        }

        // Load the node
        let node = match db::get_node(conn, slug, node_id)? {
            Some(n) => n,
            None => {
                tracing::warn!(
                    slug = slug,
                    node_id = %node_id,
                    layer = layer,
                    "node not found in DB, skipping"
                );
                result
                    .failed
                    .push((node_id.clone(), "node not found in DB".to_string()));
                continue;
            }
        };

        // Load KEEP evidence
        let evidence = db::get_keep_evidence_for_target(conn, slug, node_id)
            .context("publication: failed to load evidence")?;

        // Build derived_from based on layer
        let mut derived_from = if layer == 0 {
            build_l0_derived_from(&node, &evidence, &id_map)
        } else {
            build_upper_derived_from(&evidence, &id_map)
        };

        // Normalize weights to sum=1.0
        normalize_weights(&mut derived_from);

        // Issue 3: Zero KEEP verdicts produce empty derived_from — Wire rejects
        // empty derived_from for L1+ nodes. Skip publication and treat as orphan.
        if derived_from.is_empty() && layer > 0 {
            tracing::warn!(
                slug = slug,
                node_id = %node_id,
                layer = layer,
                "no KEEP evidence — treated as orphan (skipping publication)"
            );
            result.skipped_orphans.push(node_id.clone());
            continue;
        }

        if derived_from.is_empty() {
            tracing::warn!(
                slug = slug,
                node_id = %node_id,
                layer = layer,
                "L0 node has no derived_from entries (no evidence links)"
            );
        }

        nodes_to_publish.push(NodePublishData {
            node,
            derived_from,
        });
    }

    // ── Phase 2: Async publish loop ──────────────────────────────────────

    for data in &nodes_to_publish {
        match publisher
            .publish_pyramid_node(&data.node, &data.derived_from, None)
            .await
        {
            Ok((wire_uuid, wire_handle_path)) => {
                // Use the real handle-path from the Wire response, falling back
                // to UUID if the Wire didn't return one (older Wire versions)
                let handle_path = wire_handle_path.unwrap_or_else(|| wire_uuid.clone());

                let mapping = IdMapping {
                    local_id: data.node.id.clone(),
                    wire_handle_path: handle_path.clone(),
                    wire_uuid: Some(wire_uuid.clone()),
                    published_at: chrono::Utc::now().to_rfc3339(),
                };

                // Issue 1: Store Wire UUID in id_map for derived_from lookups,
                // not the fabricated handle-path
                id_map.insert(data.node.id.clone(), wire_uuid.clone());

                // Issue 2: Persist mapping immediately so a mid-layer crash
                // doesn't lose already-published nodes
                if let Err(e) = db::save_id_mapping_extended(conn, slug, &mapping) {
                    tracing::error!(
                        slug = slug,
                        local_id = %mapping.local_id,
                        error = %e,
                        "failed to persist id mapping immediately — will retry at layer end"
                    );
                }

                result.published.push(mapping);

                tracing::info!(
                    slug = slug,
                    node_id = %data.node.id,
                    layer = layer,
                    wire_uuid = %wire_uuid,
                    "published pyramid node to Wire"
                );
            }
            Err(e) => {
                tracing::error!(
                    slug = slug,
                    node_id = %data.node.id,
                    layer = layer,
                    error = %e,
                    "failed to publish pyramid node"
                );
                result
                    .failed
                    .push((data.node.id.clone(), e.to_string()));
                // Continue — partial publication is better than none
            }
        }
    }

    Ok(result)
}

/// Orchestrate full bottom-up pyramid publication.
///
/// Iterates layers 0 → max_depth. Each layer MUST complete before the next
/// starts because upper layers reference handle-paths from lower layers.
///
/// On individual node failure: logs error, continues. The idempotency check
/// means a retry will skip already-published nodes.
pub async fn publish_pyramid_bottom_up(
    publisher: &PyramidPublisher,
    conn: &rusqlite::Connection,
    slug: &str,
    max_depth: i64,
    orphans_by_layer: &HashMap<i64, Vec<String>>,
) -> Result<FullPublicationResult> {
    let mut full_result = FullPublicationResult {
        slug: slug.to_string(),
        layers: Vec::new(),
        total_published: 0,
        total_skipped: 0,
        total_failed: 0,
    };

    let empty_orphans: Vec<String> = Vec::new();

    for layer in 0..=max_depth {
        tracing::info!(
            slug = slug,
            layer = layer,
            max_depth = max_depth,
            "publishing layer"
        );

        // Load all node IDs at this depth
        let nodes_at_layer = db::get_nodes_at_depth(conn, slug, layer)
            .with_context(|| format!("publication: failed to load nodes at depth {}", layer))?;

        let node_ids: Vec<String> = nodes_at_layer.iter().map(|n| n.id.clone()).collect();

        if node_ids.is_empty() {
            tracing::info!(
                slug = slug,
                layer = layer,
                "no nodes at layer, skipping"
            );
            continue;
        }

        let orphans = orphans_by_layer.get(&layer).unwrap_or(&empty_orphans);

        let layer_result = publish_layer(publisher, conn, slug, layer, &node_ids, orphans).await?;

        // Mappings are already persisted inside publish_layer (Issue 2 fix).
        // This secondary pass is a safety net — the upsert is idempotent.
        for mapping in &layer_result.published {
            if let Err(e) = db::save_id_mapping_extended(conn, slug, mapping) {
                tracing::error!(
                    slug = slug,
                    local_id = %mapping.local_id,
                    error = %e,
                    "failed to persist id mapping in safety-net pass"
                );
            }
        }

        tracing::info!(
            slug = slug,
            layer = layer,
            published = layer_result.published.len(),
            skipped_published = layer_result.skipped_already_published.len(),
            skipped_orphans = layer_result.skipped_orphans.len(),
            failed = layer_result.failed.len(),
            "layer publication complete"
        );

        full_result.total_published += layer_result.published.len();
        full_result.total_skipped +=
            layer_result.skipped_already_published.len() + layer_result.skipped_orphans.len();
        full_result.total_failed += layer_result.failed.len();
        full_result.layers.push(layer_result);
    }

    tracing::info!(
        slug = slug,
        total_published = full_result.total_published,
        total_skipped = full_result.total_skipped,
        total_failed = full_result.total_failed,
        "full pyramid publication complete"
    );

    Ok(full_result)
}

/// Build a PublicationManifest for a single layer (used for logging/reporting).
pub fn build_manifest(
    slug: &str,
    layer: i64,
    node_ids: &[String],
    orphan_ids: &[String],
) -> PublicationManifest {
    let orphan_set: std::collections::HashSet<&str> =
        orphan_ids.iter().map(|s| s.as_str()).collect();

    let non_orphan: Vec<String> = node_ids
        .iter()
        .filter(|id| !orphan_set.contains(id.as_str()))
        .cloned()
        .collect();

    PublicationManifest {
        slug: slug.to_string(),
        layer,
        nodes_to_publish: non_orphan,
        skipped_orphans: orphan_ids.to_vec(),
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::types::EvidenceVerdict;

    fn make_evidence(source_id: &str, target_id: &str, weight: f64) -> EvidenceLink {
        EvidenceLink {
            slug: "test".to_string(),
            source_node_id: source_id.to_string(),
            target_node_id: target_id.to_string(),
            verdict: EvidenceVerdict::Keep,
            weight: Some(weight),
            reason: Some(format!("evidence from {}", source_id)),
        }
    }

    fn make_node(id: &str, depth: i64) -> PyramidNode {
        use crate::pyramid::types::{Correction, Decision, Term, Topic};
        PyramidNode {
            id: id.to_string(),
            slug: "test".to_string(),
            depth,
            chunk_index: None,
            headline: format!("Node {}", id),
            distilled: format!("Content of {}", id),
            topics: vec![Topic {
                name: "test".to_string(),
                current: "current".to_string(),
                entities: vec![],
                corrections: vec![],
                decisions: vec![],
            }],
            corrections: vec![],
            decisions: vec![],
            terms: vec![],
            dead_ends: vec![],
            self_prompt: String::new(),
            children: vec![],
            parent_id: None,
            superseded_by: None,
            created_at: "2026-03-26T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn test_build_l0_derived_from_with_evidence() {
        let node = make_node("L0-001", 0);
        let evidence = vec![
            make_evidence("src/main.rs", "L0-001", 0.7),
            make_evidence("src/lib.rs", "L0-001", 0.3),
        ];
        let id_map = HashMap::new(); // no Wire UUIDs for source docs

        let entries = build_l0_derived_from(&node, &evidence, &id_map);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].source_type, "source_document");
        // Issue 5: ref_path is now a placeholder UUID (not raw file path)
        assert!(entries[0].ref_path.contains('-'), "ref_path should be UUID-formatted");
        assert_eq!(entries[0].weight, 0.7);
        assert_eq!(entries[1].source_type, "source_document");
        // Issue 4: justification is never None
        assert_eq!(entries[0].justification, Some("evidence from src/main.rs".to_string()));
        assert_eq!(entries[1].justification, Some("evidence from src/lib.rs".to_string()));
    }

    #[test]
    fn test_build_l0_derived_from_with_wire_uuid_lookup() {
        let node = make_node("L0-001", 0);
        let evidence = vec![
            make_evidence("src/main.rs", "L0-001", 1.0),
        ];
        let mut id_map = HashMap::new();
        id_map.insert("src/main.rs".to_string(), "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_string());

        let entries = build_l0_derived_from(&node, &evidence, &id_map);
        assert_eq!(entries.len(), 1);
        // When a Wire UUID exists in id_map, it should be used
        assert_eq!(entries[0].ref_path, "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");
    }

    #[test]
    fn test_build_l0_derived_from_empty_evidence() {
        let node = make_node("L0-001", 0);
        let id_map = HashMap::new();

        let entries = build_l0_derived_from(&node, &[], &id_map);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source_type, "source_document");
        // Synthetic citation uses placeholder UUID
        assert!(entries[0].ref_path.contains('-'), "ref_path should be UUID-formatted");
        assert_eq!(entries[0].justification, Some("L0 extraction from source file".to_string()));
    }

    #[test]
    fn test_build_l0_derived_from_none_reason_defaults() {
        let node = make_node("L0-001", 0);
        let evidence = vec![EvidenceLink {
            slug: "test".to_string(),
            source_node_id: "src/main.rs".to_string(),
            target_node_id: "L0-001".to_string(),
            verdict: EvidenceVerdict::Keep,
            weight: Some(1.0),
            reason: None, // Issue 4: None reason
        }];
        let id_map = HashMap::new();

        let entries = build_l0_derived_from(&node, &evidence, &id_map);
        assert_eq!(entries.len(), 1);
        // Issue 4: justification defaults to non-empty string
        assert_eq!(entries[0].justification, Some("Evidence-based citation".to_string()));
    }

    #[test]
    fn test_build_upper_derived_from() {
        let evidence = vec![
            make_evidence("L0-001", "L1-001", 0.6),
            make_evidence("L0-002", "L1-001", 0.4),
            make_evidence("L0-orphan", "L1-001", 0.1), // not in id_map
        ];

        let mut id_map = HashMap::new();
        // Issue 1: id_map now stores wire_uuid, not handle-path
        id_map.insert(
            "L0-001".to_string(),
            "uuid-for-L0-001".to_string(),
        );
        id_map.insert(
            "L0-002".to_string(),
            "uuid-for-L0-002".to_string(),
        );

        let entries = build_upper_derived_from(&evidence, &id_map);
        assert_eq!(entries.len(), 2); // orphan source skipped
        assert_eq!(entries[0].source_type, "contribution");
        assert_eq!(entries[0].ref_path, "uuid-for-L0-001");
        assert_eq!(entries[1].ref_path, "uuid-for-L0-002");
    }

    #[test]
    fn test_build_upper_derived_from_none_reason_defaults() {
        let evidence = vec![EvidenceLink {
            slug: "test".to_string(),
            source_node_id: "L0-001".to_string(),
            target_node_id: "L1-001".to_string(),
            verdict: EvidenceVerdict::Keep,
            weight: Some(1.0),
            reason: None, // Issue 4: None reason
        }];

        let mut id_map = HashMap::new();
        id_map.insert("L0-001".to_string(), "uuid-for-L0-001".to_string());

        let entries = build_upper_derived_from(&evidence, &id_map);
        assert_eq!(entries.len(), 1);
        // Issue 4: justification defaults to non-empty string
        assert_eq!(entries[0].justification, Some("Evidence-based citation".to_string()));
    }

    #[test]
    fn test_make_placeholder_uuid_format() {
        let uuid = make_placeholder_uuid("C-L0-003");
        // Should be UUID-formatted: 00000000-0000-5000-8000-XXXXXXXXXXXX
        assert!(uuid.starts_with("00000000-0000-5000-8000-"));
        assert_eq!(uuid.len(), 36); // standard UUID length
    }

    #[test]
    fn test_make_placeholder_uuid_deterministic() {
        let a = make_placeholder_uuid("same-input");
        let b = make_placeholder_uuid("same-input");
        assert_eq!(a, b, "placeholder UUID should be deterministic");
    }

    #[test]
    fn test_normalize_weights() {
        let mut entries = vec![
            DerivedFromEntry {
                ref_path: "a".to_string(),
                source_type: "contribution".to_string(),
                weight: 3.0,
                justification: None,
            },
            DerivedFromEntry {
                ref_path: "b".to_string(),
                source_type: "contribution".to_string(),
                weight: 7.0,
                justification: None,
            },
        ];

        normalize_weights(&mut entries);
        assert!((entries[0].weight - 0.3).abs() < 1e-10);
        assert!((entries[1].weight - 0.7).abs() < 1e-10);

        let sum: f64 = entries.iter().map(|e| e.weight).sum();
        assert!((sum - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_normalize_weights_empty() {
        let mut entries: Vec<DerivedFromEntry> = vec![];
        normalize_weights(&mut entries); // should not panic
        assert!(entries.is_empty());
    }

    #[test]
    fn test_normalize_weights_all_zero() {
        let mut entries = vec![
            DerivedFromEntry {
                ref_path: "a".to_string(),
                source_type: "contribution".to_string(),
                weight: 0.0,
                justification: None,
            },
        ];
        normalize_weights(&mut entries);
        // Should leave as-is (no division by zero)
        assert_eq!(entries[0].weight, 0.0);
    }

    #[test]
    fn test_build_manifest() {
        let manifest = build_manifest(
            "my-slug",
            1,
            &[
                "A".to_string(),
                "B".to_string(),
                "C".to_string(),
            ],
            &["B".to_string()],
        );

        assert_eq!(manifest.slug, "my-slug");
        assert_eq!(manifest.layer, 1);
        assert_eq!(manifest.nodes_to_publish, vec!["A", "C"]);
        assert_eq!(manifest.skipped_orphans, vec!["B"]);
    }
}
