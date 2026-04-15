# Rust Handoff: Decompose convergence loop into YAML-controlled primitives

**Supersedes:** `handoff-apex-ready-signal.md`

## Principle
Every decision that shapes pyramid quality must live in YAML/prompts so it's a contribution — improvable by any agent through the Wire's contribution model. Rust is a dumb execution engine. It reads YAML and does what it says.

## Current hardcoded decisions in `execute_recursive_cluster`

Here is every decision currently baked into Rust that should be YAML-controlled:

### 1. Direct synthesis threshold: `<= 4`
**Line 4469:** `if current_nodes.len() <= 4` → skip clustering, synthesize all into apex.
**Problem:** Why 4? Why not 6? Why not "when the LLM says so"? This is a quality judgment frozen in a binary.
**YAML field:** `direct_synthesis_threshold: 4` on the step. When absent, no hardcoded threshold — always cluster unless the LLM signals apex_ready.

### 2. Apex termination: `<= 1`
**Line 4413:** `if current_nodes.len() <= 1` → done, this is the apex.
**This one stays in Rust.** A single node IS the apex by definition. This is structural, not a quality judgment.

### 3. Convergence safety net: force-merge when clusters >= input count
**Line 4684:** If clustering returns as many or more clusters than inputs, force-merge smallest pairs.
**Problem:** This is Rust making a quality decision (which clusters to merge) that should be the LLM's job. The safety net should be: re-call the LLM with a stronger instruction, not mechanically merge.
**YAML field:** `convergence_fallback: "retry_with_instruction"` or `"force_merge"` (current behavior) or `"abort"`. Default: retry once with an appended instruction telling the LLM it must reduce. If retry also fails, then force-merge as last resort.

### 4. Fallback clustering: positional groups of 3
**Lines 4612, 4662:** When LLM clustering fails entirely, fall back to `chunks(3)`.
**Problem:** Why 3? This is a quality decision. Positional grouping is meaningless — it's just "first 3 files, next 3 files."
**YAML field:** `cluster_fallback_size: 3` on the step. But more importantly, the fallback strategy itself should be YAML-controlled: `cluster_on_error: "positional(3)"` or `"retry"` or `"abort"`.

### 5. Cluster input projection: hardcoded to `node_id`, `headline`, truncated `orientation`, topic names
**Lines 4554-4565:** The data sent to the clustering LLM is hardcoded:
```rust
serde_json::json!({
    "node_id": n.id,
    "headline": n.headline,
    "orientation": truncate_for_webbing(&n.distilled, 500),
    "topics": topic_names,
})
```
**Problem:** This is a data shaping decision frozen in Rust. What if the clustering prompt needs entities? What if 500 chars of orientation is too little or too much?
**YAML field:** Already solved by `item_fields` from the previous handoff. But the recursive_cluster path doesn't use `item_fields` — it has its own hardcoded projection. **This path must also respect `item_fields`** (or a new `cluster_item_fields` if you want different projection for clustering vs synthesis within the same step).

### 6. Orientation truncation: `truncate_for_webbing(&n.distilled, 500)`
**Line 4561:** Orientation is truncated to 500 chars.
**Problem:** Arbitrary limit. Some corpora need more context for good clustering.
**YAML field:** Subsumed by `item_fields` + a new `truncate` modifier, OR just let `batch_max_tokens` handle it naturally — if orientations are too long, fewer items fit per batch.

### 7. Retry count for clustering: hardcoded `ErrorStrategy::Retry(3)`
**Line 4599:** `&ErrorStrategy::Retry(3)`
**Problem:** The step's own `on_error` field is ignored for the clustering sub-call.
**Fix:** Use the step's `on_error` for clustering too, or add `cluster_on_error` to the YAML.

### 8. `apex_ready` signal
**Not yet implemented.** The LLM should be able to signal "these are the right top-level dimensions, go to apex" instead of being forced to always produce fewer clusters.
**YAML field:** Add `apex_ready: boolean` to the `cluster_response_schema`. Rust checks it after each clustering call. If true, all current nodes go to direct apex synthesis.

## The MPS YAML surface

