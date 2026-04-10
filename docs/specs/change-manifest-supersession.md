# Change-Manifest Supersession Specification

**Version:** 1.0
**Date:** 2026-04-09
**Status:** Design — pre-implementation
**Depends on:** Nothing (foundational)
**Fixes:** Viz DAG orphaning when upper-layer nodes are superseded
**Authors:** Adam Levine, Claude (session design partner)

---

## Problem

When DADBEAR detects file changes, the stale engine propagates upward through the pyramid. Upper-layer nodes (L1+) get marked stale and rebuilt. The current rebuild path:

1. `supersede_nodes_above(slug, depth, build_id)` sets `superseded_by` on all live nodes above the specified depth
2. The build pipeline creates entirely new nodes with new IDs (L3-000 → L3-S000)
3. All structural references — evidence links, web edges, parent-child lookups in the viz — still point to the old ID
4. `get_tree()` in `query.rs` builds the parent-child graph from `pyramid_evidence` links. Those links reference L3-000. The new node L3-S000 has no evidence links pointing to it
5. The tree renders a lone apex with no children

This has happened repeatedly and is the primary viz stability issue.

---

## Root Cause

`get_tree()` (query.rs:395-633) builds the tree from evidence links:
```
SELECT source_node_id, target_node_id FROM pyramid_evidence
WHERE slug = ? AND verdict = 'KEEP'
```

This produces `children_by_parent: HashMap<String, Vec<String>>`. When L3-000 is superseded by L3-S000, the evidence still references L3-000. `children_by_parent.get("L3-S000")` returns empty.

The `live_pyramid_nodes` view filters `superseded_by IS NULL`, so L3-000 is hidden. The tree has a visible apex (L3-S000) with no visible children.

---

## Solution: Change Manifests, Not Full Regeneration

When a stale check determines an upper-layer node needs updating, instead of creating a new node:

1. Ask the LLM: "Given that these children changed in these specific ways, what needs to change in this node's synthesis?"
2. The LLM returns a **change manifest** — a targeted delta, not a full rewrite
3. Apply the manifest to the existing node **in place** — same ID, bumped version
4. All references remain valid

### Change Manifest Format

The LLM produces:

```json
{
  "node_id": "L3-000",
  "identity_changed": false,
  "content_updates": {
    "distilled": "Updated synthesis incorporating the new findings about...",
    "headline": null,
    "topics": [
      {
        "action": "update",
        "name": "diverge-then-converge_architecture",
        "current": "Updated text reflecting the change..."
      },
      {
        "action": "add",
        "name": "new_topic_name",
        "current": "New topic description..."
      },
      {
        "action": "remove",
        "name": "obsolete_topic"
      }
    ],
    "terms": null,
    "decisions": null,
    "dead_ends": null
  },
  "children_swapped": [
    { "old": "L2-002", "new": "L2-S000" }
  ],
  "reason": "Child L2-002 was updated with new webbing patterns; parent synthesis updated to reflect."
}
```

### Field Semantics

| Field | Type | Meaning |
|-------|------|---------|
| `node_id` | string | The node being updated |
| `identity_changed` | bool | Whether the node's fundamental identity changed (rare). If true, new ID created |
| `content_updates` | object | Fields to update. `null` = no change to that field |
| `content_updates.distilled` | string/null | New distilled text (the main synthesis) |
| `content_updates.headline` | string/null | New headline (only if meaning shifted) |
| `content_updates.topics` | array/null | Topic-level changes (add/update/remove) |
| `content_updates.terms` | array/null | Term updates |
| `content_updates.decisions` | array/null | Decision updates |
| `children_swapped` | array | Which children were replaced (for reference tracking) |
| `reason` | string | Human-readable summary of what changed and why |

---

## In-Place Update Flow

### Normal Case (identity_changed = false)

1. **Snapshot current version** — Copy the current node state to `pyramid_node_versions` (append-only history)
2. **Apply content updates** — Update `distilled`, `headline`, `topics`, `terms`, `decisions` in `pyramid_nodes` where `id = node_id AND slug = slug`
3. **Bump build_version** — Increment `build_version` field on the node
4. **Update children array** — Apply `children_swapped` entries: replace old child IDs with new ones in the node's children
5. **Update evidence links** — For each entry in `children_swapped`, update `pyramid_evidence` rows:
   ```sql
   UPDATE pyramid_evidence SET source_node_id = ?new
   WHERE source_node_id = ?old AND target_node_id = ?node_id AND slug = ?slug
   ```
