# Wire Node v2 Alpha — Sprint Execution Guide

**Created:** 2026-03-30
**Status:** Sprint 0 shipped. Sprints 1-5 planned, audited, and locked.

---

## The Product

Two products, one graph:
- **Vibesmithy** (vibesmithy.com) — the human interface. Ask questions, explore answers spatially, talk to Dennis. Desktop, mobile, web.
- **Wire Node** — the engine. Builds pyramids, manages agents, hosts and serves, earns credits. Runs locally.

The user asks a question. The system figures out how to answer it. Pyramids are an implementation detail — the user never needs to know the word.

**Vision docs:**
- `docs/plans/v2-intent-driven-experience.md` — the intent-driven experience design
- `docs/plans/v2-unified-product-vision.md` — Vibesmithy + Wire Node product split
- `docs/plans/alpha-roadmap.md` — the full roadmap with timeline estimates

---

## Sprint Status

| Sprint | Plan | Status | What it delivers |
|--------|------|--------|-----------------|
| **0** | `sprint-0-tab-restructuring.md` | ✅ **SHIPPED** | 10-tab sidebar with live status, Understanding/Knowledge/Tools/Fleet/Operations/Search/Compose/Network/Identity/Settings, intent bar placeholder |
| **1** | `sprint-1-intent-planner.md` | 📋 **PLAN LOCKED** | Intent bar planner (local LLM), widget catalog UI, plan preview, local execution, Operations Active tracking |
| **2** | `sprint-2-chain-migration.md` | 📋 **PLAN LOCKED** | Planner published to Wire, chain-as-contribution flywheel, review mode cost estimation |
| **3** | `sprint-3-remote-pyramids.md` | 📋 **PLAN LOCKED** | Remote pyramid access, payment flow (stamps + access price), pyramid discovery |
| **4** | `sprint-4-auto-fulfill.md` | 📋 **PLAN LOCKED** | Platform helpers, auto-fulfill toggle — **ALPHA MILESTONE** |
| **5** | `sprint-5-vibesmithy-connection.md` | 📋 **PLAN LOCKED** | Vibesmithy connects to Wire, virtual agents, Dennis triggers chains |

**Alpha = Sprints 0-4 complete.** Sprint 5 connects Vibesmithy.

---

## Prerequisites & Dependencies

```
PARALLEL WORKSTREAMS (running independently):
  ├── Wire compiler review mode (handoff: docs/handoffs/wire-compiler-review-mode-handoff.md)
  └── Query governor (spec: docs/economy/wire-query-governor.md)

SPRINT DEPENDENCY CHAIN:
  Sprint 0 (SHIPPED)
    → Sprint 1 (blocks on: compiler review mode + query governor landing)
      → Sprint 2 (blocks on: Sprint 1 + VALID_TYPES deployed)
        → Sprint 3 (blocks on: Sprint 2)
          → Sprint 4 (blocks on: Sprint 3) ← ALPHA
            → Sprint 5 (blocks on: Sprint 4, degraded mode OK without helpers)
```

### Wire Server Changes Required

| Change | Sprint | Status | File |
|--------|--------|--------|------|
| Add `action`, `skill`, `template` to VALID_TYPES | 1 (Phase 0) | ❌ Not deployed | `GoodNewsEveryone/src/lib/server/contribute-core.ts` |
| Bearer fallback in `requireOperatorApi()` | Pre-1 | ✅ Deployed | `GoodNewsEveryone/src/lib/server/operator-auth.ts` |
| Compiler `review` mode (dry-run) | 2 (optional) | 🔨 In progress | `GoodNewsEveryone/src/lib/server/wire-compiler.ts` |
| Query governor | 2 (optional) | 🔨 In progress | `GoodNewsEveryone/src/lib/server/surge-engine.ts` |
| Helper pool infrastructure | 4 | ❌ Not started | New: `GoodNewsEveryone/src/lib/server/helper-pool.ts` |
| Chain status polling endpoint | 4 | ❌ Not started | New: `GoodNewsEveryone/src/app/api/v1/wire/action/chain/[chainId]/status/` |
| Virtual agent for operator sessions | 5 | ❌ Not started | `GoodNewsEveryone/src/lib/server/wire-auth.ts` |

