# Sprint 5 — Vibesmithy Wire Connection

## Context

Sprints 0-4 complete the alpha on the Wire Node side. Sprint 5 connects Vibesmithy (the human interface) to the Wire, enabling the "no node" consumer path and the full product vision.

## What Vibesmithy Needs

Vibesmithy today:
- Runs as a Next.js web app at vibesmithy.com
- Connects to a LOCAL pyramid API at localhost:8765
- Has Dennis (Partner) for conversational exploration
- Has spatial canvas (DOM-based, Canvas migration planned)
- Has no Wire connection, no intent bar, no credit system

Vibesmithy after Sprint 5:
- **Intent bar** — same as Wire Node's, speaks the same planner protocol
- **Wire authentication** — user logs in with Wire credentials, gets credits
- **Two connection modes:**
  - **Has a node**: connects to local node (localhost:8765). Node handles Wire interaction. Private, fast, free queries.
  - **No node**: connects to Wire directly. Pyramids built/hosted by operator nodes. Costs credits, no privacy guarantee for source material.
- **Dennis triggers chains** — "Want me to expand this area?" → intent → planner → approve → execute
- **Published pyramids browsable** — user can open any published pyramid in spatial view

## Phases

### Phase 0: Virtual Agent for Operator Sessions

The critical auth gap: action/chain endpoints require agent auth, but Vibesmithy operator sessions don't have agent tokens. Solution: when an operator authenticates via Vibesmithy (no node), the Wire server auto-provisions a "virtual agent" under that operator -- a minimal agent identity with `wire:contribute` and `wire:query` scopes, managed by the Wire, no local node required. The virtual agent's token is returned alongside the operator session and used for all wire-scoped calls. This is NOT a violation of Pillar 25 -- virtual agents use the same public API as regular agents, just without a physical node backing them.

> **Sprint 4 degraded mode:** If Sprint 4 helpers are not fully deployed, Sprint 5 can still ship with degraded functionality: Vibesmithy connects to Wire for discovery and querying existing pyramids, but cannot build NEW pyramids without helpers. This is a viable partial alpha.

### Phase 1: Vibesmithy Wire Auth

Add Wire authentication to Vibesmithy:
1. Login flow: email → magic link → operator session
2. Store session token (cookie or localStorage)
3. Display credit balance
4. Wire API calls use the session token

This mirrors the Wire Node's auth but in a web context (no Tauri, no Rust — pure Next.js).

**Files:**
- `vibesmithy/src/lib/wire-auth.ts` — auth flow
- `vibesmithy/src/components/WireLogin.tsx` — login UI
- `vibesmithy/src/app/layout.tsx` — auth provider

### Phase 2: Vibesmithy Intent Bar

Port the Wire Node's intent bar to Vibesmithy:
1. Same UI: text input + Go button + expandable plan preview
2. Same widget catalog (import from shared config)
3. **Planner call**: if connected to local node → call node's planner_call via HTTP API. If no node → call Wire server's planner action (Sprint 2's published action). **Note:** The planner action on the Wire (Sprint 2 Option C) is a STATIC contribution for forking/discovery -- it is NOT remotely executable. For no-node users, the planner must run via helpers (Sprint 4) or the Wire server must implement executeLlmStep. This is an acknowledged gap.
4. Same plan preview with widgets
5. Execution: if node → local execution. If no node → auto-fulfill via helpers (Sprint 4).

**Files:**
- `vibesmithy/src/components/IntentBar.tsx` — port from Wire Node
- `vibesmithy/src/components/planner/PlanWidgets.tsx` — port widgets

**Shared code:** Create a shared npm package `@wire/plan-widgets` that both Wire Node and Vibesmithy import. This package contains PlanWidgets, IntentBar logic (not UI -- that's platform-specific), and PlannerContext types. Prevents copy-paste divergence.

### Phase 3: Vibesmithy ↔ Node Connection

When a user has a Wire Node running:
1. Use the Wire's device-auth flow (`POST /api/v1/wire/device-auth`) to pair Vibesmithy with a local node. The node registers a pairing code with the Wire; Vibesmithy queries the Wire for active pairings under the operator. No direct localhost connection needed -- the Wire is the intermediary.
2. Connection indicator in UI: "Connected to local node" / "Connected via Wire"
3. All pyramid queries go through the local node
4. The node acts as smart proxy — local pyramids first, Wire for gaps
5. Dennis reads from local pyramids (private, fast)

When no node:
1. Vibesmithy connects to Wire directly
2. Pyramids discovered via Wire search
3. Queries go through remote pyramid access (Sprint 3)
4. Dennis reads from remote pyramids (costs credits)

**Files:**
- `vibesmithy/src/lib/node-client.ts` — update to handle both local and remote connections
- `vibesmithy/src/hooks/useNodeConnection.ts` — connection detection + mode switching

### Phase 4: Dennis Triggers Chains

Dennis can now trigger intent bar actions:
1. During conversation, Dennis notices a gap: "The pyramid doesn't cover deployment configs."
2. Dennis suggests: "Want me to expand the analysis to include deployment?"
3. User says yes → Dennis formats an intent → dispatches to planner
4. Plan preview appears (same as manual intent bar use)
5. User approves → chain executes → pyramid updates
6. Dennis's context refreshes with the new pyramid data
7. Conversation continues with expanded knowledge

This connects Dennis's conversational intelligence to the execution substrate.

Dennis-initiated intents always go through the full preview-then-commit flow (Pillar 23). Dennis presents the plan, user must explicitly approve. No auto-approval, even for low-cost operations.

**Files:**
- `vibesmithy/src/hooks/usePartner.ts` — add chain trigger capability
- `vibesmithy/src/components/chat/ChatPanel.tsx` — add "Dennis suggests action" UI

### Phase 5: Published Pyramid Browser

Any published pyramid on the Wire can be opened in Vibesmithy's spatial view:
1. Wire search results include published pyramids
2. Click "Open in Space" → Vibesmithy spatial view loads the pyramid
3. The pyramid is served via remote query (Sprint 3's infrastructure)
4. Dennis can explore it conversationally
5. The user's exploration generates value (questions become seeds — Pillar 36)

**Files:**
- `vibesmithy/src/app/space/[slug]/page.tsx` — handle remote pyramid slugs
- `vibesmithy/src/lib/node-client.ts` — remote pyramid data fetching

---

## Verification

1. **New user, no node, vibesmithy.com:**
   - Login with Wire credentials
   - Type "What is the Wire?" in intent bar
   - Planner finds published Wire documentation pyramids
   - Plan: "Query existing pyramid. Cost: N credits (spot quote). Discovery: governor-adjusted."
   - Approve → answer displayed
   - Open spatial view → Dennis guides exploration

2. **User with local node:**
   - Vibesmithy detects node at localhost:8765
   - "Connected to local node" indicator
   - Pyramid queries are local (free, fast)
   - Dennis reads local pyramids

3. **Dennis triggers expansion:**
   - Exploring a pyramid, Dennis says "This area is thin. Want me to expand?"
   - User: "Yes"
   - Intent → plan → approve → pyramid rebuilds with expanded scope
   - Dennis's context refreshes with new data

4. **Published pyramid browsing:**
   - Search the Wire → find a published pyramid
   - "Open in Space" → spatial view loads
   - Dennis available for exploration
   - Credits charged per query
