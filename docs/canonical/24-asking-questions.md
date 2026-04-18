# Asking questions and question pyramids

Once you have one or more pyramids, the most powerful thing you can do is ask questions against them. A question against a pyramid creates a **question pyramid** — a derivative pyramid that decomposes the question, pulls evidence from the source pyramid(s), and synthesizes answers into a new queryable structure.

Questions compose. A question pyramid can reference other question pyramids. Answers accumulate across questions. This is where the "the tenth question is nearly free" effect comes from.

---

## Asking a single-source question

From any pyramid's detail drawer, click **Ask question**.

The question modal asks for:

- **Question text** — the apex question. Plain language. The more specific the better, but broad questions work too.
- **Slug** (optional) — the identifier for the resulting question pyramid. If you don't provide one, it's derived from the question text.
- **Advanced options** (collapsed by default):
  - **Granularity** — how many sub-questions per decomposition level. Default 3. Higher means wider decomposition, more sub-questions, more evidence coverage, more cost.
  - **Max depth** — how deep the decomposition goes. Default 3. Deeper means more fine-grained sub-questions.
  - **Manual reference override** — if you want to hand-pick which source pyramids the question references (default: just this one).

Click **Create slug** and the build starts. The Pyramid Surface opens with the new question pyramid as it's being built.

### What happens during the build

1. **Decomposition** — your apex question becomes a tree of sub-questions.
2. **Diff against existing structure** — each sub-question is compared to what's already in the source pyramid. Already-answered sub-questions become cross-links. Partially-answered ones inherit the existing answer and only fill the gaps. Entirely new ones get queued for fresh extraction.
3. **Targeted L0 extraction (if needed)** — if the question asks about aspects the existing L0 doesn't cover, the system does targeted re-examinations of specific source files with an extraction prompt shaped by the new question's needs.
4. **Evidence answering** — leaf sub-questions get answered from the assembled evidence.
5. **Synthesis** — branch answers fold up into the apex.
6. **Done.**

