# Compute Market Phase 3 — Provider Delivery Worker Spec

**Date:** 2026-04-20
**Author:** Claude (agent-wire-node upstairs mac)
**Status:** Draft — Stage 1 + Stage 2 audits + pillars pass + Wire-owner bilateral alignment
**Rev:** 0.5

---

## Purpose

Close the last hop in the compute market provider path: **node's outbox `state='ready'` → POST result envelope to Wire's callback URL → CAS ready→delivered.**

Without this, the provider's GPU completes inference, the outbox row transitions to `ready` (worker-heartbeat-lost path or natural completion), and sits there until `ready_retention_secs` (1800s) expires it to `failed`. Wire never learns the result. Every `/fill` dispatch looks like a successful handshake followed by silent abandonment — which is exactly what we reproduced on 2026-04-20: `delivery_status=pending`, `delivery_attempts=0`, `last_delivery_error=NULL` across 3 jobs on Wire.

The Apr 17 handoff explicitly listed this as "node-side Phase 3" to build in parallel with Wire routes. Commit `43b8704` shipped the requester half (inbound `/v1/compute/job-result` handler + pending_jobs). The provider half was never written. This spec plans it.

**Related pre-existing bug this spec also fixes:** `spawn_market_worker`'s failure branch calls `fleet_outbox_bump_delivery_attempt` (CAS on `status='ready'`) against rows still in `status='pending'`. The CAS silently no-ops, the attempt counter doesn't move, and the row stays `pending` until expiry-sweep synthesizes a generic error. The actual inference error message is never persisted. Must fix in this work — otherwise a failure-path delivery worker has nothing to deliver for inference failures.

## Scope

In scope:
- New worker task `market_delivery_loop` that sweeps `fleet_result_outbox` for ready MarketStandard rows and POSTs their result envelopes.
- New persistent columns on `fleet_result_outbox` for (a) the callback_auth bearer, (b) the delivery lease, (c) the post-failure backoff deadline, (d) inference latency_ms.
- State machine: ready → (claimed with lease) → delivered OR back to ready with backoff OR failed (terminal).
- Separate columns for **lease** (prevents double-POST while in flight) and **next-attempt-at** (backoff gate) — two independent semantics, no overloading.
- Exponential backoff on delivery failures (bounded by backoff_cap).
- Chronicle events distinct from Wire's chronicle (no name collision) with coverage of every terminal + CAS-lost path.
- Nudge channel so worker-completion triggers immediate delivery attempt (no 15s tick latency).
- Fix for the pre-existing pending-never-promoted bug in `spawn_market_worker`'s failure branch.
- Custom `Debug` impl on `CallbackAuth` to redact token from logs.
- ~13 unit tests covering happy-path, retry-on-5xx, terminal 4xx codes (400/401/403/404/410), max-attempts, lease race, CAS-lost-to-sweep, restart-recovery, backoff math, envelope adapter edge cases (Option→int, empty model_used), token-not-logged, panic-survivor, envelope-synth-from-sweep.

Out of scope (deliberately deferred):
- Relay-market (`callback_kind='Relay'`) delivery. Claim query filters `callback_kind = 'MarketStandard'` only. Relay rows remain in `ready` until the relay market ships; they will be picked up by a parallel relay-delivery worker (or this worker extended) when relay lands. Functional impact today: zero (no Relay rows are produced by any current code path).
- Requester-side changes — already complete via `43b8704`.
- Wire-side changes — Wire's callback endpoint already exists per contract §2.3. **EXCEPT:** chronicle event name disambiguation (MAJOR-6) is bilateral — see §"Bilateral items".

---

## Architecture

### Module: `src-tauri/src/pyramid/market_delivery.rs` (new file)

Parallel to `fleet_outbox_sweep.rs` but focused purely on **delivery** (ready → delivered), not **expiry** (pending → ready on heartbeat lost, ready → failed on wall-clock exhaustion). The existing `market_outbox_sweep_loop` continues to own expiry; this module owns the POST.

### Trigger model: nudge + periodic sweep

Two entry points into the loop:

1. **Periodic tick** — `tokio::time::interval(market_delivery_policy.outbox_sweep_interval_secs)` default 15s. Catches ready rows that (a) missed their nudge, (b) are eligible for retry after a backoff window.
2. **Nudge channel** — `MarketDispatchContext.delivery_nudge: UnboundedSender<()>`. Fired at every ready-promotion site. Delivery fires within debounce, not after the next 15s tick.

Same shape as the mirror push task (`market_mirror.rs`). Nudge is fire-and-forget (`unbounded_send().ok()`) from mutation sites.

**Nudge fire sites (enumerated — one rule: every transition INTO `status='ready'` for a MarketStandard row must nudge):**

1. `server.rs:spawn_market_worker` worker-success path, after `fleet_outbox_promote_ready_if_pending` returns 1 row matched.
2. `server.rs:spawn_market_worker` worker-failure path (NEW this spec — see §Pre-existing bug fix below).
3. `fleet_outbox_sweep.rs:sweep_expired_market_once` heartbeat-lost path (line ~568), after the synthesized-error promote.
4. Startup recovery path (NEW this spec — see §Restart semantics).

Admission-time nudging (outbox insert at `handle_market_dispatch`) is NOT fired — the row is `pending` at insert time, not `ready`, so a nudge would wake the loop for nothing. Per QUESTION-4 from audit.

### Supervisor pattern

Mirror the market-mirror supervisor landed in commit `57b1fa4` (`market_mirror.rs:supervise_mirror_loop`):
- `supervise_delivery_loop` wraps `delivery_loop` in `AssertUnwindSafe::catch_unwind`
- Emit `compute_delivery_task_panicked` / `compute_delivery_task_exited` chronicle events on panic/clean-exit
- 5s backoff on panic, respawn

Reuses the exact pattern that just shipped + prevents the silent-death class of bug we saw with the mirror task.

**Send-safety:** the POST future captures `reqwest::Client` (Send) and a `&MirrorTaskContext`-equivalent owning Arcs. No `MutexGuard` held across `.await` — locks acquired, read, dropped before any await, per the existing mirror task pattern.

---

## State machine

```
     ┌─────────┐   worker writes result    ┌──────────┐   POST 2xx + CAS=1   ┌───────────┐
     │ pending │ ──────────────────────▶   │  ready   │ ──────────────────▶   │ delivered │
     └─────────┘   (via promote_ready)     └──────────┘                        └───────────┘
         │         success OR failure          ▲   │
         │         envelope serialized         │   │ POST non-2xx OR network,
         │         (bug fix this spec)         │   │ attempts < max; bump attempt,
         │                                     │   │ delivery_next_attempt_at = now + backoff
         │                                     │   │
         │ expires_at elapsed                  └───┘
         │ (heartbeat lost) — sweep
         │ synthesizes error envelope
         │
         ▼
     (same ready state; nudge delivery)
                            │
                            │  POST 2xx, CAS returns 0 rows
                            │  because concurrent sweep
                            │  flipped ready→failed first
                            │  (expires_at exhausted)
                            ▼
                   emit compute_result_delivery_cas_lost
                   (Wire got it; node's row is 'failed' but
                    we observed the delivery — operator
                    dashboard requires the event)

     ready --attempts >= max or terminal HTTP code--> failed (CAS-ready)
     ready --expires_at <= now--> failed (sweep's existing path, untouched)
```

### Two separate columns for two semantics

The single-column overloading in rev 0.1 conflated "delivery in flight" with "backoff until retry." Split:

| Column | Semantic | Set by | Cleared by |
|---|---|---|---|
| `delivery_lease_until TIMESTAMPTZ NULL` | "A delivery worker has claimed this row and is actively POSTing. Others hands off." | Claim CAS (set to `now + callback_post_timeout_secs + 5`) | Terminal CAS (delivered/failed) OR startup recovery (cleared en-bloc on boot for ready market rows) |
| `delivery_next_attempt_at TIMESTAMPTZ NULL` | "After a transient failure, don't retry before this time." | Failure branch (set to `now + min(backoff_base × 2^attempts, backoff_cap)`) | Terminal CAS |

**Claim query** (batched — returns up to `max_concurrent_deliveries` rows per tick, atomic via `RETURNING *`):

```sql
UPDATE fleet_result_outbox
SET delivery_lease_until = ?now_plus_lease_secs
WHERE (dispatcher_node_id, job_id) IN (
  SELECT dispatcher_node_id, job_id
  FROM fleet_result_outbox
  WHERE status = 'ready'
    AND callback_kind = 'MarketStandard'   -- Relay deliberately excluded; see §Scope
    AND (delivery_lease_until IS NULL OR delivery_lease_until < ?now)
    AND (delivery_next_attempt_at IS NULL OR delivery_next_attempt_at <= ?now)
  ORDER BY created_at ASC
  LIMIT ?max_concurrent_deliveries
)
RETURNING
  dispatcher_node_id, job_id, status, callback_url, result_json,
  delivery_attempts, last_attempt_at, expires_at, created_at,
  callback_auth_token, delivery_lease_until, delivery_next_attempt_at,
  inference_latency_ms;
```

