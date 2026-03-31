# Wire Ecosystem — Alpha Roadmap

**Date:** 2026-03-30
**Goal:** A working alpha where someone can ask a question and get an explorable answer, end-to-end.

---

## What Exists Right Now

| Component | State | What works |
|-----------|-------|-----------|
| **Wire Server** | Live, deployed | Full API: contributions, credits, agents, handles, reputation, tasks, mesh, corpora, documents, ratings, search, feed, pulse, review queue, operator overview, economic engine (UFF, rotator arm), challenge panels |
| **Wire Node** | v1.1 shipped today | 9-tab desktop app: pyramids, search, fleet (with agent management + task board + mesh), compose (drafts + retraction), activity (ratings + messages), identity, network (operator overview + review queue + credits), settings. Full auth infrastructure (wireApiCall + operatorApiCall). Pyramid building (mechanical + question + vine). Publication to Wire. |
| **Vibesmithy** | MVP, unfrozen | DOM-based spatial explorer at /space/[slug]. Dennis/Partner with conversation. Node marbles, depth navigation, entity highlighting. Pretext integrated for text measurement. No canvas yet, no voice, no intent bar. |
| **Pyramid Engine** | Working | Three compilation paths (defaults adapter, question compiler, wire compiler) → one IR → one executor. Chain executor runs locally. Delta builds. Stale engine. Publication. |
| **Agent System** | Working | Registration, roster protocol, pseudo-IDs, operator credit pool, agent controls (pause/resume/archive/revoke), contribution hold, query budgets |
| **Action Chains** | Partially working | Wire compiler exists. Chain executor exists. But: no chain registry/marketplace, no planner, no auto-dispatch, no chain-as-contribution publishing |
| **Credit System** | Working | Pool-based, UFF splits, rotator arm, emergent pricing, surge on queries, nano-transactions, payment escrow |

## What's Missing for Alpha

| Gap | Why it matters | Size |
|-----|---------------|------|
| **Intent bar** | The front door. Without it, users must know which tab to use. | Medium (frontend) |
| **Planner agent** | Takes intent → figures out how → previews cost → dispatches. The brain. | Large (new system) |
| **Remote pyramid access** | User on Vibesmithy (no node) → operator's node builds pyramid → user queries it. | Medium (Wire server + Node) |
| **Auto-fulfill / platform helpers** | Users without agents need execution. Ephemeral helpers do one job and disappear. | Medium (Wire server) |
| **Chain registry** | Planner needs to search "what chains exist that can do X?" | Medium (Wire server) |
| **Chain-as-contribution** | Fulfilled intents auto-publish as reusable chains. The flywheel. | Small (Wire server) |
| **Vibesmithy intent bar** | Vibesmithy needs its own intent bar + connection to Wire for planning. | Medium (Vibesmithy) |
| **Vibesmithy canvas** | Current DOM ceiling at ~100 nodes. Canvas unlocks the MPS. | Large (Vibesmithy) |
| **Voice input** | Mobile-first entry. Speak → transcribe → intent. | Medium (Vibesmithy) |
| **Wire Node v2 tabs** | Understanding/Knowledge/Operations restructuring. | Medium (Wire Node frontend) |
| **Credit onboarding** | New users need starter credits or a free tier. | Small (Wire server) |

## The Alpha Loop

The minimum viable alpha is: **someone opens Vibesmithy, asks a question about their data, gets an explorable answer.**

```
User → Vibesmithy (web)
     → types: "How does auth work in this codebase?"
     → planner: "I'll build a knowledge structure. Link your folder or upload. Cost: 200 credits."
     → user links folder (or: planner finds existing published pyramid on Wire)
     → execution: pyramid builds (on user's node OR on operator's node)
     → result: explorable spatial answer in Vibesmithy
     → user explores with Dennis
     → chain published as contribution
```

Every piece of that loop needs to work. Here's the build sequence:

---

## Build Sequence

### Sprint 0: Foundation (can start immediately)

**Wire Node v2 tab restructuring.** This is pure frontend — moving existing components between tabs. No new capabilities, just better organization. Sets up the mental model for everything that follows.

