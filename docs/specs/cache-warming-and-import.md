# Cache Warming on Pyramid Import Specification

**Version:** 1.0
**Date:** 2026-04-09
**Status:** Design — pre-implementation
**Depends on:** LLM output cache (for `pyramid_step_cache` schema + content-addressable keys), Wire publish/pull infrastructure, DADBEAR (for post-import source watching)
**Unblocks:** Cheap pyramid imports, cross-node intelligence sharing, reproducible builds from shared caches
**Authors:** Adam Levine, Claude (session design partner)

---

## Overview

Cloning, forking, or importing a pyramid by its nature should know if it's stale based on the underlying files. To the extent it's not stale, it should not be regenerated. The LLM output cache is content-addressable — same inputs + same prompt + same model always produce the same cache key. That means a cache entry produced by another node is just as valid for this node, as long as the source file that fed it still matches.

This spec defines how imported pyramids populate the local cache from the source node's cache manifest and how staleness is determined per-node at import time so that only the truly-stale work runs fresh.

---

## Problem

When a user imports a pyramid from Wire (via ToolsMode Discover → Pull), the naive path is "run a full build from scratch" against the imported source files. This wastes LLM calls on work that's already been done by the source pyramid. If the source files are the same locally, the outputs should be valid too.

A 112-node L0 pyramid with a full upper-layer build represents hundreds of LLM calls and dollars of spend. Forcing every importer to redo that work is:

- **Wasteful**: the cache entries already exist on the source node
- **Slow**: the importer waits for LLM calls they don't need
- **Anti-cooperative**: Wire's whole point is that intelligence is shared; cache warming is the mechanical expression of that
- **Wrong-by-default**: the user's mental model is "I pulled this pyramid, it should just be there"

---

## Insight

The LLM output cache uses content-addressable keys:

```
cache_key = hash(inputs_content_hash, prompt_hash, model_id)
```

If the source file content hasn't changed, `inputs_content_hash` is the same. If the prompt hasn't changed, `prompt_hash` is the same. If the model hasn't changed, `model_id` is the same. Therefore the cache key is the same — and the output for that key is deterministic (up to LLM sampling, which is what the cache captures in the first place).

This means imported cache entries are not "trusted" or "untrusted" — they're content-addressable. Either the key matches the local computation and the entry is valid, or the key doesn't match and the entry is ignored. There is no middle ground and no trust decision to make.

Imported pyramids can publish their cache entries (or be queried for them) so the local cache is populated from the import. Source file hashes determine which entries are still valid once imported.

---

## Import Flow

```
1. User clicks "Pull" on a Wire pyramid in ToolsMode Discover
2. Backend calls pyramid_import_pyramid with:
     wire_pyramid_id + target_slug + source_path
3. Backend downloads from the source node (via its tunnel URL):
     a. Node metadata (L0 + upper layers)
     b. Evidence graph
     c. Cache manifest (per-node cache entries)
4. For each imported cache entry:
     a. Resolve the referenced source file at the new local source_path
     b. If file missing -> mark node + dependents stale, skip cache entry
     c. If file hash mismatch -> mark node + dependents stale, skip cache entry
     d. If file hash matches -> insert cache entry as-is into local pyramid_step_cache
5. Upper-layer cache entries:
     a. Valid only if ALL their L0 ancestors have valid (hash-matching) cache entries
     b. Walk the evidence graph upward from stale L0s, mark every dependent stale
     c. Stale upper-layer entries are NOT inserted into the local cache
6. The imported pyramid is added to the local DB with the populated cache
7. DADBEAR is enabled on the imported pyramid with source_path set to local path
8. Running a build on the imported pyramid uses the cache:
     a. Steps with valid entries = instant cache hits
     b. Steps without entries = run fresh (the stale subset)
     c. Build viz surfaces the cache hit rate prominently
```

The import is NOT a build. It's a data transfer plus a staleness pass. The first real build after import is where cache hits manifest as "did nothing, already done" for the matching subset and fresh LLM calls for the stale subset.

---

## Staleness Check Algorithm

