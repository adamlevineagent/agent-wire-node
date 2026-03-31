# Experiment Log — Pyramid Build Issues

## Research Phase: Architecture Analysis

### Past Lab Findings (digested from .lab.bak.*)

**Lab 1** (code pipeline optimization, ~26 experiments):
- Exp 0-2: Blind 2:1 pairing ("mathematical pyramid") → 2 apex, generic output. **Abandoned.**
- Exp 3: Semantic grouping pipeline → 7 L1 threads → single apex at L4. Score 77/100. **"Semantic grouping >> blind 2:1 pairing"**
- Exp 4: Enriched prompts → 5 threads → single apex L3. Score 80/100.
- Exp 17: Structured outputs + recursive clustering → 112 L0 → 10 L1 → 3 L2 → 1 L3. **0 failures.** Best result.
- Parking lot: carry-left orphan problem, children ID normalization, concurrency for L0.

**Lab 2** (document prompt optimization, 3 experiments + web research):
- Exp 0: Removed prescribed counts ("intelligence-driven") → **89 L0→8 L1→4 L2, NO APEX.** Recluster didn't converge.
- Exp 1: Streamlined prompts → 90 L0→9 L1→5 L2→1 L3 **but wrong apex topic (legal, not overview).**
- Exp 2: Apex-aware distill → timed out. 127 L0→11 L1, never reached L2.
- Key finding: **"Removing prescribed counts made clustering slightly WORSE"** — more threads without cap.
- Root cause: monolithic classification + clustering bottleneck (13-20 min for 127 docs).
- Recommended: two-phase split for classification and clustering.

**Lab 2 web-research** (question pyramids, 67 experiments):
- Journey: 4.7 → 8.8 composite score across ~67 experiments.
- Horizontal review was #1 blocker (collapsed all trees to flat structure).
- Clean L0 extraction prompts solved jargon leak (+2.1 audience score).
- Tree depth should scale with L0 count: max_depth ≈ log2(L0_count/3).
- Variance is high (~2 points) — same config produces 5.3 to 8.7.
- Delta build bug: multi-question accretion doesn't work (Rust bug).

### Root Cause Analysis (informed by past labs)

**Problem 1: Apex convergence failure**
The document `doc_recluster.md` prompt explicitly said:
- "If they form 7, make 7. Don't force merges"
- "Single-node groups are fine"

This *guarantees* non-convergence because the LLM is told it can return N clusters from N inputs. Past lab exp 0 confirmed: removing count guidance → no apex.

FIX: Updated all three recluster prompts with dynamic target counts (5-8 nodes → 2-3 clusters, etc) and "MUST produce STRICTLY FEWER clusters" rule. Also added Rust safety net to force-merge if LLM ignores the rule.

**Problem 2: Scaling/compactor**
`dispatch_group()` passes full `child_payload_json` (all topics, full distilled text) to synthesis. For depth >= 3, content accumulates from all children below, making payloads enormous.

FIX: Added `compact_child_payload()` function that truncates distilled text (400 chars) and topic.current (200 chars) while preserving topic names and entities. Applied at depth >= 3.

**Problem 3: Conversation pyramids**
The `conversation.yaml` chain references `zip_steps` in the `fuse` step (combine forward + reverse), but `zip_steps` is NOT implemented in the Rust chain executor. The `accumulate` config IS implemented. So forward pass and reverse pass work, but the combine step would fail.

STATUS: Not yet addressed. Need to verify whether the chain executor actually handles `zip_steps` or silently fails.

### Changes Made
1. ✅ Backed up all templates to `.lab/baseline-templates/`
2. ✅ Updated `chains/prompts/code/code_recluster.md` — dynamic target counts
3. ✅ Updated `chains/prompts/document/doc_recluster.md` — dynamic target counts
4. ✅ Updated `chains/prompts/conversation/conv_recluster.md` — dynamic target counts
5. ✅ Added convergence safety net in `chain_executor.rs` execute_recursive_cluster
6. ✅ Added `compact_child_payload()` in `build.rs` + used in `dispatch_group()`
7. ✅ `cargo check` passes (all changes)
8. ✅ Implemented `zip_steps` in `enrich_for_each_step_input` (chain_executor.rs)
   - Supports plain string form: `- forward_pass`
   - Supports object form: `- step: reverse_pass\n  reverse: true`
   - `reverse: true` flips index so reversed-chunk steps pair correctly
   - Directive key removed from resolved_input before LLM dispatch
9. ✅ Updated `conversation.yaml` — removed dead `user_template`, fixed `reverse_pass` zip entry
10. ✅ Updated `combine.md` — references `forward_pass_output` / `reverse_pass_output` field names
11. ✅ Written `.lab/chain-system-reference.md` — durable architecture documentation

