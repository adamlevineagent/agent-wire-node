# Workstream: Phase 6 — LLM Output Cache + StepContext

## Who you are

You are an implementer joining an active 17-phase initiative. Phases 0a, 0b, 1, 2, 3, 4, and 5 are shipped. You are the implementer of Phase 6, which turns `pyramid_llm_audit` from a write-only log into a content-addressable cache, and introduces the unified `StepContext` struct that Phases 2, 3, and 5 all deferred to "when Phase 6 lands."

Phase 6 is substantial but bounded. It touches the hot path (every LLM call), so correctness matters more than speed.

## Context

Phase 2 noted: "`generate_change_manifest` MUST receive a `StepContext` (defined canonically in `llm-output-cache.md`)" — but Phase 2 shipped without StepContext because Phase 6 hadn't landed. Similarly Phase 3's LLM refactor threaded providers but left temperature/max_tokens hardcoded and deferred caching. Phase 6 is where the deferred pieces finally get wired up.

The cache has three parts:
1. **Content-addressable key:** `cache_key = hash(inputs_content_hash, prompt_hash, model_id)`. Same inputs, same prompt template, same model → cache hit.
2. **Storage slot:** `pyramid_step_cache` table with `UNIQUE(slug, cache_key)` constraint.
3. **Hook point:** `call_model_unified()` checks the cache BEFORE the HTTP request, stores after the response.

The cache is transparent to callers — they call the same function and get the same `LlmResponse`. They just sometimes don't actually hit the wire.

## Required reading (in order, in full unless noted)

1. `docs/handoffs/handoff-2026-04-09-pyramid-folders-model-routing.md` — deviation protocol.
2. **`docs/specs/llm-output-cache.md` — read in full (328 lines).** Particular attention to: Cache Key (~line 38), Prompt Hash Computation (~line 62), Storage Slots (~line 71), Cache Lookup Flow (~line 109), Cache Hit Verification (~line 128), Model ID Normalization (~line 167), Where to Hook (~line 195), Threading the Cache Context (~line 210), Force-Fresh (~line 251), Cache Invalidation (~line 266), Migration (~line 304).
3. `docs/plans/pyramid-folders-model-routing-full-pipeline-observability.md` — Phase 6 section.
4. `docs/plans/pyramid-folders-model-routing-implementation-log.md` — Phase 2, 3, 5 entries. Phase 2's `generate_change_manifest` is a candidate caller for StepContext retrofit. Phase 3's `call_model_via_registry` is the primary hook site. Phase 5's prompt_cache + canonical metadata are orthogonal but don't conflict.
5. `docs/plans/pyramid-folders-model-routing-friction-log.md` — scan for Phase 2's note about hardcoded `0.2/4096` temperature/max_tokens and Phase 3's note about Pillar 37 deferral — Phase 6 does NOT fix those (that's still Phase 9's config contributions scope), but be aware they exist.

### Code reading (in full unless noted)

