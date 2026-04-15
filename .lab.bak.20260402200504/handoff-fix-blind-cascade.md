# Rust Handoff: Fix blind model cascade on HTTP 400

## The Problem
`llm.rs` line 279: any HTTP 400 from Mercury 2 silently cascades to Qwen without logging the response body. We can't diagnose WHY the cascade happened. The 400 could be:
- Context exceeded (legitimate cascade)
- Malformed request (bug, should not cascade)
- Rate limit (should retry, not cascade)
- Unsupported response_format (should retry without it, not cascade)
- Model temporarily unavailable (should retry, not cascade)

Currently the code just does `continue` with the new model — the 400 response body is thrown away.

## The Fix

### 1. Log the 400 response body BEFORE cascading
```rust
if status == 400 && use_model != config.fallback_model_2 {
    let body_text = resp.text().await.unwrap_or_default();
    warn!(
        "[LLM] HTTP 400 from {} — body: {}. Cascading to {}",
        short_name(&use_model),
        &body_text[..body_text.len().min(500)],
        short_name(&next_model),
    );
    // ... cascade logic
}
```

### 2. Only cascade on context-exceeded 400s
Parse the 400 body. If it contains "context length" or "maximum context", cascade. For all other 400s, retry on the SAME model (it's probably a transient issue or a bug in our request).

```rust
if status == 400 {
    let body_text = resp.text().await.unwrap_or_default();
    let is_context_exceeded = body_text.contains("context length")
        || body_text.contains("maximum context")
        || body_text.contains("too many tokens");

    if is_context_exceeded && use_model != config.fallback_model_2 {
        // Legitimate cascade — input too big for this model
        warn!("[LLM] Context exceeded on {}, cascading to {}", ...);
        use_model = next_model;
        continue;
    } else {
        // Not context-related — retry on same model
        warn!("[LLM] HTTP 400 from {} (not context): {}", ...);
        // fall through to retry logic
    }
}
```

### 3. Make cascade behavior YAML-controllable (MPS)
Add to config:
```json
{
  "cascade_on_400": true,          // current behavior (cascade any 400)
  "cascade_only_context": true,    // only cascade on context-exceeded 400s
  "log_400_body": true             // always log 400 response body
}
```

## Why this matters
Mercury 2 is returning 400 on some thread_narrative calls and silently falling to Qwen (168 tps vs 900 tps). The calls are only 30K tokens — well within Mercury 2's 128K limit. We're paying a 5x speed penalty for a cascade that probably shouldn't happen.

## Also: Replace char/4 estimation with real tokenization

### The Problem
`llm.rs:201`: `let est_input_tokens = (system_prompt.len() + user_prompt.len()) / 4`

This is used for:
1. Pre-flight model selection (lines 203-211): decides Mercury vs Qwen BEFORE sending
2. `batch_items_by_tokens` in chain_executor: decides how many items fit per batch

The `/4` heuristic is wrong for structured content. JSON with repeated keys, node IDs, and schema fields tokenizes at ~3 chars/token. Dense prose tokenizes at ~4-5. Code with short variable names can be ~2-3. The error margin is 30-50%, which means batches are either too small (wasting calls) or too large (causing cascades).

### The Fix

**Option A: tiktoken-rs (best)**
Use the `tiktoken-rs` crate with cl100k_base encoding (what OpenRouter models use). Exact counts, ~1ms for 100K chars.

```toml
tiktoken-rs = "0.6"
```

```rust
use tiktoken_rs::cl100k_base;
let bpe = cl100k_base().unwrap();
let token_count = bpe.encode_with_special_tokens(text).len();
```

Replace both the pre-flight estimate and the batch token estimation.

**Option B: Track actual usage from OpenRouter responses (eventual)**
OpenRouter returns `usage.prompt_tokens` and `usage.completion_tokens` in every response. Build a per-model chars-to-tokens ratio from observed data. Start with `/4`, update the ratio after each call. After 10 calls the ratio is accurate for that model + content type.

**Recommendation:** Ship Option A now (exact, deterministic, no network dependency). Add Option B later for cost tracking and model comparison.

## Files
- `src-tauri/src/pyramid/llm.rs` — `call_model_unified_with_options`, pre-flight estimation (line 201) and HTTP 400 handling (line 279)
- `src-tauri/src/pyramid/chain_executor.rs` — `batch_items_by_tokens`, token estimation
- `Cargo.toml` — add `tiktoken-rs` dependency
