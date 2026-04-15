# Memory Telemetry Plan — Pyramid Build Leak Diagnosis

## Problem

Wire Node uses 251 GB during an 836-chunk pyramid build (`source_extract` step, question pipeline). DB analysis shows only ~35 MB of actual data in memory. The leak source is unknown — we need runtime instrumentation to identify the growth curve and pinpoint the culprit.

## Goal

Add lightweight memory telemetry that logs RSS (resident set size) and key data structure sizes at regular intervals during builds. Output goes to a dedicated JSON-lines file that is written incrementally (survives OOM/SIGKILL crashes). Gated behind an environment variable so it's off by default.

## Design

### New module: `src-tauri/src/pyramid/mem_telemetry.rs`

A single file. Uses macOS `mach` APIs via `libc` for RSS.

**Dependency change:** Add `libc = "0.2"` to `[dependencies]` in `src-tauri/Cargo.toml`. Already in `Cargo.lock` as a transitive dep (zero download), but Rust requires it as a direct dep to use `libc::*`.

**Activation:** Telemetry is off by default. Set `WIRE_MEM_TELEMETRY=1` to enable. When the env var is unset or `data_dir` is `None`, `MemTelemetry::new()` returns a no-op instance (`file: None`, all methods return immediately).

**Core types:**

1. `get_task_info() -> (u64, u64)` — calls `mach_task_basic_info` on macOS to get `(resident_size, virtual_size)` in bytes. Returns `(0, 0)` on non-macOS.

2. `struct MemSample` — a snapshot:
   ```rust
   #[derive(serde::Serialize)]
   pub struct MemSample {
       pub ts_ms: u64,                           // ms since build start
       pub rss_mb: f64,                           // resident set size in MB
       pub virtual_mb: f64,                        // virtual size in MB
       pub label: String,                          // e.g. "forEach_concurrent:source_extract:item:250"
       pub step_name: String,
       pub item_index: Option<i64>,
       pub step_outputs_total_values: usize,          // sum of array lengths for array values in step_outputs, 1 for scalars (more diagnostic than key count alone)
       pub step_outputs_estimated_bytes: usize,     // force_sample only, 0 for interval samples (see Estimation section)
       pub outputs_vec_len: Option<usize>,          // Some(n) inside forEach, None elsewhere
       pub outputs_estimated_bytes: Option<usize>,  // running byte estimate of forEach outputs vec (see Estimation section)
   }
   ```

3. `struct MemTelemetry` — build-scoped collector with incremental file writes:
   ```rust
   pub struct MemTelemetry {
       start: Instant,
       slug: String,
       file: Option<File>,      // raw File, NOT BufWriter (survives SIGKILL — see Rationale)
       last_sample: Instant,
       sample_interval: Duration,  // default 10 seconds
       sample_count: usize,
   }
   ```

   Methods:
   - `noop() -> Self` — returns an inert instance (`file: None`, all methods return immediately). Used in `ChainContext::new()` as the initial value. `Instant` has no `Default`, so we use a named constructor instead of `#[derive(Default)]`.
   - `new(slug: &str, data_dir: Option<&Path>) -> Self` — checks `WIRE_MEM_TELEMETRY` env var. If set and `data_dir` is `Some`, opens the JSONL file (filename uses `SystemTime::now()` for the unix_millis suffix, NOT `Instant`). Otherwise returns no-op instance.
   - `is_active(&self) -> bool` — returns `self.file.is_some()`
   - `maybe_sample(...)` — only writes if `sample_interval` has elapsed AND telemetry is active. Writes one JSON line directly to the file.
   - `force_sample(...)` — always writes (if active). Used at step boundaries. Computes the expensive `step_outputs_estimated_bytes`.

   `Drop` impl: calls `file.sync_all()` on drop to ensure the last write is flushed. This covers normal scope exit, `?` returns, and cancel breaks. (Does NOT survive SIGKILL — but since we use raw `File` not `BufWriter`, prior writes are already on disk.)

   `Clone` impl (manual): returns a no-op shell with `file: None`. Cloned contexts (e.g., concurrent forEach snapshots, container steps) do not write telemetry. Only the original `ctx.mem_telemetry` writes to disk.

   ```rust
   impl Clone for MemTelemetry {
       fn clone(&self) -> Self {
           Self {
               start: self.start,
               slug: self.slug.clone(),
               file: None,  // no-op clone
               last_sample: self.last_sample,
               sample_interval: self.sample_interval,
               sample_count: 0,
           }
       }
   }
   ```

