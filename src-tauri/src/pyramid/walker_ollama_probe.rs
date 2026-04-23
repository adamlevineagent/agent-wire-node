// Walker v3 — Ollama probe cache (Phase 2, plan rev 1.0.2 §2.6 + §3).
//
// Background:
//   The `ProviderReadiness::can_dispatch_now` trait is synchronous
//   (Decision builder calls it while holding no async context —
//   walker_decision::build runs from any context). Ollama probing is
//   async I/O (GET /api/tags). These two are bridged by a module-local
//   cache: a background task refreshes the cache at
//   `ollama_probe_interval_secs` cadence, and `LocalReadiness`
//   reads the cached snapshot without blocking.
//
//   The cache is keyed by base_url so multiple operators pointing at
//   different Ollama instances (multi-tenant test fixtures, operator
//   demo setups) don't stomp each other. In the common single-Ollama
//   case only one key is ever populated.
//
// Design notes:
//   * `OnceLock<Mutex<HashMap<base_url, CachedProbe>>>` — mirrors the
//     precedent in `yaml_renderer.rs::OLLAMA_TAGS_CACHE`. Sync std
//     Mutex because every reader/writer is in-memory and contention
//     is nanosecond-scale; no need to drag tokio into readiness code.
//   * Absent entry = "not yet probed". `LocalReadiness` treats this
//     as OllamaOffline — a conservative default that flips to Ready
//     on the first successful background probe.
//   * Stale entry = "last probe older than interval". Readiness still
//     returns the cached answer; the background task will refresh on
//     the next tick. A stale-but-reachable cache staying Ready is
//     intentional — Ollama is LAN-local, so transient flaps are common
//     and shouldn't fail the whole call order.
//   * The cache never stores "has the operator declared this model
//     for this slot" — that's a `ResolvedProviderParams.model_list`
//     question, resolved per-call by readiness. The cache only stores
//     "what does Ollama say is installed right now?"
//
// Integration:
//   boot.rs spawns `spawn_background_probe_task(base_url, interval)`
//   once the scope cache is populated. The task ticks at
//   `ollama_probe_interval_secs` and writes the result into the
//   cache. ConfigSynced listeners can call `invalidate_cache()` when
//   the operator edits `walker_provider_local.ollama_base_url` so the
//   new URL is probed on the next tick (no wait-for-interval lag).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Snapshot of a single Ollama probe. `reachable=false` + empty
/// `models` is the "ollama down" shape; `reachable=true` + empty
/// `models` is the "ollama up but no models pulled" shape — both are
/// treated as OllamaOffline by the readiness gate, but we preserve
/// the distinction for chronicle visibility.
#[derive(Debug, Clone)]
pub struct CachedProbe {
    pub reachable: bool,
    pub models: Vec<String>,
    pub at: Instant,
}

/// Thread-safe handle to the shared cache. Constructed lazily on
/// first access via `cache_handle()`; tests + boot can reach for the
/// same singleton without plumbing.
type CacheMap = Mutex<HashMap<String, CachedProbe>>;

static OLLAMA_PROBE_CACHE: OnceLock<CacheMap> = OnceLock::new();

fn cache_handle() -> &'static CacheMap {
    OLLAMA_PROBE_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Look up the cached probe for `base_url`. Returns `None` when the
/// url has never been probed. Caller decides what to do with stale
/// entries (readiness: use them; background task: refresh them).
pub fn read_cached_probe(base_url: &str) -> Option<CachedProbe> {
    let guard = cache_handle().lock().ok()?;
    guard.get(base_url).cloned()
}

/// Overwrite the cache entry for `base_url`. Called by the background
/// task after every probe and by `seed_for_tests` in unit tests.
pub fn write_cached_probe(base_url: &str, probe: CachedProbe) {
    if let Ok(mut guard) = cache_handle().lock() {
        guard.insert(base_url.to_string(), probe);
    }
}

/// Drop the cache entry for `base_url`. Used by ConfigSynced listeners
/// when the operator edits `walker_provider_local.ollama_base_url` so
/// the readiness gate sees "no entry → OllamaOffline" until the next
/// probe lands. Invalidation is per-url, not a full clear, so other
/// base_urls (multi-tenant fixtures) aren't disturbed.
#[allow(dead_code)]
pub fn invalidate_cached_probe(base_url: &str) {
    if let Ok(mut guard) = cache_handle().lock() {
        guard.remove(base_url);
    }
}

/// Test-only: wipe the entire cache so adjacent tests don't see each
/// other's writes. Production code MUST NOT call this.
#[cfg(test)]
pub fn clear_cache_for_tests() {
    if let Ok(mut guard) = cache_handle().lock() {
        guard.clear();
    }
}

/// Probe Ollama and update the cache. Shared body used by both the
/// background task (steady-state) and the boot seeding path (first-
/// boot warm-up so readiness doesn't start in OllamaOffline for the
/// entire first interval).
///
/// Never panics. On any error the cache entry is written with
/// `reachable=false` + empty models so readiness sees the offline
/// shape on next read.
pub async fn probe_and_store(base_url: &str) {
    let probe = crate::pyramid::local_mode::probe_ollama(base_url).await;
    let entry = CachedProbe {
        reachable: probe.reachable,
        models: probe.available_models,
        at: Instant::now(),
    };
    write_cached_probe(base_url, entry);
}

