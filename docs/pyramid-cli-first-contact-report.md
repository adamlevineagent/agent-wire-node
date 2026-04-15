# Pyramid CLI: First-Contact Test Report

**Testers**: Antigravity-A (Partner) + Antigravity-B (separate session)  
**Date**: 2026-04-05  
**Target**: `lens-1` — document pyramid, 58 nodes, 3 layers  
**Source corpus**: `Core Selected Docs/architecture`  
**Total annotations deposited**: 10 (8 from A, 2 from B)  
**FAQ entries created**: 9 (from 1 baseline)

---

## What The System Is

The Knowledge Pyramid is a local-first, LLM-powered comprehension engine that transforms source material (code, documents, conversations) into a navigable hierarchical knowledge structure. It solves what the source material itself calls the **"Optimal Knowledge Problem"** — the tension between context-window cost and understanding quality.

### Architecture (discovered via CLI exploration)

| Component | Role |
|-----------|------|
| **Rust backend** (Tauri desktop app) | Chain execution, SQLite storage, HTTP API on `:8765` |
| **Chain definitions** (YAML + Markdown prompts) | Declarative build pipelines — *"Rust is a dumb execution engine"* |
| **CLI** (`cli.ts`) | Agent terminal access — 30+ commands |
| **MCP Server** (`index.ts`) | Tool-connected agent access — 12 tools |
| **DADBEAR** | Auto-update engine: Detect, Accumulate, Debounce, Batch, Evaluate, Act, Recurse |

### Key Concepts Extracted from the Pyramid Itself

- **River-Graph Boundary**: Raw data flows ephemerally; only adversarially-evaluated intelligence earns graph permanence
- **Recursive Synthesis**: 2+ siblings without a parent → collapse into 1. One rule governs the entire structure
- **Delta-Chain Versioning**: ~200-token incremental diffs, canonical collapse after ~50 deltas
- **Progressive Crystallization**: Live (instant) → Warm (seconds) → Crystal (minutes) — three tiers of synthesis cost
- **Forward/Reverse Pass Duality**: Stone preserves history; Water re-weights by current significance

---

## Consolidated Positive Findings

Both testers independently validated these strengths:

### 1. Navigation Speed & Flow
> Sub-second response times on all read operations. The apex → drill → drill flow is natural — you start with "what is this?" and progressively focus. **Zero latency friction.**

### 2. Drill Is The Killer Command
Both testers singled out `drill` as the standout. A single call returns:
- The node itself (headline, distilled synthesis, topics, terms, corrections, decisions)
- All children as full nodes (not just IDs)
- **Evidence links** with weights (0.0-1.0) and written justifications
- **Web edges** — lateral cross-branch connections
- **Gaps** — identified knowledge holes
- **Question context** — parent question + sibling questions

> *Tester B*: "Seeing that Q-L0-020 connected laterally to Q-L0-022 (the actual prompt templates for DADBEAR) gave me immediate, traversable context that a simple RAG search could never achieve."

### 3. Vocabulary Extraction
The apex's `terms[]` field captured 12 domain-specific terms with precise definitions. A cold-start agent reading just the apex immediately learns the system's language.

> *Tester A*: "This is arguably the pyramid's highest-value output."

### 4. Evidence Weights Enable Prioritized Navigation
Agents can use evidence weights to decide which branch to drill next. Not decoration — functional wayfinding.

### 5. Annotation → FAQ Pipeline
Annotations with `--question` context are processed into queryable FAQ entries within seconds. During the session the FAQ system grew from 1 to 9 entries. The pipeline includes:
- **Question generalization** — specific questions are rewritten for broader matchability
- **Match triggers** — auto-generated trigger patterns for fuzzy matching
- **Hit counting** — popularity signal for future agents

> *Tester B*: "This transforms agents from passive readers into active knowledge contributors."

### 6. Corrections & Decisions Capture
Goes beyond summarization — captures the *reasoning texture*: what was decided, why, and what was rejected. The apex had 6 architectural decisions with full rationale.

### 7. Gap Identification
Each L2 node identifies research gaps. Even if resolution status is unreliable (see F14), the gaps themselves are genuine research leads.

---

## Consolidated Friction & Issues

Issues are numbered F9+ (continuing from the earlier system-level assessment, F1-F8).

### 🔴 High Priority

#### F9: Search Is Keyword-Only, Not Semantic
**Both testers hit this independently.**

| Query | Results |
|-------|---------|
| `"how does the system handle failures"` | 0 |
| `"What is the River-Graph boundary?"` (exact apex term) | 0 via FAQ, keyword hits via search |
| `"bonding curve"` | 2 |
| `"context window cost"` | 17 |

Search is FTS/keyword matching. Agents arriving with natural language questions — which is how most agents think — get nothing. **This is the single biggest UX gap.**

#### F10: Search Scoring Is Depth-Only
Every result at the same depth gets the same score: L0=10, L1=20, L2=30, L3=40. No intra-depth relevance ranking. For "recursive synthesis": all three L2 hits scored 30, all five L1 hits scored 20. An agent gets a flat, unranked list per layer.

#### F11: Annotations Invisible in Drill Responses
The `_note` on annotate claims: *"immediately visible via annotations and drill."* But drill's response structure is `{node, children, evidence, gaps, question_context}` — **no annotations field**. Agents doing top-down exploration never see contributed knowledge unless they explicitly call `annotations <slug> [node_id]`. This breaks the compound knowledge promise.

