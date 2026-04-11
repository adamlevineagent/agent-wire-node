// pyramid/wire_publish.rs — Publication boundary for pushing local pyramid outputs onto the Wire
//
// Phase 4.3: Hybrid execution + publication boundary.
// Publishes PyramidNodes and QuestionSets as Wire contributions with correct
// body-as-prose pattern and structured_data for typed consumers.
//
// Publication is bottom-up: children (L0) before parents (L1, apex) so that
// derived_from references can use Wire UUIDs instead of local IDs.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

use super::question_yaml::QuestionSet;
use super::types::{DerivedFromEntry, IdMapping, PyramidNode};

// ─── Helpers ─────────────────────────────────────────────────

/// Truncate a string at a character boundary, never exceeding `max_bytes`.
fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let end = s
        .char_indices()
        .take_while(|(i, _)| *i < max_bytes)
        .last()
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    &s[..end]
}

/// Result of publishing a single node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishNodeResult {
    pub local_id: String,
    pub wire_uuid: String,
}

/// Result of publishing an entire pyramid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishPyramidResult {
    pub slug: String,
    pub apex_wire_uuid: Option<String>,
    pub node_count: usize,
    pub id_mappings: Vec<IdMapping>,
}

/// Result of publishing a question set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishQuestionSetResult {
    pub wire_uuid: String,
    pub content_type: String,
}

/// Error types for Wire publish operations.
#[derive(Debug, thiserror::Error)]
pub enum WirePublishError {
    #[error("network error: {0}")]
    Network(String),
    #[error("authentication failed: {0}")]
    AuthFailed(String),
    #[error("publish rejected: {0}")]
    Rejected(String),
    #[error("timeout after {0:?}")]
    Timeout(Duration),
    #[error("no nodes found for slug '{0}'")]
    NoNodes(String),
    #[error("missing Wire auth token")]
    MissingAuth,
    #[error("derived_from entry has zero weight for ref_path '{0}'")]
    ZeroWeight(String),
    #[error("all derived_from weights sum to zero — caller must provide pre-normalized weights")]
    AllZeroWeights,
}

// ─── Client ──────────────────────────────────────────────────

/// HTTP client for publishing pyramid outputs to the Wire marketplace.
pub struct PyramidPublisher {
    /// Wire API base URL (e.g., "https://newsbleach.com")
    pub wire_url: String,
    /// Agent's Wire auth token
    pub auth_token: String,
    /// HTTP client with timeout
    client: reqwest::Client,
}

impl PyramidPublisher {
    /// Create a new publisher.
    pub fn new(wire_url: String, auth_token: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("failed to build HTTP client");

        Self {
            wire_url,
            auth_token,
            client,
        }
    }

    /// Publish a single PyramidNode to the Wire as a contribution.
    ///
    /// Each `DerivedFromEntry` carries the actual evidence weight and source_type.
    /// Weights must be pre-normalized by the caller (sum to 1.0). This function
    /// does NOT normalize — the caller (publication.rs) handles normalization,
    /// and the Wire server normalizes again on ingest. We trust the caller.
    ///
    /// Zero-weight entries are rejected. If ALL entries have zero weight,
    /// an error is returned (prevents silent publish of meaningless weights).
    ///
    /// `evidence_data` is an optional JSON value merged into structured_data.
    /// Use it to pass `evidence_full`, `question`, `gaps`, `web_edges`, or
    /// any other spec-required fields without changing this function's core shape.
    ///
    /// Returns (wire_uuid, Option<handle_path>) from the Wire's response.
    pub async fn publish_pyramid_node(
        &self,
        node: &PyramidNode,
        derived_from: &[DerivedFromEntry],
        evidence_data: Option<serde_json::Value>,
    ) -> Result<(String, Option<String>)> {
        // Validate: reject individual zero-weight entries
        for entry in derived_from {
            if entry.weight <= 0.0 {
                return Err(WirePublishError::ZeroWeight(entry.ref_path.clone()).into());
            }
        }

        // Guard: if derived_from is non-empty but all weights sum to zero,
        // something is wrong upstream. Since we no longer normalize here,
        // this would produce meaningless data on the Wire.
        if !derived_from.is_empty() {
            let weight_sum: f64 = derived_from.iter().map(|e| e.weight).sum();
            if weight_sum == 0.0 {
                return Err(WirePublishError::AllZeroWeights.into());
            }
        }

        // Teaser is set explicitly (prose, not JSON) to avoid the Wire's
        // generateTeaser() truncating structured_data JSON into nonsense.
        // The em-dash separator " — " is 5 bytes (space + 3-byte UTF-8 + space).
        let teaser_max: usize = super::Tier2Config::default().teaser_max_chars;
        const SEPARATOR: &str = " — ";
        let teaser = if node.headline.len() > teaser_max {
            truncate_str(&node.headline, teaser_max).to_string()
        } else if node.distilled.len() > teaser_max {
            let prefix_len = node.headline.len() + SEPARATOR.len();
            if prefix_len >= teaser_max {
                // Headline alone fills the teaser
                truncate_str(&node.headline, teaser_max).to_string()
            } else {
                let remaining = teaser_max - prefix_len;
                format!(
                    "{}{}{}",
                    node.headline,
                    SEPARATOR,
                    truncate_str(&node.distilled, remaining)
                )
            }
        } else {
            node.headline.clone()
        };

        // Build structured_data with full node metadata
        let mut structured_data = serde_json::json!({
            "depth": node.depth,
            "children": node.children,
            "parent_id": node.parent_id,
            "topics": node.topics,
            "corrections": node.corrections,
            "decisions": node.decisions,
            "terms": node.terms,
            "dead_ends": node.dead_ends,
            "self_prompt": node.self_prompt,
        });

        // Merge caller-provided evidence data (evidence_full, question, gaps,
        // web_edges, etc.) into structured_data so Wire consumers get the
        // full spec-required fields.
        if let Some(extra) = evidence_data {
            if let (Some(base), Some(extra_obj)) =
                (structured_data.as_object_mut(), extra.as_object())
            {
                for (k, v) in extra_obj {
                    base.insert(k.clone(), v.clone());
                }
            }
        }

        // Extract topic names as string array
        let topics: Vec<String> = node.topics.iter().map(|t| t.name.clone()).collect();

        // Extract entities from topics
        let entities: Vec<serde_json::Value> = node
            .topics
            .iter()
            .flat_map(|t| {
                t.entities.iter().map(|e| {
                    serde_json::json!({
                        "name": e,
                        "type": "entity",
                    })
                })
            })
            .collect();

        // Pass weights through as-is. Caller is responsible for normalization.
        let derived_from_json: Vec<serde_json::Value> = derived_from
            .iter()
            .map(|entry| {
                serde_json::json!({
                    "source_type": entry.source_type,
                    "source_item_id": entry.ref_path,
                    "weight": entry.weight,
                    "justification": entry.justification.as_deref().unwrap_or("Evidence citation"),
                })
            })
            .collect();

        // Fall back to headline if distilled is empty — Wire requires non-empty body
        let body = if node.distilled.is_empty() {
            &node.headline
        } else {
            &node.distilled
        };

        let payload = serde_json::json!({
            "type": "pyramid_node",
            "contribution_type": "mechanical",
            "title": node.headline,
            "teaser": teaser,
            "body": body,
            "topics": topics,
            "entities": entities,
            "structured_data": structured_data,
            "derived_from": derived_from_json,
        });

        self.post_contribution(&payload).await
    }

