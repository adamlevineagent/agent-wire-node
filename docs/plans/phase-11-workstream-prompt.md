# Workstream: Phase 11 — OpenRouter Broadcast Webhook + Fail-Loud Reconciliation

## Who you are

You are an implementer joining an active 17-phase initiative. Phases 0a, 0b, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10 are shipped. You are the implementer of Phase 11, which adds the OpenRouter Broadcast webhook receiver as a **required second-channel integrity confirmation** for the synchronous cost path, plus fail-loud discrepancy handling and provider health tracking.

Phase 11 is substantial backend work. No frontend — the DADBEAR Oversight Page that surfaces this data is Phase 15.

## Context

Phase 3 established the provider registry with pricing data. Phase 6 added `pyramid_step_cache` with per-call cost metadata. The codebase already has `pyramid_cost_log` with a synchronous cost path: OpenRouter returns `usage.cost` directly in the chat completions response, and Wire Node reconciles that into `pyramid_cost_log.actual_cost` before returning the response to the caller. **That primary path is already working.** Phase 11 adds the second channel.

The three leaks Broadcast catches (per the spec):

1. **Credential exfiltration** — someone copies the user's OpenRouter API key and makes calls from elsewhere. Wire Node's synchronous path only sees its own calls. Broadcast surfaces phantom calls as **orphan broadcasts** (a broadcast with a `trace.metadata.build_id` that has no matching local cost_log row).
2. **Missing confirmations** — if Wire Node made 500 calls but only 400 broadcasts arrived, the missing 100 point at either a provider-side accounting bug or a local tunnel/handler outage. Either way the user needs to know.
3. **Cost drift** — if synchronous `usage.cost` disagrees with the broadcast's post-hoc cost beyond a configurable threshold, that's a provider reconciliation bug worth fail-loud surfacing.