```
populate_from_import(import_manifest, local_source_root):
  stale_l0_nodes = set()
  
  # Pass 1: L0 staleness by file hash
  for node in import_manifest.nodes where node.layer == 0:
    local_path = resolve_source_path(local_source_root, node.source_path)
    if not exists(local_path):
      stale_l0_nodes.add(node.id)
      continue
    local_hash = compute_file_hash(local_path)  # SHA-256 of file content
    if local_hash != node.source_hash:
      stale_l0_nodes.add(node.id)
      continue
    # Source matches -- L0 cache entries for this node are valid
    populate_cache_from_node(node)
  
  # Pass 2: walk evidence graph upward from stale L0s
  stale_upper_nodes = propagate_stale(import_manifest.evidence_graph, stale_l0_nodes)
  
  # Pass 3: upper-layer cache entries
  for node in import_manifest.nodes where node.layer > 0:
    if node.id in stale_upper_nodes:
      continue  # do NOT populate cache for stale upper nodes
    # All L0 ancestors of this upper node are valid
    populate_cache_from_node(node)
  
  return ImportReport {
    cache_entries_valid: count populated,
    cache_entries_stale: count skipped,
    nodes_needing_rebuild: |stale_l0_nodes| + |stale_upper_nodes|,
  }
```

### Dependency Propagation

When an L0 node is marked stale (source mismatch), all upper-layer nodes that derive from it are also marked stale. The propagation uses the evidence graph:

```
propagate_stale(evidence_graph, stale_l0_set):
  stale = set(stale_l0_set)
  frontier = set(stale_l0_set)
  while frontier not empty:
    next_frontier = set()
    for node_id in frontier:
      for dependent in evidence_graph.dependents_of(node_id):
        if dependent not in stale:
          stale.add(dependent)
          next_frontier.add(dependent)
    frontier = next_frontier
  return stale
```

Upper-layer cache entries are only populated if EVERY L0 ancestor is valid. A single stale L0 in the transitive closure invalidates every upper node that touches it. This is conservative by design: a synthesis that depended on a now-stale source cannot be trusted even if the synthesis step itself is content-addressable over its own inputs — the inputs came from a stale source.

### Why Not Check Upper Layers by Cache Key?

An upper-layer node's cache key is `hash(synthesis_input_hash, prompt_hash, model_id)`. The synthesis input includes the child node outputs, which are themselves upper layer or L0. If we trusted the cache key alone, we could "prove" an upper layer is valid without checking its ancestors.

This is rejected because:

1. **The import manifest might be lying or stale.** We don't trust the source node's claim about its own inputs.
2. **The L0 cache entries might already be stale** (source files changed since the manifest was produced), so the upper-layer inputs would be wrong.
3. **The check is cheap.** Walking the evidence graph is O(nodes), and file hashes are already computed for the L0 pass.

The L0-first, walk-upward approach is the authoritative answer. Cache keys on upper layers are a correctness check after population, not a shortcut for skipping validation.

---

## Cache Manifest Format

The exported pyramid includes a cache manifest per node:

```json
{
  "manifest_version": 1,
  "source_pyramid_id": "wire:pyr_abc123",
  "exported_at": "2026-04-09T15:30:00Z",
  "nodes": [
    {
      "node_id": "C-L0-001",
      "layer": 0,
      "source_path": "src/main.rs",
      "source_hash": "sha256:abc123...",
      "source_size_bytes": 14321,
      "cache_entries": [
        {
          "step_name": "source_extract",
          "cache_key": "sha256:key1...",
          "inputs_hash": "sha256:inputs1...",
          "prompt_hash": "sha256:prompt1...",
          "model_id": "inception/mercury-2",
          "output_json": "{...llm response...}",
          "token_usage_json": "{\"prompt_tokens\":1100,\"completion_tokens\":1050}",
          "tokens_used": 2150,
          "cost_usd": 0.002,
          "latency_ms": 3400,
          "created_at": "2026-04-08T12:14:30Z"
        }
      ]
    },
    {
      "node_id": "C-L1-007",
      "layer": 1,
      "derived_from": ["C-L0-001", "C-L0-002", "C-L0-005"],
      "cache_entries": [
        {
          "step_name": "cluster_synthesize",
          "cache_key": "sha256:key7...",
          "inputs_hash": "sha256:inputs7...",
          "prompt_hash": "sha256:prompt7...",
          "model_id": "claude-sonnet-4-20250514",
          "output_json": "{...}",
          "tokens_used": 8420,
          "cost_usd": 0.011,
          "latency_ms": 5100
        }
      ]
    }
  ]
}
```

### Per-Node Fields

| Field | Purpose |
|-------|---------|
| `node_id` | Original node ID from source pyramid (preserved, not renumbered) |
| `layer` | 0 for L0 nodes, 1+ for upper layers |
| `source_path` | Relative path from pyramid source root (L0 only) |
| `source_hash` | SHA-256 of source file content at time of export (L0 only) |
| `source_size_bytes` | Quick sanity check before hashing (L0 only) |
| `derived_from` | Array of ancestor node IDs (upper layers only); feeds the evidence graph |
| `cache_entries` | Array of cache entries for steps that ran against this node |

