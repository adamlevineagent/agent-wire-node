# Customizing Agent Wire Node (overview)

Almost everything in Agent Wire Node is configurable. The chains that define how a build runs, the prompts the LLM sees, the policies that govern DADBEAR, the tier routing that picks which model handles which step — all of it is data, not code. This is a deliberate design choice: the binary is a runtime, and the interesting work is in the **contributions** that run on top of it.

This doc is the map. It tells you what's customizable, which layer to change for which kind of change, and where to go next for detailed walkthroughs.

---

## The customization layers, cheapest to deepest

Start at the top. Work down only if the next layer can't express what you need.

### Layer 0: Settings (tweaks without authoring anything)

Change node-level preferences: credentials, which provider to use, tier routing, local-mode toggle, compute participation policy, auto-update. All UI-driven. See [`34-settings.md`](34-settings.md).

**Use this layer when:** you want to change *which models* are used, *which providers* are called, or *what your node does on the network* — without changing how pyramids get built.

### Layer 1: Contribution config YAMLs

Many kinds of data — tier routing defaults, folder ingestion heuristics, absorption policies, cost reconciliation policies — are contributions with a schema. You edit them through the YAML-to-UI renderer in Tools mode, which shows each field as a widget (text, slider, toggle, dropdown) and lets you refine with natural-language notes.

**Use this layer when:** you want to change a *policy* or *threshold* without touching chains or prompts.

See [`46-config-contributions.md`](46-config-contributions.md).

### Layer 2: Prompts (edit the markdown the LLM sees)

Chains reference prompts — markdown files with `{{variable}}` slots. The LLM sees the resolved prompt at each step. Changing a prompt changes what the model is asked to do for that step; the next build picks up your change.

**Use this layer when:** you like the chain's structure but want the model's instructions to emphasize something different.

See [`42-editing-prompts.md`](42-editing-prompts.md).

### Layer 3: Chain variants (edit the YAML that defines the build)

A chain is the YAML that orchestrates a build. You can author a variant — a modified version of a shipped chain — that changes iteration modes, primitives, step sequence, or step-level config. Variants can be assigned per-pyramid or globally.

**Use this layer when:** you want a structurally different build — e.g. an extra pass of synthesis, a different clustering mode, an extra primitive you want to run.

See [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md).

### Layer 4: Assembling chains (composing multiple chains)

Chains can invoke other chains. You can build higher-order pipelines by composing small chains. This is how vines work, and how complex workflows get structured.

**Use this layer when:** you want to compose a build from multiple sub-builds, or build a pipeline that spans multiple content types.

See [`43-assembling-action-chains.md`](43-assembling-action-chains.md).

### Layer 5: Skills (the thing inside a step)

A skill is a reusable prompt-plus-generation procedure for a specific primitive. Chains call skills by name; authoring a skill means writing a good prompt for a specific intent (e.g. "extract architectural decisions from a code chunk") and publishing it.

**Use this layer when:** you have a great prompt for a common primitive and want it reusable across chains or shareable across operators.

See [`44-authoring-skills.md`](44-authoring-skills.md).

### Layer 6: Question sets (preset decompositions)

A question set is a preset decomposition tree that compiles into a chain. Where a chain directly describes "do this, then this, then this", a question set describes "these sub-questions must be answered for this apex question to be answered."

**Use this layer when:** you want to standardize how a certain class of question gets asked. "What are the security properties of this codebase?" can be a question set with a preset sub-question tree.

See [`45-question-sets.md`](45-question-sets.md).

### Layer 7: Schema types (new kinds of configurable data)

A schema type is a new category of configurable data in Agent Wire Node. Adding a schema type is rare — most of the time the existing schema types cover what you need. But when you really do need a new kind of policy (say, a new kind of rate-limiting rule that no existing schema type captures), you can add one.

**Use this layer when:** nothing else expresses the kind of data you need.

See [`47-schema-types.md`](47-schema-types.md).

---

## Where the files live

All customization happens in files under your Agent Wire Node data directory:

```
~/Library/Application Support/wire-node/
├── chains/
│   ├── defaults/            — shipped chains; read-only conceptually
│   │   ├── code.yaml
│   │   ├── document.yaml
│   │   ├── conversation.yaml
│   │   ├── topical-vine.yaml
│   │   └── question.yaml
│   ├── variants/            — your chain variants
│   └── prompts/
│       ├── defaults/        — shipped prompts
│       │   ├── code/
│       │   ├── document/
│       │   ├── conversation/
│       │   └── shared/
│       └── variants/        — your prompt edits
└── ...
```

The default set is what ships with the app. Do not modify defaults — they track as bundled contributions and get overwritten on app update. Instead, copy the default you want to change into `variants/`, edit there, and assign your variant to the appropriate pyramid or all pyramids.

The Tools mode's Create wizard creates variants for you automatically. If you're comfortable editing YAML directly, the filesystem is also fine.

---

## Hot reload

All contribution changes are hot-reloaded. There is no "restart Agent Wire Node after editing YAML". The next build that uses the contribution picks up the new version automatically.

This is true for:

- Chain variants (next build uses the updated chain).
- Prompt edits (next step invocation loads the fresh markdown).
- Config contributions (next policy check uses the new values).

The exception is the Rust binary itself — app-level updates need a restart, handled by the app's built-in updater (see [`93-updates-and-dadbear-app.md`](93-updates-and-dadbear-app.md)).

---

## The governing rule: everything extensible is a contribution

When you're designing a change and wondering where it belongs, ask:

- Can this be a chain YAML change? → Layer 3.
- Can this be a prompt change? → Layer 2.
- Can this be a policy value? → Layer 1.
- Can this be a skill, a question set, or a schema annotation? → Layer 5-7.

If the answer to all of those is no, you may be looking at a genuine code change. That's rare; before you go there, re-ask, because someone has almost always thought about this case before and the answer was usually "it's expressible as a contribution."

The rule of thumb: **if you can express your change as data (YAML, markdown, config), do that. Only reach for code as a last resort.**

---

## How contributions get shared

Every contribution you author locally can be published to the Wire as a versioned artifact. Others can pull it. You get reputation and (if priced) credits when they do.

Conversely, most of what you'll consume from other operators is contributions: chain variants they've authored, skills they've published, configs they've tuned. Pulling a well-crafted contribution can save hours of authoring from scratch.

The combination — local customization that can ripple across operators — is the reason Agent Wire Node is a network rather than a standalone app. See [`04-the-wire-and-decentralization.md`](04-the-wire-and-decentralization.md).

---

## A first customization project

If you want a concrete first project, try this:

1. **Pick one of your pyramids** — preferably a small one you know well.
2. **Author a chain variant** for its content type. Start from the default (`code.yaml` or similar), copy to `variants/my-code.yaml`, make one change (e.g. add an extra synthesis step, or tune granularity).
3. **Assign your variant to the pyramid** in its detail drawer.
4. **Trigger a rebuild.**
5. **Inspect the result** in the Pyramid Surface. Drill nodes that were affected.
6. **Iterate.** Tweak the variant, rebuild again.
7. **Once it's good, publish** via Tools mode → the contribution card → Publish.

This teaches you the whole customization loop in ~30 minutes. After that, you can move to prompts, skills, configs, or anything else with the same pattern.

---

## Where to go next

- [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md) — Layer 3 deep dive.
- [`42-editing-prompts.md`](42-editing-prompts.md) — Layer 2 deep dive.
- [`43-assembling-action-chains.md`](43-assembling-action-chains.md) — Layer 4 deep dive.
- [`44-authoring-skills.md`](44-authoring-skills.md) — Layer 5 deep dive.
- [`46-config-contributions.md`](46-config-contributions.md) — Layer 1 deep dive.
- [`47-schema-types.md`](47-schema-types.md) — Layer 7 (advanced).
- [`28-tools-mode.md`](28-tools-mode.md) — the UI for authoring and publishing contributions.
