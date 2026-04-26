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
use std::time::{Duration, Instant};

use anyhow::anyhow;
use tokio::sync::RwLock;

use agent_wire_contracts::{MarketSurfaceMarket, MarketSurfaceModel, MarketSurfaceResponse};
use serde::{Deserialize, Serialize};

use crate::auth::AuthState;
use crate::http_utils::send_api_request;
use crate::WireNodeConfig;

/// UI-facing flattened model row. Returned by `pyramid_market_models`
/// IPC (Wave 4 task 29) and consumed by the Settings panel's Discovery
/// section + provider/model autocomplete.
///
/// Shape deliberately narrower than `MarketSurfaceModel` — the frontend
/// only needs identity, liquidity, and median pricing. Timestamp is the
/// `market.last_updated_at` from the last successful refresh (not
/// per-model — Wire's `last_offer_update_at` is per-model but optional;
/// aggregate staleness is more honest for a UI badge).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PyramidMarketModel {
    pub model_id: String,
    pub active_offers: i64,
    /// Median of Wire-reported rate_per_m_input, in credits. `None` when
    /// Wire couldn't compute a median (e.g. every offer has a null rate).
    pub rate_in_per_m: Option<i64>,
    /// Median of Wire-reported rate_per_m_output, in credits.
    pub rate_out_per_m: Option<i64>,
    /// RFC-3339 timestamp from `market.last_updated_at` at the last
    /// successful refresh. Same value across all rows in one snapshot.
    pub last_updated_at: String,
}

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
    last_refresh_at: Arc<RwLock<Instant>>,
    /// Shared auth handle — `refresh_now` reads `api_token` fresh on
    /// every call so token rotation is picked up without restart.
    /// `None` in test-only instances constructed via `with_test_data`.
    auth: Option<Arc<RwLock<AuthState>>>,
    /// Shared node config handle — `refresh_now` reads `api_url` fresh
    /// on every call. `None` in test-only instances.
    config: Option<Arc<RwLock<WireNodeConfig>>>,
}

impl MarketSurfaceCache {
    /// Construct an empty cache bound to shared auth + config handles.
    /// No background task is spawned; call `spawn_poller` separately
    /// (boot wiring from `main.rs`).
    pub fn new(auth: Arc<RwLock<AuthState>>, config: Arc<RwLock<WireNodeConfig>>) -> Self {
        Self {
            data: Arc::new(RwLock::new(None)),
            last_refresh_at: Arc::new(RwLock::new(Instant::now())),
            auth: Some(auth),
            config: Some(config),
        }
    }

