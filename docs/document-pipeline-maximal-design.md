# Document Pipeline — Maximal Design

## How Documents Differ From Code

Code files are atomic, self-contained, and always represent current truth. Documents are none of these things:

- **Documents evolve**: A design doc from February gets revised in March. An audit in week 1 finds bugs that a bugfix doc in week 2 resolves. The LATEST document on a subject is the truth, earlier ones are history.
- **Documents overlap**: Five different docs might discuss auth — the design spec, the audit, the implementation plan, the bugfix report, and the strategy memo. Each adds a different angle on the same subject.
- **Documents have types**: An audit report and a design spec about the same system serve completely different purposes and should be synthesized differently.
- **Documents reference each other**: "As discussed in the architecture doc..." or "See the audit results for details." These cross-references form an explicit knowledge graph.
- **Documents have authors and audiences**: A strategy memo for leadership reads differently than a technical handoff for engineers, even about the same system.

The code pipeline treats every file as equal. The document pipeline needs to understand that documents have TYPE, SUBJECT, TIME, and AUTHORITY.

## The Maximal Pipeline

```
PHASE 1: RECONNAISSANCE
  Step 1: Header scan (mechanical — first 20 lines of each doc)
  Step 2: Pre-classify (fast LLM — type, subject, date, maturity)
  Step 3: Reference graph (mechanical — extract cross-doc references)

PHASE 2: DEEP EXTRACTION
  Step 4: Full extraction (per-doc LLM, type-aware prompts)

PHASE 3: ORGANIZATION
  Step 5: Subject clustering (LLM, informed by type + subject + references)
  Step 6: L0 webbing (cross-doc connections)

PHASE 4: SYNTHESIS
  Step 7: Thread synthesis (per-thread, temporally-ordered, supersession-aware)
  Step 8: L1 webbing

PHASE 5: DISTILLATION
  Step 9: Recursive clustering (L1 → L2 → apex)
  Step 10: L2 webbing
```

### Step 1: Header Scan (mechanical, no LLM)

Read the first 20 lines of each document. Extract:
- **Title**: First `#` heading or filename
- **Date**: Any date pattern (YYYY-MM-DD, "March 2026", "Week 12")
- **Author**: "By:", "Author:", or attribution patterns
- **File path**: Directory structure often encodes project/phase
- **Raw header text**: The first 20 lines verbatim — this is cheap context for the classify step

This is free — no LLM call, pure regex/parsing. Produces metadata that every subsequent step can use.

### Step 2: Pre-Classify (single fast LLM call)

Send ALL headers (20 lines × N docs) to the LLM in one call. For each document, classify:

- **Type** (one of):
  - `design` — architecture, specification, technical design
  - `audit` — review, analysis, assessment of existing system
  - `implementation` — handoff, how-to, step-by-step plan
  - `strategy` — vision, roadmap, business direction
  - `report` — bug report, test results, status update
  - `reference` — API docs, schema definitions, configuration guide
  - `meta` — process docs, retrospectives, meeting notes

- **Subject tags** (1-3): What system/feature/domain does this cover?
  - e.g., ["auth", "identity"], ["pyramid-engine", "build-pipeline"], ["legal", "entity-structure"]

- **Date**: Normalized to YYYY-MM-DD (or "undated")

- **Maturity**: `draft` | `proposal` | `decided` | `implemented` | `superseded`

- **Supersedes**: If this document explicitly replaces an earlier one, name it

This classification becomes metadata attached to every L0 node. The clustering step uses it as primary signal.

### Step 3: Reference Graph (mechanical)

Scan each document for references to other documents:
- Explicit file references: "see `architecture/auth-design.md`"
- Title references: "as described in the Auth Architecture document"
- Inline links: `[link text](path/to/doc.md)`

Build a directed graph: `doc A references doc B`. This tells us:
- Which docs form coherent reading sequences
- Which docs are "root" docs (referenced by many, reference few) — likely foundational
- Which docs are "leaf" docs (reference many, referenced by few) — likely implementation details

Store as `pyramid_web_edges` at depth 0 with relationship type `references`.

### Step 4: Full Extraction (per-doc, type-aware)

Unlike code where every file gets the same prompt, documents get TYPE-AWARE extraction:

- **Design docs**: Extract decisions made, alternatives considered, trade-offs, constraints, open questions
- **Audit reports**: Extract findings (severity + status), recommendations, what was tested, what passed/failed
- **Implementation plans**: Extract action items (who, what, status), dependencies, blockers, timeline
- **Strategy docs**: Extract goals, positioning, competitive analysis, success metrics
- **Bug reports**: Extract bugs found (severity, status, fix), reproduction steps, root cause
- **Reference docs**: Extract API endpoints, schemas, configuration options, version info

The prompt switches based on the `type` tag from Step 2. Each type emphasizes different aspects.

All types share the same output schema (headline, orientation, topics, entities) so downstream steps work uniformly. But the CONTENT of each topic is shaped by what matters for that document type.

### Step 5: Subject Clustering (LLM, metadata-informed)

This is where the document pipeline diverges most from code. The clustering prompt receives:
- All L0 topic summaries (same as code)
- PLUS: type, subject tags, date, and maturity for each document
- PLUS: reference graph edges (which docs cite which)

Clustering rules for documents:
- **Group by SUBJECT, not by type**: All auth-related docs (design + audit + implementation) belong together
- **Respect the reference graph**: Docs that reference each other should cluster together
- **Temporal coherence**: Within a cluster, documents should form a readable timeline
- **Size balance**: 4-12 docs per thread

The LLM output includes a `temporal_order` for each thread — which document should be read first, second, etc. This order feeds into the synthesis step.

### Step 6: L0 Webbing

Same as code — identify cross-doc connections. But for documents, also flag:
- **Contradictions**: Doc A says X, Doc B says Y about the same subject
- **Supersessions**: Doc B explicitly updates/replaces Doc A's conclusions
- **Dependencies**: Doc A's implementation depends on Doc B's design decisions

### Step 7: Thread Synthesis (temporally-ordered, supersession-aware)

This is the KEY step for documents. The synthesis prompt receives:
- All docs in the thread, ordered by date (earliest first)
- Classification metadata (type, maturity) for each doc
- Web edges showing cross-doc connections and contradictions

The synthesis RULES for documents:
- **Latest wins**: When a March audit overrides a February design decision, the March finding is current truth. The February decision becomes a correction (`wrong → right`).
- **Type-aware structure**: If the thread contains a design doc + audit + bugfix, the synthesis should capture: "This subsystem was designed with approach X (design-doc, Feb 10). Audit on Feb 25 found 3 issues: A (fixed), B (fixed), C (deferred). Current state: approach X with modifications per bugfix-02."
- **Preserve the timeline**: The orientation should tell the story of how understanding evolved, not just the final state
- **Flag open items**: Unresolved questions from ANY document in the thread should surface

### Steps 8-10: Webbing + Distillation

Same as code — recursive clustering with webbing at each layer. The apex should read like an executive briefing:
- What is this document collection about?
- What are the major topic areas?
- What was decided and what's still open?
- What's the timeline — where did this project start and where is it now?

## What This Produces

A pyramid where:
- **Apex**: "This collection covers the Wire platform across 3 months of design, audit, and implementation. 6 major areas: Auth & Identity, Platform Architecture, Agent System, Legal Structure, API Design, Economy & Credits. Key status: auth redesigned from password to magic-link (decided Feb 15, implemented Feb 28), API v2 launched March 1, legal entity formed as LLC (March 10). Open: credit economy pricing not finalized, agent marketplace design still in proposal stage."
- **L2 nodes**: Each represents a major domain with full temporal evolution
- **L1 nodes**: Each represents a subject thread with complete chronological synthesis — decisions, changes, current state, open items
- **L0 nodes**: Individual documents with type-aware extraction
- **Web edges**: Cross-cutting connections, contradictions, and supersessions at every layer

## YAML Step Count: 10

Compared to code's 6 steps, document adds:
- Header scan (mechanical, ~0 cost)
- Pre-classify (single LLM call, ~2s)
- Reference graph (mechanical, ~0 cost)
- Type-aware extraction routing (prompt selection, no new step)

The extra steps are cheap but dramatically improve clustering and synthesis quality.

## Open Design Questions

1. **Should we split by project?** If the doc folder contains docs about 3 different projects, should they be separate pyramids or one unified pyramid?
2. **How to handle very long documents?** A 50-page design doc should probably be chunked before extraction, but the chunks need to stay together for clustering.
3. **Should the apex include a timeline visualization?** Not just topics but "here's what happened when" — a chronological view alongside the topical view.
4. **How to handle updates?** When a new doc is added to the folder, can we incrementally update the pyramid without rebuilding from scratch? (The stale engine + delta chain should handle this, but document supersession makes it trickier than code.)
