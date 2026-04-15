# The Understanding Pyramid: Question-Driven Hierarchical Synthesis for Persistent Agent Intelligence

> **Working Draft — Paper Structure v2**
> **Authors**: Adam B. Levine et al.
> **Target**: Systems paper (arXiv preprint, then conference submission)

---

## Abstract

We present the Understanding Pyramid, a hierarchical synthesis system designed for AI agents that constructs *understanding* — not just retrievable knowledge — through question-driven decomposition and evidence gathering. Unlike retrieval-augmented generation (RAG) systems that retrieve flat document chunks, or hierarchical approaches like RAPTOR that cluster by topic similarity, the Understanding Pyramid uses questions as the durable organizational scaffold. A declarative YAML pipeline compiles into a directed acyclic graph of primitives — decompose, extract, evidence-loop, synthesize, web — that the chain executor dispatches in parallel with barrier synchronization. The system's shared state store (DADBEAR) enables delta builds: when source material changes or new questions arise, only affected branches are recomputed, using deterministic node IDs to update in place rather than rebuilding from scratch. This yields a structure that accretes understanding through use and improves through the act of being queried. We introduce the Vine architecture for lazy, question-driven composition across independent understanding bases without requiring direct merging or manual bridge curation — a capability the federated knowledge graph literature identifies as unsolved. Combined with an integrated credit economy that rewards both understanding producers and question-askers, the system forms a self-organizing understanding network. We argue this is an agent-age native primitive: a system class that could not have existed before the current era of cheap, capable, on-demand intelligence. We evaluate against RAPTOR and GraphRAG baselines on [corpus comprehension, cross-domain synthesis, multi-session task completion] and demonstrate [results TBD].

---

## 1. Introduction

### 1.1 The Problem

AI agents face a fundamental context limitation. Large language models can reason over information within their context window but cannot maintain, navigate, or synthesize understanding across sessions, corpora, or organizational boundaries. Current solutions treat knowledge as flat retrieval targets:

- **RAG** retrieves document chunks by embedding similarity
- **RAPTOR** (Sarthi et al., 2024) organizes chunks into a topic-clustered tree but remains static and query-passive
- **GraphRAG** (Microsoft, 2024) builds entity-relationship graphs with community summaries but loses narrative and procedural knowledge at extraction time
- **Agent memory systems** (Letta/MemGPT) manage editable memory hierarchies but don't construct persistent understanding structures

All of these answer "what chunk is relevant?" but not "what do we understand about this domain?"

### 1.2 Knowledge vs. Understanding

**Knowledge** is facts you can retrieve. **Understanding** is what you have when you can answer questions you've never been explicitly given evidence for — because you've synthesized relationships between facts into a coherent model. The Understanding Pyramid doesn't store knowledge. It constructs understanding through recursive question-driven synthesis. Each layer isn't a summary (RAPTOR) or a community description (GraphRAG) — it's a synthesis that answers questions by reasoning across evidence, noting contradictions, recording decisions, and identifying dead ends.

### 1.3 The Insight

Questions are a better organizing principle than topics or entities. A knowledge structure scaffolded by the questions it answers is inherently:

- **Navigable**: agents can traverse by following questions, not similarity scores
- **Composable**: cross-domain synthesis happens by asking questions across understanding bases
- **Self-improving**: new questions extend the structure, contradictions trigger corrections, partial answers create demand signals

### 1.4 Why Now: An Agent-Age Native Primitive

The Understanding Pyramid could not have existed before approximately 2024. It requires:

1. **Sufficiently inexpensive on-demand intelligence**: Recursive synthesis across thousands of nodes requires LLM calls at commodity pricing — both cloud (varying quality tiers) and local inference
2. **Sufficiently capable models**: The synthesis step requires genuine reasoning, not just extractive summarization. Models below a capability threshold produce summaries, not understanding
3. **Both local and cloud availability**: The economic viability of building and maintaining pyramids depends on flexible compute — expensive models for hard synthesis, cheap models for routine extraction
4. **The ability to build complex systems by feel**: The Understanding Pyramid itself was designed and built through human-AI collaboration, using the same agent-native primitives it enables. It is a product of the era it serves

This is not an incremental improvement on RAG or knowledge graphs. It is a new system class native to the agent age — where intelligence is cheap enough to be structural material, not just a query interface.

### 1.5 Contributions

