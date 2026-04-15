# Pyramid Build Issues — Research Lab

## Objective
Fix three pyramid build issues in priority order:
1. **Scaling/compactor** — Large folders blow up context because dispatch_group passes full child_payload_json (all topics, full distilled text) with no compaction
2. **Apex convergence** — Pyramids produce multiple L3 nodes instead of converging to single apex
3. **Conversation pyramids** — Never tested/working in the new chain engine

## Metrics
1. Scaling: Large pyramid build completes without context overflow; token count per synthesis call stays under model limit
2. Apex: Build produces exactly 1 apex node
3. Conversation: Forward→reverse→combine→cluster→synthesize→apex pipeline completes

## Baseline Templates
Saved to `.lab/baseline-templates/` — restore from here if experiments fail

## Key Files
- `chains/defaults/code.yaml` — code pipeline chain
- `chains/defaults/document.yaml` — document pipeline chain
- `chains/defaults/conversation.yaml` — conversation pipeline chain
- `chains/prompts/{code,document,conversation}/` — prompt templates
- `src-tauri/src/pyramid/chain_executor.rs` — chain execution engine
  - `execute_recursive_cluster()` (L4055) — convergence loop
  - `dispatch_group()` (L4549) — group synthesis (scaling problem here)
  - `child_payload_json()` (build.rs L2816) — full node payload (no compaction)
  - `build_webbing_input()` (L1553) — webbing input (HAS compact_inputs support)
  - `recursive_cluster_layer_complete()` (L1047) — resume check
- `src-tauri/src/pyramid/build.rs` — legacy build pipeline (still used for some paths)

## Architecture Understanding
- Chain engine dispatches steps defined in YAML
- `recursive_cluster` steps: cluster → synthesize per cluster → repeat until ≤1 node
- `dispatch_group` builds synthesis input by calling `child_payload_json` per child node — NO compaction
- `build_webbing_input` has `compact_inputs` flag that strips down to headline+entities — but synthesis doesn't use it
- The clustering step already sends compact input (headline + truncated orientation + topic names)
- The synthesis step sends FULL child_payload_json — this is the scaling bottleneck