6. **Log the manifest** — Store the full manifest in `pyramid_change_manifests` for audit
7. **Propagate upward** — If this node itself is a child of higher nodes, enqueue those for stale check (unchanged from current behavior)

### Rare Case (identity_changed = true)

1. Create a new node with a new ID (current behavior)
2. Update ALL evidence links pointing to the old ID → new ID
3. Update parent nodes' children arrays
4. Log the identity change with reason

### DB Operations

**New table:**
```sql
CREATE TABLE IF NOT EXISTS pyramid_change_manifests (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    slug TEXT NOT NULL,
    node_id TEXT NOT NULL,
    build_version INTEGER NOT NULL,
    manifest_json TEXT NOT NULL,
    note TEXT,  -- user-provided note for reroll-with-notes; NULL for stale-check manifests
    supersedes_manifest_id INTEGER REFERENCES pyramid_change_manifests(id),  -- prior manifest this one corrects
    applied_at TEXT DEFAULT (datetime('now')),
    UNIQUE(slug, node_id, build_version)
);
```

**Modified: pyramid_nodes**
Add `build_version INTEGER DEFAULT 1` column. Bumped on each in-place update.

**Modified: pyramid_node_versions** (already exists)
Add a row on each in-place update capturing the pre-update state.

**New function: `update_node_in_place()`**
```rust
pub fn update_node_in_place(
    conn: &Connection,
    slug: &str,
    node_id: &str,
    updates: &ContentUpdates,
    children_swapped: &[(String, String)],
    build_id: &str,
) -> Result<i64> // returns new build_version
```

---

## Manifest Validation

Every change manifest is validated before it is applied. Invalid manifests are rejected (the node is left in its pre-manifest state) and logged with the failure reason. The stale check is not retried automatically — the user or an audit pass must surface the failure.

```rust
fn validate_change_manifest(
    conn: &Connection,
    slug: &str,
    node_id: &str,
    manifest: &ChangeManifest,
) -> Result<(), ManifestValidationError> {
    // 1. Target node exists and is live
    let node = load_live_node(conn, slug, node_id)
        .ok_or(ManifestValidationError::TargetNotFound)?;

    // 2. children_swapped references exist in the evidence graph
    for (old_id, new_id) in &manifest.children_swapped {
        if !evidence_link_exists(conn, slug, old_id, node_id, "KEEP") {
            return Err(ManifestValidationError::MissingOldChild(old_id.clone()));
        }
        if !node_exists(conn, slug, new_id) {
            return Err(ManifestValidationError::MissingNewChild(new_id.clone()));
        }
    }

    // 3. identity_changed semantics — if true, the manifest must rewrite the core identity fields
    if manifest.identity_changed {
        if manifest.content_updates.distilled.is_none()
            && manifest.content_updates.headline.is_none() {
            return Err(ManifestValidationError::IdentityChangedWithoutRewrite);
        }
    }

    // 4. content_updates field-level validation
    if let Some(topics) = &manifest.content_updates.topics {
        for topic_op in topics {
            match topic_op.action.as_str() {
                "add" | "update" => {
                    if topic_op.name.is_empty() || topic_op.current.is_empty() {
                        return Err(ManifestValidationError::InvalidTopicOp);
                    }
                }
                "remove" => {
                    // name required, current not used
                    if topic_op.name.is_empty() {
                        return Err(ManifestValidationError::InvalidTopicOp);
                    }
                    // Confirm the topic exists on the current node
                    if !node.topics.iter().any(|t| t.name == topic_op.name) {
                        return Err(ManifestValidationError::RemovingNonexistentTopic(topic_op.name.clone()));
                    }
                }
                _ => return Err(ManifestValidationError::InvalidTopicAction(topic_op.action.clone())),
            }
        }
    }
    // Similar validation for terms, decisions, dead_ends...

    // 5. reason field is non-empty (LLM-generated, not user note)
    if manifest.reason.trim().is_empty() {
        return Err(ManifestValidationError::EmptyReason);
    }

    // 6. build_version bump is contiguous
    if manifest.build_version != node.build_version + 1 {
        return Err(ManifestValidationError::NonContiguousVersion {
            expected: node.build_version + 1,
            got: manifest.build_version,
        });
    }

    Ok(())
}

pub enum ManifestValidationError {
    TargetNotFound,
    MissingOldChild(String),
    MissingNewChild(String),
    IdentityChangedWithoutRewrite,
    InvalidTopicOp,
    InvalidTopicAction(String),
    RemovingNonexistentTopic(String),
    EmptyReason,
    NonContiguousVersion { expected: i64, got: i64 },
}
```

