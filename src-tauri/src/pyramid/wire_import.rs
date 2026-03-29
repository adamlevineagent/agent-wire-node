// pyramid/wire_import.rs — Wire chain import client
//
// Fetches chain definitions and question sets from the Wire marketplace.
// Used by the chain resolver for two-tier lookup (local first, then remote).
// Phase 4.2: Chain import + compiler decoupling.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

// ─── Types ───────────────────────────────────────────────────

/// A chain definition imported from the Wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportedChain {
    /// Wire contribution ID
    pub id: String,
    /// Human-readable title
    pub title: String,
    /// Short teaser/description
    pub teaser: String,
    /// Content type this chain targets (code, document, conversation)
    pub content_type: Option<String>,
    /// The action definition JSON (steps, permissions, metadata)
    pub definition: serde_json::Value,
    /// Topics/tags from the Wire
    pub topics: Vec<String>,
    /// Structured data payload (if present)
    pub structured_data: Option<serde_json::Value>,
    /// When this was fetched
    pub fetched_at: String,
    /// Wire contribution type
    pub contribution_type: Option<String>,
}

/// A question set imported from the Wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportedQuestionSet {
    /// Wire contribution ID
    pub id: String,
    /// Human-readable title
    pub title: String,
    /// Short teaser/description
    pub teaser: String,
    /// The question_set_definition from structured_data
    pub question_set_definition: serde_json::Value,
    /// Topics/tags
    pub topics: Vec<String>,
    /// When this was fetched
    pub fetched_at: String,
}

/// Search result from Wire query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainSearchResult {
    pub id: String,
    pub title: String,
    pub teaser: String,
    pub content_type: Option<String>,
    pub topics: Vec<String>,
    pub significance: f64,
    pub creator_pseudonym: String,
    pub avg_accuracy: Option<f64>,
    pub avg_usefulness: Option<f64>,
}

/// Error types for Wire import operations.
#[derive(Debug, thiserror::Error)]
pub enum WireImportError {
    #[error("network error: {0}")]
    Network(String),
    #[error("authentication failed: {0}")]
    AuthFailed(String),
    #[error("contribution not found: {0}")]
    NotFound(String),
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("timeout after {0:?}")]
    Timeout(Duration),
    #[error("rate limited, retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },
}

// ─── Cache ───────────────────────────────────────────────────

/// Cached entry with TTL tracking.
#[derive(Debug, Clone)]
struct CacheEntry<T: Clone> {
    value: T,
    fetched_at: Instant,
}

/// Simple in-memory cache with TTL.
#[derive(Debug)]
pub struct ImportCache {
    chains: HashMap<String, CacheEntry<ImportedChain>>,
    question_sets: HashMap<String, CacheEntry<ImportedQuestionSet>>,
    ttl: Duration,
}

impl ImportCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            chains: HashMap::new(),
            question_sets: HashMap::new(),
            ttl,
        }
    }

    pub fn get_chain(&self, id: &str) -> Option<&ImportedChain> {
        self.chains.get(id).and_then(|entry| {
            if entry.fetched_at.elapsed() < self.ttl {
                Some(&entry.value)
            } else {
                None
            }
        })
    }

    pub fn put_chain(&mut self, id: String, chain: ImportedChain) {
        self.chains.insert(
            id,
            CacheEntry {
                value: chain,
                fetched_at: Instant::now(),
            },
        );
    }

    pub fn get_question_set(&self, id: &str) -> Option<&ImportedQuestionSet> {
        self.question_sets.get(id).and_then(|entry| {
            if entry.fetched_at.elapsed() < self.ttl {
                Some(&entry.value)
            } else {
                None
            }
        })
    }

    pub fn put_question_set(&mut self, id: String, qs: ImportedQuestionSet) {
        self.question_sets.insert(
            id,
            CacheEntry {
                value: qs,
                fetched_at: Instant::now(),
            },
        );
    }

    /// Remove expired entries from the cache.
    pub fn evict_expired(&mut self) {
        self.chains
            .retain(|_, entry| entry.fetched_at.elapsed() < self.ttl);
        self.question_sets
            .retain(|_, entry| entry.fetched_at.elapsed() < self.ttl);
    }

    /// Clear all cached entries.
    pub fn clear(&mut self) {
        self.chains.clear();
        self.question_sets.clear();
    }
}

// ─── Client ──────────────────────────────────────────────────

/// HTTP client for fetching chain definitions from the Wire marketplace.
pub struct WireImportClient {
    /// Wire API base URL (e.g., "https://newsbleach.com")
    pub wire_url: String,
    /// Agent's Wire auth token
    pub auth_token: String,
    /// HTTP client with timeout
    client: reqwest::Client,
    /// In-memory cache
    cache: Arc<Mutex<ImportCache>>,
}

