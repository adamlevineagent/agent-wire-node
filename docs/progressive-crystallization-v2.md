# Progressive Crystallization v2 — Weight-Propagated Staleness & Supersession

## Summary

When source material changes, the system doesn't rebuild the pyramid. It asks "what changed?", determines whether the change SUPERSEDES beliefs the pyramid currently holds, traces the evidence weights to find which questions are affected, re-answers only those questions with supersession-aware prompts, and propagates the impact upward until it attenuates to noise.

Staleness is "the evidence changed." Supersession is "the old belief is now wrong." Both propagate through the same weighted edges, but supersession demands immediate correction while staleness can tolerate review cycles.

## Core Principles

### Principle 1: The Pyramid Holds Beliefs

Every node above L0 contains beliefs — claims derived from evidence. "Auth uses constant-time comparison." "The credit table has columns (id, balance, user_id)." "OCU is consumption-based." These beliefs were true when the evidence was analyzed. They may not be true now.

### Principle 2: Evidence Changes vs. Belief Supersession

A change to a source file can mean two different things:

**Staleness**: The evidence changed but the beliefs built on it might still hold. A new function was added to `auth.rs` but `validate_token()` still works the same way. The L1 answer about auth is incomplete but not wrong.

**Supersession**: The evidence changed AND it contradicts a belief the pyramid holds. `validate_token()` was renamed to `check_auth()`. Or the sessions table was replaced with a JWT-only approach. Or the credit table's columns changed. The L1 answer about auth now contains claims that are FALSE. Not stale — wrong.

The delta extraction must distinguish these. "Modified" means the belief might be stale. "Contradicts" means the belief is superseded.

### Principle 3: Supersession Propagates as Contradiction, Not Just Signal

Staleness propagates as a confidence score: "this evidence changed, check whether your answer still holds." The receiving question might re-answer and find nothing changed in its conclusions.

Supersession propagates as a specific contradiction: "your answer claims X, but the evidence now says Y." The receiving question MUST update — it contains a false claim. There is no "dismiss" option for a supersession.

## The Crystallization Loop

### Trigger: A source file changes on disk

The file watcher detects a change to `src/auth.rs`.

### Step 1: Delta Extraction

**Question**: "What changed in this file relative to what we already extracted? For each change, is it an ADDITION (new capability), a MODIFICATION (same capability, different behavior), or a SUPERSESSION (old claim is now false)?"

The LLM receives:
- The existing L0 node for this file (what we previously extracted)
- The new file content
- The canonical schema (what to look for)

It produces a delta:

```json
{
  "node_id": "L0-012",
  "changes": [
    {
      "type": "supersession",
      "topic": "Token Validation",
      "old_belief": "validate_token() checks token against sessions table using constant-time comparison",
      "new_truth": "validate_token() now checks JWT signature directly, sessions table is no longer used for token validation",
      "supersedes_entities": ["table: sessions", "function: ct_eq()"],
      "significance": 0.95
    },
    {
      "type": "addition",
      "topic": "Rate Limiting",
      "new_truth": "New rate_limit_check() function, 100 requests per minute per token",
      "significance": 0.6
    },
    {
      "type": "modification",
      "topic": "User Session",
      "old_belief": "UserSession has fields: user_id, role, permissions",
      "new_truth": "UserSession now also has token_type field (jwt|session)",
      "significance": 0.4
    }
  ],
  "unchanged": ["role-based access patterns", "RBAC enforcement on endpoints"]
}
```

The critical distinction: the token validation change is a SUPERSESSION — the old belief ("uses sessions table with ct_eq") is now false. The rate limiting is an ADDITION — nothing was wrong, there's just something new. The user session change is a MODIFICATION — the old belief was incomplete, not wrong.

### Step 2: Update L0 Node

The existing L0 node is updated with the new truth. But the OLD beliefs are not deleted — they're marked as superseded with a reference to what replaced them and when:

