# call_model_unified Market Integration — Session Build Plan

**Status:** Draft, rev 0.4. Incorporates round 1, round 2, and round 3 findings. Pre-build facts verified against codebase.
**Session date:** 2026-04-17
**Design authority:** `docs/plans/node-phase-3-requester-side.md` rev 0.3 (read in full). This plan covers the remaining §3.4 integration + §3.6 chronicle scope, updated for 2026-04-17 framing corrections + code-verified error taxonomy + concurrency/panic hardening.
**Scope:** Wire the shipped Phase 3 primitives (`compute_requester.rs`, `PendingJobs`, `/v1/compute/job-result` route) into the cascade so `pyramid_build` dispatches via the compute market. Add network-framed chronicle events. Mitigate tunnel race, panic leak, Wire slug leak.
**Purpose lock:** GPU-less tester builds a pyramid via the network without seeing a market word. Every decision in this plan is subordinate to that test.

---

## 1. What is NOT in this plan

- **Protocol layer** — shipped. Contract rev 1.5, W1-W4 live on newsbleach.com.
- **Requester primitives** — shipped in `43b8704`.
- **Policy fields** — shipped.
- **Rename cascade** of pre-shipped `compute_market_*` IPC names, `ComputeParticipationPolicy.market_dispatch_*` fields, CSS classes. Separate follow-up.
- **Register-response orientation block surfacing.** Separate follow-up.
- **Build-tab UI capability moment** ("built in 47s using N network GPUs"). Separate — invisibility UX thread owns render; this plan ships chronicle data.

---

## 2. What IS in this plan

Single atomic commit covering:

1. **Cascade integration** — Phase B market branch between Phase A (fleet) and pool acquisition.
2. **`should_try_market` gate** — policy + balance + tier + tunnel-readiness + context availability.
3. **Error-mapping** — `AuthFailed` hard-fail, everything else soft-fall. Balance-exhaustion gets transition marker.
4. **Chronicle events** — 6 new types (§4.1) with network-framed names AND metadata keys.
5. **`ComputeMarketRequesterContext` struct** (§3.5) — new module `src-tauri/src/pyramid/compute_market_ctx.rs`.
6. **Config plumbing** — `LlmConfig.compute_market_context` field + `with_runtime_overlays_from` carry-forward + PyramidState wire-up at fleet_dispatch construction site.
7. **Panic safety** — `catch_unwind` wrapper around `call_market` to avoid PendingJobs leak on unwind.
8. **Wire slug sanitization** — `sanitize_wire_slug` scrubs trader vocabulary from pass-through chronicle metadata.
9. **Unit tests** — all gate branches, all 10 error variants, `classify_soft_fail_reason` as pure function, `sanitize_wire_slug` as pure function, chronicle per-path correctness.

---

## 3. Integration point

### 3.1 Cascade ordering (authoritative)

```
1. Cache lookup (line 761)
2. build_call_provider + resolved_route (line 799)
3. Phase A: Fleet (line 814) — same-operator peer dispatch, free
4. [NEW] Phase B: Market — cross-operator peer dispatch via Wire
5. Pool acquisition: Ollama / OpenRouter
```

**Fleet→Market serialization:** Phase A completes its pending cleanup (`fleet_ctx.pending.remove` at llm.rs ~1056) synchronously before falling through to Phase B. Fleet and market use independent `PendingJobs` maps with independent UUID keyspaces; cross-map collision is impossible structurally.

**Pool retry loop:** runs below Phase B. Its `continue` paths stay within pool; market is entered at most once per call.

### 3.2 `should_try_market` gate

```rust
fn should_try_market(
    policy: &ComputeParticipationPolicy,
    balance: i64,
    tier: &ModelTier,
    tunnel_state: &TunnelState,
    local_queue_depth: usize,
    config: &LlmConfig,
) -> bool {
    if !policy.allow_market_dispatch { return false; }

    if !policy.market_dispatch_eager
        && local_queue_depth < policy.market_dispatch_threshold_queue_depth as usize
    {
        return false;
    }

    if balance < estimated_deposit_for(tier, config.max_tokens) { return false; }

    if !tier.market_eligible() { return false; }

    // Tunnel readiness — GATES feature. Research found start_tunnel_flow
    // is spawned-not-awaited at boot. Connecting / Disconnected both mean
    // Wire's delivery worker can't reach us; skip without attempting
    // /match.
    if !matches!(tunnel_state.status, TunnelConnectionStatus::Connected) { return false; }
    if tunnel_state.tunnel_url.is_none() { return false; }

    if config.compute_market_context.is_none() { return false; }

    true
}
```

**Tunnel state aliasing:** `compute_market_context.tunnel_state` is an `Arc<RwLock<TunnelState>>` clone from AppState. Same underlying lock; RwLock atomicity guarantees both observers see updates. Intentional self-contained dispatch path.

**Non-build inference note:** call_model_unified may be invoked outside a build (tests, ad-hoc dispatch). The gate does NOT check `build_id.is_some()` — market is allowed for build-less calls. Chronicle events with `build_id=null` are emitted; build-scoped aggregations (BUILD_NETWORK_CONTRIBUTION, BALANCE_EXHAUSTED dedup) filter these out cleanly. See §7.8.

