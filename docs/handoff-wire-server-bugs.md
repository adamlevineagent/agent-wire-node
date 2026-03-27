# Wire Server Bug Fixes — Handoff from Pyramid Audit

> **Date:** March 27, 2026
> **Source:** Stage 2 discovery audit of pyramid contribution tree
> **Priority:** Must fix before first pyramid publishes to Wire
> **Repo:** GoodNewsEveryone

---

## Bug 1: `weightsToSlots` Doesn't Enforce Minimum 1 Slot Per Source

**Severity:** CRITICAL
**File:** `src/lib/server/rotator-arm.ts` lines 70-131
**Spec:** wire-rotator-arm.md — "Minimum 1 slot per source: if you cite it, it gets at least 1 slot"

**Problem:** The all-zero-weights case distributes slots by array position. More critically, the normal path can produce 0 slots for very low-weight sources after rounding. Every cited source MUST get at least 1 slot.

**Fix:**
```typescript
// After computing floors via largest-remainder:
const result = floors.map((s, i) => Math.max(1, s));
// Recalculate: if enforcing min-1 pushed sum > 28, reduce highest allocations
let excess = result.reduce((a, b) => a + b, 0) - TOTAL_SOURCE_SLOTS;
while (excess > 0) {
  // Find the slot with the highest allocation and reduce by 1
  let maxIdx = 0;
  for (let i = 1; i < result.length; i++) {
    if (result[i] > result[maxIdx]) maxIdx = i;
  }
  result[maxIdx]--;
  excess--;
}
```

Also: reject `derived_from` where any weight is exactly 0 after normalization. A weight of 0 means "not derived from this" — don't include it.

**Test:** Submit a contribution with 28 sources where one has weight 0.001. Verify it gets 1 slot, not 0.

---

## Bug 2: >28 Sources Pruned by Array Position, Not Weight

**Severity:** MAJOR
**File:** `src/lib/server/rotator-arm.ts` lines 75-83
**Spec:** pyramid-contribution-tree.md Action 7 — "If >28 KEEP sources, prune to top 28 by weight"

**Problem:** The fast path for ≥28 sources gives 1 slot each to the FIRST 28 entries by array order. A source with weight=0.95 at index 29 gets 0 slots while a source with weight=0.01 at index 3 gets 1 slot.

**Fix:**
```typescript
if (weights.length >= TOTAL_SOURCE_SLOTS) {
  // Sort by weight descending, keeping original indices
  const indexed = weights.map((w, i) => ({ weight: w, index: i }));
  indexed.sort((a, b) => b.weight - a.weight);

  const result = new Array(weights.length).fill(0);
  for (let i = 0; i < TOTAL_SOURCE_SLOTS; i++) {
    result[indexed[i].index] = 1;
  }
  return result;
}
```

**Test:** Submit a contribution with 30 sources. Source at index 29 has weight 0.95, source at index 0 has weight 0.001. Verify index 29 gets a slot and index 0 does not.

---

## Bug 3: Double Weight Normalization

**Severity:** CRITICAL (correctness uncertainty)
**Files:**
- `src/app/api/v1/contribute/route.ts` line 665 (`normalizeWeights` called)
- `src/lib/server/rotator-arm.ts` line 97 (normalized again inside `weightsToSlots`)

**Problem:** Weights are normalized at contribution submit time (sum to 1.0), then normalized AGAIN inside `weightsToSlots`. The second normalization is redundant if the first worked correctly, but could cause floating-point drift.

**Fix:** Remove the normalization from `weightsToSlots`. Add a strict precondition:
```typescript
export function weightsToSlots(weights: number[]): number[] {
  const sum = weights.reduce((a, b) => a + b, 0);
  if (Math.abs(sum - 1.0) > 0.001) {
    throw new Error(`weightsToSlots: input weights must sum to 1.0, got ${sum}`);
  }
  // ... proceed with allocation using weights directly, no re-normalization
}
```

Document: "Weights are normalized exactly once, at contribution submit time, before any further processing."

**Test:** Call `weightsToSlots([0.5, 0.3, 0.2])` — should work. Call `weightsToSlots([5, 3, 2])` — should throw.

---

## Bug 4: No Duplicate Source Detection

**Severity:** MAJOR
**File:** `src/app/api/v1/contribute/route.ts` line 669-680

**Problem:** The same source can appear twice in `derived_from` with different weights. The rotator arm receives two separate slot allocations for the same source ID, citation counts increment twice, and revenue traces the wrong path.

**Fix:** Add deduplication check before `normalizeWeights`:
```typescript
const seenSources = new Set<string>();
for (const entry of data.derived_from) {
  const key = `${entry.source_type}:${entry.source_item_id}`;
  if (seenSources.has(key)) {
    return Response.json({
      error: 'Each source may only be cited once in derived_from',
      param: 'derived_from'
    }, { status: 400 });
  }
  seenSources.add(key);
}
```

**Test:** Submit a contribution citing the same source UUID twice. Should get 400 error.

---

## Bug 5: No Handle-Path Generation in Contribution Endpoint

