# Handoff: Provider-Model Coupling Bug — Systemic Fix

**Date:** 2026-04-12
**From:** Session that verified the handoff, traced the full affected surface, identified the root cause, found a bonus bug, and ran a two-stage blind audit.
**To:** Implementation session.
**Audience:** Claude. Adam has reviewed the approach and confirmed the maximal fix direction.
**Audit status:** Full cycle complete (Stage 1: two informed, Stage 2: two discovery). Stage 1 found 2 critical + 4 major. Stage 2 found 1 critical + 3 major that Stage 1 missed. All corrected in this version.

---

## TL;DR

`LlmConfig.primary_model / fallback_model_1 / fallback_model_2` are always OpenRouter slugs (`inception/mercury-2`, `qwen/qwen3.5-flash-02-23`, `x-ai/grok-4.20-beta`) regardless of which provider is active. When Ollama is the active provider, every non-chain LLM call sends the correct HTTP endpoint (localhost:11434) but the wrong model name (an OpenRouter slug Ollama doesn't understand).

The chain executor path (`call_model_via_registry`) is unaffected — it resolves models from `pyramid_tier_routing`. The bug is in the **cascade path** used by 14 modules and 25+ call sites that go through `call_model_unified_with_audit_and_ctx`.

The maximal fix: resolve model fields from `pyramid_tier_routing` at config construction time in `to_llm_config_with_runtime()`, so `LlmConfig` always carries provider-correct models. This eliminates the entire bug class — any code that reads `config.primary_model` gets the truth for the active provider.

---

## Required reading

1. **This document** — the fix spec.
2. **`src-tauri/src/pyramid/llm.rs`** — lines 125-277 (LlmConfig struct + Default), lines 346-379 (build_call_provider), lines 583-691 (the core call path with the cascade), lines 1829-1920 (call_model_via_registry — the working path), lines 2362-2484 (call_model_direct — separately broken).
3. **`src-tauri/src/pyramid/mod.rs`** — lines 651-698 (to_llm_config + to_llm_config_with_runtime).
4. **`src-tauri/src/pyramid/provider.rs`** — lines 262-273 (TierRoutingEntry), lines 895-952 (active_provider_id + resolve_tier).
5. **`src-tauri/src/pyramid/chain_dispatch.rs`** — lines 191-229 (resolve_model — the working approach to learn from), lines 240-268 (dispatch_llm using clone_with_model_override).
6. **`src-tauri/src/pyramid/db.rs`** — lines 12938-13040 (seed_default_provider_registry, ensure_standard_tiers_exist — the tier seeding functions).
7. **`src-tauri/src/pyramid/local_mode.rs`** — lines 499-514 (Ollama tier seeding — required tiers list).
8. **`src-tauri/src/main.rs`** — lines 4110-4199 (pyramid_set_config IPC), lines 7912+ (pyramid_enable_local_mode), lines 7949+ (pyramid_disable_local_mode).

---

## Root cause

Three layers of the same bug:

### Layer 1: Config construction ignores active provider

`PyramidConfig::to_llm_config()` (mod.rs:651) copies `self.primary_model` / `self.fallback_model_1` / `self.fallback_model_2` into `LlmConfig`. These fields are always OpenRouter slugs from `pyramid_config.json` or the Default impl. `to_llm_config_with_runtime()` (mod.rs:683) attaches the provider registry but never uses it to resolve the models.

The tier routing table (`pyramid_tier_routing`) has the correct model_id for every provider+tier combination. It's the source of truth — `call_model_via_registry` and `chain_dispatch::resolve_model` both use it. But config construction doesn't.

### Layer 2: Cascade uses config model fields as-is

The model cascade in `call_model_unified_with_audit_and_ctx` (llm.rs:683-691):

```rust
let mut use_model = if est_input_tokens > config.fallback_1_context_limit {
    config.fallback_model_2.clone()      // "x-ai/grok-4.20-beta"
} else if est_input_tokens > config.primary_context_limit {
    config.fallback_model_1.clone()      // "qwen/qwen3.5-flash-02-23"
} else {
    config.primary_model.clone()         // "inception/mercury-2"
};
```

These go straight into the HTTP body at line ~720: `"model": use_model`. When `build_call_provider` resolved to Ollama (correct endpoint), the model name is still an OpenRouter slug.

The runtime cascade on HTTP 400 context-exceeded (~line 880+) has the same problem — it falls back to `config.fallback_model_1` / `config.fallback_model_2`.

### Layer 3: call_model_direct with hardcoded slug

`call_model_direct` (llm.rs:2362) takes an explicit `model_id` parameter. Its only caller, `ascii_art.rs:135`, passes the hardcoded constant `ASCII_ART_MODEL = "x-ai/grok-4.20-beta"`. Same bug pattern: right endpoint, wrong model.

---

## Complete affected surface

### Broken: non-chain callers (all go through the cascade)

| File | Call sites | Functions called |
|---|---|---|
| `characterize.rs` | line 156 | `call_model_unified_and_ctx` |
| `extraction_schema.rs` | lines 142, 290 | `call_model_unified_and_ctx` |
| `question_decomposition.rs` | lines 356, 1273, 1975 | `call_model_unified_and_ctx` |
| `supersession.rs` | line 122 | `call_model_unified_and_ctx` |
| `evidence_answering.rs` | lines 308, 981, 1451, 1671 | `call_model_unified_with_audit_and_ctx` |
| `stale_helpers.rs` | lines 306, 821, 1234, 1511 | `call_model_with_usage_and_ctx` |
| `stale_helpers_upper.rs` | lines 609, 934, 1291, 1355, 1656, 3069 | `call_model_with_usage_and_ctx` + `call_model_unified_with_options_and_ctx` |
| `faq.rs` | line 683 | `call_model_with_usage_and_ctx` |
| `migration_config.rs` | line 606 | `call_model_unified_with_options_and_ctx` |
| `generative_config.rs` | line 273 | `call_model_unified_with_options_and_ctx` |
| `reroll.rs` | line 188 | `call_model_unified_with_options_and_ctx` |
| `routes.rs` | lines 2906, 7366 | `call_model_unified` |
| `routes_ask.rs` | line 503 | `call_model_unified` |
| `ascii_art.rs` | line 135 | `call_model_direct` (separate path) |

All of these funnel through `call_model_unified_with_audit_and_ctx` (llm.rs:583) except `ascii_art.rs` which uses `call_model_direct` (llm.rs:2362).

### NOT broken

| Path | Why it works |
|---|---|
| `call_model_via_registry` (llm.rs:1829) | Resolves `resolved.tier.model_id` from `pyramid_tier_routing` |
| `chain_dispatch::dispatch_llm` (chain_dispatch.rs:240) | Calls `resolve_model()` which consults registry, then `clone_with_model_override()` pins all cascade slots |

---

## Bonus bug: HTTP profile switch loses registry

`routes.rs:3649`:
```rust
*config_lock = pyramid_config.to_llm_config();
```

This calls `to_llm_config()` (no registry) instead of `to_llm_config_with_runtime()`. After an HTTP profile switch, the live config has no provider_registry, no credential_store, no cache_access. All LLM calls fall through to the legacy OpenRouter-only path. The IPC path (main.rs:5713) correctly uses `to_llm_config_with_runtime()`.

This is a pre-existing bug independent of the model coupling issue but in the same area and should be fixed together.

---

## The fix

### Change 0: Seed `high` and `max` tiers in ALL tier routing sources

**AUDIT FIX (Critical x2).** Stage 1 found `high`/`max` tiers don't exist. Stage 2 found `upsert_tier_routing_from_contribution` (db.rs:14517) runs `DELETE FROM pyramid_tier_routing WHERE tier_name NOT IN (...)` — it actively DELETES any tier not in the incoming contribution. So seeding alone is insufficient; the tiers must also be in every contribution source or they get wiped on first sync.

**Files (ALL must be updated):**
- `src-tauri/src/pyramid/db.rs` — `seed_default_provider_registry` (~line 12938) and `ensure_standard_tiers_exist` (~line 13023)
- `src-tauri/src/pyramid/local_mode.rs` — required tiers list (~line 505)
- `src-tauri/assets/bundled_contributions.json` — the bundled tier_routing contribution (~line 112)

**What to add in each:**

DB seeds (OpenRouter):
- `high` tier → `qwen/qwen3.5-flash-02-23`, context_limit 900_000 (matches current fallback_model_1 default)
- `max` tier → `x-ai/grok-4.20-beta`, context_limit 1_000_000 (matches current fallback_model_2 default)

Bundled contribution (`bundled_contributions.json`): Add `high` and `max` entries to the tier_routing YAML so they survive the `DELETE WHERE NOT IN` purge during contribution sync.

Local mode required tiers (local_mode.rs:505): Add `high` and `max` to the required list so the Ollama YAML contribution includes them. Both map to the detected local model (same as all other Ollama tiers).

**Why all three:** `ensure_standard_tiers_exist` runs on boot with `INSERT OR IGNORE` (migration path for existing users). `seed_default_provider_registry` runs on first-ever boot. The bundled contribution and local_mode required list ensure the tiers survive the `DELETE WHERE NOT IN` purge during every tier_routing contribution sync. Missing ANY source means the tiers exist transiently then vanish.

### Change 1: `to_llm_config_with_runtime()` resolves models from tier routing

**File:** `src-tauri/src/pyramid/mod.rs`, function `to_llm_config_with_runtime` (line 683).

After attaching the registry (current line 689), resolve the three model fields and their context limits from `pyramid_tier_routing` based on the active provider:

```rust
pub fn to_llm_config_with_runtime(
    &self,
    provider_registry: Arc<ProviderRegistry>,
    credential_store: SharedCredentialStore,
) -> LlmConfig {
    let mut cfg = self.to_llm_config();
    cfg.provider_registry = Some(provider_registry.clone());
    cfg.credential_store = Some(credential_store.clone());

    // Populate api_key from credential store (existing logic)
    if let Ok(secret) = credential_store.resolve_var("OPENROUTER_KEY") {
        cfg.api_key = secret.raw_clone();
    }

    // NEW: Resolve model fields from tier routing so the cascade sends
    // provider-correct model names. Applies to ALL providers (including
    // OpenRouter) so the tier routing table is the single source of truth.
    // Falls through to existing config values if a tier is missing.
    if let Ok(mid) = provider_registry.resolve_tier("mid", None, None, None) {
        cfg.primary_model = mid.tier.model_id;
        if let Some(limit) = mid.tier.context_limit {
            cfg.primary_context_limit = limit;
        }
    }
    if let Ok(high) = provider_registry.resolve_tier("high", None, None, None) {
        cfg.fallback_model_1 = high.tier.model_id;
        if let Some(limit) = high.tier.context_limit {
            cfg.fallback_1_context_limit = limit;
        }
    }
    if let Ok(max_tier) = provider_registry.resolve_tier("max", None, None, None) {
        cfg.fallback_model_2 = max_tier.tier.model_id;
        // fallback_2 has no dedicated context_limit field in LlmConfig;
        // resolve_context_limit() uses max(primary, fallback_1) for
        // unknown models, which is correct behavior.
    }

    cfg
}
```

**Note:** Resolution applies regardless of provider (removed the `active_provider_id() != "openrouter"` guard). The tier routing table is the source of truth for ALL providers. This also means OpenRouter model changes in the tier routing table are respected without editing pyramid_config.json.

### Change 2: Fix `ascii_art.rs` to resolve model at the caller

**AUDIT FIX (Major).** The original plan proposed fixing model resolution inside `call_model_direct`. Both auditors flagged this: `call_model_direct` takes an explicit `model_id` parameter — silently overriding it inside the function breaks the caller's contract. Fix at the CALLER instead.

**File:** `src-tauri/src/pyramid/public_html/ascii_art.rs`

Replace the hardcoded `ASCII_ART_MODEL = "x-ai/grok-4.20-beta"` with a runtime resolution. The caller already has `state.config`:

```rust
// Resolve the art model from config. On OpenRouter this is grok (quality);
// on Ollama this is whatever the "mid" tier maps to.
let config = state.config.read().await.clone();
// config.primary_model is already provider-correct after Change 1.
// For ASCII art, use fallback_model_2 (the "max" tier) for quality,
// with primary_model as fallback.
let art_model = if config.fallback_model_2.is_empty() {
    config.primary_model.clone()
} else {
    config.fallback_model_2.clone()
};
```

Then pass `&art_model` to `call_model_direct`. The `call_model_direct` function itself remains unchanged — it's a faithful pass-through.

**`call_model_direct` stays as-is.** No changes to llm.rs:2362. The function's contract (explicit model_id) is preserved.

### Change 3: Fix HTTP profile switch + preserve credentials

**AUDIT FIX (Major).** The original plan for this change was missing api_key/auth_token preservation. The IPC path at main.rs:5718-5725 preserves these; the HTTP fix must too.

**File:** `src-tauri/src/pyramid/routes.rs`, function `handle_config_profile` (line 3623).

Replace:
```rust
*config_lock = pyramid_config.to_llm_config();
```

With:
```rust
let new_config = pyramid_config.to_llm_config_with_runtime(
    state.provider_registry.clone(),
    state.credential_store.clone(),
);
let preserved_api_key = config_lock.api_key.clone();
let preserved_auth_token = config_lock.auth_token.clone();
*config_lock = new_config;
// Profiles override model selection, not credentials.
if config_lock.api_key.is_empty() {
    config_lock.api_key = preserved_api_key;
}
if config_lock.auth_token.is_empty() {
    config_lock.auth_token = preserved_auth_token;
}
```

This mirrors the IPC profile switch pattern at main.rs:5713-5726 exactly. Verified: `PyramidState` has `provider_registry` (mod.rs:823) and `credential_store` (mod.rs:828).

### Change 4: Rebuild config on local mode toggle

**AUDIT FIX (Critical).** The original plan treated provider toggle staleness as a pre-existing issue. Both auditors flagged this: the fix creates a NEW regression path. Before the fix, model names were always wrong for Ollama (but consistently wrong). After the fix, they're correct at boot but become stale after a toggle. The toggle handlers MUST rebuild the config.

**Files:** `src-tauri/src/main.rs`
- `pyramid_enable_local_mode` handler (~line 7912)
- `pyramid_disable_local_mode` handler (~line 7949)

After each handler calls `commit_enable_local_mode` / `commit_disable_local_mode` (which refreshes the registry via `registry.load_from_db`), add a config rebuild:

```rust
// Rebuild LlmConfig so model fields match the new active provider.
let new_config = pyramid_config.to_llm_config_with_runtime(
    state.pyramid.provider_registry.clone(),
    state.pyramid.credential_store.clone(),
);
let mut live = state.pyramid.config.write().await;
let preserved_api_key = live.api_key.clone();
let preserved_auth_token = live.auth_token.clone();
*live = new_config;
if live.api_key.is_empty() {
    live.api_key = preserved_api_key;
}
if live.auth_token.is_empty() {
    live.auth_token = preserved_auth_token;
}
```

Same credential preservation pattern as the profile switch.

**Implementation note:** The enable/disable handlers don't have a `PyramidConfig` variable in scope. Load it from disk: `PyramidConfig::load(data_dir)` where `data_dir` comes from `state.pyramid.data_dir.as_ref()`. Follow the pattern at main.rs:5685-5696 (IPC profile switch). If `data_dir` is `None`, skip the rebuild with a warning (no data_dir means no custom config; defaults will be tier-resolved on next restart).

### Change 5: Resolve `collapse_model` from tier routing

**DISCOVERY AUDIT FIX (Major).** `PyramidConfig.collapse_model` defaults to `"x-ai/grok-4.20-beta"` (mod.rs:194). Used in `delta.rs:735` via `clone_with_model_override(collapse_model)` — which pins all three cascade slots to this raw OpenRouter slug. When Ollama is active, this is the same bug: right endpoint, wrong model.

`clone_with_model_override` bypasses the cascade fix in Change 1 because it overwrites all three model fields. The fix must happen where `collapse_model` is consumed.

**Fix approach:** Wherever `collapse_model` is used with `clone_with_model_override`, resolve it from the "max" tier via the config's provider registry instead of using the raw config value. The simplest site: in `delta.rs` (or wherever it's loaded from PyramidConfig), replace the raw string with a registry lookup:

```rust
let collapse_model = if let Some(ref registry) = llm_config.provider_registry {
    registry.resolve_tier("max", None, None, None)
        .map(|r| r.tier.model_id)
        .unwrap_or_else(|_| raw_collapse_model.clone())
} else {
    raw_collapse_model.clone()
};
```

### Change 6: Remove hardcoded `cluster_model` in question_decomposition.rs

**DISCOVERY AUDIT FIX (Major).** `question_decomposition.rs:1601` hardcodes `cluster_model: Some("qwen/qwen3.5-flash-02-23")` in the `Question` struct. When the chain executor processes this step, `resolve_model` (chain_dispatch.rs:193) returns `step.model` directly — bypassing the registry entirely. The handoff says chain_dispatch is "NOT broken" but this direct-model override IS broken for the same reason.

**Fix:** Replace the hardcoded model with a tier reference: `cluster_model: None` (let defaults handle it) or `model_tier: Some("high")` so it flows through the registry. Search for any other hardcoded model strings in chain step construction across the codebase.

---

## What does NOT change

- **`call_model_via_registry`** — already resolves from tier routing. Unaffected.
- **`chain_dispatch::dispatch_llm`** — already uses `resolve_model()` + `clone_with_model_override()`. Unaffected (EXCEPT for steps with a direct `model` override like the cluster_model in Change 6).
- **`clone_with_model_override`** — still works. It overrides provider-correct models with provider-correct models (from the same registry). Callers that pass raw slugs (collapse_model) are fixed by Change 5.
- **`build_call_provider`** — still returns `(provider_impl, secret, provider_type)`. No signature change needed. The provider resolution is correct; only the model resolution was missing.
- **All 25+ cascade caller sites** — they get the fix for free through the config. Zero changes to characterize.rs, extraction_schema.rs, etc.
- **`resolve_context_limit`** (llm.rs:384) — compares model strings against config fields. After the fix, both the cascade's `use_model` and the config fields carry provider-correct names, so comparisons remain valid.
- **`call_model_direct`** (llm.rs:2362) — no changes. Its contract (explicit model_id parameter) is preserved. The fix is at the caller (ascii_art.rs), not inside the function.
- **`partner_model` / Partner conversation system** — intentionally hardcodes `OpenRouterProvider`. Out of scope for this fix. Document as a known limitation of local mode (Partner always uses OpenRouter).

