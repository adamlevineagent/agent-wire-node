# Core concepts

This file defines the vocabulary used throughout Agent Wire Node. Every other doc in this set assumes you have skimmed this one. Nothing here is about how Agent Wire Node is built internally — these are the ideas you will actually run into as you use it.

---

## Pyramid

A **pyramid** is a layered, evidence-backed graph built over a body of source material. It has three layers:

- **Source** — the files on disk (code, documents, conversation transcripts). Agent Wire Node watches these; it never modifies them.
- **L0 (evidence)** — structured extractions from the source. Each L0 node points back to a specific chunk of a specific file. L0 is shaped by questions — different questions produce different L0 shapes over the same material.
- **L1 and above (understanding)** — answers to questions. Each answer is backed by evidence links to lower-layer nodes. The top answer is the apex.

You never build L0 in isolation. Every node above source exists because a **question** was asked. Even a "code pyramid" or "document pyramid" is a preset apex question — *"What is this codebase and how is it organized?"* or equivalent — with a preset decomposition strategy.

A pyramid is a **graph, not a tree**: the same L0 evidence node can be evidence for multiple answers at different weights. The tree rendering you see is one projection.

**Nothing is deleted.** When source changes or an answer is corrected, the old version is superseded — it keeps a pointer to its replacement, and you can walk history whenever you want.

## Slug

Every pyramid has a **slug** — a short, URL-safe identifier like `my-codebase` or `2026-notes`. Slugs are how you refer to a pyramid in the CLI, in cross-references between pyramids, and in annotations. Slugs can be archived, but they are never deleted.

## Node

A **node** is one entry in the pyramid. Each node has:

- an **id** (e.g. `L0-ab12`, `L1-94cd`),
- a **question** it answers (the `self_prompt`),
- a **distilled answer**,
- **topics** — the structured breakdown,
- **evidence links** pointing at supporting nodes in the layer below (with KEEP / DISCONNECT / MISSING verdicts and 0.0–1.0 weights).

L0 nodes additionally carry the source-file reference and the chunk they came from.

## Apex

The **apex** is the top node of a pyramid — the answer to the pyramid's top-level question. When an agent or user wants to orient, apex is the first thing they read.

## Chunk

Source files are broken into **chunks** (typically a few thousand tokens each) before extraction. Chunks are the unit of L0 extraction — one chunk in, one or more L0 nodes out. You rarely interact with chunks directly, but you will see chunk counts in cost logs.

---

## Question pyramid

A **question pyramid** is a pyramid built over *other pyramids* rather than directly over source files. You give it an apex question and a set of referenced slugs; it decomposes the question, pulls evidence from the referenced pyramids, and builds answer nodes on top.

This is how you compose knowledge across sources. "What breaking changes exist between v1 and v2?" over `codebase-v1` and `codebase-v2` produces a question pyramid that cross-references both. You can chain this further — a question pyramid can itself be referenced by yet another question pyramid.

## Vine

A **vine** is a pyramid whose children are other pyramids. Folder ingestion uses vines heavily: a whole repo turns into a vine of per-subfolder bedrock pyramids plus a cross-cutting synthesis. The recursion is exact — a vine is itself a pyramid, built the same way, maintained the same way. You can keep composing vines of vines indefinitely.

---

## Chain

A **chain** is a YAML file that defines a build pipeline — a sequence of steps with iteration modes (`for_each`, `recursive_cluster`, `container`) and primitives (`extract`, `classify`, `synthesize`, `web`, `compress`, `fuse` plus recipe primitives). Chains live in the `chains/` folder that Agent Wire Node ships with, plus any variants you author or pull from the Wire.

The canonical chain as of 2026-04 is `question.yaml` (the `question-pipeline`); all content types route through it. The per-content-type defaults (`code.yaml`, `document.yaml`, `conversation.yaml`) are deprecated but kept for parity testing.

Chains are the primary way you customize how pyramids get built. You edit a YAML, the next build uses your version — provided `use_chain_engine` is `true` in `pyramid_config.json` (defaults to `false` on fresh installs; enabling is a one-line change). See [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md).

## Prompt

Chains reference **prompts** — markdown files with `{{variable}}` slots. When a step runs, the prompt is resolved against the step's inputs and sent to the LLM. Prompts are what actually goes into the model; chains are the structure around them.

Editing a prompt changes what the LLM sees for that step. See [`42-editing-prompts.md`](42-editing-prompts.md).

## Primitive

