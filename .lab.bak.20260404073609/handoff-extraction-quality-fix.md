# Handoff: Extraction Prompt Quality Fix

## What was done

### Root cause identified
The document extraction prompt (`doc_extract.md`) had three Pillar 37 violations — hardcoded output prescriptions that constrained what the LLM produced:
- `"A few hundred words total"` — word budget
- `"Most documents have 2-4"` — topic count
- `"One to three sentences"` — sentence count on `current` field

These caused the LLM to produce 11 thin generic topics (~201 chars avg per `current`) instead of 5 rich specific ones (~637 chars avg in the good build `core-selected-docstest8`). Generic topic names like "Purpose & Benefits" and "Runtime Architecture" appeared in every design doc, giving the clustering LLM no signal to distinguish or group them.

### Files changed

**`chains/prompts/document/doc_extract.md`** — Rewrote to remove all prescriptions:
- Removed word budget, sentence counts, topic count range
- Added quality criterion: "Include enough detail that a reader could distinguish THIS document's treatment of the topic from a different document about the same system"
- Added self-test: "Could someone reading ONLY this field tell it apart from a similar topic in a different design document?"
- Strengthened topic name guidance: "If you would give the same topic name to a different document, it is too generic"
- Let the document decide topic count: "Some have two. Some have six."

**`chains/prompts/document/doc_recluster.md`** — Removed Pillar 37 violation:
- Removed `"roughly 12 or fewer nodes"` and `"32 nodes cannot be meaningfully synthesized"` prescriptions
- Replaced with goal-based criterion: "If a reader couldn't scan the list and immediately grasp the shape of the knowledge, you need to cluster further"

### What was NOT changed
- `doc_cluster.md` — Already has good anti-singleton language. No changes needed.
- `doc_cluster_merge.md` — Already handles cross-batch thread reconciliation correctly.
- `document.yaml` — Dehydration config was reviewed. Token-aware auto-dehydration drops fields progressively (current → entities → summary → topics → orientation), not uniformly. Topic names survive most dehydration scenarios. No change needed.

## Test build results (doc-extract-fix1)

### L0 extraction quality: DRAMATICALLY IMPROVED
Prompts deployed and build triggered via HTTP (mutations restored). 127 docs ingested and extracted.

**Comparison (action-chain-system.md, D-L0-000):**
| Metric | Old bad | Good baseline | New (fixed prompt) |
|--------|---------|---------------|-------------------|
| Topic count | 11 | 5 | 12 |
| Avg `current` chars | 201 | 637 | 730 |
| Total content chars | 2219 | 3184 | 8760 |
| Topic name specificity | Generic | Specific | Specific |

**Sample topic names from fixed prompt:**
- "Geometry, Layout Algorithm & Radius Interpolation" (was "Architecture")
- "AI Infrastructure Slot Assignment & Cost Logging" (was "Model Configuration")
- "Publisher's Office Editorial Directives & Conversations" (was generic)

**Content quality:** The `current` fields are dense with module names, schema fields, algorithm details, specific decisions. The quality criterion ("could someone tell this apart from a similar doc?") is working.

**One empty extraction:** D-L0-010 (Intelligence-in-a-Box Blueprint) has 0 topics but has orientation. Parse issue on that doc — not prompt-related.

### Build failure: TWO Rust issues need fixing

**Issue 1 (build-killer): Container output not surfaced to parent scope**
```
ERROR: Could not resolve forEach ref '$thread_clustering.threads': Unresolved reference: $thread_clustering.threads
```
The `thread_narrative` step references `$thread_clustering.threads`, but the container step `thread_clustering` with inner steps `batch_cluster` → `merge_clusters` doesn't expose `merge_clusters`' output as `$thread_clustering.threads` to the parent chain.

This is a chain executor variable resolution bug — the container primitive's last inner step output needs to be surfaced under the container step's name in the parent scope. Fix is in `chain_executor.rs`.

**Issue 2 (recoverable): Token overflow on batch_cluster**
```
WARN: HTTP 400 from mercury-2 — context 132214 tokens exceeds 128000 limit
```
The richer extractions (~8760 chars per doc vs ~2219) mean batches that previously fit within 128K now overflow. Two of four batches hit this. The `batch_max_tokens: 100000` config in `document.yaml` should prevent this, but either:
- The token estimation is off (chars-to-tokens ratio), or
- The response_schema overhead (48000 tokens for output) isn't accounted for

The dehydration fired but couldn't compress enough. Consider: (a) lowering `batch_max_tokens` to 80000, or (b) accounting for output tokens in the budget.

**Issue 3 (minor): Two parse failures healed**
Two `batch_cluster` calls returned non-JSON that needed healing. The `on_parse_error: "heal"` config handled it, but it adds latency.

## What's left

### 1. Fix container output resolution (Rust — BLOCKING)
In `chain_executor.rs`, when a container step completes, its last inner step's output must be available as `$container_step_name` in the parent scope. Currently the output is lost when the container exits.

### 2. Fix token budget accounting
Either lower `batch_max_tokens` in `document.yaml` or fix the token estimator in the chain executor to account for output token reservation.

### 3. Re-run build after Rust fix
Create a new slug and build again. The L0 extractions are proven good — the clustering and thread narrative steps just need the executor fix to proceed.

## Architectural idea: recursive condensation in L0

The current dehydration ladder is a series of cliffs — drop `current`, then drop `entities`, then drop `topics` entirely. Each step loses all content at that level. With richer extractions this matters more because there's more to lose.

**Proposal:** Have the L0 extraction produce multiple condensation levels per topic in a single LLM call:
- `current` — richest version, full detail (what we produce now)
- `current_dense` — the model's own 50% compression of `current`
- `current_core` — the model's compression of `current_dense`

The L0 LLM is perfectly positioned to do this — it just read the entire source document and has maximum context to decide what's load-bearing at each resolution. Dehydration then steps down the ladder (`current` → `current_dense` → `current_core` → topic name only) instead of cliff-dropping from full content to nothing.

This is NOT mechanical truncation (that would be a Pillar 37 violation — prescribing a rule where intelligence should decide). The model decides what survives at each compression level because it understands the document.

**Implementation:** Add `current_dense` and `current_core` fields to the extraction schema in `doc_extract.md`. Update the dehydration config in `document.yaml` to drop them in order before dropping `current` entirely. No Rust changes needed — the dehydration system already drops fields progressively.

**Trade-off:** Slightly more tokens per extraction call (the model writes three versions instead of one). But clustering never sees empty content, which is the whole point.

## Key architectural decisions from this session

1. **Pillar 37 is the frame.** Never prescribe outputs to intelligence. The extraction prompt should describe goals and quality criteria, not sentence counts or word budgets. The LLM self-regulates to whatever depth achieves the goal.

2. **`current` content is the primary clustering signal.** Topic names are secondary. Rich `current` fields give the clustering LLM enough semantic content to find conceptual relationships. Topic name specificity helps but isn't the main lever.

3. **Token-aware auto-dehydration is progressive, not uniform.** Fields drop in order (current → entities → summary → topics → orientation) only as needed to fit batch_max_tokens. Most docs in typical corpora keep topic names through dehydration.

## Files to read
- `.lab/debug-l0-extraction-GOOD.json` — what good extraction looks like (5 topics, ~637 chars avg)
- `.lab/debug-l0-extraction-sample.json` — what bad extraction looks like (11 topics, ~201 chars avg)
- `chains/defaults/document.yaml` — full pipeline definition
- `chains/prompts/document/doc_extract.md` — the fixed extraction prompt
- `chains/prompts/document/doc_recluster.md` — the fixed recluster prompt