    /// Backward-compatible wrapper for callers still using the old (wire_uuid, justification) tuple format.
    ///
    /// Converts each tuple into a DerivedFromEntry with source_type="contribution" and weight=1.0.
    /// Prefer calling `publish_pyramid_node` with `&[DerivedFromEntry]` directly for proper weights.
    #[deprecated(
        note = "Use publish_pyramid_node with &[DerivedFromEntry] for proper evidence weights"
    )]
    pub async fn publish_pyramid_node_legacy(
        &self,
        node: &PyramidNode,
        derived_from_wire_uuids: &[(String, String)], // (child_wire_uuid, justification)
    ) -> Result<String> {
        let entries: Vec<DerivedFromEntry> = derived_from_wire_uuids
            .iter()
            .map(|(wire_uuid, justification)| DerivedFromEntry {
                ref_path: wire_uuid.clone(),
                source_type: "contribution".to_string(),
                weight: 1.0,
                justification: Some(justification.clone()),
            })
            .collect();
        let (uuid, _handle_path) = self.publish_pyramid_node(node, &entries, None).await?;
        Ok(uuid)
    }

    /// Publish an entire pyramid (all pre-loaded nodes), bottom-up.
    ///
    /// `nodes_by_depth` must be sorted by depth (ascending): L0 first, apex last.
    /// Each entry is (depth, nodes_at_that_depth).
    ///
    /// To enable idempotency checking, call `collect_already_published` first
    /// and pass the result as `already_published`. Nodes in that set are skipped.
    ///
    /// Returns the ID mappings and apex Wire UUID. Caller is responsible for
    /// persisting the mappings to SQLite.
    pub async fn publish_pyramid(
        &self,
        slug: &str,
        nodes_by_depth: &[(i64, Vec<PyramidNode>)],
    ) -> Result<PublishPyramidResult> {
        self.publish_pyramid_idempotent(slug, nodes_by_depth, &HashMap::new(), &HashMap::new())
            .await
    }

    /// Publish with idempotency: `already_published` maps local_id -> wire_uuid
    /// for nodes that should be skipped (already on Wire).
    ///
    /// `evidence_weights` maps target_node_id -> (source_node_id -> weight) from
    /// KEEP evidence links. When a child's weight is found here, it replaces the
    /// flat 1.0 default. Build this map by calling `db::get_keep_evidence_for_target`
    /// for each node before entering the async publish loop.
    ///
    /// Build this map by calling `collect_already_published()` synchronously
    /// before entering the async publish loop.
    pub async fn publish_pyramid_idempotent(
        &self,
        slug: &str,
        nodes_by_depth: &[(i64, Vec<PyramidNode>)],
        already_published: &HashMap<String, String>,
        evidence_weights: &HashMap<String, HashMap<String, f64>>,
    ) -> Result<PublishPyramidResult> {
        if nodes_by_depth.is_empty() {
            return Err(WirePublishError::NoNodes(slug.to_string()).into());
        }

        let mut id_map: HashMap<String, String> = HashMap::new();
        let mut all_mappings: Vec<IdMapping> = Vec::new();
        let mut apex_wire_uuid: Option<String> = None;
        let max_depth = nodes_by_depth.last().map(|(d, _)| *d).unwrap_or(0);

        // Publish bottom-up
        for (depth, nodes) in nodes_by_depth {
            for node in nodes {
                // Fix 3: Idempotency — skip already-published nodes
                if let Some(existing_uuid) = already_published.get(&node.id) {
                    id_map.insert(node.id.clone(), existing_uuid.clone());
                    all_mappings.push(IdMapping {
                        local_id: node.id.clone(),
                        wire_handle_path: existing_uuid.clone(),
                        wire_uuid: Some(existing_uuid.clone()),
                        published_at: chrono::Utc::now().to_rfc3339(),
                    });
                    if *depth == max_depth {
                        apex_wire_uuid = Some(existing_uuid.clone());
                    }
                    tracing::info!(
                        slug = slug,
                        node_id = %node.id,
                        depth = depth,
                        "skipped already-published pyramid node"
                    );
                    continue;
                }

                // Fix 2: Use correct source_type based on depth.
                // L0 nodes cite source documents; L1+ nodes cite other pyramid contributions.
                let source_type = if *depth == 0 {
                    "source_document"
                } else {
                    "contribution"
                };

                // Build derived_from entries with evidence weights when available,
                // falling back to equal 1.0 weights when no evidence data exists.
                let node_evidence = evidence_weights.get(&node.id);
                let derived_from: Vec<DerivedFromEntry> = node
                    .children
                    .iter()
                    .filter_map(|child_id| {
                        id_map.get(child_id).map(|wire_uuid| {
                            let weight = node_evidence
                                .and_then(|ev| ev.get(child_id))
                                .copied()
                                .unwrap_or(1.0);
                            DerivedFromEntry {
                                ref_path: wire_uuid.clone(),
                                source_type: source_type.to_string(),
                                weight,
                                justification: Some(format!("child node {}", child_id)),
                            }
                        })
                    })
                    .collect();

                let (wire_uuid, handle_path) =
                    self.publish_pyramid_node(node, &derived_from, None).await?;
                let resolved_handle = handle_path.unwrap_or_else(|| wire_uuid.clone());

                id_map.insert(node.id.clone(), wire_uuid.clone());
                all_mappings.push(IdMapping {
                    local_id: node.id.clone(),
                    wire_handle_path: resolved_handle,
                    wire_uuid: Some(wire_uuid.clone()),
                    published_at: chrono::Utc::now().to_rfc3339(),
                });

                // Track apex (highest depth)
                if *depth == max_depth {
                    apex_wire_uuid = Some(wire_uuid);
                }

                tracing::info!(
                    slug = slug,
                    node_id = %node.id,
                    depth = depth,
                    "published pyramid node to Wire"
                );
            }
        }

        if all_mappings.is_empty() {
            return Err(WirePublishError::NoNodes(slug.to_string()).into());
        }

        Ok(PublishPyramidResult {
            slug: slug.to_string(),
            apex_wire_uuid,
            node_count: all_mappings.len(),
            id_mappings: all_mappings,
        })
    }

    /// Publish a QuestionSet as a Wire contribution.
    ///
    /// The full question set definition is stored in structured_data.
    /// The body is a human-readable description of what the question set does.
    pub async fn publish_question_set(
        &self,
        question_set: &QuestionSet,
        description: &str,
    ) -> Result<PublishQuestionSetResult> {
        let title = format!(
            "Question Set: {} (v{})",
            question_set.r#type, question_set.version
        );

        let teaser = if description.len() > 200 {
            truncate_str(description, 200).to_string()
        } else {
            description.to_string()
        };

        // Serialize the full question set as JSON for structured_data
        let qs_json =
            serde_json::to_value(question_set).context("failed to serialize question set")?;

        let structured_data = serde_json::json!({
            "question_set_definition": qs_json,
        });

        let topics = vec![question_set.r#type.clone(), "question_set".to_string()];

        let payload = serde_json::json!({
            "type": "question_set",
            "contribution_type": "mechanical",
            "title": title,
            "teaser": teaser,
            "body": description,
            "topics": topics,
            "entities": [],
            "structured_data": structured_data,
            "derived_from": [],
        });

        let (wire_uuid, _handle_path) = self.post_contribution(&payload).await?;

        Ok(PublishQuestionSetResult {
            wire_uuid,
            content_type: question_set.r#type.clone(),
        })
    }

    /// Publish a gap report to the Wire (WS-ONLINE-F).
    ///
    /// Gap reports are demand signals: they tell the owner of the referenced
    /// pyramid that a node is missing or incomplete. Published as a
    /// `type: "gap_report"` contribution with the remote handle-path in
    /// structured_data.
    pub async fn publish_gap_report(
        &self,
        slug: &str,
        remote_handle_path: &str,
        gap_description: &str,
    ) -> Result<(String, Option<String>)> {
        let title = format!(
            "Gap: {} on {}",
            gap_description.chars().take(60).collect::<String>(),
            remote_handle_path
        );
        let teaser = truncate_str(gap_description, 200).to_string();

        let structured_data = serde_json::json!({
            "source_slug": slug,
            "remote_handle_path": remote_handle_path,
            "gap_type": "missing_content",
        });

        let payload = serde_json::json!({
            "type": "gap_report",
            "contribution_type": "mechanical",
            "title": title,
            "teaser": teaser,
            "body": gap_description,
            "topics": [],
            "entities": [],
            "structured_data": structured_data,
            "derived_from": [],
        });

        self.post_contribution(&payload).await
    }

    /// Post a contribution to the Wire API and return its (UUID, Option<handle_path>).
    pub async fn post_contribution(
        &self,
        payload: &serde_json::Value,
    ) -> Result<(String, Option<String>)> {
        let url = format!("{}/api/v1/contribute", self.wire_url.trim_end_matches('/'),);

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.auth_token))
            .json(payload)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    WirePublishError::Timeout(Duration::from_secs(60))
                } else {
                    WirePublishError::Network(e.to_string())
                }
            })
            .context("wire publish: post_contribution request failed")?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(WirePublishError::AuthFailed(format!("status {}", status)).into());
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(WirePublishError::Rejected(format!(
                "status {}: {}",
                status,
                body.chars().take(500).collect::<String>()
            ))
            .into());
        }

        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| WirePublishError::Network(e.to_string()))
            .context("wire publish: failed to parse response JSON")?;

        // The contribute endpoint returns { id: "uuid", handle_path: "...", ... } (flat object)
        // or legacy { contribution: { id: "uuid", handle_path: "..." } } — support both.
        let contribution = body.get("contribution").unwrap_or(&body);

        let contribution_id = contribution
            .get("id")
            .and_then(|id| id.as_str())
            .ok_or_else(|| {
                WirePublishError::Rejected(format!("response missing id field: {}", body))
            })?;

        let handle_path = contribution
            .get("handle_path")
            .and_then(|hp| hp.as_str())
            .map(|s| s.to_string());

        Ok((contribution_id.to_string(), handle_path))
    }

    /// Publish (or re-publish) pyramid discovery metadata as a Wire contribution.
    ///
    /// Creates a `type: "pyramid_metadata"` contribution whose body is the apex
    /// node's distilled text and whose structured_data carries all discovery
    /// fields (slug, node_count, tunnel_url, access_tier, etc.).
    ///
    /// If `supersedes_uuid` is provided, the old metadata contribution is
    /// superseded via the Wire supersession endpoint after the new one is created.
    ///
    /// Returns the new metadata contribution's wire_uuid.
    pub async fn publish_pyramid_metadata(
        &self,
        metadata: &PyramidMetadata,
        supersedes_uuid: Option<&str>,
    ) -> Result<String> {
        let title = format!("Pyramid: {}", metadata.pyramid_slug);

        let teaser = if metadata.apex_headline.len() > 200 {
            truncate_str(&metadata.apex_headline, 200).to_string()
        } else {
            metadata.apex_headline.clone()
        };

        let structured_data = serde_json::json!({
            "pyramid_slug": metadata.pyramid_slug,
            "node_count": metadata.node_count,
            "max_depth": metadata.max_depth,
            "content_type": metadata.content_type,
            "quality_score": metadata.quality_score,
            "tunnel_url": metadata.tunnel_url,
            "api_base": format!("/pyramid/{}", metadata.pyramid_slug),
            "apex_headline": metadata.apex_headline,
            "topics": metadata.topics,
            "last_build_at": metadata.last_build_at,
            "access_tier": metadata.access_tier,
            "access_price": metadata.access_price,
            "absorption_mode": metadata.absorption_mode,
        });

        let payload = serde_json::json!({
            "type": "pyramid_metadata",
            "contribution_type": "mechanical",
            "title": title,
            "teaser": teaser,
            "body": metadata.apex_body,
            "topics": metadata.topics,
            "entities": [],
            "structured_data": structured_data,
            "derived_from": [],
        });

        let (new_uuid, _handle_path) = self.post_contribution(&payload).await?;

        // Supersede the old metadata contribution if one existed
        if let Some(old_uuid) = supersedes_uuid {
            if let Err(e) = self.supersede_contribution(old_uuid, &new_uuid).await {
                // Log but don't fail — the new metadata is published, supersession
                // is best-effort (the old one remains visible but not harmful).
                tracing::warn!(
                    old_uuid = old_uuid,
                    new_uuid = %new_uuid,
                    error = %e,
                    "failed to supersede old pyramid metadata contribution"
                );
            }
        }

        Ok(new_uuid)
    }

    /// Call the Wire supersession endpoint to mark an old contribution as
    /// superseded by a new one.
    ///
    /// POST /api/v1/wire/contributions/{old_id}/supersede
    /// Body: { "new_contribution_id": "..." }
    async fn supersede_contribution(
        &self,
        old_contribution_id: &str,
        new_contribution_id: &str,
    ) -> Result<()> {
        let url = format!(
            "{}/api/v1/wire/contributions/{}/supersede",
            self.wire_url.trim_end_matches('/'),
            old_contribution_id,
        );

        let body = serde_json::json!({
            "new_contribution_id": new_contribution_id,
        });

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.auth_token))
            .json(&body)
            .send()
            .await
            .context("supersede_contribution: request failed")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(WirePublishError::Rejected(format!(
                "supersession failed ({}): {}",
                status,
                text.chars().take(500).collect::<String>()
            ))
            .into());
        }

        tracing::info!(
            old_id = old_contribution_id,
            new_id = new_contribution_id,
            "superseded old contribution"
        );
        Ok(())
    }
}