impl WireImportClient {
    /// Create a new import client.
    ///
    /// `cache_ttl` controls how long imported chains are cached before re-fetching.
    /// Default: 1 hour.
    pub fn new(wire_url: String, auth_token: String, cache_ttl: Option<Duration>) -> Self {
        let ttl = cache_ttl.unwrap_or(Duration::from_secs(3600));
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("failed to build HTTP client");

        Self {
            wire_url,
            auth_token,
            client,
            cache: Arc::new(Mutex::new(ImportCache::new(ttl))),
        }
    }

    /// Create a client with a shared cache (for use in tests or shared state).
    pub fn with_cache(
        wire_url: String,
        auth_token: String,
        cache: Arc<Mutex<ImportCache>>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("failed to build HTTP client");

        Self {
            wire_url,
            auth_token,
            client,
            cache,
        }
    }

    /// Fetch a single chain definition from the Wire by contribution ID.
    ///
    /// Checks the cache first. On cache miss, fetches from the Wire API
    /// via `GET /api/v1/explorer/contribution/{id}`.
    pub async fn fetch_chain(&self, contribution_id: &str) -> Result<ImportedChain> {
        // Check cache
        {
            let cache = self.cache.lock().await;
            if let Some(cached) = cache.get_chain(contribution_id) {
                tracing::debug!(id = contribution_id, "wire import: cache hit for chain");
                return Ok(cached.clone());
            }
        }

        tracing::info!(
            id = contribution_id,
            "wire import: fetching chain from Wire"
        );

        let url = format!(
            "{}/api/v1/explorer/contribution/{}",
            self.wire_url.trim_end_matches('/'),
            contribution_id
        );

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.auth_token))
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    WireImportError::Timeout(Duration::from_secs(30))
                } else {
                    WireImportError::Network(e.to_string())
                }
            })
            .context("wire import: fetch_chain request failed")?;

        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(WireImportError::NotFound(contribution_id.to_string()).into());
        }
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(WireImportError::AuthFailed(format!("status {}", status)).into());
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(60);
            return Err(WireImportError::RateLimited {
                retry_after_secs: retry_after,
            }
            .into());
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(WireImportError::Network(format!(
                "unexpected status {}: {}",
                status,
                body.chars().take(200).collect::<String>()
            ))
            .into());
        }

        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| WireImportError::InvalidResponse(e.to_string()))
            .context("wire import: failed to parse response JSON")?;

        let contribution = body.get("contribution").ok_or_else(|| {
            WireImportError::InvalidResponse("missing 'contribution' field".into())
        })?;

        let chain = ImportedChain {
            id: contribution["id"]
                .as_str()
                .unwrap_or(contribution_id)
                .to_string(),
            title: contribution["title"].as_str().unwrap_or("").to_string(),
            teaser: contribution["teaser"]
                .as_str()
                .map(|s| s.to_string())
                .or_else(|| {
                    contribution["body"].as_str().map(|b| {
                        // Truncate at a char boundary to avoid panic on multi-byte UTF-8
                        let end = b
                            .char_indices()
                            .take_while(|(i, _)| *i < 300)
                            .last()
                            .map(|(i, c)| i + c.len_utf8())
                            .unwrap_or(0);
                        b[..end].to_string()
                    })
                })
                .unwrap_or_default(),
            content_type: contribution["content_type"]
                .as_str()
                .or_else(|| {
                    // Try to extract from topics
                    contribution["topics"].as_array().and_then(|topics| {
                        topics.iter().find_map(|t| {
                            let s = t.as_str()?;
                            if ["code", "document", "conversation"].contains(&s) {
                                Some(s)
                            } else {
                                None
                            }
                        })
                    })
                })
                .map(|s| s.to_string()),
            definition: contribution
                .get("definition")
                .or_else(|| contribution.get("structured_data"))
                .cloned()
                .unwrap_or(serde_json::Value::Null),
            topics: contribution["topics"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
            structured_data: contribution.get("structured_data").cloned(),
            fetched_at: chrono::Utc::now().to_rfc3339(),
            contribution_type: contribution["contribution_type"]
                .as_str()
                .map(|s| s.to_string()),
        };

        // Cache the result
        {
            let mut cache = self.cache.lock().await;
            cache.put_chain(contribution_id.to_string(), chain.clone());
        }

        Ok(chain)
    }

    /// Search the Wire for chain definitions.
    ///
    /// Uses `GET /api/v1/wire/query` with query parameters.
    pub async fn search_chains(
        &self,
        query: &str,
        content_type: Option<&str>,
    ) -> Result<Vec<ChainSearchResult>> {
        let mut url = format!(
            "{}/api/v1/wire/query?text={}",
            self.wire_url.trim_end_matches('/'),
            urlencoding::encode(query),
        );

        // Filter to action type for chain search
        url.push_str("&type=action");

        if let Some(ct) = content_type {
            url.push_str(&format!("&topics={}", urlencoding::encode(ct)));
        }

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.auth_token))
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    WireImportError::Timeout(Duration::from_secs(30))
                } else {
                    WireImportError::Network(e.to_string())
                }
            })
            .context("wire import: search_chains request failed")?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(WireImportError::AuthFailed(format!("status {}", status)).into());
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(WireImportError::Network(format!(
                "search failed with status {}: {}",
                status,
                body.chars().take(200).collect::<String>()
            ))
            .into());
        }

        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| WireImportError::InvalidResponse(e.to_string()))
            .context("wire import: failed to parse search response")?;

        let items = body["items"]
            .as_array()
            .ok_or_else(|| WireImportError::InvalidResponse("missing 'items' array".into()))?;

        let results: Vec<ChainSearchResult> = items
            .iter()
            .filter_map(|item| {
                Some(ChainSearchResult {
                    id: item["item_id"].as_str()?.to_string(),
                    title: item["title"].as_str().unwrap_or("").to_string(),
                    teaser: item["teaser"].as_str().unwrap_or("").to_string(),
                    content_type: item["content_type"].as_str().map(|s| s.to_string()),
                    topics: item["topics"]
                        .as_array()
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default(),
                    significance: item["significance"].as_f64().unwrap_or(0.0),
                    creator_pseudonym: item["creator_pseudonym"].as_str().unwrap_or("").to_string(),
                    avg_accuracy: item["avg_accuracy"].as_f64(),
                    avg_usefulness: item["avg_usefulness"].as_f64(),
                })
            })
            .collect();

        Ok(results)
    }

    /// Fetch a question set from the Wire by contribution ID.
    ///
    /// The question set definition is expected in the `structured_data` field
    /// of the contribution.
    pub async fn fetch_question_set(&self, contribution_id: &str) -> Result<ImportedQuestionSet> {
        // Check cache
        {
            let cache = self.cache.lock().await;
            if let Some(cached) = cache.get_question_set(contribution_id) {
                tracing::debug!(
                    id = contribution_id,
                    "wire import: cache hit for question set"
                );
                return Ok(cached.clone());
            }
        }

        tracing::info!(
            id = contribution_id,
            "wire import: fetching question set from Wire"
        );

        let url = format!(
            "{}/api/v1/explorer/contribution/{}",
            self.wire_url.trim_end_matches('/'),
            contribution_id
        );

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.auth_token))
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    WireImportError::Timeout(Duration::from_secs(30))
                } else {
                    WireImportError::Network(e.to_string())
                }
            })
            .context("wire import: fetch_question_set request failed")?;

        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(WireImportError::NotFound(contribution_id.to_string()).into());
        }
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(WireImportError::AuthFailed(format!("status {}", status)).into());
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(WireImportError::Network(format!(
                "unexpected status {}: {}",
                status,
                body.chars().take(200).collect::<String>()
            ))
            .into());
        }

        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| WireImportError::InvalidResponse(e.to_string()))
            .context("wire import: failed to parse response JSON")?;

        let contribution = body.get("contribution").ok_or_else(|| {
            WireImportError::InvalidResponse("missing 'contribution' field".into())
        })?;

        // Verify this is a question_set type
        let contrib_type = contribution["type"].as_str().unwrap_or("");
        if contrib_type != "question_set" {
            return Err(WireImportError::InvalidResponse(format!(
                "expected type 'question_set', got '{}'",
                contrib_type
            ))
            .into());
        }

        let structured_data = contribution.get("structured_data").ok_or_else(|| {
            WireImportError::InvalidResponse(
                "question_set contribution has no structured_data".into(),
            )
        })?;

        let qs = ImportedQuestionSet {
            id: contribution["id"]
                .as_str()
                .unwrap_or(contribution_id)
                .to_string(),
            title: contribution["title"].as_str().unwrap_or("").to_string(),
            teaser: contribution["teaser"]
                .as_str()
                .map(|s| s.to_string())
                .or_else(|| {
                    contribution["body"].as_str().map(|b| {
                        let end = b
                            .char_indices()
                            .take_while(|(i, _)| *i < 300)
                            .last()
                            .map(|(i, c)| i + c.len_utf8())
                            .unwrap_or(0);
                        b[..end].to_string()
                    })
                })
                .unwrap_or_default(),
            question_set_definition: structured_data.clone(),
            topics: contribution["topics"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
            fetched_at: chrono::Utc::now().to_rfc3339(),
        };

        // Cache the result
        {
            let mut cache = self.cache.lock().await;
            cache.put_question_set(contribution_id.to_string(), qs.clone());
        }

        Ok(qs)
    }

    /// Get a reference to the cache for external management.
    pub fn cache(&self) -> &Arc<Mutex<ImportCache>> {
        &self.cache
    }
}

