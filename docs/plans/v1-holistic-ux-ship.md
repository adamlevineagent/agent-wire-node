# Wire Node v1 — Complete UX Ship

## Context

Wire Node desktop app is feature-complete in pieces but doesn't tell a coherent story. This plan makes everything ship: Search, Market, Agent Fleet, Warroom-equivalent, all bug fixes, all UX passes. Nothing is deferred.

## Who is the v1 user?

**Primary:** A knowledge worker or developer who builds pyramids from local data, publishes to the Wire, discovers others' work, earns credits, and manages their agent fleet — all from one desktop app.

---

## Phase 0: Auth Infrastructure (prerequisite — blocks Phases 1-3, 4b-iii, 4c, 5, 6)

**Problem (CRITICAL — found by both Stage 1 auditors independently):**
`operatorApiCall()` sends the operator session token as `Bearer`. But most Wire endpoints use `requireWireScope()` which expects a `gne_live_*` agent API token. Only dual-auth endpoints (`requireOperatorOrAgentAuth`) accept operator session tokens.

| Auth type needed | Endpoints | operatorApiCall works? |
|---|---|---|
| `requireWireScope` (agent only) | pulse, query, entities, topics, roster, tasks, reputation, **contribution/[id]**, **contribution/[id]/neighborhood**, **my/earnings**, mesh/*, requests/pending | **NO** |
| `requireOperatorOrAgentAuth` (dual) | notifications, handles, contributions/human, requests | YES |
| No auth | feed, agents (public), handles/check | YES |

The existing Rust codebase already has `get_api_token()` which returns the `gne_live_*` token (used by corpora, publish flows). The frontend just can't reach it for general API calls.

**Fix:**
1. Add a new Tauri command `wire_api_call(method, path, body, headers)` that uses `get_api_token()` (the `gne_live_*` agent token). The `headers` param is a `HashMap<String, String>` for custom headers — **required for mesh endpoints** which need `X-Wire-Thread: <thread-id>`. The command must also read `config.api_url` for the base URL (same as `operator_api_call` does).
2. **Register `wire_api_call` in the `generate_handler![]` block** at `main.rs:6033` — Tauri commands silently fail at runtime if not registered (no compile-time error)
3. Add a frontend wrapper `wireApiCall(method, path, body?, headers?)` that invokes it. **Add `wireApiCall` to the `AppContextValue` interface** (line 148) and Provider value object (line 198-207) — currently only `operatorApiCall` is exposed.
4. New Wire-scoped endpoint calls use `wireApiCall`; existing operator-scoped calls keep `operatorApiCall`
5. **Fresh-install handling:** `get_api_token()` returns `Err` when no agent is registered yet (`api_token` is `None`). The `wire_api_call` command must detect this and attempt `register_with_session` (using the Supabase `access_token`) before failing. If registration also fails (no Supabase session), surface "Wire agent not registered — please log in first." Frontend should show a "Not connected to Wire" state on wireApiCall failure, not generic errors.
6. **401 retry — different strategies per auth path:**
   - `operator_api_call`: on 401, call `try_acquire_operator_session` to refresh the operator session token, retry once
   - `wire_api_call`: on 401, attempt `register_with_session` to get a fresh `api_token` (the `gne_live_*` token is long-lived, so 401 likely means it was revoked or never issued, NOT that it expired). If registration fails because Supabase session expired, refresh via `auth::refresh_session`, then re-register. **CRITICAL: `refresh_session` returns `(access_token, refresh_token)` but does NOT mutate AuthState. After calling it, acquire `auth.write()` and update `auth.access_token = Some(new_access.clone())` and `auth.refresh_token = Some(new_refresh)` BEFORE calling `register_with_session`. Also call `save_session` to persist the refreshed tokens to disk.** Do NOT call `try_acquire_operator_session` — that refreshes the wrong token.
   - `operator_api_call`: **proactive expiry check** — before making the request, check `operator_session_expires_at`. If within 5 minutes of now, proactively call `try_acquire_operator_session` first. This prevents users from ever seeing a 401 from operator endpoints.
   - **Fix `operator_api_call` response parsing:** currently calls `resp.json()` before checking status code. Non-JSON error responses (nginx 502, raw 401) produce misleading parse errors. Check `resp.status()` first, use `resp.text()` as fallback on error codes. Return structured errors that include HTTP status so retry logic can pattern-match on 401.
7. **Fix existing broken call:** DashboardMode line 30 calls `operatorApiCall('GET', '/api/v1/wire/requests/pending')` but this endpoint uses `requireWireScope` — silently failing today. Switch to `wireApiCall`. **Also update the guard at line 29** — currently checks `state.operatorSessionToken` but `wireApiCall` uses `api_token`, not operator session. Either remove the guard (let wireApiCall handle auth errors) or add a Wire auth readiness check.
   - Note: DashboardMode line 42 `corpora?owner=me` correctly uses `operatorApiCall` (dual-auth endpoint) — do NOT migrate.
8. **Smoke test:** After implementing, verify by calling `wireApiCall('GET', '/api/v1/wire/pulse')` and confirming 200 response. Phase 0 is the critical path for 8 downstream phases — must be verified before proceeding.

**Files:**
- `src-tauri/src/main.rs` — add `wire_api_call` command, register in `generate_handler![]` (line 6033), add 401 retry to `operator_api_call`, fix response parsing
- `src/contexts/AppContext.tsx` — add `wireApiCall` wrapper alongside `operatorApiCall`
- `src/components/modes/DashboardMode.tsx` — fix broken `requests/pending` call

---

## v1 Navigation Structure

**Current (10 tabs):** Pyramids, Network, Search, Warroom, Compose, Agents, Node, Activity, Identity, Settings

**v1 (9 tabs) — restructured:**

| # | Tab | What ships |
|---|-----|-----------|
| 1 | **Pyramids** | Unchanged (Command Center redesign already landed) |
| 2 | **Network** | Existing dashboard + pulse data (fleet online, active tasks, circles) |
| 3 | **Search** | Wire search: feed browse (free), paid query, entity/topic discovery |
| 4 | **Compose** | UX pass: markdown preview, contribution type help, target context, success state |
| 5 | **Fleet** | Rename from "Agents". Agent roster (2a), mesh coordination + tasks (2b), corpora |
| 6 | **Node** | Sync + Remote + Market + Logs |
| 7 | **Activity** | Expandable notification detail, full contribution body/citation, navigate-to-source, circle messages (read-only) |
| 8 | **Identity** | Bug fixes + live reputation fetch + transaction history |
| 9 | **Settings** | Unchanged |

**Removed:** Warroom (monitoring folds into Network via pulse endpoint)

---

## Phase 1: Search Mode (new feature)

**Wire endpoints (auth type noted):**
- `GET /api/v1/wire/query` — full-text search (`wire:query` scope — use `wireApiCall`) **COSTS 100+ CREDITS**
- `GET /api/v1/wire/feed` — public feed (no auth — free)
- `GET /api/v1/wire/entities` — entity browse (`wire:read` — use `wireApiCall`)
- `GET /api/v1/wire/topics` — topic index (`wire:read` — use `wireApiCall`)
- `GET /api/v1/wire/contribution/[id]` — full contribution access (`wire:read` — use `wireApiCall`)

**Implementation:**
Replace `SearchMode.tsx` placeholder with full search UI:

1. **Default tab: Feed** (free) — browse new/popular/trending contributions at zero cost
2. **Search tab** — text input with submit, maps to `/api/v1/wire/query`
   - **Cost disclosure (Pillar 23 — full compliance):** Add `GET /api/v1/wire/query/preview` endpoint to Wire server (~30 lines — reads surge multiplier from the same function the query endpoint uses, returns `{ base_cost: 100, surge_multiplier, estimated_cost }` without debiting). Search UI calls this on focus/before submit and displays "Estimated cost: X credits (Surge: Y×)" with current balance. Confirmation dialog when cost > 200 credits.
3. **Filter bar** — topics, contribution type, significance range, price range, sort order, date range
4. **Results list** — new `ContributionCard` component (does NOT exist yet — must be created from scratch) showing: title, teaser, author pseudonym, topics, price, ratings, timestamp
5. **Pagination** — load-more / infinite scroll for results, entities, topics (Wire query supports `limit`/`offset`, max 100/page, max offset 10000)
6. **Click to expand** — show full contribution body (calls `/api/v1/wire/contribution/[id]`, handles purchase confirmation if priced)
7. **Browse tabs** — Feed (default, free) / Search Results / Entities / Topics
8. **Entity detail** — click entity shows mentions, aliases, related contributions
9. **Topic detail** — click topic shows contributions tagged with it

**Credit balance sync:** Sidebar reads `creditBalance` from `AppState` (set via `SET_CREDITS` action with a `CreditStats` object from the Rust `get_credits` command). Wire-side purchases (search, contribution access) won't reflect until the next poll. After a search or purchase, call `wireApiCall('GET', '/api/v1/wire/my/earnings')` which returns `{ current_balance, recent_transactions, ... }`. Bridge the shape mismatch: add a `SET_CREDIT_BALANCE` action to `AppContext` that updates `creditBalance` directly from `my/earnings.current_balance`, without requiring the full `CreditStats` shape.

**Error states:**
- 402 (insufficient credits) → show balance + "Earn credits" guidance
- 429 (rate limited) → show retry-after countdown
- 401 (auth expired) → redirect to re-auth flow
- Generic error → message + retry button

**Files:**
- `src/components/modes/SearchMode.tsx` — rewrite from placeholder
- New: `src/components/search/ContributionCard.tsx` — reusable card (also used by Phase 5 Activity)
- New: `src/components/search/EntityBrowser.tsx` — entity exploration
- `wireApiCall` for query/entities/topics/contribution access (all `wire:read`)
- `src/contexts/AppContext.tsx` — add `SET_CREDIT_BALANCE` action to `AppAction` union and reducer: `{ type: 'SET_CREDIT_BALANCE'; balance: number }` → `return { ...state, creditBalance: action.balance }`
- **Wire server (cross-repo — deploy BEFORE testing Search):** New `GoodNewsEveryone/src/app/api/v1/wire/query/preview/route.ts` — `GET /api/v1/wire/query/preview` returns `{ base_cost, surge_multiplier, estimated_cost }` without debiting. Uses `requireWireScope('wire:query')` (matching the actual query endpoint's scope — consistency ensures only agents who can search can preview costs). Reads surge from `getSurgeMultiplier()` in `surge-engine.ts` (same function `query/route.ts` imports).

---

## Phase 2: Fleet Mode (rename + implement)

**Wire endpoints:**
- `GET /api/v1/wire/agents` — list all agents (public, no auth)
- `GET /api/v1/wire/agents/resolve/[pseudoId]` — resolve agent details
- `GET /api/v1/wire/reputation/[pseudoId]` — agent reputation (`wire:read` — use `wireApiCall`)
- `GET /api/v1/wire/pulse` — fleet_online (`wire:read` — use `wireApiCall`)
- `GET /api/v1/wire/tasks` — task management (`wire:read`/`wire:contribute` — use `wireApiCall`)
- `GET /api/v1/wire/roster` — agent roster (`wire:read` — use `wireApiCall`)

**Mesh endpoints exist at `/api/v1/mesh/*`** (NOT `/api/v1/wire/mesh/` — top-level, not under `/wire/`):
- `GET /api/v1/mesh/status` — returns `active_threads`, `threads[]`, `intents[]`, `board{}` (`wire:read` — use `wireApiCall`)
- `GET/POST /api/v1/mesh/board` — read/write shared blackboard (`wire:read`/`wire:contribute` — use `wireApiCall`)
- `GET/POST /api/v1/mesh/intent` — get/declare intents (`wire:read`/`wire:contribute` — use `wireApiCall`)
- All require `X-Wire-Thread` header for thread tracking

**Implementation — split into Phase 2a and Phase 2b:**

### Phase 2a: Fleet infrastructure + Fleet Overview + Corpora migration

The atomic mode rename, sub-tab routing infrastructure, Fleet Overview, and Corpora migration. This gives the user an immediate "Fleet works" experience.

1. **Atomic Mode type change** — replace `'agents'` with `'fleet'` in `Mode` type union and `ALL_MODES` in `AppContext.tsx`. Update ALL consumers in one atomic change. If `'agents'` is removed but any component still references it, TypeScript errors. All changes below ship together.
2. **Sub-tab routing** — FleetMode needs a local state tab selector (like NodeMode's `useState<NodeTab>`) with tabs. Phase 2a ships with 2 tabs: Fleet Overview + Corpora. Phase 2b adds Mesh + Tasks.
3. **Fleet Overview sub-tab** — agents from roster endpoint, online status from pulse `fleet_online` array, per-agent contribution count and reputation
4. **Corpora sub-tab** — existing `CorporaList`/`CorpusDetail`/`DocumentDetail` (already works, keep as-is)

**Error states:** Same pattern as Phase 1 (402/429/401/generic).

**Files (2a):**
- `src/contexts/AppContext.tsx` — replace `'agents'` with `'fleet'` in `Mode` type and `ALL_MODES`
- `src/components/modes/AgentsMode.tsx` → rename to `FleetMode.tsx`, update `currentView('agents')` → `currentView('fleet')`, add sub-tab routing
- `src/components/Sidebar.tsx` — rename tab label + icon, `'agents'` → `'fleet'`
- `src/components/ModeRouter.tsx` — update route case `'agents'` → `'fleet'`
- `src/components/stewardship/CorporaList.tsx` — line 41: `pushView('agents', ...)` → `pushView('fleet', ...)`
- `src/components/stewardship/CorpusDetail.tsx` — lines 83-84: `popView`/`pushView` `'agents'` → `'fleet'`
- `src/components/stewardship/DocumentDetail.tsx` — line 45: `popView('agents')` → `popView('fleet')`
- `src/components/stewardship/CurationQueue.tsx` — line 74: `pushView('agents', ...)` → `pushView('fleet', ...)`
- New: `src/components/fleet/FleetOverview.tsx` — agent roster + online status
- `src/styles/dashboard.css` — rename `.agents-layout` → `.fleet-layout`, `.agents-section` → `.fleet-section` (lines 4908-4919), update section header comment. New Fleet styles use `.fleet-*` prefix.
- Note: introduces `fleet/` sub-directory convention

### Phase 2b: Mesh + Tasks

Coordination features that require sibling agents to be running. Ships after 2a.

1. **Mesh sub-tab** — blackboard state (`/mesh/board`), active intents (`/mesh/intent`), sibling threads (`/mesh/status`). Read/write blackboard keys, declare/release intents. **Mesh POST endpoints require `X-Wire-Thread` header** — pass via `wireApiCall`'s headers param. Add confirmation dialog before board writes and intent declarations (actions visible to sibling agents).
2. **Tasks sub-tab** — task board (backlog/claimed/active/review), claim/update tasks via `/api/v1/wire/tasks`. **Task response is wrapped in `wireEnvelopeWithTasks`** — task data is at `response.data.tasks`, summary counts at `response.tasks`. Note: task operations execute under the agent's identity (via `wireApiCall`), not the operator's. This is correct for Fleet context — the operator is directing their agents' work.

**Error states:** Same pattern as Phase 1 (402/429/401/generic).

**Files (2b):**
- `src/components/modes/FleetMode.tsx` — add Mesh + Tasks tabs to existing sub-tab routing
- New: `src/components/fleet/MeshPanel.tsx` — blackboard + intents (use `.fleet-mesh-*` CSS prefix — existing `.mesh-option*` classes at dashboard.css:3826 will collide with bare `.mesh-*` names)
- New: `src/components/fleet/TaskBoard.tsx` — task management
- **Register new Tauri commands** needed for Fleet in `generate_handler![]` if any are added

---

## Phase 3: Network + Pulse (Warroom replacement)

**Wire endpoint:**
- `GET /api/v1/wire/pulse` — single-call dashboard (`wire:read` — use `wireApiCall`)

**Actual pulse response shape (verified against source):**
```json
{
  "unread_messages": number,
  "active_tasks": [{ "id", "title", "status", "priority", "created_at" }],
  "unread_notifications": [{ "id", "event_type", "source_contribution_id", "source_agent_pseudonym", "created_at" }],
  "fleet_online": [{ "id", "name", "last_seen_at" }],
  "circle_activity": [{ "circle_id", "circle_name", "unread_count" }],
  "cost": 0
}
```

**Implementation:**
Enhance `DashboardMode.tsx` to incorporate pulse data:

1. **Fleet presence card** — show `fleet_online` agents with name + last_seen_at
2. **Active tasks card** — show `active_tasks` with title, status, priority
3. **Circle activity card** — show `circle_activity` with circle name + unread count
4. **Unread messages badge** — show `unread_messages` count
5. **Unread notifications** — show `unread_notifications` inline (new field, not in original plan)
6. Remove Warroom tab entirely
7. **Fix DashboardMode navigation targets** — line 82 `setMode('warroom')` (Curation Queue button) and line 132 `setMode('warroom')` (Corpora card) will break when warroom is removed from Mode type. Route Corpora card to `'fleet'`. **Remove the Curation Queue button** — CurationQueue is a component inside CorpusDetail that needs a corpus slug context; a dashboard-level button can't provide that.
8. **Fix DashboardMode agents reference** — line 96 `setMode('agents')` must become `setMode('fleet')` after Phase 2 rename.

**Files:**
- `src/components/modes/DashboardMode.tsx` — add pulse data cards, fix all `setMode('warroom')` and `setMode('agents')` calls
- `src/components/modes/WarroomMode.tsx` — DELETE
- `src/components/Sidebar.tsx` — remove Warroom entry
- `src/components/ModeRouter.tsx` — remove warroom case
- `src/contexts/AppContext.tsx` — remove `'warroom'` from Mode type and ALL_MODES

---

## Phase 4: Bug Fixes

### 4a. Chain executor silent death

**Root cause:** `tokio::spawn` at `main.rs:3012` runs the build. The `JoinHandle` is never stored or awaited (fire-and-forget). If the spawned task panics, the panic is caught by tokio's runtime but the status update (line 3117) never executes. Status stays "running" forever. User can't retry (line 2973 check blocks on non-terminal status).

**Fix (Rust) — JoinHandle monitoring, NOT catch_unwind:**
`std::panic::catch_unwind` does NOT work across `.await` points in async Rust. The correct approach:

1. Store the `JoinHandle` from `tokio::spawn` at line 3012
2. Spawn a monitoring task that awaits the handle and checks the result:
```rust
let build_handle = tokio::spawn(async move { /* existing build code */ });
let monitor_status = status.clone();
tokio::spawn(async move {
    if let Err(e) = build_handle.await {
        tracing::error!("Build task panicked: {e:?}");
        let mut s = monitor_status.write().await;
        if s.status == "running" {
            s.status = "failed".to_string();
        }
    }
});
```
3. Add a "force cancel" command: if status is "running" but `started_at` > 30 minutes, allow reset (belt-and-suspenders)
4. **`pyramid_build_cancel` already exists** at line 4399, registered at line 6085. Do NOT create a duplicate. Frontend cancel button calls `invoke('pyramid_build_cancel', { slug })`.
5. Frontend: add Cancel button (calls `pyramid_build_cancel`) when running, Retry button when failed
6. Add `tracing::error!` with panic payload in the monitor to identify the actual panic source (the "l0_webbing JSON parse failure" is a hypothesis, not confirmed)
7. **Fix ALL build spawn paths — three total:**
   - `pyramid_build` (line 3012) — mechanical pyramids. Fire-and-forget `tokio::spawn`.
   - `pyramid_question_build` (line 3678) — question pyramids. Same fire-and-forget pattern.
   - `pyramid_vine_build` (line 4489) — vine builds. Same pattern.
   All three need JoinHandle monitoring. All three need cancel support (mechanical already has `pyramid_build_cancel`; question and vine builds need equivalent cancel wiring if not present).

**Files:**
- `src-tauri/src/main.rs` — lines 3012-3169 (pyramid build), 3678+ (question build), 4489-4503 (vine build) — JoinHandle monitoring on all three
- `src/components/PyramidDashboard.tsx` — add cancel/retry buttons (call existing `pyramid_build_cancel`)

### 4b. Identity display fixes

**4b-i. Reputation `\u2014` display:**
- `IdentityMode.tsx:151` has `\u2014` as raw JSX text (NOT in a `{}` expression). Verified: line 135 uses `{state.email || '\u2014'}` (correct), but line 151 uses bare `\u2014` (broken — renders as literal characters).
- Fix: change to `{'\u2014'}` or actual Unicode em dash character.

**4b-ii. Handle `@@` prefix:**
- `IdentityMode.tsx:178` does `@{currentHandle.handle.replace(/^@/, '')}`. The `handle` field from the Wire is bare (no `@`), but `display_handle` has `@`. If any code path uses `display_handle` instead of `handle`, the `@` gets doubled.
- Fix: change to `.replace(/^@+/, '')` as defensive measure against any `@`-prefixed value.

**4b-iii. Redundant balance:**
- Credit balance shows in both sidebar identity badge AND Identity page info grid (line 146-148).
- Fix: Remove the balance card from the Identity info grid. Replace with transaction history from `GET /api/v1/wire/my/earnings` (`wire:read` — use `wireApiCall`, NOT operatorApiCall). Depends on Phase 0.
- **Transaction history UI:** Simple list of recent transactions showing: amount (+/-), type (earned/spent), timestamp, related contribution title (if available). Use `my/earnings` endpoint's `limit`/`offset` for pagination. No date range filter — keep minimal for v1.

**4b-iv. HandleInfo type mismatch:**
- `HandleInfo` interface (line 6-14) has `layaway_active`, `layaway_progress`, `layaway_paid`, `layaway_total`, `registered_at` — none of these are returned by the server. Server returns `id`, `handle`, `display_handle`, `payment_type`, `status`, `created_at`. The layaway progress bar UI (lines 187-208) is dead code.
- Fix: Update `HandleInfo` to match actual server response shape. Remove dead layaway progress UI or wire it to actual data if the server supports it.

**Files:**
- `src/components/modes/IdentityMode.tsx` — all three fixes

### 4c. Reputation fetch (separate workstream — Pillar 40)

**Fix:**
1. Call `GET /api/v1/wire/reputation/[pseudoId]` (`wire:read` — use `wireApiCall`) to fetch real reputation data
2. Display `global_score` and domain-specific breakdown
3. Fallback to em dash when no reputation data exists

**Files:**
- `src/components/modes/IdentityMode.tsx` — add reputation fetch + display

### 4d. Handle persistence on restart (separate workstream — Pillar 40)

**Root cause:** Handles fetched from Wire API on mount. Persistence bug is likely auth token expiring on restart, causing empty response.

**Fix (Pillar 2 compliant — cache the Wire contribution, not a separate table):**
1. Add logging to handle fetch to see what's returned
2. Check auth validity before handle fetch (refresh if needed)
3. Cache full Wire response JSON locally (same shape — `id`, `handle`, `display_handle`, `operator_id`, `payment_type`, `status`, `created_at`). Store in existing local store, NOT a separate schema.
4. On startup, display cached handle data immediately, refresh from Wire in background
5. If Wire fetch fails, show cached data with "sync pending" indicator. **UI detail:** Subtle "syncing..." badge next to handle display while background refresh is in-flight. On failure: "cached — last synced [timestamp]" in muted text below handle.

**Files:**
- `src/components/modes/IdentityMode.tsx` — cache-first display + background refresh
- `src-tauri/src/main.rs` — add `cache_wire_handles` / `get_cached_wire_handles` commands, **register both in `generate_handler![]` (line 6033)**
- Existing SQLite local store — cache Wire handle response

---

## Phase 5: Activity Enrichment

**Wire endpoints:**
- `GET /api/v1/wire/contribution/[id]` — full contribution body (`wire:read` — use `wireApiCall`)
- `GET /api/v1/wire/contribution/[id]/neighborhood` — related contributions (`wire:read` — use `wireApiCall`)

**Implementation:**
1. **Expandable rows** — click notification row to expand inline showing:
   - Full contribution body (fetched via contribution endpoint)
   - Source pyramid and layer context
   - Author pseudonym + reputation score
   - Rating controls (accuracy/usefulness)
   - "Navigate to source" button
2. Keep existing filter bar (type, source, time)
3. **Extract a generic `SlideOverPanel` wrapper** from `PyramidDetailDrawer` (container, escape-key close, scroll-to-top) — `PyramidDetailDrawer` itself is pyramid-specific (826 lines, 13 pyramid callbacks). Reuse the CSS pattern and behavior, not the component.
4. Reuse `ContributionCard` from Phase 1 for expanded detail view
5. **Fix notification count source:** `AppShell.tsx` line 81 polls `invoke('get_messages')` which fetches Wire **messages** (DMs, circle messages). `ActivityMode.tsx` line 113 fetches Wire **notifications** (contribution events, ratings). These are **different data** that both write to the same badge state. The fix is NOT to merge them but to give them separate state: messages count and notifications count, displayed separately (or summed intentionally). The current code conflates two distinct concepts.
6. **Circle messages (read-only)** — Network pulse shows `circle_activity` with unread counts, but there's no way to read circle messages. Add a "Messages" section to Activity (below notifications) showing Wire messages.
   - `GET /api/v1/wire/messages` (`wire:read` — use `wireApiCall`) — read messages. **Response is wrapped in `wireEnvelopeWithInbox`** — messages at `response.data.messages`, inbox metadata at `response.inbox` (`{ unread, from, latest_at }` — use for unread badge).
   - `POST /api/v1/wire/messages` with `{ action: "read", ids: string[] }` (`wire:read` — use `wireApiCall`) — mark as read. Despite being POST, only requires `wire:read` scope (not `wire:contribute`).
   - Read-only for v1 — view messages, mark as read. No compose/reply (that requires circle context and can come later). This makes the pulse circle_activity counts actionable.

**Error states:** Same pattern (402/429/401/generic).

**Files:**
- `src/components/modes/ActivityMode.tsx` — add expandable detail view
- `src/components/AppShell.tsx` — separate messages count from notifications count in badge state
- `src/contexts/AppContext.tsx` — may need separate state fields for message count vs notification count
- New: `src/components/common/SlideOverPanel.tsx` — generic slide-over extracted from drawer pattern
- Reuse: `src/components/search/ContributionCard.tsx` from Phase 1

---

## Phase 6: Compose UX Pass

**Implementation:**
1. **Markdown preview** — toggle between edit and preview, use simple markdown renderer
2. **Contribution type help** — tooltip/description for each type (analysis, commentary, correction, etc.)
3. **Target context** — when replying (target contribution ID set), fetch and show the target contribution title + teaser
4. **Post-submit success state** — confirmation card with contribution ID, "View in Activity" link
5. **Wire topics autocomplete** — fetch from `GET /api/v1/wire/topics` (`wire:read` — use `wireApiCall`)

**Auth split note:** ComposeMode uses TWO auth paths. Existing `contributions/human` POST and `requests` POST use `operatorApiCall` (operator-only auth — keep as-is). Only topics autocomplete uses `wireApiCall`. Do NOT migrate the existing calls — `contributions/human` explicitly requires `auth.type === 'operator'`.

**Files:**
- `src/components/modes/ComposeMode.tsx` — all changes (needs both `operatorApiCall` and `wireApiCall` from context)

---

## Phase 7: Node Mode — Keep Market

Market tab already fully implemented (`MarketView.tsx` + `market.rs`). Keep it.

**Sub-tabs:** Sync, Market, Remote, Logs (unchanged)

No changes needed.

---

## Phase 4e. Security hardening (Pillar 38: fix all bugs when found)

**4e-i. CSP disabled (CRITICAL):**
- `tauri.conf.json:29` has `"csp": null` — no Content Security Policy. Combined with renderer-level `fetch()` calls that send API keys (PyramidSettings line 68 sends OpenRouter key directly), any XSS vector can exfiltrate secrets.
- Fix: Set restrictive CSP: `"default-src 'self'; connect-src 'self' https://openrouter.ai https://newsbleach.com https://supabase.newsbleach.com http://localhost:* https://localhost:*; style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; font-src https://fonts.gstatic.com"`

**4e-ii. OpenRouter API key leaked through renderer:**
- `PyramidSettings.tsx:67-75` — "Test Key" button makes direct `fetch()` from webview to OpenRouter with API key in Authorization header. Should go through Tauri IPC instead.
- Fix: Create `pyramid_test_api_key` Tauri command that tests server-side, register in `generate_handler![]`.

**4e-iii. Updater pubkey empty:**
- `tauri.conf.json:46` has `"pubkey": ""` — updates can't be verified or are accepted without verification.
- Fix: Generate updater keypair, set public key, sign releases.

**4e-iv. RemoteConnectionStatus fetches arbitrary user-supplied URL from renderer:**
- Line 67-93 — "Test Remote Connection" sends GET to `${userInput}/health` from renderer. With CSP null, injected content could trigger this.
- Fix: Route through Tauri IPC command.

**Files:**
- `src-tauri/tauri.conf.json` — CSP policy, updater pubkey
- `src/components/PyramidSettings.tsx` — move API key test to IPC
- `src/components/RemoteConnectionStatus.tsx` — move connection test to IPC
- `src-tauri/src/main.rs` — add `pyramid_test_api_key` + `test_remote_connection` commands, register in `generate_handler![]`

---

## Phase 4f. Pre-existing UI bugs (Pillar 38)

**4f-i. `get_wire_identity_status` command doesn't exist:**
- `RemoteConnectionStatus.tsx:44` calls `invoke("get_wire_identity_status")` — no such Rust command. Silently fails, Wire Identity always shows "Not Set".
- Fix: Implement command (check auth state for valid API token), register in `generate_handler![]`.

**4f-ii. RemoteConnectionStatus `queryStats` never populated:**
- Lines 31-36 — state initialized to zeros, rendered in UI, never updated. User sees permanent zeros.
- Fix: Wire up IPC call to fetch actual stats, or remove the section.

**4f-iii. `handleVerifyLink` doesn't refresh auth state:**
- `App.tsx:60-62` — after magic link verify, doesn't call `get_auth_state`/`setAuthState` (unlike OTP and login flows). User stays on login screen after successful magic link verification.
- Fix: Add `const state = await invoke<AuthState>("get_auth_state"); setAuthState(state);` after line 61.

**4f-iv. SyncStatus mutates prop directly:**
- `SyncStatus.tsx:165-167` — `doc.document_status = "published"` mutates AppContext state directly (React immutability violation). Won't trigger re-render, can cause stale reads.
- Fix: Remove direct mutation. `onSync()` call on line 169 already refreshes state.

**Files (4f):**
- `src/App.tsx` — magic link auth refresh (4f-iii)
- `src/components/RemoteConnectionStatus.tsx` — wire identity command, query stats (4f-i, 4f-ii)
- `src/components/SyncStatus.tsx` — remove prop mutation (4f-iv)
- `src-tauri/src/main.rs` — `get_wire_identity_status` command (4f-i)

---

## Phase 4h. Dev ergonomics + cleanup (Pillar 38/40 — separate focused task)

**4h-i. `get_logs` blocks async runtime (was 4f-v):**
- `main.rs:2457-2463` — `std::fs::read_to_string()` (blocking I/O) inside async command. With unbounded log file + 2s polling, periodically stalls Tokio worker.
- Fix: Use `tokio::fs::read_to_string` or `spawn_blocking`. Consider reading only last N bytes.

**4h-ii. ActivityFeed duplicates credits poll:**
- `ActivityFeed.tsx:38` polls `get_credits` every 3s independently of AppShell's 2s poll. Same data, ~5 IPC calls per 6 seconds.
- Fix: Consume credits from props/context instead of independent poll.

**4h-iii. Dead code: `register_wire_node`:**
- `auth.rs:308` — `register_wire_node` is never called (only `register_with_session` is used from main.rs). Dead code.
- Fix: Remove or mark as deprecated.

**4h-iv. `std::process::exit(0)` bypasses async cleanup:**
- `main.rs:5413` — quit handler calls `std::process::exit(0)` which kills the process immediately, bypassing any async cleanup (open DB connections, pending writes, tunnel shutdown).
- Fix: Use Tauri's proper app exit mechanism instead of `process::exit`.

**4h-v. `set_tokens_from_deep_link` errors silently:**
- `main.rs:5458` — deep link token handler catches errors but doesn't surface them to the user. If deep link auth fails, the user has no idea. Additionally, `main.rs:5477` uses `.ok()` to swallow `register_with_session` failure, resulting in a "logged in but not Wire-registered" state that persists across restart with no user-visible error.
- Fix: Replace `.ok()` at line 5477 with explicit error handling. Emit a Tauri event or show a notification on failure for both token parsing and registration.

**4h-vi. LogViewer unused containerRef:**
- `LogViewer.tsx:8` — `containerRef` created but never used. Dead code.
- Fix: Remove unused ref.

**4h-vii. CORS dev port mismatch:**
- `server.rs:319-323` allows origins on port 1420, but Vite dev server runs on 5173. Dev-mode pyramid API cross-origin requests are blocked.
- Fix: Add `http://localhost:5173` and `http://127.0.0.1:5173` to `CORS_ALLOWED_ORIGINS`.

