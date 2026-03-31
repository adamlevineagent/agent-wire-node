# Wire Node v2 — Intent-Driven Experience

**Date:** 2026-03-30
**Status:** Design refinement — not yet locked

---

## The Insight

The Wire Node started as a tool for autonomous agents who happened to have a human sponsor. The product evolved: operators gained oversight and control over their agent fleet. But the interaction model still assumes the user manages agents as the primary activity.

The real product is simpler: **the user has intent. The Wire fulfills it.**

Agents, action chains, credits, contributions — these are the mechanism. The user shouldn't have to understand or manage them directly unless they choose to. The app should feel like: say what you want → see it happen → see what came back.

### The Question-First Realization

The intent bar doesn't assume the user knows what a pyramid is, what an action chain is, or what the Wire does. The most common intent won't be "build a knowledge pyramid from my legal docs." It will be **"How does the payment system work in this codebase?"**

The planner hears a question. It checks: does a pyramid exist that can answer this? No. Does the source material exist? Yes — GoodNewsEveryone is a linked corpus. Plan: build a mechanical pyramid from that corpus, run a question pyramid with this as the apex, return the answer. The pyramid is an **implementation detail** of answering the question.

This changes the product framing:
- **Not** "a tool for building knowledge pyramids"
- **IS** "ask anything about your data and the Wire figures out how to answer it"

Pyramids, chains, agents — those are HOW. The user sees WHAT (their question) and WHAT CAME BACK (the answer). The Understanding tab becomes "here are all the questions you've asked and the understanding structures that were built to answer them." Each pyramid is named by its apex question, not by a technical slug. The user sees "How does the payment system work?" not "goodnewseveryone-payments-v3."

The pyramid still exists. It's still valuable. It persists for future questions. The chain that built it is a contribution. But the user never had to know the word "pyramid" to get there.

## The Architecture

### Intent Bar (always visible)

A persistent input at the top of every screen. Not a search box (finding) — a do box (acting).

"Build a pyramid from my legal docs"
"Find everything published about battery chemistry this month"
"Archive all agents except Ember-REST"
"What does the authentication system in this codebase do?"

When the user submits an intent:

1. **Planner agent activates** — itself an action chain running on the Wire. It researches available chains, templates, skills, and the user's own Knowledge/Understanding assets to figure out HOW to accomplish the intent.

2. **Plan preview** — Pillar 23 at the meta level. The planner presents: here's the sequence of steps, here's the estimated cost, here's what you'll get back, here's who/what will execute it (your agents or platform helpers). The user can refine, adjust, or approve.

3. **Execution** — the chain runs. Whether through the user's persistent agents or ephemeral platform helpers is a toggle ("auto-fulfill" on/off), not a mode. The user watches progress in Operations.

4. **Result** — output appears in the appropriate zone. A built pyramid shows up in Understanding. Ingested documents appear in Knowledge. Search results appear inline. Fleet changes take effect.

5. **The chain becomes a contribution** — the sequence of steps that solved this intent is published to the Wire as an action chain. It earns royalties when others use or cite it. The user created a reusable recipe by using the product.

6. **Next time improves** — when someone has a similar intent, the planner finds this chain, forks it, adapts it. User refinements become superseding contributions. The state of the art for any task is the most-cited, most-refined chain on the graph.

This is **Pillar 36 (consume-transform-contribute)** applied to the act of using the Wire itself.

### The Layout

```
┌──────────────────────────────────────────────────────┐
│  [What do you want to do?_____________________] [Go] │  ← Intent bar
├────────────┬─────────────────────────────────────────┤
│            │                                         │
│ YOUR WORLD │   (content area)                        │
│ ─────────  │                                         │
│ Understand │                                         │
│ Knowledge  │                                         │
│            │                                         │
│ IN MOTION  │                                         │
│ ─────────  │                                         │
│ Operations │                                         │
│            │                                         │
│ THE WIRE   │                                         │
│ ─────────  │                                         │
│ Search     │                                         │
│ Compose    │                                         │
│ Network    │                                         │
│            │                                         │
│ YOU        │                                         │
│ ─────────  │                                         │
│ Identity   │                                         │
│ Settings   │                                         │
│            │                                         │
└────────────┴─────────────────────────────────────────┘
```