    /// Test-only constructor: inject a pre-populated `CacheData` so
    /// `get_model` fixtures can exercise the read path without hitting
    /// the network. `auth` / `config` are None — `refresh_now` will
    /// error if called on an instance built this way.
    #[cfg(test)]
    pub fn with_test_data(data: CacheData) -> Self {
        Self {
            data: Arc::new(RwLock::new(Some(data))),
            last_refresh_at: Arc::new(RwLock::new(Instant::now())),
            auth: None,
            config: None,
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
        guard.as_ref().and_then(|d| d.models.get(model_id).cloned())
    }

    /// Flattened read-only snapshot of the current cache state for
    /// UI consumption. Returns `[]` when cold (no poll has landed yet).
    /// Each entry carries the fields the Settings panel + autocomplete
    /// need without leaking the full `MarketSurfaceModel` shape to the
    /// frontend. Rates use the median of the Wire-reported triple; a
    /// missing median surfaces as `None`.
    ///
    /// Wave 4 task 29. See `docs/plans/walker-re-plan-wire-2.1.md` §8.
    pub async fn snapshot_ui_models(&self) -> Vec<PyramidMarketModel> {
        let guard = self.data.read().await;
        let Some(data) = guard.as_ref() else {
            return Vec::new();
        };
        let last_updated_at = data.market.last_updated_at.clone();
        let mut out: Vec<PyramidMarketModel> = data
            .models
            .values()
            .map(|m| PyramidMarketModel {
                model_id: m.model_id.clone(),
                active_offers: m.active_offers,
                rate_in_per_m: m.price.rate_per_m_input.median,
                rate_out_per_m: m.price.rate_per_m_output.median,
                last_updated_at: last_updated_at.clone(),
            })
            .collect();
        // Stable alphabetical order so the UI doesn't reflow on refresh.
        out.sort_by(|a, b| a.model_id.cmp(&b.model_id));
        out
    }

    /// Trigger an out-of-band refresh — used by the walker as a hint
    /// after a `/quote` miss that the cache is stale, and by the
    /// polling loop below on every tick. Reads `api_token` / `api_url`
    /// fresh from the shared auth + config handles so token rotation
    /// is picked up without restart.
    ///
    /// On any failure (network error, non-2xx, JSON parse failure,
    /// missing auth) the call returns `Err` and the existing cache is
    /// left untouched — stale-but-present beats empty during a transient
    /// hiccup. Walker sees stale data; `/quote` is the authoritative
    /// viability check so this is a graceful degradation.
    pub async fn refresh_now(&self) -> Result<(), anyhow::Error> {
        let auth = self
            .auth
            .as_ref()
            .ok_or_else(|| anyhow!("MarketSurfaceCache has no auth handle (test-only instance)"))?;
        let config = self.config.as_ref().ok_or_else(|| {
            anyhow!("MarketSurfaceCache has no config handle (test-only instance)")
        })?;

        let api_url = {
            let cfg = config.read().await;
            cfg.api_url.clone()
        };
        let token = {
            let guard = auth.read().await;
            guard
                .api_token
                .clone()
                .filter(|t| !t.is_empty())
                .ok_or_else(|| anyhow!("no api_token on AuthState — node not registered"))?
        };

        let (_, body) = send_api_request(
            &api_url,
            "GET",
            "/api/v1/compute/market-surface",
            &token,
            None,
            None,
        )
        .await
        .map_err(|e| anyhow!("market-surface GET failed: {e}"))?;

        let parsed: MarketSurfaceResponse = serde_json::from_value(body)
            .map_err(|e| anyhow!("market-surface response parse failed: {e}"))?;

        // Re-index the `Vec<MarketSurfaceModel>` into a HashMap for O(1)
        // walker lookups. Contract returns `models` as Vec (rev 2.1 §3.1).
        //
        // Walker v3 fix (2026-04-23): Wire's bulk
        // /api/v1/compute/market-surface (no filter) returns each model's
        // catalog row (model_id + counts) WITHOUT the per-offer detail
        // array. To get `model.offers` populated we must call
        // ?model_id=X per known model. Walker v3's MarketReadiness
        // requires `active_offers > 0` from the cached entry; without
        // this per-model fetch, every model lands with offers=[],
        // MarketReadiness drops Market with NoMarketOffersForSlot, and
        // walker silently cascades to OpenRouter even when real market
        // offers exist on Wire. Fixed by issuing per-model GETs when the
        // bulk response gave us no offers (and the model has at least
        // one active offer per the bulk-row counter).
        let mut models_map: HashMap<String, MarketSurfaceModel> =
            HashMap::with_capacity(parsed.models.len());
        for mut m in parsed.models {
            let needs_offer_fetch =
                m.offers.as_ref().map(|o| o.is_empty()).unwrap_or(true) && m.active_offers > 0;
            if needs_offer_fetch {
                let path = format!(
                    "/api/v1/compute/market-surface?model_id={}",
                    urlencoding::encode(&m.model_id)
                );
                match send_api_request(&api_url, "GET", &path, &token, None, None).await {
                    Ok((_, per_model_body)) => {
                        match serde_json::from_value::<MarketSurfaceResponse>(per_model_body) {
                            Ok(per_model_resp) => {
                                if let Some(detail) = per_model_resp
                                    .models
                                    .into_iter()
                                    .find(|d| d.model_id == m.model_id)
                                {
                                    m.offers = detail.offers;
                                }
                            }
                            Err(e) => tracing::warn!(
                                "market-surface per-model parse failed for {}: {e}",
                                m.model_id
                            ),
                        }
                    }
                    Err(e) => tracing::warn!(
                        "market-surface per-model GET failed for {}: {e}",
                        m.model_id
                    ),
                }
            }
            models_map.insert(m.model_id.clone(), m);
        }

        // Rev 2.1 `MarketSurfaceResponse` has no top-level `generated_at`;
        // use `market.last_updated_at` (RFC-3339 string) for the same role.
        // If parse fails, fall back to `now` so stale-timestamp never
        // blocks the swap.
        let generated_at = chrono::DateTime::parse_from_rfc3339(&parsed.market.last_updated_at)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::Utc::now());

        let new_data = CacheData {
            market: parsed.market,
            models: models_map,
            generated_at,
        };

        {
            let mut slot = self.data.write().await;
            *slot = Some(new_data);
        }
        {
            let mut ts = self.last_refresh_at.write().await;
            *ts = Instant::now();
        }
        Ok(())
    }

