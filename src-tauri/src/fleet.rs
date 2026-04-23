// fleet.rs — Fleet roster and fleet dispatch client.
//
// Stores same-operator peer nodes discovered via heartbeat and direct
// fleet peer announcements. Provides fleet dispatch (POST to peer's
// tunnel) and fleet announce (POST to all peers on state change).
//
// Fleet routing is direct peer-to-peer via Cloudflare tunnels. No
// credits, no exchange, no relay. Same operator's hardware — cost is
// electricity only.
//
// Phase 3 — Async Fleet Dispatch
// ------------------------------
// The dispatch protocol is split into two legs:
//   1. Dispatcher POSTs `/v1/compute/fleet-dispatch` with a `job_id` and
//      `callback_url`. Peer replies **202 Accepted** carrying a
//      `FleetDispatchAck` (the peer's queue depth at accept time), or a
//      fast-fail status (503 overloaded, 409 duplicate, 410 unknown rule).
//   2. When the peer finishes, it POSTs `/v1/fleet/result` at the
//      dispatcher's callback URL with a `FleetAsyncResultEnvelope`
//      (success or error payload).
//
// Synchronous responses are gone from the wire protocol; the old
// `FleetDispatchResponse` struct is retained as the payload carried
// inside `FleetAsyncResult::Success`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::pyramid::fleet_delivery_policy::FleetDeliveryPolicy;
use crate::pyramid::llm::HTTP_CLIENT;
use crate::pyramid::tunnel_url::TunnelUrl;

/// A fleet peer node (same operator, different hardware).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetPeer {
    pub node_id: String,
    pub name: String,
    /// Validated tunnel URL. Freeform strings are rejected at roster
    /// ingress, so every downstream callsite can trust the invariants
    /// of `TunnelUrl` (scheme ∈ {http, https}, host present, path
    /// normalized). On-wire format remains plain string via
    /// `TunnelUrl`'s Serialize impl — old peers stay compatible.
    pub tunnel_url: TunnelUrl,
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
    /// Walker v3 §5.4.2: peer's announce_protocol_version at last seen.
    /// `0` = absent (pre-v3 peer, field wasn't in their announce body),
    /// `1` = v2.1.1 explicit, `2` = v3. Walker's fleet readiness refuses
    /// dispatch to any peer with `announce_protocol_version < 2`
    /// (§5.5.2 strict mode). Default 0 preserves backward-compat for
    /// serialized FleetRosters persisted before Phase 4 added the field.
    #[serde(default)]
    pub announce_protocol_version: u8,
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
    ///
    /// Malformed `tunnel_url` on any individual entry drops just that
    /// entry (warn-logged); the rest of the batch is applied.
    pub fn update_from_heartbeat(
        &mut self,
        peers: Vec<HeartbeatFleetEntry>,
        fleet_jwt: Option<String>,
    ) {
        let now = chrono::Utc::now();
        for entry in peers {
            let parsed_url = match TunnelUrl::parse(&entry.tunnel_url) {
                Ok(u) => u,
                Err(e) => {
                    tracing::warn!(
                        node_id = %entry.node_id,
                        raw = %entry.tunnel_url,
                        err = %e,
                        "heartbeat fleet entry has malformed tunnel_url; dropping entry"
                    );
                    continue;
                }
            };
            let peer = self
                .peers
                .entry(entry.node_id.clone())
                .or_insert_with(|| FleetPeer {
                    node_id: entry.node_id.clone(),
                    name: entry.name.clone(),
                    tunnel_url: parsed_url.clone(),
                    models_loaded: Vec::new(),
                    serving_rules: Vec::new(),
                    queue_depths: HashMap::new(),
                    total_queue_depth: 0,
                    last_seen: now,
                    handle_path: None,
                    // Heartbeat-only discovery doesn't carry an announce
                    // protocol version; default to `0` so the peer is
                    // flagged as v1 until a direct announce lands with
                    // an explicit version (§5.4.2).
                    announce_protocol_version: 0,
                });
            // Heartbeat provides tunnel_url and name. Models + serving_rules
            // come from direct announcement (preferred) or queue state mirror.
            peer.tunnel_url = parsed_url;
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
    ///
    /// Note: `FleetAnnouncement` carries a `TunnelUrl` directly, so any
    /// malformed URL would have been rejected at the deserialize boundary
    /// in `handle_fleet_announce` (server.rs). This function can trust
    /// the `announcement.tunnel_url` field.
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
                announce_protocol_version: announcement.announce_protocol_version,
            });
        peer.tunnel_url = announcement.tunnel_url;
        peer.models_loaded = announcement.models_loaded;
        peer.serving_rules = announcement.serving_rules;
        peer.queue_depths = announcement.queue_depths;
        peer.total_queue_depth = announcement.total_queue_depth;
        peer.last_seen = now;
        // Walker v3 §5.4.2: latest announce wins. A peer that upgrades
        // in place flips from v1 → v2 without needing a roster reset.
        peer.announce_protocol_version = announcement.announce_protocol_version;
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
    ///
    /// `staleness_secs` comes from `FleetDeliveryPolicy::peer_staleness_secs`.
    /// Peers whose `last_seen` is older than that window are excluded.
    pub fn find_peer_for_rule(
        &self,
        rule_name: &str,
        staleness_secs: u64,
    ) -> Option<&FleetPeer> {
        let staleness_limit =
            chrono::Utc::now() - chrono::Duration::seconds(staleness_secs as i64);
        self.peers
            .values()
            .filter(|p| p.last_seen > staleness_limit)
            .filter(|p| p.serving_rules.contains(&rule_name.to_string()))
            .min_by_key(|p| p.total_queue_depth)
    }
}

// ── Heartbeat response shapes ─────────────────────────────────────────────

/// Shape of a fleet roster entry from the heartbeat response.
///
/// `tunnel_url` is kept as `String` on the deserialize side so the
/// ingress layer (`update_from_heartbeat`) can parse and drop bad
/// entries individually instead of failing the whole batch.
#[derive(Debug, Clone, Deserialize)]
pub struct HeartbeatFleetEntry {
    pub node_id: String,
    /// Node display name — optional because Phase 1a heartbeat response
    /// only returns `{ node_id, handle_path, tunnel_url }` (no `name` key).
    /// Without `serde(default)`, deserialization would silently fail for
    /// every entry via `filter_map(|v| from_value(v).ok())`, dropping the
    /// entire fleet roster.
    #[serde(default)]
    pub name: String,
    pub tunnel_url: String,
    /// Handle path (e.g. "@hello/BEHEM") — present when server supports node identity.
    #[serde(default)]
    pub handle_path: Option<String>,
}

// ── Fleet announcement (peer-to-peer) ─────────────────────────────────────

