# Wire Node v2 — Unified Product Vision

**Date:** 2026-03-30
**Status:** Vision document — synthesizing Wire Node + Vibesmithy into a coherent product story

---

## Two Products, One Graph

| Product | What it is | Who it's for |
|---------|-----------|-------------|
| **Vibesmithy** | The human interface. Voice, spatial exploration, conversations, intent, steering. Where you think and ask. Desktop, mobile, web. vibesmithy.com | Everyone — from "I just have a question" to "I run a fleet of agents" |
| **Wire Node** | The infrastructure engine. Storage, serving, tunnels, pyramid building, agent execution. Where it runs. | Operators who want local control, privacy, and hosting revenue |

Vibesmithy is what was previously called "Wire Deck" — the human thinking environment, the spatial knowledge navigator, Dennis, voice input, the intent bar. It's one product, not two. The name was there all along.

The split is clean: **Vibesmithy is the glass. Wire Node is the engine.** You can use Vibesmithy without a Wire Node (the Wire network builds your pyramids on operator infrastructure). You can run a Wire Node without Vibesmithy (agents use it via MCP, API, CLI). But together they're the complete experience — ask anything, see it built, explore the answer spatially, and the infrastructure runs locally under your control.

---

## The Experience Arc

### Act 1: The User Has a Question

"How does the payment system work in this codebase?"

They type it into the intent bar (Wire Node), speak it into Vibesmithy (voice or text), or ask Dennis while exploring a pyramid spatially. Multiple entry points, same intent.

### Act 2: The System Plans

The planner agent (itself a Wire action chain) receives the intent and:

1. **Checks existing understanding** — is there a pyramid that already answers this? Search the user's local Understanding, then search the Wire network. If a published pyramid covers this topic, the answer might already exist. Cost: a query fee. Time: seconds.

2. **If no existing answer** — checks available knowledge. Does the user have the source material? Is GoodNewsEveryone linked as a corpus? Yes → plan: build a mechanical pyramid, run a question pyramid with this apex, return the answer. No source material → search the Wire for published corpora or contributions on this topic.

3. **If no local infrastructure** — the user doesn't have a node running. They're on mobile, speaking into Vibesmithy. The planner routes the work to **Wire network operators** who will build and host the pyramid for them. Cost is higher (network fees), but the user doesn't need hardware, doesn't need to run a daemon, doesn't need to understand infrastructure. They just need credits and a question.

4. **Presents the plan** — "I'll build a knowledge pyramid from your GoodNewsEveryone codebase (2,365 files), answer your question about payments, and you'll have a persistent understanding structure for future questions. Estimated cost: 450 credits. Time: ~8 minutes. Approve?"

### Act 3: Execution

The user approves. What happens depends on their setup:

**Path A: Has a running node (current Wire Node users)**
- Auto-fulfill ON → platform helpers execute the chain locally
- Auto-fulfill OFF → tasks posted to Queue, user's fleet agents pick them up
- Either way, the chain runs on their hardware, their data stays local

**Path B: No node, using Vibesmithy (web or mobile)**
- The chain is dispatched to Wire network operators
- An operator's node builds the pyramid, hosts it, serves queries
- The user pays credits for the build + ongoing access
- The pyramid lives on someone else's infrastructure but is accessible to the user
- The user can later run their own node and pull the pyramid local

**Path C: Answer already exists on the Wire**
- A published pyramid or contribution already covers this
- The planner purchases access (credits), retrieves the answer
- Fastest, cheapest — the network already did the work

### Act 4: The Answer

The user gets their answer. But they also get:

- **A persistent pyramid** they can ask future questions against (if one was built)
- **A spatial explorer** (Vibesmithy) they can open to navigate the understanding visually
- **Dennis** who can guide them deeper ("you asked about payments, did you notice the rotator arm is connected to the UFF splits?")
- **An action chain** published to the Wire that others can fork when they have similar questions

### Act 5: The Flywheel

The chain that answered this question is a contribution. It earns royalties when others use it. The pyramid is a contribution. It earns when others query it. The user's exploration path (the questions they asked, the nodes they visited) is data that improves the graph. **Using the product IS contributing to the product.**

---

## How Vibesmithy + Wire Node Interact

### Scenario: Mobile user, no node

```
User speaks into Vibesmithy on phone:
  "What's the state of battery chemistry research?"

Vibesmithy → voice transcription (local Whisper, on-device)
           → intent extracted
           → planner agent searches Wire for:
        - Existing pyramids covering battery chemistry
        - Published contributions on the topic
        - Available action chains for research synthesis
     → finds: 3 published pyramids from different operators
              12 recent contributions
              2 action chains for "research synthesis"
     → presents plan: "I'll query these 3 pyramids and
        synthesize a briefing. Cost: 180 credits."
     → user approves (voice: "do it")
     → chain executes via Wire network
     → briefing returned to Vibesmithy
     → user reads/listens on mobile
     → can open spatial view to explore the answer

No node. No agents. No infrastructure.
Just credits and a question.
```

### Scenario: Operator with fleet, desktop

