# Tools (contributions you author)

The **Tools** mode is where you author, manage, and publish **contributions** — the reusable units that shape how Wire Node works. Chains, skills, templates, question sets, and other configs all live here.

If Understanding is about the pyramids you build and Knowledge is about the documents you work with, Tools is about the machinery you use to build them. Changing the machinery is how you make Wire Node do what the defaults don't.

---

## What lives in Tools

Every contribution on your node shows up here, grouped by type:

- **Actions** — one-shot operations (publish, pull, etc.). Generally system-provided; you rarely author these.
- **Chains** — YAML pipelines that define how a build runs. See [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md).
- **Skills** — prompts + generation procedures that the LLM uses during builds. See [`44-authoring-skills.md`](44-authoring-skills.md).
- **Templates** — schema definitions, schema annotations, question sets, seed defaults. See [`45-question-sets.md`](45-question-sets.md) and [`47-schema-types.md`](47-schema-types.md).

Each contribution has a card showing:

- **Title.**
- **Type badge** (colored by category).
- **Description.**
- **Published / unpublished indicator.**
- **Actions** — Edit (switches to the Create tab with this contribution loaded), Publish, Delete.

Contributions marked **needs migration** are those whose schema has been updated since they were created, and accepting the migration is recommended before using them again. Click the chip; the migration review modal shows old-vs-new YAML with changes highlighted; accept, reject, or postpone.

## Tabs inside Tools mode

- **My Tools** — all your local contributions, grouped by type.
- **Needs Migration** — only appears if any contributions are flagged for migration.
- **Discover** — browse contributions published to the Wire (see [`62-pulling-and-discovery.md`](62-pulling-and-discovery.md) for the Search mode equivalent; this tab is more focused on tool-type contributions).
- **Create** — a multi-step wizard for authoring a new contribution from scratch or from an intent.

---

## The Create wizard

Clicking **Create** (or editing an existing contribution) opens a multi-step interactive wizard:

### Step 1: Schema picker

Pick the type of contribution you're authoring. Options include:

- A chain variant for a specific content type.
- A skill for a specific step primitive.
- A schema or schema annotation.
- A question set.
- A tier routing config.
- A folder ingestion heuristic config.
- Other registered schema types.

The schema you pick determines what fields the wizard collects.

### Step 2: Intent

Describe what you want the contribution to do, in plain language.

> *"I want a chain variant for code pyramids that focuses on security properties, with deeper extraction on auth flows and input validation."*

The LLM uses this intent (plus the schema and any existing seed) to generate an initial draft.

### Step 3: Draft generation

The LLM produces an initial YAML from your intent. You see the generated draft in a code editor. You can scrap it and regenerate with a revised intent, or proceed.

### Step 4: Render / refine

The wizard switches to a rendered view — the YAML is displayed through the **YAML-to-UI renderer** using the contribution's schema annotation. Each field becomes an editable widget (text input, number input, slider, toggle, dropdown, list editor, nested group). You can edit directly, or add notes and have the LLM refine.

Refining is iterative:

- Edit a value directly → saves.
- Add a note ("increase extraction granularity for auth-related files") → LLM refines → new version you can accept or discard.

Each refinement creates a new version in the supersession chain; you can walk back if you don't like a refinement.

### Step 5: Preview (dry run)

Before you publish or accept, the wizard runs a **dry run**:

- **Visibility** — what access tier this will be published at.
- **Cost estimate** — for contributions that describe builds (chains), estimate the cost per invocation.
- **Warnings** — credentials referenced, unusual configuration choices, potential conflicts with active contributions of the same schema type.
- **Supersession chain** — what this contribution supersedes (if anything).

### Step 6: Publish or save locally

Two buttons:

- **Save locally** — your contribution becomes active on your node without going to the Wire. You can use it immediately in builds.
- **Publish** — save locally AND publish to the Wire. You pick access tier, price, and any extra metadata. Other operators can pull the published contribution.

You can always publish later from the My Tools tab if you save locally first.

---

## Authoring without the wizard

The wizard is the friendly path. You can also author contributions directly by editing YAML files.

