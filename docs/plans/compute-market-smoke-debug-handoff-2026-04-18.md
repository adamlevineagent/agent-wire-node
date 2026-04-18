# Compute Market Round-Trip Smoke — Debug Handoff

**Date:** 2026-04-18
**Status:** blocked on `/api/v1/compute/match` returning `no_offer_for_model` despite offer being visible on `/market-surface`.
**Owner handing off:** Claude (this session)
**Owner picking up:** Codex (or whoever Adam deputizes)
**Repos:** `agent-wire-node` (node), `GoodNewsEveryone` (Wire). Both on `main`.

---

## TL;DR

Every ceremony step to enable cross-operator compute market dispatch is done.
Wire-side structural fix landed (`GoodNewsEveryone@307cc226`). Node-side
structural fix landed (`agent-wire-node@0f4a579`). Both machines rebuilt
and running new code. Playful-upstairs has 262,726 credits + policy flags
enabling requester-side dispatch + market visibility + eager mode.
BEHEM-downstairs has the offer published, `is_serving=true`, tunnel
Connected, `wire_offer_id: "dashing-fern-olive/106/1"`.

**Yet:** `/api/v1/compute/match` responds `404 no_offer_for_model` for every
budget under balance (11, 50, 100k, 262725). Only at budget > balance does
it switch to `409 insufficient_balance` (confirming the balance check itself
works correctly — that's a shared-types typed error body, proving Wire runs
the new code too).

Something in Wire's `match_compute_job` CTE filter is rejecting BEHEM's
offer row. Without Wire-side server logs, we can't pin which filter.

**Two fresh clues from Adam at handoff time** (not yet investigated):

1. **BEHEM (provider side)'s auto-assigned handle changed mid-session.**
   Adam clarified: this is the `adamlevinemobile+agent@gmail.com` operator
   on downstairs, NOT Playful upstairs (Playful's handle is stable).
   BEHEM's offer currently shows `wire_offer_id: "dashing-fern-olive/106/1"`
   — handle-path format `{handle}/{epoch_day}/{daily_seq}` — so current
   handle is `dashing-fern-olive`. Adam saw this handle flip from one
   random-generated value to another.
   **Hypothesis:** Wire's `match_compute_job` resolves offers via the
   operator's current handle. If BEHEM's handle was re-assigned while
   the offer row kept the old handle_path snapshot, the offer becomes
   orphaned for match-path queries — present in raw `wire_compute_offers`
   but filtered out by the handle-JOIN in the CTE. This would explain:
   - market-surface sees it (direct read on `wire_compute_offers`)
   - /match filters it (JOIN through operator handle fails)

2. **`~/Library/Application Support/wire-node/pyramid_config.json.auth_token`
   reads literally `"test..."`** (first ~20 chars, on Playful upstairs).
   Not a real `gne_live_*` machine token. Smells like test fixture auth
   that somehow persisted into the live config. Secondary clue — may or
   may not matter, worth checking whether Wire accepts it on /match.

Hypothesis #1 (BEHEM handle change) is the top suspect.

---

## What's confirmed working

### Node side (upstairs, Playful, `agent-wire-node@0f4a579` dev-mode)

- ✅ HTTP server bound on `localhost:8765` (verified via `curl`, `lsof`)
- ✅ Typed error bodies observed from Wire: `{detail:{budget,model_id},error:"no_offer_for_model"}`
  (pre-structural-fix, detail was a plain string — confirms Wire running new code)
- ✅ `parse_balance_detail` fix verified: `max_budget=999999999` now returns
  `insufficient_balance: need 999999999, have 262726` (was `have 0, need 0`
  masquerade for weeks)
- ✅ `allow_market_dispatch: true`, `allow_market_visibility: true`,
  `market_dispatch_eager: true` on Playful's policy (flipped via direct
  `PUT /pyramid/compute/policy` — see session history)
- ✅ Playful balance 262,726 credits per `system-health`
- ✅ `cargo check` clean + 1666 tests pass, same 15 pre-existing failures,
  zero new regressions

### Wire side (`GoodNewsEveryone@307cc226` live on prod)

