// provider_pools.rs — Per-provider concurrency pools.
//
// Replaces the global LOCAL_PROVIDER_SEMAPHORE (Semaphore(1)) and the global
// RATE_LIMITER with per-provider pools. Each provider gets its own semaphore
// with configurable concurrency (e.g., Ollama=1, remote-5090=2, OpenRouter=20)
// and optional per-pool sliding-window rate limiting.
//
// Loaded from the dispatch_policy contribution. Hot-reloaded via ConfigSynced
// bus events (the server.rs listener rebuilds pools when the policy changes).

use anyhow::{anyhow, Result};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::pyramid::dispatch_policy::DispatchPolicy;

// ── Acquire error ───────────────────────────────────────────────────────────

/// Non-blocking acquire outcome for [`ProviderPools::try_acquire_owned`].
///
/// The walker distinguishes these two so that `Unavailable` triggers
/// `network_route_unavailable` (config mistake — provider not in pools)
/// while `Saturated` triggers `network_route_saturated` (transient
/// capacity pressure). See plan §4.3 error-classification table.
#[derive(Debug, Clone)]
pub enum AcquireError {
    /// Provider id not present in the pools map — fresh-install or
    /// config-authoring mistake, not a transient condition.
    Unavailable(String),
    /// Pool exists but is at capacity: either the semaphore is full or
    /// the sliding-window rate limiter's quota is exhausted for the
    /// current window.
    Saturated,
}

impl std::fmt::Display for AcquireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AcquireError::Unavailable(reason) => {
                write!(f, "provider unavailable: {}", reason)
            }
            AcquireError::Saturated => write!(f, "pool saturated"),
        }
    }
}

impl std::error::Error for AcquireError {}

// ── Sliding-window rate limiter ─────────────────────────────────────────────

/// Per-pool sliding-window rate limiter. Same algorithm as the legacy global
/// `rate_limit_wait` in llm.rs: tracks timestamps in a VecDeque, evicts entries
/// older than the window, sleeps when full.
pub struct SlidingWindowLimiter {
    window: TokioMutex<VecDeque<Instant>>,
    max_requests: usize,
    window_secs: f64,
}

impl SlidingWindowLimiter {
    pub fn new(max_requests: usize, window_secs: f64) -> Self {
        Self {
            window: TokioMutex::new(VecDeque::new()),
            max_requests,
            window_secs,
        }
    }

    /// Wait until there is capacity in the sliding window, then record the call.
    pub async fn wait(&self) {
        if self.max_requests == 0 {
            return; // rate limiting disabled
        }
        loop {
            let now = Instant::now();
            let mut window = self.window.lock().await;

            // Evict entries older than the window
            while let Some(&oldest) = window.front() {
                if now.duration_since(oldest).as_secs_f64() >= self.window_secs {
                    window.pop_front();
                } else {
                    break;
                }
            }

            if window.len() < self.max_requests {
                window.push_back(now);
                return;
            }

            // Window full — compute how long until the oldest entry expires
            let oldest = window[0];
            let wait = self.window_secs - now.duration_since(oldest).as_secs_f64();
            drop(window); // release lock while sleeping
            if wait > 0.0 {
                tokio::time::sleep(std::time::Duration::from_secs_f64(wait + 0.05)).await;
            }
        }
    }

    /// Non-blocking variant of [`wait`](Self::wait): if there is capacity
    /// in the sliding window, records the call and returns `true`;
    /// otherwise returns `false` immediately without sleeping. Used by
    /// the walker's `try_acquire_owned` path — saturation reports and
    /// the walker advances to the next route entry rather than blocking.
    ///
    /// Lock contention is treated as conservative saturation (returns
    /// `false`) — the critical section is nanoseconds, and a walker
    /// that mis-reports saturated under instantaneous contention simply
    /// tries the next entry, which is the desired non-blocking semantic.
    pub fn try_acquire(&self) -> bool {
        if self.max_requests == 0 {
            return true; // rate limiting disabled
        }
        let now = Instant::now();
        let mut window = match self.window.try_lock() {
            Ok(w) => w,
            Err(_) => return false,
        };

        while let Some(&oldest) = window.front() {
            if now.duration_since(oldest).as_secs_f64() >= self.window_secs {
                window.pop_front();
            } else {
                break;
            }
        }

        if window.len() < self.max_requests {
            window.push_back(now);
            true
        } else {
            false
        }
    }
}