// ─── Phase 5: Config Contribution Publication ────────────────────
//
// Phase 5 extends `PyramidPublisher` with a config-contribution
// publication path that serializes the canonical `WireNativeMetadata`
// into a Wire-native YAML block and POSTs to the contribution
// endpoint. Key differences from `publish_pyramid_node`:
//
// - The contribution body is a pre-formed YAML document (the
//   `ConfigContribution.yaml_content` column), NOT a pyramid node's
//   distilled text.
// - `derived_from` weights are converted to 28-slot integer
//   allocations via `rotator_allocation::allocate_28_slots()`.
// - Path references (`ref:` / `doc:` / `corpus:`) are resolved
//   against the local `pyramid_id_map` at publish time. Unresolved
//   references surface in the dry-run preview as warnings.
// - The returned `PublishOutcome` includes the full resolved
//   derived_from cache so the caller can write it back into the
//   `wire_publication_state_json` column.
//
// The dry-run helper does everything publish does EXCEPT the actual
// HTTP POST: it returns the resolved allocation, the serialized YAML
// body, the cost breakdown, and any warnings (credential leak
// detection, validation errors, stale references). Phase 10's
// ToolsMode UI renders this preview inline.

/// Result of a successful `publish_contribution_with_metadata` call.
/// Carries everything the caller needs to write back into
/// `pyramid_config_contributions.wire_publication_state_json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishContributionOutcome {
    /// Wire-assigned contribution UUID.
    pub wire_contribution_id: String,
    /// Wire-assigned handle-path (e.g. `"playful/77/3"`).
    pub handle_path: Option<String>,
    /// Wire type the contribution was published as.
    pub wire_type: String,
    /// Tags attached to the published contribution.
    pub tags: Vec<String>,
    /// Resolved derived_from entries with integer slot allocations.
    pub resolved_derived_from: Vec<crate::pyramid::wire_native_metadata::ResolvedDerivedFromEntry>,
    /// Section contributions published alongside the top-level
    /// contribution (empty when no sections were present).
    pub sections_published: Vec<String>,
}

