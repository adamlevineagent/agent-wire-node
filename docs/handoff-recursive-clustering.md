# Handoff: Replace Recursive Pairing with Recursive Clustering

## Context

The code pyramid chain engine currently builds upper layers (L1 to apex) using **blind 2:1 adjacent pairing** (`recursive_pair` in `chain_executor.rs`). This creates `log2(N)` layers where each layer is a lossy summary of the previous one. With 11 L1 threads, you get 5 layers (L2-L5) of progressive information loss, with headlines repeating ("Pyramid Core Systems" at L5, L4, L3, L2) because adjacent pairs happen to cover similar topics.

**Blind testing scores:**
- 5 threads + 3 layers (exp #4): **80/100** — sweet spot because less compression
- 11 threads + 5 layers (exp #8): **70/100** — more threads is better, but 5 layers of blind pairing kills it

**The fix:** Replace `recursive_pair` with `recursive_cluster` — at each layer, instead of pairing adjacent nodes, do an LLM-based semantic re-clustering, then synthesize each cluster. This means every layer adds semantic structure instead of removing detail.

---

## What Needs to Change

### 1. New execution mode in `chain_executor.rs`: `recursive_cluster`

Currently the step dispatch logic (line ~421) is:
```
if step.mechanical → execute_mechanical
else if step.recursive_pair → execute_recursive_pair
else if step.pair_adjacent → execute_pair_adjacent
else if step.for_each → execute_for_each
else → execute_single
```

Add a new branch:
```
else if step.recursive_cluster → execute_recursive_cluster
```

### 2. Add `recursive_cluster` field to `ChainStep` (chain_engine.rs line ~125)

```rust
#[serde(default)]
pub recursive_cluster: bool,
```

Add mutual exclusivity validation with `recursive_pair` and `pair_adjacent` (near line 258).

### 3. Implement `execute_recursive_cluster` (new function in chain_executor.rs)

**Algorithm:**
```
fn execute_recursive_cluster(step, starting_depth, ...) {
    loop {
        current_nodes = get_nodes_at_depth(slug, depth)

        if current_nodes.len() <= 1:
            return apex  // done

        if current_nodes.len() <= 4:
            // Small enough to synthesize directly into apex
            // Do a single LLM call to merge all remaining nodes into one apex
            synthesize_all(current_nodes) → single apex node at depth+1
            return apex

        // Step A: CLUSTER — ask LLM to group current nodes into 3-5 clusters
        cluster_assignments = llm_classify(
            instruction: step.cluster_instruction,  // new field
            model: step.cluster_model or step.model,  // may want big-context model
            input: current_nodes (headlines + orientations + topic names)
        )
        // Returns: { "clusters": [{ "name": "...", "description": "...", "node_ids": ["L1-000", "L1-003", ...] }] }

        // Step B: SYNTHESIZE — for each cluster, synthesize assigned nodes into one parent
        for cluster in cluster_assignments.clusters:
            child_nodes = cluster.node_ids.map(|id| find node)
            new_node = llm_synthesize(
                instruction: step.instruction,  // the existing synthesis prompt
                input: child_nodes
            )
            save new_node at depth+1, with children = cluster.node_ids

        depth += 1
        // loop back — re-cluster the new layer
    }
}
```

**Key differences from `execute_recursive_pair`:**
1. Instead of fixed 2:1 pairing, the LLM decides groupings (3-5 clusters per layer)
2. Each cluster can have 2-5 nodes in it (variable fan-in, not fixed 2)
3. The `<=4` shortcut avoids a pointless clustering step when we're close to apex
4. No carry-left orphan problem — every node gets assigned to a cluster

### 4. New YAML fields needed on ChainStep

```yaml
- name: upper_layer_synthesis
  primitive: synthesize
  recursive_cluster: true           # NEW — replaces recursive_pair
  cluster_instruction: "$prompts/code/code_recluster.md"  # NEW — prompt for re-clustering
  cluster_model: "qwen/qwen3.5-flash-02-23"  # NEW — optional, model for clustering step
  instruction: "$prompts/code/code_distill.md"  # existing — prompt for synthesis step
  depth: 1
  save_as: node
  node_id_pattern: "L{depth}-{index:03}"
  model_tier: mid
  temperature: 0.3
  on_error: "retry(3)"
```

Or if you prefer to keep it simpler, the cluster instruction and model could be sub-fields:

```yaml
  recursive_cluster:
    cluster_instruction: "$prompts/code/code_recluster.md"
    cluster_model: "qwen/qwen3.5-flash-02-23"
    target_clusters: "3-5"   # or auto-scale based on node count
```

### 5. New prompt needed: `code_recluster.md`

This is a variant of `code_cluster.md` but operates on L1+ nodes (which have orientations and topics) rather than L0 nodes (which have headlines and exports). Draft:

```markdown
You are given the summaries of N nodes from a knowledge pyramid layer. Each node has a headline, orientation, and topic list.

Group these nodes into 3-5 clusters. Each cluster should represent a high-level domain that a developer would recognize as a coherent architectural area.

RULES:
- Every node must be assigned to exactly ONE cluster
- 3-5 clusters. Fewer is better if the coverage is complete.
- Cluster names should be concrete: "Backend Services & APIs", not "Group 1"
- Balance: each cluster should have at least 2 nodes

Output valid JSON only:
{
  "clusters": [
    {
      "name": "Cluster Name",
      "description": "1 sentence: what this architectural area covers",
      "node_ids": ["L1-000", "L1-003", "L1-007"]
    }
  ]
}

/no_think
```

### 6. The synthesis prompt (`code_distill.md`) needs a small update

Currently it takes exactly 2 siblings ("You read two sibling nodes"). For recursive clustering, a synthesis step may receive 2-5 nodes. Update the prompt to handle variable numbers:

```markdown
You read N sibling nodes describing parts of a codebase. Organize everything they contain into coherent TOPICS.
...
```

The existing `code_distill.md` already says "two sibling nodes" but the logic is generic enough to work with N — just update the framing sentence and remove "SIBLING B IS LATER" temporal logic (not applicable for code).

---

## Implementation Details

### How clustering step works within `execute_recursive_cluster`

The clustering step is essentially the same as the existing `execute_single` path:
1. Build user prompt: JSON array of `{ node_id, headline, orientation, topics: [{ name }] }` for all current-depth nodes
2. Call LLM with the cluster instruction
3. Parse JSON response to get cluster assignments
4. Validate: every node_id from current depth appears in exactly one cluster

If clustering fails (JSON parse error, missing assignments):
- Retry up to 3 times
- Fallback: chunk nodes into groups of 3 (positional, not semantic) — ugly but functional

### How synthesis step works within `execute_recursive_cluster`

For each cluster:
1. Gather the full node content (distilled text) for all assigned node IDs
2. Build user prompt: concatenated node content (same format as current pair dispatch, just with N nodes instead of 2)
3. Call LLM with the synthesis instruction
4. Save result as new node at depth+1 with `children = cluster.node_ids`

This is essentially `dispatch_pair` generalized to `dispatch_group`. The current `dispatch_pair` function (line 918) builds a user prompt from `left.distilled` and `right.distilled`. The new version concatenates all node distillations with headers.

### User prompt format for synthesis of a cluster

```
## CHILD NODE 1: "Backend Services & Runtime"
<distilled content of node>

## CHILD NODE 2: "Authentication & Session Management"
<distilled content of node>

## CHILD NODE 3: "Database & Persistence Layer"
<distilled content of node>
```

### Expected pyramid shapes

Before (recursive_pair with 11 threads):
```
L0: 112 files
L1: 11 threads
L2: 6 (carry-left orphan)
L3: 3
L4: 2
L5: 1 apex
Total upper layers: 4
```

After (recursive_cluster with 11 threads):
```
L0: 112 files
L1: 11 threads  (from thread clustering)
L2: 3-4 clusters (from re-clustering L1)
L3: 1 apex       (direct synthesis of 3-4 L2 nodes)
Total upper layers: 2
```

For larger codebases (50+ threads):
```
L0: 500 files
L1: 30 threads
L2: 8-10 clusters
L3: 3-4 clusters
L4: 1 apex
Total upper layers: 3
```

---

## Existing Code References

| File | What to touch | Line refs |
|------|---------------|-----------|
| `chain_engine.rs` | Add `recursive_cluster` to `ChainStep` struct | ~125 |
| `chain_engine.rs` | Add validation (mutual exclusivity) | ~258 |
| `chain_executor.rs` | Add dispatch branch | ~421-462 |
| `chain_executor.rs` | New `execute_recursive_cluster` function | after line 1163 |
| `chain_executor.rs` | `execute_recursive_pair` (line 1000) is the template — same structure, different inner loop |
| `chain_executor.rs` | `dispatch_pair` (line 918) — generalize to `dispatch_group` taking N nodes |
| `chain_dispatch.rs` | `resolve_model()` (line 65) — already handles per-step model overrides, no change needed |
| `code.yaml` | Replace `recursive_pair: true` with `recursive_cluster: true` + cluster fields |

---

## Bonus: Fix carry-left while you're in there

The carry-left orphan problem (line 1142-1148) creates duplicate nodes that ride all the way to apex without ever being synthesized. If implementing `recursive_cluster`, this is moot (clustering assigns every node). But if `recursive_pair` is kept as a fallback, the fix is: when odd count, merge the last 3 nodes instead of pairing 2 + carrying 1.

---

## Bonus: Layer-by-layer rebuild

While touching the executor, consider adding a `from_depth` parameter to the build API. If `from_depth=1`, delete all nodes at depth >= 1 and re-run steps 2+ without re-running L0 extraction. L0 takes ~8 minutes and is stable — only the clustering and synthesis prompts change between experiments.

This would let the researcher iterate 5x faster (clustering + synthesis takes ~2 min vs full rebuild ~10 min).

---

## Test Plan

1. Build the feature on `research/chain-optimization` branch
2. Run `bash .lab/run-experiment.sh opt-010` (the run script syncs prompts + YAML automatically)
3. Check structure: `sqlite3 pyramid.db "SELECT depth, COUNT(*) FROM pyramid_nodes WHERE slug='opt-010' GROUP BY depth;"`
4. Expected: L0 ~112, L1 ~8-12, L2 ~3-4, L3 1 apex
5. Blind test: spawn two haiku agents, give them only MCP tool access to the pyramid, have them answer 10 questions, score 0-10 each
6. Target: 85/100 average (currently 70-80 depending on thread count)

---

## Current Branch State

Branch `research/chain-optimization` has all prompt work committed. The `.lab/` directory (untracked, in `.gitignore`) has full experiment history. Key files already modified on this branch:

- `chains/defaults/code.yaml` — simplified 4-step pipeline (extract → cluster → thread synth → recursive pair)
- `chains/prompts/code/code_extract.md` — topic-based format matching pyramid node schema
- `chains/prompts/code/code_cluster.md` — LLM clustering prompt for L0→L1
- `chains/prompts/code/code_thread.md` — per-thread synthesis prompt
- `chains/prompts/code/code_distill.md` — code-specific distill (replaces conversation/distill.md)
- `src-tauri/src/pyramid/chain_dispatch.rs` — model override fix (line ~79)
- `src-tauri/src/pyramid/chain_executor.rs` — children wiring fix, flush timing fix
