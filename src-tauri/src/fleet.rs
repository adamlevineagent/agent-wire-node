// fleet.rs — Fleet roster and fleet dispatch client.
//
// Stores same-operator peer nodes discovered via heartbeat and direct
// fleet peer announcements. Provides fleet dispatch (POST to peer's
// tunnel) and fleet announce (POST to all peers on state change).
//
// Fleet routing is direct peer-to-peer via Cloudflare tunnels. No
// credits, no exchange, no relay. Same operator's hardware — cost is
// electricity only.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::pyramid::llm::HTTP_CLIENT;

/// A fleet peer node (same operator, different hardware).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetPeer {
    pub node_id: String,
    pub name: String,
    pub tunnel_url: String,
    /// Model IDs this peer has loaded (Ollama local models).
    pub models_loaded: Vec<String>,
    /// Per-model queue depth at the peer (model_id -> depth).
    pub queue_depths: HashMap<String, usize>,
    pub last_seen: chrono::DateTime<chrono::Utc>,
}

/// Fleet roster — all known same-operator peers.
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct FleetRoster {
    /// node_id -> FleetPeer
    pub peers: HashMap<String, FleetPeer>,
    /// Wire-signed JWT for fleet authentication (refreshed every heartbeat).
    pub fleet_jwt: Option<String>,
    /// This node's operator_id (for same-operator verification).
    pub self_operator_id: Option<String>,
}

impl FleetRoster {
    /// Update roster from heartbeat fleet_roster response.
    ///
    /// Merges rather than replacing wholesale — direct announcements
    /// may carry fresher model/queue data than the heartbeat snapshot.
    pub fn update_from_heartbeat(
        &mut self,
        peers: Vec<HeartbeatFleetEntry>,
        fleet_jwt: Option<String>,
    ) {
        let now = chrono::Utc::now();
        for entry in peers {
            let peer = self
                .peers
                .entry(entry.node_id.clone())
                .or_insert_with(|| FleetPeer {
                    node_id: entry.node_id.clone(),
                    name: entry.name.clone(),
                    tunnel_url: entry.tunnel_url.clone(),
                    models_loaded: Vec::new(),
                    queue_depths: HashMap::new(),
                    last_seen: now,
                });
            // Heartbeat provides tunnel_url and name. Models come from
            // direct announcement (preferred) or queue state mirror.
            peer.tunnel_url = entry.tunnel_url;
            peer.name = entry.name;
            peer.last_seen = now;
        }
        if fleet_jwt.is_some() {
            self.fleet_jwt = fleet_jwt;
        }
    }

    /// Update from a direct fleet peer announcement.
    pub fn update_from_announcement(&mut self, announcement: FleetAnnouncement) {
        let now = chrono::Utc::now();
        let peer = self
            .peers
            .entry(announcement.node_id.clone())
            .or_insert_with(|| FleetPeer {
                node_id: announcement.node_id.clone(),
                name: announcement.name.clone().unwrap_or_default(),
                tunnel_url: announcement.tunnel_url.clone(),
                models_loaded: Vec::new(),
                queue_depths: HashMap::new(),
                last_seen: now,
            });
        peer.tunnel_url = announcement.tunnel_url;
        peer.models_loaded = announcement.models_loaded;
        peer.queue_depths = announcement.queue_depths;
        peer.last_seen = now;
        if let Some(name) = announcement.name {
            peer.name = name;
        }
    }

    /// Remove a peer (went offline or fleet dispatch failed).
    pub fn remove_peer(&mut self, node_id: &str) {
        self.peers.remove(node_id);
    }

    /// Find a fleet peer that has the given model loaded with the
    /// lowest queue depth. Returns None if no peer qualifies (stale
    /// peers older than 120s are excluded).
    pub fn find_peer_for_model(&self, model_id: &str) -> Option<&FleetPeer> {
        let staleness_limit = chrono::Utc::now() - chrono::Duration::seconds(120);
        self.peers
            .values()
            .filter(|p| p.last_seen > staleness_limit)
            .filter(|p| p.models_loaded.contains(&model_id.to_string()))
            .min_by_key(|p| p.queue_depths.get(model_id).copied().unwrap_or(0))
    }
}

// ── Heartbeat response shapes ─────────────────────────────────────────────

