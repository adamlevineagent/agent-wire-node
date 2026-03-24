// Wire Node — Retention + Purge Handler
//
// Handles proof-of-retention challenges from heartbeat responses.
// Computes SHA-256 of specified byte ranges from local files (UTF-8 bytes, no BOM).
// Responds via POST /api/v1/node/retention-challenge.
// Handles purge directives: delete specified files, confirm via POST /api/v1/node/inventory.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// A retention challenge received from the heartbeat response
///
/// Server sends: id, document_id, byte_range_start, byte_range_end, expected_hash
/// corpus_id may not be present — when missing, we scan cache dirs to find the file.
#[derive(Debug, Clone, Deserialize)]
pub struct RetentionChallenge {
    #[serde(rename = "id")]
    pub challenge_id: String,
    pub document_id: String,
    #[serde(default)]
    pub corpus_id: Option<String>,
    #[serde(rename = "byte_range_start")]
    pub byte_start: usize,
    #[serde(rename = "byte_range_end")]
    pub byte_end: usize,
    /// The expected hash — not used client-side but included for completeness
    #[serde(default)]
    #[allow(dead_code)]
    pub expected_hash: Option<String>,
}

/// A purge directive received from the heartbeat response
///
/// corpus_id may not be present — when missing, we scan cache dirs to find the file.
#[derive(Debug, Clone, Deserialize)]
pub struct PurgeDirective {
    pub document_id: String,
    #[serde(default)]
    pub corpus_id: Option<String>,
    pub reason: Option<String>,
}

/// Result of a retention challenge response
#[derive(Debug, Serialize)]
struct ChallengeResponse {
    challenge_id: String,
    node_id: String,
    hash: String,
}

/// Handle a batch of retention challenges
pub async fn handle_retention_challenges(
    api_url: &str,
    access_token: &str,
    node_id: &str,
    challenges: &[RetentionChallenge],
    cache_dir: &Path,
) -> Result<usize, String> {
    let client = reqwest::Client::new();
    let mut successful = 0;

    for challenge in challenges {
        // Find the document file — use corpus_id if available, otherwise scan all dirs
        let file_path = if let Some(ref corpus_id) = challenge.corpus_id {
            let p =
                crate::sync::get_cached_document_path(cache_dir, corpus_id, &challenge.document_id);
            if p.exists() {
                Some(p)
            } else {
                None
            }
        } else {
            crate::sync::find_cached_document_by_id(cache_dir, &challenge.document_id)
                .await
                .map(|(_corpus, path)| path)
        };

        let file_path = match file_path {
            Some(p) => p,
            None => {
                tracing::warn!(
                    "Retention challenge for missing document: {:?}/{}",
                    challenge.corpus_id,
                    challenge.document_id
                );
                continue;
            }
        };

        // Compute SHA-256 of the specified byte range
        match crate::sync::hash_byte_range(&file_path, challenge.byte_start, challenge.byte_end) {
            Ok(hash) => {
                // Respond to the challenge
                let response = ChallengeResponse {
                    challenge_id: challenge.challenge_id.clone(),
                    node_id: node_id.to_string(),
                    hash,
                };

                let url = format!("{}/api/v1/node/retention-challenge", api_url);
                match client
                    .post(&url)
                    .header("Authorization", format!("Bearer {}", access_token))
                    .header("Content-Type", "application/json")
                    .json(&response)
                    .timeout(std::time::Duration::from_secs(10))
                    .send()
                    .await
                {
                    Ok(resp) if resp.status().is_success() => {
                        tracing::debug!(
                            "Retention challenge {} passed for {:?}/{}",
                            challenge.challenge_id,
                            challenge.corpus_id,
                            challenge.document_id
                        );
                        successful += 1;
                    }
                    Ok(resp) => {
                        let text = resp.text().await.unwrap_or_default();
                        tracing::warn!(
                            "Retention challenge {} response error: {}",
                            challenge.challenge_id,
                            text
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Retention challenge {} failed to submit: {}",
                            challenge.challenge_id,
                            e
                        );
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to compute hash for retention challenge {}: {}",
                    challenge.challenge_id,
                    e
                );
            }
        }
    }

    Ok(successful)
}

/// Handle purge directives: delete specified files, confirm via inventory
pub async fn handle_purge_directives(
    api_url: &str,
    access_token: &str,
    node_id: &str,
    directives: &[PurgeDirective],
    cache_dir: &Path,
) -> Result<usize, String> {
    let mut purged = 0;

    for directive in directives {
        tracing::info!(
            "Purging document {:?}/{} (reason: {:?})",
            directive.corpus_id,
            directive.document_id,
            directive.reason
        );

        let result = if let Some(ref corpus_id) = directive.corpus_id {
            crate::sync::delete_cached_document(cache_dir, corpus_id, &directive.document_id).await
        } else {
            crate::sync::delete_cached_document_by_id(cache_dir, &directive.document_id).await
        };

        match result {
            Ok(_) => {
                purged += 1;
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to purge {:?}/{}: {}",
                    directive.corpus_id,
                    directive.document_id,
                    e
                );
            }
        }
    }

    // Report current inventory after purge
    if purged > 0 {
        if let Err(e) = report_inventory(api_url, access_token, node_id, cache_dir).await {
            tracing::warn!("Failed to report inventory after purge: {}", e);
        }
    }

    Ok(purged)
}

/// Report current document inventory to the Wire API
pub async fn report_inventory(
    api_url: &str,
    access_token: &str,
    node_id: &str,
    cache_dir: &Path,
) -> Result<(), String> {
    use sha2::{Digest, Sha256};

    // Scan cache directory for all hosted documents
    let mut inventory: Vec<serde_json::Value> = Vec::new();

    if let Ok(mut entries) = tokio::fs::read_dir(cache_dir).await {
        while let Ok(Some(corpus_entry)) = entries.next_entry().await {
            if corpus_entry
                .file_type()
                .await
                .map(|ft| ft.is_dir())
                .unwrap_or(false)
            {
                if let Ok(mut doc_entries) = tokio::fs::read_dir(corpus_entry.path()).await {
                    while let Ok(Some(doc_entry)) = doc_entries.next_entry().await {
                        if let Some(name) = doc_entry.file_name().to_str() {
                            if name.ends_with(".body") {
                                let document_id = name.trim_end_matches(".body");
                                let file_path = doc_entry.path();
                                // Compute SHA-256 body_hash of file contents
                                let body_hash = match tokio::fs::read(&file_path).await {
                                    Ok(bytes) => {
                                        let mut hasher = Sha256::new();
                                        hasher.update(&bytes);
                                        hex::encode(hasher.finalize())
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            "Failed to read {} for hashing: {}",
                                            file_path.display(),
                                            e
                                        );
                                        continue;
                                    }
                                };
                                inventory.push(serde_json::json!({
                                    "document_id": document_id,
                                    "body_hash": body_hash,
                                }));
                            }
                        }
                    }
                }
            }
        }
    }

    let client = reqwest::Client::new();
    let url = format!("{}/api/v1/node/inventory", api_url);
    let body = serde_json::json!({
        "node_id": node_id,
        "documents": inventory,
    });

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .json(&body)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("Inventory report failed: {}", e))?;

    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        tracing::warn!("Inventory report response: {}", text);
    } else {
        tracing::info!("Reported inventory: {} documents", inventory.len());
    }

    Ok(())
}
