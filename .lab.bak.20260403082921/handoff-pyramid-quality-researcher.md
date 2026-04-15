# Researcher Handoff: Pyramid Quality — All Three Types Working

## Objective
Get document, code, and conversation pyramids building consistently with good structure — meaningful thread grouping, rich synthesis, proper convergence to apex, zero orphans, no parse failures blocking builds.

## Read First
**`chains/CHAIN-DEVELOPER-GUIDE.md`** — the canonical reference. Start with the Pillars and Patterns sections. They define how to think about every decision you'll make. The field reference is below the patterns.

Key patterns to internalize:
- **If it doesn't fit, pyramid it.** Same pattern at every scale — split, process, merge.
- **Rich inputs, dense outputs.** Input tokens are free. Output tokens compound.
- **The schema is the instruction.** It teaches the LLM what to think about.
- **Distill, don't summarize.** Keep what matters, discard what doesn't.

## Current State (2026-04-03)

### What works
- **Pipeline infrastructure**: sub-chains, batching, dehydration, self-healing parse, token bucket rate limiting, tiktoken, dynamic max_tokens, JSON depth walker, largest-first dispatch — all shipped
- **L0 extraction**: fast (~2 min for 127 docs), compact "distill/reference card" framing, topics.summary field
- **Self-healing**: catches truncated/malformed JSON and heals it (top-level steps confirmed working, inner container steps assume fixed)
- **Convergence**: recursive_cluster with apex_ready signal, configurable thresholds
- **CHAIN-DEVELOPER-GUIDE.md**: fully updated with all features, patterns, pillars, failure modes

### What's broken
- **Clustering produces 1:1 thread ratios** — every doc/file becomes its own thread. Primary quality problem.
- **Root cause identified and partially fixed**: the old response_schema had `assignments` with `topic_index` and `topic_name`, teaching the LLM to assign topics instead of grouping documents. Schema now simplified to `assignments` with only `source_node`. **Not yet validated** — need a clean build to confirm the schema fix resolves the 1:1 problem.
- **Code pipeline uses doc merge prompt** — `code.yaml` merge step references `doc_cluster_merge.md`. Needs code-specific merge prompt with subsystem semantics and ZERO ORPHANS.
- **Conversation pipeline untested** — YAML updated but no builds run with the new architecture.

### What worked before
Build `core-selected-docstest8` produced: L0:127 → L1:34 → L2:17 → L3:1 with good thread quality. That was with verbose extractions (10+ topics per doc) and the old clustering schema. The dense "distill" extraction now produces better per-doc quality but less clustering signal — which is why dehydration matters (small docs stay fully hydrated, only large docs get stripped).

## Your Priorities

### P0: Validate the schema fix — does simplified `assignments` produce real grouping?
Build a doc pyramid on `core-selected-docs` with the current YAML. The schema now has `assignments` with only `source_node` (no `topic_index`, no `topic_name`). Check:
1. Does the clustering produce 10-30 threads instead of 127?
2. Do threads contain multiple documents?
3. Pull `batch_cluster` output and count unique `source_node` per thread

If still 1:1, investigate what the clustering LLM actually receives after dehydration. Compare against `core-selected-docstest8` (the good build).

### P1: Create code-specific merge prompt
`chains/prompts/code/code_cluster_merge.md` — currently code uses `doc_cluster_merge.md` which:
- Says "documents" not "files/subsystems"
- Uses `D-L0-XXX` example IDs (code uses `C-L0-XXX`)
- Allows `unassigned` output (silently drops files)

Code merge should use architectural grouping semantics and ZERO ORPHANS.

### P2: Test conversation pipeline end-to-end
Build a conversation pyramid. Verify forward/reverse/combine/clustering/convergence works.

### P3: Improve synthesis density at L2+
The L1→L2 density cliff: L1 nodes are rich, L2+ nodes are thin enumeration. `doc_distill.md` and `code_distill.md` need to carry specifics forward — decisions, dates, metrics — not just "this synthesizes X and Y."

## Key Files

### Pipeline definitions
- `chains/defaults/document.yaml` — v7, sub-chain clustering, dehydrate, recursive_cluster
- `chains/defaults/code.yaml` — v3, sub-chain clustering, recursive_cluster
- `chains/defaults/conversation.yaml` — v3, forward/reverse/combine, sub-chain clustering

