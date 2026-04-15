# Vine Architecture: Cross-Pyramid Knowledge Composition

> **Status**: Design specification
> **Supersedes**: The original conversation-vine concept (vine-build for JSONL dirs) is the first instantiation of this generalized pattern.
> **Related**: Q-L0-642, Q-L0-678 (original vine references), Q-L0-672 (remote pyramid access), Q-L0-680 (Wire Online Push)

---

## 1. The Problem

Knowledge Pyramids are self-contained. Each pyramid synthesizes understanding from its own source material — documents, codebases, or conversations — through the recursive synthesis rule (L0 → L1 → L2 → apex). But real understanding often spans multiple pyramids. An agent working in one pyramid cannot synthesize insights that require evidence from another.

The naive solution — direct node-to-node edges between pyramids (full webbing) — creates an N×M explosion of cross-references. It's expensive, hard to maintain, and tangles the provenance graph.

The vine solves this through **lazy, question-driven composition**.

---

## 2. Core Concept

A **vine** is the mechanism that connects knowledge structures at the scale above. It is the general primitive for cross-pyramid linking.

The key insight: **don't web pyramids together directly. Ask questions of them.**

### The Flow

1. A **meta-pyramid** needs understanding that spans multiple **bedrock pyramids**.
2. Instead of directly linking nodes, the meta-pyramid **propagates a question** downward to relevant bedrock pyramids.
3. Each bedrock pyramid answers the question through its normal synthesis chain — extracting, clustering, synthesizing — producing evidence. This evidence is the **Vine L0**: the base layer of the vine connection.
4. The meta-pyramid's **L1 node** synthesizes across the vine-L0 responses from multiple bedrock pyramids.
5. The result: a cross-cutting answer with full provenance, without direct pyramid-to-pyramid edges.

```
┌─────────────────────────────┐
│       Meta-Pyramid          │
│                             │
│   ┌───────────────────┐     │
│   │    Meta L1 Node   │     │  ← Synthesizes across vine-L0s
│   └──┬──────────┬─────┘     │
│      │          │           │
│  ┌───┴──┐  ┌───┴──┐        │
│  │Vine  │  │Vine  │        │  ← Evidence produced by bedrock
│  │ L0-a │  │ L0-b │        │    pyramids answering the
│  └──┬───┘  └──┬───┘        │    propagated question
└─────┼─────────┼─────────────┘
      │         │
      │ question│ question
      │propagated propagated
      ▼         ▼
┌──────────┐ ┌──────────┐
│ Bedrock  │ │ Bedrock  │
│Pyramid A │ │Pyramid B │
│          │ │          │
│  (docs)  │ │  (code)  │
└──────────┘ └──────────┘
```

### Provenance Chain

Every cross-pyramid connection is traceable:

```
meta-L1 → meta-L0 (vine) → question propagated → bedrock evidence
```

No arbitrary edges. Every connection is question-motivated.

---

## 3. Relative Designation

**"Bedrock" and "meta-pyramid" are relative terms**, not fixed labels. They describe the relationship between two pyramids, not an intrinsic property of either.

- Any pyramid can be bedrock to a meta-pyramid above it
- Any meta-pyramid can itself be bedrock to a higher meta-pyramid
- A single pyramid can simultaneously be meta (to things below) and bedrock (to things above)

This means the vine architecture is **recursively composable**:

```
Meta-meta-pyramid          ← asks questions of meta-pyramids
    │
    ├── Meta-pyramid A     ← asks questions of bedrock pyramids
    │   ├── Bedrock 1
    │   └── Bedrock 2
    │
    └── Meta-pyramid B     ← also bedrock to the level above
        ├── Bedrock 3
        └── Bedrock 4
```

The same node mechanics, synthesis rules, and build pipeline operate at every level. The pyramid is the fractal unit. Vines are how the fractal tiles together.

### Scale Flexibility

The granularity is arbitrary and adjustable:

- **Zoomed in**: A single dense document where each chapter gets its own pyramid, with a document-level meta-pyramid composing them via vines
- **Zoomed out**: A meta-pyramid spanning every pyramid an organization has ever built
- **Network scale**: A meta-pyramid spanning pyramids across multiple operators on the Wire

You don't pre-commit to a level of resolution. You stay at the current scale until evidence demands expansion — either drilling down (splitting because there's too much density) or reaching up (composing because you need cross-cutting answers). The architecture doesn't change, only the focal length.

---

## 4. Triggers: What Causes a Vine Connection

Three sources, all equivalent from the vine's perspective — a question arrives, it gets propagated downward:

### 4a. Human Ask

A human asks "How does the credit economy interact with the identity model?" → meta-pyramid propagates sub-questions to relevant bedrock pyramids.

### 4b. Agent Ask

An agent mid-task realizes it needs cross-domain context → same propagation.

### 4c. Self-Generated During Synthesis

The meta-pyramid itself, during a build, discovers it can't synthesize an L1 without more evidence from a bedrock pyramid it hasn't queried yet. The build process generates its own questions downward.

This third trigger is what makes the system **self-expanding**. The synthesis rule creates demand for more vine connections when existing evidence is insufficient. The system grows toward completeness by need, not by plan.

---

## 5. Wire-Mediated Vines: The Network Scale

### Local Primitives (Already Built)

The local version of vine composition already exists:

- `create-question-slug --ref source-1 --ref source-2` — creates a question slug referencing bedrock pyramids
- `question-build` — decomposes a question and builds answer nodes across referenced sources
- `references` — shows the reference graph
- `composed` — composed view across a question slug and all its referenced sources

These are the local, manually-wired version of vine composition.

