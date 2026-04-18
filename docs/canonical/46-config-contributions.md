# Config contributions

Many of the numbers, policies, and thresholds that govern Agent Wire Node's behavior are stored as **config contributions** — YAML configs with a schema type, rendered into UI widgets, editable through the YAML-to-UI renderer, versioned in the contribution store, and publishable to the Wire.

This is the layer where you change behavior without touching chains or prompts. If a chain's step takes `model_tier: extractor`, the policy that maps `extractor` to a concrete `(provider, model)` pair lives in a config contribution. If folder ingestion uses a minimum file count to decide pyramid-vs-loose-file, that count lives in a config contribution. If DADBEAR has a debounce minutes value, it's a config contribution.

Config contributions are also how Agent Wire Node avoids hardcoding numbers into code or prompts — any number that would otherwise live as a Rust constant lives here, where operators and agents can tune it.

---

## Examples of config contributions

- **Tier routing** — the mapping from tier names (`extractor`, `synth_heavy`, `stale_local`, etc.) to `(provider_id, model_id)`. See [`50-model-routing.md`](50-model-routing.md).
- **Provider registry entries** — definitions of LLM providers (base URL, auth variable, capabilities). See [`52-provider-registry.md`](52-provider-registry.md).
- **Folder ingestion heuristics** — thresholds for deciding pyramid vs vine vs loose files. Min file count, homogeneity threshold, file type filters, ignored patterns.
- **DADBEAR policies** — per-layer debounce minutes, runaway threshold, stale-check tier.
- **Absorption policies** — rate limits and daily caps for incoming questions on published pyramids.
- **Evidence triage policies** — cost reconciliation, fail-loud thresholds, broadcast requirements.
- **Cost estimation defaults** — what to assume about chunk sizes and per-step input/output tokens when estimating cost ahead of build.
- **Chain assignment policy** — which chain to use for which content type by default.
- **LLM cache policies** — TTL, max size, eviction strategy.
- **Generation skills** — the prompts used when generating configs from intents.

Each of the above is a **schema type** in the schema registry. See [`47-schema-types.md`](47-schema-types.md).

---

## How config contributions work

Every config contribution is:

1. **Stored as YAML** — the raw source of truth.
2. **Described by a schema** — which fields exist, their types, validation rules.
3. **Annotated by a schema annotation** — UI metadata (widget types, grouping, descriptions, help text) that tells the YAML-to-UI renderer how to render the YAML as editable widgets.
4. **Refined by a generation skill** — the prompt used when you give the authoring wizard an intent ("make extraction cheaper for me") and the LLM produces or modifies the YAML.
5. **Versioned by supersession** — each edit creates a new contribution that supersedes the prior; nothing is overwritten.
6. **Active via the schema registry** — the registry resolves "current value for schema type X" to the latest active contribution.

When a build runs, the chain executor reads the active contribution for the relevant schema type through the registry. Editing a contribution creates a new active version; the next build reads the new values.

---

## The YAML-to-UI renderer

When you open a config contribution in Tools mode, you don't edit raw YAML by default — you see the rendered form:

- A **text input** for a string field.
- A **number input** with min/max/step for a numeric field.
- A **slider** for a bounded numeric range.
- A **toggle** for a boolean.
- A **dropdown** for an enum.
- A **list editor** with add/remove/reorder for arrays.
- A **nested group** for sub-objects.
- A **readonly widget** for derived fields.

The mapping from field to widget comes from the **schema annotation** — a sibling contribution that describes how to render each field. Multiple rendering conventions are possible; an operator who prefers a different UI can pull or author a different schema annotation for the same schema type.

You can always flip the renderer into raw YAML mode to edit directly. Both paths save the same underlying YAML.

---

## The generative config loop

Beyond editing values, you can refine a config contribution by describing what you want in natural language:

1. You open a config contribution (say, tier routing).
2. The current version is shown.
3. You type a note: *"Make extraction cheaper. I don't care about the absolute quality of L0 nodes as long as they're still accurate; synthesis should stay on the heavier tier."*
4. The LLM (via the schema's generation skill) reads the current YAML, your note, and the schema. It produces a refined YAML.
5. You review the diff. Accept (creates a new superseding contribution) or reject (discard).

This loop is the "generative config" pattern. Every behavioral configuration in Agent Wire Node flows through it — intent in, YAML out, review, accept. New contributions are versioned and shareable.

---

## Assignment scopes

A config contribution can apply at different scopes:

- **Global** — applies to everything on the node unless overridden. Default scope.
- **Per-content-type** — applies only when the content type matches (e.g. a tier routing variant just for document pyramids).
- **Per-pyramid** — applies only to a specific slug.
- **Per-chain-step** — the narrowest scope, overrides routing or policy for a single `(slug, chain_id, step_name)` triplet.

Lower scopes override higher scopes. When the executor needs a policy value, it checks narrowest first.

Assignment is done from the contribution's detail card in Tools mode, or from the relevant Settings page (tier routing has its own page, for example).

---

## Config contributions on the Wire

Like other contributions, configs can be published:

- **Tier routing defaults** for common profiles ("OpenRouter-heavy", "Ollama-local", "mixed").
- **DADBEAR policy** profiles for different use cases ("aggressive auto-update", "cheap-and-slow staleness", "paused-by-default").
- **Folder ingestion heuristics** tuned for specific codebase shapes.
- **Absorption policies** for paid pyramids vs. free ones.

Pulling a config from the Wire gives you a starting baseline; you can tune further from there. Operators who craft excellent default configs for common profiles build reputation and earn rotator arm royalties whenever their config is consumed.

---

## Migrations

Config schemas evolve. When a schema gains a new required field, removes a field, or renames one, existing config contributions using the old shape get flagged as "needs migration" in Tools mode. The migration review shows:

- Current YAML (old schema).
- Target YAML (new schema).
- Breaking changes.
- Non-breaking changes (optional fields with defaults).

You accept the migration (creates a new superseding contribution with the new shape) or postpone it (the old version keeps working as long as backwards compatibility allows).

Most migrations are additive (new optional fields) and don't require action. Breaking migrations are rare and flagged prominently.

---

## Why this pattern matters

The alternative — hardcoding numbers in Rust constants — has a specific failure mode: every operator is stuck with whatever the binary's authors decided. When the number turns out to be wrong for your setup, you're either waiting for a release with a different default, forking the binary, or flipping the flag if one exists.

Config contributions make the numbers **content, not code**:

- You tune them for your node without waiting for anyone.
- You publish your tuning, and others can adopt or iterate.
- The tuning improves over time as operators share what works for what profiles.
- The same mechanism scales from "one number" to "a whole policy bundle" — the schema describes what can change, the contribution is the value, the registry resolves the active one.

It's also the substrate the steward experimentation vision builds on. A steward that automatically tunes configs is just an agent that reads metrics, proposes a new config contribution, waits for measurement, and either keeps it active or reverts. See [`05-steward-experimentation-vision.md`](05-steward-experimentation-vision.md).

---

## Where to go next

- [`47-schema-types.md`](47-schema-types.md) — the types that configs conform to.
- [`50-model-routing.md`](50-model-routing.md) — tier routing as a worked example.
- [`34-settings.md`](34-settings.md) — the UI surfaces for common configs.
- [`28-tools-mode.md`](28-tools-mode.md) — authoring flow.
- [`05-steward-experimentation-vision.md`](05-steward-experimentation-vision.md) — where automated config tuning is headed.