**Severity:** MAJOR (blocks pyramid publication identity)
**File:** `src/app/api/v1/contribute/route.ts` line 781-806

**Problem:** The contribute route doesn't generate or store handle-paths. Per wire-handle-paths.md, every contribution must get `{handle}/{epoch-day}/{sequence}`. The `handle_path` column exists on `wire_contributions` (migration 20260318200000) but is never populated.

**Fix:**
```typescript
// Before calling insert_contribution_atomic:

// 1. Get agent's handle
const { data: agentData } = await adminClient
  .from('wire_agents')
  .select('handle')
  .eq('id', agentId)
  .single();

// 2. Compute epoch-day (Wire Time = UTC-7, fixed, no DST)
// Wire epoch: 2026-01-01 00:00:00 WT
const now = new Date();
const wireTimeMs = now.getTime() - (7 * 60 * 60 * 1000); // UTC-7
const wireDate = new Date(wireTimeMs);
const wireEpoch = new Date('2026-01-01T07:00:00Z'); // Epoch in UTC
const epochDay = Math.floor((wireDate.getTime() - wireEpoch.getTime()) / (24 * 60 * 60 * 1000));

// 3. Get daily sequence (atomic increment)
const { data: seqData } = await adminClient.rpc('get_next_daily_sequence', {
  p_agent_id: agentId,
  p_epoch_day: epochDay
});

// 4. Construct handle-path
const handlePath = `${agentData.handle}/${epochDay}/${seqData.next_seq}`;

// 5. Pass to RPC
// Add p_handle_path to insert_contribution_atomic parameters
```

The `get_next_daily_sequence` RPC needs to be created if it doesn't exist — it should atomically increment a per-agent-per-day counter.

**Test:** Create two contributions from the same agent on the same day. Verify handle-paths are `handle/N/1` and `handle/N/2`.

---

## Bug 6: Rotator Arm Sequence Collision Handling is Silent

**Severity:** MAJOR (hard to debug in production)
**File:** `src/lib/server/rotator-arm.ts` lines 147-177

**Problem:** When Euclidean rhythm positions collide, the code silently relocates slots to the nearest empty position. No logging, no metrics. In production, you won't know if payment timing is being mangled.

**Fix:**
```typescript
let collisionCount = 0;

// ... in the collision handler:
if (sequence[pos] !== -1) {
  collisionCount++;
  // ... existing nearest-empty-slot logic
}

// After sequence computation:
if (collisionCount > 0) {
  console.warn(`[rotator] ${collisionCount} sequence collisions for contribution ${contributionId}`);
}

// Return metadata
return { sequence, collisionCount };
```

Also add pre-validation:
```typescript
const totalSlots = allocations.reduce((sum, a) => sum + a.count, 0);
if (totalSlots !== TOTAL_SLOTS) {
  throw new Error(`Allocation sum ${totalSlots} !== ${TOTAL_SLOTS}`);
}
```

**Test:** Create a contribution with many sources (20+). Check logs for collision warnings.

---

## Bug 7: No Supersession Loop Detection

**Severity:** MINOR (safety)
**File:** `src/app/api/v1/contribute/route.ts` — validation section

**Problem:** A contribution can `supersede` another contribution, but there's no check for:
- The target already being superseded (single-child enforcement)
- Circular supersession chains (A supersedes B, B supersedes A)

**Fix:** Add before `insert_contribution_atomic`:
```typescript
if (supersedes) {
  // Single-child: target must not already be superseded
  const { data: target } = await adminClient
    .from('wire_contributions')
    .select('superseded_by')
    .eq('id', supersedes)
    .single();

  if (target?.superseded_by) {
    return Response.json({
      error: 'Target contribution is already superseded',
      param: 'supersedes'
    }, { status: 409 });
  }
}
```

Loop detection is lower priority — it requires chain traversal and the current system doesn't allow it structurally. Add the single-child check first.

**Test:** Create contribution A, then B superseding A, then C superseding A. C should get 409.

---

## Bug 8: `edition_item` Source Type Still Accepted

**Severity:** MINOR (cleanup)
**File:** `src/app/api/v1/contribute/route.ts` line 23

**Problem:** `VALID_DERIVED_SOURCE_TYPES` includes `'edition_item'` which is deprecated per wire-handle-paths.md.

**Fix:** Remove `'edition_item'` from the array. Add a migration to rewrite any existing `edition_item` references to `contribution`.

---

## Verification Checklist

After all fixes, run these checks:

- [ ] weightsToSlots([0.001, 0.999]) → both get ≥1 slot
- [ ] weightsToSlots with 30 inputs → top 28 by weight get slots
- [ ] weightsToSlots([0.5, 0.3, 0.2]) → no assertion error
- [ ] weightsToSlots([5, 3, 2]) → throws (not normalized)
- [ ] Duplicate source in derived_from → 400 error
- [ ] Two contributions same agent same day → sequential handle-paths
- [ ] Supersede already-superseded → 409 error
- [ ] edition_item in derived_from → 400 error
- [ ] 20+ sources → collision count logged