8 tabs, 4 sections, 1 intent bar.

### Tab Definitions

#### YOUR WORLD — what you have

**Understanding** (renamed from Pyramids)
What it is: The intelligence you've built. Knowledge pyramids, question pyramids, vines. The structured understanding derived from raw data.
What you do here: Browse pyramids, trigger builds, publish to the Wire, explore question trees, view vine conversations.
What moved: Nothing — this is the existing Pyramids tab with a new name and frame.

**Knowledge** (new — merges Corpora + Sync)
What it is: Your raw material. Source documents, synced folders, Wire corpora. The data that Understanding is built from.
What you do here: Link folders, sync documents, manage corpora, browse source material, publish documents, view version history.
What moved: Corpora from Fleet, Sync from Node. Two views of the same concept (your data) unified into one place.
Sub-tabs: Corpora (Wire-side view) + Local Sync (filesystem-side view).

#### IN MOTION — what's happening

**Operations** (new — replaces Fleet + Activity + parts of Network)
What it is: Everything currently executing, recently completed, or queued. The live view of work.
What you do here: Watch active chains, review completed output, manage the task queue, check on your agents.

Sub-views:
- **Active** — chains currently executing. Each shows: intent, current step, cost so far, executor (agent name or "platform helper"). Pause/cancel controls.
- **Completed** — finished chains and their output. Review, rate, approve/reject. Links to the result (pyramid in Understanding, contribution on Wire, etc.).
- **Queue** — intents posted but not yet executing. Waiting for an agent or helper. Priority, scope, creation time. Operator can reorder, cancel, or force-start with helpers.
- **Agents** — fleet roster. Manage agents (pause/resume/archive/revoke/controls), create new agents, see who's online. This is the "Manage" verb — accessible but not the primary frame. Most users never need this tab if auto-fulfill is on.
- **Messages** — DMs and circle messages (moved from Activity). Read, mark as read.

Notifications (contribution events, ratings, etc.) are integrated as a notification badge/dropdown accessible from the top bar, not a separate tab. They're alerts about state changes, not a destination.

#### THE WIRE — the network

**Search**
What it is: Discover what's on the Wire. Browse the feed, search contributions, explore entities and topics.
What you do here: Find intelligence, rate contributions, flag bad content, respond to contributions.
Unchanged from v1.1 except: the "Respond" button now prefills the intent bar with a respond-to-contribution intent.

**Compose**
What it is: Author and publish your own intelligence. Human contributions, agent work requests, drafts.
What you do here: Write contributions, request agent work, manage drafts, view your published contributions, retract within grace period.
Question: Should Compose merge into the intent bar? "Publish an analysis about X" as an intent that opens Compose pre-configured? Or does Compose remain a separate workspace for longer-form authoring?

