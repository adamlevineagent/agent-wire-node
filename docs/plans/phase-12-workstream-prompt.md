# Workstream: Phase 12 — Evidence Triage + Demand Signal Propagation + Cache Retrofit Sweep

## Who you are

You are an implementer joining an active 17-phase initiative. Phases 0a, 0b, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11 are shipped. You are the implementer of Phase 12 — the evidence-triage gate, demand signal tracking & propagation, deferred-question persistence & re-evaluation, PLUS the Phase 6 wanderer's sweep: thread `StepContext` (the Phase 6 cache-aware context) through every remaining production `call_model_*` site so the LLM output cache becomes reachable from every build step, not just the single Phase 6 retrofit and the Phase 6-fix-pass chain_dispatch::dispatch_ir_llm path.

Phase 12 is large because it rolls together (a) the Part-2 evidence-triage spec, (b) the Part-2 demand-signal tracking and propagation, (c) deferred-question persistence + policy-change re-evaluation, and (d) the Phase 6 wanderer's explicit ask that Phase 12 sweep every non-cache-aware LLM call site. You ship all four. Do not defer.

## Context

Phase 6 shipped `StepContext` + the `pyramid_step_cache` table + `call_model_unified_with_options_and_ctx` + the `generate_change_manifest` retrofit. The Phase 6 fix pass then retrofitted `chain_dispatch::dispatch_ir_llm` (the v3 IR dispatcher) via a `CacheDispatchBase` struct carried on `chain_dispatch::StepContext`. That left everything else on the legacy non-ctx path.

The Phase 6 wanderer's explicit handoff, captured in `docs/plans/pyramid-folders-model-routing-friction-log.md`:

> "Phase 12's workstream prompt should explicitly require the implementer to grep for every `call_model_*` call site in the repo and thread a StepContext through it, and the verifier should grep for `call_model_unified_with_options` vs `call_model_unified_with_options_and_ctx` to confirm the ratio flips."

You carry that explicit mandate. There are ~59 `call_model_*` sites outside `llm.rs`. Most (~50) are not cache-aware today.

Phase 9 shipped the `evidence_policy` schema type with `upsert_evidence_policy` / `pyramid_evidence_policy` table. Phase 4 shipped `reevaluate_deferred_questions` as a stub; Phase 12 wires it up for real.

## Required reading (in order)

### Spec docs

1. `docs/handoffs/handoff-2026-04-09-pyramid-folders-model-routing.md` — deviation protocol.
2. **`docs/specs/evidence-triage-and-dadbear.md` Part 2 (lines 118-438) in full.** This is your primary implementation contract for triage + demand signals + deferred questions + re-evaluation.
3. **`docs/specs/llm-output-cache.md` — re-read the "StepContext" / "Threading the Cache Context" sections.** You are extending the reach of this existing primitive, not building it.
4. `docs/plans/pyramid-folders-model-routing-full-pipeline-observability.md` — Phase 12 section (line 256).
5. `docs/plans/pyramid-folders-model-routing-implementation-log.md` — scan Phase 6 entries for the cache wiring pattern.
6. **`docs/plans/pyramid-folders-model-routing-friction-log.md`** — search for "Phase 12" and "call_model_" to see the wanderer's full scope analysis from Phase 6. This is your retrofit road map.

### Code reading (targeted)

