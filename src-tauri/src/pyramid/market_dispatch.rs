// pyramid/market_dispatch.rs — Market dispatch primitives
// (MarketDispatchRequest, MarketDispatchAck, MarketAsyncResult + envelope,
// MarketDispatchContext, PendingMarketJobs).
//
// Per `compute-market-phase-2-exchange.md` §III "server.rs: Job
// Dispatch Endpoint" and `compute-market-architecture.md` §VIII.6
// DD-C / DD-D / DD-F / DD-Q. The compute market is the fleet async
// dispatch protocol with a different JWT audience (`compute` vs
// `fleet`), a ChatML `messages` payload instead of
// (system_prompt, user_prompt), and a JWT-gated callback URL instead
// of a roster-matched one. All the outbox / CAS / sweep / admission
// scaffolding from `async-fleet-dispatch.md` is reused verbatim via
// `fleet_result_outbox.callback_kind` (see WS0 and `fleet.rs`
// CallbackKind enum).
//
// **Scope of this module:** the TYPES and the Arc bundle. The
// Phase 2 WS5 dispatch handler lives in `server.rs`; the Phase 3
// callback-delivery worker lives elsewhere. This module contains no
// handler logic — just the shapes WS5 and WS3+ will compose over.
//
// **Parallel to fleet:** these types mirror `fleet.rs` fleet-side
// counterparts byte-for-byte except:
//   - Request carries `messages: serde_json::Value` (ChatML),
//     converted on the provider side via
//     `crate::pyramid::messages::messages_to_prompt_pair`.
//   - Request carries credit rates and privacy tier (market-specific).
//   - Callback URL is a JWT-gated `TunnelUrl` — validated by
//     `fleet::validate_callback_url(kind=MarketStandard|Relay)` which
//     accepts any HTTPS URL (the JWT on the POST is the auth).
//   - PendingMarketJob tracks `provider_id` rather than `peer_id` —
//     fleet peers are same-operator-roster entries; market providers
//     are anyone the Wire matched. The forgery-prevention semantic
//     (verify caller identity against the registered expectation)
//     carries over unchanged.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::pyramid::market_delivery_policy::MarketDeliveryPolicy;
use crate::pyramid::tunnel_url::TunnelUrl;

// ── Market dispatch request / response types ─────────────────────────

/// Body of `POST /v1/compute/job-dispatch` — Wire → provider.
///
/// DD-C: diverges from `FleetDispatchRequest` at the prompt shape
/// only. Everything else (job_id, model, callback_url, optional
/// sampling params) mirrors fleet. The provider-side handler
/// converts `messages` → `(system_prompt, user_prompt)` via
/// `messages_to_prompt_pair` BEFORE enqueueing to `compute_queue`,
/// keeping the downstream Ollama call path shape-identical to local
/// and fleet jobs.
///
/// Auth rides in the `Authorization: Bearer <wire_job_token>` header,
/// not in the body — same single-source-of-truth discipline as
/// `FleetDispatchRequest`.
///
/// `#[serde(deny_unknown_fields)]` so a typo'd field from the Wire
/// side (or a future Wire version that adds a field this provider
/// hasn't been upgraded to recognize) surfaces as a visible 400
/// instead of being silently dropped. Matches the `MarketDeliveryPolicy`
/// pattern — strict-on-read for everything contribution-shaped.
/// Bearer-token envelope Wire uses for the callback auth. Echoed
/// verbatim by the provider in its `Authorization: Bearer <token>`
/// header when POSTing the result envelope — the provider does not
/// interpret `kind`, just carries the opaque `token`.
///
/// Per contract §2.1 rev 1.5 — `callback_auth: {"type": "bearer",
/// "token": "<opaque>"}`. The `type` JSON key is renamed from the
/// Rust-reserved `type` to `kind` via serde rename.
///
/// `Debug` is implemented manually below to redact `token` — preventing
/// the bearer from leaking through any `tracing::warn!("{:?}", req)`
/// site or chronicle metadata that serializes the full request body.
/// The token is single-use-per-job but enables forged deliveries if
/// exfiltrated; redaction in Debug is defense-in-depth. Phase 3 spec
/// §"Token redaction."
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CallbackAuth {
    #[serde(rename = "type")]
    pub kind: String,
    pub token: String,
}

impl std::fmt::Debug for CallbackAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallbackAuth")
            .field("kind", &self.kind)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// `Debug` is implemented manually below to redact `requester_delivery_jwt` —