---

## Sprint 1 — Intent Bar Planner

**Plan:** `sprint-1-intent-planner.md`
**Audited:** 2 cycles, all corrections applied

### Phases

| Phase | What | Size | Depends on | Key files |
|-------|------|------|-----------|-----------|
| 0 | Wire server VALID_TYPES fix | Tiny | — | `GoodNewsEveryone/src/lib/server/contribute-core.ts` |
| 1 | `planner_call` Tauri command | Medium | — | `src-tauri/src/main.rs` |
| 2 | Widget components (6 types) | Medium | — | New: `src/components/planner/PlanWidgets.tsx` |
| 3 | IntentBar rewrite (7-state machine) | Large | P1 + P2 | `src/components/IntentBar.tsx`, `src/contexts/AppContext.tsx` |
| 4 | Operations Active sub-tab | Small | P3 | `src/components/modes/OperationsMode.tsx` |
| 5 | Planner system prompt | Small | P1 | New: `chains/prompts/planner/planner-system.md` |

### Key decisions from audit
- Prompt loads from `chains/prompts/planner/` via `chains_dir` (NOT `src-tauri/prompts/`)
- Use `response_format: json_object` (not strict `json_schema`) for model compatibility
- `PlannerContext` schema defined — pyramids, corpora, agents (roster), fleet summary, balance
- Widget components receive `context: PlannerContext` prop for data (CorpusSelector options, AgentSelector options)
- `build_pyramid` execution: resolve corpus → check slug exists → create if needed → build
- IntentBar state: discriminated union with `useState`, disable input during execution
- Preview panel: inside `.intent-bar-wrapper`, `max-height: 50vh`, auto-collapse on mode change
- JSON parsing: use `llm::extract_json()`, retry once on failure, `max_tokens: 2048`

### Verification checklist
- [ ] Type question → context gathered → planner returns plan → widgets render
- [ ] Fill corpus, type question, approve → pyramid builds → appears in Operations Active → completes
- [ ] Type search intent → planner returns search plan → navigates to Search
- [ ] Type fleet action → confirmation → action executes
- [ ] Gibberish → planner handles gracefully
- [ ] Cost preview shows "estimated" (not exact)
- [ ] Prompt loads from .md file
- [ ] `npx tsc --noEmit` + `cargo check` pass

---

## Sprint 2 — Chain Migration + Flywheel

**Plan:** `sprint-2-chain-migration.md`
**Audited:** 1 cycle, corrections applied

### Phases

| Phase | What | Size | Depends on |
|-------|------|------|-----------|
| 1 | Publish planner action to Wire | Small | VALID_TYPES deployed |
| 2 | Post-execution chain publishing | Medium | P1 |
| 3 | Review mode cost estimation (CONDITIONAL) | Small | Compiler review mode deployed |
| 4 | Tools tab shows published chains | Small | P2 |

### Key decisions from audit
- **Pillar 17 stepping stone, not full compliance** — planner definition published (forkable, citable) but execution stays local. Tracked debt.
- Verify VALID_TYPES fix before Phase 1 (test POST with `type='action'`)
- Review mode Phase 3 SKIPPED entirely if `executeLlmStep` still 501
- `derived_from` links only for assets published to Wire (omit for local-only)

### Verification checklist
- [ ] Planner action published to Wire → UUID in config
- [ ] Execute plan with publish ON → chain appears on Wire → visible in Tools
- [ ] Execute plan with publish OFF → no contribution
- [ ] (If review mode) Cost preview shows governor-adjusted costs
- [ ] Published chain shows usage/revenue in Tools tab
- [ ] `derived_from` links point to correct published assets

---

## Sprint 3 — Remote Pyramid Access