### Why raw File, not BufWriter

`BufWriter` holds an 8 KB in-memory buffer and only writes to the OS when the buffer fills or `flush()` is called. When the OS sends SIGKILL (OOM killer), destructors don't run, so the buffer contents are lost. Since we write at most one ~200-byte JSON line every 10 seconds, `BufWriter`'s batching provides no meaningful performance benefit. Raw `File::write_all` goes through the kernel immediately — the data is in the OS page cache (or on disk) even if the process is killed mid-write.

### Size estimation without large allocations

The `step_outputs_estimated_bytes` field must NOT allocate a temporary string proportional to `step_outputs` size. Serializing 836 extraction results to a String to measure `.len()` could allocate 100+ MB, worsening the very problem being diagnosed.

Instead, use a counting writer:
```rust
struct CountingWriter(usize);
impl std::io::Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

fn estimate_json_bytes<T: serde::Serialize>(value: &T) -> usize {
    let mut w = CountingWriter(0);
    let _ = serde_json::to_writer(&mut w, value);
    w.0
}
```

This traverses the JSON tree and counts bytes without allocating the output string. Applied to `ctx.step_outputs` on force_sample only.

The `step_outputs_total_values` field is computed as:
```rust
fn count_total_values(step_outputs: &HashMap<String, Value>) -> usize {
    step_outputs.values().map(|v| match v {
        Value::Array(arr) => arr.len(),
        Value::Null => 0,
        _ => 1,
    }).sum()
}
```
This is O(keys) — no tree walk — and reveals the actual data volume. After `source_extract` with 836 items, this returns 836 (not 1 like a key count would).

For the `outputs_estimated_bytes` field (forEach outputs vec), maintain a running accumulator in the concurrent collector loop:

```rust
let mut outputs_estimated_bytes: usize = 0;
// ... in the collector loop, inside Ok(ref output) arm (line ~7581):
outputs_estimated_bytes += estimate_json_bytes(output);  // before the clone into outputs[index]
```

This counts all results placed into the `outputs` Vec, including resumed ones (which are loaded from DB but still occupy memory in the Vec). The accumulator reflects the Vec's true in-memory footprint regardless of whether each result came from an LLM call or a DB resume load. Each individual result is small (one extraction ~2-3 KB), so the per-result `estimate_json_bytes` call is cheap. The running total avoids re-scanning the entire vec on every sample.

For the sequential forEach path, maintain the same pattern: `let mut outputs_estimated_bytes: usize = 0;` before the item loop, increment on each `outputs.push(...)`, and pass as `Some(outputs_estimated_bytes)` to `maybe_sample`.

### Placement: field on ChainContext

Add `mem_telemetry` as a field on `ChainContext`:

```rust
pub struct ChainContext {
    // ... existing fields ...
    pub mem_telemetry: MemTelemetry,
}
```

The `#[derive(Clone)]` on `ChainContext` stays unchanged — it automatically calls `MemTelemetry::clone()` (which returns a no-op shell).

`ChainContext::new()` signature is NOT changed. Instead, `MemTelemetry` is constructed in `execute_chain_from` (where `state.data_dir` is available) and assigned to `ctx.mem_telemetry` immediately after `ChainContext::new()`:

```rust
let mut ctx = ChainContext::new(slug, &chain.content_type, chunks);
ctx.mem_telemetry = MemTelemetry::new(slug, state.data_dir.as_deref());
```

This preserves `ChainContext::new()`'s existing signature. Every function that takes `&mut ChainContext` gets telemetry for free with zero signature changes to the executor functions.

### Sampling points (10 total)

**Force-sampled** (always fire, include `step_outputs_estimated_bytes`):