6. **`src-tauri/src/pyramid/llm.rs` — read in full.** This is where the cache hook lives. Pay attention to `call_model_unified`, `call_model_via_registry` (Phase 3's registry-aware entry), `build_call_provider` (Phase 3's transitional fallback), `LlmConfig` (now carrying provider_registry + credential_store), and the LLM response path.
7. **`src-tauri/src/pyramid/chain_executor.rs` — targeted read**. It's ~15000 lines. Grep for `ChainContext` struct definition (the existing state holder), `step_outputs`, `load_prior_step_output`, `hydrate_skipped_step_output`, `send_save_step`. Understand where step outputs are currently persisted and hydrated. Your StepContext lives alongside ChainContext — they cooperate, not replace.
8. `src-tauri/src/pyramid/db.rs` — targeted read. Find `pyramid_pipeline_steps` + `pyramid_llm_audit` table definitions for the conventions, find `init_pyramid_db` (where you'll add the new table).
9. `src-tauri/src/pyramid/stale_helpers_upper.rs` — find `generate_change_manifest` (Phase 2 added it, it does `call_model_with_usage` without a StepContext). This is a retrofit target — after Phase 6, it should build a StepContext and pass it through.
10. `src-tauri/src/pyramid/provider.rs` — Phase 3's `ProviderRegistry::resolve_tier` function. This is what `StepContext.resolved_model_id` gets populated from. The spec's "Model ID Normalization" section wants tier → canonical model_id resolution cached in `ChainContext.resolved_models`.

## What to build

### 1. `pyramid_step_cache` table (in `db.rs`)

Add to `init_pyramid_db` exactly per the spec:

```sql
CREATE TABLE IF NOT EXISTS pyramid_step_cache (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    slug TEXT NOT NULL,
    build_id TEXT NOT NULL,
    step_name TEXT NOT NULL,
    chunk_index INTEGER DEFAULT -1,
    depth INTEGER DEFAULT 0,
    cache_key TEXT NOT NULL,
    inputs_hash TEXT NOT NULL,
    prompt_hash TEXT NOT NULL,
    model_id TEXT NOT NULL,
    output_json TEXT NOT NULL,
    token_usage_json TEXT,
    cost_usd REAL,
    latency_ms INTEGER,
    created_at TEXT DEFAULT (datetime('now')),
    force_fresh INTEGER DEFAULT 0,
    supersedes_cache_id INTEGER,
    UNIQUE(slug, cache_key)
);
CREATE INDEX IF NOT EXISTS idx_step_cache_lookup ON pyramid_step_cache(slug, step_name, chunk_index, depth);
CREATE INDEX IF NOT EXISTS idx_step_cache_key ON pyramid_step_cache(cache_key);
```

### 2. CRUD helpers (in `db.rs` or a new `step_cache.rs` module)

```rust
pub fn check_cache(
    conn: &Connection,
    slug: &str,
    cache_key: &str,
) -> Result<Option<CachedStepOutput>>

pub fn store_cache(
    conn: &Connection,
    entry: &CacheEntry,
) -> Result<()>  // INSERT OR REPLACE on (slug, cache_key)

pub fn delete_cache_entry(
    conn: &Connection,
    slug: &str,
    cache_key: &str,
) -> Result<()>  // used by verification-failure path

pub fn supersede_cache_entry(
    conn: &Connection,
    slug: &str,
    prior_cache_key: &str,
    new_entry: &CacheEntry,
) -> Result<()>  // force-fresh path: new entry with supersedes_cache_id
```

Define `CachedStepOutput` and `CacheEntry` in `types.rs` or alongside the module.

### 3. `StepContext` struct (new, in a new file `step_context.rs` or in `chain_executor.rs`)

Per the spec:

```rust
pub struct StepContext {
    // Build metadata
    pub slug: String,
    pub build_id: String,
    pub step_name: String,
    pub primitive: String,
    pub depth: i64,
    pub chunk_index: Option<i64>,
    
    // Cache
    pub db_path: String,
    pub force_fresh: bool,
    
    // Event emission
    pub bus: Arc<BuildEventBus>,
    
    // Model resolution (from provider registry)
    pub model_tier: String,
    pub resolved_model_id: Option<String>,
    pub resolved_provider_id: Option<String>,
}
```

Plus a constructor helper:
```rust
impl StepContext {
    pub fn from_chain(ctx: &ChainContext, step_name: &str, primitive: &str) -> Self
}
```

### 4. Extend `ChainContext` with caches

Per the spec's "Model ID Normalization" section:

```rust
pub struct ChainContext {
    // existing fields ...
    pub prompt_hashes: HashMap<String, String>,     // path → SHA-256 of instruction file content
    pub resolved_models: HashMap<String, String>,   // tier_name → canonical model_id
}
```

Add these fields. Populate `prompt_hashes` lazily (first time a prompt is used in this build, hash it and cache). Populate `resolved_models` lazily via `resolve_model_for_tier(ctx, tier)`.

### 5. Cache key computation

```rust
fn compute_cache_key(inputs_hash: &str, prompt_hash: &str, model_id: &str) -> String {
    let composite = format!("{}|{}|{}", inputs_hash, prompt_hash, model_id);
    sha256_hex(composite.as_bytes())
}

fn compute_inputs_hash(system_prompt: &str, user_prompt: &str) -> String {
    // Hash the concatenated resolved prompts (after variable substitution).
    let combined = format!("{}\n---\n{}", system_prompt, user_prompt);
    sha256_hex(combined.as_bytes())
}
```

Use `sha2::Sha256` (already in the deps).

### 6. Cache lookup + verification in `call_model_unified`

Per the spec's "Cache Lookup Flow":

```rust
pub async fn call_model_unified_with_options(
    config: &LlmConfig,
    ctx: Option<&StepContext>,  // NEW — optional so tests without a cache still work
    system_prompt: &str,
    user_prompt: &str,
    model: &str,
    temperature: f32,
    max_tokens: u32,
    options: &LlmCallOptions,
) -> Result<LlmResponse>
```

Where a StepContext is provided (production path):
1. Compute `inputs_hash` from the resolved prompts
2. Look up or compute `prompt_hash` (Phase 6's `ctx.prompt_hashes` cache — but ChainContext lives above LLM call, so this hash needs to be computed by the caller and passed in via StepContext — add `prompt_hash: String` field to StepContext)
3. Compute `cache_key`
4. If `!ctx.force_fresh`: check cache via `db::check_cache(conn, &ctx.slug, &cache_key)`
   - If hit AND `verify_cache_hit(cached, inputs_hash, prompt_hash, model_id)` returns `Valid`: emit `CacheHit` event, return cached `LlmResponse`
   - If verification fails: log WARN, delete the stale entry, emit `CacheHitVerificationFailed`, fall through
5. Run the existing HTTP retry loop
6. On success: `db::store_cache(conn, &CacheEntry { ... })`
7. Emit `LlmCallCompleted` event (or similar)

Where NO StepContext is provided (test path, pre-init): skip cache logic entirely, go straight to the HTTP retry loop. This preserves backward compatibility.

### 7. `verify_cache_hit` function

Per the spec:

```rust
pub fn verify_cache_hit(
    cached: &CachedStepOutput,
    current_inputs_hash: &str,
    current_prompt_hash: &str,
    current_model_id: &str,
) -> CacheHitResult
```

Four cases: `Valid`, `MismatchInputs`, `MismatchPrompt`, `MismatchModel`, `CorruptedOutput`. Implement exactly per the spec.

### 8. Force-fresh path for reroll

When `ctx.force_fresh == true`:
- Skip the cache lookup entirely
- Run the HTTP request
- Store the new entry with `force_fresh: 1` and `supersedes_cache_id` pointing at the prior entry if one existed for the same `cache_key`

The reroll IPC command itself is Phase 13's scope. Phase 6 just provides the `force_fresh: bool` plumbing so Phase 13 can flip it.

### 9. Retrofit `generate_change_manifest` (Phase 2) with StepContext

Phase 2's `stale_helpers_upper::generate_change_manifest` currently calls `call_model_with_usage` without a StepContext. After Phase 6:

1. Change its signature to accept `ctx: &StepContext`
2. The caller (execute_supersession) must construct a StepContext with:
   - `step_name: "change_manifest"`
   - `primitive: "manifest_generation"`
   - `depth: current_node.depth`
   - `chunk_index: None`
3. Pass the StepContext through to `call_model_*`
4. The cache layer sees manifest generation as just another LLM call with its own cache key

This is the first retrofit validation of the StepContext pattern. Phase 12 (evidence triage) will need similar StepContext threading but is out of scope here.

### 10. Event bus integration

Add new `TaggedKind` variants (or extend existing ones) for:
- `CacheHit { slug, step_name, cache_key, chunk_index, depth }`
- `CacheMiss { slug, step_name, cache_key, chunk_index, depth }`  — optional
- `CacheHitVerificationFailed { slug, step_name, cache_key, reason }`

These feed Phase 13's build viz expansion. Phase 6 just emits them; no consumer today.

### 11. Tests

- `test_compute_cache_key_stable` — same inputs produce same key across runs
- `test_compute_cache_key_changes_on_input_change` — single field change → different key (test for all three: inputs, prompt, model)
- `test_check_cache_hit_and_verify` — store a cache entry, look it up, verify fields
- `test_cache_hit_verification_rejects_input_mismatch` — exercise each MismatchX variant
- `test_cache_hit_verification_rejects_corrupted_output` — store a row with malformed `output_json`, verify it's rejected
- `test_force_fresh_bypasses_cache` — `force_fresh: true` runs LLM even with a valid cache entry (mock the LLM so the test doesn't fire real HTTP)
- `test_supersede_cache_entry_links_back` — force-fresh stores `supersedes_cache_id`
- `test_unique_constraint_on_slug_cache_key` — duplicate insert is an UPDATE (INSERT OR REPLACE)
- `test_step_context_creation` — construct StepContext from ChainContext, verify fields propagate
- `test_model_id_normalization_cached` — `resolve_model_for_tier` called twice returns same id without hitting the registry twice (caching)
- `test_generate_change_manifest_with_step_context_compiles` — ensure Phase 2's retrofit actually type-checks and calls through

Mocking the LLM: Phase 3's `ProviderRegistry` test fixtures have examples. Use the same pattern — construct a test `PyramidState` without a live HTTP client, call the caching path directly with a pre-populated cache row.

## Scope boundaries

**In scope:**
- `pyramid_step_cache` table + indices
- CRUD helpers (check_cache, store_cache, delete_cache_entry, supersede_cache_entry)
- `StepContext` struct + constructor
- `ChainContext.prompt_hashes` + `ChainContext.resolved_models`
- Cache key + inputs hash computation
- Cache lookup + `verify_cache_hit` in `call_model_unified_with_options`
- Force-fresh bypass path
- Retrofit of `generate_change_manifest` (Phase 2) to thread StepContext
- New `TaggedKind::CacheHit` / `CacheHitVerificationFailed` variants (emitted only, no consumer)
- Tests

**Out of scope:**
- Build viz cache-hit rendering — Phase 13
- `pyramid_reroll_config` / `pyramid_reroll_node` IPC commands — Phase 13
- Hardcoded temperature/max_tokens cleanup — still deferred to Phase 9 (config contributions for LLM config)
- Cache warming on import — Phase 7
- Retrofitting every other LLM call site (evidence triage, FAQ, delta, webbing, meta) — Phase 12 and later. Phase 6 only retrofits `generate_change_manifest` as the proof-of-concept.
- The existing 7 pre-existing unrelated test failures

## Verification criteria

1. `cargo check --lib`, `cargo build --lib` — clean, zero new warnings.
2. `cargo test --lib pyramid::step_cache` (or wherever) — all new tests passing.
3. `cargo test --lib pyramid::stale_helpers_upper` — Phase 2's existing tests still pass after the retrofit. Post-Phase-5 there are 7 tests in that module.
4. `cargo test --lib pyramid` — overall 923 passing (Phase 5's post-wanderer count) + your new Phase 6 tests. Same 7 pre-existing failures.
5. `grep -n "call_model_unified" src-tauri/src/pyramid/llm.rs` — the function accepts `Option<&StepContext>` (or similar) and the cache lookup lives in it.
6. `grep -n "StepContext" src-tauri/src/pyramid/stale_helpers_upper.rs` — `generate_change_manifest` receives a StepContext.
7. `grep -rn "pyramid_step_cache" src-tauri/src/` — the table creation, CRUD, and hook points are all wired.

## Deviation protocol

Standard. Most likely deviations:
- **Cache key stability across Rust versions** — `sha2::Sha256` is stable, but if you use `#[derive(Hash)]` anywhere, flag it (`std::hash::Hash` is NOT stable across Rust versions). Content-addressable storage MUST use SHA-256 or equivalent, never `std::hash`.
- **`call_model_unified` signature changes** — if adding `Option<&StepContext>` breaks the transitional fallback from Phase 3's `build_call_provider`, handle it by threading `None` through the fallback path.
- **ChainContext ownership** — `prompt_hashes` and `resolved_models` live on ChainContext which is often passed as `&mut ctx`. If your retrofit creates borrow-checker friction, you may need `RefCell` or move the caches to a sibling struct.

## Implementation log protocol

Append Phase 6 entry to `docs/plans/pyramid-folders-model-routing-implementation-log.md`. Document the schema, CRUD, StepContext, ChainContext extensions, cache hook point in call_model_unified, force-fresh plumbing, Phase 2 retrofit, event variants, tests, and verification results. Status: `awaiting-verification`.

## Mandate

- **Correct before fast.** The cache is on the hot path. `verify_cache_hit` is load-bearing — incorrect cache hits are silent correctness bugs.
- **Backward compatibility.** `call_model_unified` must still work without a StepContext (for tests and edge call sites). The cache is opt-in via StepContext presence.
- **No new scope.** Phase 6 is the cache primitive + the one StepContext retrofit proof-of-concept. Other retrofits are later phases.
- **Pillar 37 awareness.** The cache is not config-constrained — it's pure primitive. No new hardcoded LLM-constraining numbers.
- **Commit when done.** Single commit with message `phase-6: llm output cache + StepContext`. Body: 5-7 lines summarizing table + CRUD + hook + StepContext + retrofit + tests. Do not amend. Do not push.

## End state

Phase 6 is complete when:

1. `pyramid_step_cache` table exists with content-addressable key + verification fields.
2. CRUD helpers exist and are tested.
3. `StepContext` struct + `ChainContext.prompt_hashes` / `resolved_models` exist.
4. `call_model_unified_with_options` checks the cache before HTTP when a StepContext is provided, stores after success, and skips the cache when no StepContext is given.
5. `verify_cache_hit` correctly detects all four mismatch variants + corruption.
6. `generate_change_manifest` (Phase 2) now threads a StepContext through its LLM call.
7. `TaggedKind::CacheHit` / `CacheHitVerificationFailed` events emitted from the cache path.
8. All tests pass, no regressions.
9. Implementation log Phase 6 entry complete.
10. Single commit on branch `phase-6-llm-output-cache`.

Begin with the spec. Then the existing call_model path. Then write.

Good luck. Build carefully.
