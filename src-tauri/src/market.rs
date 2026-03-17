// Wire Node — Market Daemon
//
// Reads storage market surface from heartbeat response.
// Evaluates opportunities against local competitive position.
// Auto-hosts documents: pull from origin, verify hash, store locally.
// Auto-drops underperformers.
// Respects storage_cap_gb and mesh_hosting_enabled settings.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Market daemon state
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct MarketState {
    pub hosted_documents: HashMap<String, HostedDocument>,
    pub total_hosted_bytes: u64,
    pub last_evaluation_at: Option<String>,
    pub is_evaluating: bool,
}

/// A document this node has chosen to host
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostedDocument {
    pub document_id: String,
    pub corpus_id: String,
    pub body_hash: String,
    pub size_bytes: u64,
    pub pulls_served: u64,
    pub credits_earned: f64,
    pub hosted_since: String,
}

/// Market opportunity from heartbeat response — field names match server
#[derive(Debug, Clone, Deserialize)]
pub struct MarketOpportunity {
    pub document_id: String,
    pub corpus_id: String,
    pub pulls_30d: u64,
    pub current_replicas: u64,
    pub word_count: u64,
    pub body_hash: String,
}

/// Load market state from disk
pub fn load_market_state(data_dir: &Path) -> Option<MarketState> {
    let path = data_dir.join("market_state.json");
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Save market state to disk
pub fn save_market_state(data_dir: &Path, state: &MarketState) {
    let path = data_dir.join("market_state.json");
    if let Ok(json) = serde_json::to_string(state) {
        let _ = std::fs::write(&path, json);
    }
}

/// Evaluate market opportunities and decide what to host/drop
pub async fn evaluate_opportunities(
    api_url: &str,
    access_token: &str,
    node_id: &str,
    opportunities: &[MarketOpportunity],
    market_state: &mut MarketState,
    cache_dir: &Path,
    storage_cap_gb: f64,
    mesh_hosting_enabled: bool,
) {
    if !mesh_hosting_enabled {
        tracing::debug!("Mesh hosting disabled, skipping market evaluation");
        return;
    }

    let cap_bytes = (storage_cap_gb * 1024.0 * 1024.0 * 1024.0) as u64;
    let current_usage = market_state.total_hosted_bytes;

    market_state.is_evaluating = true;
    market_state.last_evaluation_at = Some(chrono::Utc::now().to_rfc3339());

    // Sort opportunities by efficiency: pulls_30d / current_replicas (higher = more demand per replica)
    let mut scored: Vec<(&MarketOpportunity, f64)> = opportunities.iter()
        .filter(|o| !market_state.hosted_documents.contains_key(&o.document_id))
        .map(|o| {
            let efficiency = if o.current_replicas > 0 {
                o.pulls_30d as f64 / o.current_replicas as f64
            } else {
                o.pulls_30d as f64 // No replicas = maximum demand
            };
            (o, efficiency)
        })
        .collect();

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Auto-host top opportunities that fit within storage cap
    // Estimate size from word_count (~6 bytes per word average)
    let mut bytes_used = current_usage;
    for (opportunity, _score) in &scored {
        let estimated_size = opportunity.word_count * 6;
        if bytes_used + estimated_size > cap_bytes {
            continue;
        }

        // Pull document from origin, verify hash, store locally
        match host_document(
            api_url,
            access_token,
            node_id,
            opportunity,
            cache_dir,
        ).await {
            Ok(hosted) => {
                bytes_used += hosted.size_bytes;
                market_state.hosted_documents.insert(
                    hosted.document_id.clone(),
                    hosted,
                );
                market_state.total_hosted_bytes = bytes_used;
            }
            Err(e) => {
                tracing::warn!("Failed to host document {}: {}", opportunity.document_id, e);
            }
        }
    }

    // Auto-drop underperformers — documents with zero pulls and low expected value
    let drop_candidates: Vec<String> = market_state.hosted_documents.iter()
        .filter(|(_, doc)| {
            doc.pulls_served == 0 && doc.credits_earned == 0.0
        })
        .map(|(id, _)| id.clone())
        .collect();

    // Only drop if we're near capacity (>90% full)
    if bytes_used as f64 > cap_bytes as f64 * 0.9 {
        for doc_id in drop_candidates.iter().take(5) {
            match drop_document(api_url, access_token, node_id, doc_id, cache_dir, market_state).await {
                Ok(_) => {
                    tracing::info!("Dropped underperforming document: {}", doc_id);
                }
                Err(e) => {
                    tracing::warn!("Failed to drop document {}: {}", doc_id, e);
                }
            }
        }
    }

    market_state.is_evaluating = false;
}

/// Pull a document from origin, verify hash, store locally, report pin
async fn host_document(
    api_url: &str,
    access_token: &str,
    node_id: &str,
    opportunity: &MarketOpportunity,
    cache_dir: &Path,
) -> Result<HostedDocument, String> {
    // Download document body
    let file_size = crate::sync::cache_document_for_serving(
        api_url,
        access_token,
        &opportunity.document_id,
        &opportunity.corpus_id,
        &opportunity.body_hash,
        cache_dir,
    ).await?;

    // Report pin to Wire API
    let client = reqwest::Client::new();
    let url = format!("{}/api/v1/node/host", api_url);
    let body = serde_json::json!({
        "node_id": node_id,
        "document_id": opportunity.document_id,
        "corpus_id": opportunity.corpus_id,
        "body_hash": opportunity.body_hash,
    });

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .json(&body)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("Host report failed: {}", e))?;

    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        tracing::warn!("Host report failed: {}", text);
    }

    tracing::info!("Hosted document: {} ({} bytes)", opportunity.document_id, file_size);

    Ok(HostedDocument {
        document_id: opportunity.document_id.clone(),
        corpus_id: opportunity.corpus_id.clone(),
        body_hash: opportunity.body_hash.clone(),
        size_bytes: file_size,
        pulls_served: 0,
        credits_earned: 0.0,
        hosted_since: chrono::Utc::now().to_rfc3339(),
    })
}

/// Drop a document: delete local file, report to API
async fn drop_document(
    api_url: &str,
    access_token: &str,
    node_id: &str,
    document_id: &str,
    cache_dir: &Path,
    market_state: &mut MarketState,
) -> Result<(), String> {
    // Find corpus_id for this document
    let corpus_id = market_state.hosted_documents.get(document_id)
        .map(|d| d.corpus_id.clone())
        .unwrap_or_default();

    // Delete local cached file
    crate::sync::delete_cached_document(cache_dir, &corpus_id, document_id).await?;

    // Report drop to API
    let client = reqwest::Client::new();
    let url = format!("{}/api/v1/node/drop", api_url);
    let body = serde_json::json!({
        "node_id": node_id,
        "document_id": document_id,
    });

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .json(&body)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("Drop report failed: {}", e))?;

    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        tracing::warn!("Drop report response: {}", text);
    }

    // Remove from hosted documents
    if let Some(doc) = market_state.hosted_documents.remove(document_id) {
        market_state.total_hosted_bytes = market_state.total_hosted_bytes.saturating_sub(doc.size_bytes);
    }

    Ok(())
}
