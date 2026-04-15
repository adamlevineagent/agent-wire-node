# Client-Side Rate Limiting for LLM Dispatch

## The Problem
OpenRouter uses Cloudflare DDoS protection that blocks bursts of concurrent requests. A single Wire Node building a 127-doc pyramid fires 6+ concurrent API calls sustained over minutes. Multiple Wire Nodes building simultaneously multiplies this. Cloudflare returns 403 and the exponential backoff makes it worse — all slots back off in lockstep, then hit the API simultaneously again.

This is a shipping blocker, not a dev inconvenience. Every Wire Node user will hit this.

## Current Behavior
- `concurrency: 6` on `for_each` steps fires 6 simultaneous requests
- On 403, the Rust retries with exponential backoff: 2s, 4s, 8s, 16s, 32s
- All 6 slots hit 403 at the same time, back off the same duration, then fire simultaneously again — amplifying the burst pattern
- No pre-emptive rate limiting — we only react after the 403

## The Fix: Token Bucket Rate Limiter

### YAML surface
```yaml
defaults:
  rate_limit:
    requests_per_minute: 30    # sustained throughput
    burst: 6                   # max simultaneous in-flight
    jitter_ms: 500             # random delay per request to de-synchronize
```

Per-step override:
```yaml
- name: l0_doc_extract
  concurrency: 6
  rate_limit:
    requests_per_minute: 20    # this step is heavier, throttle more
```

### How it works

The executor maintains a shared token bucket for all LLM dispatch:

1. **Bucket capacity** = `burst` (e.g., 6 tokens)
2. **Refill rate** = `requests_per_minute / 60` tokens per second (e.g., 0.5 tokens/sec)
3. **Before each API call**: acquire a token from the bucket. If no token available, wait until one refills.
4. **Jitter**: add random 0-`jitter_ms` delay before each call to prevent synchronized bursts after backoff
5. **On 403**: do NOT consume another token for the retry — the original token is still held. Back off with jitter, then retry using the same token slot.

### Token bucket vs concurrency

`concurrency: 6` controls how many items are being processed in parallel (including response parsing, node saving, etc.). `rate_limit.burst: 6` controls how many are hitting the API simultaneously. They're related but distinct:

- `concurrency: 6` + `burst: 6` + `rpm: 30`: 6 items in flight, all 6 can call API at once, but sustained at 30/min
- `concurrency: 6` + `burst: 3` + `rpm: 20`: 6 items in flight but only 3 can be waiting on API at once, others wait for a token

### Shared across steps

The token bucket is shared across ALL steps in a build, not per-step. A build running `l0_doc_extract` at concurrency 6 and `thread_narrative` at concurrency 5 simultaneously would have 11 slots competing for the same bucket. This prevents aggregate burst even when multiple steps overlap.

### Per-node global limit

When multiple pyramids are building or stale checks are running, they all share one OpenRouter API key. The rate limiter should be **per API key**, not per build. All builds on this node share one token bucket.

### Config location

In `pyramid_config.json` (operational config):
```json
{
  "rate_limit_requests_per_minute": 30,
  "rate_limit_burst": 6,
  "rate_limit_jitter_ms": 500
}
```

Chain YAML `defaults.rate_limit` overrides for specific pipelines. `pyramid_config.json` is the global default.

### Adaptive rate limiting (future contribution)

An agent that monitors 403 rates could submit a superseding `rate_limit` configuration:
- Track 403 count over rolling window
- If 403s spike, reduce `requests_per_minute` and submit as economic_parameter contribution
- If 403s drop to zero, gradually increase
- Different OpenRouter plans have different limits — the agent learns the actual limit for this node's API key

### Backoff improvement

Current exponential backoff: all slots back off identically and re-fire in sync.

Fix: add per-slot jitter to the backoff. Each slot adds `random(0..backoff_duration * 0.5)` to its wait time. Slots desynchronize naturally after the first 403.

```rust
let base_wait = config.retry_base_sleep_secs * 2u64.pow(attempt + 1);
let jitter = rand::thread_rng().gen_range(0..=(base_wait / 2));
tokio::time::sleep(Duration::from_secs(base_wait + jitter)).await;
```

## Implementation

### New Rust: TokenBucket struct
```rust
struct TokenBucket {
    capacity: usize,
    available: AtomicUsize,
    refill_rate: f64,  // tokens per second
    last_refill: Mutex<Instant>,
    jitter_ms: u64,
}

impl TokenBucket {
    async fn acquire(&self) { /* wait for available token, add jitter */ }
    fn release(&self) { /* return token after call completes */ }
}
```

Shared via `Arc<TokenBucket>` across all executor tasks.

### Integration point
In `llm.rs` `call_model_unified_with_options()`, before the HTTP request:
```rust
rate_limiter.acquire().await;
let resp = client.post(url).json(&body).send().await;
rate_limiter.release();
```

### Files
- `src-tauri/src/pyramid/llm.rs` — acquire/release around API calls, jitter on backoff
- `src-tauri/src/pyramid/mod.rs` — TokenBucket struct, rate limit config fields
- `src-tauri/src/pyramid/chain_engine.rs` — rate_limit field on ChainDefaults
- `src-tauri/src/pyramid/chain_executor.rs` — pass rate limiter to dispatch context
- `pyramid_config.json` — default rate limit values
- Chain YAMLs — `defaults.rate_limit` section