7. **`src-tauri/src/pyramid/step_context.rs` in full** (~590 lines). Understand the existing `StepContext`, its builder methods (`with_model_resolution`, `with_prompt_hash`, `with_bus`), and `cache_is_usable()`.
8. **`src-tauri/src/pyramid/chain_dispatch.rs` lines 30-220, 1050-1300** — read the `CacheDispatchBase` struct + `build_cache_ctx_for_ir_step` helper. Phase 12 generalizes this pattern for non-chain call sites.
9. **`src-tauri/src/pyramid/llm.rs`** — find `call_model_unified_with_options_and_ctx`. Confirm the cache lookup / hit / write path. Confirm `call_model_unified_with_options` is the legacy shim that delegates to `..._and_ctx(config, None, ...)`.
10. **`src-tauri/src/pyramid/evidence_answering.rs` lines 80-520** — understand `pre_map_layer` + `answer_questions`, the two fat LLM-calling functions this phase extends with triage + cache.
11. **`src-tauri/src/pyramid/db.rs:11620-11700`** — `EvidencePolicyYaml`, `upsert_evidence_policy`. You will EXTEND `EvidencePolicyYaml` to parse the Part-2 fields (triage_rules, demand_signals, budget, demand_signal_attenuation).
12. **`src-tauri/src/pyramid/config_contributions.rs:880-900`** — the `reevaluate_deferred_questions` stub. You replace it.
13. `src-tauri/src/pyramid/routes.rs:2758-2795` — `handle_drill` (HTTP drill handler — user_drill signal recording).
14. `src-tauri/src/pyramid/query.rs` — `drill()` function + friends (read path for signal node ids).
15. `src-tauri/src/main.rs` — find the IPC handler list. You'll add `pyramid_reevaluate_deferred_questions`.
16. `src-tauri/src/pyramid/stale_engine.rs` (and `stale_helpers*.rs`) — the DADBEAR tick loop. You'll add a deferred-question scanner.
17. Every file under `src-tauri/src/pyramid/` that appears in the "retrofit sweep" list below (see "Retrofit sweep" section). Read enough to understand the LLM call site in context.

## What to build

### 1. Demand signal tracking + propagation

#### 1a. New table + db helpers

Add to `db.rs`:

```sql
CREATE TABLE IF NOT EXISTS pyramid_demand_signals (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    slug TEXT NOT NULL,
    node_id TEXT NOT NULL,
    signal_type TEXT NOT NULL,       -- "agent_query", "user_drill", "search_hit"
    source TEXT,                     -- agent name or "user"
    weight REAL NOT NULL DEFAULT 1.0,
    source_node_id TEXT,             -- original leaf node (for propagation tracing)
    created_at TEXT DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_demand_signals ON pyramid_demand_signals(slug, node_id, signal_type, created_at);
```

Helpers (all in `db.rs`):
- `insert_demand_signal(conn, slug, node_id, signal_type, source, weight, source_node_id) -> Result<()>`
- `sum_demand_weight(conn, slug, node_id, signal_type, since_window_modifier) -> Result<f64>` — runs the `SELECT SUM(weight)` query with the SQLite datetime modifier format (`"-14 days"`, `"-7 days"`).
- `load_parents_via_evidence(conn, slug, node_id) -> Result<Vec<String>>` — walks `pyramid_evidence` KEEP links to find parent nodes (already partially exists? check first; add if not).

#### 1b. Demand signal propagation

New module: `src-tauri/src/pyramid/demand_signal.rs`.

```rust
pub fn record_demand_signal(
    conn: &Connection,
    slug: &str,
    node_id: &str,
    signal_type: &str,
    source: Option<&str>,
    policy: &EvidencePolicy,
) -> Result<()>
```

Implementation:
- Start at `node_id` with weight `1.0` and depth `0`.
- BFS walking parents via `pyramid_evidence` KEEP links (see the spec's pseudocode at ~line 290).
- Apply `policy.demand_signal_attenuation.factor` per layer.
- Stop when `weight < policy.demand_signal_attenuation.floor` OR `depth > policy.demand_signal_attenuation.max_depth`.
- Use an explicit `HashSet<String>` visited set to prevent cycles.
- Each `insert_demand_signal` row stores the propagated weight AND `source_node_id = original leaf`.
- Fire-and-forget from the caller's perspective (the caller wraps this in `tokio::spawn`/`spawn_blocking` — but the function itself is synchronous since it's DB-bound).

Tests:
- `test_propagate_respects_floor` — weight drops below 0.1 after N layers, propagation stops.
- `test_propagate_respects_max_depth` — hits max_depth before floor.
- `test_propagate_cycle_guard` — seed a cyclic KEEP graph, assert no infinite loop.
- `test_propagate_records_source_node_id` — every row has the original leaf id as `source_node_id`.
- `test_propagate_disabled_when_attenuation_factor_zero` — factor 0 → only the leaf gets a row.

#### 1c. Signal recording points

