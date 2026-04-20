# Compute Market Phase 3 — Wire-Developer Handoff

**Date:** 2026-04-20
**From:** Claude on agent-wire-node (Adam's upstairs mac) after a 4-rev audited spec
**To:** Wire-side owner on `GoodNewsEveryone` (moltbot-served)
**Status:** Non-blocking improvements identified + one smoking-gun verification of the end-to-end gap

---

## TL;DR

The provider-side delivery worker on the node (POST result envelope to `/api/v1/compute/callback/[job_id]`) was never built. It's being built now under `docs/plans/compute-market-phase-3-provider-delivery-spec.md` (rev 0.4, two audit rounds applied). No Wire-side changes are required for the node to ship this.

**But:** the investigation surfaced five systemic gaps at the Wire↔node boundary that would make this phase — and future market-adjacent phases — cleaner. Sorted by load-bearing-ness. None block the node-side ship; all represent 100-year-level protocol decisions worth your pass.

---

## Smoking-gun context (live-data verified on moltbot 2026-04-20)

```sql
-- Zero compute_result_* events in wire_chronicle, EVER.
SELECT event_type, COUNT(*) FROM wire_chronicle
WHERE event_type LIKE 'compute_result_%' GROUP BY event_type;
-- (0 rows)
```

Confirmed: no provider callback has ever landed successfully since schema deployment. The three jobs from today's smoke (`a59cf9a6…`, `e58420d9…`, `99c27321…`) all stuck at `status='filled'`/`delivery_status='pending'`/`delivery_attempts=0`. Even the `99c27321…` with `completed_at` set and `timeout_at` long past still shows `delivery_status='pending'` — which points to a separate Wire-side gap (see §5 below).

The node-side spec closes the provider-side half. This doc is about the systemic cross-repo items.

---

## 1. Wire-to-node visibility into operationally-relevant economic_parameters

**Gap:** The node has no read path to Wire's current `fill_job_ttl_secs` (default 1800s) or the +5min grace Wire adds at secret mint time (`fill/route.ts:518`). The node-side `ready_retention_secs` policy field sits on the node — the two values must stay compatible (node's retention ≤ Wire's secret lifetime) or rows sit `ready` past their secret expiry and terminal-401 silently.

**What the node does today (and in rev 0.4):** runtime 401-age heuristic — if a 401 arrives on a row that's older than `ready_retention_secs`, chronicle reason is `terminal_http_401_likely_secret_expired` vs generic `terminal_http_401`. Works as telemetry; doesn't prevent the silent stall.

**Proposed systemic fix:** the heartbeat response (`/api/v1/node/heartbeat`) carries a `wire_parameters` block snapshot of economic_parameters the node reasonably needs. Probably:

```json
{
  ...,
  "wire_parameters": {
    "fill_job_ttl_secs": 1800,
    "callback_secret_grace_secs": 300,
    "queue_mirror_staleness_s": 90,
    "node_heartbeat_staleness_s": 180,
    "compute_job_timeout_s_per_queue_position": 600
  }
}
```

Node stores last-known values under `AuthState.wire_parameters` (or similar), uses them for correctness-adjacent invariants (e.g., asserting `ready_retention_secs + backoff_cap ≤ fill_job_ttl_secs + grace`). Other phases benefit automatically — staleness thresholds already matter for the market-surface filter, node_heartbeat staleness for match.

**Why 100-year:** today every economic_parameter Wire tunes requires node-side hard-coding or guesswork. Opaque defaults drift silently. An explicit parameter-snapshot-on-heartbeat channel solves it once, for every future phase.

**Migration scope on your side:** one new field on heartbeat response payload; economic_parameter reads Wire already does for its own RPCs are reused. Probably ~30 lines in `heartbeat/route.ts`. Non-breaking for existing nodes (they ignore the field).

---

## 2. Retryability as explicit protocol data, not hardcoded HTTP enum

**Gap:** Node rev 0.4 maintains `const TERMINAL_HTTP_CODES: &[u16] = &[400, 401, 403, 404, 409, 410, 413]`. Any time you add a new SQLSTATE→HTTP mapping in `classifyComputeRpcError` (currently `compute-errors.ts:104-140`), the node's terminal list drifts. Today we had to extend 409 because your P0409/P0410/P0411/P0412 handlers all return 409 via the classifier — and if a P0404 return path ever bypasses the callback route's 2xx `already_settled` short-circuit (`callback/route.ts:499-500`), 404 becomes terminal correctly in rev 0.4 but we're still enumerating by SQLSTATE-code-knowledge rather than protocol intent.

**Proposed systemic fix:** Wire callback responses (and ideally every node-facing response) include an explicit retry-intent signal. Two shapes, your choice:

- **Header form:** `X-Wire-Retry: never | transient | backoff-then-retry`. Parallel to the existing `X-Wire-Reason` header pattern on 503s.
- **Body form:** `{..., "retryable": false}` on non-2xx responses.

Node reads the explicit signal first, falls back to heuristics only if absent. Decoupled from HTTP-code enumeration.

**Why 100-year:** same as §1 — opaque protocol-as-magic-numbers is a drift trap. Explicit protocol-as-data survives classifier evolution on your side without node-side edits.

**Migration scope:** a couple of lines in each response helper in `compute-errors.ts`. You already differentiate retryable/terminal internally in your classifier; this just surfaces that decision.

---

## 3. Chronicle event naming collision: `compute_result_delivered`

**Gap:** Wire emits `compute_result_delivered` when the Wire→requester hop succeeds (`wire/maintenance/tick/route.ts:319`). The node planned to emit the same event name for the node→Wire hop. Different semantic hops; cross-system observability queries that UNION both chronicles would double-count or conflate.

**What the node did:** renamed to `market_result_delivered_to_wire` on the node side, staying within the existing `compute_chronicle.rs` `market_*` namespace. Avoids the collision unilaterally.

**Systemic question for you:** would it be cleaner if Wire renamed its event to `compute_result_forwarded_to_requester` (more accurate — Wire is the forwarder, not the delivery origin) so the two sides both have semantically-precise names? Single unilateral rename on your side; we've already done ours. If you agree, one-line change on your side; if not, node-side is fine as-is.

**Node-side taxonomy for your awareness** (rev 0.4):

- `market_result_delivered_to_wire` — node POSTed callback, Wire returned 2xx.
- `market_result_delivery_cas_lost` — POST 2xx but node's row-flipped-by-sweep during POST. Observability-only.
- `market_result_delivery_attempt_failed` — transient failure (5xx, network, 503+Retry-After). Still retrying.
- `market_result_delivery_failed` — terminal. `reason` enum includes `terminal_http_401_likely_secret_expired`, `orphaned_by_migration`, `callback_url_validation_failed`, `envelope_parse_failed`, `callback_auth_token_invalid`, `terminal_http_4xx`, `max_attempts`.
- `market_delivery_task_panicked` / `market_delivery_task_exited` — supervisor lifecycle.

---

## 4. Contract §2.3 `error.code` enumeration is unbounded

**Gap:** Contract §2.3 lists `"code": "model_timeout|model_error|..."` with a trailing ellipsis. Wire's `mapFailureCodeToReason` switch at `callback/route.ts:528` currently only enumerates `model_timeout` and `model_error`; everything else defaults to `execution_error`. Node rev 0.4 ships a classifier emitting: `worker_heartbeat_lost`, `model_timeout`, `oom`, `invalid_messages`, `model_error` (default). These are substring-matched from the underlying LLM provider's error string.

**Proposed systemic fix:** pin the canonical enum in contract §2.3 — the codes you'd be willing to operate against for provider reputation / retry classification in the future. Node-side classifier extends to match; new codes require bilateral agreement.

**Why it matters for operator UX:** today every failure is observability-flat. If Wire wants to penalize providers that emit lots of `oom` vs `worker_heartbeat_lost` differently in provider reputation scoring (a future phase), you need a pinned enum or the scoring becomes regex-on-message-strings. That's bad.

**Scope:** zero-code contract doc edit + a small `mapFailureCodeToReason` switch expansion when you're ready.

---

## 5. Wire's own expiry-sweep doesn't reconcile timed-out provider jobs

**Gap (surfaced during this investigation):** all three test jobs today are `status='filled' OR 'failed'` with `delivery_status='pending'` and `delivery_attempts=0`. One of them has `completed_at` set and `timeout_at` 10+ hours in the past. Wire's own maintenance sweep isn't transitioning `delivery_status='pending' → 'expired_undelivered'` for rows where:

- `timeout_at < now()` AND
- `delivery_status = 'pending'` AND
- `completed_at IS NULL` (or set-but-never-delivered, depending on your definition)

Without this, the node's eventual successful delivery (after this phase ships) would CAS-lose for every pre-this-phase stuck row — Wire would report "already_settled" (or worse, "not_found") because the intermediate state never resolved. We'll chronicle it correctly node-side, but operators looking at Wire's `delivery_status` see stale phantoms forever.

**Proposed fix:** extend the existing `wire-maintenance-compute` cron (the delivery+transit_retention sweep) to transition stale `pending` rows to `expired_undelivered` when `timeout_at + grace_window < now()`. The `wire_compute_jobs_delivery_status_check` constraint already allows `expired_undelivered` per the `\d wire_compute_jobs` we pulled earlier.

**Scope:** Small SQL update in your sweep cron. One concern: the grace_window needs to account for the node's max delivery attempt budget (`ready_retention_secs + backoff_cap × max_attempts` ≈ 2000s default). Or you defer grace to "ready_retention_secs-equivalent" on the Wire side and accept that some late-but-legitimate deliveries would land on an already-expired Wire row.

Operator-facing implication: the node-side `market_result_delivery_failed` chronicle with `reason: "terminal_http_404"` becomes the canonical "Wire forgot about this job" signal. We emit that today. Confirm you're comfortable with that split.

---

## What the node is shipping (no Wire coordination needed)

Contract-conformant with rev 1.5 §2.1-§2.5. No protocol changes. New node-side migration only.

- **Delivery worker** — the missing hop. Nudge+tick-driven, bounded-parallel, CAS-disciplined, supervisor-wrapped. POSTs bare-tagged-enum `MarketAsyncResult` (confirmed as the persisted shape at `server.rs:3957-3966`) into Wire's canonical §2.3 envelope via pure-function adapter using `row.job_id` (UUID per §10.5).
- **Pre-existing `spawn_market_worker` failure-branch bug** — fixed. `fleet_outbox_bump_delivery_attempt`'s `WHERE status='ready'` was silently no-op'ing against still-`pending` rows on inference failure. Spec now promotes pending→ready with the real error envelope, then lets the new worker deliver.
- **Pre-existing `fleet_outbox_mark_failed_if_ready` sweep-vs-lease race** — fixed. Sweep's CAS now includes `AND (delivery_lease_until IS NULL OR delivery_lease_until < now)` so an in-flight POST isn't yanked mid-flight.
- **5 new columns on `fleet_result_outbox`**: `callback_auth_token`, `delivery_lease_until`, `delivery_next_attempt_at`, `inference_latency_ms`, `request_id`. Additive, `fleet_*` rows stay unaffected.
- **New `pyramid_schema_versions` table** — migration apply-time tracking. Zero-ceremony primitive the codebase lacked.
- **ConfigSynced hot-reload listener** for `market_delivery_policy` — closes the analogous half-shipped gap we created by adding new tunable policy fields.
- **Frontend**: `DeliveryHealth` badge on Compute Offer Manager + per-job `delivery` status column on Compute Market Dashboard. Token always redacted server-side in the IPC + HTTP/CLI companion route (per `feedback_agent_first_cli`).

~20 tests covering every race window + envelope edge case + security invariant (Debug redaction, control-char token rejection, redirect policy, error-truncation enforcement).

---

## Coordination asks (non-blocking, ranked)

1. **§1 heartbeat response `wire_parameters` block** — highest leverage. Benefits every future phase.
2. **§5 Wire-side expiry-sweep for `delivery_status='pending'`** — needed for operator-surface parity; node-side catches the symmetric failure via `terminal_http_404` reason but Wire's `wire_compute_jobs.delivery_status` stays phantom.
3. **§2 explicit retry-intent signal** — pays back over every node update cycle.
4. **§3 chronicle naming** — cosmetic; one-line rename on your side if you agree.
5. **§4 `error.code` enum pinning** — contract-only, cheap.

None of these gate the node-side ship. If you want to track any as your own workstreams, open them on your side; we'll keep the node-side chronicle naming backwards-compatible if you later rename yours.

---

## Contact

- Adam relays paste-backs between threads. Messages to you should be self-contained.
- Node-side spec at `docs/plans/compute-market-phase-3-provider-delivery-spec.md` (rev 0.4) has the full verified evidence trail — every CRITICAL in rev 0.4 was directly source-code-verified after the audit flagged it.
- Purpose frame still: **GPU-less tester runs `pyramid_build`, other nodes' GPUs serve inference, tester never sees a market word.** This phase closes the last protocol gap. Everything after this is polish + operational hardening.