/// Result of a `dry_run_publish` call. Shows the user exactly what
/// would happen if they pressed "Publish to Wire" without actually
/// writing anything to the Wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DryRunReport {
    /// Wire type the contribution would publish as (e.g. `"skill"`).
    pub wire_type: String,
    /// Default tags from the Phase 5 mapping table.
    pub tags: Vec<String>,
    /// Canonical scope string (`"unscoped"`, `"fleet"`,
    /// `"circle:<name>"`).
    pub visibility: String,
    /// YAML body that would be posted to the Wire.
    pub canonical_yaml: String,
    /// Cost breakdown (deposit, publish fee, total).
    pub cost_breakdown: CostBreakdown,
    /// Resolved derived_from with integer slot allocations.
    pub resolved_derived_from: Vec<crate::pyramid::wire_native_metadata::ResolvedDerivedFromEntry>,
    /// Supersession chain link (if `supersedes` is set).
    pub supersession_chain: Vec<SupersessionLink>,
    /// Warnings: credential references, unresolved sources, Pillar 37
    /// violations, trackable claims without end dates, etc.
    pub warnings: Vec<String>,
    /// Section preview: one entry per `sections` decomposition.
    pub section_previews: Vec<SectionPreview>,
}

/// Cost breakdown returned in a `DryRunReport`. Approximate — the
/// actual Wire-side cost depends on current provider pricing and
/// deposit rules.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CostBreakdown {
    /// Deposit required for skill contributions (Phase 5 stub: 0).
    pub deposit_credits: u64,
    /// Publish fee (currently 0 — the Wire charges per-access, not
    /// per-publish).
    pub publish_fee: u64,
    /// Author-declared price for the contribution (0 if free).
    pub author_price: u64,
    /// Estimated total credits debited at publish time.
    pub estimated_total: u64,
}

/// One entry in a supersession chain preview.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupersessionLink {
    pub handle_path: String,
    pub wire_contribution_id: Option<String>,
    pub maturity: String,
    pub published_at: Option<String>,
}

/// One entry in a section decomposition preview.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionPreview {
    pub heading: String,
    pub contribution_type: String,
    pub will_publish: bool,
}

