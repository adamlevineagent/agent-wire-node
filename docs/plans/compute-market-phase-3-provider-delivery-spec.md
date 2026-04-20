# Compute Market Phase 3 — Provider Delivery Worker Spec

**Date:** 2026-04-20
**Author:** Claude (agent-wire-node upstairs mac)
**Status:** Draft — rewritten against rev 2.0 contract (P2P delivery)
**Rev:** 0.6
**Contract:** `GoodNewsEveryone/docs/architecture/wire-node-compute-market-contract.md` rev 2.0 (commit `838b7700`)
**Supersedes:** rev 0.1–0.5 (Wire-in-middle topology)

---

## Purpose

Close the provider side of the two-POST delivery protocol defined by contract rev 2.0. After a market dispatch completes inference, the provider node:

1. **POSTs content directly to the requester** (`requester_callback_url`) with `Bearer <requester_delivery_jwt>` — §2.6.
2. **POSTs settlement metadata to Wire** (`callback_url`) with `Bearer <callback_auth.token>` — §2.3.

Both POSTs are independent. Both must eventually land (or exhaust their per-leg retry budget). Wire is zero-storage for content (§2.4); requester-attestation that content was received is off-Wire-observability (§2.6).

This spec replaces rev 0.5's Wire-in-middle architecture entirely. Commits `5faff2d` + `46bd4cd` + `974d37a` shipped the rev 0.5 shape — most of the scaffolding survives (supervisor, envelope-adapter pattern, JWT verification pattern, custom-Debug redaction, HTTPS/SSRF validation). What changes is the **topology**: one leg becomes two, each with its own URL + Bearer + retry budget.

---

## Scope

**In scope:**

