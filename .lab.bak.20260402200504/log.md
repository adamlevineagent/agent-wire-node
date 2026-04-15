# Experiment Log — Understanding Pyramids

## Pre-Lab: Root Cause Analysis

### Finding 1: IR executor convergence bug
The `use_ir_executor: true` path statically unrolls convergence rounds in `converge_expand.rs`.
Each round has guard `count($prev) > shortcut_at` (shortcut_at=4). The shortcut (direct apex
synthesis when ≤4 nodes) only checks the INITIAL input. When an intermediate round produces
2-4 nodes, the next round's guard is false but no shortcut fires. Result: orphan top-layer
nodes, no apex.

Evidence from database:
- `agent-wire-node2`: 18 L1 → 15 L2 → 4 L3 → STOP (no L4 apex)
- `agentwiredocsmaster`: 7 L1 → 5 L2 → 3 L3 → STOP
- `goodnewseveryone`: 60 L1 → 7 L2 → 2 L3 → STOP

**Fix applied:** Set `use_ir_executor: false` in pyramid_config.json. Chain executor path
(`chain_executor.rs:4136`) has correct dynamic loop with ≤4 direct synthesis inside the loop.

### Finding 2: Over-prescription in recluster prompts
Current recluster prompts prescribe exact cluster counts:
- 5-8 nodes → 2-3 clusters
- 9-15 nodes → 3-5 clusters
- 16+ nodes → 4-6 clusters

This is mechanical, not intelligent. The pyramid should represent natural dimensions of
understanding. The LLM should decide structure based on what the material actually needs
for comprehension, not hit a target count.

### Finding 3: Synthesis prompts focus on summarization, not understanding
The distill prompt says "create a parent node that synthesizes them" — summary framing.
Should say: "what does someone need to understand about this?" — understanding framing.

---

## Experiment 0: Switch Document Pipeline to Mercury 2

### Finding 4: Document pipeline hardcoded to Qwen for no good reason
Four steps in `document.yaml` v4.0.0 were hardcoded to `qwen/qwen3.5-flash-02-23`:
- `doc_classify_perdoc` (602 parallel calls × 20 lines each — trivially fits Mercury 2)
- `doc_taxonomy` (single call, ~120KB metadata — fits Mercury 2)
- `doc_concept_areas` (single call, ~100-200KB compacted — fits Mercury 2)
- `doc_assign` (602 parallel calls × 2-5KB each — trivially fits Mercury 2)
- `cluster_model` in `upper_layer_synthesis` (recluster calls, ~5-20KB each — trivially fits Mercury 2)

**None of these need Qwen's 900K context.** They were assigned conservatively.

### Corpus Analysis (agentwiredocschainexecutor)
- 602 documents, 9.5MB total
- 79% under 20KB, 17% are 20-50KB, 3% are 50-128KB
- 3 docs are 128-512KB (tight fit for Mercury 2's ~512KB text window)
- 1 doc is 1MB (the only one exceeding Mercury 2's context)
- **Mercury 2 can handle 598/602 docs individually. The remaining 4 need chunking.**

### Fix Applied
Changed all `model: "qwen/qwen3.5-flash-02-23"` → `model_tier: mid` in document.yaml.
Removed `cluster_model` override (falls through to step's `model_tier: mid`).
All 9 steps now use Mercury 2 (2.47K tps) instead of mixed Mercury 2 / Qwen (100-130 tps).

**Expected speedup for classify_perdoc:** 602 calls × ~1-3s each (Mercury 2) vs 602 × 18-155s each (Qwen) = ~10-50x faster.

### Status: Ready to test
Synced to runtime directory. Restart app and build on vibesmithy to validate.


