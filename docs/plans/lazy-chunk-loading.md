# Plan: Lazy Chunk Loading for Large Corpus Builds

**Status:** Audited (2 rounds, 4 independent auditors, all findings resolved)
**Branch:** research/lens-framework
**Date:** 2026-04-05

## Context

Building a pyramid from 698 docs causes a `StackOverflow` panic because `execute_chain_from` preloads ALL chunk content into `Vec<Value>` before any processing begins. At 6,000-10,000 docs this would also OOM. Chunks are only needed by `source_extract` (the `for_each: $chunks` step) and a post-extraction file path lookup. After extraction, the pipeline works entirely from `pyramid_nodes` in the DB.

## Approach: Replace `Vec<Value>` with `ChunkProvider`

A lightweight struct that holds `count + slug + db_reader` and loads chunk content on-demand. No chunk content is ever held in memory beyond the current dispatch window (concurrency x 1 chunk).

## Changes

### 1. New struct: `ChunkProvider` (in `chain_executor.rs`, NOT `chain_resolve.rs`)

**Location rationale:** `db_read` is a private async helper defined in `chain_executor.rs` (line 133) and duplicated in `build.rs`. `ChunkProvider`'s async methods (`load_content`, `load_header`) need `db_read`. Placing `ChunkProvider` in `chain_executor.rs` avoids extracting `db_read` into a shared module (separate refactor). `ChainContext` in `chain_resolve.rs` imports the type.

```rust
#[derive(Clone)]
pub struct ChunkProvider {
    pub count: i64,
    pub slug: String,
    pub reader: Arc<tokio::sync::Mutex<Connection>>,
}

impl ChunkProvider {
    pub fn len(&self) -> usize { self.count.max(0) as usize }
    pub fn is_empty(&self) -> bool { self.count <= 0 }

    /// Build lightweight stubs: [{"index": 0}, {"index": 1}, ...]
    /// No content loaded — stubs are used for forEach iteration count.
    pub fn stubs(&self) -> Vec<Value> {
        (0..self.count.max(0)).map(|i| json!({"index": i})).collect()
    }

    /// Load a single chunk's full content from DB. Async-safe via db_read pattern.
    pub async fn load_content(&self, index: i64) -> Result<String> {
        let slug = self.slug.clone();
        db_read(&self.reader, move |conn| db::get_chunk(conn, &slug, index))
            .await
            .map(|opt| opt.unwrap_or_default())
    }

    /// Load just the first line of a chunk (for file path header extraction).
    /// Delegates to db::get_chunk_header() — all SQL stays in db.rs.
    pub async fn load_header(&self, index: i64) -> Result<Option<String>> {
        let slug = self.slug.clone();
        db_read(&self.reader, move |conn| db::get_chunk_header(conn, &slug, index))
            .await
    }

    /// In-memory variant for tests.
    pub fn test(items: Vec<Value>) -> Self {
        let conn = Connection::open_in_memory().expect("test db");
        conn.execute_batch("CREATE TABLE pyramid_chunks (
            slug TEXT NOT NULL, chunk_index INTEGER NOT NULL, content TEXT NOT NULL,
            id INTEGER PRIMARY KEY AUTOINCREMENT, batch_id INTEGER, line_count INTEGER, char_count INTEGER,
            UNIQUE(slug, chunk_index)
        )").expect("test schema");
        for (i, item) in items.iter().enumerate() {
            let content = item.get("content").and_then(|v| v.as_str()).unwrap_or("");
            conn.execute(
                "INSERT INTO pyramid_chunks (slug, chunk_index, content) VALUES ('test', ?1, ?2)",
                rusqlite::params![i as i64, content],
            ).expect("test insert");
        }
        Self {
            count: items.len() as i64,
            slug: "test".to_string(),
            reader: Arc::new(tokio::sync::Mutex::new(conn)),
        }
    }

    /// Empty provider (0 chunks).
    pub fn empty() -> Self {
        let conn = Connection::open_in_memory().expect("empty db");
        Self { count: 0, slug: String::new(), reader: Arc::new(tokio::sync::Mutex::new(conn)) }
    }
}
```