/// Shape of a direct fleet peer announcement.
/// Sent when: startup, model load/unload, shutdown.
///
/// `tunnel_url` is a validated `TunnelUrl` — malformed URLs are rejected
/// at the deserialize boundary (i.e. when the server decodes the
/// announcement body). On-wire format remains a plain string thanks to
/// `TunnelUrl`'s string-based Serialize/Deserialize.
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
    pub tunnel_url: TunnelUrl,
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
    /// Walker v3 §5.4.2 / B-I1: explicit announce-protocol version so
    /// walker requesters can strict-refuse dispatch to pre-v3 peers that
    /// populate `models_loaded` with observability-only semantics
    /// (§5.5.2 — readiness returns `PeerIsV1Announcer`).
    ///
    /// Version mapping:
    ///   `0` — pre-v3 peer (field absent in their announce body).
    ///   `1` — v2.1.1 with explicit versioning.
    ///   `2` — v3 (current).
    ///
    /// `serde(default)` returns `0` so a pre-v3 peer's existing announce
    /// body deserializes cleanly; walker flags those as v1 announcers.
    #[serde(default = "announce_protocol_version_default")]
    pub announce_protocol_version: u8,
}

/// Default for `FleetAnnouncement.announce_protocol_version` when the
/// field is absent in the deserialized body (pre-v3 peer). See
/// §5.4.2 / B-F7.
#[allow(dead_code)]
pub fn announce_protocol_version_default() -> u8 {
    0
}

/// Walker v3 current announce-protocol version. Emitted by this node
/// when sending a `FleetAnnouncement`. Bumped when the announce wire
/// format gains gating-impacting semantics.
#[allow(dead_code)]
pub const ANNOUNCE_PROTOCOL_VERSION: u8 = 2;

// ── Fleet dispatch request / response types (async protocol) ──────────────

/// Shape of a fleet dispatch request (sent TO a peer node via HTTP POST).
///
/// Dispatches by routing rule name — model names never cross node
/// boundaries. The peer resolves the rule to a locally-loaded model.
///
/// The fleet JWT is **not** in the body any more; it rides in the
/// `Authorization: Bearer …` header only. Carrying it in both places
/// was a single-point-of-truth hazard (a forged body JWT vs a real
/// header JWT — which wins?).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetDispatchRequest {
    /// Dispatcher-issued correlation id. Returned on the callback so
    /// the dispatcher can match the async result back to the pending
    /// request.
    pub job_id: String,
    pub rule_name: String,
    pub system_prompt: String,
    pub user_prompt: String,
    pub temperature: f32,
    pub max_tokens: usize,
    pub response_format: Option<serde_json::Value>,
    /// URL the peer must POST the `FleetAsyncResultEnvelope` to when
    /// the work finishes. `validate_callback_url` at the peer pins
    /// authority + path before accepting.
    pub callback_url: String,
}

/// Peer acknowledgement of a dispatch. Returned in the HTTP 202 body
/// when the peer has accepted the job onto its queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetDispatchAck {
    pub job_id: String,
    /// Peer's total queue depth at accept time. Used by the dispatcher
    /// for optional load-shedding metrics, not for correctness.
    pub peer_queue_depth: u64,
}

/// The success payload inside a `FleetAsyncResult::Success`. Identical
/// in shape to the old synchronous response — the LLM output itself
/// doesn't change, only the transport does.
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

/// Outcome of a fleet job, carried inside `FleetAsyncResultEnvelope`.
///
/// Tagged-enum JSON representation (`{"kind":"Success","data":{...}}` /
/// `{"kind":"Error","data":"..."}`) so the callback handler can
/// discriminate without a peek-then-parse dance.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum FleetAsyncResult {
    Success(FleetDispatchResponse),
    Error(String),
}

/// Envelope the peer POSTs to the dispatcher's `callback_url` when a
/// fleet job completes (or fails non-retryably).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetAsyncResultEnvelope {
    pub job_id: String,
    pub outcome: FleetAsyncResult,
}

// ── Fleet dispatch errors ────────────────────────────────────────────────

/// Typed fleet dispatch error — callers can distinguish timeout from dead
/// peer from auth failure, enabling correct retry/backoff policy instead
/// of string-matching folklore.
#[derive(Debug)]
pub struct FleetDispatchError {
    pub kind: FleetDispatchErrorKind,
    pub peer_id: String,
    pub status_code: Option<u16>,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetDispatchErrorKind {
    /// Our HTTP client timed out before getting a response
    ClientTimeout,
    /// Cloudflare 524 — origin (peer GPU) didn't respond in time
    OriginTimeout,
    /// Network/transport error (DNS, connection refused, tunnel down)
    Transport,
    /// Non-success HTTP status (403, 500, etc.). Specific codes the
    /// caller cares about (503 overloaded / 409 duplicate /
    /// 410 unknown rule) live in `status_code`.
    HttpStatus,
    /// Response body couldn't be parsed as `FleetDispatchAck`.
    ResponseParse,
}

impl std::fmt::Display for FleetDispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Fleet dispatch to {} failed ({:?}): {}", self.peer_id, self.kind, self.message)
    }
}

impl FleetDispatchError {
    /// Whether this error means the peer is likely dead/unreachable
    /// (vs just slow or temporarily overloaded).
    pub fn is_peer_dead(&self) -> bool {
        matches!(self.kind, FleetDispatchErrorKind::Transport)
    }
}

// ── Fleet delivery errors (callback POST from peer → dispatcher) ─────────

/// Errors surfaced by [`deliver_fleet_result`]. Peer-side callers map
/// these to outbox retry decisions (backoff vs give up).
#[derive(Debug)]
pub enum FleetDeliveryError {
    /// Network/transport-level failure (connection refused, DNS, timeout).
    Transport(String),
    /// No fleet JWT available in the roster — cannot authenticate.
    NoJwt,
    /// Fleet JWT is expired. Caller should back off and retry after the
    /// next heartbeat refreshes the token.
    JwtExpired,
    /// Dispatcher returned a non-2xx status.
    HttpStatus { status_code: u16, message: String },
    /// Dispatcher returned success but body was unparseable (logged;
    /// currently we do not parse a response body, but reserve this for
    /// future use).
    ResponseParse(String),
}

impl std::fmt::Display for FleetDeliveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FleetDeliveryError::Transport(e) => {
                write!(f, "fleet result delivery transport error: {}", e)
            }
            FleetDeliveryError::NoJwt => write!(f, "fleet result delivery: no fleet JWT in roster"),
            FleetDeliveryError::JwtExpired => {
                write!(f, "fleet result delivery: fleet JWT is expired")
            }
            FleetDeliveryError::HttpStatus { status_code, message } => {
                write!(
                    f,
                    "fleet result delivery HTTP {}: {}",
                    status_code, message
                )
            }
            FleetDeliveryError::ResponseParse(e) => {
                write!(f, "fleet result delivery response parse error: {}", e)
            }
        }
    }
}

impl std::error::Error for FleetDeliveryError {}

// ── JWT expiry helper ────────────────────────────────────────────────────