```json
{
  "topic": "Token Validation",
  "current": "validate_token() checks JWT signature directly...",
  "superseded": [
    {
      "was": "validate_token() checks token against sessions table using constant-time comparison",
      "superseded_by": "delta-2026-03-25-001",
      "date": "2026-03-25",
      "reason": "Auth system migrated from session-based to JWT-based validation"
    }
  ]
}
```

The supersession history is preserved. The pyramid remembers what it used to believe and why it changed. This is the audit trail.

### Step 3: Trace Evidence Weights AND Belief Dependencies

The system queries two things:

**Evidence weights** (same as before):
```
L1-003 ("How does auth work?")       → weight 0.95
L1-007 ("What are the gotchas?")     → weight 0.30
L1-001 ("What are the major parts?") → weight 0.15
```

**Belief dependencies** (new): Which upstream nodes contain claims that reference the superseded entities?

```sql
-- Find all nodes whose content references "sessions" table or "ct_eq()"
SELECT question_id, node_content
FROM pyramid_nodes
WHERE slug = ?
AND (
  node_content LIKE '%sessions table%'
  OR node_content LIKE '%ct_eq%'
  OR entities_json LIKE '%"sessions"%'
  OR entities_json LIKE '%"ct_eq"%'
)
```

This might find:
```
L1-003: mentions "validates against sessions table" in its orientation       → SUPERSEDED BELIEF
L1-007: mentions "ct_eq for constant-time comparison" as a gotcha           → SUPERSEDED BELIEF
L2-001: mentions "session-based auth" in its architecture summary           → SUPERSEDED BELIEF
L3-000 (apex): mentions "constant-time token validation" in security topic  → SUPERSEDED BELIEF
```

The belief trace can find supersession impacts that the weight-based trace would MISS. L2-001 might have weight 0.1 to L0-012 (low staleness score, would be ignored) but it contains the specific claim "session-based auth" which is now false. The belief dependency trace catches this.

### Step 4: Classify Impact

For each affected question, classify the impact:

| Question | Weight Staleness | Belief Supersession | Action |
|----------|-----------------|-------------------|--------|
| L1-003 | 0.95 × 0.95 = 0.90 | YES — claims "sessions table" | **Mandatory re-answer** |
| L1-007 | 0.30 × 0.95 = 0.29 | YES — claims "ct_eq" | **Mandatory re-answer** (supersession overrides low staleness) |
| L1-001 | 0.15 × 0.95 = 0.14 | No | Ignore (low staleness, no supersession) |
| L2-001 | indirect | YES — claims "session-based auth" | **Mandatory re-answer** (supersession found via belief trace) |
| L3-000 | indirect | YES — claims "constant-time validation" | **Mandatory re-answer** |

Supersession OVERRIDES staleness thresholds. L1-007 would normally be "flagged for review" at staleness 0.29. But it contains a false claim, so it must be re-answered regardless of the staleness score.

The belief trace catches L2-001 and L3-000 which the weight-based trace would have missed entirely or dismissed as low-impact.

### Step 5: Re-Answer with Supersession Context

Each mandatory re-answer gets a supersession-aware prompt. The LLM receives:

1. The question
2. The existing answer (what the node currently says)
3. The updated evidence
4. **Explicit supersession notices**: "WARNING: Your current answer claims 'validates against sessions table.' This is no longer true. The evidence now says 'validates JWT signature directly.' You MUST update this claim. Any downstream implications of this change should be noted."

The supersession notice is not optional context — it's a directive. The LLM cannot ignore it or decide the old claim still holds. The evidence has changed and the specific claim is identified as false.

The re-answer produces:
```json
{
  "headline": "Auth Token Lifecycle",
  "orientation": "The system uses JWT tokens validated by signature verification...",
  "topics": [...],
  "evidence": [...],
  "supersessions_applied": [
    {
      "old_claim": "validates against sessions table using ct_eq",
      "new_claim": "validates JWT signature directly, sessions table used only for refresh tokens",
      "source_delta": "delta-2026-03-25-001"
    }
  ]
}
```