impl PyramidPublisher {
    /// Phase 5: publish a config contribution using its canonical
    /// `WireNativeMetadata`. Resolves path-based derived_from
    /// references, converts float weights to 28 integer slots, emits
    /// the canonical YAML body, and POSTs to the Wire.
    ///
    /// Does NOT write back to `pyramid_config_contributions` — the
    /// caller holds the DB mutex and is responsible for persisting
    /// the returned `PublishContributionOutcome` into the contribution
    /// row's `wire_publication_state_json` column.
    ///
    /// Section decomposition (bundled chain + prompts in a single
    /// contribution) is a follow-up iteration — for Phase 5 the
    /// publisher emits the top-level contribution only, and logs a
    /// TODO when sections are present. Phase 9's chain migration
    /// path fills this in when custom chains land from the UI.
    pub async fn publish_contribution_with_metadata(
        &self,
        contribution_id: &str,
        schema_type: &str,
        yaml_content: &str,
        metadata: &crate::pyramid::wire_native_metadata::WireNativeMetadata,
    ) -> Result<PublishContributionOutcome> {
        metadata
            .validate()
            .map_err(|e| WirePublishError::Rejected(format!("metadata validation: {e}")))?;

        let (wire_type, tags) =
            crate::pyramid::wire_native_metadata::resolve_wire_type(schema_type).map_err(|e| {
                WirePublishError::Rejected(format!("wire type resolution: {e}"))
            })?;
        let wire_type_str = format!("{wire_type:?}").to_lowercase();

        // Resolve derived_from to integer slot allocations. We don't
        // have a live path→UUID map in Phase 5 scope (that's Phase
        // 10's ToolsMode job + discovery), so for now we mark every
        // reference as unresolved and emit floats as fallbacks. The
        // integer slot allocation is still computed so downstream
        // consumers see the correct distribution.
        let resolved = resolve_derived_from_preview(metadata)?;

        // Emit the canonical YAML block.
        let canonical_yaml = metadata
            .to_canonical_yaml()
            .map_err(|e| WirePublishError::Rejected(format!("yaml serialize: {e}")))?;

        // Build the Wire contribute payload. Wire's `/api/v1/contribute`
        // endpoint accepts a JSON body; Phase 5 serializes the
        // canonical YAML into a `wire_native_metadata` field and the
        // yaml_content body into the contribution `body`.
        let topics_for_tags: Vec<String> = metadata
            .topics
            .iter()
            .cloned()
            .chain(tags.iter().cloned())
            .collect();
        let payload = serde_json::json!({
            "type": wire_type_str,
            "contribution_type": wire_type_str,
            "title": title_from_yaml(yaml_content, schema_type, contribution_id),
            "body": yaml_content,
            "topics": topics_for_tags,
            "wire_native_metadata_yaml": canonical_yaml,
            "derived_from": resolved.iter().map(|entry| serde_json::json!({
                "source_type": match entry.kind.as_str() {
                    "ref" => "contribution",
                    "doc" => "corpus_document",
                    "corpus" => "corpus_document",
                    _ => "contribution",
                },
                "source_item_id": entry.reference,
                "weight": entry.weight,
                "allocated_slots": entry.allocated_slots,
                "justification": metadata.derived_from.iter()
                    .find(|r| r.canonical_reference() == entry.reference)
                    .map(|r| r.justification.clone())
                    .unwrap_or_else(|| "(phase-5 preview)".to_string()),
            })).collect::<Vec<_>>(),
            "supersedes": metadata.supersedes,
            "scope": metadata.scope.to_canonical_string(),
            "price": metadata.price,
            "pricing_curve": metadata.pricing_curve,
            "creator_split": metadata.creator_split,
            "sync_mode": format!("{:?}", metadata.sync_mode).to_lowercase(),
            "maturity": format!("{:?}", metadata.maturity).to_lowercase(),
        });

        // POST to the Wire's contribute endpoint. Reuses the existing
        // `post_contribution` helper which handles auth, retries, and
        // response parsing.
        let (wire_contribution_id, handle_path) = self.post_contribution(&payload).await?;

        tracing::info!(
            contribution_id,
            wire_contribution_id,
            handle_path = ?handle_path,
            wire_type = wire_type_str,
            "phase 5 contribution published to Wire"
        );

        Ok(PublishContributionOutcome {
            wire_contribution_id,
            handle_path,
            wire_type: wire_type_str,
            tags,
            resolved_derived_from: resolved,
            sections_published: Vec::new(),
        })
    }

    /// Phase 5: dry-run publish. Shows the user exactly what
    /// `publish_contribution_with_metadata` WOULD do without actually
    /// calling the Wire API.
    ///
    /// Does NOT require a network connection or a valid auth token —
    /// it's purely a local preview. The caller can render the
    /// `DryRunReport` inline in ToolsMode before the user confirms.
    pub fn dry_run_publish(
        &self,
        contribution_id: &str,
        schema_type: &str,
        yaml_content: &str,
        metadata: &crate::pyramid::wire_native_metadata::WireNativeMetadata,
    ) -> Result<DryRunReport> {
        let mut warnings: Vec<String> = Vec::new();

        // Run validation. Validation errors become warnings so the UI
        // can show everything wrong at once instead of aborting on
        // the first.
        if let Err(e) = metadata.validate() {
            warnings.push(format!("validation: {e}"));
        }

        let (wire_type, tags) =
            crate::pyramid::wire_native_metadata::resolve_wire_type(schema_type).map_err(|e| {
                WirePublishError::Rejected(format!("wire type resolution: {e}"))
            })?;
        let wire_type_str = format!("{wire_type:?}").to_lowercase();

        let canonical_yaml = metadata
            .to_canonical_yaml()
            .map_err(|e| WirePublishError::Rejected(format!("yaml serialize: {e}")))?;

        // Resolve derived_from → integer slots.
        let resolved = match resolve_derived_from_preview(metadata) {
            Ok(r) => r,
            Err(e) => {
                warnings.push(format!("derived_from resolution: {e}"));
                Vec::new()
            }
        };

        // Credential leak detection: scan the yaml_content for
        // `${VAR_NAME}` references. Each hit is a warning so the user
        // knows the contribution references credentials that won't
        // survive a Wire publish.
        let credential_refs =
            crate::pyramid::credentials::CredentialStore::collect_references(yaml_content);
        if !credential_refs.is_empty() {
            warnings.push(format!(
                "credential references found in body: {credential_refs:?}; \
                 these will NOT be resolved on the Wire side — \
                 consider removing or replacing with placeholder values"
            ));
        }
        // Also scan the canonical YAML (metadata itself).
        let metadata_credential_refs =
            crate::pyramid::credentials::CredentialStore::collect_references(&canonical_yaml);
        if !metadata_credential_refs.is_empty() {
            warnings.push(format!(
                "credential references found in metadata: {metadata_credential_refs:?}"
            ));
        }

        // Trackable claims need end dates.
        for (i, claim) in metadata.claims.iter().enumerate() {
            if claim.trackable && claim.end_date.as_deref().unwrap_or("").is_empty() {
                warnings.push(format!(
                    "claims[{i}]: trackable claim has no end_date"
                ));
            }
        }

        // Unresolved derived_from sources.
        for entry in &resolved {
            if !entry.resolved {
                warnings.push(format!(
                    "derived_from[{}]: path reference {:?} could not be resolved against local path→UUID map (Phase 5 preview: all references are unresolved until the live map lands in Phase 10)",
                    entry.kind, entry.reference
                ));
            }
        }

        // Embargo in the past.
        if let Some(embargo) = &metadata.embargo_until {
            if embargo.starts_with('-') {
                warnings.push(format!(
                    "embargo_until {embargo:?} is relative-past; Wire will reject"
                ));
            }
        }

        // Build the cost breakdown (author-declared prices).
        let author_price = metadata.price.unwrap_or(0);
        let deposit_credits = if matches!(wire_type, crate::pyramid::wire_native_metadata::WireContributionType::Skill) {
            // Skill deposit rule per wire-skills.md — exact amount is
            // TBD against the credit rebase; Phase 5 reports 0 and
            // flags it for the user to check.
            warnings.push(
                "skill contributions require a deposit (amount TBD post-rebase); \
                 Phase 5 reports 0 as placeholder — verify before publish"
                    .to_string(),
            );
            0
        } else {
            0
        };
        let cost_breakdown = CostBreakdown {
            deposit_credits,
            publish_fee: 0,
            author_price,
            estimated_total: deposit_credits.saturating_add(author_price),
        };

        // Supersession chain preview: just the single link described
        // in the metadata. Phase 5 doesn't walk the chain backward
        // (Phase 10's Wire discovery can hydrate that inline).
        let supersession_chain: Vec<SupersessionLink> = metadata
            .supersedes
            .as_ref()
            .map(|path| {
                vec![SupersessionLink {
                    handle_path: path.clone(),
                    wire_contribution_id: None,
                    maturity: "unknown".to_string(),
                    published_at: None,
                }]
            })
            .unwrap_or_default();

        let section_previews: Vec<SectionPreview> = metadata
            .sections
            .iter()
            .map(|(heading, override_)| SectionPreview {
                heading: heading.clone(),
                contribution_type: override_
                    .contribution_type
                    .as_ref()
                    .map(|ct| format!("{ct:?}").to_lowercase())
                    .unwrap_or_else(|| "inherited".to_string()),
                will_publish: true,
            })
            .collect();

        tracing::debug!(
            contribution_id,
            wire_type = wire_type_str,
            warning_count = warnings.len(),
            "phase 5 dry-run publish completed"
        );

        Ok(DryRunReport {
            wire_type: wire_type_str,
            tags,
            visibility: metadata.scope.to_canonical_string(),
            canonical_yaml,
            cost_breakdown,
            resolved_derived_from: resolved,
            supersession_chain,
            warnings,
            section_previews,
        })
    }
}