/// Parse a JWT's `exp` claim without verifying the signature and check
/// it against current time (with a ~5s clock-skew window).
///
/// Returns `true` if the token is expired OR the `exp` claim cannot be
/// located. "Unparseable is expired" is the safe default — an
/// unreadable token cannot be trusted to authenticate outbound calls.
///
/// NOTE: This is a lightweight pre-flight check before outbound calls.
/// The authoritative verification still happens server-side on the
/// receiving handler (which rejects expired tokens at decode time).
pub fn is_jwt_expired(token: &str) -> bool {
    use base64::Engine;

    // Clock-skew window: permit tokens within 5 seconds of their nominal
    // expiry to avoid false-positive rejections on slightly drifted
    // clocks. Small enough that it doesn't meaningfully extend a
    // compromised token's lifetime.
    const CLOCK_SKEW_SECS: u64 = 5;

    // JWT shape: header.payload.signature — all three base64url segments.
    let mut parts = token.trim_start_matches("Bearer ").split('.');
    let _header = parts.next();
    let payload_b64 = match parts.next() {
        Some(p) if !p.is_empty() => p,
        _ => return true, // malformed → treat as expired
    };

    let payload_bytes = match base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
    {
        Ok(b) => b,
        Err(_) => return true,
    };

    #[derive(Deserialize)]
    struct ExpOnly {
        exp: Option<u64>,
    }
    let exp = match serde_json::from_slice::<ExpOnly>(&payload_bytes)
        .ok()
        .and_then(|c| c.exp)
    {
        Some(e) => e,
        None => return true, // no exp claim → treat as expired
    };

    let now = match std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
    {
        Ok(d) => d.as_secs(),
        // Clock before UNIX epoch — treat as expired. Impossible in
        // practice; hedging is correct.
        Err(_) => return true,
    };

    // Expired iff now has moved past (exp + skew).
    now > exp.saturating_add(CLOCK_SKEW_SECS)
}

// ── Fleet dispatch client ─────────────────────────────────────────────────

/// Dispatch an LLM call to a fleet peer by routing rule name.
///
/// The peer resolves the rule name to a local model — model names never
/// cross node boundaries. On success the peer replies 202 with a
/// [`FleetDispatchAck`] and the actual work runs asynchronously; the
/// peer will POST a [`FleetAsyncResultEnvelope`] back to `callback_url`
/// when done.
///
/// `timeout_secs` is the ACK timeout (how long we wait for the 202),
/// NOT the job wall-clock. Callers pass
/// `policy.dispatch_ack_timeout_secs`.
///
/// 503/409/410 are surfaced via `FleetDispatchError { kind: HttpStatus,
/// status_code: Some(…) }` — no new `FleetDispatchErrorKind` variants
/// for these; callers discriminate on the HTTP code.
pub async fn fleet_dispatch_by_rule(
    peer: &FleetPeer,
    job_id: &str,
    callback_url: &str,
    rule_name: &str,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f32,
    max_tokens: usize,
    response_format: Option<&serde_json::Value>,
    fleet_jwt: &str,
    timeout_secs: u64,
) -> Result<FleetDispatchAck, FleetDispatchError> {
    let url = peer.tunnel_url.endpoint("/v1/compute/fleet-dispatch");
    let peer_id = peer.node_id.clone();

    let request = FleetDispatchRequest {
        job_id: job_id.to_string(),
        rule_name: rule_name.to_string(),
        system_prompt: system_prompt.to_string(),
        user_prompt: user_prompt.to_string(),
        temperature,
        max_tokens,
        response_format: response_format.cloned(),
        callback_url: callback_url.to_string(),
    };

    let resp = match HTTP_CLIENT
        .post(&url)
        .header("Authorization", format!("Bearer {}", fleet_jwt))
        .header("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .json(&request)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let kind = if e.is_timeout() {
                FleetDispatchErrorKind::ClientTimeout
            } else if e.is_connect() {
                FleetDispatchErrorKind::Transport
            } else {
                FleetDispatchErrorKind::Transport
            };
            return Err(FleetDispatchError {
                kind,
                peer_id,
                status_code: None,
                message: e.to_string(),
            });
        }
    };

    let status = resp.status();
    if !status.is_success() {
        let status_code = status.as_u16();
        let text = resp.text().await.unwrap_or_default();
        // 524/408/504 are transport-side origin timeouts (Cloudflare or
        // gateway-level). Every other non-success becomes HttpStatus
        // and the caller discriminates via `status_code` (notably 503
        // "peer overloaded", 409 "duplicate job_id", 410 "unknown rule").
        let kind = if status_code == 524 || status_code == 408 || status_code == 504 {
            FleetDispatchErrorKind::OriginTimeout
        } else {
            FleetDispatchErrorKind::HttpStatus
        };
        return Err(FleetDispatchError {
            kind,
            peer_id,
            status_code: Some(status_code),
            message: format!("{}: {}", status, text),
        });
    }

    resp.json::<FleetDispatchAck>()
        .await
        .map_err(|e| FleetDispatchError {
            kind: FleetDispatchErrorKind::ResponseParse,
            peer_id,
            status_code: Some(status.as_u16()),
            message: e.to_string(),
        })
}

// ── Callback URL validation (peer side) ───────────────────────────────────

/// The kind of callback URL being validated. Each kind has a different
/// policy for which roster to consult and which path to pin.
///
/// `MarketStandard` and `Relay` land in Phase 3 of the compute market;
/// they are listed here so `validate_callback_url` has a single
/// exhaustive switch across the whole callback surface, but the
/// implementation is deferred.
pub enum CallbackKind<'a> {
    Fleet { dispatcher_nid: &'a str },
    /// Reserved — compute market Phase 3.
    MarketStandard,
    /// Reserved — compute market Phase 3.
    Relay,
}

/// Reasons `validate_callback_url` may reject a URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallbackValidationError {
    /// `dispatcher_nid` is not in the local roster.
    UnknownDispatcher,
    /// URL authority (scheme/host/port) does not match the roster entry.
    AuthorityMismatch,
    /// URL path is not exactly `/v1/fleet/result`.
    PathMismatch,
    /// URL failed to parse as a valid tunnel URL.
    UnparseableUrl,
    /// URL scheme is not HTTPS (MarketStandard/Relay variants).
    SchemeNotHttps,
    /// URL host is missing or empty (MarketStandard/Relay variants).
    MissingHost,
}

impl std::fmt::Display for CallbackValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallbackValidationError::UnknownDispatcher => {
                write!(f, "callback_url: dispatcher not in fleet roster")
            }
            CallbackValidationError::AuthorityMismatch => {
                write!(f, "callback_url: authority does not match roster entry")
            }
            CallbackValidationError::PathMismatch => {
                write!(f, "callback_url: path is not /v1/fleet/result")
            }
            CallbackValidationError::UnparseableUrl => {
                write!(f, "callback_url: failed to parse as tunnel URL")
            }
            CallbackValidationError::SchemeNotHttps => {
                write!(f, "callback_url: scheme must be https")
            }
            CallbackValidationError::MissingHost => {
                write!(f, "callback_url: host is missing or empty")
            }
        }
    }
}

impl std::error::Error for CallbackValidationError {}