/// preventing the EdDSA bearer from leaking through any
/// `tracing::warn!("{:?}", req)` site (e.g. the admission-handler's body
/// parse-error log) or chronicle metadata that serializes the full request
/// body. `callback_auth` is already redacted by `CallbackAuth`'s own custom
/// `Debug` impl. Contract §3.4 bearer + redaction policy; spec test 12
/// (`requester_delivery_jwt_never_in_logs`).
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketDispatchRequest {
    /// UUID generated by the Wire at match time (in `match_compute_job`).
    /// Used as the compound-PK slot in `fleet_result_outbox`, as the
    /// `sub` claim in the `wire_job_token`, and as the DADBEAR
    /// `batch_id` / `target_id` / semantic-path seed.
    pub job_id: String,

    /// Requested model id. Resolved by the provider's local runtime
    /// (Ollama / bridge) — the Wire does not require a specific backend.
    /// Renamed from `model` to `model_id` in contract rev 1.5 — the
    /// dispatch body field name matches `/match` response + `/offers`
    /// creation, so the whole dispatch chain uses one canonical key.
    pub model_id: String,

    /// Wire-side offer identifier (handle-path, e.g.
    /// `behem/106/1`). Used for chronicle correlation + stale-offer
    /// cleanup when the provider rejects dispatch with
    /// `no_offer_for_model`. Added in contract rev 1.5.
    pub offer_id: String,

    /// Queue-discount multiplier applied at match time (in basis
    /// points; 10000 = 1.0×). Authoritative for settlement — the
    /// provider MUST use this value, not recompute from its own
    /// offer curve, since the Wire quoted this rate to the requester
    /// at match time. Added in contract rev 1.5.
    pub matched_multiplier_bps: i32,

    /// ChatML array: `[{role: "system"|"user", content: "..."}, ...]`.
    /// Converted to the two-string `(system_prompt, user_prompt)` pair
    /// at the handler boundary via
    /// `crate::pyramid::messages::messages_to_prompt_pair`.
    pub messages: serde_json::Value,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<usize>,

    /// Where the provider POSTs the `MarketAsyncResultEnvelope` when
    /// the job completes. Under the SOTA privacy model:
    ///   - Bootstrap mode (network launch): Wire relay endpoint
    ///     `{wire_base}/v1/compute/result-relay`.
    ///   - Post-relay-market, 0 relays: requester's tunnel URL.
    ///   - Post-relay-market, N relays: first relay hop.
    /// Validated by `fleet::validate_callback_url(kind=MarketStandard)`
    /// or `(kind=Relay)` — structural https+host only; the
    /// `callback_auth` token below is the actual auth on the POST.
    pub callback_url: TunnelUrl,

    /// Bearer-token envelope the provider echoes in its result POST.
    /// Added in contract rev 1.5 — pre-rev-1.5 Wire relied on the
    /// original `wire_job_token` JWT for the callback round-trip;
    /// rev 1.5 issues a separate opaque token so the callback auth
    /// is narrowly scoped to one result POST. Provider treats `kind`
    /// + `token` as opaque and echoes `token` verbatim.
    pub callback_auth: CallbackAuth,

    /// Contract rev 2.0 §2.1 / §2.6: the requester's direct-POST URL for
    /// the content leg of the two-POST delivery topology. Provider POSTs
    /// `MarketAsyncResultEnvelope` here with `Authorization: Bearer
    /// <requester_delivery_jwt>`. Distinct from `callback_url` above, which
    /// is the settlement-leg (Wire) URL.
    ///
    /// Required field — pre-rev-2.0 Wire dispatches (which lacked this
    /// field) fail serde's required-field validation and surface at the
    /// admission handler as a visible 400 `requester_callback_url_missing_or_invalid`.
    /// Zero-lockstep fail-loud posture consistent with the outer struct's
    /// `deny_unknown_fields` (§2.1 rev 2.0).
    pub requester_callback_url: TunnelUrl,

    /// Contract rev 2.0 §3.4: opaque Bearer the provider echoes on the
    /// content-leg POST. Minted by Wire at match time with
    /// `aud="requester-delivery"`, `sub=<uuid_job_id>`,
    /// `rid=<requester_operator_id>`, `exp=now + requester_delivery_jwt_ttl_secs`.
    /// Provider treats as an opaque string — verification happens on the
    /// requester side (sibling of `verify_market_identity`).
    ///
    /// Required field — same serde-enforced required-field validation as
    /// `requester_callback_url` above; pre-rev-2.0 dispatches 400 at
    /// admission.
    pub requester_delivery_jwt: String,

    /// Privacy tier string — currently `"standard"`, `"direct"`, or
    /// `"bootstrap-relay"`. Warn-don't-reject on unknown values per
    /// contract Q-PROTO-3. Carried through the dispatch → settlement
    /// chain for observability.
    pub privacy_tier: String,

    /// ACK-handshake timeout in milliseconds (NOT inference timeout).
    /// Wire enforces on its side of the fetch; the provider reads it
    /// for observability + outbox row timing. Default 5000ms. Added
    /// in contract rev 1.5.
    pub timeout_ms: u64,

    /// Forward-compat escape hatch. Wire adds new observability fields
    /// (e.g. `trace_id`) under `extensions.*` without forcing a
    /// lockstep node upgrade. Older nodes silently deserialize
    /// unknown fields into this map and ignore them; newer nodes read
    /// the keys they care about. Per contract §10.1.
    ///
    /// The outer struct keeps `deny_unknown_fields` so typos at the
    /// TOP level still surface as 400. This field is the bounded
    /// escape valve: unknown fields must live under `extensions`, not
    /// at the root.
    #[serde(default)]
    pub extensions: std::collections::HashMap<String, serde_json::Value>,
}

