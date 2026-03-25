# Question-Driven Pyramid — v2 Architecture

## Summary

A pyramid is built in two phases: **architect** (top-down question decomposition) and **answer** (bottom-up extraction with horizontal pre-mapping at every layer). The apex question defines the shape. The source material fills it in. Every connection is justified, weighted, and prunable.

## Phase 1: Architecture (top-down, no source material)

### Input
- One apex question (e.g., "What should I know about this codebase?")
- A folder map (file names, extensions, directory structure — no content)

### Process

The system decomposes the apex question recursively. Each decomposition is a single LLM call that:
1. Takes the parent question + the folder map
2. Produces sub-questions
3. Declares whether each sub-question needs further decomposition or is answerable directly from source files

**Call 1 — Apex → L1:**
```
Input: "What should I know about this codebase?" + folder map
Output:
  L1-000: "What is this system and what problem does it solve?"
  L1-001: "What are its major components and how are they organized?"
  L1-002: "How do the components communicate with each other?"
  L1-003: "What does it store and how?"
  L1-004: "What will surprise or trip up a new developer?"
  → All need further decomposition
```

**Call 2 — L1 → L2** (one call, all L1 questions decompose together so they can see each other and avoid overlap):
```
Input: All L1 questions + folder map
Output:
  Under L1-001 "What are its major components?":
    L2-000: "What is the frontend and how is it structured?"
    L2-001: "What is the backend and what services does it provide?"
    L2-002: "What is the pyramid engine and how does it process content?"
    → L2-000 and L2-001 need further decomposition
    → L2-002 is granular enough — answer from source files

  Under L1-002 "How do they communicate?":
    L2-003: "What IPC channels exist between frontend and backend?"
    L2-004: "What HTTP endpoints does the backend expose?"
    → Both granular enough — answer from source files

  Under L1-003 "What does it store?":
    L2-005: "What database tables exist and what are their schemas?"
    L2-006: "What config files and env vars control behavior?"
    → Both granular enough

  ...etc
```

**Call 3 — L2 → L3** (if any L2 questions need further decomposition):
```
Input: Remaining L2 questions + folder map
Output:
  Under L2-000 "What is the frontend?":
    L3-000: "What is the app shell and routing?"
    L3-001: "What are the main views/pages?"
    L3-002: "How is state managed?"
    → All granular enough
  ...etc
```

Decomposition stops when every leaf question is declared "answerable from source files."

### Output: The Question Tree

A complete tree of questions. Every node is a question. Every edge is "to answer this parent, I need these children answered first." The leaf questions define what L0 extraction must capture.

### Output: Canonical Schema

Alongside the question tree, the system produces a schema — what fields matter for this pyramid. Derived from the apex question and the decomposition.

For "What should a developer know?":
```yaml
schema:
  node_fields:
    - headline        # 2-6 word label
    - orientation     # 3-5 sentence answer to this node's question
    - topics          # sub-aspects with entities, decisions, corrections
    - evidence        # which child nodes contributed, with weights
  entity_types:
    - function        # function/method names
    - type            # struct/interface/class names
    - table           # database table with columns
    - endpoint        # HTTP/IPC/WebSocket endpoint
    - env_var         # environment variable with purpose
    - gotcha          # surprising behavior or hidden dependency
  topic_fields:
    - name            # aspect label
    - current         # current state (3-5 sentences)
    - entities        # specific named things
    - decisions       # what was decided and why
    - corrections     # what changed (wrong → right)
```

For "What are the security vulnerabilities?":
```yaml
schema:
  entity_types:
    - trust_boundary  # where trust changes
    - auth_check      # where authentication is verified
    - input_source    # where untrusted input enters
    - validation      # where input is validated (or isn't)
    - secret          # hardcoded credential or key
    - attack_surface  # exposed endpoint without adequate protection
  topic_fields:
    - name
    - current
    - entities
    - severity        # how bad if exploited
    - mitigation      # what currently prevents exploitation
    - gaps            # what's missing
```

Each question node gets the RELEVANT SUBSET of the schema in its prompt. L0 extraction for a security pyramid looks for trust boundaries and attack surfaces. L0 extraction for a developer onboarding pyramid looks for entry points and gotchas. Same files, different extraction, because the schema told the LLM what matters.

## Phase 2: L0 Extraction (shaped by the question tree)

### Input
- The leaf questions from the question tree
- The canonical schema
- The source files

### Process

The system collects all leaf questions and identifies what each one needs from source files. It builds a per-file extraction prompt that includes every aspect any leaf question might need from that file.

