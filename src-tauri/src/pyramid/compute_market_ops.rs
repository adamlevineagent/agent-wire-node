// Compute market operations — shared business logic for offer management,
// market browsing, and serving toggles.
//
// Why this module exists:
// The Tauri IPC commands (`compute_offer_create`, `compute_market_surface`,
// etc.) and the new HTTP routes (`POST /pyramid/compute/offers` and
// friends) need to do the same work. Without this module they'd duplicate
// ~500 lines of Wire-API calls + local-state mutation + chronicle emission
// + mirror nudging. With this module, both transports call the same
// functions and the IPC + HTTP handlers become thin glue.
//
// Design:
// Each op takes the concrete handles it needs (auth, config, market_state,
// market_dispatch, pyramid_data_dir) rather than a wrapper struct. Verbose
// at call sites but dependencies are explicit. Errors come back as a typed
// `ComputeMarketOpError` so callers can map to `String` (IPC) or
// `warp::reply::Response` (HTTP) as appropriate.

use crate::auth::AuthState;
use crate::compute_market::{ComputeMarketState, ComputeOffer, QueueDiscountPoint};
use crate::http_utils::send_api_request;
use crate::pyramid::compute_chronicle::{self, ChronicleEventContext};
use crate::pyramid::market_dispatch::MarketDispatchContext;
use crate::WireNodeConfig;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Queue-discount curve point in the **Wire contract shape**:
/// `{queue_depth, discount_bps}`. This is what agents / CLI send over
/// HTTP and what we forward to Wire on `/api/v1/compute/offers`.
///
/// Distinct from the node-internal `QueueDiscountPoint` (fields
/// `{depth, multiplier_bps}`) which is Wire's internal representation
/// — Wire translates `multiplier_bps = 10000 - discount_bps` on the
/// server side. We store in the internal shape locally because the
/// rest of the node's admission + settlement logic already operates
/// on multipliers; we translate at the HTTP boundary.
///
/// See §1.1 in `wire-node-compute-market-contract.md` — the on-wire
/// shape is authoritative for field names.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfferQueueDiscountPoint {
    pub queue_depth: usize,
    /// Basis points of discount off the base rate. `500` = 5% off.
    /// Wire translates to `multiplier_bps = 10000 - discount_bps`.
    pub discount_bps: i32,
}

impl OfferQueueDiscountPoint {
    /// Translate to the node-internal storage shape.
    pub fn to_internal(&self) -> QueueDiscountPoint {
        QueueDiscountPoint {
            depth: self.queue_depth,
            multiplier_bps: 10_000 - self.discount_bps,
        }
    }
}

/// Unified error type for compute market ops. The IPC layer maps this to
/// `String`, the HTTP layer to `(StatusCode, error-json-body)`.
#[derive(Debug, thiserror::Error)]
pub enum ComputeMarketOpError {
    /// Caller violated an input contract — missing model, bad price, etc.
    /// Maps to HTTP 400 Bad Request.
    #[error("{0}")]
    BadRequest(String),

    /// Pre-condition on node state not satisfied (e.g. model not loaded
    /// for `compute_offer_create`). Maps to HTTP 409 Conflict.
    #[error("{0}")]
    PreconditionFailed(String),

    /// Wire rejected the request (status + body). Maps to HTTP 502 Bad
    /// Gateway — we reached Wire but it didn't like what we sent.
    #[error("Wire rejected request: {status} — {body}")]
    WireRejected { status: u16, body: String },

    /// Auth token lookup failed. Maps to HTTP 401.
    #[error("auth unavailable: {0}")]
    AuthUnavailable(String),

    /// Catch-all for i/o + serde + unexpected failures. Maps to HTTP 500.
    #[error("internal: {0}")]
    Internal(String),
}

