// pyramid/routes_operator.rs — Operator-facing HTTP routes
//
// These routes expose compute market operations, system observability,
// and node configuration to local agents and CLI tooling. They are the
// HTTP counterpart of the Tauri IPC commands that the desktop UI uses;
// a headless agent should be able to drive the node end-to-end through
// this surface.
//
// Auth: LOCAL-ONLY (reuses `routes::with_auth_state`). Wire JWTs are
// rejected — these routes can mutate node state (offer lifecycle,
// market enable/disable, model loading) and aren't safe for remote use.
// The same local bearer token from `pyramid_config.json` that gates
// /pyramid/:slug/* routes gates these too.
//
// Error shape: all handlers return warp::reply::Response with a JSON
// body. 2xx returns the operation data; 4xx/5xx returns `{ "error": "..." }`
// matching the convention in `http_utils::json_error`.
//
// Route groups:
//   /pyramid/compute/*           — compute market (offers + market state + policy)
//   /pyramid/system/*            — node observability (health, credits, tunnel, etc.)
//   /pyramid/:slug/local-mode/*  — local-mode control (enable/disable/switch)
//   /pyramid/:slug/providers     — provider list

use crate::compute_market::ComputeMarketState;
use crate::http_utils::{json_error, json_ok};
use crate::pyramid::compute_market_ops;
use crate::pyramid::compute_requester::{self, LatencyPreference, MarketInferenceRequest};
use crate::pyramid::market_dispatch::MarketDispatchContext;
use crate::pyramid::pending_jobs::PendingJobs;
use crate::pyramid::PyramidState;
use crate::work::WorkStats;
use crate::WireNodeConfig;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use warp::Filter;
use warp::Reply;

use super::routes::with_auth_state;

// ════════════════════════════════════════════════════════════════════════
// Context — the ensemble of handles every operator handler needs.
// ════════════════════════════════════════════════════════════════════════

/// Bundle of handles threaded through operator route handlers. Lets the
/// warp filter layer clone a single Arc per route instead of building
/// 8-argument closures.
#[derive(Clone)]
pub struct OperatorContext {
    pub pyramid: Arc<PyramidState>,
    pub auth: Arc<RwLock<crate::auth::AuthState>>,
    pub credits: Arc<RwLock<crate::credits::CreditTracker>>,
    pub config: Arc<RwLock<WireNodeConfig>>,
    pub tunnel_state: Arc<RwLock<crate::tunnel::TunnelState>>,
    pub sync_state: Arc<RwLock<crate::sync::SyncState>>,
    pub fleet_roster: Arc<RwLock<crate::fleet::FleetRoster>>,
    pub work_stats: Arc<RwLock<WorkStats>>,
    pub node_id: Arc<RwLock<String>>,
    /// `None` in test fixtures / pre-init boot; production always
    /// constructs both before starting the HTTP server. Routes that
    /// touch these gate on 503 when absent.
    pub compute_market_state: Option<Arc<RwLock<ComputeMarketState>>>,
    pub compute_market_dispatch: Option<Arc<MarketDispatchContext>>,
    /// Phase 3 requester-side PendingJobs registry, shared with the
    /// `/v1/compute/job-result` inbound handler in `server.rs`.
    /// Operator-facing smoke/test routes (compute-market-call) register
    /// and await entries here.
    pub pending_market_jobs: PendingJobs,
}

fn market_state_or_503(
    ctx: &OperatorContext,
) -> Result<(Arc<RwLock<ComputeMarketState>>, Arc<MarketDispatchContext>), warp::reply::Response> {
    match (
        ctx.compute_market_state.as_ref(),
        ctx.compute_market_dispatch.as_ref(),
    ) {
        (Some(ms), Some(md)) => Ok((ms.clone(), md.clone())),
        _ => Err(json_error(
            warp::http::StatusCode::SERVICE_UNAVAILABLE,
            "compute market not initialized on this node",
        )),
    }
}

/// Map a `ComputeMarketOpError` onto a JSON error response with the
/// right status code.
fn op_error_to_response(e: compute_market_ops::ComputeMarketOpError) -> warp::reply::Response {
    json_error(e.http_status(), &e.to_string())
}

// ════════════════════════════════════════════════════════════════════════
// Route entry point
// ════════════════════════════════════════════════════════════════════════