- ✅ Migrations applied, PostgREST restarted (Adam confirmed earlier)
- ✅ Platform operator resolves: `b4f7141b-b8df-4d37-8964-e565ed6c15e9` (previously invisible via broken JOIN)
- ✅ `wire_compute_offers.last_queue_push_at` + `est_next_available_s` columns present
- ✅ `wire_compute_queue_state` table dropped
- ✅ `wire_chronicle_event_type_check` CHECK constraint in place

### BEHEM side (downstairs, `adamlevinemobile+agent@gmail.com`, `agent-wire-node@0f4a579` dev-mode on Windows)

- ✅ Running new binary (confirmed — toggled off+on, no new `queue_mirror_push_failed` events with old URL)
- ✅ `is_serving: true`, `queue_mirror_seq: {gemma4:26b: 1}`
- ✅ `wire_offer_id: "dashing-fern-olive/106/1"` — offer published
- ✅ Tunnel Connected, `tunnel_url: "https://node-37a4a1b3-....agent-wire.com/"`
- ⚠️ `compute_queue_mirror_pushed: []` — zero node-side chronicle events for
  push success. **But** that event is only emitted by Wire-side into
  `wire_chronicle`, not into node's `pyramid_compute_events`. Node chronicle
  being empty doesn't prove pushes aren't happening. (Next commit adds a
  node-local chronicle emit on success — see "Unfinished work" below.)
- ⚠️ Only `queue_mirror_push_failed` event is from `2026-04-18T00:23:27 UTC`
  (7.5h pre-fix, with old URL). No new failures since relaunch.

### Public market surface

- ✅ `curl https://newsbleach.com/api/v1/compute/market-surface`
  → `{gemma4:26b, active_offers:1, providers:1, queue:{capacity:8, depth:0}}`

---

## Where the smoke fails

From upstairs (Playful):

```
cd "/Users/adamlevine/AI Project Files/agent-wire-node"
node mcp-server/dist/cli.js compute-market-call gemma4:26b \
  --prompt "Say hello in one word" \
  --max-budget 100000 \
  --max-tokens 20 \
  --max-wait-ms 120000
```

Response (reproducible):

```json
{
  "error": "no market match: {\"detail\":{\"budget\":100000,\"model_id\":\"gemma4:26b\"},\"error\":\"no_offer_for_model\"}"
}
```

### Budget bisection (rules out budget-threshold filter)

| `max_budget` | Response |
|---|---|
| `11` / `50` / `200_000` / `262_725` / `262_726` | 404 `no_offer_for_model` |
| `999_999_999` | 409 `insufficient_balance: need 999999999, have 262726` |

Balance check at 262,726 is the ONLY threshold that switches response class.
Below that → match filter rejects. Above → balance filter rejects (meaning match filter would otherwise run with the offer included).

**Inference:** the filter rejecting the offer is NOT budget-based.
Match_compute_job is excluding the offer for some other CTE condition.

---

## What to investigate next (ordered by likelihood)

### 1. BEHEM handle change orphaning the offer (HIGH — user-reported clue)

Adam observed BEHEM's auto-assigned handle change from one random value to
another during today's session. `adamlevinemobile+agent@gmail.com` operator,
downstairs node. Current offer shows `wire_offer_id: "dashing-fern-olive/106/1"`.
Playful upstairs handle is stable — not the same issue.

If the offer row on Wire keeps the OLD handle_path snapshot while BEHEM's
current operator → handle_path resolution returns the NEW handle, any
match-RPC CTE that does `JOIN wire_handles ON wire_compute_offers.handle_path
= wire_handles.handle_path WHERE operator_id = $requester_operator` (or
similar handle-resolved JOIN) silently drops the offer.

**Probes:**
- **Wire DB, primary**:
  ```sql
  -- Offer row as Wire has it:
  SELECT id, handle_path, operator_id, model_id, current_queue_depth,
         max_queue_depth, inactive_reason, last_queue_push_at, updated_at,
         est_next_available_s
  FROM wire_compute_offers
  WHERE handle_path = 'dashing-fern-olive/106/1';

  -- Handle history for BEHEM's operator:
  SELECT h.*, o.current_handle_path
  FROM wire_handles h
  JOIN wire_operators o ON o.id = h.operator_id
  WHERE h.email = 'adamlevinemobile+agent@gmail.com'
     OR o.email = 'adamlevinemobile+agent@gmail.com'
  ORDER BY h.created_at DESC;

  -- Does operator → current handle match the offer's handle_path?
  -- If NO: handle change orphaned the offer.
  -- If YES: different bug.
  ```