impl ComputeMarketOpError {
    /// Map to a warp HTTP status code. IPC callers can ignore; HTTP
    /// callers use this to build the response.
    pub fn http_status(&self) -> warp::http::StatusCode {
        use warp::http::StatusCode;
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::PreconditionFailed(_) => StatusCode::CONFLICT,
            Self::WireRejected { .. } => StatusCode::BAD_GATEWAY,
            Self::AuthUnavailable(_) => StatusCode::UNAUTHORIZED,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// Input for create/update offer. Field names match the Wire contract
/// §1.1 on-wire shape so that a CLI caller can cut+paste the Wire doc's
/// example body and it just works. Also the exact body shape we forward
/// to Wire — no field rewriting at the HTTP boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfferRequest {
    pub model_id: String,
    #[serde(default = "default_provider_type")]
    pub provider_type: String,
    pub rate_per_m_input: i64,
    pub rate_per_m_output: i64,
    pub reservation_fee: i64,
    /// Wire contract shape: `{queue_depth, discount_bps}`. Translated
    /// to node-internal `QueueDiscountPoint` before local storage.
    #[serde(default)]
    pub queue_discount_curve: Vec<OfferQueueDiscountPoint>,
    pub max_queue_depth: usize,
}

fn default_provider_type() -> String {
    "local".to_string()
}

/// Fetch the api token from auth state.
pub async fn get_api_token(auth: &Arc<RwLock<AuthState>>) -> Result<String, ComputeMarketOpError> {
    let guard = auth.read().await;
    guard
        .api_token
        .clone()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| {
            ComputeMarketOpError::AuthUnavailable(
                "no api_token on AuthState — node not registered / logged in".to_string(),
            )
        })
}

