# Enhanced Memory Telemetry Plan — Finding the 55 MB/s Invisible Leak

## What We Know

Phase 1 telemetry proved:
- Normal forEach processing: **flat at 437 MB** across 700 items (no leak)
- Oversized chunk split path triggers **55 MB/s runaway growth** that doesn't stop
- The growth is NOT in `step_outputs` (2 bytes change) or `outputs_vec` (13 KB change during spike)
- The growth is NOT in prompt/response data (total LLM data during spike: ~1 MB)
- The growth continues even after the oversized chunk finishes processing
- Something invisible to our current metrics allocates 55 MB/s indefinitely

## What We Need

Four new capabilities to isolate the leak:

### 1. Counting Allocator — track WHAT the Rust heap is doing

Wrap the global allocator to count every `alloc`, `dealloc`, and `realloc`. This tells us whether the 55 MB/s growth is Rust heap allocations or something else (WebView, mmap, SQLite, native TLS, kernel pages).

**Location:** `src-tauri/src/counting_alloc.rs` — lives in the lib crate (not main.rs) so `mem_telemetry.rs` can import it via `crate::counting_alloc`. The `#[global_allocator]` registration goes in `main.rs` referencing the lib crate's type.

**Implementation:**

```rust
// src-tauri/src/counting_alloc.rs (in the lib crate, next to lib.rs)

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

pub static ALLOCATED: AtomicU64 = AtomicU64::new(0);
pub static FREED: AtomicU64 = AtomicU64::new(0);

pub struct CountingAlloc;

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() {
            ALLOCATED.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        FREED.fetch_add(layout.size() as u64, Ordering::Relaxed);
        System.dealloc(ptr, layout);
    }

    // CRITICAL: override realloc to delegate to System.realloc, preserving
    // in-place resize behavior. The default trait impl does alloc+copy+free
    // which changes the allocation pattern being measured (observer effect).
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_ptr = System.realloc(ptr, layout, new_size);
        if !new_ptr.is_null() {
            FREED.fetch_add(layout.size() as u64, Ordering::Relaxed);
            ALLOCATED.fetch_add(new_size as u64, Ordering::Relaxed);
        }
        // If realloc fails (null return), old allocation still lives — no counter change.
        new_ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc_zeroed(layout);
        if !ptr.is_null() {
            ALLOCATED.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        ptr
    }
}

/// Net bytes: allocated minus freed. This is the Rust heap's logical size.
/// Note: Relaxed ordering means the two loads are not atomic with respect
/// to each other — there's a nanosecond TOCTOU gap. At 10-second sample
/// intervals this is irrelevant. saturating_sub handles the theoretical
/// negative case.
///
/// Relaxed is correct because these are independent monotonic counters used
/// for approximate observation, not synchronization primitives.
pub fn heap_net_bytes() -> u64 {
    let a = ALLOCATED.load(Ordering::Relaxed);
    let f = FREED.load(Ordering::Relaxed);
    a.saturating_sub(f)
}

/// Total bytes ever allocated (monotonically increasing). Useful for
/// computing allocation RATE between samples.
pub fn heap_total_allocated() -> u64 {
    ALLOCATED.load(Ordering::Relaxed)
}
```

**Registration in main.rs:**

```rust
#[global_allocator]
static GLOBAL: wire_node_lib::counting_alloc::CountingAlloc = wire_node_lib::counting_alloc::CountingAlloc;
```

**And in lib.rs:**

```rust
pub mod counting_alloc;
```

**Performance:** ~2 atomic increments per alloc/dealloc/realloc. On M-series, `fetch_add(Relaxed)` is 1-2 ns. Actual allocation rate in a Tauri app with WebView + tokio + serde may be 500K-2M/s during active processing. Worst case: 2M × 2 ns = 4 ms/s overhead — negligible. The `realloc` override preserves in-place resize behavior so allocation patterns are unchanged.