/// Validate a `callback_url` given to us by a dispatcher against the
/// local roster. For the `Fleet` kind:
///   * the dispatcher's `nid` must be in our roster,
///   * the URL authority must match the roster entry's tunnel,
///   * the path must be exactly `/v1/fleet/result`.
///
/// Pinning both authority and path defends against dispatchers pointing
/// callbacks at arbitrary hosts or exploit paths on the roster-known
/// peer.
pub fn validate_callback_url(
    callback_url: &str,
    kind: &CallbackKind,
    roster: &FleetRoster,
) -> Result<(), CallbackValidationError> {
    let got =
        TunnelUrl::parse(callback_url).map_err(|_| CallbackValidationError::UnparseableUrl)?;
    match kind {
        CallbackKind::Fleet { dispatcher_nid } => {
            let peer = roster
                .peers
                .get(*dispatcher_nid)
                .ok_or(CallbackValidationError::UnknownDispatcher)?;
            if got.authority() != peer.tunnel_url.authority() {
                return Err(CallbackValidationError::AuthorityMismatch);
            }
            if got.path() != "/v1/fleet/result" {
                return Err(CallbackValidationError::PathMismatch);
            }
            Ok(())
        }
        CallbackKind::MarketStandard | CallbackKind::Relay => {
            // JWT-gated variants (per architecture §VIII.6 DD-D / DD-Q):
            // no roster check because the Wire is not a peer and relay hops
            // may be operated by any third party. URL validation enforces
            // structural invariants only; the wire_job_token / relay JWT on
            // the callback POST is the actual auth.
            //
            // HTTPS-only per DD-Q part 3 ("if got.0.scheme() != \"https\"").
            // The three tiers all ride over Cloudflare tunnels at maturity
            // and the Wire bootstrap endpoint is https. If dev rigs need to
            // run without TLS later, add an explicit `allow_http_callbacks`
            // knob on `market_delivery_policy` (per the "no hardcoded policy
            // decisions" pattern) — do not loosen this check inline.
            //
            // Note: `TunnelUrl::parse` is more permissive than this check
            // (it accepts both http and https); the scheme check below is
            // the only layer enforcing https for Market/Relay callbacks.
            // The host check IS redundant with `TunnelUrl::parse`'s
            // rejection of empty hosts; keeping it as defense-in-depth
            // against a future path that bypasses `parse`.
            let (scheme, host, _port) = got.authority();
            if scheme != "https" {
                return Err(CallbackValidationError::SchemeNotHttps);
            }
            match host {
                Some(h) if !h.is_empty() => Ok(()),
                _ => Err(CallbackValidationError::MissingHost),
            }
        }
    }
}

/// Sentinel `dispatcher_node_id` for market dispatches in `fleet_result_outbox`.
/// The Wire is not a fleet peer and has no node_id in the roster sense; this
/// constant marks market rows so sweep helpers can skip the Fleet-roster path
/// (which would fail with `UnknownDispatcher`). Per architecture §VIII.6 DD-Q.
pub const WIRE_PLATFORM_DISPATCHER: &str = "wire-platform";

/// String serialization of a `CallbackKind` for the `fleet_result_outbox.callback_kind`
/// column. Sweep helpers read the column and call `callback_kind_from_str` to
/// reconstruct the variant for `validate_callback_url` revalidation.
pub fn callback_kind_str(kind: &CallbackKind) -> &'static str {
    match kind {
        CallbackKind::Fleet { .. } => "Fleet",
        CallbackKind::MarketStandard => "MarketStandard",
        CallbackKind::Relay => "Relay",
    }
}

/// Reconstruct a unit-shaped `CallbackKind` from its column string. The
/// `Fleet` case requires a `dispatcher_nid` which we retrieve from the outbox
/// row's `dispatcher_node_id` column at the sweep call site — so this helper
/// returns an enum that the sweep wraps with the right borrowed nid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallbackKindColumn {
    Fleet,
    MarketStandard,
    Relay,
}

pub fn callback_kind_from_str(s: &str) -> Option<CallbackKindColumn> {
    match s {
        "Fleet" => Some(CallbackKindColumn::Fleet),
        "MarketStandard" => Some(CallbackKindColumn::MarketStandard),
        "Relay" => Some(CallbackKindColumn::Relay),
        _ => None,
    }
}

// ── Pending fleet jobs map (dispatcher side) ──────────────────────────────

/// Pending-job registry on the dispatcher side: `job_id → oneshot sender`.
///
/// Synchronous `std::sync::Mutex` is deliberate — the entry points
/// are short (`register`, `remove`, `peek_matches`, `sweep_expired`),
/// they do NOT hold the lock across `.await`, and the async layer
/// around them uses `tokio::sync::oneshot` for the actual wake-up. The
/// moment a contributor adds an `.await` inside a `self.jobs.lock()`
/// scope, that's a bug; a plain mutex makes the bug a compile-time
/// `Send` error instead of a runtime deadlock risk.
pub struct PendingFleetJobs {
    /// std::sync::Mutex — never held across .await.
    jobs: std::sync::Mutex<std::collections::HashMap<String, PendingFleetJob>>,
}

impl Default for PendingFleetJobs {
    fn default() -> Self {
        Self::new()
    }
}

