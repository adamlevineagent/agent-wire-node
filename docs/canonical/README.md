# Agent Wire Node — canonical documentation

This directory is the authoritative user-facing documentation for Agent Wire Node. Everything you need to understand the app, use it, customize it, connect it to the Wire, and troubleshoot it is here.

These docs are the corpus the alpha tester's pyramid is built on — so when you (or an agent on your behalf) ask a question about how something works, the answer comes from this set. The docs describe the system **as shipped today**, with known issues and planned features explicitly marked.

---

## Where to start

New to Agent Wire Node:

1. [`00-what-is-wire-node.md`](00-what-is-wire-node.md) — elevator pitch.
2. [`01-concepts.md`](01-concepts.md) — vocabulary (pyramid, chain, DADBEAR, contribution, the Wire).
3. [`02-how-it-all-fits.md`](02-how-it-all-fits.md) — how the pieces compose.
4. [`10-install.md`](10-install.md) → [`11-first-run-and-onboarding.md`](11-first-run-and-onboarding.md) → [`12-credentials-and-keys.md`](12-credentials-and-keys.md).
5. [`21-building-your-first-pyramid.md`](21-building-your-first-pyramid.md).

Already running and want to go deeper:

- Customize the machinery → start with [`40-customizing-overview.md`](40-customizing-overview.md).
- Connect an agent → [`81-mcp-server.md`](81-mcp-server.md).
- Understand the Wire → [`04-the-wire-and-decentralization.md`](04-the-wire-and-decentralization.md).
- Participate in the compute market → [`70-compute-market-overview.md`](70-compute-market-overview.md).

Something broke:

- Grab-bag → [`A0-common-issues.md`](A0-common-issues.md).
- Build stuck → [`A1-build-stuck-or-failed.md`](A1-build-stuck-or-failed.md).
- Provider/network → [`A2-provider-and-network-errors.md`](A2-provider-and-network-errors.md).
- DADBEAR → [`A3-staleness-and-breakers.md`](A3-staleness-and-breakers.md).

---

## The full index

### Overview (00–05)

- [`00-what-is-wire-node.md`](00-what-is-wire-node.md) — what the app is and who it's for.
- [`01-concepts.md`](01-concepts.md) — core vocabulary.
- [`02-how-it-all-fits.md`](02-how-it-all-fits.md) — how concepts compose into flows.
- [`03-why-wire-node-exists.md`](03-why-wire-node-exists.md) — the motivation; the problems it solves.
- [`04-the-wire-and-decentralization.md`](04-the-wire-and-decentralization.md) — the network layer.
- [`05-steward-experimentation-vision.md`](05-steward-experimentation-vision.md) — where autonomous node optimization is headed (planned).

### Getting started (10–12)

- [`10-install.md`](10-install.md) — install (macOS alpha), from `.dmg` or source.
- [`11-first-run-and-onboarding.md`](11-first-run-and-onboarding.md) — first launch, login, onboarding wizard.
- [`12-credentials-and-keys.md`](12-credentials-and-keys.md) — API keys, the credentials file, safety.

### Using the app — one file per sidebar mode (20–34)

- [`20-pyramids.md`](20-pyramids.md) — Understanding mode: dashboard, grid, builds, oversight.
- [`21-building-your-first-pyramid.md`](21-building-your-first-pyramid.md) — walkthrough end-to-end.
- [`22-content-types.md`](22-content-types.md) — code / document / conversation / vine / question.
- [`23-pyramid-surface.md`](23-pyramid-surface.md) — the visualization and node inspector.
- [`24-asking-questions.md`](24-asking-questions.md) — question pyramids and composition.
- [`25-dadbear-oversight.md`](25-dadbear-oversight.md) — staleness system, breakers, cost.
- [`26-annotations-and-faqs.md`](26-annotations-and-faqs.md) — how knowledge accumulates.
- [`27-knowledge-corpora.md`](27-knowledge-corpora.md) — Knowledge mode: corpora, local sync.
- [`28-tools-mode.md`](28-tools-mode.md) — authoring and managing contributions.
- [`29-fleet.md`](29-fleet.md) — agents and peer coordination.
- [`30-operations.md`](30-operations.md) — real-time dashboard.
- [`31-search-and-discovery.md`](31-search-and-discovery.md) — finding things on the Wire.
- [`32-compose.md`](32-compose.md) — drafting long-form contributions.
- [`33-identity-credits-handles.md`](33-identity-credits-handles.md) — Identity mode.
- [`34-settings.md`](34-settings.md) — every settings panel.

### Customizing the machinery (40–47)

- [`40-customizing-overview.md`](40-customizing-overview.md) — the customization layers, cheapest to deepest.
- [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md) — chain structure, primitives, iteration modes.
- [`42-editing-prompts.md`](42-editing-prompts.md) — prompt authoring conventions.
- [`43-assembling-action-chains.md`](43-assembling-action-chains.md) — composition via recipe primitives.
- [`44-authoring-skills.md`](44-authoring-skills.md) — publishable prompt bundles.
- [`45-question-sets.md`](45-question-sets.md) — preset decompositions.
- [`46-config-contributions.md`](46-config-contributions.md) — policy configs via the YAML-to-UI renderer.
- [`47-schema-types.md`](47-schema-types.md) — when to author a new schema type.

