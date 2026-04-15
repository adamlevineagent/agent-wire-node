# Knowledge Pyramid Exploration Log — lens-1

**Explorer**: Partner (Antigravity)  
**Date**: 2026-04-05  
**Pyramid**: `lens-1` (document type, 58 nodes, 3 layers)  
**Source**: `/Users/adamlevine/AI Project Files/Core Selected Docs/architecture`  
**Session duration**: ~5 minutes of active exploration  
**Annotations deposited**: 8 (IDs 261-268)  

---

## Method

Systematic top-down exploration starting from apex, drilling each L2 branch, then probing L1 and L0 nodes. Interleaved with search queries (keyword, natural language, edge cases), FAQ testing, annotation writing, and feedback loop velocity measurement.

---

## What Works Well ✅

### P1: Navigation is Fast and Natural
Response times are sub-second for all read operations (apex, drill, search, node). The apex → drill → drill flow feels like a natural zoom — you start with "what is this?" and progressively focus. **No latency friction at all.**

### P2: Evidence Links Are Genuinely Useful
Each L2 node comes with `evidence[]` — a list of L1 sources with weights (0.0-1.0) and written justifications for why each was kept. This is not decoration — an agent can use weights to prioritize which children to drill into. Example: L2-179e had 10 KEEP sources with weights from 0.60 to 0.95, clearly showing which L1 nodes carried the most insight.

### P3: Vocabulary Extraction is a Killer Feature
The apex's `terms[]` field captured 12 domain-specific terms with precise definitions (Optimal Knowledge Problem, Recursive Synthesis, Delta-chain Versioning, Progressive Crystallization, etc.). A cold-start agent reading just the apex immediately learns the system's language. This is arguably the pyramid's highest-value output.

### P4: Corrections and Decisions Capture Is Smart
The apex captured 1 correction ("River-Graph boundary is not a performance optimization, it's an architectural invariant") and 6 architectural decisions with rationale and rejected alternatives. This goes beyond summarization — it captures the *reasoning texture* of the source material.

### P5: FAQ Pipeline is Impressively Fast
Annotations with `--question` context were processed into queryable FAQ entries within seconds. All 5 of my question-annotated findings appeared as FAQs by the time I checked the FAQ directory. The FAQ system went from 1 entry to 7 during my session.

### P6: Annotation Types Are Well-Chosen
The five types (observation, correction, question, friction, idea) are semantically distinct and useful. I naturally used `observation` for findings and `friction` for UX issues without having to think about the taxonomy.

### P7: Gap Identification Provides Research Leads
Each L2 node identifies **gaps** — things the source material doesn't cover. Example gaps from L2-c6f1fa50: "Concrete examples of what a bounty looks like operationally", "How embargo policies on the Wire Graph actually work in practice." These are genuine research directions.

### P8: Self-Prompt as Question Framing
L1 nodes use `self_prompt` as a clear question: "How does the delta-chain versioning system work and why was it chosen over conventional state management?" This immediately tells an agent what intellectual territory the node covers.

### P9: Question Context in Drill
The `question_context` field on drill responses shows both the parent question and sibling questions. This is legitimately helpful for understanding where you are in the knowledge tree and what parallel branches exist.

### P10: Drill Response Is Comprehensive
A single drill response gives you: the node itself, all children (full nodes, not just IDs), evidence links with weights, gaps, and question context. One API call = full situational awareness for that branch.

---

## Friction & Issues ⚠️

### F9: Search is Keyword-Only, Not Semantic
`search lens-1 "how does the system handle failures"` → **0 results**  
`search lens-1 "bonding curve"` → **2 results**  
`search lens-1 "context window cost"` → **17 results**  

Search is FTS/keyword matching. Agents arriving with natural language questions (which is how most agents think) will hit walls. This is the single biggest UX gap.

### F10: Search Scoring is Depth-Only, No Intra-Depth Ranking
Every result at the same depth gets the same score: L0=10, L1=20, L2=30, L3=40. Within a depth level, all results are tied. For `"recursive synthesis"`: L3 got 40, all three L2 nodes got 30, all five L1 nodes got 20, all three L0 nodes got 10. There's no relevance differentiation within a layer.

### F11: Annotations Invisible in Drill Responses
The `_note` on annotation responses claims: *"It is immediately visible via 'annotations' and 'drill'."* But the drill response structure is `{node, children, evidence, gaps, question_context}` — no annotations field. An agent doing top-down drill navigation will never see a single annotation unless it explicitly calls `annotations <slug> [node_id]`.

### F12: No Search ↔ FAQ Cross-Referral
When search returns 0 results, the response is just `[]` — no hint that the FAQ system might have answers. When FAQ returns no matches, no hint to try keyword search. The two retrieval systems exist in isolation.

