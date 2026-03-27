# Question Pyramid Builder — Wire Action Chain Specification v3

> **Status:** Canonical — March 27, 2026
> **Supersedes:** pyramid-contribution-tree.md, question-driven-pyramid-v2.md (action sequence only)
> **Canonical behavioral spec:** question-driven-pyramid-v2.md (evidence model, pre-mapping, reconciliation)
> **Canonical economic spec:** wire-rotator-arm.md, wire-credit-economy.md
> **Canonical identity spec:** wire-handle-paths.md, wire-identity-system.md
> **Canonical crystallization spec:** progressive-crystallization-v2.md

---

## What This Is

A Wire skill that takes a question and a folder, and produces a navigable knowledge pyramid published as Wire contributions. Every node earns royalties through the UFF. Every connection has a weight and a reason. The pyramid gets smarter through use.

The skill is itself a Wire contribution — forkable, improvable, market-competitive.

---

## Identity

Pyramid nodes use the Wire's three-layer identity system:

- **Master Identity** — UUID v4, platform-only. Owns reputation, credits.
- **Pseudo-ID** — `wire_agent_<hex>`, public. Tags contributions for privacy.
- **Handle** — human-readable name in handle-paths: `playful/84/7`

Every pyramid node gets a handle-path at publication. Handle-paths are the permanent external identity. Local node IDs (`C-L0-012`, `L1-003`) are workspace conveniences that map to handle-paths at the Wire boundary.

**Reference formats in `derived_from`:**
- `{ ref: "playful/84/3", source_type: "contribution", weight: 0.38 }` — citing another pyramid node
- `{ ref: "vibesmithy/src/auth.ts", source_type: "source_document", weight: 1.0 }` — citing a source file

---

## The Build Loop

Everything flows from one question. The build has five phases. Each phase completes before the next begins.

### Phase 1: Architecture

No source files are read. This is pure question design.

**Step 1.1 — Characterize**

Input: user's question + folder path.

The system reads the folder map (file names, extensions, directories — no content) AND the user's question. One LLM call interprets the question in context of the material: what kind of material is this, what is the user actually asking, who is the audience, what tone.

Output: `{ material_profile, interpreted_question, audience, tone }`

**User checkpoint.** The output is presented to the user in Partner chat for confirmation or correction. The build does not proceed until the user confirms. This is implemented as a two-chain pattern: Chain A characterizes and returns. Chain B (triggered by confirmation) runs everything else.

**Step 1.2 — Decompose**

Input: confirmed interpretation + material profile.

The system decomposes the question into the minimum set of sub-claims that, if each proven from evidence, constitute a complete answer. Each sub-question is marked as leaf (answerable from source) or branch (needs further decomposition).

Recursion: branches decompose further. Each decomposition level is one LLM call that sees ALL sibling questions (horizontal awareness — no duplicates, no gaps). Recursion stops when every sub-question is a leaf.

No hardcoded ranges. The material determines how many sub-questions and how deep.

Output: `{ question_tree, leaf_questions, depth }`

**Step 1.3 — Generate Extraction Schema**

Input: all leaf questions + material profile + audience.

From the complete question tree, the system generates:
- **Extraction prompt** — what to look for in each file, derived from what leaf questions need. NOT "list every function" — specifically what the downstream questions require.
- **Topic schema** — what fields each node should have (varies by question: a security pyramid needs `trust_boundaries`, a product overview needs `user_features`).
- **Orientation guidance** — how detailed, what tone.

Output: `{ extraction_prompt, topic_schema, orientation_guidance }`

---

### Phase 2: L0 Extraction

**Step 2.1 — Extract**

Input: chunks (source files) + extraction prompt + topic schema.

Run the extraction prompt on each source file. Parallel, 8x concurrency, mercury-2. One L0 node per file, saved LOCALLY in `pyramid_nodes`. Not published yet — orphans haven't been identified.

Output: `{ l0_nodes[] }` — saved locally, not on Wire.

---

### Phase 3: Bottom-Up Answering

This is the core loop. It runs once per layer, starting from L0 and working up to the apex. Each layer has five steps that run in strict order.

**For each layer (starting at L0 → L1, then L1 → L2, etc.):**

**Step 3.1 — Horizontal Pre-Mapping**

Input: all questions at this layer + all nodes from the layer below.

One LLM call reads ALL questions for this layer and ALL completed nodes below, and produces candidate connections:

"Question L1-003 ('How does auth work?') should draw from: L0-012 (has validate_token), L0-045 (defines UserSession), L0-067 (mentions auth in a comment), L0-089 (token refresh)."

The pre-mapping intentionally OVER-INCLUDES. Better to give a question a false positive than miss a real connection. The answering step will prune.

Output: `{ candidate_map: { question_id → [candidate_node_ids] } }`

**Step 3.2 — Vertical Answering**

Input: each question + its candidate nodes (full content) + synthesis prompt from Phase 1.

Parallel, 5x concurrency. Each question is answered independently. The prompt contains:
1. The question itself
2. The relevant schema fields
3. The pre-mapped candidate nodes
4. Instruction: "Answer this question. For each candidate, report KEEP(weight, reason), DISCONNECT(reason), or MISSING(what you wish you had)."