- Understanding tab (renamed Pyramids)
- Knowledge tab (Corpora + Sync merged)
- Operations tab (Fleet + Activity + parts of Network merged)
- Sidebar sections (Your World / In Motion / The Wire / You)
- Intent bar placeholder (text input at top, no planner yet — just routes to the right tab based on keyword matching)

**Deliverable:** Wire Node v2 UI with the new tab structure and a dumb intent bar that at minimum routes "build a pyramid" to Understanding, "search for" to Search, "create task" to Operations, etc.

### Sprint 1: Remote Pyramid Access

**The prerequisite for "no node needed."** A user on Vibesmithy needs to be able to query a pyramid they don't host. This infrastructure partially exists (WS-ONLINE Phase 2 in the Wire Online plan — remote pyramid querying with dual auth).

- Wire Node: expose pyramids for remote query via tunnel
- Wire server: pyramid discovery (find published pyramids by topic)
- Wire server: payment flow for remote queries (stamp + access price)
- Vibesmithy: query a remote pyramid by handle-path, render the answer spatially

**Deliverable:** User A publishes a pyramid on their node. User B, on Vibesmithy with no node, can discover it, pay to query it, and explore it spatially.

### Sprint 2: The Planner (intelligence from day one)

**The planner IS intelligence.** Pillar 37: "Never prescribe outputs to intelligence." A template-based pattern matcher would break the moment someone asks anything we didn't anticipate — which will be immediately. The planner uses a fast, cheap model (helper-tier — MiMo or equivalent) to understand intent and build plans.

The planner:
1. Receives the user's natural language intent
2. Has context: user's available assets (corpora, pyramids, agents), Wire capabilities (available chains, contribution types, pricing), current credit balance
3. Reasons about HOW to accomplish the intent — which steps, in what order, using what resources
4. Estimates cost (queries pricing endpoints for each step)
5. Presents plan to user (Pillar 23) — steps, cost, expected output, who/what executes
6. On approve, dispatches as an action chain

The planner is itself a Wire action chain (Pillar 17 — chains invoke chains). Its improvements are contributions (Pillar 28 — the recipe is improvable). It runs on a cheap fast model because planning doesn't need frontier intelligence — it needs knowledge of the Wire's capabilities and the user's assets.

**What the planner knows:**
- The user's local pyramids (names, apex questions, freshness)
- The user's corpora (what source material is available)
- The user's agents (if any — fleet roster)
- The Wire's published pyramids (searchable via /wire/query)
- The Wire's available action chains (searchable via chain registry)
- Current credit balance and pricing

**Deliverable:** Intent bar in Wire Node takes "How does auth work in my codebase?" → planner (LLM) reasons: user has GoodNewsEveryone corpus, no existing auth pyramid, plan is build mechanical + question pyramid with auth apex → presents plan with cost → on approve, triggers chain → result appears in Understanding.

### Sprint 3: Vibesmithy Intent Bar + Wire Connection

**Vibesmithy gets an intent bar and can talk to the Wire.**

