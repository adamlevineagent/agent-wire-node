// pyramid/chain_publish.rs — WS-CHAIN-PUBLISH
//
// Makes chain configurations (YAML + prompts) publishable to the Wire
// contribution graph. Chain configs are first-class Wire contributions:
// forkable, publishable, improvable, attributable.
//
// Publication flow:
//   1. Load chain definition from disk
//   2. Serialize chain YAML + referenced prompt files into a bundle
//   3. Create/update the chain_publications record
//   4. If Wire is connected: publish as a Wire contribution
//   5. If Wire is not connected: mark as 'local' (publishable later)
//
// Fork flow:
//   Copy chain YAML + prompts to a new chain ID, record fork lineage.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::chain_engine::ChainDefinition;
use super::chain_loader::{discover_chains, load_chain};
use super::db;
use super::types::ChainPublication;
use super::wire_publish::PyramidPublisher;
use super::PyramidState;

/// Bundle of a chain definition + all referenced prompt file contents.
/// This is what gets serialized into a Wire contribution's structured_data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainBundle {
    /// The raw YAML content of the chain definition file.
    pub chain_yaml: String,
    /// Map of prompt reference path -> prompt file contents.
    /// Keys are the relative paths (e.g., "conversation/forward.md").
    pub prompt_files: std::collections::HashMap<String, String>,
    /// Parsed chain metadata.
    pub chain_id: String,
    pub chain_name: String,
    pub content_type: String,
    pub version: String,
    pub author: String,
    pub step_count: usize,
}

/// Publish a chain configuration to the Wire contribution graph.
///
/// Steps:
/// 1. Loads the chain definition from disk (resolves prompts)
/// 2. Serializes chain YAML + all referenced prompt files into a bundle
/// 3. Creates/updates the `pyramid_chain_publications` record
/// 4. If Wire is connected: publishes as a Wire contribution
/// 5. If Wire is not connected: marks as 'local'
/// 6. Returns the publication record
pub async fn publish_chain_to_wire(
    state: &PyramidState,
    chain_id: &str,
) -> Result<ChainPublication> {
    let chains_dir = &state.chains_dir;

    // Step 1: Find the chain YAML file on disk
    let chain_yaml_path = find_chain_file(chains_dir, chain_id)?;

    // Step 2: Read raw YAML and load the parsed definition
    let raw_yaml = std::fs::read_to_string(&chain_yaml_path)
        .with_context(|| format!("failed to read chain file: {}", chain_yaml_path.display()))?;

    let chain_def = load_chain(&chain_yaml_path, chains_dir)
        .with_context(|| format!("failed to load chain '{}'", chain_id))?;

    // Step 3: Collect referenced prompt files
    let prompt_files = collect_prompt_files(&chain_def, chains_dir)?;

    let bundle = ChainBundle {
        chain_yaml: raw_yaml,
        prompt_files,
        chain_id: chain_def.id.clone(),
        chain_name: chain_def.name.clone(),
        content_type: chain_def.content_type.clone(),
        version: chain_def.version.clone(),
        author: chain_def.author.clone(),
        step_count: chain_def.steps.len(),
    };

    // Step 4: Create/update the publication record in DB
    let pub_record = {
        let conn = state.writer.lock().await;

        // Check if a record already exists
        let existing = db::get_chain_publication(&conn, chain_id)?;
        let version = match &existing {
            Some(rec) => {
                // If already published, increment version
                if rec.status == "published" {
                    db::increment_chain_version(&conn, chain_id)?
                } else {
                    rec.version
                }
            }
            None => {
                // First publication: create version 1
                let new_record = ChainPublication {
                    id: 0,
                    chain_id: chain_id.to_string(),
                    version: 1,
                    wire_handle_path: None,
                    wire_uuid: None,
                    published_at: None,
                    description: Some(chain_def.description.clone()),
                    author: Some(chain_def.author.clone()),
                    forked_from: None,
                    status: "local".to_string(),
                    created_at: String::new(),
                    updated_at: String::new(),
                };
                db::save_chain_publication(&conn, &new_record)?;
                1
            }
        };

        // Re-read the current record to get full DB state
        db::get_chain_publication_by_version(&conn, chain_id, version)?
            .ok_or_else(|| anyhow::anyhow!("failed to read back chain publication after save"))?
    };

    // Step 5: Attempt Wire publication if auth is available
    let config = state.config.read().await;
    let wire_url =
        std::env::var("WIRE_URL").unwrap_or_else(|_| "https://newsbleach.com".to_string());
    let wire_auth = config.auth_token.clone();
    drop(config);

    if wire_auth.is_empty() {
        // No Wire auth — stay as 'local', return the record
        tracing::info!(
            chain_id = chain_id,
            version = pub_record.version,
            "chain publication saved as local (no Wire auth configured)"
        );
        return Ok(pub_record);
    }

    // Publish to Wire
    let publisher = PyramidPublisher::new(wire_url, wire_auth);
    let bundle_json = serde_json::to_value(&bundle)
        .context("failed to serialize chain bundle")?;

    let title = format!("Chain: {} (v{})", chain_def.name, chain_def.version);
    let teaser = if chain_def.description.len() > 200 {
        chain_def.description[..200].to_string()
    } else {
        chain_def.description.clone()
    };

    let structured_data = serde_json::json!({
        "chain_bundle": bundle_json,
        "chain_id": chain_def.id,
        "content_type": chain_def.content_type,
        "chain_version": chain_def.version,
        "step_count": chain_def.steps.len(),
    });

    let payload = serde_json::json!({
        "type": "chain_config",
        "contribution_type": "mechanical",
        "title": title,
        "teaser": teaser,
        "body": chain_def.description,
        "topics": [chain_def.content_type, "chain_config"],
        "entities": [],
        "structured_data": structured_data,
        "derived_from": [],
    });

    match publisher.post_contribution(&payload).await {
        Ok((wire_uuid, handle_path)) => {
            // Update the DB record with Wire info
            let conn = state.writer.lock().await;
            db::mark_chain_published(
                &conn,
                chain_id,
                handle_path.as_deref().unwrap_or(&wire_uuid),
                &wire_uuid,
            )?;
            let final_record = db::get_chain_publication(&conn, chain_id)?
                .ok_or_else(|| anyhow::anyhow!("failed to read back chain publication after publish"))?;
            tracing::info!(
                chain_id = chain_id,
                wire_uuid = %wire_uuid,
                "chain published to Wire"
            );
            Ok(final_record)
        }
        Err(e) => {
            tracing::warn!(
                chain_id = chain_id,
                error = %e,
                "failed to publish chain to Wire — saved as local"
            );
            // Keep as 'local' on Wire failure
            Ok(pub_record)
        }
    }
}

