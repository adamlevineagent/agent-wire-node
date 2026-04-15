# Understanding Pyramid: Deep Research Compilation

> **Date**: April 6, 2026
> **Sources**: Perplexity deep research across five query domains
> **Purpose**: Raw research material for the Understanding Pyramid paper

---

## 1. RAPTOR & GraphRAG Deep Comparison

### RAPTOR Construction Pipeline (Bottom-Up Embed-Cluster-Summarize)

1. Chunk source documents into leaf nodes (~100 token segments)
2. Embed all chunks using dense embedding model (e.g., SBERT)
3. Reduce dimensionality via UMAP — two passes: wide n_neighbors (global themes) + narrow (local sub-clusters)
4. Cluster using Gaussian Mixture Models with **soft assignment** (chunks can belong to multiple clusters)
5. Summarize each cluster with LLM into parent node
6. Re-embed summaries and repeat until single root node

Key: If any cluster exceeds LLM context window, RAPTOR recursively subdivides before summarizing.

**Retrieval modes:**
- Collapsed Tree: Flatten all nodes into one vector store, cosine similarity regardless of level
- Tree Traversal: Start at root, descend greedily — prone to early commitment errors

**Ref**: [arxiv RAPTOR](https://arxiv.org/html/2401.18059v1)

### GraphRAG Construction (Entity-Relationship-First)

1. Extract entities and relationships via LLM (people, places, organizations, events, claims)
2. Build property graph (nodes = entities, edges = typed relationships)
3. Detect communities via Leiden algorithm (dense subgraphs)
4. Summarize communities bottom-up: Level 0 most specific → root most general
5. Answer queries via map-reduce across community summaries

Hierarchy is graph-topological, not semantic-distance-based.

**Ref**: [Microsoft GraphRAG](https://www.microsoft.com/en-us/research/blog/graphrag-new-tool-for-complex-data-discovery-now-on-github/)

### The Update Problem (Critical for Both)

- RAPTOR: Any source change technically requires full tree reconstruction (GMM refit, summary regeneration)
- EraRAG (2025): LSH + merge-split strategy confines changes to localized regions — academic, not production
- GraphRAG: Same problem — re-run entity extraction, Leiden, and community summaries. No published incremental update mechanism as of early 2026

**Ref**: [EraRAG](https://arxiv.org/html/2506.20963v2)

### Information Loss

**RAPTOR losses:**
- ~4% hallucination rate from LLM summarization (Stanford analysis)
- Numerical precision, dates, named entities dropped at higher levels
- Cross-topic chunks not soft-assigned to right clusters are absent
- UMAP dimensionality reduction is inherently lossy

**GraphRAG losses:**
- Narrative, implicit knowledge, procedural content dropped at entity extraction
- Higher community summaries sacrifice precision for coverage
- Leiden is non-deterministic — same corpus may produce different communities

---

## 2. Agent Memory Benchmarks

### Core Problem
Agents with near-saturated performance on recall benchmarks fail at real agentic tasks. Static memorization ≠ functional memory. (MemoryArena, 2026)

### Recall/Conversational Benchmarks
- **LoCoMo**: Multi-session dialogue, event-grounded QA (single-hop, multi-hop, temporal, open-domain). Most widely used.
- **LongMemEval**: Temporal reasoning, knowledge conflicts, 115k+ tokens. SOTA: 71.43% multi-session (Supermemory)
- **MSC**: Earlier ACL benchmark, now considered too easy

### Functional/Agentic Benchmarks
- **MemoryArena (2026)**: SOTA for functional memory. Four domains: web shopping, travel planning, progressive search, sequential reasoning. Key finding: current systems fail here even when acing recall benchmarks.
- **MemoryAgentBench (2025)**: Four competencies: retention, update, retrieval, **conflict resolution**. No single approach excels at all four.

### Multi-Document Synthesis Benchmarks
- **LongBench**: Long-context comprehension, 2-5 document integration
- **LOFT**: Google. Up to 1M tokens. SQL-style reasoning, multi-hop QA, KV lookup
- **HiCBench (2025)**: Hierarchical chunking quality. "Cascade recall" — do relevant passages survive each retrieval stage?
- **DocRAG-Bench (2025)**: Multimodal RAG, 1600 QA pairs

### Measuring Understanding vs. Retrieval
- Contrast sets (answer + contrastive variant)
- Compositional generalization tests
- Conflict resolution tasks (MemoryAgentBench)
- Cascade recall (HiCBench)

### Critical Gap
**No benchmark evaluates question-indexed knowledge structures vs. topic-clustered or entity-graph structures.** A new eval combining MemoryArena + HiCBench would itself be publishable.

---

## 3. Question Decomposition Full Landscape

### Foundational: QDMR (Question Decomposition Meaning Representation)
- Allen AI + Tel Aviv, 2020
- Represents complex questions as ordered DAG of atomic steps
- Break dataset: 83,978 questions from 10 benchmarks
- Any valid QDMR compiles to pseudo-SQL
- Defines "atomicity": leaf step requires single lookup

**Ref**: [QDMR paper](https://arxiv.org/abs/2001.11770)

### Multi-Hop QA Benchmarks
- **HotpotQA**: 113K questions, 2-hop. Now "too solvable" — shortcut exploitation
- **MuSiQue**: Most rigorous. DAG-structured, masking ensures each hop is required
- **2WikiMultiHopQA**: Structured 2-hop across Wikipedia articles
- **Bamboogle**: Adversarial, 125 questions, defeats statistical shortcuts
- **MEQA** (NeurIPS 2024): Event-centric reasoning

### Failure Modes (March 2026 empirical analysis)
| Mode | Frequency |
|---|---|
| Global planning absence (no decomposition produced) | 44.96% |
| Unfaithful execution (decomposition ignored) | 28.45% |
| Compositional drift | Documented in HotpotQA |
| Premature collapse | Common on 2-hop |
| Decomposition hallucination | ~15% on Bamboogle |
| Granularity mismatch | Core QDMR problem |
| Error propagation | Sequential pipelines |

### Decomposition Architectures
1. **Sequential/Pipeline**: All sub-questions upfront, sequential answering. Brittle.
2. **Iterative/Interleaved (IRCoT, BEAM)**: One sub-question → answer → next sub-question. Much better.
3. **Factored (Anthropic 2023)**: Isolated context per sub-question. Best for faithfulness.
4. **HQDT/RoHT (ACL 2023)**: Persistent tree during question processing. **Closest to Understanding Pyramid but inference-time only, not persistent.**
5. **GenDec**: Seq2seq trained decomposition generation.

### Good Decomposition Criteria
1. Atomicity: leaf = single lookup
2. Completeness: all sub-answers sufficient for root
3. Independence: sub-questions answerable independently when possible
4. Faithfulness: reflects actual reasoning path

### The Persistent Structure Gap
ALL systems treat decomposition as inference-time. No published system uses it as durable scaffold. Panini (Feb 2026) is closest but answer-indexed, not question-indexed.

---

## 4. Economic Mechanism Design for Knowledge Contribution

### Theory
- **VCG mechanism**: Pay contributors their externality. Truthful reporting as dominant strategy. Problem: requires knowing realized value (counterfactual), can violate budget balance.
- **Information goods**: Non-rivalrous → standard pricing breaks. Lemons problem. Solutions: IP law, reputation, bundling.
- **Wikipedia economics (HBS)**: Social benefits primary driver. Chinese Wikipedia block study: contributions dropped non-linearly when community shrank.
- **Prediction markets**: Aggregate dispersed information into prices. 2014 ICML: prediction market + collaboration platform is self-sustaining, doesn't crowd out intrinsic motivation, filters low quality through creative destruction.

### Real-World Systems
**Wikipedia works because**: Social identity, low marginal contribution cost, visible impact. **Fails on**: contested topics (cost of defending good edits > benefit), quality stagnation.

**Stack Overflow collapsed because**: Reputation rewarded answering AND gatekeeping → incumbent-contributor conflict → newcomers hostile experience → escape valve via LLMs → 50% traffic drop by 2026.

**Open source sustained by**: Employer-sponsored work, signaling value (GitHub as resume), personal utility. Benkler conditions: high modularity, low granularity, low integration costs.

### Emerging Approaches
- **ASI Alliance** (Fetch.ai + Ocean + SingularityNET, July 2024): Compute-to-Data without data leaving source. Pricing thin-market and volatile.
- **Token-Curated Registries (TCRs)**: Stake to vouch, stake to challenge, redistribute loser's stake. Works in narrow domains, fails when quality criteria are contested.

### Five Conditions for Self-Sustaining Knowledge Incentives
1. Value capture proportional to value creation (pay for downstream use, not submission)
2. Non-excludable but attributable
3. Low minimum viable contribution
4. Social and epistemic diversity maintained
5. Separation of curation from gatekeeping

### Incentive Collapse Patterns
| Pattern | Example |
|---|---|
| Incumbent capture | Stack Overflow |
| Commons tragedy | Undersized wikis |
| Quality-quantity inversion | Early Wikipedia stubs |
| Network thinning | Chinese Wikipedia post-block |
| Token circularity | Failed TCRs |
| LLM substitution | Stack Overflow 2024-2026 |

---

## 5. Self-Organizing Knowledge Systems

### Stigmergy: The Unifying Principle
Indirect coordination through environmental traces. Four principles:
1. Agents modify shared environment
2. Modifications persistent and visible
3. Future agents respond to accumulated traces
4. Feedback loops amplify quality

Key insight: **the environment itself must do most of the work** — the structure should make obvious what needs doing next.

### Wikipedia Self-Organization
**Succeeds**: Vandalism reversion (swarm), article density proportional to demand, emergent link structure
**Fails**: Systemic bias (amplifies participant demographics), elite entrenchment, quality maintenance gap (bots handle vandalism but not slow erosion), wiki-gangs exploit stigmergy

### MIT Crystal Framework (Buehler Lab, Feb 2025)
- LLM examines current graph → generates missing/underconnected concepts → merge with deduplication → serialize graph as context → repeat
- **Emergent**: Scale-free network, bridge nodes, flattening centrality distributions, open-ended growth
- **Applied**: Materials design cross-domain insights validated experimentally
- **Limitations**: Not grounded in source docs (can hallucinate relationships), no internal quality signal, superlinear compute scaling

**Ref**: [arxiv Crystal](https://arxiv.org/abs/2502.13025)

### Knowledge Improving Through Querying
- Learning-by-doing KMS (ScienceDirect 2025): Query resolution creates reusable knowledge nodes
- Amazon Q topic curation: Usage analytics inform disambiguation
- Recommendation systems (Netflix/Spotify): Latent embedding improves entirely through use

**Gap**: No system applies usage-driven self-improvement to hierarchical question-indexed structures specifically.

### Conditions for Quality vs. Noise
**For quality**: Ground truth signal exists, trace granularity matches contribution granularity, balanced positive/negative feedback, agent diversity maintained
**For noise**: No quality/popularity distinction, coordination costs exceed contribution benefits, external adversarial pressure

**Deepest theoretical result**: Self-organization produces reliable quality only when quality and fitness are aligned — when what survives is what's actually true and useful.