Chains and prompts live under `chains/` in the Wire Node data directory. The shipped defaults are in `chains/defaults/` (don't modify those — they're tracked as bundled contributions). Your variants go in `chains/variants/`.

```
~/Library/Application Support/wire-node/chains/
├── defaults/           — shipped with the app; read-only conceptually
├── variants/           — your variants; edit freely
└── prompts/
    ├── defaults/
    └── variants/
```

Edit a variant YAML → the next build that uses it picks it up. No restart needed.

See [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md) for the chain YAML structure and [`42-editing-prompts.md`](42-editing-prompts.md) for the prompt markdown conventions.

---

## Managing existing contributions

The My Tools tab shows everything you have. Useful operations per contribution:

- **Edit** — opens the Create wizard pre-filled with the current YAML. Save to supersede.
- **Publish** — push to the Wire if you haven't already.
- **Unpublish / retract** — pull it from the Wire (the contribution stays local).
- **Delete** — archive (never actually deletes; you can unarchive).
- **Copy** — duplicate to author a variant.
- **Diff against parent** — if you pulled this from someone else's contribution, see how yours differs.

### Assignments

Some contributions (chains, tier routing) can be assigned at different scopes:

- **Global** — used by all pyramids unless overridden.
- **Per-pyramid** — used by one specific pyramid (e.g. a code chain variant only for one repo).
- **Per-step** — for tier routing, override for a specific chain step.

Assignment is done from the contribution card or from the relevant Settings page. See [`46-config-contributions.md`](46-config-contributions.md).

---

## The "needs migration" flow

When a schema evolves (e.g. tier routing adds a new required field), existing contributions of that schema might no longer match. They still work, but accepting the migration updates them to the new shape.

The Needs Migration tab shows flagged contributions. Click one to see:

- **Current shape** (old schema).
- **Target shape** (new schema).
- **Breaking changes** — fields added, removed, renamed.
- **Non-breaking changes** — optional fields, defaults.

Three actions:

- **Accept** — apply the migration. Creates a new superseding version of the contribution with the new shape; defaults filled in where applicable.
- **Reject** — mark as "don't migrate"; the contribution continues to work with the old shape as long as the schema's backwards-compatibility window allows.
- **Postpone** — leave flagged; decide later.

Migrations are rare but important to stay on top of — an unsigned-off migration can become required in a future release.

---

## Publishing your first contribution

The easiest first contribution to author is a **chain variant**. It teaches you the whole workflow and produces something immediately useful.

1. Open Tools → Create.
2. Pick "Chain variant" → base it on `code.yaml`.
3. Intent: "emphasize architectural decisions over line-by-line explanation."
4. Review the generated draft.
5. Refine with notes if needed.
6. Save locally.
7. Go to one of your code pyramids; assign this variant in its detail drawer.
8. Trigger a rebuild. You should see somewhat different extraction behavior.
9. If you like it, publish. Other operators can pull it.

See [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md) for deeper guidance on what to change in a chain variant.

---

## Contributions and the Wire

Every published contribution has a **handle-path** — `@you/contribution-name/v1` style. That handle-path is how other operators cite and pull your work. It's durable: the handle-path never changes; new versions get new suffixes.

When you publish:

- The contribution's YAML is sent to the Wire.
- Credentials in the YAML (as `${VAR}` references) are preserved; actual secret values are never sent.
- Metadata (type, tags, requires-credentials tags) is auto-injected.
- The coordinator allocates your handle-path and registers it globally.

When someone pulls:

- They get the YAML.
- They resolve the `${VAR}` references against their own credentials file.
- The contribution becomes active in their Tools mode.

See [`61-publishing.md`](61-publishing.md) and [`62-pulling-and-discovery.md`](62-pulling-and-discovery.md).

---

## Where to go next

- [`40-customizing-overview.md`](40-customizing-overview.md) — what you can customize and why.
- [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md) — deep dive on chain authoring.
- [`42-editing-prompts.md`](42-editing-prompts.md) — editing the markdown prompts.
- [`44-authoring-skills.md`](44-authoring-skills.md) — skill contributions.
- [`45-question-sets.md`](45-question-sets.md) — question-set contributions.
- [`47-schema-types.md`](47-schema-types.md) — what a schema type is and how to add one.