**Gating:** The counting allocator is always active in all builds including release (compile-time choice — `#[global_allocator]` cannot be toggled). This is intentional: diagnostic runs use the same binary as production, no special build required. Overhead is negligible (~4 ms/s worst case). Sampling only reads the atomics when `WIRE_MEM_TELEMETRY=1`. When off, atomics increment but nobody reads them.

**Note on WebView:** On macOS, Tauri's WKWebView allocates through the system malloc, NOT through Rust's `#[global_allocator]`. The counting allocator tracks only Rust-side allocations. If the leak is in the WebView (JavaScript/React accumulating state, leaked event listeners, IPC message retention), `heap_net` will be flat while RSS/phys_footprint grows. The diagnosis table addresses this scenario.

### 2. Physical Footprint metric — match what Force Quit shows

Phase 1 uses `mach_task_basic_info.resident_size`. Force Quit shows `phys_footprint` (includes compressed + swapped pages). `task_vm_info_data_t` is NOT in `libc 0.2.180`, so we use `proc_pid_rusage` with `rusage_info_v3` (which IS in libc).

**Implementation:** Keep existing `mach_task_basic_info` for RSS + virtual (proven working). ADD `proc_pid_rusage` for `phys_footprint`:

```rust
#[cfg(target_os = "macos")]
fn get_phys_footprint() -> u64 {
    use std::mem;
    unsafe {
        let pid = libc::getpid();
        let mut info: libc::rusage_info_v3 = mem::zeroed();
        let ret = libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V3,
            &mut info as *mut _ as *mut libc::rusage_info_t,
        );
        if ret == 0 {
            info.ri_phys_footprint
        } else {
            // Log once on first failure (use AtomicBool gate to avoid spam)
            use std::sync::atomic::AtomicBool;
            static LOGGED: AtomicBool = AtomicBool::new(false);
            if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                tracing::warn!("[MEM-TELEMETRY] proc_pid_rusage failed (ret={ret}), phys_footprint will be 0");
            }
            0
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn get_phys_footprint() -> u64 { 0 }
```

Types verified in `libc 0.2.180`: `rusage_info_v3`, `proc_pid_rusage`, `RUSAGE_INFO_V3`, `ri_phys_footprint`, `rusage_info_t`, `getpid`.

### 3. Enhanced MemSample — new fields + rate tracking state

Add to the existing `MemSample` struct:

```rust
pub struct MemSample {
    // ... existing fields ...
    pub schema_version: u8,            // 2 (Phase 1 was implicitly 1)
    pub phys_footprint_mb: f64,        // matches Force Quit / Activity Monitor
    pub heap_net_mb: f64,              // allocated - freed (logical heap size)
    pub heap_allocated_total_mb: f64,  // total ever allocated (monotonic)
    pub alloc_rate_mb_per_sec: f64,    // (allocated_delta) / time_delta since last sample
    // NOTE: realloc credits FULL new_size to ALLOCATED (not just delta),
    // so alloc_rate overstates under heavy Vec/String growth. Use heap_net_mb
    // growth rate as the true leak signal; alloc_rate shows allocation velocity.
}
```

**Rate computation state** — add to `MemTelemetry`:

```rust
pub struct MemTelemetry {
    // ... existing fields ...
    prev_heap_allocated: u64,
    prev_heap_ts: Instant,
}
```