/// Resolve pyramid data_dir for persistence + chronicle writes. Falls
/// back to `.` only in test fixtures (production always has one set).
fn resolve_data_dir(pyramid: &Arc<crate::pyramid::PyramidState>) -> PathBuf {
    pyramid
        .data_dir
        .as_ref()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Persist `ComputeMarketState` to disk. Matches IPC-layer behavior:
/// log on failure but don't propagate, because in-memory state is
/// already updated and the disk write retries on the next mutation.
pub fn persist_market_state(state: &ComputeMarketState, data_dir: &Path) {
    if let Err(e) = state.save(data_dir) {
        tracing::warn!(
            data_dir = %data_dir.display(),
            error = %e,
            "compute_market_state save failed"
        );
    }
}

/// Validate that a model is actually loaded on this node before we let
/// the operator publish an offer for it. Prevents the common footgun of
/// publishing `llama3.1:70b` when Ollama only has `llama3.1:8b`.
///
/// Soft-passes when local mode is off (operator may be running
/// bridge-only offers) and when the model name matches the configured
/// local model even if it isn't in the `available_models` list yet
/// (first-time-load race).
pub async fn validate_model_loaded(
    pyramid: &Arc<crate::pyramid::PyramidState>,
    model_id: &str,
) -> Result<(), ComputeMarketOpError> {
    // NB: `pyramid.data_dir` is the DIRECTORY, not the DB file path —
    // the DB lives at `data_dir/pyramid.db`. The original IPC-layer
    // `validate_model_loaded` had a latent bug here that passed the
    // directory directly to `open_pyramid_connection`; SQLite would
    // treat the directory as a path-to-open and fail with
    // "Failed to open pyramid connection at …/wire-node". Fixed 2026-04-16
    // when the HTTP route exercised the path for the first time.
    let db_path = pyramid
        .data_dir
        .as_ref()
        .ok_or_else(|| ComputeMarketOpError::Internal("no data_dir on pyramid state".into()))?
        .join("pyramid.db");
    let conn = crate::pyramid::db::open_pyramid_connection(&db_path)
        .map_err(|e| ComputeMarketOpError::Internal(format!("failed to open pyramid db: {e}")))?;
    let status = crate::pyramid::local_mode::load_status_snapshot(&conn)
        .unwrap_or_else(|_| crate::pyramid::local_mode::LocalModeStatus::disabled_default());
    if !status.enabled {
        // Local mode off → soft-pass (bridge-only offer, or operator
        // knows what they're doing).
        return Ok(());
    }
    if status.available_models.iter().any(|m| m == model_id) {
        return Ok(());
    }
    if status.model.as_deref() == Some(model_id) {
        return Ok(());
    }
    Err(ComputeMarketOpError::PreconditionFailed(format!(
        "model '{model_id}' is not loaded locally — load the model first or check the name"
    )))
}

// ═════════════════════════════════════════════════════════════════════════
// Operations
// ═════════════════════════════════════════════════════════════════════════

/// Publish a new offer to Wire + persist locally + emit chronicle + nudge
/// mirror. Returns the Wire-assigned offer_id.
///
/// If Wire rejects, local state is NOT modified (preserves the invariant
/// that local state reflects successfully-published offers only). If
/// Wire accepts but persistence fails, we log but return Ok — persistence
/// retries on the next mutation.
pub async fn create_offer(
    req: OfferRequest,
    auth: &Arc<RwLock<AuthState>>,
    config: &Arc<RwLock<WireNodeConfig>>,
    market_state: &Arc<RwLock<ComputeMarketState>>,
    market_dispatch: &Arc<MarketDispatchContext>,
    pyramid: &Arc<crate::pyramid::PyramidState>,
) -> Result<String, ComputeMarketOpError> {
    if req.model_id.trim().is_empty() {
        return Err(ComputeMarketOpError::BadRequest(
            "model_id must be non-empty".to_string(),
        ));
    }
    if req.rate_per_m_input < 0 || req.rate_per_m_output < 0 || req.reservation_fee < 0 {
        return Err(ComputeMarketOpError::BadRequest(
            "rates and reservation_fee must be non-negative".to_string(),
        ));
    }

    // Pre-flight: require the model be loaded (or loadable) locally.
    validate_model_loaded(pyramid, &req.model_id).await?;

    // Snapshot any pre-existing `wire_offer_id` for this model under
    // the read lock. Used after the Wire POST to detect and clean up
    // a stale Wire-side offer from a concurrent create-vs-create race.
    // Without this, two simultaneous POSTs for the same model_id leak
    // a Wire offer: both succeed on the Wire (UPSERT semantics there
    // produce two distinct offer_ids under different `created_at`
    // races), but only the second writer's offer_id survives in local
    // state, orphaning the first. Detection is best-effort — if the
    // stale id is the same as the new one, Wire already UPSERT-ed;
    // otherwise we fire a DELETE for the old id after the new row is
    // committed locally. Wanderer caught this 2026-04-16.
    let prior_wire_offer_id: Option<String> = {
        let ms = market_state.read().await;
        ms.offers.get(&req.model_id).and_then(|o| o.wire_offer_id.clone())
    };

    // Wire-side publish first — we only touch local state if Wire accepts.
    let (api_url, token, node_id) = {
        let cfg = config.read().await;
        let auth_read = auth.read().await;
        let node_id = auth_read.node_id.clone();
        drop(auth_read);
        (cfg.api_url.clone(), get_api_token(auth).await?, node_id)
    };
    // Include `node_id` when we know ours. Wire auto-infers when the
    // operator owns exactly one node and the body omits it, but returns
    // 400 `multiple_nodes_require_explicit_node_id` when >1 node is
    // owned. Sending it always sidesteps the multi-node branch entirely
    // — belt + suspenders against future operators adding more nodes.
    let mut body = serde_json::json!({
        "model_id": req.model_id,
        "provider_type": req.provider_type,
        "rate_per_m_input": req.rate_per_m_input,
        "rate_per_m_output": req.rate_per_m_output,
        "reservation_fee": req.reservation_fee,
        "queue_discount_curve": req.queue_discount_curve,
        "max_queue_depth": req.max_queue_depth,
    });
    if let Some(nid) = node_id {
        if !nid.is_empty() {
            body["node_id"] = serde_json::Value::String(nid);
        }
    }
    let send_result = send_api_request(
        &api_url,
        "POST",
        "/api/v1/compute/offers",
        &token,
        Some(&body),
        None,
    )
    .await;
    let (status, resp) = match send_result {
        Ok(ok) => ok,
        Err(e) => {
            // send_api_request encodes !is_success as Err with an
            // `API error {status}: {body}` format. Treat those as
            // WireRejected; network failures fall into Internal.
            return Err(classify_send_error(e));
        }
    };
    if !status.is_success() {
        return Err(ComputeMarketOpError::WireRejected {
            status: status.as_u16(),
            body: serde_json::to_string(&resp).unwrap_or_else(|_| "<unserializable>".to_string()),
        });
    }
    // UUID-OR-HANDLE-PATH: Wire currently returns canonical UUID v4.
    // Post-Pillar-14 migration, this will be a handle-path string
    // `{agent_handle}/{epoch-day}/{seq}`. See `ComputeOffer::wire_offer_id`
    // for the migration-wide note. No code change needed here — the
    // string is opaque to us.
    let offer_id = resp
        .get("offer_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            ComputeMarketOpError::Internal(format!("Wire response missing offer_id: {resp}"))
        })?
        .to_string();

    // Local state update + persist. Translate the Wire-contract-shaped
    // curve (`{queue_depth, discount_bps}`) into the node-internal
    // storage shape (`{depth, multiplier_bps}`) — the admission +
    // settlement paths work on multipliers, not discounts.
    let internal_curve: Vec<QueueDiscountPoint> = req
        .queue_discount_curve
        .iter()
        .map(|p| p.to_internal())
        .collect();
    let data_dir = resolve_data_dir(pyramid);
    {
        let mut ms = market_state.write().await;
        ms.offers.insert(
            req.model_id.clone(),
            ComputeOffer {
                model_id: req.model_id.clone(),
                provider_type: req.provider_type.clone(),
                rate_per_m_input: req.rate_per_m_input,
                rate_per_m_output: req.rate_per_m_output,
                reservation_fee: req.reservation_fee,
                queue_discount_curve: internal_curve,
                max_queue_depth: req.max_queue_depth,
                wire_offer_id: Some(offer_id.clone()),
            },
        );
        ms.last_evaluation_at = Some(chrono::Utc::now().to_rfc3339());
        persist_market_state(&ms, &data_dir);
    }

    emit_market_offered_event(
        &data_dir,
        &req.model_id,
        req.rate_per_m_input,
        req.rate_per_m_output,
        req.reservation_fee,
        &offer_id,
    );

    // Race cleanup: if there was a prior Wire offer_id for this model
    // AND Wire issued a NEW id (not UPSERT-collapsed), fire a DELETE
    // for the orphan. Fire-and-forget — if the DELETE fails, the
    // offer sits on Wire until its staleness TTL expires. We don't
    // block on it because the operator-visible state is already correct.
    if let Some(prior) = prior_wire_offer_id {
        if prior != offer_id {
            let api_url_cleanup = {
                let cfg = config.read().await;
                cfg.api_url.clone()
            };
            let token_cleanup = token.clone();
            let prior_encoded = urlencoding::encode(&prior).into_owned();
            tokio::spawn(async move {
                let path = format!("/api/v1/compute/offers/{}", prior_encoded);
                match send_api_request(&api_url_cleanup, "DELETE", &path, &token_cleanup, None, None)
                    .await
                {
                    Ok(_) => tracing::info!(
                        prior_offer_id = %prior,
                        "cleaned up orphan Wire offer after concurrent re-create"
                    ),
                    Err(e) => tracing::warn!(
                        prior_offer_id = %prior,
                        error = %e,
                        "failed to clean up orphan Wire offer (will stale out at Wire TTL)"
                    ),
                }
            });
        }
    }

    let _ = market_dispatch.mirror_nudge.send(());

    Ok(offer_id)
}