/// Whether a cached probe is fresh relative to `interval`. Missing
/// entries are treated as stale so the background task refreshes
/// them on first tick.
pub fn probe_is_fresh(probe: &CachedProbe, interval: Duration) -> bool {
    probe.at.elapsed() <= interval
}

/// Spawn the background probe task. Runs for the process lifetime,
/// ticking at `interval_secs`. Cheap enough (one HTTP call per tick
/// to localhost) that running unconditionally is fine — the operator
/// paying for that cost is paying for accurate readiness.
///
/// Returns the `JoinHandle` so boot can hold it. Dropping the handle
/// does NOT cancel the task (per tokio semantics for detached tasks);
/// we keep the handle for symmetry with the other boot-spawned tasks.
///
/// `interval_secs == 0` is treated as 60s so a pathological config
/// doesn't busy-loop the runtime. The resolver already clamps via
/// SYSTEM_DEFAULT, but this belt-and-suspenders guard catches the
/// case where a caller bypasses the resolver.
#[allow(dead_code)]
pub fn spawn_background_probe_task(
    base_url: String,
    interval_secs: u64,
) -> tokio::task::JoinHandle<()> {
    let interval = if interval_secs == 0 {
        Duration::from_secs(60)
    } else {
        Duration::from_secs(interval_secs)
    };
    tokio::spawn(async move {
        // Probe once immediately so readiness leaves OllamaOffline the
        // moment the task is up, not one full interval later.
        probe_and_store(&base_url).await;
        loop {
            tokio::time::sleep(interval).await;
            probe_and_store(&base_url).await;
        }
    })
}

// ── Readiness-facing helpers ────────────────────────────────────────────────

/// Snapshot handle passed into `LocalReadiness`. Wraps the
/// base_url-keyed lookup so the readiness impl stays a thin shim over
/// the cache. Cloning is cheap: it's an `Arc`-free newtype around an
/// owned String.
#[derive(Debug, Clone)]
pub struct LocalProbeHandle {
    /// Static global cache is the source of truth; this type carries
    /// no state of its own. The field is present so `LocalReadiness`
    /// can be constructed in tests with a specific base_url pin if a
    /// future test needs to route lookups.
    _marker: (),
}

impl LocalProbeHandle {
    /// Production constructor — reads the global cache.
    #[allow(dead_code)]
    pub fn global() -> Self {
        Self { _marker: () }
    }

    /// Look up the cached probe for `base_url`. `None` = never probed.
    pub fn probe_for(&self, base_url: &str) -> Option<CachedProbe> {
        read_cached_probe(base_url)
    }
}

impl Default for LocalProbeHandle {
    fn default() -> Self {
        Self::global()
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique base_url per test so the shared global cache doesn't
    /// cross-contaminate. `#[cfg(test)] clear_cache_for_tests()` is an
    /// option, but per-test base_urls are a stronger isolation guarantee
    /// (parallel test execution can't race on cache writes).
    fn unique_base_url(tag: &str) -> String {
        format!("http://test-{}.invalid:11434/v1", tag)
    }

    #[test]
    fn read_cached_probe_absent_returns_none() {
        let url = unique_base_url("absent");
        invalidate_cached_probe(&url);
        assert!(read_cached_probe(&url).is_none());
    }

    #[test]
    fn write_then_read_roundtrips() {
        let url = unique_base_url("roundtrip");
        write_cached_probe(
            &url,
            CachedProbe {
                reachable: true,
                models: vec!["gemma3:27b".into()],
                at: Instant::now(),
            },
        );
        let got = read_cached_probe(&url).expect("must be present after write");
        assert!(got.reachable);
        assert_eq!(got.models, vec!["gemma3:27b".to_string()]);
        invalidate_cached_probe(&url);
    }

    #[test]
    fn invalidate_drops_the_entry() {
        let url = unique_base_url("invalidate");
        write_cached_probe(
            &url,
            CachedProbe {
                reachable: true,
                models: vec![],
                at: Instant::now(),
            },
        );
        assert!(read_cached_probe(&url).is_some());
        invalidate_cached_probe(&url);
        assert!(read_cached_probe(&url).is_none());
    }

    #[test]
    fn freshness_honors_interval() {
        let fresh = CachedProbe {
            reachable: true,
            models: vec![],
            at: Instant::now(),
        };
        assert!(probe_is_fresh(&fresh, Duration::from_secs(60)));
        // Synthesize staleness by pretending the probe ran long ago.
        // Instant::now() - big duration; use checked_sub to avoid
        // platform panics on low monotonic clocks, falling back to
        // now() which will fail the assertion (detected by test).
        let stale_at = Instant::now()
            .checked_sub(Duration::from_secs(3600))
            .expect("monotonic clock must support subtraction");
        let stale = CachedProbe {
            reachable: true,
            models: vec![],
            at: stale_at,
        };
        assert!(!probe_is_fresh(&stale, Duration::from_secs(60)));
    }

    #[test]
    fn local_probe_handle_routes_through_cache() {
        let url = unique_base_url("handle");
        invalidate_cached_probe(&url);
        let h = LocalProbeHandle::global();
        assert!(h.probe_for(&url).is_none());
        write_cached_probe(
            &url,
            CachedProbe {
                reachable: true,
                models: vec!["llama3.2:latest".into()],
                at: Instant::now(),
            },
        );
        let got = h.probe_for(&url).expect("present after write");
        assert_eq!(got.models, vec!["llama3.2:latest".to_string()]);
        invalidate_cached_probe(&url);
    }
}