Example: if leaf questions include "What IPC channels exist?" and "What tables does the backend use?" and "What will surprise a developer?", then every file's L0 extraction prompt says:
- Note any IPC invoke commands or event listeners
- Note any database table reads/writes with column names
- Note any surprising behavior, hidden dependencies, or non-obvious side effects

The L0 extraction runs per-file, parallel, using the merged extraction targets. Each L0 node captures everything any question in the tree might need.

### Output
L0 nodes with topics, entities, and metadata shaped by the question tree's needs.

## Phase 3: Bottom-Up Answering with Horizontal Pre-Mapping

This is the core build loop. It runs once per layer, from the deepest leaf questions up to the apex.

### For each layer (starting at the deepest):

**Step A — Horizontal Pre-Mapping**

A single LLM call reads:
- All questions at this layer
- All completed nodes from the layer below (L0 nodes for the first pass, L1 nodes for the second, etc.)
- The schema

It produces candidate connections: "Question L1-003 ('how does auth work?') should draw from these lower nodes: L0-012 (weight: likely), L0-045 (weight: strong), L0-067 (weight: maybe), L0-089 (weight: strong)."

The pre-mapping intentionally OVER-INCLUDES. Better to give a question too many candidates than to miss a relevant one. The answering step will prune.

**Step B — Vertical Answering (parallel)**

Each question at this layer gets answered. The prompt contains:
- The question itself
- The relevant schema fields
- The pre-mapped candidate nodes from below (with their full content)
- Instruction: "Answer this question using the provided evidence. For each candidate connection, report: KEEP (with weight 0.0-1.0) or DISCONNECT (false positive). You may also flag connections you wish you had but weren't provided."

The answer comes back as:
```json
{
  "headline": "Auth Token Lifecycle",
  "orientation": "The system uses JWT tokens issued by Supabase, validated via constant-time comparison against the sessions table...",
  "topics": [...],
  "evidence": [
    {"node": "L0-012", "status": "keep", "weight": 0.95, "reason": "Contains validate_token() implementation"},
    {"node": "L0-045", "status": "keep", "weight": 0.7, "reason": "Defines UserSession struct used by auth"},
    {"node": "L0-067", "status": "disconnect", "reason": "Mentions auth in a comment but has no auth logic"},
    {"node": "L0-089", "status": "keep", "weight": 0.85, "reason": "Token refresh and expiry handling"}
  ],
  "missing": ["Would benefit from a node covering the Supabase configuration"]
}
```

Every connection is now justified, weighted, and either confirmed or pruned.

**Step C — Post-Answering Reconciliation**

After all questions at this layer are answered:
- Collect all "missing" flags — these are gaps the question tree didn't anticipate
- Collect all weights — these define the strength of every edge in the pyramid
- Identify any L0 nodes that NO question claimed — these are orphans that might indicate a missing question
- Optionally: generate a supplementary question to cover orphans and gaps

### Repeat for next layer up

L1 is now complete. Move to L2:
- Pre-map: which L1 nodes are relevant to each L2 question?
- Answer: each L2 question uses its pre-mapped L1 evidence
- Reconcile: check for gaps, orphans, missing questions

Continue until the apex question is answered.

## What This Produces

### A pyramid where every edge is justified

Current pyramids: "L1-003 contains L0-012 because the clustering algorithm assigned them to the same group."

Question-driven pyramids: "L1-003 drew from L0-012 with weight 0.95 because L0-012 contains the validate_token() implementation which directly answered the question 'how does auth work?'"

### A pyramid where depth = question complexity

A simple question ("What is this?") produces a shallow pyramid — maybe 2 layers. A complex question ("What are all the security vulnerabilities and how are they mitigated?") produces a deep pyramid — maybe 5 layers with specialized branches.

### A pyramid where shape = reader's needs

The same codebase produces different pyramids for different questions. The security pyramid is deep and narrow (3 branches, 4 layers deep). The onboarding pyramid is wide and shallow (8 branches, 2 layers). The architecture pyramid is balanced (5 branches, 3 layers). The shape is driven by the question, not by the data.

### A pyramid where orphans are visible

If L0-077 wasn't claimed by any question, that's a signal: either the question tree missed something, or that file genuinely isn't relevant to the apex question. Both are valuable information. The system can surface orphans: "These 5 files weren't relevant to any question in the tree. They cover: logging utilities, test fixtures, build scripts."

### A pyramid where gaps are visible