/// Remove an offer from Wire + local state. 404 from Wire is treated as
/// success (Wire already cleaned up or never saw the offer). Local state
/// is always cleaned up even if Wire returns other errors — we prefer a
/// consistent local picture over preserving a failed delete for retry.
pub async fn remove_offer(
    model_id: &str,
    auth: &Arc<RwLock<AuthState>>,
    config: &Arc<RwLock<WireNodeConfig>>,
    market_state: &Arc<RwLock<ComputeMarketState>>,
    market_dispatch: &Arc<MarketDispatchContext>,
    pyramid: &Arc<crate::pyramid::PyramidState>,
) -> Result<(), ComputeMarketOpError> {
    if model_id.trim().is_empty() {
        return Err(ComputeMarketOpError::BadRequest(
            "model_id must be non-empty".to_string(),
        ));
    }

    let wire_offer_id = {
        let ms = market_state.read().await;
        ms.offers.get(model_id).and_then(|o| o.wire_offer_id.clone())
    };

    if let Some(offer_id) = wire_offer_id {
        let (api_url, token) = {
            let cfg = config.read().await;
            (cfg.api_url.clone(), get_api_token(auth).await?)
        };
        // UUID-OR-HANDLE-PATH: `offer_id` is opaque and URL-encoded on
        // the way into the path. Handle-paths contain `/` characters
        // (e.g. `myhandle/19852/42`) — urlencoding::encode turns them
        // into `%2F`, so the DELETE request path stays a single path
        // segment. Wire URL-decodes on receipt. Works for both formats.
        let path = format!("/api/v1/compute/offers/{}", urlencoding::encode(&offer_id));
        // We tolerate 404 (already deleted). Other failures still trigger
        // local cleanup below, then surface the error to the caller.
        if let Err(e) = send_api_request(&api_url, "DELETE", &path, &token, None, None).await {
            // Status 404 → success path. Extract code from error string.
            let is_404 = e.starts_with("API error 404");
            if !is_404 {
                // Non-404 failure: still clean up local state, then return error.
                do_remove_local(model_id, market_state, market_dispatch, pyramid).await;
                return Err(classify_send_error(e));
            }
        }
    }

    do_remove_local(model_id, market_state, market_dispatch, pyramid).await;
    Ok(())
}