**Failure handling**: validation failures are logged WARN-level with full manifest + error details and surfaced in the DADBEAR oversight page as an unapplied manifest entry. The user can:
- Manually reroll the manifest with notes explaining the issue (see "Reroll for Upper-Layer Manifests" below)
- Skip the failed manifest and accept the current node state
- Trigger a full stale re-check on the node

Validation is deliberately strict. A bad manifest silently applied would corrupt the pyramid permanently; an unapplied manifest is visible and recoverable.

---

## LLM Prompt: Change Manifest Generation

```
System: You are updating a knowledge synthesis node based on changes to its children.
Instead of regenerating the synthesis from scratch, identify what SPECIFICALLY needs
to change and produce a targeted update manifest.

The node's current content and its children's changes are provided below.

Output a JSON change manifest with these fields:
- identity_changed: boolean (true ONLY if the node's fundamental topic/coverage changed — very rare)
- content_updates: object with fields to update (set null for unchanged fields)
  - distilled: updated synthesis text incorporating the changes
  - headline: updated headline (only if the meaning shifted)
  - topics: array of {action: "add"|"update"|"remove", name, current} entries
  - terms, decisions, dead_ends: same pattern
- children_swapped: array of {old, new} for any child IDs that changed
- reason: one sentence explaining what changed and why

Guiding principles:
- Most updates only need distilled text changes. Don't touch headline unless meaning shifted.
- If a child was updated but the parent synthesis already captures the gist, say so — distilled: null.
- Prefer small targeted updates over wholesale rewrites.
```

**Input to the LLM:**
- Current node: `headline`, `distilled`, `topics`, `terms`, `decisions`
- Each changed child: old summary vs. new summary (delta)
- The stale check reason (from `dispatch_node_stale_check`)

This is a cheaper, more focused prompt than "regenerate the entire synthesis from all children."

### LLM Call Integration: StepContext

Both `generate_change_manifest()` and `apply_change_manifest()` LLM paths MUST receive a `StepContext` (defined canonically in `llm-output-cache.md`). This gives them cache support, event emission, and cost tracking uniformly with the rest of the pipeline.

| LLM call site | StepContext values |
|---|---|
| `generate_change_manifest()` (per stale-check result) | `step_name = "change_manifest"`, `primitive = "manifest_generation"`, `depth = node.depth`, `chunk_index = None` |
| `generate_change_manifest()` for reroll (user-initiated) | Same as above, `force_fresh = true` |
| `generate_vine_change_manifest()` (vine-level propagation) | `step_name = "vine_change_manifest"`, `primitive = "manifest_generation"`, `depth = vine_node.depth` |

The StepContext threads through from `stale_helpers_upper.rs:dispatch_node_stale_check()` which creates it with the shared event bus + DB path.

**Cache behavior for manifest generation**: manifest cache entries use the inputs_hash of `(node_current_state + children_deltas + reason)`. If the same stale check fires twice on the same node with the same inputs, the second call is a cache hit. If the user rerolls with a note, `force_fresh = true` bypasses the cache; the new manifest is stored with the same cache key (replacing the old entry) and carries the user's note on the new `pyramid_change_manifests` row.

---

## Scope and Notes Field

**Scope clarification:**
- `pyramid_change_manifests` is ONLY for stale-check-driven node updates (not config refinement)
- Config version history uses the contribution model (see config-contribution-and-wire-sharing.md)

