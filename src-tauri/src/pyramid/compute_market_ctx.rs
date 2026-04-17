//! Compute market requester context — runtime handles plumbed onto
//! `LlmConfig` so `call_model_unified` can attempt a Phase B market
//! dispatch without taking new parameters.
//!
//! Lives in a module separate from `compute_requester.rs` to avoid the
//! cyclic import that would otherwise bind `llm.rs` and the requester
//! together (llm.rs depends on this context; compute_requester depends
//! on `LlmProvider` traits transitively via `llm.rs`).
//!
//! See `docs/plans/call-model-unified-market-integration.md` §3.5.

use std::sync::Arc;
use tokio::sync::RwLock;

use crate::auth::AuthState;
use crate::tunnel::TunnelState;
use crate::WireNodeConfig;

use super::pending_jobs::PendingJobs;

/// Context bundle attached to `LlmConfig.compute_market_context` at
/// runtime to enable the Phase B market branch in `call_model_unified`.
///
/// `None` in tests and in the narrow pre-init boot window — the market
/// branch is gated on presence of this context (see `should_try_market`),
/// so absent means "bypass market, go straight to pool".
///
/// Cloning is cheap: every field is either an `Arc<RwLock<...>>` clone
/// (pointer bump) or a `PendingJobs` which is itself self-Arc'd
/// internally (`Arc<Mutex<HashMap<...>>>` under the hood).
#[derive(Clone)]
pub struct ComputeMarketRequesterContext {
    /// Shared auth state — read for `api_token` at dispatch time.
    pub auth: Arc<RwLock<AuthState>>,
    /// Shared node config — read for `api_url` at dispatch time.
    pub config: Arc<RwLock<WireNodeConfig>>,
    /// Shared pending-jobs map. PendingJobs is self-Arc'd internally
    /// (see `pyramid/pending_jobs.rs` — the inner `Arc<Mutex<HashMap>>`
    /// is what `Clone` bumps). Same handle is held on `AppState`
    /// (field `pending_market_jobs`) and on the inbound
    /// `/v1/compute/job-result` handler's `ServerState`, so a clone
    /// here rendezvouses with the same map.
    pub pending_jobs: PendingJobs,
    /// Shared tunnel state — read for readiness gating in
    /// `should_try_market`. Same `Arc<RwLock<TunnelState>>` clone as
    /// `AppState.tunnel_state`, so the gate observes every live
    /// transition atomically.
    pub tunnel_state: Arc<RwLock<TunnelState>>,
}