// ─── Remote Pyramid Client (WS-ONLINE-C) ─────────────────────

/// Response from a remote pyramid apex query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteApexResponse {
    pub slug: String,
    pub node: serde_json::Value,
}

/// Response from a remote pyramid drill query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteDrillResponse {
    pub slug: String,
    pub node: serde_json::Value,
    pub children: Vec<serde_json::Value>,
}

/// Response from a remote pyramid search query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteSearchResponse {
    pub slug: String,
    pub results: Vec<serde_json::Value>,
}

/// Response from a remote pyramid entities query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteEntitiesResponse {
    pub slug: String,
    pub entities: Vec<serde_json::Value>,
}

/// Response from a remote pyramid tree query (C2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteTreeResponse {
    pub slug: String,
    pub tree: Vec<serde_json::Value>,
}

/// Response from a remote pyramid export query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteExportResponse {
    pub slug: String,
    pub nodes: Vec<serde_json::Value>,
}

/// Cost preview response from a remote serving node (WS-ONLINE-H).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteCostPreview {
    /// Stamp fee (always 1 credit)
    pub stamp: u64,
    /// Access price (0 for public pyramids)
    pub access_price: i64,
    /// Total cost (stamp + access_price)
    pub total: i64,
    /// Pyramid slug
    pub slug: String,
    /// Serving node's operator ID (needed for payment-intent)
    pub serving_node_id: String,
}