The new question pyramid has its own apex (the answer to your question) and its own sub-questions (answers to the decomposition's leaves). It's fully queryable. You can ask follow-up questions against *it* — they will reference it and indirectly the source.

### Examples

A source code pyramid + follow-up questions:

- *"What modules handle user authentication?"*
- *"Where does input validation happen?"*
- *"What would a new developer need to know first?"*
- *"What are the security properties of the session handling code?"*

A document pyramid + follow-up questions:

- *"What are the main claims and what supports them?"*
- *"What counter-arguments does the text engage with?"*
- *"Where does the text contradict itself?"*
- *"What would someone unfamiliar with this field need to know?"*

A conversation pyramid + follow-up questions:

- *"What decisions were made in the last month?"*
- *"What was Alice's position on the API refactor?"*
- *"What problems keep coming back unresolved?"*

---

## Asking questions across multiple pyramids

A question pyramid can reference **multiple** source pyramids. This is how you compose knowledge across sources.

Two ways to set this up:

**From the `pyramid-cli`** (quickest if you're scripting):

```bash
pyramid-cli create-question-slug my-question --ref codebase-v1 --ref codebase-v2
pyramid-cli question-build my-question "What breaking changes exist between v1 and v2?"
```

**From the UI:**

1. Create an empty question pyramid via Add Workspace → "Question pyramid" option.
2. In its detail drawer, add references to source pyramids via the "References" section.
3. Click **Ask question** and give it the apex question.

Multi-source question pyramids draw evidence from all referenced sources. They are the path to:

- Diff-style questions (*"What changed between X and Y?"*).
- Comparison-style questions (*"How does project A handle errors differently from project B?"*).
- Integration-style questions (*"How do these two specs need to fit together?"*).
- Cross-domain questions (*"What does my codebase's auth model look like under the threat model in this paper?"*).

The resulting question pyramid cites evidence across sources; drilling shows which source each evidence node came from.

---

## Derivative questions (questions on questions)

Because a question pyramid is just a pyramid, you can ask questions against it. From a question pyramid's detail drawer, click **Ask question** again.

This chains. You can have a five-deep tower of question pyramids, each refining the previous. Common patterns:

- **Broad → narrow.** "What is this codebase?" → "How does module X work?" → "What edge cases does function Y handle?"
- **Decompose → recompose.** Ask several parallel sub-questions, then a synthesizing question that references all the answers.
- **Hypothesis → test.** "What would break if we changed Z?" → "What parts of the change are we most confident about?" → "What parts should we double-check before shipping?"

Each level's evidence is reusable by the next. Answering increasingly specific questions gets increasingly cheap.

---

## How the decomposer is smart about existing work

The decomposer does not re-do work that's already been done. When it sees your question:

1. It decomposes into sub-questions.
2. For each sub-question, it asks: *"Is there already an answer to this (or something like it) in the source pyramid?"*
3. If yes → cross-link (KEEP verdict to the existing answer node). **No new LLM call for this sub-question.**
4. If partially → inherit the existing answer; only the answer's MISSING verdicts trigger new work.
5. If no → full decomposition into leaf questions and fresh evidence gathering.

This is why the 10th question on a rich pyramid is almost free. Most of the work it would need has already been done by prior questions.

You can see this in the build log. Sub-questions marked "cross-linked" did no new work; sub-questions marked "partial match" did gap-filling; sub-questions marked "new decomposition" did fresh work.

---

## Granularity and max_depth, and when to change them

Defaults (granularity 3, max_depth 3) produce roughly 3³ = 27 leaf sub-questions for a full decomposition. This is the sweet spot for most questions.

Bump granularity when:
- You want more coverage. More sub-questions = more evidence touched = more comprehensive answer.
- Your question is multi-part and a wider decomposition fits it better.

Bump max_depth when:
- You want more specificity at the leaves. Deeper decomposition = more fine-grained evidence queries.
- The material is dense and you want the pyramid to go more granular.

Both cost more. Double granularity, double cost (roughly). Double max_depth, double cost again.

Lower either when:
- The question is simple and doesn't need elaborate decomposition.
- You're exploring and willing to trade depth for speed.
- Budget is tight.

---

## What to do if the answer feels off

A question pyramid that comes out looking wrong has a few common causes and fixes:

**The apex synthesis is vague.** Usually the sub-questions are too abstract. Ask a more specific version of the same question. Alternatively, drill into the sub-question answers — they're often more useful than the apex.

**The answer cites evidence but the cited evidence doesn't support the claim.** This is a synthesis bug. Reroll the apex with a note: "ensure the synthesis faithfully reflects the cited evidence; don't over-generalize."

**The pyramid missed an important aspect of the question.** Look at the decomposition (visible in the Pyramid Surface, or via `pyramid-cli drill` on the apex's question_tree). If a sub-question you expected isn't there, the decomposer didn't see the aspect. You can either ask a more explicitly-phrased version, or bump granularity.

**The evidence has MISSING verdicts for things you know exist.** The pre-mapper didn't find the relevant L0. Sometimes this means the L0 extraction (from an earlier build) didn't surface the right material. Rebuild the source pyramid with a question shaped around what's missing, then ask the question again.

**Answer is hallucinated.** Rare, but possible with over-summarization. Reroll with: "ground every claim in a specific evidence node by ID."

## Annotating the question pyramid

Once you have an answer you like, annotate the nodes that were load-bearing in getting you there. The annotation with a question context feeds the FAQ, and the next time someone asks something similar the FAQ entry is the first thing they see.

Annotate from the node inspector. See [`26-annotations-and-faqs.md`](26-annotations-and-faqs.md).

---

## Where to go next

- [`26-annotations-and-faqs.md`](26-annotations-and-faqs.md) — how your annotations improve future questions.
- [`23-pyramid-surface.md`](23-pyramid-surface.md) — visualize a question pyramid.
- [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md) — question pyramids use a chain too; customize it.
- [`82-querying-pyramids.md`](82-querying-pyramids.md) — how agents ask questions programmatically.
