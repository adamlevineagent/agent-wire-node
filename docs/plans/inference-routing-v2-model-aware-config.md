# Inference Routing v2 — Model-Aware Config Surface

**Date:** 2026-04-21
**Status:** **RETIRED 2026-04-21** — superseded by `walker-provider-configs-and-slot-policy-v3.md`. Adam's framing that v2's per-provider tier bindings were "a surface rearrangement, not canonical" stands. The canonical carve-up is by PROVIDER TYPE (local / fleet / openrouter / market), each its own contribution, with a separate call-order config and slot-policy config. This doc is kept for historical context — the §0 "Successor frame" note was the stub; v3 is the real artifact. Do NOT implement from this plan.
**Rev:** 0.1
**Author context:** planning thread, written immediately after walker re-plan (rev 0.3) shipped to main at `4b85102` + W1/C1 fixes at `a8e413d`. Walker validates dispatch-time classification; this plan addresses the config-time surface that feeds the walker.

**Prereq reads:**
- `docs/plans/walker-re-plan-wire-2.1.md` (rev 0.3; walker's dispatch-time model)
- Walker's impl/friction logs (`walker-re-plan-wire-2.1-IMPL-LOG.md`, `...FRICTION-LOG.md`) — real debug history of the smoke that surfaced this plan
- Node canonical: `docs/SYSTEM.md` §6 (contributions), §10 (provider registry)
- Wire canonical: `/Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/SYSTEM.md` §1.6 (node = storage+compute)
- Memory: `project_provider_model_coupling_bug.md` (2026-04-12 — the root data-layer issue this plan surfaces)

---

## 0. Successor frame — v3 Walker Policy Contributions (added 2026-04-21)

This plan is **one facet of a larger v3 pattern** that crystallized during the post-walker ship-blocker debug cycle. Every ship-blocker that surfaced in the week of 2026-04-14 through 2026-04-21 — characterize's primary_model broadcast, tier_routing contamination by Local Mode, walker's `multiple_nodes_require_explicit_node_id` misclassification, `/quote`'s missing `requester_node_id` — has the same root shape: **walker behavior is hardcoded in Rust instead of contribution-driven**, and specifically, walker consumes *one* global config where it should consume *many* scoped contributions that compose at dispatch time.

### 0.1 The canonical shape

Walker's configuration surface is a **composable mesh of scoped contributions**, not a single blob. Each scope owns its own contribution; each is independently superseded; all compose at runtime into a derived effective view ("master yaml") that operators can inspect but never author directly.

Concrete facets (each a `schema_type` in the contribution store):

| Facet | Governs | Example |
|---|---|---|
| `walker_phase_policy` | Per-step config (characterize, synthesize, stale_remote, etc.) | `{step: "characterize", tier_pref: "max", body_template_ref: "quote_v2", error_policy_ref: "conservative_advance"}` |
| `walker_tier_bindings` | (tier, provider) → (model, ctx_limit) | this plan's §5.2, extended composite PK |
| `walker_error_classification` | Wire error slug → {retryable, route_skipped, call_terminal} + scope (branch_specific, all_branches, transient) | Wire publishes the authoritative seed; operators override edge cases |
| `walker_body_templates` | Per-branch required-field injection; walker composes body from template | Auto-inject `requester_node_id` into `/quote` when node knows its id |
| `walker_retry_policy` | Backoff, max attempts, per-provider overrides | Already partially in code |
| `walker_route_order` | The cascade order itself (= current `dispatch_policy.routing_rules`) | Already a contribution — v3 formalizes it under the walker_policy umbrella |

Operator supersedes any of these independently. Walker composes at dispatch time: scope → tier_pref → (provider, tier_bindings) → concrete (model, ctx, endpoint, body). Wire publishes authoritative defaults for the facets that are Wire-protocol-shaped (error classification, body templates). Adding a new facet does not require Rust changes — only a schema_definition + generation_skill + schema_annotation contribution.

### 0.2 Why this matters for v2

v2 as scoped today is the right shape for *tier_bindings* specifically — composite PK, derived-view UI, probe-driven dropdowns — but the **larger pattern** is that walker's entire policy surface decomposes this way. Framing v2 narrowly as "Inference Routing panel" risks building one facet's UI in a shape that doesn't generalize when the other facets land.

The panel should be built as **a composable view over the walker_policy contribution family**, not a purpose-built UI for tier_bindings alone. In practice:
- The panel renders one section per schema_type in the walker_policy family.
- Operator edits any scope; supersession writes a new contribution in that family.
- The "cascade preview" (§5.5 Section D) composes across all facets, not just tier_bindings.

If you land v2 first and v3 later, the v2 panel has to be rewritten. If v3 lands first and v2 lives inside it, the panel is already structured for the rest.

### 0.3 Sequencing — on ship-blockers vs refactor

The ship-blocker fixes already committed (characterize tier resolution @ `fafca94`, Local Mode as derived view @ `cc16af5`, walker classification correction @ `aaf3470`) are symptom patches inside v2/v3 territory. They're correct as emergency unblocks for tester onboarding but don't reduce the systemic debt — each is still hardcoded Rust, just with better values. The v3 plan is what retires the debt.

**Decision 2026-04-21:** ship the patches (testers unblock), then pivot v2 rev 0.2 into v3 proper. v2's §5.2 tier_bindings and §5.5 panel design carry forward as v3's `walker_tier_bindings` facet; the other facets (error_classification, body_templates, phase_policy) get their own sections in the v3 plan.

---

## 1. TL;DR

The walker now dispatches LLM calls cleanly across fleet, market, openrouter, and ollama-local route entries with per-branch error classification (a8e413d). What remains broken is **the operator's config surface**: the Inference Routing panel is free-text form fields with no knowledge of what models are actually loaded / authorized / available per provider. Operators must hand-type model IDs across providers that have incompatible ID formats (Ollama tags like `gemma4:26b` vs OpenRouter slugs like `openai/gpt-4o-mini`). A single `config.primary_model` string broadcasts across all providers, guaranteeing a crash the first time a cascade crosses providers with mismatched formats.

This plan flips the config surface from imperative ("type the right string") to declarative ("we detected these, pick one"). Probes enumerate per-provider model availability at panel-load and on demand. A tier-bindings data model replaces `primary_model`-broadcast with per-provider-per-tier resolution. The panel becomes a dropdown-driven picker with a cascade preview. First-launch wizard auto-configures based on what's detected.

Net result: impossible for an operator to set up a route that can't work, because we never show them invalid options. Same data model + probes subsume PUNCHLIST P0-1 (`resolve_ir_model` hardcoding) and the 25-site `primary_model` refactor called out in `project_provider_model_coupling_bug.md`.

---

## 2. Background — what just shipped + what remains

### 2.1 What shipped (walker cycle)

Walker re-plan rev 0.3 shipped at `agent-wire-node@4b85102`, ff-merge, 6 waves, ~60 commits. Net +~900 LOC.

Dispatch-time behaviors now live:
- Unified per-entry walker over `dispatch_policy.routing_rules[*].route_to`
- Three-tier `EntryError { Retryable, RouteSkipped, CallTerminal }` classification
- `prepare_for_replay(origin)` centralizes replay-config clearing (origin-independent, rev 0.3)
- `branch_allowed(branch, origin)` enforces "inbound jobs don't re-dispatch"
- Per-route `max_budget_credits: Option<i64>` on `RouteEntry`
- Wire rev 2.1 three-RPC client (`/quote → /purchase → /fill`) in `compute_quote_flow.rs`
- MarketSurfaceCache with 60s polling
- Bundled dispatch_policy schema family (seed + schema_definition + schema_annotation + generation skill)
- `pyramid_llm_audit.provider_id` column (Wave 1 migration)

Post-ship fixes at `a8e413d`:
- `classify_pool_400` + `classify_pool_404` + `truncate_utf8` helpers — three-tier classification on pool-branch HTTP errors (provider-rejected-model → RouteSkipped, malformed → CallTerminal)
- Option C hybrid model resolver: `entry.model_id → tier_routing(tier, provider_id) → config.primary_model` fallback
- Bundled seed ships `openrouter: model_id: "openai/gpt-4o-mini"` so fresh installs don't cascade-crash
- 13 new tests (1770 pass / 15 pre-existing fail)

### 2.2 What the walker DOES NOT solve

Walker logic is correct given the config it receives. The **config that operators produce** can still be broken shapes:

- `config.primary_model = "gemma4:26b"` is broadcast across every route entry's HTTP dispatch unless an explicit `entry.model_id` override exists.
- Operator has no visibility into whether each provider actually accepts that model string.
- `tier_routing` table EXISTS (per-provider-per-tier model), but UI doesn't populate it cleanly — operator either sets `primary_model` alone OR hand-edits `tier_routing` via raw YAML.
- `resolve_ir_model` at `chain_dispatch.rs:1198` still hardcodes `tier → config.primary_model` mapping (PUNCHLIST P0-1), bypassing tier_routing entirely.
- No source of truth for "what's installed on Ollama right now" — operator has to know.
- No source of truth for "what models does this OpenRouter key authorize" — operator has to know.
- No source of truth for "what models does the network market have offers for today" — operator has to check another panel.

Operators create crashes by setting plausible-but-wrong combinations because the system doesn't present them only with possible-right options.

### 2.3 Symptom observed during walker smoke

Mac smoke build on 2026-04-21:
1. Adam's `primary_model` carried `gemma4:26b` from a prior Local Mode session.
2. Walker's market branch skipped (`no_market_context` — orthogonal to this plan; investigation open).
3. Walker's fleet branch skipped (no peers).
4. Walker's openrouter branch sent `gemma4:26b` as the model → HTTP 400 `not a valid model ID` → 5 retries → **pre-a8e413d**: CallTerminal (over-classified), walker bubbled, ollama-local never tried.
5. Build failed at 31s | 0/0 steps.

Post-a8e413d: walker advances past openrouter 400 to ollama-local and completes. But the cascade-crossing class of bug only goes away for good when the OPERATOR STOPS CREATING INVALID CONFIGS. That's this plan.

---

## 3. Problem framing

### 3.1 From the operator's seat

> "I set my primary model to gemma4:26b because that's what I'm running locally. Then I tried to enable the compute market and OpenRouter as fallback. Everything crashed on the first cascade because apparently the cloud doesn't know what gemma4:26b is. Nobody told me."

The operator is correct. Nobody told them. The UI let them type `gemma4:26b` and accepted it globally, including for OpenRouter which would never accept that string.

The fix is at the UI layer: **don't let the operator choose a model that won't work for a given provider**. Present the list of what each provider CAN do, per-provider. Auto-pick reasonable defaults per tier. Show them what the cascade actually looks like.

### 3.2 From the system's seat

Three kinds of sources-of-truth that the operator needs, but today each lives in a different incomplete place:

| What | Where today | What's missing |
|---|---|---|
| Models loaded on local Ollama | Settings tab shows 3 active tags | No "unloaded" view; not probed dynamically; not cross-referenced with routing |
| Models OpenRouter authorizes for this key | Nowhere — operator checks the OpenRouter website | Not probed; not cached; not surfaced |
| Models the compute market has offers for | Market tab (chronicle-style) | Surfaced but not CROSS-referenced with routing — operator has to manually correlate |
| Models this operator's fleet peers serve | Fleet panel (mostly empty today) | No per-peer model/rule enumeration in UI |

The panel that should unify all of these is the Inference Routing panel — the entity that EXISTS to decide where LLM calls go. Today that panel doesn't even know about most of these data sources.

### 3.3 Root cause hierarchy

1. **Data-model root cause:** `config.primary_model: String` is a single field broadcast to every provider.
2. **Resolution-layer root cause:** `resolve_ir_model` (chain_dispatch.rs:1198) bypasses tier_routing, always returns `primary_model`. PUNCHLIST P0-1. (Walker's d509a1e consults tier_routing correctly for walker dispatches, but the IR path still skips it.)
3. **Surface root cause:** No config UI presents model lists scoped to the provider the operator is configuring.
4. **Cross-cutting root cause:** No systematic probe layer — each provider's "what do I know about" is either ad-hoc IPC or missing entirely.

This plan addresses roots 3 + 4 (the surface) AND fixes root 1 (the data model) as a necessary companion. Root 2 (PUNCHLIST P0-1) is cross-cut: needs to be fixed or the new surface writes to an authoritative tier_routing that IR dispatch still ignores. Scope decision: **fix P0-1 as a Wave 0 prereq** (this plan), not as a separate follow-up.

---

## 4. Design principles

1. **Never show the operator an invalid choice.** If OpenRouter's `/v1/models` list doesn't include `gemma4:26b`, the OpenRouter row's Model dropdown doesn't offer `gemma4:26b`. Free-text entry is a fallback with a red warning, not the primary path.

2. **Autodiscovery over manual configuration.** Probe what we can. Cache probe results with TTLs (Ollama: every panel mount; OpenRouter: once per hour; market: already in MarketSurfaceCache at 60s). Operator's job is to CONFIRM + override, not to KNOW.

3. **Progressive disclosure.** Default view: "cascade of providers, model per tier auto-resolved, preview." Advanced view: per-route model override, per-entry max_budget, per-tier model pinning. Don't ambush new operators with 40 form fields.

4. **Fail loud, fail early.** If probe fails (Ollama not running, OpenRouter key invalid), surface it in the panel with a clear fix-it action. Don't let the operator save a config that references unreachable providers.

5. **Contribution-native.** All persistent changes route through contribution supersession (dispatch_policy + tier_routing + compute_participation_policy). Follows node §6 + Wire §1.1 everything-is-a-contribution.

6. **Panel is declarative surface over a single data model.** Behind the dropdowns, one canonical source of truth: `tier_bindings` map on the tier_routing contribution. Walker reads it; UI writes it; IR path reads it (post-P0-1 fix). Never duplicate state.

---

## 5. Architecture

### 5.1 Four layers

```
┌─────────────────────────────────────────────────────────────────┐
│ LAYER 4: Panel (React/TS) — InferenceRoutingPanel v2            │
│   - Dropdown-driven tier bindings                               │
│   - Route order drag-reorder                                    │
│   - Cascade preview                                             │
│   - Probe status indicators                                     │
│   - First-launch wizard overlay                                 │
└──────────┬──────────────────────────────────────────────────────┘
           │ IPC calls
┌──────────▼──────────────────────────────────────────────────────┐
│ LAYER 3: IPC handlers (Rust, Tauri #[tauri::command])           │
│   - pyramid_probe_ollama_models                                 │
│   - pyramid_probe_openrouter_models                             │
│   - pyramid_fleet_models_available                              │
│   - pyramid_market_models (extends existing)                    │
│   - pyramid_get_tier_bindings / pyramid_set_tier_bindings       │
│   - pyramid_apply_inference_routing (transactional multi-write) │
└──────────┬──────────────────────────────────────────────────────┘
           │ probes + contribution writes
┌──────────▼──────────────────────────────────────────────────────┐
│ LAYER 2: Probe clients (Rust)                                   │
│   - ollama_probe.rs — GET /api/tags                             │
│   - openrouter_probe.rs — GET /v1/models                        │
│   - fleet_roster_introspect — in-memory read                    │
│   - MarketSurfaceCache (existing, extend model detail surfacing) │
└──────────┬──────────────────────────────────────────────────────┘
           │ per-provider-per-tier model resolution
┌──────────▼──────────────────────────────────────────────────────┐
│ LAYER 1: tier_bindings data model                               │
│   - Extension of pyramid_tier_routing table                     │
│   - (tier_name, provider_id) → model_id                         │
│   - Walker's d509a1e resolver already reads this                │
│   - IR dispatcher WILL read this (via P0-1 fix in Wave 0)        │
└─────────────────────────────────────────────────────────────────┘
```

### 5.2 Data model — tier_bindings

**Current state (read from earlier investigation + db.rs:1536-1547):**

```sql
CREATE TABLE pyramid_tier_routing (
    tier_name TEXT PRIMARY KEY,
    provider_id TEXT NOT NULL REFERENCES pyramid_providers(id),
    model_id TEXT,
    context_limit INTEGER,
    max_completion_tokens INTEGER,
    ...
);
CREATE INDEX ... ON pyramid_tier_routing(provider_id);
```

**Today's issue:** PRIMARY KEY is `tier_name` alone. One provider_id per tier. So you can't have BOTH `(tier=mid, provider=openrouter, model=gpt-4o-mini)` AND `(tier=mid, provider=ollama-local, model=gemma4:26b)` coexist — the current tier_routing only lets ONE of those per tier.

**v2 schema change:** extend the PRIMARY KEY to `(tier_name, provider_id)`. Now tier bindings are per-tier-per-provider. Walker resolves:
- `entry.model_id` if explicitly set on route entry
- Else look up `tier_bindings(tier_name, entry.provider_id)` from `pyramid_tier_routing`
- Else fall through to `config.primary_model` (legacy — should become unreachable post-migration)

**Schema migration (Wave 1):**
1. ALTER TABLE to drop old PK, add composite PK `(tier_name, provider_id)`.
2. Existing rows preserved (each is `(tier, one provider)`, becomes a valid tier_bindings row).
3. `ProviderRegistry::get_tier(tier_name)` still works by returning the first matching provider for back-compat, OR is deprecated in favor of `get_tier_for_provider(tier_name, provider_id)`.

**Contribution-native write path:** changes to tier_bindings go through `tier_routing` contribution supersession (not direct DB writes). Existing `config_contributions.rs::dispatch("tier_routing")` arm already calls `upsert_tier_routing` — that helper already does DELETE + INSERT per contribution, idempotent.

### 5.3 Probe layer

#### 5.3.1 Ollama probe — `src-tauri/src/pyramid/ollama_probe.rs` (new)

```rust
pub struct OllamaModel {
    pub name: String,              // "gemma4:26b"
    pub size_bytes: u64,
    pub parameter_size: String,    // "26B"
    pub quantization: String,      // "Q4_K_M"
    pub family: String,            // "gemma"
    pub modified_at: DateTime<Utc>,
}

pub struct OllamaProbeResult {
    pub base_url: String,
    pub reachable: bool,
    pub models: Vec<OllamaModel>,
    pub probe_error: Option<String>,    // non-fatal; surface in UI
    pub probed_at: DateTime<Utc>,
}

pub async fn probe_ollama(base_url: &str) -> OllamaProbeResult {
    // GET {base_url}/api/tags
    // Parse response per Ollama docs
    // Return structured result; swallow errors into probe_error
}
```

Cache in-memory on `PyramidState` with a 5-minute TTL. Invalidate on manual "refresh" + on provider config change.

#### 5.3.2 OpenRouter probe — `src-tauri/src/pyramid/openrouter_probe.rs` (new)

```rust
pub struct OpenRouterModel {
    pub id: String,                  // "openai/gpt-4o-mini"
    pub name: String,                // human-readable
    pub context_length: u32,
    pub prompt_price_per_m: f64,     // USD per 1M input tokens
    pub completion_price_per_m: f64, // USD per 1M output tokens
    pub top_provider: String,        // "openai"
    pub free_tier: bool,
}

pub struct OpenRouterProbeResult {
    pub key_valid: bool,
    pub models: Vec<OpenRouterModel>,
    pub rate_limit_remaining: Option<u32>,
    pub probe_error: Option<String>,
    pub probed_at: DateTime<Utc>,
}

pub async fn probe_openrouter(api_key: &str) -> OpenRouterProbeResult {
    // GET https://openrouter.ai/api/v1/models with Authorization: Bearer <key>
    // Parse response per OpenRouter docs
    // Surface key_valid=false on 401
    // Swallow network errors into probe_error
}
```

Cache in-memory with 1-hour TTL (OpenRouter catalog doesn't churn often). Invalidate on manual refresh + on credential store update.

#### 5.3.3 Fleet roster introspection — helper (not a new module)

Fleet roster is already in-memory at `LlmConfig.fleet_roster: Option<Arc<RwLock<FleetRoster>>>`. Introspection is a read of `.peers` — shape:

```rust
pub struct FleetPeerSummary {
    pub peer_id: String,
    pub handle: Option<String>,
    pub models_loaded: Vec<String>,
    pub serving_rules: Vec<String>,
    pub queue_depth: i64,
    pub last_seen_secs_ago: u64,
    pub is_stale: bool,     // against policy.peer_staleness_secs
}

pub async fn summarize_fleet_peers(
    roster: &FleetRoster,
    staleness_secs: u64,
) -> Vec<FleetPeerSummary> { ... }
```

#### 5.3.4 Market surface model detail — extends existing MarketSurfaceCache

Walker's MarketSurfaceCache already holds `catalog.model_ids_sorted + models: HashMap<String, MarketSurfaceModel>`. Existing IPC `pyramid_market_models` returns a flat list (walker Wave 4 task 29).

Extend to include per-model detail:

```rust
pub struct MarketModelSummary {
    pub model_id: String,
    pub active_offers: u32,
    pub price: Option<PriceAggregate>,      // min/median/max rate_per_m
    pub performance: Option<PerformanceAggregate>, // p50/p95 latency, tps
    pub top_of_book: Option<TopOfBookEntry>,
    pub last_offer_update_at: DateTime<Utc>,
}
```

Existing cache already has these from Wire's `/market-surface` response. Just need the IPC to project them out.

### 5.4 IPC surface

All under existing `pyramid_*` naming convention. All `#[tauri::command]` handlers registered in main.rs.

```rust
// Probes
#[tauri::command]
async fn pyramid_probe_ollama_models(state: State<AppState>) 
    -> Result<OllamaProbeResult, String>;

#[tauri::command]
async fn pyramid_probe_openrouter_models(state: State<AppState>) 
    -> Result<OpenRouterProbeResult, String>;

#[tauri::command]
async fn pyramid_fleet_models_available(state: State<AppState>) 
    -> Result<Vec<FleetPeerSummary>, String>;

#[tauri::command]
async fn pyramid_market_models_detailed(state: State<AppState>) 
    -> Result<Vec<MarketModelSummary>, String>;

// tier_bindings CRUD
#[tauri::command]
async fn pyramid_get_tier_bindings(state: State<AppState>) 
    -> Result<TierBindings, String>;
// TierBindings = HashMap<TierName, HashMap<ProviderId, ModelId>>

#[tauri::command]
async fn pyramid_set_tier_bindings(
    bindings: TierBindings,
    change_note: String,
    state: State<AppState>,
) -> Result<String /* new_contribution_id */, String>;

// Transactional multi-write
#[tauri::command]
async fn pyramid_apply_inference_routing(
    patch: InferenceRoutingPatch,
    state: State<AppState>,
) -> Result<InferenceRoutingResult, String>;
// patch includes: tier_bindings changes, dispatch_policy route reorder,
// per-entry model_id overrides. All applied in one contribution supersession
// chain so partial failures don't leave inconsistent state.
```

### 5.5 UI layer — InferenceRoutingPanel v2

**Location:** `src/components/settings/InferenceRoutingPanel.tsx` (replaces v1 from walker Wave 4 task 30).

**Sections top-to-bottom:**

#### Section A: Provider Status (autodetected)

```
┌──────────────────────────────────────────────────────────────────┐
│ ● Local (Ollama)     http://localhost:11434                     │
│   3 models loaded: gemma4:26b (active), gemma4:e4b, qwen3.5:27b │
│   Last probed: 2s ago  [Refresh]                                 │
├──────────────────────────────────────────────────────────────────┤
│ ● Cloud (OpenRouter) Key valid                                   │
│   87 models authorized                                           │
│   Last probed: 12m ago  [Refresh]                                │
├──────────────────────────────────────────────────────────────────┤
│ ○ Fleet              0 peers online                              │
│   No peers in roster                                             │
├──────────────────────────────────────────────────────────────────┤
│ ● Network (Market)   12 models available, 2 new since review    │
│   Top by offer: gemma4:26b (8 offers), gpt-4o-mini (3 offers)   │
│   [Review new]                                                    │
└──────────────────────────────────────────────────────────────────┘
```

Filled circle = reachable + has models. Empty circle = unreachable or empty. Each row has a refresh button that calls the corresponding probe IPC.

#### Section B: Tier Bindings (primary picker)

Grid with tiers as rows, providers as columns. Dropdowns populated by provider status in Section A.

```
Tier        Local (Ollama)     OpenRouter           Network (cheapest)  Fleet
─────────── ──────────────── ──────────────────── ─────────────────── ──────
mid         gemma4:26b     ▼  openai/gpt-4o-mini▼  auto-cheapest    ▼  auto
high        qwen3.5:27b    ▼  anthropic/claude-3-5 auto-balanced    ▼  auto
max         —                  openai/gpt-5        ▼  —                  —
stale_local gemma4:e4b     ▼  —                    —                    —
```

Each dropdown shows ONLY the models that provider has available. `—` means "this tier doesn't use this provider"; explicit operator choice via a "disable this cell" option. `auto-cheapest` / `auto-balanced` are Network-only magic options that let Wire pick at /quote time based on `latency_preference`.

Dropdown item format: `gemma4:26b (26B · Q4_K_M · 16.8 GB)` for Ollama; `openai/gpt-4o-mini ($0.15/M in, $0.60/M out, 128K ctx)` for OpenRouter; `any (cheapest)` / `any (balanced)` for Network.

#### Section C: Route Order (reorder via up/down buttons, same as v1)

```
#   Provider        Enabled   Resolved Model (for mid tier)         Reorder
1   market          [✓]       (tier-resolved: cheapest)             ↑↓ ×
2   fleet           [✓]       (tier-resolved: auto)                 ↑↓ ×
3   openrouter      [✓]       openai/gpt-4o-mini                    ↑↓ ×
4   ollama-local    [✓]       gemma4:26b                            ↑↓ ×
+ Add route entry
```

Route entries show the RESOLVED model for a representative tier (default: mid). Explicit per-entry `model_id` override exposed via an "Override..." drill-in. Most operators never touch it; tier bindings are enough.

#### Section D: Cascade Preview

```
Cascade for "mid" tier (evidence work):
  1. market     → 8 offers for gemma4:26b, cheapest $0.001/M in. Try.
  2. fleet      → 0 peers. Skip.
  3. openrouter → openai/gpt-4o-mini. $0.15/M in.
  4. ollama-local → gemma4:26b. Local concurrency=1.

For "high" tier (synthesis):
  [similar ...]
```

Rendered from the tier_bindings + route_order + current provider status. Updates live as operator edits.

#### Section E: First-launch wizard (overlay)

Fires when `tier_bindings` is empty (fresh install). Runs all probes, proposes a sensible config, offers Accept / Customize.

```
We detected:
  Local Ollama: gemma4:26b, gemma4:e4b  (ready)
  OpenRouter: key valid, 87 models authorized
  Network: 12 models on the market
  Fleet: 0 peers

Proposed routing:
  Tier "mid" (evidence):  market → openrouter → ollama-local
    - openrouter: openai/gpt-4o-mini
    - ollama-local: gemma4:26b
  Tier "high" (synthesis): openrouter → ollama-local
    - openrouter: anthropic/claude-3-5-sonnet
    - ollama-local: qwen3.5:27b
  [...]

[Accept defaults]  [Customize]  [Skip (use bundled seed)]
```

Accept writes the proposed bindings + route order as a fresh contribution supersession.

#### Section F: Banner nudges (ambient)

- "New model available on the network for tier 'mid': anthropic/claude-3-5-haiku. Add to routing? [Yes] [Skip]"
- "Ollama model 'gemma4:26b' was unloaded locally but your routing references it. [Load now] [Switch to gemma4:e4b] [Dismiss]"
- "OpenRouter key authorized 3 new models since last review. [Review] [Dismiss]"

---

## 6. Implementation waves

Serial implementer pattern, same as walker. Workflow agent → serial verifier → wanderer (at specified gates). No pyramid queries. Impl log + friction log maintained after every commit. Pre-flight Q&A before first commit.

### Wave 0 — Prereqs + PUNCHLIST P0-1 fix (~500 LOC)

1. **Fix PUNCHLIST P0-1** — `chain_dispatch.rs::resolve_ir_model` at line 1198 bypasses tier_routing. Change to consult `ProviderRegistry::resolve_tier(tier_name, provider_id)` (already exists per walker d509a1e notes). Fall through to `config.primary_model` only if tier_routing has no entry. Same semantic as walker's Option C resolver.
2. **Schema migration for tier_routing composite PK** — ALTER TABLE PK to `(tier_name, provider_id)`. Idempotent pragma_table_info check pattern (same as walker's `provider_id` migration).
3. **`TierBindings` Rust type** — `HashMap<String /* tier_name */, HashMap<String /* provider_id */, String /* model_id */>>`. Serializable to/from YAML.
4. **`tier_routing` contribution body shape update** — current YAML is flat list of `(tier_name, provider_id, model_id)` rows; extend to support multiple rows per tier_name. Backward-compat: old flat shape still parses (each row becomes one tier_binding entry).
5. **Probe module skeletons** — `ollama_probe.rs`, `openrouter_probe.rs` with `unimplemented!()` stubs. Types defined. No HTTP yet.
6. **IPC handler stubs** — all 7 handlers registered in main.rs, return `unimplemented!()`. Wires them to frontend so Wave 2 has endpoints.
7. **Verifier pass + unit tests** for PUNCHLIST P0-1 fix + schema migration + TierBindings serde.

### Wave 1 — Probe implementations (~400 LOC)

8. **Ollama probe body** — `reqwest::get({base_url}/api/tags)` + parse. Handle: connection refused (Ollama not running), malformed JSON, empty model list. 5-min in-memory cache keyed by base_url.
9. **OpenRouter probe body** — `reqwest::get("https://openrouter.ai/api/v1/models")` with `Authorization: Bearer <key>`. Handle: 401 (key invalid), 429 (rate limit), network error. 1-hour cache.
10. **Fleet roster summarizer** — reads `LlmConfig.fleet_roster` + current time. Returns `Vec<FleetPeerSummary>` with staleness flags.
11. **MarketSurfaceCache model detail extension** — implement `pyramid_market_models_detailed` IPC. Read existing cache, project `MarketModelSummary`.
12. **Probe caching layer** — in-memory + TTL per probe. Invalidation hook on config change.
13. **Unit tests** for each probe — mock HTTP, verify parsing + cache behavior + error paths.

### Wave 2 — InferenceRoutingPanel v2 (~700 LOC React/TS)

14. **Delete v1** at `src/components/settings/InferenceRoutingPanel.tsx`. Replace with v2 scaffolding.
15. **Section A: Provider Status** — four rows, each calling its probe IPC on mount + on refresh click. Status indicators (filled/empty circle). Probe timestamp + error surfacing.
16. **Section B: Tier Bindings grid** — dropdown per (tier, provider) cell. Populated from corresponding probe's model list. Writes to local state; debounced push to `pyramid_set_tier_bindings` on blur/Apply.
17. **Section C: Route Order** — reuse v1 up/down buttons + enable/disable + remove. Add "Override model..." drill-in per entry. Show resolved model inline.
18. **Section D: Cascade Preview** — pure derived from state. Rebuild on every state change. Uses current tier bindings + route order + probe status.
19. **Section E: First-launch wizard overlay** — render conditional on `tier_bindings === {}`. Probe-all-providers flow. Proposed config rendered as Sections B+C preview. Accept writes via `pyramid_apply_inference_routing`.
20. **Section F: Banner nudges** — on panel mount, compare current probe state against current bindings. Surface nudges. Can defer to post-v2 polish; skeleton in v2.
21. **Apply button + change_note** — debounced save pattern (walker v1 had this, preserve). Transactional write via `pyramid_apply_inference_routing`.
22. **Verifier + wanderer pass** — end-to-end: fresh install, launch, wizard runs, accept defaults, panel renders state, manual edit of a tier binding, Apply, re-mount — state persists.

### Wave 3 — Transactional write + consistency (~200 LOC)

23. **`pyramid_apply_inference_routing` handler** — single IPC that:
    - Takes `InferenceRoutingPatch { tier_bindings_changes, route_order_changes, per_entry_overrides }`
    - Wraps all writes in a single SQL transaction
    - Supersedes `tier_routing` and `dispatch_policy` contributions atomically
    - Fires ConfigSynced for both schema types
    - Returns list of new contribution IDs + any validation errors
    - On partial failure, rolls back all changes
24. **Idempotency** — repeated Apply with identical patch is a no-op (no supersession created).
25. **Validation** — reject patches where route entries reference unknown provider_ids, or where tier bindings reference models the probe doesn't list for that provider (with an override flag to bypass — "I know what I'm doing").
26. **Verifier** — integration test: rapid click-Apply-Apply-Apply produces exactly one supersession per distinct patch.

### Wave 4 — `primary_model` deprecation + provider_model_coupling refactor (~400 LOC)

Ties into memory `project_provider_model_coupling_bug.md` — the 25+-site refactor.

27. **Audit every call site of `config.primary_model`** — grep, triage:
    - Sites that should resolve via tier_bindings: refactor to use `resolve_tier_for_provider(tier, provider_id)`.
    - Sites that genuinely need "a default model when we can't determine tier" (e.g., legacy callers, tests): keep fallback but log a deprecation warning.
    - Sites that cache primary_model: eliminate the cache, read tier_bindings on demand.
28. **Deprecation path** — mark `config.primary_model` as `#[deprecated(note = "use tier_bindings(tier, provider_id) resolver")]`. Each fixed call site removes a warning.
29. **Integration test** — operator with `primary_model = "gemma4:26b"` PLUS tier_bindings populated, makes calls that would previously mis-resolve. Assert every path uses tier_bindings.
30. **Final wanderer pass** — full stack: fresh install, wizard, Apply, trigger build, verify chronicle shows correct model per route entry, verify audit rows carry correct provider_id.

### Wave 5 — Cleanup + polish (~150 LOC)

31. **Remove v1 Inference Routing panel components** — any residual.
32. **Dispatch_policy bundled seed update** — remove explicit `openrouter: model_id: "openai/gpt-4o-mini"` from the route entry (now resolved via tier_bindings). Route entries in seed are pure `{provider_id}` again.
33. **Documentation sweep** — update `docs/SYSTEM.md` §10 to point at the new tier_bindings data model. Update CHAIN-DEVELOPER-GUIDE if it references model tier resolution.
34. **Final full-feature wanderer** — end-to-end on fresh install: boot → wizard → build → verify events → toggle Local Mode → verify bindings survive → switch OpenRouter key → verify probe re-fires + UI updates.

**Total: ~2350 LOC. Realistic 4-6 sessions** including 2 audit rounds, verifier + wanderer per wave, and the cross-cutting primary_model refactor. Estimate could drift higher if Wave 4's site-by-site audit surfaces unexpected coupling.

---

## 7. Cross-repo / cross-plan dependencies

### 7.1 PUNCHLIST P0-1 (`resolve_ir_model` hardcoded)

Scheduled as Wave 0 task 1 of this plan. Fixing P0-1 in isolation without the tier_bindings surface means the IR dispatcher STILL doesn't have per-provider model values to read (tier_routing exists but isn't populated well). Fix them together.

### 7.2 `project_provider_model_coupling_bug.md`

25+-site refactor of `config.primary_model` → tier-resolved values. Scheduled as Wave 4 of this plan. This plan is the natural execution vehicle for that memory item — the tier_bindings data model + per-route resolver make the refactor actionable.

### 7.3 Walker plan (rev 0.3 shipped)

This plan builds on:
- Walker's `entry.model_id` override in `RouteEntry` (already shipped)
- Walker's d509a1e Option C resolver (already shipped — reads tier_routing)
- MarketSurfaceCache + `pyramid_market_models` IPC (already shipped)
- `pyramid_llm_audit.provider_id` column (already shipped — tier_bindings will populate it correctly)

No walker changes needed for this plan. Walker's interface is the contract.

### 7.4 Wire side

**No Wire-side changes required.** Market's `/market-surface` already exposes the per-model details this plan consumes. OpenRouter's API is third-party.

### 7.5 Compute_market_context = None investigation (open)

Not addressed by this plan. Separate investigation ticket. Symptom: walker's market-branch runtime gate observes `config.compute_market_context.is_none()` despite boot hydration at main.rs:11842-11909 setting it. Post-walker smoke confirmed this is orthogonal to the cascade-crossing bug this plan addresses. Investigation is a separate chip.

---

## 8. Explicitly NOT in scope

- **Rebuild of the credentials/OpenRouter-key UI.** Inference Routing panel consumes an existing OpenRouter key; doesn't rotate / manage it. Credentials surface is a separate system.
- **Ollama installation/launch management.** Panel reports whether Ollama is reachable; doesn't start it or install models. That's Ollama's own surface (or a future node-side wrapper).
- **Multi-endpoint Ollama** (e.g., Ollama on a second LAN host). v1: single base_url per provider row. v2: multi-endpoint is future work.
- **Per-model cost caps / budget monitoring.** `max_budget_credits` already exists per route entry; this panel surfaces it via Override but doesn't build new budget tracking UX.
- **Compute market speculation / parallel purchase.** Deferred in walker plan; not re-opened here.
- **Chain-step-level model override.** Operators can't say "this specific chain step uses gpt-5 regardless of tier." Tier is the granularity; chain step inherits from tier.
- **Auto-adjustment of tier_bindings based on observed costs.** Operator manually curates.
- **SSE for market-surface.** Already deferred in walker plan; still polling at 60s.
- **Dynamic provider registration.** Providers are still YAML/contribution-registered per existing ai_registry pattern. No UI for "add a new provider type."
- **Windows/Linux-specific UI.** Tauri handles cross-platform; no platform-specific UI code.

---

## 9. Known tradeoffs (documented, accepted)

1. **Probe staleness windows.** Ollama: 5-min. OpenRouter: 1-hour. Market: 60s. Operator can manually refresh but usually doesn't need to. Model appearing/disappearing within the window is surfaced at next probe or on next panel mount. Tradeoff: fewer HTTP calls vs. less-fresh data. Acceptable.

2. **First-launch wizard is opinionated.** Proposes a routing that may not match every operator's intent. Mitigation: `[Customize]` path + `[Skip]` path. Operators rebuild via editing after wizard if needed.

3. **Composite PK migration on `pyramid_tier_routing`** is a schema change. Existing rows migrate forward cleanly (each old row becomes one tier_binding). Rollback requires reversing the PK change; SQLite doesn't support DROP PRIMARY KEY directly. Mitigation: migration is additive (new PK allows superset), rollback is rare in single-operator wipe-and-fresh-install model.

4. **OpenRouter key probe calls OpenRouter's API.** Increments no credits (list endpoint is free) but does require valid key. If key is invalid, panel surfaces error. Acceptable.

5. **tier_bindings as HashMap<String, HashMap<String, String>> in TypeScript** loses some type safety. Mitigation: generate TS types from Rust (ts-rs) OR maintain hand-written type alias with a comment linking to Rust source. Defer ts-rs to its own initiative.

6. **Probe-layer test coverage relies on mockito or wiremock.** Not currently a Rust dev dep in this repo. Wave 1 adds `mockito = "1"` to dev-dependencies. Acceptable.

7. **First-launch wizard runs on FRESH installs only.** An operator with pre-v2 tier_routing gets dropped into the standard panel (no wizard). Edge case: operator clears tier_routing manually; wizard re-fires. Edge case acceptable.

8. **`config.primary_model` deprecation takes one cycle.** Wave 4 fixes call sites but field stays on LlmConfig struct (with `#[deprecated]` attribute) for one rev so external code (tests, pre-rev callers) compiles. Hard deletion in the following plan cycle.

9. **IR dispatcher vs walker dispatcher divergence.** Post-Wave 0-task-1 fix, both read tier_routing via `ProviderRegistry::resolve_tier(tier, provider_id)`. Two call sites for the same resolution logic — acceptable because they serve different caller shapes (IR step via chain_dispatch vs walker via call_model_unified).

10. **Banner-nudge fatigue.** Three potential nudge classes (new market model, missing local model, new OpenRouter model). Mitigation: dismissible with "don't show again for 24h" localStorage flag per nudge class.

---

## 10. Acceptance criteria

- Wave 0: PUNCHLIST P0-1 closed. IR dispatcher consults tier_routing via `ProviderRegistry::resolve_tier(tier, provider_id)`. Unit test: `(tier=mid, provider=ollama-local)` returns ollama model; `(tier=mid, provider=openrouter)` returns openrouter model, even when both tier bindings exist simultaneously.
- Wave 0: `pyramid_tier_routing` PK is composite `(tier_name, provider_id)`. Migration is idempotent. Existing data preserved.
- Wave 1: All four probes (Ollama, OpenRouter, Fleet, Market) are callable via IPC, return structured data, handle unreachable / error cases gracefully.
- Wave 2: Fresh install launches → wizard overlay appears → probe status renders → Accept writes new contributions → panel re-mounts with written state → cascade preview accurate.
- Wave 2: Operator with stale `primary_model = "gemma4:26b"` can re-open Inference Routing panel and see each provider offering ONLY its valid models. Selecting a valid openrouter model eliminates the gemma4:26b leak.
- Wave 3: Single `pyramid_apply_inference_routing` call writes all changes transactionally. Partial failure rolls back. Idempotent re-apply is a no-op.
- Wave 4: `config.primary_model` has no unmarked call sites in src/. All references either use tier-resolved values or carry a `#[deprecated]` warning.
- End-to-end: operator with no prior setup installs the app, goes through wizard, triggers a mid-tier evidence build. Build succeeds because walker + IR dispatcher both receive valid per-provider model IDs. Chronicle shows resolved model matches what the panel displays.
- End-to-end: operator switches OpenRouter API key. Panel re-probes, shows different authorized model list. Tier bindings that reference now-unauthorized models get a red warning. Operator fixes or dismisses.
- `cargo check` default target green. `cargo test --lib` passes for new test modules. `npm run build` green. `bun run tauri build` produces a usable bundle.

---

## 11. Open questions

These need decisions before Wave 2 starts. Some should go to Adam directly; some may surface Wire-dev coordination; some are implementer judgment calls.

### For Adam

1. **Wizard default proposed routing** — the "sensible defaults" need explicit operator endorsement. For tier "mid", is the default cascade `[market, fleet, openrouter, ollama-local]`? For tier "max", is it `[openrouter, ollama-local]`? Define the table before Wave 2 or leave it to implementer judgment per existing bundled seed's precedent.

2. **Should tier bindings ship bundled defaults too?** Fresh install could have pre-populated tier_bindings instead of running the wizard — but that requires knowing operator context (OpenRouter key validity, local Ollama availability) which is probe-only. Suggest: bundled defaults include `(tier=mid, provider=openrouter, model=openai/gpt-4o-mini)` and `(tier=high, provider=openrouter, model=anthropic/claude-3-5-sonnet)` as reasonable cloud-only defaults. Wizard supplements with local + market once probes run. Confirm.

3. **`auto-cheapest` / `auto-balanced` as Network tier binding values** — are these acceptable ENUM values for `tier_bindings[tier][provider=market]`, or should Network always resolve dynamically at quote-time regardless of tier binding? If the latter, tier bindings for Network are meaningless and the column is dropped from Section B.

4. **First-launch wizard vs. ambient banner** — some operators may disable the wizard overlay (power users restoring from backup). Escape hatch via `[Skip (use bundled seed)]`, but confirm skip is one-click, not hidden.

5. **Deprecation cadence for `primary_model`** — Wave 4 flips it to `#[deprecated]` with call sites migrated. Follow-up plan hard-deletes. One cycle, two cycles, never? Confirm.

### For Wire dev

6. **MarketSurfaceCache: are we still good at 60s poll?** Walker plan deferred SSE. With v2 panel showing per-model offer counts prominently, operators might expect real-time updates. If SSE is cheap to stand up, consider moving the deferred-v2 forward in this plan's scope. Ask.

7. **OpenRouter's `/v1/models` response format stability** — third-party API, not Wire. Just flag: if OpenRouter changes response shape, probe breaks silently. Confirm schema versioning strategy (pin on first probe, revalidate monthly).

### For implementer judgment (not escalated)

8. **Probe cache TTLs** — Ollama 5-min, OpenRouter 1-hour, Market 60s. Tune per observed probe call patterns post-ship.

9. **Section E wizard's "customize" vs "accept defaults"** — UX split. Defaults is one click; customize opens the full panel with values prepopulated. Acceptable either way.

10. **Per-entry `model_id` override UX** — drill-in modal vs. inline field. Inline is faster; drill-in is less cluttered for the 80% of operators who never override. Default: drill-in.

---

## 12. Audit history

- **Rev 0.1 (2026-04-21)** — initial draft. Written immediately after walker rev 0.3 ship + a8e413d W1/C1 fix. Planning thread's context is fresh. Ready for Stage 1 informed audit.

**Planned audit cadence (same as walker):**
- Stage 1 informed pair — two auditors with full plan + source. Comprehensive coverage.
- Stage 2 discovery pair — two auditors with purpose statement only + short known-issues list. Find-what-they-find.
- Pre-flight Q&A round with Adam — implementer-style clarification questions.
- Rev 0.2 applies audit findings.
- Rev 0.3 applies pre-flight answers.
- Hand off to implementation thread.

---

## 13. Glossary

| Term | Definition |
|---|---|
| **Tier** | Named LLM quality/capability level (`mid`, `high`, `max`, `stale_local`, etc.). Tier is the granularity at which operators express "how much effort do I want for this kind of work." |
| **Tier binding** | A mapping `(tier_name, provider_id) → model_id`. Tells the walker/IR-dispatcher what model to use when this tier is asked for at this provider. |
| **Tier bindings (plural, as a data structure)** | `HashMap<TierName, HashMap<ProviderId, ModelId>>`. Full operator-configured tier × provider → model map. |
| **Provider** | A LLM backend: `ollama-local`, `openrouter`, `market`, `fleet`, or any registered provider row. Walker's route entries reference these by `provider_id`. |
| **Route entry** | `RouteEntry { provider_id, model_id?, tier_name?, is_local, max_budget_credits? }`. One entry in a `routing_rules[*].route_to` list. Walker iterates these. |
| **Probe** | An async call to a provider's catalog endpoint to enumerate what models it can serve for this operator. Cached with a TTL. |
| **Cascade** | The ordered sequence of route entries the walker iterates. Top of list tried first. First success wins; failures advance. |
| **Auto-resolved model** | The model the walker will USE for a given route entry, given current tier bindings. Shown in Section C next to each entry. |
| **Override model** | An explicit `entry.model_id` set per route entry. Wins over tier_bindings when both are set. |
| **Wizard** | First-launch overlay that runs all probes + proposes a default tier_bindings + route order. |

---

## 14. Code surface map (files that will be touched)

**Rust (src-tauri/src/):**
- `pyramid/llm.rs` — NO changes to walker logic; may need minor TierBindings deserialization helper additions near the Option C resolver (line ~886+ per rev 0.3).
- `pyramid/dispatch_policy.rs` — no changes; route entries stay as shipped.
- `pyramid/provider.rs` — `ProviderRegistry::resolve_tier(tier, provider_id)` already exists; confirm it's sufficient for IR dispatcher use. Possibly add `resolve_tier_for_provider` for clarity.
- `pyramid/chain_dispatch.rs` — **PUNCHLIST P0-1 FIX**: `resolve_ir_model` at line 1198 consults tier_routing.
- `pyramid/db.rs` — tier_routing schema migration (composite PK). Migration block near existing `pyramid_tier_routing` CREATE TABLE.
- `pyramid/config_contributions.rs` — `tier_routing` dispatcher arm already exists; may need update for new multi-row-per-tier YAML shape.
- `pyramid/ollama_probe.rs` — NEW.
- `pyramid/openrouter_probe.rs` — NEW.
- `pyramid/market_surface_cache.rs` — EXTEND (expose per-model details).
- `pyramid/mod.rs` — register new modules.
- `main.rs` — register 7 new IPC handlers (Section 5.4). `tauri::generate_handler!` macro invocation.

**Frontend (src/components/settings/):**
- `InferenceRoutingPanel.tsx` — FULL REWRITE (Wave 2). v1 version was walker Wave 4 task 30.
- `Settings.tsx` — update import/insert point if component path/name changes.
- New: `src/components/settings/ProviderStatusRow.tsx`, `TierBindingsGrid.tsx`, `RouteOrderList.tsx`, `CascadePreview.tsx`, `FirstLaunchWizard.tsx` (or single-file depending on team preference).
- `src/types/inference_routing.ts` — TS types for TierBindings, probe responses, etc.

**Data / bundled:**
- `src-tauri/assets/bundled_contributions.json` — update dispatch_policy seed (remove explicit `openrouter: model_id:`), add initial tier_routing bundled seed.

**Docs:**
- `docs/SYSTEM.md` — §10 provider registry section updates.
- `docs/plans/inference-routing-v2-model-aware-config.md` — THIS FILE.
- `docs/plans/inference-routing-v2-IMPL-LOG.md` — NEW, created at first impl commit (see §15).
- `docs/plans/inference-routing-v2-FRICTION-LOG.md` — NEW (see §15).
- `docs/plans/inference-routing-v2-HANDOFF.md` — NEW (see §15).

---

## 15. Handoff discipline (reuses walker pattern)

Same as walker cycle. Implementation thread must:

1. **Create two log files at first commit on the implementation branch:**
   - `docs/plans/inference-routing-v2-IMPL-LOG.md` — append-only, one entry per commit. Plan task cited, changes summary, cargo check + test status, deviations from plan.
   - `docs/plans/inference-routing-v2-FRICTION-LOG.md` — real-time record of surprises. Newest at top. Flag plan errors, spec ambiguities, codebase learning moments.

2. **Orchestration pattern:** workflow agent → serial verifier → wanderer at specified gates. Small work (<500 LOC) is direct-write + verifier-only. No pyramid queries. No prompt taint (no prior-stage summaries, no verdict nudges).

3. **Pre-flight Q&A round** before first commit. Implementation thread asks clarifying questions on ambiguous plan items; planning thread (this doc's authors) answers in-thread. Eliminates mid-wave stalls.

4. **Wire-dev coordination** via Adam relay, same as walker. Flag Wire-blocking questions in friction log; Adam carries them across.

5. **Verification gates:**
   - `cargo check` default target (not `--lib`) at each commit.
   - `cargo test --lib` specific modules per wave.
   - Dev-mode smoke after each wave (Adam runs, implementer queues checklist).
   - Serial verifier after every workflow agent.
   - Full-feature wanderer after Wave 2 + Wave 4.
   - Final-ship wanderer after Wave 5.

6. **Escalation triggers** — halt and escalate to Adam if:
   - Wire-dev Q6 (MarketSurfaceCache SSE) blocks Wave 1 (shouldn't; poll suffices).
   - `cargo check` breaks for reasons outside current wave scope.
   - Schema migration trips on unexpected pre-existing state.
   - Any cross-repo divergence between Wire spec/contracts and Wire's running code.

7. **Fast-forward merge to main** after Wave 5 wanderer clean. Single operator (Adam), no PR review.

---

## 16. Reference — walker cycle artifacts

For pattern-matching, the walker cycle produced:
- Plan: `docs/plans/walker-re-plan-wire-2.1.md` (rev 0.1 → 0.3)
- Handoff: `docs/plans/walker-re-plan-wire-2.1-HANDOFF.md`
- Impl log: `docs/plans/walker-re-plan-wire-2.1-IMPL-LOG.md`
- Friction log: `docs/plans/walker-re-plan-wire-2.1-FRICTION-LOG.md`
- Shipped at: `agent-wire-node@4b85102`; post-ship fixes `fc4a55e` + `a8e413d`.

This plan follows the same shape. Reuse templates verbatim for impl/friction log headers.

---

## 17. What picking this up cold looks like

If a fresh agent lands on this plan with zero prior context, here's the path:

1. **Read this doc in full.** You're looking at it; keep reading.
2. **Read the walker plan** (`walker-re-plan-wire-2.1.md`) at least §1-§5 + §13 audit history. This plan builds on walker's data-layer primitives + observability vocabulary.
3. **Read the walker impl/friction logs** for ~30 min. Gets you a tactile feel for the repo's idioms + what bites during implementation.
4. **Read `project_provider_model_coupling_bug.md`** (node-side memory). Context for Wave 4's refactor scope.
5. **Read `docs/SYSTEM.md` §6 + §10.** Contributions model + provider registry.
6. **Grep the code surface map in §14** — spend 20 min skimming the existing files. `pyramid_tier_routing` (db.rs), `dispatch_policy.rs`, `provider.rs`, the existing (v1) InferenceRoutingPanel.
7. **Run the existing Mac build + open Settings → Inference Routing.** See what the current panel looks like. That's what's being replaced.
8. **Pre-flight Q&A** — open a chat with Adam, ask clarifications on §11 open questions, read his answers.
9. **Stage 1 audit.** Hand this doc to two informed auditors per the walker's conductor-audit-pass pattern.
10. **Stage 2 audit.** Purpose statement only + short known-issues list.
11. **Rev 0.2 / 0.3** absorbing audit findings + Q&A answers.
12. **Begin Wave 0.** Create impl + friction logs as first commits.

Estimated time from cold-start to Wave 0 task 1 code: 2-3 hours (reads + audits + Q&A).

---

**End of plan v0.1.**

Written 2026-04-21 by the planning thread immediately after walker ship + W1/C1 post-ship. Ready for audit.