---

## Config write sites (complete enumeration)

The live config (`PyramidState.config: Arc<RwLock<LlmConfig>>`) is written at 7 sites:

| # | Site | What it does | Fix needed? |
|---|---|---|---|
| 1 | `main.rs:9968` | Boot construction via `to_llm_config_with_runtime` | Change 1 fixes this |
| 2 | `main.rs:5713` | IPC profile switch via `to_llm_config_with_runtime` | Change 1 fixes this |
| 3 | `main.rs:4122` | `pyramid_set_config` IPC — writes individual model fields directly | See edge case below |
| 4 | `main.rs:7632` | `pyramid_set_credential` — writes api_key only | No (benign) |
| 5 | `main.rs:7652` | `pyramid_delete_credential` — clears api_key only | No (benign) |
| 6 | `routes.rs:3563` | `handle_config` HTTP POST — writes individual model fields | See edge case below |
| 7 | `routes.rs:3627` | `handle_config_profile` HTTP POST — **bonus bug** | Change 3 fixes this |

Sites 3 and 6 directly write user-supplied model strings. After our fix, the config starts with provider-correct models. If the frontend sends OpenRouter slugs via `pyramid_set_config` while Ollama is active, it overwrites the correct models with wrong ones.

**DISCOVERY AUDIT finding:** Sites 3 and 6 also PERSIST model fields to `pyramid_config.json` (main.rs:4184-4195, routes.rs:3601-3605). This happens automatically whenever ANY config field is updated — the handler reads ALL model fields from the live config and writes them to disk. After the fix, Ollama model names get persisted. On next boot, `to_llm_config_with_runtime` overrides them from tier routing, so runtime behavior is correct. But `pyramid_config.json` contains confusing provider-specific values. Not blocking for this fix, but a data hygiene issue to address later (e.g., stop persisting model fields since tier routing is now the source of truth).