impl PendingFleetJobs {
    pub fn new() -> Self {
        Self {
            jobs: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Insert a new pending job. Overwrites a prior entry for the same
    /// `job_id` (should not happen in practice — `job_id`s are UUIDs).
    pub fn register(&self, job_id: String, entry: PendingFleetJob) {
        let mut jobs = self.jobs.lock().expect("PendingFleetJobs mutex poisoned");
        jobs.insert(job_id, entry);
    }

    /// Remove and return the pending entry for `job_id`. Returns
    /// `None` if no entry is registered.
    pub fn remove(&self, job_id: &str) -> Option<PendingFleetJob> {
        let mut jobs = self.jobs.lock().expect("PendingFleetJobs mutex poisoned");
        jobs.remove(job_id)
    }

    /// Check whether an incoming callback's (job_id, peer_id) pair
    /// matches a registered entry without consuming it.
    ///
    /// Use pattern on the callback handler:
    ///   match pending.peek_matches(&job_id, &caller_nid) {
    ///       PeekResult::NotFound => orphan,
    ///       PeekResult::Mismatch => forgery,
    ///       PeekResult::Match    => pending.remove(&job_id) and deliver,
    ///   }
    pub fn peek_matches(&self, job_id: &str, expected_peer_id: &str) -> PeekResult {
        let jobs = self.jobs.lock().expect("PendingFleetJobs mutex poisoned");
        match jobs.get(job_id) {
            None => PeekResult::NotFound,
            Some(entry) if entry.peer_id == expected_peer_id => PeekResult::Match,
            Some(_) => PeekResult::Mismatch,
        }
    }

    /// Remove every entry whose `dispatched_at.elapsed() >
    /// expected_timeout * multiplier`. Returns the evicted `job_id`s
    /// for the caller to chronicle as `fleet_pending_orphaned`.
    ///
    /// `multiplier` is clamped to `[1, 10]` — a zero multiplier would
    /// evict every entry immediately, and arbitrary-large multipliers
    /// would let orphaned entries grow unboundedly. `saturating_mul`
    /// on the resulting duration protects against overflow on
    /// pathological timeouts.
    pub fn sweep_expired(&self, multiplier: u64) -> Vec<String> {
        let clamped = multiplier.clamp(1, 10);
        let mut expired: Vec<String> = Vec::new();
        {
            let jobs = self.jobs.lock().expect("PendingFleetJobs mutex poisoned");
            for (job_id, entry) in jobs.iter() {
                // Total eviction window = expected_timeout * clamped.
                let window = entry
                    .expected_timeout
                    .saturating_mul(clamped as u32);
                if entry.dispatched_at.elapsed() > window {
                    expired.push(job_id.clone());
                }
            }
        }
        // Second pass removes — keep the lock scope tight and avoid
        // holding it across the potential allocation in remove().
        {
            let mut jobs = self.jobs.lock().expect("PendingFleetJobs mutex poisoned");
            for job_id in &expired {
                jobs.remove(job_id);
            }
        }
        expired
    }
}

/// Outcome of `PendingFleetJobs::peek_matches`.
#[derive(Debug, PartialEq, Eq)]
pub enum PeekResult {
    /// No pending entry for this job_id.
    NotFound,
    /// Entry exists and `peer_id` matches the caller.
    Match,
    /// Entry exists but `peer_id` does NOT match — forgery attempt.
    Mismatch,
}

/// A single pending dispatch waiting for its callback.
pub struct PendingFleetJob {
    /// Oneshot sender woken when the callback arrives. The receiving
    /// side (the caller that registered this entry) awaits this and
    /// then propagates the result to the waiting compute.
    pub sender: tokio::sync::oneshot::Sender<FleetAsyncResult>,
    /// Instant the dispatch POST was issued. Used by `sweep_expired`.
    pub dispatched_at: std::time::Instant,
    /// Raw node_id of the peer the job was dispatched to.
    ///
    /// **MUST be `peer.node_id` (raw), not `peer.handle_path`.** The
    /// incoming callback authenticates via fleet JWT whose `nid`
    /// claim carries the raw node_id; comparing against anything else
    /// (e.g. a display-only `handle_path`) would false-positive
    /// forgery checks or, worse, accept a forgery as a match.
    pub peer_id: String,
    /// Upper bound on how long this job should take. The orphan sweep
    /// evicts entries older than `expected_timeout * multiplier`.
    pub expected_timeout: std::time::Duration,
}

// ── Fleet dispatch context (plumbed into HTTP handlers) ──────────────────

/// Collection of shared state the fleet dispatch paths need on both
/// sides of the protocol. Constructed once at app startup and passed
/// by clone (of the `Arc`s) into the HTTP handlers and the orphan
/// sweep task.
///
/// Ownership:
/// - `tunnel_state` is **borrowed** — mutated by the tunnel lifecycle
///   elsewhere. We only read from it (for the local tunnel URL used
///   to construct our own `callback_url`).
/// - `fleet_roster` is **borrowed** — mutated by heartbeat and announce
///   handlers. The peer-side outbox sweep reads it to resolve the
///   dispatcher's live tunnel URL + fleet JWT for callback delivery.
///   Bundling it here keeps the sweep loop signature
///   `fn(db_path, Arc<FleetDispatchContext>)` as the spec prescribes.
/// - `pending` and `policy` are **owned by this feature**.
pub struct FleetDispatchContext {
    /// Borrowed handle to the node's tunnel state.
    pub tunnel_state: Arc<tokio::sync::RwLock<crate::tunnel::TunnelState>>,
    /// Borrowed handle to the live fleet roster. Peer-side sweep reads
    /// it to resolve the dispatcher's current tunnel URL + JWT.
    pub fleet_roster: Arc<tokio::sync::RwLock<FleetRoster>>,
    /// Owned: the in-memory pending-job registry.
    pub pending: Arc<PendingFleetJobs>,
    /// Owned: the operational policy, re-readable under hot reload.
    pub policy: Arc<tokio::sync::RwLock<FleetDeliveryPolicy>>,
}

// ── Fleet result delivery (peer side → dispatcher callback) ───────────────

/// POST a [`FleetAsyncResultEnvelope`] back to the dispatcher's
/// callback URL.
///
/// URL resolution: if the dispatcher is in our roster, prefer
/// `roster.peers[dispatcher_nid].tunnel_url.endpoint("/v1/fleet/result")`
/// (tunnel URLs rotate, and the live roster value wins over a stale
/// stored one). Otherwise fall back to the `stored_callback_url` the
/// dispatcher supplied at dispatch time.
///
/// JWT sourcing: reads `roster.fleet_jwt` live — it rotates every
/// heartbeat. If the token is missing we return `NoJwt`; if it is
/// already expired we return `JwtExpired` without attempting the POST
/// (Cloudflare would reject it, burning a delivery attempt).
pub async fn deliver_fleet_result(
    dispatcher_nid: &str,
    stored_callback_url: &str,
    envelope: &FleetAsyncResultEnvelope,
    roster: &FleetRoster,
    policy: &FleetDeliveryPolicy,
) -> Result<(), FleetDeliveryError> {
    // 1. Resolve effective URL.
    let effective_url = match roster.peers.get(dispatcher_nid) {
        Some(peer) => peer.tunnel_url.endpoint("/v1/fleet/result"),
        None => stored_callback_url.to_string(),
    };

    // 2. Source JWT and pre-flight expiry.
    let jwt = roster
        .fleet_jwt
        .as_deref()
        .ok_or(FleetDeliveryError::NoJwt)?;
    if is_jwt_expired(jwt) {
        return Err(FleetDeliveryError::JwtExpired);
    }

    // 3. POST.
    let resp = HTTP_CLIENT
        .post(&effective_url)
        .header("Authorization", format!("Bearer {}", jwt))
        .header("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(
            policy.callback_post_timeout_secs,
        ))
        .json(envelope)
        .send()
        .await
        .map_err(|e| FleetDeliveryError::Transport(e.to_string()))?;

    let status = resp.status();
    if !status.is_success() {
        let status_code = status.as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(FleetDeliveryError::HttpStatus {
            status_code,
            message: format!("{}: {}", status, body),
        });
    }

    Ok(())
}

// ── Dispatcher-side orphan sweep loop ────────────────────────────────────

/// Background task that evicts stale [`PendingFleetJob`] entries whose
/// dispatcher never received a callback within
/// `expected_timeout * orphan_sweep_multiplier`. Spawned once at startup
/// with an `Arc<FleetDispatchContext>` plus an optional `db_path` for
/// chronicle emission.
///
/// Eviction drops the entry's `oneshot::Sender`; the Phase A await that
/// registered the entry then wakes with `RecvError` and records
/// `fleet_dispatch_failed` at the call site where `StepContext` is in
/// scope. In addition, when `db_path` is present we write a discrete
/// `fleet_pending_orphaned` chronicle event here so the observability
/// layer can distinguish "caller gave up, no callback ever arrived"
/// (this sweep) from "caller gave up and we also never heard back"
/// (Phase A timeout). Both events can fire for the same `job_id` —
/// they describe different observations.
///
/// Never exits under normal operation. Under Tauri async_runtime the
/// task is cancelled when the app shuts down; there is no per-iteration
/// shutdown channel because the sweep has no mid-flight state to flush.
pub async fn pending_jobs_sweep_loop(
    ctx: Arc<FleetDispatchContext>,
    db_path: Option<PathBuf>,
) {
    loop {
        let (interval, multiplier) = {
            let p = ctx.policy.read().await;
            (
                p.orphan_sweep_interval_secs.max(1),
                p.orphan_sweep_multiplier,
            )
        };
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
        let evicted = ctx.pending.sweep_expired(multiplier);
        for job_id in evicted {
            tracing::info!(
                %job_id,
                "fleet_pending_orphaned: dispatcher sweep evicted stale pending entry"
            );
            // Emit the canonical chronicle event so Fleet Analytics /
            // Compute Chronicle can surface dispatcher-side orphans.
            // We open a fresh connection inside spawn_blocking — the
            // loop itself stays async-clean, and sweep cadence is low
            // (~seconds) so the open overhead is negligible.
            if let Some(ref dbp) = db_path {
                let dbp_clone = dbp.clone();
                let job_id_clone = job_id.clone();
                tokio::task::spawn_blocking(move || {
                    if let Ok(conn) = rusqlite::Connection::open(&dbp_clone) {
                        let ctx_ev = crate::pyramid::compute_chronicle::ChronicleEventContext::minimal(
                            &format!("fleet-dispatch:{}", job_id_clone),
                            crate::pyramid::compute_chronicle::EVENT_FLEET_PENDING_ORPHANED,
                            crate::pyramid::compute_chronicle::SOURCE_FLEET,
                        )
                        .with_metadata(serde_json::json!({
                            "job_id": job_id_clone,
                            "reason": "dispatcher_sweep_evicted",
                        }));
                        let _ = crate::pyramid::compute_chronicle::record_event(&conn, &ctx_ev);
                    }
                });
            }
        }
    }
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
            if entry.provider_id == "fleet" || entry.provider_id == "market" {
                // Skip walker sentinel entries. Neither "fleet" nor "market"
                // is a real local handler — both dispatch out to the network.
                // The `is_local` check below would already exclude them by
                // convention (both sentinels carry `is_local: false`), but we
                // filter explicitly for parallelism with `resolve_local_for_rule`
                // and to protect against a misconfigured `is_local: true` on a
                // sentinel slipping through. See plan §8 Wave 5 task 37.
                continue;
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
        let url = peer.tunnel_url.endpoint("/v1/fleet/announce");
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

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    // ── Helpers ─────────────────────────────────────────────────────────

    fn mk_peer(node_id: &str, tunnel: &str) -> FleetPeer {
        FleetPeer {
            node_id: node_id.to_string(),
            name: node_id.to_string(),
            tunnel_url: TunnelUrl::parse(tunnel).expect("valid tunnel url"),
            models_loaded: vec![],
            serving_rules: vec!["rule-a".to_string()],
            queue_depths: HashMap::new(),
            total_queue_depth: 0,
            last_seen: chrono::Utc::now(),
            handle_path: None,
            announce_protocol_version: ANNOUNCE_PROTOCOL_VERSION,
        }
    }

    fn mk_roster_with(peer: FleetPeer) -> FleetRoster {
        let mut roster = FleetRoster::default();
        roster.peers.insert(peer.node_id.clone(), peer);
        roster
    }

    /// Build a base64url JWT-shaped string with the given `exp`. No
    /// signature verification is done by `is_jwt_expired`, so signing
    /// is unnecessary.
    fn mk_jwt_with_exp(exp: u64) -> String {
        let header = r#"{"alg":"none","typ":"JWT"}"#;
        let payload = format!(r#"{{"exp":{}}}"#, exp);
        let h = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(header);
        let p = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload);
        format!("{}.{}.fake-sig", h, p)
    }

    // ── FleetDispatchRequest serde (no fleet_jwt in body) ───────────────

    #[test]
    fn fleet_dispatch_request_serde_roundtrip() {
        let req = FleetDispatchRequest {
            job_id: "job-123".into(),
            rule_name: "rule-a".into(),
            system_prompt: "sys".into(),
            user_prompt: "usr".into(),
            temperature: 0.7,
            max_tokens: 128,
            response_format: Some(serde_json::json!({"type":"json_object"})),
            callback_url: "https://me.example.com/v1/fleet/result".into(),
        };
        let json = serde_json::to_string(&req).expect("serialize");
        // No fleet_jwt in the body on the wire.
        assert!(
            !json.contains("fleet_jwt"),
            "body must not carry fleet_jwt; got: {json}"
        );
        let back: FleetDispatchRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.job_id, "job-123");
        assert_eq!(back.callback_url, "https://me.example.com/v1/fleet/result");
        assert_eq!(back.rule_name, "rule-a");
    }

    // ── FleetDispatchAck serde ─────────────────────────────────────────

    #[test]
    fn fleet_dispatch_ack_serde_roundtrip() {
        let ack = FleetDispatchAck {
            job_id: "job-456".into(),
            peer_queue_depth: 3,
        };
        let json = serde_json::to_string(&ack).expect("serialize");
        let back: FleetDispatchAck = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.job_id, "job-456");
        assert_eq!(back.peer_queue_depth, 3);
    }