impl std::fmt::Debug for MarketDispatchRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MarketDispatchRequest")
            .field("job_id", &self.job_id)
            .field("model_id", &self.model_id)
            .field("offer_id", &self.offer_id)
            .field("matched_multiplier_bps", &self.matched_multiplier_bps)
            .field("messages", &self.messages)
            .field("temperature", &self.temperature)
            .field("max_tokens", &self.max_tokens)
            .field("callback_url", &self.callback_url)
            .field("callback_auth", &self.callback_auth)
            .field("requester_callback_url", &self.requester_callback_url)
            .field("requester_delivery_jwt", &"<redacted>")
            .field("privacy_tier", &self.privacy_tier)
            .field("timeout_ms", &self.timeout_ms)
            .field("extensions", &self.extensions)
            .finish()
    }
}

/// Success payload inside `MarketAsyncResult::Success`. Identical in
/// shape to `FleetDispatchResponse` — the LLM output itself is
/// protocol-agnostic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketDispatchResponse {
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<i64>,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    /// The model the provider actually resolved + used (for
    /// observability — operators can audit "was the requested model
    /// the one that served?").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_model: Option<String>,
}

/// Outcome of a market job, carried inside `MarketAsyncResultEnvelope`.
///
/// Tagged-enum JSON shape (`{"kind":"Success","data":{...}}` /
/// `{"kind":"Error","data":"..."}`) matches `FleetAsyncResult` so
/// future generic outbox-delivery code can handle both without a
/// peek-then-parse dance.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum MarketAsyncResult {
    Success(MarketDispatchResponse),
    Error(String),
}

/// Envelope the provider POSTs to the requester's `callback_url`
/// when a market job completes (or fails non-retryably).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketAsyncResultEnvelope {
    pub job_id: String,
    pub outcome: MarketAsyncResult,
}

/// Provider's 202 ACK body — returned to the Wire when the job has
/// been accepted onto the provider's queue. Identical shape to
/// `FleetDispatchAck`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketDispatchAck {
    pub job_id: String,
    /// Provider's total queue depth at accept time. Used by the
    /// Wire's queue-mirror cleanup and observability metrics, NOT for
    /// correctness.
    pub peer_queue_depth: u64,
}

// ── Pending market jobs (requester-side) ──────────────────────────────
//
// The Phase 2 PROVIDER side (this workstream scope) does NOT register
// entries here — it durably persists via the outbox and has no
// oneshot-await discipline to maintain. PendingMarketJobs is defined
// here alongside the other primitives because Phase 3 (requester
// integration) will populate it — the types belong together, and
// WS5 dispatch-context construction shouldn't wait on Phase 3 to
// know about the shape.
//
// Semantics mirror `PendingFleetJobs` with one rename:
//   - fleet: `peer_id` is the raw node_id of the peer we dispatched
//     to; callback's JWT `nid` claim must match.
//   - market: `provider_id` is the node_id of the provider the Wire
//     matched us with; callback's JWT `pid` claim must match.

/// In-memory registry of market dispatches awaiting their callback.
///
/// Uses `std::sync::Mutex` (NOT tokio's) because no lock ever spans
/// an `.await`. See `PendingFleetJobs` module docs in `fleet.rs` for
/// the rationale (compile-time Send errors beat runtime deadlock
/// risk). The actual wake-up uses a `tokio::sync::oneshot` channel
/// stored in each `PendingMarketJob.sender`.
pub struct PendingMarketJobs {
    jobs: std::sync::Mutex<std::collections::HashMap<String, PendingMarketJob>>,
}

impl Default for PendingMarketJobs {
    fn default() -> Self {
        Self::new()
    }
}

