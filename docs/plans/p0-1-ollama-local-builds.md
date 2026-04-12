# P0-1: Fix Ollama Local Builds — Systemic Provider Resolution

**Status:** Ready to implement
**Author:** Claude + Adam
**Date:** 2026-04-12
**Audit cycles:** 4 rounds (8 auditors), rev 1→4→5→6 (systemic)

---

## Problem

When Ollama local mode is enabled, ALL LLM calls still route through OpenRouter. The `pyramid_tier_routing` table is correctly configured by `local_mode.rs` (all tiers → ollama-local + qwen3:30b-a3b), but nothing in the call path reads it.

## Root Cause — Two Systemic Issues

### Issue 1: Model and provider are resolved separately

`resolve_model()` picks a model string. `build_call_provider()` picks a provider. They never coordinate. The tier routing table links them — tier → (provider_id, model_id, context_limit) — but the two functions each have their own hardcoded logic that ignores the table.

### Issue 2: Model strings are injected at construction time, not resolved at call time

The stale engine sets its model once at construction from `config.primary_model`. When local mode is toggled, the pre-set model string doesn't update.

## Design — Five Systemic Changes + Supporting

### Change 1: `build_call_provider()` uses the active provider

**File:** `src-tauri/src/pyramid/llm.rs` — `build_call_provider()` (~line 338)
**File:** `src-tauri/src/pyramid/provider.rs` — add `active_provider_id()`

**Current:** Always `registry.get_provider("openrouter")`.

**Fix:** `active_provider_id()` checks for an enabled non-openrouter provider:

```rust
pub fn active_provider_id(&self) -> String {
    let providers = self.providers.read().expect("providers RwLock poisoned");
    for (id, provider) in providers.iter() {
        if id != "openrouter" && provider.enabled {
            return id.clone();
        }
    }
    "openrouter".to_string()
}
```

**CRITICAL: `commit_disable_local_mode` must set `provider.enabled = false`** on the ollama-local row. Currently it does not — it only restores tier routing and flips `pyramid_local_mode_state.enabled`. Without this, `active_provider_id()` returns "ollama-local" even after the user toggles local mode off.

**File:** `src-tauri/src/pyramid/local_mode.rs` — `commit_disable_local_mode()` (~line 713)

Add before `registry.load_from_db(conn)`:
```rust
// Disable the local provider so active_provider_id() falls back to openrouter
if let Some(mut local_provider) = db::get_provider(conn, OLLAMA_LOCAL_PROVIDER_ID)? {
    local_provider.enabled = false;
    db::save_provider(conn, &local_provider)?;
}
```

Verify: `commit_enable_local_mode()` already sets `enabled: true` on the provider row (confirmed at local_mode.rs ~468).

### Change 2: `resolve_model()` and `resolve_ir_model()` check registry first

**File:** `src-tauri/src/pyramid/chain_dispatch.rs` — `resolve_model()` (~line 186), `resolve_ir_model()` (~line 1023)

Add `registry.resolve_tier()` check before the hardcoded fallback. Direct `step.model` overrides still take highest precedence.

```rust
let tier = step.model_tier.as_deref().unwrap_or(defaults.model_tier.as_str());

// Phase 3: consult provider registry (canonical source)
if let Some(ref registry) = config.provider_registry {
    if let Ok(resolved) = registry.resolve_tier(tier, None, None, None) {
        return resolved.tier.model_id;
    }
    tracing::warn!("[CHAIN] tier '{}' not in registry, falling back to legacy", tier);
}
// ... legacy fallback unchanged
```

Same pattern for `resolve_ir_model()` using `reqs.tier.as_deref().unwrap_or("mid")`.

### Change 3: `resolve_context_limit()` and `resolve_ir_context_limit()` check registry first

**File:** `src-tauri/src/pyramid/chain_dispatch.rs` — `resolve_context_limit()` (~line 1085), `resolve_ir_context_limit()` (~line 1056)

Same pattern as Change 2 — check `registry.resolve_tier()` for the context_limit before the hardcoded fallback:

```rust
let tier = step.model_tier.as_deref().unwrap_or(defaults.model_tier.as_str());

if let Some(ref registry) = config.provider_registry {
    if let Ok(resolved) = registry.resolve_tier(tier, None, None, None) {
        if let Some(limit) = resolved.tier.context_limit {
            return limit;
        }
    }
}
// ... legacy fallback unchanged
```

