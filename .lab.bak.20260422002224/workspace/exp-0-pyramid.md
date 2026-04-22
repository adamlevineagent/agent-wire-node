# Pyramid: lens-0 (Baseline — 4-lens prompts)

## Apex (L3)
This is an AI collaboration platform that solves the cold-start problem—where every AI session traditionally rebuilds context from scratch at high cost—by maintaining a persistent, self-composing knowledge graph called the Knowledge Pyramid. The platform operates through three interlocking systems: a 5-level recursive synthesis tree (L0-L4) that continuously compresses raw conversation, code, and documents into optimized ~300-token apex contexts, keeping costs flat regardless of conversation length; a Machine Layer that executes declarative action chains server-side and self-extends by posting Build operations when encountering missing capabilities; and Agent Systems (Partner agents with 3-tier memory, Editorial teams with Editor-in-Chief and beat-subscribed Reporters) that interact with users and write discoveries back to the pyramid. The system uses a two-pass temporal architecture where forward pass preserves immutable historical reasoning (carved in stone) while reverse pass captures mutable current state (written in water)—both feeding L1 synthesis. Knowledge evolves through delta chains: ~200-token incremental updates accumulate, then collapse into new canonical nodes after ~50 deltas, creating a supersession chain that IS the thread's history. A DADBEAR pipeline (Detect-Accumulate-Debounce-Batch-Evaluate-Act-Recurse) with 5-minute independent timers per layer maintains freshness. The platform supports multi-tenancy through PostgreSQL Row-Level Security at the editorial boundary while sharing the wire layer (sources, ingest, enrichment) across all tenants. A 9-stage editorial pipeline runs continuously with human override surfaces at critical junctions, and the entire system inverts traditional context-cost economics: helper models ($0.05/M) compress upward continuously so expensive partner models ($1-5/M) always operate on optimized contexts, delivering $0.80/hr standard pricing regardless of conversation length.

## L2 Branches (4)
1. **"What is the core purpose and value proposition?"** — Persistent Intelligence That Compounds: Solving the Cold-Start Problem
2. **"What are the major architectural components and their relationships?"** — Core Platform Architecture: Three Pillars, Four Layers
3. **"How does the system manage data state, knowledge, and temporal freshness?"** — Two-pass temporal architecture with DADBEAR debouncing and delta chains
4. **"What operational capabilities and workflows does the system support?"** — Multi-layered operational engine combining editorial pipelines, agent orchestration

## L1 Answers (21 question-answer nodes)
- What user or business problem does this system exist to solve?
- What measurable or deliverable value does this system provide?
- How does architecture translate capability into delivered value?
- What is the overarching purpose and value proposition?
- How is the data architecture structured across hierarchical layers?
- What mechanisms govern data mutation and versioning?
- How does the system manage data freshness and lifecycle states?
- What is the architecture for job orchestration?
- What agent systems exist and what roles do they play?
- How does the architecture support multi-tenant isolation?
- What are the primary interaction patterns between major components?
- What is the knowledge pyramid structure?
- How do delta-chain mechanisms enable state mutations?
- What auto-stale freshness policies govern data?
- How does the system orchestrate knowledge processing and job scheduling?
- How does multi-tenancy affect knowledge isolation?
- What are the primary operational workflow patterns?
- How does job orchestration operate?
- What capabilities do agent systems provide?
- How does the system handle data movement and state mutation?
- What operational capabilities are provided for multi-tenant environments?

## Sample L0 Nodes (6 of 34)
- Contributions-Back System — feedback loop transforming agent annotations into pyramid intelligence
- Intelligence Operation as Configurable Template — reusable template for intelligence operations
- Delta-Chain Knowledge Pyramid — hierarchical knowledge architecture with delta chains
- Agent Wire Compiler — Machine Layer architecture for compiling declarative intelligence operations
- Unified Job Execution Architecture — single execution model replacing fragmented layers
- Vine Meta-Pyramid Architecture — meta-pyramid synthesizing multiple conversation pyramids

## Stats
- 34 L0, 21 L1, 4 L2, 1 L3 (apex) = 60 nodes
- 134 KEEP, 10 DISCONNECT evidence links
- 0 empty nodes
- Build time: 1244s (~21 minutes)
