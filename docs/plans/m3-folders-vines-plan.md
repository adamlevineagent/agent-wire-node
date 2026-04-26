# Mission #3 — Recursive Folders + Vines: Scan/Curate/Build Architecture

**Plan type:** mission-decomposition | **Author:** deepseek-peterman | **Date:** 2026-04-25
**Target branch:** `puddy/m3-folders-vines-plan`
**Anchor:** `docs/handoffs/handoff-2026-04-11-folder-nodes-as-checklists.md`
**Canonical spec:** `docs/vision/self-describing-filesystem.md`
**Prior art plan:** `docs/plans/self-describing-fs-2026-04-11.md` (Claude session, 1046 lines — read independently for the full dispatch-layer SDFS design; this plan defers that path in favor of Adam's four-step scan/curate/build decomposition)

---

## Goal

Replace the current scan→immediately-dispatch-all folder ingestion model with a four-step architectural rebuild: diagnose the live `65-FAILED (idle) 0/0 steps` dispatch bug, implement per-folder `.understanding/filemap.yaml` as idempotent scan output, enable editor-native user curation of filemaps, and replace the dispatcher with a build-from-checklists path that reads only user-checked entries. This is plan-only — this document decomposes all four steps into sub-cards that bania can claim and execute independently.

---

## Approach

### Step 1 — Diagnose the 65-FAILED dispatch bug

**The bug:** After shipping Phase 18 fixes, Adam triggered folder ingestion on the Mac bundle (`Apr 11 08:27:09`) against a real folder. Every build entered the `active_build` map and immediately transitioned to `FAILED (idle) 0/0 steps $0.000` — zero steps completed, zero pipeline rows written, zero cost. Identical on both local mode (Ollama) and OpenRouter, ruling out provider resolution as root cause.

**Trace from UI to dispatch path:**
1. UI clicks "Ingest" → Tauri IPC `pyramid_ingest_folder` (`main.rs:4564`)
2. `plan_ingestion` walks target folder, produces `IngestionPlan`
3. `execute_plan` writes slugs/vines/DADBEAR configs to SQLite
4. `spawn_initial_builds` (`folder_ingestion.rs:1811`) extracts leaves via `extract_build_dispatches`, for each leaf calls `prepopulate_chunks_for` then `spawn_question_build`
5. `spawn_question_build` (`question_build.rs:33`) calls `run_decomposed_build` — the question pipeline, which expects chunks or a question tree
6. `run_decomposed_build` returns `Err` before any step writes → `BuildHandle.status` set to `"failed"` at `question_build.rs:287-291` with `failures = -1`

**Diagnosis — the likely failure mode:**

`spawn_initial_builds` routes **every** leaf (Code, Document, Conversation) through `spawn_question_build` → `run_decomposed_build`. But in `build_runner::run_build_from` (the canonical dispatch), Code and Document content types route to `run_chain_build` (line 386), NOT to `run_decomposed_build`. The question pipeline is for Question and Conversation slugs only.

Specifically: `run_decomposed_build` needs either a stored `QuestionTree` (for Question slugs) or falls back to a default apex question (for Conversation slugs). Code and Document pyramids created by folder ingestion have neither. When `run_decomposed_build` attempts to decompose the question or work with chunks, it fails before any `pyramid_pipeline_steps` row is written.

**Evidence:**
- `build_runner.rs:261-300` — Question type: loads question tree, returns `Err` if missing
- `build_runner.rs:307-355` — Conversation type: falls back to hardcoded default apex question (does NOT fail)
- `build_runner.rs:357-402` — Code/Document: routes to `run_chain_build`, NOT question pipeline
- `folder_ingestion.rs:1811-1830` — `spawn_initial_builds` unconditionally calls `spawn_question_build` for every leaf
- `question_build.rs:33` → calls `run_decomposed_build` directly, bypassing `build_runner::run_build_from` content-type dispatch gate

**Proposed fix:** `spawn_initial_builds` must mirror `build_runner::run_build_from` dispatch logic:
- Code/Document leaves → route through `build_runner::run_build_from` (which dispatches to `run_chain_build`)
- Conversation leaves → route through `spawn_question_build` (has the default apex question fallback)

**Files touched in diagnosis:**
- `src-tauri/src/pyramid/folder_ingestion.rs` — `spawn_initial_builds` (lines ~1800-1860)
- `src-tauri/src/pyramid/question_build.rs` — `spawn_question_build` (lines 33-310)
- `src-tauri/src/pyramid/build_runner.rs` — `run_build_from` (lines 186-402) — correct pattern to mirror
- `src-tauri/src/main.rs` — `pyramid_build` Tauri command (lines 3861-4240) — reference shape

**Acceptance:**
- [ ] `cargo check` (default target) on branch with fix passes cleanly
- [ ] `spawn_initial_builds` against plan with Code, Document, and Conversation leaves dispatches all three without Err before step 0
- [ ] At least one Code leaf build reaches step ≥ 1 (pipeline steps table non-empty)
- [ ] Fix is isolated to `spawn_initial_builds` — no changes to `run_decomposed_build` or chain executor

**Risks:**
- **Misdiagnosis:** If actual failure is chain-resolution for Code/Document in `run_chain_build` or DB contention, fix routes through a different path that may also fail. Mitigation: Card 3.1 must run the diagnostic (click "View" on failed build, capture error string) before implementing fix.
- **`folder_builds_sequential` flag:** Must be preserved across the refactor.

---

### Step 2 — Implement `.understanding/filemap.yaml` per-folder as scan output

**Design:** The scan step walks a target folder recursively and writes a `.understanding/filemap.yaml` into each folder visited. This file is the canonical record. SQLite remains a derived cache.

**Decisions encoded (from Adam, per anchor handoff):**
1. File lives in `.understanding/filemap.yaml` (not root-level, not `.understanding/folder.md`)
2. Top-level keys: `scan:`, `entries:`, `deleted:`
3. Scanner-owned fields: `path`, `size_bytes`, `mtime`, `sha256`, `detected_content_type`, `detected_inclusion`, `exclusion_pattern` (when applicable), plus post-build fields (`built_as_pyramid_node`, `last_build_at`, `last_build_error`)
4. User-owned fields: `user_included` (tri-state: null/true/false), `user_content_type`, `user_notes`
5. Re-scan is field-level: scanner overwrites scanner columns; user columns preserved
6. New files appear with `user_included: null` — never auto-included, never auto-excluded
7. Files that disappear move to `deleted:` list with last-known state
8. Five `detected_inclusion` values: `included`, `excluded_by_pattern`, `excluded_by_size`, `excluded_by_type`, `unsupported_content_type`, `failed_extraction`

**Split from plan_recursive:** The current `plan_recursive` (lines 908-1180) both scans AND plans. Under the new model, scan writes filemaps; plan reads them. The two decouple.

**Files touched:**
- New: `src-tauri/src/pyramid/filemap.rs` — `Filemap` struct (serde), `write_filemap()`, `read_filemap()`, `merge_scan_into_filemap()` (field-level merge)
- `src-tauri/src/pyramid/folder_ingestion.rs` — new `scan_folder_to_filemap()` wrapper
- `src-tauri/src/pyramid/db.rs` — no schema changes (filemaps are additive)

**Acceptance:**
- [ ] Scan walks >=3 levels with >=50 files and writes `.understanding/filemap.yaml` in every visited folder
- [ ] Every entry has `sha256`, `size_bytes`, `mtime`, `detected_content_type`, `detected_inclusion`
- [ ] Files matching ignore patterns appear with `detected_inclusion: excluded_by_pattern` + pattern recorded
- [ ] Re-running scan is idempotent: hashes/mtimes update if changed; user fields untouched
- [ ] New files appear with `user_included: null`; deleted files move to `deleted:` with timestamp
- [ ] `cargo check` (default target) passes

**Risks:**
- **Hash performance:** SHA-256 on large files. `max_file_size_bytes` (default 10MB) caps it; above-threshold files excluded and not hashed.
- **Concurrent access:** Two scanners race on filemap writes. Mitigation: write-to-temp-then-rename (atomic on macOS).
- **YAML round-trip:** serde_yaml may reorder keys. Mitigation: tool owns the YAML structure; users edit values only.

---

### Step 3 — Enable user curation (editor-native first, UI second)

**Design:** The filemap is canonical. Users curate by editing `user_included`, `user_content_type`, and `user_notes` fields directly in `.understanding/filemap.yaml` using their preferred editor. A UI reads filemaps across the tree and presents an aggregate curation surface that writes back to the files.

**Editor-native curation (Phase 3a — ships first):**
- Document the filemap YAML format with inline comments explaining each field
- No code changes needed — the format IS the interface
- Users open `.understanding/filemap.yaml` in any editor, toggle `user_included`, save

**UI curation (Phase 3b — ships second):**
- Frontend reads filemaps via new IPC `pyramid_read_filemap(folder_path)`
- Renders as checklist: path | detected type | scanner suggestion | user toggle | type override | notes
- User toggles entries, overrides types, adds notes
- UI writes back via `pyramid_write_filemap_entries(folder_path, entries)` — field-level merge
- The UI never holds its own state — every read from disk, every write to disk

**Files touched:**
- Frontend: `src/components/FilemapCuration.tsx`, `src/components/FilemapSummary.tsx`, `src/hooks/useFilemap.ts`, extend `src/components/AddWorkspace.tsx`
- Backend: New Tauri commands in `src-tauri/src/main.rs`: `pyramid_read_filemap`, `pyramid_write_filemap_entries`
- Backend: `src-tauri/src/pyramid/filemap.rs` — serde structs, read/write/merge (shared with Step 2)

**Acceptance:**
- [ ] Opening `.understanding/filemap.yaml` in any YAML editor shows human-readable entries
- [ ] Editing `user_included: null` -> `user_included: true`, saving, re-running build causes that file to be built
- [ ] UI checklist loads all filemaps in tree and displays as a flat or hierarchical list
- [ ] Toggling entry in UI writes back to filemap within 500ms
- [ ] `cat .understanding/filemap.yaml` after UI toggle shows updated value
- [ ] `cargo check` + `npm run build` both pass

**Risks:**
- **Large trees:** 10,000+ files across 1,000 folders = loading 1,000 YAML files. Mitigation: paginate, lazy-load.
- **File locking:** User editing in VS Code + UI writing simultaneously = "file changed on disk" warning. Mitigation: document as expected.
- **YAML syntax errors from user editing:** Invalid YAML breaks parser. Mitigation: clear error message with line number.

---

### Step 4 — Build-from-checklists: replace scan-and-immediately-dispatch-all

**Design:** The new model: scan writes filemaps, user curates, build reads only `user_included: true` entries.

**New flow:**
1. Walk tree, read every `.understanding/filemap.yaml`
2. Collect entries where `user_included == true`
3. Group by content type (or `user_content_type` override)
4. Assemble `IngestionPlan` containing only checked entries
5. Execute plan via existing `execute_plan` dispatcher (no rewrite needed)
6. After successful build: write `built_as_pyramid_node`, `last_build_at` back to filemap
7. After failed build: write `last_build_error` to filemap

**Build ordering:** Topological — root vine first, sub-vines, then bedrocks. `folder_builds_sequential` flag gates concurrent vs sequential dispatch.

**Inheritance (Q4 from handoff):**
- Parent `.understanding/filemap.yaml` may specify `children_default: unchecked | include | skip`
- `unchecked`: subfolder entries default to `user_included: null` (user must curate)
- `include`: subfolder entries with `detected_inclusion: included` default to `user_included: true`
- `skip`: subfolder entries default to `user_included: false`
- Subfolder filemap can override parent; default when absent: `unchecked`

**Files touched:**
- `src-tauri/src/pyramid/filemap.rs` — `build_plan_from_filemaps(root) -> IngestionPlan`
- `src-tauri/src/pyramid/folder_ingestion.rs` — refactor `plan_recursive` to accept filemaps instead of calling `scan_folder` live
- `src-tauri/src/main.rs` — new Tauri command `pyramid_build_from_filemaps(root)`
- `src-tauri/src/pyramid/build_runner.rs` — no changes (dispatcher consumes same plan shape)

**Acceptance:**
- [ ] `wire-node build <root>` with 10/50 `user_included: true` creates exactly 10 builds
- [ ] `user_included: false` entries skipped (no build, no slug created)
- [ ] `user_included: null` (uncurated) entries skipped with log message
- [ ] Successful build writes `built_as_pyramid_node` + `last_build_at` back to filemap
- [ ] Failed build writes `last_build_error` back to filemap
- [ ] `children_default: include` on root causes subfolder entries to default to included
- [ ] Re-running build after all succeeded is idempotent
- [ ] `cargo check` (default target) passes

**Risks:**
- **Orphan `.understanding/` dirs:** Folder deleted but filemap remains stale. Mitigation: build reports "folder has filemap but folder doesn't exist — skipping."
- **Partial build state:** Crash mid-build leaves partial state. Mitigation: `built_as_pyramid_node` written immediately after each build succeeds.
- **`execute_plan` idempotency:** Phase 18e fix already handles re-running against existing slugs.

---

## Sub-card breakdown (ordered, one bania card each)

### Card 3.1 — Reproduce the 65-FAILED bug and capture the error
**WHAT:** Launch Mac bundle, ingest a real folder, click "View" on a failed build, capture exact error string from `BuildHandle.error`. Write one-line root cause diagnosis to `docs/plans/m3-65-failed-diagnosis.md`. If error matches content-type dispatch mismatch, confirm; otherwise document actual failure mode.

### Card 3.2 — Fix `spawn_initial_builds` dispatch routing
**WHAT:** Refactor `spawn_initial_builds` in `folder_ingestion.rs` to mirror `build_runner::run_build_from` content-type dispatch: Code/Document leaves -> `build_runner::run_build_from`, Conversation leaves -> `spawn_question_build`. Preserve `folder_builds_sequential`. Add regression test verifying Code leaf build reaches step >= 1.

### Card 3.3 — Implement `filemap.rs` module: read/write/merge
**WHAT:** Create `src-tauri/src/pyramid/filemap.rs` with `Filemap` struct (serde), `write_filemap()`, `read_filemap()`, `merge_scan_into_filemap()` (field-level merge). Unit tests: fresh write, idempotent re-scan, file-added case (new entry with null), file-deleted case (tombstone), user field preservation.

### Card 3.4 — Wire `scan_folder` output into filemap writes
**WHAT:** Add `scan_folder_to_filemap()` wrapper in `folder_ingestion.rs` that calls existing `scan_folder(path, config)` then writes result to `.understanding/filemap.yaml`. Wire via new Tauri command `pyramid_scan_folder(root) -> ScanResult` (scan-only, no planning, no building).

### Card 3.5 — Implement `pyramid_read_filemap` and `pyramid_write_filemap_entries` Tauri IPCs
**WHAT:** Two new Tauri commands in `main.rs`: `pyramid_read_filemap(folder_path)` returns parsed `Filemap`; `pyramid_write_filemap_entries(folder_path, entries)` does field-level merge. Register in `invoke_handler!`.

### Card 3.6 — Build the filemap curation UI (checklist component)
**WHAT:** Frontend: `src/components/FilemapCuration.tsx` renders filemap as sortable, filterable checklist. `src/hooks/useFilemap.ts` for IPC calls. Every toggle calls `pyramid_write_filemap_entries` within 500ms. Wire into folder ingestion wizard as "Curate" step between preview and build.

### Card 3.7 — Implement `build_plan_from_filemaps`
**WHAT:** New function in `filemap.rs`: walks root tree, reads all `.understanding/filemap.yaml`, collects `user_included: true` entries, groups by content type, assembles `IngestionPlan`. Handles `children_default` inheritance. Returns plan consumable by `execute_plan`.

### Card 3.8 — Wire `pyramid_build_from_filemaps` Tauri command
**WHAT:** New Tauri command `pyramid_build_from_filemaps(root)`: calls `build_plan_from_filemaps`, then `execute_plan`, then `spawn_initial_builds` (with Card 3.2 fix). After each build, writes `built_as_pyramid_node`/`last_build_at`/`last_build_error` back to filemap entry.

### Card 3.9 — Integration test: end-to-end scan -> curate -> build
**WHAT:** Integration test in `folder_ingestion.rs` test module: creates temp directory tree with mixed types, runs scan (writes filemaps), simulates user curation (edits `user_included` fields), runs build-from-filemaps, asserts only checked files produced builds.

### Card 3.10 — Documentation and handoff
**WHAT:** Update `docs/handoffs/` with post-Mission #3 handoff summarizing what was built, deferred, and current `.understanding/` migration state. Update memory file `project_folder_nodes_checklist.md` to reflect completion. Note: nodes/, edges/, evidence/, configs/, conversations/, cache/ NOT yet populated (per Adam decision #1).

---

## Non-goals (confirmed per Adam's framing and the anchor doc)

- Do NOT write any implementation code in this plan (plan-only pass)
- Do NOT modify the anchor handoff doc (`docs/handoffs/handoff-2026-04-11-folder-nodes-as-checklists.md`)
- Do NOT pre-decompose Phase 9 from Mission #2 (still on hold)
- Do NOT speculate beyond what the anchor doc + Adam's 4-step framing scopes
- Do NOT migrate node payloads, edges, evidence, configs, conversations, or cache into `.understanding/` — filemap only
- Do NOT build the "checkbox-in-accordion" stopgap — the real pivot replaces it
- Do NOT touch `folder_ingestion_heuristics` further — the pattern list is correct
- Do NOT skip `cargo check` default target on any change touching Tauri commands

---

## Open questions (for Partner via puddy)

- **Q1 — Phase placement:** Handoff left this open. Recommendation: land inside current 17-phase initiative as Phase 18f (post-18e). Rationale: Phase 17 output is directly blocked by the 65-FAILED bug; new model routes around it structurally. Counter-argument: major architectural change while initiative is wrapping up. Partner choice needed.

- **Q2 — `.understanding/filemap.yaml` filename:** Handoff said "propose ONE." This plan proposes `filemap.yaml` (not `folder.md`) — YAML is machine-parseable and maps cleanly to structured classification. `folder.md` reserved for eventual human-readable folder node document.

- **Q3 — Forward-only migration:** Handoff proposed forward-only (new scans use new model, existing pyramids stay on old). This plan adopts that. Confirm.

- **Q4 — `cargo check` default target ceremony:** Cards 3.2, 3.4, 3.5, 3.8 touch Tauri handlers. Bania must run `cargo check` (default target) as pre-commit gate.

---

## References

- Anchor: `docs/handoffs/handoff-2026-04-11-folder-nodes-as-checklists.md`
- Canonical spec: `docs/vision/self-describing-filesystem.md`
- Prior SDFS plan: `docs/plans/self-describing-fs-2026-04-11.md`
- Phase 17 implementation: `src-tauri/src/pyramid/folder_ingestion.rs`
- Build runner: `src-tauri/src/pyramid/build_runner.rs`
- Question build spawn: `src-tauri/src/pyramid/question_build.rs`
- Tauri commands: `src-tauri/src/main.rs`
- Memory: `~/.claude/projects/-Users-adamlevine-AI-Project-Files/memory/project_folder_nodes_checklist.md`
- Bundled patterns: `src-tauri/assets/bundled_contributions.json`