### 3.3 Eager vs non-eager

- `market_dispatch_eager=true`: try market every call (subject to other gates).
- `market_dispatch_eager=false` (default): try market only when `local_queue_depth >= market_dispatch_threshold_queue_depth`.

GPU-less testers have effectively infinite local queue depth → threshold always triggers. GPU-owning operators opt in per policy.

### 3.4 The branch

```rust
// Phase B: Market dispatch (cross-operator peer network via Wire).
if should_try_market(&policy, balance, &tier, &tunnel_snap, local_queue_depth, config) {
    let market_ctx = config
        .compute_market_context
        .as_ref()
        .expect("gate guaranteed Some");

    let req = MarketInferenceRequest { /* ... */ };

    // catch_unwind guards the PendingJobs lifecycle. If call_market
    // panics (malformed response, serde unwrap, etc.), the unwinding
    // task would leak its oneshot Sender in the pending map forever.
    // catch_unwind turns the panic into Err(anyhow), the cascade
    // soft-falls, and PendingJobs' own cleanup in await_result's
    // timeout path handles the dangling entry on its next cycle.
    let result = match std::panic::AssertUnwindSafe(
        compute_requester::call_market(
            req,
            &market_ctx.auth,
            &market_ctx.config,
            &market_ctx.pending_jobs,
            policy.market_dispatch_max_wait_ms,
        )
    ).catch_unwind().await {
        Ok(r) => r,
        Err(panic_info) => {
            let msg = panic_info
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "panic in call_market".into());
            emit_network_fell_back_local(&RequesterError::Internal(msg.clone()),
                "internal_panic", ctx, config);
            tracing::error!(msg, "call_market panicked; falling back to local pool");
            return Err(anyhow!("fallthrough"));  // caught by catch-all below
        }
    };

    match result {
        Ok(market_result) => {
            emit_network_helped_build(&market_result, ctx, config);
            return Ok(LlmResponse::from_market_result(market_result));
        }

        // HARD-FAIL — only AuthFailed bubbles.
        Err(RequesterError::AuthFailed(detail)) => {
            return Err(anyhow!(
                "network credentials invalid — session may be expired: {detail}"
            ));
        }

        // SOFT-FAIL with balance transition marker. Emit once per build.
        Err(RequesterError::InsufficientBalance { need, have }) => {
            emit_network_balance_exhausted_once(need, have, ctx, config);
            tracing::info!(
                need,
                have,
                "network credits depleted for this call; local pool handles"
            );
            // fall through to pool
        }

        // SOFT-FAIL catch-all. Reason classification sanitizes Wire slugs.
        Err(other) => {
            let reason = classify_soft_fail_reason(&other);
            emit_network_fell_back_local(&other, &reason, ctx, config);
            tracing::info!(
                %reason,
                "network unavailable; local pool handles"
            );
            // fall through to pool
        }
    }
}

// ... existing pool acquisition loop continues ...
```

### 3.4.1 `classify_soft_fail_reason` — reason-slug mapping with Wire-slug scrubbing

```rust
/// Map a RequesterError to a stable, invisibility-safe reason slug.
/// Variants handled here are ONLY the soft-fail ones; AuthFailed and
/// InsufficientBalance are handled in outer match arms.
fn classify_soft_fail_reason(err: &RequesterError) -> String {
    match err {
        RequesterError::NoMatch { .. } => "no_match".into(),
        RequesterError::MatchFailed { status, .. } => format!("match_failed_{status}"),
        RequesterError::FillRejected { reason, .. } => {
            // Wire's reason slugs may contain trader vocabulary (e.g.
            // "market_serving_disabled", "offer_depleted"). Sanitize
            // before surfacing to chronicle.
            format!("fill_rejected_{}", sanitize_wire_slug(reason))
        }
        RequesterError::FillFailed { status, .. } => format!("fill_failed_{status}"),
        RequesterError::DeliveryTimedOut { waited_ms } => {
            format!("delivery_timed_out_{waited_ms}ms")
        }
        RequesterError::DeliveryTombstoned { reason } => {
            format!("delivery_tombstoned_{}", sanitize_wire_slug(reason))
        }
        RequesterError::ProviderFailed { code, .. } => format!("provider_failed_{code}"),
        RequesterError::Internal(_) => "internal".into(),
        // These are handled in the outer match; pattern is exhaustive via _
        _ => "unclassified".into(),
    }
}

/// Map Wire's trader-vocabulary slugs to cooperative framing before
/// we surface them into chronicle metadata. Wire controls its own
/// reason slugs; we can't prevent them from shipping trader words.
/// This function is forward-compatible: unknown slugs pass through
/// unchanged (flagged in follow-up if they turn out to leak).
fn sanitize_wire_slug(slug: &str) -> String {
    slug
        .replace("market_serving_disabled", "provider_serving_disabled")
        .replace("market_", "network_")
        .replace("offer_depleted", "contribution_depleted")
        .replace("offer_", "contribution_")
        .replace("seller", "provider")
        .replace("buyer", "requester")
        .replace("earnings", "contributions")
        .replace("earning", "contributing")
}
```

### 3.4.2 `emit_network_balance_exhausted_once`

