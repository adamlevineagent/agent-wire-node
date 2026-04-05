# Understanding Web Architecture

This document is the canonical design for the Wire knowledge system. It supersedes the scattered design docs that preceded it (question-pyramid-architecture.md, question-driven-pyramid-v2.md, two-pass-l0-contracts.md) by unifying their insights into a single coherent architecture. Those documents remain as historical context for the decisions captured here.

## The Core Thesis

A knowledge pyramid is a materialized view over a question-answer graph. The graph is the truth; the pyramid is a rendering. Different questions materialize different pyramids from the same evidence base. The evidence base gets richer with every question asked. The system gets smarter over time without rebuilding from scratch.

There is no separate "mechanical pipeline" and "question pipeline." There is one system: questions drive everything. What was previously called a "mechanical build" is a preset question — "What is this body of knowledge and how is it organized?" — with a preset decomposition strategy. Content types (code, document, conversation) are question templates, not architecture.

## The Three Layers

### Source Layer (corpus documents)

Source files are corpus documents. They are not contributions, not Wire entities — they are local files on disk registered in `pyramid_slugs` with a `source_path`. The build pipeline reads them. Publication to the Wire is a separate, optional export step. No Wire API calls happen during build.

Source files are watched by DADBEAR's file watcher, which tracks SHA-256 hashes in `pyramid_file_hashes` and writes mutations to `pyramid_pending_mutations` when files change.

### Evidence Layer (L0)

L0 is the evidence base. Every L0 extraction is shaped by questions — there is no "question-agnostic" extraction. The decomposition runs first, the L1 questions are examined holistically, and the resulting extraction prompt and schema determine what L0 looks for in each source file.

**First-question extraction** (`C-L0-{index:03}` IDs): The first question asked of a corpus produces the initial L0 extraction. If the question is broad ("What is this and how is it organized?"), the extraction is broad. If the question is narrow ("What are the security vulnerabilities?"), the extraction focuses on security. The extraction prompt and output schema are generated from the decomposed sub-questions — not hardcoded.

**Targeted re-examination** (`L0-{uuid}` IDs): When a subsequent question asks something the existing L0 evidence doesn't cover, MISSING verdicts identify the gaps. Targeted re-examinations extract new evidence from specific source files, focused on what the new question needs. These sit alongside the existing L0 nodes, enriching the evidence base.

Both kinds of L0 node live in the same `pyramid_nodes` table, distinguished by ID format and `self_prompt` content. Both are valid evidence for any question — the pre-mapper doesn't care which question originally shaped the extraction.

L0 nodes are permanent. They are superseded (not deleted) when source files change. Old versions are preserved with `superseded_by` pointers for audit trail.

### Understanding Layer (L1+)

Everything above L0 exists because a question was asked. Each node is an answer to a question, backed by evidence links (KEEP verdicts with weights and reasons) pointing to nodes in the layer below. The `self_prompt` field contains the question. The `distilled` field contains the answer. The `topics` array contains the structured breakdown.

The understanding layer is a DAG, not a tree. The same evidence node can be KEEP'd by multiple questions at different weights. This many-to-many relationship is the graph structure that makes the web work — a single L0 node about authentication might be evidence for an architecture question (weight 0.9), a security question (weight 0.8), and an operations question (weight 0.3). Those three weights tell you this node is central to architecture and security, peripheral to operations.

## Evidence Sets

When a question pyramid runs and its answers report MISSING evidence, the system can trigger targeted re-examinations of source files to fill the gaps. These re-examinations form an **evidence set** — a collection of L0 nodes created to serve a specific question's evidence needs.

### Sets are pyramids

An evidence set is itself a DADBEAR-managed pyramid. When the set has one node, it's trivially its own apex. When it grows past one node, DADBEAR dispatches a helper to synthesize a set apex — a summary of what this set contains and why it exists. As the set grows further, DADBEAR's cascade naturally produces intermediate structure: the set apex stales, re-synthesis discovers internal groupings, the set deepens.

