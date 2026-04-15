# Prompt Quality Session — L0 Extraction

## What you're doing
Reviewing the L0 extraction quality for the document pyramid pipeline. The extraction prompt produces too many topics with too many output tokens, causing downstream failures (clustering batches hit Mercury 2's 48K output cap).

## Key principle
Input tokens are cheap and fast (prefill). Output tokens are expensive, slow, and propagate bloat up the chain. The pipeline should send RICH inputs and demand DENSE outputs. Currently it's doing the opposite — aggressively compressing inputs while letting outputs balloon.

## The other key principle
No prescribed ranges. Don't say "2-5 topics" or "3-5 sentences." Let the material decide. But DO demand density — every sentence earns its place, every topic is something someone MUST understand.

## Files to compare

### Source document (13KB)
`/Users/adamlevine/AI Project Files/agent-wire-node/.lab/sample-source-doc.md`
This is `architecture/intelligence-operation-in-a-box.md` — a strategic exploration doc from March 2026.

### L0 extraction produced from it
`/Users/adamlevine/AI Project Files/agent-wire-node/.lab/sample-l0-extraction.json`
This is what the current `doc_extract.md` prompt produced. It has 16 topics. That's too many — a 13KB exploration doc doesn't have 16 dimensions of understanding.

### The extraction prompt
`/Users/adamlevine/AI Project Files/agent-wire-node/chains/prompts/document/doc_extract.md`
This is what you're improving.

### The thread synthesis prompt (downstream consumer of L0s)
`/Users/adamlevine/AI Project Files/agent-wire-node/chains/prompts/document/doc_thread.md`

### The clustering prompt (downstream consumer of L0s)
`/Users/adamlevine/AI Project Files/agent-wire-node/chains/prompts/document/doc_cluster.md`

### Token budget context
- Mercury 2: 128K context, 48K max output
- L0 extraction output target: ~500-1000 tokens per doc (not 3000)
- 127 docs in the test corpus
- Clustering receives projected L0s: `item_fields: ["node_id", "headline", "orientation", "topics.name"]`
- Thread synthesis receives FULL L0s for all docs in a thread
- A thread with 10 docs × 3000 tokens = 30K input for synthesis. At 1000 tokens per doc, that's 10K — much more headroom

## What to evaluate
1. Read the source doc. What are the 2-4 things someone MUST understand from it?
2. Read the L0 extraction. Does it capture those things? Does it capture a bunch of stuff that doesn't matter?
3. How should the extraction prompt change to produce dense, high-signal output?
4. What's the right output shape — fewer topics with richer content, or more topics with tighter content?

## Rules
- Only modify `.yaml` and `.md` files. No Rust changes.
- No prescribed ranges in prompts ("2-5 topics", "3-5 sentences")
- Density over coverage. The extraction is NOT a summary of the document. It's the things someone NEEDS TO UNDERSTAND.
- Changes to `doc_extract.md` must be tested by looking at what downstream steps actually need from L0 nodes

## What downstream steps need from L0s
- **Clustering** needs: what is this doc about? (headline, orientation, topic names)
- **Thread synthesis** needs: what does this doc SAY? (decisions, findings, specifics, temporal context)
- **Webbing** needs: what does this doc reference? (entities, cross-references)

The extraction must serve all three, but densely. Not 16 topics restating the document — the key claims, decisions, and findings that a reader or agent needs.