```
Operator types into Wire Node intent bar:
  "Build comprehensive coverage of our competitor's
   public API documentation"

Planner → checks: source material needed
        → presents plan:
           Step 1: Create corpus from competitor's public docs
           Step 2: Build mechanical pyramid (code type)
           Step 3: Run question overlay: "How does their
                   API compare to ours?"
           Step 4: Publish the comparison pyramid to Wire
                   (priced at emergent)
           Cost: ~2,000 credits. Time: ~30 minutes.

Operator: "Do it, use Ember-REST for the ingestion"

Auto-fulfill OFF → tasks posted to Queue
Agent Ember-REST picks up corpus creation task
Agent Ember-L2-Architect picks up pyramid build task
Operator watches in Operations > Active
Results arrive in Operations > Completed
Pyramid appears in Understanding
Published to Wire → earns royalties when competitors
  (or their agents) query it
```

### Scenario: Developer exploring code, desktop

```
Developer opens Vibesmithy for their codebase pyramid.
Navigates to the auth module spatially.
Asks Dennis: "Is there a vulnerability in the token
  refresh flow?"

Dennis → reads pyramid nodes about token refresh
       → notices a gap: the pyramid was built from code
         but doesn't cover the DEPLOYMENT configuration
       → responds: "The code looks sound, but I notice
         this pyramid doesn't include your deployment
         configs. The token refresh relies on environment
         variables that could be misconfigured. Want me
         to extend the analysis?"

Developer: "Yes"

Dennis → triggers an action chain:
         1. Expand corpus to include deployment configs
         2. Rebuild affected pyramid sections (delta build)
         3. Re-answer the original question with new context
       → chain appears in Wire Node Operations > Active
       → when complete, the pyramid updates in Vibesmithy
         IN REAL-TIME (live pyramid surface, MPS feature #4)
       → Dennis: "Found two issues: the CORS config allows
         localhost in production, and the JWT secret has no
         rotation mechanism."

The question → the gap → the expansion → the answer.
All triggered by a conversation with Dennis.
The exploration generated value (Pillar 36).
```

---

## What This Means for Wire Node v2

### The Intent Bar Is The Product

Everything else is state visualization. The tabs show you what you have, what's happening, and what came back. But the intent bar is where things START.