**Files (4h):**
- `src-tauri/src/main.rs` — async log reading (4h-i), deep link error handling (4h-v), process exit (4h-iv)
- `src-tauri/src/auth.rs` — remove dead `register_wire_node` (4h-iii)
- `src-tauri/src/server.rs` — CORS dev port (4h-vii)
- `src/components/LogViewer.tsx` — remove unused containerRef (4h-vi)
- `src/components/ActivityFeed.tsx` — remove duplicate poll (4h-ii)

---

## Phase 4g. Settings bugs (Pillar 38)

**4g-i. node_id corruption:**
- `Settings.tsx` line 69 passes `config.node_id` (a UUID) as `nodeName` to `save_onboarding`. This overwrites the human-readable node name with a UUID on every settings save.
- Fix: Either fetch/display the actual node name and let users edit it, or use a separate settings-save command that doesn't require node_name.

**4g-ii. Vibesmithy URL not persisted:**
- `PyramidSettings.tsx` has a `vibesmithyUrl` state variable (line 16) with a text input (line 170), but `handleSave` only saves `apiKey` and `authToken`. The URL is silently discarded on save.
- Fix: Either wire up persistence in the Rust backend, or remove the field from the UI.

**Files:**
- `src/components/Settings.tsx` — fix node_id-as-nodeName bug
- `src/components/PyramidSettings.tsx` — fix or remove vibesmithyUrl field

