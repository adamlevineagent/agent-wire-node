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
                justification: Some(
                    e.reason
                        .clone()
                        .unwrap_or_else(|| "Evidence-based citation".to_string()),
                ),
            }
        })
        .collect()
}

/// Generate a deterministic placeholder UUID v5 from a local ID.
///
/// The Wire validates source_item_id as UUID or handle-path format. For L0
/// source document citations where we don't have a real Wire UUID, we produce
/// a real UUID v5 (SHA-1 based, stable across all Rust versions and platforms).
/// TODO: Replace with real Wire UUIDs once source document registration is implemented.
fn make_placeholder_uuid(local_id: &str) -> String {
    use uuid::Uuid;
    // Fixed namespace for Wire Node placeholder UUIDs (generated once, never changes).
    // This is a v4 UUID used solely as a namespace for deterministic v5 generation.
    const WIRE_NODE_NAMESPACE: Uuid = Uuid::from_bytes([
        0x9b, 0x6a, 0xe3, 0x2f, 0x1c, 0x4d, 0x4a, 0x7e, 0xb8, 0x52, 0xd3, 0xf1, 0xa0, 0xc9, 0x67,
        0x2b,
    ]);
    Uuid::new_v5(&WIRE_NODE_NAMESPACE, local_id.as_bytes()).to_string()
}

