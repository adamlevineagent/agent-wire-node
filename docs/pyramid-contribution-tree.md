# Pyramid Builder — Wire Contribution Tree

> **Status:** Design — March 27, 2026
> **Updated after:** Stage 1 audit (two independent auditors), identity system review
> **Cross-references:** [wire-handle-paths.md](../../Core%20Selected%20Docs/wire-handle-paths.md), [wire-identity-system.md](../../Core%20Selected%20Docs/wire-identity-system.md), [unified-chain-architecture.md](../../GoodNewsEveryone/docs/architecture/unified-chain-architecture.md), [question-driven-pyramid-v2.md](question-driven-pyramid-v2.md)

---

## The Skill

**Name**: `build-question-pyramid`
**Type**: Skill (Wire contribution, type: `action`, action_type: `chain`)
**Input**: `{ question: string, source_path: string }`
**Output**: `{ slug: string, node_count: number, depth: number, apex_handle_path: string }`
**Description**: Takes a natural language question and a folder path. Produces a navigable knowledge pyramid that answers the question. Each node is published as a Wire contribution with handle-path identity and evidence-weighted `derived_from` links.

**Permission manifest**:
```json
{
  "permissions": {
    "contribute": true,
    "max_contributions": "$estimated_node_count",
    "max_cost": "$estimated_total_cost"
  }
}
```

Cost estimate is computable from Action 2's output (question tree depth × breadth = node count, × 50 credit deposit each + LLM costs).

---

## Identity Model

Pyramid nodes use the Wire's three-layer identity system:

| Layer | What | Role in Pyramids |
|-------|------|-----------------|
| Master Identity | UUID v4, platform-only, never exposed | Owns reputation, credits, royalty aggregation |
| Pseudo-ID | `wire_agent_<hex>`, public | Tags each contribution for privacy |
| Handle | Human-readable name (`playful`) | Used in handle-paths: `playful/84/7` |

Every pyramid node becomes a Wire contribution with a handle-path: `{handle}/{epoch-day}/{sequence}`. This IS the node's permanent identity. The local slug is a workspace convenience; the Wire uses handle-paths.

**Reference types in `derived_from`**:
- `{ ref: "playful/84/3", weight: 0.38 }` — handle-path (for citing other pyramid node contributions)
- `{ corpus: "vibesmithy/src/auth.ts", weight: 1.0 }` — corpus path (for citing source files)

`source_type` is either `contribution` or `corpus_document`. (`edition_item` is deprecated.)

---

## The Actions (in execution order)

### Action 1: `characterize-material`

**Wire operation**: `llm` (primitive: `classify`)
**Input**: `{ question: string, folder_map: FileEntry[] }`
**Output**: `{ material_profile: string, interpreted_question: string, audience: string, tone: string }`
**Contributes**: Nothing (step output only)

Reads the folder structure (file names, extensions, directories — no content) AND the user's question. Interprets the question in context of the material. Determines audience and tone.

**Conversation checkpoint**: Output is presented to the user for confirmation/correction before proceeding. Implemented as a two-chain pattern: Chain A runs Action 1 and returns. Chain B (triggered by user confirmation with corrections applied) runs Actions 2-10+.

---

### Action 2: `decompose-question`

**Wire operation**: `llm` (primitive: `interrogate`, recursive)
**Input**: `{ interpreted_question: string, material_profile: string, audience: string }`
**Output**: `{ question_tree: QuestionNode[], leaf_questions: string[], depth: number }`
**Contributes**: `question_set` contribution (the question tree itself is publishable/forkable)

Decomposes the interpreted question into the minimum set of sub-claims that, if each proven from evidence, constitute a complete answer. Each sub-question is marked as leaf (answerable from source) or branch (needs further decomposition).

Recurses on branches until all leaves are identified. Each decomposition level is one LLM call that sees ALL sibling questions (horizontal awareness). No hardcoded ranges — the material determines how many sub-questions exist.

---

### Action 3: `generate-extraction-schema`

**Wire operation**: `llm` (primitive: `draft`)
**Input**: `{ question_tree: QuestionNode[], leaf_questions: string[], material_profile: string, audience: string }`
**Output**: `{ extraction_prompt: string, topic_schema: TopicField[], orientation_guidance: string }`
**Contributes**: Nothing (step output only)

Reads all leaf questions and generates the L0 extraction prompt — what to look for in each file, derived from what downstream questions need. Also generates the topic schema (what fields nodes should have) and orientation guidance (how detailed, what tone).

This replaces ALL hardcoded content-type prompt files. No `code_extract.md`, no `doc_extract.md` — the question determines what to extract.

---

### Action 4: `extract-source-material`

**Wire operation**: `llm` (primitive: `extract`, forEach parallel, concurrency: 8)
**Input**: `{ chunks: Chunk[], extraction_prompt: string, topic_schema: TopicField[] }`
**Output**: `{ l0_nodes: L0Node[] }`
**Contributes**: One `pyramid_node` contribution per file (depth 0)