Output per question:
```json
{
  "headline": "Auth Token Lifecycle",
  "orientation": "3-5 sentences answering the question...",
  "topics": [...],
  "evidence": [
    { "node": "L0-012", "verdict": "KEEP", "weight": 0.95, "reason": "Contains validate_token() implementation" },
    { "node": "L0-045", "verdict": "KEEP", "weight": 0.70, "reason": "Defines UserSession struct" },
    { "node": "L0-067", "verdict": "DISCONNECT", "reason": "Mentions auth in a comment but no auth logic" },
    { "node": "L0-089", "verdict": "KEEP", "weight": 0.85, "reason": "Token refresh and expiry handling" }
  ],
  "missing": ["Would benefit from Supabase configuration details"]
}
```

Nodes saved LOCALLY in `pyramid_nodes`. Evidence saved in `pyramid_evidence`. Not published yet.

**Step 3.3 — Reconciliation**

No LLM call. Mechanical aggregation.

Input: all answered questions at this layer + all nodes from layer below + all evidence links.

Identifies:
- **Orphan nodes**: nodes from the layer below that no question claimed (KEEP or DISCONNECT — never even considered). These are gaps in the question tree.
- **Gap reports**: questions that reported MISSING evidence. Each gap can optionally generate a bounty contribution.
- **Central nodes**: nodes cited by many questions with high weight (cross-cutting concerns).

Output: `{ orphans[], gaps[], central_nodes[], weight_map }`

**Step 3.4 — Web Edges**

Input: all answered nodes at this layer + connection types from Phase 1.

One LLM call identifies cross-references between sibling nodes at this layer. Connection types are tuned to the question (shared tables for a dev pyramid, shared concerns for a strategy pyramid).

Output: `{ edges[] }` — saved to `pyramid_web_edges` locally AND stored in `structured_data.web_edges` on each endpoint node.

**Step 3.5 — Publish**

Input: all confirmed (non-orphan) nodes at this layer + evidence links + web edges.

Publish each node as a Wire contribution. Bottom-up order within the layer (if any dependencies exist).

Each contribution:
```yaml
type: pyramid_node
handle_path: <assigned by Wire>
body: <distilled orientation text>
structured_data:
  depth: <layer>
  topics: [...]
  entities: [...]
  evidence_full: [...]  # Complete KEEP/DISCONNECT/MISSING map
  web_edges: [...]
  question: "The sub-question this node answers"
  gaps: [...]  # MISSING items
derived_from:
  # Only KEEP entries, weights normalized to sum=1.0
  - { ref: "<handle-path of source node>", source_type: "contribution", weight: 0.38, justification: "..." }
  - { ref: "<handle-path of source node>", source_type: "contribution", weight: 0.34, justification: "..." }
```

For L0 nodes, `derived_from` cites source files:
```yaml
derived_from:
  - { ref: "<corpus path>", source_type: "source_document", weight: 1.0, justification: "Extracted from source file" }
```

Orphan L0 nodes are NOT published. Credits saved.

After publication, the Wire returns handle-paths. These are stored in `pyramid_id_map` (local_id → handle_path). All subsequent layers reference published nodes by handle-path.

**Repeat Steps 3.1-3.5** for the next layer up, until a single apex remains.

---

### Phase 4: Apex

The final iteration of the loop produces one apex node answering the original question. It's published like any other node, with `derived_from` citing the top-layer nodes below it.

The apex answer uses the original user question as its synthesis prompt. It should read as a direct, complete answer to what was asked.

---

### Phase 5: Crystallization (on source change)

When source files change, the system asks "What changed and what does that affect?" Two propagation channels, as specified in progressive-crystallization-v2.md:

**Channel A — Weight-Based Staleness**

A file changes → delta extraction classifies the change (ADDITION / MODIFICATION / SUPERSESSION) → trace evidence weights upward. A high-weight evidence link (0.95) means the question's answer is probably stale. A low-weight link (0.1) means probably not. Configurable threshold determines which questions get re-answered.

Staleness attenuates through layers. The operator can dismiss a staleness flag if they determine it's irrelevant.

**Channel B — Belief Supersession**

A change contradicts a specific claim in the pyramid. "validate_token() now checks expiry" directly supersedes "validate_token() does not check expiry." This traces through every node that contains the superseded claim, regardless of weight. It does NOT attenuate. It cannot be dismissed.

**Re-answering:**

Affected questions are re-answered using the same pre-mapping → answering → reconciliation loop. New nodes `supersede` old Wire contributions. The `structured_data.supersession_history` carries the correction audit trail:
```json
{
  "superseded_claim": "validate_token() does not check expiry",
  "corrected_to": "validate_token() checks expiry with 5-minute window",
  "source": "playful/91/3",
  "channel": "belief_supersession"
}
```

Publication order is bottom-up during crystallization: new L0 published first (get new handle-paths), then new L1 citing the new L0 paths, and so on upward.

---

## Royalty Cascade

The UFF applies uniformly. No distinction between mechanical and intelligence contributions — it's all agents.

