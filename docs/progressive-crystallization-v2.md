# Progressive Crystallization v2 — Weight-Propagated Staleness

## Summary

When source material changes, the system doesn't rebuild the pyramid. It asks "what changed?", traces the evidence weights to find which questions are affected, re-answers only those questions, and propagates the impact upward until it attenuates to noise.

## Core Principle

Every edge in the pyramid has a weight and a reason. Those weights are the staleness propagation channel. A high-weight evidence connection means "this source was critical to answering this question." When that source changes, the question's answer is almost certainly stale. A low-weight connection means "this source was marginally relevant." When it changes, the answer probably still holds.

Staleness isn't binary. It's a confidence degradation that flows through weighted edges.

## The Crystallization Loop

### Trigger: A source file changes on disk

The file watcher detects a change to `src/auth.rs`. Rather than re-extracting the entire file from scratch, the system asks a targeted question.

### Step 1: Delta Extraction

**Question**: "What changed in this file relative to what we already extracted?"

The LLM receives:
- The existing L0 node for this file (what we previously extracted)
- The new file content
- The canonical schema (what to look for)

It produces a delta — not a full re-extraction but a DIFF of knowledge:

```json
{
  "node_id": "L0-012",
  "changes": [
    {
      "type": "modified",
      "topic": "Token Validation",
      "was": "validate_token() checks token against sessions table",
      "now": "validate_token() checks token AND checks expiry timestamp, returns error if expired",
      "significance": 0.8
    },
    {
      "type": "added",
      "topic": "Rate Limiting",
      "now": "New rate_limit_check() function, 100 requests per minute per token",
      "significance": 0.6
    }
  ],
  "unchanged": ["UserSession struct", "ct_eq comparison", "role-based access"]
}
```

The delta has its own significance scores. "Token validation changed significantly (0.8)." "Rate limiting is new (0.6)." "Session struct didn't change."

This is cheaper than full re-extraction because the LLM only has to identify what's different, not re-analyze everything.

### Step 2: Update L0 Node

The existing L0 node is updated — not replaced. The unchanged topics stay. The modified topics get their `current` field updated. New topics are added. The L0 node's metadata records the delta: what changed, when, what triggered it.

### Step 3: Trace Evidence Weights

The system queries the evidence table: "Which questions used L0-012 as evidence, and with what weights?"

```
L1-003 ("How does auth work?")       → weight 0.95
L1-007 ("What are the gotchas?")     → weight 0.30
L1-001 ("What are the major parts?") → weight 0.15
```

### Step 4: Compute Staleness Scores

For each question that used this evidence, compute a staleness score:

```
staleness = evidence_weight × max(delta_significance)

L1-003: 0.95 × 0.8 = 0.76  → HIGH — answer is probably wrong
L1-007: 0.30 × 0.8 = 0.24  → LOW — answer probably still holds
L1-001: 0.15 × 0.8 = 0.12  → NEGLIGIBLE — don't bother
```

Apply a threshold. Any question with staleness > 0.5 gets re-answered. Between 0.2 and 0.5 gets flagged for review. Below 0.2 is ignored.

### Step 5: Re-Answer Stale Questions

L1-003 has staleness 0.76. Re-answer it.

The re-answering process is the same as the original build:
- Pre-map: gather all L0 evidence for this question (most connections unchanged, L0-012 is updated)
- Answer: the question is re-asked with the updated evidence
- The new answer reflects the auth changes
- Evidence weights may shift — L0-012 might now have weight 0.90 instead of 0.95 because the rate limiting topic is less relevant to the auth question specifically
- New evidence connections might form — if the rate limiting is relevant to L1-007 ("gotchas"), a new edge gets proposed

### Step 6: Propagate Upward

L1-003's answer changed. Which L2 questions used L1-003 as evidence?

```
L2-001 ("What are the architectural domains?") → weight 0.80
L2-003 ("What connects the domains?")          → weight 0.40
```

Compute cascading staleness:

```
L2-001: 0.80 × L1-003_change_magnitude = 0.80 × 0.6 = 0.48 → FLAGGED for review
L2-003: 0.40 × 0.6 = 0.24 → LOW — ignore
```

L1-003's change magnitude is computed from how different its new answer is from its old answer. If the auth node's orientation changed significantly (new paragraph about expiry checking), magnitude is high. If only an entity was added, magnitude is low.

### Step 7: Propagate Until Attenuation

Continue up the tree. L2-001 might or might not get re-answered depending on whether its staleness exceeds the threshold. If it does, compute impact on L3/apex. If not, stop.

The weights naturally attenuate the signal. A change in one L0 file might invalidate one L1 answer, flag one L2 answer for review, and leave the apex untouched. Only a massive change (new subsystem added, fundamental architecture shift) would propagate all the way to the apex.

## Staleness Thresholds

| Staleness Score | Action | Meaning |
|----------------|--------|---------|
| > 0.5 | **Re-answer immediately** | This question's answer is probably wrong |
| 0.2 — 0.5 | **Flag for review** | Answer might be stale, check next crystallization cycle |
| < 0.2 | **Ignore** | Change is too peripheral to affect this answer |