```yaml
- name: upper_layer_synthesis
  primitive: synthesize
  instruction: "$prompts/document/doc_distill.md"
  recursive_cluster: true
  cluster_instruction: "$prompts/document/doc_recluster.md"
  cluster_item_fields: ["node_id", "headline", "orientation", "topics.name"]
  cluster_response_schema:
    type: object
    properties:
      apex_ready:
        type: boolean
        description: "true if these nodes are the natural top-level dimensions"
      clusters:
        type: array
        items:
          type: object
          properties:
            name:
              type: string
            description:
              type: string
            node_ids:
              type: array
              items:
                type: string
          required: ["name", "description", "node_ids"]
    required: ["apex_ready", "clusters"]
  # Convergence controls — all optional, all overridable
  direct_synthesis_threshold: null   # null = no hardcoded threshold, trust apex_ready
  convergence_fallback: "retry"      # "retry" | "force_merge" | "abort"
  cluster_on_error: "retry(3)"       # independent from step on_error
  cluster_fallback_size: 3           # only used if convergence_fallback is force_merge
  depth: 1
  save_as: node
  node_id_pattern: "L{depth}-{index:03}"
  model_tier: mid
```

## Rust implementation

### New fields on ChainStep

```rust
#[serde(default)]
pub direct_synthesis_threshold: Option<usize>,  // None = no threshold, rely on apex_ready

#[serde(default)]
pub convergence_fallback: Option<String>,  // "retry" | "force_merge" | "abort"

#[serde(default)]
pub cluster_on_error: Option<String>,  // independent error strategy for clustering calls

#[serde(default)]
pub cluster_fallback_size: Option<usize>,  // positional fallback chunk size

#[serde(default)]
pub cluster_item_fields: Option<Vec<String>>,  // projection for clustering input (separate from synthesis)
```

### Changes to `execute_recursive_cluster`

1. **Direct synthesis threshold:** Replace `if current_nodes.len() <= 4` with:
   ```rust
   if let Some(threshold) = step.direct_synthesis_threshold {
       if current_nodes.len() <= threshold { /* direct synthesis */ }
   }
   // If no threshold set, only apex_ready or <= 1 triggers apex
   ```

2. **Cluster input projection:** Replace hardcoded json! with:
   ```rust
   let fields = step.cluster_item_fields.as_ref()
       .or(step.item_fields.as_ref());
   let cluster_input = if let Some(fields) = fields {
       current_nodes.iter().map(|n| project_node(n, fields)).collect()
   } else {
       // current hardcoded projection as fallback
   };
   ```

3. **apex_ready check:** After clustering call returns:
   ```rust
   if let Some(true) = output.get("apex_ready").and_then(|v| v.as_bool()) {
       // Jump to direct apex synthesis with all current_nodes
   }
   ```

4. **Convergence fallback:** Replace force-merge block with:
   ```rust
   let fallback = step.convergence_fallback.as_deref().unwrap_or("force_merge");
   match fallback {
       "retry" => { /* re-call LLM with stronger convergence instruction */ }
       "force_merge" => { /* current force-merge logic */ }
       "abort" => { return Err(...) }
   }
   ```

5. **Cluster error handling:** Replace hardcoded `Retry(3)` with:
   ```rust
   let error_strategy = step.cluster_on_error.as_deref()
       .map(parse_error_strategy)
       .unwrap_or(ErrorStrategy::Retry(3));
   ```

6. **Positional fallback size:** Replace hardcoded `chunks(3)` with:
   ```rust
   let fallback_size = step.cluster_fallback_size.unwrap_or(3);
   current_nodes.chunks(fallback_size)
   ```

## Files
- `src-tauri/src/pyramid/chain_engine.rs` — add new fields to ChainStep
- `src-tauri/src/pyramid/chain_executor.rs` — refactor `execute_recursive_cluster` to read all decisions from step config
- All chain YAML files — add convergence controls and `apex_ready` to cluster_response_schema
- All recluster prompts — add apex_ready instruction

## What stays in Rust
- The loop structure itself (read nodes → cluster → synthesize → repeat)
- `<= 1` node = apex (structural definition)
- Node persistence, progress tracking, resume/replay logic
- LLM dispatch, retry mechanics, DB operations

Everything else moves to YAML.