/// HTTP client for querying remote pyramids via tunnel URLs (WS-ONLINE-C).
///
/// Each method will eventually integrate payment flow (WS-ONLINE-H),
/// but for now just does authenticated requests with Wire JWT.
pub struct RemotePyramidClient {
    /// The remote node's tunnel URL (e.g., "https://abcd1234.tunnel.wire.example.com")
    pub tunnel_url: String,
    /// Wire JWT for authenticating with the remote node
    pub wire_jwt: String,
    /// Wire server URL for obtaining tokens (used in WS-ONLINE-H payment flow)
    pub wire_server_url: String,
    /// HTTP client
    client: reqwest::Client,
}

impl RemotePyramidClient {
    /// Create a new remote pyramid client.
    pub fn new(tunnel_url: String, wire_jwt: String, wire_server_url: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("failed to build HTTP client for RemotePyramidClient");

        Self {
            tunnel_url,
            wire_jwt,
            wire_server_url,
            client,
        }
    }

    /// Update the Wire JWT (e.g., after token refresh).
    pub fn set_jwt(&mut self, jwt: String) {
        self.wire_jwt = jwt;
    }

    /// GET /pyramid/{slug}/apex — fetch the apex node of a remote pyramid.
    pub async fn remote_apex(&self, slug: &str) -> Result<RemoteApexResponse> {
        let url = format!(
            "{}/pyramid/{}/apex",
            self.tunnel_url.trim_end_matches('/'),
            slug
        );

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.wire_jwt))
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    WireImportError::Timeout(Duration::from_secs(30))
                } else {
                    WireImportError::Network(e.to_string())
                }
            })
            .context("remote pyramid: apex request failed")?;

        self.check_response_status(&response)?;

        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| WireImportError::InvalidResponse(e.to_string()))
            .context("remote pyramid: failed to parse apex response")?;

        Ok(RemoteApexResponse {
            slug: slug.to_string(),
            node: body,
        })
    }

    /// GET /pyramid/{slug}/drill/{node_id} — drill into a specific node.
    pub async fn remote_drill(&self, slug: &str, node_id: &str) -> Result<RemoteDrillResponse> {
        let url = format!(
            "{}/pyramid/{}/drill/{}",
            self.tunnel_url.trim_end_matches('/'),
            slug,
            node_id
        );

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.wire_jwt))
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    WireImportError::Timeout(Duration::from_secs(30))
                } else {
                    WireImportError::Network(e.to_string())
                }
            })
            .context("remote pyramid: drill request failed")?;

        self.check_response_status(&response)?;

        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| WireImportError::InvalidResponse(e.to_string()))
            .context("remote pyramid: failed to parse drill response")?;

        Ok(RemoteDrillResponse {
            slug: slug.to_string(),
            node: body.get("node").cloned().unwrap_or(serde_json::Value::Null),
            children: body
                .get("children")
                .and_then(|c| c.as_array())
                .cloned()
                .unwrap_or_default(),
        })
    }

    /// GET /pyramid/{slug}/search?q={query} — search a remote pyramid.
    pub async fn remote_search(
        &self,
        slug: &str,
        query: &str,
    ) -> Result<RemoteSearchResponse> {
        let url = format!(
            "{}/pyramid/{}/search?q={}",
            self.tunnel_url.trim_end_matches('/'),
            slug,
            urlencoding::encode(query)
        );

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.wire_jwt))
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    WireImportError::Timeout(Duration::from_secs(30))
                } else {
                    WireImportError::Network(e.to_string())
                }
            })
            .context("remote pyramid: search request failed")?;

        self.check_response_status(&response)?;

        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| WireImportError::InvalidResponse(e.to_string()))
            .context("remote pyramid: failed to parse search response")?;

        let results = body
            .get("results")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();

        Ok(RemoteSearchResponse {
            slug: slug.to_string(),
            results,
        })
    }

    /// GET /pyramid/{slug}/export — export all nodes from a remote pyramid.
    ///
    /// This is a heavier operation; rate limiting applies on the serving node.
    pub async fn remote_export(&self, slug: &str) -> Result<RemoteExportResponse> {
        let url = format!(
            "{}/pyramid/{}/export",
            self.tunnel_url.trim_end_matches('/'),
            slug
        );

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.wire_jwt))
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    WireImportError::Timeout(Duration::from_secs(30))
                } else {
                    WireImportError::Network(e.to_string())
                }
            })
            .context("remote pyramid: export request failed")?;

        self.check_response_status(&response)?;

        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| WireImportError::InvalidResponse(e.to_string()))
            .context("remote pyramid: failed to parse export response")?;

        let nodes = body
            .get("nodes")
            .and_then(|n| n.as_array())
            .cloned()
            .unwrap_or_default();

        Ok(RemoteExportResponse {
            slug: slug.to_string(),
            nodes,
        })
    }

    /// GET /pyramid/{slug}/entities — fetch entity list from a remote pyramid.
    pub async fn remote_entities(&self, slug: &str) -> Result<RemoteEntitiesResponse> {
        let url = format!(
            "{}/pyramid/{}/entities",
            self.tunnel_url.trim_end_matches('/'),
            slug
        );

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.wire_jwt))
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    WireImportError::Timeout(Duration::from_secs(30))
                } else {
                    WireImportError::Network(e.to_string())
                }
            })
            .context("remote pyramid: entities request failed")?;

        self.check_response_status(&response)?;

        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| WireImportError::InvalidResponse(e.to_string()))
            .context("remote pyramid: failed to parse entities response")?;

        let entities = body
            .get("entities")
            .and_then(|e| e.as_array())
            .cloned()
            .unwrap_or_default();

        Ok(RemoteEntitiesResponse {
            slug: slug.to_string(),
            entities,
        })
    }

    /// Get tree of a remote pyramid (C2).
    pub async fn remote_tree(&self, slug: &str) -> Result<RemoteTreeResponse> {
        let url = format!(
            "{}/pyramid/{}/tree",
            self.tunnel_url.trim_end_matches('/'),
            slug
        );

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.wire_jwt))
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    WireImportError::Timeout(Duration::from_secs(30))
                } else {
                    WireImportError::Network(e.to_string())
                }
            })
            .context("remote pyramid: tree request failed")?;

        self.check_response_status(&response)?;

        let tree: Vec<serde_json::Value> = response
            .json()
            .await
            .map_err(|e| WireImportError::InvalidResponse(e.to_string()))
            .context("remote pyramid: failed to parse tree response")?;

        Ok(RemoteTreeResponse {
            slug: slug.to_string(),
            tree,
        })
    }

    /// Check HTTP response status and return appropriate error.
    fn check_response_status(&self, response: &reqwest::Response) -> Result<()> {
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }

        match status {
            reqwest::StatusCode::NOT_FOUND => {
                Err(WireImportError::NotFound("remote pyramid or slug not found".into()).into())
            }
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                Err(WireImportError::AuthFailed(format!("status {}", status)).into())
            }
            reqwest::StatusCode::TOO_MANY_REQUESTS => {
                Err(WireImportError::RateLimited {
                    retry_after_secs: 60,
                }
                .into())
            }
            _ => Err(WireImportError::Network(format!(
                "remote pyramid returned status {}",
                status
            ))
            .into()),
        }
    }
}