// ── Provider pool ───────────────────────────────────────────────────────────

/// Concurrency + rate-limit pool for a single provider.
pub struct ProviderPool {
    pub provider_id: String,
    pub semaphore: Arc<Semaphore>,
    pub rate_limiter: Option<SlidingWindowLimiter>,
}

// ── Provider pools collection ───────────────────────────────────────────────

/// Manages per-provider concurrency pools and per-rule sequencers.
/// Built from a `DispatchPolicy` and held in server state.
pub struct ProviderPools {
    pools: HashMap<String, ProviderPool>,
    /// Semaphore(1) per sequential routing rule, ensuring calls matching
    /// that rule execute one at a time.
    rule_sequencers: HashMap<String, Arc<Semaphore>>,
}

impl ProviderPools {
    /// Build pools from the dispatch policy's provider_pools config and
    /// create sequencer semaphores for any routing rule with `sequential: true`.
    pub fn new(policy: &DispatchPolicy) -> Self {
        let mut pools = HashMap::new();

        for (provider_id, pool_cfg) in &policy.pool_configs {
            let rate_limiter = pool_cfg
                .rate_limit
                .as_ref()
                .map(|rl| SlidingWindowLimiter::new(rl.max_requests, rl.window_secs));

            pools.insert(
                provider_id.clone(),
                ProviderPool {
                    provider_id: provider_id.clone(),
                    semaphore: Arc::new(Semaphore::new(pool_cfg.concurrency)),
                    rate_limiter,
                },
            );
        }

        let mut rule_sequencers = HashMap::new();
        for rule in &policy.rules {
            if rule.sequential {
                rule_sequencers
                    .entry(rule.name.clone())
                    .or_insert_with(|| Arc::new(Semaphore::new(1)));
            }
        }

        Self {
            pools,
            rule_sequencers,
        }
    }

    /// Acquire a concurrency permit from the named provider's pool.
    ///
    /// If the pool has a rate limiter, waits for rate-limit capacity first,
    /// then acquires the semaphore permit. The returned `OwnedSemaphorePermit`
    /// is held across await points and released on drop.
    ///
    /// Returns an error if `provider_id` is not in the pools map.
    pub async fn acquire(&self, provider_id: &str) -> Result<OwnedSemaphorePermit> {
        let pool = self
            .pools
            .get(provider_id)
            .ok_or_else(|| anyhow!("no pool configured for provider '{}'", provider_id))?;

        // Rate limit first (if configured)
        if let Some(ref limiter) = pool.rate_limiter {
            limiter.wait().await;
        }

        // Then acquire concurrency semaphore
        let permit = pool
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| anyhow!("semaphore closed for provider '{}'", provider_id))?;