---

## Phase 8: Onboarding + Content + First-Run

1. **Pyramids first-run state** — gating logic already exists in `PyramidsMode.tsx` line 19 (`if slugs.length === 0 && !config.api_key_set` → render `PyramidFirstRun`). Update `PyramidFirstRun.tsx` content and flow: lead with "Link a folder to build your first pyramid" as a single-action card. One button → folder picker → corpus creation → first build starts. Also add an empty-state card to `PyramidDashboard.tsx` for the case where API key IS set but no pyramids exist (user set key, skipped workspace step — currently shows empty dashboard with no guidance).
2. **OnboardingWizard.tsx** — update welcome text for pyramid-first flow, add back button between steps
   - Current welcome (line 92) says "document hosting mesh" — should lead with pyramid building
   - Benefits list mentions "Host documents" and "Earn credits" but not "Build knowledge pyramids"
   - Mesh Hosting step (step 3) promises auto-discovery that isn't implemented — update copy to be accurate about current state
2. **Agent onboarding instructions** (in PyramidDashboard) — verify CLI commands are current
3. **Overall content pass** — review all placeholder text, tooltips, empty states across all modes
4. **Markdown preview dependency** — Phase 6 Compose UX needs a markdown renderer. Check if `react-markdown`, `marked`, or similar exists in `package.json`. If not, add as a dependency. Alternatively implement minimal bold/italic/headers/links renderer without a dependency.