// ─── WS-ONLINE-H: Cost preview and payment integration ──────

impl RemotePyramidClient {
    /// GET /pyramid/{slug}/query-cost — fetch cost preview from a remote serving node.
    ///
    /// Returns the stamp (1 credit), access_price, and total cost for querying
    /// this pyramid. The caller uses this to decide whether to proceed and to
    /// call POST /api/v1/wire/payment-intent on the Wire server.
    pub async fn query_cost(
        &self,
        slug: &str,
        query_type: &str,
        node_id: Option<&str>,
    ) -> Result<RemoteCostPreview> {
        let mut url = format!(
            "{}/pyramid/{}/query-cost?query_type={}",
            self.tunnel_url.trim_end_matches('/'),
            slug,
            urlencoding::encode(query_type)
        );
        if let Some(nid) = node_id {
            url.push_str(&format!("&node_id={}", urlencoding::encode(nid)));
        }

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.wire_jwt))
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    WireImportError::Timeout(Duration::from_secs(30))
                } else {
                    WireImportError::Network(e.to_string())
                }
            })
            .context("remote pyramid: query-cost request failed")?;

        self.check_response_status(&response)?;

        let preview: RemoteCostPreview = response
            .json()
            .await
            .map_err(|e| WireImportError::InvalidResponse(e.to_string()))
            .context("remote pyramid: failed to parse query-cost response")?;

        tracing::debug!(
            slug = %slug,
            stamp = %preview.stamp,
            access_price = %preview.access_price,
            total = %preview.total,
            serving_node = %preview.serving_node_id,
            "received cost preview from remote node"
        );

        Ok(preview)
    }

    /// Fetch apex with cost preview (WS-ONLINE-H).
    ///
    /// Calls query-cost first, then fetches the apex. Returns both the apex
    /// response and the cost preview so the caller can decide about payment.
    ///
    /// TODO(WS-ONLINE-H): When Wire server payment-intent endpoint exists,
    /// this method should: (1) call query_cost, (2) call payment-intent on
    /// the Wire server to get a payment_token, (3) include the payment_token
    /// as X-Payment-Token header on the apex request.
    pub async fn remote_apex_with_cost(
        &self,
        slug: &str,
    ) -> Result<(RemoteApexResponse, RemoteCostPreview)> {
        let cost = self.query_cost(slug, "apex", None).await?;
        let apex = self.remote_apex(slug).await?;
        Ok((apex, cost))
    }

    /// Fetch drill with cost preview (WS-ONLINE-H).
    ///
    /// TODO(WS-ONLINE-H): Integrate payment-intent/token flow when Wire server ready.
    pub async fn remote_drill_with_cost(
        &self,
        slug: &str,
        node_id: &str,
    ) -> Result<(RemoteDrillResponse, RemoteCostPreview)> {
        let cost = self.query_cost(slug, "drill", Some(node_id)).await?;
        let drill = self.remote_drill(slug, node_id).await?;
        Ok((drill, cost))
    }

    /// Fetch search with cost preview (WS-ONLINE-H).
    ///
    /// TODO(WS-ONLINE-H): Integrate payment-intent/token flow when Wire server ready.
    pub async fn remote_search_with_cost(
        &self,
        slug: &str,
        query: &str,
    ) -> Result<(RemoteSearchResponse, RemoteCostPreview)> {
        let cost = self.query_cost(slug, "search", None).await?;
        let search = self.remote_search(slug, query).await?;
        Ok((search, cost))
    }

    /// Fetch export with cost preview (WS-ONLINE-H).
    ///
    /// Note: Export queries may take longer than 60s payment token TTL.
    /// Per WS-ONLINE-H design, payment should be redeemed BEFORE executing
    /// the export (payment collected upfront). This method fetches cost first;
    /// the actual payment-intent + redeem-before-execute flow is a TODO.
    pub async fn remote_export_with_cost(
        &self,
        slug: &str,
    ) -> Result<(RemoteExportResponse, RemoteCostPreview)> {
        let cost = self.query_cost(slug, "export", None).await?;
        let export = self.remote_export(slug).await?;
        Ok((export, cost))
    }
}