This is recursive all the way up and down. Each leaf question's evidence set is a pyramid. Each branch question's pyramid is composed of its leaf pyramids. The whole question build is a pyramid of its branch pyramids. The pattern is the same at every level.

### Sets are shared, not duplicated

Evidence sets are identified by what they contain, not which question created them. If Build 1 creates a targeted re-examination of `auth_middleware.rs` focused on token validation, and Build 2 also needs token validation evidence, Build 2 cross-links to Build 1's existing L0 node via a KEEP verdict. It does not create a redundant extraction.

This is the DAG principle at L0: one authoritative extraction referenced by many consumers. The same evidence node appearing in multiple questions' KEEP lists with different weights is a signal — it tells you that node is central to multiple concerns. Redundant copies would destroy this signal.

The pre-mapper's first job when a new question arrives is: "Does evidence for this already exist?" Only when existing evidence doesn't cover the need does a new targeted re-examination run.

### Set identity

Each evidence set has:
- An `evidence_set_id` (the build that created it, or a content-addressed ID based on the question that triggered it)
- A set apex node (DADBEAR-managed, synthesized from members)
- Member L0 nodes (targeted re-examinations, each with `self_prompt` recording the triggering question)
- Provenance: which question's MISSING verdict triggered the set's creation

Evidence links (`pyramid_evidence`) connect set members to the questions they serve. These links are build_id-scoped and never deleted — old builds' evidence assessments persist as audit trail.

## How Questions Drive the System

### Phase 1: Decompose (top-down)

The decomposer receives the apex question AND a map of the full existing understanding structure: all evidence set apexes, all L1+ answer nodes, all MISSING verdicts. It decomposes the apex question into sub-questions, then diffs against the existing structure.

- Sub-questions already answered by existing nodes → cross-link (KEEP verdict from new question to existing answer node). No new work.
- Sub-questions partially answered → the existing answer's MISSING verdicts point to the gaps. Only the gaps trigger new evidence gathering.
- Sub-questions with no existing coverage → full decomposition into leaf questions, which trigger evidence gathering from L0.

The first question on a fresh corpus: full decomposition, everything is new.
The tenth question on a rich corpus: mostly cross-linking, tiny delta of new work.

### Phase 2: Generate extraction schema (holistic)

After decomposition, the system examines all L1 sub-questions holistically — not individually, but as a complete picture of what needs to be understood. From this holistic view, it generates:

- **Extraction prompt**: What should L0 look for in each source file? This is not a static prompt — it is generated from the specific questions that need answering. "What are the security vulnerabilities?" produces an extraction prompt focused on auth flows, input validation, error handling. "What is this and how is it organized?" produces a broader prompt covering architecture, structure, relationships.

- **Output schema**: What fields should the extracted evidence contain? The schema defines the shape of L0 nodes — topic structure, entity types, relationship categories — tailored to what the questions need.

This is the `extraction_schema` step. It runs once per question build, after decomposition and before L0 extraction. It ensures that the evidence layer captures exactly what the understanding layer needs.

### Phase 3: Extract evidence (L0)

For each source file chunk, the system runs the generated extraction prompt to produce L0 nodes. This is the only LLM-intensive step that touches source files directly.

On a fresh corpus: every chunk is extracted, producing the initial evidence base.
On a corpus with existing L0: only chunks that lack evidence for the current questions are re-examined. The existing evidence is reused where it covers the need.

The extraction is parallelized (concurrency matched to the LLM provider) and produces one L0 node per chunk. Each node contains the question-shaped evidence: headline, orientation, topics with the generated schema's fields, entities.

### Phase 4: Answer questions (bottom-up)

For each leaf question that needs answering:

1. **Pre-map**: Which L0 nodes might contain relevant evidence? This is a single LLM call that reads evidence summaries and returns candidate lists. Over-includes rather than misses — false positives are cheap, missed evidence is permanent.

2. **Answer**: Synthesize an answer from the candidates. Report KEEP (with weight 0.0-1.0 and reason), DISCONNECT (false positive), or MISSING (evidence gap) for each candidate.

