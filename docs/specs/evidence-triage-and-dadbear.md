# Evidence Triage & DADBEAR Stabilization Specification

**Version:** 1.0
**Date:** 2026-04-09
**Status:** Design — pre-implementation
**Depends on:** Provider registry (for tier routing), generative config pattern (for policy YAML)
**Authors:** Adam Levine, Claude (session design partner)

---

## Part 1: DADBEAR In-Flight Lock (Pipeline B tick guard)

### Scope

**This lock guards Pipeline B only — the `dadbear_extend.rs` tick loop.** It prevents concurrent `run_tick_for_config` calls for the same config when the previous tick's chain dispatch takes longer than the scan interval. It is NOT the fix for the 200 → 528 L0 blowup symptom — that's Phase 2's change-manifest rewrite of `execute_supersession`. This lock addresses a distinct Pipeline B race that becomes live once Phase 0b wires the real `fire_ingest_chain`.

### Background: the two DADBEAR pipelines

The DADBEAR subsystem has two file-change pipelines with different responsibilities:

- **Pipeline A (`watcher.rs`, 2026-03-23)** — maintenance of already-ingested files. fs-notify events → `pyramid_pending_mutations` → `stale_engine.rs` polls (with per-layer debounce at `start_timer` line 328 and a runaway circuit breaker at `check_runaway` line 612) → `stale_helpers_upper.rs::execute_supersession` creates new node versions. This is the path that produces the 200 → 528 L0 cascade when `execute_supersession` INSERTs new nodes (line 1671) and cross-thread propagation at `stale_helpers_upper.rs:1327` writes more `confirmed_stale` mutations. Phase 2 fixes that.
- **Pipeline B (`dadbear_extend.rs`, 2026-04-08)** — creation/extension. Polling scanner → `pyramid_ingest_records` → `dispatch_pending_ingests` → `fire_ingest_chain`. The chain dispatch was stubbed in the original commit; Phase 0b replaces the stub with real dispatch. This Phase 1 lock exists because once real dispatch is wired, a tick can take minutes and the next 1-second base tick would otherwise start a concurrent dispatch for the same config.

### Problem this lock solves

After Phase 0b lands and `dispatch_pending_ingests` actually runs chain builds via `fire_ingest_chain`, a single tick can occupy several minutes. The tick loop checks configs every 1 second. Without an in-flight guard, a slow tick would not prevent subsequent ticks from launching concurrent dispatches for the same config, racing on:

- `pyramid_ingest_records` status transitions (pending → processing → complete)
- `LockManager` write lock acquisition order for the config's slug
- Chain executor state for the same (slug, build_id) combination
- Ingest event emission ordering (duplicate `IngestStarted` events for the same record)

None of these problems exist today because `dispatch_pending_ingests` at dadbear_extend.rs:401-408 stubs chain dispatch with `format!("dadbear-ingest-{}-{}", slug, uuid::Uuid::new_v4())` and returns immediately. This Phase 1 lock is therefore **defense-in-depth against a race that becomes live after Phase 0b**, not a fix for an observable bug in the current tree.

### NOT the source of the 200 → 528 L0 blowup

The L0 cascade originally described as a DADBEAR symptom is Pipeline A's problem, not Pipeline B's. It's produced by:

1. A source file change → `watcher.rs` writes a `pyramid_pending_mutations` row
2. `stale_engine.rs` picks it up, dispatches a stale check via `dispatch_node_stale_check`
3. `stale_helpers_upper.rs::execute_supersession` (lines 1387-1700+) INSERTs a new node at line 1671 with a fresh ID and sets `superseded_by` on the old node at line 1695
4. Cross-thread propagation at `stale_helpers_upper.rs:1327` writes additional `confirmed_stale` rows to `pyramid_pending_mutations` for related threads
5. The stale engine picks up those new mutations, which trigger more supersessions, which trigger more propagations
6. Result: a single file change cascades into hundreds of new nodes

**Phase 2 (change-manifest supersession) is what breaks this cascade** by rewriting `execute_supersession` to do in-place updates instead of inserting new nodes. This Phase 1 lock cannot and does not affect that cascade — it lives in a different pipeline.

### Current Code (dadbear_extend.rs)

```
loop {
    sleep(1s)
    for config in configs {
        if now - ticker.last_tick >= ticker.interval {
            ticker.last_tick = now;
            run_tick_for_config(db_path, config, event_bus).await;  // NO IN-FLIGHT GUARD
        }
    }
}
```

### Fix

Add `HashMap<i64, Arc<AtomicBool>>` to the tick loop, keyed by `config.id`:

```rust
let mut in_flight: HashMap<i64, Arc<AtomicBool>> = HashMap::new();

// In the loop:
for config in &configs {
    let flag = in_flight.entry(config.id)
        .or_insert_with(|| Arc::new(AtomicBool::new(false)));

    if flag.load(Ordering::Relaxed) {
        debug!(slug = %config.slug, "DADBEAR: skipping tick, previous dispatch in-flight");
        continue;
    }

    if now.duration_since(ticker.last_tick) < ticker.interval {
        continue;
    }
    ticker.last_tick = now;

    flag.store(true, Ordering::Relaxed);
    let flag_clone = flag.clone();
    // Run the tick; clear flag on completion
    match run_tick_for_config(db_path, config, event_bus).await {
        Ok(()) => {}
        Err(e) => error!("DADBEAR tick failed: {e}"),
    }
    flag_clone.store(false, Ordering::Relaxed);
}
```

**Why AtomicBool instead of LockManager write lock**: The LockManager write lock is per-slug and would block queries. The in-flight flag is per-config and only prevents re-entrant ticks — queries continue unaffected.

### Files
- `src-tauri/src/pyramid/dadbear_extend.rs` — ~20 lines changed

### Verification

Phase 1 cannot be verified against the current tree because `dispatch_pending_ingests` is stubbed and returns immediately. After Phase 0b wires real dispatch, verification is:

1. Mock or temporarily extend `fire_ingest_chain` to block on a `tokio::time::sleep(30s)` future (or use a slow test chain that takes ≥30s to run end-to-end)
2. Enable DADBEAR on a test folder with `scan_interval_secs: 1`
3. Drop a new source file into the folder
4. Assert:
   - First tick observes the new file, writes an ingest record, enters `dispatch_pending_ingests`, blocks on the slow chain
   - Subsequent 1-second ticks observe `in_flight[config.id] == true`, log `"DADBEAR: skipping tick, previous dispatch in-flight"`, and continue without invoking `run_tick_for_config` for that config
   - Other configs (if any) are unaffected by the in-flight flag for this config
   - When the slow chain completes, `in_flight[config.id]` is cleared and the next tick proceeds normally