// ─── WS-ONLINE-D: Pull remote pyramid for pinning ───────────

impl RemotePyramidClient {
    /// Pull a remote pyramid's full node data for local pinning.
    ///
    /// Calls GET /pyramid/{slug}/export and parses the response nodes
    /// into Vec<PyramidNode>. Returns an error if the export fails or
    /// the response cannot be parsed.
    pub async fn pull_remote_pyramid(
        &self,
        slug: &str,
    ) -> Result<Vec<super::types::PyramidNode>> {
        let export = self.remote_export(slug).await?;

        let mut nodes = Vec::with_capacity(export.nodes.len());
        for node_val in &export.nodes {
            let node: super::types::PyramidNode = serde_json::from_value(node_val.clone())
                .map_err(|e| {
                    WireImportError::InvalidResponse(format!(
                        "failed to parse exported node: {}",
                        e
                    ))
                })?;
            nodes.push(node);
        }

        tracing::info!(
            slug = %slug,
            node_count = nodes.len(),
            tunnel_url = %self.tunnel_url,
            "pulled remote pyramid for pinning"
        );

        Ok(nodes)
    }
}

// ─── SQLite persistence for imported chains ──────────────────

/// Initialize the imported chains table in SQLite.
pub fn init_import_tables(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_imported_chains (
            contribution_id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            teaser TEXT NOT NULL DEFAULT '',
            content_type TEXT,
            definition TEXT NOT NULL,
            structured_data TEXT,
            topics TEXT NOT NULL DEFAULT '[]',
            fetched_at TEXT NOT NULL,
            contribution_type TEXT,
            expires_at TEXT
        );

        CREATE TABLE IF NOT EXISTS pyramid_imported_question_sets (
            contribution_id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            teaser TEXT NOT NULL DEFAULT '',
            question_set_definition TEXT NOT NULL,
            topics TEXT NOT NULL DEFAULT '[]',
            fetched_at TEXT NOT NULL,
            expires_at TEXT
        );
        ",
    )?;
    Ok(())
}