---

## Edge cases

| Case | Behavior |
|---|---|
| No registry (tests, early boot) | Falls back to current behavior: OpenRouter defaults |
| Registry present, all tiers are OpenRouter | Tier routing resolves to OpenRouter models (same as before, but now from tier table not hardcoded) |
| Tier missing from routing table | `resolve_tier` errors; keep the config's existing value as fallback |
| All Ollama tiers map to same model | Cascade picks the same model for all three slots — correct, cascade is harmless |
| Different Ollama models per tier | Each cascade slot gets the right model — cascade works as designed |
| Provider toggle (enable/disable local mode) | Change 4 rebuilds the config after registry refresh. No staleness. |
| `pyramid_set_config` IPC writes raw model strings | Can overwrite resolved models. User error if they send OpenRouter slugs while Ollama is active. Not a regression — same as today. Frontend improvement deferred. |
| `handle_config` HTTP POST writes raw model strings | Same as above. |
| `config_for_model` in test fixtures | Test-only, builds configs with hardcoded models and no registry. Unaffected, expected behavior. |
| Audit row model field | After fix, audit rows record the actual provider-correct model name. Improvement, not regression. Downstream analysis that groups by model will see different values. |

---

## Test plan

1. **Build check:** `cargo check` (NOT `--lib` — must include main.rs to catch Send errors).
2. **Unit test — all three model fields:** Add a test that constructs an LlmConfig with a mock ProviderRegistry where active_provider_id is "ollama-local", tier routing maps mid→"qwen3:30b-a3b", high→"qwen3:30b-a3b", max→"qwen3:30b-a3b", and verifies that `to_llm_config_with_runtime` produces a config with ALL THREE fields (`primary_model`, `fallback_model_1`, `fallback_model_2`) set to `"qwen3:30b-a3b"` and context limits from the tier routing entries.
3. **Unit test — missing tier fallback:** Test that if a tier is missing from routing, the config keeps its existing value (graceful degradation).
4. **Integration test:** With Ollama running, trigger a build and verify the HTTP requests to localhost:11434 contain the Ollama model name, not the OpenRouter slug. Check the `pyramid_cost_log` or `llm_audit` rows — the `model` column should show the Ollama model name.
5. **Regression test:** With OpenRouter active, trigger a build and verify model names come from tier routing (should be the same OpenRouter slugs as before if tiers were seeded correctly).
6. **Provider toggle test:** Enable local mode, verify config has Ollama models. Disable local mode, verify config has OpenRouter models. No stale model names after toggle.
7. **Profile switch test:** Switch profile via HTTP API, then trigger a build. Verify the config still has its registry and correct models (bonus bug fix). Verify api_key is preserved.
8. **ASCII art test:** Click the ASCII art button with Ollama active. Verify it sends to Ollama (may produce different quality — that's expected and accepted).

---

## Implementation order

1. Change 0 (seed high/max tiers in ALL sources: db.rs + local_mode.rs + bundled_contributions.json) — prerequisite for everything else
2. Change 1 (to_llm_config_with_runtime) — the core fix
3. Change 4 (local mode toggle config rebuild) — prevents staleness regression
4. Change 3 (HTTP profile switch) — fix the bonus bug with credential preservation
5. Change 5 (collapse_model tier resolution) — same bug class via clone_with_model_override
6. Change 6 (hardcoded cluster_model) — same bug class via direct step.model override
7. Change 2 (ascii_art.rs caller fix) — separate path, same bug class
8. `cargo check` (full, not --lib)
9. Test per the plan above