The node records which supersessions it applied. This is the audit trail — you can trace exactly which beliefs changed, when, why, and what triggered the change.

### Step 6: Cascade Supersession Check

After re-answering, check: did the re-answer itself produce new supersessions?

L1-003's old answer said "sessions table is the auth backbone." Its new answer says "JWT is the auth backbone, sessions table is only for refresh." Any L2 node that quoted L1-003's old claim now has a superseded belief.

Run the belief dependency trace AGAIN on the re-answered nodes. This catches cascading supersessions — a change in L0 that supersedes a claim in L1, which causes the L1 re-answer to supersede a claim in L2, which causes...

### Step 7: Propagate Until Resolution

The cascade continues until either:
- No more superseded beliefs are found (all claims are consistent with current evidence)
- The apex has been re-answered (the change propagated all the way up)

For staleness (non-supersession changes), attenuation still applies — the signal weakens through weights and eventually falls below threshold. For supersession, there is NO attenuation — a false claim must be corrected regardless of how far from the source it sits.

## Two Propagation Channels

| | Staleness | Supersession |
|---|----------|-------------|
| **Signal** | "Evidence changed" | "A specific belief is now false" |
| **Propagation** | Through evidence weights | Through belief dependency (entity/claim matching) |
| **Attenuation** | Yes — weight × significance decreases each layer | No — a false claim is false regardless of distance |
| **Threshold** | Configurable (0.5 re-answer, 0.2 flag) | None — always mandatory |
| **Dismissable** | Yes — reviewer can say "answer still holds" | No — the claim is factually wrong |
| **Action** | Re-answer (answer may or may not change) | Re-answer with explicit correction directive |

Both channels run simultaneously on every delta. A single file change might produce staleness signals (additions, modifications) AND supersession signals (contradictions). The staleness signals attenuate normally. The supersession signals propagate until every false claim is corrected.

## Supersession History

Every node maintains a supersession log:

```json
{
  "supersession_history": [
    {
      "date": "2026-03-25",
      "old_claim": "Auth uses session-based validation against sessions table",
      "new_claim": "Auth uses JWT signature validation, sessions table for refresh only",
      "triggered_by": "delta-2026-03-25-001 (src/auth.rs changed)",
      "cascade_depth": 2,
      "nodes_affected": ["L1-003", "L1-007", "L2-001", "L3-000"]
    }
  ]
}
```

This serves multiple purposes:
- **Audit trail**: When did the pyramid's understanding change and why?
- **Confidence signal**: A node with many supersessions has been volatile — its current claims might change again
- **Learning signal**: Frequent supersessions in one area suggest the source material is actively evolving — increase monitoring frequency
- **Temporal understanding**: The pyramid doesn't just know what's true NOW — it knows what WAS true and when it changed

## Staleness Thresholds

| Staleness Score | Supersession? | Action |
|----------------|--------------|--------|
| Any | YES | **Mandatory re-answer with correction directive** |
| > 0.5 | No | **Re-answer** — evidence changed significantly |
| 0.2 — 0.5 | No | **Flag for review** — might be stale |
| < 0.2 | No | **Ignore** — change is too peripheral |

Supersession always wins. A node with staleness 0.05 (normally ignored) that contains a superseded belief MUST be re-answered.

## Multiple Simultaneous Changes

When multiple files change at once (e.g., a git pull brings 20 changed files):

1. Delta-extract all 20 files in parallel
2. Collect ALL supersession signals across all deltas
3. Collect all staleness signals
4. Run belief dependency trace for ALL superseded entities at once (one query, not 20)
5. De-duplicate: if L1-003 is affected by supersessions from L0-012 AND L0-089, merge the correction directives
6. Re-answer all mandatory (supersession) questions first, in parallel
7. Check for cascading supersessions from re-answers
8. Re-answer staleness-triggered questions in parallel
9. Continue upward until all supersessions are resolved and staleness has attenuated