5. Unit test alternative: factor the tick iteration into a testable function that takes an injectable `run_tick_fn`, then assert re-entry behavior directly without needing real filesystem events

Verification is NOT expected to observe any change in `pyramid_pending_mutations` row counts or L0 node counts — those metrics live in Pipeline A, which this lock does not touch.

---

## Part 2: Evidence Triage

### Problem

A dumb numerical cap on evidence nodes is a Pillar 37 violation. The current system creates evidence for every question without considering:
- Whether the question is worth answering
- Whether the answer is stable enough to check infrequently
- What model tier can answer this reliably

### Solution: Triage Step

Evidence questions go through a **triage step** before answering:

```
Question arrives → Triage (cheap local LLM) → Route decision:
    → answer (with model_tier)
    → defer (check later, with interval)
    → skip (not worth answering)
```

The triage step is itself a cheap LLM call (local model, short context) that gates expensive evidence answering calls.

### LLM Call Integration: StepContext

**All LLM calls in the triage pipeline MUST receive a `StepContext`** (defined canonically in `llm-output-cache.md`). This gives them cache support, event emission, cost tracking, and force_fresh semantics uniformly with the rest of the build pipeline.

Specifically:

| LLM call site | StepContext values |
|---|---|
| `triage_evidence_question()` (per-question or per-batch) | `step_name = "evidence_triage"`, `primitive = "triage"`, `depth` = the target node's depth, `chunk_index` = `None` |
| `answer_evidence_question()` | `step_name = "evidence_answer"`, `primitive = "evidence_answering"`, `depth` = target depth |
| `dispatch_node_stale_check()` (per batch) | `step_name = "node_stale_check"`, `primitive = "stale_check"`, `depth` = upper layer depth |

The StepContext threads through from the caller (`evidence_answering.rs` for the first two, `stale_engine.rs` for the third). Each caller constructs the StepContext with a handle to `BuildEventBus` and the shared cache DB path. This ensures:

- Cache lookup before the LLM call (identical triage on the same question = cache hit)
- `LlmCallStarted` / `LlmCallCompleted` / `CacheHit` events emitted for build viz
- Cost accrual in `pyramid_cost_log` via the centralized helper
- Force-fresh path available when the triage policy changes and deferred questions re-evaluate

**Cache behavior for triage calls**: triage cache entries use the inputs_hash of `(question_text + target_node_distilled + policy_yaml_hash)`. If the policy changes, the policy_yaml_hash changes, the cache misses, and triage re-runs. This is the correct behavior for the policy-change re-evaluation path.

### Triage Policy YAML

The triage decision is driven by a policy YAML (generative config pattern). ALL values shown below (thresholds, windows, intervals, max_concurrent) flow from the user's `evidence_policy` generative config and are never hardcoded in implementation:

```yaml
schema_type: evidence_policy
version: 1

triage_rules:
  - condition: "first_build AND depth == 0"
    action: answer
    model_tier: fast_extract
    priority: normal

  - condition: "stale_check AND no_demand_signals"
    action: defer
    check_interval: "never"

  - condition: "stale_check AND has_demand_signals"
    action: answer
    model_tier: stale_local

  - condition: "evidence_question_trivial"
    action: skip

demand_signals:
  - type: agent_query_count
    threshold: 2
    window: "14d"
  - type: user_drill_count
    threshold: 1
    window: "7d"

budget:
  maintenance_model_tier: stale_local
  initial_build_model_tier: fast_extract
  max_concurrent_evidence: 3
```

### Triage Conditions (Built-in Vocabulary)

| Condition | True When |
|-----------|-----------|
| `first_build` | This is the initial build (no prior nodes at this depth) |
| `stale_check` | This is a DADBEAR-triggered stale update |
| `no_demand_signals` | No agent queries or user drills in the configured window |
| `has_demand_signals` | At least one demand signal threshold exceeded |
| `evidence_question_trivial` | The triage LLM classifies the question as trivial/obvious |
| `evidence_question_high_value` | The triage LLM classifies the question as high-value |
| `depth == N` | Evidence question is at specific depth |

### Demand Signal Tracking

```sql
CREATE TABLE IF NOT EXISTS pyramid_demand_signals (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    slug TEXT NOT NULL,
    node_id TEXT NOT NULL,
    signal_type TEXT NOT NULL,       -- "agent_query", "user_drill", "search_hit"
    source TEXT,                     -- agent name or "user"
    weight REAL NOT NULL DEFAULT 1.0, -- 1.0 at the leaf, attenuated on parents
    source_node_id TEXT,              -- original leaf node that caused propagation (for debugging)
    created_at TEXT DEFAULT (datetime('now'))
);
CREATE INDEX idx_demand_signals ON pyramid_demand_signals(slug, node_id, signal_type, created_at);
```

### Demand Signal Recording Points

| Signal Type | Recorded By | Location |
|------------|-------------|----------|
| `agent_query` | MCP server handler | When any MCP tool resolves and returns pyramid data for a node |
| `user_drill` | `query.rs` drill endpoint handler | When GET /pyramid/:slug/drill/:id is called |
| `search_hit` | `query.rs` search endpoint handler | When a node appears in search results AND the user subsequently drills into it (not just search) |

All recording is a fire-and-forget INSERT -- no locking, no blocking the query response.

### Demand Signal Query Path

### Query Path at Triage Time

When the triage step evaluates `has_demand_signals` for a question at node N:

```sql
SELECT SUM(weight) FROM pyramid_demand_signals
WHERE slug = ?slug
  AND node_id = ?node_id
  AND signal_type = ?signal_type
  AND created_at > datetime('now', ?window);
```

For each signal type in the policy's `demand_signals` array, run a weighted-sum query with the configured threshold. If ANY signal type's summed weight meets or exceeds its threshold, `has_demand_signals = true`.

The policy's `demand_signals[].threshold` is now a float (summed weight), not an integer count:

```yaml
demand_signals:
  - type: agent_query
    threshold: 2.0
    window: "-14 days"     # SQLite datetime modifier format
  - type: user_drill
    threshold: 1.0
    window: "-7 days"
```

### Propagation

Demand signals propagate upward with attenuation, mirroring evidence weight propagation. Demand at a leaf is weaker evidence of demand at its parent, but not zero.

- When a signal is recorded on node N, a weighted signal is also recorded on each parent of N (walked via the evidence graph — KEEP links).
- Attenuation factor: `0.5` per layer. A leaf signal of weight `1.0` becomes a parent signal of weight `0.5`, grandparent `0.25`, and so on.
- Attenuation floor: `0.1`. Once the attenuated weight would fall below this, stop propagating.
- Propagation is synchronous with the signal recording. Fire-and-forget, no locking.
- Each propagated row stores `source_node_id = original leaf node` so we can trace where a parent's demand came from.
- Policy evaluation of `has_demand_signals` uses `SUM(weight) >= threshold` (see Query Path above) rather than `COUNT(*) >= threshold`.

