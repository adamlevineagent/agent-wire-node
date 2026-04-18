# How it all fits together

This doc traces the shape of the whole system: the data flow from raw files to queryable understanding, how the pieces plug together, and which concepts you reach for at each point. Read [`01-concepts.md`](01-concepts.md) first for vocabulary.

Everything here is about what you see and do as a user. The internal mechanics are not the point; the point is how to think about the system when you are sitting in front of it.

---

## The one-paragraph mental model

Wire Node builds **knowledge pyramids** over local corpora. There is one build path, one staleness system, and one extensibility mechanism. **Questions drive everything**: a "mechanical build" is a preset question with a frozen decomposition — there is no separate mechanical pipeline. **One executor**: every build is a YAML chain interpreted by the same runtime — provided `use_chain_engine` is on (it defaults to off on fresh installs today; enabling it is a one-line config change and the executor is the production path). **One staleness system** (DADBEAR): every change, no matter the source, is a mutation that feeds the same recursive loop. **Everything extensible is a contribution**: chains, prompts, configs, schema annotations, FAQ entries, annotations. New behavior ships as content, not as a new app version — with the caveat that a few specialized build phases (evidence loop, decomposition, gap handling) are still Rust-native and invoked as recipe primitives; moving them into expressible YAML is on the near-term roadmap.

---

## The flow of a build

```
  [source files]
        │  the file watcher hashes them; any change becomes pending work
        ▼
  [chunks]           ← files get split into chunks of a few thousand tokens
        │
        │  characterization runs once per build:
        │    picks content type, tone, audience
        ▼
  [decomposition]    ← apex question → sub-questions → leaf questions;
        │              diff against what's already in the pyramid,
        │              cross-link where possible, only plan new work for gaps
        ▼
  [extraction schema] ← one holistic pass: what should L0 nodes look like
        │               given the full set of leaf questions?
        ▼
  [L0 extraction]    ← per chunk, per question-shaped extraction; L0 nodes
        │              are written into the pyramid
        ▼
  [evidence answering] ← for each leaf question: pre-map candidate L0 nodes,
        │                answer with KEEP/DISCONNECT/MISSING verdicts,
        │                weights, reasons
        ▼
  [synthesis]        ← leaf answers fold into branch answers fold into the
        │              apex answer
        ▼
  [reconciliation]   ← look for orphan L0 (unreferenced — a decomposition
        │              gap), central L0 (load-bearing evidence), gap clusters
        │              (systemic MISSING that wants a targeted re-examination)
        ▼
  [pyramid ready]
```

Every step emits events the UI subscribes to, so you can watch the pyramid fill out live in the Pyramid Surface. Every LLM call is tracked for cost and cached; identical inputs don't re-call the model.

The first question on a fresh corpus runs the whole flow. The tenth question on a rich corpus is nearly free — mostly cross-linking, with a tiny delta of new work for whatever the new question genuinely needs.

---

## What happens after the build

### DADBEAR maintains the pyramid

After the initial build, DADBEAR takes over. Its loop:

1. **Detect** — the file watcher notices source changes by hash.
2. **Accumulate** — each change becomes pending work for the layer it affects.
3. **Debounce** — per-layer timers wait for a settling window before draining.
4. **Batch** — the rotator arm distributes mutations into balanced batches.
5. **Evaluate** — stale-check helpers call the LLM: *"given old content X and new content Y, is this node still right?"*
6. **Act** — confirmed stale means supersede the node; leave the old version in place with a pointer.
7. **Recurse** — the supersession at layer N is itself a change at layer N+1, so the loop walks upward until nothing more is stale.

The same loop handles file changes, deletions (tombstones), renames, new file discovery, contradiction-driven supersession, annotation triggers, and policy changes. One loop, one system.

You see DADBEAR activity in **Understanding → Oversight**: pending work, recent evaluations, cost, whether the breaker is tripped. You can pause per-pyramid or globally. See [`43-auto-update-and-staleness.md`](43-auto-update-and-staleness.md) for how to tune it.

### Queries run over the current pyramid

You, or an agent, query the pyramid through:

- **The Pyramid Surface** — the visualization inside the app. Drill, hover, inspect.
- **`pyramid-cli`** — typed commands from a terminal. Great for scripts.
- **The MCP server** — any Claude or MCP-capable agent connects over stdio.
- **HTTP API** — any tool hits `localhost:8765/pyramid/…` directly.

All four paths see the same pyramid. There is no "agent API" and "UI API" — everything is one surface.

---

## How questions accumulate knowledge

This is the key idea that separates a Wire Node pyramid from a one-shot LLM summary.

