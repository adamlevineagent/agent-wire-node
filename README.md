# Wire Node

A desktop app that builds **knowledge pyramids** over your local files — code, documents, conversations — and makes that structured understanding queryable by humans and agents. It also connects your machine to **the Wire**, an intelligence marketplace where pyramids, skills, chains, and compute capacity are shared and traded.

Tauri 2 desktop app, local-first. Rust backend, React frontend, SQLite storage. LLMs via OpenRouter or local Ollama.

**Status: alpha.** Core path works; several features are planned-but-not-yet-shipped and are marked as such throughout the docs. See [shipped state](#shipped-state) below.

---

## Where the real documentation is

Every user-facing concept and workflow is documented in [`docs/canonical/`](docs/canonical/). This README is a launchpad, not the documentation.

Start here:

- [What is Wire Node](docs/canonical/00-what-is-wire-node.md) — the elevator pitch
- [Core concepts](docs/canonical/01-concepts.md) — vocabulary
- [How it all fits together](docs/canonical/02-how-it-all-fits.md) — the shape of the system
- [Why Wire Node exists](docs/canonical/03-why-wire-node-exists.md) — what it's trying to solve
- [The Wire and decentralization](docs/canonical/04-the-wire-and-decentralization.md) — the network layer
- [Steward experimentation (vision)](docs/canonical/05-steward-experimentation-vision.md) — the autonomous-optimization horizon

Then follow your intent:

- **Install and use it:** [Install](docs/canonical/10-install.md) → [First run](docs/canonical/11-first-run-and-onboarding.md) → [Credentials](docs/canonical/12-credentials-and-keys.md) → [Understanding (Pyramids)](docs/canonical/20-pyramids.md) → [Building your first pyramid](docs/canonical/21-building-your-first-pyramid.md).
- **Customize the machinery:** [Customization overview](docs/canonical/40-customizing-overview.md) → [Chain YAMLs](docs/canonical/41-editing-chain-yamls.md) → [Prompts](docs/canonical/42-editing-prompts.md) → [Assembling chains](docs/canonical/43-assembling-action-chains.md).
- **Connect an agent (Claude or any MCP client):** [`mcp-server/README.md`](mcp-server/README.md).
- **Understand the internals as a contributor:** [`docs/SYSTEM.md`](docs/SYSTEM.md) — authoritative internal architecture map.

---

## Shipped state

Alpha means some things work and some things don't. Accurate summary of the current build:

**Works today:**

- Local pyramid builds on OpenRouter (code, document, conversation, vine content types).
- Incremental question pyramids on top of source pyramids.
- Pyramid Surface visualization, node inspector, reroll, annotate.
- FAQ auto-generation from annotations with question context.
- `pyramid-cli` and MCP server (64 commands across 16 categories).
- Publishing and pulling contributions on the Wire.
- Compute market as **provider** (Phase 2 shipped) — you can opt in to serve inference and earn credits.
- Cloudflare tunnel for reachability from the Wire.

**Partially shipped:**

- Compute market as **requester** (Phase 3 in progress) — dispatching inference off-node to the market is landing piece by piece.
- `use_chain_engine` flag defaults to `false` on fresh installs — the chain executor is the production path, but you currently need to enable it explicitly in `pyramid_config.json`. Legacy hardcoded pipelines are what fresh installs hit.
- **A few build phases are still Rust, not YAML.** Notably the **evidence loop** (the pre-map → answer → MISSING-verdict cycle) lives as a recipe primitive implemented in Rust that chains invoke by name. Same story for `recursive_decompose`, `process_gaps`, `build_lifecycle`, and `cross_build_input`. Moving these into expressible YAML is on the near-term roadmap — until it lands, they behave like built-ins you can call but not rewrite.

**Known issues:**

- Ollama local mode has a tier-routing wiring gap (P0-1 in `docs/PUNCHLIST.md`) — configuring Ollama doesn't yet route tier resolution through the provider registry correctly. Builds on OpenRouter work fine; Ollama-only setups need the fix to land.

**Planned (not yet shipped):**

- **Steward experimentation** — autonomous node optimization via a three-tier daemon-sentinel-smart-steward architecture. Vision doc: [docs/canonical/05](docs/canonical/05-steward-experimentation-vision.md).
- **Privacy-preserving relays** — forwarding nodes that separate query identity from destination, so queries to published pyramids are unlinkable. Today, pyramid queries are attributed.
- **Pyramid stewards / question contracts** — agents that mediate pyramid access with negotiation rather than binary access control.
- **`invoke_chain` composition** — referencing other chains as steps rather than expressing composition via recipe primitives inside one chain.
- **Full emergent (paid) access tier on published contributions** — basic visibility tiers (public / unlisted / private) work; the emergent paid-on-pull path is under construction.

The per-doc shipped/planned status is noted in each canonical doc. If something you read about in a doc doesn't work, it's either a bug (please report) or a feature still arriving.

---

## Install (user)

Pre-built `.dmg` (macOS only in the alpha, Apple Silicon or Intel):

1. Download the `.dmg` from the alpha channel.
2. Double-click, drag **Wire Node** to `/Applications`.
3. Right-click → **Open** the first launch (Gatekeeper).
4. Sign in with email (magic link). Walk through the onboarding wizard.
5. Add your OpenRouter API key in **Settings → Credentials**.
6. Go to **Understanding → Add Workspace**, pick a folder, build a pyramid.

Full walkthrough: [`docs/canonical/10-install.md`](docs/canonical/10-install.md).

---

## Build from source (developer)

```bash
# Prerequisites: Rust 1.75+, Node 20+, Tauri CLI v2, Xcode Command Line Tools
cd agent-wire-node
npm install

# Dev mode (hot-reload frontend, rebuilds backend on change)
cargo tauri dev

# Production build
cargo tauri build
# Output: src-tauri/target/release/bundle/macos/Wire Node.app
#         src-tauri/target/release/bundle/dmg/Wire Node_<version>_<arch>.dmg
```

The HTTP server runs on `localhost:8765` (agent-facing API). The Tauri window is the operator UI.

See [`docs/canonical/10-install.md`](docs/canonical/10-install.md) for signed-build and distribution notes.

---

## Repository layout

```
agent-wire-node/
├── src/                       # React frontend (Tauri renderer)
├── src-tauri/                 # Rust backend (Tauri host + HTTP server + pyramid engine)
│   └── src/pyramid/           # Build executor, DADBEAR, contributions, provider registry
├── chains/                    # Canonical YAML chains + prompts shipped with the binary
│   ├── CHAIN-DEVELOPER-GUIDE.md   # Authoritative quick reference — read before editing chains
│   ├── defaults/              # question.yaml is canonical; others deprecated but present
│   └── prompts/
├── mcp-server/                # pyramid-cli + MCP server (thin TS clients over HTTP)
├── docs/
│   ├── canonical/             # USER documentation — what ships with the tester pyramid
│   ├── SYSTEM.md              # Authoritative internal architecture map for contributors
│   ├── architecture/          # Design docs (understanding-web, action-chain, ...)
│   ├── specs/                 # Implementation specs
│   ├── vision/                # Forward-looking vision docs (stewards, futures, cut-line)
│   ├── handoffs/              # Build-phase handoff notes
│   ├── PUNCHLIST.md           # Known issues and status
│   └── DIVERGENCE-TRIAGE.md   # Where running code diverges from SYSTEM.md
└── plans/                     # Active build plans
```

---

## Core loop, briefly

Wire Node builds knowledge pyramids over local corpora. There is one build path, one staleness system (DADBEAR), and one extensibility mechanism (contributions).

- **Questions drive everything.** A "mechanical" content-type build is a preset question with a frozen decomposition. One executor; content types are chain variants, not different code paths.
- **DADBEAR keeps pyramids current.** Every change — file edit, deletion, rename, new file, annotation, policy change — becomes a mutation that feeds one recursive loop. Per-layer timers, batched evaluation, supersession upward until nothing more is stale.
- **Everything extensible is a contribution.** Chain YAMLs, prompts, configs, schema annotations, FAQ entries, annotations — all rows in a supersession-linked contribution store. New behavior ships by writing a contribution, not by adding a code path.

The full vocabulary is in [`docs/canonical/01-concepts.md`](docs/canonical/01-concepts.md). The flow is in [`docs/canonical/02-how-it-all-fits.md`](docs/canonical/02-how-it-all-fits.md).

---

## Agent integration

Any MCP-capable agent can talk to a pyramid via:

- **`pyramid-cli`** — 64 commands across exploration, analysis, operations, annotation, composition, question pyramids, vines, agent coordination, reading modes, vocabulary, recovery. Thin HTTP client over `localhost:8765`.
- **MCP server** — stdio transport, same 64 capabilities exposed as tools. Drop into Claude Desktop's config and go.

Setup: [`mcp-server/README.md`](mcp-server/README.md) and [`docs/canonical/81-mcp-server.md`](docs/canonical/81-mcp-server.md) *(canonical doc coming)*.

Auth: bearer token resolved via `PYRAMID_AUTH_TOKEN` env var or `~/Library/Application Support/wire-node/pyramid_config.json`.

---

## Reporting issues

Alpha testers: please file friction in the Wire Node feedback channel. Include:

- Node version (Settings → About, or bottom of the sidebar).
- macOS version + architecture.
- The last few dozen lines from `~/Library/Application Support/wire-node/wire-node.log`.
- What you were trying to do, what you expected, what happened.

Check [`docs/canonical/70-common-issues.md`](docs/canonical/70-common-issues.md) *(coming)* and [`docs/PUNCHLIST.md`](docs/PUNCHLIST.md) first — a number of things are known.

---

## License and attribution

License: TBD in the alpha. Ask Adam.

Wire Node is a working name for what Adam calls "the Wire platform" — an intelligence marketplace with local-first nodes. See [`docs/canonical/04-the-wire-and-decentralization.md`](docs/canonical/04-the-wire-and-decentralization.md) for the network vision; [`docs/canonical/05-steward-experimentation-vision.md`](docs/canonical/05-steward-experimentation-vision.md) for where autonomous node optimization is headed.