Initialize `prev_heap_allocated` to `crate::counting_alloc::heap_total_allocated()` in `new()` (so the first sample shows delta from telemetry start, not process start — avoids a false spike from the app's 200+ MB startup allocations). In `noop()`, initialize to 0 (never read). Initialize `prev_heap_ts` to `Instant::now()` in both. In `write_sample`:

```rust
let current_alloc = crate::counting_alloc::heap_total_allocated();
let elapsed = self.prev_heap_ts.elapsed().as_secs_f64();
let alloc_rate = if elapsed > 0.001 {
    (current_alloc.saturating_sub(self.prev_heap_allocated)) as f64 / (1024.0 * 1024.0) / elapsed
} else {
    0.0  // guard against division by near-zero for rapid consecutive samples
};
self.prev_heap_allocated = current_alloc;
self.prev_heap_ts = Instant::now();
```

### 4. Split-path specific instrumentation

**Sequential path (ctx.mem_telemetry has file):** Add force_sample calls directly via a new `force_sample_heap` method:

```rust
/// Force sample with heap counters only — no step_outputs computation.
/// Used inside the split path where step_outputs is irrelevant.
/// Writes the same MemSample struct with step_outputs fields set to 0
/// and outputs fields set to None. The `label` prefix ("split_*")
/// distinguishes these from regular samples in analysis.
pub fn force_sample_heap(&mut self, label: &str, step_name: &str) {
    if self.file.is_none() { return; }
    let (rss, virt) = get_task_info();
    let phys = get_phys_footprint();
    let heap_net = crate::counting_alloc::heap_net_bytes();
    let current_alloc = crate::counting_alloc::heap_total_allocated();
    let elapsed = self.prev_heap_ts.elapsed().as_secs_f64();
    let alloc_rate = if elapsed > 0.001 {
        (current_alloc.saturating_sub(self.prev_heap_allocated)) as f64
            / (1024.0 * 1024.0) / elapsed
    } else { 0.0 };
    self.prev_heap_allocated = current_alloc;
    self.prev_heap_ts = Instant::now();

    let sample = MemSample {
        schema_version: 2,
        ts_ms: self.start.elapsed().as_millis() as u64,
        rss_mb: rss as f64 / (1024.0 * 1024.0),
        virtual_mb: virt as f64 / (1024.0 * 1024.0),
        phys_footprint_mb: phys as f64 / (1024.0 * 1024.0),
        heap_net_mb: heap_net as f64 / (1024.0 * 1024.0),
        heap_allocated_total_mb: current_alloc as f64 / (1024.0 * 1024.0),
        alloc_rate_mb_per_sec: alloc_rate,
        label: label.to_string(),
        step_name: step_name.to_string(),
        item_index: None,
        step_outputs_total_values: 0,  // not relevant inside split
        step_outputs_estimated_bytes: 0,
        outputs_vec_len: None,
        outputs_estimated_bytes: None,
    };
    // ... write JSONL line same as write_sample
}
```

`write_split_telemetry` writes a JSONL line with the **same MemSample schema** plus additional fields inlined:

```rust
/// Write a split telemetry record from a concurrent work task's SplitTelemetry.
/// Produces a MemSample line augmented with split-specific fields.
pub fn write_split_telemetry(&mut self, node_id: &str, step_name: &str, st: &SplitTelemetry) {
    // Writes: standard MemSample fields (current RSS/heap/rate at collector time)
    //   + "split_num_sub": st.num_sub_chunks
    //   + "split_heap_before": st.heap_before_split (MB)
    //   + "split_heap_after_subs": st.heap_after_last_sub (MB)
    //   + "split_heap_after_merge": st.heap_after_merge (MB)
    //   + "split_alloc_before": st.alloc_total_before_split (MB)
    //   + "split_alloc_after": st.alloc_total_after_merge (MB)
    // Uses a separate SplitMemSample struct with #[derive(Serialize)]
    // that extends MemSample's fields. Analysis: jq 'select(.split_num_sub != null)'
}
```

New sampling points (added to existing 10):

11. **Split start** — after `split_chunk()` returns. Label: `"split_start:{step}:{node_id}:{num_sub}"`.
12. **Split sub-chunk done** — after each sub-chunk dispatch. Label: `"split_sub:{step}:{node_id}:{sub_idx}"`. Also capture `sub_results.len()` and estimated bytes of sub_results so far.
13. **Split merge start** — before merge dispatch. Label: `"split_merge_start:{step}:{node_id}"`.
14. **Split merge done** — after merge returns. Label: `"split_merge_done:{step}:{node_id}"`.

**Concurrent path (ctx.mem_telemetry has file: None):** Use `SplitTelemetry` on `ForEachTaskOutcome`:

```rust
struct ForEachTaskOutcome {
    index: usize,
    node_id: String,
    output: Result<Value>,
    sub_failures: i32,
    split_telemetry: Option<SplitTelemetry>,
}

struct SplitTelemetry {
    num_sub_chunks: usize,
    heap_before_split: u64,        // heap_net_bytes() before split starts
    heap_after_last_sub: u64,      // heap_net_bytes() after last sub-chunk dispatch
    heap_after_merge: u64,         // heap_net_bytes() after merge completes
    alloc_total_before_split: u64, // heap_total_allocated() for rate computation
    alloc_total_after_merge: u64,
}
```

**Call site enumeration:** All existing `send(ForEachTaskOutcome { ... })` sites (~16 total) get `split_telemetry: None`. The 3 sites in the concurrent split path that get `Some(...)` are: (a) merge success path (~line 7438), (b) no-merge path (~line 7488), (c) single-result path (~line 7538). Each snapshots `heap_net_bytes()` and `heap_total_allocated()` at before-split, after-last-sub, and after-merge points within the split processing.

**Collector writes split telemetry** via a new method on `MemTelemetry`:

```rust
pub fn write_split_telemetry(&mut self, node_id: &str, step_name: &str, st: &SplitTelemetry) {
    // Writes a JSONL line with split-specific fields + standard timestamps
}
```

The collector calls this when `result.split_telemetry.is_some()`.

**Post-split continued growth instrumentation** — the KEY symptom. Two additional points:

15. **Split collected** — in the collector, when receiving a ForEachTaskOutcome with split_telemetry. Label: `"split_collected:{step}:{node_id}"`. Force sample via `ctx.mem_telemetry`. This shows RSS at the moment the split result arrives at the collector.
16. **Post-split normal** — the next non-split ForEachTaskOutcome after a split one. Label: `"post_split_normal:{step}:{node_id}"`. Force sample. This shows whether growth rate returns to baseline or continues.

**Collector state for point 16:** Add `let mut last_was_split = false;` before the collector loop. After processing each result: if `result.split_telemetry.is_some()`, set `last_was_split = true` and fire point 15. If `last_was_split && result.split_telemetry.is_none()`, fire point 16 and reset to false.

**Important limitation:** SplitTelemetry heap snapshots are PROCESS-GLOBAL, not per-task. With concurrency=10, the deltas include allocations from 9 other concurrent tasks. For clean per-task attribution, run a diagnostic build with `concurrency: 1` on the split-triggering step (or set concurrency_cap to 1 via build_strategy table). The 9-task noise is roughly constant, so the signal from the oversized chunk is still visible as a delta above baseline.

## Diagnosis Table

| Scenario | heap_net | rss | phys_footprint | Diagnosis | Next step |
|---|---|---|---|---|---|
| heap_net grows at 55 MB/s, rss matches | GROWS | GROWS | GROWS | **Rust heap leak** — alloc without dealloc | jemalloc profiling (`MALLOC_CONF=prof:true`) for call-stack attribution |
| heap_net flat, rss grows at 55 MB/s | FLAT | GROWS | GROWS | **Non-GlobalAlloc allocation** — most likely **WebView/WKWebView** (React state, leaked event listeners, IPC payload retention). Also: mmap, SQLite WAL, native TLS buffers. | Run `vmmap <pid> \| grep 'WebKit\|MALLOC\|mmap\|SQLite'` during leak. Check frontend `performance.memory`. Try `pool_max_idle_per_host(0)` on reqwest to rule out HTTP pool. |
| heap_net flat, rss flat, phys_footprint grows | FLAT | FLAT | GROWS | **macOS compression/swap accounting** — not a Rust bug | Investigate WebView memory or reduce concurrent working set |
| split_telemetry shows heap jump during sub-chunk dispatch | JUMP | — | — | **Leak in LLM dispatch for oversized prompts** — HTTP client buffers, reqwest connection pool, response body retention | Instrument reqwest client pool, check keep-alive behavior |
| split_telemetry shows heap jump AFTER merge | JUMP | — | — | **Leak in post-merge processing** — node save, event emission, or result cloning | Narrow to specific post-merge code path |
| growth continues after split_collected but split_telemetry shows no jump | GROWS | GROWS | — | **Leak is triggered by split but lives elsewhere** — something the split path started (background task, leaked Arc, expanded pool) that persists | Check for spawned tasks, Arc reference cycles, connection pool expansion |

**Note:** The counting allocator tracks all Rust heap allocations including tokio runtime, serde, and reqwest internals. It cannot distinguish application code from library code. If heap_net confirms a Rust leak but split-path samples don't isolate it, the next step is jemalloc profiling with call-stack attribution.

## Files Changed

| File | Change |
|---|---|
| `src-tauri/src/counting_alloc.rs` | **New file** — ~50 lines. Counting allocator with alloc/dealloc/realloc/alloc_zeroed overrides. Lives in lib crate. |
| `src-tauri/src/lib.rs` | Add `pub mod counting_alloc;` |
| `src-tauri/src/main.rs` | Add `#[global_allocator] static GLOBAL: wire_node_lib::counting_alloc::CountingAlloc = ...;` |
| `src-tauri/src/pyramid/mem_telemetry.rs` | Add `get_phys_footprint()` via `proc_pid_rusage`. Add `phys_footprint_mb`, `heap_net_mb`, `heap_allocated_total_mb`, `alloc_rate_mb_per_sec`, `schema_version` to `MemSample`. Add `prev_heap_allocated`, `prev_heap_ts` to `MemTelemetry`. Add `force_sample_heap()` method (no step_outputs needed). Add `write_split_telemetry()` method. Read `crate::counting_alloc` atomics in `write_sample`. |
| `src-tauri/src/pyramid/chain_executor.rs` | Add `SplitTelemetry` struct and `split_telemetry: Option<SplitTelemetry>` on `ForEachTaskOutcome`. Sequential split path: 4 `force_sample_heap` calls (points 11-14). Concurrent split path: snapshot heap_net/alloc_total into SplitTelemetry, attach to outcome. Collector: call `write_split_telemetry` when split_telemetry is Some, force sample at points 15-16. |

## Performance Impact

- Counting allocator: ~2 ns per alloc/dealloc/realloc (atomic fetch_add). At 2M allocs/s (worst case): 4 ms/s overhead. Negligible. In-place realloc preserved — no observer effect on allocation patterns.
- `proc_pid_rusage` syscall: ~1 µs per call, alongside existing `mach_task_basic_info`. No change.
- Split-path samples: 4-6 force samples per oversized chunk. 2-5 chunks per build. 8-30 extra samples total. Negligible.
- `ForEachTaskOutcome`: +48 bytes per result (Option<SplitTelemetry>). 800 × 48 = 38 KB. Negligible.

## How to Use

Same as before:
```bash
WIRE_MEM_TELEMETRY=1 /path/to/Wire\ Node.app/Contents/MacOS/Wire\ Node
```

For clean per-task split attribution, set concurrency cap to 1 via the build strategy table or chain override before the diagnostic run.

Analysis after the run:
```bash
# Full timeline with heap tracking:
cat mem-telemetry-*.jsonl | jq -r '[.ts_ms, .rss_mb, .heap_net_mb, .phys_footprint_mb, .alloc_rate_mb_per_sec, .label] | @tsv'

# Split-path snapshots:
cat mem-telemetry-*.jsonl | jq 'select(.label | startswith("split_") or startswith("post_split_"))'

# Complementary macOS diagnostics during the leak:
vmmap <pid> | grep 'MALLOC\|mmap\|SQLite'
```
