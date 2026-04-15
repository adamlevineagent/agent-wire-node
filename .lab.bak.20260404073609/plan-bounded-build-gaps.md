# Plan: Close Bounded Build Gaps

## Current state

`stop_after` and `force_from` are **fully implemented** in:
- `chain_executor.rs` — validates step names, invalidates cache, halts cleanly after named step
- `build_runner.rs` — `run_build_from()` accepts and forwards both params
- `routes.rs` — `handle_build()` parses both from query string, passes to `run_build_from()`

**What works today via HTTP:**
```bash
curl -X POST "localhost:8765/pyramid/my-slug/build?stop_after=l0_doc_extract"
curl -X POST "localhost:8765/pyramid/my-slug/build?stop_after=l0_webbing"
curl -X POST "localhost:8765/pyramid/my-slug/build?stop_after=thread_clustering&force_from=l0_doc_extract"
```

## Gaps to close

### Gap 1: CLI flags (mcp-server/src/cli.ts)

**File:** `mcp-server/src/cli.ts`, lines 422–431

**Current:**
```typescript
case "build": {
    const slug = requireArg(1, "slug");
    const fromDepth = flags["from-depth"];
    const query = fromDepth ? `?from_depth=${fromDepth}` : "";
    output(await pf(`/pyramid/${enc(slug)}/build${query}`, { ... }));
}
```

**Change:** Parse `--stop-after` and `--force-from` flags, append to query string alongside `--from-depth`.

**Result:**
```bash
pyramid-cli build my-slug --stop-after l0_doc_extract
pyramid-cli build my-slug --stop-after l0_webbing
pyramid-cli build my-slug --force-from l0_doc_extract --stop-after thread_clustering
pyramid-cli build my-slug --from-depth 1 --stop-after thread_narrative
```

### Gap 2: Tauri command params (main.rs)

**File:** `src-tauri/src/main.rs`, line 3455

**Current:** `pyramid_build(state, slug)` — calls `run_build()` (no params).

**Change:** Add optional params to the Tauri command signature:
```rust
async fn pyramid_build(
    state: tauri::State<'_, SharedState>,
    slug: String,
    from_depth: Option<i64>,
    stop_after: Option<String>,
    force_from: Option<String>,
) -> Result<BuildStatus, String>
```

Call `run_build_from()` when any param is provided, `run_build()` otherwise.

### Gap 3: Step activity in build status

**Files:** `src/pyramid/types.rs`, `chain_executor.rs`, `routes.rs`

**Current:** `BuildStatus` reports `status`, `progress`, `failures`, `elapsed_seconds`. No per-step breakdown.

**Change:** Add a `steps` field to the build status response:

```json
{
  "status": "complete",
  "progress": { "done": 127, "total": 127 },
  "steps": [
    { "name": "l0_doc_extract", "status": "ran", "elapsed_seconds": 45.2, "items": 127 },
    { "name": "l0_webbing", "status": "ran", "elapsed_seconds": 2.1, "items": 1 },
    { "name": "thread_clustering", "status": "stopped" },
    { "name": "thread_narrative", "status": "stopped" },
    { "name": "l1_webbing", "status": "stopped" },
    { "name": "upper_layer_synthesis", "status": "stopped" },
    { "name": "l2_webbing", "status": "stopped" }
  ]
}
```

Three statuses: `ran` (LLM called), `reused` (hydrated from cache), `stopped` (not reached due to `stop_after`).

**Implementation:** In `chain_executor.rs` main loop, collect a `Vec<StepActivity>` as steps execute. Return it alongside the existing `(apex_id, failure_count)` tuple. Thread it up through `build_runner` → `routes` into the response.

### Gap 4: Transactional force_from invalidation

**File:** `chain_executor.rs`, lines 3537–3544

**Current:** Multiple `DELETE` statements with `.ok()` ignoring errors, no transaction wrapper.

**Change:** Wrap in a single `execute_batch()` or `BEGIN/COMMIT` block. Log errors instead of `.ok()`.

## Build order

1. Gap 1 (CLI flags) — standalone TypeScript change, no Rust
2. Gap 2 (Tauri params) — Rust, standalone
3. Gap 4 (transaction fix) — tiny Rust fix, do alongside Gap 2
4. Gap 3 (step activity reporting) — touches executor, types, routes, build_runner; do last

Gaps 1 and 2 are independent — can be built in parallel. Gap 3 depends on understanding the executor output path.

## Verification

After all gaps are closed, the full workflow should work:

```bash
# Step 1: L0 only
pyramid-cli build my-slug --stop-after l0_doc_extract
pyramid-cli build-status my-slug  # should show l0_doc_extract: ran, rest: stopped

# Step 2: Add webbing (L0 extract reused)
pyramid-cli build my-slug --stop-after l0_webbing
pyramid-cli build-status my-slug  # should show l0_doc_extract: reused, l0_webbing: ran, rest: stopped

# Step 3: Re-extract after prompt change, stop at webbing
pyramid-cli build my-slug --force-from l0_doc_extract --stop-after l0_webbing
pyramid-cli build-status my-slug  # both ran fresh

# Step 4: Add clustering
pyramid-cli build my-slug --stop-after thread_clustering

# Step 5: Full pipeline
pyramid-cli build my-slug
```