/// Persist an imported chain to SQLite for offline access.
pub fn save_imported_chain(conn: &rusqlite::Connection, chain: &ImportedChain) -> Result<()> {
    let expires_at = chrono::Utc::now()
        .checked_add_signed(chrono::Duration::hours(24))
        .map(|t| t.to_rfc3339())
        .unwrap_or_default();

    conn.execute(
        "INSERT INTO pyramid_imported_chains
            (contribution_id, title, teaser, content_type, definition, structured_data, topics, fetched_at, contribution_type, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(contribution_id) DO UPDATE SET
            title = excluded.title,
            teaser = excluded.teaser,
            content_type = excluded.content_type,
            definition = excluded.definition,
            structured_data = excluded.structured_data,
            topics = excluded.topics,
            fetched_at = excluded.fetched_at,
            contribution_type = excluded.contribution_type,
            expires_at = excluded.expires_at",
        rusqlite::params![
            chain.id,
            chain.title,
            chain.teaser,
            chain.content_type,
            serde_json::to_string(&chain.definition)?,
            chain.structured_data.as_ref().map(|v| serde_json::to_string(v)).transpose()?,
            serde_json::to_string(&chain.topics)?,
            chain.fetched_at,
            chain.contribution_type,
            expires_at,
        ],
    )?;
    Ok(())
}

/// Load an imported chain from SQLite (for offline/cached access).
pub fn load_imported_chain(
    conn: &rusqlite::Connection,
    contribution_id: &str,
) -> Result<Option<ImportedChain>> {
    let mut stmt = conn.prepare(
        "SELECT contribution_id, title, teaser, content_type, definition, structured_data, topics, fetched_at, contribution_type
         FROM pyramid_imported_chains WHERE contribution_id = ?1",
    )?;

    let result = stmt.query_row(rusqlite::params![contribution_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, Option<String>>(8)?,
        ))
    });

    match result {
        Ok((
            id,
            title,
            teaser,
            content_type,
            def_str,
            sd_str,
            topics_str,
            fetched_at,
            contribution_type,
        )) => Ok(Some(ImportedChain {
            id,
            title,
            teaser,
            content_type,
            definition: serde_json::from_str(&def_str)?,
            structured_data: sd_str.map(|s| serde_json::from_str(&s)).transpose()?,
            topics: serde_json::from_str(&topics_str)?,
            fetched_at,
            contribution_type,
        })),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Save an imported question set to SQLite.
pub fn save_imported_question_set(
    conn: &rusqlite::Connection,
    qs: &ImportedQuestionSet,
) -> Result<()> {
    let expires_at = chrono::Utc::now()
        .checked_add_signed(chrono::Duration::hours(24))
        .map(|t| t.to_rfc3339())
        .unwrap_or_default();

    conn.execute(
        "INSERT INTO pyramid_imported_question_sets
            (contribution_id, title, teaser, question_set_definition, topics, fetched_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(contribution_id) DO UPDATE SET
            title = excluded.title,
            teaser = excluded.teaser,
            question_set_definition = excluded.question_set_definition,
            topics = excluded.topics,
            fetched_at = excluded.fetched_at,
            expires_at = excluded.expires_at",
        rusqlite::params![
            qs.id,
            qs.title,
            qs.teaser,
            serde_json::to_string(&qs.question_set_definition)?,
            serde_json::to_string(&qs.topics)?,
            qs.fetched_at,
            expires_at,
        ],
    )?;
    Ok(())
}

/// Load an imported question set from SQLite (for offline/cached access).
pub fn load_imported_question_set(
    conn: &rusqlite::Connection,
    contribution_id: &str,
) -> Result<Option<ImportedQuestionSet>> {
    let mut stmt = conn.prepare(
        "SELECT contribution_id, title, teaser, question_set_definition, topics, fetched_at
         FROM pyramid_imported_question_sets WHERE contribution_id = ?1",
    )?;

    let result = stmt.query_row(rusqlite::params![contribution_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
        ))
    });

    match result {
        Ok((id, title, teaser, def_str, topics_str, fetched_at)) => Ok(Some(ImportedQuestionSet {
            id,
            title,
            teaser,
            question_set_definition: serde_json::from_str(&def_str)?,
            topics: serde_json::from_str(&topics_str)?,
            fetched_at,
        })),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

// ─── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_hit_and_miss() {
        let mut cache = ImportCache::new(Duration::from_secs(3600));

        // Miss
        assert!(cache.get_chain("abc-123").is_none());

        // Insert
        let chain = ImportedChain {
            id: "abc-123".into(),
            title: "Test Chain".into(),
            teaser: "A test".into(),
            content_type: Some("code".into()),
            definition: serde_json::json!({"steps": []}),
            topics: vec!["code".into()],
            structured_data: None,
            fetched_at: "2026-03-25T00:00:00Z".into(),
            contribution_type: Some("action".into()),
        };
        cache.put_chain("abc-123".into(), chain);

        // Hit
        let hit = cache.get_chain("abc-123");
        assert!(hit.is_some());
        assert_eq!(hit.unwrap().title, "Test Chain");
    }

    #[test]
    fn test_cache_expiry() {
        let mut cache = ImportCache::new(Duration::from_millis(1));

        let chain = ImportedChain {
            id: "expire-test".into(),
            title: "Expiring".into(),
            teaser: "".into(),
            content_type: None,
            definition: serde_json::json!({}),
            topics: vec![],
            structured_data: None,
            fetched_at: "2026-03-25T00:00:00Z".into(),
            contribution_type: None,
        };
        cache.put_chain("expire-test".into(), chain);

        // Wait for expiry
        std::thread::sleep(Duration::from_millis(5));

        assert!(cache.get_chain("expire-test").is_none());
    }

    #[test]
    fn test_cache_evict_expired() {
        let mut cache = ImportCache::new(Duration::from_millis(1));

        let chain = ImportedChain {
            id: "evict-test".into(),
            title: "Evictable".into(),
            teaser: "".into(),
            content_type: None,
            definition: serde_json::json!({}),
            topics: vec![],
            structured_data: None,
            fetched_at: "2026-03-25T00:00:00Z".into(),
            contribution_type: None,
        };
        cache.put_chain("evict-test".into(), chain);

        std::thread::sleep(Duration::from_millis(5));
        cache.evict_expired();

        assert!(cache.chains.is_empty());
    }

    #[test]
    fn test_question_set_cache() {
        let mut cache = ImportCache::new(Duration::from_secs(3600));

        let qs = ImportedQuestionSet {
            id: "qs-001".into(),
            title: "Code Questions".into(),
            teaser: "Questions for code".into(),
            question_set_definition: serde_json::json!({"questions": []}),
            topics: vec!["code".into()],
            fetched_at: "2026-03-25T00:00:00Z".into(),
        };
        cache.put_question_set("qs-001".into(), qs);

        let hit = cache.get_question_set("qs-001");
        assert!(hit.is_some());
        assert_eq!(hit.unwrap().title, "Code Questions");
    }

    #[test]
    fn test_sqlite_persistence_roundtrip() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_import_tables(&conn).unwrap();

        let chain = ImportedChain {
            id: "persist-001".into(),
            title: "Persisted Chain".into(),
            teaser: "A persisted chain".into(),
            content_type: Some("document".into()),
            definition: serde_json::json!({"steps": [{"name": "extract"}]}),
            topics: vec!["document".into(), "test".into()],
            structured_data: Some(serde_json::json!({"meta": true})),
            fetched_at: "2026-03-25T12:00:00Z".into(),
            contribution_type: Some("action".into()),
        };

        save_imported_chain(&conn, &chain).unwrap();

        let loaded = load_imported_chain(&conn, "persist-001").unwrap();
        assert!(loaded.is_some());
        let loaded = loaded.unwrap();
        assert_eq!(loaded.id, "persist-001");
        assert_eq!(loaded.title, "Persisted Chain");
        assert_eq!(loaded.content_type, Some("document".into()));
        assert_eq!(loaded.topics, vec!["document", "test"]);
        assert!(loaded.structured_data.is_some());
    }

    #[test]
    fn test_sqlite_missing_chain_returns_none() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_import_tables(&conn).unwrap();

        let loaded = load_imported_chain(&conn, "nonexistent").unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn test_sqlite_upsert_updates() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_import_tables(&conn).unwrap();

        let chain_v1 = ImportedChain {
            id: "upsert-001".into(),
            title: "Version 1".into(),
            teaser: "".into(),
            content_type: None,
            definition: serde_json::json!({"v": 1}),
            topics: vec![],
            structured_data: None,
            fetched_at: "2026-03-25T00:00:00Z".into(),
            contribution_type: None,
        };
        save_imported_chain(&conn, &chain_v1).unwrap();

        let chain_v2 = ImportedChain {
            id: "upsert-001".into(),
            title: "Version 2".into(),
            teaser: "updated".into(),
            content_type: Some("code".into()),
            definition: serde_json::json!({"v": 2}),
            topics: vec!["new".into()],
            structured_data: None,
            fetched_at: "2026-03-25T01:00:00Z".into(),
            contribution_type: None,
        };
        save_imported_chain(&conn, &chain_v2).unwrap();

        let loaded = load_imported_chain(&conn, "upsert-001").unwrap().unwrap();
        assert_eq!(loaded.title, "Version 2");
        assert_eq!(loaded.content_type, Some("code".into()));
    }

    #[test]
    fn test_sqlite_question_set_roundtrip() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_import_tables(&conn).unwrap();

        let qs = ImportedQuestionSet {
            id: "qs-persist-001".into(),
            title: "Persisted QS".into(),
            teaser: "A persisted question set".into(),
            question_set_definition: serde_json::json!({"questions": [{"q": "Why?"}]}),
            topics: vec!["code".into(), "test".into()],
            fetched_at: "2026-03-25T12:00:00Z".into(),
        };

        save_imported_question_set(&conn, &qs).unwrap();

        let loaded = load_imported_question_set(&conn, "qs-persist-001").unwrap();
        assert!(loaded.is_some());
        let loaded = loaded.unwrap();
        assert_eq!(loaded.id, "qs-persist-001");
        assert_eq!(loaded.title, "Persisted QS");
        assert_eq!(loaded.topics, vec!["code", "test"]);
        assert_eq!(
            loaded.question_set_definition,
            serde_json::json!({"questions": [{"q": "Why?"}]})
        );
    }

    #[test]
    fn test_sqlite_question_set_missing_returns_none() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_import_tables(&conn).unwrap();

        let loaded = load_imported_question_set(&conn, "nonexistent").unwrap();
        assert!(loaded.is_none());
    }

    // Mock-based HTTP tests would go here in a real test suite.
    // For now, the client logic is tested through cache + SQLite persistence.
    // Integration tests against a real Wire instance belong in a separate test binary.
}