/// Shape of a fleet roster entry from the heartbeat response.
#[derive(Debug, Clone, Deserialize)]
pub struct HeartbeatFleetEntry {
    pub node_id: String,
    pub name: String,
    pub tunnel_url: String,
}

// ── Fleet announcement (peer-to-peer) ─────────────────────────────────────

/// Shape of a direct fleet peer announcement.
/// Sent when: startup, model load/unload, shutdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetAnnouncement {
    pub node_id: String,
    pub name: Option<String>,
    pub tunnel_url: String,
    pub models_loaded: Vec<String>,
    pub queue_depths: HashMap<String, usize>,
    pub operator_id: String,
}

// ── Fleet dispatch request/response (LLM call forwarding) ─────────────────

/// Shape of a fleet dispatch request (sent TO a peer node via HTTP POST).
/// Fields match the QueueEntry shape: system_prompt + user_prompt as
/// separate strings, temperature as f32, max_tokens as usize.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetDispatchRequest {
    pub model: String,
    pub system_prompt: String,
    pub user_prompt: String,
    pub temperature: f32,
    pub max_tokens: usize,
    pub response_format: Option<serde_json::Value>,
    /// Fleet JWT for authentication (sent in both body and Authorization header).
    pub fleet_jwt: String,
}

/// Shape of a fleet dispatch response (returned FROM a peer node).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetDispatchResponse {
    pub content: String,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub model: String,
    pub finish_reason: Option<String>,
}

// ── Fleet dispatch client ─────────────────────────────────────────────────

/// Dispatch an LLM call to a fleet peer. Returns the response directly.
///
/// Uses the shared HTTP_CLIENT for connection reuse.
/// Timeout: 120s (LLM calls can be slow on large prompts).
/// TODO: contribution-driven timeout (Pillar 37)
pub async fn fleet_dispatch(
    peer: &FleetPeer,
    request: &FleetDispatchRequest,
) -> Result<FleetDispatchResponse, String> {
    let url = format!("{}/v1/compute/fleet-dispatch", peer.tunnel_url);

    let resp = HTTP_CLIENT
        .post(&url)
        .header("Authorization", format!("Bearer {}", request.fleet_jwt))
        .header("Content-Type", "application/json")
        // 120s timeout for fleet dispatch — LLM calls can be slow.
        // TODO: contribution-driven (Pillar 37)
        .timeout(std::time::Duration::from_secs(120))
        .json(request)
        .send()
        .await
        .map_err(|e| format!("Fleet dispatch to {} failed: {}", peer.node_id, e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!(
            "Fleet dispatch to {} returned {}: {}",
            peer.node_id, status, text
        ));
    }

    resp.json::<FleetDispatchResponse>()
        .await
        .map_err(|e| format!("Fleet dispatch response parse error: {}", e))
}

// ── Fleet announce (fire-and-forget to all peers) ─────────────────────────

/// Announce this node's state to all known fleet peers.
/// Called on startup, model load/unload, and going offline.
///
/// Fire-and-forget: each peer gets a tokio::spawn'd POST. Slow or dead
/// peers do not block the caller.
pub async fn announce_to_fleet(roster: &FleetRoster, self_announcement: &FleetAnnouncement) {
    let jwt = match &roster.fleet_jwt {
        Some(j) => j.clone(),
        None => {
            tracing::debug!("No fleet JWT, skipping fleet announce");
            return;
        }
    };

    for peer in roster.peers.values() {
        let url = format!("{}/v1/fleet/announce", peer.tunnel_url);
        let body = serde_json::to_value(self_announcement).unwrap_or_default();
        let jwt_clone = jwt.clone();
        let peer_id = peer.node_id.clone();

        // Fire-and-forget per peer. Don't block on slow/dead peers.
        tokio::spawn(async move {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_default();

            match client
                .post(&url)
                .header("Authorization", format!("Bearer {}", jwt_clone))
                .json(&body)
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    tracing::debug!("Fleet announce to {} succeeded", peer_id);
                }
                Ok(resp) => {
                    tracing::warn!("Fleet announce to {} failed: {}", peer_id, resp.status());
                }
                Err(e) => {
                    tracing::warn!("Fleet announce to {} error: {}", peer_id, e);
                }
            }
        });
    }
}
