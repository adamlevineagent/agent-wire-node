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
            let rate_limiter = pool_cfg.rate_limit.as_ref().map(|rl| {
                SlidingWindowLimiter::new(rl.max_requests, rl.window_secs)
            });

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::dispatch_policy::{
        BuildCoordinationConfig, DispatchPolicy, EscalationConfig, MatchConfig,
        ProviderPoolConfig, RateLimitConfig, RouteEntry, RoutingRule, WorkType,
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
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("no pool configured")
        );
    }
}