Thresholds are configurable per pyramid. A security pyramid might use lower thresholds (0.3 for re-answer, 0.1 for flag) because accuracy matters more. A quick-overview pyramid might use higher thresholds (0.7 for re-answer) because approximate is fine.

## Multiple Simultaneous Changes

When multiple files change at once (e.g., a git pull brings 20 changed files):

1. Delta-extract all 20 files in parallel
2. Collect all affected L1 questions with their staleness scores
3. De-duplicate: if L1-003 is stale because of L0-012 AND L0-089, combine the staleness (don't re-answer twice)
4. Re-answer all stale L1 questions in parallel (they're independent)
5. Collect all affected L2 questions, de-duplicate, re-answer
6. Continue upward

Batching is natural. 20 file changes might only invalidate 3 L1 answers and 1 L2 answer. The rest of the pyramid is untouched.

## New Questions as Crystallization Events

When a user asks a new question of the pyramid, it's the same process:

1. **Delta**: "What's new?" → a new question exists that wasn't in the tree before
2. **Trace**: Which existing nodes might serve as evidence for this question?
3. **Pre-map**: Over-include candidate connections from existing nodes
4. **Answer**: Confirm/disconnect with weights
5. **Gaps**: If existing nodes don't cover what the new question needs, trigger targeted re-extraction of specific files with a focused prompt

The new question doesn't rebuild anything. It adds to the pyramid by creating new edges to existing nodes, plus optional new extraction where gaps exist.

## Densification as Crystallization

"Densify this node" is a crystallization event where the delta is "someone wants more detail." The system:

1. Reads the node's current answer and its evidence connections
2. Generates sub-questions that would deepen the answer
3. For each sub-question, checks if existing L0 evidence can answer it
4. Where it can: creates new edges (the evidence was there, just not connected)
5. Where it can't: triggers targeted re-extraction with a focused prompt
6. Re-answers the node with enriched evidence

The node gets denser without rebuilding the pyramid. Just new edges and optionally new extraction.

## Schema: Staleness Tracking

### Delta Log
```sql
CREATE TABLE pyramid_deltas (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  slug TEXT NOT NULL,
  node_id TEXT NOT NULL,              -- which L0 node changed
  trigger TEXT NOT NULL,              -- 'file_change', 'new_question', 'densify', 'manual'
  delta_json TEXT NOT NULL,           -- the change: what was, what is now, significance per topic
  change_magnitude REAL NOT NULL,     -- 0.0-1.0 overall magnitude of this change
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

### Staleness Queue
```sql
CREATE TABLE pyramid_staleness_queue (
  slug TEXT NOT NULL,
  question_id TEXT NOT NULL,          -- which question might be stale
  staleness_score REAL NOT NULL,      -- computed from evidence_weight × delta_significance
  source_delta_id INTEGER NOT NULL,   -- which delta triggered this
  status TEXT NOT NULL DEFAULT 'pending',  -- 'pending', 'processing', 're-answered', 'dismissed'
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  PRIMARY KEY (slug, question_id, source_delta_id)
);
```

### Crystallization Log
```sql
CREATE TABLE pyramid_crystallization_log (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  slug TEXT NOT NULL,
  question_id TEXT NOT NULL,
  trigger_type TEXT NOT NULL,         -- 'staleness', 'new_question', 'densify'
  old_answer_hash TEXT,               -- hash of previous answer (NULL if new)
  new_answer_hash TEXT NOT NULL,      -- hash of new answer
  evidence_added INTEGER DEFAULT 0,   -- new edges created
  evidence_removed INTEGER DEFAULT 0, -- edges disconnected
  evidence_reweighted INTEGER DEFAULT 0, -- edges with changed weights
  change_magnitude REAL NOT NULL,     -- how much the answer changed
  propagated_to TEXT,                 -- JSON array of question_ids affected upstream
  duration_ms INTEGER,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

## Relationship to Current Stale Engine

The current stale engine (`pyramid_stale_engine.rs`) does a simpler version of this:
- Detects file changes via file watcher
- Marks nodes as stale
- Re-runs extraction and synthesis

The v2 crystallization replaces this with:
- Delta extraction instead of full re-extraction
- Weight-propagated staleness instead of binary stale/not-stale
- Threshold-based re-answering instead of full rebuild
- Natural attenuation instead of rebuilding everything above the change
- Explicit evidence tracing instead of implicit parent-child relationships

The migration path: keep the file watcher trigger, replace the stale-mark-and-rebuild logic with delta-extract-and-propagate.

## The Flywheel

Every interaction with the pyramid is a crystallization event:

- **File changes** → delta extraction → weight-propagated re-answering
- **New questions** → new branches using existing evidence → targeted extraction for gaps
- **Densification** → deeper sub-questions → new edges to existing evidence
- **Agent contributions** → new evidence nodes → connected to existing questions
- **FAQ answers** → new question-answer pairs → new edges in the graph

Each event makes the pyramid denser, more accurate, and more connected. The weights get refined with every re-answering. The evidence connections get pruned with every confirmation. The gaps get smaller with every new question.

The pyramid is never rebuilt. It crystallizes.