3. **Gap handling**: MISSING verdicts are demand signals, not creation orders. They are recorded as evidence of what the question needed but couldn't find. DADBEAR or a separate agent can later inspect MISSING verdicts and decide whether to trigger targeted re-examinations. The gap creates demand; something else fills it.

### Phase 5: Synthesize (bottom-up)

Branch questions are answered by synthesizing the answers to their leaf questions. The apex question is answered by synthesizing the answers to its branch questions. Each synthesis produces a new node in the understanding layer with evidence links (KEEP verdicts) pointing to the nodes it synthesized from.

Every KEEP candidate that represents a genuinely distinct dimension of the answer is reflected in the synthesis. The synthesis is dense and specific — names, decisions, relationships from the evidence, not vague overviews.

### Phase 6: Reconcile

After all questions are answered, reconciliation identifies:
- **Orphan nodes**: L0 evidence not referenced by ANY question (even DISCONNECT). These signal missing questions in the decomposition.
- **Central nodes**: Evidence KEEP'd by many questions at high weight. These are the load-bearing facts of the corpus.
- **Gap clusters**: Groups of MISSING verdicts that point to the same kind of evidence. These are systematic gaps that a single targeted re-examination might fill.

## How the Web Stays Current

DADBEAR manages the entire understanding web through per-layer debounce timers with batched helper dispatch.

### Source file changes

1. File watcher detects hash change → writes mutation to `pyramid_pending_mutations` at L0
2. L0 timer debounces (5 minutes), drains mutations, dispatches stale-check helpers
3. Helper reads new file content, computes diff against existing L0 node, determines: ADDITION (new material), MODIFICATION (same capability, different behavior), or SUPERSESSION (old claim now false)
4. Confirmed stale → `execute_supersession()` creates new L0 node, sets `superseded_by` on old node
5. Propagation: all evidence links referencing the old L0 node trigger staleness checks at L1

### Staleness vs. supersession

Two propagation channels, both critical:

**Staleness** (attenuating): Evidence changed but beliefs might still hold. Propagates through evidence weights — high-weight evidence changing is more likely to stale the answer than low-weight evidence. Attenuation means distant effects are smaller. Staleness triggers re-evaluation but doesn't mandate correction.

**Supersession** (non-attenuating): A specific claim is now false. Propagates through belief dependency — find every node that contains the superseded claim, regardless of weight or distance. A false claim must be corrected everywhere it appears. Supersession mandates re-answering with explicit correction directives.

### Cascade

Re-answering a question may itself produce new supersessions (the updated answer contradicts something the layer above claims). The cascade continues until no more superseded beliefs exist or the apex has been re-answered. Per-layer timers prevent cascade storms — each layer waits for its inputs to settle before dispatching.

Runaway breaker: if more than 75% of nodes at a layer are stale simultaneously, the breaker trips. Options: resume cascade, rebuild layer from scratch, or freeze.

## How Accretion Works

The evidence base grows with every question asked, without redundancy.

### First question on a fresh corpus

Full pipeline: decompose → generate extraction schema → extract all chunks → answer all questions → synthesize to apex. This produces the initial L0 evidence shaped by the first question's needs, plus the full L1+ understanding structure.

### Second question on the same corpus

The decomposer sees the existing L0 evidence and L1+ answers. It decomposes the new question and diffs:
- Sub-questions covered by existing answers → cross-link. No new work.
- Sub-questions where existing L0 evidence is sufficient → answer from existing evidence. No new extraction.
- Sub-questions where L0 evidence has gaps → MISSING verdicts trigger targeted re-examinations of specific source files, using a new extraction prompt shaped by the new question's needs.

The second question is cheaper than the first. It inherits the existing evidence base and only does new work for genuine gaps.

### Tenth question on a rich corpus

The evidence base is dense. Most sub-questions are already answered somewhere in the web. The delta is tiny — maybe one or two targeted re-examinations, a few new L1 answers. The rest is cross-linking. The system is nearly free to query because the evidence has already been gathered.

### Different questions, different extractions

