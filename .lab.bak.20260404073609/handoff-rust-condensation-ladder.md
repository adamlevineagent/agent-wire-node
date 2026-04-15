# Handoff: Rust Changes for Condensation Ladder

## What was done (prompt/YAML — complete)

All document pipeline prompts now produce a condensation ladder: `current`, `current_dense`, `current_core` per topic. The pattern is recursion-friendly — same language in `doc_extract.md` (L0), `doc_thread.md` (L1), and `doc_distill.md` (L2+). The guideline: "Carry forward the most important details in high relief, then map out everything below so someone looking for that information can easily find it. Maximize for usefulness of understanding and the fewest number of tokens to achieve it completely and satisfactorily."

Also fixed:
- `doc_web.md` — removed "5-20 edges" and strength band prescriptions (Pillar 37)
- `doc_cluster.md` — removed "10-25 threads" and "2-8 docs per thread" prescriptions (Pillar 37), added dehydration awareness
- `doc_recluster.md` — removed "roughly 12 or fewer" prescription (done earlier)
- `document.yaml` — dehydration config includes new fields, `cluster_item_fields` includes `topics.current_core`
- 7 dead prompts archived to `chains/prompts/document/_archived/`
- All changes deployed to `~/Library/Application Support/wire-node/`

## What needs Rust changes

### 1. Add condensation fields to Topic struct

**File:** `src-tauri/src/pyramid/types.rs:89-96`

Current:
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Topic {
    pub name: String,
    pub current: String,
    pub entities: Vec<String>,
    pub corrections: Vec<Correction>,
    pub decisions: Vec<Decision>,
}
```

Add three fields:
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Topic {
    pub name: String,
    #[serde(default)]
    pub summary: String,
    pub current: String,
    #[serde(default)]
    pub current_dense: String,
    #[serde(default)]
    pub current_core: String,
    pub entities: Vec<String>,
    pub corrections: Vec<Correction>,
    pub decisions: Vec<Decision>,
}
```

All three new fields need `#[serde(default)]` because:
- Existing nodes in the DB don't have them
- Non-document pipelines (code, conversation) don't produce them yet
- The JSON walker parse doesn't guarantee their presence

### 2. No changes needed to chain_dispatch.rs parser

**File:** `src-tauri/src/pyramid/chain_dispatch.rs:344-353`

```rust
let topics: Vec<Topic> = output
    .get("topics")
    .and_then(|t| t.as_array())
    .map(|arr| {
        arr.iter()
            .filter_map(|t| serde_json::from_value(t.clone()).ok())
            .collect()
    })
    .unwrap_or_default();
```

This already deserializes via serde — once the Topic struct has the new fields with `#[serde(default)]`, they'll be picked up automatically. Unknown fields are silently ignored (no `deny_unknown_fields`), so this is backwards-compatible.

### 3. DB storage — verify

The `pyramid_nodes` table stores topics as a JSON string column. The Topic struct is serialized to JSON for storage and deserialized on read. Since serde handles the new fields, storage should work automatically — the JSON will include `current_dense` and `current_core` when present. Existing rows without these fields will deserialize with empty strings via `#[serde(default)]`.

**Verify:** Check `db.rs` around line 1565 where `topics_json` is read. No changes should be needed, but confirm the JSON round-trip works.

### 4. Dehydration implementation — verify

The dehydration config in `document.yaml` now includes:
```yaml
dehydrate:
  - drop: "topics.current"
  - drop: "topics.current_dense"
  - drop: "topics.entities"
  - drop: "topics.summary"
  - drop: "topics.current_core"
  - drop: "topics"
  - drop: "orientation"
```

**Verify:** The dehydration code in the chain executor handles `topics.current_dense` and `topics.current_core` as dot-path field drops. It should work since the pattern is the same as `topics.current`, but confirm the implementation supports arbitrary sub-field names.

### 5. cluster_item_fields — verify

The `document.yaml` now has:
```yaml
cluster_item_fields: ["node_id", "headline", "orientation", "topics.current_core"]
```

**Verify:** The chain executor code that builds the recluster input from `cluster_item_fields` handles the dot-path `topics.current_core` correctly — extracting just the `current_core` sub-field from each topic.

## Scope

This is a small change — add 3 fields with `#[serde(default)]` to one struct. The serde machinery handles everything else. The verifications are just confirming the existing code handles the new field paths in dehydration and cluster_item_fields.

## Separate issue: 19/50 L0 nodes have 0 topics

In the test build `doc-full-fix1`, 19 out of 50 checked L0 nodes had empty topic arrays. This is likely NOT caused by the condensation fields (serde ignores unknown fields). It may be:
- A pre-existing issue (check if the previous build `doc-extract-fix2` had a similar rate)
- A JSON walker/truncation issue on larger outputs
- A heal step that drops topics

Worth investigating separately. The condensation ladder change shouldn't make this worse — the `.ok()` filter_map on line 350 only drops topics that fail to deserialize, and unknown fields don't cause deserialization failures.

## Test plan
1. Add the 3 fields to Topic struct
2. `cargo build` — confirm it compiles
3. Run a fresh document build on `Core Selected Docs`
4. Check that `current_dense` and `current_core` are populated in L0 nodes
5. Check that dehydration drops the fields in the correct order
6. Check that `cluster_item_fields` passes `current_core` to the recluster step
