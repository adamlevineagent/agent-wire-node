# LLM Output Cache Specification

**Version:** 1.0
**Date:** 2026-04-09
**Status:** Design — pre-implementation
**Depends on:** Provider registry (for model_id in cache key)
**Unblocks:** Crash recovery, stale propagation cache hits, full build viz, reroll-with-notes
**Authors:** Adam Levine, Claude (session design partner)

---

## Overview

Every LLM output is intelligence. The cost to store is near-zero, the cost to regenerate is real (time + money). The cache turns `pyramid_llm_audit` from a write-only log into a content-addressable cache.

Every step defined in a chain YAML creates a **named storage slot** in the database. The chain YAML is not just an execution plan — it's a storage schema. Each step declares a persistent position. When the step runs, its output is persisted to that slot. The slot stays current until its inputs change.

---

## Current State

| Component | Status | Location |
|-----------|--------|----------|
| `pyramid_pipeline_steps` | Partially stores step outputs (keyed by slug/step_type/chunk_index/depth/node_id) | db.rs |
| `pyramid_llm_audit` | Captures every LLM call (prompts, responses, tokens, latency) — write-only | db.rs |
| `ChainContext.step_outputs` | In-flight HashMap — ephemeral, lost on crash | chain_executor.rs |
| `send_save_step()` | Persists individual step outputs to `pyramid_pipeline_steps` | chain_executor.rs |
| `load_prior_step_output()` | Loads step output from DB (used for resume/hydration) | chain_executor.rs |
| `hydrate_skipped_step_output()` | Fills step_outputs from DB when a step is skipped via `when` condition | chain_executor.rs |

The infrastructure for per-step persistence largely exists. What's missing:
1. **Content-addressable cache lookup** before LLM calls
2. **Universal slot coverage** — some steps (webbing, intermediate computations) don't persist
3. **Cache-hit skip path** in `call_model_unified()`

---

## Cache Key

Every LLM call is keyed by a content-addressable triple:

```
cache_key = hash(inputs_content_hash, prompt_hash, model_id)
```

| Component | What It Captures | How to Compute |
|-----------|-----------------|----------------|
| `inputs_content_hash` | The actual input data (system prompt content + user prompt content after variable resolution) | SHA-256 of concatenated resolved prompts |
| `prompt_hash` | The prompt template (instruction file) — distinguishes same input with different prompts | SHA-256 of the resolved instruction file content (see Prompt Hash Computation below) |
| `model_id` | The resolved model name — same inputs with different models produce different outputs | String from tier routing resolution |

### Why Not Just Hash the Full Prompt?

The full prompt includes the input data. `inputs_content_hash` already captures that. Separating prompt template from input data means:
- If only the input changes (file edit), cache miss (correct)
- If only the prompt changes (prompt improvement), cache miss (correct)
- If only the model changes (routing update), cache miss (correct)
- If nothing changes, cache hit (correct)

---

## Prompt Hash Computation

`prompt_hash` is SHA-256 of the resolved instruction file content. Computed once at build start for each unique instruction path, cached in `ChainContext.prompt_hashes: HashMap<String, String>`. This means:
- Editing a prompt file and rebuilding produces cache misses (correct).
- Editing a prompt file mid-build has no effect until next build (correct -- build-scoped).
- Two steps using the same instruction file share the same prompt_hash (correct -- cache reuse where inputs also match).

---

## Storage Slots

### pyramid_step_cache Table

```sql
CREATE TABLE IF NOT EXISTS pyramid_step_cache (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    slug TEXT NOT NULL,
    build_id TEXT NOT NULL,
    step_name TEXT NOT NULL,
    chunk_index INTEGER DEFAULT -1,
    depth INTEGER DEFAULT 0,
    cache_key TEXT NOT NULL,              -- SHA-256 of (inputs_hash, prompt_hash, model_id)
    inputs_hash TEXT NOT NULL,            -- SHA-256 of resolved input content
    prompt_hash TEXT NOT NULL,            -- SHA-256 of instruction template
    model_id TEXT NOT NULL,              -- resolved model name
    output_json TEXT NOT NULL,           -- the LLM output (full response)
    token_usage_json TEXT,              -- {prompt_tokens, completion_tokens}
    cost_usd REAL,                      -- estimated cost
    latency_ms INTEGER,                 -- wall-clock time
    created_at TEXT DEFAULT (datetime('now')),
    force_fresh INTEGER DEFAULT 0,       -- 1 = this was a force-fresh (reroll) call
    supersedes_cache_id INTEGER,         -- if force_fresh, which prior cache entry this replaced
    UNIQUE(slug, cache_key)              -- content-addressable: same inputs = same entry
);
CREATE INDEX idx_step_cache_lookup ON pyramid_step_cache(slug, step_name, chunk_index, depth);
CREATE INDEX idx_step_cache_key ON pyramid_step_cache(cache_key);
```