**Plan:** `sprint-3-remote-pyramids.md`
**Audited:** 1 cycle, corrections applied

### Phases

| Phase | What | Size | Depends on |
|-------|------|------|-----------|
| 1 | Pyramid query token (EXISTS — verify) | Small | — |
| 2 | Payment flow (stamp + access) | Medium | P1 |
| 3 | Serve remote queries (JWT auth) | Medium | P2 |
| 4 | Query remote pyramids | Medium | P2 + P3 |
| 5 | Frontend UX (discovery, badges, planner) | Medium | P4 |

### Key decisions from audit
- Query token endpoint is **POST** (not GET) with body `{ slug, query_type, target_node_id }`
- One payment-intent for total (stamp + access); Wire splits internally via rotator arm
- Phase ordering: Payment (P2) before remote query (P4)
- Sprint 5 auth gap flagged: `wire:node` scope required, no-node users need virtual agent

### Verification checklist
- [ ] User A publishes pyramid → discoverable in Wire search
- [ ] User B queries User A's pyramid → payment flows → answer received
- [ ] Planner suggests remote pyramid with cost comparison
- [ ] Remote pyramid shows "remote" badge in Understanding
- [ ] Stamp payment reaches serving node's credit pool

---

## Sprint 4 — Auto-Fulfill (ALPHA MILESTONE)

**Plan:** `sprint-4-auto-fulfill.md`
**Audited:** 1 cycle, corrections applied

> ⚠️ **Sprint 4 is the largest and most uncertain sprint.** Helper pool is entirely new infrastructure. Phase 1 may be a multi-sprint effort.

### Phases

| Phase | What | Size | Depends on |
|-------|------|------|-----------|
| 1 | Wire server helper pool | LARGE | — |
| 2 | Auto-fulfill toggle | Small | P1 |
| 3 | Remote chain execution | Medium | P1 + P2 |
| 4 | UX polish | Small | P3 |

### Key decisions from audit
- Helpers use same public API as any agent (Pillar 25 — no internal-only APIs)
- Auth: `wireApiCall` (agent token with `wire:contribute` scope)
- Sprint 4 = no-agent execution for users WITH a node. No-node = Sprint 5.
- Progress: polling `GET /api/v1/wire/action/chain/${chainId}/status` every 5s
- Helper fee: 10% of estimated cost, minimum 1 credit
- New files: `helper-pool.ts`, `wire/helper/` directory, `wire_helper_assignments` migration

### Alpha acceptance criteria
- [ ] User with no agents types question → helper builds pyramid → result in Understanding
- [ ] User with fleet, auto-fulfill OFF → tasks queue → agents claim
- [ ] Helper fee shown in cost preview before approval
- [ ] Cancel button works mid-execution
- [ ] Cost breakdown: helper fee + access costs shown separately

---

## Sprint 5 — Vibesmithy Connection

**Plan:** `sprint-5-vibesmithy-connection.md`
**Audited:** 1 cycle, corrections applied

### Phases

| Phase | What | Size | Depends on |
|-------|------|------|-----------|
| 0 | Virtual agent for operator sessions | Medium | — |
| 1 | Vibesmithy Wire auth | Medium | P0 |
| 2 | Vibesmithy intent bar | Medium | P1 |
| 3 | Vibesmithy ↔ node connection | Medium | P1 |
| 4 | Dennis triggers chains | Medium | P2 + P3 |
| 5 | Published pyramid browser | Small | P3 |

### Key decisions from audit
- **Virtual agent**: Wire server auto-provisions a minimal agent identity for operator sessions without nodes. Same public API, no Pillar 25 violation.
- **Node detection**: device-auth pairing via Wire (NOT localhost:8765/health — CORS blocks it)
- **Shared code**: `@wire/plan-widgets` npm package shared between Wire Node and Vibesmithy
- **Dennis Pillar 23**: all Dennis-initiated intents go through full preview-then-commit, no auto-approval
- **Degraded mode**: Sprint 5 viable without helpers — discovery + query work, new pyramid builds don't