- **Wire DB, offer-orphan test**: run the match_compute_job CTE manually
  against BEHEM's offer with Playful as requester, step through each filter
  predicate and check which one excludes it.

- **Node side**: when BEHEM auto-registered and got a handle, did the handle
  get written into `ms.offers[model_id].wire_offer_id`? If Wire's publish
  response embedded the NEW handle but node cached the OLD one (or vice
  versa), the mirror push would send the wrong handle and Wire might reject
  silently.

- **Auto-rehandling mechanism**: why did BEHEM's handle change in the first
  place? That's a bug in itself — handles should be stable post-registration.
  Grep Wire for `auto_generate_handle`, `regenerate_handle`, or anywhere a
  handle can be changed without explicit operator action.

### 2. auth_token is `"test..."` (HIGH — bypassing real auth?)

Adam's `pyramid_config.json.auth_token` literally begins `"test..."`, not the
`gne_live_*` prefix expected for real machine tokens. If the node is
authenticating against Wire with a test token and Wire happens to accept it
for read-paths (market-surface is unauthed; system-health lookups work),
but rejects or mishandles it on match-path, we'd see exactly this.

**Probes:**
- `cat ~/Library/Application\ Support/wire-node/pyramid_config.json | jq .auth_token`
  — full value, not just prefix
- Grep node codebase for where `auth_token` is read + compare to machine-token
  format validation
- Wire-side: log what authenticated operator_id resolves for the match
  request (if Wire sees a different op id than Playful, match may fail
  self-dealing or filter operator = null)

### 3. Match filter trace (MEDIUM — needs Wire-side debugging)

Grep Wire server logs (moltbot) for match attempts with:
- `operator=hello@callmeplayful.com` OR `node_id=eff53295-7ab5-4297-a9b7-6b11ab40e620`
- `model_id=gemma4:26b`
- Timestamp range: recent, Pacific April 18

The `match_compute_job` CTE in `supabase/migrations/20260421400000_inactive_reason_wiring.sql`
(and possibly newer migrations) lists each filter predicate. Identify which
one excludes `"dashing-fern-olive/106/1"`.

Known filter predicates (from Wire-side structural fix plan §2.3):
- `model_id` matches
- `inactive_reason IS NULL` (active offer)
- `operator_id != requester_operator_id` (cross-operator self-dealing)
- `node_id != requester_node_id` (node-level — Wire owner said this is the
  only self-dealing layer, not operator-level)
- `COALESCE(last_queue_push_at, updated_at) > NOW() - staleness_threshold`
  (freshness fallback during mirror-URL transition)
- `current_queue_depth < max_queue_depth` (capacity)
- Budget passes (confirmed not the gate per bisection)

### 4. BEHEM push landing on Wire (MEDIUM — no direct evidence either way)

BEHEM's node chronicle has zero events for `compute_queue_mirror_pushed`,
but that event is emitted server-side by Wire, not by node. Need either:
- BEHEM log grep for `"queue mirror pushed"` (node-side tracing::debug — only
  visible at debug log level in release builds, or if the diagnostic commit
  below lands bumping to INFO)
- Wire-side query: `SELECT last_queue_push_at FROM wire_compute_offers WHERE
  handle_path='dashing-fern-olive/106/1'` — if fresh (last few minutes),
  pushes ARE landing; if NULL or stale, pushes aren't reaching Wire

### 5. Mirror-task alive at BEHEM startup? (LOW — very plausible given silence)

`spawn_market_mirror_task` logs `"market queue mirror task started"` at INFO
on first execution. If BEHEM's `~/Library/Application\ Support/wire-node/wire-node.log`
(or Windows equivalent) doesn't contain this string, the task never ran.
Could be a panic at task-body entry that tokio's spawn eats silently.

---

## Unfinished work (uncommitted on upstairs)

I have a diagnostic commit ready to land in `market_mirror.rs`:

- **Bump `tracing::debug!("queue mirror pushed")` → `tracing::info!`** so the
  happy path is visible in default log output without RUST_LOG=debug.
- **Emit node-local chronicle event `queue_mirror_pushed`** on successful
  push (via fire-and-forget spawn_blocking matching the failure-path pattern).
  Closes the observability gap — operators will see push activity in their
  own chronicle instead of only failures.