**Atomicity is load-bearing:** UPDATE + RETURNING is a single statement and a single DB round-trip per tick. Rows materialize with the lease stamp already present, so no TOCTOU window exists between "lease acquired" and "rows handed to worker." SQLite 3.35+ (2021-03) ships RETURNING; rusqlite exposes it via `prepare` + `query_map`. `bundled` sqlite3 feature on current rusqlite ships 3.44+. Verified available.

Without RETURNING, an UPDATE + SELECT split is a race — two concurrent ticks could each SELECT the same freshly-leased rows by re-reading the shared `?now_plus_lease_secs` timestamp. RETURNING closes this unconditionally.

The two predicates are independent: a row must be (not-leased OR lease expired) AND (backoff satisfied). Backoff no longer inflated by lease_secs (rev 0.1 bug). In-flight rows stay off-limits for lease duration regardless of backoff state.

**Lease duration** = `callback_post_timeout_secs + lease_grace_secs` — both policy-tunable per Pillar 37 spirit. A POST that takes longer than the lease duration is dead; next claim reclaims and Wire's idempotent handler deduplicates if the original POST did land.

**Bounded parallelism (Pillar 44 spirit):** one tick claims up to `max_concurrent_deliveries` rows; inside the loop they're processed via `tokio::stream::iter(claimed).for_each_concurrent(max_concurrent_deliveries, deliver_one)`. Prevents starvation when N rows go ready at once (sequential POSTs at 30s each × 1000 rows = 8 hours head-of-line blocking). Under normal load (N ≤ 32 = `max_inflight_jobs`) this is immaterial; under pathological load it bounds throughput correctly.

### Startup recovery

On `main.rs` startup, before spawning `supervise_delivery_loop`, run a one-shot:

```sql
UPDATE fleet_result_outbox
SET delivery_lease_until = NULL
WHERE callback_kind IN ('MarketStandard', 'Relay') AND status = 'ready';
```