/// Fork a chain: copies source chain YAML + prompts to a new chain ID.
/// Records the fork lineage in the publications table.
///
/// Returns the path to the new chain YAML file.
pub fn fork_chain(
    chains_dir: &Path,
    source_chain_id: &str,
    new_chain_id: &str,
    author: &str,
    conn: &rusqlite::Connection,
) -> Result<String> {
    // Find source chain file
    let source_path = find_chain_file(chains_dir, source_chain_id)?;

    // Read and modify the chain YAML
    let raw_yaml = std::fs::read_to_string(&source_path)
        .with_context(|| format!("failed to read source chain: {}", source_path.display()))?;

    let mut chain_def: ChainDefinition = serde_yaml::from_str(&raw_yaml)
        .with_context(|| format!("failed to parse source chain YAML"))?;

    // Update identity fields for the fork
    chain_def.id = new_chain_id.to_string();
    chain_def.name = format!("{} (fork)", chain_def.name);
    chain_def.author = author.to_string();
    chain_def.version = "0.1.0".to_string();

    // Write the forked chain YAML to variants/ directory
    let variants_dir = chains_dir.join("variants");
    if !variants_dir.exists() {
        std::fs::create_dir_all(&variants_dir)
            .with_context(|| format!("failed to create variants dir: {}", variants_dir.display()))?;
    }

    let new_yaml = serde_yaml::to_string(&chain_def)
        .context("failed to serialize forked chain YAML")?;

    let new_path = variants_dir.join(format!("{}.yaml", new_chain_id));
    std::fs::write(&new_path, &new_yaml)
        .with_context(|| format!("failed to write forked chain: {}", new_path.display()))?;

    // Copy referenced prompt files (if the source has $prompts/ references)
    // The prompt files are shared across chains (they live in chains/prompts/),
    // so the fork can reference the same prompt files. No copying needed.

    // Record the fork in the publications table
    db::fork_chain_publication(conn, source_chain_id, new_chain_id, author)?;

    tracing::info!(
        source = source_chain_id,
        fork = new_chain_id,
        path = %new_path.display(),
        "chain forked"
    );

    Ok(new_path.to_string_lossy().into_owned())
}

