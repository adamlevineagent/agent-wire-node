# Gap Analysis: Document Pipeline Prompts

## Pipeline map (document.yaml v7.0.0)

| Step | Name | Prompt | Status |
|------|------|--------|--------|
| 1 | l0_doc_extract | doc_extract.md | ACTIVE — just fixed |
| 1 (split) | merge_sub_chunks | shared/merge_sub_chunks.md | ACTIVE |
| 1 (heal) | heal_json | shared/heal_json.md | ACTIVE |
| 2 | l0_webbing | doc_web.md | ACTIVE |
| 3a | batch_cluster | doc_cluster.md | ACTIVE |
| 3b | merge_clusters | doc_cluster_merge.md | ACTIVE |
| 4 | thread_narrative | doc_thread.md | ACTIVE |
| 5 | l1_webbing | doc_web.md (reused) | ACTIVE |
| 6 | upper_layer_synthesis | doc_distill.md | ACTIVE |
| 6 (cluster) | recursive recluster | doc_recluster.md | ACTIVE — just fixed |
| 7 | l2_webbing | doc_web.md (reused) | ACTIVE |

### Dormant prompts (not referenced by document.yaml)
- `doc_classify.md` — batch classification, old pipeline
- `doc_classify_perdoc.md` — per-doc classification, old pipeline
- `doc_taxonomy.md` — keyword normalization, old pipeline
- `doc_concept_areas.md` — thread definition from taxonomy, old pipeline
- `doc_assign.md` — per-doc thread assignment, old pipeline
- `doc_group.md` — **creative fiction** grouping prompt, clearly wrong domain
- `doc_thread_merge.md` — thread batch merge, not currently referenced

These are dead code. The old pipeline was: classify → taxonomy → concept_areas → assign → group. The v7 pipeline replaced all of that with: cluster → merge. The dormant files can be archived.

---

## Step-by-step findings

### Step 1: doc_extract.md — L0 extraction
**Status:** Just rewritten this session. Pillar 37 violations removed.

**Remaining gap: No condensation ladder.**
The `summary` field exists in the schema but comes back empty (0 chars across all test docs). The model ignores it because `current` is now rich enough that `summary` feels redundant. Two options:
1. Replace `summary` with a condensation ladder (`current_dense`, `current_core`) as discussed — the L0 LLM produces progressively compressed versions in the same call.
2. At minimum, reinforce that `summary` is required and serves a different purpose (dehydration fallback). Currently the prompt says "make it count" but the model skips it.

Option 1 is the maximal solution. The L0 LLM has full document context and is the right intelligence to decide what survives compression.

**Remaining gap: `orientation` has no quality guidance.**
The prompt says `"What this document is, what it concludes, what to take away."` — no self-test like `current` has. Orientation is the LAST field to survive dehydration and the FIRST thing the clustering LLM reads. It should have guidance proportional to its importance.

### Step 1 (split): merge_sub_chunks.md
**Status:** Clean.

**Minor gap:** Says `"3-5 sentences"` for orientation — a Pillar 37 prescription. Not severe because this is a merge step (combining existing content, not generating from intelligence), but should be consistent with the extraction prompt's approach.

### Step 2: doc_web.md — L0 cross-doc webbing
**Status:** Mostly clean.

**Pillar 37 violation: `"5-20 edges for a typical 6-12 node layer"`** — prescribes an edge count range AND a node count range. The model should decide how many edges exist based on the actual connections it finds. A corpus with deeply interconnected docs might have 50 edges; a corpus with isolated topics might have 3.

**Pillar 37 violation: strength ranges are prescribed.**
`"Strength 0.9-1.0: direct dependency"`, `"0.6-0.8: shared context"`, `"0.3-0.5: thematic"` — these are calibration guidance, not prescriptions per se, but they constrain the model to specific numeric bands. Better: describe what strength MEANS (how much would understanding one node change your understanding of the other?) and let the model calibrate.

### Step 3a: doc_cluster.md — batch clustering
**Status:** Recently improved with anti-singleton language.

**Pillar 37 violation: `"roughly 10-25 threads"`** — Line 8 says "If you receive 50 documents, you should produce roughly 10-25 threads." This prescribes a ratio. The model should decide thread count based on the conceptual structure of the material.

