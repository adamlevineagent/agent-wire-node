# Authoring skills

A **skill** is a publishable prompt-plus-targeting bundle: a markdown prompt, a specification of which step primitive it's for, any schema it produces, and optionally default tier routing. Other operators pull your skill, and their chains can invoke it by name instead of pointing at a local prompt file.

If a prompt is the raw material and a chain is the orchestration, a **skill is a prompt packaged for reuse**. It's the unit of sharing the model-shaping work.

---

## Skill vs prompt vs chain

- A **prompt** is a markdown file on disk; it lives in your own prompts directory and your chain points at it directly.
- A **skill** is a published contribution that wraps a prompt, adds metadata (which primitive, expected inputs, output schema, author handle), and is pullable from the Wire.
- A **chain** is the orchestrator. A step in a chain can point at a local prompt (`instruction: $prompts/…`) or at a published skill contribution by handle-path.

You author locally as prompts. You **publish as skills**. You **pull others' work as skills**. Skills are the Wire-level unit; prompts are the filesystem-level unit.

---

## When to author a skill vs just keep a local prompt

Keep it as a local prompt if:

- It's specific to your chain's structure and wouldn't help anyone else.
- You're iterating rapidly and don't want the publish overhead on every change.
- It references private context that can't be published.

Author and publish a skill if:

- The prompt does one focused thing (extraction of a specific kind, synthesis with a specific shape, classification into your preferred taxonomy).
- Other operators with similar pyramids would benefit.
- The quality is high enough to stand on its own — you've iterated, you're confident.
- You want reputation credit for the work.

A great skill tends to be a focused prompt paired with a clear schema, not a monolithic prompt that tries to do several things.

---

## Skill structure

Conceptually a skill has:

- **Handle-path** — `@you/skill-name/v1` once published.
- **Target primitive** — one of `extract`, `classify`, `synthesize`, `web`, `compress`, `fuse`.
- **Prompt markdown** — the actual text the LLM sees.
- **Input schema** — what fields the prompt expects via `{{variable}}` slots.
- **Output schema** — JSON shape of the output (same as `response_schema` on a chain step).
- **Default tier** — suggested `model_tier` this skill is tuned for.
- **Tags** — topics and intended use cases.
- **Required credentials** — auto-injected at publish time if the prompt or metadata references `${VAR_NAME}`.
- **Description** — short human description for Search.

Authoring flow in Tools mode:

1. Pick "Skill" as the contribution type.
2. Intent: describe what the skill should do.
3. Draft: LLM generates an initial prompt + schema from your intent plus a schema annotation.
4. Refine: edit in the renderer; add notes for LLM refinement; iterate.
5. Preview: dry run. See exactly what gets published.
6. Publish: assigns the handle-path and pushes to the Wire.

---

## Using a pulled skill in your chain

When you pull a skill into your local store, it shows up in Tools. Reference it in a chain step like:

```yaml
- name: extract_with_my_skill
  primitive: extract
  skill: "@someone/architectural-extract/v3"
  for_each: "$chunks"
  save_as: node
  depth: 0
  node_id_pattern: "Q-L0-{index:03}"
  model_tier: extractor
```

The `skill:` field references the pulled contribution by handle-path. The step uses the skill's prompt, enforces its output schema, and respects its default tier (unless you override with `model_tier`).

> **Status:** the `skill:` field is planned-but-not-yet-fully-shipped. Current workaround: pulling a skill lands its prompt in your `chains/prompts/variants/` directory, and you reference it via `instruction: $prompts/variants/...` like any other local prompt. Metadata (schema, tier default, tags) is tracked in the contribution store even though the ergonomic `skill:` field isn't yet the wired path. Treat the `skill:` form as "what authoring will look like when this lands."

---

## Anatomy of a good skill

The best skills share a few properties:

**Focused intent.** "Extract architectural decisions from source code" is focused. "Extract everything about this codebase" is not — that's a chain's job, not a skill's.

**Explicit negative constraints.** What does this skill NOT do? List it. Skills that ride well usually have a "WHAT DOES NOT BELONG" section in the prompt body.

**Tight schema.** Every field in the output JSON should have a clear purpose. If a field is optional, the description should say when to include it. Loose schemas produce inconsistent outputs.

**Tier honesty.** If the skill works on a mid-tier model and doesn't need a heavy one, say so. If it needs reasoning-mode, say so. Over-specifying "needs the biggest model" when a mid one works fine hurts adoption.

**Teaches `apex_ready` for cluster skills.** Any skill targeting `classify` or `synthesize` in a `recursive_cluster` loop must teach the LLM the `apex_ready: boolean` signal.

**Works with projection.** If the skill will run inside a step with `item_fields`, the prompt should only reference projected fields.

**Ends with `/no_think`.**

---

## Rotator arm and skill authorship

Skills participate in the economy. When your skill is used by someone's chain, your author share flows via the rotator arm. The rate depends on:

- Whether the skill is priced at pull time or free.
- How "used" is defined (per-invocation? per-pyramid? per-build?). The Wire's contribution mapping handles this; you set the policy at publish time.
- The rotator arm default split (76% creator, 2% platform, 2% treasury, remainder reserved for roles like relays once shipped). See [`74-economics-credits.md`](74-economics-credits.md).

This means a well-tuned skill that gets widely adopted is a real revenue stream. It's also incentive-compatible: the skill that produces better outputs earns more because it gets adopted more. No need to price it high up front; let usage carry the signal.

---

## Supersession

Skills are contributions. When you publish an improved version, it supersedes the previous via `supersedes_id`. Consumers on older versions get notified that an update is available and can accept or decline.

Breaking changes (schema field renamed, new required input) should be versioned clearly — `@you/my-skill/v1` vs `@you/my-skill/v2`. Non-breaking refinements can supersede within the same version series; consumers get rolling updates.

---

## Common skill types (examples to aim for)

**Extraction skills:** `architectural-extract`, `security-extract`, `decision-extract`, `glossary-extract`, `entity-extract`.

**Classification skills:** `thread-cluster`, `security-classify`, `complexity-classify`.

**Synthesis skills:** `narrative-synthesize`, `comparative-synthesize`, `timeline-synthesize`.

**Web skills:** `architectural-web`, `dependency-web`, `cross-cutting-web`.

**Compression skills:** `forward-compress`, `reverse-compress`, `context-compress`.

Many more niches will emerge as operators ship domain-specific skills — "extract TLA+ specifications from papers," "classify incident post-mortems by failure mode," "synthesize a codebase's state-management patterns."

---

## Where to go next

- [`42-editing-prompts.md`](42-editing-prompts.md) — the prompt authoring conventions skills inherit.
- [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md) — how chains call skills.
- [`28-tools-mode.md`](28-tools-mode.md) — the UI for authoring and publishing skills.
- [`61-publishing.md`](61-publishing.md) — publish mechanics.
- [`74-economics-credits.md`](74-economics-credits.md) — rotator arm economics.