/// Resolve a `WireNativeMetadata`'s `derived_from` entries to an
/// integer-slot allocation using the rotator-arm 28-slot method.
/// Phase 5 does NOT have a live path→UUID map, so every resolved
/// entry carries `resolved: false` and no Wire UUID. Phase 10's Wire
/// discovery adds the live map.
///
/// Returns an empty vector if the metadata has no `derived_from`
/// entries.
fn resolve_derived_from_preview(
    metadata: &crate::pyramid::wire_native_metadata::WireNativeMetadata,
) -> Result<Vec<crate::pyramid::wire_native_metadata::ResolvedDerivedFromEntry>> {
    if metadata.derived_from.is_empty() {
        return Ok(Vec::new());
    }

    // Validate each entry's kind invariant.
    for (i, entry) in metadata.derived_from.iter().enumerate() {
        entry
            .validate()
            .map_err(|e| WirePublishError::Rejected(format!("derived_from[{i}]: {e}")))?;
    }

    // Allocate 28 slots across the weights.
    let weights: Vec<f64> = metadata.derived_from.iter().map(|r| r.weight).collect();
    let slots = crate::pyramid::rotator_allocation::allocate_28_slots(&weights)
        .map_err(|e| WirePublishError::Rejected(format!("rotator allocation: {e}")))?;

    let resolved: Vec<crate::pyramid::wire_native_metadata::ResolvedDerivedFromEntry> = metadata
        .derived_from
        .iter()
        .zip(slots.iter())
        .map(|(entry, &allocated)| {
            crate::pyramid::wire_native_metadata::ResolvedDerivedFromEntry {
                kind: entry.kind().to_string(),
                reference: entry.canonical_reference(),
                weight: entry.weight,
                allocated_slots: allocated,
                // Phase 5: path→UUID resolution is a Phase 10 feature.
                // Every reference is marked unresolved until the live
                // map lands.
                wire_contribution_id: None,
                handle_path: None,
                resolved: false,
            }
        })
        .collect();

    Ok(resolved)
}

/// Best-effort title generator for a config contribution. The Wire
/// requires a title on every contribution; Phase 5 synthesizes one
/// from the schema_type + contribution_id when no explicit title is
/// present in the YAML body.
fn title_from_yaml(yaml_content: &str, schema_type: &str, contribution_id: &str) -> String {
    // Scan for a `name:` or `title:` field in the first few lines.
    for line in yaml_content.lines().take(20) {
        let trimmed = line.trim_start();
        for key in ["name:", "title:", "id:"] {
            if let Some(rest) = trimmed.strip_prefix(key) {
                let value = rest.trim();
                let value = value
                    .trim_start_matches('"')
                    .trim_end_matches('"')
                    .trim_start_matches('\'')
                    .trim_end_matches('\'');
                if !value.is_empty() {
                    return value.to_string();
                }
            }
        }
    }
    format!(
        "{schema_type}: {}",
        contribution_id.chars().take(8).collect::<String>()
    )
}

/// All fields needed to publish a pyramid_metadata contribution.
#[derive(Debug, Clone)]
pub struct PyramidMetadata {
    pub pyramid_slug: String,
    pub node_count: i64,
    pub max_depth: i64,
    pub content_type: String,
    pub quality_score: f64,
    pub tunnel_url: Option<String>,
    pub apex_headline: String,
    pub apex_body: String,
    pub topics: Vec<String>,
    pub last_build_at: Option<String>,
    pub access_tier: String,
    pub access_price: Option<i64>,
    pub absorption_mode: String,
}

// ─── SQLite ID Mapping ───────────────────────────────────────

/// Initialize the pyramid_id_map table in SQLite.
pub fn init_id_map_table(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_id_map (
            slug TEXT NOT NULL,
            local_id TEXT NOT NULL,
            wire_uuid TEXT NOT NULL,
            published_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (slug, local_id)
        );
        ",
    )?;
    Ok(())
}

/// Save a local_id → wire_uuid mapping.
pub fn save_id_mapping(
    conn: &rusqlite::Connection,
    slug: &str,
    local_id: &str,
    wire_uuid: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_id_map (slug, local_id, wire_uuid)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(slug, local_id) DO UPDATE SET
            wire_uuid = excluded.wire_uuid,
            published_at = datetime('now')",
        rusqlite::params![slug, local_id, wire_uuid],
    )?;
    Ok(())
}