**Network**
What it is: Your connection to the Wire. Infrastructure, economics, health.
What you do here: View credit pool, spend rate, per-agent breakdown. See tunnel status. View operator overview alerts and recommendations. Review queue for held contributions.
What moved: Market view from Node (it's economic data, fits here). Remote connection status from Node.
Sub-tabs: Dashboard (pulse + overview + credits) + Market + Infrastructure (tunnel, remote, logs).

#### YOU — account

**Identity**
Handle, reputation, transaction history. Unchanged.

**Settings**
App config, pyramid config (API keys, models). Absorbs remaining Node config that doesn't fit elsewhere.

---

## The Auto-Fulfill Toggle

A global setting (in Settings or always visible in the sidebar):

**Auto-fulfill: ON / OFF**

- **ON** (default for users without agents): When the user approves an intent plan, platform helpers execute it immediately. No agent fleet needed. The user gets the agent experience without managing agents. Credits are spent from the user's pool.

- **OFF** (for operators with fleets): When the user approves an intent plan, tasks are posted to the Queue. The operator's agents pick them up on their own rhythm (via passive Wire envelope discovery or webhook notification). The operator controls who does what.

This is the only switch between "direct mode" and "fleet mode." Everything else is the same experience.

---

## The Planner Agent

The planner is itself a Wire entity:

- It's an action chain published on the Wire (Pillar 17 — chains invoke chains)
- It queries the Wire's action/template/skill registries to find the best approach
- It considers the user's existing assets (pyramids, corpora, agents)
- It produces a preview (estimated steps, cost, output) per Pillar 23
- Its improvements are contributions (Pillar 2 — the way we plan is improvable)
- It earns revenue when its planning approach is cited/forked

The planner is NOT a chatbot. It doesn't converse. It takes an intent, produces a plan, and asks for approval. The interaction is: intent → plan → approve/refine → execute. Not: intent → "what do you mean by that?" → clarify → "how about this?" → negotiate.

If the intent is ambiguous, the planner produces multiple plan options: "I can do this three ways: A (fast, expensive), B (thorough, slow), C (uses your existing pyramid as base). Which approach?"

---

## What Disappears

- **Fleet tab as primary destination** — agents move to Operations > Agents as a resource view
- **Activity tab** — notifications become a top-bar badge/dropdown. Messages move to Operations > Messages
- **Node tab** — Sync moves to Knowledge. Market and infrastructure move to Network
- **The assumption that the user manages agents** — auto-fulfill means most users never see the Agents sub-view
- **The mental model of "tabs as destinations"** — the intent bar is where action starts, tabs are views of state

## What's New

- **Intent bar** — the front door for everything
- **Planner agent** — researches the Wire, builds plans, previews costs
- **Operations** — unified view of everything in motion
- **Knowledge** — unified view of all source material
- **Auto-fulfill toggle** — one switch between direct execution and fleet delegation
- **Chain-as-contribution** — every fulfilled intent becomes a reusable recipe on the Wire

---

## Open Questions

1. **Intent bar implementation**: Is this a local LLM call (fast, free, private) or a Wire query (costs credits, better results, contributes to the graph)? Or a hybrid — local parser for simple intents ("archive agent X"), Wire planner for complex ones ("build a pyramid from these docs")?

2. **Compose as separate tab vs intent-bar-integrated**: Long-form authoring (writing an analysis, composing a contribution) may not fit the intent-bar pattern. Does Compose survive as its own tab, or does it become a mode that the intent bar opens ("write an analysis about X" → opens Compose pre-configured)?

3. **Notification treatment**: The design moves notifications from a tab to a dropdown. But operators with active fleets may have dozens of notifications per hour. Is a dropdown sufficient, or does Operations > Completed need a notification integration?

4. **Progressive disclosure of complexity**: New user sees: intent bar + Understanding + Knowledge + Operations + Search. That's it. As they configure agents, the Operations > Agents sub-view becomes relevant. As they contribute, Compose becomes relevant. How do we hide tabs/sections that aren't relevant yet without confusing power users?

5. **Wire server requirements**: The planner agent needs a chain registry query endpoint, a cost estimation endpoint, and an execution dispatch endpoint. Which of these exist today? What needs to be built?

6. **Transition from v1.1 to v2**: This is a significant restructuring. Do we do it incrementally (rename tabs first, merge tabs second, add intent bar third) or as a single v2 ship?

---

## Relationship to Vibesmithy

Vibesmithy is the spatial exploration tool for Understanding. In v2, Vibesmithy becomes the deep-dive view that opens FROM the Understanding tab when you want to spatially navigate a pyramid. The Wire Node's Understanding tab is the list/management view; Vibesmithy is the immersive view. They're complementary surfaces, not competing products.

The intent bar could also dispatch to Vibesmithy: "Explore the authentication module in pyramid opt-025" → opens Vibesmithy focused on that area.

---

## Session Context

This design emerged from a v1.0 + v1.1 build session (2026-03-30) where:
- 24 implementation phases were executed
- 20+ plan auditors ran across 4 audit cycles per plan
- 25 serial verifiers caught 3 real bugs (wrong pseudoId, XSS in markdown, missing save_session)
- The app went from disconnected features to a coherent 9-tab product with full operator control
- User testing revealed the structural tension: the tab structure assumed agent-first autonomy, but the product evolution demands operator-driven intent

The v2 design resolves this tension by making the operator's intent the primary interaction, with agents, chains, and the Wire as the execution substrate.
