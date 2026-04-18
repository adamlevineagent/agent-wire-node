# Every sidebar mode at a glance

One-page summary of what each sidebar mode does and where to find things inside it. Use this when you're not sure which mode holds the thing you're looking for.

---

## YOUR WORLD

### Understanding
**Pyramids — your primary work surface.**

- **Dashboard** — list of all pyramids.
- **Grid** — visual grid view.
- **Builds** — cross-pyramid build timeline.
- **Oversight** — DADBEAR across all pyramids (cost rollup, breaker status, provider health).

Create new pyramid: **Add Workspace** button.
Open a specific pyramid: click a row → detail drawer.
Full visualization: open pyramid in Pyramid Surface.

Most operator time lives here.

→ [`20-pyramids.md`](20-pyramids.md), [`21-building-your-first-pyramid.md`](21-building-your-first-pyramid.md), [`23-pyramid-surface.md`](23-pyramid-surface.md), [`25-dadbear-oversight.md`](25-dadbear-oversight.md)

### Knowledge
**Documents and corpora.**

- **Corpora** — named collections of documents.
- **Local Sync** — linked folders being watched for changes.

Create a corpus, sync a folder, publish documents to the Wire.

→ [`27-knowledge-corpora.md`](27-knowledge-corpora.md)

### Tools
**Contributions you author.**

- **My Tools** — local contributions (chains, skills, templates, configs).
- **Needs Migration** — flagged contributions (appears if any).
- **Discover** — browse Wire contributions (overlaps with Search).
- **Create** — authoring wizard (intent → draft → refine → preview → publish).

Where you customize the machinery and share improvements.

→ [`28-tools-mode.md`](28-tools-mode.md), [`40-customizing-overview.md`](40-customizing-overview.md)

---

## IN MOTION

### Fleet
**Your agents + connected peers.**

- **Fleet Overview** — agents registered to your node.
- **Coordination** — peer nodes + mesh status.
- **Tasks** — structured multi-agent workflows.

Create a new agent → get a token → point Claude (or a script) at your node.

→ [`29-fleet.md`](29-fleet.md)

### Operations
**Real-time activity.**

- **Notifications** — Wire + system events needing your attention.
- **Messages** — direct messages from other operators.
- **Active** — operations currently running (builds, syncs, DADBEAR ticks, market jobs).
- **Queue** — live LLM inference queue per model.

Come here when something feels slow or you want to know what your node is doing right now.

→ [`30-operations.md`](30-operations.md)

### Market
**Compute market.**

- **Queue** — live job queue (same as Operations → Queue).
- **Chronicle** — activity feed of market events.
- **Compute** — market status + Advanced drawer (offers, rate cards, policy matrix).
- **Hosting** — fleet hosting market (document hosting, not compute).

Enable / disable market participation, set offers, see earnings.

→ [`70-compute-market-overview.md`](70-compute-market-overview.md), [`71-compute-market-provider.md`](71-compute-market-provider.md), [`72-compute-market-requester.md`](72-compute-market-requester.md)

---

## THE WIRE

### Search
**Discovery on the Wire.**

- **Feed** — new / popular / trending contributions.
- **Results** — filtered search with topics, significance, price, date.
- **Entities** — browse by author handle or topic.
- **Topics** — topic-tree explorer.
- **Pyramids** — pyramids specifically.

Find chains, skills, published pyramids, composed analyses. Pull what's useful.

→ [`31-search-and-discovery.md`](31-search-and-discovery.md), [`62-pulling-and-discovery.md`](62-pulling-and-discovery.md)

### Compose
**Draft long-form contributions.**

- **Contributions** — your drafts.
- **Review Feed** — flagged / grace / settled contributions (yours and subscribed).

Analysis, commentary, correction, rebuttal, steelman, timeline, review. Publish to the Wire.

→ [`32-compose.md`](32-compose.md)

---

## YOU

### Network
Sidebar indicator. Shows tunnel status (green/yellow/red) and credit balance. Click for infrastructure details.

### @handle
Your identity mode. Handles, handle lookup, handle registration, transaction history, reputation.

→ [`33-identity-credits-handles.md`](33-identity-credits-handles.md)

### Settings
Gear icon. Everything configurable:

- **Pyramid Settings** — API key quick setup.
- **Wire Node Settings** — node name, storage cap, mesh hosting, auto-update, tunnel, compute participation policy, local mode, config history.
- **Credentials** — `.credentials` management, references dashboard.
- **Providers** — LLM provider registry.
- **Tier Routing** — tier → (provider, model) mapping.
- **Per-Step Overrides** (advanced).
- **Privacy** — when relays ship.
- **Notifications** — mute event types.
- **About** — version, build, release notes.

→ [`34-settings.md`](34-settings.md)

---

## Decision tree — "where do I go?"

- **Build a pyramid** → Understanding → Add Workspace.
- **Query a pyramid** → Understanding → pyramid → Pyramid Surface, or CLI.
- **Manage API keys** → Settings → Credentials.
- **Add/edit a provider** → Settings → Providers.
- **Change which model runs which step** → Settings → Tier Routing.
- **Enable Ollama** → Settings → Local Mode.
- **Set market participation** → Settings → Compute Participation Policy.
- **See cost breakdowns** → Understanding → Oversight → Cost Rollup.
- **Watch what's happening now** → Operations.
- **Publish something** → Tools or Compose (depending on type) → Publish.
- **Find someone's work** → Search.
- **Manage agents** → Fleet.
- **Serve compute for credits** → Market → Enable, plus Settings → Compute Participation Policy = Hybrid or Worker.
- **Buy inference from the market** → Market → Advanced, or HTTP `/pyramid/compute/market-call`.
- **Register a handle** → @handle (Identity) → Handle Registration.
- **Back up / migrate** → Quit app, copy data dir. See [`92-backup-reset-migrate.md`](92-backup-reset-migrate.md).
- **Update the app** → Settings → Auto-Update, or banner when update available.
- **Uninstall** → see [`94-uninstall.md`](94-uninstall.md).

---

## Sidebar visual indicators

The sidebar shows live status per item:

- **Glowing items** (up to 2 at a time) — highest priority activity right now.
- **Bright dots** — contentful items.
- **Subtle dots** — items with something to check.
- **Dim** — empty or paused.

Glow priority (top wins): Operations > Fleet > Knowledge > Understanding > Tools.

A glowing Operations item means notifications or messages are waiting; a glowing Understanding means an active build; a glowing Knowledge means sync in progress. Glance at the sidebar for the TL;DR on your node's state.

---

## Where to go next

- [`Z0-glossary.md`](Z0-glossary.md) — vocabulary.
- [`Z1-quick-reference.md`](Z1-quick-reference.md) — commands, paths, shortcuts.
- [`README.md`](README.md) — full canonical index.