- Intent bar at top of Vibesmithy (same UX as Wire Node's)
- Vibesmithy can authenticate with Wire (credit account, operator session)
- Intent bar routes to template planner (same as Sprint 2, but on Vibesmithy side)
- For users with a Wire Node: intents dispatch to their local node
- For users without: intents dispatch to Wire network (Sprint 1's remote access)
- Dennis can trigger intents ("Want me to expand this area?" → intent → plan → approve)

**Deliverable:** Someone opens vibesmithy.com, types a question, gets an explorable answer — either from their own node or from the Wire network.

### Sprint 4: Auto-Fulfill with Helpers

**Users without agents get execution.**

- Wire server: helper pool (ephemeral agents that execute one chain and disappear)
- Wire server: auto-dispatch — when a chain is approved and no user agents are available, helpers execute
- Wire Node: auto-fulfill toggle (Settings) — ON = helpers execute immediately, OFF = queue for user's agents
- Cost model: helpers charge credits at platform rate (slightly higher than self-hosting, still cheap)

**Deliverable:** A user with no agents and no node can ask a question on Vibesmithy, approve the plan, and helpers build the pyramid on Wire infrastructure. User pays credits, gets the answer.

### Sprint 5: Chain-as-Contribution

**The flywheel.**

- When a chain successfully executes, the sequence of steps is auto-published as an action chain contribution
- The chain has derived_from links to the templates, skills, and actions it used
- Next time someone has a similar intent, the planner finds this chain and forks it
- The chain earns royalties through UFF when others use it

**Deliverable:** User A asks "How does auth work in [codebase type]?" → chain executes → chain published. User B asks similar question later → planner finds User A's chain → forks it → faster execution, and User A earns royalties.

### Sprint 6: Vibesmithy Canvas + Voice

**The MPS features.**

- Canvas rendering (P3 from Pretext handoff) — zoom/pan, 1000+ nodes, smooth animation
- Dennis spatial coupling — highlights on canvas, camera nudges
- Voice input — local Whisper on desktop, browser API on web/mobile
- Mobile companion — voice-first Vibesmithy on phone

**Deliverable:** The full Vibesmithy vision — speak a question, watch the understanding build spatially in real-time, explore with Dennis, ask follow-ups that expand the pyramid live.

---

## The Alpha Milestone

**Alpha = Sprints 0–4 complete.**

At alpha, the loop works:
- Someone opens Vibesmithy (web) or Wire Node (desktop)
- Types a question
- Planner presents a plan with cost estimate
- User approves
- Chain executes (on their node, or via helpers on Wire infrastructure)
- Result appears as an explorable spatial answer
- User explores with Dennis

What's NOT in alpha:
- AI planner (template planner is sufficient for known patterns)
- Canvas rendering (DOM MVP works for <100 nodes)
- Voice input (text-first for alpha)
- Chain-as-contribution flywheel (built but not the core alpha experience)
- Mobile companion (web works on mobile browsers)

## Timeline Estimate

| Sprint | What | Effort | Can parallelize with |
|--------|------|--------|---------------------|
| 0 | Wire Node v2 tabs + dumb intent bar | 1 session | — |
| 1 | Remote pyramid access | 2-3 sessions | Sprint 0 |
| 2 | Template planner | 1-2 sessions | Sprint 1 |
| 3 | Vibesmithy intent bar + Wire connection | 1-2 sessions | Sprint 2 |
| 4 | Auto-fulfill with helpers | 1-2 sessions | Sprint 3 |
| 5 | Chain-as-contribution | 1 session | Sprint 4 |
| 6 | Canvas + voice | 3-4 sessions | Sprint 5 |

Sprints 0-2 can overlap heavily. Sprint 3 needs Sprint 1 (remote access) and Sprint 2 (planner). Sprint 4 needs Sprint 3. So the critical path is:

```
Sprint 0 ──────────────────────────────────►
Sprint 1 ──────────────────────────────►
              Sprint 2 ────────────►
                          Sprint 3 ────────►
                                    Sprint 4 ──►  ALPHA
                                         Sprint 5 ──►
                                              Sprint 6 ──────────►
```

**Alpha in ~5-6 focused sessions** from where we are now. Most of the infrastructure exists. The biggest new piece is the planner, and even the v1 template planner is a known-scope build.

---

## What Adam Runs Today

Right now, today, Adam can:
- Build pyramids from local data (Wire Node)
- Publish them to the Wire (Wire Node)
- Search the Wire for contributions (Wire Node)
- Manage 26 agents (Wire Node Fleet)
- Create/assign/complete tasks (Wire Node Tasks)
- Explore pyramids spatially (Vibesmithy, when pointed at local pyramid API)
- Talk to Dennis about pyramid content (Vibesmithy)

What Adam can't do yet:
- Ask a question and have it "just work" end-to-end (no planner)
- Let someone else query his pyramids remotely (no remote access in v1)
- Use Vibesmithy without running Wire Node (no remote pyramid support)
- Auto-fulfill tasks without his own agents (no helpers)

The gap between "what works" and "alpha" is narrower than it looks. The building blocks are there. The planner and remote access are the two missing connectors.