Each L0 contribution:
```yaml
handle_path: "playful/84/3"
type: pyramid_node
body: <distilled orientation text>
structured_data:
  depth: 0
  topics: [...]
  entities: [...]
  question_id: <leaf question this serves>
derived_from:
  - { corpus: "vibesmithy/src/app/page.tsx", weight: 1.0 }
```

L0 nodes cite corpus documents (source files) via `corpus:` path. Rotator arm: 76 creator slots / 2 Wire / 2 Graph Fund (original work).

---

### Action 5: `generate-grouping-schema`

**Wire operation**: `llm` (primitive: `classify`)
**Input**: `{ question_tree: QuestionNode[], l0_nodes: L0Node[] (compact), material_profile: string }`
**Output**: `{ clustering_prompt: string, grouping_criteria: string, connection_types: string, synthesis_prompts: SynthesisPrompt[] }`
**Contributes**: Nothing (step output only)

Now that L0 is complete, the system knows what's IN the material. Generates all downstream prompts:
- Clustering prompt (how to group L0 topics)
- Grouping criteria (what makes items belong together)
- Connection types (what cross-references matter for this question)
- Synthesis prompts (one per non-leaf question — how to answer that sub-question)

---

### Action 6: `cluster-topics`

**Wire operation**: `llm` (primitive: `classify`)
**Input**: `{ l0_nodes: L0Node[] (compact: headlines + topic names only), clustering_prompt: string }`
**Output**: `{ thread_assignments: ThreadAssignment[] }`
**Contributes**: Nothing (step output only)

Groups L0 topics into threads. Uses default model (mercury-2) for small inputs; escalates to qwen only when compacted input exceeds ~100K chars.

---

### Action 7: `synthesize-threads`

**Wire operation**: `llm` (primitive: `synthesize`, forEach parallel, concurrency: 5)
**Input**: `{ thread: ThreadAssignment, l0_nodes: L0Node[], synthesis_prompt: string, topic_schema: TopicField[] }`
**Output**: `{ l1_node: L1Node, evidence: EvidenceLink[] }`
**Contributes**: One `pyramid_node` contribution per thread (depth 1) with evidence-weighted `derived_from`

Each L1 contribution:
```yaml
handle_path: "playful/84/15"
type: pyramid_node
body: <synthesized answer to sub-question>
structured_data:
  depth: 1
  topics: [...]
  entities: [...]
  evidence_full: [...]  # Full KEEP/DISCONNECT/MISSING map
  web_edges: [...]       # Added by Action 9
  question: "What problem does it solve?"
derived_from:
  # Only KEEP entries, weights normalized to sum=1.0
  - { ref: "playful/84/3", weight: 0.38, justification: "Describes the spatial exploration concept" }
  - { ref: "playful/84/7", weight: 0.34, justification: "Explains Partner-mediated interaction" }
  - { ref: "playful/84/5", weight: 0.28, justification: "Positions in Wire ecosystem" }
```

**Evidence handling**:
- `KEEP(weight)` → published as `derived_from` entry with normalized weight
- `DISCONNECT` → stored in `structured_data.evidence_full` only (not in `derived_from`)
- `MISSING` → stored in `structured_data.evidence_full` only; optionally generates a `bounty` contribution for the gap

**Weight-to-slot conversion** (at rotator arm level):
1. Normalize KEEP weights to sum = 1.0
2. Multiply by 28 (total source slots)
3. Round using largest-remainder method to guarantee integer sum = 28
4. If >28 KEEP sources, prune to top 28 by weight

---

### Action 8: `reconcile-layer`

**Wire operation**: `transform` (mechanical, no LLM)
**Input**: `{ l1_nodes: L1Node[], l0_nodes: L0Node[], evidence_links: EvidenceLink[] }`
**Output**: `{ orphans: string[], gaps: GapReport[], weight_map: WeightMap }`
**Contributes**: Nothing (diagnostic output only, performed locally before Wire publication)

Identifies:
- **Orphan L0 nodes**: not claimed by any L1 question (gap in question tree)
- **Missing evidence**: L1 questions that said they needed more
- **Central nodes**: L0 nodes with high weight across many L1 questions (cross-cutting concerns)

---

### Action 9: `web-layer`

**Wire operation**: `llm` (primitive: `cross_reference`)
**Input**: `{ nodes: Node[], connection_types: string }`
**Output**: `{ edges: WebEdge[] }`
**Contributes**: Web edges stored in `structured_data.web_edges` on both endpoint node contributions

No separate Wire contribution type for web edges. Instead, each node's `structured_data` carries its web edges. When L1-003 connects to L1-007, both contributions' `structured_data.web_edges` include the edge.

---

### Action 10: `recurse-upward`

**Wire operation**: `converge` block (compile-time expansion to conditional classify + reduce steps)
**Input**: `{ current_layer_nodes: Node[], question_tree: QuestionNode[], synthesis_prompts: SynthesisPrompt[] }`
**Output**: `{ next_layer_nodes: Node[], evidence_links: EvidenceLink[] }`
**Contributes**: `pyramid_node` contributions at each upper layer, each with `derived_from` citing the layer below

