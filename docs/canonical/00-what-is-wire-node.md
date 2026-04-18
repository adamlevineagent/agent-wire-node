# What is Agent Wire Node

Agent Wire Node is a desktop application that turns your local files into **structured, queryable understanding**.

Point it at a folder — code, documents, chat transcripts, design notes — and it builds a **knowledge pyramid**: a layered, evidence-backed graph you can query by drilling, searching, asking questions in plain language, or having an agent walk it via an API. The pyramid keeps itself current as the source files change.

The app also connects to **the Wire** — a network where pyramids, skills, templates, and compute capacity are shared and traded between nodes. On the Wire, your node can publish pyramids for others to query, pull in chains and prompts other operators have authored, earn credits by serving inference requests, or pay to have questions answered by someone else's pyramid.

Agent Wire Node is local-first. Nothing leaves your machine unless you publish it. Even when you're connected to the Wire, builds happen against local storage using local compute (or LLM calls paid from your account). The Wire is a marketplace, not a cloud backend.

---

## What Agent Wire Node is good at

- **Making unfamiliar bodies of knowledge navigable.** Drop a codebase, a stack of PDFs, a year of meeting notes, a conversation log. Get back an apex-down structure you can read or search.
- **Answering questions against that knowledge with provenance.** Every answer is backed by evidence nodes pointing at specific passages or files. You can always drill to the source.
- **Keeping understanding fresh.** When source files change, the pyramid re-evaluates only the pieces that are actually affected — not the whole rebuild.
- **Serving structured knowledge to agents.** Any Claude or other MCP-capable agent can talk to a pyramid through `pyramid-cli` or the MCP server. Agents navigate, search, and annotate the pyramid as they work.
- **Composing cross-source questions.** A "question pyramid" can draw evidence from multiple source pyramids and synthesize a unified answer.
- **Running compute for others and earning credits.** If your hardware can run a local model, you can opt in to the compute market and get paid for inferences.

## What Agent Wire Node is *not*

- Not a cloud service. You run it. Your data stays on your disk.
- Not a chat app. It does not replace Claude/ChatGPT — it is a tool those agents use.
- Not an agentic IDE. It will build understanding of your codebase, but it does not write code for you.
- Not currently cross-platform for the alpha. macOS only (Apple Silicon or Intel). Linux and Windows are not supported in the alpha.

---

## Two ways to use it

**As an operator** — you drive the desktop app. You create pyramids, ask questions, configure model routing, opt into the market. Most of this doc set is written for this role.

**As an agent** — you are Claude (or another LLM-backed agent) talking to someone's running Agent Wire Node over HTTP. You call the CLI or MCP tools to explore pyramids and leave annotations for the next agent. See [`51-pyramid-cli.md`](51-pyramid-cli.md) and [`52-mcp-server.md`](52-mcp-server.md).

In practice the same person wears both hats: you build pyramids as an operator, then your AI partner uses them as an agent.

---

## The shape of a session

A typical session looks like this:

1. **Open Agent Wire Node.** Sign in. The sidebar shows Understanding, Knowledge, Tools, Fleet, Operations, Market, Search, Compose.
2. **Go to Understanding.** Click **Add Workspace**, pick a folder, pick a content type (code / document / conversation / vine), optionally ask an apex question.
3. **Watch the build.** The Pyramid Surface renders nodes live as chunks get extracted and answers get synthesized. You can interrupt, reroll, or just let it finish.
4. **Query.** Drill the apex, search for a term, or ask a follow-up question that builds a derivative pyramid referencing the first.
5. **Hand off to an agent.** Your Claude session connects via MCP and explores the same pyramid. It leaves annotations that show up back in the UI.
6. **Let it run.** DADBEAR auto-update keeps the pyramid in sync with source changes while you do other work. Your balance ticks up if you opted into the compute market.

---

## Where to go next

- If you want the conceptual vocabulary (pyramid, layers, chains, contributions, DADBEAR): [`01-concepts.md`](01-concepts.md).
- If you want to install and start using it: [`10-install.md`](10-install.md) → [`11-first-run-and-onboarding.md`](11-first-run-and-onboarding.md).
- If you want to see everything the UI can do, start with [`20-pyramids.md`](20-pyramids.md) and walk through the mode docs in order.
- If you are integrating an agent or a script: [`51-pyramid-cli.md`](51-pyramid-cli.md) and [`52-mcp-server.md`](52-mcp-server.md).