/// Build the operator route tree. Composed into the main router in
/// `server::start_server` alongside pyramid_routes.
pub fn operator_routes(
    ctx: OperatorContext,
) -> warp::filters::BoxedFilter<(warp::reply::Response,)> {
    let prefix = warp::path("pyramid");

    // Helper: clone ctx into a route's body. Each route gets its own
    // clone because warp filters own the captures.
    macro_rules! with_ctx {
        () => {{
            let ctx = ctx.clone();
            warp::any().map(move || ctx.clone())
        }};
    }

    macro_rules! route {
        ($filter:expr) => {
            $filter.map(|r: warp::reply::Response| r).boxed()
        };
    }

    // ── /pyramid/compute/... ────────────────────────────────────────────

    // POST /pyramid/compute/offers
    let compute_offers_create = route!(prefix
        .and(warp::path("compute"))
        .and(warp::path("offers"))
        .and(warp::path::end())
        .and(warp::post())
        .and(with_auth_state(ctx.pyramid.clone()))
        .and(warp::body::json::<compute_market_ops::OfferRequest>())
        .and(with_ctx!())
        .and_then(handle_compute_offer_create));

    // PUT /pyramid/compute/offers/:model_id — upsert
    let compute_offers_update = route!(prefix
        .and(warp::path("compute"))
        .and(warp::path("offers"))
        .and(warp::path::param::<String>())
        .and(warp::path::end())
        .and(warp::put())
        .and(with_auth_state(ctx.pyramid.clone()))
        .and(warp::body::json::<compute_market_ops::OfferRequest>())
        .and(with_ctx!())
        .and_then(handle_compute_offer_update));

    // DELETE /pyramid/compute/offers/:model_id
    let compute_offers_delete = route!(prefix
        .and(warp::path("compute"))
        .and(warp::path("offers"))
        .and(warp::path::param::<String>())
        .and(warp::path::end())
        .and(warp::delete())
        .and(with_auth_state(ctx.pyramid.clone()))
        .and(with_ctx!())
        .and_then(handle_compute_offer_delete));

    // GET /pyramid/compute/offers
    let compute_offers_list = route!(prefix
        .and(warp::path("compute"))
        .and(warp::path("offers"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(ctx.pyramid.clone()))
        .and(with_ctx!())
        .and_then(handle_compute_offers_list));

    // GET /pyramid/compute/market/surface?model_id=...
    let compute_market_surface = route!(prefix
        .and(warp::path("compute"))
        .and(warp::path("market"))
        .and(warp::path("surface"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(ctx.pyramid.clone()))
        .and(warp::query::<HashMap<String, String>>())
        .and(with_ctx!())
        .and_then(handle_compute_market_surface));

    // POST /pyramid/compute/market/enable
    let compute_market_enable = route!(prefix
        .and(warp::path("compute"))
        .and(warp::path("market"))
        .and(warp::path("enable"))
        .and(warp::path::end())
        .and(warp::post())
        .and(with_auth_state(ctx.pyramid.clone()))
        .and(with_ctx!())
        .and_then(|_pyramid, ctx| handle_compute_market_set_serving(true, ctx)));

    // POST /pyramid/compute/market/disable
    let compute_market_disable = route!(prefix
        .and(warp::path("compute"))
        .and(warp::path("market"))
        .and(warp::path("disable"))
        .and(warp::path::end())
        .and(warp::post())
        .and(with_auth_state(ctx.pyramid.clone()))
        .and(with_ctx!())
        .and_then(|_pyramid, ctx| handle_compute_market_set_serving(false, ctx)));

    // GET /pyramid/compute/market/state
    let compute_market_state_route = route!(prefix
        .and(warp::path("compute"))
        .and(warp::path("market"))
        .and(warp::path("state"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(ctx.pyramid.clone()))
        .and(with_ctx!())
        .and_then(handle_compute_market_state));

    // GET /pyramid/compute/policy — compute participation policy (durable contribution)
    let compute_policy_get = route!(prefix
        .and(warp::path("compute"))
        .and(warp::path("policy"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(ctx.pyramid.clone()))
        .and_then(handle_compute_policy_get));

    // PUT /pyramid/compute/policy
    let compute_policy_set = route!(prefix
        .and(warp::path("compute"))
        .and(warp::path("policy"))
        .and(warp::path::end())
        .and(warp::put())
        .and(with_auth_state(ctx.pyramid.clone()))
        .and(warp::body::json::<
            crate::pyramid::local_mode::ComputeParticipationPolicy,
        >())
        .and_then(handle_compute_policy_set));

    // ── /pyramid/system/... ─────────────────────────────────────────────

    // GET /pyramid/system/health
    let system_health = route!(prefix
        .and(warp::path("system"))
        .and(warp::path("health"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(ctx.pyramid.clone()))
        .and(with_ctx!())
        .and_then(handle_system_health));

    // GET /pyramid/system/credits
    let system_credits = route!(prefix
        .and(warp::path("system"))
        .and(warp::path("credits"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(ctx.pyramid.clone()))
        .and(with_ctx!())
        .and_then(handle_system_credits));

    // GET /pyramid/system/work-stats
    let system_work_stats = route!(prefix
        .and(warp::path("system"))
        .and(warp::path("work-stats"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(ctx.pyramid.clone()))
        .and(with_ctx!())
        .and_then(handle_system_work_stats));

    // GET /pyramid/system/fleet-roster
    let system_fleet_roster = route!(prefix
        .and(warp::path("system"))
        .and(warp::path("fleet-roster"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(ctx.pyramid.clone()))
        .and(with_ctx!())
        .and_then(handle_system_fleet_roster));

    // GET /pyramid/system/tunnel
    let system_tunnel = route!(prefix
        .and(warp::path("system"))
        .and(warp::path("tunnel"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(ctx.pyramid.clone()))
        .and(with_ctx!())
        .and_then(handle_system_tunnel));

    // GET /pyramid/system/auth
    let system_auth = route!(prefix
        .and(warp::path("system"))
        .and(warp::path("auth"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(ctx.pyramid.clone()))
        .and(with_ctx!())
        .and_then(handle_system_auth));

    // GET /pyramid/system/compute/events
    let system_compute_events = route!(prefix
        .and(warp::path("system"))
        .and(warp::path("compute"))
        .and(warp::path("events"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(ctx.pyramid.clone()))
        .and(warp::query::<HashMap<String, String>>())
        .and(with_ctx!())
        .and_then(handle_system_compute_events));

    // GET /pyramid/system/compute/summary
    let system_compute_summary = route!(prefix
        .and(warp::path("system"))
        .and(warp::path("compute"))
        .and(warp::path("summary"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(ctx.pyramid.clone()))
        .and(warp::query::<HashMap<String, String>>())
        .and(with_ctx!())
        .and_then(handle_system_compute_summary));

    // GET /pyramid/system/compute/timeline
    let system_compute_timeline = route!(prefix
        .and(warp::path("system"))
        .and(warp::path("compute"))
        .and(warp::path("timeline"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(ctx.pyramid.clone()))
        .and(warp::query::<HashMap<String, String>>())
        .and(with_ctx!())
        .and_then(handle_system_compute_timeline));

    // GET /pyramid/system/compute/chronicle-dimensions
    let system_compute_dimensions = route!(prefix
        .and(warp::path("system"))
        .and(warp::path("compute"))
        .and(warp::path("chronicle-dimensions"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(ctx.pyramid.clone()))
        .and(with_ctx!())
        .and_then(handle_system_compute_dimensions));

    // ── /pyramid/:slug/local-mode/... + providers ───────────────────────

    // GET /pyramid/:slug/local-mode — status snapshot
    //
    // Note: local-mode status is currently node-scoped, not slug-scoped;
    // the slug is accepted for URL symmetry with other slug-prefixed
    // routes but isn't used. Future per-slug local mode would plug in
    // here without breaking the URL shape.
    let local_mode_status = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("local-mode"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(ctx.pyramid.clone()))
        .and_then(|_slug: String, pyramid| handle_local_mode_status(pyramid)));

    // POST /pyramid/:slug/local-mode/enable
    let local_mode_enable = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("local-mode"))
        .and(warp::path("enable"))
        .and(warp::path::end())
        .and(warp::post())
        .and(with_auth_state(ctx.pyramid.clone()))
        .and(warp::body::json::<LocalModeEnableBody>())
        .and_then(|_slug: String, pyramid, body: LocalModeEnableBody| {
            handle_local_mode_enable(pyramid, body)
        }));

    // POST /pyramid/:slug/local-mode/disable
    let local_mode_disable = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("local-mode"))
        .and(warp::path("disable"))
        .and(warp::path::end())
        .and(warp::post())
        .and(with_auth_state(ctx.pyramid.clone()))
        .and_then(|_slug: String, pyramid| handle_local_mode_disable(pyramid)));

    // POST /pyramid/:slug/local-mode/switch-model
    let local_mode_switch = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("local-mode"))
        .and(warp::path("switch-model"))
        .and(warp::path::end())
        .and(warp::post())
        .and(with_auth_state(ctx.pyramid.clone()))
        .and(warp::body::json::<LocalModeSwitchBody>())
        .and_then(|_slug: String, pyramid, body: LocalModeSwitchBody| {
            handle_local_mode_switch(pyramid, body)
        }));

    // GET /pyramid/:slug/providers
    let providers_list = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("providers"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(ctx.pyramid.clone()))
        .and_then(|_slug: String, pyramid| handle_providers_list(pyramid)));

    // POST /pyramid/compute/market-call — Phase 3 smoke entry point.
    //
    // One-shot "ask the market to run this inference." Blocks on the
    // push-delivery round-trip and returns the content to the caller.
    // This is the primitive call_model_unified will later delegate to
    // for its market-dispatch branch; exposing it directly here gives
    // us a CLI-level smoke surface without plumbing through the full
    // LLM pipeline.
    //
    // LOCAL-ONLY auth (operator Bearer). Unauthorized callers can't
    // drain the operator's balance.
    let compute_market_call = route!(prefix
        .and(warp::path("compute"))
        .and(warp::path("market-call"))
        .and(warp::path::end())
        .and(warp::post())
        .and(with_auth_state(ctx.pyramid.clone()))
        .and(warp::body::json::<MarketCallBody>())
        .and(with_ctx!())
        .and_then(|_pyramid, body, ctx| handle_compute_market_call(body, ctx)));

    // ── Compose ─────────────────────────────────────────────────────────
    //
    // Warp's `.or().or()` chaining builds `Either` types that expand in
    // compile-time at an exponential rate. We use `.boxed()` via the
    // `route!` macro to tame the type explosion — each branch is the
    // same concrete `BoxedFilter<(Response,)>` so the chain type is
    // shallow. This mirrors the pattern in `pyramid_routes`.

    // Batched `.or().unify().boxed()` chains to cap the warp type-
    // explosion. Each `.or().unify()` link adds a nested Either in
    // the filter's output type; the compiler blows the Unpin-check
    // recursion at ~20 links without intermediate `.boxed()` resets.
    // Batch into logical groups of ~5 routes, box the group result,
    // then flatten across groups.
    let compute_offers_group = compute_offers_create
        .or(compute_offers_update)
        .unify()
        .or(compute_offers_delete)
        .unify()
        .or(compute_offers_list)
        .unify()
        .or(compute_market_surface)
        .unify()
        .boxed();

    let compute_market_group = compute_market_enable
        .or(compute_market_disable)
        .unify()
        .or(compute_market_state_route)
        .unify()
        .or(compute_policy_get)
        .unify()
        .or(compute_policy_set)
        .unify()
        .or(compute_market_call)
        .unify()
        .boxed();

    let system_group_a = system_health
        .or(system_credits)
        .unify()
        .or(system_work_stats)
        .unify()
        .or(system_fleet_roster)
        .unify()
        .or(system_tunnel)
        .unify()
        .or(system_auth)
        .unify()
        .boxed();

    let system_group_b = system_compute_events
        .or(system_compute_summary)
        .unify()
        .or(system_compute_timeline)
        .unify()
        .or(system_compute_dimensions)
        .unify()
        .boxed();

    let local_mode_group = local_mode_status
        .or(local_mode_enable)
        .unify()
        .or(local_mode_disable)
        .unify()
        .or(local_mode_switch)
        .unify()
        .or(providers_list)
        .unify()
        .boxed();

    compute_offers_group
        .or(compute_market_group)
        .unify()
        .or(system_group_a)
        .unify()
        .or(system_group_b)
        .unify()
        .or(local_mode_group)
        .unify()
        .boxed()
}

// ════════════════════════════════════════════════════════════════════════
// Handlers — compute market
// ════════════════════════════════════════════════════════════════════════

async fn handle_compute_offer_create(
    _pyramid: Arc<PyramidState>,
    body: compute_market_ops::OfferRequest,
    ctx: OperatorContext,
) -> Result<warp::reply::Response, warp::Rejection> {
    let (market_state, market_dispatch) = match market_state_or_503(&ctx) {
        Ok(handles) => handles,
        Err(resp) => return Ok(resp),
    };
    match compute_market_ops::create_offer(
        body,
        &ctx.auth,
        &ctx.config,
        &market_state,
        &market_dispatch,
        &ctx.pyramid,
    )
    .await
    {
        // UUID-OR-HANDLE-PATH: `offer_id` echoed verbatim in the HTTP
        // response. Agent/CLI callers treat it as an opaque string; no
        // assumption of UUID shape anywhere downstream.
        Ok(offer_id) => Ok(json_ok(&serde_json::json!({ "offer_id": offer_id }))),
        Err(e) => Ok(op_error_to_response(e)),
    }
}

/// Update is upsert — same backend call as create. The `model_id` in the
/// URL path must match the body's `model_id` (we reject mismatches to
/// avoid the HTTP-semantic confusion of silent renames).
///
/// Warp extracts args in the order the `.and()` chain declares them:
/// path_param → with_auth_state(pyramid) → body → ctx.
async fn handle_compute_offer_update(
    path_model_id: String,
    _pyramid: Arc<PyramidState>,
    body: compute_market_ops::OfferRequest,
    ctx: OperatorContext,
) -> Result<warp::reply::Response, warp::Rejection> {
    // URL-decode the path param before comparing. `warp::path::param`
    // does NOT decode percent-encoded segments, so a colon-bearing
    // model name like `gemma4:26b` arrives as `gemma4%3A26b` here.
    // Agents that percent-encode the path (every HTTP client does)
    // would false-match-fail without this. Bug caught during prod
    // smoke 2026-04-17.
    let decoded_path = match urlencoding::decode(&path_model_id) {
        Ok(s) => s.into_owned(),
        Err(_) => path_model_id.clone(),
    };
    if decoded_path != body.model_id {
        return Ok(json_error(
            warp::http::StatusCode::BAD_REQUEST,
            "model_id in URL path must match model_id in body",
        ));
    }
    let (market_state, market_dispatch) = match market_state_or_503(&ctx) {
        Ok(handles) => handles,
        Err(resp) => return Ok(resp),
    };
    match compute_market_ops::create_offer(
        body,
        &ctx.auth,
        &ctx.config,
        &market_state,
        &market_dispatch,
        &ctx.pyramid,
    )
    .await
    {
        Ok(offer_id) => Ok(json_ok(&serde_json::json!({ "offer_id": offer_id }))),
        Err(e) => Ok(op_error_to_response(e)),
    }
}

async fn handle_compute_offer_delete(
    model_id: String,
    _pyramid: Arc<PyramidState>,
    ctx: OperatorContext,
) -> Result<warp::reply::Response, warp::Rejection> {
    let (market_state, market_dispatch) = match market_state_or_503(&ctx) {
        Ok(handles) => handles,
        Err(resp) => return Ok(resp),
    };
    // URL-decode the path param — warp doesn't do it for us. Same bug
    // as the update handler; see that function's decode comment.
    let decoded_model_id = match urlencoding::decode(&model_id) {
        Ok(s) => s.into_owned(),
        Err(_) => model_id.clone(),
    };
    match compute_market_ops::remove_offer(
        &decoded_model_id,
        &ctx.auth,
        &ctx.config,
        &market_state,
        &market_dispatch,
        &ctx.pyramid,
    )
    .await
    {
        Ok(()) => Ok(json_ok(&serde_json::json!({ "ok": true }))),
        Err(e) => Ok(op_error_to_response(e)),
    }
}

async fn handle_compute_offers_list(
    _pyramid: Arc<PyramidState>,
    ctx: OperatorContext,
) -> Result<warp::reply::Response, warp::Rejection> {
    let market_state = match ctx.compute_market_state.as_ref() {
        Some(ms) => ms.clone(),
        None => {
            // No market initialized → empty list is the correct answer.
            return Ok(json_ok(&serde_json::json!({ "offers": [] })));
        }
    };
    let offers = compute_market_ops::list_offers(&market_state).await;
    Ok(json_ok(&serde_json::json!({ "offers": offers })))
}

async fn handle_compute_market_surface(
    _pyramid: Arc<PyramidState>,
    query: HashMap<String, String>,
    ctx: OperatorContext,
) -> Result<warp::reply::Response, warp::Rejection> {
    let model_id = query.get("model_id").map(|s| s.as_str());
    match compute_market_ops::market_surface(model_id, &ctx.auth, &ctx.config).await {
        Ok(surface) => Ok(json_ok(&surface)),
        Err(e) => Ok(op_error_to_response(e)),
    }
}

async fn handle_compute_market_set_serving(
    enabled: bool,
    ctx: OperatorContext,
) -> Result<warp::reply::Response, warp::Rejection> {
    let (market_state, market_dispatch) = match market_state_or_503(&ctx) {
        Ok(handles) => handles,
        Err(resp) => return Ok(resp),
    };
    match compute_market_ops::set_serving(enabled, &market_state, &market_dispatch, &ctx.pyramid)
        .await
    {
        Ok(()) => Ok(json_ok(&serde_json::json!({
            "ok": true,
            "is_serving": enabled,
        }))),
        Err(e) => Ok(op_error_to_response(e)),
    }
}

async fn handle_compute_market_state(
    _pyramid: Arc<PyramidState>,
    ctx: OperatorContext,
) -> Result<warp::reply::Response, warp::Rejection> {
    let market_state = match ctx.compute_market_state.as_ref() {
        Some(ms) => ms.clone(),
        None => {
            return Ok(json_error(
                warp::http::StatusCode::SERVICE_UNAVAILABLE,
                "compute market not initialized on this node",
            ));
        }
    };
    let snapshot = compute_market_ops::get_state(&market_state).await;
    Ok(json_ok(&snapshot))
}

async fn handle_compute_policy_get(
    pyramid: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let reader = pyramid.reader.lock().await;
    match crate::pyramid::local_mode::get_compute_participation_policy(&reader) {
        Ok(policy) => Ok(json_ok(&policy)),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

async fn handle_compute_policy_set(
    pyramid: Arc<PyramidState>,
    policy: crate::pyramid::local_mode::ComputeParticipationPolicy,
) -> Result<warp::reply::Response, warp::Rejection> {
    let mut writer = pyramid.writer.lock().await;
    match crate::pyramid::local_mode::set_compute_participation_policy(
        &mut writer,
        &pyramid.build_event_bus,
        &policy,
    ) {
        Ok(()) => Ok(json_ok(
            &serde_json::json!({ "ok": true, "message": "Compute participation policy updated" }),
        )),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

// ════════════════════════════════════════════════════════════════════════
// Handlers — system observability
// ════════════════════════════════════════════════════════════════════════

async fn handle_system_health(
    _pyramid: Arc<PyramidState>,
    ctx: OperatorContext,
) -> Result<warp::reply::Response, warp::Rejection> {
    let credits = ctx.credits.read().await.clone();
    let tunnel = ctx.tunnel_state.read().await.clone();
    let node_id = ctx.node_id.read().await.clone();
    let auth = ctx.auth.read().await;
    let has_api_token = auth.api_token.as_deref().map_or(false, |t| !t.is_empty());
    drop(auth);

    let version = env!("CARGO_PKG_VERSION").to_string();
    Ok(json_ok(&serde_json::json!({
        "status": "ok",
        "version": version,
        "node_id": node_id,
        "authenticated": has_api_token,
        "credits": credits,
        "tunnel": tunnel,
    })))
}

async fn handle_system_credits(
    _pyramid: Arc<PyramidState>,
    ctx: OperatorContext,
) -> Result<warp::reply::Response, warp::Rejection> {
    let credits = ctx.credits.read().await.clone();
    Ok(json_ok(&credits))
}

async fn handle_system_work_stats(
    _pyramid: Arc<PyramidState>,
    ctx: OperatorContext,
) -> Result<warp::reply::Response, warp::Rejection> {
    let work = ctx.work_stats.read().await.clone();
    Ok(json_ok(&work))
}

async fn handle_system_fleet_roster(
    _pyramid: Arc<PyramidState>,
    ctx: OperatorContext,
) -> Result<warp::reply::Response, warp::Rejection> {
    let roster = ctx.fleet_roster.read().await;
    match serde_json::to_value(&*roster) {
        Ok(val) => Ok(json_ok(&val)),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &format!("fleet_roster serialize: {e}"),
        )),
    }
}

async fn handle_system_tunnel(
    _pyramid: Arc<PyramidState>,
    ctx: OperatorContext,
) -> Result<warp::reply::Response, warp::Rejection> {
    let tunnel = ctx.tunnel_state.read().await.clone();
    Ok(json_ok(&tunnel))
}

/// Whoami — returns enough identity info for an agent to know which
/// node it's talking to and whether auth is working. Does NOT leak the
/// api_token or any other secrets.
async fn handle_system_auth(
    _pyramid: Arc<PyramidState>,
    ctx: OperatorContext,
) -> Result<warp::reply::Response, warp::Rejection> {
    let auth = ctx.auth.read().await;
    let node_id = ctx.node_id.read().await.clone();
    let config = ctx.config.read().await;
    let api_url = config.api_url.clone();
    drop(config);

    let body = serde_json::json!({
        "node_id": node_id,
        "operator_id": auth.user_id.clone(),
        "has_api_token": auth.api_token.as_deref().map_or(false, |t| !t.is_empty()),
        "has_operator_session": auth.operator_session_token.as_deref().map_or(false, |t| !t.is_empty()),
        "operator_session_expires_at": auth.operator_session_expires_at.clone(),
        "api_url": api_url,
    });
    Ok(json_ok(&body))
}

async fn handle_system_compute_events(
    _pyramid: Arc<PyramidState>,
    query: HashMap<String, String>,
    ctx: OperatorContext,
) -> Result<warp::reply::Response, warp::Rejection> {
    let db_path = match ctx.pyramid.data_dir.as_ref() {
        Some(d) => d.join("pyramid.db"),
        None => {
            return Ok(json_error(
                warp::http::StatusCode::SERVICE_UNAVAILABLE,
                "no pyramid data_dir configured",
            ))
        }
    };
    let filters = crate::pyramid::compute_chronicle::ChronicleQueryFilters {
        slug: query.get("slug").cloned(),
        build_id: query.get("build_id").cloned(),
        chain_name: query.get("chain_name").cloned(),
        content_type: query.get("content_type").cloned(),
        step_name: query.get("step_name").cloned(),
        primitive: query.get("primitive").cloned(),
        depth: query.get("depth").and_then(|s| s.parse().ok()),
        model_id: query.get("model_id").cloned(),
        source: query.get("source").cloned(),
        event_type: query.get("event_type").cloned(),
        after: query.get("after").cloned(),
        before: query.get("before").cloned(),
        limit: query
            .get("limit")
            .and_then(|s| s.parse().ok())
            .unwrap_or(100),
        offset: query
            .get("offset")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0),
    };
    let result = tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open(&db_path).map_err(|e| e.to_string())?;
        crate::pyramid::compute_chronicle::query_events(&conn, &filters).map_err(|e| e.to_string())
    })
    .await;
    match result {
        Ok(Ok(events)) => Ok(json_ok(&serde_json::json!({ "events": events }))),
        Ok(Err(e)) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e,
        )),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

async fn handle_system_compute_summary(
    _pyramid: Arc<PyramidState>,
    query: HashMap<String, String>,
    ctx: OperatorContext,
) -> Result<warp::reply::Response, warp::Rejection> {
    let period_start = match query.get("period_start").cloned() {
        Some(s) => s,
        None => {
            return Ok(json_error(
                warp::http::StatusCode::BAD_REQUEST,
                "period_start query param is required (RFC3339 timestamp)",
            ))
        }
    };
    let period_end = match query.get("period_end").cloned() {
        Some(s) => s,
        None => {
            return Ok(json_error(
                warp::http::StatusCode::BAD_REQUEST,
                "period_end query param is required (RFC3339 timestamp)",
            ))
        }
    };
    let group_by = query
        .get("group_by")
        .cloned()
        .unwrap_or_else(|| "source".to_string());

    let db_path = match ctx.pyramid.data_dir.as_ref() {
        Some(d) => d.join("pyramid.db"),
        None => {
            return Ok(json_error(
                warp::http::StatusCode::SERVICE_UNAVAILABLE,
                "no pyramid data_dir configured",
            ))
        }
    };
    let result = tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open(&db_path).map_err(|e| e.to_string())?;
        crate::pyramid::compute_chronicle::query_summary(&conn, &period_start, &period_end, &group_by)
            .map_err(|e| e.to_string())
    })
    .await;
    match result {
        Ok(Ok(summary)) => Ok(json_ok(&serde_json::json!({ "summary": summary }))),
        Ok(Err(e)) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e,
        )),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

async fn handle_system_compute_timeline(
    _pyramid: Arc<PyramidState>,
    query: HashMap<String, String>,
    ctx: OperatorContext,
) -> Result<warp::reply::Response, warp::Rejection> {
    let start = match query.get("start").cloned() {
        Some(s) => s,
        None => {
            return Ok(json_error(
                warp::http::StatusCode::BAD_REQUEST,
                "start query param is required (RFC3339 timestamp)",
            ))
        }
    };
    let end = match query.get("end").cloned() {
        Some(s) => s,
        None => {
            return Ok(json_error(
                warp::http::StatusCode::BAD_REQUEST,
                "end query param is required (RFC3339 timestamp)",
            ))
        }
    };
    let bucket_size = query
        .get("bucket_size_minutes")
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);

    let db_path = match ctx.pyramid.data_dir.as_ref() {
        Some(d) => d.join("pyramid.db"),
        None => {
            return Ok(json_error(
                warp::http::StatusCode::SERVICE_UNAVAILABLE,
                "no pyramid data_dir configured",
            ))
        }
    };
    let result = tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open(&db_path).map_err(|e| e.to_string())?;
        crate::pyramid::compute_chronicle::query_timeline(&conn, &start, &end, bucket_size)
            .map_err(|e| e.to_string())
    })
    .await;
    match result {
        Ok(Ok(timeline)) => Ok(json_ok(&serde_json::json!({ "timeline": timeline }))),
        Ok(Err(e)) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e,
        )),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