1. **Chain start** (`execute_chain_from`, after `ctx.mem_telemetry` assignment) — label: `"chain_start"`
2. **Step start** (step loop, after `when` check passes) — label: `"step_start:{step.name}"`
3. **Step end** (step loop, after each step's dispatch returns, before the next iteration) — label: `"step_end:{step.name}"`. This gives clean per-step memory deltas (step_end RSS minus step_start RSS = memory attributable to that step).
4. **Chain end** (`execute_chain_from`, before `drop(writer_tx)`) — label: `"chain_end"`. Place this before BOTH `drop(writer_tx)` sites: the normal completion path (~line 4570) AND the abort/error path (~line 4553). The abort path fires on step failure with `ErrorStrategy::Abort` — capturing RSS at failure time is the most diagnostic sample.

**Interval-gated** (every 10s, skip `step_outputs_estimated_bytes`):

5. **Sequential forEach item** (`execute_for_each`) — label: `"forEach:{step.name}:item:{index}"`. **IMPORTANT:** There are 4 separate `done += 1` sites in `execute_for_each` (resume-complete path ~line 6540, split-no-merge ~line 6752, split-merge ~line 6811, normal path ~line 6985). Place the `maybe_sample` call at ALL 4 sites individually. Do NOT try to refactor to a single post-iteration call — 3 of the 4 paths use `continue` so there is no convergence point before the next loop iteration.
6. **Concurrent forEach collector** (`execute_for_each_concurrent`, after `done += 1` in the collector loop ~line 7622) — label: `"forEach_concurrent:{step.name}:item:{done}"`. Pass `outputs_estimated_bytes` from the running accumulator.
7. **Recursive cluster depth** (`execute_recursive_cluster`, after each depth synthesis round) — label: `"recursive_cluster:{step.name}:depth:{d}"`. Like sequential forEach, recursive_cluster has multiple `done += 1` sites (resume, direct synthesis, apex-ready, normal cluster). Place `maybe_sample` at all `done += 1` sites — the interval guard prevents over-sampling.
8. **Pair adjacent item** (`execute_pair_adjacent`, after each pair completes) — label: `"pair_adjacent:{step.name}:pair:{i}"`. Same pattern: multiple `done += 1` sites (resume vs normal), instrument all of them.
9. **Evidence loop layer** (`execute_evidence_loop`, inside the `for layer in evidence_start_layer..=max_layer` loop body, immediately after the `info!(slug, layer, nodes_created, total_nodes, "layer complete")` log at ~line 5611, BEFORE the closing brace of the loop body) — label: `"evidence_loop:layer:{layer}"`. Note: `step_outputs_estimated_bytes` at this site reflects `ctx.step_outputs` but the evidence loop's actual memory consumers are local structures (question trees, layer results), not step_outputs. RSS delta between layer samples is the main diagnostic signal here.

**Caller guidance for outputs fields:**

- `outputs_vec_len` and `outputs_estimated_bytes`: pass `Some(n)` only at forEach sampling sites (points 5 and 6) where an active `outputs` Vec exists. All other sites (chain_start, step_start, step_end, chain_end, recursive_cluster, pair_adjacent, evidence_loop) pass `None` for both fields. This produces `null` in the JSONL, making it unambiguous that the field is not applicable rather than zero.

### Output format

Incremental JSONL file at `~/Library/Application Support/wire-node/mem-telemetry-{slug}-{unix_millis}.jsonl` (millisecond timestamp to avoid same-second collisions):

```json
{"ts_ms":0,"rss_mb":145.2,"virtual_mb":312.8,"label":"chain_start","step_name":"","item_index":null,"step_outputs_keys":0,"step_outputs_estimated_bytes":0,"outputs_vec_len":null,"outputs_estimated_bytes":null}
{"ts_ms":11500,"rss_mb":210.3,"virtual_mb":890.2,"label":"forEach_concurrent:source_extract:item:50","step_name":"source_extract","item_index":50,"step_outputs_keys":0,"step_outputs_estimated_bytes":0,"outputs_vec_len":50,"outputs_estimated_bytes":128400}
```

Each line is written directly via `File::write_all` as it's collected. If the process is OOM-killed at item 800, the file contains all samples up to that point.

### macOS RSS implementation

```rust
#[cfg(target_os = "macos")]
fn get_task_info() -> (u64, u64) {
    use std::mem;
    unsafe {
        // Use mach_task_self_ (the static) instead of mach_task_self()
        // (the wrapper fn), which is deprecated since libc 0.2.55.
        let task = libc::mach_task_self_;
        let mut info: libc::mach_task_basic_info_data_t = mem::zeroed();
        let mut count = libc::MACH_TASK_BASIC_INFO_COUNT;
        let kr = libc::task_info(
            task,
            libc::MACH_TASK_BASIC_INFO,
            &mut info as *mut _ as *mut _,
            &mut count,
        );
        if kr == libc::KERN_SUCCESS {
            (info.resident_size as u64, info.virtual_size as u64)
        } else {
            (0, 0)
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn get_task_info() -> (u64, u64) { (0, 0) }
```

Types verified present in `libc 0.2.180` (the version already in `Cargo.lock`).

### Error handling

All telemetry operations are fire-and-forget:
- Env var unset: no-op telemetry, zero overhead
- File open failure at construction: log `warn!`, set `file: None`, all subsequent calls are no-ops
- Write failure: log `warn!`, continue (never panic, never propagate errors)
- Serialization failure: skip the sample, log `warn!`
- `data_dir` is `None`: no-op telemetry (tests, pre-init)

### Known limitations (documented, not fixed)

- **RSS is process-wide:** Concurrent builds on different slugs see each other's memory. Run diagnostic builds one at a time.
- **Concurrent forEach blind spot:** Spawned work tasks cannot sample telemetry (would require Send/Sync). Only the collector sees results. Memory growth inside individual LLM calls is invisible between collector samples. The `outputs_estimated_bytes` running counter partially compensates.
- **Container and loop steps have no-op telemetry** from their cloned ChainContext. RSS growth during container/loop execution is visible at the next force_sample (step boundary) but per-item visibility inside them is absent. If this is the leak path, add inner-step sampling in a follow-up.
- **invoke_chain creates a child ChainContext** with its own MemTelemetry instance and JSONL file. Parent and child telemetry are separate files. Correlate by timestamp overlap.
- **IR executor path (`execution_state.rs`) is not instrumented.** The leaking builds use the primary `execute_chain_from` path.
- **No automatic file cleanup.** Telemetry files accumulate in the data dir. Delete manually after analysis.
- **Copy-on-write risk:** `step_outputs` is `Arc<HashMap>`. If concurrent tasks call `Arc::make_mut`, each gets a full deep copy. Watch for `step_outputs_estimated_bytes` growing across steps — this could indicate a memory multiplier.

## Files changed

| File | Change |
|---|---|
| `src-tauri/Cargo.toml` | Add `libc = "0.2"` to `[dependencies]` |
| `src-tauri/src/pyramid/mem_telemetry.rs` | **New file** — ~180 lines |
| `src-tauri/src/pyramid/mod.rs` | Add `pub mod mem_telemetry;` |
| `src-tauri/src/pyramid/chain_resolve.rs` | Add `pub mem_telemetry: MemTelemetry` field to `ChainContext`. `#[derive(Clone)]` stays — MemTelemetry has manual Clone. `ChainContext::new()` signature unchanged (initializes with `MemTelemetry::noop()` — see below). |
| `src-tauri/src/pyramid/chain_executor.rs` | Assign `ctx.mem_telemetry` in `execute_chain_from`. 10 sampling call sites (4 force + 5 interval-gated + caller guidance, including all 4 `done += 1` sites in sequential forEach). Step_end force sample after each step dispatch. Chain_end force sample at both normal and abort `drop(writer_tx)` sites. Running `outputs_estimated_bytes` accumulator in both sequential and concurrent forEach. |

## How to use

Enable telemetry:
```bash
WIRE_MEM_TELEMETRY=1 /path/to/Wire\ Node.app/Contents/MacOS/Wire\ Node
```

During a build:
```bash
tail -f ~/Library/Application\ Support/wire-node/mem-telemetry-*.jsonl
```

After a build (or crash):
```bash
cat ~/Library/Application\ Support/wire-node/mem-telemetry-*.jsonl | jq -r '[.ts_ms, .rss_mb, .label] | @tsv'
```

Feed the file to Claude for analysis. The growth curve shape tells us:
- **Linear growth** with item count → per-item leak (data accumulation)
- **Exponential/superlinear** → multiplicative leak (clone of growing data, Arc::make_mut deep copies)
- **Staircase jumps** → step-boundary issue (step_outputs)
- **Flat with spike at end** → transient allocation pressure / fragmentation
- **outputs_estimated_bytes growing faster than outputs_vec_len** → individual outputs are getting larger (running_context accumulation or similar)