**Pillar 37 violation: `"Contains 2-8 documents"`** — "WHAT MAKES A GOOD THREAD" section prescribes docs-per-thread range. A thread about a central system might legitimately contain 15 documents. A thread about a niche topic might contain 1 (and that's correct, not a singleton failure).

**Gap: No acknowledgment of dehydrated inputs.** The prompt says "Each item has `node_id`, `headline`, `orientation`, and `topics`" but dehydration may strip some of these. The clustering LLM should know what to expect when fields are missing.

### Step 3b: doc_cluster_merge.md — merge batch results
**Status:** Clean. No violations found.

**Minor gap:** The prompt is thin — just principles and schema. It works because the merge task is straightforward (match thread names across batches). No changes needed.

### Step 4: doc_thread.md — thread narrative synthesis
**Status:** Strong prompt. Temporal authority, type-aware synthesis, chronological ordering.

**No violations found.** This prompt describes goals and frameworks, not counts. "Let the material determine how many" — correct. The orientation section asks for comprehensive coverage without prescribing length. The type-aware synthesis section (design docs → decisions, audits → findings, etc.) is quality guidance, not output prescription.

**Minor gap: No condensation ladder.** Thread narratives (L1 nodes) are the input to upper-layer clustering. If they're too large, the recluster step dehydrates them. Same argument as L0: the thread narrative LLM could produce condensation levels.

However, this is less critical than L0 because:
- There are fewer L1 nodes than L0 nodes (threads < docs)
- The recluster step uses `cluster_item_fields: ["node_id", "headline", "orientation"]` — it only sends headline + orientation anyway, not topics. So condensation levels on topics wouldn't help the recluster step.

**Defer this** unless upper-layer clustering shows quality problems.

### Step 5: doc_web.md (reused for L1)
Same findings as Step 2. The edge count and strength range prescriptions apply here too.

### Step 6: doc_distill.md — upper-layer synthesis
**Status:** Clean. No violations found.

Good prompt. Describes goals: "EVERY child must be represented", "merge topics that cover the same domain", "let the material determine how many topics". The headline guidance distinguishes apex-level from intermediate-level, which is contextually appropriate.

**No changes needed.**

### Step 6 (cluster): doc_recluster.md — recursive re-clustering
**Status:** Just fixed this session. Removed "roughly 12 or fewer" prescription.

**No remaining violations.** The fixed version uses goal-based apex readiness ("could a reader hold all these in their head as the top-level map").

### Step 7: doc_web.md (reused for L2)
Same findings as Step 2.

---

## Summary: what needs prompt changes and why

### Must fix (Pillar 37 violations)

| Prompt | Violation | Fix |
|--------|-----------|-----|
| doc_extract.md | `summary` field empty, no condensation ladder | Add `current_dense` + `current_core` fields; LLM produces progressive compressions |
| doc_web.md | `"5-20 edges for a typical 6-12 node layer"` | Remove count ranges; describe what makes an edge worth reporting |
| doc_web.md | Strength band prescriptions (0.9-1.0, 0.6-0.8, 0.3-0.5) | Describe what strength means semantically; let model calibrate |
| doc_cluster.md | `"roughly 10-25 threads"` from 50 docs | Remove ratio prescription |
| doc_cluster.md | `"Contains 2-8 documents"` per thread | Remove per-thread doc count range |

### Should fix (gaps, not violations)

| Prompt | Gap | Fix |
|--------|-----|-----|
| doc_extract.md | `orientation` has no quality guidance | Add self-test: orientation should tell someone whether to read this document |
| doc_cluster.md | No acknowledgment of dehydrated inputs | Add: "Some documents may arrive with only headline and topic names" |
| merge_sub_chunks.md | `"3-5 sentences"` for orientation | Remove sentence count; use quality criterion |

### Clean (no changes needed)

| Prompt | Notes |
|--------|-------|
| doc_thread.md | Strong. Goal-driven, no prescriptions. |
| doc_distill.md | Strong. Material-driven topic count. |
| doc_recluster.md | Just fixed. Goal-based apex readiness. |
| doc_cluster_merge.md | Simple and correct. |
| heal_json.md | Mechanical fix step, no intelligence decisions. |

### Dead code (archive)

| Prompt | Notes |
|--------|-------|
| doc_classify.md | Old pipeline, replaced by direct clustering |
| doc_classify_perdoc.md | Old pipeline |
| doc_taxonomy.md | Old pipeline |
| doc_concept_areas.md | Old pipeline |
| doc_assign.md | Old pipeline |
| doc_group.md | Wrong domain entirely (creative fiction) |
| doc_thread_merge.md | Not referenced by v7 chain |

---

## YAML changes needed alongside prompts

| File | Change | Why |
|------|--------|-----|
| document.yaml | Update dehydration config to include `current_dense`, `current_core` in the drop ladder | New condensation fields need to dehydrate in order: `current` → `current_dense` → `current_core` → topic names |
| document.yaml | Lower `batch_max_tokens` from 100000 to ~80000, or fix token estimator | Richer extractions overflow mercury-2's 128K context (132K observed). Output token reservation (48K) not accounted for |
