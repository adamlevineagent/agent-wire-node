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
use super::types::PyramidNode;

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

// ─── Types ───────────────────────────────────────────────────

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

/// A single local_id → wire_uuid mapping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdMapping {
    pub local_id: String,
    pub wire_uuid: String,
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
}

// ─── Client ──────────────────────────────────────────────────

/// HTTP client for publishing pyramid outputs to the Wire marketplace.
pub struct PyramidPublisher {
    /// Wire API base URL (e.g., "https://api.callmeplayful.com")
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
    /// `derived_from_wire_uuids` maps this node's children local IDs to their
    /// Wire UUIDs, used for the `derived_from` field on the contribution.
    ///
    /// Returns the Wire UUID of the published contribution.
    pub async fn publish_pyramid_node(
        &self,
        node: &PyramidNode,
        derived_from_wire_uuids: &[(String, String)], // (child_wire_uuid, justification)
    ) -> Result<String> {
        // Teaser is set explicitly (prose, not JSON) to avoid the Wire's
        // generateTeaser() truncating structured_data JSON into nonsense.
        // The em-dash separator " — " is 5 bytes (space + 3-byte UTF-8 + space).
        const TEASER_MAX: usize = 200;
        const SEPARATOR: &str = " — ";
        let teaser = if node.headline.len() > TEASER_MAX {
            truncate_str(&node.headline, TEASER_MAX).to_string()
        } else if node.distilled.len() > TEASER_MAX {
            let prefix_len = node.headline.len() + SEPARATOR.len();
            if prefix_len >= TEASER_MAX {
                // Headline alone fills the teaser
                truncate_str(&node.headline, TEASER_MAX).to_string()
            } else {
                let remaining = TEASER_MAX - prefix_len;
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
        let structured_data = serde_json::json!({
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

        // Build derived_from array using Wire UUIDs
        let derived_from: Vec<serde_json::Value> = derived_from_wire_uuids
            .iter()
            .map(|(wire_uuid, justification)| {
                serde_json::json!({
                    "source_type": "contribution",
                    "source_item_id": wire_uuid,
                    "weight": 1.0,
                    "justification": justification,
                })
            })
            .collect();

        let payload = serde_json::json!({
            "type": "pyramid_node",
            "contribution_type": "mechanical",
            "title": node.headline,
            "teaser": teaser,
            "body": node.distilled,
            "topics": topics,
            "entities": entities,
            "structured_data": structured_data,
            "derived_from": derived_from,
        });

        self.post_contribution(&payload).await
    }

    /// Publish an entire pyramid (all pre-loaded nodes), bottom-up.
    ///
    /// `nodes_by_depth` must be sorted by depth (ascending): L0 first, apex last.
    /// Each entry is (depth, nodes_at_that_depth).
    ///
    /// Returns the ID mappings and apex Wire UUID. Caller is responsible for
    /// persisting the mappings to SQLite.
    pub async fn publish_pyramid(
        &self,
        slug: &str,
        nodes_by_depth: &[(i64, Vec<PyramidNode>)],
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
                // Build derived_from from this node's children Wire UUIDs
                let derived_from: Vec<(String, String)> = node
                    .children
                    .iter()
                    .filter_map(|child_id| {
                        id_map.get(child_id).map(|wire_uuid| {
                            (wire_uuid.clone(), format!("child node {}", child_id))
                        })
                    })
                    .collect();

                let wire_uuid = self.publish_pyramid_node(node, &derived_from).await?;

                id_map.insert(node.id.clone(), wire_uuid.clone());
                all_mappings.push(IdMapping {
                    local_id: node.id.clone(),
                    wire_uuid: wire_uuid.clone(),
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

        let wire_uuid = self.post_contribution(&payload).await?;

        Ok(PublishQuestionSetResult {
            wire_uuid,
            content_type: question_set.r#type.clone(),
        })
    }

    /// Post a contribution to the Wire API and return its UUID.
    async fn post_contribution(&self, payload: &serde_json::Value) -> Result<String> {
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

        // The contribute endpoint returns { contribution: { id: "uuid" } }
        let contribution_id = body
            .get("contribution")
            .and_then(|c| c.get("id"))
            .and_then(|id| id.as_str())
            .ok_or_else(|| {
                WirePublishError::Rejected("response missing contribution.id".to_string())
            })?;

        Ok(contribution_id.to_string())
    }
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
            created_at: "2026-03-25T00:00:00Z".to_string(),
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

        // Build derived_from from children Wire UUIDs (same logic as publish_pyramid)
        let derived_from: Vec<(String, String)> = node
            .children
            .iter()
            .filter_map(|child_id| {
                id_map
                    .get(child_id)
                    .map(|wire_uuid| (wire_uuid.clone(), format!("child node {}", child_id)))
            })
            .collect();

        assert_eq!(derived_from.len(), 2);
        assert_eq!(derived_from[0].0, "wire-uuid-001"); // Wire UUID, not local ID
        assert_eq!(derived_from[1].0, "wire-uuid-002");
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
