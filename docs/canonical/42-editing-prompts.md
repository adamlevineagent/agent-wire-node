# Editing prompts

Chains orchestrate builds; **prompts** are what the LLM actually sees. A prompt is a markdown file with `{{variable}}` slots that the chain executor fills in at runtime. When you want to change what a step asks the model to do, you're editing a prompt.

Editing a prompt is usually the cheapest and fastest way to improve build quality. The chain's structure is stable; the prompt is where the model's behavior gets shaped. This doc covers how prompts work in the shipped chain executor, the conventions they follow, and how to author good ones.

---

## Where prompts live

```
chains/prompts/
├── question/                — prompts used by question-pipeline (the canonical chain)
│   ├── source_extract.md
│   ├── decompose.md
│   ├── decompose_delta.md
│   ├── enhance_question.md
│   ├── extraction_schema.md
│   ├── pre_map.md
│   ├── answer.md
│   ├── answer_merge.md
│   ├── synthesis_prompt.md
│   ├── question_web.md
│   ├── targeted_extract.md
│   ├── web_cluster.md
│   ├── horizontal_review.md
│   └── characterize.md
├── code/                    — legacy per-content-type prompts
├── document/
├── conversation/
├── conversation-episodic/
├── conversation-chronological/
├── vine/
├── shared/                  — cross-chain utilities (heal_json, merge_sub_chunks, change_manifest)
├── generation/              — prompts used when generating configs/chains
├── migration/               — prompts used for config migration
└── planner/                 — prompts used by the planner
```

Chain steps reference a prompt via `instruction: "$prompts/question/source_extract.md"`. The `$prompts/...` prefix is resolved against the prompts directory in your runtime data dir.

If you want to override a prompt without changing the shipped version, you author a **chain variant** that references a different prompt path, or you edit in place (and accept that updates may overwrite your edits). The Tools mode's Create wizard handles variant authoring for you.

---

## The shape of a shipped prompt

Looking at an actual prompt (`question/source_extract.md`):

```markdown
You are distilling a single source into a reference card...

YOUR OUTPUT IS A REFERENCE CARD, NOT A REWRITE...

WHAT BELONGS IN A TOPIC:
- The conceptual purpose this source serves...
- How it relates to time...
- The functional value or state mutations...

WHAT DOES NOT BELONG:
- Implementation details...
- Internal mechanics...
- Boilerplate...

RULES:
- Be concrete: actual names, terms, references from the source.
- Topic names are used for clustering — name the concept, not the file.
- The `summary` field is a single-sentence distillation used when...
- Entities: cross-references to other components, systems, or concepts...

Output valid JSON only:
{
  "headline": "2-6 word source label",
  "orientation": "2-3 sentences...",
  "topics": [
    {
      "name": "Topic Name",
      "summary": "...",
      "current": "...",
      "entities": ["..."],
      "corrections": [...],
      "decisions": [...]
    }
  ]
}

/no_think
```

A few things to notice about real shipped prompts:

- **No [SYSTEM]/[USER] markers.** The whole prompt goes as one message to the LLM. Simpler than I've seen some frameworks insist on.
- **Role framing up top.** One or two sentences establishing what the LLM is doing.
- **"What belongs" and "what doesn't belong" lists.** Concrete negative constraints. This is load-bearing — it's often the difference between shallow and useful extraction.
- **Rules.** A short list of prescriptive guidelines.
- **Output JSON example.** Shows the schema the model must conform to. The chain executor additionally enforces `response_schema` when one is set, but the example in the prompt body is still the main way the LLM sees what shape is expected.
- **End with `/no_think`.** This suppresses extended chain-of-thought from reasoning-mode models, which would otherwise waste tokens on internal reasoning that doesn't help the output. Every shipped prompt ends with this.

This is the pattern you should follow when authoring new prompts.

---

## Variable substitution

Anything included in the step's `input` map is available as `{{name}}` in the prompt. Resolution is strict: unresolved `{{foo}}` is a runtime error.

```yaml
# in the chain YAML
- name: enhance_question
  primitive: extract
  instruction: "$prompts/question/enhance_question.md"
  input:
    apex_question: "$apex_question"
    corpus_context: "$refresh_state.l0_summary"
    characterization: "$characterize"
```

```markdown
# in the prompt
Apex question: {{apex_question}}
What we already know about the corpus:
{{corpus_context}}

Characterization: {{characterization}}
```

Variable paths in the prompt are flat. You can't write `{{foo.bar}}`; instead, expose the nested value in the chain's `input` declaration and give it a flat name.

### What else the prompt sees

In addition to the step's explicit `input` map, some prompts in the shipped set pull implicit context like `{{audience}}`, `{{content_type}}`, `{{slug}}`. These are standard globals exposed by the chain executor. If a prompt references them, the chain doesn't need to pass them explicitly.

### Escaping

To write a literal `{{x}}` that shouldn't be resolved, use `\{{x\}}`.

---

## Prompt-writing conventions (from `CHAIN-DEVELOPER-GUIDE.md`)

These are explicit rules from the authoritative guide:

### 1. End with `/no_think`

Every prompt. This is not optional. Reasoning-mode models will otherwise waste tokens on internal chain-of-thought that doesn't make the JSON output any better.

### 2. Specify the exact JSON format