1. The Understanding Pyramid — a hierarchical synthesis structure built through recursive question decomposition (§3)
2. The Vine architecture — lazy, economically-incentivized composition across independent pyramids (§4)
3. Self-accreting understanding through use, via annotation-driven FAQ generalization (§5)
4. An integrated credit economy making understanding production and question-asking economically self-sustaining (§6)
5. A working implementation with CLI, MCP server, and Rust/TypeScript backend (§7)
6. Evaluation against RAPTOR and GraphRAG baselines (§8)

---

## 2. Related Work

### 2.1 Hierarchical Retrieval and Synthesis

**RAPTOR** (Sarthi et al., 2024): Bottom-up embed-cluster-summarize using GMMs with soft assignment. Achieves 20% accuracy improvement on complex QA. Two retrieval modes: collapsed tree (flat vector search across all levels) and tree traversal (greedy top-down). Critical weakness: any source change requires full tree reconstruction — EraRAG (2025) proposes LSH-based localized updates but is not production-deployed. Information loss: ~4% hallucination rate at summarization, numerical precision and named entities degraded at higher levels.

**GraphRAG** (Edge et al., 2024): Entity-relationship extraction → Leiden community detection → community summaries. Dominates multi-hop reasoning (80% correct vs. 50.83% for flat RAG in enterprise settings) but loses narrative/procedural content at entity extraction. Community structure is non-deterministic. No incremental update mechanism.

**HiRAG** (2025): Hierarchical KG with bridging mechanism linking entity facts to community summaries.