**Notes and the `reason` field:**
- Node reroll-with-notes stores the user-provided note in the `pyramid_change_manifests.note` column AND in the `pyramid_node_versions` snapshot
- The `reason` field in the manifest JSON (`manifest_json`) serves as the triggering reason for both stale checks and user-initiated rerolls -- it is LLM-generated context about what changed and why
- The `note` column is the raw user-provided text (NULL for automated stale-check manifests)

### Reroll for Upper-Layer Manifests

The change manifest flow currently runs automatically on stale propagation. Users should be able to ALSO trigger it manually with notes: "This manifest auto-generated an update that's wrong. Here's what should actually change."

The `pyramid_reroll_node` IPC command (defined in `build-viz-expansion.md`) accepts any node ID. For upper-layer nodes — where the previous update was a change manifest — the reroll:

1. Loads the prior manifest from `pyramid_change_manifests` (the latest row for `slug, node_id`)
2. Constructs a reroll prompt combining:
   - Original children
   - Current distillation
   - The user's note
   - `"The previous manifest was: {prior_manifest}. The user disagreed: {note}. Produce a corrected manifest."`
3. Calls the LLM with `force_fresh: true`
4. Stores the new manifest with `note` populated and `supersedes_manifest_id` set to the prior manifest's `id`
5. Re-applies the new manifest to the node via the same in-place update flow used by stale-check manifests

**Manifest supersession chain:** `supersedes_manifest_id` lets us walk the chain of manually-corrected manifests for a node (e.g. "stale-check manifest → user reroll v1 → user reroll v2") for audit and rollback purposes.

**Note semantics remain consistent:** manually-rerolled manifests always carry a user `note`. Automated stale-check manifests have `note = NULL` (the existing behavior). Combined with `supersedes_manifest_id`, this makes the history self-describing — any row with a non-NULL `note` is a user correction, and its `supersedes_manifest_id` points at the thing being corrected.

---

## Integration with Stale Engine

### Scope boundary: which call sites this phase touches

`supersede_nodes_above()` in `db.rs` has **three callers** in the current tree. This phase modifies only ONE of them. The other two are intentionally left alone because they use correct wholesale-rebuild semantics.

**Modified by this phase:**

- **`stale_helpers_upper.rs::execute_supersession` (lines 1387-1700+)** — the stale-update path. Currently INSERTs a new node with a fresh ID at line 1671 and sets `superseded_by` on the old node at line 1694. This is what produces the viz orphaning bug (evidence links point to the old ID, which is now hidden by the `live_pyramid_nodes` view filter, so `get_tree()` can't resolve children). This phase rewrites `execute_supersession` to use change-manifest in-place updates. **This is the only place the code change lands.**

**NOT modified by this phase** (intentional full-rebuild semantics, correct as-is):

- **`vine.rs:3381`** — inside the vine L2+ rebuild path. The comment at line 3384 reads: *"Superseded {nodes_superseded} nodes and cleared {steps_deleted} steps above L1"*. This is an explicit wholesale rebuild triggered when vine composition changes (e.g., a new bedrock pyramid was added) and the L2+ structure needs to be recomputed from L1 up. The intent is literally "throw away the old upper layers and rebuild fresh" — it's the opposite of a targeted delta. Forcing it through the change-manifest flow would require the LLM to invent per-node deltas for nodes that are supposed to be replaced entirely, producing incoherent half-patched / half-rebuilt state.
- **`chain_executor.rs:4821`** — inside `build_lifecycle` fresh path. The comment at line 4815 reads: *"Fresh path: supersede all prior L1+ overlay nodes"*. This runs at the start of a fresh build (non-delta path) and clears any leftover L1+ overlay nodes from a prior build attempt so the new build can write over them. Same reasoning: it's a "clear and rebuild" operation, not a "what changed?" question, and change-manifest generation has no meaningful input context here (we're specifically saying "we don't care what the old children were").

**In both non-modified cases, the viz DAG stays coherent** because the wholesale rebuild creates a complete new upper tree with its own evidence links built during the rebuild — there is no half-updated state where old evidence links reference new nodes. The viz orphaning bug specifically requires the pattern of "insert a new node, leave all the old evidence links pointing at the old ID," which is exactly what `execute_supersession` does today and what this phase fixes there.

### Current flow (stale_helpers_upper.rs)

