# Querying pyramids (for agents)

This doc is written for agents — Claude, a scripted auditor, anyone operating at the MCP or CLI level — that needs to navigate a pyramid effectively. The tools are documented in [`80-pyramid-cli.md`](80-pyramid-cli.md); this doc covers the *patterns* for using them.

If you're a human reading the Pyramid Surface, the same tools exist in the UI (search, drill, inspector, FAQ). This doc applies to you too, just translated into UI actions.

---

## The first rule: use FAQ before doing anything

A pyramid accumulates annotations over time. Annotations with `question_context` feed the FAQ. If someone (human or agent) has already answered your question, the answer is in the FAQ.

```
pyramid_faq_match my-pyramid "How does the stale engine handle deletions?"
```

If the FAQ has a match, you're done. The FAQ's answer is canonical — it synthesizes multiple annotations into a single statement, with provenance back to the original annotations.

If the FAQ gives nothing, proceed to search/drill.

---

## Cold-start onboarding

When you first encounter a pyramid, get oriented before you ask specific questions:

```
pyramid_handoff my-pyramid
```

Handoff is a composite call — it bundles apex + FAQ directory + recent annotations + DADBEAR status in one response. You get:

- The pyramid's top-level answer (apex).
- The vocabulary (terms).
- The accumulated FAQ entries.
- Recent annotations (what have other agents been learning?).
- DADBEAR status (is the pyramid actively maintained? how stale might answers be?).

Read this first. Then your follow-up queries land in context.

---

## The navigation loop

The canonical navigation sequence for an unknown question:

```
1. faq_match       — has this been asked?
2. search          — is the relevant material in the pyramid?
3. drill           — pull the specific nodes into context
4. navigate        — one-shot QA if you need a synthesized answer
5. annotate        — leave what you learned for the next agent
```

Each step is optional. The loop compresses into fewer calls for familiar pyramids.

### FAQ match

Cheap. No LLM call. Just matches your question against existing FAQ entries by keyword + embedding.

```
pyramid_faq_match my-pyramid "how does authentication work?"
```

Returns matched entries with the canonical answer. Use these first.

### Search

FTS across pyramid nodes. Ranked by depth (higher depths first — L3 > L2 > L1 > L0) and term frequency. Free unless you fall back to semantic rewrite.

```
pyramid_search my-pyramid "authentication middleware"
pyramid_search my-pyramid "auth flow" --semantic  # LLM fallback on 0 results
```

Search returns nodes with snippets. Pick the most relevant 1-3.

### Drill

Full detail on a specific node. Includes children, evidence links, gaps, question context, inline annotations, and a breadcrumb from apex to this node.

```
pyramid_drill my-pyramid L1-003
```

Drill is the meat. Most of your time should be in drill or in reading drill results.

### Navigate (one-shot QA)

If you need a synthesized direct answer with citations and you're willing to spend 1 LLM call:

```
pyramid_navigate my-pyramid "How does the stale engine decide what to rebuild?"
```

Navigate searches, fetches relevant content, synthesizes an answer, cites evidence by node ID. Useful for "just tell me the answer" flows.

### Annotate

When you learn something non-obvious, leave it:

```
pyramid_annotate my-pyramid L0-042 \
  "Retry logic caps at 3 with exponential backoff.
  
Generalized understanding: when you see retry logic in this codebase, check for the 3-attempt cap — it's the convention." \
  --question "What is the retry strategy for LLM calls?" \
  --author my-agent-pseudonym \
  --type observation
```

The key elements:

- **Specific finding** first — what you learned concretely.
- **Generalized understanding** section — the mechanism-level insight that applies beyond this specific node.
- **Question context** — the question this answers. Triggers FAQ generation.
- **Type** — observation / correction / question / friction / idea.

Annotations with question context feed the FAQ automatically. The FAQ entry that results may synthesize across multiple annotations — your note joins the canonical knowledge.

---

## Token-efficient variants

Some commands have lighter variants when you don't need full data:

- **`pyramid_apex --summary`** — strips apex to headline/distilled/self_prompt/children/terms. 10x smaller response.
- **`pyramid_drill`** returns children with IDs but not full content by default. Follow up with `pyramid_node <child_id>` only for the specific children you care about.
- **`pyramid_search`** returns snippets, not full node content. Good for ranking before drill.

Chain cheap calls into informed expensive calls. Don't drill 15 nodes when 2 are what you need.

---

## Drilling deep vs scanning wide

Two modes:

### Drilling deep (focused)

You have a specific question. Find the most relevant starting node, drill, follow children/web edges that look relevant, accumulate a focused picture.

```
search → pick best result → drill → follow one child → drill → stop when answered
```

This is cheap and precise. Use for specific questions.

### Scanning wide (exploratory)

You're orienting yourself. Read the full structure rather than one path.

```
apex --summary → handoff → tree → drill a few top-level L2 nodes
```

This is more expensive but gives you the full shape. Use for onboarding or for broad questions.

The right mix depends on what you're trying to do. Specific "what does this function do?" is deep. Broad "how is this codebase organized?" is wide.

---

## Cross-pyramid navigation

If you're working with multiple related pyramids (e.g. `codebase-v1` and `codebase-v2`, or a codebase pyramid + a design-docs pyramid):

- **`pyramid_compare slug1 slug2`** — surfaces shared/unique terms, conflicting definitions, structural diffs.
- **Question pyramid** — create one that references both and ask synthesizing questions:

  ```
  pyramid_create_question_slug migration-analysis --ref codebase-v1 --ref codebase-v2
  pyramid_question_build migration-analysis "What breaking changes exist between v1 and v2?"
  ```

- **`pyramid_composed_view`** — full cross-pyramid view of a question pyramid and its sources.

---

## Dealing with stale or uncertain answers

Pyramids can go stale. Check DADBEAR status:

```
pyramid_dadbear_status my-pyramid
```

If auto-update is disabled and the pyramid hasn't been rebuilt in a while, treat synthesized answers with caution. Cross-check against actual source material if stakes are high.

Annotations with type `correction` that supersede existing claims are authoritative (the pyramid author or operator has explicitly said the pyramid is wrong on this point). Pay attention to them.

---

## Leaving good annotations — quality patterns

Good annotations:

- **Specific.** "Retry caps at 3" beats "has retry logic."
- **Generalized.** Always include the "Generalized understanding" — what this tells you about the system beyond the specific node.
- **Question-contextualized.** Ensure `--question` is set so the FAQ can take over.
- **Typed.** Pick the right type. Correction is strong; observation is neutral.

Avoid:

- Restating what the node already says.
- Vague friction without specifics ("this is confusing" — be specific about what).
- Information that's local to one session (what you were doing when you noticed — keep that in your own log, not the pyramid).

Future-you is one consumer of your annotations. Future-other-agents are the other. Write for them both.

---

## Common pitfalls

**Drilling into L0 when L1/L2 would answer.** Lower depth is more granular; higher depth is synthesized. For "how does X work" type questions, start higher. L0 is for specific evidence lookups.

**Ignoring FAQ.** FAQ is the accumulated knowledge. Asking the same question that someone already answered wastes effort and doesn't grow the FAQ. Always check first.

**Over-relying on navigate.** `navigate` is convenient but costs 1 LLM call. If you're doing 50 questions in a session, `search + drill + read` is dramatically cheaper and often just as good.

**Forgetting to annotate.** An agent session that leaves no annotations does no compounding work — the same question will take the same time next session. Annotate even briefly.

---

## Where to go next

- [`80-pyramid-cli.md`](80-pyramid-cli.md) — full tool catalog.
- [`81-mcp-server.md`](81-mcp-server.md) — MCP integration.
- [`83-agent-sessions.md`](83-agent-sessions.md) — multi-agent coordination.
- [`26-annotations-and-faqs.md`](26-annotations-and-faqs.md) — annotation mechanics.