If L1-003's answer says "missing: would benefit from a node covering Supabase configuration," that's a signal the question tree should have asked about external service configuration. The system can propose: "Add question L2-007: 'How are external services configured and authenticated?'"

## Schema Changes

### Question Tree Table
```sql
CREATE TABLE pyramid_question_tree (
  slug TEXT NOT NULL,
  question_id TEXT NOT NULL,        -- e.g., "L1-003"
  parent_question_id TEXT,          -- NULL for apex
  question TEXT NOT NULL,           -- the natural language question
  depth INTEGER NOT NULL,
  is_leaf BOOLEAN NOT NULL DEFAULT FALSE,
  decomposition_notes TEXT,         -- why this question exists
  PRIMARY KEY (slug, question_id)
);
```

### Evidence Table (replaces implicit children arrays)
```sql
CREATE TABLE pyramid_evidence (
  slug TEXT NOT NULL,
  question_id TEXT NOT NULL,        -- the question that used this evidence
  evidence_node_id TEXT NOT NULL,   -- the node that provided evidence
  status TEXT NOT NULL,             -- 'keep' or 'disconnect'
  weight REAL NOT NULL DEFAULT 0.5, -- 0.0-1.0 relevance
  reason TEXT,                      -- why this evidence was relevant
  phase TEXT NOT NULL,              -- 'pre-map' or 'confirmed'
  PRIMARY KEY (slug, question_id, evidence_node_id)
);
```

### Gaps Table
```sql
CREATE TABLE pyramid_gaps (
  slug TEXT NOT NULL,
  question_id TEXT NOT NULL,        -- which question identified the gap
  description TEXT NOT NULL,        -- what's missing
  suggested_question TEXT,          -- optional: proposed question to fill it
  resolved BOOLEAN NOT NULL DEFAULT FALSE,
  PRIMARY KEY (slug, question_id, description)
);
```

### Orphans View
```sql
CREATE VIEW pyramid_orphans AS
SELECT n.slug, n.id, n.headline
FROM pyramid_nodes n
WHERE n.depth = 0
AND NOT EXISTS (
  SELECT 1 FROM pyramid_evidence e
  WHERE e.slug = n.slug
  AND e.evidence_node_id = n.id
  AND e.status = 'keep'
);
```

## What Changes from Current Architecture

| Aspect | Current (v2) | Question-Driven (v3) |
|--------|-------------|---------------------|
| Design direction | Bottom-up: data → clusters → apex | Top-down: question → decomposition → extraction |
| L0 extraction | Generic per content type | Shaped by leaf questions |
| Grouping | LLM clusters by topic similarity | Questions define groups; pre-mapping assigns evidence |
| Connections | Clustering assigns, children array stores | Pre-mapping proposes, answering confirms with weights |
| Edge justification | None — "the algorithm put them together" | Every edge has a reason and weight |
| Depth | Fixed by layer count | Determined by question complexity |
| Shape | Same for all pyramids of a content type | Different per apex question |
| Orphans | Silent — unassigned L0s disappear | Explicit — surfaced as "not relevant to any question" |
| Gaps | Invisible | Explicit — surfaced as "missing evidence" |
| Schema | Fixed per content type | Generated per apex question |
| YAML | Defines pipeline steps | Generated from question decomposition |

## Implementation Path

### Step 1: Question Decomposer
- Input: apex question + folder map
- Output: question tree + canonical schema
- Implementation: 2-4 LLM calls, no source file reading
- Can be tested immediately with any folder

### Step 2: Schema-Shaped L0 Extraction
- Input: leaf questions + schema + source files
- Output: L0 nodes shaped by schema
- Implementation: modify extract prompt to include schema-relevant extraction targets
- Backward compatible: current extract prompts are just a fixed schema

### Step 3: Horizontal Pre-Mapper
- Input: all questions at depth N + all nodes at depth N-1
- Output: candidate connections with initial weights
- Implementation: single LLM call per layer, new primitive or classify variant

### Step 4: Evidence-Weighted Answering
- Input: question + candidate nodes + schema
- Output: answer node + confirmed/disconnected evidence with weights + gaps
- Implementation: modify synthesize primitive to return evidence array

### Step 5: Reconciliation
- Input: all answers at depth N + all evidence + all gaps
- Output: orphan report, gap report, optional supplementary questions
- Implementation: mechanical (SQL queries) + optional LLM call for gap analysis

Each step can be built and tested independently. Step 1 works without steps 2-5. Steps 2-5 can fall back to current behavior (generic extraction, clustering-based grouping) while being built.