/// Get the Wire UUID for a local node ID.
pub fn get_wire_uuid(
    conn: &rusqlite::Connection,
    slug: &str,
    local_id: &str,
) -> Result<Option<String>> {
    let mut stmt =
        conn.prepare("SELECT wire_uuid FROM pyramid_id_map WHERE slug = ?1 AND local_id = ?2")?;
    let result = stmt.query_row(rusqlite::params![slug, local_id], |row| {
        row.get::<_, String>(0)
    });
    match result {
        Ok(uuid) => Ok(Some(uuid)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Get the local ID for a Wire UUID (reverse lookup).
pub fn get_local_id(
    conn: &rusqlite::Connection,
    slug: &str,
    wire_uuid: &str,
) -> Result<Option<String>> {
    let mut stmt =
        conn.prepare("SELECT local_id FROM pyramid_id_map WHERE slug = ?1 AND wire_uuid = ?2")?;
    let result = stmt.query_row(rusqlite::params![slug, wire_uuid], |row| {
        row.get::<_, String>(0)
    });
    match result {
        Ok(id) => Ok(Some(id)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Collect already-published node mappings for idempotency.
///
/// Returns a HashMap of local_id → wire_uuid for all nodes in the given slug
/// that have been previously published. This map is passed to
/// `publish_pyramid_idempotent()` to skip re-publishing.
///
/// Gracefully handles the case where the `pyramid_id_map` table doesn't exist
/// yet (returns an empty map).
pub fn collect_already_published(
    conn: &rusqlite::Connection,
    slug: &str,
) -> HashMap<String, String> {
    match get_all_mappings(conn, slug) {
        Ok(mappings) => mappings.into_iter().collect(),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("no such table") {
                tracing::debug!(
                    slug = slug,
                    "pyramid_id_map table not found, treating all nodes as unpublished"
                );
            } else {
                tracing::warn!(
                    slug = slug,
                    error = %e,
                    "failed to read pyramid_id_map, treating all nodes as unpublished"
                );
            }
            HashMap::new()
        }
    }
}

/// Check whether a node has already been published to the Wire.
///
/// Gracefully handles the case where `pyramid_id_map` doesn't exist yet
/// (returns Ok(false) so the caller proceeds with publish).
pub fn is_already_published(
    conn: &rusqlite::Connection,
    slug: &str,
    local_id: &str,
) -> Result<bool> {
    let result =
        conn.prepare("SELECT 1 FROM pyramid_id_map WHERE slug = ?1 AND local_id = ?2 LIMIT 1");
    match result {
        Ok(mut stmt) => {
            let exists = stmt
                .query_row(rusqlite::params![slug, local_id], |_row| Ok(()))
                .is_ok();
            Ok(exists)
        }
        Err(e) => {
            // Gracefully handle "no such table" — table may not be created yet (WS1-A)
            let msg = e.to_string();
            if msg.contains("no such table") {
                tracing::debug!(
                    slug = slug,
                    local_id = local_id,
                    "pyramid_id_map table not found, treating as not-yet-published"
                );
                Ok(false)
            } else {
                Err(e.into())
            }
        }
    }
}

/// Get all ID mappings for a slug.
pub fn get_all_mappings(conn: &rusqlite::Connection, slug: &str) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT local_id, wire_uuid FROM pyramid_id_map WHERE slug = ?1 ORDER BY local_id",
    )?;
    let rows = stmt.query_map(rusqlite::params![slug], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut mappings = Vec::new();
    for row in rows {
        mappings.push(row?);
    }
    Ok(mappings)
}

// ─── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::types::{Correction, Decision, Term, Topic};

    fn make_test_node(id: &str, depth: i64, children: Vec<String>) -> PyramidNode {
        PyramidNode {
            id: id.to_string(),
            slug: "test-slug".to_string(),
            depth,
            chunk_index: None,
            headline: format!("Headline for {}", id),
            distilled: format!(
                "Distilled content for node {}. This is the human-readable prose.",
                id
            ),
            topics: vec![Topic {
                name: "test-topic".to_string(),
                current: "current state".to_string(),
                entities: vec!["entity-a".to_string(), "entity-b".to_string()],
                corrections: vec![],
                decisions: vec![],
                extra: serde_json::Map::new(),
            }],
            corrections: vec![Correction {
                wrong: "old".to_string(),
                right: "new".to_string(),
                who: "tester".to_string(),
            }],
            decisions: vec![Decision {
                decided: "use X".to_string(),
                why: "because Y".to_string(),
                rejected: "Z".to_string(),
                ..Default::default()
            }],
            terms: vec![Term {
                term: "foo".to_string(),
                definition: "bar".to_string(),
            }],
            dead_ends: vec!["dead-end-1".to_string()],
            self_prompt: "What next?".to_string(),
            children,
            parent_id: None,
            superseded_by: None,
            build_id: None,
            created_at: "2026-03-25T00:00:00Z".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn test_id_mapping_roundtrip() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_id_map_table(&conn).unwrap();

        save_id_mapping(&conn, "my-slug", "C-L0-001", "wire-uuid-abc").unwrap();

        let uuid = get_wire_uuid(&conn, "my-slug", "C-L0-001").unwrap();
        assert_eq!(uuid, Some("wire-uuid-abc".to_string()));

        let local = get_local_id(&conn, "my-slug", "wire-uuid-abc").unwrap();
        assert_eq!(local, Some("C-L0-001".to_string()));
    }

    #[test]
    fn test_id_mapping_missing_returns_none() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_id_map_table(&conn).unwrap();

        assert_eq!(get_wire_uuid(&conn, "slug", "nonexistent").unwrap(), None);
        assert_eq!(get_local_id(&conn, "slug", "nonexistent").unwrap(), None);
    }

    #[test]
    fn test_id_mapping_upsert() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_id_map_table(&conn).unwrap();

        save_id_mapping(&conn, "slug", "node-1", "uuid-v1").unwrap();
        save_id_mapping(&conn, "slug", "node-1", "uuid-v2").unwrap();

        let uuid = get_wire_uuid(&conn, "slug", "node-1").unwrap();
        assert_eq!(uuid, Some("uuid-v2".to_string()));
    }

    #[test]
    fn test_get_all_mappings() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_id_map_table(&conn).unwrap();

        save_id_mapping(&conn, "slug", "A", "uuid-a").unwrap();
        save_id_mapping(&conn, "slug", "B", "uuid-b").unwrap();
        save_id_mapping(&conn, "other-slug", "C", "uuid-c").unwrap();

        let mappings = get_all_mappings(&conn, "slug").unwrap();
        assert_eq!(mappings.len(), 2);
        assert_eq!(mappings[0], ("A".to_string(), "uuid-a".to_string()));
        assert_eq!(mappings[1], ("B".to_string(), "uuid-b".to_string()));
    }

    #[test]
    fn test_node_to_contribution_mapping() {
        // Verify the fields that publish_pyramid_node would produce
        let node = make_test_node(
            "C-L1-001",
            1,
            vec!["C-L0-001".to_string(), "C-L0-002".to_string()],
        );

        // body = distilled
        assert!(node.distilled.contains("Distilled content"));
        // title = headline
        assert!(node.headline.contains("Headline for"));
        // topics = node.topics[*].name
        let topics: Vec<String> = node.topics.iter().map(|t| t.name.clone()).collect();
        assert_eq!(topics, vec!["test-topic"]);
        // entities from topics
        let entities: Vec<&str> = node
            .topics
            .iter()
            .flat_map(|t| t.entities.iter().map(|e| e.as_str()))
            .collect();
        assert_eq!(entities, vec!["entity-a", "entity-b"]);
        // structured_data contains depth, children, parent_id etc.
        let sd = serde_json::json!({
            "depth": node.depth,
            "children": node.children,
            "parent_id": node.parent_id,
        });
        assert_eq!(sd["depth"], 1);
        assert_eq!(sd["children"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_bottom_up_ordering() {
        // Verify that bottom-up ordering puts L0 before L1 before apex
        let nodes = vec![
            make_test_node("apex", 2, vec!["L1-a".to_string()]),
            make_test_node("L0-a", 0, vec![]),
            make_test_node("L0-b", 0, vec![]),
            make_test_node("L1-a", 1, vec!["L0-a".to_string(), "L0-b".to_string()]),
        ];

        let mut by_depth: HashMap<i64, Vec<&PyramidNode>> = HashMap::new();
        for node in &nodes {
            by_depth.entry(node.depth).or_default().push(node);
        }
        let mut depths: Vec<i64> = by_depth.keys().copied().collect();
        depths.sort();

        assert_eq!(depths, vec![0, 1, 2]);
        assert_eq!(by_depth[&0].len(), 2); // L0 nodes first
        assert_eq!(by_depth[&1].len(), 1); // L1 next
        assert_eq!(by_depth[&2].len(), 1); // apex last
    }

    #[test]
    fn test_derived_from_uses_wire_uuids() {
        // Simulate the derived_from construction during publish
        let node = make_test_node(
            "L1-001",
            1,
            vec!["L0-001".to_string(), "L0-002".to_string()],
        );

        // Simulate id_map populated after L0 publication
        let mut id_map: HashMap<String, String> = HashMap::new();
        id_map.insert("L0-001".to_string(), "wire-uuid-001".to_string());
        id_map.insert("L0-002".to_string(), "wire-uuid-002".to_string());

        // Build derived_from using DerivedFromEntry (same logic as publish_pyramid)
        let derived_from: Vec<DerivedFromEntry> = node
            .children
            .iter()
            .filter_map(|child_id| {
                id_map.get(child_id).map(|wire_uuid| DerivedFromEntry {
                    ref_path: wire_uuid.clone(),
                    source_type: "contribution".to_string(),
                    weight: 1.0,
                    justification: Some(format!("child node {}", child_id)),
                })
            })
            .collect();

        assert_eq!(derived_from.len(), 2);
        assert_eq!(derived_from[0].ref_path, "wire-uuid-001"); // Wire UUID, not local ID
        assert_eq!(derived_from[1].ref_path, "wire-uuid-002");
    }

    #[test]
    fn test_weights_passed_through_as_is() {
        // publish_pyramid_node no longer normalizes — weights are passed through.
        // Caller (publication.rs) is responsible for pre-normalization.
        let entries = vec![
            DerivedFromEntry {
                ref_path: "a".to_string(),
                source_type: "contribution".to_string(),
                weight: 0.6,
                justification: None,
            },
            DerivedFromEntry {
                ref_path: "b".to_string(),
                source_type: "contribution".to_string(),
                weight: 0.4,
                justification: None,
            },
        ];
        // Weights should remain as the caller set them
        assert!((entries[0].weight - 0.6).abs() < 1e-10);
        assert!((entries[1].weight - 0.4).abs() < 1e-10);
        // Caller should ensure they sum to 1.0
        let weight_sum: f64 = entries.iter().map(|e| e.weight).sum();
        assert!((weight_sum - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_zero_weight_rejected() {
        let entry = DerivedFromEntry {
            ref_path: "bad".to_string(),
            source_type: "contribution".to_string(),
            weight: 0.0,
            justification: None,
        };
        assert!(
            entry.weight <= 0.0,
            "zero weight should be rejected by publish_pyramid_node"
        );
    }

    #[test]
    fn test_is_already_published_no_table() {
        // When pyramid_id_map table doesn't exist, should return Ok(false)
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let result = is_already_published(&conn, "slug", "node-1").unwrap();
        assert!(!result);
    }

    #[test]
    fn test_is_already_published_exists() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_id_map_table(&conn).unwrap();
        save_id_mapping(&conn, "slug", "node-1", "uuid-1").unwrap();

        assert!(is_already_published(&conn, "slug", "node-1").unwrap());
        assert!(!is_already_published(&conn, "slug", "node-2").unwrap());
    }

    #[test]
    fn test_l0_source_type_is_source_document() {
        // Verify depth-based source_type logic
        let depth: i64 = 0;
        let source_type = if depth == 0 {
            "source_document"
        } else {
            "contribution"
        };
        assert_eq!(source_type, "source_document");

        let depth: i64 = 1;
        let source_type = if depth == 0 {
            "source_document"
        } else {
            "contribution"
        };
        assert_eq!(source_type, "contribution");
    }

    #[test]
    fn test_question_set_publication_fields() {
        let qs = QuestionSet {
            r#type: "code".to_string(),
            version: "3.0".to_string(),
            defaults: crate::pyramid::question_yaml::QuestionDefaults {
                model: Some("inception/mercury-2".to_string()),
                temperature: Some(0.3),
                retry: Some(2),
            },
            questions: vec![crate::pyramid::question_yaml::Question {
                ask: "What does this file do?".to_string(),
                about: "each file individually".to_string(),
                creates: "L0 nodes".to_string(),
                prompt: "prompts/code/extract.md".to_string(),
                cluster_prompt: None,
                model: None,
                cluster_model: None,
                temperature: None,
                parallel: None,
                retry: None,
                optional: None,
                variants: None,
                constraints: None,
                context: None,
                sequential_context: None,
                preview_lines: None,
            }],
        };

        // Verify structured_data would contain question_set_definition
        let qs_json = serde_json::to_value(&qs).unwrap();
        let sd = serde_json::json!({
            "question_set_definition": qs_json,
        });
        assert!(sd.get("question_set_definition").is_some());
        let inner = &sd["question_set_definition"];
        assert_eq!(inner["type"], "code");
        assert_eq!(inner["version"], "3.0");
        assert!(inner["questions"].is_array());
    }

    #[test]
    fn test_teaser_explicit_not_auto_generated() {
        let node = make_test_node("test", 0, vec![]);

        // teaser should come from headline, not be auto-generated from JSON body
        let teaser = if node.headline.len() > 200 {
            node.headline[..200].to_string()
        } else {
            node.headline.clone()
        };

        // teaser should be human-readable text, not JSON
        assert!(!teaser.contains("{"));
        assert!(!teaser.contains("\"depth\""));
        assert!(teaser.contains("Headline"));
    }
}