#### F12: No Search ↔ FAQ Cross-Referral
Search returning 0 results gives `[]` — no hint to try FAQ. FAQ returning no matches gives no hint to try keyword search. Two retrieval systems in complete isolation.

> *Tester B*: "The FAQ seems heavily bound to question annotations rather than falling back to the robust terms dictionaries stored in the pyramid nodes."

### 🟡 Medium Priority

#### F13: `self_prompt` Has Inconsistent Semantics
- L1 nodes: contains a question ("How does delta-chain versioning work?")
- Q-L0 nodes: contains the full distilled content (400+ chars, not a question)
- Agents relying on `self_prompt` as "the question this node answers" get wrong data at L0

#### F14: Gaps Marked "resolved: true" Too Optimistically
Gaps like "Ablation studies" and "Comparison with academic literature" are marked resolved despite being inherently unanswerable from architecture docs. Suppresses them as research targets.

#### F15: No Breadcrumb / Upward Traversal
> *Tester B*: "When drilling into deep nodes like Q-L0-020, there's no reverse-tree summary to show me where I am in the pyramid (e.g., L0 → L1-foo → L2-bar → L3-apex). I can see parent_id, but I have to do multiple node calls to traverse upwards."

No `tree` or `map` command exists. An agent must iteratively drill to discover the full structure.

#### F16: Apex Payload Is Overwhelming
> *Tester B*: "apex returns a massive JSON payload. Having a simplified summary mode that just returns the highest-level synthesis (without full terms, corrections, dead ends, and children manifest) might be beneficial for agents with smaller context bounds."

No `--summary` or `--brief` flag to trim the apex response.

#### F17: No DADBEAR Status Command
The handoff template references DADBEAR status (auto-update, debounce, last check) but there's no `dadbear-status <slug>` CLI command. Agents must infer DADBEAR state from pyramid nodes or SQLite queries.

### 🟢 Low Priority

#### F18: `faq` and `faq-dir` Return Identical Results
Both commands produce the same JSON structure. Either they should differ in behavior or one should be an alias.

#### F19: Search Results Lack Structural Hints
Results show `{node_id, depth, snippet, score}` but not child count or annotation count. An agent can't tell if a hit is a leaf or a rich subtree.

#### F20: Error Messages Don't Suggest Alternatives
- `{"error": "Node not found"}` — doesn't suggest similar IDs
- `{"error": "No apex node found"}` — same message for "slug doesn't exist" vs "slug exists but not built"

---

## Structural Observations

### Pyramid Shape
58 nodes: 1 apex (L3) → 4 L2 → ~15 L1 → ~38 L0. ~4x branching factor per level — readable and navigable.

### L2 Question Decomposition
"What is this?" decomposed into:
1. Purpose and problem domain
2. Architectural structure and component organization
3. Core capabilities and features
4. Distinctive design innovations

Reasonable but architecture-biased. Missing: operational concerns, failure modes, deployment, development workflow.

### Evidence Redundancy Is Self-Aware
Evidence entries explicitly note when nodes are "Redundant with other nodes on core pyramid mechanics but adds explicit evidence for content-type configuration." The system tracks overlap.

### FAQ Generalization Is Good But Lossy
Agent question "Is pyramid search semantic or keyword-based?" → FAQ question "What underlying search mechanism does a system use to find information — semantic/AI-based..." Broader matchability, but original specificity is lost.

---

## Priority Fix Recommendations

| # | Fix | Impact | Effort |
|---|-----|--------|--------|
| 1 | **Inline annotations in drill response** | Closes the compound knowledge loop — one change, massive payoff | Low (add `annotations` field to DrillResult) |
| 2 | **Semantic search or NL→keyword rewriting** | Unlocks natural-language agents | High (needs embedding index or LLM rewrite step) |
| 3 | **Search↔FAQ cross-referral** | Zero-result search suggests FAQ; zero-match FAQ suggests keywords | Low (CLI/response-level change) |
| 4 | **Breadcrumb path in drill** | Shows `L0 → L1 → L2 → Apex` path without manual traversal | Low (walk parent_id chain server-side) |
| 5 | **Intra-depth search ranking** | Makes search results sortable within layers | Medium (needs TF-IDF or BM25 scoring) |
| 6 | **`tree <slug>` command** | One-call structural overview | Low (recursive query, already in SQLite) |
| 7 | **`--summary` flag on apex** | Smaller payload for context-constrained agents | Low (field filtering) |
| 8 | **`dadbear-status <slug>` command** | Exposes auto-update config without SQLite access | Low (reads `pyramid_auto_update_config`) |

---

## Verdict

The Knowledge Pyramid CLI is a **genuinely useful agent comprehension tool**. Both testers independently found the drill command, evidence weighting, vocabulary extraction, and annotation pipeline to be high-value capabilities that go well beyond what RAG or passive context windows provide.

The friction concentrates in **two areas**:
1. **Discoverability for cold-start agents** — keyword-only search, no semantic retrieval, no cross-system referral
2. **Knowledge loop closure** — annotations exist but don't surface in the navigation flow agents actually use

Fix #1 (inline annotations in drill) and Fix #3 (search↔FAQ cross-referral) are low-effort, high-impact changes that would meaningfully improve the first-contact experience for every future agent.