    // ── FleetAsyncResult tagged enum serde ──────────────────────────────

    #[test]
    fn fleet_async_result_success_serde_shape() {
        let ok = FleetAsyncResult::Success(FleetDispatchResponse {
            content: "hello".into(),
            prompt_tokens: Some(5),
            completion_tokens: Some(7),
            model: "llama3".into(),
            finish_reason: Some("stop".into()),
            peer_model: Some("llama3".into()),
        });
        let json = serde_json::to_value(&ok).expect("serialize");
        assert_eq!(json["kind"], "Success");
        assert_eq!(json["data"]["content"], "hello");
        let back: FleetAsyncResult = serde_json::from_value(json).expect("deserialize");
        match back {
            FleetAsyncResult::Success(resp) => assert_eq!(resp.content, "hello"),
            _ => panic!("expected Success variant"),
        }
    }

    #[test]
    fn fleet_async_result_error_serde_shape() {
        let err = FleetAsyncResult::Error("model crashed".into());
        let json = serde_json::to_value(&err).expect("serialize");
        assert_eq!(json["kind"], "Error");
        assert_eq!(json["data"], "model crashed");
        let back: FleetAsyncResult = serde_json::from_value(json).expect("deserialize");
        match back {
            FleetAsyncResult::Error(msg) => assert_eq!(msg, "model crashed"),
            _ => panic!("expected Error variant"),
        }
    }

    // ── FleetAsyncResultEnvelope serde ──────────────────────────────────