### Document prompts
- `chains/prompts/document/doc_extract.md` — "distill/reference card" framing with topics.summary
- `chains/prompts/document/doc_cluster.md` — clustering ("group DOCUMENTS, not topics")
- `chains/prompts/document/doc_cluster_merge.md` — merge batch results
- `chains/prompts/document/doc_thread.md` — thread synthesis (temporal authority, type-aware)
- `chains/prompts/document/doc_distill.md` — upper layer synthesis
- `chains/prompts/document/doc_recluster.md` — convergence (apex_ready, ≤12 nodes)
- `chains/prompts/document/doc_web.md` — cross-reference webbing

### Code prompts
- `chains/prompts/code/code_extract.md` — code extraction with topics.summary
- `chains/prompts/code/code_cluster.md` — code clustering
- `chains/prompts/code/code_cluster_merge.md` — **NEEDS TO BE CREATED** (currently uses doc version)
- `chains/prompts/code/code_thread.md` — thread synthesis
- `chains/prompts/code/code_distill.md` — upper layer synthesis
- `chains/prompts/code/code_recluster.md` — convergence (apex_ready)
- `chains/prompts/code/code_web.md` — cross-reference webbing

### Shared prompts
- `chains/prompts/shared/heal_json.md` — self-healing for malformed JSON
- `chains/prompts/shared/merge_sub_chunks.md` — merge split document chunks

### Reference
- `chains/CHAIN-DEVELOPER-GUIDE.md` — **CURRENT** — pillars, patterns, field reference, failure modes
- `.lab/handback-2026-04-02-mega-session.md` — session handback with full shipped feature list

## Database queries
```sql
-- Pyramid shape
SELECT depth, count(*) FROM pyramid_nodes WHERE slug='SLUG' GROUP BY depth;

-- Pipeline step timing
SELECT step_type, count(*), min(created_at), max(created_at)
FROM pyramid_pipeline_steps WHERE slug='SLUG'
GROUP BY step_type ORDER BY min(created_at);

-- Sample L0 extraction (check density)
SELECT output_json FROM pyramid_pipeline_steps
WHERE slug='SLUG' AND step_type='l0_doc_extract' LIMIT 1;

-- Sample clustering output (check grouping quality)
SELECT output_json FROM pyramid_pipeline_steps
WHERE slug='SLUG' AND step_type='batch_cluster' LIMIT 1;

-- Count unique docs per thread in clustering output (the key quality metric)
-- Pull batch_cluster output_json, parse threads, count unique source_nodes per thread
-- If every thread has 1 doc → 1:1 collapse, clustering failed
-- If threads have 3-15 docs → good grouping

-- Compare against the good build
SELECT depth, count(*) FROM pyramid_nodes WHERE slug='core-selected-docstest8' GROUP BY depth;
-- Result: 0|127, 1|34, 2|17, 3|1 — this is what good looks like
```

DB path: `/Users/adamlevine/Library/Application Support/wire-node/pyramid.db`

## Test corpora
- **Documents**: `/Users/adamlevine/AI Project Files/Core Selected Docs/` (127 docs)
- **Code (small)**: Vibesmithy codebase (34 files) — fast iteration
- **Code (large)**: agent-wire-node codebase (165 files)

## Rules
- Only modify `.yaml` and `.md` files. No Rust.
- Read the Pillars and Patterns in CHAIN-DEVELOPER-GUIDE.md before making changes.
- No prescribed ranges. Let the material decide.
- Dense outputs — reference cards, not rewrites.
- Test on vibesmithy (34 files) before the 127-doc corpus.
- When something doesn't fit, pyramid it — don't truncate, don't skip.

## What success looks like
- Document: L0:127 → L1:15-30 → L2:5-12 → L3:1, zero orphans, build < 10 min
- Code: L0:34 → L1:8-15 → L2:3-6 → L3:1, zero orphans, build < 5 min
- Conversation: forward/reverse/combine → L0 → threads → apex, complete pipeline
- Each thread contains multiple documents/files that genuinely relate to the same concept/subsystem
