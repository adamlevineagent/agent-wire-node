# forEach Scale Fix — Audit Findings & Implementation Plan

## Problem
`execute_for_each_concurrent()` in `chain_executor.rs` crashes the server at 699 chunks.
- 26 chunks: 57s OK
- 91 chunks: 45s OK
- 699 chunks: crash after ~70s, zero DB writes

## Root Causes (confirmed via 2 audit cycles, 8 independent auditors)

### Primary (crash)
1. **Pending Vec memory bomb** — Sequential prep loop materializes all items (~350MB) before dispatch
2. **O(N²) context clone** — `(*ctx_snapshot).clone()` per item deep-copies full step_outputs HashMap
3. **Bounded channel deadlock** — Result channel fills, tasks block holding semaphore permits, collector hasn't started

### Secondary (throughput)
4. **reqwest::Client per LLM call** — No HTTP connection reuse, wasted TLS handshakes
5. **700 sequential DB calls** for resume state checking
6. **Write drain backpressure** — 256-slot channel + per-write Mutex = cascading stall
7. **Global rate limiter** — 4 req/5s ceiling serializes all concurrent work

### Tertiary (resilience)
8. **No step-level checkpoints** — Must re-scan all items on resume
9. **step_outputs accumulates unboundedly** across chain steps
10. **Task panics silently swallowed**

## Implementation Plan

### Tier 1 — Fixes the crash

#### 1. ResolveView struct (load-bearing refactor)
`resolve_value`/`resolve_ref` are `&self` on ChainContext reading `self.current_item`/`self.current_index`.
Can't share Arc without this change.

Create `ResolveView { ctx: &ChainContext, current_item: Option<&Value>, current_index: Option<usize> }`.
Implement resolve methods on it. ~15 call sites change. Eliminates per-item deep clone entirely.

#### 2. Streaming dispatch (producer as separate task)
- Producer task iterates items, acquires semaphore, spawns work tasks
- Sends ALL outcomes (resumed + dispatched) through bounded channel (concurrency * 4)
- Collector loop is sole writer to outputs[], done, failures
- Producer JoinHandle awaited after collector, errors propagated

#### 3. Cancel via tokio::select!
```rust
tokio::select! {
    permit = semaphore.clone().acquire_owned() => { /* spawn */ }
    _ = cancel.cancelled() => { break; }
}
```
Abort path calls `cancel.cancel()` to stop producer + all tasks.

#### 4. Oversized splitting spawned as tasks
Reversal from Cycle 1. Oversized items hold semaphore for full split+merge duration.
This is correct — semaphore exists to limit concurrent LLM calls. Producer stays unblocked.

#### 5. Batch resume state
Single LEFT JOIN query:
```sql
SELECT pps.chunk_index, pps.node_id, pps.output_json,
       CASE WHEN pn.id IS NOT NULL THEN 1 ELSE 0 END as node_exists
FROM pyramid_pipeline_steps pps
LEFT JOIN pyramid_nodes pn ON pn.slug = pps.slug AND pn.id = pps.node_id
WHERE pps.slug = ? AND pps.step_type = ? AND pps.depth = ?
```
Also batch `load_prior_step_output` for resumed items.

#### 6. Static reqwest::Client
One-line fix: `static CLIENT: LazyLock<reqwest::Client>` in llm.rs.

### Tier 2 — Throughput + resilience

#### 7. Write drain batching
Accumulate writes, execute in single BEGIN/COMMIT transaction.

#### 8. Step-level checkpoint sentinels
Sentinel row: `chunk_index=-1, depth=<step_depth>, node_id="__STEP_COMPLETE__"`.
For forEach steps, reconstruct outputs from bulk query on resume.
force_from deletion naturally clears sentinels.

#### 9. step_outputs eviction
At chain load time, build `HashMap<step_name, last_referenced_by_step_index>` by scanning
all $refs in input/for_each/when/zip_steps/instruction fields.
After step N, evict outputs where `last_referenced < N`.
Conservative: never evict if unsure (dynamic refs in templates).

#### 10. Task panic logging
Check `JoinError::is_panic()` after collection. Ignore `is_cancelled()`.

#### 11. Sub-chunk retry cap
Max 1 retry for split sub-chunks.

### Tier 3 — Follow-up
- Connection pool for reader (measure after batch resume)
- Rate limiter architecture (per-build or token-bucket)
- active_build cleanup on terminal state
- log_cost routing through write drain

## Key Auditor Agreements (high confidence)
- ResolveView approach: 4/4 informed auditors
- Oversized in tasks (not producer): 4/4 across both cycles
- reqwest::Client reuse: 2/2 discovery auditors independently
- Write drain batching: 2/2 discovery auditors independently
- Bounded channel (not unbounded): corrected in Cycle 2
