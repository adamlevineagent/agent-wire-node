# Schema types

A **schema type** is a registered category of configurable data. Every config contribution in Agent Wire Node has a `schema_type` that identifies what kind of thing it is — `tier_routing`, `dadbear_policy`, `folder_ingestion_heuristics`, and so on.

Most operators never author a new schema type; the shipped set covers the common cases. This doc is for the rarer situation where you genuinely need a new category of configurable data that the existing schemas can't express.

---

## When to author a schema type

You want a new schema type when:

- You're adding a capability to the system whose behavior is driven by values that should be editable, versioned, and shareable.
- Existing schema types don't fit the data naturally.
- The data has enough structure (multiple fields, validation rules) that a flat key-value pair isn't enough.

You don't want one when:

- The data would fit naturally as a field inside an existing schema. Extend the existing schema.
- The data is a one-off value with no structure — use a generic config field.
- The data describes a pipeline or LLM behavior — that's a chain, skill, or prompt, not a schema type.

In practice, shipping a new schema type is rare. Most extension happens through adding fields to existing schemas or authoring new chains and skills against existing schemas.

---

## The four things a schema type needs

A fully-functional schema type is a bundle of four contributions that reference each other:

### 1. Schema definition

A JSON Schema (or equivalent) describing the fields, types, constraints, and validation rules of the YAML.

```yaml
schema_type: my_policy
version: 1
definition:
  type: object
  required: ["threshold", "mode"]
  properties:
    threshold:
      type: number
      minimum: 0
      maximum: 1
      description: "Trigger threshold (0.0–1.0)."
    mode:
      type: string
      enum: ["strict", "lenient", "adaptive"]
      description: "Enforcement mode."
    per_pyramid_overrides:
      type: object
      additionalProperties:
        $ref: "#/properties/threshold"
      description: "Per-slug threshold overrides."
```

The schema definition is the source of truth for what fields are valid.

### 2. Schema annotation

UI metadata that tells the YAML-to-UI renderer how to render the schema's fields as editable widgets.

```yaml
schema_type: my_policy
schema_annotation:
  threshold:
    widget: slider
    min: 0
    max: 1
    step: 0.01
    help: "Lower values trigger more often. Tune with the cost monitor open."
  mode:
    widget: dropdown
    options:
      - value: strict
        label: "Strict (fail on miss)"
      - value: lenient
        label: "Lenient (warn, don't fail)"
      - value: adaptive
        label: "Adaptive (auto-tune)"
    help: "Strict is best when you have room for a fail fast; adaptive works for noisy environments."
  per_pyramid_overrides:
    widget: list
    item:
      widget: nested
```

The annotation is what turns a raw YAML schema into an editable form. Different annotations can produce different UIs over the same schema (e.g. a beginner-friendly annotation vs an advanced one).

### 3. Generation skill

A prompt used when an operator provides a natural-language intent for a config of this schema type and the LLM produces a YAML draft.

```markdown
You are generating a my_policy config from the operator's intent.

The schema is:
{{schema_definition}}

The operator wrote:
{{intent}}

Here is the current active config (if any):
{{current_config}}

Produce a refined YAML config that reflects the intent. Do not invent fields; stick to the schema. Prefer adjusting existing fields over restructuring. If the intent is unclear, make conservative changes and list your assumptions as comments.

Output YAML only:
...

/no_think
```

### 4. Default seed

An initial config value shipped with the app or published as a starting point.

```yaml
schema_type: my_policy
version: 1
threshold: 0.5
mode: adaptive
per_pyramid_overrides: {}
```

The seed is what a fresh install uses before any operator-authored or pulled configs exist.

These four — schema, annotation, generation skill, default seed — are all contributions. They can each be independently superseded, forked, published, and pulled. An operator can adopt your schema with a different UI annotation, or your annotation with their own generation skill.

---

## The shipped schema types (non-exhaustive)

These are the current shipped schema types in Agent Wire Node. The set grows over time:

- `tier_routing` — tier-to-model mapping.
- `provider` — LLM provider definition (one per provider).
- `dadbear_policy` — DADBEAR per-pyramid config.
- `folder_ingestion_heuristics` — thresholds for folder-to-pyramid decisions.
- `absorption_policy` — incoming-question rate limits and daily caps.
- `cost_estimation_defaults` — assumptions for pre-build cost estimation.
- `chain_assignment` — which chain to use for what content type or slug.
- `evidence_triage_policy` — cost reconciliation + fail-loud rules.
- `compute_participation_policy` — compute market participation mode.
- `onboarding_config` — node name, storage cap, mesh hosting toggles.
- `step_override` — per-step field overrides for specific `(slug, chain, step)` triplets.
- `generation_skill` — the prompts used when generating configs (meta-schema).
- `schema_annotation` — UI metadata (meta-schema).

Plus many smaller ones. You can query the schema registry to see the full current list.

---

## Authoring a new schema type

The workflow:

1. **Design the schema.** What fields does your schema type have? What are the validation rules? What's the minimum info that describes your concept?
2. **Write the four contributions.** Schema definition, annotation, generation skill, default seed.
3. **Register the schema type.** Publish the bundle to the Wire and register the schema type name with the schema registry.
4. **Write the code path that consumes it.** A new schema type is useless until the executor (or some other component) reads configs of this type and does something with them. This is usually a Rust-level change.
5. **Test the generative loop.** Give the wizard an intent, see if it produces a sensible config. Iterate on the generation skill prompt.

Step 4 is the gating one. A new schema type usually requires a new code path to consume it. If you don't need new code, you probably don't need a new schema type — you need a new field on an existing one.

This is why shipping new schema types is rare. The payoff is when your new capability is sharable and tunable from the get-go. If you add a feature by hardcoding its numbers, operators can't tune it. If you add it through a new schema type, they can.

---

## Relationship to code

Schema types sit at the boundary between content and code:

- The **schema, annotation, generation skill, and default seed** are content (contributions).
- The **code that consumes configs of this type** is code (Rust, in our case).

When you author a schema type, you're defining a contract. The code on the consuming side reads the active contribution for your schema type through the registry and does something with it. As long as the contract is stable, the content side (values, UI, generation prompts) evolves independently of the code.

This is why schema types can evolve forward without breaking builds: the schema supersession chain + the registry's "resolve current active version" mechanism means consumers always see a valid config.

---

## The meta-case: schema types defining schema types

The generation skill and schema annotation are themselves schema types (meta-schemas). This is the recursive part — the system that describes configurable data is itself configurable data.

Practically, this means you can author an alternative generation skill for an existing schema type ("I want the generation LLM to be more conservative when producing tier routing configs for Ollama"), publish it, and consumers can pick yours over the default. Same for annotations — a cleaner, more opinionated UI for a shipped schema type is a valuable contribution.

---

## Where to go next

- [`46-config-contributions.md`](46-config-contributions.md) — what configs look like once schema types are defined.
- [`40-customizing-overview.md`](40-customizing-overview.md) — where schema types fit in the customization stack.
- [`28-tools-mode.md`](28-tools-mode.md) — the UI for authoring.