"What is this codebase and how is it organized?" generates an extraction focused on architecture, module structure, data flow.

"What are the security vulnerabilities?" generates an extraction focused on auth flows, input validation, error handling, credential storage.

Same source files, different extraction prompts, different L0 nodes. Both sets of evidence accumulate in the web. A third question about "How does the API work?" can draw from both — architecture evidence from the first question and auth evidence from the second, without re-extracting either.

## Data Model

All data lives in SQLite. The system is local-first. No Wire connectivity required during build.

### Key tables

- `pyramid_nodes`: All nodes (L0 first-question, L0 targeted, L1+ answers). Key fields: `id`, `slug`, `depth`, `headline`, `distilled`, `topics`, `self_prompt`, `superseded_by`, `build_id`
- `pyramid_evidence`: Evidence links between nodes. Key fields: `source_node_id`, `target_node_id`, `verdict` (KEEP/DISCONNECT/MISSING), `weight`, `reason`, `build_id`. Build_id-scoped, never deleted.
- `pyramid_question_tree`: Persisted decomposition tree (JSON). One per slug per build.
- `pyramid_question_nodes`: Flat index of all questions at all depths. Used by the evidence loop.
- `pyramid_web_edges`: Cross-cutting connections between sibling nodes (shared systems, shared decisions).
- `pyramid_pending_mutations`: WAL for DADBEAR. Crash-recoverable mutation queue.
- `pyramid_file_hashes`: Source file tracking for change detection.
- `pyramid_gaps`: MISSING verdicts accumulated across builds. Demand signals for future evidence gathering.

### Node ID conventions

- `C-L0-{index:03}`: First-question extraction. Deterministic per file order.
- `L0-{uuid}`: Targeted re-examination. Random per build.
- `L{depth}-{uuid}`: Answer nodes. Random per build. Depth encodes pyramid layer.

### Evidence set tracking

Evidence sets are tracked by grouping L0 nodes that share the same triggering question context. The `self_prompt` field on targeted L0 nodes identifies which question spawned them. Set apexes are regular pyramid nodes managed by DADBEAR — they are synthesized from their member L0 nodes and stale-checked when members change.

## What This Replaces

This architecture unifies and supersedes:

- **Mechanical pipelines** (document.yaml, code.yaml chain definitions): These become preset questions with preset decomposition strategies. The chain YAML is a frozen decomposition of "What is this [content_type] and how is it organized?" A question pyramid with the same apex question should produce equivalent or better results.

- **Static extraction prompts** (doc_extract.md, code_extract.md): These become what the extraction schema generator produces when given the preset question. The static prompts are a frozen extraction schema — what you'd get if you hardcoded the output of extraction_schema.rs for the default question.

- **Content type distinction**: Code, document, and conversation are question templates, not separate architectures. The content type informs the characterization step (which sets audience, tone, and extraction focus) but does not change the pipeline.

- **Separate mechanical and question build paths**: One executor, one evidence loop, one DADBEAR cascade. The `run_decomposed_build()` path is the only path. Mechanical builds are syntactic sugar for a decomposed build with a preset question.

## What This Preserves

- **The DAG model**: Pyramids are materialized views over a question-answer graph. The tree is a rendering choice.
- **Evidence-weighted answering**: KEEP/DISCONNECT/MISSING verdicts with 0.0-1.0 weights and reasons. Every connection is justified.
- **Staleness vs. supersession**: Two propagation channels, both necessary. Staleness attenuates; supersession does not.
- **DADBEAR**: Per-layer timers, batched dispatch, tombstones, edges as first-class propagation targets, runaway breaker.
- **Local-first**: SQLite, no Wire during build, publication is optional export.
- **Archive not delete**: Slugs archived, nodes superseded, evidence build_id-scoped. Nothing is destroyed.
- **Gaps as demand signals**: MISSING verdicts create demand for evidence. They do not create evidence.
- **The contribution pattern**: Evidence, annotations, FAQ entries, and eventually DADBEAR mutations are all contributions. The system's knowledge grows through contributions, not through special-case data paths.