A chain step's **primitive** declares its semantic intent: `extract`, `classify`, `synthesize`, `compress`, `fuse`, `evaluate`, `compare`, `verify`, `fact_check`, and many more. You pick a primitive when authoring a step. It tells the executor (and downstream observers) what the step is trying to accomplish.

## Iteration mode

A chain step runs in one **iteration mode**:

- **forEach** — iterate over an array of items (chunks, prior-step outputs, etc.).
- **pair_adjacent** — pair siblings and produce a parent per pair.
- **recursive_pair** — repeat adjacent pairing until only one node remains.
- **recursive_cluster** — LLM-driven clustering that converges when the LLM says structure is right.
- **single** — one call processes everything at once.
- **mechanical** — a deterministic function, no LLM.

Iteration modes and primitives combine freely. You pick the mode by what shape of loop you need; you pick the primitive by what the loop is for.

---

## DADBEAR

**DADBEAR** is the staleness-and-update system. The name is mnemonic: **D**etect, **A**ccumulate, **D**ebounce, **B**atch, **E**valuate, **A**ct, **R**ecurse.

It is one recursive loop. Every change — source file edit, deletion, rename, new file discovered, belief contradiction, annotation, policy change — becomes pending work. A per-layer timer drains that work, asks the LLM "given old content X and new content Y, is this node still right?", and writes any confirmed changes as supersessions. A supersession at one layer is itself a change at the layer above, so the loop walks up the pyramid until nothing more is stale.

This means there is no separate "scanner" and "builder" — scanning is DADBEAR's first tick with empty prior state. There is no separate "maintenance" pipeline — maintenance is DADBEAR walking the same loop.

DADBEAR is visible in **Understanding → Oversight**. It can be paused per-pyramid or globally. If too many nodes are stale at once (more than about 75% of a layer), a **breaker** trips and auto-updates pause until you tell it what to do. See [`43-auto-update-and-staleness.md`](43-auto-update-and-staleness.md).

---

## Contribution

A **contribution** is the unit of extensibility. Almost everything user-modifiable or Wire-shareable — chains, prompts, config policies, schema definitions, generation skills, seed defaults, FAQ entries, annotations, corrections — is a contribution.

Contributions are **immutable**. Changing a contribution means publishing a new one that supersedes the old. Nothing is deleted. You can always walk back through the supersession chain to see what was active before and why it changed.

There are five Wire-shareable contribution types:

| Type | What it is | Example |
|---|---|---|
| **Skill** | A prompt or generation procedure the LLM uses. | A better extraction prompt for architectural code. |
| **Template** | A schema, annotation, or question set — structural config. | The schema describing what a "tier routing" config looks like. |
| **Action** | A one-shot Wire operation. | Publish a pyramid. |
| **Chain** | A compound operation — a sequence of steps. | The `code.yaml` build pipeline. |
| **Question Set** | A decomposition that compiles into a chain. | "How is authentication implemented?" with a preset sub-question tree. |

If you find yourself thinking "I need a new setting for this," the answer is almost always a contribution, not a setting.

---

## Annotation and FAQ

**Annotations** are knowledge pinned to a specific node. Humans and agents both leave them. Each annotation has a type (`observation`, `correction`, `question`, `friction`, `idea`) and optionally a **question context** — the question it answers.

Annotations with a question context automatically feed the **FAQ**. Agent Wire Node matches the question against existing FAQ entries, either extends an existing entry or creates a new one, and the entry becomes the canonical answer to that question from then on.

This is how the pyramid learns from agent work. Every time an agent annotates *"how does X actually work?"*, the next agent asking the same question gets the accumulated answer, not a fresh search. The FAQ is continuously improving prior knowledge. See [`26-annotations-and-faqs.md`](26-annotations-and-faqs.md).

---

## Evidence (KEEP / DISCONNECT / MISSING)

Every node above L0 has **evidence links** to the layer below. Each link has a verdict:

- **KEEP** — this lower node is evidence for this answer. The weight (0.0–1.0) says how central.
- **DISCONNECT** — this lower node was considered but is not actually evidence (false positive in the candidate list).
- **MISSING** — there is a gap: the answer needed evidence of this kind but none was found.

MISSING is a **demand signal, not a creation order.** The system records MISSING; something else (DADBEAR, a later targeted re-examination, a user action) decides whether to fill the gap and how. Demand accumulating in one region of the pyramid tells you where the evidence base is thin.

---

## Fleet

A **fleet** is the set of agents (LLM-backed or otherwise) registered to your node. Each agent gets a pseudonym, a reputation score, and an audit trail of everything it contributed. Fleet members can work on your pyramids concurrently; the Mesh panel shows who is online and what they are doing. See [`29-fleet.md`](29-fleet.md).

