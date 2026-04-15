# Handoff: L0 Extraction Review — doc-ct4

## What happened

Build `doc-ct4` completed L0 extraction (127/127, 0 failures) and is currently running post-L0 steps (webbing/clustering). This is the first build with:
- Condensation ladder (`current`, `current_dense`, `current_core`, `summary`) in the extraction prompt
- `#[serde(flatten)]` on Topic struct (extra fields captured automatically)
- Adaptive dehydration live in chain executor

## The problem to investigate

**Two L0 extractions hit Mercury 2's output token cap (finish reason "length"):**

1. **19,909 prompt → 46,662 completion** — large doc chunk (split-merge path)
2. **5,252 prompt → 47,358 completion** — small doc, only 5K prompt tokens, still overflowed

This is NOT a new problem introduced by the condensation ladder. It's a pre-existing issue where Mercury 2 sometimes generates exhaustively until hitting the 48K output cap. Most extractions finish correctly (finish reason "stop") with reasonable ratios.

**Successful extraction example:** 9,981 prompt → 1,517 completion (finish reason "stop"). That's a 6.5:1 input:output ratio — correct behavior.

## Build results at a glance

- **Slug:** `doc-ct4`
- **Source:** `/Users/adamlevine/AI Project Files/Core Selected Docs` (127 docs)
- **L0 nodes:** 127 total, 124 populated, 3 empty distilled
- **Avg distilled (orientation) length:** 485.5 chars (populated nodes)
- **Max distilled length:** 728 chars
- **Build still running** at time of handoff (~8 min elapsed, in post-L0 phase)

### Empty nodes (0-length distilled)

| Node ID | Headline | Source Size | Topics? |
|---------|----------|-------------|---------|
| D-L0-030 | Wire Deck Retro Summary | 21,398 chars | [] (empty) |
| D-L0-052 | Self-Hosted Supabase Guide | 5,577 chars | [] (empty) |
| D-L0-101 | Wire Technical Spec | 44,123 chars | 21,749 chars of topics! |

D-L0-101 is the interesting case — topics parsed fine (10 topics, very detailed) but `distilled` is empty. This is likely a length-truncated response where `headline` and `topics` were emitted but `orientation` was either at the end of the JSON and got cut, or the self-healer recovered topics but not orientation.

D-L0-030 and D-L0-052 are fully empty — the extraction failed completely or returned unparseable output.

### Topic count distribution

| Topics per node | Count |
|----------------|-------|
| 5 | 9 |
| 6 | 25 |
| 7 | 19 |
| 8 | 19 |
| 9 | 17 |
| 10 | 13 |
| 11 | 4 |
| 12 | 9 |
| 13 | 6 |
| 14 | 1 |
| 17 | 1 |
| 19 | 1 |

Median is around 7-8 topics. The 17 and 19-topic nodes are probably the large docs. This is richer than the pre-fix extraction (which averaged ~5 topics) but the question is whether it's *too* rich — more topics means more output tokens.

### Sample good extraction (D-L0-003, Auto-Stale Implementation Plan)

```
Orientation (728 chars): "This v4.2 design extends the v3 delta-chain with a fully-automated pyramid-freshness engine..."
Topics: 13 topics, each with current/current_dense/current_core/summary/entities
Topic names: "Recursive Balanced Fan-Out Pipeline", "Tombstone Supersession & Deterministic Parent Re-Parenting", etc.
```

The condensation ladder IS working — topics have all three levels populated. Topic names are specific (not generic). Content is dense and specific.

## What to investigate

1. **Look at the actual OpenRouter responses** for the two "length" truncations. What did the LLM actually produce? Is it writing exhaustively, or is the document legitimately complex?

2. **Compare token ratios** across all 127 extractions. Are most in the healthy 3:1-10:1 range with just 2 outliers? Or is there a tail of borderline cases?

3. **The 3 empty nodes** — what went wrong? Self-healing should have caught parse failures. Check if these were also "length" truncations that produced un-parseable partial JSON.

4. **Topic counts of 12+** — are these genuinely multi-dimensional docs, or is the LLM over-splitting? The prompt says "Ask: Would removing this topic leave a gap in understanding? If no, it doesn't deserve to be a topic." Is Mercury 2 following that?

## Current extraction prompt

Located at: `chains/prompts/document/doc_extract.md`

**Note:** I made a small edit during this session that should be reverted — I tightened lines 2-3 and the `current` ladder description based on a wrong diagnosis (I thought the condensation ladder was causing the bloat, but Adam corrected that this is a pre-existing problem). The edit was:

- Line 2-3: Changed "If your extraction approaches the length..." to "A reference card is a fraction of the source..."
- Line 28: Changed `current` description from "Everything someone needs to understand this topic without reading the source" to "still a distillation, not a rewrite. A dense paragraph, not an essay."

These changes were deployed to `~/Library/Application Support/wire-node/chains/prompts/document/doc_extract.md`. Whether to keep or revert is a judgment call — they're not wrong, they're just not the fix for the length problem.

## How to query the build data

```bash
TOKEN="vibesmithy-test-token"
BASE="http://localhost:8765"

# Build status
curl -s -H "Authorization: Bearer $TOKEN" "$BASE/pyramid/doc-ct4/build/status"

# Get a specific node
curl -s -H "Authorization: Bearer $TOKEN" "$BASE/pyramid/doc-ct4/node/D-L0-003"

# DB queries
sqlite3 ~/Library/Application\ Support/wire-node/pyramid.db

# All L0 nodes with sizes
SELECT id, headline, length(distilled), length(topics)
FROM pyramid_nodes WHERE slug='doc-ct4' AND depth=0 AND superseded_by IS NULL
ORDER BY length(topics) DESC;

# Source chunk for a node (chunk_index = number from node ID, e.g., D-L0-003 → 3)
SELECT length(content) FROM pyramid_chunks WHERE slug='doc-ct4' AND chunk_index=3;
```

## OpenRouter logs

The build ran at approximately 2026-04-03 20:25-20:29 UTC. All calls are from `inception/mercury-2`. The two "length" finish reasons are the ones to examine — they're the largest completion token counts in the batch.
