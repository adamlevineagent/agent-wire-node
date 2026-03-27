# Audit: Pyramid Contribution Tree vs Architectures

> **Prepared for**: Adam (Partner Handoff)
> **Target**: `pyramid-contribution-tree.md`
> **Date**: March 26, 2026

## Executive Summary

The target document (`pyramid-contribution-tree.md`) aims to unify the Wire contribution schema with the Pyramid building steps. However, the current draft is structurally out of sync with the underlying architectures it references (v3 Question-Driven Architecture and the v2 Progressive Crystallization specs). 

It currently relies on deprecated v2 clustering mechanics instead of question decomposition, completely misses the core "belief-based supersession propagation" required for accurate staleness, and incorrectly specifies Web Edge mutation semantics that violate the immutability of published Wire contributions.

## Critical Architectural Findings

### 1. Reversion to v2 Topic Clustering (Violates `question-driven-pyramid-v2.md`)
- **The Issue**: Action 5 (`generate-grouping-schema`) and Action 6 (`cluster-topics`) describe a bottom-up clustering approach ("Groups L0 topics into threads" via a "clustering_prompt"). Action 10 also explicitly uses `cluster-topics` as a recursion block.
- **The Contradiction**: `question-driven-pyramid-v2.md` explicitly deprecates bottom-up clustering in favor of top-down question definition. *Questions* define the groups. A "Horizontal Pre-Mapping" phase assigns evidence (L0 nodes) to pre-existing questions. 
- **The Fix**: Actions 5 and 6 must be entirely rewritten to implement **Question Pre-Mapping** (Step A) and **Vertical Answering** (Step B). L1 nodes are materialized answers to Question Tree branches, not dynamically generated clusters.

### 2. Missing Belief Supersession Tracing (Violates `progressive-crystallization-v2.md`)
- **The Issue**: In the "Crystallization (Supersession)" section, the plan claims supersession propagates upward via "Evidence weight trace... which L1 nodes cited the changed L0 with high weight?"
- **The Contradiction**: `progressive-crystallization-v2.md` introduces a vital dual-channel propagation: Staleness (weight-based) AND Supersession (text/SQL-based belief-dependency trace). Weight decay is strictly for staleness; belief supersession *does not attenuate* and demands mandatory re-answering with correction directives. The target document entirely omits the Belief Trace, meaning an L2 node with a false claim but low staleness weight will silently remain wrong.
- **The Fix**: The Crystallization procedure and the Gap Analysis must explicitly include the **Belief Dependency Trace** (querying text for superseded entities) and distinct Staleness/Supersession propagation channels.

### 3. Wire Contribution Immutability & Web Edges
- **The Issue**: Action 9 (`web-layer`) states: "No separate Wire contribution type for web edges. Instead, each node's `structured_data` carries its web edges... both contributions' `structured_data.web_edges` include the edge."
- **The Contradiction**: The publication sequence explicitly states "Each layer's nodes must be published BEFORE the next layer synthesizes". If L1 nodes are published in Action 7 (Synthesis) to satisfy Action 10's strict bottom-up order, mutating their `structured_data` later in Action 9 (Web Layer) would require creating completely new Wire contributions (`supersedes`) just to backfill a duplex web edge.
- **The Fix**: Either delay the Wire publication of Layer *N* until *after* Action 9 completes, or commit to modeling web edges as independent Wire Contribution types. Mutating an immutable Wire contribution is a logical fault without a supersession workflow.

### 4. Vocabulary Mismatch with YAML v3 (`question-yaml-format.md`)
- **The Issue**: Action 10 refers to the `converge` block as a "compile-time expansion to conditional classify + reduce steps". 
- **The Contradiction**: `question-yaml-format.md` was explicitly written to obsolete these engine leakages. The YAML parser compiles `about:` and `creates:` rules directly into execution chains. Exposing `primitive: classify` logic in the Contribution Tree design conflates the legacy V2 executor implementation with the V3 declarative syntax.
- **The Fix**: Describe Action 10 in terms of the Question YAML v3 compilation logic (e.g., executing the pre-computed tree of schemas layer by layer) rather than hardcoded primitive routines.

### 5. Node Count Cost Estimation Flaw
- **The Issue**: The permission manifest calculates the `max_contributions` / `node_count` cost strictly based on "question tree depth × breadth". 
- **The Contradiction**: The L0 layer volume is purely dictated by the number of source files evaluated in the folder map. Action 2's tree decomposition doesn't know how many files exist; it only generates the leaf extraction schemas.
- **The Fix**: `node_count` estimation must explicitly factor in `O(source_files)`. The folder map length from Action 1 is the baseline floor for worst-case credit consumption.

## Supplementary Execution Gaps
- **Missing SQLite Engine Tables**: The Capability Gap analysis checklist neglects the local SQL infrastructure defined in the architecture docs (`pyramid_gaps`, `pyramid_orphans` view, `pyramid_deltas`, `pyramid_supersessions`, and `pyramid_staleness_queue`).
- **Orphan Node Cost**: Action 8 spots orphans as diagnostic output. If L0 nodes are published blindly in Action 4, and Action 8 determines they are orphans, do they sit as dangling nodes soaking up 50-credit deposits on the Wire graph indefinitely? The protocol must clarify whether unpublished candidate nodes operate locally until tethered, or if it eats the cost.
