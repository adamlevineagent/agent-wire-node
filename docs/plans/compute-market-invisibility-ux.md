# Compute Market — Invisibility UX Pass

**Status:** Plan, ready to implement
**Scope:** Node-side only (agent-wire-node). Market → Compute tab reframe.
**Dependencies:** Wire W1 shipped (offer CRUD live on prod). W2 (market-surface detailed) not blocking this pass.
**Follow-up surfaces:** Builds tab reframe waits for node Phase 3 (requester-side).
**Estimated scope:** ~1–2h focused implementation, 30–60 min realistic per corrected estimation heuristic.

---

## 1. The frame shift

### The thing we were about to build (wrong)

A trader dashboard. Primary KPI: credits earned. Toggle labeled "earn by serving compute." Narrative: you are a provider optimizing for revenue. Session earnings as the hero number.

### The thing the compute market actually is (right)

A **mutualized compute pool** dressed up as a market. Under the hood there are real market primitives — rotator arm, prices, queue discounts, settlement. But the value proposition to a human is not "income." It's **"my 200-node pyramid build takes 45 seconds instead of 75 minutes, because the network has my back, and I have theirs when they need it."**

Credits are the accounting mechanism that keeps the pool balanced — nobody draws more than they give over time. Like a timebank ledger. The ledger exists; it's not the point.

### Closest reference metaphors (strongest → weakest)

1. **WiFi connectivity.** You're connected. The network does work. You don't think about it. You notice when it's off.
2. **BitTorrent swarm.** Leechers get slower speeds; seeders are first-class. Ratio matters but isn't the point — the file arrived fast because the swarm exists.
3. **Mutual-aid timebank.** Hours in, hours out; ledger balances but nobody shows up to "earn hours."

BitTorrent is the sharpest reference — it had a real tit-for-tat market mechanism but users experienced it as "I'm seeding, my download is fast." The mechanism served the experience.

---

## 2. Language contract

Every user-facing string in the primary surface treats this as a network you're part of, not a venue you trade on. The market mechanism (rates, matching, settlement) exists in Advanced only.

| ❌ Never say | ✅ Say |
|---|---|
| earn credits | contribute (or: help) |
| sell compute | contribute when idle |
| marketplace | compute network (or: the pool) |
| job accepted | helping a build |
| revenue / earnings | balance |
| providers & consumers | members |
| match found | connected to a build |
| negative balance | behind on contributions |
| offer published | publishing configuration → Advanced |

**The one-line story** — should be defensible against every surface we build:

> "Your pyramids build fast because you're in the network. When you're idle, you help the network. Balance evens out."

---

## 3. The four primary states (Market → Compute tab)

### 3.1 Default — model loaded, contributing, network active

```
┌───────────────────────────────────────────────────────┐
│  🌐 Compute network · Connected                       │
│                                                        │
│  Contribute GPU when idle         [ ●━━ ]            │
│  Model served: gemma4:26b                             │
│                                                        │
│  ─────────────                                         │
│                                                        │
│  Last hour                                             │
│    Helped · 4 builds · ~3 min total GPU time          │
│    Used   · 1 build  · ~47s (184 nodes helped you)    │
│                                                        │
│  This week                                             │
│    Contributed 8,200 · Used 7,400 · Balance +800      │
│                                                        │
│  ─────────────                                         │
│                                                        │
│  ▸ Advanced — rates, offers, market inspector         │
└───────────────────────────────────────────────────────┘
```

Data sources (Phase 2):
- "Model served" ← `compute_market_get_state().offers[*].model_id`
- "Last hour helped" ← chronicle `EVENT_MARKET_RECEIVED` events, last 60 min, aggregated
- "Last hour used" ← **N/A in Phase 2**; this row is gated behind Phase 3 (requester-side). Show placeholder "—" for now or hide until requester data exists.
- "This week contributed/used/balance" ← chronicle aggregation + credit balance snapshot. Use `get_compute_summary(period_start=7d, group_by='source')` already-implemented route.

### 3.2 Idle — connected, model loaded, but no traffic

```
┌───────────────────────────────────────────────────────┐
│  🌐 Compute network · Ready                           │
│                                                        │
│  Contribute GPU when idle         [ ●━━ ]            │
│  Model served: gemma4:26b                             │
│                                                        │
│  Quiet on the network right now.                      │
│  Helped 3 builds earlier today.                       │
│                                                        │
│  ▸ Advanced                                           │
└───────────────────────────────────────────────────────┘
```

Trigger: no `EVENT_MARKET_RECEIVED` in the last ~10 min AND no `ComputeMarketState.active_jobs` entries. Soft, ambient, not a dashboard.

### 3.3 Active — network is using your GPU right now

```
┌───────────────────────────────────────────────────────┐
│  🌐 Compute network · Helping a build                 │
│                                                        │
│  L0 cluster · 184 tokens/s                            │
│  Next slot available in ~40s                          │
│                                                        │
│  [ Pause contribution ]    ▸ Advanced                │
└───────────────────────────────────────────────────────┘
```