The attenuation behavior is itself a configurable policy field on `evidence_policy`:

```yaml
demand_signal_attenuation:
  factor: 0.5       # multiplicative per layer
  floor: 0.1        # below this, stop propagating
  max_depth: 6      # absolute cap to prevent runaway propagation on pathological graphs
```

### Propagation Loop Guard

The evidence graph SHOULD be a DAG, but bugs or manual data edits could introduce cycles. Propagation uses an explicit visited-set to prevent infinite loops on cycles:

```rust
pub fn propagate_demand_signal(
    conn: &Connection,
    slug: &str,
    node_id: &str,
    signal_type: &str,
    weight: f64,
    source_node_id: &str,
    policy: &EvidencePolicy,
) -> Result<()> {
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, f64, u32)> = VecDeque::new();
    queue.push_back((node_id.to_string(), weight, 0));

    let attenuation = &policy.demand_signal_attenuation;

    while let Some((current_node, current_weight, depth)) = queue.pop_front() {
        // Loop guard: skip if already visited
        if !visited.insert(current_node.clone()) {
            continue;
        }

        // Depth cap
        if depth > attenuation.max_depth {
            continue;
        }

        // Floor cap
        if current_weight < attenuation.floor {
            continue;
        }

        // Record the signal at this node
        insert_demand_signal(
            conn,
            slug,
            &current_node,
            signal_type,
            current_weight,
            source_node_id,
        )?;

        // Walk up via evidence graph (KEEP links)
        let parents = load_parents_via_evidence(conn, slug, &current_node)?;
        let next_weight = current_weight * attenuation.factor;
        let next_depth = depth + 1;
        for parent_id in parents {
            queue.push_back((parent_id, next_weight, next_depth));
        }
    }

    Ok(())
}
```

**Properties**:
- `visited` set prevents cycles from re-entering
- `max_depth` hard cap prevents runaway on pathological graphs
- `floor` cap prevents negligible-weight signals from being recorded
- BFS order ensures parents at the same depth are recorded before grandparents (natural aggregation order)
- Fire-and-forget from the caller's perspective — the caller doesn't await propagation completion (runs in a background task)

### Integration Point

In `evidence_answering.rs`, before `answer_questions()`:

```rust
// For each question in the batch:
let triage_result = triage_evidence_question(
    &policy, &question, &demand_signals, is_first_build
).await?;

match triage_result.action {
    TriageAction::Answer { model_tier } => {
        // proceed with answering using the specified tier
    }
    TriageAction::Defer { check_interval } => {
        // record deferred question with next_check_at
        defer_evidence_question(db, slug, &question, check_interval)?;
    }
    TriageAction::Skip => {
        // log skip reason, don't create evidence node
    }
}
```

### pyramid_deferred_questions Schema

Deferred evidence questions are stored for later re-check. The DADBEAR tick scans this table for expired deferrals.

```sql
CREATE TABLE IF NOT EXISTS pyramid_deferred_questions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    slug TEXT NOT NULL,
    question_id TEXT NOT NULL,          -- the evidence question identifier
    question_json TEXT NOT NULL,        -- full question payload (text, target_node_id, layer, etc.)
    deferred_at TEXT NOT NULL DEFAULT (datetime('now')),
    next_check_at TEXT NOT NULL,        -- computed from check_interval
    check_interval TEXT NOT NULL,       -- "7d", "30d", "never", "on_demand" (from policy)
    triage_reason TEXT,                 -- why it was deferred (from triage LLM)
    contribution_id TEXT,               -- which evidence_policy contribution deferred it (for audit)
    UNIQUE(slug, question_id)
);
CREATE INDEX idx_deferred_questions_next ON pyramid_deferred_questions(slug, next_check_at);
CREATE INDEX idx_deferred_questions_interval ON pyramid_deferred_questions(check_interval);
```

### Deferred Question Re-Check Flow

The DADBEAR tick includes a deferred question scanner that:

1. Runs once per tick cycle (after `dispatch_pending_ingests`)
2. Queries: `SELECT * FROM pyramid_deferred_questions WHERE slug = ?slug AND next_check_at <= datetime('now') AND check_interval != 'never'`
3. For each expired question: re-runs triage against the current active `evidence_policy` (which may have changed since the original deferral)
4. If triage now returns `answer`: moves the question to the active evidence queue
5. If triage returns `defer` again: updates `next_check_at` and `contribution_id` to the current policy
6. If triage returns `skip`: DELETE from the table

Questions with `check_interval = "never"` are only re-triggered by explicit demand signals (agent query or user drill on the target node). The demand signal handler checks for matching deferred questions and moves them to the active queue immediately — bypassing the time-based re-check.

Questions with `check_interval = "on_demand"` behave the same as `"never"`: only demand signals can reactivate them.

### Re-evaluation on Policy Change

Policy changes should apply immediately. Otherwise a user tightening their triage policy won't see the effect until the next natural re-check, which could be weeks away.