At each layer above L1:
1. Cluster current nodes (Action 6 pattern) → `transform` to reshape
2. Synthesize each cluster (Action 7 pattern) → publish with `derived_from`
3. Reconcile (Action 8 pattern)
4. Web (Action 9 pattern)

Repeats until a single apex node remains. The apex synthesis uses the original question as its prompt.

**Publication order**: bottom-up. Each layer's nodes must be published (and receive Wire UUIDs / handle-paths) BEFORE the next layer synthesizes, so `derived_from` can reference them.

---

## Royalty Cascade

When someone accesses the apex (`playful/84/22`):

1. Rotator arm fires on apex → 60% creator (`playful`), 35% sources (L2 nodes by weight)
2. When a source slot lands on L2-000 (`playful/84/19`), L2-000's rotator arm fires → 60% creator, 35% sources (L1 nodes by weight)
3. When a source slot lands on L1-003 (`playful/84/15`), L1-003's rotator arm fires → 60% creator, 35% sources (L0 nodes by weight)
4. L0 nodes' rotator arms → 95% creator (original extraction work, source files are corpus docs not contributions)

The UFF never changes. Multi-hop attenuation is by design — the apex is the most valuable artifact, L0 extractors earn primarily from direct access to their nodes.

---

## Crystallization (Supersession)

When source files change:

1. Delta extraction: "What changed?" → produces change classification
2. New L0 contribution created that `supersedes` the old one:
   ```
   playful/91/3 (supersedes: playful/84/3)
   ```
3. Evidence weight trace upward: which L1 nodes cited the changed L0 with high weight?
4. Affected L1 nodes re-answered → new contributions that `supersede` old ones, citing NEW L0 handle-paths
5. Propagation continues upward until delta attenuates to noise
6. All supersession links form an immutable audit trail

**Publication order**: bottom-up during crystallization too. New L0 published first (get new handle-paths), then new L1 citing the new L0 paths, etc.

---

## Capability Gap Analysis (Updated)

| Capability | Status | Notes |
|-----------|--------|-------|
| File system folder listing | ✅ Exists | Ingest pipeline |
| LLM dispatch (single + parallel + sequential) | ✅ Exists | chain_dispatch.rs, chain_executor.rs |
| Compact input serialization | ✅ Exists | chain_executor.rs (compact_inputs) |
| Structured JSON output | ✅ Exists | chain_dispatch.rs (response_format) |
| Node save (local) | ✅ Exists | db.rs |
| Web edge save (local) | ✅ Exists | db.rs |
| Wire contribution publication | ✅ Exists | unified-chain-architecture Phase 4 |
| Handle-path identity | ✅ Exists | wire-handle-paths.md, handle_path column on contributions |
| `derived_from` with weights | ✅ Exists | Wire contribution model |
| Supersession chains | ✅ Exists | `supersedes` field on contributions |
| Corpus document identity | ✅ Exists | `source_type: 'corpus_document'` |
| Rotator arm (UFF) | ✅ Exists | wire-rotator-arm.md |
| Multi-parent DAG | ✅ Exists (Wire) | Multiple `derived_from` entries per contribution |
| Multi-parent DAG | ⚠️ Partial (local) | `parent_id` is single-parent; needs evidence table for DAG |
| Evidence link tracking | ❌ New | Storage + query for weighted source→target with reasons |
| Reconciliation | ❌ New | Post-synthesis orphan/gap detection (transform, no LLM) |
| Conversation checkpoint | ❌ New | Two-chain pattern: characterize → confirm → build |
| Dynamic prompt generation | ❌ New | Action output becomes next action's prompt |
| Weight-to-slot conversion | ❌ New | Normalize floats → 28 integer slots (largest-remainder) |

### New capabilities needed (5):
1. **Evidence links** — local storage + query for weighted connections with reasons
2. **Reconciliation** — transform step for orphan/gap detection
3. **Conversation checkpoint** — two-chain split (characterize chain + build chain)
4. **Dynamic prompt generation** — action output flows as prompt to next action
5. **Weight-to-slot conversion** — normalize evidence weights to 28 rotator arm slots

### Capabilities needing modification (2):
6. **Multi-parent children (local)** — migrate from `parent_id` to evidence table for DAG
7. **Recursive decomposition** — make fully LLM-driven instead of pattern-based

---

## What This Replaces

When implemented:
- All content-type-specific prompt files → replaced by dynamic prompt generation (Actions 3, 5)
- The defaults adapter → replaced by question compiler emitting action chains
- The legacy chain executor → replaced by IR executor (already validated)
- Content-type detection and routing → the question determines everything
- Hardcoded thread counts, model selection, file sizes → generated dynamically

What remains:
- The IR executor (runs Wire action chains)
- The expression engine (resolves `$step.field` references)
- The converge expansion (for recursive upward synthesis)
- DB operations (node save, edge save, evidence save)
- The LLM dispatch (sends prompts, parses JSON)
- The Wire publication layer (contribute, supersede, derived_from)