    #[test]
    fn fleet_async_result_envelope_serde_roundtrip() {
        let env = FleetAsyncResultEnvelope {
            job_id: "job-789".into(),
            outcome: FleetAsyncResult::Success(FleetDispatchResponse {
                content: "ok".into(),
                prompt_tokens: None,
                completion_tokens: None,
                model: "m".into(),
                finish_reason: None,
                peer_model: None,
            }),
        };
        let json = serde_json::to_string(&env).expect("serialize");
        let back: FleetAsyncResultEnvelope = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.job_id, "job-789");
        match back.outcome {
            FleetAsyncResult::Success(r) => assert_eq!(r.content, "ok"),
            _ => panic!("expected Success"),
        }
    }

    // ── validate_callback_url ───────────────────────────────────────────

    #[test]
    fn validate_callback_url_matches_authority_and_path() {
        let peer = mk_peer("node-alpha", "https://alpha.example.com");
        let roster = mk_roster_with(peer);
        let r = validate_callback_url(
            "https://alpha.example.com/v1/fleet/result",
            &CallbackKind::Fleet {
                dispatcher_nid: "node-alpha",
            },
            &roster,
        );
        assert!(r.is_ok(), "expected Ok, got {:?}", r);
    }

    #[test]
    fn validate_callback_url_unknown_dispatcher() {
        let roster = FleetRoster::default();
        let r = validate_callback_url(
            "https://alpha.example.com/v1/fleet/result",
            &CallbackKind::Fleet {
                dispatcher_nid: "node-alpha",
            },
            &roster,
        );
        assert_eq!(r.unwrap_err(), CallbackValidationError::UnknownDispatcher);
    }

    #[test]
    fn validate_callback_url_authority_mismatch() {
        let peer = mk_peer("node-alpha", "https://alpha.example.com");
        let roster = mk_roster_with(peer);
        let r = validate_callback_url(
            "https://evil.example.com/v1/fleet/result",
            &CallbackKind::Fleet {
                dispatcher_nid: "node-alpha",
            },
            &roster,
        );
        assert_eq!(r.unwrap_err(), CallbackValidationError::AuthorityMismatch);
    }

    #[test]
    fn validate_callback_url_path_mismatch() {
        let peer = mk_peer("node-alpha", "https://alpha.example.com");
        let roster = mk_roster_with(peer);
        let r = validate_callback_url(
            "https://alpha.example.com/v1/wrong/path",
            &CallbackKind::Fleet {
                dispatcher_nid: "node-alpha",
            },
            &roster,
        );
        assert_eq!(r.unwrap_err(), CallbackValidationError::PathMismatch);
    }

    #[test]
    fn validate_callback_url_unparseable() {
        let roster = FleetRoster::default();
        let r = validate_callback_url(
            "not a url",
            &CallbackKind::Fleet {
                dispatcher_nid: "whoever",
            },
            &roster,
        );
        assert_eq!(r.unwrap_err(), CallbackValidationError::UnparseableUrl);
    }

    #[test]
    fn validate_callback_url_market_standard_accepts_https_any_host() {
        // Per DD-Q: MarketStandard callbacks are JWT-gated (wire_job_token),
        // not roster-gated. validate_callback_url enforces structural
        // invariants (scheme + non-empty host) and leaves auth to the token
        // check on the POST. An empty roster is fine — the Wire is not a peer.
        let roster = FleetRoster::default();
        let r = validate_callback_url(
            "https://wire.example.com/v1/market/result",
            &CallbackKind::MarketStandard,
            &roster,
        );
        assert!(r.is_ok(), "valid https URL must pass for MarketStandard: {:?}", r);
    }

    #[test]
    fn validate_callback_url_relay_accepts_https_any_host() {
        // Per DD-Q: Relay callbacks are JWT-gated (relay JWT), not roster-
        // gated. Relay hops may be operated by any third party, so no peer
        // lookup makes sense. Structural validation only.
        let roster = FleetRoster::default();
        let r = validate_callback_url(
            "https://relay-hop-7.example.com/v1/relay/result",
            &CallbackKind::Relay,
            &roster,
        );
        assert!(r.is_ok(), "valid https URL must pass for Relay: {:?}", r);
    }

    #[test]
    fn validate_callback_url_reserved_kinds_reject_http() {
        // Per DD-Q part 3: HTTPS-only. `TunnelUrl::parse` may accept http
        // for other purposes (the peer tunnel path is authority-matched, so
        // http works there as long as the roster agrees), but the
        // MarketStandard/Relay callback path is explicitly https-only.
        // If dev rigs need non-TLS callbacks later, add an explicit
        // `allow_http_callbacks` contribution field — do not relax the
        // check inline.
        let roster = FleetRoster::default();
        let r_ms = validate_callback_url(
            "http://localhost:8080/v1/market/result",
            &CallbackKind::MarketStandard,
            &roster,
        );
        assert_eq!(r_ms.unwrap_err(), CallbackValidationError::SchemeNotHttps);
        let r_r = validate_callback_url(
            "http://localhost:8080/v1/relay/result",
            &CallbackKind::Relay,
            &roster,
        );
        assert_eq!(r_r.unwrap_err(), CallbackValidationError::SchemeNotHttps);
    }

    #[test]
    fn callback_kind_str_roundtrips_all_variants() {
        // The column discriminator strings MUST stay byte-aligned with the
        // SQL filters in `fleet_outbox_sweep_expired`, `fleet_outbox_retry_
        // candidates`, `fleet_outbox_startup_recovery`,
        // `fleet_outbox_count_inflight_excluding`, and
        // `fleet_outbox_expire_exhausted`, all of which literal-match on
        // `'Fleet'`. If a variant name changes or the string mapping drifts,
        // the sweep loop would silently stop picking up rows. This test
        // fails loudly before that can happen.
        let f = CallbackKind::Fleet { dispatcher_nid: "nid-x" };
        assert_eq!(callback_kind_str(&f), "Fleet");
        assert_eq!(callback_kind_str(&CallbackKind::MarketStandard), "MarketStandard");
        assert_eq!(callback_kind_str(&CallbackKind::Relay), "Relay");

        assert_eq!(callback_kind_from_str("Fleet"), Some(CallbackKindColumn::Fleet));
        assert_eq!(
            callback_kind_from_str("MarketStandard"),
            Some(CallbackKindColumn::MarketStandard)
        );
        assert_eq!(
            callback_kind_from_str("Relay"),
            Some(CallbackKindColumn::Relay)
        );

        // Unknown strings return None rather than panicking — the sweep
        // call site will log + skip the row rather than crash the sweep.
        assert_eq!(callback_kind_from_str(""), None);
        assert_eq!(callback_kind_from_str("fleet"), None); // case-sensitive
        assert_eq!(callback_kind_from_str("nonsense"), None);
    }

    // ── PendingFleetJobs register/peek/remove ───────────────────────────

    #[test]
    fn pending_fleet_jobs_register_peek_remove_roundtrip() {
        let pending = PendingFleetJobs::new();
        let (tx, _rx) = tokio::sync::oneshot::channel::<FleetAsyncResult>();
        pending.register(
            "job-1".to_string(),
            PendingFleetJob {
                sender: tx,
                dispatched_at: std::time::Instant::now(),
                peer_id: "peer-x".to_string(),
                expected_timeout: std::time::Duration::from_secs(60),
            },
        );
        assert_eq!(
            pending.peek_matches("job-1", "peer-x"),
            PeekResult::Match
        );
        assert!(pending.remove("job-1").is_some());
        // Second remove returns None.
        assert!(pending.remove("job-1").is_none());
        // After removal, peek reports NotFound.
        assert_eq!(
            pending.peek_matches("job-1", "peer-x"),
            PeekResult::NotFound
        );
    }

    #[test]
    fn pending_fleet_jobs_peek_forgery_returns_mismatch() {
        let pending = PendingFleetJobs::new();
        let (tx, _rx) = tokio::sync::oneshot::channel::<FleetAsyncResult>();
        pending.register(
            "job-2".to_string(),
            PendingFleetJob {
                sender: tx,
                dispatched_at: std::time::Instant::now(),
                peer_id: "legit-peer".to_string(),
                expected_timeout: std::time::Duration::from_secs(60),
            },
        );
        assert_eq!(
            pending.peek_matches("job-2", "attacker-peer"),
            PeekResult::Mismatch
        );
        // Mismatch must not consume — entry still present for legit caller.
        assert_eq!(
            pending.peek_matches("job-2", "legit-peer"),
            PeekResult::Match
        );
    }

    #[test]
    fn pending_fleet_jobs_sweep_expired_only_evicts_expired() {
        let pending = PendingFleetJobs::new();
        // Fresh entry — expected_timeout 60s, dispatched now → not expired.
        let (tx_fresh, _rx_fresh) = tokio::sync::oneshot::channel::<FleetAsyncResult>();
        pending.register(
            "fresh".to_string(),
            PendingFleetJob {
                sender: tx_fresh,
                dispatched_at: std::time::Instant::now(),
                peer_id: "peer".to_string(),
                expected_timeout: std::time::Duration::from_secs(60),
            },
        );
        // Expired entry: dispatched 10 minutes ago, expected_timeout 1ms —
        // 1ms * multiplier is way in the past.
        let (tx_old, _rx_old) = tokio::sync::oneshot::channel::<FleetAsyncResult>();
        let long_ago = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(600))
            .expect("subtract 10 minutes");
        pending.register(
            "old".to_string(),
            PendingFleetJob {
                sender: tx_old,
                dispatched_at: long_ago,
                peer_id: "peer".to_string(),
                expected_timeout: std::time::Duration::from_millis(1),
            },
        );
        let evicted = pending.sweep_expired(2);
        assert_eq!(evicted, vec!["old".to_string()]);
        // Fresh entry remains.
        assert_eq!(
            pending.peek_matches("fresh", "peer"),
            PeekResult::Match
        );
        // Expired entry gone.
        assert_eq!(
            pending.peek_matches("old", "peer"),
            PeekResult::NotFound
        );
    }

    // ── find_peer_for_rule staleness ────────────────────────────────────

    #[test]
    fn find_peer_for_rule_respects_staleness_secs() {
        let mut roster = FleetRoster::default();
        let mut fresh = mk_peer("fresh", "https://fresh.example.com");
        fresh.last_seen = chrono::Utc::now();
        roster.peers.insert(fresh.node_id.clone(), fresh);

        let mut stale = mk_peer("stale", "https://stale.example.com");
        // Last seen 5 minutes ago — stale under the canonical 120s window.
        stale.last_seen = chrono::Utc::now() - chrono::Duration::seconds(300);
        roster.peers.insert(stale.node_id.clone(), stale);

        // With 120s window: stale excluded, fresh picked.
        let pick = roster
            .find_peer_for_rule("rule-a", 120)
            .expect("one peer is fresh");
        assert_eq!(pick.node_id, "fresh");

        // With a huge window (3600s): both qualify; picks the one with
        // lowest queue depth — tie → any is fine. Since both tied at 0,
        // assert simply that we still get a peer.
        let pick2 = roster
            .find_peer_for_rule("rule-a", 3600)
            .expect("both peers qualify with wide window");
        assert!(matches!(pick2.node_id.as_str(), "fresh" | "stale"));

        // With a 1s window: even "fresh" might be excluded depending on
        // test scheduling, so we only assert that we don't crash.
        let _ = roster.find_peer_for_rule("rule-a", 1);
    }

    // ── is_jwt_expired ──────────────────────────────────────────────────

    #[test]
    fn is_jwt_expired_returns_true_for_expired_token() {
        let past = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 3600;
        let token = mk_jwt_with_exp(past);
        assert!(is_jwt_expired(&token));
    }

    #[test]
    fn is_jwt_expired_returns_false_for_fresh_token() {
        let future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600;
        let token = mk_jwt_with_exp(future);
        assert!(!is_jwt_expired(&token));
    }

    #[test]
    fn is_jwt_expired_true_for_malformed_token() {
        // No dots, no payload — expired by safe default.
        assert!(is_jwt_expired("not-a-jwt"));
        assert!(is_jwt_expired(""));
        // Shape looks right but payload isn't base64url.
        assert!(is_jwt_expired("aaa.!!!not-base64!!!.bbb"));
    }

    #[test]
    fn is_jwt_expired_true_when_exp_absent() {
        let header = r#"{"alg":"none"}"#;
        let payload = r#"{"sub":"no-exp"}"#;
        let h = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(header);
        let p = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload);
        let token = format!("{}.{}.sig", h, p);
        assert!(is_jwt_expired(&token));
    }

    #[test]
    fn is_jwt_expired_strips_bearer_prefix() {
        let future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600;
        let token = format!("Bearer {}", mk_jwt_with_exp(future));
        assert!(!is_jwt_expired(&token));
    }

    // ── Heartbeat ingress drops malformed entries gracefully ────────────

    #[test]
    fn update_from_heartbeat_drops_malformed_entry_and_keeps_batch() {
        let mut roster = FleetRoster::default();
        let peers = vec![
            HeartbeatFleetEntry {
                node_id: "good".into(),
                name: "good-node".into(),
                tunnel_url: "https://good.example.com".into(),
                handle_path: None,
            },
            HeartbeatFleetEntry {
                node_id: "bad".into(),
                name: "bad-node".into(),
                tunnel_url: "not a url".into(),
                handle_path: None,
            },
            HeartbeatFleetEntry {
                node_id: "also-good".into(),
                name: "also-good-node".into(),
                tunnel_url: "http://localhost:8080".into(),
                handle_path: None,
            },
        ];
        roster.update_from_heartbeat(peers, None);
        // Good entries present.
        assert!(roster.peers.contains_key("good"));
        assert!(roster.peers.contains_key("also-good"));
        // Malformed entry dropped — did NOT fail the whole batch.
        assert!(!roster.peers.contains_key("bad"));
    }
}