impl PendingMarketJobs {
    pub fn new() -> Self {
        Self {
            jobs: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Insert a new pending job. Overwrites a prior entry for the
    /// same `job_id` (should not happen — job_ids are Wire-generated
    /// UUIDs, unique per match).
    pub fn register(&self, job_id: String, entry: PendingMarketJob) {
        let mut jobs = self.jobs.lock().expect("PendingMarketJobs mutex poisoned");
        jobs.insert(job_id, entry);
    }

    /// Remove and return the pending entry for `job_id`. Returns
    /// `None` if no entry registered (orphan callback, or already
    /// consumed).
    pub fn remove(&self, job_id: &str) -> Option<PendingMarketJob> {
        let mut jobs = self.jobs.lock().expect("PendingMarketJobs mutex poisoned");
        jobs.remove(job_id)
    }

    /// Check whether an incoming callback's (job_id, provider_id)
    /// pair matches a registered entry without consuming it.
    ///
    /// Usage pattern on the requester-side callback handler:
    ///   match pending.peek_matches(&job_id, &jwt_pid) {
    ///       PeekResult::NotFound => orphan — chronicle + 410,
    ///       PeekResult::Mismatch => forgery — chronicle + 403,
    ///       PeekResult::Match    => pending.remove(&job_id) + deliver,
    ///   }
    pub fn peek_matches(&self, job_id: &str, expected_provider_id: &str) -> PeekResult {
        let jobs = self.jobs.lock().expect("PendingMarketJobs mutex poisoned");
        match jobs.get(job_id) {
            None => PeekResult::NotFound,
            Some(entry) if entry.provider_id == expected_provider_id => PeekResult::Match,
            Some(_) => PeekResult::Mismatch,
        }
    }

    /// Remove every entry whose `dispatched_at.elapsed() >
    /// expected_timeout * multiplier`. Returns the evicted `job_id`s
    /// for the caller to chronicle.
    ///
    /// `multiplier` is clamped to `[1, 10]` to protect against
    /// zero-multiplier evict-everything and pathological-large
    /// multipliers. `saturating_mul` on the duration protects
    /// against overflow.
    pub fn sweep_expired(&self, multiplier: u64) -> Vec<String> {
        let clamped = multiplier.clamp(1, 10);
        let mut expired: Vec<String> = Vec::new();
        {
            let jobs = self.jobs.lock().expect("PendingMarketJobs mutex poisoned");
            for (job_id, entry) in jobs.iter() {
                let window = entry.expected_timeout.saturating_mul(clamped as u32);
                if entry.dispatched_at.elapsed() > window {
                    expired.push(job_id.clone());
                }
            }
        }
        {
            let mut jobs = self.jobs.lock().expect("PendingMarketJobs mutex poisoned");
            for job_id in &expired {
                jobs.remove(job_id);
            }
        }
        expired
    }
}

/// Outcome of `PendingMarketJobs::peek_matches`. Mirrors
/// `fleet::PeekResult` — both enums are intentionally local to their
/// respective modules (same name, same variants, different crate
/// path) so the caller's match discriminates which registry the
/// result came from.
#[derive(Debug, PartialEq, Eq)]
pub enum PeekResult {
    /// No pending entry for this job_id.
    NotFound,
    /// Entry exists and `provider_id` matches the caller's JWT `pid`.
    Match,
    /// Entry exists but `provider_id` does NOT match — forgery.
    Mismatch,
}

/// A single pending market dispatch waiting for its callback.
pub struct PendingMarketJob {
    /// Oneshot sender woken when the callback arrives. The receiving
    /// side (the caller that registered this entry) awaits this and
    /// then propagates the result to the waiting compute.
    pub sender: tokio::sync::oneshot::Sender<MarketAsyncResult>,
    /// Instant the dispatch was initiated (Wire's `fill_compute_job`
    /// ack time). Used by `sweep_expired`.
    pub dispatched_at: std::time::Instant,
    /// Provider's node_id (matches the `pid` claim on the callback's
    /// `wire_job_token`). Parallels `PendingFleetJob.peer_id`; the
    /// forgery-prevention invariant is identical.
    pub provider_id: String,
    /// Upper bound on how long this job should take — typically
    /// `fill_job_ttl_secs` (from `market_delivery_policy`) plus a
    /// grace margin.
    pub expected_timeout: std::time::Duration,
}

// ── Market dispatch context ──────────────────────────────────────────

/// Collection of shared state the market dispatch paths need.
/// Constructed once at app startup and passed by clone (of the
/// `Arc`s) into the HTTP handlers (Phase 2 WS5) and the
/// callback-delivery sweep (Phase 3).
///
/// Ownership mirrors `FleetDispatchContext`:
/// - `tunnel_state` is **borrowed** — the dispatch handler reads it
///   to construct the outbox row's `callback_url` for the Wire's
///   view of where to reach us.
/// - `pending`, `policy`, and `mirror_nudge` are **owned by this feature**.
pub struct MarketDispatchContext {
    /// Borrowed handle to the node's tunnel state. Read-only from
    /// the market path.
    pub tunnel_state: Arc<tokio::sync::RwLock<crate::tunnel::TunnelState>>,
    /// Owned: the in-memory pending-job registry (requester-side).
    /// Empty-and-unused in Phase 2 provider-only boot; populated by
    /// Phase 3 when the requester integration lands.
    pub pending: Arc<PendingMarketJobs>,
    /// Owned: the operational policy, re-readable under hot reload.
    /// ConfigSynced event listener (future WS / Phase 2 extension
    /// when `MarketDispatchContext` is wired into main.rs) writes
    /// into this RwLock when an operator supersedes the contribution.
    pub policy: Arc<tokio::sync::RwLock<MarketDeliveryPolicy>>,
    /// Phase 2 WS6: queue-mirror nudge channel. Every call site that
    /// mutates market queue state (dispatch handler, worker status
    /// transitions, offer IPCs, enable/disable toggles) calls
    /// `mirror_nudge.send(()).ok()` — fire-and-forget. The mirror task
    /// (`pyramid::market_mirror::spawn_market_mirror_task`) receives
    /// on the paired receiver, debounces by
    /// `market_delivery_policy.queue_mirror_debounce_ms`, and pushes
    /// the current snapshot to the Wire's
    /// `POST /api/v1/compute/queue-state` endpoint.
    ///
    /// `unbounded_send` is non-blocking and returns `Err` only when
    /// the receiver has been dropped (mirror task gone). Call sites
    /// use `.ok()` so a shutdown race can't panic on a dispatch path.
    pub mirror_nudge: tokio::sync::mpsc::UnboundedSender<()>,
    /// Phase 3 (provider delivery worker): nudge channel fired whenever a
    /// market outbox row transitions into `ready` — (1) worker success
    /// path after promote_ready_if_pending, (2) worker failure path after
    /// the bug-fix promote-with-error, (3) sweep's heartbeat-lost
    /// synthesize path. The delivery task (`pyramid::market_delivery::
    /// supervise_delivery_loop`) receives on the paired receiver and
    /// claims ready rows for POST to Wire's callback endpoint.
    ///
    /// Same discipline as `mirror_nudge`: unbounded send, fire-and-forget,
    /// `.ok()` on the send so a shutdown race can't panic the call site.
    pub delivery_nudge: tokio::sync::mpsc::UnboundedSender<()>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Serde roundtrips ─────────────────────────────────────────────

    #[test]
    fn market_dispatch_request_roundtrips_minimal() {
        let req = MarketDispatchRequest {
            job_id: "job-abc".into(),
            model_id: "gemma3:27b".into(),
            offer_id: "playful/106/1".into(),
            matched_multiplier_bps: 10000,
            messages: serde_json::json!([{"role": "user", "content": "hi"}]),
            temperature: None,
            max_tokens: None,
            callback_url: TunnelUrl::parse("https://wire.example.com/v1/compute/result-relay")
                .unwrap(),
            callback_auth: CallbackAuth {
                kind: "bearer".into(),
                token: "opaque-token".into(),
            },
            requester_callback_url: TunnelUrl::parse(
                "https://newsbleach.com/api/v1/compute/callback/job-abc",
            )
            .unwrap(),
            requester_delivery_jwt: "opaque-requester-jwt".into(),
            privacy_tier: "standard".into(),
            timeout_ms: 5000,
            extensions: std::collections::HashMap::new(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: MarketDispatchRequest = serde_json::from_str(&json).unwrap();
        // Compare field-by-field (MarketDispatchRequest isn't Eq).
        assert_eq!(back.job_id, req.job_id);
        assert_eq!(back.model_id, req.model_id);
        assert_eq!(back.offer_id, req.offer_id);
        assert_eq!(back.matched_multiplier_bps, req.matched_multiplier_bps);
        assert_eq!(back.callback_url, req.callback_url);
        assert_eq!(back.callback_auth, req.callback_auth);
        assert_eq!(back.privacy_tier, req.privacy_tier);
        assert_eq!(back.timeout_ms, req.timeout_ms);
        // Optional fields elided from wire (skip_serializing_if).
        assert!(
            !json.contains("\"temperature\""),
            "None optional must not serialize: {json}"
        );
        assert!(
            !json.contains("\"max_tokens\""),
            "None optional must not serialize: {json}"
        );
        // callback_auth serializes with renamed JSON key (`type`).
        assert!(
            json.contains("\"type\":\"bearer\""),
            "callback_auth.kind must serialize as `type`: {json}"
        );
    }

    #[test]
    fn market_dispatch_request_accepts_contract_shape_verbatim() {
        // Regression guard: this is the exact body shape the Wire's
        // /api/v1/compute/fill route constructs per contract §2.1
        // rev 1.5. If a field gets renamed without updating the Wire
        // side (or this struct drifts ahead of Wire), this test
        // surfaces the mismatch immediately. The JSON below is
        // copy-pasted from the contract doc + the literal
        // `dispatchBody` object at fill/route.ts.
        let contract_shape = r#"{
            "job_id": "b8d98b7b-0ba0-4f8a-ac6e-d0a9c1c62a3c",
            "model_id": "gemma4:26b",
            "offer_id": "playful/106/1",
            "matched_multiplier_bps": 10000,
            "privacy_tier": "bootstrap-relay",
            "callback_url": "https://wire.example.com/api/v1/compute/settle/b8d98b7b-0ba0-4f8a-ac6e-d0a9c1c62a3c",
            "callback_auth": {"type": "bearer", "token": "zOpYp-4-o8EQsX4pDEFzKGrEcJGNsoH5Y39E0VTHV9Q"},
            "requester_callback_url": "https://newsbleach.com/api/v1/compute/callback/b8d98b7b-0ba0-4f8a-ac6e-d0a9c1c62a3c",
            "requester_delivery_jwt": "eyJhbGciOiJFZERTQSIsInR5cCI6IkpXVCJ9.opaque.signature",
            "max_tokens": 2048,
            "messages": [
                {"role": "system", "content": "Be concise."},
                {"role": "user", "content": "hi"}
            ],
            "temperature": 0.7,
            "timeout_ms": 5000,
            "extensions": {"request_id": "0c5e3d4c-dba6-4ef7-a21a-0f6a76fe7e84"}
        }"#;
        let parsed = serde_json::from_str::<MarketDispatchRequest>(contract_shape)
            .expect("contract §2.1 rev 1.5 body must parse; if this fails, struct has drifted from the Wire-side /fill dispatch body");
        assert_eq!(parsed.model_id, "gemma4:26b");
        assert_eq!(parsed.offer_id, "playful/106/1");
        assert_eq!(parsed.matched_multiplier_bps, 10000);
        assert_eq!(parsed.callback_auth.kind, "bearer");
        assert_eq!(parsed.timeout_ms, 5000);
        assert_eq!(
            parsed.extensions.get("request_id").and_then(|v| v.as_str()),
            Some("0c5e3d4c-dba6-4ef7-a21a-0f6a76fe7e84")
        );
    }

    #[test]
    fn market_dispatch_request_rejects_unknown_fields() {
        // `deny_unknown_fields` ensures a Wire-side typo or a
        // forward-compatible-but-unknown field surfaces as a visible
        // deserialization failure (handler returns 400) rather than a
        // silent drop. This guards against the class of bug where a
        // future `priority` field silently vanishes on older providers.
        let json_with_typo = r#"{
            "job_id": "job-abc",
            "model_id": "gemma3:27b",
            "offer_id": "playful/106/1",
            "matched_multiplier_bps": 10000,
            "messages": [{"role": "user", "content": "hi"}],
            "callback_url": "https://wire.example.com/v1/compute/result-relay",
            "callback_auth": {"type": "bearer", "token": "t"},
            "requester_callback_url": "https://newsbleach.com/api/v1/compute/callback/job-abc",
            "requester_delivery_jwt": "opaque",
            "privacy_tier": "standard",
            "timeout_ms": 5000,
            "priority": "urgent"
        }"#;
        let err = serde_json::from_str::<MarketDispatchRequest>(json_with_typo);
        assert!(
            err.is_err(),
            "unknown field must be rejected; got {:?}",
            err
        );
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("priority") || msg.contains("unknown field"),
            "expected error to mention the unknown field; got: {msg}"
        );
    }

    // Spec test 18 — `pre_rev_2_0_dispatch_missing_fields_400s`: a dispatch
    // body lacking `requester_callback_url` (pre-rev-2.0 Wire shape) must
    // fail serde's required-field validation. The admission handler maps
    // this failure to a 400 `requester_callback_url_missing_or_invalid`.
    // This test guards the serde-enforced half of that contract — the
    // required-field gate cannot regress silently.
    #[test]
    fn market_dispatch_request_rejects_missing_requester_callback_url() {
        let json_missing = r#"{
            "job_id": "job-pre-rev2",
            "model_id": "gemma3:27b",
            "offer_id": "playful/106/1",
            "matched_multiplier_bps": 10000,
            "messages": [{"role": "user", "content": "hi"}],
            "callback_url": "https://wire.example.com/v1/compute/settle/x",
            "callback_auth": {"type": "bearer", "token": "t"},
            "requester_delivery_jwt": "opaque",
            "privacy_tier": "standard",
            "timeout_ms": 5000
        }"#;
        let err = serde_json::from_str::<MarketDispatchRequest>(json_missing);
        assert!(
            err.is_err(),
            "missing requester_callback_url must be rejected by serde; got {:?}",
            err
        );
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("requester_callback_url") || msg.contains("missing field"),
            "expected error to mention the missing field; got: {msg}"
        );
    }

    // Spec test 18 corollary — same guard for the sibling required field
    // `requester_delivery_jwt`. Pre-rev-2.0 Wire dispatches won't carry
    // either; omitting this one must also 400 at admission via serde.
    #[test]
    fn market_dispatch_request_rejects_missing_requester_delivery_jwt() {
        let json_missing = r#"{
            "job_id": "job-pre-rev2",
            "model_id": "gemma3:27b",
            "offer_id": "playful/106/1",
            "matched_multiplier_bps": 10000,
            "messages": [{"role": "user", "content": "hi"}],
            "callback_url": "https://wire.example.com/v1/compute/settle/x",
            "callback_auth": {"type": "bearer", "token": "t"},
            "requester_callback_url": "https://newsbleach.example/cb/x",
            "privacy_tier": "standard",
            "timeout_ms": 5000
        }"#;
        let err = serde_json::from_str::<MarketDispatchRequest>(json_missing);
        assert!(
            err.is_err(),
            "missing requester_delivery_jwt must be rejected by serde; got {:?}",
            err
        );
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("requester_delivery_jwt") || msg.contains("missing field"),
            "expected error to mention the missing field; got: {msg}"
        );
    }

    // Spec test 19 (serde half) — `privacy_tier_bootstrap_relay_not_rejected`:
    // the legacy `"bootstrap-relay"` value is a free-form string, not an
    // enum, and must round-trip cleanly through the dispatch struct.
    // `handle_market_dispatch` then applies the warn-don't-reject branch
    // (Q-PROTO-3) and emits the `market_unknown_privacy_tier` chronicle
    // event — that admission-handler behavior is an integration concern and
    // is covered by the handler's unit-of-work wiring tests. This narrow
    // serde test guards the "not rejected at parse time" half of the
    // contract so a future `#[serde(deny_unknown_variants)]` or enum tightening
    // can't silently break zero-lockstep compatibility with an older Wire.
    #[test]
    fn market_dispatch_request_accepts_legacy_privacy_tier_bootstrap_relay() {
        let json_bootstrap = r#"{
            "job_id": "job-bootstrap",
            "model_id": "gemma3:27b",
            "offer_id": "playful/106/1",
            "matched_multiplier_bps": 10000,
            "messages": [{"role": "user", "content": "hi"}],
            "callback_url": "https://wire.example.com/v1/compute/settle/x",
            "callback_auth": {"type": "bearer", "token": "t"},
            "requester_callback_url": "https://newsbleach.example/cb/x",
            "requester_delivery_jwt": "opaque",
            "privacy_tier": "bootstrap-relay",
            "timeout_ms": 5000
        }"#;
        let parsed = serde_json::from_str::<MarketDispatchRequest>(json_bootstrap).expect(
            "legacy privacy_tier='bootstrap-relay' must parse — warn-don't-reject per Q-PROTO-3",
        );
        assert_eq!(
            parsed.privacy_tier, "bootstrap-relay",
            "legacy tier string must round-trip verbatim, not be normalized"
        );
    }

    #[test]
    fn market_dispatch_request_roundtrips_full() {
        let req = MarketDispatchRequest {
            job_id: "job-xyz".into(),
            model_id: "llama3.2:70b".into(),
            offer_id: "mac-lan/106/2".into(),
            matched_multiplier_bps: 9500,
            messages: serde_json::json!([
                {"role": "system", "content": "Be concise."},
                {"role": "user", "content": "What is 2+2?"}
            ]),
            temperature: Some(0.2),
            max_tokens: Some(1024),
            callback_url: TunnelUrl::parse("https://relay-hop.example.com/r/inbound").unwrap(),
            callback_auth: CallbackAuth {
                kind: "bearer".into(),
                token: "some-opaque-token".into(),
            },
            requester_callback_url: TunnelUrl::parse(
                "https://newsbleach.com/api/v1/compute/callback/job-xyz",
            )
            .unwrap(),
            requester_delivery_jwt: "opaque-requester-jwt-full".into(),
            privacy_tier: "cloud_relay".into(),
            timeout_ms: 7000,
            extensions: std::collections::HashMap::new(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: MarketDispatchRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.temperature, Some(0.2));
        assert_eq!(back.max_tokens, Some(1024));
        assert_eq!(back.matched_multiplier_bps, 9500);
        assert_eq!(back.privacy_tier, "cloud_relay");
        assert_eq!(back.timeout_ms, 7000);
    }

    /// Spec test 12 — `requester_delivery_jwt_never_in_logs`. Custom
    /// `Debug` impl must elide the EdDSA bearer; `callback_auth` stays
    /// redacted via `CallbackAuth`'s own custom `Debug`. Regression guard
    /// for the security-relevant gap Wave 3B flagged: without this impl,
    /// any `tracing::warn!("{:?}", req)` at an admission-handler parse
    /// error would exfiltrate the token.
    #[test]
    fn requester_delivery_jwt_never_appears_in_debug_output() {
        let req = MarketDispatchRequest {
            job_id: "job-debug".into(),
            model_id: "gemma3:27b".into(),
            offer_id: "behem/106/1".into(),
            matched_multiplier_bps: 10000,
            messages: serde_json::json!([{"role": "user", "content": "hi"}]),
            temperature: None,
            max_tokens: None,
            callback_url: TunnelUrl::parse("https://wire.example.com/settle/x").unwrap(),
            callback_auth: CallbackAuth {
                kind: "bearer".into(),
                token: "super-secret-callback-token-xyz123".into(),
            },
            requester_callback_url: TunnelUrl::parse("https://req.example.com/cb/x").unwrap(),
            requester_delivery_jwt:
                "eyJhbGciOiJFZERTQSIsInR5cCI6IkpXVCJ9.eyJhdWQiOiJyZXF1ZXN0ZXItZGVsaXZlcnkifQ.super-secret-signature-material".into(),
            privacy_tier: "direct".into(),
            timeout_ms: 5000,
            extensions: std::collections::HashMap::new(),
        };
        let dbg = format!("{:?}", req);
        assert!(
            !dbg.contains("super-secret-signature-material"),
            "requester_delivery_jwt leaked through Debug: {dbg}"
        );
        assert!(
            !dbg.contains("eyJhbGciOiJFZERTQSI"),
            "any part of the JWT leaked through Debug: {dbg}"
        );
        assert!(
            !dbg.contains("super-secret-callback-token"),
            "callback_auth.token leaked through Debug (expected redaction via CallbackAuth): {dbg}"
        );
        assert!(
            dbg.contains("<redacted>"),
            "Debug should show the <redacted> placeholder: {dbg}"
        );
        assert!(
            dbg.contains("job-debug"),
            "Debug should still show non-sensitive fields: {dbg}"
        );
    }

    #[test]
    fn market_async_result_success_serializes_tagged() {
        let result = MarketAsyncResult::Success(MarketDispatchResponse {
            content: "4".into(),
            prompt_tokens: Some(8),
            completion_tokens: Some(1),
            model: "gemma3:27b".into(),
            finish_reason: Some("stop".into()),
            provider_model: Some("gemma3:27b".into()),
        });
        let json = serde_json::to_string(&result).unwrap();
        // Tagged-enum representation: {"kind":"Success","data":{...}}.
        assert!(
            json.starts_with("{\"kind\":\"Success\",\"data\":{"),
            "unexpected wire form: {json}"
        );
        let back: MarketAsyncResult = serde_json::from_str(&json).unwrap();
        match back {
            MarketAsyncResult::Success(r) => {
                assert_eq!(r.content, "4");
                assert_eq!(r.prompt_tokens, Some(8));
                assert_eq!(r.model, "gemma3:27b");
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[test]
    fn market_async_result_error_serializes_tagged() {
        let result = MarketAsyncResult::Error("model out of memory".into());
        let json = serde_json::to_string(&result).unwrap();
        assert_eq!(
            json,
            "{\"kind\":\"Error\",\"data\":\"model out of memory\"}"
        );
        let back: MarketAsyncResult = serde_json::from_str(&json).unwrap();
        match back {
            MarketAsyncResult::Error(s) => assert_eq!(s, "model out of memory"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn market_async_result_envelope_roundtrips() {
        let env = MarketAsyncResultEnvelope {
            job_id: "job-xyz".into(),
            outcome: MarketAsyncResult::Error("timeout".into()),
        };
        let json = serde_json::to_string(&env).unwrap();
        let back: MarketAsyncResultEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back.job_id, "job-xyz");
        match back.outcome {
            MarketAsyncResult::Error(s) => assert_eq!(s, "timeout"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn market_dispatch_ack_roundtrips() {
        let ack = MarketDispatchAck {
            job_id: "job-abc".into(),
            peer_queue_depth: 7,
        };
        let json = serde_json::to_string(&ack).unwrap();
        let back: MarketDispatchAck = serde_json::from_str(&json).unwrap();
        assert_eq!(back.job_id, "job-abc");
        assert_eq!(back.peer_queue_depth, 7);
    }

    // ── PendingMarketJobs register/peek/remove ───────────────────────

    #[test]
    fn pending_market_jobs_register_peek_remove_roundtrip() {
        let pending = PendingMarketJobs::new();
        let (tx, _rx) = tokio::sync::oneshot::channel::<MarketAsyncResult>();
        pending.register(
            "job-1".into(),
            PendingMarketJob {
                sender: tx,
                dispatched_at: std::time::Instant::now(),
                provider_id: "node-provider-x".into(),
                expected_timeout: std::time::Duration::from_secs(60),
            },
        );
        assert_eq!(
            pending.peek_matches("job-1", "node-provider-x"),
            PeekResult::Match
        );
        assert!(pending.remove("job-1").is_some());
        assert!(pending.remove("job-1").is_none());
        assert_eq!(
            pending.peek_matches("job-1", "node-provider-x"),
            PeekResult::NotFound
        );
    }

    #[test]
    fn pending_market_jobs_peek_forgery_returns_mismatch() {
        // A callback claiming to come from the same job_id but a
        // DIFFERENT provider (stolen-token-replay attempt) must be
        // caught without consuming the registered entry.
        let pending = PendingMarketJobs::new();
        let (tx, _rx) = tokio::sync::oneshot::channel::<MarketAsyncResult>();
        pending.register(
            "job-42".into(),
            PendingMarketJob {
                sender: tx,
                dispatched_at: std::time::Instant::now(),
                provider_id: "node-provider-alpha".into(),
                expected_timeout: std::time::Duration::from_secs(60),
            },
        );
        assert_eq!(
            pending.peek_matches("job-42", "node-provider-beta"),
            PeekResult::Mismatch
        );
        // Entry must still be registered — peek did not consume it.
        assert_eq!(
            pending.peek_matches("job-42", "node-provider-alpha"),
            PeekResult::Match
        );
    }

    #[test]
    fn pending_market_jobs_sweep_expired_respects_multiplier() {
        // Use a very short expected_timeout so we can exceed it
        // predictably. The multiplier is clamped to [1, 10] — we
        // test both extremes.
        let pending = PendingMarketJobs::new();
        let (tx, _rx) = tokio::sync::oneshot::channel::<MarketAsyncResult>();
        pending.register(
            "job-short".into(),
            PendingMarketJob {
                sender: tx,
                dispatched_at: std::time::Instant::now() - std::time::Duration::from_millis(500),
                provider_id: "p1".into(),
                expected_timeout: std::time::Duration::from_millis(100),
            },
        );
        // With multiplier=1 → window = 100ms × 1 = 100ms. The entry
        // is 500ms old → expired.
        let evicted = pending.sweep_expired(1);
        assert_eq!(evicted, vec!["job-short".to_string()]);
        // Entry is gone.
        assert_eq!(
            pending.peek_matches("job-short", "p1"),
            PeekResult::NotFound
        );
    }

    #[test]
    fn pending_market_jobs_sweep_clamps_zero_multiplier() {
        // multiplier=0 must NOT evict everything. Clamp to 1.
        let pending = PendingMarketJobs::new();
        let (tx, _rx) = tokio::sync::oneshot::channel::<MarketAsyncResult>();
        pending.register(
            "job-fresh".into(),
            PendingMarketJob {
                sender: tx,
                dispatched_at: std::time::Instant::now(),
                provider_id: "p1".into(),
                expected_timeout: std::time::Duration::from_secs(60),
            },
        );
        let evicted = pending.sweep_expired(0);
        assert!(
            evicted.is_empty(),
            "multiplier=0 must clamp to 1, not evict-all"
        );
        assert_eq!(pending.peek_matches("job-fresh", "p1"), PeekResult::Match);
    }

    #[test]
    fn pending_market_jobs_sweep_saturates_pathological_multiplier() {
        // A u64::MAX multiplier would overflow the duration
        // multiplication without saturating_mul. Clamp to 10 and
        // confirm no panic.
        let pending = PendingMarketJobs::new();
        let (tx, _rx) = tokio::sync::oneshot::channel::<MarketAsyncResult>();
        pending.register(
            "job-pathological".into(),
            PendingMarketJob {
                sender: tx,
                dispatched_at: std::time::Instant::now(),
                provider_id: "p1".into(),
                expected_timeout: std::time::Duration::from_secs(30),
            },
        );
        let _evicted = pending.sweep_expired(u64::MAX);
        // Must not panic. Whether the entry gets evicted depends on
        // clock jitter; assertion is only about liveness.
    }
}