```
PendingMutation → dispatch_node_stale_check → stale=true →
    → execute_supersession → INSERT new node (line 1671) + set superseded_by on old (line 1694)
    → evidence links still reference old ID → live_pyramid_nodes view hides the old node
    → get_tree()'s children_by_parent lookup finds orphans ← BREAKS VIZ
```

### New flow (stale_helpers_upper.rs)

```
PendingMutation → dispatch_node_stale_check → stale=true →
    → generate_change_manifest(node, changed_children) →
        if identity_changed:
            → create new node, update all evidence links to point at the new ID (rare path)
        else:
            → update_node_in_place(node, manifest) → SAME ID, bumped build_version
    → evidence links remain valid because the ID didn't change
    → get_tree() resolves children correctly ← VIZ WORKS
```

### Where to Hook In

The integration point is inside `stale_helpers_upper.rs::execute_supersession` itself — replace the INSERT/UPDATE pair at lines 1671-1697 with the manifest-driven in-place update. The stale engine's result-handling path (`stale_engine.rs:drain_and_dispatch` and friends) does not need to change — it still calls into `execute_supersession` the same way.

**Key files:**
- `stale_helpers_upper.rs` — add `generate_change_manifest()` alongside existing `dispatch_node_stale_check()`; rewrite `execute_supersession` body to call it and apply the manifest in-place
- `stale_engine.rs` — change result handling from "mark for rebuild" to "generate and apply manifest"
- `db.rs` — add `update_node_in_place()`, `pyramid_change_manifests` table, `build_version` column
- `query.rs` — simplify `get_tree()` now that IDs are stable (may not need `live_pyramid_nodes` view filter for superseded upper nodes)

---

## Benefits

1. **Viz never breaks** — Node IDs are stable, all references valid
2. **Cheaper LLM calls** — "What changed?" is smaller than "regenerate everything"
3. **Better quality** — LLMs are better at targeted edits than full regeneration from scratch
4. **Audit trail** — Change manifests explain *why* each update happened, not just *that* it happened
5. **Aligns with notes paradigm** — A stale check is the system providing "notes" on an existing node

---

## Migration

1. Add `build_version` column to `pyramid_nodes` (default 1)
2. Create `pyramid_change_manifests` table
3. Update stale result handling to use change manifests
4. Existing pyramids continue to work — old nodes with `superseded_by` set are already handled by `live_pyramid_nodes` view

No data migration needed. The change is purely in the update path going forward.

---

## Vine-Level Manifests

Vines are pyramids that compose other pyramids. When a bedrock in a vine updates, the vine's apex and intermediate L1+ nodes may need updating. This uses the same change manifest flow as regular pyramids, with one difference:

- The "children" referenced in the manifest are NOT child nodes of the same pyramid. They are apex references from bedrock pyramids composed in this vine.
- The manifest's `children_swapped` list contains entries like `{old: "bedrock-x:L3-000", new: "bedrock-x:L3-S001"}` — the slug prefix identifies which bedrock's apex changed.
- The manifest generation LLM receives the vine node's current synthesis + the changed bedrock apex summaries, and produces a targeted update to the vine node.

Integration point: `vine_composition.rs:notify_vine_of_bedrock_completion()` — after a bedrock completes, look up which vines include it, and for each affected vine node, call `generate_change_manifest()` with the bedrock apex as the changed child.

No schema changes needed — the existing `pyramid_change_manifests` table handles vine nodes transparently (they're just nodes).

For vine-of-vines: the propagation walks up through the vine hierarchy. A bedrock update triggers manifests for its direct parent vine, which triggers manifests for its parent vine, etc. Each level is a separate manifest with its own LLM call.

---

## Open Questions

1. **Batch manifests**: Should multiple stale nodes at the same depth be batched into one LLM call (like current `dispatch_node_stale_check`)? Probably yes for efficiency, but each node gets its own manifest in the response.

2. **Manifest validation**: How aggressively to validate the LLM's manifest before applying? Recommend: validate field types and that referenced child IDs exist, but trust content updates (they're just text).

3. **Rollback**: If a manifest produces bad content, should there be a "revert to previous version" UI? The data is in `pyramid_node_versions`, so this is straightforward to implement later.