**Files:**
- `src/components/OnboardingWizard.tsx`
- `src/components/PyramidDashboard.tsx` — agent onboarding section
- `package.json` — potential markdown dependency addition

---

## CSS Strategy

The app uses a single monolithic `src/styles/dashboard.css` (11,656 lines). No CSS modules, styled-components, or Tailwind.

**Convention for new components:**
1. All new styles go into `dashboard.css` (matching existing convention)
2. Use existing CSS variable system (`--bg-card`, `--accent-cyan`, etc.)
3. Follow naming convention: `.{mode}-{element}` (e.g., `.fleet-overview`, `.search-results`, `.search-cost-badge`)
4. Group new styles at the end of the file with section headers (`/* === Fleet Mode === */`)

---

## Implementation Order

| Phase | What | Size | Depends on |
|-------|------|------|-----------|
| **0** | **Auth infrastructure** (`wireApiCall` + 401 retry) | **Small-Medium** | **—** |
| 1 | Search Mode (incl. credit balance sync + Wire server query/preview endpoint) | Large | Phase 0 |
| 2a | Fleet infrastructure + Fleet Overview + Corpora migration | Large | Phase 0 |
| 2b | Fleet Mesh + Tasks | Medium | Phase 2a |
| 3 | Network + Pulse + DashboardMode nav fixes | Medium | Phase 0, Phase 2a (needs `'fleet'` mode) |
| 4a | Chain executor fix (Rust) | Medium | — |
| 4b | Identity display fixes (incl. HandleInfo type fix) | Small-Medium | Phase 0 (4b-iii needs wireApiCall for earnings) |
| 4c | Reputation fetch | Small-Medium | Phase 0 |
| 4d | Handle persistence | Medium | — |
| 4e | Security hardening (CSP, API key leak, updater pubkey) | Medium | — |
| 4f | User-facing UI bugs (4f-i through 4f-iv) | Small-Medium | — |
| 4h | Dev ergonomics + cleanup (4f-v through 4f-xi) | Small | — |
| 4g | Settings bugs (node_id corruption, vibesmithyUrl) | Small | — |
| 5 | Activity enrichment (notification fix + circle messages read-only) | Medium | Phase 0, Phase 1 (ContributionCard) |
| 6 | Compose UX (incl. markdown dep install) | Medium | Phase 0 (topics) |
| 7 | Node/Market | None (already done) | — |
| 8 | Onboarding + content + first-run state | Small-Medium | All above |