Mirrors the existing `fleet_outbox_startup_recovery` pattern. Clears leases from a prior process that died mid-POST. Does NOT clear `delivery_next_attempt_at` — backoff across restarts is still the right behavior (a transient Wire-side blip doesn't reset just because the node rebooted).

### Callback auth token storage

Four new columns on `fleet_result_outbox` — additive migration, no breaking change for fleet rows (they'll have NULL for all four):

```sql
ALTER TABLE fleet_result_outbox ADD COLUMN callback_auth_token TEXT;
ALTER TABLE fleet_result_outbox ADD COLUMN delivery_lease_until TIMESTAMPTZ;
ALTER TABLE fleet_result_outbox ADD COLUMN delivery_next_attempt_at TIMESTAMPTZ;
ALTER TABLE fleet_result_outbox ADD COLUMN inference_latency_ms INTEGER;

-- Idempotent PRAGMA guard for existing DBs (mirror the pattern at db.rs
-- for prior conditional ALTERs). Check pragma_table_info('fleet_result_outbox')
-- for each column name before adding.
```

**`OutboxRow` struct extension** — the existing struct in `db.rs` carries `{dispatcher_node_id, job_id, status, callback_url, result_json, delivery_attempts, last_attempt_at, expires_at}`. The spec adds `created_at: String` AND the four new columns. All existing SELECT helpers that materialize `OutboxRow` (`fleet_outbox_sweep_expired`, `fleet_outbox_retry_candidates`, `market_outbox_sweep_expired`, `market_outbox_retry_candidates`) MUST be updated to include these in the projection. Without this, the struct field access at rusqlite `row.get("...")` raises at runtime for any code path consuming a row built via the old projection.

**New migration-tracking table** for the orphan-detection heuristic:

```sql
CREATE TABLE IF NOT EXISTS pyramid_schema_versions (
  name TEXT PRIMARY KEY,
  applied_at TIMESTAMPTZ NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
```

The migration that adds the four outbox columns also does:

```sql
INSERT OR IGNORE INTO pyramid_schema_versions(name)
VALUES ('fleet_result_outbox_v2_callback_auth_token');
```

The helper `row_predates_migration(row) = row.created_at < applied_at_for('fleet_result_outbox_v2_callback_auth_token')` looks up the timestamp once, caches it. A row with `callback_auth_token IS NULL` AND `row.created_at < applied_at` is a pre-migration orphan — one-shot deploy artifact with distinct terminal reason `orphaned_by_migration`. Rows with NULL token but `created_at >= applied_at` indicate a genuine token-plumbing bug and use the generic `callback_auth_token_invalid` reason.

Rationale for same-table: the outbox row IS the delivery unit; a separate table adds a join and another row-lifecycle to reason about with no correctness benefit. Per the "generalize not enumerate" feedback memory — same table serves Fleet + MarketStandard + Relay via `callback_kind` discriminator.

**Latency tracking:** `inference_latency_ms` is written by `spawn_market_worker` (which measures wall-clock duration of the LLM call) at promote-to-ready time. Delivery worker reads it at POST time. For sweep-synthesized errors (worker heartbeat lost), the column is NULL → envelope emits `latency_ms: 0` per the Wire validator's non-negative-integer requirement.

### Token redaction

`CallbackAuth` gets a custom `Debug` impl in `market_dispatch.rs`:

```rust
impl std::fmt::Debug for CallbackAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallbackAuth")
            .field("kind", &self.kind)
            .field("token", &"<redacted>")
            .finish()
    }
}
```

Blocks the token from leaking through any `tracing::warn!("... {:?}", req)` or chronicle metadata that serializes the full request body. Test: `debug_format_redacts_token`.

Additionally, on POST failure, `err_msg = format!("{:?}", err)` must never contain the Authorization header contents. `reqwest::Error::Display` doesn't echo request headers by default; document this invariant in the POST helper so a future crate upgrade doesn't regress it. Test: `error_metadata_does_not_leak_token` — submit a failing POST and grep the stored `last_error` for the literal token string, assert absent.

### Token validation

Wire mints 32-byte base64url tokens (contract §1.7). On read from DB before building the Authorization header, validate:

```rust
fn is_valid_bearer(t: &str) -> bool {
    !t.is_empty()
        && t.len() < 512  // sanity cap
        && t.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '='))
}
```

Anything else is terminal-fail with `"callback_auth_token malformed"`. Defense in depth against header-injection if Wire ever ships a buggy token. Test: `control_char_token_terminal_fails`.

---

## Envelope adapter (MarketAsyncResult → CallbackEnvelope)

**Input shape pinned:** what's persisted in `fleet_result_outbox.result_json` is a bare `MarketAsyncResult` (tagged-enum `{"kind":"Success","data":{...}}` or `{"kind":"Error","data":"..."}`). NOT a `MarketAsyncResultEnvelope` (which wraps it with `{job_id, outcome}`). Verified by reading `server.rs:3957-3967` (worker success writer) and `db.rs:synthesize_worker_error_json` (sweep-synth writer). Both write bare `MarketAsyncResult`.

**Adapter responsibility:** parse `row.result_json` as `MarketAsyncResult`, then synthesize the Wire `CallbackEnvelope` at POST time using `row.job_id` (guaranteed UUID — see Pillar 14 / contract §10.5). The envelope field `body.job_id` is ALWAYS sourced from `row.job_id`, never from any persisted/passed-through payload.

**Signature:**

```rust
fn build_callback_envelope(
    row: &OutboxRow,
    result: &MarketAsyncResult,
) -> Result<CallbackEnvelope, AdapterError> { ... }
```

**Pre-POST invariant (compile-guarded):**

```rust
debug_assert!(
    uuid::Uuid::parse_str(&row.job_id).is_ok(),
    "OutboxRow.job_id must be UUID-format (contract §10.5); handle-path lives in callback_url"
);
```

Test: `envelope_job_id_is_uuid_not_handle_path` — seed a row, inspect POSTed body, assert `uuid::Uuid::parse_str(body.job_id).is_ok()`.

Contract §2.3 (verified against `src/app/api/v1/compute/callback/[job_id]/route.ts:34-57`):

```json
// Success
{ "type": "success", "job_id": "<uuid>",
  "result": { "content": "...", "input_tokens": N, "output_tokens": N,
              "model_used": "...", "latency_ms": N, "finish_reason"?: "..." } }
// Failure
{ "type": "failure", "job_id": "<uuid>",
  "error": { "code": "...", "message": "..." } }
```

Node's internal `MarketAsyncResult` / `MarketDispatchResponse` types stay unchanged. A dedicated Wire-facing POST struct `CallbackEnvelope` in `market_delivery.rs` is constructed via a pure-function adapter:

```rust
fn build_callback_envelope(
    row: &OutboxRow,
    envelope: &MarketAsyncResultEnvelope,
) -> Result<CallbackEnvelope, AdapterError> { ... }
```

**Top-level envelope mapping:**

| Wire field | Source |
|---|---|
| `type` | Literal `"success"` when `MarketAsyncResult::Success`, `"failure"` when `MarketAsyncResult::Error`. |
| `job_id` | `row.job_id` (UUID, never the handle-path from callback_url). |
| `result` / `error` | Built from the inner `MarketDispatchResponse` / error string per sub-mappings below. |

**Success field mapping** (`result.*` — verified against Wire's `isIntNonNeg` validator in `src/app/api/v1/compute/callback/[job_id]/route.ts:80-103`):

| Wire field | Source | Fallback / edge case |
|---|---|---|
| `content` | `MarketDispatchResponse.content` | If empty string, still emit — Wire rejects only if `content` is missing entirely. |
| `input_tokens` | `MarketDispatchResponse.prompt_tokens: Option<i64>` | **None → 0** (Wire rejects null). |
| `output_tokens` | `MarketDispatchResponse.completion_tokens: Option<i64>` | **None → 0**. |
| `model_used` | `MarketDispatchResponse.provider_model.as_deref().filter(non_empty).or(model.as_deref()).unwrap_or("unknown")` | Practical: worker always sets `model` non-empty (from dispatch body), so this effectively passes through. Last-resort literal `"unknown"` avoids `AdapterError::ModelUsedEmpty` terminal-fail for a field Wire treats as observability not correctness. |
| `latency_ms` | `outbox_row.inference_latency_ms: Option<i64>` | **None → 0** + chronicle metadata `latency_ms_source: "sweep_synth"` (loud default per `feedback_loud_deferrals`; see below for the extended taxonomy). |
| `finish_reason` | `MarketDispatchResponse.finish_reason: Option<String>` | Passthrough. Omit from JSON if None (Wire accepts absent). |

**Failure field mapping:**

| Wire field | Source |
|---|---|
| `code` | `classify_failure_code(&error_message)` — small switch (see below). |
| `message` | `MarketAsyncResult::Error(String)` as-is, truncated to 1024 chars. |

**Failure classifier** — short helper that pattern-matches on the error string and emits a richer code than rev 0.1's flat `"provider_error"`:

```rust
fn classify_failure_code(msg: &str) -> &'static str {
    let lower = msg.to_ascii_lowercase();
    if lower.contains("worker heartbeat")           { return "worker_heartbeat_lost"; }
    if lower.contains("timeout") || lower.contains("timed out") { return "model_timeout"; }
    if lower.contains("out of memory") || lower.contains("oom") { return "oom"; }
    if lower.contains("messages") && lower.contains("invalid") { return "invalid_messages"; }
    // Default: Wire's taxonomy accepts free-form strings today but
    // conventionally "model_error" is the default-failure bucket per
    // Wire's mapFailureCodeToReason switch.
    "model_error"
}
```

The codes align with Wire's `mapFailureCodeToReason` switch at `src/app/api/v1/compute/callback/[job_id]/route.ts` as of rev 1.5. If Wire extends the switch, we extend the classifier. Classifier bug → wrong code → Wire still accepts (it defaults everything unknown to `execution_error` reason anyway), so this is a telemetry quality concern, not a correctness one.

---

## Pre-existing bug fix in `spawn_market_worker` failure branch

**Current behavior** (broken): on LLM call failure, `spawn_market_worker` calls `fleet_outbox_bump_delivery_attempt` — which CAS'es on `status='ready'`. The row is still `status='pending'` at that point. CAS returns 0 rows, silently. The `last_error` is never written, `delivery_attempts` is never bumped, and the row sits as `pending` until the sweep's heartbeat-lost path synthesizes a generic "worker heartbeat lost — market sweep promoted" message — losing the actual inference error.

**Fix:** on failure, serialize the `MarketAsyncResult::Error(<message>)` envelope and call `fleet_outbox_promote_ready_if_pending` (same helper as the success path, different payload). Then fire `delivery_nudge`. After the promote, the row is `ready` with the real inference error, the delivery worker picks it up, constructs a `failure` envelope with the classified code, and POSTs it.

Code change is narrow — mirror the success branch with a different `result_json`. ~20 lines in `spawn_market_worker`.

**Test:** `inference_failure_promotes_with_error_envelope` — mock LLM failure, assert row becomes `ready` (not sweep-promoted), assert `result_json` deserializes to `MarketAsyncResultEnvelope::Error("the actual message")`.

---

## Loop body (pseudocode)

```rust
async fn delivery_loop(ctx: &DeliveryContext, rx: &mut UnboundedReceiver<()>) {
    // Read policy once for interval setup; re-read inside deliver_one for
    // per-POST fields so operator supersession takes effect on the next
    // iteration.
    let policy_snap = ctx.policy.read().await.clone();
    let mut interval = tokio::time::interval(Duration::from_secs(policy_snap.outbox_sweep_interval_secs.max(1)));

    // Fire one tick on entry so post-restart state isn't stalled waiting
    // for the first natural tick.
    boot_push(ctx).await;

    loop {
        tokio::select! {
            Some(()) = rx.recv() => { while rx.try_recv().is_ok() {} }
            _ = interval.tick() => {}
        }

        // Re-read policy fresh each tick (ConfigSynced listener writes through
        // the RwLock; see §Integration points). Clone once for the whole batch.
        let p = ctx.policy.read().await.clone();
        let claimed = claim_ready_market_rows(&ctx.db_path, &p).await;

        // Bounded-parallel POSTs. max_concurrent_deliveries caps the in-flight
        // futures; rows beyond that queue in the stream but don't block the
        // next tick's claim.
        use futures_util::stream::{iter, StreamExt};
        iter(claimed)
            .for_each_concurrent(Some(p.max_concurrent_deliveries as usize), |row| {
                deliver_one(ctx, row)
            })
            .await;
    }
}

async fn deliver_one(ctx: &DeliveryContext, row: OutboxRow) {
    // Read policy once at the top of deliver_one; drop the guard immediately
    // via `.clone()`. Holding a RwLock read guard across a POST `.await`
    // would block config-supersession writers for the duration of a 30s POST
    // and could surface as a Send-error on cargo check of the Tauri binary
    // (some tokio::sync::RwLockReadGuard-future composition patterns are
    // not Send). Clone-and-drop is the safe pattern.
    let p = ctx.policy.read().await.clone();

    // 1. Result parse. The outbox stores bare MarketAsyncResult (not an
    //    envelope). Malformed = terminal (code bug, no retry helps).
    //    This shape is contract-pinned by both the success writer
    //    (server.rs::spawn_market_worker) and the sweep-synth writer
    //    (db::synthesize_worker_error_json). Never accept an envelope
    //    shape here — it would indicate an upstream write-path bug.
    let async_result: MarketAsyncResult = match serde_json::from_str(&row.result_json) {
        Ok(r) => r,
        Err(e) => {
            let err = truncate(&format!("result_json parse: {e}"), p.max_error_message_chars);
            mark_failed_with_error_cas(&ctx.db_path, &row, &err).await;
            emit("market_result_delivery_failed", &row, Some("envelope_parse_failed")).await;
            return;
        }
    };

    // 2. Bearer extract + validation. A missing token on a row that predates
    //    the migration apply-time is a one-shot deploy artifact; distinguish
    //    from token-plumbing bugs on post-migration rows.
    let bearer = match row.callback_auth_token.as_deref() {
        Some(t) if is_valid_bearer(t) => t,
        None if row_predates_migration(&row, &ctx.db_path).await => {
            mark_failed_with_error_cas(&ctx.db_path, &row, "orphaned by migration").await;
            emit("market_result_delivery_failed", &row, Some("orphaned_by_migration")).await;
            return;
        }
        _ => {
            mark_failed_with_error_cas(&ctx.db_path, &row, "callback_auth_token missing or malformed").await;
            emit("market_result_delivery_failed", &row, Some("callback_auth_token_invalid")).await;
            return;
        }
    };

    // 3. Envelope adapter. `build_callback_envelope` synthesizes the Wire-facing
    //    envelope (type + job_id + result|error) from the parsed MarketAsyncResult
    //    and row.job_id. See §Envelope adapter for field mapping.
    let wire_envelope = build_callback_envelope(&row, &async_result)
        .expect("model_used fallback-to-\"unknown\" means adapter is infallible for today's shapes");

    // 4. POST. Reqwest client is configured once at module level with
    //    `redirect(Policy::none())` so a hostile DNS / intermediary can't
    //    cross-origin-leak the bearer. Token never enters a log format string
    //    — Authorization header is built inline via `.header("Authorization",
    //    format!("Bearer {bearer}"))` and never captured into any tracing
    //    macro. Response body bounded to 64 KiB (we only need status +
    //    optional reason headers; the body is small JSON).
    let post_started = std::time::Instant::now();
    let post_result = post_envelope(
        &row.callback_url, bearer, &wire_envelope,
        p.callback_post_timeout_secs,
    ).await;
    let post_duration_ms = post_started.elapsed().as_millis() as i64;

    // 5. Branch on outcome. All error strings go through `truncate(&err, p.max_error_message_chars)`
    //    BEFORE DB write or chronicle emit to enforce the policy bound.
    match post_result {
        Ok((status, headers)) if status.is_success() => {
            let cas_rows = mark_delivered_if_ready_cas(&ctx.db_path, &row).await;
            if cas_rows == 1 {
                emit_with_metadata("market_result_delivered_to_wire", &row,
                    json!({ "attempts": row.delivery_attempts + 1,
                            "latency_ms": row.inference_latency_ms.unwrap_or(0),
                            "latency_ms_source": if row.inference_latency_ms.is_some() { "inference" } else { "sweep_synth" },
                            "duration_ms": post_duration_ms })).await;
            } else {
                // CAS lost — row was transitioned away from 'ready' by a
                // concurrent sweep. Wire got the 2xx (idempotent handler);
                // our observability must record the delivery still happened.
                emit_with_metadata("market_result_delivery_cas_lost", &row,
                    json!({ "attempts": row.delivery_attempts + 1,
                            "reason": "sweep_raced_to_failed",
                            "duration_ms": post_duration_ms })).await;
            }
        }
        Ok((status, _headers)) if TERMINAL_HTTP_CODES.contains(&status.as_u16()) => {
            let code = status.as_u16();
            // Discriminator for the common secret-expired case — see
            // §Runtime 401-likely-secret-expired detection above.
            let reason = if code == 401 && row_older_than_retention(&row, p.ready_retention_secs) {
                "terminal_http_401_likely_secret_expired"
            } else {
                match code {
                    400 => "terminal_http_400", 401 => "terminal_http_401",
                    403 => "terminal_http_403", 404 => "terminal_http_404",
                    409 => "terminal_http_409", 410 => "terminal_http_410",
                    413 => "terminal_http_413", _ => "terminal_http_other",
                }
            };
            let err = truncate(&format!("terminal http {code}"), p.max_error_message_chars);
            mark_failed_with_error_cas(&ctx.db_path, &row, &err).await;
            emit_with_metadata("market_result_delivery_failed", &row,
                json!({ "attempts": row.delivery_attempts + 1,
                        "final_error": err,
                        "reason": reason })).await;
        }
        Ok((status, headers)) => {
            // 5xx and other transient — backoff + retry until max_attempts.
            let code = status.as_u16();
            let err = truncate(&format!("http {code}"), p.max_error_message_chars);
            // parse_retry_after_header handles BOTH integer-seconds (RFC 7231 §7.1.3 first form)
            // AND HTTP-date (second form). Returns (None, source) if neither parses.
            let (retry_after, retry_after_source) = parse_retry_after_header(&headers);
            bump_attempt_with_backoff(&ctx.db_path, &row, &err, retry_after, &p).await;
            emit_with_metadata("market_result_delivery_attempt_failed", &row,
                json!({ "attempt": row.delivery_attempts + 1,
                        "error": err,
                        "status_code": code,
                        "retry_after_source": retry_after_source })).await;
        }
        Err(network_err) => {
            let err = truncate(&format!("network: {network_err}"), p.max_error_message_chars);
            bump_attempt_with_backoff(&ctx.db_path, &row, &err, None, &p).await;
            emit_with_metadata("market_result_delivery_attempt_failed", &row,
                json!({ "attempt": row.delivery_attempts + 1,
                        "error": err,
                        "retry_after_source": "computed_backoff" })).await;
        }
    }
}

/// Retry classification is PROTOCOL DATA now, not hardcoded-HTTP-enum magic.
///
/// Wire ships `X-Wire-Retry: never | transient | backoff` on every non-2xx
/// response. The node reads that header first; `X-Wire-Retry: never` →
/// terminal regardless of HTTP code. `X-Wire-Retry: transient` → retry
/// up to max_attempts. `X-Wire-Retry: backoff` → retry honoring
/// Retry-After (which carries the delay separately per existing
/// X-Wire-Reason pattern).
///
/// The const below is the FALLBACK enumeration for requests where the
/// header is absent (pre-upgrade Wire, or a proxy stripped it). Explicit
/// header is authoritative when present.
const TERMINAL_HTTP_CODES_FALLBACK: &[u16] = &[400, 401, 403, 404, 409, 410, 413];

enum WireRetryIntent { Never, Transient, Backoff, Unknown }

fn parse_wire_retry_header(headers: &reqwest::header::HeaderMap) -> WireRetryIntent {
    match headers.get("X-Wire-Retry").and_then(|v| v.to_str().ok()) {
        Some("never") => WireRetryIntent::Never,
        Some("transient") => WireRetryIntent::Transient,
        Some("backoff") => WireRetryIntent::Backoff,
        None => WireRetryIntent::Unknown,
        Some(other) => {
            // Forward-compat: unknown value warn-don't-reject.
            tracing::warn!(header_value = %other, "X-Wire-Retry: unknown value, falling back to HTTP-code enumeration");
            WireRetryIntent::Unknown
        }
    }
}

/// Final retry decision combines explicit + fallback in precedence order.
fn classify_retry(status: reqwest::StatusCode, headers: &reqwest::header::HeaderMap) -> RetryDecision {
    match parse_wire_retry_header(headers) {
        WireRetryIntent::Never => RetryDecision::Terminal { source: "x_wire_retry_never" },
        WireRetryIntent::Transient => RetryDecision::Retry { source: "x_wire_retry_transient" },
        WireRetryIntent::Backoff => RetryDecision::Retry { source: "x_wire_retry_backoff" },
        WireRetryIntent::Unknown if TERMINAL_HTTP_CODES_FALLBACK.contains(&status.as_u16()) => {
            RetryDecision::Terminal { source: "http_code_fallback" }
        }
        WireRetryIntent::Unknown => RetryDecision::Retry { source: "http_code_fallback" },
    }
}

async fn bump_attempt_with_backoff(
    db_path: &Path,
    row: &OutboxRow,
    err: &str,
    retry_after_secs: Option<u64>,
    policy: Arc<RwLock<MarketDeliveryPolicy>>,
) {
    let p = policy.read().await.clone();
    let new_attempts = row.delivery_attempts + 1;
    let backoff_secs = if let Some(ra) = retry_after_secs {
        ra  // Respect Wire's Retry-After header when present.
    } else {
        p.backoff_base_secs.saturating_mul(1u64 << new_attempts.min(6) as u32)
            .min(p.backoff_cap_secs)
    };

    // CAS-guarded bump; also clears lease so next claim cycle can pick it up
    // after the backoff window.
    let next_at = now_plus_secs(backoff_secs);
    let _ = sqlite_exec(db_path, |conn| {
        conn.execute(
            "UPDATE fleet_result_outbox
             SET delivery_attempts = ?1, last_error = ?2,
                 delivery_lease_until = NULL,
                 delivery_next_attempt_at = ?3
             WHERE dispatcher_node_id = ? AND job_id = ?
               AND status = 'ready'",
            params![new_attempts, err, next_at, row.dispatcher_node_id, row.job_id],
        )
    }).await;

    // Terminal if exceeded — transition ready→failed via CAS (not unconditional write).
    if new_attempts >= p.max_delivery_attempts {
        mark_failed_with_error_cas(db_path, row, "max_delivery_attempts exceeded").await;
        emit("compute_result_delivery_failed", row, Some("max_attempts")).await;
    }
}
```

All terminal writes (`mark_failed_with_error_cas`) are `WHERE status='ready'` — matches the fleet pattern. Rowcount=0 is logged, not an error.

---

## Chronicle events

**No name collision with Wire's chronicle.** Wire emits `compute_result_delivered` for the Wire→requester hop (`src/app/api/v1/wire/maintenance/tick/route.ts:319`). The node emits for the node→Wire hop. Same event_type name → dashboard double-count.

Fix: the node-side emissions use a distinct prefix. Since the node chronicle (`pyramid_compute_events`) has no CHECK constraint, we have free naming — but staying close to the protocol words keeps the taxonomy learnable.

**Naming convention:** node-side provider-path events use `market_*` prefix per the existing `compute_chronicle.rs` taxonomy (`EVENT_MARKET_OFFERED`, `EVENT_MARKET_RECEIVED`, `EVENT_QUEUE_MIRROR_PUSH_FAILED`, etc.). Avoids collision with Wire-side `compute_result_delivered` (which fires on Wire→requester hop, semantically different event despite the overlap in words).

| Node event | Fires when | Metadata |
|---|---|---|
| `market_result_delivered_to_wire` | 2xx + CAS win | `job_id`, `request_id`, `attempts`, `latency_ms`, `latency_ms_source` (`inference` or `sweep_synth`), `duration_ms` (POST latency) |
| `market_result_delivery_cas_lost` | 2xx + CAS=0 rows | `job_id`, `request_id`, `attempts`, `reason: "sweep_raced_to_failed"`, `duration_ms` |
| `market_result_delivery_attempt_failed` | Transient failure, still retrying | `job_id`, `request_id`, `attempt`, `error`, `status_code`, `next_attempt_at`, `retry_after_source` (`header_seconds` / `header_http_date` / `computed_backoff`) |
| `market_result_delivery_failed` | Terminal failure | `job_id`, `request_id`, `attempts`, `final_error`, `reason` (one of: `envelope_parse_failed`, `callback_auth_token_invalid`, `callback_url_validation_failed`, `terminal_http_400/401/403/404/409/410/413`, `terminal_http_401_likely_secret_expired`, `max_attempts`, `orphaned_by_migration`) |
| `market_delivery_task_panicked` | Supervisor caught panic | `message`, `backoff_secs` |
| `market_delivery_task_exited` | Loop exited (channel closed) | `reason` |

Dropped from rev 0.3: `compute_delivery_policy_invariant_violated` — no longer fires because the boot-time invariant was unimplementable (see §Runtime 401-likely-secret-expired detection).

**Bilateral item:** Wire uses `compute_result_delivered` for its Wire→requester hop. Following this spec's naming, the two sides don't collide. But cross-side aggregation queries (`SELECT event_type, node_id FROM wire_chronicle + pyramid_compute_events`) still need to know the semantic split. Packaged for Wire owner separately; not a build-blocker for this spec.

---

## Policy fields

**Two new fields** per Pillar 37 spirit (operator tunability where meaningful) and Pillar 44 spirit (bounded parallelism). Existing fields unchanged.

| Field | Default | Role | New? |
|---|---|---|---|
| `callback_post_timeout_secs` | 30 | POST timeout | existing |
| `outbox_sweep_interval_secs` | 15 | Periodic tick cadence | existing |
| `backoff_base_secs` | 1 | First retry delay | existing |
| `backoff_cap_secs` | 64 | Max retry delay | existing |
| `max_delivery_attempts` | 20 | Terminal-failure threshold | existing |
| `delivered_retention_secs` | 3600 | Retention after successful delivery | existing |
| `failed_retention_secs` | 604800 | Retention for post-mortem | existing |
| `lease_grace_secs` | 5 | Added to `callback_post_timeout_secs` to form total lease duration. Operator-tunable because the right grace depends on network jitter characteristics. | **NEW** |
| `max_concurrent_deliveries` | 4 | Max concurrent POSTs per tick. Bounded fan-in per Pillar 44 spirit. | **NEW** |
| `max_error_message_chars` | 1024 | Truncation cap for `last_error` + chronicle metadata. Operator-tunable because chronicle verbosity is an operational concern. | **NEW** |

**Hardcoded constants (deliberately NOT policy fields)** and the rationale:

| Constant | Value | Why not tunable |
|---|---|---|
| `MAX_TOKEN_LEN` | 512 | DoS guard; base64url-encoded 32-byte tokens are ~43 chars. Anything approaching 512 is a contract violation, not an operational knob. |
| `TERMINAL_HTTP_CODES` | `[400, 401, 403, 404, 410]` | HTTP protocol semantics; tuning would violate the protocol. |
| Retry shift cap | `1u64 << new_attempts.min(6)` | Arithmetic safety (prevents u64 overflow on pathological attempts counter); max effective shift = 64 = backoff_cap_secs anyway. |

**Wire-parameters-aware secret-expiry detection** (replaces rev 0.3's unimplementable boot-time invariant check AND rev 0.4's runtime-age heuristic, now that Wire ships a keyed `wire_parameters` block on every heartbeat response):

Wire mints callback secrets with `expires_at = now + fill_job_ttl_secs + callback_secret_grace_secs` (both economic_parameters, no hardcoded 300). The node's heartbeat loop now writes these values into `AuthState.wire_parameters: HashMap<String, serde_json::Value>` on each successful heartbeat response (see §Wire-parameters consumption below).

**Boot + per-delivery invariant:** at policy load time AND before any POST, compute the actual secret window:

```rust
let secret_window_secs: i64 = auth.wire_parameters
    .get("fill_job_ttl_secs").and_then(|v| v.as_i64())
    .unwrap_or(1800)  // Pre-wire_parameters fallback: contract-default.
  + auth.wire_parameters
    .get("callback_secret_grace_secs").and_then(|v| v.as_i64())
    .unwrap_or(300);  // Pre-wire_parameters fallback: contract-default.

let node_max_window_secs: i64 = policy.ready_retention_secs as i64
    + cumulative_backoff_cap(&policy);  // sum of 1,2,4,8,...,backoff_cap across max_delivery_attempts

if node_max_window_secs > secret_window_secs {
    emit("market_delivery_secret_window_invariant_violated", json!({
        "node_max_window_secs": node_max_window_secs,
        "secret_window_secs": secret_window_secs,
        "source": if auth.wire_parameters.contains_key("fill_job_ttl_secs") { "wire_heartbeat" } else { "fallback_defaults" },
    }));
}
```

**Per-POST classification** uses the observed-at-runtime signal too. When a 401 arrives:

```rust
let row_age_secs = now.signed_duration_since(row.created_at).num_seconds();
let reason = if row_age_secs > secret_window_secs {
    // Secret definitely expired. Canonical reason.
    "terminal_http_401_secret_expired"
} else if row_age_secs > policy.ready_retention_secs as i64 {
    // Plausibly secret-expired based on local retention, but within Wire's
    // window. Soft classification.
    "terminal_http_401_likely_secret_expired"
} else {
    // Fresh row + 401 = genuine auth bug.
    "terminal_http_401"
};
```

**Fallback behavior:** if `wire_parameters` isn't populated yet (pre-Wire-upgrade, or first-boot before first heartbeat), both branches degrade to the rev 0.4 runtime-age heuristic using `ready_retention_secs` as the upper bound. Invariant check emits with `source: "fallback_defaults"` to make the degraded mode visible.

---

## Integration points

### `handle_market_dispatch` (server.rs)

At admission, plumb the bearer token into the outbox row. `market_outbox_insert_or_ignore` gains an `Option<&str>` parameter:

```rust
market_outbox_insert_or_ignore(
    &conn, &req.job_id, &req.callback_url.to_string(),
    "MarketStandard", &expires_str,
    Some(&req.callback_auth.token),  // <-- new; None for fleet call sites
)?;
```

Fleet call sites unchanged (pass `None`).

### `spawn_market_worker` (server.rs)

- **Success path**: after `fleet_outbox_promote_ready_if_pending` returns 1, emit `delivery_nudge.send(()).ok()`. ALSO: pass `inference_latency_ms` into the promote helper so the new column is populated.
- **Failure path (FIX)**: serialize the error envelope, call `fleet_outbox_promote_ready_if_pending` with an Error payload, emit `delivery_nudge.send(()).ok()`. Mirror the success branch with different JSON. ~20 lines.

### `fleet_outbox_sweep.rs` sweep's heartbeat-lost path (line ~568)

After `fleet_outbox_promote_ready_if_pending` (the heartbeat-lost synth), fire `market_ctx.delivery_nudge.send(()).ok()`. Requires threading `delivery_nudge` into the sweep context (small refactor) OR a global accessor. Prefer threading for symmetry with the mirror pattern.

### `MarketDispatchContext` (market_dispatch.rs)

Add one field:

```rust
pub delivery_nudge: tokio::sync::mpsc::UnboundedSender<()>,
```

### ConfigSynced listener for `market_delivery_policy` (main.rs)

**In-scope this commit, not a pre-existing gap to punt on.** rev 0.4 adds three new policy fields (`lease_grace_secs`, `max_concurrent_deliveries`, `max_error_message_chars`). Shipping them without their hot-reload pathway is the same half-shipped pattern as Phase 3 requester-only-without-provider — operator supersession wouldn't take effect until node restart, which invalidates the Pillar 37 tunability claim.

Mirror the fleet pattern at `main.rs:~11889` (existing fleet_delivery_policy ConfigSynced arm):

```rust
// When a market_delivery_policy contribution supersedes, parse + swap.
if contribution_type == "market_delivery_policy" {
    match MarketDeliveryPolicy::from_yaml(&yaml_body) {
        Ok(new_policy) => {
            let mut guard = compute_market_dispatch_shared.policy.write().await;
            *guard = new_policy;
            tracing::info!("market_delivery_policy hot-reloaded");
        }
        Err(e) => tracing::warn!("market_delivery_policy parse failed: {e}"),
    }
}
```

Test: `market_policy_supersession_takes_effect_without_restart` — seed policy, simulate ConfigSynced event with updated YAML, assert `ctx.policy.read().lease_grace_secs` matches new value.

### OutboxRow SELECT-helper enumeration

**Every SELECT helper that materializes `OutboxRow` must project the new columns** or the struct reads default/NULL on rows from those sites. Enumerated explicitly so no site is missed:

| Helper | File | Fix |
|---|---|---|
| `fleet_outbox_sweep_expired` | db.rs:~2820 | Add `created_at, callback_auth_token, delivery_lease_until, delivery_next_attempt_at, inference_latency_ms` to SELECT list |
| `fleet_outbox_retry_candidates` | db.rs (via fleet pattern) | Same |
| `market_outbox_sweep_expired` | db.rs (market mirror of fleet) | Same |
| `market_outbox_retry_candidates` | db.rs (market mirror of fleet) | Same |
| New `claim_ready_market_rows` | db.rs (added this commit) | Uses `RETURNING *` so inherits full projection automatically |

Regression guard test: `outbox_row_projection_matches_struct` — grep-style assertion that each helper's SELECT list includes every non-ephemeral field of `OutboxRow`. Fail loudly if a future column addition forgets a site.

### Callback URL re-validation at delivery time

Contract §2.5 requires callback URLs are re-validated at every delivery attempt (not just admission). Defense-in-depth: prevents a scenario where `fleet_result_outbox.callback_url` is mutated between admission (where we validated) and delivery (where we POST). Cheap and systemic.

```rust
// Before POST, re-validate callback_url structurally (HTTPS + host).
let empty_roster = FleetRoster::empty();
if let Err(e) = validate_callback_url(&row.callback_url, &CallbackKind::MarketStandard, &empty_roster) {
    // Terminal: a structurally-invalid URL never becomes valid.
    let err = truncate(&format!("callback_url re-validation failed: {e}"), p.max_error_message_chars);
    mark_failed_with_error_cas(&ctx.db_path, &row, &err).await;
    emit_with_metadata("market_result_delivery_failed", &row,
        json!({ "attempts": row.delivery_attempts + 1,
                "final_error": err,
                "reason": "callback_url_validation_failed" })).await;
    return;
}
```

### Wire-parameters consumption (generic, not delivery-specific)

Wire now ships a `wire_parameters: object` block on every heartbeat response — a keyed map of economic_parameter values Wire's maintainer considers node-operationally-relevant (`fill_job_ttl_secs`, `callback_secret_grace_secs`, `queue_mirror_staleness_s`, `node_heartbeat_staleness_s`, `compute_job_timeout_s_per_queue_position`, etc.). The key set is itself a contribution on Wire's side — operators can supersede the allow-list.

**Plumbing (generic; every future subsystem that needs Wire's view of its own parameters consumes the same struct):**

1. Extend `AuthState` (auth.rs): add `pub wire_parameters: std::collections::HashMap<String, serde_json::Value>` default-empty.
2. Heartbeat response handler (main.rs:~13340): after existing `jwt_public_key` self-heal, read `response.get("wire_parameters")`; if present, deserialize into the HashMap, lock-swap `AuthState.wire_parameters` under the existing auth RwLock, persist to `session.json`.
3. Invariant: non-present key → use contract-default fallback constants. The defaults live in a single `wire_parameter_defaults()` helper so future constants don't proliferate.

The delivery worker is this commit's first consumer. Future phases (market-surface filter on node side; match-result caching; etc.) hit the same struct.

**Migration note:** `wire_parameters` on heartbeat response is purely additive; nodes pre-upgrade ignore the field and degrade to fallback defaults. No lockstep required with the Wire ship.

**Chronicle event on wire_parameters change:** `market_wire_parameters_updated` fires when a heartbeat response changes any value (including first-population from empty). Metadata: the diff (before/after per key). Operator sees when Wire supersedes a parameter relevant to them.

### Trace correlation via `request_id`

rev 0.4 persists Wire's `extensions.request_id` (when present) into a new outbox column `request_id TEXT` at admission, and emits it in every chronicle event metadata (`market_result_delivered_to_wire`, `market_result_delivery_attempt_failed`, `market_result_delivery_failed`, `market_result_delivery_cas_lost`). Operator bug-reports referencing a Wire request_id can be resolved to the node-side chronicle in one query.

Schema add in the same migration:

```sql
ALTER TABLE fleet_result_outbox ADD COLUMN request_id TEXT;  -- Wire extensions.request_id at admission time.
```

Admission writes `row.request_id = req.extensions.get("request_id").and_then(|v| v.as_str()).map(String::from)`.

### `main.rs` startup wiring

**Exact sequence** (no hand-waving — mirror the existing mirror task setup exactly):

```rust
// 1. Create the delivery nudge channel BEFORE MarketDispatchContext.
let (market_delivery_nudge_tx, market_delivery_nudge_rx) =
    tokio::sync::mpsc::unbounded_channel::<()>();

// 2. Construct the dispatch context with BOTH senders.
let compute_market_dispatch_shared = Arc::new(
    MarketDispatchContext {
        tunnel_state: shared_tunnel.clone(),
        pending: Arc::new(PendingMarketJobs::new()),
        policy: Arc::new(RwLock::new(market_delivery_policy_init)),
        mirror_nudge: market_mirror_nudge_tx,
        delivery_nudge: market_delivery_nudge_tx,  // <-- new
    }
);

// 3. AppState construction as today.
let state = Arc::new(AppState { ... });

// 4. AFTER AppState: run one-shot startup recovery (clears stale leases).
market_delivery_startup_recovery(&pyramid_db_path).await;

// 5. Spawn the existing mirror + expiry tasks (unchanged).
spawn_market_mirror_task(...);
spawn_market_outbox_sweep_loop(...);

// 6. NEW: spawn the delivery supervisor. Takes the receiver by value.
let delivery_ctx = DeliveryContext {
    db_path: pyramid_db_path.to_path_buf(),
    policy: compute_market_dispatch_shared.policy.clone(),
};
tokio::spawn(supervise_delivery_loop(delivery_ctx, market_delivery_nudge_rx));

// 7. Spawn HTTP server (can now fire delivery_nudge via market_ctx).
```

Order matters because `server.rs` handlers eventually hold `state.compute_market_dispatch.delivery_nudge.clone()` and will panic-on-send if the receiver was dropped before the server started. Receiver is owned by the supervisor task (spawned before server) so it stays alive for the process lifetime.

---

## Testing

**13 tests** — adds 6 new tests over rev 0.1 to cover the audit findings:

1. `happy_path_delivery` — insert ready row, mock 2xx POST, verify `compute_result_delivered_to_wire` chronicle + state=delivered.
2. `retry_on_5xx_bumps_attempts_not_delivered` — insert ready, mock 503, verify `delivery_attempts=1`, `delivery_next_attempt_at` in future, state still ready.
3. `terminal_on_400_401_403_404_410` — parametric test covering each terminal HTTP code. Verify state→failed, chronicle `compute_result_delivery_failed` with reason=`terminal_http_N`.
4. `max_attempts_terminal` — seed attempts = max - 1, mock 503, verify transitions to failed.
5. `lease_prevents_double_delivery` — two concurrent `claim_ready_market_rows` calls on same row; exactly one wins, second returns empty.
6. `cas_lost_to_sweep_emits_distinct_chronicle` — mid-POST, mock sweep transitioning row ready→failed; verify on 2xx the CAS returns 0 AND `compute_result_delivery_cas_lost` fires.
7. `backoff_schedule_matches_spec` — seed row at attempts=3, mock 503, assert `delivery_next_attempt_at ≈ now + min(1 × 2^3, 64) = 8s`.
8. `restart_recovery_clears_stale_leases` — seed row with `delivery_lease_until = now - 1s`, call startup recovery, verify lease cleared. Seed fresh process (no startup recovery), verify a tick can claim.
9. `envelope_adapter_none_tokens_become_zero` — MarketDispatchResponse with `prompt_tokens: None, completion_tokens: None` → CallbackEnvelope emits `input_tokens: 0, output_tokens: 0`.
10. `envelope_adapter_model_used_fallback` — `provider_model: None` + `model: "gemma4:26b"` → `model_used: "gemma4:26b"`. Both empty/null → terminal fail.
11. `envelope_adapter_failure_classifier` — error message "worker heartbeat lost" → `code: worker_heartbeat_lost`. "timed out" → `model_timeout`. "OOM" → `oom`. Default → `model_error`.
12. `debug_format_redacts_token` — Debug-format a `CallbackAuth`, grep output for `<redacted>` + assert absence of literal token.
13. `error_metadata_does_not_leak_token` — mock POST returning 401, inspect stored `last_error` in DB + `compute_result_delivery_failed` chronicle metadata, assert absence of literal token string.
14. `panic_survivor` — deliberately panic inside POST path, supervisor respawns, second delivery attempt succeeds.
15. `inference_failure_promotes_with_error_envelope` — the pre-existing-bug fix. Mock LLM failure in worker → row becomes `ready` (not sweep-synth), `result_json` deserializes to `MarketAsyncResultEnvelope::Error("the actual message")`.
16. `sweep_synth_error_envelope_is_deliverable` — insert a sweep-synthesized `pending→ready` row, verify delivery worker can parse + POST it.

Mock HTTP via `mockito` or an ephemeral warp server on localhost:0 — same pattern as existing tests in `fleet_outbox_sweep.rs`.

---

## Build order

1. **Verify envelope format** — DONE (read Wire's `/callback/[job_id]/route.ts`). §2.3 shape is canonical; node adapts via `CallbackEnvelope` struct.
2. Add new columns to `fleet_result_outbox` + idempotent PRAGMA-guarded ALTER in `db::init_pyramid_db`. Record migration apply-time in `pyramid_schema_version`. Verify with a fresh DB init + an existing-DB migration.
3. Add `callback_auth_token` param to `market_outbox_insert_or_ignore` (+ update all call sites, including fleet sites passing `None`).
4. Add `lease_grace_secs`, `max_concurrent_deliveries`, `max_error_message_chars` fields to `MarketDeliveryPolicy` + seed YAML + default-matches-seed test.
5. Add `delivery_nudge` field to `MarketDispatchContext`.
6. Add `market_delivery_startup_recovery` function + boot-time policy invariant check.
7. Write `market_delivery.rs`: `supervise_delivery_loop`, `delivery_loop`, `claim_ready_market_rows` (batched with `max_concurrent_deliveries`), `mark_delivered_if_ready_cas`, `mark_failed_with_error_cas`, `bump_attempt_with_backoff`, `build_callback_envelope`, `classify_failure_code`, `is_valid_bearer`, `post_envelope`, `parse_retry_after_header`, `row_predates_migration`.
8. Add custom `Debug` for `CallbackAuth` (redact `token`).
9. Add node-side chronicle constants (`EVENT_COMPUTE_RESULT_DELIVERED_TO_WIRE`, etc.) to `compute_chronicle.rs`.
10. Wire into `handle_market_dispatch` (outbox insert with token).
11. Wire into `spawn_market_worker` — BOTH success path (nudge with inference_latency_ms) AND failure path (FIX: promote pending→ready with error envelope, then nudge).
12. Wire into `market_outbox_sweep_loop` heartbeat-lost path — nudge after sweep synth.
13. Spawn `supervise_delivery_loop` in `main.rs` per the exact sequence above. Bounded parallelism via `for_each_concurrent(max_concurrent_deliveries)`.
14. Add new IPC `compute_market_get_outbox_rows` (redacts `callback_auth_token`) for frontend Jobs panel.
15. Frontend: add `DeliveryHealth` indicator to `ComputeOfferManager.tsx` + per-job delivery status column to `ComputeMarketDashboard.tsx`. Extend chronicle event types the components consume.
16. Write the 16+ tests (core backend) + `retry_after_both_forms_parse` + `debug_format_redacts_token` + `error_metadata_does_not_leak_token`.
17. Run `cargo check` (**default target** — `--lib` alone misses binary Send errors per `feedback_cargo_check_lib_insufficient_for_binary.md`) + `cargo test --lib market_delivery` + `tsc --noEmit` for frontend.
18. Commit + push.
19. BEHEM rebuilds.
20. Fire smoke (raw `compute-market-call` first, then `pyramid_build`).
21. Visual smoke in dev mode — verify `DeliveryHealth` badge transitions correctly through a live roundtrip.
22. **Serial verifier pass** (Pillar 39 + `feedback_serial_verifier` + `feedback_wanderer_after_verifier`). After the cross-repo ship (node-side delivery worker + Wire's heartbeat `wire_parameters` + Wire's `X-Wire-Retry` header + Wire's `callback_secret_grace_secs` contribution + Wire's `/ops/compute` delivery-state surface): fresh-eyes auditor reviews for drift between CTE ↔ RPC ↔ shared-types enum ↔ ops UI ↔ node chronicle. Catches the class of "both sides shipped but one side's symbol got renamed" bug. This is also the point at which `contract §2.3 error.code` enum pinning gets cross-verified (contract doc ↔ shared-types ↔ node classifier ↔ Wire's mapFailureCodeToReason).
23. If the serial verifier finds drift, fix in-place and re-verify. Per `feedback_audit_until_clean`.

**Estimate, range per `feedback_estimate_ranges.md`:** best case 8-10h, realistic 16-24h (rev 0.3 added frontend + retry-after parsing + policy invariants + IPC + 3 more tests). Worst case if CAS-race tests surface issues or frontend contract consumption needs iteration: 30-40h.

---

## Frontend / UX (Pillar 42 — always include frontend)

Per Pillar 42 and `feedback_always_scope_frontend.md`, every backend feature needs a corresponding UI surface. Delivery is invisible to the operator without UI. Two additions:

### 1. `DeliveryHealth` indicator on `ComputeOfferManager`

Parallel to the `MirrorHealth` badge shipped in commit `57b1fa4` in the offer manager pane. One compact status line at the top summarizing delivery worker state:

| State | Label | Color |
|---|---|---|
| No delivered jobs yet | "Delivery: no jobs served" | gray |
| Last delivery < 60s ago | "Last delivered Ns ago" | green |
| Last delivery < 5min ago | "Last delivered Nm ago" | green (softer) |
| Last delivery ≥ 5min ago AND ready rows in outbox | "N jobs waiting to deliver" (yellow) or "N deliveries failing" (red) | yellow/red |
| Task panicked event in last hour | "Delivery task panicked (supervisor respawned)" | red |
| Task exited event | "Delivery task exited — restart node" | red |
| >0 ready rows older than 5× outbox_sweep_interval_secs | "N jobs overdue for delivery" | red |

Fetches node chronicle events (`get_compute_events`) for `compute_result_delivered_to_wire`, `compute_result_delivery_failed`, `compute_delivery_task_*`, and queries the outbox (via a new small IPC `compute_outbox_stats`) for counts by state. 15s refresh. Fails gracefully on read errors (renders "—" rather than an error banner).

Component lives in `src/components/market/ComputeOfferManager.tsx` alongside `MirrorHealth`, or extracted to a shared `src/components/market/OutboxHealth.tsx` if it grows. Start inline to minimize sprawl.

### 2. Per-job delivery status column in the Jobs panel

The existing `ComputeMarketDashboard` renders a recent-jobs list. Extend it with a `delivery` column showing:

- `✓ delivered` (green) — terminal success
- `… posting` (blue, animated) — lease in flight (`delivery_lease_until > now`)
- `↻ retry N/20` (yellow) — retrying, with attempt counter
- `✗ failed: <reason>` (red) — terminal failure, reason from chronicle metadata (envelope_parse_failed | callback_auth_token_invalid | adapter_model_used_empty | terminal_http_400/401/403/404/410 | max_attempts | orphaned_by_migration)

Tooltip on hover: shows the last POST attempt duration + latest error from `last_error` (truncated to `max_error_message_chars`). Click to filter the events pane to that `job_id`.

Backend IPC: extend `compute_get_state` (or add `compute_market_get_outbox_rows`) to return outbox row snapshots for active market jobs. Never exposes `callback_auth_token` in the IPC response (redacted server-side).

### Tester-facing framing (per `project_compute_market_purpose_brief`)

For the GPU-less tester, the UI surfaces this as **network activity they caused**, not delivery plumbing. Already covered by `ComputeNetworkStatus` hero's "N pushes received from network" framing — this spec adds the provider-side mirror on the operator path. No new tester-facing surface needed; just the operator-path DeliveryHealth above.

### Frontend tests

`tsc --noEmit` + manual smoke (boot dev mode, toggle serving, fire a smoke dispatch, verify DeliveryHealth transitions green on success, yellow on retry). Chronicle emission contract is tested server-side; frontend tests are contract-consuming.

---

## Bilateral items

1. **Chronicle event naming disambiguation** — Wire uses `compute_result_delivered` for its Wire→requester hop. Node-side uses `compute_result_delivered_to_wire` per this spec for the node→Wire hop. Operator dashboards doing cross-side aggregation must know this. Paste-over TBD; not a build-blocker.

2. **Contract §2.3 `error.code` enumeration** — contract lists `"model_timeout|model_error|..."` with trailing ellipsis. Node's `classify_failure_code` emits `worker_heartbeat_lost`, `model_timeout`, `model_error`, `oom`, `invalid_messages`, `model_error` (default). Wire's `mapFailureCodeToReason` only calls out `model_timeout` and `model_error` today. If Wire expands the switch, node extends the classifier. Worth flagging to Wire owner to pin the enum.

---

## Parse_retry_after_header (Pillar 38: no deferral)

Both RFC 7231 §7.1.3 formats are handled — no MINOR-follow-up.

```rust
fn parse_retry_after_header(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let v = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;

    // Form 1: decimal integer seconds.
    if let Ok(secs) = v.trim().parse::<u64>() {
        return Some(secs);
    }

    // Form 2: HTTP-date. Use `httpdate` crate (already in tree for Tauri)
    // or reqwest's header parsing helpers. Convert to delta-seconds relative
    // to now(); negative/past dates return 0.
    if let Ok(target) = httpdate::parse_http_date(v) {
        let now = std::time::SystemTime::now();
        return Some(target.duration_since(now).map(|d| d.as_secs()).unwrap_or(0));
    }

    // Unparseable — log at warn, fall through to computed backoff.
    tracing::warn!(header_value = %v, "Retry-After header neither seconds nor HTTP-date; ignoring");
    None
}
```

Test `retry_after_both_forms_parse`: assert parsing of `"5"`, `"Wed, 21 Oct 2026 07:28:00 GMT"`, and a malformed value.

---

## Risks / unknowns

1. **`result_json` shape between worker-success and sweep-synth paths.** The worker writes one shape; the sweep's `synthesize_worker_error_json` writes another. Delivery adapter must parse BOTH. Tests 15 and 16 cover this. If the shapes disagree, an MPS fix normalizes both call sites to write the same canonical shape (MarketAsyncResultEnvelope serde form).

2. **SQLite lease query under concurrent tokio tasks.** SQLite WAL mode serializes writes at the connection level; the claim UPDATE's rowcount check is the exclusion primitive. Verify under the mockito test harness with `tokio::join!(claim1, claim2)` — expect exactly one non-zero rowcount. (The `fill_job_ttl_secs` policy mismatch risk is now addressed by the boot-time invariant check — see §Policy fields.)

---

## Audit history

- **rev 0.1 → 0.2**: Stage 1 informed audit (2026-04-20). Two independent informed auditors, 11 CRITICAL/MAJOR findings across both, 11 MINOR/QUESTION findings. Key revisions: split `delivery_lease_until` from `delivery_next_attempt_at` (eliminates column overloading bug); use `callback_kind='MarketStandard'` only in claim filter; enumerate nudge fire sites; add CAS-lost chronicle event; add `spawn_market_worker` failure-branch fix as in-scope (pre-existing bug exposed by the spec); add `inference_latency_ms` column; add startup recovery; add custom `Debug` redaction on `CallbackAuth`; extend terminal-HTTP taxonomy to `[400, 401, 403, 404, 410]`; add 6 new tests; rename chronicle events to avoid Wire-side collision; widen estimate to realistic range.

- **rev 0.2 → 0.3**: Pillars pass (2026-04-20). Checked against `GoodNewsEveryone/docs/wire-pillars.md` + operator feedback memory. Changes:
  - **Pillar 42 violation fixed**: added Frontend / UX section — `DeliveryHealth` indicator on `ComputeOfferManager` + per-job delivery status column on `ComputeMarketDashboard` + new IPC `compute_market_get_outbox_rows` (token-redacted).
  - **Pillar 38 violation fixed**: `parse_retry_after_header` handles BOTH RFC 7231 integer-seconds AND HTTP-date (no MINOR follow-up deferral).
  - **Pillar 37 spirit**: promoted `lease_grace_secs`, `max_concurrent_deliveries`, `max_error_message_chars` to `MarketDeliveryPolicy` fields (operator-tunable). Documented which hardcoded constants deliberately stay non-tunable + why (`MAX_TOKEN_LEN`, `TERMINAL_HTTP_CODES`, shift-overflow guard).
  - **Pillar 44 spirit**: bounded parallelism via `for_each_concurrent(max_concurrent_deliveries)` on the claim batch + batched claim query. Prevents head-of-line blocking under pathological load.
  - **`feedback_loud_deferrals`**: `latency_ms: 0` on sweep-synth rows now emits distinct chronicle metadata `latency_ms_source: "sweep_synth"` so the default is visible, not silent.
  - **`feedback_loud_deferrals`**: pre-migration orphan rows get distinct terminal reason `orphaned_by_migration` via `row_predates_migration` helper. One-shot deploy artifact is visibly distinct from token-plumbing bugs.

- **rev 0.3 → 0.4**: Stage 2 discovery audit + verification sweep (2026-04-20). Two independent discovery auditors + direct source-code verification of every load-bearing claim. Changes:
  - **CRITICAL: envelope shape** — verified at `server.rs:3957-3966` + `db.rs:2916-2922`. Outbox stores bare `MarketAsyncResult` tagged enum, NOT a `MarketAsyncResultEnvelope`. Adapter now parses bare form and synthesizes the Wire envelope at POST time using `row.job_id`.
  - **CRITICAL: `OutboxRow` missing `created_at`** — verified at `db.rs:2377-2386`. Added to struct + explicitly enumerated every SELECT helper that must be updated.
  - **CRITICAL: failure-branch silent no-op** — verified at `db.rs:2692` (`WHERE status = 'ready'`) + `server.rs:4067-4077` (calls bump on a still-pending row). Spec now includes explicit fix.
  - **CRITICAL: sweep doesn't check lease** — verified at `db.rs:2671`. `fleet_outbox_mark_failed_if_ready` updated to CAS on `status='ready' AND (delivery_lease_until IS NULL OR delivery_lease_until < now)`.
  - **CRITICAL: fill_job_ttl_secs+5min grace** — verified at Wire `fill/route.ts:518`. Rev 0.3's boot-time invariant was unimplementable (node has no read path to Wire's economic_parameters). Replaced with runtime 401-age detection producing `terminal_http_401_likely_secret_expired` chronicle reason.
  - **CRITICAL: 409 missing from terminal** — verified at Wire `compute-errors.ts:119-129`. Extended `TERMINAL_HTTP_CODES` to `[400, 401, 403, 404, 409, 410, 413]`.
  - **CRITICAL: claim SQL atomicity** — replaced UPDATE+SELECT split with `UPDATE ... RETURNING *` (SQLite 3.35+). Single atomic statement; no TOCTOU.
  - **MAJOR: ConfigSynced listener for `market_delivery_policy`** — in-scope this commit (not a pre-existing gap to punt on). Tunability claim only holds if hot-reload works.
  - **MAJOR: `OutboxRow` SELECT projection enumeration** — explicit list of 4 helpers that must be updated, plus a regression-guard test.
  - **MAJOR: callback URL re-validation at delivery time** — defense-in-depth per contract §2.5; cheap to add; prevents mutated-URL exfiltration risk.
  - **MAJOR: trace correlation via `request_id`** — new outbox column + threaded through every chronicle event so operator bug reports map to Wire request_id.
  - **MAJOR: `mark_failed_with_error_cas` explicit SQL** — was undefined in rev 0.3; now spelled out including the predicates it CASes on (both `pending` and `ready` to cover all terminal-write sites).
  - **MAJOR: pseudocode consistency** — replaced `for row in claimed { deliver_one(ctx, row).await; }` with `stream::iter(claimed).for_each_concurrent(...).await`. Prose and code now agree.
  - **MAJOR: RwLock guard across `.await`** — pseudocode now reads policy once at top of `deliver_one` into a clone; no guard held across POST.
  - **MAJOR: redirect policy** — reqwest client explicitly configured with `.redirect(Policy::none())` to prevent cross-origin token leak via DNS compromise.
  - **MAJOR: missing crates** — `httpdate` + `mockito` added to Cargo.toml in Step 2.5; `cargo check` verifies them picked up.
  - **MAJOR: `pyramid_schema_versions` table** — added to the same migration that introduces the new outbox columns; records apply-time for orphan-detection heuristic.
  - **MAJOR: chronicle event naming** — renamed `compute_*` → `market_*` to match existing `compute_chronicle.rs` taxonomy and avoid collision with Wire-side `compute_result_delivered`.
  - **MAJOR: terminal path clears both lease AND `delivery_next_attempt_at`** — CAS helper writes both to NULL on terminal transitions.
  - **Chronicle metadata** extended: `retry_after_source`, `latency_ms_source`, `request_id`, expanded `reason` enum.
  - **Build order** grew from 21 to 23 steps (adds ConfigSynced arm + OutboxRow enumeration + URL re-validation + request_id threading).
  - **Bilateral items identified** for Wire owner — see §"Wire-developer paste-back" in the session chronicle. Not build-blockers for this phase; represent systemic improvements to cross-repo protocol that would simplify future phases.
  - **Live-data verifications on moltbot**: confirmed zero `compute_result_*` events in `wire_chronicle` ever (the provider-side callback path has never fired once since schema deployment — corroborates our gap-is-total diagnosis). All three test jobs from 2026-04-20 still `delivery_status=pending`, `delivery_attempts=0`. Wire's own expiry-sweep is also not reconciling provider-timed-out jobs — a separate Wire-side gap surfaced by this investigation. Confirmed `fill_job_ttl_secs = 1800` from economic_parameter (stored at key `ttl_secs` under `parameter_name: 'fill_job_ttl_secs'`).

- **rev 0.4 → 0.5**: Wire-owner bilateral alignment (2026-04-20). Wire owner returned maximal-shape answers to each systemic ask from rev 0.4's paste-back. Every workaround in rev 0.4 is now replaced by a proper bilateral mechanism:
  - **§1 Wire-parameters visibility**: heartbeat piggyback (not separate endpoint — Pillar 25). Keyed map sourced from economic_parameter set with node-facing allow-list (itself a contribution — Pillar 2). Spec adds `AuthState.wire_parameters` + generic plumbing; delivery worker consumes `fill_job_ttl_secs` + `callback_secret_grace_secs` keys.
  - **§2 X-Wire-Retry protocol data**: 3-value enum (`never | transient | backoff`) on non-2xx responses; Retry-After header carries duration. Spec replaces `TERMINAL_HTTP_CODES` enumeration with `classify_retry()` that prefers the header; enumeration is fallback for pre-upgrade Wire.
  - **§3 chronicle naming**: forward-only rename. Wire keeps `compute_result_delivered` for historical rows (grandfathered in CHECK constraint); new emissions on the node side use `market_result_delivered_to_wire`. No UPDATE on existing rows.
  - **§4 error.code enum**: pinned in contract §2.3 + shared-types package (bilateral, not a contribution). Spec's `classify_failure_code` conforms.
  - **§5 grace_window**: published as `callback_secret_grace_secs` economic_parameter (Pillar 2). Node reads via wire_parameters allow-list.
  - **+ Additional Wire-side items** (owner's proactive adds per pillars): audit of hardcoded values in `src/app/api/v1/compute/fill/route.ts` (+5min grace literal, 1800 default); `/ops/compute` delivery-state visibility (pending-jobs-by-age + expired_undelivered count in last 24h per Pillar 42); post-ship serial verifier pass per Pillar 39 + `feedback_serial_verifier`.
  - **Spec impact**: rev 0.5 replaces the rev 0.4 runtime-age-heuristic for secret-expiry with a proper `wire_parameters`-aware invariant check; replaces `TERMINAL_HTTP_CODES` enumeration with `X-Wire-Retry` header-first classification; adds a generic `wire_parameters` plumbing path on the node that future phases can consume; adds post-ship serial verifier as Build Order Step 22.
  - All rev 0.4 workarounds preserved as fallback behavior for pre-Wire-upgrade nodes. Zero lockstep required between the two ships.