### Models and compute backends (50–52)

- [`50-model-routing.md`](50-model-routing.md) — the AI Registry, tier routing.
- [`51-local-mode-ollama.md`](51-local-mode-ollama.md) — Ollama integration (known wiring issue).
- [`52-provider-registry.md`](52-provider-registry.md) — providers, adding new ones.

### The Wire (60–64)

- [`60-the-wire-explained.md`](60-the-wire-explained.md) — handle-paths, coordinator, Wire Native Documents.
- [`61-publishing.md`](61-publishing.md) — publish flows per contribution type.
- [`62-pulling-and-discovery.md`](62-pulling-and-discovery.md) — finding and pulling.
- [`63-relays-and-privacy.md`](63-relays-and-privacy.md) — privacy architecture (planned).
- [`64-agent-wire.md`](64-agent-wire.md) — agents across nodes.

### Compute market (70–74)

- [`70-compute-market-overview.md`](70-compute-market-overview.md) — the market at a high level.
- [`71-compute-market-provider.md`](71-compute-market-provider.md) — provider side (Phase 2 shipped).
- [`72-compute-market-requester.md`](72-compute-market-requester.md) — requester side (Phase 3 in progress).
- [`73-participation-policy.md`](73-participation-policy.md) — Coordinator / Hybrid / Worker modes.
- [`74-economics-credits.md`](74-economics-credits.md) — rotator arm, credits, settlement.

### Agent integration (80–84)

- [`80-pyramid-cli.md`](80-pyramid-cli.md) — the 64-command CLI.
- [`81-mcp-server.md`](81-mcp-server.md) — Claude Desktop and MCP integration.
- [`82-querying-pyramids.md`](82-querying-pyramids.md) — navigation patterns for agents.
- [`83-agent-sessions.md`](83-agent-sessions.md) — session registration and coordination.
- [`84-http-operator-api.md`](84-http-operator-api.md) — raw HTTP surface.

### Operations (90–94)

- [`90-data-layout.md`](90-data-layout.md) — what lives where on disk.
- [`91-logs-and-diagnostics.md`](91-logs-and-diagnostics.md) — logs, health checks, diagnostics.
- [`92-backup-reset-migrate.md`](92-backup-reset-migrate.md) — back up, reset, move to a new machine.
- [`93-updates-and-dadbear-app.md`](93-updates-and-dadbear-app.md) — app auto-update.
- [`94-uninstall.md`](94-uninstall.md) — clean removal.

### Troubleshooting (A0–A3)

- [`A0-common-issues.md`](A0-common-issues.md) — grab bag.
- [`A1-build-stuck-or-failed.md`](A1-build-stuck-or-failed.md) — build-specific.
- [`A2-provider-and-network-errors.md`](A2-provider-and-network-errors.md) — providers, tunnel, coordinator.
- [`A3-staleness-and-breakers.md`](A3-staleness-and-breakers.md) — DADBEAR.

### Reference (Z0–Z2)

- [`Z0-glossary.md`](Z0-glossary.md) — every term.
- [`Z1-quick-reference.md`](Z1-quick-reference.md) — commands, paths, shortcuts.
- [`Z2-mode-at-a-glance.md`](Z2-mode-at-a-glance.md) — each sidebar mode summarized.

---

## Convention: shipped vs planned

Throughout these docs, planned features are explicitly marked. If something is described without a "planned" or "in progress" or "known issue" flag, it is **shipped and working today**.

Feature status at a glance:

- **Shipped:** local pyramid builds, Pyramid Surface, annotations + FAQ, `pyramid-cli` (64 commands), MCP server, publish/pull (public/unlisted), compute market provider side (Phase 2), Cloudflare tunnel.
- **Partially shipped:** compute market requester side (Phase 3), the chain executor as default (flag needs flipping on fresh installs), a few build phases still in Rust (evidence loop, decomposition — moving to YAML near-term), private and emergent access tiers, cross-provider fallback chains.
- **Known issues:** Local Mode (Ollama-only) has a wiring gap; mixed cloud+Ollama works, pure Ollama needs P0-1 fix.
- **Planned:** relays + unlinkable queries, pyramid stewards, steward-daemon three-tier node optimization, `invoke_chain` composition, handle release/transfer workflows.

The full status list is in the main repo [`README.md`](../../README.md) and in [`docs/PUNCHLIST.md`](../PUNCHLIST.md) (authoritative issues list).

---

## Where else to look

- [`docs/SYSTEM.md`](../SYSTEM.md) — authoritative internal architecture map for contributors (developer-facing, not user-facing).
- [`chains/CHAIN-DEVELOPER-GUIDE.md`](../../chains/CHAIN-DEVELOPER-GUIDE.md) — canonical chain YAML reference.
- [`docs/vision/`](../vision/) — forward-looking design docs (stewards, futures, cut-line privacy).
- [`mcp-server/README.md`](../../mcp-server/README.md) — CLI + MCP authoritative reference.

---

## Feedback

If a doc here is wrong, outdated, or unclear, file it in the alpha feedback channel. These docs are maintained alongside the code; corrections get pulled in on review.