Broadcast is **dashboard-configured** on OpenRouter's side — users enable it in Settings > Observability and add a Webhook destination pointing at Wire Node's tunnel URL. Phase 11 does NOT programmatically configure Broadcast (the OpenRouter API doesn't expose that). Wire Node's Settings UI (Phase 15) will provide a "Copy webhook URL" button.

## Required reading (in order)

### Spec docs

1. `docs/handoffs/handoff-2026-04-09-pyramid-folders-model-routing.md` — deviation protocol.
2. **`docs/specs/evidence-triage-and-dadbear.md` — read Parts 3/4 in full.** Your primary implementation contract. Sections: `pyramid_cost_log Schema` (~line 440), Cost Reconciliation Guarantees (~line 476), Provider Health Alerting (~line 575), Part 4: OpenRouter Broadcast (~line 674) through the end.
3. Scan `docs/specs/provider-registry.md` for the `pyramid_providers` schema and the existing `augment_request_body` method on `LlmProvider` (Phase 3's spec). Phase 11 extends both.
4. `docs/plans/pyramid-folders-model-routing-full-pipeline-observability.md` — Phase 11 section.
5. `docs/plans/pyramid-folders-model-routing-implementation-log.md` — scan Phase 3 entry for provider registry shape and Phase 6 entry for StepContext + cost hook patterns.

### Code reading

6. **`src-tauri/src/pyramid/provider.rs`** — Phase 3's `LlmProvider` trait + `OpenRouterProvider`. Find `augment_request_body` — Phase 11 extends it to inject the full `trace.metadata` block (pyramid_slug, build_id, step_name, depth, chain_id, layer, check_type). Phase 6 already threads StepContext here; Phase 11 just wires more metadata fields.
7. **`src-tauri/src/pyramid/llm.rs`** — Phase 6's cache-aware LLM call path. Find where `pyramid_cost_log` rows are written on successful response parse. Phase 11 adds the `broadcast_confirmed_at` column population path.
8. **`src-tauri/src/pyramid/db.rs`** — find `pyramid_cost_log` table definition + CRUD. Phase 11 adds columns for Broadcast tracking. Also find `pyramid_providers` — Phase 11 adds health columns there.
9. **`src-tauri/src/server.rs`** — the HTTP server (not the Tauri IPC). Phase 11 adds a new route `POST /hooks/openrouter`. Grep for existing route registration patterns (`axum` or `warp` — check which the codebase uses).
10. **`src-tauri/src/pyramid/tunnel.rs`** (if exists) — the Cloudflare tunnel integration. Phase 11 doesn't modify this but the webhook route depends on the tunnel being live.
11. `src-tauri/src/main.rs` — find the IPC command block. Phase 11 adds 2-3 new IPC commands for provider health.
12. `src-tauri/src/pyramid/config_contributions.rs` — Phase 4's dispatcher. The `dadbear_policy` sync branch should consume the new `cost_reconciliation` fields from the policy YAML (discrepancy_ratio, provider_degrade_count, provider_degrade_window_secs). Thread these into whatever the runtime reads.

## What to build

### 1. Schema additions (`db.rs`)

Add new columns to existing tables:

```sql
-- pyramid_cost_log additions
ALTER TABLE pyramid_cost_log ADD COLUMN broadcast_confirmed_at TEXT;
ALTER TABLE pyramid_cost_log ADD COLUMN broadcast_payload_json TEXT;  -- full OTLP span for audit
ALTER TABLE pyramid_cost_log ADD COLUMN broadcast_cost_usd REAL;       -- cost from broadcast; compare to actual_cost
ALTER TABLE pyramid_cost_log ADD COLUMN broadcast_discrepancy_ratio REAL;  -- |bc - ac| / ac when both present

-- pyramid_providers additions (for health)
ALTER TABLE pyramid_providers ADD COLUMN provider_health TEXT NOT NULL DEFAULT 'healthy';
ALTER TABLE pyramid_providers ADD COLUMN health_reason TEXT;
ALTER TABLE pyramid_providers ADD COLUMN health_since TEXT;
ALTER TABLE pyramid_providers ADD COLUMN health_acknowledged_at TEXT;
```

Use the idempotent `ALTER TABLE` pattern (try-and-ignore-on-duplicate) from prior phases.

New table for orphan broadcasts:

```sql
CREATE TABLE IF NOT EXISTS pyramid_orphan_broadcasts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    received_at TEXT DEFAULT (datetime('now')),
    generation_id TEXT,                       -- OpenRouter gen-xxx from the trace, if present
    session_id TEXT,                           -- trace.metadata.session_id ("{slug}/{build_id}")
    pyramid_slug TEXT,                         -- trace.metadata.pyramid_slug (may not match any local slug)
    build_id TEXT,                             -- trace.metadata.build_id
    step_name TEXT,                            -- trace.metadata.step_name
    model TEXT,                                -- gen_ai.request.model
    cost_usd REAL,
    payload_json TEXT NOT NULL                 -- full span payload for audit
);
CREATE INDEX IF NOT EXISTS idx_orphan_broadcasts_generation
    ON pyramid_orphan_broadcasts(generation_id);
CREATE INDEX IF NOT EXISTS idx_orphan_broadcasts_received
    ON pyramid_orphan_broadcasts(received_at);
```

### 2. `augment_request_body` extensions (`provider.rs`)

Phase 3 already set the foundation. Phase 11 extends `OpenRouterProvider::augment_request_body` to inject the full `trace` object:

```rust
pub struct RequestMetadata {
    pub pyramid_slug: Option<String>,
    pub build_id: Option<String>,
    pub step_name: Option<String>,
    pub depth: Option<i64>,
    pub chain_id: Option<String>,
    pub layer: Option<i64>,
    pub check_type: Option<String>,
    pub node_identity: Option<String>,
}

fn augment_request_body(&self, body: &mut serde_json::Value, metadata: &RequestMetadata) {
    // Phase 11: inject trace.metadata for Broadcast correlation
    let trace = serde_json::json!({
        "metadata": {
            "pyramid_slug": metadata.pyramid_slug,
            "build_id": metadata.build_id,
            "step_name": metadata.step_name,
            "depth": metadata.depth,
            "chain_id": metadata.chain_id,
            "layer": metadata.layer,
            "check_type": metadata.check_type,
        }
    });
    if let Some(obj) = body.as_object_mut() {
        obj.insert("trace".to_string(), trace);
        if let (Some(slug), Some(build_id)) = (&metadata.pyramid_slug, &metadata.build_id) {
            obj.insert("session_id".to_string(), serde_json::json!(format!("{slug}/{build_id}")));
        }
        if let Some(identity) = &metadata.node_identity {
            obj.insert("user".to_string(), serde_json::json!(identity));
        }
    }
}
```

Thread `RequestMetadata` through Phase 6's StepContext → the call path → `augment_request_body`. Phase 6 already has StepContext with `slug`, `build_id`, `step_name`, `depth`, `chunk_index` — just map them to RequestMetadata and pass through.

### 3. Webhook route (`server.rs`)

Add a new HTTP route:

```
POST /hooks/openrouter
Content-Type: application/json
Body: OTLP JSON trace payload
```

Implementation:

1. Parse the OTLP JSON structure (`resourceSpans[].scopeSpans[].spans[]`)
2. For each span, extract attributes into a `BroadcastTrace` struct:
   - `generation_id` from `trace.metadata.generation_id` or span `traceId` (check the spec's attribute conventions)
   - `session_id` from `session.id` attribute (format: `"{slug}/{build_id}"`)
   - `pyramid_slug`, `build_id`, `step_name`, `depth`, `chain_id` from `trace.metadata.*`
   - `model` from `gen_ai.request.model`
   - `cost_usd` from `gen_ai.usage.cost` if present (OpenRouter-specific attribute)
   - `prompt_tokens`, `completion_tokens` from `gen_ai.usage.*`
3. Correlate the trace against `pyramid_cost_log`:
   - Primary correlation: `WHERE generation_id = ?`
   - Fallback: `WHERE session_id = ? AND step_name = ?` (if no generation_id yet, e.g., the row was written after parse failure)
4. If a matching row is found:
   - Set `broadcast_confirmed_at = now()`
   - Set `broadcast_cost_usd = <trace cost>`
   - Compute `broadcast_discrepancy_ratio = |bc - ac| / ac` if both present
   - If ratio > policy threshold, set `reconciliation_status = 'discrepancy'` and emit `CostReconciliationDiscrepancy` event via `BuildEventBus`
5. If no matching row is found:
   - Insert into `pyramid_orphan_broadcasts` with full payload for audit
   - Log a WARN with the span's slug/build_id/step_name
6. Return `200 OK` to OpenRouter so it doesn't retry

### 4. Webhook auth

The route MUST validate a secret before processing. Options per the spec:

- Header-based: `X-Webhook-Secret: <secret>` — OpenRouter's custom-headers support lets users configure this in the Broadcast destination
- Path-based: `/hooks/openrouter/<token>` — token embedded in the URL, user copies it from Wire Node's settings and pastes into OpenRouter's webhook URL field

Pick header-based (cleaner, logs don't leak the secret) and store the secret in `pyramid_providers.broadcast_config_json` (already defined in Phase 3).

On request:
1. Extract `X-Webhook-Secret` header
2. Look up the OpenRouter provider row, parse `broadcast_config_json`, get the expected secret
3. Constant-time compare with `subtle::ConstantTimeEq` (or whatever crypto crate is already in deps)
4. Return `401 Unauthorized` on mismatch, do not log the secret value
5. Return `503 Service Unavailable` if no secret configured yet (graceful handling during first-time setup)

### 5. Leak detection sweep

A background task that periodically (say, every 5 minutes) walks `pyramid_cost_log`:

```sql
UPDATE pyramid_cost_log
SET reconciliation_status = 'broadcast_missing'
WHERE reconciliation_status = 'synchronous'
  AND broadcast_confirmed_at IS NULL
  AND created_at < datetime('now', '-10 minutes')  -- grace period from policy
```

The grace period comes from `dadbear_policy.cost_reconciliation.broadcast_grace_period_secs` (new field, default 600). If the user's policy says `broadcast_required: false`, the sweep skips.

Spawn this task from `main.rs` setup alongside the other background loops. Use the same cancellation token pattern Phase 0b used for DADBEAR extend.

### 6. Provider health state machine

Add to `provider.rs` (or a new `provider_health.rs`):

```rust
pub async fn record_provider_error(
    conn: &Connection,
    provider_id: &str,
    error_kind: ProviderErrorKind,  // http_5xx | connection_failure | cost_discrepancy
    policy: &DadbearPolicy,
) -> Result<()> {
    // Based on error_kind + policy thresholds (provider_degrade_count, provider_degrade_window_secs):
    // - If 3+ cost_discrepancy in 10 min: set provider_health = 'degraded'
    // - If 3+ http_5xx: set provider_health = 'degraded'
    // - If connection_failure: set provider_health = 'down'
    // Update health_reason, health_since
    // Emit ProviderHealthChanged event via BuildEventBus
}

pub async fn acknowledge_provider_health(
    conn: &Connection,
    provider_id: &str,
) -> Result<()> {
    // Set provider_health = 'healthy', health_acknowledged_at = now()
    // Does NOT clear health_reason (keeps audit trail)
}
```

The LLM call path (Phase 3's `call_model_via_registry` or `call_model_unified_with_options_and_ctx`) should:
- On HTTP 5xx response → call `record_provider_error(ProviderErrorKind::Http5xx)`
- On connection failure → call `record_provider_error(ProviderErrorKind::ConnectionFailure)`
- On cost discrepancy detection (in the webhook handler) → call `record_provider_error(ProviderErrorKind::CostDiscrepancy)`

The provider resolver should log a WARN on every resolution against a non-`healthy` provider. No traffic rerouting — this is user signal, not auto-failover.

### 7. IPC commands

New commands in `main.rs`:

```rust
#[tauri::command]
async fn pyramid_provider_health(state: State<'_, PyramidState>) -> Result<Vec<ProviderHealthEntry>, String>

#[tauri::command]
async fn pyramid_acknowledge_provider_health(
    state: State<'_, PyramidState>,
    provider_id: String,
) -> Result<(), String>
```

Shape per the spec's Part 2 → Provider Health Alerting section (~line 599).

Optional (for Phase 15 debugging): `pyramid_list_orphan_broadcasts` that returns the `pyramid_orphan_broadcasts` table. Not strictly required for Phase 11 to ship but useful.

### 8. Tests

- `test_augment_request_body_injects_trace_metadata` — verify the full metadata shape
- `test_webhook_parses_otlp_payload` — feed a sample OTLP JSON, verify extraction
- `test_webhook_correlates_to_cost_log_by_generation_id` — insert a cost_log row, send a matching broadcast, verify `broadcast_confirmed_at` is set
- `test_webhook_correlates_to_cost_log_by_session_id_fallback` — same but with generation_id missing
- `test_webhook_orphan_detection` — send a broadcast with no matching cost_log row, verify it lands in `pyramid_orphan_broadcasts`
- `test_webhook_auth_rejects_missing_secret` — POST without `X-Webhook-Secret`, expect 401
- `test_webhook_auth_rejects_wrong_secret` — POST with wrong secret, expect 401
- `test_discrepancy_detection_fires_event` — mock a discrepancy beyond the ratio threshold, verify `reconciliation_status = 'discrepancy'` and event emission
- `test_leak_detection_flags_unconfirmed_rows` — insert an old synchronous row, run the sweep, verify `reconciliation_status = 'broadcast_missing'`
- `test_provider_health_degrades_on_3_discrepancies` — simulate 3 discrepancies within the window, verify `provider_health = 'degraded'`
- `test_provider_health_acknowledged_clears_alert` — degrade then acknowledge, verify state
- `test_provider_health_ipc_returns_full_state`

## Scope boundaries

**In scope:**
- `pyramid_cost_log` column additions (broadcast_confirmed_at, broadcast_payload_json, broadcast_cost_usd, broadcast_discrepancy_ratio)
- `pyramid_providers` column additions (provider_health, health_reason, health_since, health_acknowledged_at)
- `pyramid_orphan_broadcasts` table + CRUD
- `augment_request_body` extension for trace.metadata + session_id + user
- `POST /hooks/openrouter` HTTP route with OTLP JSON parsing
- Correlation logic (gen_id primary, session_id fallback)
- Discrepancy detection + fail-loud event emission
- Leak detection background sweep (cancellation-safe)
- Provider health state machine + error recording hooks
- `pyramid_provider_health` + `pyramid_acknowledge_provider_health` IPC commands
- Tests

**Out of scope:**
- DADBEAR Oversight Page frontend (Phase 15)
- "Copy webhook URL" Settings UI button (Phase 15)
- Pause/resume all DADBEAR IPC (Phase 15)
- `pyramid_cost_reconciliation` aggregate IPC (Phase 15)
- Manual webhook-secret generation UI (Phase 15)
- Cross-provider fallback routing based on health (spec says health is a signal, NOT auto-failover)
- `pyramid_list_orphan_broadcasts` IPC (ship only if trivial, otherwise defer to Phase 15)
- The 7 pre-existing unrelated test failures

## Verification criteria

1. `cargo check --lib`, `cargo build --lib` — clean, zero new warnings.
2. `cargo test --lib pyramid` — 1048+ passing + new Phase 11 tests. Same 7 pre-existing failures.
3. Grep sanity: `grep -n "broadcast_confirmed_at" src-tauri/src/pyramid/` — shows the column write + the leak sweep query.
4. Grep sanity: `grep -n "provider_health" src-tauri/src/pyramid/` — shows the state machine + the degrade path.
5. Manual verification path (document in log): a curl command that POSTs a sample OTLP JSON to `localhost:{tunnel_port}/hooks/openrouter` and verifies the cost_log row is correlated OR orphan row is written.

## Deviation protocol

Standard. Most likely deviations:

- **No `subtle` crypto crate in deps.** Use `ring::constant_time::verify_slices_are_equal` or a manual constant-time byte compare with `std::hint::black_box`. Don't use `==` on the secret.
- **OTLP JSON attribute key divergence.** The spec shows `trace.metadata.pyramid_slug` etc. but OpenRouter's actual output may use different key paths. The spec's attribute convention table (~line 756) is the reference. If reality differs, flag and adjust the parser.
- **`server.rs` uses axum vs warp vs actix.** Whatever the existing HTTP server uses, match it. Don't add a new framework.
- **`session_id` format differences.** The spec says `{slug}/{build_id}`. Phase 3's augment_request_body already sets this. Verify consistency.
- **Policy fields on `dadbear_policy` YAML.** The spec adds new fields (discrepancy_ratio, provider_degrade_count, etc.) that may not be in Phase 4/9's seed YAML. Extend the bundled seed if needed, or hardcode defaults in the policy loader with a TODO pointing at Phase 12/15.

## Implementation log protocol

Append Phase 11 entry to `docs/plans/pyramid-folders-model-routing-implementation-log.md`. Document the schema additions, augment_request_body extension, webhook route, auth logic, correlation algorithm, leak sweep, health state machine, IPC commands, tests, and verification results. Status: `awaiting-verification`.

## Mandate

- **No auto-correction.** Discrepancies fire loud events and set flags. Do NOT silently update `cost_per_token` or rewrite `actual_cost` to match broadcast. The user wants to know the synchronous ledger is wrong, not have it silently corrected.
- **No auto-failover.** Provider health is a signal. Do NOT change provider selection automatically based on health state.
- **Webhook auth is mandatory.** A publicly-exposed webhook without auth is a leak attack surface. Use constant-time comparison.
- **No backend test infrastructure sprawl.** Use the existing Rust test patterns. No new test framework.
- **Fix all bugs found.** Standard.
- **Commit when done.** Single commit with message `phase-11: openrouter broadcast webhook + fail-loud reconciliation`. Body: 5-7 lines summarizing schema + augment + webhook + auth + leak sweep + health + IPC. Do not amend. Do not push.

## End state

Phase 11 is complete when:

1. New columns exist on `pyramid_cost_log` and `pyramid_providers`.
2. `pyramid_orphan_broadcasts` table exists with CRUD.
3. `augment_request_body` injects the full `trace.metadata` + `session_id` + `user`.
4. `POST /hooks/openrouter` route is registered and auth-gated.
5. OTLP JSON parser extracts attributes per spec convention.
6. Correlation logic handles both gen_id and session_id paths.
7. Discrepancy detection fires `CostReconciliationDiscrepancy` events.
8. Leak detection background sweep marks stale rows `broadcast_missing`.
9. Provider health state machine degrades/acknowledges per spec.
10. 2 new IPC commands registered.
11. `cargo check`, `cargo build`, `cargo test --lib pyramid` pass.
12. Implementation log Phase 11 entry complete.
13. Single commit on branch `phase-11-openrouter-broadcast`.

Begin with the spec (Parts 3/4 in full) and the existing server.rs HTTP server patterns.

Good luck. Build carefully.