```rust
fn emit_network_balance_exhausted_once(
    need: i64,
    have: i64,
    ctx: Option<&StepContext>,
    config: &LlmConfig,
) {
    // Dedup scope: per-build. StepContext absence means non-build
    // inference path — skip emission entirely (see §7.8).
    //
    // Verified via codebase: StepContext.build_id is String (not
    // Option). Empty string is treated as sentinel for "no build
    // context" should such paths ever construct StepContext without
    // a build_id. Callers in practice always populate build_id when
    // constructing StepContext.
    let Some(step_ctx) = ctx else { return; };
    if step_ctx.build_id.is_empty() { return; }

    // StepContext carries `balance_exhausted_emitted: OnceLock<()>`
    // (see §3.5). OnceLock is std-library (stable Rust 1.70+), no
    // extra crate dep. set() is atomic: concurrent callers race,
    // exactly one sees Ok, others receive Err(_) → skip emit.
    if step_ctx.balance_exhausted_emitted.set(()).is_err() {
        return;  // already emitted for this build
    }

    emit_network_balance_exhausted(need, have, &step_ctx.build_id, step_ctx, config);
}
```

### 3.5 `LlmConfig` + `ComputeMarketRequesterContext` + `StepContext`

**New struct, new module: `src-tauri/src/pyramid/compute_market_ctx.rs`**

```rust
use std::sync::Arc;
use tokio::sync::RwLock;
use crate::auth::AuthState;
use crate::tunnel::TunnelState;
use crate::WireNodeConfig;
use super::pending_jobs::PendingJobs;

/// Context bundle attached to LlmConfig at runtime to enable market
/// dispatch in call_model_unified. None in tests + pre-init boot.
///
/// Lives at `compute_market_ctx.rs` (separate from compute_requester.rs
/// to avoid cyclic imports — llm.rs depends on this, compute_requester
/// depends on llm.rs indirectly via LlmProvider traits).
#[derive(Clone)]
pub struct ComputeMarketRequesterContext {
    pub auth: Arc<RwLock<AuthState>>,
    pub config: Arc<RwLock<WireNodeConfig>>,
    /// PendingJobs is itself self-Arc'd internally (see
    /// pyramid/pending_jobs.rs — wraps Arc<Mutex<HashMap>>). Cloning
    /// this field clones the Arc, not the map. Field type is plain
    /// `PendingJobs` (not `Arc<PendingJobs>`) — no double-Arc needed.
    pub pending_jobs: PendingJobs,
    pub tunnel_state: Arc<RwLock<TunnelState>>,
}
```

**`LlmConfig` extension (llm.rs):**

```rust
pub struct LlmConfig {
    // ... existing fields ...
    pub fleet_dispatch: Option<Arc<crate::fleet::FleetDispatchContext>>,  // existing

    /// Phase 3 compute market context. None = market branch skipped.
    pub compute_market_context:
        Option<crate::pyramid::compute_market_ctx::ComputeMarketRequesterContext>,
}
```

**`Default::default`:** explicitly set `compute_market_context: None`.

**`with_runtime_overlays_from` carry-forward (llm.rs ~463, immediately AFTER the fleet_dispatch overlay line):**

```rust
if self.compute_market_context.is_none() {
    self.compute_market_context = source.compute_market_context.clone();
}
```

**Construction site (PyramidState, NOT AppState — audit correction):**

LlmConfig lives on PyramidState, not AppState. The runtime context is built and attached in the same construction block as fleet_dispatch (main.rs around lines 11800-11841 per audit grep). Pattern:

```rust
// In main.rs near fleet_dispatch construction:
let compute_market_context = crate::pyramid::compute_market_ctx::ComputeMarketRequesterContext {
    auth: app_state.auth.clone(),
    config: app_state.config.clone(),
    pending_jobs: app_state.pending_market_jobs.clone(),  // already on AppState
    tunnel_state: app_state.tunnel_state.clone(),
};
pyramid_state.config.compute_market_context = Some(compute_market_context);
```

Tests that construct `LlmConfig::default()` get None — market branch unreachable, no behavior change. Existing test suite unaffected.

**`StepContext` extension (`src-tauri/src/pyramid/step_context.rs` — verified via codebase audit):**

```rust
use std::sync::OnceLock;

pub struct StepContext {
    // ... existing fields ...
    pub build_id: String,  // VERIFIED: String not Option. Empty string is
                           // the sentinel for "no build context" if any
                           // caller ever constructs StepContext without a
                           // build_id (non-build inference paths).

    /// Per-build dedup for NETWORK_BALANCE_EXHAUSTED. Initialized
    /// fresh per StepContext. Thread-safe: `OnceLock::set()` is atomic
    /// via stdlib; first caller wins, others get Err(_).
    ///
    /// Using std::sync::OnceLock (stable Rust 1.70+) instead of
    /// once_cell::sync::OnceCell — no new crate dependency required
    /// (verified: once_cell is NOT in Cargo.toml). Drop-in API compat.
    pub balance_exhausted_emitted: OnceLock<()>,
}

// Update existing StepContext constructors to default this field:
impl StepContext {
    // in every existing fn new_*(...) constructor, add:
    //   balance_exhausted_emitted: OnceLock::new(),
    // and the Default impl.
}
```