- When a new `evidence_policy` contribution is activated (supersedes the prior), immediately re-evaluate ALL deferred questions for affected pyramids against the new policy.
- **Implementation:** the contribution supersession handler in `config_contributions.rs` calls `reevaluate_deferred_questions(slug, new_policy)` after the operational table sync.
- Re-evaluation runs the triage step for each deferred question against the new policy (batched per the new policy's `triage_batch_size`).
- If triage now returns `answer`, the question moves to the active evidence queue.
- If triage returns `defer` with a different `check_interval`, `next_check_at` is recomputed and `contribution_id` is updated to the new policy.
- If triage returns `skip`, the question is `DELETE`d.
- Emit a `DeferredQuestionsReevaluated` event per affected slug:

  ```
  { slug, policy_contribution_id, evaluated: u64, activated: u64, skipped: u64, still_deferred: u64 }
  ```

**Manual trigger IPC** — for the ToolsMode policy editor "Apply to all deferred" button, and for re-running the re-evaluation on demand:

```
POST pyramid_reevaluate_deferred_questions
  Input: { slug: String }
  Output: { evaluated: u64, activated: u64, skipped: u64, still_deferred: u64 }
```

This command runs the same `reevaluate_deferred_questions` logic as the automatic on-supersession path.

---

## pyramid_cost_log Schema

Used by the primary (synchronous, in-response) cost reconciliation path, the optional Broadcast webhook reconciliation (Part 4), and the DADBEAR Oversight Page (Part 3). This table already exists in the codebase.

```sql
CREATE TABLE IF NOT EXISTS pyramid_cost_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    slug TEXT NOT NULL,
    operation TEXT NOT NULL,          -- "build_step", "stale_check", "evidence", "triage", "manifest"
    model TEXT NOT NULL,              -- canonical model ID (e.g. "inception/mercury-2.7-preview-03")
    input_tokens INTEGER,             -- estimated at INSERT, actual after response parsed
    output_tokens INTEGER,
    estimated_cost REAL,              -- computed from pricing_json before the call
    actual_cost REAL,                 -- authoritative; from usage.cost in response body for OpenRouter, 0 for Ollama
    actual_tokens_in INTEGER,        -- from response.usage.prompt_tokens
    actual_tokens_out INTEGER,       -- from response.usage.completion_tokens
    source TEXT,                      -- "chain_executor", "auto-stale", "triage", "manifest"
    layer INTEGER,
    check_type TEXT,
    chain_id TEXT,
    step_name TEXT,
    tier TEXT,
    latency_ms INTEGER,
    generation_id TEXT,               -- OpenRouter response.id, format "gen-xxxxxxxxxxxxxx"; locally-generated UUID for non-OpenRouter
    reconciled_at TEXT,               -- set at first reconciliation (usually synchronous from response body)
    reconciliation_status TEXT,       -- "synchronous" | "synchronous_local" | "broadcast" | "generation_api" | "estimated" | "discrepancy"
    estimated_cost_usd REAL,
    created_at TEXT DEFAULT (datetime('now'))
);
CREATE INDEX idx_cost_log_slug ON pyramid_cost_log(slug, created_at);
CREATE INDEX idx_cost_log_generation ON pyramid_cost_log(generation_id);
CREATE INDEX idx_cost_log_reconciliation ON pyramid_cost_log(reconciliation_status, created_at);
```

---

## Cost Reconciliation Guarantees

**Primary cost path is SYNCHRONOUS** — OpenRouter returns `usage.cost` directly in the chat completions response body (USD). Cost is reconciled before the LLM response is handed back to the caller. **Broadcast is a required second channel** that asynchronously confirms every synchronous entry. The full healthy path is `synchronous` → `broadcast_confirmed_at` set within the grace period. Missing confirmations trigger leak detection (Part 4 below).

### Reconciliation paths

| `reconciliation_status` | `broadcast_confirmed_at` | When it's set | Source of `actual_cost` | Health |
|---|---|---|---|---|
| `synchronous` | `NULL` | Immediately after OpenRouter response parsed; awaiting broadcast | `response.usage.cost` (USD) | Pending broadcast confirmation |
| `synchronous` | set | Broadcast trace arrived and matched this row; costs agree within tolerance | `response.usage.cost` | Healthy — fully confirmed |
| `synchronous_local` | N/A (skipped) | Immediately after Ollama/local response parsed | Always `0` | Healthy — local, not subject to Broadcast |
| `broadcast` | set | Primary path failed mid-call; broadcast arrived and supplied the cost | OTLP payload | Degraded but recovered |
| `generation_api` | set or `NULL` | Primary path failed; recovered via `GET /api/v1/generation?id=<gen-id>` | `total_cost` from the response | Degraded but recovered |
| `estimated` | `NULL` | Both primary and recovery paths failed | `NULL`; `estimated_cost` used as fallback for rollups | Degraded — surfaces in oversight |
| `discrepancy` | set | Synchronous + Broadcast both arrived and differ beyond policy threshold | Kept at synchronous value; flag raised | Red alert — fail-loud |
| `broadcast_missing` | `NULL` after grace period | Synchronous entry was never confirmed by an async broadcast; flagged by the leak audit | `response.usage.cost` (unconfirmed) | Red alert — leak suspected |

### generation_id Assignment Timing

```
1. Before LLM call:
   - For OpenRouter: no generation_id yet, INSERT row with { estimated_cost, generation_id: NULL }
   - For Ollama/local: generate a local UUID and INSERT directly
   - For custom OAI-compat: treat like OpenRouter (INSERT without ID first)
2. Make LLM call synchronously
3. Parse response body immediately on success:
   - OpenRouter: extract response.id (format "gen-xxxxxxxxxxxxxx")
   - Custom OAI-compat: extract response.id if present; else generate local UUID
   - Ollama: keep local UUID
4. UPDATE pyramid_cost_log:
     generation_id = <extracted or local>
     actual_cost = response.usage.cost (OpenRouter) | 0 (Ollama) | manual (custom)
     actual_tokens_in, actual_tokens_out from response.usage
     reconciled_at = now()
     reconciliation_status = "synchronous" | "synchronous_local" | "estimated"
5. Return LlmResponse to caller
```

This means every normal OpenRouter call has authoritative cost + reconciled row before the chain executor sees the result. No async dependency.

### Secondary path: `GET /api/v1/generation?id=<gen-id>`

If step 3 above fails (response parse error, partial body, connection drop after bytes started arriving), we may have captured `generation_id` but not `usage.cost`. The recovery path:

```
- Schedule a deferred fetch: GET /api/v1/generation?id=<gen-id>
- Poll with brief retry: 3 attempts at 1-second intervals (OpenRouter publishes stats within seconds)
- On success: UPDATE actual_cost, actual_tokens_*, reconciliation_status = "generation_api"
- On repeated failure: mark reconciliation_status = "estimated" and log WARN
```

This is also the path used by audit sweeps — an operator can re-verify historical rows against the provider's view.

### Broadcast webhook as cross-verification

Broadcast is now OPTIONAL and runs as a cross-verification layer (detailed in Part 4 below). When a trace arrives:

- If `reconciliation_status` is already `synchronous`: compare the Broadcast cost against the synchronous value. If they differ beyond the discrepancy threshold, set status to `discrepancy` and emit the fail-loud event. This catches provider-side cost recalculation bugs that would otherwise be invisible.
- If `reconciliation_status` is `estimated` (primary path failed): use Broadcast data to set actual values, status = `broadcast`.
- Broadcast arrival is asynchronous and unreliable (no retries, sampling may drop traces) — never depend on it for primary reconciliation.

### Unconfirmed rows: when it's a warning vs a failure

With both the synchronous primary path and the required Broadcast confirmation, a healthy row has `reconciliation_status = "synchronous"` AND `broadcast_confirmed_at IS NOT NULL` within the grace period. Deviations:

- **`reconciliation_status = "estimated"`** (both synchronous and recovery paths failed): rare but real. Surfaces as a warning in the oversight page. `estimated_cost` is used for rollups. Not a red alert by itself.
- **`reconciliation_status = "synchronous"` with `broadcast_confirmed_at IS NULL` inside the grace period**: normal — just waiting for async confirmation.
- **`reconciliation_status = "synchronous"` with `broadcast_confirmed_at IS NULL` past the grace period**: this is `broadcast_missing`. It IS a red alert (leak detection, Part 4). For users who have opted out of Broadcast via `broadcast_required: false`, this is treated as expected and not alerted.
- **Sampled-out rows**: if the user's declared `broadcast_expected_sampling_rate < 1.0`, the leak audit adjusts expectations proportionally. Rows that fall into the sampled-out fraction are still marked `broadcast_missing` individually but don't count against the coverage threshold until the aggregate ratio is computed.
- **Privacy Mode**: strips content, does NOT affect cost reconciliation or leak detection — still fully compatible.

For cost rollups, `estimated_cost` is used when `actual_cost IS NULL`. The DADBEAR oversight page shows `estimated` / `broadcast_missing` / `discrepancy` counts as distinct health signals with different severity levels.

**Fail-loud discrepancy handling:** when a webhook arrives and the estimated/actual cost diverge past a user-configurable threshold, the system surfaces the mismatch aggressively rather than silently correcting it.

The thresholds are NOT hardcoded. They flow from the active `dadbear_policy` contribution:

```yaml
schema_type: dadbear_policy
version: 1
# ...existing fields...
cost_reconciliation:
  discrepancy_ratio: 0.10           # fraction; fire alert when actual/estimated diff exceeds this
  provider_degrade_count: 3         # # of discrepancies within window to flip provider to degraded
  provider_degrade_window_secs: 600 # window for the above count (10 minutes default)
```

On webhook arrival, if `abs(actual_cost - estimated_cost) / estimated_cost > policy.cost_reconciliation.discrepancy_ratio`:

1. Set `reconciliation_status = "discrepancy"` on the `pyramid_cost_log` row
2. Emit a `CostReconciliationDiscrepancy` event via `BuildEventBus`
3. Log a WARN-level entry with full details (pyramid, step, estimated, actual, ratio)
4. Surface as a red alert banner in the DADBEAR Oversight page with "Click to investigate"
5. If `provider_degrade_count` discrepancies occur within `provider_degrade_window_secs` for the same provider: set `provider_health = "degraded"` on that provider's row in `pyramid_providers` — the provider resolver now warns on every resolution against this provider until an admin manually acknowledges the alert

No auto-correction. No self-learning. No silent updates to `cost_per_token`.

**Rationale:** cost estimation errors mean the system's model of reality is wrong. Self-correcting hides that. We want to know loudly so we can fix the root cause. The thresholds are configurable because "meaningful discrepancy" varies by deployment — a cost-sensitive local setup might want a 5% threshold; a cloud-only setup running many small calls might accept 20% noise.

### Provider Health Alerting

Provider health is a fail-loud signal surfaced to the user; it does NOT automatically reroute traffic or change provider selection.

- **`provider_health` enum:** `"healthy" | "degraded" | "down"`
- **Health is set by:**
  - Cost discrepancies (3+ in 10 minutes → `degraded`)
  - Consecutive HTTP 5xx errors from the provider → `degraded`
  - Connection failures (DNS, TCP, TLS) → `down`
- **Health is cleared by:** manual admin acknowledge via IPC (`pyramid_acknowledge_provider_health`). No automatic clearing.
- **Consumers:**
  - The provider resolver logs a WARN on every resolution against a non-`healthy` provider until acknowledged
  - The Settings UI shows a health indicator next to each provider row (green/yellow/red with reason tooltip)
  - The DADBEAR Oversight page shows the "recent discrepancies" count per provider

**New columns on `pyramid_providers`:**

```sql
provider_health TEXT NOT NULL DEFAULT 'healthy',  -- healthy | degraded | down
health_reason TEXT,                                -- last reason for degradation
health_since TEXT,                                 -- when health last changed
health_acknowledged_at TEXT                        -- when admin last acknowledged
```

**New IPC commands:**

```
GET pyramid_provider_health
  Input: {}
  Output: [{ provider_id, health, reason, since, acknowledged_at, recent_discrepancies: u64 }]

POST pyramid_acknowledge_provider_health
  Input: { provider_id: String }
  Output: { ok: bool }
```

---

## Part 3: DADBEAR Oversight Page

A unified view of all DADBEAR activity across pyramids. Frontend-only — assembles existing data.

### Data Sources (Already Exist)

| Table | What It Shows |
|-------|-------------|
| `pyramid_stale_check_log` | Stale check results per node |
| `pyramid_pending_mutations` | WAL queue status |
| `pyramid_llm_audit` | LLM calls with costs |
| `pyramid_cost_log` | Cost breakdown by operation |
| `pyramid_dadbear_config` | Per-pyramid DADBEAR config |
| `pyramid_demand_signals` (new) | Demand tracking |

### UI Layout

```
DADBEAR Oversight

  ┌─ Global Controls ──────────────────────────────┐
  │  [Pause All]  [Resume All]  [Set Default Norms] │
  └────────────────────────────────────────────────┘

  Per-Pyramid Status
  ┌──────────────────────────────────────────────────┐
  │ agent-wire-node-definitive                       │
  │   Status: Active (scanning every 30s)            │
  │   Last scan: 2 min ago                           │
  │   Pending mutations: 3                           │
  │   In-flight stale checks: 1                      │
  │   Cost (24h): $0.42 (est) / $0.38 (actual)      │
  │   [Pause] [Configure]                            │
  ├──────────────────────────────────────────────────┤
  │ all-docs-definitive                              │
  │   Status: Paused                                 │
  │   Last scan: 3 hours ago                         │
  │   [Resume] [Configure]                           │
  └──────────────────────────────────────────────────┘

  Cost Reconciliation (OpenRouter Broadcast)
  ┌──────────────────────────────────────────────────┐
  │  Estimated: $1.24     Actual: $1.18              │
  │  Discrepancy: -$0.06 (4.8%)                      │
  │  [View details]                                  │
  └──────────────────────────────────────────────────┘
```

### IPC Commands

```
GET pyramid_dadbear_overview        — all pyramid statuses + aggregate costs
POST pyramid_dadbear_pause(slug)    — pause DADBEAR for a pyramid
POST pyramid_dadbear_resume(slug)   — resume DADBEAR for a pyramid
POST pyramid_dadbear_pause_all      — pause all
POST pyramid_dadbear_resume_all     — resume all
GET pyramid_cost_reconciliation     — estimated vs actual costs (from Broadcast)
```

---

## Part 4: OpenRouter Broadcast (Required Integrity Confirmation + Leak Detection)

**Important**: Broadcast is **required**, not optional. The synchronous cost path (Part 3, `reconciliation_status = "synchronous"` from `response.usage.cost`) gives us authoritative per-call accounting as the response returns. Broadcast runs as a **second, asynchronous integrity check** that confirms each call actually happened on OpenRouter's side with the costs and tokens we recorded. This is our leak detection layer.

### Why Broadcast is required

Three leak scenarios the synchronous path alone cannot catch:

1. **Credential exfiltration**: someone copies the user's OpenRouter API key and makes calls from elsewhere. Our synchronous path only sees calls Wire Node initiates — it has no visibility into "phantom" calls made by a thief. Broadcast surfaces them as **orphan broadcasts** (a broadcast arrives with a `trace.metadata.build_id` that no local `pyramid_cost_log` row expected).
2. **Missing confirmations**: every Wire-Node-initiated call should produce exactly one broadcast trace (when Broadcast sampling is 100%). If our cost_log says "I made 500 calls this hour" and only 400 broadcasts arrived, something is wrong: either our handler was down, the tunnel flapped, or a provider-side accounting bug silently dropped calls. Either way the user needs to know.
3. **Cost drift**: if the synchronous `usage.cost` and the broadcast's post-hoc cost disagree by more than the configured threshold, that's a provider-side reconciliation bug worth surfacing loudly.

Broadcast is the asynchronous confirmation that the synchronous ledger is complete, consistent, and honest.

### What Broadcast is

Broadcast is OpenRouter's trace fan-out feature. When enabled in the OpenRouter dashboard (Settings > Observability), every API call produces a trace that's pushed to user-configured destinations. Supported destinations include: Langfuse, LangSmith, Datadog, Grafana Cloud, New Relic, Sentry, PostHog, W&B Weave, Arize AI, Braintrust, Comet Opik, ClickHouse, Snowflake, S3/S3-Compatible, Webhook (raw HTTP), and OpenTelemetry Collector.

We use the **Webhook** destination to have OpenRouter push OTLP JSON to our own HTTP endpoint (served via the existing Cloudflare tunnel in `tunnel.rs`).

### Configuration

Broadcast is **dashboard-configured, not API-configured**. Users enable it manually:

1. Log in to the OpenRouter dashboard, go to Settings > Observability
2. Toggle "Enable Broadcast"
3. Add a Webhook destination pointing at `{tunnel_url}/hooks/openrouter`
4. Optionally set custom headers for auth (e.g., `X-Webhook-Secret: <secret>`)
5. Optionally configure sampling rate, privacy mode, API key filter

Wire Node's Settings UI provides a "Copy webhook URL" button and documents the setup flow, but the actual Broadcast toggle lives on OpenRouter's side. Our spec does NOT try to programmatically configure Broadcast (the OpenRouter API does not expose this endpoint).

### Webhook endpoint

```
POST {tunnel_url}/hooks/openrouter
Content-Type: application/json
```

### OTLP JSON payload structure

OpenRouter sends traces in OTLP (OpenTelemetry Protocol) JSON format. The structure:

```json
{
  "resourceSpans": [
    {
      "resource": {
        "attributes": [
          { "key": "service.name", "value": { "stringValue": "openrouter" } }
        ]
      },
      "scopeSpans": [
        {
          "spans": [
            {
              "traceId": "...",
              "spanId": "...",
              "name": "chat",
              "startTimeUnixNano": "1705312800000000000",
              "endTimeUnixNano": "1705312801000000000",
              "attributes": [
                { "key": "gen_ai.request.model", "value": { "stringValue": "openai/gpt-4" } },
                { "key": "gen_ai.usage.prompt_tokens", "value": { "intValue": "100" } },
                { "key": "gen_ai.usage.completion_tokens", "value": { "intValue": "50" } },
                { "key": "gen_ai.usage.total_tokens", "value": { "intValue": "150" } },
                { "key": "user.id", "value": { "stringValue": "wire-node-xyz" } },
                { "key": "session.id", "value": { "stringValue": "my-slug/build-id-123" } },
                { "key": "trace.metadata.pyramid_slug", "value": { "stringValue": "my-slug" } },
                { "key": "trace.metadata.build_id", "value": { "stringValue": "build-id-123" } },
                { "key": "trace.metadata.step_name", "value": { "stringValue": "source_extract" } },
                { "key": "trace.metadata.depth", "value": { "intValue": "0" } }
              ]
            }
          ]
        }
      ]
    }
  ]
}
```

### Attribute key conventions

| OTLP attribute key | Source | Meaning |
|---|---|---|
| `gen_ai.request.model` | GenAI semantic convention | Canonical model ID |
| `gen_ai.usage.prompt_tokens` | GenAI semantic convention | Input token count |
| `gen_ai.usage.completion_tokens` | GenAI semantic convention | Output token count |
| `gen_ai.usage.total_tokens` | GenAI semantic convention | Sum of above |
| `user.id` | Maps to request's `user` field | End-user identifier |
| `session.id` | Maps to request's `session_id` field | Our `"{slug}/{build_id}"` |
| `trace.metadata.<key>` | Any field inside the request's `trace` object | Our custom metadata (slug, build_id, step_name, depth, etc.) |

Cost is embedded in the span attributes as well (typically a `gen_ai.usage.cost` attribute or similar — the exact key is not in the public docs; the webhook handler should search for any key containing `.cost` under `gen_ai.*` and `parseFloat()` the stringValue). Since the synchronous path already has `actual_cost` from the response body, the webhook path uses cost primarily for cross-verification, not as the authoritative value.

**Correlation**: each span's `traceId` / `spanId` are OTLP-specific identifiers, not OpenRouter generation_ids. To correlate a webhook trace back to a `pyramid_cost_log` row, use `trace.metadata.build_id` + `trace.metadata.step_name` (or the combination of `session.id` + `gen_ai.request.model` + timestamps as a fallback). Store OpenRouter's generation_id on the row at call time (from response body) and match OTLP `traceId` only as a secondary hint.

### Request metadata we send

When making OpenRouter calls, `OpenRouterProvider.augment_request_body()` adds:

```json
{
  "user": "{node_identity}",
  "session_id": "{slug}/{build_id}",
  "trace": {
    "trace_id": "{build_id}",
    "trace_name": "{chain_id}",
    "span_name": "{step_name}",
    "generation_name": "{step_name}",
    "pyramid_slug": "{slug}",
    "build_id": "{build_id}",
    "step_name": "{step_name}",
    "depth": "{depth}"
  }
}
```

`trace_id`, `trace_name`, `span_name`, `generation_name`, `parent_span_id` are OpenRouter-recognized keys that control trace hierarchy in observability dashboards. `pyramid_slug`, `build_id`, `step_name`, `depth` are custom keys passed through to all destinations under the `trace.metadata.*` namespace.

### Webhook handler requirements

1. **Accept `POST` and `PUT`** — Broadcast's Webhook destination lets users configure either method
2. **Return 2xx on success** — OpenRouter expects a 2xx response; non-2xx is treated as delivery failure but **is not retried**
3. **Handler durability is critical** — since OpenRouter does not retry, a handler outage creates leak-detection blind spots. The handler lives inside the Wire Node server and comes up with the app. If the tunnel is temporarily down, missed broadcasts surface as `broadcast_missing` rows in the leak audit (see below).
4. **Handle `X-Test-Connection: true` header specially** — when a user saves the Webhook destination in the OpenRouter dashboard, OpenRouter sends an empty OTLP payload with this header to verify connectivity. Our handler must return 2xx immediately with NO reconciliation or audit side effects. Per the docs, a 400 response is also accepted by OpenRouter as a valid test response (some endpoints reject empty payloads), but we return 200 for clarity.
5. **Parse the OTLP payload** — walk `resourceSpans[].scopeSpans[].spans[].attributes[]`, extract the attributes listed in "OTLP attribute key conventions" above, and UPDATE the matching `pyramid_cost_log` row via the correlation algorithm (next section).
6. **Reconciliation logic**:
   - If the row's `reconciliation_status` is `synchronous` (expected normal case): compare the broadcast's cost against the row's `actual_cost`. If divergent beyond `dadbear_policy.cost_reconciliation.discrepancy_ratio`, set status to `discrepancy` and emit `CostReconciliationDiscrepancy` event. Otherwise set `broadcast_confirmed_at = now()` and keep status as `synchronous` (the row is both synchronously reconciled AND broadcast-confirmed).
   - If `reconciliation_status` is `estimated` (primary path failed mid-call): use Broadcast values to populate `actual_cost`, `actual_tokens_in`, `actual_tokens_out`, `reconciliation_status = "broadcast"`, `reconciled_at = now()`, `broadcast_confirmed_at = now()`.
   - If NO matching local row is found (no `pyramid_cost_log` row has the corresponding `(build_id, step_name)` from the broadcast's `trace.metadata.*`): insert an **orphan broadcast** row into `pyramid_orphan_broadcasts` (new table, see below) and emit `OrphanBroadcastDetected`. This is a potential credential leak indicator.
7. **Authentication** — see "Webhook authentication" below.

### Correlation: matching a broadcast to its local row

When a broadcast trace arrives, we must find the local `pyramid_cost_log` row it confirms. The correlation algorithm:

```
1. Extract from the OTLP span's attributes:
   - build_id    = attributes["trace.metadata.build_id"].stringValue
   - step_name   = attributes["trace.metadata.step_name"].stringValue
   - chunk_index = attributes["trace.metadata.chunk_index"].intValue  (optional, may be missing)
   - model_id    = attributes["gen_ai.request.model"].stringValue
   - session_id  = attributes["session.id"].stringValue  (format: "<slug>/<build_id>")

2. Query pyramid_cost_log for matching row:
   SELECT id FROM pyramid_cost_log
   WHERE slug = (split_session_id(session_id)[0])
     AND step_name = ?step_name
     AND model = ?model_id
     AND (?chunk_index IS NULL OR chunk_index = ?chunk_index)
     AND generation_id IS NOT NULL
     AND broadcast_confirmed_at IS NULL
   ORDER BY created_at ASC
   LIMIT 1;

3. If exactly one row found: UPDATE that row's broadcast_confirmed_at + reconciliation comparison.
4. If zero rows found: insert orphan broadcast record.
5. If multiple rows found (shouldn't happen — broadcasts are per-call): take the oldest unconfirmed, log a WARN, continue.
```

We do NOT correlate by OTLP `traceId` / `spanId` — those are OpenRouter-generated identifiers that we don't know in advance on the local side. Correlation is by our own trace metadata.

### Leak Detection

The `pyramid_broadcast_audit` background task runs every 15 minutes (configurable via `dadbear_policy.cost_reconciliation.audit_interval_secs`) and compares expected-vs-confirmed broadcasts:

```
For each provider where Broadcast is configured:
  1. Read the Broadcast health configuration for this provider from pyramid_providers.broadcast_config_json:
     {
       "enabled": true,
       "expected_sampling_rate": 1.0,         // user-declared rate matching the dashboard setting
       "coverage_threshold": 0.95,            // below this, warn
       "coverage_critical": 0.80,             // below this, mark provider degraded
       "grace_period_secs": 600               // rows younger than this are excluded from the audit
     }

  2. Query pyramid_cost_log for completed rows older than grace_period_secs:
       expected  = COUNT(*) WHERE provider_id = ? AND reconciliation_status IN ("synchronous", "broadcast")
                              AND created_at < now() - grace_period_secs
       confirmed = COUNT(*) WHERE ... AND broadcast_confirmed_at IS NOT NULL

  3. coverage = confirmed / (expected * expected_sampling_rate)

  4. If coverage < coverage_threshold:
       - Emit BroadcastCoverageDegraded { provider_id, coverage, expected, confirmed }
       - Surface in the DADBEAR oversight page as a warning
  5. If coverage < coverage_critical for 3 consecutive audits (45 minutes by default):
       - Set pyramid_providers.provider_health = "suspicious"
       - Emit BroadcastLeakSuspected with the full audit breakdown
       - Require manual acknowledgment to clear
  6. Query pyramid_orphan_broadcasts for the same window:
       orphans = COUNT(*) WHERE provider_id = ? AND created_at < now() - grace_period_secs
     If orphans > 0:
       - Emit OrphanBroadcastDetected with the list
       - Surface as a red alert: "Someone made N API calls with your credentials that Wire Node did not initiate. Investigate and rotate your key."
```

The leak signal is bidirectional:
- **Missing confirmations** (`expected > confirmed`): Wire Node sent calls that OpenRouter doesn't confirm — either our ledger is spurious, the tunnel is dropping webhooks, or provider-side accounting is lagging beyond the grace period.
- **Orphan broadcasts** (broadcast arrived for a call Wire Node didn't initiate): the API key is being used elsewhere. Credential compromise likely.

### pyramid_orphan_broadcasts table

```sql
CREATE TABLE IF NOT EXISTS pyramid_orphan_broadcasts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    provider_id TEXT NOT NULL,
    raw_payload_json TEXT NOT NULL,     -- full OTLP span attributes for investigation
    model_id TEXT,
    reported_cost REAL,
    reported_tokens_in INTEGER,
    reported_tokens_out INTEGER,
    extracted_metadata_json TEXT,        -- any trace.metadata.* keys we did see
    detected_at TEXT DEFAULT (datetime('now')),
    acknowledged_at TEXT,                -- when the user has reviewed and dismissed
    acknowledgment_reason TEXT           -- user's explanation (e.g., "tested from curl")
);
CREATE INDEX idx_orphan_unreviewed ON pyramid_orphan_broadcasts(provider_id, acknowledged_at);
```

### Additional `pyramid_cost_log` columns for leak detection

```sql
ALTER TABLE pyramid_cost_log
  ADD COLUMN provider_id TEXT,                     -- which provider this call went to (for audit grouping)
  ADD COLUMN broadcast_confirmed_at TEXT;          -- when the matching broadcast trace arrived; NULL = still pending or never received
```

`broadcast_confirmed_at` is the critical field. After the grace period, any `synchronous` row with `broadcast_confirmed_at IS NULL` is a candidate for the coverage alert.

### Broadcast configuration schema

Added to `dadbear_policy`:

```yaml
cost_reconciliation:
  discrepancy_ratio: 0.10
  provider_degrade_count: 3
  provider_degrade_window_secs: 600
  # New fields:
  broadcast_required: true                   # globally require Broadcast confirmation
  broadcast_expected_sampling_rate: 1.0      # user declares the rate they set in OpenRouter dashboard
  broadcast_coverage_threshold: 0.95         # coverage warn threshold
  broadcast_coverage_critical: 0.80          # coverage critical threshold
  broadcast_grace_period_secs: 600           # how long to wait before counting a row as unconfirmed
  broadcast_audit_interval_secs: 900         # how often to run the audit
  orphan_broadcast_auto_alert: true          # whether to surface orphan broadcasts immediately
```

All values are user-configurable via the dadbear_policy generative config. A user who explicitly opts out of Broadcast sets `broadcast_required: false` — the audit skips the coverage check but still processes any broadcasts that do arrive (treating them as bonus data). The opt-out is recorded with a dismissal reason and surfaced on the oversight page as "Broadcast disabled — leak detection unavailable" so the user can always see the tradeoff they've accepted.

### Webhook authentication

OpenRouter's Broadcast Webhook destination supports user-configured custom HTTP headers. Wire Node auth strategy (defensive-in-depth):

1. **Shared secret header (primary, required)**:
   - On provider setup, Wire Node generates a random 32-byte secret and stores it in the credentials file under `OPENROUTER_BROADCAST_SECRET`
   - The Settings UI displays the webhook URL + the required header pair: `X-Webhook-Secret: <secret>`
   - The user copies these into the OpenRouter dashboard's Webhook destination custom headers
   - Our handler rejects any request whose `X-Webhook-Secret` doesn't match

2. **IP allowlisting (opportunistic)**:
   - We track source IPs that successfully authenticated via the shared secret
   - If a request arrives from a new IP, we log it as a "new broadcast source IP" info event but still process it (OpenRouter's egress can use multiple IPs)
   - This is observational — useful for security review, not a hard gate

3. **HMAC verification (future-proofing)**:
   - OpenRouter does not currently publicly document HMAC signing for broadcasts
   - IF OpenRouter adds HMAC in the future: our handler checks for an `X-Signature` header (or similar) as an ADDITIONAL layer on top of the shared secret
   - The shared secret remains the primary gate, HMAC becomes a belt-and-suspenders check

4. **Rate limiting**:
   - The handler rate-limits per source IP to prevent abuse if the shared secret is somehow leaked
   - Default limit: 100 broadcasts/second per IP (far exceeds normal traffic; configurable)

### Sampling and privacy compatibility

- **Sampling rate per destination**: users can configure Broadcast to send only a fraction of traces (e.g., 10% of production). Since sampling is deterministic on `session_id`, entire builds are either fully traced or fully not. For leak detection, the user declares their sampling rate via `dadbear_policy.cost_reconciliation.broadcast_expected_sampling_rate` so the coverage calculation can adjust expectations. Users who change the dashboard setting must also update the declared rate — we warn when declared vs observed diverges significantly (indicating drift).
- **Privacy Mode per destination**: strips `input messages` and `output choices` from traces. All other data (tokens, costs, timing, model, custom metadata) is still sent. Our reconciliation logic does NOT need prompt/completion content, so Privacy Mode is fully compatible with leak detection.
- **API Key Filter per destination**: users can restrict Broadcast to specific API keys. If enabled with a filter that excludes our inference key, coverage drops to zero — we surface this as a "Broadcast configured but not receiving from your active API key" warning.

### For local calls (Ollama)

Ollama does not participate in Broadcast. Cost is zero, and the synchronous path sets `reconciliation_status = "synchronous_local"` at call time. These rows are excluded from the leak audit entirely — `provider_id` for local providers is flagged `broadcast_eligible = false` in `pyramid_providers`, and the audit query skips them.

---

## Implementation Order

1. **DADBEAR in-flight lock** — independent, fix immediately
2. **OpenRouter Broadcast webhook** — requires provider registry
3. **Demand signal tracking** — small, can ship independently
4. **Evidence triage** — requires generative config pattern + provider routing
5. **DADBEAR oversight page** — frontend assembly of existing + new data
6. **Cost reconciliation** — requires Broadcast webhook data flowing

---

## Files Modified

| Phase | Files |
|-------|-------|
| In-flight lock | `dadbear_extend.rs` |
| Broadcast webhook | `server.rs` (new route), `llm.rs` (trace metadata), `provider.rs` (augment_request_body) |
| Demand signals | `db.rs` (new table), `query.rs` (record drill), MCP server (record query) |
| Evidence triage | `evidence_answering.rs` (triage gate), new `triage.rs`, `db.rs` (deferred questions) |
| Oversight page | New `src/components/DadbearOversight.tsx` |

---

## Open Questions

1. **Triage LLM call cost**: The triage step is a cheap LLM call per evidence question. For pyramids with 500+ evidence questions, this adds up. Should triage be batched (multiple questions per call)? Recommend: yes. Batch size flows from `evidence_policy.budget.triage_batch_size` (user-configurable, suggest seeding at 15).

2. **Deferred question re-check**: When a deferred question's check_interval expires, how is the re-check triggered? Recommend: DADBEAR tick includes a deferred question scanner that picks up expired deferrals.

3. **Freeze/unfreeze restart**: The vision doc mentions that after unfreezing, DADBEAR should re-apply without app restart. Currently requires restart. The fix: DADBEAR tick loop reads config from DB each cycle (it already does), so unfreezing + enabling in DB is sufficient. Need to verify the config read path includes the `enabled` check.