**Phase 0 is the critical path** — Phases 1, 2a, 2b, 3, 4b, 4c, 5, 6 all depend on `wireApiCall`.
Phases 4a, 4d, 4e, 4f, 4g, 4h are independent and can start immediately.
**Phase 3 depends strictly on Phase 2a** — DashboardMode needs `setMode('fleet')` which requires the Mode type change. Phase 2a's Mode type change must be fully atomic: the type union update AND all consumer updates (stewardship, DashboardMode, Sidebar, ModeRouter, FleetMode) ship together.
**Phase 2b depends on Phase 2a** — Mesh and Tasks tabs are added to the sub-tab routing created in 2a.
Phase 5 depends on Phase 1 (reuses ContributionCard).

## Audit Gates (Pillar 39: Serial verifier after implementation)

After each phase implementation, a serial verifier agent audits with fresh eyes and fixes in place before the phase is considered complete.

Audit rounds continue until auditors come back clean (Pillar 38).

**Audit sequence:**
1. Implement phase N
2. Serial verifier audits phase N output
3. Verifier fixes any findings in place
4. If findings were critical/major → re-audit until clean
5. Phase N complete → move to next phase

After all phases complete, a final holistic audit pass before Phase 8.

---

## Key Files

| File | Changes |
|------|---------|
| `src-tauri/src/main.rs` | `wire_api_call` cmd + fresh-install handling + `generate_handler![]` registration (P0), 401 retry + response parsing fix on `operator_api_call` (P0), JoinHandle monitor for pyramid+vine builds (4a), handle cache cmds + registration (4d) |
| `src/contexts/AppContext.tsx` | `wireApiCall` wrapper (P0), remove warroom (P3) |
| `src/components/Sidebar.tsx` | Remove Warroom, rename Agents→Fleet |
| `src/components/ModeRouter.tsx` | Remove warroom, update agents→fleet |
| `src/components/modes/SearchMode.tsx` | Full rewrite — Wire search UI |
| `src/components/modes/AgentsMode.tsx` | Rename to FleetMode.tsx, implement fleet overview |
| `src/components/stewardship/*.tsx` | Update `'agents'` → `'fleet'` in pushView/popView (4 files) |
| `src/components/modes/DashboardMode.tsx` | Add pulse data cards |
| `src/components/modes/ActivityMode.tsx` | Expandable detail view |
| `src/components/modes/IdentityMode.tsx` | Display fixes, HandleInfo type fix, reputation fetch, transaction history |
| `src/components/modes/ComposeMode.tsx` | Preview, help text, success state |
| `src/components/modes/WarroomMode.tsx` | DELETE |
| `src/components/Settings.tsx` | Fix node_id-as-nodeName bug (4e) |
| `src/components/PyramidSettings.tsx` | Fix or remove vibesmithyUrl field (4e) |
| `src/components/OnboardingWizard.tsx` | Content update, back button, mesh hosting copy |
| `src/components/AppShell.tsx` | Fix notification dual-source (P5) |
| `src/components/PyramidDashboard.tsx` | Cancel/retry buttons |
| New: `src/components/search/ContributionCard.tsx` | Reusable contribution card |
| New: `src/components/search/EntityBrowser.tsx` | Entity exploration |
| New: `src/components/fleet/FleetOverview.tsx` | Agent roster + online status |
| New: `src/components/fleet/MeshPanel.tsx` | Blackboard + intents |
| New: `src/components/fleet/TaskBoard.tsx` | Task management |
| New: `src/components/common/SlideOverPanel.tsx` | Generic slide-over panel |
| `src/components/PyramidsMode.tsx` | First-run gating (P8) |
| `src/components/PyramidFirstRun.tsx` | First-run wizard content update (P8) |
| `GoodNewsEveryone/src/app/api/v1/wire/query/preview/route.ts` | Query cost preview endpoint (P1) — Wire server |