### Per-Cache-Entry Fields

All fields correspond 1:1 to `pyramid_step_cache` columns. The manifest is a direct export of cache entries without transformation — content-addressable means the rows can be lifted and dropped without re-keying.

---

## Publication Side

When a pyramid is published to Wire (see existing `wire_publish.rs`), the cache manifest is included alongside the node data. This means publishing is "heavier" (more bytes to upload) but importing is much cheaper (no LLM calls for the matching subset).

### Publish Extension

`wire_publish.rs::publish_pyramid()` gains a new step after node upload:

```
1. Upload node metadata + evidence graph (existing)
2. Upload source document references (existing)
3. NEW: Build cache manifest from pyramid_step_cache
4. NEW: Upload cache manifest as a linked payload
```

The cache manifest is built by querying `pyramid_step_cache` for the latest build of the published pyramid, grouped by node_id:

```sql
SELECT 
  psc.step_name,
  psc.cache_key,
  psc.inputs_hash,
  psc.prompt_hash,
  psc.model_id,
  psc.output_json,
  psc.token_usage_json,
  psc.cost_usd,
  psc.latency_ms,
  psc.created_at,
  ps.node_id,
  ps.layer,
  ps.source_path,
  ps.source_hash
FROM pyramid_step_cache psc
JOIN pyramid_pipeline_steps ps USING (slug, step_name, chunk_index, depth)
WHERE psc.slug = ?1 AND psc.build_id = ?2
ORDER BY ps.layer ASC, ps.node_id ASC;
```

The resulting rows are grouped by `node_id` and serialized into the manifest JSON.

### Manifest Size

For a 112-L0 pyramid with ~5 cache entries per node across layers, the manifest is ~560-2000 entries. Each entry's `output_json` is typically 2-20 KB. Expected total: 5-40 MB uncompressed, 1-8 MB gzipped. This is well within Wire's existing contribution size budgets for corpus documents.

---

## Privacy Consideration

Cache contents may include sensitive data from source files. Publishing a cache manifest effectively exposes the extraction output — which, for a code or document pyramid, is a summary or interpretation of the source content. A private codebase or confidential document pyramid should not leak its cache.

### Default Policy

The cache manifest is included only for **public-source pyramids** — those where the source documents are themselves public corpus documents (e.g., open source code, published papers, public web scrapes). Private or circle-scoped pyramids do NOT publish cache manifests.

### Detection

A pyramid is public-source if all its L0 nodes reference corpus documents whose visibility is `public`. If any L0 node references a private or circle-scoped corpus document, the cache manifest is withheld entirely. This is all-or-nothing per pyramid — no partial manifests, since a synthesis might blend public and private inputs in ways that leak private data through the upper layers.

### Override

A user can explicitly opt in via a checkbox on the publish flow: "Include cache manifest (publishes LLM outputs for this pyramid)". The checkbox is:

- Default OFF for circle-scoped pyramids
- Default OFF for private pyramids
- Default ON for public-source pyramids
- With a warning if the user opts in to publishing a cache for a non-public pyramid

### Consumption Side

Importers receive whatever manifest the source chose to publish. If no manifest is present, the import falls back to "empty cache, rebuild everything on first build." The importer's import flow works identically in both cases — a missing manifest is just a manifest with zero entries.

---

## Import Resumability

If the import is interrupted (network failure, user cancellation, process crash), a partial state is saved and the next import attempt picks up where it left off.

### Checkpointing

The importer writes progress to a new table `pyramid_import_state`:

```sql
CREATE TABLE IF NOT EXISTS pyramid_import_state (
    target_slug TEXT PRIMARY KEY,
    wire_pyramid_id TEXT NOT NULL,
    source_path TEXT NOT NULL,
    status TEXT NOT NULL,                 -- 'downloading_manifest', 'validating_sources', 'populating_cache', 'complete', 'failed'
    nodes_total INTEGER,
    nodes_processed INTEGER DEFAULT 0,
    cache_entries_total INTEGER,
    cache_entries_validated INTEGER DEFAULT 0,
    cache_entries_inserted INTEGER DEFAULT 0,
    last_node_id_processed TEXT,          -- resumption cursor
    error_message TEXT,
    started_at TEXT DEFAULT (datetime('now')),
    updated_at TEXT DEFAULT (datetime('now'))
);
```