### 1b. New DB function: `get_chunk_header` (in `db.rs`)

All SQL stays in `db.rs` — convention compliance per discovery audit.

```rust
pub fn get_chunk_header(conn: &Connection, slug: &str, chunk_index: i64) -> Result<Option<String>> {
    conn.query_row(
        "SELECT SUBSTR(content, 1, 200) FROM pyramid_chunks WHERE slug = ?1 AND chunk_index = ?2",
        rusqlite::params![slug, chunk_index],
        |row| row.get::<_, Option<String>>(0),
    ).map_err(|e| anyhow!("chunk header: {e}"))
}
```

### 2. Update `ChainContext` (`chain_resolve.rs`)
- `chunks: Vec<Value>` -> `chunks: ChunkProvider` (imported from `chain_executor`)
- `new()` takes `ChunkProvider` instead of `Vec<Value>`
- `resolve_ref("$chunks")` returns `self.chunks.stubs()` (lightweight, no content)
- `resolve_ref("$chunks_reversed")` returns reversed stubs
- **Invariant:** `ChunkProvider` is on `ChainContext` for `stubs()`/`len()` access only. Async methods (`load_content`, `load_header`) are called from the executor, never from `ChainContext`'s sync `resolve_ref`/`resolve_value` methods.

### 3. Update `ExecutionState` (`execution_state.rs`)
- `chunks: Vec<Value>` -> `chunks: ChunkProvider`
- `new()` takes `ChunkProvider`
- All `.chunks.len()` calls -> `.chunks.len()` (unchanged, delegates to `count.max(0) as usize`)

### 4. Update preloading site (`chain_executor.rs:3529-3542`)
- Remove the N-iteration loop that loads all content
- Replace with: `let chunks = ChunkProvider { count: num_chunks, slug: slug.to_string(), reader: state.reader.clone() };`

### 5. Content hydration in forEach executor — THREE paths + IR env_map (CRITICAL)

When forEach iterates over `$chunks`, items are stubs without content. Content must be loaded on-demand BEFORE the item is used for resolution or dispatch.

**Hydration helper** (shared by all paths):
```rust
/// Enrich a chunk stub with content from DB. No-op if content already present.
async fn hydrate_chunk_stub(
    item: &mut Value,
    provider: &ChunkProvider,
) -> Result<()> {
    if item.get("content").is_none() {
        if let Some(idx) = item.get("index").and_then(|v| v.as_i64()) {
            let content = provider.load_content(idx).await?;
            item["content"] = Value::String(content);
        }
    }
    Ok(())
}
```

**Path A — Sequential legacy forEach** (`chain_executor.rs:~5442`):
- Between loop iteration start and `ctx.current_item = Some(...)` at line 5519
- Clone the item, call `hydrate_chunk_stub(&mut enriched, &chunks_provider).await?`
- Set `ctx.current_item = Some(enriched)`
- Add debug assertion: `debug_assert!(enriched.get("content").is_some(), "chunk hydration must happen before current_item assignment")`
- **`split_chunk()` dependency:** `split_chunk` at line 5534 reads `content` from `resolved_input`. Since `$item.content` resolves through the hydrated `ctx.current_item`, the resolved input will have content. Ordering is: hydrate -> set current_item -> resolve_value -> split_chunk.

**Path B — Concurrent legacy forEach** (`chain_executor.rs:~5953`):
- In the prep loop, immediately after selecting the item from the stubs vector, clone and hydrate it
- The enriched item then flows into `ctx.current_item` and `resolved_input` as normal
- `execute_for_each_work_item` (line 6338) is a sub-function of Path B that receives pre-resolved inputs — no separate hydration needed
- Content loading happens sequentially in the prep loop (acceptable: 6s for 6K chunks, masked by LLM latency)

