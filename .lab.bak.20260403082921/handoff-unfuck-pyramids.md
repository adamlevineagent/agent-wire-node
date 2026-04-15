# Handoff: Fix Pyramid Clustering Quality

## Context
You're working on the Wire Node pyramid build system. The pipeline builds knowledge pyramids from document/code collections. The architecture is solid (sub-chains, batching, dehydration, self-healing parse) but the output quality is broken: clustering produces 1:1 thread ratios (every doc becomes its own thread, no grouping happens).

## The State Right Now
- Document pipeline: L0:127 → L1:127 → apex. No grouping. Every doc is its own "thread."
- Code pipeline: L0:34 → L1:34 → apex. Same problem.
- The pipeline infrastructure works — it builds, produces nodes, reaches apex. The problem is entirely in prompt quality and input signal for the clustering step.

## Key Files
- `chains/defaults/document.yaml` — document pipeline definition
- `chains/defaults/code.yaml` — code pipeline definition
- `chains/prompts/document/doc_cluster.md` — document clustering prompt
- `chains/prompts/document/doc_cluster_merge.md` — document merge prompt
- `chains/prompts/code/code_cluster.md` — code clustering prompt
- `chains/prompts/document/doc_extract.md` — document extraction prompt (produces L0 nodes)
- `chains/prompts/code/code_extract.md` — code extraction prompt
- `chains/prompts/code/code_extract_frontend.md` — frontend-specific extraction
- `chains/CHAIN-DEVELOPER-GUIDE.md` — full reference for YAML fields and primitives

## The Clustering Step
The clustering step is a sub-chain in the YAML:
```yaml
- name: thread_clustering
  primitive: container
  steps:
    - name: batch_cluster
      primitive: classify
      instruction: "$prompts/document/doc_cluster.md"
      for_each: $l0_doc_extract
      item_fields: ["node_id", "headline", "orientation", "topics.name,summary"]
      batch_size: 150
      batch_max_tokens: 50000
      response_schema: { ... threads ... }
    - name: merge_clusters
      instruction: "$prompts/document/doc_cluster_merge.md"
      input:
        batch_results: $batch_cluster
```

`item_fields` projects each L0 extraction down to: node_id, headline, orientation, and topics (name + summary only). This is what the clustering LLM sees per document.

## Known bugs
1. **Code pipeline uses `doc_cluster_merge.md`** for its merge step. This prompt says "documents", uses `D-L0-XXX` example IDs, and allows `unassigned` output — which silently drops code files. **Create `code_cluster_merge.md` with code semantics, `C-L0-XXX` IDs, and ZERO ORPHANS.**

2. **`doc_cluster_merge.md` allows `unassigned`** — for code, this should be prohibited (ZERO ORPHANS). For docs it's debatable but contributing to the 1:1 problem.

## The actual clustering problem
The 1:1 ratio means the LLM looks at 127 projected documents and decides each one is its own distinct concept. This happens because:

- With `topics.name,summary` projection, each doc looks like: `{node_id, headline, orientation (2-3 sentences), topics: [{name, summary}]}`. For 127 docs about the same project, the headlines and topic names are all distinct enough that the LLM doesn't see overlap.
- The clustering prompt says "let the material decide the shape" but doesn't push hard enough for grouping.
- With `response_schema` (structured output), Mercury 2 through OpenRouter is slow (27s per call) and sometimes produces truncated JSON.

## What to investigate
1. **Check what the clustering LLM actually receives.** Pull a batch_cluster pipeline step output and the L0 extractions it was built from. Understand what signal the LLM has.
2. **Compare against successful builds.** `core-selected-docstest8` (L0:127 → L1:34 → L2:17 → L3:1) was a good build. What was different about that run's clustering input?
3. **The extraction prompt changed.** The current `doc_extract.md` uses a "distill/reference card" framing that produces very compact extractions. The `topics.name` and `topics.summary` fields may be too generic to cluster on. Compare L0 extraction quality between the current prompt and earlier versions.

## Rules
- Only modify `.yaml` and `.md` files. No Rust changes.
- No prescribed ranges in prompts ("produce 5-8 threads")
- The pipeline architecture is correct — don't restructure it. Fix the prompts and projection.
- Test on the `vibesmithy` corpus (34 code files, fast iteration) before the 127-doc corpus.
- The test corpus for docs is in `/Users/adamlevine/AI Project Files/Core Selected Docs/` (127 design docs)

## Database queries
```sql
-- Check pyramid shape
SELECT depth, count(*) FROM pyramid_nodes WHERE slug='SLUG' GROUP BY depth;

-- Check pipeline step timing
SELECT step_type, count(*), min(created_at), max(created_at)
FROM pyramid_pipeline_steps WHERE slug='SLUG' GROUP BY step_type ORDER BY min(created_at);

-- Sample an L0 extraction
SELECT output_json FROM pyramid_pipeline_steps WHERE slug='SLUG' AND step_type='l0_doc_extract' LIMIT 1;

-- Sample clustering output
SELECT output_json FROM pyramid_pipeline_steps WHERE slug='SLUG' AND step_type='batch_cluster' LIMIT 1;

-- Compare against good build
SELECT depth, count(*) FROM pyramid_nodes WHERE slug='core-selected-docstest8' GROUP BY depth;
```

DB path: `/Users/adamlevine/Library/Application Support/wire-node/pyramid.db`

## What success looks like
- Document pyramid: L0:127 → L1:15-30 threads → L2:5-12 clusters → L3:1 apex
- Code pyramid: L0:34 → L1:8-15 threads → L2:3-6 clusters → L3:1 apex
- Each thread contains 3-15 documents/files that genuinely relate to the same concept/subsystem
- Zero orphaned files
- Build completes in under 10 minutes on Mercury 2