### Verification checklist
- [ ] No-node user: login → type question → planner → approve → helper builds → explore in space
- [ ] With-node user: Vibesmithy detects node via device-auth → local queries (free, fast)
- [ ] Dennis suggests expansion → intent → plan preview → approve → pyramid updates live
- [ ] Published pyramid browsable in spatial view
- [ ] Credits charged per remote query

---

## Execution Playbook

For each sprint:

1. **Read the plan** in `v2-alpha-sprints/sprint-N-*.md`
2. **Verify prerequisites** — check that prior sprint shipped and parallel workstreams landed
3. **Run wire-rules pillar check** against the plan (may have drifted since last audit if Wire server changed)
4. **Implement phases** in the order specified, parallelizing where the plan allows
5. **Serial verifier** after each phase (Pillar 39)
6. **Post-implementation audit** — informed + discovery pair
7. **Build release app** — `cargo tauri build` from the Wire Node directory
8. **Test against verification checklist** above
9. **Save the plan** with completion status updated

### Key files (Wire Node codebase)

| Category | Files |
|----------|-------|
| Auth infrastructure | `src-tauri/src/main.rs` (wireApiCall, operatorApiCall, planner_call) |
| Frontend context | `src/contexts/AppContext.tsx` (Mode type, state, actions) |
| Sidebar | `src/components/Sidebar.tsx` (live status) |
| Intent bar | `src/components/IntentBar.tsx` |
| Widget catalog | `src/config/widget-catalog.ts`, `src/components/planner/PlanWidgets.tsx` |
| Tool config | `src/config/wire-actions.ts` |
| Planner prompt | `chains/prompts/planner/planner-system.md` |
| Mode components | `src/components/modes/*.tsx` |
| Fleet components | `src/components/fleet/*.tsx` |
| Stewardship | `src/components/stewardship/*.tsx` |
| CSS | `src/styles/dashboard.css` (15K+ lines, section-organized) |
| Rust pyramid engine | `src-tauri/src/pyramid/` (llm.rs, chain_executor.rs, build_runner.rs) |

### Key files (Wire server)

| Category | Files |
|----------|-------|
| Auth | `src/lib/server/wire-auth.ts`, `src/lib/server/operator-auth.ts` |
| Action compiler | `src/lib/server/wire-compiler.ts` |
| Contribute | `src/lib/server/contribute-core.ts` (VALID_TYPES) |
| Query pricing | `src/lib/server/surge-engine.ts` (governor) |
| API routes | `src/app/api/v1/wire/` (all endpoints) |
| Operator routes | `src/app/api/v1/operator/` |

### Wire Development Pillars

The pillars at `GoodNewsEveryone/docs/wire-pillars.md` are non-negotiable. Key ones for this work:
- **Pillar 1**: No production DELETE. Supersede, don't destroy.
- **Pillar 2**: Everything is a contribution (plans, prompts, chains).
- **Pillar 17**: Chains invoke chains. The planner must eventually be a chain.
- **Pillar 23**: Preview-then-commit. Show cost before execution.
- **Pillar 25**: Platform agents use the public API. No shortcuts.
- **Pillar 28**: The pyramid recipe is a contribution. Prompts are improvable.
- **Pillar 37**: Never prescribe outputs to intelligence.
- **Pillar 39**: Serial verifier after implementation.

---

## Session History

This plan stack was produced in a single session (2026-03-30) that also shipped v1.0 + v1.1 + Sprint 0:
- 24 implementation phases executed
- 20+ plan auditors across 4 plan audit cycles per sprint
- 25 serial verifiers (3 caught real bugs: wrong pseudoId, XSS in markdown, missing save_session)
- 3 post-implementation audits
- ~90 individual findings caught and fixed
- Session state saved at `~/.claude/projects/-Users-adamlevine-AI-Project-Files/memory/project_v2_session_state.md`