async fn handle_system_compute_dimensions(
    _pyramid: Arc<PyramidState>,
    ctx: OperatorContext,
) -> Result<warp::reply::Response, warp::Rejection> {
    let db_path = match ctx.pyramid.data_dir.as_ref() {
        Some(d) => d.join("pyramid.db"),
        None => {
            return Ok(json_error(
                warp::http::StatusCode::SERVICE_UNAVAILABLE,
                "no pyramid data_dir configured",
            ))
        }
    };
    let result = tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open(&db_path).map_err(|e| e.to_string())?;
        crate::pyramid::compute_chronicle::query_distinct_dimensions(&conn)
    })
    .await;
    match result {
        Ok(Ok(dims)) => Ok(json_ok(&dims)),
        Ok(Err(e)) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

// ════════════════════════════════════════════════════════════════════════
// Handlers — local mode + providers
// ════════════════════════════════════════════════════════════════════════

#[derive(Deserialize)]
struct LocalModeEnableBody {
    /// Ollama base URL (e.g. "http://localhost:11434/v1"). Required.
    base_url: String,
    /// Ollama model to select (e.g. "llama3.1:8b"). Optional — if None,
    /// local mode is enabled without selecting a specific model.
    #[serde(default)]
    model: Option<String>,
}

#[derive(Deserialize)]
struct LocalModeSwitchBody {
    model: String,
}

async fn handle_local_mode_status(
    pyramid: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    // Mirror the IPC handler's split-lock pattern: snapshot under the
    // reader lock, then probe reachability after dropping the lock so
    // we don't hold it across a slow network call.
    let snapshot = {
        let reader = pyramid.reader.lock().await;
        match crate::pyramid::local_mode::load_status_snapshot(&reader) {
            Ok(s) => s,
            Err(e) => {
                return Ok(json_error(
                    warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                    &e.to_string(),
                ));
            }
        }
    };
    let refreshed = crate::pyramid::local_mode::refresh_status_reachability(snapshot).await;
    Ok(json_ok(&refreshed))
}

async fn handle_local_mode_enable(
    pyramid: Arc<PyramidState>,
    body: LocalModeEnableBody,
) -> Result<warp::reply::Response, warp::Rejection> {
    // Active build guard — matches the IPC layer.
    {
        let active = pyramid.active_build.read().await;
        if !active.is_empty() {
            return Ok(json_error(
                warp::http::StatusCode::CONFLICT,
                "Cannot change model routing while a build is in progress — wait for it to complete or cancel it.",
            ));
        }
    }
    let plan = match crate::pyramid::local_mode::prepare_enable_local_mode(body.base_url, body.model)
        .await
    {
        Ok(p) => p,
        Err(e) => {
            return Ok(json_error(
                warp::http::StatusCode::BAD_REQUEST,
                &e.to_string(),
            ));
        }
    };
    // commit_enable_local_mode returns Result<()> — the IPC layer pulls
    // a fresh snapshot afterward. Mirror that pattern so the cascade
    // rebuild + reachability refresh see post-commit state.
    let snapshot = {
        let mut writer = pyramid.writer.lock().await;
        if let Err(e) = crate::pyramid::local_mode::commit_enable_local_mode(
            &mut writer,
            &pyramid.build_event_bus,
            &pyramid.provider_registry,
            plan,
        ) {
            return Ok(json_error(
                warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                &e.to_string(),
            ));
        }
        match crate::pyramid::local_mode::load_status_snapshot(&writer) {
            Ok(s) => s,
            Err(e) => {
                return Ok(json_error(
                    warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                    &e.to_string(),
                ));
            }
        }
    };
    // Registry refreshed inside commit_enable_local_mode — rebuild the
    // live LlmConfig's cascade model fields so subsequent inference
    // calls use the new provider selection (matches IPC layer).
    crate::pyramid::local_mode::rebuild_cascade_from_registry(&pyramid).await;
    let refreshed = crate::pyramid::local_mode::refresh_status_reachability(snapshot).await;
    Ok(json_ok(&refreshed))
}

async fn handle_local_mode_disable(
    pyramid: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    {
        let active = pyramid.active_build.read().await;
        if !active.is_empty() {
            return Ok(json_error(
                warp::http::StatusCode::CONFLICT,
                "Cannot change model routing while a build is in progress — wait for it to complete or cancel it.",
            ));
        }
    }
    let snapshot = {
        let mut writer = pyramid.writer.lock().await;
        if let Err(e) = crate::pyramid::local_mode::commit_disable_local_mode(
            &mut writer,
            &pyramid.build_event_bus,
            &pyramid.provider_registry,
        ) {
            return Ok(json_error(
                warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                &e.to_string(),
            ));
        }
        match crate::pyramid::local_mode::load_status_snapshot(&writer) {
            Ok(s) => s,
            Err(e) => {
                return Ok(json_error(
                    warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                    &e.to_string(),
                ));
            }
        }
    };
    crate::pyramid::local_mode::rebuild_cascade_from_registry(&pyramid).await;
    let refreshed = crate::pyramid::local_mode::refresh_status_reachability(snapshot).await;
    Ok(json_ok(&refreshed))
}

async fn handle_local_mode_switch(
    pyramid: Arc<PyramidState>,
    body: LocalModeSwitchBody,
) -> Result<warp::reply::Response, warp::Rejection> {
    {
        let active = pyramid.active_build.read().await;
        if !active.is_empty() {
            return Ok(json_error(
                warp::http::StatusCode::CONFLICT,
                "Cannot change model routing while a build is in progress — wait for it to complete or cancel it.",
            ));
        }
    }

    // Split-phase: read base_url synchronously, then prepare async.
    let base_url = {
        let reader = pyramid.reader.lock().await;
        let row = match crate::pyramid::db::load_local_mode_state(&reader) {
            Ok(r) => r,
            Err(e) => {
                return Ok(json_error(
                    warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                    &e.to_string(),
                ));
            }
        };
        if !row.enabled {
            return Ok(json_error(
                warp::http::StatusCode::CONFLICT,
                "Local mode is not enabled — cannot switch model",
            ));
        }
        row.ollama_base_url
            .unwrap_or_else(|| "http://localhost:11434/v1".to_string())
    };

    let plan = match crate::pyramid::local_mode::prepare_enable_local_mode(
        base_url,
        Some(body.model),
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            return Ok(json_error(
                warp::http::StatusCode::BAD_REQUEST,
                &e.to_string(),
            ));
        }
    };
    let snapshot = {
        let mut writer = pyramid.writer.lock().await;
        if let Err(e) = crate::pyramid::local_mode::commit_enable_local_mode(
            &mut writer,
            &pyramid.build_event_bus,
            &pyramid.provider_registry,
            plan,
        ) {
            return Ok(json_error(
                warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                &e.to_string(),
            ));
        }
        match crate::pyramid::local_mode::load_status_snapshot(&writer) {
            Ok(s) => s,
            Err(e) => {
                return Ok(json_error(
                    warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                    &e.to_string(),
                ));
            }
        }
    };
    crate::pyramid::local_mode::rebuild_cascade_from_registry(&pyramid).await;
    let refreshed = crate::pyramid::local_mode::refresh_status_reachability(snapshot).await;
    Ok(json_ok(&refreshed))
}

