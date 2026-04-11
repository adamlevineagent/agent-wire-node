# Workstream: Phase 18c — Privacy Opt-in + Pause-all Scoping

## Who you are

You are an implementer joining a coordinated fix-pass across the pyramid-folders/model-routing/observability initiative. Phase 18 reclaims 9 dropped cross-phase handoffs. You are implementing workstream **18c**, claiming ledger entries **L4 and L9** from `docs/plans/deferral-ledger.md`.

Three other Phase 18 workstreams (18a/18b/18d) run in parallel on their own branches. Do not touch files outside your scope. Your commits land on branch `phase-18c-privacy-pause-all`.

## Context

**L4 (Cache-publish privacy opt-in):** Phase 7 shipped `export_cache_manifest` with a default-OFF privacy gate — it returns `None` unless the caller explicitly opts in via a parameter. Phase 10 was supposed to add a user-visible checkbox in the publish preview modal that lets the user opt in with clear warnings about exposing cached LLM outputs. Phase 10 didn't pick it up. Result: `export_cache_manifest` is safe by default but unreachable — there's no UI path to flip the opt-in on, so Wire-shared pyramids never ship cache manifests, so cache warming on import (Phase 7's whole premise) is structurally dead from the frontend.

**L9 (Folder/circle scoped pause-all):** Phase 13 shipped `pyramid_pause_dadbear_all` / `pyramid_resume_dadbear_all` with `scope: "all"` only. The spec (`cross-pyramid-observability.md`) defined three scopes — `all`, `folder`, `circle` — to support "pause my work pyramids while I'm in personal projects" and "pause all pyramids shared with team X." Phase 13 shipped only `all` and deferred the other two to Phase 14/15. Phase 15 explicitly re-deferred them in its out-of-scope list. Result: pause-all is coarse-grained, users can't selectively pause subsets.

## Ledger entries you claim

| L# | Item | Source spec |
|---|---|---|
| **L4** | Cache-publish privacy opt-in checkbox in ToolsMode publish preview modal | `docs/specs/cache-warming-and-import.md` "Privacy Consideration" (~line 270); Phase 7 workstream prompt line 163 (narrow default-OFF); Phase 10 spec note "Phase 10 adds the opt-in checkbox with warnings" |
| **L9** | Folder/circle scoped pause-all DADBEAR | `docs/specs/cross-pyramid-observability.md` "Pause-All Semantics" (~line 286); Phase 13 workstream prompt line 251 (scope=all only); Phase 15 out-of-scope line 284 |

## Required reading (in order)

1. `docs/plans/phase-18-plan.md` — Phase 18 overall structure; skim.
2. `docs/plans/deferral-ledger.md` — entries L4 and L9 in full.
3. **`docs/specs/cache-warming-and-import.md`** — "Privacy Consideration" section around line 270 + the Publication Side section around line 223. Primary source for L4.
4. **`docs/specs/cross-pyramid-observability.md`** — "Pause-All Semantics" lines ~286-335. Primary source for L9, including the exact SQL for each scope.
5. `docs/plans/phase-7-workstream-prompt.md` lines 160-170 (L4 origin) and phase-10 spec note about "Phase 10 adds the checkbox."
6. `docs/plans/phase-13-workstream-prompt.md` lines 245-260 (L9 origin) — the "scope=all only" framing.

### Code reading

7. **`src-tauri/src/pyramid/wire_publish.rs`** — find `export_cache_manifest`. Understand the current default-OFF parameter shape. L4's IPC wrapper needs to pass the opt-in through.
8. **`src-tauri/src/pyramid/publication.rs`** — the publish flow. Cache manifest is an optional attachment to a pyramid publish; find where it's called from the publish path.
9. `src-tauri/src/main.rs` — grep for `pyramid_publish_pyramid`, `pyramid_dry_run_publish`, or whatever IPC the Publish Preview modal calls. L4 extends it with an optional `include_cache_manifest: bool` parameter defaulting to false.
10. **`src/components/PublishPreviewModal.tsx`** (Phase 10) — the modal the user sees when clicking Publish to Wire. L4's checkbox lives here.
11. **`src-tauri/src/pyramid/db.rs`** around `pyramid_dadbear_config` — the `source_path` column is the key for L9's folder scoping.
12. **`src-tauri/src/main.rs`** — `pyramid_pause_dadbear_all` and `pyramid_resume_dadbear_all` IPCs (Phase 13). L9 extends the scope handling beyond `"all"`.
13. `src-tauri/src/pyramid/db.rs` — find `pyramid_metadata` or equivalent that tracks circle membership. The spec's SQL for circle scope references `circle_id` — verify whether the schema has this column, or if circles are tracked elsewhere.
14. **`src/components/CrossPyramidTimeline.tsx`** (Phase 13) — the Pause All button lives here. L9 extends the confirmation modal with a scope picker.
15. **`src/components/DadbearOversightPage.tsx`** (Phase 15) — also has a Pause All button per Phase 15 spec. L9's scope picker lives here too.

## What to build

### 1. L4: Cache-publish privacy opt-in checkbox

**Backend (minimal):**

Find the IPC that the PublishPreviewModal calls. Extend its input shape with an optional field:

```rust
#[derive(Deserialize)]
struct PublishPyramidInput {
    slug: String,
    visibility: String,
    // ... existing fields ...
    #[serde(default)]
    include_cache_manifest: bool,  // L4: user opt-in for cache manifest attachment
}
```

Thread `include_cache_manifest` through to `export_cache_manifest`'s existing opt-in parameter. No new storage, no new table — this is a per-call opt-in that lives on the publish request only.

**Frontend (the load-bearing part):**

In `PublishPreviewModal.tsx`, add a new section to the modal body:

```
┌─ Advanced Publishing Options ───────────────────┐
│                                                  │
│  ☐ Include cache manifest                       │
│                                                  │
│     Pullers of this pyramid will be able to     │
│     reuse your cached LLM outputs to rebuild     │
│     instantly without re-running expensive       │
│     model calls — a large cost saving for        │
│     popular pyramids.                            │
│                                                  │
│     ⚠ Warning: cached outputs may contain        │
│     excerpts from your source material.          │
│     Only enable for pyramids whose source is     │
│     already public and whose L0 nodes reference  │
│     public corpus documents.                     │
│                                                  │
│     Last audit: {N} L0 nodes, {M} reference     │
│     private corpus docs (would be stripped if   │
│     you opt in).                                 │
│                                                  │
└──────────────────────────────────────────────────┘
```

The "Last audit" line requires a backend preview IPC that scans the pyramid's L0 nodes and counts how many would need stripping. If that's complex, simplify to: on checkbox-click, run the audit and display the result inline. If the audit is not available, ship the checkbox with just the warning text and document the audit-count as a follow-up.

**Default state:** unchecked. Checking it requires the user to acknowledge the warning — do NOT auto-check based on any heuristic. The user must explicitly opt in.

**Interaction:** when the user clicks Publish, the checkbox value gets passed through as `include_cache_manifest` in the IPC call.

### 2. L9: Folder/circle scoped pause-all DADBEAR

**Backend extension:**

Extend the existing IPC shapes:

```rust
#[derive(Deserialize)]
struct PauseDadbearAllInput {
    scope: String,              // "all" | "folder" | "circle"
    scope_value: Option<String>, // folder path (for "folder"), circle id (for "circle")
}
```

In the IPC handler, dispatch on scope:

- `"all"`: existing behavior — `UPDATE pyramid_dadbear_config SET enabled = 0 WHERE enabled = 1`
- `"folder"`: scope_value must be non-None, it's the folder path. SQL:
  ```sql
  UPDATE pyramid_dadbear_config
  SET enabled = 0
  WHERE enabled = 1
    AND (source_path = ?1 OR source_path LIKE ?1 || '/%')
  ```
- `"circle"`: scope_value must be non-None, it's the circle id. SQL per spec:
  ```sql
  UPDATE pyramid_dadbear_config
  SET enabled = 0
  WHERE enabled = 1
    AND slug IN (
      SELECT slug FROM pyramid_metadata WHERE circle_id = ?1
    )
  ```
  If `pyramid_metadata.circle_id` doesn't exist (check schema), document circle scope as deferred to a later phase (once circle membership tracking lands) and ship `all` + `folder` only. Don't invent the circle schema in this phase.

- Return `{ affected: u64 }` — count of rows where the UPDATE flipped state.

Mirror the same scope handling in `pyramid_resume_dadbear_all`.

**Frontend scope picker:**

Both `CrossPyramidTimeline.tsx` and `DadbearOversightPage.tsx` have a "Pause All" button with a confirmation modal. Extend the modal with a scope picker:

```
┌─ Pause DADBEAR ─────────────────────────────────┐
│                                                  │
│  Scope:                                          │
│    ● All pyramids ({N})                         │
│    ○ Pyramids under folder: [/path/... ▾]      │
│    ○ Pyramids in circle: [circle name ▾]       │
│                                                  │
│  This will pause background maintenance for     │
│  {M} pyramid(s). In-flight builds are not      │
│  affected. Use Resume to re-enable.              │
│                                                  │
│  [Cancel]                    [Pause {M}]        │
└──────────────────────────────────────────────────┘
```

- The folder dropdown should be populated from the distinct `source_path` values across `pyramid_dadbear_config` — add a helper IPC `pyramid_list_dadbear_source_paths() -> Vec<String>` that returns the distinct source paths. Or, simpler: use a text input that the user types, with the current pyramid count shown live as they type.
- Circle dropdown is populated from `pyramid_metadata.circle_id` values OR left hidden/disabled if the circle schema isn't present.
- The count `{M}` updates live as the user changes scope — call a new `pyramid_count_dadbear_scope(scope, scope_value) -> u64` IPC that runs the SELECT without the UPDATE, so the user sees impact before confirming.

Rename the existing Pause All button label to "Pause..." (with ellipsis) to signal the scope picker opens.

### 3. Tests

**L4 tests:**
- `export_cache_manifest` with opt-in = true returns Some manifest
- `export_cache_manifest` with opt-in = false returns None (existing behavior)
- Publish IPC with `include_cache_manifest: true` actually passes through to `export_cache_manifest`
- Publish IPC with `include_cache_manifest: false` OR default (omitted) does NOT include the manifest

**L9 tests:**
- `pyramid_pause_dadbear_all` with scope=all matches existing Phase 13 test
- `pyramid_pause_dadbear_all` with scope=folder matches only rows whose source_path is under the given folder (test with a path hierarchy: `/a`, `/a/b`, `/a/b/c`, `/d` — scope=folder `/a` matches first three)
- `pyramid_pause_dadbear_all` with scope=folder handles trailing-slash + no-slash variants correctly
- `pyramid_pause_dadbear_all` with scope=circle matches only rows whose slug is in the circle (mock the metadata)
- Idempotent: second call with same scope returns affected=0
- Resume mirrors the same scope behavior
- Count IPC returns the right number for each scope without side effects

## Scope boundaries

**In scope:**
- `include_cache_manifest` parameter on the Publish IPC
- Opt-in checkbox in `PublishPreviewModal.tsx` with warning text + audit count (or follow-up)
- Extended `pyramid_pause_dadbear_all` / `pyramid_resume_dadbear_all` IPCs with `scope: all | folder | circle`
- Scope picker in `CrossPyramidTimeline.tsx` + `DadbearOversightPage.tsx` pause-all modals
- New helper IPCs: `pyramid_list_dadbear_source_paths`, `pyramid_count_dadbear_scope` (OR merge the count into the scope IPCs via a `dry_run: bool` param)
- Rust tests for both L4 and L9
- Implementation log entry

**Out of scope (other Phase 18 workstreams):**
- Local mode toggle — 18a
- Cache retrofit for audited calls — 18b
- search_hit signal path — 18b
- Schema migration UI — 18d
- CC memory subfolder ingestion — 18e

**Out of scope permanently:**
- Building the circle membership schema if it doesn't exist (ship `all`+`folder`, defer `circle`)
- L4 "audit count" backend if it's nontrivial — ship the checkbox with warning text + follow-up note in log
- Auto-opt-in heuristics for L4 (must be explicit user action)
- Cross-pyramid publishing — L4 is per-pyramid

## Verification criteria

1. **Rust clean:** `cargo check --lib` — 3 pre-existing warnings allowed, zero new.
2. **Test count:** `cargo test --lib pyramid` — prior count + new Phase 18c tests.
3. **Frontend build:** `npm run build` clean.
4. **IPC registrations:** grep `main.rs` for any new IPCs you added (`pyramid_list_dadbear_source_paths`, `pyramid_count_dadbear_scope`, or extended shapes) — each defined + registered in `invoke_handler!`.
5. **Manual verification for L4:** document steps — Publish Preview modal → open Advanced section → check the opt-in → click Publish → observe cache manifest is included (via backend log or SQLite inspect of the publish payload).
6. **Manual verification for L9:** document steps — CrossPyramidTimeline → Pause... → pick scope=folder → type a path → observe live count → confirm → check SQLite that only matching rows flipped.

## Deviation protocol

- **L4 audit-count complexity:** if the "N L0 nodes reference private docs" preview is non-trivial, ship the checkbox with warning text only and note the audit-count as a follow-up.
- **`pyramid_metadata.circle_id` missing:** defer circle scope entirely, ship all + folder. Document.
- **Source path matching edge cases:** trailing slash, canonicalization, symlinks — document your choice (canonicalize on insert? on match? lexical only?).
- **Count IPC merged into scope IPC:** if splitting `pyramid_count_dadbear_scope` feels like IPC proliferation, add a `dry_run: bool` to the existing pause IPC.

## Mandate

- **`feedback_always_scope_frontend.md`:** both L4 and L9 have visible UI surfaces. Don't ship backend-only; the user must be able to see and click the opt-in checkbox and the scope picker. If the UI changes don't land, the phase failed.
- **No Pillar 37 violations.** No hardcoded folder depth, no hardcoded count caps, no hardcoded circle limits.
- **Default-OFF privacy.** L4's checkbox defaults to unchecked. No clever auto-check heuristics.
- **Reversibility.** L9's resume mirrors pause exactly — same scopes, same SQL shape.

## Commit format

Single commit on `phase-18c-privacy-pause-all`:

```
phase-18c: cache-publish privacy opt-in + pause-all scoping

<5-8 line body summarizing:
- PublishPreviewModal opt-in checkbox + warning text
- Publish IPC include_cache_manifest pass-through
- pause-all scope: all | folder | circle (or all+folder if circle schema absent)
- CrossPyramidTimeline + DadbearOversightPage scope picker
- Claims L4 and L9 from deferral-ledger.md>
```

Do not amend. Do not push. Do not merge.

## Implementation log

Append Phase 18c entry to `docs/plans/pyramid-folders-model-routing-implementation-log.md`:
1. L4: IPC param change + checkbox component
2. L9: extended SQL per scope + circle-schema check result
3. Helper IPCs added
4. Tests added
5. Manual verification steps
6. Any deviations
7. Status: `awaiting-verification`

## End state

Phase 18c is complete when:
1. Publish preview modal has a visible, clickable opt-in checkbox with warning
2. Pause All button in CrossPyramidTimeline + DadbearOversightPage opens a scope picker
3. Both new backend paths have test coverage
4. `cargo check --lib` + `cargo test --lib pyramid` + `npm run build` clean
5. Single commit on branch `phase-18c-privacy-pause-all`

Begin with the specs (both are short), then the existing Phase 7/10/13/15 shipped code, then the new IPCs, then the modals.

Good luck.