Same for `resolve_ir_context_limit()`.

### Change 4: `dispatch_llm` AND `dispatch_ir_llm` override ALL model slots

**File:** `src-tauri/src/pyramid/chain_dispatch.rs` — `dispatch_llm()` (~line 250), `dispatch_ir_llm()` (~line 1218)

**Current:** Only overrides `primary_model` and `primary_context_limit`. On cascade, `fallback_model_1`/`fallback_model_2` are still OpenRouter models.

**Fix:** Use `clone_with_model_override()` instead of manual struct construction. This function already overrides all three model slots (primary + fallback_1 + fallback_2):

```rust
// Instead of:
// let mut overridden = ctx.config.clone();
// overridden.primary_model = resolved_model.clone();
// overridden.primary_context_limit = resolved_limit;

// Use:
let mut overridden = ctx.config.clone_with_model_override(&resolved_model);
overridden.primary_context_limit = resolved_limit;
```

Apply to BOTH `dispatch_llm` AND `dispatch_ir_llm`.

In local mode (all tiers → same model), cascade is neutralized — all slots point to the same model. In cloud mode, cascade retries the same tier's model rather than escaping to a different provider's model.

### Change 5: Stale engine resolves model from registry at dispatch time

**File:** `src-tauri/src/pyramid/stale_engine.rs` — `drain_and_dispatch()` (~line 700)

**Current:** `let model_owned = model.to_string();` where `model` comes from the engine's static field set at construction.

**Fix:** Resolve from registry at dispatch time. Use `"stale_remote"` tier always — it exists in both modes (seeded for OpenRouter in defaults, re-pointed to Ollama when local mode is on):

```rust
let model_owned = if let Some(ref registry) = base_config.provider_registry {
    registry.resolve_tier("stale_remote", Some(slug), None, None)
        .map(|r| r.tier.model_id)
        .unwrap_or_else(|_| model.to_string())
} else {
    model.to_string()
};
```

This is the single convergence point — all three callers (`start_timer`, `start_poll_loop`, `run_layer_now`) flow through `drain_and_dispatch`.

---

## Supporting Changes

### S1: `local_mode.rs:505-511` — Add `mid` and `extractor` to required tier list

```rust
for required in [
    "fast_extract", "web", "synth_heavy", "stale_remote", "stale_local",
    "mid", "extractor",
] { ... }
```

### S2: `local_mode.rs:524` — Set `supported_parameters_json` on local tier entries

```rust
supported_parameters_json: Some(r#"["response_format"]"#.to_string()),
```

### S3: `db.rs` — `ensure_standard_tiers_exist()` with INSERT OR IGNORE

Called from `init_pyramid_db` after `seed_default_provider_registry` (~line 1641). Covers all 6 standard tiers with their OpenRouter defaults:

| Tier | Model | Context |
|------|-------|---------|
| fast_extract | inception/mercury-2 | 120,000 |
| web | x-ai/grok-4.1-fast | 2,000,000 |
| synth_heavy | minimax/minimax-m2.7 | 200,000 |
| stale_remote | minimax/minimax-m2.7 | 200,000 |
| mid | inception/mercury-2 | 120,000 |
| extractor | inception/mercury-2 | 120,000 |

Uses `INSERT OR IGNORE` — never overwrites existing (including Ollama-routed) rows.

Also add `mid` and `extractor` to `seed_default_provider_registry` for fresh installs.

### S4: `chain_engine.rs:379` — Update VALID_MODEL_TIERS

```
["low", "mid", "high", "max", "extractor", "synth_heavy", "web", "fast_extract", "stale_remote", "stale_local"]
```

### S5: Question + default chain YAMLs — Remove ALL hardcoded model strings

**Question YAMLs — remove `defaults.model` entirely:**
- `chains/questions/code.yaml`: remove `model: inception/mercury-2` from defaults
- `chains/questions/conversation.yaml`: same
- `chains/questions/document.yaml`: same

Removing `defaults.model` (rather than replacing with `model_tier`) avoids needing a struct change to `QuestionDefaults`. The compiler falls through to `resolve_ir_model` with `model = None`, which hits the tier routing via Change 2.