### F13: `self_prompt` Has Inconsistent Semantics
- In L1 nodes: Contains a question ("How does delta-chain versioning work...?")
- In Q-L0 nodes: Contains the full distilled content (400+ chars of description, not a question at all)
- An agent relying on `self_prompt` as "the question this node answers" will get wrong data from L0 document nodes

### F14: Gaps Marked as "resolved: true" Too Optimistically
Gaps like "Ablation studies showing contribution of each innovation independently" and "Comparison with other self-organizing memory systems in academic literature" are marked `resolved: true`. These are inherently unanswerable from architecture docs — marking them resolved suppresses them as research targets.

### F15: `faq` and `faq-dir` Return Identical Results
Both commands return the same JSON structure. If they're functionally identical, one should be an alias. If they're meant to differ in behavior, the differentiation isn't visible.

### F16: No "Map" or "Tree" Command
There's no way to see the full pyramid shape in one call. An agent must iteratively drill to discover the structure. A `tree lens-1` command showing the hierarchy (ID + headline per node, indented by depth) would dramatically accelerate orientation.

### F17: No Node Count or Depth Hint in Search Results
Search results show `{node_id, depth, snippet, score}`. They don't show how many children a node has or whether it's been annotated. An agent can't tell from search results whether a hit is a leaf node or a rich subtree.

---

## Structural Observations 🔬

### S1: Document Pyramid Shape
58 nodes across 4 layers: 1 apex (L3) → 4 L2 → ~15 L1 → ~38 L0. This is a roughly 4x branching factor, which produces a readable pyramid — each level has 3-6 children, manageable for sequential drill.

### S2: Evidence Redundancy Detection
The evidence system explicitly captures redundancy. For L2-179e, one evidence entry notes: "Redundant with other nodes on core pyramid mechanics but adds explicit evidence for content-type configuration." The system is self-aware about overlap.

### S3: L2 Question Decomposition
The four L2 questions decomposing "What is this?" are:
1. Purpose and problem domain
2. Architectural structure and component organization  
3. Core capabilities and features
4. Distinctive design innovations

This is a reasonable decomposition but heavily architecture-biased. Missing: operational concerns, failure modes, development workflow, deployment topology.

### S4: FAQ Question Generalization
The FAQ system doesn't store my exact questions — it **generalizes** them. My annotation question "Is pyramid search semantic or keyword-based?" became FAQ question "What underlying search mechanism does a system use to find information — semantic/AI-based..." This is good behavior (makes the FAQ more broadly matchable) but means the original specific question is lost.

### S5: Annotation-to-FAQ Pipeline Observations
- **Velocity**: Seconds (all 5 annotations → FAQs by next check)
- **Generalization**: Questions are rewritten to be more generic
- **Match triggers**: Each FAQ gets `match_triggers[]` for auto-matching (different from the canonical question)
- **Hit counting**: The FAQ tracks `hit_count` — a popularity/usefulness signal

### S6: Edge Case Behavior
| Test | Result | Assessment |
|------|--------|------------|
| Empty search query | `Error: missing required argument <query>` | Good — fails clearly |
| Nonexistent node ID | `{"error": "Node not found"}` | Clean but doesn't suggest alternatives |
| Nonexistent slug | `{"error": "No apex node found"}` | Ambiguous — same as "exists but not built" |
| `faq` without query | Returns full directory listing | Good — graceful fallback |

---

## Annotations Deposited

| # | Node | Type | Question |
|---|------|------|----------|
| 261 | L2-179e | observation | How reliable is the gap resolution status? |
| 262 | Q-L0-010 | observation | What does self_prompt contain in document L0 nodes? |
| 263 | L3 (apex) | friction | Is pyramid search semantic or keyword-based? |
| 264 | L2-c554a | observation | Is Newsbleach a newsroom or configurable template? |
| 265 | L1-1075c | observation | What distinguishes high-quality L1 synthesis? |
| 266 | L3 (apex) | friction | Are annotations visible inline during drill? |
| 267 | L2-c6f1f | observation | How well do pyramids extract domain vocabulary? |
| 268 | L3 (apex) | observation | Overall agent navigation experience assessment |

---

## Summary Verdict

The Knowledge Pyramid is a **genuinely useful codebase/document comprehension tool** with a clear information architecture. Its strengths are in structured navigation, vocabulary extraction, evidence-weighted synthesis, and fast annotation feedback loops. Its weaknesses are concentrated in **discoverability for agents who don't know the vocabulary yet** — keyword-only search, no semantic retrieval, and annotations hidden from the drill flow. 

For an agent that knows the system's terms, the pyramid is excellent. For a cold-start agent with natural language questions, it's a wall until they learn to reformulate queries as keywords.

**Priority fixes by impact:**
1. **Inline annotations in drill** — closes the compound knowledge loop
2. **Semantic search or NL→keyword rewriting** — unlocks natural language agents  
3. **Search↔FAQ cross-referral** — prevents dead-end 0-result responses
4. **Intra-depth search ranking** — makes search results actually sortable
5. **`tree` command** — one-call structural overview