async fn handle_providers_list(
    pyramid: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let providers = pyramid.provider_registry.list_providers();
    Ok(json_ok(&serde_json::json!({ "providers": providers })))
}

// ════════════════════════════════════════════════════════════════════════
// compute-market-call — one-shot smoke entry point
// ════════════════════════════════════════════════════════════════════════

#[derive(Deserialize)]
struct MarketCallBody {
    model_id: String,
    /// Convenience: plain prompt text. Node constructs a single
    /// ChatML `[{"role": "user", "content": prompt}]` from it. For
    /// structured message arrays, use `messages` instead.
    #[serde(default)]
    prompt: Option<String>,
    /// Full ChatML message array. Takes precedence over `prompt` when
    /// both are present.
    #[serde(default)]
    messages: Option<serde_json::Value>,
    #[serde(default = "default_max_budget")]
    max_budget: i64,
    #[serde(default = "default_input_tokens")]
    input_tokens: i64,
    #[serde(default = "default_max_tokens")]
    max_tokens: usize,
    #[serde(default = "default_temperature")]
    temperature: f32,
    #[serde(default = "default_latency_preference")]
    latency_preference: String,
    #[serde(default = "default_privacy_tier")]
    privacy_tier: String,
    /// Override the default `requester_callback_url`. Useful for
    /// testing; the default is built from the node's tunnel URL.
    #[serde(default)]
    requester_callback_url: Option<String>,
    /// Max wall-clock wait in milliseconds. Defaults to 60s.
    #[serde(default = "default_max_wait_ms")]
    max_wait_ms: u64,
}