- **Two-POST state machine** on the existing `fleet_result_outbox`. Per-leg state tracked in new columns; one outbox row owns both legs of the delivery.
- **Per-leg retry budget** per Q-PROTO-6 — independent `max_attempts_content` + `max_attempts_settlement` from `compute_delivery_policy` economic_parameter. Shared `backoff_schedule_secs`.
- **Two envelope adapters** (`build_content_envelope` + `build_settlement_envelope`) from one internal `MarketAsyncResult`. Settlement is §2.3 shape minus `result.content`; content is §2.6 full shape.
- **Two Bearer sources:** `requester_delivery_jwt` from dispatch body for content POST (opaque to provider, verified by requester); `callback_auth.token` from dispatch body for settlement POST (unchanged from rev 0.5 semantic, matches Wire's sha256-at-rest verification).
- **Per-leg lease** — prevents double-POST of the same leg; leg independence means the content leg can be in-flight while settlement is already done, or vice versa.
- **Chronicle events renamed** per rev 2.0 §2.5 grandfathering — new emissions use the rev-2.0-aligned names; old names stay in the CHECK constraint as deprecated.
- **Requester-delivery JWT verifier** on the node (for when this node is the requester) — sibling of `verify_market_identity` at `result_delivery_identity.rs::verify_requester_delivery_token` per §3.4. Wires into the existing `/v1/compute/job-result` handler.
- **Integration fixes** to commit 46bd4cd's `spawn_market_worker` failure-branch pending→ready promotion (unchanged from rev 0.5; still the right fix).
- **Frontend**: `DeliveryHealth` badge tracks per-leg status; `ContentDelivery` badge may separate out (or one combined indicator with richer states).

**Out of scope (deferred):**

- **Relay-market delivery** (`privacy_tier != "direct"`). The relay layer is a separate phase per `63-relays-and-privacy.md`. Phase 3 handles `privacy_tier = "direct"` only.
- **JWT token refresh** mid-retry. Per §2.6, default is `requester_delivery_jwt_ttl_secs = fill_job_ttl_secs` so one token survives the full retry sequence. A refresh endpoint ships in v0.2 if ops data shows TTL is too tight.
- **Requester-offline dead-letter** on Wire (per D5). If provider exhausts content-leg attempts, content is lost; requester polls `/api/v1/compute/jobs/:job_id` and sees `delivery_status = failed_content_only`.

---

## Architecture

### Two-POST state machine

```
admission (worker success/failure → ready) 
          │
          ▼
   claim_for_delivery  (per-leg: skip legs already done OR still in backoff)
          │
          ▼
      ┌───┴───┐
      ▼       ▼
  content   settlement
   POST      POST
   2xx?      2xx?
    │         │
    └────┬────┘
         ▼
  both legs OK?  ── NO ── retry the failed leg(s) on next tick
   │
   YES → CAS ready → delivered. Chronicle summary event.
```

**Independence:** the two legs do not block each other. Settlement can 2xx on attempt 1 while content is still retrying on attempt 3. Terminal-exhaustion of one leg does not abort the other.

**Terminal states per leg:**

- Both legs 2xx → row transitions `ready → delivered`. Emit `market_result_delivered` summary chronicle.
- Content exhausts, settlement 2xx → `delivery_status = failed_content_only` on row (terminal). Chronicle `market_result_content_delivery_failed` + metadata flag. Requester polls Wire to reconcile.
- Settlement exhausts, content 2xx → `delivery_status = failed_settlement_only`. Chronicle `market_result_settlement_delivery_failed`. **Provider unpaid; Wire doesn't know inference ran.** Dispute/manual-recovery path.
- Both exhaust → `delivery_status = failed`. Chronicle `market_result_delivery_failed` with both reasons.

### Schema changes

**Dead columns from rev 0.5** (commit `5faff2d` ALTERs — never exercised against real traffic):

- `delivery_lease_until`, `delivery_next_attempt_at` — replaced by per-leg equivalents. Migration leaves them in place (SQLite DROP COLUMN is expensive); documented as "rev 0.5 unused."
- Existing `delivery_attempts` on `fleet_result_outbox` from Phase 2 — **repurposed as content-leg counter** since its semantic was always "delivery attempts." Rename in code to `content_delivery_attempts`; no SQL rename.

**New columns** (additive migration):

```sql
ALTER TABLE fleet_result_outbox ADD COLUMN requester_callback_url TEXT;
ALTER TABLE fleet_result_outbox ADD COLUMN requester_delivery_jwt TEXT;

-- Content leg (provider → requester)
ALTER TABLE fleet_result_outbox ADD COLUMN content_posted_ok INTEGER NOT NULL DEFAULT 0;
ALTER TABLE fleet_result_outbox ADD COLUMN content_lease_until TEXT;
ALTER TABLE fleet_result_outbox ADD COLUMN content_next_attempt_at TEXT;
ALTER TABLE fleet_result_outbox ADD COLUMN content_last_error TEXT;
-- content leg reuses existing `delivery_attempts` as its counter
-- (semantic was always "delivery attempts" = content-delivery).

-- Settlement leg (provider → Wire)
ALTER TABLE fleet_result_outbox ADD COLUMN settlement_posted_ok INTEGER NOT NULL DEFAULT 0;
ALTER TABLE fleet_result_outbox ADD COLUMN settlement_delivery_attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE fleet_result_outbox ADD COLUMN settlement_lease_until TEXT;
ALTER TABLE fleet_result_outbox ADD COLUMN settlement_next_attempt_at TEXT;
ALTER TABLE fleet_result_outbox ADD COLUMN settlement_last_error TEXT;
```

Nine new columns. All nullable or defaulted; additive; PRAGMA-guarded per the existing idempotent-ALTER pattern in `db::init_pyramid_db`. Fleet rows remain unaffected (all columns NULL/0 on fleet-kind rows).

**`OutboxRow` struct extension** — the existing struct already got `created_at` + `callback_auth_token` + `request_id` + `inference_latency_ms` in rev 0.5. Rev 0.6 adds the 9 new per-leg fields. Centralized `OUTBOX_SELECT_*` consts + `map_outbox_row` helper from rev 0.5 still apply — one place to update column order.

**Index:**

```sql
CREATE INDEX IF NOT EXISTS idx_fleet_outbox_market_delivery_legs
    ON fleet_result_outbox (status, content_posted_ok, settlement_posted_ok)
    WHERE callback_kind IN ('MarketStandard', 'Relay');
```

Replaces the rev 0.5 index on `delivery_lease_until` (index hangs around; SQLite no-op on IF NOT EXISTS — eventually drop in cleanup migration).

### Claim query (rev 0.6 shape)

**Per-leg eligibility** — a row is eligible for a POST on a leg iff:
1. Row status = `'ready'`.
2. `callback_kind = 'MarketStandard'`.
3. That leg's `*_posted_ok = 0`.
4. That leg's `*_lease_until IS NULL OR < now()` (not currently being POSTed).
5. That leg's `*_next_attempt_at IS NULL OR <= now()` (backoff satisfied).

Rather than one combined claim, rev 0.6 issues **two independent claim queries** — one per leg — per tick. Each runs `UPDATE ... RETURNING *` with the per-leg lease/backoff predicates.

```sql
-- Content-leg claim
UPDATE fleet_result_outbox
SET content_lease_until = datetime('now', ?1)
WHERE rowid IN (
  SELECT rowid FROM fleet_result_outbox
  WHERE status = 'ready'
    AND callback_kind = 'MarketStandard'
    AND content_posted_ok = 0
    AND (content_lease_until IS NULL OR content_lease_until < datetime('now'))
    AND (content_next_attempt_at IS NULL OR content_next_attempt_at <= datetime('now'))
  ORDER BY created_at ASC
  LIMIT ?2
)
RETURNING <OUTBOX_SELECT_COLUMNS>;
```

Settlement-leg claim is identical, swapping `content_*` → `settlement_*`.

`max_concurrent_deliveries` budget divides across legs — half each by default, or both legs get the full budget and the tick's concurrency limit is applied at the `for_each_concurrent` level across the union. Simpler: each leg's claim uses half the budget.

### Per-leg POST flow (pure per-leg function; called twice per tick)

```rust
async fn deliver_leg(
    ctx: &DeliveryContext,
    row: &OutboxRow,
    leg: Leg,  // Content or Settlement
    p: &MarketDeliveryPolicy,
    dp: &ComputeDeliveryPolicy,  // rev 2.0: per-leg attempts from economic_parameter
) {
    // 1. Construct envelope per leg.
    let body = match leg {
        Leg::Content    => build_content_envelope(row, &parse_result(&row.result_json)?)?,
        Leg::Settlement => build_settlement_envelope(row, &parse_result(&row.result_json)?)?,
    };

    // 2. Look up URL + Bearer per leg.
    let (url, bearer) = match leg {
        Leg::Content => (
            row.requester_callback_url.clone().ok_or(AdapterError::MissingRequesterUrl)?,
            row.requester_delivery_jwt.clone().ok_or(AdapterError::MissingRequesterJwt)?,
        ),
        Leg::Settlement => (
            row.callback_url.clone(),
            row.callback_auth_token.clone().ok_or(AdapterError::MissingCallbackToken)?,
        ),
    };

    // 3. SSRF re-validate URL.
    validate_callback_url(&url, &kind_for(leg), &FleetRoster::empty())?;

    // 4. POST with same reqwest client pattern as rev 0.5 (redirect::none, timeout,
    //    Bearer header, truncate-on-error, no {:?} on request).
    let (status, headers) = post_leg(&url, &bearer, &body, p).await;

    // 5. Branch on outcome + update per-leg state.
    match classify_outcome(status, &headers, leg) {
        Outcome::Success => {
            mark_leg_posted_ok(ctx, row, leg, p).await;
            if both_legs_done(row after update) {
                emit("market_result_delivered", summary_metadata).await;
                mark_row_delivered(ctx, row, p).await;
            } else {
                emit(per_leg_success_event(leg), leg_metadata).await;
            }
        }
        Outcome::Terminal(reason) => {
            mark_leg_failed(ctx, row, leg, reason, p).await;
            emit(per_leg_terminal_event(leg), { reason, error }).await;
            // Other leg continues independently per D3/D8.
        }
        Outcome::Transient(err) => {
            bump_leg_attempt_with_backoff(ctx, row, leg, err, dp).await;
            emit(per_leg_attempt_failed_event(leg), metadata).await;
        }
    }
}
```

**Reused from rev 0.5** (unchanged or near-unchanged):

- Supervisor (`supervise_delivery_loop` → still wraps the loop in `AssertUnwindSafe::catch_unwind`; chronicle events rename but semantics identical).
- `tokio::select!` nudge + periodic tick trigger model.
- `classify_retry` for X-Wire-Retry header reading — applies to settlement leg only (arbitrary requester callbacks don't standardize the header).
- `parse_retry_after_header` (both RFC 7231 forms via `httpdate`).
- `is_valid_bearer` validator — now called twice (for `callback_auth.token` AND for the new `requester_delivery_jwt`).
- `truncate` (UTF-8-safe error string truncation).
- `classify_failure_code` (pinned enum from §2.3).
- Reqwest client config with `redirect(Policy::none())`.
- `CallbackAuth` custom `Debug` redaction (rev 0.5 shipped this; rev 0.6 adds the same to a new type holding `requester_delivery_jwt` if we make it a struct).

### Requester-delivery JWT verifier (new — node as requester)

When this node is the REQUESTER (not provider), inbound `POST /v1/compute/job-result` must verify the `Authorization: Bearer <requester_delivery_jwt>`. §3.4 defines:

- `aud = "requester-delivery"` (distinct from `"compute"` and legacy `"result-delivery"`)
- `iss = "wire"`
- `sub = <uuid_job_id>` (must match `body.job_id`)
- `rid = <requester_operator_id>` (must match `self.operator_id`)
- `exp` not expired (±60s skew)
- EdDSA signature vs `jwt_public_key` (same key material as dispatch JWT)

Implementation: new function `verify_requester_delivery_token` in `src-tauri/src/pyramid/result_delivery_identity.rs` (sibling of existing `verify_result_delivery_token` from commit `43b8704`). The existing handler at `server.rs::handle_compute_job_result` switches from `verify_result_delivery_token` to `verify_requester_delivery_token` (or adds a compat mode that tries both during rev-2.0-transition).

**Zero-lockstep note:** pre-rev-2.0 Wire emits `aud="result-delivery"` (legacy); rev 2.0 emits `aud="requester-delivery"`. Transition: the handler accepts EITHER `aud` for a short transition window, logs which one was used, and emits a deprecation chronicle on the legacy aud. After Wire is fully rev-2.0, legacy aud acceptance drops.

### Auth token shapes persisted on outbox

| Column | Purpose | Source | Leg that uses it |
|---|---|---|---|
| `callback_auth_token` (rev 0.5) | Settlement bearer | `req.callback_auth.token` | settlement |
| `requester_delivery_jwt` (new) | Content bearer | `req.requester_delivery_jwt` | content |

Both stored at admission time in `handle_market_dispatch`. Both are opaque strings to the provider. Neither ever appears in logs (enforced by custom `Debug` + `truncate` + "no `{:?}` on request" grep test).

### Chronicle events (rev 0.6 final taxonomy)

Per rev 2.0 §2.5 grandfathering, rev-0.5 names stay in the CHECK constraint on `wire_chronicle` for historical rows but are DEPRECATED for new emissions. Node-side `pyramid_compute_events` has no CHECK constraint so the rename is free.

| Event | Fires when | Metadata |
|---|---|---|
| `market_result_delivered` | BOTH legs 2xx + CAS ready→delivered | `job_id, request_id, content_attempts, settlement_attempts, latency_ms, total_duration_ms` |
| `market_content_leg_succeeded` | Content leg alone 2xx (settlement still in flight OR also just done) | `job_id, request_id, attempts, duration_ms, latency_ms_source` |
| `market_settlement_leg_succeeded` | Settlement leg alone 2xx | `job_id, request_id, attempts, duration_ms` |
| `market_content_delivery_attempt_failed` | Content leg transient failure | `job_id, attempt, error, status_code, next_attempt_at` |
| `market_settlement_delivery_attempt_failed` | Settlement leg transient failure | `job_id, attempt, error, status_code, retry_after_source` |
| `market_content_delivery_failed` | Content leg terminal (max-attempts or terminal HTTP) | `job_id, attempts, final_error, reason` |
| `market_settlement_delivery_failed` | Settlement leg terminal | `job_id, attempts, final_error, reason` |
| `market_result_delivery_failed` | BOTH legs terminal — row dead-end | `job_id, content_error, settlement_error` |
| `market_result_delivery_cas_lost` | 2xx + CAS=0 (sweep raced) — rare under per-leg model but possible | `job_id, leg, reason` |
| `market_delivery_task_panicked` / `_task_exited` | Supervisor lifecycle | unchanged from rev 0.5 |
| `market_wire_parameters_updated` | Heartbeat diff | unchanged from rev 0.5 |

Rev-0.5 names kept for back-compat on the node side:
- `market_result_delivered_to_wire` — **do not emit in rev 0.6.** Chronicle queries can UNION both names during transition.

### Reason enum on `market_*_delivery_failed` events

Distinct per-leg:

**Content leg reasons:**
- `envelope_parse_failed` (code bug)
- `requester_callback_url_missing` (pre-migration orphan OR Wire bug)
- `requester_delivery_jwt_missing_or_invalid` (same)
- `callback_url_validation_failed` (SSRF re-validation fired)
- `terminal_http_400/401/403/404/410/413` (requester rejected)
- `terminal_http_401_likely_jwt_expired` (401 after row older than `requester_delivery_jwt_ttl_secs`)
- `max_attempts_content`

**Settlement leg reasons:**
- `envelope_parse_failed`
- `callback_auth_token_missing_or_malformed`
- `callback_url_validation_failed`
- `terminal_http_400/401/403/404/409/410/413` (Wire rejected via X-Wire-Retry `never` OR fallback enum)
- `terminal_http_401_likely_secret_expired` (per rev 0.5 §Wire-parameters-aware secret-expiry detection)
- `max_attempts_settlement`
- `orphaned_by_migration` (pre-rev-0.6 row with NULL callback_auth_token)

### Policy reads

| Field | Source | Role |
|---|---|---|
| `max_attempts_content` | `compute_delivery_policy` economic_parameter via heartbeat `wire_parameters` OR direct chain read at load | Content-leg budget (D8 / Q-PROTO-6) |
| `max_attempts_settlement` | same | Settlement-leg budget |
| `backoff_schedule_secs` | same — array `[1, 5, 30, 300, 3600]` default | Shared backoff on both legs (indexed by attempt#) |
| `callback_post_timeout_secs` | `MarketDeliveryPolicy` | Per-POST timeout (both legs) |
| `lease_grace_secs` | `MarketDeliveryPolicy` | Added to POST timeout for lease duration |
| `max_concurrent_deliveries` | `MarketDeliveryPolicy` | **Unified cap across both legs.** `for_each_concurrent(N)` over a flat list of `(row_id, leg)` pairs — NOT per-leg. The cap bounds the node's outbound HTTP/socket budget, which is a shared resource across both legs of all in-flight rows. Q-PROTO-6's per-leg semantic lives in the retry budget (attempts), not the concurrency budget (in-flight POSTs). Bilateral clarification with Wire owner 2026-04-20. |
| `max_error_message_chars` | `MarketDeliveryPolicy` | Truncation cap (both legs) |
| `callback_secret_grace_secs` | `wire_parameters` (Wire ships via heartbeat) | Settlement-leg 401-likely-secret-expired discriminator |
| `requester_delivery_jwt_ttl_secs` | `wire_parameters` | Content-leg 401-likely-jwt-expired discriminator |
| `fill_job_ttl_secs` | `wire_parameters` | Upper-bound sanity check (shared; only used for observability on 401 classification) |

**`compute_delivery_policy`** is a new economic_parameter Wire ships rev 2.0. Node reads via `wire_parameters`:

```rust
let dp = ComputeDeliveryPolicy::from_wire_parameters(&auth.wire_parameters)
    .unwrap_or_else(ComputeDeliveryPolicy::contract_defaults); // {5, 5, [1,5,30,300,3600]}
```

New node-side struct `ComputeDeliveryPolicy` parallel to `MarketDeliveryPolicy` but protocol-scoped (not node-operator-tunable separately; Wire is the source of truth).

### Backoff schedule (shared, per-leg independent)

Per-attempt delay looks up `backoff_schedule_secs[attempt-1]` with the last element replicated for attempts beyond the schedule length. Default schedule `[1, 5, 30, 300, 3600]` — attempt 1 retries after 1s, attempt 5 after 3600s (1hr). No exponential math; just table lookup. Operator tunes by superseding `compute_delivery_policy.backoff_schedule_secs`.

### Retry budgets (Q-PROTO-6)

- Content leg: `dp.max_attempts_content` (default 5). After 5 failures → terminal with reason `max_attempts_content`.
- Settlement leg: `dp.max_attempts_settlement` (default 5). After 5 failures → terminal with reason `max_attempts_settlement`.
- Legs **do not share budget.** Flaky requester tunnel does not exhaust settlement attempts.

### Retry-After semantics (rev 2.0 §2.3)

- **Settlement leg:** Wire emits `X-Wire-Retry` + `Retry-After` per §2.2 / §2.3. Node honors header values (`never | transient | backoff`), backoff schedule from header overrides local schedule.
- **Content leg:** requester responses are arbitrary HTTP — may or may not emit `Retry-After` or `X-Wire-Retry`. Node reads `Retry-After` if present (standard HTTP); `X-Wire-Retry` ignored on content leg (not a requester-protocol header). On ambiguous 5xx, fall back to `backoff_schedule_secs[attempt-1]`.

---

## Integration points (code diff from rev 0.5)

### `market_dispatch.rs`

- New fields on `MarketDispatchRequest`:
  - `requester_callback_url: TunnelUrl` (not optional — dispatch requires it; pre-rev-2.0 compat = reject with 400 `requester_callback_url_required`)
  - `requester_delivery_jwt: String` (opaque to provider; store on row)

Zero-lockstep: if Wire sends a pre-rev-2.0 dispatch missing these fields, `deny_unknown_fields` serde config catches it as malformed request → node 400s with `requester_callback_url_missing`. Wire owner's plan will ship rev 2.0 Wire-side ahead of node; transition handled via handshake-fail-loud rather than handshake-succeed-broken.

### `handle_market_dispatch` (server.rs)

- At admission, plumb TWO new values into `market_outbox_insert_or_ignore`: `requester_callback_url`, `requester_delivery_jwt`.
- Signature grows: `market_outbox_insert_or_ignore(conn, job_id, callback_url, callback_kind, expires_at, callback_auth_token, request_id, requester_callback_url, requester_delivery_jwt)`.
- Admission-time validation: re-SSRF-check `requester_callback_url` before accepting the dispatch (defense-in-depth; Wire already validated, but re-check at every receive).
- Return 400 with body shape `{"error": "requester_callback_url_missing_or_invalid"}` if the dispatch doesn't include it.

### `spawn_market_worker`

- **Success path**: unchanged from rev 0.5 (promote `pending→ready`, nudge delivery, chronicle).
- **Failure path**: unchanged from rev 0.5 fix (promote `pending→ready` with real error envelope, nudge delivery).

Both paths now trigger BOTH legs of the two-POST state machine (delivery worker sees `ready` row, launches both legs independently).

### `market_delivery.rs` (the main rewrite)

Roughly half-rewrite of rev 0.5's module. Kept: supervisor, nudge+tick, reqwest client config, envelope serialization primitives, classify_retry, parse_retry_after_header, is_valid_bearer, truncate, classify_failure_code, custom Debug redaction. Changed:

- `DeliveryContext` gains `wire_parameters: Arc<RwLock<AuthState>>` (already had via auth ref — just document).
- `tick()` issues TWO claim queries (content + settlement), dispatches POSTs to `deliver_leg(Leg::Content)` + `deliver_leg(Leg::Settlement)` in bounded parallel.
- Envelope adapter split: `build_content_envelope(&row, &result) → CallbackEnvelope`, `build_settlement_envelope(&row, &result) → SettlementEnvelope`. Settlement envelope omits `content`; internally can be a struct with `#[serde(skip_serializing_if = "never_serialize")]` on content, or a distinct type.
- DB helpers: 4 new per-leg CAS helpers (`claim_content_for_delivery`, `claim_settlement_for_delivery`, `mark_content_posted_ok_if_ready`, `mark_settlement_posted_ok_if_ready`, `bump_content_attempt_with_backoff`, `bump_settlement_attempt_with_backoff`). All CAS-guarded on `status='ready'` + per-leg `posted_ok=0`.
- `market_outbox_mark_failed_with_error_cas` keeps terminal-row semantics; called only when at least one leg terminal-exhausts AND the other is also terminal (both-ways exhaust) OR on early terminal (envelope parse fails, etc.).
- New helper `check_both_legs_complete(row) → bool`: after any leg's success CAS, re-read the row and flip `status → delivered` if both legs are now OK.

### `main.rs` heartbeat self-heal

- `wire_parameters` consumption already shipped in rev 0.5 (commit `46bd4cd`). Rev 0.6 adds two more keys to the parse path: `compute_delivery_policy` (full object), `requester_delivery_jwt_ttl_secs` (scalar). Node-side fallback defaults (contract rev 2.0 §6 values) kick in if Wire doesn't ship them (pre-rev-2.0 Wire).

### `main.rs` startup

- Startup recovery now clears leases on BOTH legs (`content_lease_until = NULL`, `settlement_lease_until = NULL` for all ready MarketStandard rows).
- Delivery task spawn unchanged (supervisor pattern).
- Nudge sender on `MarketDispatchContext` unchanged (still one sender; both legs are processed within each tick).

### `result_delivery_identity.rs` (requester-side verifier)

- New function `verify_requester_delivery_token` per §3.4.
- Existing `verify_result_delivery_token` kept for legacy-aud support during transition.
- `handle_compute_job_result` (server.rs inbound) prefers `verify_requester_delivery_token`; falls back to legacy on `aud` mismatch during transition; emits deprecation warning chronicle.

### Frontend (`ComputeOfferManager.tsx`)

Rev 0.5's `DeliveryHealth` indicator updates:
- Events queried: `market_result_delivered`, `market_content_delivery_attempt_failed`, `market_settlement_delivery_attempt_failed`, `market_content_delivery_failed`, `market_settlement_delivery_failed`, `market_result_delivery_failed`, `market_delivery_task_panicked/_exited`.
- State machine: per-leg health surfaced separately OR unified with worst-leg wins.
- Display: "Both legs delivered" | "Content delivered, settlement retrying" | "Settlement delivered, content retrying" | "Delivery failed (reason)" | "Task panicked/exited".

Split into `ContentDeliveryHealth` + `SettlementDeliveryHealth` OR one badge with richer text? Lean toward **one badge** — operators care about end-state (delivered / failing / dead), not leg breakdowns until triaging.

---

## State transitions (outbox row perspective)

```
                      ┌─────────────┐
admission ─(worker)→  │   pending   │
                      └──────┬──────┘
                             │ promote (worker success OR failure
                             │          envelope synthesized)
                             ▼
                      ┌─────────────┐
                      │    ready    │
                      │  content:   │ content_posted_ok=0
                      │  settle:    │ settlement_posted_ok=0
                      └──────┬──────┘
                             │ ticks deliver each leg independently
           ┌─────────────────┼────────────────────┐
           ▼                 ▼                    ▼
      content 2xx       both 2xx            settlement 2xx
      only              (at same             only
      flag set          tick)                flag set
           │             │                    │
           │             ▼                    │
           │       ┌──────────┐                │
           │       │ delivered│  ← terminal   │
           │       │ (both OK)│                │
           │       └──────────┘                │
           │                                   │
           ├───── settlement exhausts ─────────┤
           │                                   │
           ▼                                   ▼
   failed_content_only                  failed_settlement_only
   (content in, settlement lost)         (settlement in, content lost)
```

Both-exhausted → `delivery_status = failed` + terminal chronicle.

---

## Test plan

Builds on rev 0.5's 19 unit tests. Adds:

1. `content_leg_success_while_settlement_retrying` — mock content 2xx, settlement 503; assert per-leg state.
2. `settlement_leg_success_while_content_retrying` — inverse.
3. `both_legs_success_transitions_to_delivered` — both 2xx; row transitions `ready → delivered`.
4. `content_leg_exhausts_settlement_unaffected` — content 5 × 5xx, settlement 2xx; assert `failed_content_only`.
5. `settlement_leg_exhausts_content_unaffected` — inverse.
6. `both_legs_exhaust_row_failed` — both hit max-attempts.
7. `content_jwt_expiry_terminates_with_specific_reason` — 401 after row older than `requester_delivery_jwt_ttl_secs`; reason `terminal_http_401_likely_jwt_expired`.
8. `settlement_401_expiry_via_callback_secret_grace_window` — rev 0.5 kept + validated under per-leg.
9. `per_leg_budget_not_shared` — content hits 5 attempts, settlement starts fresh.
10. `envelope_adapter_settlement_omits_content` — `build_settlement_envelope` never serializes `result.content`.
11. `envelope_adapter_content_includes_content` — `build_content_envelope` includes it.
12. `requester_delivery_jwt_never_in_logs` — truncate + Debug redaction test, now for the new token too.
13. `requester_callback_url_ssrf_revalidated_at_delivery` — mutate stored URL to loopback; assert terminal with `callback_url_validation_failed`.
14. `requester_delivery_jwt_verifier_happy_path` — sibling to verify_market_identity tests, on the requester-side verifier.
15. `requester_delivery_jwt_aud_mismatch_rejected` — `aud="compute"` or `"result-delivery"` rejected.
16. `requester_delivery_jwt_rid_mismatch_rejected` — wrong operator_id rejected.
17. `backoff_schedule_from_economic_parameter` — policy supersession updates delay; assert retry after schedule[attempt].
18. `pre_rev_2_0_dispatch_missing_fields_400s` — dispatch body without `requester_callback_url` → 400 at admission.
19. `legacy_aud_accepted_during_transition` — `verify_result_delivery_token` falls through to legacy aud; emits deprecation chronicle.

Total: rev 0.5's 19 + 19 new = 38+. Keep each small.

---

## Build order

1. Contract verification — DONE (rev 2.0 landed as `838b7700`).
2. New outbox columns (migration + struct + OUTBOX_SELECT_COLUMNS update + map_outbox_row update).
3. `market_outbox_insert_or_ignore` signature: add `requester_callback_url` + `requester_delivery_jwt` params. Update all callers.
4. `MarketDispatchRequest` struct gains the two fields. Add serde shape; `deny_unknown_fields` catches pre-rev-2.0 drift.
5. `handle_market_dispatch`: admission-time SSRF re-validation + 400s with new reason codes if required fields missing.
6. Split envelope adapters: `build_content_envelope` + `build_settlement_envelope`. Each is pure; test-first.
7. Per-leg DB helpers: `claim_content_for_delivery`, `claim_settlement_for_delivery`, `mark_*_posted_ok_if_ready`, `bump_*_attempt_with_backoff`, `mark_failed_with_legs_error_cas`.
8. Rewrite `market_delivery.rs::tick` to issue two claims + dispatch via `deliver_leg(Leg::_)`.
9. Rewrite `market_delivery.rs::deliver_one` → `deliver_leg(leg)` pure function.
10. Chronicle event constants rename + add new per-leg names.
11. `ComputeDeliveryPolicy` node-side struct + heartbeat self-heal parse path.
12. `result_delivery_identity.rs`: add `verify_requester_delivery_token`; legacy aud transition.
13. Server.rs `handle_compute_job_result`: prefer new verifier, fallback legacy, deprecation chronicle.
14. Frontend `DeliveryHealth` updated to consume new chronicle events.
15. 19 new unit tests.
16. `cargo check` (default target) + `cargo test --lib market_delivery result_delivery_identity`.
17. Commit + push.
18. Wait for Wire side to ship rev 2.0 (or match pace for zero-lockstep).
19. Rebuild Playful + BEHEM. Fire smoke. Verify both legs green in chronicle.
20. Post-ship serial-verifier pass per D9 / Pillar 39.

Estimate: 8-16h realistic. Rev 0.5 did most of the architectural heavy-lifting; rev 0.6 is mostly surgical changes to URL + auth + retry bookkeeping.

---

## Bilateral items (open)

None — P1 resolved (D1–D8 all answered; paste-back confirmed). Any further bilateral question during implementation surfaces as a new DD-W decision doc.

---

## Risks / unknowns

1. **`requester_delivery_jwt` mint timing.** Wire mints at `/fill`; if the content retry sequence exceeds `requester_delivery_jwt_ttl_secs`, token expires mid-retry. Phase 3 default TTL = fill_job_ttl_secs (30 min) so the full retry sequence should fit. If ops shows otherwise, §2.6 "Token refresh" option 1 (Wire-side refresh endpoint) ships in v0.2.

2. **Requester-side verifier backwards-compat window.** `aud="result-delivery"` (legacy) vs `"requester-delivery"` (rev 2.0). Both-accepted transition has a small security risk (replay of legacy token against new verifier). Mitigation: deprecation chronicle on every legacy use so operators see when to drop legacy.

3. **Startup-migration ordering.** Rev 0.5 shipped 5 columns; rev 0.6 adds 9 more. A node restarting between rev 0.5 and rev 0.6 deploys gets both migrations in order; no cross-version rows exist because nothing ever wrote the rev 0.5 columns in production. Safe.

4. **Per-leg concurrency budget split — RESOLVED.** Unified cap across legs, not split. `for_each_concurrent(max_concurrent_deliveries)` over a flat list of `(row_id, leg)` pairs. Bounds the node's outbound HTTP/socket budget as a shared resource. Q-PROTO-6 per-leg semantic applies to retry attempts, not concurrency. Per bilateral with Wire owner 2026-04-20.

5. **Content envelope size — RESOLVED.** Not a protocol-level cap (Pillar 37 — Wire doesn't prescribe LLM output size). Rev 2.0 §2.6 adds a NOTE recommending requester implementations enforce a bounded reader (10 MiB recommended) and return 413 on overflow. Node-side (when acting as requester): bounded body read on `/v1/compute/job-result`, 413 with `X-Wire-Retry: never` on overflow. Node-side (when acting as provider): provider also SHOULD bound its own outbound POST body reader so a pathological LLM output doesn't blow the reqwest timeout budget. Implementation note, not a contract constraint. If post-launch ops shows pathological payloads, revisit as `max_result_content_bytes` economic_parameter.

---

## Audit history

- **rev 0.1 → 0.5**: Wire-in-middle architecture. Two audit cycles (Stage 1 informed + Stage 2 discovery), pillars pass, source-code verification of every load-bearing claim, Wire-owner bilateral alignment. Shipped as commits `5faff2d` + `46bd4cd` + `974d37a`.

- **rev 0.5 → 0.6 (2026-04-20)**: Architectural reversal per rev 2.0 contract (`GoodNewsEveryone@838b7700`). Wire ownership reclassified as coordinator, not content carrier (canonical `63-relays-and-privacy.md`). Two-POST topology: content → requester direct (§2.6), settlement → Wire (§2.3 minus content). Per-leg retry budget (Q-PROTO-6). New `requester_delivery_jwt` (§3.4, `aud="requester-delivery"`). Nine new outbox columns (per-leg state). ~60% of rev 0.5 code survives: supervisor, envelope adapter pattern (split into two), JWT verification pattern, reqwest config, classify_retry, truncate, Debug redaction, heartbeat self-heal, chronicle supervisor. Target URLs + Bearer sources + retry bookkeeping new.

- **Pending**: Wire-side P3 re-plan (Wire owner shipping in parallel). Cross-audit after both sides finalize plans. Zero-lockstep via fallback behaviors on both sides.
