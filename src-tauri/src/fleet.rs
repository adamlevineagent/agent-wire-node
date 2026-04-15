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
    /// Model IDs this peer has loaded (Ollama local models). Kept for observability.
    pub models_loaded: Vec<String>,
    /// Routing rule names this peer can serve locally.
    #[serde(default)]
    pub serving_rules: Vec<String>,
    /// Per-model queue depth at the peer (model_id -> depth).
    pub queue_depths: HashMap<String, usize>,
    /// Total queue depth across all models (for fleet load balancing).
    #[serde(default)]
    pub total_queue_depth: usize,
    pub last_seen: chrono::DateTime<chrono::Utc>,
    /// Handle path (e.g. "@hello/BEHEM") — present when server supports node identity.
    /// New nodes prefer this over node_id for provenance and display.
    #[serde(default)]
    pub handle_path: Option<String>,
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
                    serving_rules: Vec::new(),
                    queue_depths: HashMap::new(),
                    total_queue_depth: 0,
                    last_seen: now,
                    handle_path: None,
                });
            // Heartbeat provides tunnel_url and name. Models + serving_rules
            // come from direct announcement (preferred) or queue state mirror.
            peer.tunnel_url = entry.tunnel_url;
            peer.name = entry.name;
            peer.last_seen = now;
            // Store handle_path when the server provides it.
            if entry.handle_path.is_some() {
                peer.handle_path = entry.handle_path;
            }
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
                serving_rules: Vec::new(),
                queue_depths: HashMap::new(),
                total_queue_depth: 0,
                last_seen: now,
                handle_path: None,
            });
        peer.tunnel_url = announcement.tunnel_url;
        peer.models_loaded = announcement.models_loaded;
        peer.serving_rules = announcement.serving_rules;
        peer.queue_depths = announcement.queue_depths;
        peer.total_queue_depth = announcement.total_queue_depth;
        peer.last_seen = now;
        if let Some(name) = announcement.name {
            peer.name = name;
        }
        // Construct handle_path from announcement's node_handle + operator_handle.
        if let (Some(nh), Some(oh)) = (&announcement.node_handle, &announcement.operator_handle) {
            peer.handle_path = Some(format!("@{}/{}", oh, nh));
        }
    }

    /// Remove a peer (went offline or fleet dispatch failed).
    pub fn remove_peer(&mut self, node_id: &str) {
        self.peers.remove(node_id);
    }

    /// Find a fleet peer that can serve the given routing rule name,
    /// picking the one with the lowest total queue depth.
    /// Returns None if no peer qualifies (stale peers older than 120s
    /// are excluded).
    pub fn find_peer_for_rule(&self, rule_name: &str) -> Option<&FleetPeer> {
        let staleness_limit = chrono::Utc::now() - chrono::Duration::seconds(120);
        self.peers
            .values()
            .filter(|p| p.last_seen > staleness_limit)
            .filter(|p| p.serving_rules.contains(&rule_name.to_string()))
            .min_by_key(|p| p.total_queue_depth)
    }
}

// ── Heartbeat response shapes ─────────────────────────────────────────────

/// Shape of a fleet roster entry from the heartbeat response.
#[derive(Debug, Clone, Deserialize)]
pub struct HeartbeatFleetEntry {
    pub node_id: String,
    pub name: String,
    pub tunnel_url: String,
    /// Handle path (e.g. "@hello/BEHEM") — present when server supports node identity.
    #[serde(default)]
    pub handle_path: Option<String>,
}

// ── Fleet announcement (peer-to-peer) ─────────────────────────────────────

/// Shape of a direct fleet peer announcement.
/// Sent when: startup, model load/unload, shutdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetAnnouncement {
    pub node_id: String,
    pub name: Option<String>,
    /// Node handle local part (e.g. "BEHEM") — new field for node identity.
    #[serde(default)]
    pub node_handle: Option<String>,
    /// Operator handle (e.g. "hello") — new field for node identity.
    #[serde(default)]
    pub operator_handle: Option<String>,
    pub tunnel_url: String,
    /// Model IDs this peer has loaded (kept for observability).
    pub models_loaded: Vec<String>,
    /// Routing rule names this peer can serve locally.
    #[serde(default)]
    pub serving_rules: Vec<String>,
    pub queue_depths: HashMap<String, usize>,
    /// Total queue depth across all models (for fleet load balancing).
    #[serde(default)]
    pub total_queue_depth: usize,
    pub operator_id: String,
}

// ── Fleet dispatch request/response (LLM call forwarding) ─────────────────

/// Shape of a fleet dispatch request (sent TO a peer node via HTTP POST).
/// Dispatches by routing rule name — model names never cross node boundaries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetDispatchRequest {
    pub rule_name: String,
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
    /// The model the peer actually resolved and used (for observability).
    pub peer_model: Option<String>,
}

// ── Fleet dispatch client ─────────────────────────────────────────────────

/// Dispatch an LLM call to a fleet peer by routing rule name.
///
/// The peer resolves the rule name to a local model — model names never
/// cross node boundaries. Timeout is configurable (reads from the matched
/// rule's `max_wait_secs` in the dispatch policy).
pub async fn fleet_dispatch_by_rule(
    peer: &FleetPeer,
    rule_name: &str,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f32,
    max_tokens: usize,
    response_format: Option<&serde_json::Value>,
    fleet_jwt: &str,
    timeout_secs: u64,
) -> Result<FleetDispatchResponse, String> {
    let url = format!("{}/v1/compute/fleet-dispatch", peer.tunnel_url);

    let request = FleetDispatchRequest {
        rule_name: rule_name.to_string(),
        system_prompt: system_prompt.to_string(),
        user_prompt: user_prompt.to_string(),
        temperature,
        max_tokens,
        response_format: response_format.cloned(),
        fleet_jwt: fleet_jwt.to_string(),
    };

    let resp = HTTP_CLIENT
        .post(&url)
        .header("Authorization", format!("Bearer {}", fleet_jwt))
        .header("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .json(&request)
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

// ── Derive serving rules ─────────────────────────────────────────────────

/// Derive which routing rules this node can serve locally.
/// A rule is servable if it has a RouteEntry with `is_local: true`
/// whose model is currently loaded (or model_id is None and something
/// is loaded). Uses the `is_local` flag — no string matching.
pub fn derive_serving_rules(
    dispatch_policy: &crate::pyramid::dispatch_policy::DispatchPolicy,
    loaded_models: &[String],
) -> Vec<String> {
    let mut serving = Vec::new();
    for rule in &dispatch_policy.rules {
        for entry in &rule.route_to {
            if entry.provider_id == "fleet" {
                continue; // skip fleet entries
            }
            if entry.is_local {
                let model_match = match &entry.model_id {
                    Some(m) => loaded_models.contains(m),
                    None => !loaded_models.is_empty(),
                };
                if model_match {
                    serving.push(rule.name.clone());
                    break;
                }
            }
        }
    }
    serving
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