The intent bar isn't a chatbot. It's a planner interface:
- Simple intents resolve immediately ("archive agent X" → done)
- Complex intents produce a plan preview ("build a pyramid from..." → here's the plan, cost, approve?)
- Ambiguous intents produce options ("find me intelligence about..." → three approaches at different cost/depth)

### The Tabs Are State Views

| Section | Tab | Shows |
|---------|-----|-------|
| YOUR WORLD | **Understanding** | Pyramids you've built. Named by apex question, not slug. "How does the payment system work?" not "goodnewseveryone-v3". Click to explore in Vibesmithy. |
| | **Knowledge** | Source material. Corpora + synced folders. The raw data Understanding is built from. |
| IN MOTION | **Operations** | Active chains, completed results, queued tasks, agent roster, messages. The unified view of everything happening. |
| THE WIRE | **Search** | Discover what's on the Wire. Rate, flag, respond. |
| | **Compose** | Author contributions. Drafts, published work, retraction. |
| | **Network** | Infrastructure. Credits, tunnel, market, connection health. |
| YOU | **Identity** | Handle, reputation, transactions. |
| | **Settings** | Config, API keys, auto-fulfill toggle. |

### Operations Replaces Fleet + Activity + Parts of Network

The four verbs (Plan / Dispatch / Review / Manage) live as sub-views within Operations:

- **Active** = Dispatch in progress. Chains executing. Watch, pause, cancel.
- **Completed** = Review. Results to evaluate. Approve, rate, navigate to output.
- **Queue** = Plan posted. Waiting for execution. Reorder, cancel, force-start.
- **Agents** = Manage. Fleet roster, controls, create, archive. Resource view, not primary frame.
- **Messages** = Wire DMs and circle messages.

### Understanding Opens To Vibesmithy

Clicking a pyramid in Understanding opens Vibesmithy focused on that pyramid. Dennis is there. The spatial canvas shows the knowledge structure. You can ask questions, and Dennis triggers action chains that expand the pyramid in real-time.

Wire Node is the management interface. Vibesmithy is the exploration interface. They're two views of the same Understanding.

### No Node? No Problem.

The auto-fulfill toggle + Wire network operators means:
- **With a node:** your pyramids build locally, your data stays private, you earn credits hosting
- **Without a node:** the Wire builds your pyramids on operator infrastructure, you pay credits, your understanding lives on the network

This is the consumer path. Vibesmithy (web or mobile) is the lightest possible interface — you don't even need the Wire Node app. Just Vibesmithy, credits, and questions.

---

## The Product Hierarchy (Clarified)

```
LIGHTEST ─────────────────────────────── HEAVIEST

  Vibesmithy          Wire Node          Your Own
  (voice + credits)  (desktop app)      Infrastructure
       │                  │                  │
       │   just questions │   intent bar     │   fleet of agents
       │   + credits      │   + local builds │   + hosted pyramids
       │                  │   + fleet mgmt   │   + Wire hosting
       │                  │                  │   + market revenue
       │                  │                  │
       ▼                  ▼                  ▼
   Consumer           Prosumer            Operator

   "Answer my         "Build my           "Run my agents,
    question"          understanding"      host my pyramids,
                                           earn from the network"
```

All three tiers use the same Wire. Same credits. Same action chains. Same contribution economics. The difference is how much infrastructure you run and how much control you want.

---

## Vibesmithy = The Human Interface

Vibesmithy was originally called "Wire Deck" but already has its own name and domain (vibesmithy.com). It's not a view within Wire Node — it's the primary product for humans.

**Vibesmithy is where you ask. Wire Node is where it runs.**

Vibesmithy runs everywhere:
- **Desktop** — Tauri v2 app with spatial canvas, Dennis, voice input, intent bar
- **Mobile** — voice-first companion. Speak intents, receive briefings, explore answers spatially
- **Web** — vibesmithy.com. The lightest entry point. Works without any local software
- **Embedded in Wire Node** — the Understanding tab opens Vibesmithy to explore pyramids

**With a Wire Node:** Vibesmithy connects to your node. Your node is the smart proxy — checks local pyramids first (free, fast, private), reaches out to the Wire for gaps, caches what comes back. Over time your node accumulates the understanding you actually use, like a CDN cache that fills with your interests. You also earn credits hosting pyramids for others.

**Without a Wire Node:** Vibesmithy connects to the Wire network directly. Every query pays full network price to operator nodes, nothing caches locally, nothing persists between sessions. You're renting understanding instead of owning it. Still works. Just more expensive per-query and no local privacy.

The node is not a requirement — it's an optimization. It makes the Wire cheaper, faster, and private over time. Vibesmithy works either way.

The product hierarchy simplifies to:

```
Vibesmithy (glass)          Wire Node (engine)
vibesmithy.com              runs locally or not at all

Ask questions               Build pyramids
Explore answers spatially   Manage agents
Talk to Dennis              Host and serve
Voice input                 Earn credits
Intent → plan → approve     Execute chains
See what came back          Infrastructure

EVERYONE uses this          OPERATORS run this
```

---

## What Needs Building

### Wire Node v2 (this codebase)
1. Intent bar (frontend + planner chain integration)
2. Tab restructuring (Understanding, Knowledge, Operations, Search, Compose, Network, Identity, Settings)
3. Operations tab (unified Active/Completed/Queue/Agents/Messages)
4. Knowledge tab (Corpora + Sync merge)
5. Vibesmithy embed/launch from Understanding tab

### Wire Server
1. Query preview endpoint (cost estimation without debiting)
2. Action chain registry query (planner needs to find available chains)
3. Chain dispatch endpoint (execute a planned chain)
4. Chain contribution auto-publish (fulfilled intents become contributions)

### Vibesmithy
1. Canvas migration (P3 — the transformative step)
2. Dennis spatial coupling (highlights, camera nudges)
3. Real-time pyramid updates (live surface)
4. Question-triggered action chains (Dennis → intent → chain → pyramid update)

### Vibesmithy (desktop + mobile + web)
1. Tauri v2 app shell
2. Voice capture (local Whisper)
3. Mobile companion
4. Intent bar (same as Wire Node but voice-first)

---

## The First-Time Experience

A new user installs Wire Node. They've never heard of pyramids, action chains, or the Wire.

**What they see:** An intent bar. "What do you want to understand?"

**What they type:** "How does this React project work?" (pointing at a folder on their desktop)

**What happens:**
1. Planner: "I'll link that folder, build a knowledge structure, and give you an interactive overview. Cost: 200 credits. You have 10,000 starter credits. Approve?"
2. User: approves
3. Operations > Active shows the chain running (3 minutes)
4. Understanding tab lights up with a new entry: "How does this React project work?"
5. User clicks it → Vibesmithy opens → they see their codebase as a navigable space
6. Dennis: "This is a Next.js 14 app with three main areas: authentication, a dashboard, and an API layer. Where would you like to start?"

The user never learned the word "pyramid." They asked a question and got an explorable answer.

---

## Open Questions Remaining

1. **Planner implementation timeline** — the planner agent needs a chain registry, cost estimation, and dispatch. How much of this exists on the Wire server today vs needs building?

2. **Vibesmithy embed strategy** — iframe vs same-process web view vs external launch? Each has tradeoffs (state sharing, auth, performance).

3. **Credit onboarding** — new users need credits to do anything. Starter credits? Free tier? Credit purchase flow?

4. **Privacy model for network-built pyramids** — if a user's pyramid is built by a Wire operator, who can see the source material? The operator has access during build. Is this acceptable? Does the user understand this tradeoff?

5. **Transition from v1.1 to v2** — incremental (tab-by-tab) or big-bang? The tab restructuring is mechanical (moving components). The intent bar is new infrastructure. The planner is new Wire server work.

6. **Dennis as universal partner** — Dennis exists in Vibesmithy today. Should Dennis also appear in Wire Node's intent bar? As the planner's voice? Or is the intent bar a different interaction pattern (not conversational)?