### Wire as Routing Layer

The Wire replaces manual `--ref` flags with **economic discovery**. When a question propagates through the Wire:

1. The question is broadcast to every pyramid that will accept it
2. Pyramid owners **voluntarily expand** to answer because:
   - They retain full ownership of the understanding base built on top
   - They get **95% of royalties** (60% creator + 35% source chain) on synthesis derived from their answer — vs. only 35% if they were merely cited downstream
3. The asker pays a small credit fee but **owns the question slot**
4. If the question is good — causing many pyramids to expand — the question slot earns ongoing citation royalties

### The Incentive Loop

```
Good questions → pyramids expand to answer
    → expanded pyramids attract more queries
    → more queries = more revenue for owners
    → owners accept more questions
    → network gets smarter
    → smarter network attracts better question-askers
    → better questions → (loop)
```

**Asking good questions becomes a form of productive labor.** The most valuable actors in the network may not be the ones with the best answers, but the ones who ask the best questions — causing the most pyramids to expand and generating the most understanding across the network.

### Economic Settlement

Cross-pyramid cost accounting uses the existing remote pyramid access infrastructure:

- **Authentication**: Two-JWT handshake (pyramid-query JWT + payment JWT) per Q-L0-672
- **Settlement**: Nano-transaction flow (stamp + access price) with payment-intent/redeem per Q-L0-680
- **Access tiers**: Per-slug controls (public, circle-scoped, priced, embargoed)

No new economic primitives needed. A vine question propagating through the Wire is a remote pyramid query — same plumbing.

---

## 6. Staleness and Updates

Vine staleness uses the **same DADBEAR system** as intra-pyramid staleness — helper dispatch, supersession, and event-driven updates.

Key principles:

- **Whether to update is always the owner's choice.** Pyramid owners can freeze their pyramid and stop accepting vine-propagated questions.
- **Subscribers to stale content pay nothing.** If a pyramid that someone else owns rebuilds, the subscriber can choose not to pull the new version. They keep their stale local copy at zero cost.
- **Invalidation follows existing patterns.** When a bedrock pyramid rebuilds and its evidence changes, DADBEAR events signal upstream vine connections. The meta-pyramid owner decides whether to rebuild.

---

## 7. Contradiction Handling

When bedrock pyramids return contradictory evidence to the same vine-propagated question, the meta-pyramid uses **intelligence** — the same LLM-driven synthesis that handles intra-pyramid contradictions.

The synthesis process:
- Notes the contradiction as a first-class signal
- Evaluates evidence quality, freshness, and provenance
- May surface the contradiction explicitly if it represents genuine disagreement rather than staleness

This process **improves the pyramids with use**: contradictions detected through vine composition create demand for corrections, which flow back to the bedrock pyramids as annotations and fact-checks. Understanding accretes. The system gets smarter through the act of being queried.

---

## 8. Cycle Prevention

Vine connections route through the Wire as **request-response** operations. A question propagates downward, evidence flows upward. There is no mechanism for the question to loop back — it's not a continuous subscription.

If a higher pyramid later re-asks a similar question of a pyramid that is itself downstream, it's a **new question** producing **new evidence**. The worst case is redundant work, not infinite loops. And redundant work is not wasted — it refines understanding because the target pyramid is denser now than last time.

The Wire's request-response semantics are the natural circuit breaker. DAG enforcement at the routing layer is a possible future hardening if edge cases emerge.

---

## 9. Historical Evolution

The vine concept has generalized through three phases:

1. **Conversation Vines** (original build): Aggregate individual conversation pyramids (JSONL dirs) into a meta-pyramid. CLI: `vine-build`, `vine-bunches`, `vine-eras`, `vine-threads`, etc. This was the entry point — the first instantiation of the general pattern.

2. **Question Composition** (current): Cross-pyramid queries via `create-question-slug --ref`. Local, manually-wired composition across document/code pyramids. The generalization from conversations to any pyramid type.

3. **Wire-Mediated Vines** (this specification): The same composition, but with economic discovery replacing manual `--ref` flags, and the Wire providing routing, authentication, and settlement. The generalization from local to network scale.

Each phase uses the same architectural DNA: question-driven synthesis across knowledge boundaries. The vine is the primitive. The scale changes.

---

## 10. Comparison: What Is This At Scale?

At network scale, the Wire-mediated vine architecture is a **knowledge futures market grafted onto a self-organizing research network**:

- The **question** is a futures contract — a bet that this understanding will be valuable
- The **pyramids that accept it** are producers — they invest compute because they keep the deep synthesis
- The **vine-L0** is the settlement — the evidence connecting asker to producer
- The **ongoing royalties** are yield — both parties earn from ongoing access

The closest real-world parallel is **how cities work**: nobody plans the economy top-down. People and businesses co-locate because proximity creates value. Infrastructure grows to serve demand. The system gets denser and more valuable through use. Vines are the roads. Pyramids are the buildings. The Wire is the municipal infrastructure. And nobody has to plan the whole thing.

---

## 11. Open Questions

- **Cycle hardening**: Current analysis suggests request-response semantics prevent cycles naturally. Monitor for edge cases as vine depth increases. Consider explicit DAG enforcement if needed.
- **Question quality scoring**: The system rewards good questions through citation royalties, but is there a role for explicit quality signals on questions themselves (beyond market-driven emergent pricing)?
- **Vine depth limits**: At what point does the meta-meta-meta-pyramid become more noise than signal? Is there a natural depth at which synthesis stops adding value? Or does the recursive synthesis rule handle this by producing diminishing returns that naturally discourage deeper stacking?