Supersession resolution takes priority over staleness processing. A node that needs both a supersession correction and a staleness re-answer gets BOTH in one pass.

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

## Schema

### Delta Log
```sql
CREATE TABLE pyramid_deltas (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  slug TEXT NOT NULL,
  node_id TEXT NOT NULL,              -- which L0 node changed
  trigger TEXT NOT NULL,              -- 'file_change', 'new_question', 'densify', 'manual'
  delta_json TEXT NOT NULL,           -- changes with type (addition/modification/supersession)
  change_magnitude REAL NOT NULL,     -- 0.0-1.0 overall magnitude
  has_supersessions BOOLEAN NOT NULL DEFAULT FALSE,
  superseded_entities TEXT,           -- JSON array of entity names that are now false
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

### Supersession Log
```sql
CREATE TABLE pyramid_supersessions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  slug TEXT NOT NULL,
  question_id TEXT NOT NULL,          -- which node had a false belief corrected
  old_claim TEXT NOT NULL,            -- what the node used to say
  new_claim TEXT NOT NULL,            -- what the node says now
  source_delta_id INTEGER NOT NULL,   -- which delta triggered this
  cascade_depth INTEGER NOT NULL,     -- how many layers from the original change
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

### Staleness Queue
```sql
CREATE TABLE pyramid_staleness_queue (
  slug TEXT NOT NULL,
  question_id TEXT NOT NULL,
  staleness_score REAL NOT NULL,
  is_supersession BOOLEAN NOT NULL DEFAULT FALSE,  -- supersessions skip threshold check
  superseded_claims TEXT,             -- JSON: specific claims that are false (NULL for pure staleness)
  source_delta_id INTEGER NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending',
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
  trigger_type TEXT NOT NULL,         -- 'staleness', 'supersession', 'new_question', 'densify'
  old_answer_hash TEXT,
  new_answer_hash TEXT NOT NULL,
  supersessions_applied INTEGER DEFAULT 0,
  evidence_added INTEGER DEFAULT 0,
  evidence_removed INTEGER DEFAULT 0,
  evidence_reweighted INTEGER DEFAULT 0,
  change_magnitude REAL NOT NULL,
  propagated_to TEXT,                 -- JSON array of question_ids affected upstream
  duration_ms INTEGER,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

## Relationship to Current Stale Engine

The current stale engine (`pyramid_stale_engine.rs`) does a simpler version:
- Detects file changes via file watcher
- Marks nodes as stale (binary)
- Re-runs extraction and synthesis (full rebuild)

The v2 crystallization replaces this with:
- Delta extraction instead of full re-extraction
- Supersession detection — distinguishing "changed" from "contradicted"
- Two propagation channels: weight-based (attenuating) and belief-based (non-attenuating)
- Threshold-based re-answering for staleness, mandatory re-answering for supersession
- Explicit correction directives in re-answer prompts
- Supersession history as audit trail
- Natural attenuation for staleness, forced propagation for contradictions

The migration path: keep the file watcher trigger, replace stale-mark-and-rebuild with delta-extract → classify-impact → trace-beliefs → re-answer-with-directives → cascade.

## The Flywheel

Every interaction with the pyramid is a crystallization event:

- **File changes** → delta extraction → supersession detection → weight-propagated re-answering
- **New questions** → new branches using existing evidence → targeted extraction for gaps
- **Densification** → deeper sub-questions → new edges to existing evidence
- **Agent contributions** → new evidence nodes → connected to existing questions
- **FAQ answers** → new question-answer pairs → new edges in the graph

Each event makes the pyramid denser, more accurate, and more connected. The weights get refined with every re-answering. The evidence connections get pruned with every confirmation. The supersession history grows with every correction. The gaps get smaller with every new question.

The pyramid is never rebuilt. It crystallizes — and it remembers every belief it ever held and why it changed.
