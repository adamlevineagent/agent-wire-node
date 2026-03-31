# Sprint 4 — Auto-Fulfill with Platform Helpers (ALPHA MILESTONE)

> **WARNING:** Sprint 4 is the largest and most uncertain sprint. The helper pool is entirely new Wire server infrastructure -- zero code exists today. Phase 1 alone may be a multi-sprint effort. This plan defines the target architecture; implementation may need to be split into sub-sprints.

## Context

Sprints 1-3 built the intent bar, chain-as-contribution flywheel, and remote pyramid access. But execution still requires either the user's own agents or their own node infrastructure. Sprint 4 adds **platform helpers** — ephemeral agents that execute chains for users who don't have persistent agents or local nodes. This completes the alpha loop.

## The Alpha Loop (Sprints 0-4)

After Sprint 4, the full experience works:
1. User asks a question (intent bar or Vibesmithy)
2. Planner builds a plan with cost estimate
3. User approves
4. **Auto-fulfill ON**: platform helpers execute the chain immediately (no user agents needed)
5. **Auto-fulfill OFF**: tasks posted to queue, user's fleet agents pick them up
6. Result delivered: pyramid appears in Understanding, contribution on Wire
7. Chain published as reusable recipe

## What Needs Building

### 1. Helper Pool on Wire Server

Platform helpers are ephemeral agents managed by the Wire:
- Registered under a platform operator account
- Each helper has a short-lived `gne_live_*` token
- Helpers spin up per-chain, execute, report results, shut down
- Multiple helpers can run in parallel for different users
- Helpers pay for LLM usage from the platform operator's OpenRouter key
- Users pay the platform a helper execution fee (credits)

### 2. Auto-Fulfill Toggle

A global setting (in Settings tab + visible in sidebar):
- **ON** (default for users without agents): approved plans execute immediately via helpers
- **OFF** (for operators with fleets): approved plans post to queue, fleet agents pick up

The toggle affects the execution path ONLY — planning, preview, approval are identical.

### 3. Chain Dispatch to Helpers

When auto-fulfill is ON and the user approves a plan:
1. IntentBar dispatches the chain to the Wire: `wireApiCall('POST', '/api/v1/wire/action/chain', { action_id, mode: 'trusted', input, chain_id })`
2. The Wire server assigns the chain to a helper from the pool
3. The helper executes the chain (may involve building a pyramid on a hosting node)
4. Results are returned to the user via the chain's completion callback
5. The user pays: helper execution fee + any access costs

### 4. Progress Tracking

While helpers execute:
- Operations > Active shows the chain with "Executing via helper" status
- Progress updates come from the Wire (polling or WebSocket)
- The user can cancel mid-execution
- On completion, result appears in the appropriate tab

## Phases

### Phase 1: Wire Server — Helper Pool Infrastructure

1. Platform operator account with helper management
2. Helpers register and operate through the same public API any agent uses (Pillar 25). A platform operator account manages helpers via the standard operator/agent endpoints. No internal-only APIs.
3. Chain dispatch-to-helper assignment logic
4. Helper execution monitoring + timeout handling
5. Helper fee calculation and debit (helper fee is 10% of the chain's estimated cost, minimum 1 credit; shown in the cost preview alongside other costs)

**Auth note:** Chain dispatch uses wireApiCall (agent token). The node's registered agent token has `wire:contribute` scope, which satisfies the action/chain endpoint auth requirements.

**Files:**
- Wire server: new `src/lib/server/helper-pool.ts` (pool management)
- Wire server: new `src/app/api/v1/wire/helper/` directory (dispatch, status, cleanup endpoints)
- Database: new `wire_helper_assignments` table in a migration

### Phase 2: Wire Node — Auto-Fulfill Toggle

1. Add `autoFulfill: boolean` to Settings (persisted in config)
2. Add `autoFulfill` to AppState
3. IntentBar reads `autoFulfill` to determine execution path:
   - ON → dispatch via Wire action chain (helpers)
   - OFF → post tasks to local queue (fleet picks up)

### Phase 3: Wire Node — Remote Chain Execution

1. **Quote-first flow**: Before trusted execution, POST with `mode: 'quote'` to get a spot quote with binding price. Show to user in cost preview. On approve, POST with `mode: 'trusted', quote_id: <token>`. If quote TTL expires before approval, re-quote.
2. IntentBar dispatches chain to Wire server when auto-fulfill is ON
3. Poll for chain status updates: polling via existing wireApiCall. Poll `GET /api/v1/wire/action/chain/${chainId}/status` every 5 seconds (new endpoint needed). No WebSocket -- polling is sufficient for Sprint 4.
4. Handle results: pyramid built on remote node → add to Understanding as remote
5. Handle errors: helper failure, timeout, insufficient credits

### Phase 4: Wire Node — UX Polish

1. "Executing via helper" status in Operations > Active
2. Cancel button for helper-executed chains
3. Cost breakdown: helper fee + access costs shown separately. Action execution costs shown via spot quote. Governor-adjusted query costs within chains are dynamic -- show "dynamic" for query steps.
4. Progress indicator with step-by-step updates
5. Settings toggle visible in sidebar (compact section)

---

## Verification (ALPHA ACCEPTANCE CRITERIA)

**Scoping correction:** Sprint 4 enables no-agent execution for users WITH a node. The no-node consumer path is Sprint 5.

1. **New user, no agents, no node infrastructure:**
   - Opens Wire Node (or Vibesmithy in Sprint 5)
   - Types "How does auth work in this React project?"
   - Links a folder
   - Planner shows plan: "Build pyramid, answer question. Cost: 250 credits."
   - Approves → helper builds pyramid on Wire infrastructure
   - Result appears in Understanding → can explore in Vibesmithy
   - Chain published to Wire → others can fork it

2. **Operator with fleet, auto-fulfill OFF:**
   - Types intent → planner shows plan → approves
   - Tasks posted to queue → fleet agents claim and execute
   - Same result, operator's infrastructure, operator's data stays local

3. **Costs are transparent:**
   - Helper fee shown before approval
   - Access costs shown before approval
   - Governor-adjusted query costs reflected (note: governor costs are only known at query time, not pre-execution)
   - Post-execution cost breakdown available

4. **Everything works end-to-end** — that's the alpha.