### Resumption Logic

```
pyramid_import_pyramid(wire_pyramid_id, target_slug, source_path):
  existing = load_import_state(target_slug)
  if existing and existing.status != 'complete':
    if existing.wire_pyramid_id != wire_pyramid_id:
      error("slug already importing a different pyramid")
    # Resume from cursor
    resume_from(existing.last_node_id_processed)
  else:
    # Fresh import
    create_import_state(target_slug, wire_pyramid_id, source_path)
    begin_fresh_import()
```

### Idempotency

Cache entries are content-addressable. Re-importing the same entry is a no-op because `pyramid_step_cache` has `UNIQUE(slug, cache_key)` and the import uses `INSERT OR IGNORE`. A partial import's inserted entries are not duplicated on resume.

### Cleanup

On successful import completion, `pyramid_import_state.status` is set to `'complete'`. On explicit user cancel, the row is deleted along with any partially inserted cache entries and the target slug's DB rows. On crash, the row remains for resume on next launch.

---

## Integration with DADBEAR

After a successful import, DADBEAR is enabled on the imported pyramid with `source_path` set to the local path. This closes the loop:

1. **Import populates the cache** from the source node's manifest.
2. **DADBEAR watches the local source path** for subsequent edits.
3. **If the user edits a source file**, DADBEAR detects the change on the next tick, recomputes the file hash, and marks the L0 node whose `source_path` matches as stale.
4. **Normal stale propagation walks upward** through the evidence graph, marking dependents.
5. **The next build reuses the cache** for unchanged nodes and runs fresh for stale ones — identical to the behavior for a native pyramid.

The imported cache seeds the system but doesn't freeze it. From DADBEAR's perspective there is no distinction between "cache that came from an import" and "cache that was built locally" — it's all just entries in `pyramid_step_cache` with content-addressable keys.

### DADBEAR Enable on Import

At the end of a successful import, the importer writes a row to `pyramid_dadbear_config`:

```sql
INSERT INTO pyramid_dadbear_config (slug, source_path, scan_interval_secs, enabled, ...)
VALUES (?1, ?2, 60, 1, ...);
```

The user can override these defaults in a "DADBEAR settings" step of the import wizard, or leave them at defaults and change later from the DADBEAR oversight page.

---

## IPC Contract

```
POST pyramid_import_pyramid
  Input: {
    wire_pyramid_id: String,
    target_slug: String,
    source_path: String
  }
  Output: {
    imported_nodes: u64,
    cache_entries_valid: u64,
    cache_entries_stale: u64,
    nodes_needing_rebuild: u64
  }

GET pyramid_import_progress
  Input: { target_slug: String }
  Output: {
    status: String,                    -- "downloading_manifest" | "validating_sources" | "populating_cache" | "complete" | "failed"
    progress: f64,                     -- 0.0 to 1.0
    nodes_imported: u64,
    cache_entries_validated: u64
  }

POST pyramid_import_cancel
  Input: { target_slug: String }
  Output: { cancelled: bool, partial_rollback: bool }
```

### Progress Semantics

`pyramid_import_progress` is polled by the frontend during the import. Progress is computed as:

```
progress = (nodes_processed / nodes_total) * 0.5
         + (cache_entries_validated / cache_entries_total) * 0.5
```

Nodes and cache entries are weighted equally since cache entry validation dominates CPU time for large pyramids.

### Error Handling

- **Wire pyramid not found**: return error immediately, no state written
- **Network failure mid-download**: save partial state, return partial error, user can retry
- **Source path not a valid folder**: return error, no state written
- **Target slug already in use by a different pyramid**: return error, suggest a different slug
- **Manifest parse error**: return error, no cache populated, user sees "corrupt manifest" message

---

## UI Integration

### ToolsMode Discover Tab

The existing ToolsMode Discover tab gains Wire pyramids (in addition to configs). Each pyramid card shows:

- Pyramid name + description
- Source node handle
- Node count + layer count
- Cache manifest size (or "no cache manifest" if withheld)
- Estimated cache savings (cost that would have been spent vs will be spent on import)
- "Pull" button

### Import Wizard

Clicking "Pull" opens a wizard:

1. **Source path picker**: "Where are the source files for this pyramid on your machine?" (e.g., a local clone of the same repo). User selects a folder.
2. **Slug picker**: "What local slug should this pyramid have?" (defaults to source pyramid's slug; editable to avoid conflicts).
3. **DADBEAR settings**: "Watch this folder for changes?" (default ON) + scan interval.
4. **Confirm**: shows pre-flight summary:
   - `X nodes in source pyramid`
   - `Y cache entries available`
   - `Matching source files in your folder: Z/X`
   - `Estimated cache hit rate: Y%`
   - `Estimated LLM cost to complete first build: $N`
5. **Import**: progress bar + "Cancel" button.

### Post-Import Indicators

After import, the new pyramid appears in the sidebar with:

- **"Imported" badge** — subtle pill next to the pyramid name indicating its origin
- **Cache hit rate indicator** — "Cache: 87% (98/112 nodes)" shown on first load; disappears after first build
- **Source node reference** — tooltip shows "Imported from @handle on 2026-04-09"

The cache hit rate is a historical snapshot ("what fraction of the import was usable"), not a live metric. Once the user runs their first build on the imported pyramid, the live build viz takes over with real-time cache hit display (see `build-viz-expansion.md`).

### Failed Import Recovery

If an import fails, the sidebar shows the target slug with a "resume import" banner:

```
my-imported-pyramid  [import failed: 47/112 nodes]  [Resume] [Cancel]
```

Clicking Resume re-calls `pyramid_import_pyramid` which reads `pyramid_import_state` and continues from the cursor.

---

## Files Modified

| Component | Files |
|-----------|-------|
| DB schema | `db.rs` — add `pyramid_import_state` table |
| Import logic | New `pyramid_import.rs` — manifest parsing, staleness check, cache population |
| Manifest export | `wire_publish.rs` — add `export_cache_manifest()` |
| IPC handlers | `routes.rs` — `handle_import_pyramid()`, `handle_import_progress()`, `handle_import_cancel()` |
| Evidence graph walker | Reuse existing `evidence_graph.rs` helpers for dependent propagation |
| DADBEAR integration | `dadbear.rs` — enable DADBEAR config post-import |
| Frontend wizard | New `ImportPyramidWizard.tsx` in ToolsMode |
| Frontend progress | New `ImportProgress.tsx` polling hook |
| Frontend sidebar | `Sidebar.tsx` — add imported badge, resume banner |

---

## Migration

1. Create `pyramid_import_state` table
2. Extend `wire_publish.rs::publish_pyramid()` to build and upload cache manifest (public-source pyramids only by default)
3. Implement `pyramid_import.rs` with manifest download, staleness check, and cache population
4. Wire up IPC handlers in `routes.rs`
5. Build the ImportPyramidWizard frontend
6. Extend ToolsMode Discover to surface Wire pyramids alongside configs
7. Test: publish a pyramid from node A, import into node B, verify cache hit rate, run a build, observe cache hits in build viz

---

## Integration with LLM Output Cache

This spec is a consumer of the `pyramid_step_cache` table defined in `llm-output-cache.md`. It does not modify the cache schema; it populates it from imported data. The cache lookup path in `call_model_unified()` works identically for imported entries and locally produced entries — they're both content-addressable rows in the same table.

The import does NOT use the `build_id` semantics of the cache — imported entries get a synthetic `build_id = "import:{wire_pyramid_id}"` to distinguish them in audit trails without affecting the cache key lookup (which ignores `build_id`).

---

## Open Questions

1. **Manifest versioning**: future schema changes to cache manifest format will need backwards compatibility. The `manifest_version` field in the manifest header addresses this. v1 supports only the fields defined above; v2 additions must be additive, never breaking.

2. **Cross-version cache compatibility**: if the source pyramid was built with a different Wire Node version whose prompts differ, the `prompt_hash` won't match and the cache entries will be stale even if the source files match. This is correct behavior but may produce low cache hit rates when nodes are on mismatched versions. Mitigation: the pre-flight summary shows "estimated cache hit rate" based on prompt_hash overlap, so users see what they're getting before pulling.

3. **Partial source trees**: if the user only has a subset of the source files locally (e.g., they cloned a subfolder), the import should still populate the cache for the files they have and mark the rest as stale. The current algorithm handles this correctly — missing files fall into the "mark stale" branch.

4. **Fork vs import**: forking implies an intent to diverge; importing implies an intent to track. Should they have different behaviors? Recommend: no difference at the cache level. Both use the same flow. Divergence happens through normal build + annotation, not at import time.

5. **Cache hit rate surfacing**: the pre-flight and post-import cache hit rate percentages are derived from node counts. A more accurate metric is "cost savings in dollars" (sum of cached `cost_usd` fields). Recommend: show both, with cost as the primary number ("Saves ~$4.20 in LLM calls") and node count as supporting detail.
