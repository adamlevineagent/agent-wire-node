# Handoff: Structured Outputs for Chain Pipeline

## Problem
The chain pipeline's #1 reliability issue is JSON parse failures. The LLM (especially qwen for clustering) sometimes returns markdown prose, thinking text, or malformed JSON instead of the expected schema. This caused experiment #14 to fail (0 clusters → no apex).

Current mitigations (prompt instructions + retry at temp 0.1) are band-aids. OpenRouter supports `response_format: json_schema` which **enforces** valid JSON at the API level.

## What Needs to Change

### 1. `src-tauri/src/pyramid/llm.rs` — Add response_format support

`call_model()` (line 44) currently builds the request body as:
```rust
let body = serde_json::json!({
    "model": use_model,
    "messages": [...],
    "temperature": temperature,
    "max_tokens": max_tokens
});
```

Add an optional `response_format` parameter:
```rust
pub async fn call_model(
    config: &LlmConfig,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f32,
    max_tokens: usize,
    response_format: Option<&serde_json::Value>,  // NEW
) -> Result<String> {
```

Then conditionally include it in the body:
```rust
let mut body = serde_json::json!({
    "model": use_model,
    "messages": [...],
    "temperature": temperature,
    "max_tokens": max_tokens
});
if let Some(rf) = response_format {
    body.as_object_mut().unwrap().insert("response_format".to_string(), rf.clone());
}
```

Also do the same for `call_model_with_usage()` (line 172).

All existing callers pass `None` — zero breakage.

### 2. `src-tauri/src/pyramid/chain_engine.rs` — Add schema field to ChainStep

```rust
pub struct ChainStep {
    // ... existing fields ...

    /// Optional JSON schema for structured output enforcement via OpenRouter.
    /// If set, the LLM response is guaranteed to match this schema.
    #[serde(default)]
    pub response_schema: Option<serde_json::Value>,
}
```

### 3. `src-tauri/src/pyramid/chain_dispatch.rs` — Pass schema to LLM

In `dispatch_llm()` (line 105), build the response_format from the step's schema:

```rust
let response_format = step.response_schema.as_ref().map(|schema| {
    serde_json::json!({
        "type": "json_schema",
        "json_schema": {
            "name": step.name.replace("-", "_"),
            "strict": true,
            "schema": schema
        }
    })
});

let response = llm::call_model(
    config_ref, system_prompt, &user_prompt, temperature, max_tokens,
    response_format.as_ref()
).await?;
```

When response_format is active, the JSON-retry logic (lines ~150-165) becomes unnecessary but can stay as a safety net.

### 4. Chain YAML — Declare schemas per step

Example for the recluster step (the one that keeps failing):

```yaml
- name: upper_layer_synthesis
  primitive: synthesize
  instruction: "$prompts/code/code_distill.md"
  recursive_cluster: true
  cluster_instruction: "$prompts/code/code_recluster.md"
  cluster_model: "qwen/qwen3.5-flash-02-23"
  cluster_response_schema:
    type: object
    properties:
      clusters:
        type: array
        items:
          type: object
          properties:
            name:
              type: string
              description: "2-6 word cluster label"
            description:
              type: string
              description: "1-2 sentences on what this cluster covers"
            node_ids:
              type: array
              items:
                type: string
              description: "IDs of nodes in this cluster"
          required: ["name", "description", "node_ids"]
          additionalProperties: false
    required: ["clusters"]
    additionalProperties: false
```

Same pattern for extract, thread_cluster, thread_narrative, and distill steps — each declares its expected schema.

## Model Compatibility

- **qwen/qwen3.5-flash-02-23**: Should support structured outputs (most open-source models do via OpenRouter)
- **inception/mercury-2**: Need to verify — check OpenRouter models page for structured_outputs support
- **Fallback**: If a model doesn't support it, the existing prompt-based approach still works (response_format is optional per step)

## Priority

**HIGH** — This is the single biggest reliability improvement available. It would:
1. Eliminate all JSON parse failures (currently ~3-6% of LLM calls)
2. Remove the need for retry-at-temp-0.1 workarounds
3. Make qwen clustering 100% reliable (currently fails ~30% of the time)
4. Allow removing all "Output valid JSON only" prompt boilerplate

## Testing

After implementation:
1. Run `bash .lab/run-experiment.sh opt-015`
2. Check [CHAIN] logs for zero "JSON parse failed" lines
3. Run 3 consecutive builds — all should complete with 0 failures
4. Blind test the result

## Files to Modify

| File | Change |
|------|--------|
| `src-tauri/src/pyramid/llm.rs` | Add `response_format` param to `call_model()` and `call_model_with_usage()` |
| `src-tauri/src/pyramid/chain_engine.rs` | Add `response_schema` field to `ChainStep` |
| `src-tauri/src/pyramid/chain_dispatch.rs` | Build response_format from step schema, pass to `call_model()` |
| `chains/defaults/code.yaml` | Add `response_schema` to each step |
| All other `call_model()` callers | Pass `None` for response_format (no behavior change) |

## Current Experiment Status

| Experiment | Score | Key Issue |
|-----------|-------|-----------|
| #11 | 83/100 | Best with normal scoring |
| #13 | 85/100 | Best with harsh scoring, richer prompts |
| #14 | FAILED | qwen returned markdown instead of JSON for recluster |
| #14b | Pending | Stronger JSON prompt — band-aid, structured outputs would be definitive |

## Lab Location

All experiment history, configs, and results are in `.lab/` (gitignored, survives all git ops).