- `routes.rs::handle_drill` — after a successful drill, record `user_drill` demand on `node_id`. Use the `agent_id.unwrap_or("user")` as source. Call through a fire-and-forget helper (spawn a tokio task).
- `routes.rs::handle_search` — after a search hit leads to a drill (we don't have this chain today — the simplest approximation is: when drill is called, check if the current request's `Referer` or agent_id suggests it came from search, and if so record `search_hit`). **Simpler alternative (recommended):** only record `search_hit` when the drill endpoint is called AND the query_params indicate the node came from a search context. For MVP: skip `search_hit` and document it as deferred to Phase 13 (search-source tracking needs a session/referer mechanism Phase 12 doesn't add).
- `main.rs::pyramid_drill` IPC — same as routes.rs, fire-and-forget demand signal recording.
- `routes.rs` + `main.rs` — any MCP-exposed endpoint that returns pyramid node data (`pyramid_apex`, `pyramid_search`, `pyramid_drill`, `pyramid_answer`): record `agent_query` when `agent_id.is_some()`.

The key insight: the spec says "MCP server handler" but Wire Node does NOT have a dedicated MCP server — it exposes HTTP endpoints that MCP clients call. The HTTP route layer is the right spot. Record on every authenticated agent request resolving pyramid node data.

### 2. EvidencePolicy type extension

Extend `db::EvidencePolicyYaml` to parse the Part-2 fields. You need:

```rust
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct EvidencePolicyYaml {
    #[serde(default)]
    pub triage_rules: Option<Vec<TriageRule>>,
    #[serde(default)]
    pub demand_signals: Option<Vec<DemandSignalRule>>,
    #[serde(default)]
    pub budget: Option<PolicyBudget>,
    #[serde(default)]
    pub demand_signal_attenuation: Option<DemandSignalAttenuation>,
}

pub struct TriageRule {
    pub condition: String,  // DSL string: "stale_check AND has_demand_signals"
    pub action: TriageAction,  // "answer" | "defer" | "skip"
    pub model_tier: Option<String>,
    pub check_interval: Option<String>,  // "7d" | "30d" | "never" | "on_demand"
    pub priority: Option<String>,
}

pub struct DemandSignalRule {
    pub r#type: String,  // "agent_query" | "user_drill" | "search_hit"
    pub threshold: f64,  // summed weight, not count
    pub window: String,  // SQLite datetime modifier: "-14 days"
}

pub struct PolicyBudget {
    pub maintenance_model_tier: Option<String>,
    pub initial_build_model_tier: Option<String>,
    pub max_concurrent_evidence: Option<usize>,
    pub triage_batch_size: Option<usize>,
}

pub struct DemandSignalAttenuation {
    pub factor: f64,     // default 0.5 if unset
    pub floor: f64,      // default 0.1 if unset
    pub max_depth: u32,  // default 6 if unset
}
```

**Backwards compatibility:** existing Phase 9 `upsert_evidence_policy` callers pass the old three-field struct. Make the new fields `#[serde(default)]` so pre-Phase-12 YAML still parses. Add conversion helpers that default to spec values when `demand_signal_attenuation` is None (factor=0.5, floor=0.1, max_depth=6).

Add a loader: `pub fn load_active_evidence_policy(conn: &Connection, slug: Option<&str>) -> Result<EvidencePolicy>` where `EvidencePolicy` is a runtime representation with defaults filled in.

### 3. Triage module (new)

Create `src-tauri/src/pyramid/triage.rs`.

```rust
pub struct TriageContext<'a> {
    pub policy: &'a EvidencePolicy,
    pub question: &'a LayerQuestion,
    pub target_node_distilled: Option<&'a str>,
    pub is_first_build: bool,
    pub is_stale_check: bool,
    pub has_demand_signals: bool,
}

pub enum TriageDecision {
    Answer { model_tier: String },
    Defer { check_interval: String, triage_reason: String },
    Skip { reason: String },
}

pub async fn triage_evidence_question(
    llm_config: &LlmConfig,
    ctx: &TriageContext<'_>,
    step_ctx: Option<&StepContext>,
) -> Result<TriageDecision>
```

Implementation:
1. Walk `policy.triage_rules` in order, evaluate each `condition` DSL against the facts in `ctx`. First matching rule wins.
2. If no policy rule matches (or `triage_rules` is empty), fall back to the cheap LLM call: classify the question as high-value/trivial via the configured triage model tier. Use `StepContext` so the triage call is cacheable.
3. Return the decision.

**Condition DSL:** support only the vocabulary from the spec (~line 200):
- `first_build`, `stale_check`, `no_demand_signals`, `has_demand_signals`
- `evidence_question_trivial`, `evidence_question_high_value` (LLM-classified flags — only set when the LLM classification runs)
- `depth == N` (numeric comparison on the target's depth)
- `AND`, `OR`, `NOT`, `(`, `)`

Write a tiny recursive-descent evaluator (~100 lines of Rust). No third-party expression crate.

**StepContext for triage:** use `step_name = "evidence_triage"`, `primitive = "triage"`, `depth = target node depth`, `chunk_index = None`. Cache inputs hash = `(question_text + target_node_distilled + policy_yaml_hash)`. The `policy_yaml_hash` is the hash of the active evidence_policy contribution YAML — when the policy changes, the hash changes, the cache misses, and triage re-runs (this is the correct behavior for the policy-change re-evaluation path).

**Batching:** `policy.budget.triage_batch_size` (default 15 if unset). If batching, one LLM call evaluates N questions; parse per-question decisions. The individual per-question cache entries still work — just use the batched-prompt hash as the prompt_hash and the per-question inputs for inputs_hash.

Tests:
- `test_triage_dsl_parse_simple`
- `test_triage_dsl_and_or_precedence`
- `test_triage_rule_first_match_wins`
- `test_triage_defer_computes_next_check_at_from_interval`
- `test_triage_cache_key_changes_with_policy_hash`
- `test_triage_batch_returns_per_question_decisions` (use a fake LLM stub)

### 4. pyramid_deferred_questions table + helpers

Add to `db.rs`:

```sql
CREATE TABLE IF NOT EXISTS pyramid_deferred_questions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    slug TEXT NOT NULL,
    question_id TEXT NOT NULL,
    question_json TEXT NOT NULL,
    deferred_at TEXT NOT NULL DEFAULT (datetime('now')),
    next_check_at TEXT NOT NULL,
    check_interval TEXT NOT NULL,
    triage_reason TEXT,
    contribution_id TEXT,
    UNIQUE(slug, question_id)
);
CREATE INDEX IF NOT EXISTS idx_deferred_questions_next ON pyramid_deferred_questions(slug, next_check_at);
CREATE INDEX IF NOT EXISTS idx_deferred_questions_interval ON pyramid_deferred_questions(check_interval);
```

Helpers:
- `defer_question(conn, slug, question, check_interval, reason, contribution_id) -> Result<()>` — UPSERT on `(slug, question_id)`, computes `next_check_at` from `check_interval` + `datetime('now')`.
- `list_expired_deferred(conn, slug) -> Result<Vec<DeferredQuestion>>`
- `list_all_deferred(conn, slug) -> Result<Vec<DeferredQuestion>>`
- `remove_deferred(conn, slug, question_id) -> Result<()>`
- `update_deferred_next_check(conn, slug, question_id, check_interval, contribution_id) -> Result<()>`

**Parse `check_interval`:** support `"7d"`, `"30d"`, `"1h"`, `"never"`, `"on_demand"`. The `"never"` and `"on_demand"` cases set `next_check_at` to a far-future sentinel (e.g., `'9999-12-31 00:00:00'`). Helper: `parse_check_interval_to_next_check_at(interval: &str) -> String`.

### 5. Re-evaluation on policy change

Replace the Phase 4 stub `config_contributions::reevaluate_deferred_questions`:

```rust
fn reevaluate_deferred_questions(conn: &Connection, slug: Option<&str>) -> Result<()>
```

Implementation:
1. Load the active `EvidencePolicy` for `slug`.
2. `let deferred = list_all_deferred(conn, slug_str)?;`
3. For each deferred question, re-run triage against the new policy.
4. If new decision is `Answer`: `remove_deferred` + enqueue on the evidence queue (for simplicity, store a pending marker row that the evidence_answering step picks up on next build; OR immediately dispatch the per-question answering flow — preferred if it's not too invasive to `evidence_answering.rs`).
5. If new decision is `Defer`: `update_deferred_next_check` with the new interval + `contribution_id`.
6. If new decision is `Skip`: `remove_deferred`.
7. Emit a `DeferredQuestionsReevaluated` event via `BuildEventBus` with the counts.

**Scope note:** since re-running triage may require an LLM call, AND `reevaluate_deferred_questions` is called from the synchronous `sync_config_to_operational` path, the LLM path must be async. Refactor to a `tokio::spawn` background task — record the new policy contribution_id + slug, then spawn a task that loads the policy and runs triage. The sync_config_to_operational handler returns immediately after scheduling.

### 6. Deferred question scanner in DADBEAR tick

Add to the stale_engine DADBEAR tick loop (find the main tick function in `stale_engine.rs`):

```rust
// After dispatch_pending_ingests:
let expired = db::list_expired_deferred(&conn, Some(slug))?;
for q in expired {
    // Skip "never" and "on_demand" — they're only reactivated by demand signals
    if q.check_interval == "never" || q.check_interval == "on_demand" {
        continue;
    }
    // Re-run triage; same outcomes as re-evaluation above.
    match triage_evidence_question(...).await? {
        Answer { model_tier } => { db::remove_deferred + enqueue }
        Defer { check_interval, .. } => { db::update_deferred_next_check }
        Skip { .. } => { db::remove_deferred }
    }
}
```

### 7. Demand-signal reactivation of "on_demand" deferrals

In `demand_signal::record_demand_signal`: after propagating, query `pyramid_deferred_questions` for `(slug, node_id)` rows where `check_interval IN ('never', 'on_demand')`. For each match, re-run triage. If it now returns `Answer`, remove the deferred row and enqueue the question.

### 8. Triage gate in evidence_answering

In `evidence_answering.rs::answer_questions`, before the actual `answer_single_question` dispatch:

1. Load the active `EvidencePolicy` for the slug.
2. For each question, build a `TriageContext` (compute `has_demand_signals` via `sum_demand_weight` for each signal type in the policy, check if ANY exceeds its threshold).
3. Call `triage.rs::triage_evidence_question` (batched per `policy.budget.triage_batch_size`).
4. Partition the questions into Answer / Defer / Skip buckets.
5. `Answer` flows into the existing `answer_single_question` path (passing the triage-resolved `model_tier` if set).
6. `Defer` goes to `db::defer_question` (fire-and-forget).
7. `Skip` is logged and dropped.
8. Return a summary in `AnswerBatchResult` showing counts per action.

**Signature change:** `answer_questions` needs a `db_path: &Path` parameter (or a `Connection` handle) so it can call `sum_demand_weight`. Thread this through from the caller.

### 9. pyramid_reevaluate_deferred_questions IPC

Add to `main.rs`:

```rust
#[tauri::command]
async fn pyramid_reevaluate_deferred_questions(
    slug: String,
    state: tauri::State<'_, PyramidState>,
) -> Result<ReevaluateResult, String>
```

where `ReevaluateResult` has `{ evaluated, activated, skipped, still_deferred }`.

Register in the `invoke_handler!` list.

### 10. Retrofit sweep — thread StepContext through every remaining call_model_* site

**This is the Phase 6 wanderer's explicit mandate.** Do NOT skip it.

**Method:**
1. Run `grep -rn "call_model\|call_model_unified\|call_model_audited\|call_model_structured\|call_model_with_usage\|call_model_via_registry\|call_model_direct" --include="*.rs" src-tauri/src/pyramid/ | grep -v "^src-tauri/src/pyramid/llm.rs:" | grep -v test`.
2. Produce a table in the implementation log listing every call site with: file:line, function, current path (`cache-aware`, `legacy-shim`, `registry`, `direct`, `audited`), and action taken (`retrofitted`, `not-applicable-reason`).
3. For every site where a build context + step identity exists, construct a `StepContext` and route the call through `call_model_unified_with_options_and_ctx`.
4. Sites where NO build context exists (e.g., `call_model_direct` diagnostics, `public_html/routes_ask.rs` free-form ask, `routes.rs` semantic-search path, ASCII art generation, etc.) — keep them on the legacy path but DOCUMENT them as "intentionally bypassed — not a step."

**Target call sites (from the Phase 6 wanderer's list — verify each exists):**

| File | Function | Build context in scope | Retrofit? |
|---|---|---|---|
| `chain_dispatch.rs::dispatch_llm` | v2 legacy chain step | ctx.cache_base is already threaded | YES |
| `evidence_answering.rs::pre_map_layer` | layer pre-mapping | slug + build via caller | YES (new `StepContext` per batch) |
| `evidence_answering.rs::answer_single_question` | per-question answering | slug + build | YES |
| `evidence_answering.rs` (two more call sites) | synthesis | slug + build | YES |
| `faq.rs` | 6 `call_model`/`call_model_with_usage` sites | slug + operation | YES |
| `delta.rs` | 4 `call_model` sites | slug + delta step | YES |
| `meta.rs` | 4 `call_model` sites | slug | YES |
| `webbing.rs` | `call_model` | slug | YES |
| `characterize.rs` | `call_model_unified` | slug | YES |
| `supersession.rs` | `call_model_unified` | slug + depth | YES |
| `stale_helpers.rs` | 4 `call_model_with_usage` sites | slug + stale step name | YES |
| `stale_helpers_upper.rs` | 5 `call_model_with_usage` sites + 1 `call_model_unified_with_options_and_ctx` already retrofitted | slug + depth | YES (remaining 5) |
| `question_decomposition.rs` | 3 `call_model_unified` sites | slug + layer | YES |
| `extraction_schema.rs` | 2 `call_model_unified` sites | slug + build | YES |
| `build.rs` | 2 `call_model` sites | slug + build | YES |
| `generative_config.rs` | 1 `call_model_unified_with_options_and_ctx` | already Phase 9 retrofitted | SKIP |
| `public_html/routes_ask.rs` | `call_model_unified` | no build | SKIP (intentional) |
| `public_html/ascii_art.rs` | `call_model_direct` | no build | SKIP (intentional) |
| `routes.rs` two sites (semantic search, keyword rewrite) | no build | SKIP (intentional) |

**Helper strategy for retrofitting:** many of these sites are inside non-chain flows (DADBEAR, FAQ, delta, meta) where there's no `CacheDispatchBase` in scope. Introduce a lighter constructor:

```rust
// In step_context.rs or a new helper module:
pub fn make_step_context_from_slug(
    slug: &str,
    build_id: Option<&str>,
    step_name: &str,
    primitive: &str,
    depth: i64,
    chunk_index: Option<i64>,
    db_path: &str,
    bus: Option<Arc<BuildEventBus>>,
    model_tier: &str,
    resolved_model_id: &str,
    system_prompt: &str,
    user_prompt: &str,
    prompt_template_body: Option<&str>,
) -> StepContext
```

This constructs the full StepContext including a `prompt_hash` computed on-the-fly from `prompt_template_body` (or from the concatenated system+user prompt if no template is available). For sites that don't know their build_id (e.g., DADBEAR maintenance calls), generate one from `slug + "maintenance-" + operation` — the cache will still work because the cache_key is built from inputs/prompt/model, not build_id. build_id is used for telemetry/provenance only, not for lookup.

**Scope protection:** ANY site that you retrofit must still pass tests. If a site's tests mock `call_model_unified` and you route through `..._and_ctx`, the mocks may break. When that happens, extend the test to mock both or to pass `None` for the ctx (which routes back through the legacy path — safe fallback).

**Do NOT retrofit the AUDITED path (`call_model_audited`).** Phase 6 fix pass explicitly documented: the audited path doesn't go through `..._and_ctx` yet. That's Phase 13+ scope. When you encounter a site that has both an audited and non-audited branch (like the two-arm pattern in `evidence_answering.rs` and `chain_dispatch.rs`), retrofit the non-audited arm only. Log the audited arm as deferred.

### 11. Tests (Rust)

New test modules/tests:
- `triage.rs` — see section 3.
- `demand_signal.rs` — see section 1b.
- `db.rs` — tests for `insert_demand_signal`, `sum_demand_weight`, `defer_question`, `list_expired_deferred`, `list_all_deferred`.
- `evidence_answering.rs` — new test: triage gate correctly partitions questions into answer/defer/skip buckets when a policy with triage_rules is active. Use an in-memory DB + a fake LLM.
- `config_contributions.rs` — test that supersession of an `evidence_policy` contribution triggers `reevaluate_deferred_questions`. Existing tests around `sync_config_to_operational` should be extended.
- Cache retrofit verification: add at least one test per retrofitted module showing that the retrofitted call now takes the cache-aware path when a StepContext is present. A simple pattern: set `force_fresh=false`, pre-populate the cache with a known row for the (inputs,prompt,model) tuple, call the retrofitted function, assert the cached result was returned (no HTTP mock needed).

### 12. Implementation log entry

Append Phase 12 entry to `docs/plans/pyramid-folders-model-routing-implementation-log.md`:
- The retrofit table (file:line list with status).
- A before/after count for `call_model_unified_with_options` vs `call_model_unified_with_options_and_ctx` mentions. The ratio should flip decisively toward `..._and_ctx`.
- New tables + schemas.
- Triage module overview.
- Re-evaluation flow.
- Tests added and passing.
- Status: `awaiting-verification`.

## Scope boundaries

**In scope:**
- Evidence triage policy parsing + DSL evaluator
- Triage LLM call with StepContext integration
- Demand signal table + fire-and-forget recording from HTTP routes + IPC
- Demand signal propagation module with attenuation + loop guard
- Deferred questions table + helpers
- DADBEAR tick scanner for expired deferrals
- On-demand reactivation via demand-signal handler
- Policy-change re-evaluation + `pyramid_reevaluate_deferred_questions` IPC
- `evidence_policy` YAML type extension (triage_rules, demand_signals, budget, attenuation)
- **StepContext retrofit sweep across all non-test, non-legacy-bypass call_model_* sites**
- Implementation log + Rust tests

**Out of scope:**
- DADBEAR Oversight page UI (Phase 15)
- Demand signal UI surfacing (Phase 15)
- Policy editor UI (Phase 10 already shipped the generative config loop; a dedicated "Apply to all deferred" button UI is Phase 15)
- Search-hit signal recording (deferred — see section 1c)
- `call_model_audited` retrofit (deferred to Phase 13+)
- Intentionally-bypassed call sites (ask/ascii/semantic search/direct)
- Frontend tests — there's no frontend change in Phase 12
- CSS/styling work — N/A
- The 7 pre-existing unrelated Rust test failures

## Verification criteria

1. **Rust clean:** `cargo check --lib`, `cargo build --lib` — zero new warnings.
2. **Test count:** `cargo test --lib pyramid` — expect Phase 11 count (1073 passing) + new Phase 12 tests. Same 7 pre-existing failures.
3. **Retrofit ratio flip:** `grep -c "call_model_unified_with_options_and_ctx" src-tauri/src/pyramid/*.rs` should be substantially higher than `grep -c "call_model_unified_with_options(" src-tauri/src/pyramid/*.rs` (the legacy shim). Document the before/after counts in the log.
4. **Signal recording:** manual verification path — `cargo test --lib pyramid::routes::tests -- --nocapture` (if route tests exist), OR the new demand_signal tests exercise the full record → propagate → query path.
5. **Triage gate behavior:** a new integration test in `evidence_answering.rs` tests shows that questions routed through `answer_questions` are correctly partitioned.
6. **Policy re-evaluation:** a new test in `config_contributions.rs` supersedes an evidence_policy contribution and asserts that `reevaluate_deferred_questions` is invoked (the test can use a spy/counter since the actual LLM call is async).

## Deviation protocol

Standard. Most likely deviations:

- **`search_hit` signal recording is non-trivial** because Wire Node doesn't track search → drill chains in a single session. Recommended: skip for Phase 12, document as "needs session tracking", leave a TODO and hand off to Phase 15.
- **`call_model_audited` retrofit**: if the auditing layer has a reachable wrapper that can take a StepContext, retrofit it; otherwise leave for later. Log the decision either way.
- **Retrofit call-site count may differ** from the table above (files may have changed). Trust the grep, update the table to match reality, document any divergence.
- **`evidence_answering.rs::answer_questions` signature change** — if threading a `db_path` requires changes in 10+ callers, consider passing `&Connection` or `Arc<Mutex<Connection>>` through instead, whichever is idiomatic for the existing callers. Document the choice.
- **Re-evaluation LLM call path in `reevaluate_deferred_questions`**: if the sync config handler can't easily spawn an async task (because it's deep inside a spawn_blocking DB transaction), wire an `on_evidence_policy_change` event to `BuildEventBus` and have a separate async subscriber consume the event and run the re-eval. Document the choice in the log.
- **If the triage LLM classification (`evidence_question_trivial` / `evidence_question_high_value`) is not needed** because all active policies only use time-based + demand-based conditions: skip the LLM classification and make the DSL evaluator error cleanly on those conditions if encountered. Document the simplification.

## Implementation log protocol

Append Phase 12 entry to `docs/plans/pyramid-folders-model-routing-implementation-log.md`. Include:
1. Modules created + their role
2. **Retrofit table** (file:line → action taken) — this is load-bearing for the wanderer + verifier
3. Before/after cache call ratio grep counts
4. Signal recording points added
5. Test count delta
6. Manual verification steps for the triage gate
7. Any deviations from the spec or this prompt, with rationale
8. Status: `awaiting-verification`

## Mandate

- **No backend API contract breaks.** Phase 9-11 IPC + contribution table + schema registry must keep working. Extend, don't replace.
- **No new hardcoded LLM-constraining numbers.** All thresholds flow from `evidence_policy` contribution. Defaults live in `DemandSignalAttenuation::default()` and similar — fine, those are UI defaults, not LLM constraints.
- **Fix all bugs found during the sweep.** Standard repo convention. Phase 6's wanderer found multiple issues via this exact grep pattern; you will probably find more.
- **Retrofit is the biggest scope expansion** in any phase so far. Do not give in to the temptation to skip some call sites as "not important". Every retrofitted site is a point where a repeat build of the same material stops costing real tokens. The cache-reachability ratio is a concrete measurable.
- **Match existing backend conventions.** `db.rs` table definitions live alongside helpers; serde structs live in `db.rs` or `types.rs`; new modules go in `src-tauri/src/pyramid/`.
- **Commit when done.** Single commit with message `phase-12: evidence triage + demand signals + propagation + cache retrofit sweep`. Body: 8-12 lines summarizing the demand signal pipeline, the triage gate, the deferred-question scanner, the re-evaluation IPC, and the retrofit sweep (with the before/after cache call count). Do not amend. Do not push.

## End state

Phase 12 is complete when:

1. `pyramid_demand_signals` + `pyramid_deferred_questions` tables exist with the spec's schemas.
2. `src-tauri/src/pyramid/triage.rs` implements the DSL evaluator + triage LLM call with StepContext integration.
3. `src-tauri/src/pyramid/demand_signal.rs` implements `record_demand_signal` with propagation, attenuation, loop guard, and on-demand reactivation.
4. `EvidencePolicyYaml` parses the Phase 12 fields (triage_rules, demand_signals, budget, attenuation) without breaking existing test fixtures.
5. `evidence_answering.rs::answer_questions` has a triage gate that partitions questions into answer/defer/skip via `triage.rs`.
6. Stale engine DADBEAR tick scans `pyramid_deferred_questions` for expired rows.
7. `config_contributions::reevaluate_deferred_questions` is wired up (no longer a stub).
8. `pyramid_reevaluate_deferred_questions` IPC is registered in `main.rs`.
9. Routes + IPC drill handlers record `user_drill` / `agent_query` signals fire-and-forget.
10. **StepContext retrofit sweep** has retrofitted every non-bypass `call_model_*` call site outside `llm.rs` to route through `call_model_unified_with_options_and_ctx` with a valid StepContext — the implementation log contains the full retrofit table, and `grep -c` counts flip decisively.
11. `cargo check --lib` + `cargo build --lib` clean, zero new warnings.
12. `cargo test --lib pyramid` passes with prior count + new Phase 12 tests. Same 7 pre-existing failures.
13. Implementation log Phase 12 entry complete with retrofit table + manual verification steps.
14. Single commit on branch `phase-12-evidence-triage-propagation`.

Begin with the spec. Then the code. The retrofit sweep is the biggest time sink — plan it carefully, use the Phase 6 wanderer's friction-log table as the starting point, and grep to verify each site before touching it.

Good luck. Build carefully.
