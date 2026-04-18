# Question sets

A **question set** is a preset decomposition — an apex question plus a tree of sub-questions that the decomposer can use verbatim instead of generating a decomposition from scratch. When you publish a question set, other operators can run the same investigation against their own pyramids without re-doing the decomposition work.

Question sets are one of the five Wire-shareable contribution types alongside skills, templates, actions, and chains. They're the unit of sharing "how to ask about X."

---

## When to author a question set

Every pyramid build starts with an apex question and a decomposition. The decomposer is an LLM step — it takes the apex and produces sub-questions. For a new or one-off question, generating the decomposition fresh is the right thing.

For a **recurring** investigation — "what are the security properties of this codebase?", "what does this spec claim and how does it support the claims?", "how is error handling structured?" — there's a lot of wasted motion in decomposing the same apex question over and over across different pyramids. A question set freezes a well-tuned decomposition so every consumer gets the same (usually better than ad-hoc) sub-question tree.

Author a question set when:

- You've asked a variant of this question across several pyramids and landed on a decomposition that works.
- The investigation has a shape other operators would benefit from (security audits, onboarding packs, architecture reviews).
- You want to standardize an analysis across a team's pyramids.
- You want reputation credit for the framing you put behind a recurring question.

Don't author one when the question is one-off, when the decomposition is still evolving, or when the underlying material varies enough that a generic decomposition wouldn't transfer.

---

## Question set structure

A question set is a YAML document. The shape (simplified):

```yaml
schema_version: 1
id: security-audit-v1
name: Security Audit
description: "Standard security audit decomposition for code pyramids."
content_type: code
version: "1.0.0"
author: "@adam/primary"

apex_question: "What are the security properties of this codebase and where are its weak points?"

decomposition:
  granularity: 4
  max_depth: 3
  tree:
    - question: "How is authentication handled?"
      children:
        - question: "What authentication mechanisms are used?"
        - question: "Where are credentials stored and transmitted?"
        - question: "How are sessions managed?"
        - question: "What protections exist against credential stuffing and brute force?"
    - question: "How is authorization enforced?"
      children:
        - question: "What is the authorization model (RBAC, ABAC, capability)?"
        - question: "Where is authorization checked and where is it skipped?"
        - question: "How are privilege escalation paths guarded?"
    - question: "How is input validated?"
      # ... etc.
    - question: "How is output encoded?"
    - question: "How are secrets handled?"
    - question: "What are the cross-cutting security assumptions?"

default_tier_hints:
  extractor: mid
  synth_heavy: high

tags: ["security", "code-audit", "authentication", "authorization"]
```

Key fields:

- **`apex_question`** — the top-level question. Must be self-contained.
- **`decomposition.granularity`** and **`max_depth`** — overridable by the caller, but the set's defaults reflect what the author found worked.
- **`decomposition.tree`** — the preset sub-questions. Can be as deep as `max_depth` allows.
- **`default_tier_hints`** — suggested tier routing. Callers can override.
- **`tags`** — for discovery.

---

## How a question set is used

When you create a question pyramid in the UI, one of the options is "use a published question set." You pick a pulled question set by handle-path, point it at source pyramids (if the set is about multi-source composition) or a specific corpus, and confirm.

The decomposer in `question.yaml` checks: is there a preset decomposition for this apex? If yes, it uses the preset tree instead of running `recursive_decompose` from scratch. The rest of the pipeline (extraction schema generation, evidence answering, synthesis) runs normally against the preset tree.

You can override the preset in the UI — bump granularity, edit a sub-question — and the edited tree replaces the preset for this specific build.

> **Status:** the UI integration for "use a published question set" is partially shipped. Today you can author and publish a question set, and its handle-path is durable and searchable, but the "plug and play" consumer flow that skips `recursive_decompose` entirely is still landing. Consumers currently pull a question set and invoke it by reference in their build command. Full UI integration is near-term.

---

## Authoring a good question set

A question set is "how to investigate a thing." The hard part is getting the decomposition right:

**Each level should be answerable independently.** A sub-question that requires answering another sub-question first is poorly factored. The decomposer should be able to dispatch leaves in parallel.

**Leaves should be concrete.** "What is the authentication mechanism?" is specific. "How does authentication work?" is not — the leaf should name what it wants to know.

**Cover the space without overlap.** If two sub-questions would retrieve the same evidence, they're not carrying their weight. Each should open a different aspect.

**Don't over-specify.** The pyramid's extraction and synthesis fill in detail. The question set provides the skeleton; it shouldn't try to anticipate every nuance.

**Include a cross-cutting question.** Most good decompositions end with a sub-question that asks about the interactions *between* the others — "what are the cross-cutting security assumptions?" This is where synthesis lives.

**Tune granularity.** A granularity-2 tree has 2 sub-questions per level; granularity-5 has 5. Deeper trees cost more. Tune to what the investigation genuinely needs.

---

## Publishing and reputation

Question sets participate in the economy like other contributions:

- Pulling a question set may be free or priced.
- When used, rotator arm royalties flow to the author.
- Reputation accrues based on adoption and outcomes.

A question set that many operators adopt and report good outcomes from is itself a reputation signal — "the @foo/security-audit-v3 is the gold standard" is a sentence that can emerge.

---

## Supersession and evolution

Question sets are contributions; new versions supersede old. Breaking changes (different apex, incompatible structure) should bump the major version. Refinements to the tree can supersede within a version.

Consumers on an older version get notified when you publish an update and can accept or decline. This is how a question set improves over time without forcing a fork for every change.

---

## Question sets vs chain variants

Both are ways to standardize investigation. When to pick which:

- **Question set** — you want to standardize *what's asked*. The decomposition is the product. The chain can be the default.
- **Chain variant** — you want to standardize *how it's built*. The question can be whatever; the pipeline structure (extraction emphasis, synthesis strategy, error handling) is the product.

A fancy security audit is usually a question set. A code-pyramid chain that puts heavier emphasis on architecture and less on data flow is usually a chain variant. You can ship both — they compose.

---

## Where to go next

- [`24-asking-questions.md`](24-asking-questions.md) — question pyramids in detail.
- [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md) — chain variants as the complement.
- [`44-authoring-skills.md`](44-authoring-skills.md) — another contribution type that often rides alongside question sets.
- [`61-publishing.md`](61-publishing.md) — publish mechanics.
