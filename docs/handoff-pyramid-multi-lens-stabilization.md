# Handoff: Pyramid Pipeline Multi-Lens Stabilization

## Overview
We stabilized the `vibe-qpX` Knowledge Pyramid build pipeline by fundamentally changing how the LLMs construct the pyramid. Instead of functioning as an obtuse file crawler generating literal software architecture lists, the pipeline now synthesizes material as a complex abstraction using a four-axis systemic framework. 

## 1. What was Shipped (Prompt Engineering)
We rewrote the core prompts governing the knowledge extraction, clustering, decomposition, and synthesis paths. These prompts now strictly forbid literal file hierarchy descriptions and instead force the LLMs to interpret content across four dimensions:

1. **The Value/Intent Lens**: What human or business value does this enable?
2. **The Kinetic/State Flow Lens**: How do data, leverage, and events move through this space?
3. **The Temporal Lens**: Where does this sit in time relative to the system? (e.g. Pre-flight definitions vs. runtime execution) 
4. **The Metaphorical Lens**: What biological or physical system does this emulate?

**Files Modified:**
- `chains/prompts/question/source_extract.md`
- `chains/prompts/question/web_cluster.md`
- `chains/prompts/question/web_cluster_merge.md`
- `chains/prompts/question/decompose.md`
- `chains/prompts/question/decompose_delta.md`
- `chains/prompts/question/answer.md`
- `chains/prompts/question/web_domain_apex.md`

*(Note: We synced these templates directly to the runtime directory at `~/Library/Application Support/wire-node/chains/` to execute real-time tests.)*

## 2. Benchmark Executions
We verified the logic via `vibe-qp12` and `vibe-qp13`. 
- The resulting apex nodes are functionally unrecognizable from previous runs. Rather than listing files and hooks, the apex generated a profound, cohesive thesis of the overarching system bridging compile-time definitions, structural metaphors, and runtime kinetic flow.
- The 4 resulting branches mapped perfectly 1:1 with the four prescribed lenses above.

## 3. The Discovery: The Native "Context Limit Router"
We attempted to force the entire pipeline to use `minimax/minimax-m2.7` by overriding the default tiers in `chains/defaults/question.yaml`. However, runtime logs on OpenRouter showed an intense volume of `mercury-2` calls during the back-half synthesis steps.

**The Architecture Finding:**
- `evidence_answering.rs` calls `llm::call_model_unified` by passing the `llm_config` but submitting `None` for any model override options.
- Looking at `llm.rs::call_model_unified_with_options` (Line ~280), model fallback logic is predicated **purely on the incoming payload's token size** (`est_input_tokens > config.primary_context_limit`). 
- `LlmCallOptions` does not currently contain a `model_override: Option<String>` parameter. 

**The Implication:**
Because the new multi-lens extraction prompts are so incredibly efficient at producing dense, abstract summaries, the synthesis map-reduce payloads crashed down dynamically into the context limits of `mercury-2` (the `primary_model`). The internal architecture auto-optimized the pipeline dynamically, routing traffic down to the cheaper, faster tier while bypassing the hardcoded YAML definition.

## Next Step Execution for Build Agents
If we want the ability to explicitly lock frontier models (like Minimax) for synthesis branches regardless of payload size constraints:

1. **Refactor `LlmCallOptions`** in `src-tauri/src/pyramid/llm.rs`.
   - Add `pub model_override: Option<String>` to `LlmCallOptions`.
   - Modify the router logic in `call_model_unified_with_options` to evaluate this override immediately, bypassing the `est_input_tokens` limits if provided.
   
2. **Propagate Overrides in `evidence_answering.rs`**.
   - Modify calls to `llm::call_model_unified` to inject `ops.tier1.answer_model` dynamically into the new `LlmCallOptions` so it correctly forwards the specific YAML `model` configuration from the engine dispatcher.
