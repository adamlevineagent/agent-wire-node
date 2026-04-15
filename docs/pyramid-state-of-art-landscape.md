# Knowledge Pyramid: State of the Art Landscape Analysis

> **Date**: April 6, 2026
> **Source**: Perplexity research synthesis + Partner analysis
> **Purpose**: Contextualize the Knowledge Pyramid within the current research and industry landscape

---

## Summary

The Knowledge Pyramid sits at the intersection of five active research fronts. Each component has partial precedents, but the combination — a persistent, question-indexed knowledge pyramid that grows through use, composes across independent knowledge bases via lazy question propagation (vines), and rewards contributors economically — does not exist as a unified system anywhere in published literature or industry implementations.

The most defensible novelty is the use of **question decomposition as the durable organizational primitive** rather than topic clustering (RAPTOR), entity-relationship extraction (knowledge graphs), or runtime-only query strategies (QDT).

---

## Landscape by Dimension

### 1. Hierarchical Knowledge Synthesis

**Best analog**: RAPTOR (Stanford, 2024) — recursively clusters and summarizes document chunks into a tree with LLM-generated abstractions at each level. 20% accuracy improvement on complex QA.

**Also relevant**: HiRAG (2025) — hierarchical knowledge graph with bridging mechanism linking entity-level facts to community-level summaries for multi-hop reasoning.

**Gap**: Both are static (built offline, don't improve through use) and organize by topic clustering rather than question decomposition.

- [RAPTOR paper](https://arxiv.org/html/2401.18059v1)
- [HiRAG paper](https://arxiv.org/html/2503.10150v3)

### 2. Cross-KB Composition Without Merging

**Best analog**: Federated Knowledge Graphs — each domain maintains its own graph with own schema; cross-domain reasoning via explicit "bridge" mappings (equivalence relations, translation rules).

**Gap**: Bridges are hand-curated. LLM-native federation where an agent reasons across KB boundaries on-the-fly remains an unsolved problem in production systems. The vine architecture solves this through lazy question propagation without manual bridge curation.

### 3. Self-Accreting Knowledge Through Use

**Best analog**: Letta/MemGPT — agents manage their own memory hierarchy with editable memory blocks and "sleep-time compute" for restructuring during idle periods.

**Also relevant**: MIT agentic autonomous graph expansion framework (Feb 2025) — iteratively adds concepts, generates follow-up questions from its own evolving structure, produces emergent hub/bridge patterns without manual curation.

**Gap**: No persistent question scaffold. Neither system organizes accreted knowledge around the questions that generated it.

- [Letta/MemGPT analysis](https://thebigdataguy.substack.com/p/agentic-ai-agent-memory-and-context)
- [MIT Crystal approach](https://atalupadhyay.wordpress.com/2025/03/03/building-self-organizing-knowledge-graphs-with-ai-agents-the-crystal-approach/)

### 4. Question-Driven Decomposition as Organizing Principle

**Most novel axis. Real gap in ecosystem.**

**Closest**: Question Decomposition Tree (2023) — uses questions as organizing structure but only at inference time; doesn't build persistent structures.

**Also relevant**: HCAG (2026) — multi-resolution knowledge graph where nodes are annotated with questions they answer, but domain-specific (code repos) and not generalized.

**Gap**: No existing system uses question decomposition as the *durable scaffold* where answering enriches the structure for future queries. This is the Knowledge Pyramid's core differentiator.

- [QDT paper](https://arxiv.org/abs/2306.07597)
- [HCAG paper](https://arxiv.org/html/2603.20299v1)

### 5. Economic Incentive Mechanisms

**Closest**: Microsoft Research crowdsourcing mechanism design (approval voting + incentive-compatible compensation). Cross-platform crowdsourcing incentive mechanisms.

**Emerging**: Agent Labor Market concept (Van der Schaar Lab, 2025) — AI agents buy, hire, and contract skills in open markets.

**Gap**: No integration with LLM-based knowledge systems. The Wire's revenue model (pyramid owners get 95% of synthesis royalties, question-askers earn citation royalties for good questions) is entirely novel in this space.

- [Microsoft Research](https://www.microsoft.com/en-us/research/publication/approval-voting-and-incentives-in-crowdsourcing/)
- [Van der Schaar slides](https://www.vanderschaar-lab.com/wp-content/uploads/2025/12/AI4NextGen-slides.pdf)

---

## Comparison Table

| Dimension | Best Existing Analog | Key Gap | Our Solution |
|---|---|---|---|
| Hierarchical synthesis | RAPTOR, HiRAG | Static, not query-driven | Recursive synthesis rule, question-driven |
| Cross-KB federation | Federated KGs | Manual bridge curation | Vine: lazy question propagation |
| Self-accreting knowledge | Letta, MIT crystal graph | No persistent question scaffold | DADBEAR + annotation accretion |
| Question-driven organization | QDT (inference-only) | No durable structure | Question pyramid as organizing primitive |
| Economic incentives | Crowdsourcing mechanism design | Not integrated with LLM pipelines | Wire credit economy + UFF royalties |

---

## Strategic Assessment

The combination is genuinely novel. Key strategic implications:

1. **Question scaffold is the core differentiator** — everyone else clusters by topic/entity. Scaffolding by what you're trying to understand is fundamentally different and explains why agents navigate pyramids efficiently.

2. **Vine architecture fills the identified gap** in LLM-native federation — no manual bridge curation, economically self-incentivizing.

3. **Economic layer is the moat** — nobody else is integrating incentive mechanisms with LLM knowledge synthesis. The insight that good questions are productive labor makes the system self-organizing at network scale.

4. **Working implementation vs. theory** — most entries in this landscape are papers or prototypes. The Knowledge Pyramid has a working build pipeline, CLI, and MCP server.
