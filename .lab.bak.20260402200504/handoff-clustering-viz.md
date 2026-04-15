# Frontend Handoff: Visualizing non-node pipeline steps

## The Problem
After L0 extraction completes (127/127 nodes), the UI shows "FINALIZING" with no progress for 1-2 minutes while clustering and merge steps run. The user sees a dead screen between L0 completing and L1 nodes appearing.

## What's happening in that gap

The document pipeline runs these steps between L0 and L1:

| Step | Type | Duration | Produces nodes? | What it does |
|------|------|----------|-----------------|-------------|
| `l0_webbing` | web | ~5s | No (web_edges) | Cross-references L0 nodes |
| `thread_clustering_batch` | classify, batched | ~30-60s | No (step_only) | Splits L0s into batches, clusters each batch into concept threads |
| `thread_clustering` | classify, merge | ~15-30s | No (step_only) | Merges batch results into final unified threads |
| `thread_narrative` | synthesize, per-thread | ~2-3min | **Yes** (L1 nodes) | Synthesizes each thread into an L1 node |

The gap is `l0_webbing` + `thread_clustering_batch` + `thread_clustering` — about 1-2 minutes of invisible work before the first L1 node appears.

Similarly, between L1 completing and L2 appearing:

| Step | Duration | Produces nodes? | What it does |
|------|----------|-----------------|-------------|
| `l1_webbing` | ~5s | No (web_edges) | Cross-references L1 nodes |
| `upper_layer_synthesis` (cluster sub-call) | ~10-20s | No (internal) | Clusters L1 nodes into higher domains |
| `upper_layer_synthesis` (synthesis) | ~15-30s | **Yes** (L2 nodes) | Synthesizes each cluster |

And between each upper layer: a recluster call (~10s, no nodes) then synthesis (~15-30s, nodes).

## How steps are triggered

The chain executor runs steps sequentially in YAML order. Each step's output becomes available to the next via `$step_name` references. The executor emits `LayerEvent` messages:

- `LayerEvent::Discovered { depth, step_name, estimated_nodes }` — a new layer is about to start
- `LayerEvent::NodeCompleted { depth, step_name, node_id, label }` — a node finished within a layer
- `LayerEvent::LayerCompleted { depth, step_name }` — all nodes at this depth are done

**Only steps with `save_as: node` emit NodeCompleted/LayerCompleted events.** The clustering and webbing steps are invisible to the layer event channel.

## What the frontend needs

A step-level progress indicator between layers. The chain executor already logs step transitions:

```
[CHAIN] step "l0_webbing" complete
[CHAIN] step "thread_clustering_batch" complete
[CHAIN] step "thread_clustering" complete
```

### Option A: Expose current step name via existing channel

The `BuildProgress` struct already sends `done` and `total` counts. Add an optional `current_step: Option<String>` field. The frontend shows "Clustering..." or "Webbing..." between layer fills.

### Option B: New StepEvent channel

Add a parallel event channel for step-level events:

```rust
enum StepEvent {
    StepStarted { step_name: String, step_index: usize, total_steps: usize },
    StepCompleted { step_name: String },
}
```

The frontend receives these alongside LayerEvents and can show a step indicator.

### Option C: Derive from LayerEvents

The frontend already knows the pipeline structure (it's in the chain YAML). When L0 LayerCompleted fires and no L1 Discovered has arrived yet, the frontend knows clustering is running. Show a "Clustering..." indicator based on time-since-last-layer-completed.

## Step names to display

Map internal step names to user-friendly labels:

| step_name | Display |
|-----------|---------|
| `l0_webbing` | "Cross-referencing..." |
| `thread_clustering_batch` | "Clustering documents..." |
| `thread_clustering` | "Merging clusters..." |
| `l1_webbing` | "Cross-referencing threads..." |
| `l2_webbing` | "Cross-referencing layers..." |
| `upper_layer_synthesis` (cluster sub-call) | "Organizing layers..." |

## The build viz already shows the right structure

The pyramid build viz correctly shows layers filling in. The only gap is the dead time between layers. Any of the three options above fills that gap with a step-level indicator.