Data sources:
- "Helping a build" ← any entry in `active_jobs` with status `Queued` or `Executing`
- "L0 cluster" ← the job's `step_name` or `primitive` if available; fall back to generic "a build" if not
- "tokens/s" ← derived from `active_jobs[*].started_at` + current token count (may not be available; if not, drop)
- "Next slot available in ~40s" ← projected queue drain time = current queue depth × average per-job latency

### 3.4 No-GPU / no model loaded — consumer-only membership

```
┌───────────────────────────────────────────────────────┐
│  🌐 Compute network · Consumer member                 │
│                                                        │
│  You're in the network — your pyramids build with     │
│  network help. Load a local model to also contribute. │
│                                                        │
│  Credit balance · 12,400                              │
│  (earned from your other Wire contributions)          │
│                                                        │
│  [ Load a local model ]   [ Not right now ]          │
└───────────────────────────────────────────────────────┘
```

Trigger: local mode disabled OR no model selected. "Not right now" dismisses for the session; next session re-shows until dismissed persistently (local UI state, no contribution needed).

### 3.5 Paused — serving disabled by operator

```
┌───────────────────────────────────────────────────────┐
│  🌐 Compute network · Paused                          │
│                                                        │
│  Contribute GPU when idle         [ ━━● ]            │
│                                                        │
│  You're still connected as a consumer — your          │
│  builds still get network help (paid from balance).   │
│  Turn contribution back on to help others and         │
│  keep your balance even.                              │
│                                                        │
│  Balance · 12,400                                     │
│                                                        │
│  ▸ Advanced                                           │
└───────────────────────────────────────────────────────┘
```

Data source: `ComputeMarketState.is_serving === false`.

### 3.6 Empty network — honest framing

```
┌───────────────────────────────────────────────────────┐
│  🌐 Compute network · Quiet                           │
│                                                        │
│  Contribute GPU when idle         [ ●━━ ]            │
│  No active builds on the network right now.           │
│  Your builds will run local-speed until others join.  │
│                                                        │
│  ▸ Advanced                                           │
└───────────────────────────────────────────────────────┘
```

Trigger: no active_jobs + no chronicle events in the last ~10 min + no other operators showing in fleet roster. Distinguish from 3.2 (Ready) by the emphasis — 3.6 says "nobody's here," 3.2 says "I'm here and available."

*Note:* distinguishing 3.2 vs 3.6 is nice-to-have; if the heuristic is hard, default to 3.2 phrasing. Don't overthink.

---

## 4. Capability moment — for reference, NOT in scope this pass

Phase 3 (requester-side) will add the **capability moment** in the Builds tab:

```
Built in 47s using 184 network GPUs
(Solo build would have taken ~14m)
```

This is the emotional payoff — the reason the whole thing exists. Provider surface should *point at* it but not claim to deliver it. A line on the Market → Compute tab like "Your own builds have used the network 12 times this week" points toward it. Don't build the full capability moment here — it belongs in the Builds tab after Phase 3.

---

## 5. Advanced drawer — everything that currently lives on the page, demoted

The `<AdvancedDrawer>` component (new, collapsible, default-closed) holds:

- Per-model offer configuration: `rate_per_m_input`, `rate_per_m_output`, `reservation_fee`
- Queue discount curves (JSON editor)
- Per-offer `max_queue_depth`
- Market surface inspector (post-W2): price ranges, provider counts, performance medians
- Raw offer list with full `wire_offer_id` (UUID-OR-HANDLE-PATH)
- The full `compute_participation_policy` matrix (all 10 fields: mode + 9 allow_* booleans)
- Chronicle event stream for this market (debugging aid)
- Full credit ledger: earned/spent/pending per source breakdown

Power operators can still do everything. Default-state testers don't see it.

The drawer itself should feel sober — monospaced numbers, dense tables, no hand-holding language. It's the expert surface; it can look like one.

---

## 6. Implementation plan

### 6.1 Files to touch

| File | Action |
|---|---|
| `src/components/market/ComputeMarketDashboard.tsx` | Rewrite into state-driven primary surface (§3.1–3.6) |
| `src/components/market/ComputeOfferManager.tsx` | Move under Advanced; keep logic intact, remove prominence |
| `src/components/market/ComputeMarketSurface.tsx` | Move under Advanced; keep as-is structurally |
| `src/components/market/AdvancedDrawer.tsx` | **NEW** — collapsible, holds the above three |
| `src/components/market/ComputeNetworkStatus.tsx` | **NEW** — the primary hero component (handles state 3.1–3.6 selection) |
| `src/components/market/ContributionSummary.tsx` | **NEW** — "last hour / this week" rows |
| `src/components/market/ActiveBuildIndicator.tsx` | **NEW** — state 3.3 component |
| `src/components/market/ConsumerMemberInvite.tsx` | **NEW** — state 3.4 component |
| `src/components/modes/MarketMode.tsx` | Rename "Hosting" sub-tab → "Network" (operator already-familiar language); update Compute sub-tab to mount new Dashboard |
| `src/styles/dashboard.css` | Add styles for new components; reuse existing design tokens |

