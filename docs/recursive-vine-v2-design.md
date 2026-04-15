# Recursive Vine Architecture — V2 Design

**Status**: Design complete, pending build + audit  
**Date**: 2026-04-07  
**Authors**: Adam (product), Partner/Antigravity (design synthesis)

---

## 1. Core Principle

**There is only one thing: a pyramid.**

"Vine" and "bedrock" are relative labels based on perspective, not types. From one pyramid's perspective, the pyramids it queries are its bedrock. From below, that same pyramid is a vine. There is no structural difference between a "regular pyramid" and a "vine" — the only variable is what a pyramid's **sources** are: raw files, other pyramids, or a mix.

This means the entire recursive hierarchy — from raw JSONL files up through conversation pyramids, domain vines, personal me-vines, collaborative us-vines, and eventually METABRAIN — is built from one mechanism applied recursively.

## 2. The Source Abstraction

Currently, a pyramid's sources are raw files in a directory. The change: a pyramid's sources can also be **other pyramids**, identified by slug.

### Source Types
- **Raw files** (existing): A filesystem directory. The evidence loop extracts L0 nodes from source files.
- **Pyramid sources** (new): A list of pyramid slugs. The evidence loop gathers evidence by querying those pyramids via search, drill, and (when needed) ask.

A pyramid can have sources of both types simultaneously. A me-vine might source from a code-vine (pyramid source) AND a local notes directory (raw files).

### What Already Exists
- `SlugInfo` already has `referenced_slugs` and `referencing_slugs` — the cross-pyramid reference graph is tracked.
- Question pyramids already bypass source-path validation — precedent for non-filesystem sources.
- `ContentType::Vine` already exists in the enum.
- The `_ask` endpoint already creates question pyramids across pyramid boundaries.

## 3. Evidence Escalation Ladder

When a pyramid's source is another pyramid, the evidence loop uses a three-stage escalation to gather evidence. Each stage is more expensive but more thorough. You only advance when the prior stage's evidence is insufficient (per KEEP/DISCONNECT/MISSING verdicts).

### Stage 1: Search (fast, non-mutating)
Fan-out `search` across all source pyramids for each sub-question. Matching nodes become candidate evidence. This handles the common case — the source pyramids already have relevant nodes.

### Stage 2: Drill (medium, non-mutating)
For candidates that scored well but need more detail, `drill` into the specific nodes to get full content, children, evidence links. This provides deeper context without creating anything new.

### Stage 3: Ask (expensive, mutating, recursive)
When Stages 1-2 return MISSING verdicts — the source pyramids don't have what's needed — trigger `_ask` on the source pyramid with the unsatisfied question. This:
- Creates a new question pyramid within the source
- The source pyramid runs its own decomposition → evidence loop → synthesis
- The resulting answer nodes flow back up as evidence
- **The source pyramid is permanently enriched** — it now has nodes it didn't have before

This is the recursive case. The source pyramid might itself query *its* sources, which might ask *their* sources. The recursion is bounded by:
- **Accuracy threshold**: Stop when KEEP verdict weights are sufficient to answer
- **Depth limit**: Configurable maximum recursion depth (recommend default 2-3)

### Why Accuracy Controls, Not Cost
The escalation ladder is governed by evidence quality, not budget. You escalate because the answer isn't good enough, and you stop when it is. Cost is a natural byproduct of accuracy — shallow questions get answered cheaply (Stage 1), deep questions cost more (Stage 3), and over time the system gets cheaper because Stage 3 permanently enriches sources, making future queries resolve at Stage 1.

## 4. The Hook Point: Gap-Driven Targeted Extraction

The evidence loop already has exactly the right mechanism. Today:

```
Sub-question → evidence mapping → answer with verdicts
    → MISSING verdict → resolve_files_for_gap() → re-extract from source files
```

The change:

```
Sub-question → evidence mapping → answer with verdicts
    → MISSING verdict → resolve_pyramids_for_gap() → search/drill/ask source pyramids
```

Same control flow. Same verdict system. Same gap detection. Different evidence provider. The `targeted_reexamination` path already exists — it just needs a pyramid-aware sibling alongside the filesystem-aware one.

## 5. The Recursive Stack

Every level uses the same mechanism. Every boundary is the same evidence escalation.

```
                    ┌─────────────┐
                    │  METABRAIN  │  sources: [adam-vine, buddy-vine, ...]
                    └──────┬──────┘
                           │
                    ┌──────┴──────┐
                    │  ME-VINE    │  sources: [convo-vine, code-vine, docs-vine]
                    └──┬───┬───┬──┘
                       │   │   │
             ┌─────────┘   │   └─────────┐
      ┌──────┴──────┐ ┌────┴─────┐ ┌─────┴──────┐
      │ CONVO VINE  │ │CODE VINE │ │ DOCS VINE  │  sources: [individual pyramids]
      └──┬───┬───┬──┘ └──┬───┬──┘ └──┬───┬───┬──┘
         │   │   │       │   │       │   │   │
         P1  P2  P3     P4  P5     P6  P7  P8     sources: [raw files]
         │   │   │       │   │       │   │   │
       JSONL code docs  repos  ...  docs, books, songs
```

### Domain Vines
Group same-type pyramids: all conversations, all repos, all doc collections. The apex question is domain-shaped: "What happened across all my conversations?" or "What's the architecture of my codebase?"

### Me-Vine
Cross-domain personal vine. Sources are domain vines. The apex question is personal: "What is the current state of everything I'm working on?" or whatever the operator wants. This is the bespoke intelligence surface.