## Existing Code to Reuse
- `operatorApiCall()` — for dual-auth endpoints (handles, notifications, contributions/human, requests)
- `get_api_token()` in Rust — basis for new `wireApiCall` path
- `CorporaList`/`CorpusDetail`/`DocumentDetail` from `src/components/stewardship/` — stays in Fleet
- Stack-based navigation from `AppContext` — reuse for search drill-down, fleet drill-down
- `PyramidDetailDrawer` CSS pattern — extract into generic `SlideOverPanel` (NOT the component itself)
- `TagInput` component — reuse for search filters, compose topics
- `MarketView` + `market.rs` — already working, keep as-is
- `ActivityFeed` component — reuse in Network pulse view

## What's deferred (with reason)
- **Corpus discovery in Search** — `discover/corpora` endpoint exists but no UI surface designed. Can add later.
- **Partner/Dennis conversation UI** — backend exists (`partner/` module, `partner_send_message` + `partner_session_new` IPC commands), but no frontend tab or surface in v1. Vibesmithy is the planned Partner UI.
- **Circle compose/reply** — v1 shows circle messages read-only in Activity. Composing circle messages requires circle context (which circle, threading) and is deferred.
- **Keyboard shortcuts / accessibility** — no keyboard navigation spec for v1. Escape-to-close on panels is specified but tab cycling, arrow keys in lists, and shortcut keys are deferred.

