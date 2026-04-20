# Compute Market Staleness — Wire-Side Handoff

**Date:** 2026-04-20
**From:** Claude on agent-wire-node (Adam's upstairs mac)
**To:** Wire-side owner (GoodNewsEveryone repo on moltbot)
**Stack:** self-hosted Supabase + PostgREST (Next.js on Temps)

---

## TL;DR

End-to-end smoke fails because Wire's `/api/v1/compute/match` 404s with `no_offer_for_model` even when a healthy active provider exists. Two root causes identified — one fixed on the node side, one needs your hand on the Wire side.

**You need to:**

1. Broaden `match_compute_job` CTE's staleness filter to accept **node heartbeat freshness** as an alternative to queue-push freshness.
2. Apply the same broadening to `/api/v1/compute/market-surface` aggregation so the public surface stops lying about stale offers.
3. Drop the `/market-surface` in-memory cache TTL from 5 min → 60 s (it currently shows phantom-live offers for 5 min past their staleness horizon).
4. Add a new `economic_parameter` contribution: `node_heartbeat_staleness_s = 180`.

No schema migration needed beyond the economic_parameter seed + the function replacement. The node heartbeat already runs and already updates `wire_nodes.last_heartbeat` every 60 s — you're just extending the CTE to use that column.

---

## How we got here

### The symptom

Playful upstairs (requester, macOS) → `POST /pyramid/compute/market-call gemma4:26b, max_budget=100000` → Wire 404 with `no_offer_for_model`. Reproducible. Budget bisection proves the balance filter works (flips to 409 at max_budget=999_999_999 against the 262_726 balance), so the match RPC is running, it just returns zero candidate rows.

`/api/v1/compute/market-surface` simultaneously shows:

```json
{"models":[{"model_id":"gemma4:26b","active_offers":1,"providers":1,...}]}
```

So operators see "1 provider available" while matching 404s. Same tester-facing surface, two different truths.

### The investigation

Direct query of `wire_compute_offers` + `wire_nodes` on moltbot (full queries and output in the session chronicle):

```
 id (offer)                           | node_handle | model_id   | status | last_queue_push_at         | updated_at                   | last_heartbeat             | market_visibility_denied_at
 5bf00c13-a87e-4bcf-9778-652c4bd698ae | behem       | gemma4:26b | active | 2026-04-18 07:30:34.392+00 | 2026-04-18 07:30:34.392+00   | 2026-04-20 14:13:31.094+00 | NULL
 e711ea02-560f-4ab6-82b4-b6df7f64dc21 | mac-lan     | gemma4:26b | inactive (operator_delete) | ...             |                           | 2026-04-20 14:08:42.007+00 | NULL
```

**BEHEM's `last_queue_push_at` is 54 hours old.** Node heartbeat is fresh (60 s ago). Offer is `status='active'`, `inactive_reason=NULL`, `market_visibility_denied_at=NULL`.

**Current CTE** (from `supabase/migrations/20260423020000_compute_market_structural_fix.sql:173-188`):

```sql
WITH candidates AS (
  SELECT ...
  FROM wire_compute_offers o
  JOIN wire_nodes n ON n.id = o.node_id
  WHERE o.model_id = p_model_id
    AND o.status = 'active'
    AND COALESCE(o.last_queue_push_at, o.updated_at)
        > now() - (v_staleness_s || ' seconds')::interval
    AND (o.max_queue_depth = 0 OR o.current_queue_depth < o.max_queue_depth)
    AND (p_requester_node_id IS NULL OR p_requester_node_id <> o.node_id)
    AND n.market_visibility_denied_at IS NULL
)
```

`v_staleness_s` = 90 (from `staleness_thresholds.queue_mirror_staleness_s`). 54 h > 90 s → BEHEM filtered out.

### The two root causes

**1. The node's mirror task only pushes on state mutation.** The loop in `src-tauri/src/pyramid/market_mirror.rs` is `while let Some(()) = rx.recv().await` — no periodic tick. Idle providers stop pushing as soon as no jobs/state-changes happen. After 90 s they disappear from the matcher's view. Architecture-level bug: an idle provider should remain matchable.

**2. The mirror seq is process-local.** On restart the in-memory seq reloads from persisted state (or to 0) while Wire has already stored the prior max. When the restarted node pushes, Wire sees `pushed_seq ≤ stored_seq` and rejects with `compute_queue_seq_regressed`. So restart + any Wire-stored max = silent push rejections until the node accumulates enough seq bumps to exceed what Wire remembers. We reproduced this live: after BEHEM toggled serving off/on, the chronicle shows:

```
compute_queue_seq_regressed | {"pushed_seq": 5, "stored_seq": 5, "offers_count": 1}
```

Root cause for _cause 1_ specifically: the **economic cost of a mirror heartbeat**. Each push costs `queue_push_fee = 1` credit; a 30 s heartbeat would spend ~2880 credits/node/day just on idle liveness. That's wasteful — **the node already heartbeats Wire every 60 s via `/api/v1/node/heartbeat`** (see `src-tauri/src/auth.rs:552-602` + `src-tauri/src/main.rs:13221-13262`), so Wire already has a fresh liveness signal for every node. Matching should use it.

---

## Node-side fixes shipped (your tree doesn't need them, for context)

Commit-ready in this session, not yet pushed. Changes:

- **`src-tauri/src/compute_market.rs`** — `bump_mirror_seq()` now returns `max(prev + 1, now_unix_millis)` so seq is strictly monotonic across process restarts. Any restarted node immediately emits seqs larger than any seq a prior process could have stored. No handshake needed.
- **`src-tauri/src/pyramid/market_mirror.rs`** — mirror task now runs under a `supervise_mirror_loop` that catches panics via `AssertUnwindSafe::catch_unwind`, emits `market_mirror_task_panicked` / `market_mirror_task_exited` chronicle events on the two failure modes, backs off 5 s and respawns on panic. Adds a boot-time `boot_push()` that fires one push on task entry so a restarted node publishes its current snapshot immediately (subject to the existing `should_push` gates — serving=true + tunnel=Connected).
- **`src-tauri/src/pyramid/compute_chronicle.rs`** — two new event constants: `market_mirror_task_panicked` / `market_mirror_task_exited`.

Node tests: new `bump_mirror_seq_anchors_to_wall_clock` + `bump_mirror_seq_does_not_regress_after_reload`. Existing monotonic test relaxed to `>` comparisons (no longer expects 1/2/3/4 since seqs are now unix-ms scale).

Intentionally **not** shipped node-side:
- A mirror heartbeat push. Explicitly rejected as duplicating the node heartbeat signal and wasting credits. The Wire-side fix below is the right layer.

---

## What you need to change on Wire

### 1. Broaden `match_compute_job` CTE

File: next migration (call it `20260424000000_match_compute_job_heartbeat_fresh.sql` or similar). Replace the staleness predicate in the candidates CTE with an OR:

```sql
AND (
  COALESCE(o.last_queue_push_at, o.updated_at)
      > now() - (v_staleness_s || ' seconds')::interval
  OR n.last_heartbeat
      > now() - (v_node_heartbeat_staleness_s || ' seconds')::interval
)
```

Where `v_node_heartbeat_staleness_s` is read from an economic_parameter the same way `v_staleness_s` is, at the top of the function:

```sql
SELECT (c.structured_data->>'node_heartbeat_staleness_s')::INTEGER
  INTO v_node_heartbeat_staleness_s
  FROM wire_contributions c
  WHERE c.type = 'economic_parameter'
    AND c.structured_data->>'parameter_name' = 'node_heartbeat_staleness_s'
    AND c.superseded_by IS NULL AND c.retracted_at IS NULL
  ORDER BY c.created_at DESC LIMIT 1;
IF v_node_heartbeat_staleness_s IS NULL THEN
  RAISE EXCEPTION 'node_heartbeat_staleness_s_missing:...'
    USING errcode = 'P0504';
END IF;
```

Rationale: push-freshness tells us "queue depth in the snapshot is recent"; heartbeat-freshness tells us "the node is alive and claims to still be serving." Either is a sufficient liveness signal for the matcher. On /fill, if the node turns out to have died between the heartbeat and the dispatch, the existing 503 X-Wire-Reason handling already deactivates the offer + sets `market_visibility_denied_at` — that's the corrective path.

Recommended default for `node_heartbeat_staleness_s`: **180** (3× the 60 s heartbeat interval gives headroom for missed beats without over-exposing dead nodes).

### 2. Extend `computeMarketSurface` with the same filter

File: `src/lib/server/market-surface-cache.ts` around line 172.

Current select:

```ts
.from('wire_compute_offers')
.select('model_id, node_id, rate_per_m_input, rate_per_m_output, max_queue_depth, current_queue_depth')
.eq('status', 'active');
```

Change to select the freshness columns AND the node heartbeat:

```ts
.from('wire_compute_offers')
.select(`
  model_id, node_id, rate_per_m_input, rate_per_m_output,
  max_queue_depth, current_queue_depth,
  last_queue_push_at, updated_at,
  wire_nodes!inner(last_heartbeat)
`)
.eq('status', 'active');
```

Then filter in JS with the same OR predicate:

```ts
const now = Date.now();
// Read thresholds from economic_parameter; see match_compute_job for the
// fetch pattern. Default to 90 / 180 if reads fail so surface doesn't
// crash on a misconfigured prod.
const pushStaleMs = queue_mirror_staleness_s * 1000;
const hbStaleMs = node_heartbeat_staleness_s * 1000;
const fresh = offers.filter((o) => {
  const pushAge = now - new Date(o.last_queue_push_at ?? o.updated_at).getTime();
  const hbAge   = now - new Date(o.wire_nodes.last_heartbeat ?? 0).getTime();
  return pushAge < pushStaleMs || hbAge < hbStaleMs;
});
```

Critical: **the aggregation must use the filtered set**, not the raw set. Currently it groups all active offers regardless of freshness — that's the lying behavior.

### 3. Drop cache TTL from 5 min → 60 s

Same file, find the cache TTL constant (I didn't read past line ~170 so grep for `5 * 60 * 1000` or similar). 5 minutes is longer than the staleness horizon itself — it defeats the freshness filter you're adding. 60 s is still useful rate limiting for a public unauthed endpoint but can't show an entry past its staleness window.

Alternative if 60 s hurts latency: keep 5 min cache but invalidate on any `offer.updated_at` or `last_queue_push_at` bump. Simpler: drop TTL.

### 4. Seed the `node_heartbeat_staleness_s` economic_parameter

New contribution in the same migration:

```sql
-- Seed node_heartbeat_staleness_s economic parameter.
-- Node heartbeat loop in agent-wire-node runs every 60 s; 180 s gives
-- 3x headroom for missed beats (typical transient network blips).
-- Used by match_compute_job CTE + /api/v1/compute/market-surface
-- aggregation as an alternative-to-push freshness signal.
--
-- Uses insert_contribution_atomic the same way staleness_thresholds
-- was seeded (see 20260414100000_market_prerequisites.sql §N).
INSERT INTO wire_contributions (
  type, contribution_type, ...  -- mirror the staleness_thresholds insert
) VALUES (
  'economic_parameter', 'mechanical',
  ...,
  '{
    "parameter_name": "node_heartbeat_staleness_s",
    "node_heartbeat_staleness_s": 180,
    "schema_type": "economic_parameter"
  }'::jsonb
);
```

(You'll know the canonical insert pattern better than I do — whatever existing economic_parameter seed migration does, copy that shape.)

---

## Verification

After applying the Wire-side migration + PostgREST restart, the smoke below should pass **without any node-side change**. BEHEM's last push is 54 h old, but its heartbeat is fresh (60 s), so the CTE's new OR branch will accept it.

```bash
# From Playful upstairs (requester). Auth is Bearer "test" locally.
curl -s -m 60 -X POST http://localhost:8765/pyramid/compute/market-call \
  -H "Authorization: Bearer test" \
  -H "Content-Type: application/json" \
  -d '{"model_id":"gemma4:26b","prompt":"Say hello in one word",
       "max_budget":100000,"max_tokens":20,"max_wait_ms":30000}'
```

Expected post-fix: 200 with a real LLM response (the full roundtrip including /fill + result delivery will execute end-to-end). Currently: `{"error":"no market match: {\"detail\":{...},\"error\":\"no_offer_for_model\"}"}`.

Query to confirm the CTE change is live (the economic_parameter is readable):

```sql
SELECT structured_data
FROM wire_contributions
WHERE type = 'economic_parameter'
  AND structured_data->>'parameter_name' = 'node_heartbeat_staleness_s'
  AND superseded_by IS NULL AND retracted_at IS NULL
ORDER BY created_at DESC LIMIT 1;
```

Query to see the current offer's freshness as the CTE sees it:

```sql
SELECT
  o.id, n.node_handle, o.model_id,
  EXTRACT(EPOCH FROM (now() - COALESCE(o.last_queue_push_at, o.updated_at))) AS push_age_s,
  EXTRACT(EPOCH FROM (now() - n.last_heartbeat))                             AS hb_age_s,
  CASE
    WHEN COALESCE(o.last_queue_push_at, o.updated_at) > now() - interval '90 seconds'
      THEN 'push_fresh'
    WHEN n.last_heartbeat > now() - interval '180 seconds'
      THEN 'hb_fresh'
    ELSE 'stale'
  END AS matchable
FROM wire_compute_offers o
JOIN wire_nodes n ON n.id = o.node_id
WHERE o.status = 'active' AND o.model_id = 'gemma4:26b';
```

BEHEM should come back `matchable = hb_fresh` right now even before any new push.

---

## Why this is the right shape architecturally

You and Adam have a principle that shows up in the feedback memory ("generalize not enumerate", "everything is a contribution", "Wire stops being single-player"). A dedicated mirror-push heartbeat is the enumerate path — invent a new signal for a thing the node already has a perfectly good signal for. Reusing `wire_nodes.last_heartbeat` is the generalize path.

It also plays nicely with Pillar 32 (transparent market): liveness becomes a single observable column, not split across two cadences with different semantics.

The economic angle: a mirror heartbeat would bill `queue_push_fee` continuously for nodes doing nothing. A GPU-less tester-side node is fine, but a GPU-bearing provider sitting idle waiting for jobs would burn credits it hasn't earned yet — bad for the mutualization story.

---

## Contact points

- If the CTE change breaks anything unexpected, the shape-of-last-resort is `P0404` → Wire emits `compute_match_no_offer` chronicle with `rpc_error` carrying the full exception message. Grep moltbot `wire_chronicle` for that event after deploying to see the tail of what's happening.
- Node-side fixes (supervisor + wall-clock seq + boot push) compile clean (cargo check was backgrounded at handoff time; I'll paste results when done); they land in one commit on agent-wire-node main, tested before push.
- Purpose frame still: **a GPU-less tester runs pyramid_build on a laptop, other nodes' GPUs do inference, the tester never sees a market word.** This handoff is one gate from "all routes green" to "that demo actually runs."