// ── Internal helpers ────────────────────────────────────────────────────────

/// Find a chain YAML file by chain_id. Searches defaults/ then variants/.
fn find_chain_file(chains_dir: &Path, chain_id: &str) -> Result<PathBuf> {
    // First try discovering all chains and matching by ID
    let chains = discover_chains(chains_dir)
        .with_context(|| "failed to discover chains")?;

    for meta in &chains {
        if meta.id == chain_id {
            return Ok(PathBuf::from(&meta.file_path));
        }
    }

    anyhow::bail!(
        "chain '{}' not found in chains directory '{}'",
        chain_id,
        chains_dir.display()
    )
}

/// Collect all prompt files referenced by a chain definition.
/// Scans step instructions for `$prompts/...` references and reads those files.
fn collect_prompt_files(
    def: &ChainDefinition,
    chains_dir: &Path,
) -> Result<std::collections::HashMap<String, String>> {
    let mut prompt_files = std::collections::HashMap::new();

    fn collect_from_steps(
        steps: &[super::chain_engine::ChainStep],
        chains_dir: &Path,
        prompt_files: &mut std::collections::HashMap<String, String>,
    ) -> Result<()> {
        for step in steps {
            // Check all instruction-like fields for $prompts/ references
            let fields = [
                step.instruction.as_deref(),
                step.cluster_instruction.as_deref(),
                step.merge_instruction.as_deref(),
                step.heal_instruction.as_deref(),
            ];

            // Note: instruction fields have already been resolved by load_chain
            // (contents replaced inline). We collect prompt files by scanning
            // the content-type prompt directory below rather than parsing
            // resolved instructions.
            let _ = fields;

            // Check instruction_map values
            if let Some(ref map) = step.instruction_map {
                for value in map.values() {
                    if let Some(rel) = value.strip_prefix("$prompts/") {
                        let prompt_path = chains_dir.join("prompts").join(rel);
                        if prompt_path.exists() {
                            let content = std::fs::read_to_string(&prompt_path)?;
                            prompt_files.insert(rel.to_string(), content);
                        }
                    }
                }
            }

            // Recurse into container steps
            if let Some(ref inner_steps) = step.steps {
                collect_from_steps(inner_steps, chains_dir, prompt_files)?;
            }
        }
        Ok(())
    }

    // Since load_chain resolves $prompts/ references in-place (replacing them
    // with file contents), we need to scan the raw YAML for references. But
    // the ChainDefinition we have already has them resolved. Instead, we'll
    // use a pragmatic approach: scan the content type's prompt directory
    // and include all files there.
    let content_type_dir = chains_dir.join("prompts").join(&def.content_type);
    if content_type_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&content_type_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if let Some(filename) = path.file_name().and_then(|f| f.to_str()) {
                        let rel_key = format!("{}/{}", def.content_type, filename);
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            prompt_files.insert(rel_key, content);
                        }
                    }
                }
            }
        }
    }

    // Also include shared prompts
    let shared_dir = chains_dir.join("prompts").join("shared");
    if shared_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&shared_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if let Some(filename) = path.file_name().and_then(|f| f.to_str()) {
                        let rel_key = format!("shared/{}", filename);
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            prompt_files.insert(rel_key, content);
                        }
                    }
                }
            }
        }
    }

    // Run the step-level scan for any instruction_map $prompts/ refs
    collect_from_steps(&def.steps, chains_dir, &mut prompt_files)?;

    Ok(prompt_files)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        // Create the chain publications table
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS pyramid_chain_publications (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                chain_id TEXT NOT NULL,
                version INTEGER NOT NULL DEFAULT 1,
                wire_handle_path TEXT,
                wire_uuid TEXT,
                published_at TEXT,
                description TEXT,
                author TEXT,
                forked_from TEXT,
                status TEXT NOT NULL DEFAULT 'local',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(chain_id, version)
            );
            ",
        )
        .unwrap();
        conn
    }

    #[test]
    fn test_save_and_retrieve_chain_publication() {
        let conn = setup_test_db();

        let pub_record = ChainPublication {
            id: 0,
            chain_id: "conversation-default".to_string(),
            version: 1,
            wire_handle_path: None,
            wire_uuid: None,
            published_at: None,
            description: Some("Standard conversation chain".to_string()),
            author: Some("wire-default".to_string()),
            forked_from: None,
            status: "local".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        };

        db::save_chain_publication(&conn, &pub_record).unwrap();

        let retrieved = db::get_chain_publication(&conn, "conversation-default")
            .unwrap()
            .expect("should find the record");

        assert_eq!(retrieved.chain_id, "conversation-default");
        assert_eq!(retrieved.version, 1);
        assert_eq!(retrieved.status, "local");
        assert_eq!(
            retrieved.description.as_deref(),
            Some("Standard conversation chain")
        );
        assert_eq!(retrieved.author.as_deref(), Some("wire-default"));
    }

    #[test]
    fn test_increment_version() {
        let conn = setup_test_db();

        let pub_record = ChainPublication {
            id: 0,
            chain_id: "code-default".to_string(),
            version: 1,
            wire_handle_path: None,
            wire_uuid: None,
            published_at: None,
            description: Some("Code analysis chain".to_string()),
            author: Some("wire-default".to_string()),
            forked_from: None,
            status: "local".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        };

        db::save_chain_publication(&conn, &pub_record).unwrap();

        let new_version = db::increment_chain_version(&conn, "code-default").unwrap();
        assert_eq!(new_version, 2);

        // The latest version should be 2
        let latest = db::get_chain_publication(&conn, "code-default")
            .unwrap()
            .expect("should find latest version");
        assert_eq!(latest.version, 2);
        assert_eq!(latest.status, "local");

        // Version 1 should still exist
        let v1 = db::get_chain_publication_by_version(&conn, "code-default", 1)
            .unwrap()
            .expect("should find version 1");
        assert_eq!(v1.version, 1);
    }

    #[test]
    fn test_fork_records_lineage() {
        let conn = setup_test_db();

        // Create the source chain publication
        let source = ChainPublication {
            id: 0,
            chain_id: "conversation-default".to_string(),
            version: 1,
            wire_handle_path: Some("wire/conversation-default".to_string()),
            wire_uuid: Some("uuid-abc-123".to_string()),
            published_at: Some("2026-04-08T12:00:00Z".to_string()),
            description: Some("Original conversation chain".to_string()),
            author: Some("wire-default".to_string()),
            forked_from: None,
            status: "published".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        };
        db::save_chain_publication(&conn, &source).unwrap();

        // Fork it
        db::fork_chain_publication(
            &conn,
            "conversation-default",
            "conversation-custom",
            "adam",
        )
        .unwrap();

        // Check the fork record
        let fork = db::get_chain_publication(&conn, "conversation-custom")
            .unwrap()
            .expect("should find fork record");

        assert_eq!(fork.chain_id, "conversation-custom");
        assert_eq!(fork.version, 1);
        assert_eq!(fork.forked_from.as_deref(), Some("conversation-default"));
        assert_eq!(fork.author.as_deref(), Some("adam"));
        assert_eq!(fork.status, "local");
        // Fork should inherit the description
        assert_eq!(
            fork.description.as_deref(),
            Some("Original conversation chain")
        );
    }

    #[test]
    fn test_list_publications() {
        let conn = setup_test_db();

        // Insert two chains with multiple versions
        for (chain_id, desc) in [
            ("conversation-default", "Conversation chain"),
            ("code-default", "Code chain"),
        ] {
            let pub_record = ChainPublication {
                id: 0,
                chain_id: chain_id.to_string(),
                version: 1,
                wire_handle_path: None,
                wire_uuid: None,
                published_at: None,
                description: Some(desc.to_string()),
                author: Some("wire-default".to_string()),
                forked_from: None,
                status: "local".to_string(),
                created_at: String::new(),
                updated_at: String::new(),
            };
            db::save_chain_publication(&conn, &pub_record).unwrap();
        }

        // Add version 2 for conversation-default
        db::increment_chain_version(&conn, "conversation-default").unwrap();

        let list = db::list_chain_publications(&conn).unwrap();
        assert_eq!(list.len(), 2, "should list one entry per chain_id");

        // conversation-default should be at version 2 (latest)
        let conv = list.iter().find(|p| p.chain_id == "conversation-default").unwrap();
        assert_eq!(conv.version, 2);

        // code-default should be at version 1
        let code = list.iter().find(|p| p.chain_id == "code-default").unwrap();
        assert_eq!(code.version, 1);
    }
}