    /// Spawn the 60s polling task (plan §6.2). Performs one initial
    /// refresh before entering the interval loop so walkers that hit
    /// `get_model` in the first 60s have something to consult. On tick
    /// failure the existing cache is preserved; poller keeps going.
    ///
    /// No explicit shutdown hook — the task dies when the tokio runtime
    /// shuts down at process exit. That's the Wave 3 scope (§6.2).
    pub fn spawn_poller(cache: Arc<Self>) {
        tokio::spawn(async move {
            // Initial refresh so the cache warms before the first 60s tick.
            match cache.refresh_now().await {
                Ok(()) => {
                    let n = cache
                        .data
                        .read()
                        .await
                        .as_ref()
                        .map(|d| d.models.len())
                        .unwrap_or(0);
                    tracing::debug!("MarketSurfaceCache initial refresh ok ({n} models)");
                }
                Err(e) => {
                    tracing::warn!("MarketSurfaceCache initial refresh failed: {e}");
                }
            }

            let mut interval = tokio::time::interval(Duration::from_secs(60));
            // First tick fires immediately; skip it (we already refreshed).
            interval.tick().await;
            loop {
                interval.tick().await;
                match cache.refresh_now().await {
                    Ok(()) => {
                        let n = cache
                            .data
                            .read()
                            .await
                            .as_ref()
                            .map(|d| d.models.len())
                            .unwrap_or(0);
                        tracing::debug!("MarketSurfaceCache refresh ok ({n} models)");
                    }
                    Err(e) => {
                        tracing::warn!("MarketSurfaceCache refresh failed: {e}");
                    }
                }
            }
        });
        tracing::info!("MarketSurfaceCache poller spawned (60s cadence)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal `MarketSurfaceMarket` for tests via serde_json
    /// from a fixed shape — avoids hand-constructing every nested
    /// MarketSurface* child type.
    fn test_market() -> MarketSurfaceMarket {
        serde_json::from_value(serde_json::json!({
            "active_providers": 0,
            "active_offers_total": 0,
            "models_offered": 0,
            "total_queue_capacity": 0,
            "total_queue_depth": 0,
            "capacity_utilization": 0.0,
            "settled_24h": {
                "jobs": 0,
                "credits": 0,
                "failure_rate": 0.0,
                "median_latency_p95_ms": null,
                "median_tps": null,
            },
            "economic": {
                "float_pool": {
                    "balance": 0,
                    "max": 0,
                    "inflow_24h": 0,
                    "outflow_24h": 0,
                    "destroyed_24h": 0,
                    "minted_24h": 0,
                },
                "wire_take_24h": 0,
                "graph_fund_24h": 0,
                "reservation_fees_24h": 0,
            },
            "velocity_1h": {
                "new_offers": 0,
                "retired_offers": 0,
                "rate_changes": 0,
                "jobs_matched": 0,
            },
            "last_updated_at": "2026-01-01T00:00:00Z",
        }))
        .expect("fixture shape must match MarketSurfaceMarket")
    }

    fn test_model(model_id: &str, active_offers: i64) -> MarketSurfaceModel {
        serde_json::from_value(serde_json::json!({
            "model_id": model_id,
            "provider_count": 1,
            "active_offers": active_offers,
            "price": {
                "rate_per_m_input": { "min": null, "median": null, "max": null },
                "rate_per_m_output": { "min": null, "median": null, "max": null },
            },
            "queue": {
                "total_capacity": 0,
                "current_depth": 0,
                "unbounded_offers": 0,
            },
            "performance": {
                "p50_latency_ms": null,
                "p95_latency_ms": null,
                "median_tps": null,
                "success_rate_7d": null,
            },
            "top_of_book": { "cheapest_with_headroom": null },
            "demand_24h": {
                "jobs_matched": 0,
                "jobs_settled": 0,
                "queue_fill_events": 0,
            },
            "last_offer_update_at": null,
        }))
        .expect("fixture shape must match MarketSurfaceModel")
    }

    /// Cold cache (no data written): `get_model` returns `None`.
    #[tokio::test]
    async fn cold_cache_get_model_returns_none() {
        let cache = MarketSurfaceCache::with_test_data(CacheData {
            market: test_market(),
            models: HashMap::new(),
            generated_at: chrono::Utc::now(),
        });
        assert!(cache.get_model("nonexistent").await.is_none());
    }

    /// Warm cache with one model entry: present lookup returns `Some`,
    /// absent lookup returns `None`.
    #[tokio::test]
    async fn warm_cache_get_model_hits_and_misses() {
        let mut models = HashMap::new();
        models.insert(
            "anthropic/claude-opus-4.7".to_string(),
            test_model("anthropic/claude-opus-4.7", 3),
        );
        let cache = MarketSurfaceCache::with_test_data(CacheData {
            market: test_market(),
            models,
            generated_at: chrono::Utc::now(),
        });

        let hit = cache.get_model("anthropic/claude-opus-4.7").await;
        assert!(hit.is_some(), "present slug should hit");
        assert_eq!(hit.unwrap().active_offers, 3);

        assert!(
            cache.get_model("unknown/model").await.is_none(),
            "absent slug should miss"
        );
    }

    /// `snapshot_ui_models` on cold cache returns an empty Vec, not a
    /// panic or error. The IPC contract promises `[]` for pre-tunnel +
    /// pre-first-poll nodes.
    #[tokio::test]
    async fn snapshot_ui_models_cold_cache_is_empty() {
        // Construct with no data at all — the only way to reach the
        // `None` branch of `self.data` without a test-mode helper.
        // We use with_test_data but with an empty models map; the
        // `snapshot_ui_models` fn returns `[]` for both `None` and
        // `Some(empty)` which is the invariant we're asserting.
        let cache = MarketSurfaceCache::with_test_data(CacheData {
            market: test_market(),
            models: HashMap::new(),
            generated_at: chrono::Utc::now(),
        });
        let snap = cache.snapshot_ui_models().await;
        assert!(snap.is_empty(), "empty cache must produce empty snapshot");
    }

    /// Warm cache with two models: snapshot returns both rows in stable
    /// alphabetical order; median rates flow through from the price
    /// triple; last_updated_at mirrors `market.last_updated_at`.
    #[tokio::test]
    async fn snapshot_ui_models_warm_cache_shape() {
        // Build a market with a known last_updated_at distinct from the
        // default fixture so we can assert the passthrough.
        let mut market = test_market();
        market.last_updated_at = "2026-04-21T10:00:00Z".to_string();

        // Model A: both medians populated. Model B: medians absent.
        let model_a: MarketSurfaceModel = serde_json::from_value(serde_json::json!({
            "model_id": "provider/model-a",
            "provider_count": 2,
            "active_offers": 5,
            "price": {
                "rate_per_m_input": { "min": 100, "median": 150, "max": 200 },
                "rate_per_m_output": { "min": 300, "median": 400, "max": 500 },
            },
            "queue": { "total_capacity": 0, "current_depth": 0, "unbounded_offers": 0 },
            "performance": {
                "p50_latency_ms": null, "p95_latency_ms": null,
                "median_tps": null, "success_rate_7d": null,
            },
            "top_of_book": { "cheapest_with_headroom": null },
            "demand_24h": { "jobs_matched": 0, "jobs_settled": 0, "queue_fill_events": 0 },
            "last_offer_update_at": null,
        }))
        .expect("fixture shape");

        let model_b = test_model("provider/model-b", 2);

        let mut models = HashMap::new();
        // Insert in reverse sorted order so we can detect the sort.
        models.insert(model_b.model_id.clone(), model_b);
        models.insert(model_a.model_id.clone(), model_a);

        let cache = MarketSurfaceCache::with_test_data(CacheData {
            market,
            models,
            generated_at: chrono::Utc::now(),
        });

        let snap = cache.snapshot_ui_models().await;
        assert_eq!(snap.len(), 2);
        // Alphabetical: model-a < model-b.
        assert_eq!(snap[0].model_id, "provider/model-a");
        assert_eq!(snap[0].active_offers, 5);
        assert_eq!(snap[0].rate_in_per_m, Some(150));
        assert_eq!(snap[0].rate_out_per_m, Some(400));
        assert_eq!(snap[0].last_updated_at, "2026-04-21T10:00:00Z");

        assert_eq!(snap[1].model_id, "provider/model-b");
        assert_eq!(snap[1].active_offers, 2);
        assert_eq!(snap[1].rate_in_per_m, None);
        assert_eq!(snap[1].rate_out_per_m, None);
    }

    /// `refresh_now` on a test-only instance (no auth/config handles)
    /// returns `Err` rather than panicking — the test constructor path
    /// fails loudly per `feedback_loud_deferrals`.
    #[tokio::test]
    async fn refresh_now_without_handles_errors() {
        let cache = MarketSurfaceCache::with_test_data(CacheData {
            market: test_market(),
            models: HashMap::new(),
            generated_at: chrono::Utc::now(),
        });
        let err = cache.refresh_now().await.unwrap_err().to_string();
        assert!(
            err.contains("no auth handle") || err.contains("no config handle"),
            "expected missing-handle error, got: {err}"
        );
    }
}