/// Internal helper for `remove_offer` — cleans up local state and nudges
/// the mirror. Factored so both the happy path and the 404-tolerance
/// path can share it.
async fn do_remove_local(
    model_id: &str,
    market_state: &Arc<RwLock<ComputeMarketState>>,
    market_dispatch: &Arc<MarketDispatchContext>,
    pyramid: &Arc<crate::pyramid::PyramidState>,
) {
    let data_dir = resolve_data_dir(pyramid);
    {
        let mut ms = market_state.write().await;
        ms.offers.remove(model_id);
        ms.last_evaluation_at = Some(chrono::Utc::now().to_rfc3339());
        persist_market_state(&ms, &data_dir);
    }
    let _ = market_dispatch.mirror_nudge.send(());
}

/// List all offers this node has published. Read-only snapshot.
pub async fn list_offers(
    market_state: &Arc<RwLock<ComputeMarketState>>,
) -> Vec<ComputeOffer> {
    let ms = market_state.read().await;
    ms.offers.values().cloned().collect()
}

/// Fetch the market surface from Wire — passes through verbatim so the
/// caller can render whatever fields Wire returns without this module
/// tracking the schema.
pub async fn market_surface(
    model_id: Option<&str>,
    auth: &Arc<RwLock<AuthState>>,
    config: &Arc<RwLock<WireNodeConfig>>,
) -> Result<serde_json::Value, ComputeMarketOpError> {
    let (api_url, token) = {
        let cfg = config.read().await;
        (cfg.api_url.clone(), get_api_token(auth).await?)
    };
    let path = match model_id {
        Some(m) if !m.is_empty() => format!(
            "/api/v1/compute/market-surface?model_id={}",
            urlencoding::encode(m)
        ),
        _ => "/api/v1/compute/market-surface".to_string(),
    };
    match send_api_request(&api_url, "GET", &path, &token, None, None).await {
        Ok((_, resp)) => Ok(resp),
        Err(e) => Err(classify_send_error(e)),
    }
}

/// Set the runtime `is_serving` flag. Does NOT modify the durable
/// `compute_participation_policy` contribution — `allow_market_visibility
/// = false` still prevents publishing regardless of this flag.
pub async fn set_serving(
    enabled: bool,
    market_state: &Arc<RwLock<ComputeMarketState>>,
    market_dispatch: &Arc<MarketDispatchContext>,
    pyramid: &Arc<crate::pyramid::PyramidState>,
) -> Result<(), ComputeMarketOpError> {
    let data_dir = resolve_data_dir(pyramid);
    {
        let mut ms = market_state.write().await;
        ms.is_serving = enabled;
        ms.last_evaluation_at = Some(chrono::Utc::now().to_rfc3339());
        persist_market_state(&ms, &data_dir);
    }
    let _ = market_dispatch.mirror_nudge.send(());
    Ok(())
}

/// Read the full `ComputeMarketState` for observability. Returned by
/// value (clone under read lock) so callers don't hold the lock.
pub async fn get_state(
    market_state: &Arc<RwLock<ComputeMarketState>>,
) -> ComputeMarketState {
    market_state.read().await.clone()
}

// ═════════════════════════════════════════════════════════════════════════
// Error classification
// ═════════════════════════════════════════════════════════════════════════

