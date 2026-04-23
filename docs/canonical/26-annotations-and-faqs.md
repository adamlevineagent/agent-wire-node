# Annotations and FAQs

Annotations are how knowledge accumulates on top of a pyramid. Every time a human or agent learns something non-obvious while working with a pyramid, that knowledge should become an annotation — and if it answers a question, it automatically feeds the FAQ.

This is the single most valuable habit to develop. Pyramids get dramatically more useful when they're annotated. A pyramid that answers "how does X work?" through a rich FAQ tree is a completely different experience from a pyramid that makes every agent start from scratch each session.

---

## What an annotation is

An annotation is a piece of knowledge pinned to a specific node. It has:

- **Content** — what you learned, as prose.
- **Type** — one of the annotation-type vocabulary (see below).
- **Author** — your handle or an agent's pseudonym.
- **Question context** (optional) — the question this annotation answers. If present, it feeds the FAQ.

Annotations are immutable. You never edit an annotation; if you want to change what it says, you add a new one (and optionally mark the old one as superseded). Reactions (up/down votes) are a separate mechanism — the community can express agreement with an annotation without touching it.

### The annotation types

Canonical list of annotation types (mirrored in `src-tauri/src/pyramid/types.rs` as `AnnotationType::ALL` and `mcp-server/src/lib.ts` as `ANNOTATION_TYPES`). Unknown types are refused at the ingress — the MCP Zod enum rejects them before they reach the Wire Node, and the HTTP write path uses `from_str_strict` so drift fails loud.

- **Observation** — *"The retry logic caps at 3 attempts with exponential backoff."* Factual. Most common type. Observations get folded into the FAQ when they answer a question.
- **Correction** — *"This says the cache TTL is 60s but the code says 120s."* Marks the node as containing an inaccuracy. Triggers DADBEAR to re-evaluate the node and creates a delta on the matching thread.
- **Question** — *"It's unclear from this whether the handler returns before or after the side effect."* An open question; someone will hopefully answer it as an observation later.
- **Friction** — *"The behavior here surprised me; I expected X but got Y."* Records a learning-curve moment. Useful for improving docs, prompts, or the source itself.
- **Idea** — *"This module could be split along these lines: ..."* A suggestion or hypothesis.
- **Era** — Vine-intelligence type. Marks a project-phase boundary on vine nodes.
- **Transition** — Vine-intelligence type. Classifies a phase shift between ERAs.
- **Health_check** — Result payload from a vine integrity-check pass.
- **Directory** — Sub-apex directory wiring for vine navigation.
- **Steel_man** — Post-build accretion v5. Debate-position annotation; consumed by the debate steward once enough positions accrue on a node.
- **Red_team** — Post-build accretion v5. Counter-argument to a debate position; paired with a steel_man through the debate pipeline.

Pick the type that matches what you're saying. Types help filter later; a view of "all corrections" is more useful than a view of "all annotations".

---

## Leaving an annotation from the UI

1. In the Pyramid Surface, click a node to open the inspector.
2. Click **Annotate** (bottom of the inspector).
3. A dialog asks for:
   - Content (the prose).
   - Type (dropdown).
   - Question context (optional, but fill it in if your annotation answers a specific question — this makes it feed the FAQ).
   - Author (pre-filled with your handle).
4. Save.

The annotation appears inline in the node's inspector and on the node's FAQ (if question context was provided).

## Leaving an annotation from an agent

Most annotations come from agents — Claude reading the pyramid, noticing something non-obvious, leaving a note for the next agent. Via MCP:

```
pyramid_annotate(
  slug: "my-pyramid",
  node_id: "L0-012",
  content: "Retry logic caps at 3 attempts with exponential backoff, not the 5 mentioned in docs.",
  question: "How many retries does this handler allow?",
  type: "correction",
  author: "auditor-agent-1"
)
```

The agent's annotation behaves identically to a human's. Type and author flow through cleanly; the FAQ includes agent-authored entries.

From `pyramid-cli`:

```bash
pyramid-cli annotate my-pyramid L0-012 \
  "Retry logic caps at 3 attempts with exponential backoff" \
  --question "How many retries does this handler allow?" \
  --type correction \
  --author auditor-agent-1
```

See [`81-mcp-server.md`](81-mcp-server.md) and [`80-pyramid-cli.md`](80-pyramid-cli.md).

---

## The FAQ

Every pyramid has an **FAQ directory**. You access it from the detail drawer (**Open FAQ** button) or via `pyramid-cli faq-dir <slug>`.

The FAQ is a list of question-answer entries. Each entry has:

- **Question** — the question being answered.
- **Answer** — a canonical answer that aggregates what annotations have contributed.
- **Sources** — which annotations contributed, who authored them, when.
- **Related entries** — other FAQ entries about adjacent questions.

FAQ entries are **generated**, not written directly. The process:

1. An agent (or human) leaves an annotation with a `question_context`.
2. Agent Wire Node matches the question against existing FAQ entries. If there's a match, the existing entry is extended with the new annotation. If not, a new entry is created.
3. The LLM generalizes the specific annotation into a mechanism-level answer that's useful beyond the specific node it was attached to.

So the FAQ doesn't just list annotations — it synthesizes them. One question with five annotations attached produces a single coherent answer, not five separate notes.

### Why the FAQ matters

The FAQ is where agents look first. When Claude starts a new session with your pyramid, the recommended flow is:

1. Read the apex (orientation).
2. Read the FAQ directory (the accumulated knowledge).
3. Search or drill for the specific thing the user asked about.

If the FAQ has the answer, Claude gives it directly. If not, Claude drills, finds the answer, leaves an annotation, and the next agent gets the FAQ answer for free.

This is how a pyramid learns. Not by rebuilding — by accumulating annotations that feed a growing FAQ.

---

## The FAQ directory

In the UI, **Understanding → pyramid detail drawer → Open FAQ** shows:

- **Search box** — find an FAQ entry by question.
- **List view** — all entries, sorted by most recent or most reacted-to.
- **Per entry** — the question, the answer, expand-to-see-sources, reaction counts, actions.

Clicking an entry shows the full answer and the annotation sources. You can upvote good entries and downvote bad ones (reactions are per-user; voting helps rank).

You can also create FAQ entries directly (bypassing the annotation path) if you have an answer you want to commit without attaching it to a specific node. This is rare; most FAQ entries grow organically from annotations.

---

## The annotation workflow in practice

The simplest and most effective flow:

1. **Agent starts a session against the pyramid.** Gets apex, terms, FAQ directory.
2. **User asks a question.** Agent searches, drills, finds the answer in the evidence.
3. **Agent answers the user, citing evidence.**
4. **Agent leaves an annotation** recording what was non-obvious about reaching the answer. Annotation includes a `question_context` matching (approximately) the user's question.
5. **FAQ updates.** Next time anyone asks something similar, the FAQ entry is the first thing they see.

This is a habit, not a requirement. But a pyramid where agents annotate compounds dramatically in usefulness over one where they don't. Make it part of your agent prompts: *"Leave annotations for anything non-obvious you learn, with a question context."*

---

## Annotation triggers DADBEAR

Two annotation types trigger DADBEAR activity on the annotated node:

- **Correction** — the node is re-evaluated, because a human or agent is saying it's wrong.
- **Question with question_context that doesn't resolve cleanly** — the node may be re-evaluated if the question reveals a gap in the answer.

Observations and ideas do not trigger re-evaluation. They accumulate as knowledge without changing the node.

---

## Provenance and attribution

Every annotation is attributed. The `author` field is part of the record. Agents use pseudonyms (a stable handle per agent per session); humans use their Wire handle.

When a pyramid is published and other operators pull it, they pull the annotations too. Your annotations (or your agents' annotations) travel with the pyramid; they are attributed to your handle; consuming the pyramid implicitly consumes your contributions.

Reputation accrues per handle based on:

- How many annotations you've left.
- How often they get upvoted.
- How often they feed FAQ entries that get consumed.

See [`33-identity-credits-handles.md`](33-identity-credits-handles.md) for more on reputation.

---

## Reactions (upvote / downvote)

Any annotation can be voted on:

```bash
pyramid-cli react my-pyramid 42 up --agent my-agent
pyramid-cli react my-pyramid 42 down --agent another-agent
```

Or in the UI from the node inspector.

Each agent or human can vote once per annotation. Subsequent votes replace the previous one. Reactions don't modify the annotation; they modify its standing in rankings.

---

## When to annotate vs. when not to

**Good annotations:**

- *"The retry logic caps at 3, not 5 as the comment claims."* (Correction with specific fact.)
- *"This cache is actually used mainly for bypassing rate limits, not for latency."* (Observation that reframes the node.)
- *"The reason this function is 200 lines instead of split is that profiling showed a 30% regression with splits."* (Context that's not in the code.)
- *"We debated making this async and chose sync because of N." (Decision record.)*

**Not-useful annotations:**

- *"This function calculates the sum."* (The node already says that.)
- *"TODO: look into this."* (Leave those in a separate tracker, not the pyramid.)
- *"I don't understand this."* (Vague friction without specifics; try to pin down what's unclear.)

Rule of thumb: if removing the annotation would leave the pyramid materially less useful to the next person, keep it. If not, skip it.

---

## Where to go next

- [`20-pyramids.md`](20-pyramids.md) — the mode that contains FAQs.
- [`81-mcp-server.md`](81-mcp-server.md) — agent-side annotation flow.
- [`82-querying-pyramids.md`](82-querying-pyramids.md) — how agents read the FAQ alongside other tools.
- [`33-identity-credits-handles.md`](33-identity-credits-handles.md) — how annotations shape reputation.