**Path C — IR executor forEach** (`chain_executor.rs:~9937`):
- **DIFFERENT RESOLUTION MECHANISM:** The IR path does NOT use `ctx.current_item` for variable resolution. It builds an `env_map` (line 9954-9973) and resolves input via `resolve_refs_in_value(&step.input, &env_value)`.
- Hydration must happen to the `item` variable BEFORE it is inserted into `item_state_outputs["item"]` at line 9954
- The enriched item must flow into both `item_state_outputs["item"]` AND `ctx.current_item`
- Pattern:
```rust
let mut enriched = item.clone();
hydrate_chunk_stub(&mut enriched, &chunks_provider).await?;
item_state_outputs.insert("item".to_string(), enriched.clone());
// ... build env_map with enriched item ...
```

**The ChunkProvider reference** must be accessible in all paths. Pass it as a parameter to the forEach functions (they already receive `&PyramidState` which has `reader`; the provider adds `slug` and `count`).

**Hydration MUST use the stub's `"index"` field, not the loop counter**, because `$chunks_reversed` makes them differ.

### 6. File path extraction (`chain_executor.rs:3904-3928`) (CRITICAL)

After forEach completes, the code extracts `## FILE: path` from chunk content for stale engine tracking. With stubs, `ctx.chunks.get(i).content` silently returns None and ALL file path mappings fail.

**Fix:** Use `ChunkProvider::load_header()` which delegates to `db::get_chunk_header()`:
```rust
// INVARIANT: outputs[i] corresponds to chunk index i (guaranteed for forward $chunks iteration)
let file_path = if let Ok(Some(header)) = chunks_provider.load_header(i as i64).await {
    header.lines().next().and_then(|first_line| {
        first_line.strip_prefix("## FILE: ")
            .or_else(|| first_line.strip_prefix("## DOCUMENT: "))
            .map(|p| p.to_string())
    })
} else {
    None
};
```