/// Build derived_from for L1+ node (cites published lower-layer nodes).
///
/// Looks up each evidence source_node_id in the per-slug id_maps to get Wire UUIDs.
/// For cross-slug handle-paths (e.g. "other-slug/0/node-id"), parses the slug
/// and bare node_id to look up in the correct slug's id_map.
/// Sources without a mapping are logged and skipped (the lower-layer node
/// may have been an orphan or failed to publish).
fn build_upper_derived_from(
    evidence: &[EvidenceLink],
    id_maps: &HashMap<String, HashMap<String, String>>, // slug → (local_id → wire_uuid)
    current_slug: &str,
) -> Vec<DerivedFromEntry> {
    let mut entries = Vec::new();
    for e in evidence {
        // Try to resolve the source_node_id via handle-path parsing first
        let wire_uuid =
            if let Some((ref_slug, _depth, bare_id)) = db::parse_handle_path(&e.source_node_id) {
                // Cross-slug handle-path: look up in the referenced slug's id_map
                id_maps.get(ref_slug).and_then(|m| m.get(bare_id))
            } else {
                // Same-slug bare node_id: look up in current slug's id_map
                id_maps
                    .get(current_slug)
                    .and_then(|m| m.get(&e.source_node_id))
            };

        match wire_uuid {
            Some(uuid) => {
                entries.push(DerivedFromEntry {
                    ref_path: uuid.clone(),
                    source_type: "contribution".to_string(),
                    weight: e.weight.unwrap_or(1.0),
                    justification: Some(
                        e.reason
                            .clone()
                            .unwrap_or_else(|| "Evidence-based citation".to_string()),
                    ),
                });
            }
            None => {
                tracing::warn!(
                    source_node_id = %e.source_node_id,
                    current_slug = current_slug,
                    "skipping derived_from entry: lower-layer node not in id_maps (orphan or unpublished)"
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

/// Result of synchronous data collection for a layer (Phase 1).
/// Contains everything needed for the async publish phase, plus any
/// skip/fail results accumulated during the DB read phase.
struct LayerCollectResult {
    nodes_to_publish: Vec<NodePublishData>,
    skipped_already_published: Vec<String>,
    skipped_orphans: Vec<String>,
    failed: Vec<(String, String)>,
}

/// Phase 1 (SYNC): Collect all data needed to publish a layer from SQLite.
///
/// This function does all DB reads synchronously and returns owned data.
/// The `conn` reference does NOT escape this function — it is safe to drop
/// the connection after this call and before entering the async publish phase.
pub fn collect_layer_publish_data(
    conn: &rusqlite::Connection,
    slug: &str,
    layer: i64,
    node_ids: &[String],
    orphan_ids: &[String],
    id_maps: &HashMap<String, HashMap<String, String>>,
) -> Result<LayerCollectResult> {
    let orphan_set: std::collections::HashSet<&str> =
        orphan_ids.iter().map(|s| s.as_str()).collect();

    let mut result = LayerCollectResult {
        nodes_to_publish: Vec::new(),
        skipped_already_published: Vec::new(),
        skipped_orphans: Vec::new(),
        failed: Vec::new(),
    };

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

        // Load KEEP evidence (cross-slug aware)
        let evidence = db::get_keep_evidence_for_target_cross(conn, slug, node_id)
            .context("publication: failed to load evidence")?;

        // Build a flat id_map for L0 (only needs current slug's mappings)
        let flat_id_map: HashMap<String, String> = id_maps.get(slug).cloned().unwrap_or_default();

        // Build derived_from based on layer
        let mut derived_from = if layer == 0 {
            build_l0_derived_from(&node, &evidence, &flat_id_map)
        } else {
            build_upper_derived_from(&evidence, id_maps, slug)
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

        result
            .nodes_to_publish
            .push(NodePublishData { node, derived_from });
    }

    Ok(result)
}

/// Phase 2 (ASYNC): Publish pre-collected node data to the Wire.
///
/// Takes ownership of data collected by `collect_layer_publish_data`.
/// No `conn` reference — all DB reads happened in Phase 1.
/// Returns the layer result with published mappings for the caller to persist.
pub async fn publish_layer(
    publisher: &PyramidPublisher,
    slug: &str,
    layer: i64,
    collected: LayerCollectResult,
) -> Result<LayerPublishResult> {
    let mut result = LayerPublishResult {
        layer,
        published: Vec::new(),
        skipped_already_published: collected.skipped_already_published,
        skipped_orphans: collected.skipped_orphans,
        failed: collected.failed,
    };

    for data in &collected.nodes_to_publish {
        match publisher
            .publish_pyramid_node(&data.node, &data.derived_from, None)
            .await
        {
            Ok((wire_uuid, wire_handle_path)) => {
                let handle_path = wire_handle_path.unwrap_or_else(|| wire_uuid.clone());

                let mapping = IdMapping {
                    local_id: data.node.id.clone(),
                    wire_handle_path: handle_path.clone(),
                    wire_uuid: Some(wire_uuid.clone()),
                    published_at: chrono::Utc::now().to_rfc3339(),
                };

                tracing::info!(
                    slug = slug,
                    node_id = %data.node.id,
                    layer = layer,
                    wire_uuid = %wire_uuid,
                    "published pyramid node to Wire"
                );

                result.published.push(mapping);
            }
            Err(e) => {
                tracing::error!(
                    slug = slug,
                    node_id = %data.node.id,
                    layer = layer,
                    error = %e,
                    "failed to publish pyramid node"
                );
                result.failed.push((data.node.id.clone(), e.to_string()));
                // Continue — partial publication is better than none
            }
        }
    }

    Ok(result)
}

/// Orchestrate full bottom-up pyramid publication (local-first architecture).
///
/// Pattern: SYNC phase collects all data from SQLite per layer, drops the conn
/// reference, then ASYNC phase publishes to Wire. This avoids holding a
/// `rusqlite::Connection` (which is `!Send`) across `.await` points.
///
/// Iterates layers 0 → max_depth. Each layer MUST complete before the next
/// starts because upper layers reference handle-paths from lower layers.
///
/// After successful publication, writes `build_id` to `last_published_build_id`
/// on the slug so the sync timer knows this build has been published.
///
/// Returns a `FullPublicationResult` with all mappings. The caller is
/// responsible for persisting ID mappings back to SQLite (scoped write lock).
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

    // Build per-slug id_maps from already-published entries.
    // Load current slug's mappings first, then all referenced slugs.
    let mut id_maps: HashMap<String, HashMap<String, String>> = HashMap::new();

    // Current slug
    let existing_mappings = db::get_all_id_mappings(conn, slug)
        .context("publication: failed to load existing id mappings")?;
    let current_slug_map: HashMap<String, String> = existing_mappings
        .into_iter()
        .map(|m| {
            let wire_ref = m.wire_uuid.clone().unwrap_or(m.wire_handle_path.clone());
            (m.local_id, wire_ref)
        })
        .collect();
    id_maps.insert(slug.to_string(), current_slug_map);

    // Referenced slugs (cross-slug evidence needs their id_maps)
    let referenced_slugs = db::get_slug_references(conn, slug).unwrap_or_default();
    for ref_slug in &referenced_slugs {
        let ref_mappings = db::get_all_id_mappings(conn, ref_slug).unwrap_or_default();
        let ref_map: HashMap<String, String> = ref_mappings
            .into_iter()
            .map(|m| {
                let wire_ref = m.wire_uuid.clone().unwrap_or(m.wire_handle_path.clone());
                (m.local_id, wire_ref)
            })
            .collect();
        if !ref_map.is_empty() {
            id_maps.insert(ref_slug.clone(), ref_map);
        }
    }

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
            tracing::info!(slug = slug, layer = layer, "no nodes at layer, skipping");
            continue;
        }

        let orphans = orphans_by_layer.get(&layer).unwrap_or(&empty_orphans);

        // Phase 1 (SYNC): collect all data from SQLite — conn ref is scoped here
        let collected =
            collect_layer_publish_data(conn, slug, layer, &node_ids, orphans, &id_maps)?;

        // Phase 2 (ASYNC): publish to Wire — no conn reference held
        let layer_result = publish_layer(publisher, slug, layer, collected).await?;

        // Update id_maps with newly published mappings for next layer's derived_from
        for mapping in &layer_result.published {
            if let Some(ref wire_uuid) = mapping.wire_uuid {
                id_maps
                    .entry(slug.to_string())
                    .or_default()
                    .insert(mapping.local_id.clone(), wire_uuid.clone());
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

    // After successful publication, record the build_id so the sync timer
    // knows this build has been published and won't re-trigger.
    if full_result.total_published > 0 || full_result.total_skipped > 0 {
        if let Ok(Some(build_id)) = db::get_current_build_id(conn, slug) {
            if let Err(e) = db::set_last_published_build_id(conn, slug, &build_id) {
                tracing::warn!(
                    slug = slug,
                    build_id = %build_id,
                    error = %e,
                    "failed to update last_published_build_id after publication"
                );
            } else {
                tracing::info!(
                    slug = slug,
                    build_id = %build_id,
                    "updated last_published_build_id"
                );
            }
        }
    }

    Ok(full_result)
}

// ─── Discovery Metadata (WS-ONLINE-B) ────────────────────────────────────────

/// Data needed to publish pyramid discovery metadata, collected synchronously
/// from SQLite before the async publish call.
pub struct MetadataPublishData {
    pub metadata: super::wire_publish::PyramidMetadata,
    pub supersedes_uuid: Option<String>,
}

/// Phase 1 (SYNC): Collect all data needed to publish discovery metadata.
///
/// Reads slug info, apex node, access tier, and existing metadata UUID from
/// SQLite. The `conn` reference does NOT escape this function.
///
/// `tunnel_url` is passed in because it lives in the app's TunnelState, which
/// is not accessible from the publication context.
pub fn collect_metadata_publish_data(
    conn: &rusqlite::Connection,
    slug: &str,
    tunnel_url: Option<String>,
) -> Result<Option<MetadataPublishData>> {
    // Load slug info
    let slug_info = match db::get_slug(conn, slug)? {
        Some(info) => info,
        None => return Ok(None),
    };

    // Load apex node (highest depth)
    let apex_nodes = db::get_nodes_at_depth(conn, slug, slug_info.max_depth)?;
    let apex = match apex_nodes.first() {
        Some(n) => n,
        None => {
            tracing::warn!(slug = slug, "no apex node found for metadata publish");
            return Ok(None);
        }
    };

    // Load access tier, price, absorption mode
    let (access_tier, access_price, absorption_mode) = db::get_slug_online_fields(conn, slug)?;

    // Load existing metadata contribution UUID for supersession
    let supersedes_uuid = db::get_slug_metadata_contribution_id(conn, slug)?;

    // Collect topics from the apex node
    let topics: Vec<String> = apex.topics.iter().map(|t| t.name.clone()).collect();

    let metadata = super::wire_publish::PyramidMetadata {
        pyramid_slug: slug.to_string(),
        node_count: slug_info.node_count,
        max_depth: slug_info.max_depth,
        content_type: slug_info.content_type.as_str().to_string(),
        quality_score: 0.0, // placeholder for now
        tunnel_url,
        apex_headline: apex.headline.clone(),
        apex_body: apex.distilled.clone(),
        topics,
        last_build_at: slug_info.last_built_at.clone(),
        access_tier,
        access_price,
        absorption_mode,
    };

    Ok(Some(MetadataPublishData {
        metadata,
        supersedes_uuid,
    }))
}

// ─── Corpus Registration ─────────────────────────────────────────────────────

/// Register a source document with the Wire as a corpus document.
///
/// Calls `POST /api/v1/wire/corpora/{slug}/documents` on the Wire server.
/// Returns the Wire-assigned corpus document UUID.
///
/// Falls back to a deterministic placeholder UUID if the HTTP call fails,
/// so publication can proceed without a live Wire connection.
pub async fn register_corpus_document(
    slug: &str,
    file_path: &str,
    content_hash: &str,
    publisher: &PyramidPublisher,
) -> Result<String> {
    let url = format!(
        "{}/api/v1/wire/corpora/{}/documents",
        publisher.wire_url.trim_end_matches('/'),
        slug
    );

    // Derive a title from the file path (filename without extension)
    let title = std::path::Path::new(file_path)
        .file_stem()
        .map(|s| s.to_string_lossy().replace('-', " ").replace('_', " "))
        .unwrap_or_else(|| file_path.to_string().into());

    // Infer format from extension
    let format = match std::path::Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
    {
        Some("md" | "markdown") => "text/markdown",
        Some("html" | "htm") => "text/html",
        Some("pdf") => "application/pdf",
        _ => "text/plain",
    };

    let body = serde_json::json!({
        "title": title,
        "format": format,
        "source_path": file_path,
        "content_hash": content_hash,
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("register_corpus_document: failed to build HTTP client")?;

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", publisher.auth_token))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await;

    match resp {
        Ok(response) if response.status().is_success() => {
            #[derive(serde::Deserialize)]
            struct CorpusDocResponse {
                id: String,
            }
            match response.json::<CorpusDocResponse>().await {
                Ok(parsed) => {
                    tracing::info!(
                        slug = slug,
                        file_path = file_path,
                        wire_uuid = %parsed.id,
                        "registered corpus document on Wire"
                    );
                    Ok(parsed.id)
                }
                Err(e) => {
                    tracing::warn!(
                        slug = slug,
                        file_path = file_path,
                        error = %e,
                        "failed to parse corpus doc response, falling back to placeholder UUID"
                    );
                    let composite_key = format!("{}:{}:{}", slug, file_path, content_hash);
                    Ok(make_placeholder_uuid(&composite_key))
                }
            }
        }
        Ok(response) => {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            tracing::warn!(
                slug = slug,
                file_path = file_path,
                status = %status,
                body = %text.chars().take(200).collect::<String>(),
                "corpus document registration failed, falling back to placeholder UUID"
            );
            let composite_key = format!("{}:{}:{}", slug, file_path, content_hash);
            Ok(make_placeholder_uuid(&composite_key))
        }
        Err(e) => {
            tracing::warn!(
                slug = slug,
                file_path = file_path,
                error = %e,
                "corpus document registration network error, falling back to placeholder UUID"
            );
            let composite_key = format!("{}:{}:{}", slug, file_path, content_hash);
            Ok(make_placeholder_uuid(&composite_key))
        }
    }
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
            build_id: None,
            live: Some(true),
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
            build_id: None,
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
        assert!(
            entries[0].ref_path.contains('-'),
            "ref_path should be UUID-formatted"
        );
        assert_eq!(entries[0].weight, 0.7);
        assert_eq!(entries[1].source_type, "source_document");
        // Issue 4: justification is never None
        assert_eq!(
            entries[0].justification,
            Some("evidence from src/main.rs".to_string())
        );
        assert_eq!(
            entries[1].justification,
            Some("evidence from src/lib.rs".to_string())
        );
    }

    #[test]
    fn test_build_l0_derived_from_with_wire_uuid_lookup() {
        let node = make_node("L0-001", 0);
        let evidence = vec![make_evidence("src/main.rs", "L0-001", 1.0)];
        let mut id_map = HashMap::new();
        id_map.insert(
            "src/main.rs".to_string(),
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_string(),
        );

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
        assert!(
            entries[0].ref_path.contains('-'),
            "ref_path should be UUID-formatted"
        );
        assert_eq!(
            entries[0].justification,
            Some("L0 extraction from source file".to_string())
        );
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
            build_id: None,
            live: Some(true),
        }];
        let id_map = HashMap::new();

        let entries = build_l0_derived_from(&node, &evidence, &id_map);
        assert_eq!(entries.len(), 1);
        // Issue 4: justification defaults to non-empty string
        assert_eq!(
            entries[0].justification,
            Some("Evidence-based citation".to_string())
        );
    }

    #[test]
    fn test_build_upper_derived_from() {
        let evidence = vec![
            make_evidence("L0-001", "L1-001", 0.6),
            make_evidence("L0-002", "L1-001", 0.4),
            make_evidence("L0-orphan", "L1-001", 0.1), // not in id_maps
        ];

        let mut slug_map = HashMap::new();
        slug_map.insert("L0-001".to_string(), "uuid-for-L0-001".to_string());
        slug_map.insert("L0-002".to_string(), "uuid-for-L0-002".to_string());
        let mut id_maps = HashMap::new();
        id_maps.insert("test".to_string(), slug_map);

        let entries = build_upper_derived_from(&evidence, &id_maps, "test");
        assert_eq!(entries.len(), 2); // orphan source skipped
        assert_eq!(entries[0].source_type, "contribution");
        assert_eq!(entries[0].ref_path, "uuid-for-L0-001");
        assert_eq!(entries[1].ref_path, "uuid-for-L0-002");
    }

    #[test]
    fn test_build_upper_derived_from_cross_slug() {
        // Evidence with cross-slug handle-path source_node_id
        let evidence = vec![
            make_evidence("L0-001", "L1-001", 0.5), // same-slug bare id
            make_evidence("other-slug/0/L0-X01", "L1-001", 0.3), // cross-slug handle-path
            make_evidence("missing-slug/0/L0-Y01", "L1-001", 0.2), // cross-slug, no mapping
        ];

        let mut current_map = HashMap::new();
        current_map.insert("L0-001".to_string(), "uuid-current-001".to_string());
        let mut other_map = HashMap::new();
        other_map.insert("L0-X01".to_string(), "uuid-other-X01".to_string());

        let mut id_maps = HashMap::new();
        id_maps.insert("test".to_string(), current_map);
        id_maps.insert("other-slug".to_string(), other_map);

        let entries = build_upper_derived_from(&evidence, &id_maps, "test");
        assert_eq!(entries.len(), 2); // missing-slug ref skipped
        assert_eq!(entries[0].ref_path, "uuid-current-001");
        assert_eq!(entries[1].ref_path, "uuid-other-X01");
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
            build_id: None,
            live: Some(true),
        }];

        let mut slug_map = HashMap::new();
        slug_map.insert("L0-001".to_string(), "uuid-for-L0-001".to_string());
        let mut id_maps = HashMap::new();
        id_maps.insert("test".to_string(), slug_map);

        let entries = build_upper_derived_from(&evidence, &id_maps, "test");
        assert_eq!(entries.len(), 1);
        // Issue 4: justification defaults to non-empty string
        assert_eq!(
            entries[0].justification,
            Some("Evidence-based citation".to_string())
        );
    }

    #[test]
    fn test_make_placeholder_uuid_format() {
        let uuid = make_placeholder_uuid("C-L0-003");
        // Should be a valid UUID v5 (36 chars, version nibble = 5, variant bits correct)
        assert_eq!(uuid.len(), 36, "standard UUID length");
        let parsed = uuid::Uuid::parse_str(&uuid).expect("should be a valid UUID");
        assert_eq!(parsed.get_version_num(), 5, "should be UUID v5");
    }

    #[test]
    fn test_make_placeholder_uuid_deterministic() {
        let a = make_placeholder_uuid("same-input");
        let b = make_placeholder_uuid("same-input");
        assert_eq!(a, b, "placeholder UUID should be deterministic");
    }

    #[test]
    fn test_make_placeholder_uuid_stable_across_versions() {
        // Pin known outputs so any accidental change to the namespace or
        // algorithm is caught immediately. UUID v5 is SHA-1 based and stable
        // across all Rust versions and platforms.
        assert_eq!(
            make_placeholder_uuid("C-L0-003"),
            "e74baebc-cabc-5ec8-b9bb-86e16cb1b663",
            "output must not change across builds"
        );
        assert_eq!(
            make_placeholder_uuid("same-input"),
            "822de58f-e0c1-535f-b5aa-8c69c550800b",
            "output must not change across builds"
        );
        // Different inputs produce different UUIDs
        assert_ne!(
            make_placeholder_uuid("C-L0-003"),
            make_placeholder_uuid("C-L0-004"),
            "different inputs must produce different UUIDs"
        );
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
        let mut entries = vec![DerivedFromEntry {
            ref_path: "a".to_string(),
            source_type: "contribution".to_string(),
            weight: 0.0,
            justification: None,
        }];
        normalize_weights(&mut entries);
        // Should leave as-is (no division by zero)
        assert_eq!(entries[0].weight, 0.0);
    }

    #[test]
    fn test_build_manifest() {
        let manifest = build_manifest(
            "my-slug",
            1,
            &["A".to_string(), "B".to_string(), "C".to_string()],
            &["B".to_string()],
        );

        assert_eq!(manifest.slug, "my-slug");
        assert_eq!(manifest.layer, 1);
        assert_eq!(manifest.nodes_to_publish, vec!["A", "C"]);
        assert_eq!(manifest.skipped_orphans, vec!["B"]);
    }
}