### Us-Vine
Cross-person collaborative vine. Sources are me-vines from different operators. Access controlled via Wire's publication/gating/credit system. The apex question is collective.

### METABRAIN
The logical conclusion. We don't need to architect this — it emerges from the same mechanism applied at scale.

## 6. Staleness Propagation

When a source pyramid rebuilds (new conversation added, code changed, doc updated):

1. Its `build_id` changes
2. DADBEAR detects the change in any pyramid that lists it as a source
3. The dependent pyramid's delta decomposition runs — re-queries only the changed source pyramids using the existing delta mechanism (reuse valid answers, only re-answer questions affected by the change)
4. Changes propagate upward through the stack

This means: add a new conversation → conversation pyramid rebuilds → conversation vine's DADBEAR detects stale source → vine re-queries → me-vine detects stale vine → me-vine re-queries. Each level only processes what changed.

## 7. Sequential Content & Triple-Pass Chrono

The bidirectional distillation (forward → reverse → combine) is the generalized treatment for **any ordered content** — content where order creates asymmetry between "what you knew then" and "what you know now."

| Content Type | What Forward Captures | What Reverse Captures |
|---|---|---|
| Conversations | Evolving understanding at each moment | What actually mattered given the outcome |
| Books / chapters | Reader experience, mounting tension | Authorial intent, thematic payoff |
| Movies / songs | First impression, emotional arc | Structural craft, motif repetition |
| Git history | Intent at time of each change | Which changes survived and mattered |
| Legislation | Drafting logic, clause-by-clause reasoning | Final binding effect after amendments |
| Course lectures | Learning progression | What competency was built |

The triple-pass chrono approach applies this at the bedrock level for any sequential source material. The vine level above doesn't need to know or care whether its sources used triple-pass — it just queries them.

## 8. Persistence & Identity

Vines are **real pyramids**. They have slugs, build_ids, DADBEAR tracking, the full lifecycle. They're queryable via the same CLI (search, drill, apex, faq). They're publishable to Wire. They generate contributions.

There is no ephemeral mode. If you ask a question and it triggers a recursive vine, the result is a real pyramid that persists, gets smarter over time, and can be queried by other pyramids above it.

## 9. What This Means for Wire

(Not the focus of this doc, but noting for completeness since it's the natural endpoint.)

Every pyramid node is a Wire contribution. Every evidence link is a `derived_from` chain. Every chain/recipe/prompt is itself a contribution. When a vine queries its sources, credits flow through the evidence links. When a recursive `ask` enriches a source, the enrichment is a contribution that earns future royalties.

The recipes (chain YAML, prompt templates) are contributions too — they compete on the marketplace. A better extraction prompt produces better pyramid nodes, gets more queries, earns more credits, which flow back to the prompt author. The system self-improves through market pressure on every component.

## 10. Build Phases

### Phase 1: Pyramid Evidence Provider
Add the ability for a pyramid to declare other pyramids as sources. Implement search fan-out across source pyramids. Wire search results into the evidence loop as candidate evidence. Track source pyramid `build_id` for DADBEAR staleness.

**Exit criteria**: A test vine built over two existing pyramids successfully answers questions using evidence gathered from source pyramid search.

### Phase 2: Gap-to-Ask Escalation
When evidence loop returns MISSING verdicts and sources are pyramids, trigger the existing `_ask` endpoint on the source pyramid. Answer nodes flow back as evidence. Configurable depth limit and accuracy threshold.

**Exit criteria**: A vine question that can't be answered by search alone triggers recursive ask on a source pyramid, enriching the source and answering the vine question.

### Phase 3: Domain Vine Creation UX
CLI and UI for creating vines: `create-vine --sources slug1,slug2 --question "..."` and auto-discovery by content type. Staleness propagation when source pyramids rebuild. Delta re-query on changes.

**Exit criteria**: Operator can create convo-vine, code-vine, docs-vine, and me-vine through CLI. Changes to source pyramids propagate through the vine stack.

### Phase 4: Cross-Operator Vines
Published pyramids as vine sources via remote pinning. Credit flow through evidence queries. Access control enforcement in the evidence provider.

**Exit criteria**: Two operators' me-vines serve as sources for a shared us-vine with proper Wire credit settlement.

---

## Appendix: Audit Findings from Code Pyramid

The following existing infrastructure was identified by querying the `agent-wire-node-bigsmart-2` code pyramid:

| Component | What Exists | How It's Reused |
|---|---|---|
| `SlugInfo.referenced_slugs` | Bidirectional cross-pyramid reference tracking | Source pyramid registry — no new data model needed |
| Question pyramid source bypass | Source-path validation skipped for question content type | Extend to vine content type |
| `_ask` endpoint | Creates question pyramids across pyramid boundaries | Recursive escalation target — expose for internal programmatic use |
| `targeted_reexamination` | MISSING verdict → re-read source files → extract | Replace file resolution with pyramid search/drill |
| `ContentType::Vine` | Already exists in the enum | Wire to new evidence provider |
| `pre_map_layer` (Step 3.1) | Maps questions to candidate evidence from below | Same pattern, different evidence source |
| Evidence verdicts | KEEP/DISCONNECT/MISSING with weights | Completely unchanged |
| Delta decomposition | Reuses existing answers, only re-answers changed questions | Works for vines — re-queries only changed sources |
| DADBEAR | Per-layer staleness detection and propagation | Add source pyramid `build_id` tracking |