fn default_max_budget() -> i64 {
    10_000
}
fn default_input_tokens() -> i64 {
    // Best-effort default. Callers doing anything real should pre-count.
    256
}
fn default_max_tokens() -> usize {
    512
}
fn default_temperature() -> f32 {
    0.7
}
fn default_latency_preference() -> String {
    "best_price".to_string()
}
fn default_privacy_tier() -> String {
    "bootstrap-relay".to_string()
}
fn default_max_wait_ms() -> u64 {
    60_000
}

async fn handle_compute_market_call(
    body: MarketCallBody,
    ctx: OperatorContext,
) -> Result<warp::reply::Response, warp::Rejection> {
    // Build messages from either `prompt` (convenience) or `messages`
    // (explicit). At least one must be supplied.
    let messages = match (body.messages.as_ref(), body.prompt.as_ref()) {
        (Some(m), _) => m.clone(),
        (None, Some(p)) => serde_json::json!([{"role": "user", "content": p}]),
        (None, None) => {
            return Ok(json_error(
                warp::http::StatusCode::BAD_REQUEST,
                "either `messages` or `prompt` is required",
            ));
        }
    };

    // Resolve requester_callback_url. Prefer the body override (tester
    // can point it at a specific URL); fall back to
    // `<tunnel_url>/v1/compute/job-result` per contract §2.5.
    let callback_url = match body.requester_callback_url {
        Some(s) => s,
        None => {
            let tunnel = ctx.tunnel_state.read().await;
            let base = match tunnel.tunnel_url.as_ref() {
                Some(u) => u.as_str().to_string(),
                None => {
                    drop(tunnel);
                    return Ok(json_error(
                        warp::http::StatusCode::SERVICE_UNAVAILABLE,
                        "node tunnel URL unavailable — can't construct requester_callback_url",
                    ));
                }
            };
            drop(tunnel);
            // Strip trailing slash if any, then append the canonical path.
            let base_trimmed = base.trim_end_matches('/');
            format!("{}/v1/compute/job-result", base_trimmed)
        }
    };

    let latency = match body.latency_preference.as_str() {
        "best_price" => LatencyPreference::BestPrice,
        "balanced" => LatencyPreference::Balanced,
        "lowest_latency" => LatencyPreference::LowestLatency,
        other => {
            return Ok(json_error(
                warp::http::StatusCode::BAD_REQUEST,
                &format!(
                    "invalid latency_preference '{}' (want best_price|balanced|lowest_latency)",
                    other
                ),
            ));
        }
    };

    let req = MarketInferenceRequest {
        model_id: body.model_id,
        max_budget: body.max_budget,
        input_tokens: body.input_tokens,
        latency_preference: latency,
        messages,
        max_tokens: body.max_tokens,
        temperature: body.temperature,
        privacy_tier: body.privacy_tier,
        requester_callback_url: callback_url,
    };

    match compute_requester::call_market(
        req,
        &ctx.auth,
        &ctx.config,
        &ctx.pending_market_jobs,
        body.max_wait_ms,
    )
    .await
    {
        Ok(result) => Ok(json_ok(&serde_json::json!({
            "content": result.content,
            "input_tokens": result.input_tokens,
            "output_tokens": result.output_tokens,
            "model_used": result.model_used,
            "latency_ms": result.latency_ms,
            "finish_reason": result.finish_reason,
        }))),
        Err(e) => {
            let status = match &e {
                compute_requester::RequesterError::AuthFailed(_) => {
                    warp::http::StatusCode::UNAUTHORIZED
                }
                compute_requester::RequesterError::InsufficientBalance { .. } => {
                    warp::http::StatusCode::PAYMENT_REQUIRED
                }
                compute_requester::RequesterError::NoMatch { .. }
                | compute_requester::RequesterError::DeliveryTombstoned { .. } => {
                    warp::http::StatusCode::SERVICE_UNAVAILABLE
                }
                compute_requester::RequesterError::DeliveryTimedOut { .. } => {
                    warp::http::StatusCode::GATEWAY_TIMEOUT
                }
                compute_requester::RequesterError::ProviderFailed { .. } => {
                    warp::http::StatusCode::BAD_GATEWAY
                }
                compute_requester::RequesterError::MatchFailed { .. }
                | compute_requester::RequesterError::FillFailed { .. }
                | compute_requester::RequesterError::FillRejected { .. } => {
                    warp::http::StatusCode::BAD_GATEWAY
                }
                compute_requester::RequesterError::Internal(_) => {
                    warp::http::StatusCode::INTERNAL_SERVER_ERROR
                }
            };
            Ok(json_error(status, &e.to_string()))
        }
    }
}