Show an example object with the field names, the types, and the intent for each field. Don't say "return JSON"; show what the JSON should look like.

### 3. Never prescribe counts or ranges

Don't say "produce 3-5 clusters" or "extract between 2 and 7 pieces of evidence." Say "let the material decide" and constrain only by structural invariants ("fewer groups than inputs"). Hardcoded counts are a Pillar 37 violation — they pull the LLM toward fitting a quota instead of reporting what's actually there.

If a count genuinely matters, it should flow in via `{{variable}}` populated from a config contribution, not be hardcoded in the prompt text.

### 4. Teach `apex_ready` in recluster prompts

For `recursive_cluster` steps, the cluster sub-call must be taught the `apex_ready` signal:

> FIRST: Decide if these nodes are ALREADY the right top-level structure.
> If further grouping would only reduce clarity, set `apex_ready: true` and return empty clusters.

This is how pyramids avoid mechanical 23→6→5→4→apex narrowing where the middle layers add nothing. The LLM decides when clustering is done.

### 5. Work with whatever fields are projected

When a step sets `item_fields` to limit which fields are sent to the LLM, the prompt should refer only to the projected fields. If `item_fields: ["node_id", "headline", "orientation"]` is set, don't write "examine the topics and entities of each item" — the topics and entities aren't there.

---

## What makes a good prompt

Beyond the explicit rules, there are habits that produce reliably better extractions:

**Ground in the inputs.** "You are looking at a chunk of Python source code for the authentication module" primes better extraction than "Analyze this code."

**Negative constraints are load-bearing.** "WHAT DOES NOT BELONG" in the source_extract prompt is what keeps L0 from devolving into boilerplate listings. Every extraction prompt benefits from an explicit list of what's out of scope.

**Ask for specific names, terms, references.** Concrete outputs are more useful than abstract ones. "Name the concept, not the file" is a shipped-prompt rule for a reason.

**Use the `summary`/`current` two-level pattern.** Shipped prompts ask for both a one-sentence `summary` and a one-to-three-sentence `current`. This gives downstream steps a choice between a tight headline and a richer paragraph. It's also how the dehydration machinery decides what to drop when the budget is tight.

**Handle edge cases explicitly.** If the input might contain speaker markers, timestamps, empty chunks, boilerplate — say what to do for each. The source_extract prompt has a whole paragraph about handling `--- SPEAKER [timestamp] ---` markers in conversation chunks.

**Iterate.** The first prompt you write is rarely the best. Build a small pyramid, look at a few nodes' outputs in the inspector's Prompt and Response tabs, refine.

---

## Anti-patterns

**Hardcoded counts.** "Extract exactly 5 pieces of evidence." Pillar 37 violation. Cost without benefit.

**Multiple conflicting asks.** "Extract evidence, also summarize the whole thing, also classify the content, also generate a title." One step, one job. Use multiple steps if you need multiple things.

**Elaborate personas.** "You are an expert senior principal staff engineer with 40 years of experience..." doesn't make the output better. Tight task framing does.

**No output format.** Every shipped prompt has a JSON example. Yours should too.

**Prompting against model tendencies.** "Do not be verbose" gets ignored. Constraints in the system around the prompt (response_schema, item_fields, max tokens) work; polite instructions in the prompt body don't.

---

## Tuning a prompt — walkthrough

Suppose code pyramids are extracting L0 nodes that are too generic — every headline is "handles X" with no specific content. Diagnose:

1. Build a small test pyramid.
2. Open a few L0 nodes in the inspector. Check the **Prompt** tab to see the resolved prompt.
3. Check the **Response** tab to see the raw model output. Is the vagueness coming from the prompt or from the model?

If the prompt is thin, improve it:

1. Copy the shipped prompt:
   ```bash
   cp chains/prompts/question/source_extract.md \
      chains/prompts/variants/question/source_extract.md
   ```
2. Author a chain variant that points at the new prompt path.
3. Change the prompt to:
   - Add more specific "WHAT BELONGS / DOES NOT BELONG" lists.
   - Require specific names and references in headlines.
   - Give 2-3 concrete before/after examples of weak vs strong extractions.
4. Rebuild the test pyramid, inspect new nodes.

This iterate loop is where most prompt-level improvement happens.

---

## The cache and prompts

Wire Node caches LLM calls by a hash of `(prompt_content + resolved_inputs + model_id)`. Editing a prompt changes the hash — the next invocation misses the cache and calls the model freshly. Reverting the prompt brings the old cache entries back in reach.

Practically, this means prompt edits are cheap: you don't lose accumulated work when you change a prompt. You just start accumulating new cache entries for the new version.

---

## Publishing prompts

A prompt with a specific purpose can be bundled into a **skill** contribution — a prompt plus a specification of which primitive it targets. Publishing a skill lets other operators pull it and use it in their chains.

See [`44-authoring-skills.md`](44-authoring-skills.md).

---

## Where to go next

- [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md) — the chains that reference prompts.
- [`chains/CHAIN-DEVELOPER-GUIDE.md`](../../chains/CHAIN-DEVELOPER-GUIDE.md) — authoritative reference, ships with the app.
- [`44-authoring-skills.md`](44-authoring-skills.md) — publish a prompt as a shareable skill.
- [`23-pyramid-surface.md`](23-pyramid-surface.md) — inspector's Prompt and Response tabs for debugging.