**Note:** This assumes 1:1 correspondence between output index `i` and chunk index `i`. This holds for forward `$chunks` iteration. For `$chunks_reversed`, file path extraction does not run (reversed steps don't save depth-0 nodes). Document this invariant.

### 7. Update env map builders (3 locations: lines 8432, 9966, 10421)
- `Value::Array(state.chunks.clone())` -> `Value::Array(state.chunks.stubs())`
- These feed expression evaluation. No current chain YAML accesses `$chunks[i].content` in expressions (verified by 4 auditors across 2 rounds).
- Add comment: `// INVARIANT: env map chunks are stubs (index only). Content access requires forEach hydration.`

### 8. Update context conversion (`chain_executor.rs:8494`)
- `ChainContext::new(..., state.chunks.clone())` -> pass `ChunkProvider` directly (it's Clone via Arc)

### 9. Update IR pipeline chunk loading (`chain_executor.rs:~9464`)
- Same as Change #4: replace preloading loop with `ChunkProvider { count, slug, reader }`
- Preserve the question-pipeline zero-chunk tolerance (line 3520-3526 logic): question pipelines can proceed with 0 chunks. The IR path at line 9460 currently hard-fails on 0 chunks — align it with the legacy path's behavior.

### 10. Update resume output loading (`chain_executor.rs:10561`)
- `exec_state.chunks.len()` -> `exec_state.chunks.len()` (no change needed, `ChunkProvider.len()` returns count from `db::count_chunks`, same source that sized the original Vec)

### 11. Update tests
- `ChainContext::new("slug", "code", vec![...])` -> `ChainContext::new("slug", "code", ChunkProvider::test(vec![...]))`
- `ChunkProvider::test()` creates in-memory SQLite with `pyramid_chunks` table, inserts items, returns provider
- `ChunkProvider::empty()` for tests that don't need chunks
- Tests that set `state.chunks = vec![...]` -> `state.chunks = ChunkProvider::test(vec![...])`

## Workstream Decomposition

### Phase 1 (parallel):
- **WS-A:** `db.rs` — add `get_chunk_header()` (Change 1b)
- **WS-B:** `chain_executor.rs` — add `ChunkProvider` struct + `hydrate_chunk_stub()` + test/empty constructors (Change 1)

### Phase 2 (sequential, depends on Phase 1):
- **WS-C:** Full integration — update `ChainContext` (Change 2), `ExecutionState` (Change 3), remove preload loops (Changes 4, 9), wire hydration into all forEach paths (Change 5), update file path extraction (Change 6), update env maps (Change 7), update context conversion (Change 8), update tests (Change 11)

WS-C must be sequential because it touches all 3 files and every change depends on the ChunkProvider type existing.

## Concurrency Design Note

The reader mutex (`Arc<tokio::sync::Mutex<Connection>>`) is shared across all DB reads. With concurrency=4 in forEach, chunk loads serialize through the mutex. At ~1ms per SQLite read, 6,000 chunks = ~6s serial overhead. This is acceptable because:
1. LLM dispatch latency (2-10s per call) dominates — chunk loading is never on the critical path
2. For concurrent forEach (Path B), content is loaded sequentially in the prep loop BEFORE spawning concurrent tasks, so there's no contention between spawned tasks
3. The alternative (per-ChunkProvider connection) adds connection management complexity for negligible gain

## Audit Trail

### Informed Audit (Stage 1) — 2 independent agents
Found 8 issues (2 critical, 3 major, 3 minor). Key findings:
- File path extraction silently breaks with stubs (CRITICAL)
- forEach content hydration needed in 3 paths (CRITICAL)
- Concurrent forEach serializes on reader mutex (MAJOR)
- Test helpers underspecified (MAJOR)

### Discovery Audit (Stage 2) — 2 independent agents
Found 13 issues (1 critical, 5 major, 7 minor). New categories not caught by Stage 1:
- `db_read` is private — ChunkProvider must live in `chain_executor.rs` (MAJOR)
- IR forEach uses `env_map` resolution, not `ctx.current_item` — separate hydration needed (CRITICAL)
- `load_header` SQL should be in `db.rs` (MINOR)
- `split_chunk()` ordering dependency on hydration (MAJOR)

All findings resolved in plan before proceeding to implementation.

## Files Modified

| File | Changes |
|------|---------|
| `src-tauri/src/pyramid/db.rs` | Add `get_chunk_header()` function |
| `src-tauri/src/pyramid/chain_executor.rs` | Add `ChunkProvider` struct + `hydrate_chunk_stub()`, remove preload loops (2), add content hydration (3+1 forEach paths), update file path extraction, update env maps (3), update context conversion, update tests |
| `src-tauri/src/pyramid/chain_resolve.rs` | Import `ChunkProvider`, update `ChainContext.chunks` type, update `resolve_ref` |
| `src-tauri/src/pyramid/execution_state.rs` | Import `ChunkProvider`, update `chunks` type |

## Memory Profile

| Corpus Size | Before (preloaded) | After (lazy) |
|-------------|-------------------|--------------|
| 698 docs | ~35MB in memory, stack overflow | ~0.5MB (stubs only) |
| 6,000 docs | ~300MB, likely OOM | ~0.5MB |
| 10,000 docs | ~500MB, OOM | ~0.5MB |

Peak during extraction: `concurrency x avg_chunk_size` = 4 x 50KB = 200KB (concurrent path loads sequentially in prep loop)

## Verification

1. `cargo check` — compiles clean
2. Build a small pyramid (10 docs) — completes successfully with audit trail
3. Build a medium pyramid (100+ docs) — verify Theatre shows progress
4. Build the 698-doc corpus — no stack overflow, completes
5. Check Inspector Prompt/Response tabs populate for L0 nodes
6. Verify file path extraction works (stale engine tracks source connections after build)
7. Verify `$chunks_reversed` works for conversation chains