**Question + default chain YAMLs — convert ALL per-step `model:` and `cluster_model:` to `model_tier:`:**
- `chains/questions/conversation.yaml:34` — `model: qwen/...` → `model_tier: web`
- `chains/questions/conversation.yaml:62` — `model: qwen/...` → `model_tier: web`
- `chains/questions/document.yaml:40` — `model: qwen/...` → `model_tier: web`
- `chains/questions/document.yaml:54` — `model: qwen/...` → `model_tier: web`
- `chains/questions/document.yaml:83` — `cluster_model: qwen/...` → check compiler, convert or remove
- `chains/questions/conversation-chronological.yaml:91,121` — same pattern

Grep `chains/` for any remaining `model:` fields with literal model slugs (not `model_tier:`) and convert all.

---

## What this fixes

ALL LLM callers, because the five changes are in the universal call path:

| Caller | Model fix | Provider fix |
|--------|-----------|--------------|
| dispatch_llm (chain executor) | Change 2 | Change 1 |
| dispatch_ir_llm (IR executor) | Change 2 | Change 1 |
| stale_helpers (DADBEAR L0) | Change 5 | Change 1 |
| stale_helpers_upper (DADBEAR L1+) | Change 5 | Change 1 |
| evidence_answering | config model | Change 1 |
| faq, delta, webbing, meta, reroll | config model | Change 1 |
| Cascade on context-exceeded | Change 4 | Change 1 |

## Known Limitations (Phase 2)

- **Global rate limiter** (20 req/5s) throttles Ollama — per-provider limiter
- **No build-active guard on toggle** — snapshot registry or cancel build
- **Retry policy**: 500 not in retryable_status_codes
- **Pre-existing local mode users** need re-toggle for mid/extractor tiers
- **call_model_via_registry** stays dead code — systemic fix makes it unnecessary
- **Audit trail** works because the legacy call path is preserved (no regression)
- **min_timeout_secs** works because the legacy call path is preserved (no regression)

## Audit findings addressed

| Finding | Severity | Resolution |
|---------|----------|------------|
| active_provider_id breaks on disable | CRITICAL | commit_disable must set provider.enabled=false |
| Per-step model: overrides not enumerated | MAJOR | S5 enumerates all, converts to model_tier |
| Change 5 pseudocode is_local_check wrong | MAJOR | Use stale_remote always (exists in both modes) |
| dispatch_ir_llm needs Change 4 too | MAJOR | Explicit: apply to both dispatch functions |
| QuestionDefaults has no model_tier field | MAJOR | Remove defaults.model instead of adding field |
| resolve_context_limit not fixed | MAJOR | Change 3 added |
| Cascade neutralization in cloud mode | MINOR | Acceptable: retry same tier's model, not cross-provider |
| cluster_model: in question YAMLs | MINOR | Included in S5 sweep |
| HashMap iteration non-deterministic | MINOR | Acceptable: only 2 providers (openrouter + ollama) |
| stale engine injection point | MINOR | Corrected: drain_and_dispatch is the convergence point |

## Verification

1. `cargo check` — must compile clean
2. Restart app in dev mode
3. `SELECT * FROM pyramid_tier_routing` — should have mid, extractor, all Ollama-routed
4. Create test slug + ingest + build with Ollama enabled
5. Confirm calls hit localhost:11434
6. Confirm nodes created with valid content
7. Confirm cost log has Ollama model name
8. Disable Ollama — confirm `active_provider_id()` returns "openrouter"
9. Rebuild — confirm reverts to OpenRouter
10. Trigger DADBEAR stale check — confirm it uses Ollama

## Files

| File | Changes |
|------|---------|
| `src-tauri/src/pyramid/llm.rs` | Change 1: build_call_provider |
| `src-tauri/src/pyramid/provider.rs` | Change 1: active_provider_id() |
| `src-tauri/src/pyramid/chain_dispatch.rs` | Changes 2, 3, 4 |
| `src-tauri/src/pyramid/stale_engine.rs` | Change 5: drain_and_dispatch |
| `src-tauri/src/pyramid/local_mode.rs` | Change 1 (disable), S1, S2 |
| `src-tauri/src/pyramid/db.rs` | S3: ensure_standard_tiers_exist |
| `src-tauri/src/pyramid/chain_engine.rs` | S4: VALID_MODEL_TIERS |
| `chains/questions/*.yaml` | S5: remove hardcoded models |
| `chains/defaults/*.yaml` | S5: check for per-step model overrides |