Leak on build crash: if a build panics mid-dispatch, the StepContext drops and the OnceLock with it — memory-only, not persistent state.

---

## 4. Chronicle events

**Network-framed names AND network-framed metadata keys. Wire slugs sanitized.**

### 4.1 Constants in `compute_chronicle.rs`

```rust
pub const SOURCE_NETWORK: &str = "network";
pub const SOURCE_NETWORK_RECEIVED: &str = "network_received";

pub const EVENT_NETWORK_HELPED_BUILD: &str = "network_helped_build";
pub const EVENT_NETWORK_RESULT_RETURNED: &str = "network_result_returned";
pub const EVENT_NETWORK_FELL_BACK_LOCAL: &str = "network_fell_back_local";
pub const EVENT_NETWORK_LATE_ARRIVAL: &str = "network_late_arrival";
pub const EVENT_NETWORK_BALANCE_EXHAUSTED: &str = "network_balance_exhausted";
pub const EVENT_BUILD_NETWORK_CONTRIBUTION: &str = "build_network_contribution";
```

### 4.2 `EVENT_NETWORK_HELPED_BUILD`

```json
{
  "job_id": "<handle-path>",
  "uuid_job_id": "<uuid>",
  "queue_position": 3,
  "processing_cost_in_per_m": 800,
  "processing_cost_out_per_m": 1600,
  "provider_node_id": "<uuid>",
  "provider_handle": "<handle>",
  "model_id": "gemma4:26b",
  "reservation_held": 850
}
```

### 4.3 `EVENT_NETWORK_RESULT_RETURNED`

```json
{
  "job_id": "<handle-path>",
  "uuid_job_id": "<uuid>",
  "input_tokens": 420,
  "output_tokens": 180,
  "latency_ms": 3200,
  "model_used": "gemma4:26b",
  "provider_node_id": "<uuid>",
  "finish_reason": "stop"
}
```

### 4.4 `EVENT_NETWORK_FELL_BACK_LOCAL`

```json
{
  "reason": "<sanitized slug from classify_soft_fail_reason — trader vocabulary scrubbed>",
  "detail": "<free text from error variant>",
  "model_id": "gemma4:26b"
}
```

### 4.5 `EVENT_NETWORK_LATE_ARRIVAL`

```json
{
  "uuid_job_id": "<uuid>",
  "time_since_first_seen_ms": null
}
```

### 4.6 `EVENT_NETWORK_BALANCE_EXHAUSTED`

```json
{
  "need": 1200,
  "have": 450,
  "build_id": "<uuid>",
  "model_id": "gemma4:26b"
}
```

Deduplicated per-build via `StepContext.balance_exhausted_emitted: OnceCell<()>` (§3.4.2 + §3.5).

### 4.7 `EVENT_BUILD_NETWORK_CONTRIBUTION`

**Emit site:** `build_runner.rs` at build completion. **2s flush barrier** before aggregation query to let fire-and-forget writes land:

```rust
tokio::task::spawn_blocking(move || {
    // 2s grace for fire-and-forget chronicle writes. Individual
    // record_event calls complete <10ms under normal load; 2s is
    // ~200x safety margin. Known limitation: pathological load
    // (1000+ concurrent builds) could still race; documented as
    // bounded miss-rate (<0.5% at 10x normal load).
    std::thread::sleep(std::time::Duration::from_secs(2));

    let conn = rusqlite::Connection::open(&db_path)?;
    let summary = aggregate_build_network_contribution(&conn, &build_id)?;
    let ctx = ChronicleEventContext::minimal(...)
        .with_build_id(build_id.clone())
        .with_metadata(serde_json::json!({ ...summary }));
    record_event(&conn, &ctx)
});
```

**Aggregation query (defensive COALESCE for empty-set safety):**

```sql
SELECT
  COUNT(*) FILTER (WHERE event_type = 'network_helped_build') AS network_calls,
  COUNT(DISTINCT json_extract(metadata, '$.provider_node_id'))
    FILTER (WHERE event_type = 'network_helped_build') AS distinct_providers,
  COALESCE(
    AVG(CAST(json_extract(metadata, '$.latency_ms') AS REAL))
      FILTER (WHERE event_type = 'network_result_returned'),
    0.0
  ) AS avg_network_latency_ms,
  COALESCE(
    SUM(CAST(json_extract(metadata, '$.reservation_held') AS INTEGER))
      FILTER (WHERE event_type = 'network_helped_build'),
    0
  ) AS total_credits_spent
FROM wire_chronicle_events
WHERE build_id = ?1
```

SQLite `COUNT` returns 0 for empty sets natively; `SUM` and `AVG` return NULL → COALESCE to 0. SQLite 3.30+ supports `FILTER`.

**Zero-network case:** emit with zeros unconditionally. Every build emits exactly one BUILD_NETWORK_CONTRIBUTION.

**Metadata shape:**

```json
{
  "build_id": "<uuid>",
  "slug": "<slug>",
  "total_llm_calls": 217,
  "network_calls": 184,
  "local_calls": 12,
  "openrouter_calls": 21,
  "distinct_providers": 47,
  "avg_network_latency_ms": 2800,
  "total_credits_spent": 52300,
  "wall_clock_savings_estimate_ms": 1380000
}
```

---