When someone accesses a pyramid node:
1. Rotator arm fires: 48 creator slots, 28 source slots (by evidence weight), 2 Wire, 2 Graph Fund
2. Source slots cascade: each cited node has its OWN rotator arm
3. Cascade continues to L0
4. L0 nodes' rotator arms: 76 creator slots (original extraction), 2 Wire, 2 Graph Fund — or 48/28/2/2 if citing source documents that are themselves Wire contributions

Multi-hop attenuation is by design. The apex is the most valuable artifact. L0 extractors earn primarily from direct access to their nodes, not from apex trickle-down.

**Weight-to-slot conversion:**
1. Normalize KEEP weights to sum = 1.0
2. Multiply by 28 (total source slots)
3. Round using largest-remainder method to guarantee integer sum = 28
4. Every source gets minimum 1 slot
5. If >28 sources: prune to top 28 by weight
6. Zero-weight entries rejected (weight must be > 0)

---

## Permission Manifest

```json
{
  "permissions": {
    "contribute": true,
    "max_contributions": "<source_file_count + upper_layer_estimate + overhead>",
    "max_cost": "<deposits + LLM costs>"
  }
}
```

Cost estimation: `source_file_count` from folder map (Phase 1), upper layer estimate from question tree depth × breadth, 50 credits per contribution deposit + LLM token costs.

---

## Wire Enhancements Required

These capabilities don't exist yet but are needed for the full system:

### 1. `pyramid_node` as First-Class Type
Add to contribution type enum. Already in migration schema, needs formal support in validation + queries.

### 2. Contribution Annotation Without Supersession
Web edges enrich published contributions post-publish. Options:
- `PATCH /api/v1/contributions/:id/structured_data` (additive merge)
- Separate `annotation` contribution type attached to parent
Core `body` stays immutable; only `structured_data` sub-fields enrichable.

### 3. Batch Publication with Handle-Path Reservation
350 sequential publishes is slow. Either:
- `POST /api/v1/contribute/batch` with ordered results
- Handle-path reservation: `POST /api/v1/handle-paths/reserve { count: 350 }`

### 4. Corpus Document Citation End-to-End
Verify: corpus sync → document UUID stability → `derived_from` acceptance with `source_type: "source_document"` → rotator arm routing to corpus contributor.

---

## Local Schema Requirements

Seven tables needed beyond current `pyramid_nodes`:

| Table | Purpose |
|-------|---------|
| `pyramid_evidence` | Many-to-many weighted evidence links (node → node, KEEP/DISCONNECT/MISSING, weight, reason) |
| `pyramid_question_tree` | Question decomposition tree per slug |
| `pyramid_gaps` | Missing evidence reports from answering |
| `pyramid_id_map` | Local node ID → Wire handle-path mapping |
| `pyramid_deltas` | Per-file change log for crystallization |
| `pyramid_supersessions` | Belief correction audit trail |
| `pyramid_staleness_queue` | Pending re-answer work items |

Migration must include: CREATE statements, backfill from `children` JSON → evidence table, backward compatibility for existing pyramids.

---

## Code Fixes Required

| Fix | File | Issue |
|-----|------|-------|
| Evidence weights | wire_publish.rs | `derived_from` weights hardcoded to 1.0 — must pass actual evidence weights |
| Source document citation | wire_publish.rs | L0 nodes hardcode `source_type: "contribution"` — must use `"source_document"` for source files |
| Publication idempotency | wire_publish.rs | No `pyramid_id_map` check — re-run creates duplicates. Must be resumable. |
| Crystallization locking | crystallization.rs | No per-node mutex — concurrent deltas can drop corrections |

---

## What This Replaces

When implemented:
- All content-type-specific prompt files (code_extract.md, doc_extract.md, etc.) — replaced by dynamic prompt generation from question decomposition
- The defaults adapter — replaced by question compiler emitting Wire action chains
- The legacy chain executor — replaced by IR executor (validated at parity)
- Content-type detection and routing — the question determines everything
- Hardcoded thread counts, model selection, file sizes — generated dynamically from the question tree

What remains:
- The IR executor (runs Wire action chains)
- The expression engine (resolves `$step.field` references)
- The converge expansion (for recursive upward synthesis, max 10 iterations, stops when single node remains, errors if consecutive iterations don't reduce)
- DB operations (node save, edge save, evidence save)
- The LLM dispatch (sends prompts, parses JSON)
- The Wire publication layer (contribute, supersede, derived_from)

---

## Densify

After the initial build, every interaction makes the pyramid smarter.

A human asks Partner "how does the stale engine work?" Partner reads the pyramid, assembles an answer, and that Q&A pair creates new edges — connections the structural questions never asked about. Each densify answer is itself a contribution with `derived_from` pointing to the nodes it synthesized from.

"Densify" as a command means: dispatch a helper to generate questions about a node, answer them against the pyramid, weave answers back as edges. Automated curiosity. The pyramid gets smarter without anyone asking specific questions.

The pyramid starts sparse (structural scaffold from the build). After a hundred interactions, the webbing is richer than the scaffold. After a thousand, the pyramid knows things no single document ever stated — emergent understanding from accumulated questions.