- **First question on a fresh corpus.** Full pipeline: extract everything, decompose the question, answer, synthesize. The L0 evidence is shaped by what the first question needed.
- **Second question.** The decomposer sees the existing structure. Sub-questions already answered by existing nodes? Cross-link. Sub-questions partially answered? Only fill the MISSING verdicts. Sub-questions entirely new? Targeted extractions from specific source files, focused on what this question needs.
- **Tenth question.** Mostly cross-linking. The evidence base is dense and increasingly reusable. Work is a small delta.

The evidence base grows with every question. It never redundantly re-extracts. Different questions produce differently-shaped L0 over the same files, and both sets accumulate — a question about architecture enriches what a later question about security can draw on.

This compounds. The more work you put into a pyramid, the more any future question gets for free.

---

## How the UI maps to what the system is doing

Every sidebar mode is a window onto one slice of the same underlying system. You will find yourself moving between them constantly.

| Sidebar mode | What you do here |
|---|---|
| **Understanding** | Manage pyramids: create, build, query, publish, configure DADBEAR. |
| **Knowledge** | Manage the document and corpus side: link folders, sync, version docs. |
| **Tools** | Author or manage contributions (chains, skills, templates, question sets). |
| **Fleet** | Manage the agents that work on your node. |
| **Operations** | Watch notifications, messages, and the live job queue. |
| **Market** | Watch compute market activity; manage your offers and policy. |
| **Search** | Discover contributions on the Wire. |
| **Compose** | Draft new contributions to publish to the Wire. |
| **Settings** | Credentials, providers, tier routing, local mode, auto-update. |

Most workflows cross several modes. Building a first pyramid touches Understanding + Knowledge + Settings. Publishing it involves Compose. Earning from it draws on Market. And so on.

---

## How local and Wire relate

Wire Node is **local-first.** That has a specific meaning.

- Builds run against local files, using local (or remote-API-called-by-your-node) compute.
- Your data is the source of truth. Everything persists locally.
- The Wire is reached only through two explicit verbs: **publish** and **pull**.
- During a build, no Wire API calls happen. You can unplug mid-build and the build keeps running.

The Wire adds five capabilities on top:

1. **Publishing** — share a pyramid, chain, skill, template, or question set.
2. **Pulling** — import someone else's contribution.
3. **Discovery** — search the Wire for contributions.
4. **Compute market** — dispatch inference off-node, or serve inference for others.
5. **Fleet coordination** — agents on your node can work across nodes.

If you never connect to the Wire, Wire Node still works — you lose marketplace and sharing; you keep everything else. This is deliberate. You should never be in a position where "Wire is down" means "I can't look at my own knowledge."

See [`60-the-wire-explained.md`](60-the-wire-explained.md) for why the Wire exists and [`63-relays-and-privacy.md`](63-relays-and-privacy.md) for how decentralization and privacy coexist.

---

## Customizing the machinery

When you want to make Wire Node do something different from the defaults, the path almost always goes through **contributions**, not settings.

In rough order, from easiest to most ambitious:

1. **Change a number** (e.g. debounce minutes, absorption cap). Do this in the relevant contribution's YAML via the UI's YAML-to-UI renderer, or directly in the YAML file.
2. **Edit a prompt.** Change the markdown file that the chain references. The next build picks it up.
3. **Author a chain variant.** Copy the default chain, change iteration modes or primitives or step sequence, assign your variant to a specific slug or all slugs.
4. **Author a skill.** Write a better prompt for a specific step and bundle it as a contribution that supersedes the default.
5. **Author a schema or question set.** Define a new shape of configurable data, or a new way to decompose a question.
6. **Publish to the Wire.** Share your variant with other operators; pull theirs.

See [`40-customizing-overview.md`](40-customizing-overview.md) and the files that follow it for detailed walkthroughs.

---

## Three things most newcomers get wrong

1. **Expecting a separate "mechanical" and "question" pipeline.** There isn't one. Everything is questions. A code pyramid is the preset question *"What is this codebase and how is it organized?"* with a preset decomposition.

2. **Expecting MISSING verdicts to create evidence.** They don't. MISSING is a demand signal. Something else (DADBEAR, a targeted re-examination, you) decides whether to fill the gap.

3. **Expecting Wire Node to act like a cloud service.** It isn't. Your node is the authority. Wire is a sharing layer on top. If you disconnect, everything local keeps working.

---

## Where to go next

- [`03-why-wire-node-exists.md`](03-why-wire-node-exists.md) — why this thing was built at all.
- [`04-the-wire-and-decentralization.md`](04-the-wire-and-decentralization.md) — why networking at all, and how privacy and decentralization coexist.
- [`10-install.md`](10-install.md) — get it running.
- [`20-pyramids.md`](20-pyramids.md) — walk the main mode.
- [`40-customizing-overview.md`](40-customizing-overview.md) — how to make the machinery yours.