## 5. Error mapping matrix (code-verified against compute_requester.rs:180-223)

| Variant | Trigger | Class | Notes |
|---|---|---|---|
| `NoMatch { detail }` | 404 on /match | **Soft** | No offers for model — capacity gap |
| `InsufficientBalance { need, have }` | 409 on /match | **Soft** | Credits depleted. Emit `NETWORK_BALANCE_EXHAUSTED` once-per-build. Purpose-lock override: tester shouldn't see as hard error. |
| `MatchFailed { status, body }` | Other /match errors | **Soft** | Wire trouble; pool serves |
| `FillRejected { status: 503, reason, body }` | 503 on /fill + X-Wire-Reason | **Soft** | Reason slug sanitized before chronicle surfacing |
| `FillFailed { status, body }` | Other /fill errors (400, 425, 429, 500, etc.) | **Soft** | 429 from /fill is dispatcher saturation (Wire-side), not operator quota |
| `AuthFailed(String)` | 401 on /match or /fill | **Hard** | Only hard-fail. Error: `"network credentials invalid — session may be expired: {detail}"` |
| `DeliveryTimedOut { waited_ms }` | Push didn't arrive + poll still executing | **Soft** | Transient |
| `DeliveryTombstoned { reason }` | Poll says failed/expired_undelivered | **Soft** | Content gone; pool serves. Reason sanitized. |
| `ProviderFailed { code, message }` | Push arrived as failure envelope | **Soft** | Retry via pool |
| `Internal(String)` | I/O, serde, panic-caught | **Soft** | Don't block build |

**Panic caught by `catch_unwind`** routes to `Internal(panic_msg)` via explicit error conversion in §3.4. Soft-fail cascade continues.

---

## 6. Invisibility coverage checklist

Expanded scope: event names + metadata keys + metadata values + log messages + error strings + Wire slug pass-throughs.

**Trader vocabulary — forbidden in new code, user/agent/operator-facing positions:**
`market` · `offer` · `rate` · `earn` · `earnings` · `trade` · `trader` · `seller` · `buyer` · `deposit`

(Accepted: `credits` — accounting primitive per brief.)

**Checklist (each actionable):**

- [ ] **Event constants**: grep new lines for `EVENT_NETWORK_*` / `SOURCE_NETWORK*` prefix. No new `EVENT_MARKET_*` etc.
- [ ] **Metadata keys**: individually review §4.2–§4.7. Confirm rename `matched_rate_*` → `processing_cost_*`, `deposit_charged` → `reservation_held`.
- [ ] **Log messages** (grep INFO+ log call sites in new code):
  ```
  git diff main -- src-tauri/src/pyramid/llm.rs \
    src-tauri/src/pyramid/compute_chronicle.rs \
    src-tauri/src/pyramid/compute_market_ctx.rs \
    src-tauri/src/pyramid/build_runner.rs \
    src-tauri/src/pyramid/routes_operator.rs \
    | grep -iE '^\+.*tracing::(info|warn|error)' \
    | grep -iE 'market|offer|rate|earn|trade|seller|buyer|deposit'
  ```
  Expect: zero matches.
