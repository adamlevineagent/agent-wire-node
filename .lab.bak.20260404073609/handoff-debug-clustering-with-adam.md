# Debug Session: Why Clustering Produces Singletons

## What to look at together

### 1. L0 Extraction comparison — same document, two different extractions

**Source doc:** `action-chain-system.md` (Action Chain System design spec)

**GOOD extraction** (from `core-selected-docstest8`, the working pyramid):
- File: `.lab/debug-l0-extraction-GOOD.json`
- 5 topics, 3184 chars of content, 35 entities
- Topic names: "Overview & Goals", "Architecture & Execution Flow", "Chain Definition Schema & Variable Resolution", etc.
- Orientation: 648 chars, specific and detailed
- No `summary` field
- Each topic's `current`: ~637 chars avg — dense, specific, full of concrete details

**CURRENT extraction** (from recent build):
- File: `.lab/debug-l0-extraction-sample.json`
- 11 topics, 2219 chars of content, 41 entities
- Topic names: "Purpose & Benefits", "Runtime Architecture", "Chain Definition Schema", etc.
- Orientation: 502 chars
- Has `summary` field
- Each topic's `current`: ~201 chars avg — thin, generic

**The "distill/reference card" prompt produced MORE topics with LESS substance per topic.** The opposite of what we intended. The good extraction had 5 meaty topics. The current one has 11 thin ones. The topic names are more generic ("Purpose & Benefits" vs "Overview & Goals"). This means the clustering LLM sees 11 vague labels per doc instead of 5 specific ones — every doc looks like every other doc.

### 2. What the clustering LLM actually received and returned

**Cluster output:** `.lab/debug-cluster-output-sample.json`
- This is what `batch_cluster` produced from one batch
- Check: how many threads, how many docs per thread, are thread names meaningful?

**Current cluster prompt:** `.lab/debug-current-cluster-prompt.md`
- This is what the LLM was told to do

**Current extraction prompt:** `.lab/debug-current-extract-prompt.md`
- This is what produced the thin L0 extractions

### 3. The key question

The extraction prompt change is likely the root cause. The "distill/reference card" framing tells the LLM to be compact, and it responds by producing many thin generic topics instead of few rich specific ones. The topic names become clustering-useless labels like "Purpose & Benefits" that appear in every design doc.

Is the fix:
- A. Revert to the old extraction prompt style (verbose, 5 topics, rich `current` text)?
- B. Fix the current extraction prompt to produce fewer, richer, more specifically-named topics?
- C. Something else about how the clustering step consumes the extractions?

### Files in .lab/
- `debug-l0-extraction-sample.json` — current extraction (11 thin topics)
- `debug-l0-extraction-GOOD.json` — good extraction (5 rich topics)
- `debug-cluster-output-sample.json` — what clustering produced
- `debug-current-extract-prompt.md` — current extraction prompt
- `debug-current-cluster-prompt.md` — current clustering prompt
- `sample-source-doc.md` — an earlier source doc sample (intelligence-operation-in-a-box.md)
- `sample-l0-extraction.json` — its extraction

### What the good build used differently
- Extraction prompt: the OLD verbose one (before "distill/reference card" rewrite)
- Clustering: single Qwen call with ALL 127 full extractions, response_schema, old assignments schema with topic_index
- No sub-chain, no batching, no dehydrate

### What to investigate
The extraction prompt is the most likely culprit. The clustering machinery (batching, sub-chains, schema) may also contribute, but the extraction quality is the input signal for everything downstream. Fix the signal first.