No Rust changes needed for this pass. Purely frontend reframe.

### 6.2 State derivation — pure function

The hero component picks a state from the already-available IPC surface:

```ts
// Given: ComputeMarketState + local-mode-status + chronicle-summary
// Return: one of {Connected, Ready, Active, Paused, Consumer, Quiet}

function deriveNetworkState(snapshot): NetworkState {
  if (!localModeEnabled || noModelLoaded) return "Consumer";
  if (!market_state.is_serving)          return "Paused";
  if (active_jobs_queued_or_executing)   return "Active";
  if (helpedInLast10min)                 return "Ready";
  return "Quiet";
}
```

One state at a time; no ambiguous composites. All six header variants are just different render branches off the same selector.

### 6.3 Component contract

```tsx
<ComputeNetworkStatus>
  <NetworkHeader state={state} />      {/* 🌐 line + connection word */}
  <PrimaryAction state={state} />      {/* toggle OR CTA OR pause button */}
  <StateBody state={state} />          {/* body per §3.x */}
  <AdvancedDrawer>{/* everything else */}</AdvancedDrawer>
</ComputeNetworkStatus>
```

Keep components small, keep the state-enum central.

### 6.4 Data plumbing (no new IPC needed)

All data already available via existing IPC:
- `compute_market_get_state()` — offers, active_jobs, is_serving, balance counters
- `pyramid_get_local_mode_status(slug)` — enabled + selected model
- `get_compute_summary(period_start, period_end, group_by='source')` — weekly aggregation
- `get_compute_events({event_type: 'market_received', after: 10min ago})` — last-hour activity

Polling cadence: existing patterns (5–10s for the state snapshot; hourly for the weekly roll-up).

---

## 7. Scope boundaries

### 7.1 In this pass
- Reframe of Market → Compute tab per §3
- New `AdvancedDrawer` component holding all existing trader-surface components
- String pass per §2 language contract
- Empty-state / no-model / no-network flows
- Pause/resume toggle semantics

### 7.2 Waits for node Phase 3
- "My build used 184 network GPUs" capability moment in Builds tab
- Per-build historical network attribution
- "Used" half of the "helped/used" split in state 3.1 (Phase 2 only has provider-side data)

### 7.3 Waits for Wire W2
- Live market surface inspector populated in Advanced (needs `/api/v1/compute/market-surface` to return data, not 404)

### 7.4 Explicitly out of scope
- Onboarding flow redesign (separate concern)
- Builds tab changes (Phase 3 territory)
- Any new IPC or HTTP routes (data is sufficient)
- Rust backend changes

---

## 8. Success criteria

Tester walks into the Market → Compute tab on a freshly-built node with a model loaded. They see:

> 🌐 Compute network · Connected
> Contribute GPU when idle [ ●━━ ]
> Model served: gemma4:26b

They flip the toggle. Nothing they need to understand. They go build a pyramid. When the pyramid build hits the network (Phase 3, later), the capability moment in the Builds tab sells the rest.

At no point in the primary surface does the tester see the words "market," "earn," "sell," or "revenue." At no point do they need to configure an offer to participate. Rates are already sensible defaults; queue caps are already reasonable. The Advanced drawer exists for operators who care to tune.

If I showed this to a non-technical friend and asked "what is this?" — they'd say "oh, it's a compute network, like WiFi for GPUs." That's the pass criterion.

---

## 9. Sequencing — this pass in context

1. **This pass — Market → Compute reframe** (unblocked, in this plan)
2. **Node `extensions` field on MarketDispatchRequest** (~15 min, prereq for Phase 3; bundle with this pass)
3. **Wait for Wire W3 planning kick-off** → start node Phase 3 (requester-side)
4. **Builds tab reframe** — the capability moment. After Phase 3 can actually render it.
5. **Handle-path migration cutover** — grep `UUID-OR-HANDLE-PATH`, review, ship. After Wire ships their handle-path fix.

The invisibility UX pass is the foundation. Phase 3 sits on top of it. If Phase 3 arrived before this pass, we'd redesign twice.

---

## 10. Anti-goals — things to NOT do

- **Don't gamify.** No badges, streaks, "rank on the network," leaderboards. The pool isn't a game; badges would undercut the utility framing.
- **Don't apologize for the mechanism.** The rates + settlement + rotator arm are load-bearing economic infrastructure, not a dirty secret. Advanced drawer shows them proudly. The default surface just doesn't lead with them.
- **Don't hide that it's peer-to-peer.** If a user asks "where is my GPU time going," we should say honestly "to another Wire operator's pyramid build, anonymized by privacy tier." Mystery-network framing would be creepy.
- **Don't tell testers how much they'll "earn."** That's the trader frame. The value prop is capability, not cash.
- **Don't build metrics that pressure contribution** ("you're only at 40% of this week's potential!"). The pool is meant to be low-pressure background utility, not a Fitbit for compute.