/// Parse a `send_api_request` error string into our typed error.
///
/// `send_api_request` returns `Err(String)` in two cases:
///   1. "API error {code}: {body}" for non-2xx responses
///   2. reqwest network failures (timeouts, DNS, conn refused)
///
/// We want case 1 to surface as `WireRejected` (so HTTP callers return
/// 502), and case 2 as `Internal` (so HTTP callers return 500 — the
/// node-to-Wire path is broken, not a request-level problem).
fn classify_send_error(e: String) -> ComputeMarketOpError {
    // Expected format: "API error 409: {...}"
    if let Some(rest) = e.strip_prefix("API error ") {
        // Split at first ':' to get the status code.
        if let Some((code_str, body)) = rest.split_once(':') {
            if let Ok(status) = code_str.trim().parse::<u16>() {
                return ComputeMarketOpError::WireRejected {
                    status,
                    body: body.trim().to_string(),
                };
            }
        }
    }
    ComputeMarketOpError::Internal(e)
}

// ═════════════════════════════════════════════════════════════════════════
// Chronicle emission helper
// ═════════════════════════════════════════════════════════════════════════

/// Fire-and-forget chronicle write for `market_offered`. Matches the IPC
/// layer's previous behavior — blocking DB open + record_event on a
/// tokio blocking thread, log on failure.
fn emit_market_offered_event(
    data_dir: &Path,
    model_id: &str,
    rate_in: i64,
    rate_out: i64,
    reservation_fee: i64,
    wire_offer_id: &str,
) {
    let db_path = data_dir.join("pyramid.db");
    let job_path = format!("market/offer/{}", model_id);
    let ctx = ChronicleEventContext::minimal(
        &job_path,
        compute_chronicle::EVENT_MARKET_OFFERED,
        compute_chronicle::SOURCE_MARKET,
    )
    .with_model_id(model_id.to_string())
    // UUID-OR-HANDLE-PATH: the `wire_offer_id` value in chronicle
    // metadata is stored verbatim as a string. UUIDs today, handle-paths
    // post-migration. Chronicle UI/query paths should render whatever
    // shape the value has without assuming UUID structure.
    .with_metadata(serde_json::json!({
        "rate_per_m_input": rate_in,
        "rate_per_m_output": rate_out,
        "reservation_fee": reservation_fee,
        "wire_offer_id": wire_offer_id,
    }));
    tokio::task::spawn_blocking(move || {
        if let Ok(conn) = rusqlite::Connection::open(&db_path) {
            let _ = compute_chronicle::record_event(&conn, &ctx);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_error_http_status_mapping() {
        use warp::http::StatusCode;
        assert_eq!(
            ComputeMarketOpError::BadRequest("x".into()).http_status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            ComputeMarketOpError::PreconditionFailed("x".into()).http_status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            ComputeMarketOpError::WireRejected {
                status: 500,
                body: "x".into()
            }
            .http_status(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            ComputeMarketOpError::AuthUnavailable("x".into()).http_status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            ComputeMarketOpError::Internal("x".into()).http_status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn offer_request_default_provider_type() {
        let json = serde_json::json!({
            "model_id": "test-model",
            "rate_per_m_input": 100,
            "rate_per_m_output": 200,
            "reservation_fee": 10,
            "max_queue_depth": 5,
        });
        let req: OfferRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.provider_type, "local");
    }

    #[test]
    fn offer_request_explicit_provider_type() {
        let json = serde_json::json!({
            "model_id": "test-model",
            "provider_type": "bridge",
            "rate_per_m_input": 100,
            "rate_per_m_output": 200,
            "reservation_fee": 10,
            "max_queue_depth": 5,
        });
        let req: OfferRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.provider_type, "bridge");
    }

    #[test]
    fn classify_send_error_wire_rejected_with_numeric_code() {
        let e = classify_send_error(
            "API error 409: {\"error\":\"model_not_loaded\"}".to_string(),
        );
        match e {
            ComputeMarketOpError::WireRejected { status, body } => {
                assert_eq!(status, 409);
                assert!(body.contains("model_not_loaded"));
            }
            other => panic!("expected WireRejected, got {other:?}"),
        }
    }

    #[test]
    fn classify_send_error_falls_through_to_internal() {
        let e = classify_send_error("reqwest: connection refused".to_string());
        matches!(e, ComputeMarketOpError::Internal(_));
    }

    #[test]
    fn classify_send_error_malformed_status() {
        // Non-numeric after "API error " falls through to Internal.
        let e = classify_send_error("API error foo: body".to_string());
        matches!(e, ComputeMarketOpError::Internal(_));
    }
}