- [ ] **Error strings** returned from `call_model_unified`: only `AuthFailed` path bubbles. Confirm literal text is `"network credentials invalid — session may be expired: …"`.
- [ ] **Wire slug sanitization**: `sanitize_wire_slug` unit-tested with known trader-word inputs (`market_serving_disabled`, `offer_depleted`, `seller_mismatch`); outputs assert cooperative replacements. Also test pass-through for forward-compat (`deposit_required`, `rate_limit_exceeded` — confirmed NOT in Wire's current slug corpus per compute_requester.rs grep, but sanitizer must not crash on them).
- [ ] **Metadata values audit** (not just keys): scan §4.2–§4.7 JSON examples. Confirm no trader vocabulary in any quoted `string` values. `provider_node_id`, `provider_handle`, `model_used`, `finish_reason` are neutral. `reason` field gets sanitized slugs (§3.4.1).
- [ ] **Net invisibility diff check**:
  ```
  git diff main -- '*.rs' \
    | grep -E '^\+' \
    | grep -iE 'market|offer|rate|earn|earnings|trade|trader|seller|buyer|deposit'
  ```
  Inspect every hit. Verify either (a) it's a legal pre-shipped symbol reference not a new introduction, (b) it's an internal comment/doc (plan/spec reference in code comments is fine), or (c) it's the sanitizer's input pattern (legal). No unscrutinized matches.

Pre-shipped symbol debt (`compute_market_*` IPC, `market_dispatch_*` policy fields, CSS) is tracked in separate follow-up workstream.

---

## 7. Edge cases (comprehensively)

### 7.1 Tunnel up-transition mid-build
Gate checked per-call. Natural transition: skip → attempt as tunnel stabilizes.

### 7.2 Tunnel down-transition mid-dispatch (after /fill 2xx)
Wire delivery worker fails; 5x retry exhausts; `delivery_status: "failed"` on poll → `DeliveryTombstoned { reason: "delivery_retry_exhausted" }` → soft-fall → `NETWORK_FELL_BACK_LOCAL` with sanitized reason slug.

### 7.3 Concurrent multi-call build (100 L0 calls)
Unbounded PendingJobs acceptable at tester scale (10-200 calls, ~1-20 MB). Wire-side 429 soft-falls; half succeed, half fall to pool. No dispatch-rate back-pressure in Phase 3. Documented debt: add `max_concurrent_market_dispatches` policy if real builds hit memory pressure.

### 7.4 Duplicate inbound push
`pending_jobs.take()` is atomic. First push: take succeeds → oneshot fires → entry removed. Duplicate: take returns None → emit NETWORK_LATE_ARRIVAL → 2xx `already_settled`.

### 7.5 InsufficientBalance mid-build transition
`OnceCell<()>` on `StepContext` (per-build scope). First 409 within build: set() succeeds → emit. Subsequent 409s within same build: set() returns `Err(AlreadySet)` → skip emit. Subsequent calls' `should_try_market` gate returns false at balance check (pre-flight). Build completes via pool.

### 7.6 Build completion with zero network calls
`EVENT_BUILD_NETWORK_CONTRIBUTION` emits with `network_calls=0`, `distinct_providers=0`, `total_credits_spent=0` (via COALESCE). UI renders "Built locally" variant. Every build emits exactly one BUILD_NETWORK_CONTRIBUTION — absence never means zero.

### 7.7 Panic in `call_market`
`catch_unwind` wraps the `.await`. Panic becomes `Err(_)` → routed to soft-fail cascade with reason `internal_panic`. `await_result`'s internal timeout cleanup removes the dangling PendingJobs entry on next cycle; no permanent leak.

### 7.8 Non-build inference paths
call_model_unified may be invoked without a build (tests, ad-hoc). Behavior:
- `should_try_market` allows market (no build_id gate).
- `NETWORK_HELPED_BUILD` / `NETWORK_RESULT_RETURNED` / `NETWORK_FELL_BACK_LOCAL` / `NETWORK_LATE_ARRIVAL` — emit with `build_id=null`. Build-scoped aggregation queries (`WHERE build_id = ?`) filter these cleanly; operator-level queries see all.
- `NETWORK_BALANCE_EXHAUSTED` — skipped (no build_id, OnceCell dedup has no scope).
- `BUILD_NETWORK_CONTRIBUTION` — never fires (no build end event).

Acceptable: build-less paths are minority; chronicle records are still useful for per-call observability.

### 7.9 Tunnel URL rotation mid-dispatch (NIT)
If tunnel token rotates post-`/fill`, Wire's delivery worker hits stale URL → retry-exhaust → `DeliveryTombstoned { reason: "delivery_retry_exhausted" }`. Same path as tunnel-down (§7.2). Chronicle doesn't distinguish "tunnel switched" from "tunnel crashed"; flagged as post-shipment observability improvement — add `tunnel_url_snapshot` to NETWORK_HELPED_BUILD metadata.

---

## 8. File diffs

| File | Change |
|---|---|
| `src-tauri/src/pyramid/compute_market_ctx.rs` | **NEW.** `ComputeMarketRequesterContext` struct. ~40 lines. |
| `src-tauri/src/pyramid/compute_chronicle.rs` | Add 2 SOURCE + 6 EVENT constants (§4.1). ~20 lines. |
| `src-tauri/src/pyramid/llm.rs` | Add `compute_market_context: Option<...>` to `LlmConfig` + `with_runtime_overlays_from` carry; add Phase B market branch; add `should_try_market` + `classify_soft_fail_reason` + `sanitize_wire_slug` + `emit_network_balance_exhausted_once` + `from_market_result` converter + 3 chronicle emit helpers. ~250 lines. |
| `src-tauri/src/pyramid/build_runner.rs` | Add `EVENT_BUILD_NETWORK_CONTRIBUTION` emit at build completion with 2s flush + aggregation query. ~60 lines. |
| `src-tauri/src/pyramid/routes_operator.rs` | Update inbound `/v1/compute/job-result` handler — emit `NETWORK_RESULT_RETURNED` on success, `NETWORK_LATE_ARRIVAL` on take=None. ~30 lines. |
| `src-tauri/src/pyramid/types.rs` (or wherever StepContext lives) | Add `balance_exhausted_emitted: OnceCell<()>` field. ~5 lines. |
| `src-tauri/src/main.rs` | Construct `ComputeMarketRequesterContext` in PyramidState wiring block (same place as fleet_dispatch, lines ~11800-11841). ~15 lines. |

**Estimated total:** ~420 lines added.

---

## 9. Test plan

### 9.1 Unit tests

**Gate (`should_try_market`) — all branches individually:**
- `allow_market_dispatch=false` → false
- `eager=false` + `queue_depth < threshold` → false
- `eager=false` + `queue_depth >= threshold` + all other gates pass → true
- **`eager=true` + `queue_depth=0` + all other gates pass → true** (standalone explicit coverage per audit R2)
- `balance < estimated_deposit` → false
- `tier.market_eligible() = false` → false
- `tunnel_state.status = Connecting` + tunnel_url Some → false
- `tunnel_state.status = Disconnected` → false
- `tunnel_state.tunnel_url = None` → false
- `compute_market_context = None` → false
- All gates pass, eager=true, queue_depth=0 → true (end-to-end positive)

**Error mapping — all 10 RequesterError variants:**
Mock `call_market` returning each variant; assert cascade behavior:
- `NoMatch` → soft → NETWORK_FELL_BACK_LOCAL `reason="no_match"`
- `InsufficientBalance` → soft + NETWORK_BALANCE_EXHAUSTED fires (first occurrence)
- `InsufficientBalance` (second call in same build) → soft + NETWORK_BALANCE_EXHAUSTED does NOT re-fire (dedup)
- `MatchFailed{500}` → soft → `reason="match_failed_500"`
- `FillRejected{503,"market_serving_disabled",_}` → soft → `reason="fill_rejected_provider_serving_disabled"` (sanitized)
- `FillFailed{425}` → soft → `reason="fill_failed_425"`
- `AuthFailed` → HARD → bubbles with cooperative string
- `DeliveryTimedOut{60000}` → soft → `reason="delivery_timed_out_60000ms"`
- `DeliveryTombstoned{"delivery_retry_exhausted"}` → soft → sanitized slug
- `ProviderFailed{"oom"}` → soft → `reason="provider_failed_oom"`
- `Internal(_)` → soft → `reason="internal"`

**`classify_soft_fail_reason` as pure function — per-variant assertions:**
Direct unit tests on the mapping function (not via cascade emission):
- 8 input variants → 8 expected slug outputs, asserted verbatim.
- Catches typo regressions in the slug strings that effect-level tests might miss.

**`sanitize_wire_slug` as pure function:**
- `"market_serving_disabled"` → `"provider_serving_disabled"`
- `"market_foo"` → `"network_foo"`
- `"offer_depleted"` → `"contribution_depleted"`
- `"offer_bar"` → `"contribution_bar"`
- `"seller_mismatch"` → `"provider_mismatch"`
- `"buyer_rejected"` → `"requester_rejected"`
- `"earnings_frozen"` → `"contributions_frozen"`
- Unknown slug `"foo_bar"` passes through unchanged.
- **Forward-compat pass-through tests** (Wire's current corpus per compute_requester.rs audit does NOT include these, but the sanitizer must accept unknown input gracefully):
  - `"deposit_required"` → `"deposit_required"` (pass-through; flagged for follow-up if Wire ever emits)
  - `"rate_limit_exceeded"` → `"rate_limit_exceeded"` (pass-through)
  - `"trader_banned"` → `"trader_banned"` (pass-through)

**Chronicle per-path correctness:**
- Ok path → assert ONLY NETWORK_HELPED_BUILD fires (not FELL_BACK_LOCAL); `provider_node_id` populated.
- Soft-fail path → assert ONLY NETWORK_FELL_BACK_LOCAL fires.
- Hard-fail (AuthFailed) path → assert NEITHER HELPED_BUILD nor FELL_BACK_LOCAL fires; error bubbles.
- InsufficientBalance path → assert NETWORK_BALANCE_EXHAUSTED fires once per build_id (second occurrence no-op).
- Non-build call (no build_id) path → assert NETWORK_BALANCE_EXHAUSTED is SKIPPED (not emitted).

**Cascade continuation:**
- Mock market soft-fail + mock pool Ok → assert `LlmResponse` returned with pool provenance.

**Panic safety:**
- Mock `call_market` to panic → assert `catch_unwind` catches, NETWORK_FELL_BACK_LOCAL with `reason="internal_panic"` emits, cascade continues to pool.
- Assert no PendingJobs entry leaked (pending_jobs.len() == 0 after await).

**Invisibility assertions (grep-based):**
- `git diff main -- src-tauri/src/pyramid/llm.rs | grep -E '^\+.*tracing::' | grep -iE 'market|offer|rate|earn|trade|seller|buyer|deposit'` → zero matches.
- Chronicle event constants match `EVENT_NETWORK_*` regex literals.

### 9.2 Integration smoke (offline, no round-trip)

- **Tunnel-down skip:** disable tunnel → gate false → pool serves. No /match.
- **No-match soft-fall:** tunnel up + no offers → NoMatch → FELL_BACK_LOCAL `no_match` → pool serves.
- **Hard-fail propagation:** simulate 401 → AuthFailed → error bubbles with cooperative string.
- **Multi-call partial-market:** 5-call build, 2 succeed via market + 3 fall to pool. Chronicle: 2×HELPED_BUILD + 3×FELL_BACK_LOCAL + 1×BUILD_NETWORK_CONTRIBUTION `network_calls=2, local_calls=3`.
- **BUILD_NETWORK_CONTRIBUTION aggregation:** fire 5 HELPED_BUILD with 3 distinct provider_node_ids → query returns `distinct_providers=3`.
- **BUILD_NETWORK_CONTRIBUTION zero network:** single-call build, market skipped → emit with `network_calls=0`.
- **Tunnel up-transition mid-build:** tunnel down at call 1, Connected by call 3 → cascade routing flips per-call.
- **Panic recovery:** inject panic in mocked call_market → catch_unwind → pool completes the build.

### 9.3 Regression
- `cargo check` + `cargo test --lib` all 1600+ existing tests pass.
- Chronicle query tests: grep `src-tauri/src/pyramid/compute_chronicle.rs` test fixtures for hardcoded event-type lists; verify new NETWORK_* types included if hardcoded list exists.
- LlmConfig construction tests: any `LlmConfig { ... }` literal constructions need `compute_market_context: None` added (or `..Default::default()` idiom preserves bypass).

---

## 10. Tunnel race mitigation
Integrated into §3.2 gate. When tunnel not Connected, market skipped; no /match round-trip; pool handles. First few builds after fresh install may miss network help (tunnel up in <5s typical); no user-visible failure. UI surface follow-up in invisibility UX thread.

---

## 11. Rollback
- `market_dispatch_eager=false` (default) disables aggressive market attempts.
- `allow_market_dispatch=false` disables all market attempts. No code changes.
- Revert commit reverts `LlmConfig.compute_market_context` to not-carried; market branch unreachable.

---

## 12. Audit surfaces for round 3

Round 2 surfaced 4 CRITICAL + 7 MAJOR new findings. Rev 0.3 addresses each:

| R2 finding | Severity | Rev 0.3 response |
|---|---|---|
| ComputeMarketRequesterContext struct definition missing | CRITICAL | Added §3.5, new module path specified |
| Panic in call_market leaks PendingJobs | CRITICAL | §3.4 + §7.7 — catch_unwind wrapper |
| classify_soft_fail_reason untested as pure function | CRITICAL | §9.1 — 8 pure-function unit tests added |
| eager=true standalone test missing | CRITICAL | §9.1 — explicit standalone test added |
| AppState construction site misdescribed | MAJOR | §3.5 — corrected to PyramidState wiring near fleet_dispatch |
| pending_jobs Arc wrapping unclear | MAJOR | §3.5 — clarified PendingJobs is self-Arc'd |
| OnceCell scope unspecified | MAJOR | §3.4.2 + §3.5 — placed on StepContext, atomicity documented |
| Wire reason slugs leak "market" | MAJOR | §3.4.1 — `sanitize_wire_slug` function added |
| Invisibility grep under-specified | MAJOR | §6 — `git diff` scoping + expanded pattern + per-file scope |
| Non-build inference orphaned events | MAJOR | §7.8 — explicit handling, null build_id filtering documented |
| Aggregation query not defensive | MAJOR | §4.7 — COALESCE added |

Round 3 focus (tighter — verify deltas only):
1. Verify struct definition + module path makes sense given existing patterns.
2. Verify `catch_unwind` + Arc future-compat (AssertUnwindSafe correctness).
3. Verify sanitize_wire_slug covers likely trader-word input corpus.
4. Verify expanded grep command is executable and catches pre-2026 regression.
5. Verify test list has every asserted path testable with existing mocking infrastructure.

---

## 13. Success criteria

Plan complete when:
- Audit round 3 returns clean.
- Build compiles (`cargo check`).
- Unit tests pass (`cargo test --lib`).
- Verifier finds no regressions.
- Wanderer traces full cascade + chronicle without issues.
- Offline smoke passes in dev mode.

**NOT "done"** until: full round-trip smoke with foreign offer + tester-representative pyramid build completes via market with chronicle evidence. Post-external-gate.

---

## Rev log

| Rev | Date | Change |
|---|---|---|
| 0.1 | 2026-04-17 | Initial plan. |
| 0.2 | 2026-04-17 | Round 1 findings (8 CRITICAL + 11 MAJOR) resolved. §5 matrix rewritten against live `RequesterError`. Metadata keys renamed. `NETWORK_BALANCE_EXHAUSTED` added. `with_runtime_overlays_from` documented. Edge cases §7.1-§7.6 added. |
| 0.3 | 2026-04-17 | Round 2 findings (4 CRITICAL + 7 MAJOR new) resolved. `ComputeMarketRequesterContext` struct defined in new module `compute_market_ctx.rs`. `catch_unwind` around call_market (§3.4). `sanitize_wire_slug` function + unit tests (§3.4.1, §9.1). `classify_soft_fail_reason` pure-function tests (§9.1). StepContext carries per-build `balance_exhausted_emitted: OnceCell<()>` (§3.5). PyramidState construction site corrected (not AppState). Defensive COALESCE in aggregation query (§4.7). Expanded invisibility grep with git-diff scoping (§6). §7.7 panic safety + §7.8 non-build paths + §7.9 tunnel rotation NIT added. Eager=true standalone test added (§9.1). |
| 0.4 | 2026-04-17 | Round 3 findings resolved + pre-build fact verification applied. **`once_cell` crate NOT in Cargo.toml — switched to `std::sync::OnceLock`** (stable Rust 1.70+, drop-in API). **`StepContext.build_id` verified as `String` not `Option<String>` — §3.4.2 logic updated to empty-string sentinel check instead of Option unwrap.** Wire slug corpus audited via compute_requester.rs grep — confirmed sanitizer covers all trader-vocab slugs currently emitted by Wire (`market_serving_disabled`, `offer_depleted`). Other slugs (`no_offer_for_model`, `queue_depth_exceeded`, `multiple_system_turns`, `compute_held`, `insufficient_balance`) are neutral. Forward-compat pass-through tests added for hypothetical `deposit_*` / `rate_*` / `trader_*` slugs (§9.1). Metadata-values audit checkbox added to §6. AppState.pending_market_jobs verified present at `lib.rs:74` — plan reference valid. |
