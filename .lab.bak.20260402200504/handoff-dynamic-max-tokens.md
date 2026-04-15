# Rust Handoff: Fix max_tokens vs OpenRouter context accounting

## The Problem
Every LLM call requests `max_tokens: 100000`. OpenRouter adds this to the input token count and rejects if the sum exceeds the model's context limit: "you requested about 142149 tokens (42149 of text input, 100000 in the output)."

This is an OpenRouter accounting quirk. `max_tokens` is a ceiling, not a reservation — the model generates until it's done, not until it hits 100K. Setting max_tokens low would truncate legitimate large outputs.

## The Fix
Compute `max_tokens` as `model_context_limit - estimated_input_tokens` to pass OpenRouter's check while giving the model maximum room to work. This is NOT a quality decision — it's infrastructure plumbing.

### In `llm.rs`, before building the request body:

```rust
let est_input = estimate_tokens(system_prompt, user_prompt);
let model_limit = resolve_context_limit(&use_model, config);
// Give the model all remaining headroom, capped at 48K (Mercury 2's actual max output)
let effective_max_tokens = (model_limit.saturating_sub(est_input)).min(48_000).max(1024);
```

Use `effective_max_tokens` in the API body instead of the hardcoded `max_tokens` parameter.

### Remove ir_max_tokens: 100_000 default
`mod.rs:275` — this value is meaningless now. The dynamic calculation replaces it.

## What this is NOT
This is NOT a `max_output_tokens` YAML field. Capping output is wrong — if the model needs 8K tokens for a rich synthesis, it should get them. The only limit is the model's actual context window minus the input. OpenRouter shouldn't be counting max_tokens as reserved space, but since they do, we work around it.

## Files
- `src-tauri/src/pyramid/llm.rs` — compute effective max_tokens dynamically in `call_model_unified_with_options`
- `src-tauri/src/pyramid/mod.rs` — remove `ir_max_tokens: 100_000` default (no longer needed)
