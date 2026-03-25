# Audit Handoff: Chain Optimization Full Pass

## Overview
This document structures the blind audit of the Chain Optimization Full Pass handoff. As Partner, my goal is to ensure the architectural changes specified in `docs/handoff-chain-optimization-final.md` are robust, state-safe, and align with the YAML-driven design of the pyramid engine before implementation agents execute them.

## Target Scope
The audit targets 8 specific modifications to `agent-wire-node`:
1. Additive web edges in `drill`, `apex`, and `node` API responses.
2. Web edge context injection for clustering and synthesis.
3. `max_thread_size` enforcement and semantic overflow splitting.
4. L3+ headline deduplication via sibling context.
5. Insertion of `l0_webbing` in `code.yaml`.
6. Frontend-specific extraction prompt routing.
7. Algorithm detail guidance in base extract prompts.

## Blind Teams & Specific Questions

### Team A: Rust API & Database Layer
**Focus**: Safety of DB queries, lock contention during string generation, and JSON payload serialization.

1. **API Response Flattening**: The handoff states: "return the existing top-level node fields plus web_edges additively via flattened response types; do not nest apex under node." 
   - *Question for Team A*: Should we modify the canonical `PyramidNode` struct in `types.rs` to include an `Option<Vec<ConnectedWebEdge>>` field, or should the warp Route handlers intercept the JSON serialization and merge the arrays directly to avoid database layer pollution?
2. **Read Locks in Executor**: The handoff requires querying `pyramid_web_edges` during `thread_narrative` and `upper_layer_synthesis` prompt formatting.
   - *Question for Team A*: Prompt generation in `chain_executor.rs` occurs during the execution loop. Can we safely `spawn_blocking` to query the web edges without holding the SQLite read lock too long or causing deadlocks with concurrent sibling threads?

### Team B: Action Chain Pipeline & Prompts
**Focus**: Structural purity of the YAML pipeline, token limits, and fallback determinism.

1. **Frontend Dispatcher Override**: The handoff suggests routing `.tsx`/`.jsx` chunks to `code_extract_frontend.md` inside the extract dispatcher (`chain_executor.rs`).
   - *Question for Team B*: Does hardcoding an extension-based fallback in Rust violate the "YAML-driven" architectural goal? Should this dispatch logic be represented in `code.yaml` instead, perhaps via a new `primitive: extract_router` or conditional step array?
2. **Semantic Overflow Split**: The handoff proposes invoking the clustering model via `code_thread_split.md` to break oversized threads.
   - *Question for Team B*: If the fallback to "deterministic Part N splits" is triggered, how do we guarantee zero orphans? Does the split logic handle remainder files correctly when `assignments.len() % max_thread_size != 0`?
3. **L0 Webbing Token Limits**: Implementing `l0_webbing` before clustering sends potentially 100+ L0 nodes into an LLM context.
   - *Question for Team B*: The handoff suggests "compact mode" (only `node_id`, `headline`, `source_path`, and `entities`). Is this compact mode going to be structurally defined in Rust, or does `web` primitive need a new configuration flag in YAML (e.g., `compact_inputs: true`)?

## Example Findings (What to look for)

- **Example Finding 1 (YAML Purity Violation)**: "Modifying `dispatch_extract` in Rust to conditionally load `code_extract_frontend.md` bypasses the `instruction: "$prompts/code/code_extract.md"` defined in `code.yaml`. We should either make `instruction` accept a JSON mapping of extensions to prompts in the YAML, or do the switch at the pipeline level."
- **Example Finding 2 (Serialization Breakage)**: "Adding `web_edges` to `DrillResult.node` breaks the desktop app's typed `PyramidNode` interface because the desktop expects identical shapes between the DB row and the API response. We should wrap the node at the API boundary instead of mutating `types::PyramidNode`."
- **Example Finding 3 (Deadlock Risk)**: "Injecting web edges into the `thread_narrative` requires a DB read. If this read is awaited inside the main concurrency semaphore map loop without releasing the lock, it could stall other active workers."

## Execution Plan Post-Audit
Once the blind teams return their findings against these specific questions:
1. Revise the implementation plan to patch identified risks.
2. Delegate to **Claude Code** (build agent) to execute the corrected plan.
3. Verify via `from_depth=0` rebuild using the primary `Ember` identity.