Ready to commit + push. See `src-tauri/src/pyramid/market_mirror.rs` around
the `push_snapshot` success branch. Hand-review the diff before committing.

---

## Key files + recent commits

### agent-wire-node (node)

- `0f4a579` — structural alignment with Wire rev 307cc226 (today)
- `20d8295` — cleanup stale Phase 2/3 comments
- `6e3d552` — Market Surface 30s auto-poll
- `2e25c7c` — Market Surface UI → rev 1.5 schema
- `3b5c7f2` — remove stale "Phase 2 provider-side only" prose
- `f3da100` — auto-publish default offer on serving toggle
- `9bcff31` — offer form field-name bug fix + model picker
- `1fb0496` — call_model_unified market branch + chronicle + purpose-lock defaults

Hot files for this bug:
- `src-tauri/src/pyramid/market_mirror.rs` — mirror task, push body, gate
- `src-tauri/src/pyramid/compute_requester.rs` — /match client, error classification
- `src-tauri/src/pyramid/compute_market_ops.rs` — offer create/remove/publish
- `src-tauri/src/auth.rs` — machine-token flow
- `src-tauri/src/main.rs` lines 12680–12760 — mirror task spawn + wiring

### GoodNewsEveryone (Wire)

- `307cc226` — structural fix: operator-scoped handles, queue-state removal, shared-types, SQLSTATE (plan §1–§6)
- `b58adda0` — platform-operator lookup fix
- `54a12bce` — welcome bonus CAS on register-with-session

Hot files for this bug:
- `src/app/api/v1/compute/match/route.ts` — match endpoint handler
- Whichever migration has the current `match_compute_job` body (follow
  `canonical base` references in `docs/plans/compute-market-structural-fix-plan.md`)
- `packages/agent-wire-contracts/` — shared types (error bodies, event enum)

---

## How to reproduce

### Bring upstairs up

```bash
cd "/Users/adamlevine/AI Project Files/agent-wire-node"
git pull                     # ensure 0f4a579 or later
osascript -e 'tell application "Agent Wire Node" to quit'  # kill stale install
export PATH="/opt/homebrew/bin:$PATH"
cargo tauri dev              # source build; first compile ~1m30s
```

### Verify upstairs

```bash
node mcp-server/dist/cli.js system-health | jq .status      # "ok"
node mcp-server/dist/cli.js compute-policy-get | jq '{allow_market_dispatch, allow_market_visibility, market_dispatch_eager}'
# Must show: allow_market_dispatch: true, allow_market_visibility: true, market_dispatch_eager: true

curl -s https://newsbleach.com/api/v1/compute/market-surface | jq '.models[]'
# Must show: gemma4:26b active_offers:1 providers:1
```

### Run the smoke

```bash
node mcp-server/dist/cli.js compute-market-call gemma4:26b \
  --prompt "Say hello in one word" \
  --max-budget 100000 \
  --max-tokens 20 \
  --max-wait-ms 120000
```

Expected (once the filter bug is fixed): content response with network
provenance. Chronicle shows `network_helped_build` + `network_result_returned`.

Current: `no market match: {"detail":{"budget":100000,"model_id":"gemma4:26b"},"error":"no_offer_for_model"}`

---

## Mission reminder

The operational litmus test: **GPU-less tester builds a pyramid via the
network without seeing a market word.** Every diagnostic and fix in this
thread serves that test. When investigating, ask: does this advance the
GPU-less-tester-builds-via-network path? If yes, proceed. If no, question.

Purpose brief lives in memory at
`~/.claude/projects/-Users-adamlevine-AI-Project-Files/memory/project_compute_market_purpose_brief.md`.

---

## Contact / coordination

- Adam relays paste-backs between this thread and Wire owner's thread.
  Give him a self-contained message to forward; don't assume he'll edit it.
- BEHEM is on Windows (per earlier diagnostic showing `C:\Project Growth\`).
  PowerShell-compatible diagnostics preferred over bash.
- Upstairs (this machine) is macOS Sequoia, zsh, cargo tauri dev from source.
- Wire owner has been responsive and thorough. He's grep'd Wire logs on
  request before. The `match_compute_job` trace request is a reasonable ask.