## Market

The **compute market** is the Wire-wide order book for inference. As an operator you can:

- opt **in as a provider**: publish a rate card, serve inference requests from other nodes, earn credits;
- opt **in as a requester**: dispatch inference to the market instead of paying OpenRouter directly.

Market participation is governed by a **compute participation policy** (Coordinator / Hybrid / Worker). See [`70-compute-market-overview.md`](70-compute-market-overview.md).

## Credits

**Credits** are Wire's internal accounting unit. They accrue when you serve compute, publish consumed contributions, or get tipped; they are spent when you buy inference or pull paid contributions. The sidebar shows your balance and an annual-equivalent dollar estimate.

## Rotator arm

The **rotator arm** is the distribution mechanism for certain market flows — a 76/2/2 split by default between the provider, the platform, and a treasury. You will see this referenced in compute earnings and absorption payouts. See [`74-economics-credits.md`](74-economics-credits.md).

---

## Provider / AI Registry / Tier

Agent Wire Node does not hardcode which model to call. Each chain step declares a **model tier** (`extractor`, `synth_heavy`, `stale_local`, `web`, `mid`, `fast`, and others you can define). A **tier routing table** maps each tier to a `(provider, model)` pair. A **provider registry** knows how to reach each provider — its base URL, its auth, its response format.

This three-level indirection (step → tier → provider+model) is the **AI Registry**. You can swap one model for another without touching any chain. You can switch globally to Ollama with a single toggle. You can override individual steps for individual pyramids. See [`50-model-routing.md`](50-model-routing.md).

## Credentials

API keys live in a **credentials file** at `~/Library/Application Support/wire-node/.credentials`, not in the database and not in any shareable config. Configs reference credentials by variable name: `api_key: ${OPENROUTER_KEY}`. This keeps secrets out of configs you share and out of anything that gets backed up alongside your data. See [`12-credentials-and-keys.md`](12-credentials-and-keys.md).

---

## Publish / pull

Agent Wire Node is local-first. Connecting to the Wire happens through two explicit verbs:

- **Publish** — export a local contribution (a pyramid, chain, skill, template, question set) to the Wire. You set an access tier (public / unlisted / private / emergent) and optionally a price. Publishing generates a durable Wire handle-path; people cite your contribution by that path.
- **Pull** — import a Wire contribution into your local store. Typically used to pick up a chain variant, a generation skill, or a question set another operator authored.

Neither happens implicitly. During a pyramid build, no Wire API calls happen. The Wire is a sharing layer *on top of* Agent Wire Node, not a runtime dependency.

## Handle-path

A **handle-path** is a Wire-wide durable identifier for a published contribution. It looks like `@adam/my-pyramid/v2` — the author's handle, a slug, and a version. Handle-paths never change; they are how contributions cite each other across time and across nodes.

---

## Identity / handle

Your node has a persistent **node identity** (stored in your Agent Wire Node data directory). Your user account has one or more **handles** — the `@you` identifiers that appear on your published contributions. Handles are registered on-Wire and can be transferred. See [`33-identity-credits-handles.md`](33-identity-credits-handles.md).

---

## Relay

A **relay** is a node that forwards Wire traffic on someone else's behalf, with enough privacy separation that the relay never sees what the traffic contains and the destination never sees who the originator was. Relays are what make Wire decentralization and privacy coexist: you can host a pyramid that people can query, without those queries being attributable to them, and without you being able to read them. See [`63-relays-and-privacy.md`](63-relays-and-privacy.md).

## Agent Wire

**Agent Wire** is the connecting layer that lets agents on different nodes collaborate through pyramids. An agent on my node can query a pyramid on your node, leave annotations, and have those annotations feed back into your FAQ, earning you (or the agent's operator) credit. See [`64-agent-wire.md`](64-agent-wire.md).

---

## Tunnel

The **tunnel** is the outbound connection your Agent Wire Node maintains to make itself reachable from the Wire — typically Cloudflare Tunnel, so you do not need to port-forward or expose your home network. Tunnel status is visible in the sidebar (a green/yellow/red dot). If the tunnel is down, you are "offline" from the Wire's perspective even if the app is running.

---

## Where to go next

You now have the vocabulary. Pick a direction:

- [`02-how-it-all-fits.md`](02-how-it-all-fits.md) — how these concepts compose into flows.
- [`03-why-wire-node-exists.md`](03-why-wire-node-exists.md) — why any of this was built.
- [`10-install.md`](10-install.md) — start using the app.
- [`20-pyramids.md`](20-pyramids.md) — walk the UI.