### Integration with Existing Tables

- `pyramid_pipeline_steps` continues to store per-step outputs for the executor's resume path
- `pyramid_step_cache` adds the content-addressable layer on top
- `pyramid_llm_audit` continues as the raw audit trail (every call, including cache misses)
- Cache entries reference `build_id` for provenance but are looked up by `cache_key`

---

## Cache Lookup Flow

Before each LLM call in `call_model_unified()`:

```
1. Resolve the prompt template and inputs for this step
2. Compute cache_key = hash(inputs_hash, prompt_hash, model_id)
3. Look up: SELECT * FROM pyramid_step_cache WHERE slug = ? AND cache_key = ?
4. If found AND force_fresh is not set:
   → Verify the cached entry (see "Cache Hit Verification" below)
   → If verification passes: return cached output, log cache_hit
   → If verification fails: treat as cache miss, re-run LLM, overwrite entry
5. If not found OR force_fresh:
   → Make LLM call
   → Store result in pyramid_step_cache
   → Log to pyramid_llm_audit with source = "llm_call"
```

### Cache Hit Verification

SHA-256 collisions are vanishingly rare but not impossible. More practically, model ID normalization drift, stale cached entries from a prior model version, or partial writes could cause a cache hit to return incorrect content. Every cache hit is re-verified before use:

```rust
fn verify_cache_hit(
    cached: &CacheEntry,
    current_inputs_hash: &str,
    current_prompt_hash: &str,
    current_model_id: &str,
) -> CacheHitResult {
    // 1. Verify all three components match (not just the composite cache_key)
    if cached.inputs_hash != current_inputs_hash {
        return CacheHitResult::MismatchInputs;
    }
    if cached.prompt_hash != current_prompt_hash {
        return CacheHitResult::MismatchPrompt;
    }
    if cached.model_id != current_model_id {
        return CacheHitResult::MismatchModel;
    }

    // 2. Verify the output_json parses (corruption detection)
    if serde_json::from_str::<Value>(&cached.output_json).is_err() {
        return CacheHitResult::CorruptedOutput;
    }

    CacheHitResult::Valid
}
```

If `verify_cache_hit` returns anything other than `Valid`:
- Log a WARN-level entry with the mismatch kind
- Delete the stale/corrupt cache entry
- Fall through to the LLM call path (re-generates and stores a fresh entry)
- Emit a `CacheHitVerificationFailed` event for the oversight page

The verification adds a few extra comparisons per lookup but guarantees data correctness. A composite cache_key hash collision would be caught at verification time.

### Model ID Normalization

Model IDs must be resolved to a canonical form **once per build** and cached to prevent drift:

```rust
// In ChainContext, alongside prompt_hashes:
pub struct ChainContext {
    pub prompt_hashes: HashMap<String, String>,     // path → SHA-256
    pub resolved_models: HashMap<String, String>,   // tier_name → canonical model_id
    // ...
}

fn resolve_model_for_tier(ctx: &mut ChainContext, tier_name: &str) -> Result<String> {
    if let Some(cached) = ctx.resolved_models.get(tier_name) {
        return Ok(cached.clone());
    }
    // First resolution for this tier in this build — resolve via provider registry
    let model_id = provider_resolver::resolve_tier(tier_name)?;
    ctx.resolved_models.insert(tier_name.to_string(), model_id.clone());
    Ok(model_id)
}
```

**Guarantees**:
- All cache writes in a single build use consistent model_ids (no drift mid-build)
- Changing the tier routing between builds causes cache misses on the next build (correct — new model means new output)
- The resolved model_id is what goes into `cache_key` and `pyramid_step_cache.model_id`, NOT the tier name
- Alias resolution (e.g., `"m2.7"` → `"inception/mercury-2.7-preview-03"`) happens exactly once per build per tier

### Where to Hook

The cache check happens in `call_model_unified()` (llm.rs), BEFORE the HTTP request. The cache is transparent to callers — they call the same function and get the same `LlmResponse`.

```rust
// In call_model_unified(), before the retry loop:
if let Some(cached) = check_cache(db_path, slug, &cache_key).await? {
    log_cache_hit(db_path, slug, step_name, &cache_key, &cached);
    return Ok(cached);
}
// ... existing retry loop ...
// After successful LLM call:
store_cache(db_path, slug, step_name, chunk_index, depth, &cache_key, &response, build_id).await?;
```

### Threading the Cache Context

`call_model_unified()` currently receives `LlmConfig` + prompts. It needs additional context for caching:
- `db_path` — to access the cache table
- `slug` — cache is per-pyramid
- `step_name`, `chunk_index`, `depth` — for slot identification
- `build_id` — for provenance
- `force_fresh` — to bypass cache

Rather than a cache-specific struct, a unified `StepContext` bundles cache, event bus, and step metadata into a single context threaded through all LLM-calling code paths:

```rust
/// Execution context threaded through chain step handlers.
/// Replaces the previous CacheContext — combines cache, event bus, and step metadata.
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
    pub resolved_model_id: Option<String>,    // populated after tier resolution
    pub resolved_provider_id: Option<String>,
}
```

StepContext is the single context object threaded through all LLM-calling code paths. It combines the responsibilities of cache lookup/storage, event bus emission, and step metadata tracking. Created at step dispatch time in chain_executor.rs, passed down to call_model_unified() and any sub-helpers.

---

## Force-Fresh (Reroll with Notes)

When a user rerolls a node:

1. UI presents a notes field (strongly encouraged, not required)
2. Note + existing output sent to LLM: "The user wants a different version. Their feedback: {note}. The current output: {existing}. Address their concern."
3. `force_fresh: true` bypasses cache check
4. New result stored in `pyramid_step_cache` with `supersedes_cache_id` pointing to the prior entry
5. Old cache entry remains (for version history)
6. The note is attached to the supersession record in `pyramid_change_manifests`

This is the notes paradigm applied to any cached output.

---

## Cache Invalidation

The cache is naturally invalidated by content change:
- File edit → inputs_hash changes → cache miss (correct)
- Prompt improvement → prompt_hash changes → cache miss (correct)
- Model change → model_id changes → cache miss (correct)

No explicit invalidation needed. The `UNIQUE(slug, cache_key)` constraint means new outputs for the same cache key overwrite (via INSERT OR REPLACE).

Imported cache entries (from `cache-warming-and-import.md`) are not special — they go through the same content-addressable check as any other entry. If a source file changes after import, the hash changes, the cache key becomes a miss, and the entry is ignored. Imported entries are just cache rows that happened to come from elsewhere; they live in the same `pyramid_step_cache` table, obey the same `UNIQUE(slug, cache_key)` constraint, and are looked up the same way in `call_model_unified()`.

### When Cache Does NOT Help

- **Primary stale checks are NOT cache hits** — DADBEAR triggers on file hash changes, meaning the inputs changed, so the cache key is different
- **Parent propagation MAY be cache hits** — if a parent's synthesis inputs haven't materially changed despite a child update
- **Crash recovery IS a cache hit** — completed steps have valid cached outputs
- **Imports with changed source files are NOT cache hits** — the source file content changed, so `inputs_content_hash` differs, so the cache key is different. The imported entry is dead weight and is ignored.

### When Cache DOES Help

- **Re-running the same build with no changes**: every step is a cache hit
- **Crash recovery mid-build**: completed steps resume as cache hits without re-running
- **Imports with matching sources**: imported entries whose referenced source files still match locally are valid cache hits on the first build after import (see `cache-warming-and-import.md`)
- **Parent re-synthesis where inputs were stable**: upper-layer steps whose aggregated inputs haven't materially changed

---

## Build Viz Integration

Every step with a cached output has a persisted record. The build viz can show:
- **Cache hit** — step completed instantly, show as green with "cached" badge
- **LLM call** — step running/completed with timing info
- **Pending** — step not yet reached

This requires the viz to query `pyramid_step_cache` alongside `pyramid_pipeline_steps` for step status.

---

## Migration

1. Create `pyramid_step_cache` table
2. Add `StepContext` parameter to `call_model_unified()` (see Threading the Cache Context above)
3. Add cache check before HTTP call, cache store after successful call
4. Surface `force_fresh` via `StepContext.force_fresh` for reroll path
5. Backfill: existing `pyramid_llm_audit` entries can optionally be migrated to populate the cache for recent builds (not required — cache warms naturally)

---

## Cache Warming on Pyramid Import

When a user imports a pyramid from Wire (via ToolsMode Discover → Pull), the source node's cache manifest is populated into the local `pyramid_step_cache` table. Entries whose referenced source files still match locally (by SHA-256 hash) are inserted as-is and become valid cache hits on the first build. Entries whose source files have diverged are skipped; the corresponding nodes and their dependents are marked for fresh rebuild.

This is mechanically "just a bulk INSERT into `pyramid_step_cache`" — the content-addressable cache key means imported entries are indistinguishable from locally produced entries once inserted. The lookup path in `call_model_unified()` works identically for both.

See `cache-warming-and-import.md` for the authoritative spec on the import flow, cache manifest format, staleness propagation via the evidence graph, privacy constraints on publication, and the `pyramid_import_pyramid` IPC contract.

---

## Open Questions

1. **Cache scope**: Per-pyramid (`slug`) or global? Per-pyramid is safer (different pyramids may have different prompt versions even for the same content). Recommend per-pyramid.

2. **Cache size limits**: Should old cache entries be pruned? The data is small (text). Recommend: no pruning in v1, add TTL-based cleanup if storage becomes an issue.