All three are **static** (built offline), **topic/entity-organized** (not question-driven), and **query-passive** (don't improve through use).

### 2.2 Agent Memory

**Letta/MemGPT**: Editable memory hierarchy with "sleep-time compute" for restructuring during idle periods. Closest to self-accreting understanding but no persistent structural scaffold.

**MemoryArena** (2026): Demonstrates that recall benchmark performance does not predict functional memory utility. Current systems fail at multi-session task completion even when acing LoCoMo and LongMemEval.

**MemoryAgentBench** (2025): Evaluates retention, update, retrieval, and conflict resolution. No single approach excels at all four. Conflict resolution is the closest proxy for "understanding."

### 2.3 Question Decomposition

**QDMR** (Wolfson et al., 2020): Formal representation of complex questions as ordered DAGs of atomic steps. Break dataset (83,978 questions). Defines atomicity, completeness, independence, faithfulness.

**Decomposition architectures**: Sequential (brittle), iterative/interleaved (IRCoT — much better), factored (Anthropic 2023 — best faithfulness), HQDT/RoHT (ACL 2023 — persistent tree during processing, closest to Understanding Pyramid but inference-time only).

**Failure modes** (March 2026): 44.96% from no decomposition produced, 28.45% from decomposition ignored during execution. The Understanding Pyramid addresses both: the scaffold is pre-built and persistent, and execution follows it structurally.

**Critical gap**: No published system uses question decomposition as a *durable organizational primitive*. All existing systems decompose at inference time and discard the structure. The Understanding Pyramid is the first system where the decomposition itself is the knowledge scaffold.

### 2.4 Federated Knowledge Composition

Federated Knowledge Graphs use explicit bridge mappings for cross-domain reasoning. Bridges are hand-curated. "LLM-native federation where an agent reasons across KB boundaries on-the-fly remains an unsolved problem in production systems." The Vine architecture (§4) solves this through lazy question propagation.

### 2.5 Economic Mechanisms for Knowledge Production

**VCG mechanisms**: Theoretically optimal but require counterfactual value measurement and can violate budget balance.

**Wikipedia**: Sustained by social identity, low marginal cost, visible impact. Fails on contested topics and quality maintenance. Fragile to community size reduction (HBS Chinese Wikipedia study).

**Stack Overflow**: Collapsed via incumbent capture — reputation system rewarded gatekeeping, creating hostile newcomer experience. 50% traffic drop by 2026, accelerated by LLM substitution.

**Prediction markets** (2014 ICML): Prediction market + collaboration platform is self-sustaining, doesn't crowd out intrinsic motivation, filters quality through creative destruction. Most promising existing design for knowledge incentives.

**TCRs**: Work in narrow domains, fail when quality criteria are contested.

**Five conditions for sustainability**: (1) value capture proportional to creation, (2) non-excludable but attributable, (3) low minimum contribution, (4) maintained diversity, (5) curation separated from gatekeeping.

### 2.6 Self-Organizing Knowledge Systems

**Stigmergy**: Indirect coordination through environmental traces. Wikipedia's self-organization succeeds (vandalism reversion, proportional article density) and fails (systemic bias, elite entrenchment, wiki-gangs).

**MIT Crystal framework** (Buehler, 2025): Autonomous graph expansion via LLM reasoning. Produces scale-free networks with emergent bridge nodes. Not grounded in source documents, no internal quality signal, superlinear compute cost.

**The gap**: No system applies usage-driven self-improvement to hierarchical question-indexed structures. The Understanding Pyramid is the first where querying actively improves the structure through annotation accretion, FAQ generalization, and contradiction resolution.

---

## 3. The Understanding Pyramid

### 3.1 Architecture
- Layered structure: L0 (source evidence extracts) → L1 (answer nodes from evidence loop) → L2+ (higher synthesis) → Apex
- Each layer is produced by a different class of primitive, not by repeated application of a single rule
- Immutability and versioned supersession chains — never delete, always version
- Structured node schema: headlines, distilled synthesis, topics (with entities, corrections, decisions), terms, dead ends, provenance

### 3.2 Why Questions, Not Topics
- RAPTOR clusters by embedding similarity → hierarchy reflects source-material structure
- GraphRAG clusters by entity co-occurrence → hierarchy reflects entity relationships
- Understanding Pyramid decomposes by questions → hierarchy reflects knowledge needs
- Information loss is governed by what the question requires, not by summarization artifacts
- Navigation is natural: agents follow questions, which map to their actual goals

### 3.3 The Declarative Build Pipeline

The build pipeline is defined entirely in YAML as a chain of named steps, each tagged with a primitive type. The chain executor compiles this into a DAG, sorts topologically, and dispatches:

**Question Pipeline (question.yaml):**
1. **cross_build_input** (load_prior_state): Load existing pyramid state, question tree, overlay answers, unresolved gaps
2. **extract** (source_extract): Content-neutral L0 extraction from source material — parallel, largest-first, auto-splitting oversized documents, with JSON healing for malformed output. Only runs on first build (`when: l0_count == 0`)
3. **web** (l0_webbing): Build corpus structure map — LLM produces JSON edge lists connecting L0 nodes. Informs decomposition about thematic boundaries
4. **cross_build_input** (refresh_state): Re-read L0 summaries into shared variable map for downstream steps
5. **extract** (enhance_question): Enrich the apex question with corpus context
6. **recursive_decompose** (decompose OR decompose_delta): Decompose question into branch/leaf sub-question tree guided by actual source material, not abstract categories. If a prior build exists, `decompose_delta` takes the existing tree + answers + gaps and evolves rather than rebuilds
7. **extract** (extraction_schema): Generate structured extraction schema from question tree
8. **evidence_loop**: Core primitive — repeatedly filters, aggregates, and transforms L0 evidence to answer each sub-question. Produces answer nodes
9. **process_gaps**: Detect missing evidence and orphaned branches, trigger targeted re-extraction
10. **web** (l1/l2_webbing): Cross-cutting edges between sibling answer nodes at each depth

**Document Pipeline (document.yaml):**
1. **extract** → **web** → **classify** (batch cluster → merge) → **synthesize** (thread narrative) → **web** → **synthesize** (recursive_cluster with apex_ready gate) → **web**

**Key design properties:**
- Parallel dispatch with barrier synchronization (all workers finish before refresh)
- Token-compact JSON-only prompts — not full documents
- Three-tier fault tolerance: step-level retry/skip/abort, JSON healing for malformed LLM output, model fallback for oversized prompts
- Every step output written to DADBEAR shared store as structured records with deterministic IDs

### 3.4 Delta Builds: Evolution, Not Reconstruction
- When a question tree already exists, `decompose_delta` receives the existing tree, existing answers, evidence sets, and unresolved gaps
- `evidence_loop` pulls only new/changed evidence; `merge_batches` combines with existing nodes
- `gap_processing` spots missing branches and triggers targeted re-extraction
- Deterministic node IDs (derived from step `creates:` fields) enable in-place updates of changed fields only
- This is the fundamental difference from RAPTOR: the Understanding Pyramid evolves; RAPTOR must rebuild

### 3.5 Webbing: Lateral Connections
- Cross-cutting edges between sibling nodes within a layer
- LLM-generated JSON edge lists with relationship types and strength scores
- Captures relationships the hierarchy misses
- Applied at each depth level after node creation

---

## 4. The Vine Architecture

### 4.1 The Cross-Understanding Composition Problem
- Independent pyramids can't synthesize across boundaries
- Direct node-to-node webbing creates N×M explosion
- Federated KG-style manual bridges don't scale

### 4.2 Lazy Question Propagation
- Meta-pyramid propagates questions downward to bedrock pyramids
- Each bedrock answers through normal synthesis → produces Vine L0
- Meta-pyramid synthesizes across Vine L0s at its L1
- Full provenance: meta-L1 → vine-L0 → question → bedrock evidence

### 4.3 Relative Designation and Recursive Composability
- Bedrock/meta is a relationship, not an identity
- Any pyramid can be either, simultaneously
- Architecture composes recursively at any scale: chapter → document → corpus → network
- The pyramid is the fractal unit; vines are how it tiles

### 4.4 Wire-Mediated Vines: The Network Scale
- The Wire replaces manual --ref flags with economic discovery
- Pyramid owners voluntarily expand: they retain 95% of synthesis royalties
- Question-askers pay small credit fee but own the question slot
- Good questions cause network-wide expansion at near-zero cost to asker
- The self-reinforcing incentive loop: questions → expansion → more queries → more revenue → more expansion

### 4.5 Question-Asking as Productive Labor
- Askers earn ongoing citation royalties for questions that cause valuable expansion
- The network's most valuable actors may be those who ask the best questions
- Stigmergic signal: good questions leave visible, high-value traces that attract expansion

---

## 5. DADBEAR: Shared State and Self-Accreting Understanding

DADBEAR is not merely a staleness tracker — it is the shared state management system that makes the entire pipeline work. Every step in the build pipeline writes its output to DADBEAR as a structured record containing a unique ID, content fields, and dependency metadata. This shared store is what enables delta builds, cross-build state continuity, and annotation-driven knowledge accretion.

### 5.1 State Management Role
- All step outputs stored as structured records with deterministic IDs and dependency links
- `cross_build_input` / `refresh_state` primitives read from DADBEAR to provide downstream steps with current layer summaries
- Build-ID versioning tracks which build produced each node
- Dependency metadata enables the system to detect what changed and recompute only affected branches

### 5.2 Annotation-Driven Enrichment
- Agent annotations during pyramid use: observations, corrections, friction, ideas
- Annotations attach to specific nodes, accumulate across sessions
- FAQ generalization: when unprocessed annotations cross a threshold, DADBEAR clusters them and synthesizes generalizable knowledge items with match triggers
- The pyramid gets smarter through being queried — understanding accretes without explicit curation

### 5.3 Stigmergic Properties
- The pyramid structure itself signals what needs work (gaps from `process_gaps`, contradictions in topics, staleness from file-hash changes)
- Agents respond to structural traces, not explicit task assignment
- Meets all four stigmergic criteria: persistent modification, visible traces, trace-responsive agents, quality-amplifying feedback
- The environment does the coordination: gap nodes tell agents what to work on, annotation counts signal where understanding is deepening

### 5.4 Staleness Detection and Propagation
- File-hash tracking detects source changes
- Build-ID versioning identifies which nodes are current
- Owner sovereignty: freeze, subscribe, or ignore updates
- Cross-vine staleness via DADBEAR event propagation

### 5.5 Quality vs. Noise: Why This System Self-Organizes Well
- Ground truth signal exists: source documents provide verifiable evidence
- Trace granularity matches contribution: annotations attach to specific nodes
- Positive/negative feedback balanced: corrections and expansions coexist
- Quality and fitness aligned: what survives is what answers questions accurately
- Three-tier fault tolerance at build time prevents noise from entering the structure

---

## 6. Credit Economy Integration

### 6.1 The Unified Flow Formula
- 60% creator / 35% source chain / 2.5% platform / 2.5% graph fund
- Value capture proportional to value creation (not submission-based like SO)
- Non-excludable but attributable (contributions are accessible but provenance-tracked)

### 6.2 Avoiding Known Incentive Collapse Patterns
| Failure Pattern | How Understanding Pyramid Avoids It |
|---|---|
| Incumbent capture | No gatekeeping power — anyone can ask questions, anyone can answer |
| Commons tragedy | Contributors retain 95% of synthesis royalties — contributing is profitable |
| Quality-quantity inversion | Royalties based on downstream citations, not volume |
| Network thinning | Economic incentives attract expansion; good questions grow the network |
| LLM substitution | LLMs ARE the contributors — the system is native to the agent age |

### 6.3 The Knowledge Futures Market
- Questions as futures contracts
- Pyramids as producers
- Vine-L0 as settlement
- Ongoing royalties as yield
- Prediction market + collaboration platform model (cf. ICML 2014)

---

## 7. Implementation

### 7.1 System Architecture
- Rust backend: chain engine (YAML → DAG compilation), chain executor (topological dispatch), chain dispatcher (primitive routing), DADBEAR shared state store
- TypeScript: CLI (39 commands), MCP server (native agent tool integration)
- YAML declarative pipelines: versioned, forkable, contributable — the pipeline definition is itself a contribution to the understanding structure
- Primitives: extract, synthesize, web, classify, recursive_decompose, evidence_loop, process_gaps, cross_build_input, container (sub-chains)
- Local-first with optional Wire network integration

### 7.2 Multi-Pyramid Type Support
- Document pyramids: synthesize understanding from document corpora
- Code pyramids: synthesize understanding from codebases
- Conversation pyramids (vines): aggregate conversation threads
- Question pyramids: compose understanding across source pyramids

### 7.3 Agent Interface
- Self-documenting CLI with structured JSON help
- MCP server for native integration with AI agents
- Apex → search → drill navigation pattern
- Annotation and FAQ contribution during use

---

## 8. Evaluation

### 8.1 Corpus Comprehension (vs. RAPTOR, GraphRAG)
- Task: Agent must answer complex questions about a large corpus
- Baseline: Same corpus indexed by RAPTOR tree and GraphRAG community structure
- Metrics: Accuracy, hallucination rate, time to correct answer, navigation efficiency
- Hypothesis: Understanding Pyramid outperforms on questions requiring synthesis across topics (where RAPTOR clusters fail) and on questions requiring narrative/procedural knowledge (where GraphRAG entity extraction loses information)

### 8.2 Cross-Domain Synthesis (Vine Evaluation)
- Task: Answer questions requiring evidence from multiple independent corpora
- Baseline: Multi-document RAG, multi-corpus GraphRAG
- Metrics: Answer completeness, provenance accuracy, cost efficiency
- Hypothesis: Vine composition produces more complete, better-provenanced answers than flat multi-source retrieval

### 8.3 Multi-Session Task Completion
- Task: Agent completes interdependent tasks across sessions, where earlier findings inform later decisions
- Framing: Adapted from MemoryArena methodology
- Baseline: Conversation log replay, Letta-style memory, cold start
- Metrics: Task completion rate, error propagation, context efficiency
- Hypothesis: Pyramid + annotations outperform all baselines on multi-session coherence

### 8.4 Self-Improvement Through Use
- Measure: Pyramid quality metrics (annotation count, FAQ items generated, correction accuracy) across agent sessions
- Hypothesis: Quality monotonically increases with use. Understanding accretes.

### 8.5 Novel Benchmark Contribution
- Combine MemoryArena task-completion framing with HiCBench structural evaluation
- Evaluate whether question-indexed structures outperform topic-clustered and entity-graph structures for agent task completion
- **This evaluation framework itself is a publishable contribution** — no existing benchmark covers this

---

## 9. Discussion

### 9.1 The Agent-Age Native Primitive Argument
The Understanding Pyramid is not an improvement on RAG or knowledge graphs. It is a new system class that requires:
- Commodity-priced intelligence (local and cloud)
- Models capable of genuine synthesis (not just extraction)
- Flexible compute tiers (expensive for hard synthesis, cheap for routine)
- Human-AI collaborative design (the system was built using the paradigm it enables)

Previous eras had the storage, the retrieval, and the graph algorithms — but not the intelligence to recursively synthesize understanding at scale. The Understanding Pyramid is to the agent age what the relational database was to the structured computing age: a foundational primitive that couldn't exist before its enabling technology matured.

### 9.2 Limitations
- LLM dependency: synthesis quality bounded by model capability
- Build cost: initial construction is compute-intensive (mitigated by tiered models)
- Cold start: new pyramids have no annotations or FAQ items
- Question quality: decomposition quality gates everything downstream

### 9.3 Future Work
- Vine depth analysis: natural limits of recursive meta-composition
- Real-time incremental updates (streaming synthesis)
- Cross-model pyramid compatibility
- Question quality scoring beyond market-driven pricing

---

## 10. Conclusion

[Dependent on evaluation results. Core claim: the Understanding Pyramid represents a new system class — the first agent-age native primitive for persistent, self-improving, economically-sustainable understanding.]

---

## Appendices

### A. Understanding Pyramid Node Schema (full JSON)
### B. YAML Pipeline Definitions
### C. CLI Command Catalog (39 commands)
### D. Vine Architecture Specification
### E. DADBEAR System Design
### F. Evaluation Benchmark Specification
