//! MarketSurfaceCache — rev 2.1 `/api/v1/compute/market-surface` cache.
//!
//! Walker consults this cache (advisory) on the `"market"` branch to decide
//! whether to even call `/quote`. Populated by a Tokio interval task spawned
//! at boot (60s cadence, aligned with Wire's `Cache-Control: max-age=60`).
//! `/quote` remains the authoritative viability check — this cache is only
//! a pre-filter against obviously-cold models.
//!
//! See `docs/plans/walker-re-plan-wire-2.1.md` §6 for the full spec,
//! §8 Wave 0 task 9 for the skeleton scope, and §2 "Walker adds" for the
//! lifecycle note (polling v1; SSE `/market-surface/stream` deferred to v2).
//!
//! # Wave 0 scope
//!
//! Types + public method signatures only. Bodies for `refresh_now` and
//! `spawn_poller` are `unimplemented!("Wave 4")` / stub-logs. `get_model`
//! reads the `Arc<RwLock<Option<CacheData>>>` — works today but always
//! returns `None` until Wave 4 wires the poller that populates the cell.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::RwLock;

use agent_wire_contracts::{MarketSurfaceMarket, MarketSurfaceModel};

use crate::auth::AuthState;
use crate::WireNodeConfig;

/// Snapshot of the last successful `/api/v1/compute/market-surface` response.
///
/// Fields mirror plan §6.1: whole-market block, per-model entries keyed by
/// `model_id`, and the server-reported `generated_at`. Swapped in-place by
/// the Wave 4 poller; read via `MarketSurfaceCache::get_model`.
///
/// `MarketSurfaceMarket` / `MarketSurfaceModel` are reused from the rev 2.1
/// `agent-wire-contracts` crate — no local type declarations needed.
#[derive(Debug, Clone)]
pub struct CacheData {
    /// Whole-market block (rev 2.1 §3.1 `market`).
    pub market: MarketSurfaceMarket,
    /// Per-model rollup entries keyed by `model_id`. Wire returns `models`
    /// as a `Vec`; we index it into a `HashMap` at refresh time so walker
    /// `get_model` lookups are O(1).
    pub models: HashMap<String, MarketSurfaceModel>,
    /// Server-reported generation timestamp (rev 2.1 §3.1). Distinct from
    /// `last_refresh_at` which is our local wall-clock at fetch time.
    pub generated_at: chrono::DateTime<chrono::Utc>,
}

/// Cache for Wire's `/api/v1/compute/market-surface` response.
///
/// Thread-safe; cheap to clone via `Arc`. A single instance lives on
/// `PyramidState` (Wave 4 wiring); walker + Settings panel both read it.
///
/// See plan §6 for the full design.
pub struct MarketSurfaceCache {
    /// The last successful snapshot. `None` until the first poll fills it
    /// (cold-cache state — walker treats as Unavailable and advances per
    /// §5.1 "cold-cache market entries advance silently").
    data: Arc<RwLock<Option<CacheData>>>,
    /// Local wall-clock time of the last successful refresh. Used for
    /// staleness telemetry + the Settings panel "last refreshed Xs ago"
    /// readout. Initialized to `Instant::now()` at construction so a
    /// cold cache still has a meaningful reference point.
    #[allow(dead_code)] // Wave 4 wires this; skeleton keeps the shape.
    last_refresh_at: Arc<RwLock<Instant>>,
}

impl MarketSurfaceCache {
    /// Construct an empty cache. No background task is spawned; call
    /// `spawn_poller` separately (Wave 4 wiring from `main.rs` after
    /// tunnel-connect).
    pub fn new() -> Self {
        Self {
            data: Arc::new(RwLock::new(None)),
            last_refresh_at: Arc::new(RwLock::new(Instant::now())),
        }
    }

    /// Look up a single model's rollup. Returns `None` on cold cache or
    /// unknown model. Walker uses this on the `"market"` branch before
    /// deciding whether to `/quote`.
    ///
    /// Wave 0 skeleton: the read path is live; `None` is the correct
    /// answer until the Wave 4 poller populates the cell.
    pub async fn get_model(&self, model_id: &str) -> Option<MarketSurfaceModel> {
        let guard = self.data.read().await;
        guard
            .as_ref()
            .and_then(|d| d.models.get(model_id).cloned())
    }

    /// Trigger an out-of-band refresh — used by the walker as a hint after
    /// a `/quote` miss that the cache is stale. Does NOT bypass Wire's
    /// `Cache-Control: max-age=60`; the underlying HTTP client honors the
    /// header either way. Stub until Wave 4.
    pub async fn refresh_now(&self) -> Result<(), anyhow::Error> {
        unimplemented!("Wave 4: refresh_now body lands alongside the polling loop")
    }

    /// Spawn the 60s polling task (plan §6.2). Wave 0 logs and returns
    /// immediately; Wave 4 replaces the body with the Tokio interval +
    /// GET `/api/v1/compute/market-surface` + swap-in-place.
    ///
    /// Takes the shared `auth` + `config` handles so the poller can read
    /// the latest `api_token` + `api_url` on every tick (same pattern as
    /// `ComputeMarketRequesterContext` at `compute_market_ctx.rs`).
    pub fn spawn_poller(
        auth: Arc<RwLock<AuthState>>,
        config: Arc<RwLock<WireNodeConfig>>,
        cache: Arc<Self>,
    ) {
        // Suppress unused-variable warnings without hiding the signature.
        let _ = (auth, config, cache);
        tracing::info!("MarketSurfaceCache poller spawn (Wave 4)");
    }
}

impl Default for MarketSurfaceCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Skeleton compile-only test: construct via `new()` and confirm a
    /// cold-cache lookup returns `None`. Exercises the read path through
    /// the `RwLock<Option<CacheData>>` so the type-machinery compiles.
    #[tokio::test]
    async fn cold_cache_get_model_returns_none() {
        let cache = MarketSurfaceCache::new();
        assert!(cache.get_model("nonexistent").await.is_none());
    }
}