        Ok(permit)
    }

    /// Returns the sequencer semaphore for a sequential routing rule, if one exists.
    pub fn get_sequencer(&self, rule_name: &str) -> Option<Arc<Semaphore>> {
        self.rule_sequencers.get(rule_name).cloned()
    }

    /// Non-blocking variant of [`acquire`](Self::acquire): returns
    /// immediately with either an owned permit or an [`AcquireError`]
    /// describing why capacity was refused.
    ///
    /// Walker uses this on every pool-branch entry — saturation advances
    /// to the next route entry rather than blocking. Rate-limiter check
    /// runs first (cheaper); if it passes, the semaphore
    /// `try_acquire_owned` is attempted.
    ///
    /// This is a pure `&self` method (no await) so the walker can call
    /// it from any context.
    pub fn try_acquire_owned(
        &self,
        provider_id: &str,
    ) -> std::result::Result<OwnedSemaphorePermit, AcquireError> {
        let pool = self
            .pools
            .get(provider_id)
            .ok_or_else(|| AcquireError::Unavailable("provider_not_in_pool".into()))?;

        if let Some(ref limiter) = pool.rate_limiter {
            if !limiter.try_acquire() {
                return Err(AcquireError::Saturated);
            }
        }

        pool.semaphore
            .clone()
            .try_acquire_owned()
            .map_err(|_| AcquireError::Saturated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::dispatch_policy::{
        BuildCoordinationConfig, DispatchPolicy, EscalationConfig, MatchConfig, ProviderPoolConfig,
        RateLimitConfig, RouteEntry, RoutingRule, WorkType,
    };

    fn test_policy() -> DispatchPolicy {
        let mut pool_configs = std::collections::BTreeMap::new();
        pool_configs.insert(
            "ollama".into(),
            ProviderPoolConfig {
                concurrency: 1,
                rate_limit: None,
            },
        );
        pool_configs.insert(
            "openrouter".into(),
            ProviderPoolConfig {
                concurrency: 20,
                rate_limit: Some(RateLimitConfig {
                    max_requests: 60,
                    window_secs: 60.0,
                }),
            },
        );

        DispatchPolicy {
            rules: vec![
                RoutingRule {
                    name: "build_local".into(),
                    match_config: MatchConfig {
                        work_type: Some(WorkType::Build),
                        min_depth: None,
                        step_pattern: None,
                    },
                    route_to: vec![RouteEntry {
                        provider_id: "ollama".into(),
                        model_id: None,
                        tier_name: None,
                        is_local: false,
                        max_budget_credits: None,
                    }],
                    bypass_pool: false,
                    sequential: true,
                },
                RoutingRule {
                    name: "catch_all".into(),
                    match_config: MatchConfig {
                        work_type: None,
                        min_depth: None,
                        step_pattern: None,
                    },
                    route_to: vec![RouteEntry {
                        provider_id: "openrouter".into(),
                        model_id: None,
                        tier_name: None,
                        is_local: false,
                        max_budget_credits: None,
                    }],
                    bypass_pool: false,
                    sequential: false,
                },
            ],
            escalation: EscalationConfig::default(),
            build_coordination: BuildCoordinationConfig::default(),
            pool_configs,
            max_batch_cost_usd: None,
            max_daily_cost_usd: None,
        }
    }

    #[test]
    fn test_pools_created_from_policy() {
        let policy = test_policy();
        let pools = ProviderPools::new(&policy);

        assert!(pools.pools.contains_key("ollama"));
        assert!(pools.pools.contains_key("openrouter"));
        assert_eq!(pools.pools.len(), 2);

        // Ollama has no rate limiter
        assert!(pools.pools["ollama"].rate_limiter.is_none());
        // OpenRouter has a rate limiter
        assert!(pools.pools["openrouter"].rate_limiter.is_some());
    }

    #[test]
    fn test_sequencer_created_for_sequential_rules() {
        let policy = test_policy();
        let pools = ProviderPools::new(&policy);

        assert!(pools.get_sequencer("build_local").is_some());
        assert!(pools.get_sequencer("catch_all").is_none());
        assert!(pools.get_sequencer("nonexistent").is_none());
    }

    #[tokio::test]
    async fn test_acquire_known_provider() {
        let policy = test_policy();
        let pools = ProviderPools::new(&policy);

        let permit = pools.acquire("ollama").await;
        assert!(permit.is_ok());
        // Permit is held — drop it
        drop(permit);
    }

    #[tokio::test]
    async fn test_acquire_unknown_provider_errors() {
        let policy = test_policy();
        let pools = ProviderPools::new(&policy);

        let result = pools.acquire("nonexistent").await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("no pool configured"));
    }

    // ── try_acquire_owned (non-blocking walker path) ─────────────────────────

    #[test]
    fn test_try_acquire_owned_known_provider_ok() {
        let policy = test_policy();
        let pools = ProviderPools::new(&policy);

        let permit = pools.try_acquire_owned("ollama");
        assert!(permit.is_ok(), "fresh pool should have capacity");
        drop(permit);
    }

    #[test]
    fn test_try_acquire_owned_unknown_provider_unavailable() {
        let policy = test_policy();
        let pools = ProviderPools::new(&policy);

        let result = pools.try_acquire_owned("nonexistent");
        match result {
            Err(AcquireError::Unavailable(reason)) => {
                assert_eq!(reason, "provider_not_in_pool");
            }
            other => panic!("expected Unavailable, got {:?}", other),
        }
    }

    #[test]
    fn test_try_acquire_owned_saturated_when_semaphore_exhausted() {
        let policy = test_policy();
        let pools = ProviderPools::new(&policy);

        // Ollama pool has concurrency=1; take the only permit.
        let _held = pools.try_acquire_owned("ollama").expect("first acquire ok");
        let second = pools.try_acquire_owned("ollama");
        assert!(matches!(second, Err(AcquireError::Saturated)));
    }

    #[test]
    fn test_try_acquire_owned_releases_permit_on_drop() {
        // Walker-re-plan-wire-2.1 Wave 5 task 38. Confirms the walker's
        // pool-branch Drop-semantics work: when a pool entry hits
        // Retryable or RouteSkipped, the OwnedSemaphorePermit is dropped
        // and the NEXT walker iteration's try_acquire_owned succeeds on
        // the same pool. Without this, a single retryable failure would
        // starve subsequent route entries pointing at the same pool.
        let policy = test_policy();
        let pools = ProviderPools::new(&policy);

        // Ollama pool has concurrency=1 — easiest to observe the release.
        let first = pools
            .try_acquire_owned("ollama")
            .expect("initial acquire on a fresh concurrency=1 pool");

        // While the permit is held, a second try must fail.
        let contested = pools.try_acquire_owned("ollama");
        assert!(
            matches!(contested, Err(AcquireError::Saturated)),
            "pool must report saturation while the sole permit is held"
        );

        // Drop the first permit — walker branch does this implicitly on
        // error/exit via scope. Capacity must return to the semaphore.
        drop(first);

        // Now the next try must succeed, confirming the permit really
        // released on drop (not e.g. leaked into the pool's pending set).
        let second = pools
            .try_acquire_owned("ollama")
            .expect("permit must be available after first was dropped");
        drop(second);
    }

    #[test]
    fn test_try_acquire_owned_saturated_when_rate_limiter_full() {
        // Pool with rate_limit max_requests=1 over a long window; first call
        // consumes the quota, second returns Saturated.
        let mut pool_configs = std::collections::BTreeMap::new();
        pool_configs.insert(
            "tight_rate".into(),
            ProviderPoolConfig {
                concurrency: 20,
                rate_limit: Some(RateLimitConfig {
                    max_requests: 1,
                    window_secs: 3600.0,
                }),
            },
        );
        let policy = DispatchPolicy {
            rules: vec![],
            escalation: EscalationConfig::default(),
            build_coordination: BuildCoordinationConfig::default(),
            pool_configs,
            max_batch_cost_usd: None,
            max_daily_cost_usd: None,
        };
        let pools = ProviderPools::new(&policy);

        let _first = pools.try_acquire_owned("tight_rate").expect("first ok");
        let second = pools.try_acquire_owned("tight_rate");
        assert!(matches!(second, Err(AcquireError::Saturated)));
    }

    // ── SlidingWindowLimiter::try_acquire ────────────────────────────────────

    #[test]
    fn test_sliding_window_try_acquire_under_limit() {
        let limiter = SlidingWindowLimiter::new(3, 60.0);
        assert!(limiter.try_acquire());
        assert!(limiter.try_acquire());
        assert!(limiter.try_acquire());
    }

    #[test]
    fn test_sliding_window_try_acquire_at_limit_returns_false() {
        let limiter = SlidingWindowLimiter::new(2, 60.0);
        assert!(limiter.try_acquire());
        assert!(limiter.try_acquire());
        assert!(
            !limiter.try_acquire(),
            "third call within window should refuse"
        );
    }

    #[test]
    fn test_sliding_window_try_acquire_disabled_always_true() {
        // max_requests=0 means rate limiting disabled.
        let limiter = SlidingWindowLimiter::new(0, 60.0);
        for _ in 0..100 {
            assert!(limiter.try_acquire());
        }
    }
}