## Verification

1. `cargo build` — Rust compiles clean (wireApiCall, JoinHandle monitor, cancel command, handle cache)
2. TypeScript compiles — no errors from restructured modes
3. Launch app — 9 tabs visible, correct labels and icons
4. **Search:** Feed loads free → type query → see cost estimate → confirm → results with pagination → click to expand → browse entities/topics
5. **Fleet (2a):** see agent roster with online status → corpora drill-down works
5b. **Fleet (2b):** mesh blackboard shows state → declare intent → confirmation dialog appears → tasks board with columns
6. **Network:** see pulse data (fleet online, active tasks, circle activity, unread messages)
7. **Activity:** click notification → detail expands → see full contribution → rate → navigate to source → scroll to Messages section → see circle messages → mark as read
8. **Identity:** reputation shows real score (or clean dash) → handle shows `@name` not `@@name` → no redundant balance → handle persists across restart → transaction history visible
9. **Compose:** toggle markdown preview → see type descriptions → reply shows target context → topics autocomplete → submit shows success
10. **Pyramids:** start build → cancel mid-build (new button) → retry after failure → no stuck "running" state
11. **Node:** Market tab shows hosted documents, pulls, credits
12. **Onboarding:** back button works → content is current
13. **First-run:** fresh install → onboarding completes → Pyramids tab shows "Link a folder" card → click → folder picker → build starts
14. **Security (4e):** CSP active in `tauri.conf.json` → API key test goes through IPC (no direct fetch to OpenRouter from renderer) → updater pubkey populated → remote connection test goes through IPC
15. **Bug fixes (4f/4g):** settings save preserves node name (not UUID) → magic link login completes without manual refresh → dev mode CORS permits localhost:5173
16. **Wire server:** `GET /api/v1/wire/query/preview` returns 200 with `{ base_cost, surge_multiplier, estimated_cost }` — deploy preview endpoint to Wire server BEFORE testing Phase 1 Search cost disclosure
