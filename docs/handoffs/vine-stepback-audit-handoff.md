# Vine Conversation System — Stepback Audit Handoff

**Date:** 2026-03-24
**From:** Session 0319cb80 (vine implementation session)
**Status:** Backend functionally complete, frontend has bugs, needs full-surface audit

---

## What Was Built

### Backend (Rust) — ~3000 lines in `src-tauri/src/pyramid/`

**New files:**
- `vine.rs` — Core vine system: JSONL discovery, bunch building, vine L0/L1/L2+ construction, 6 intelligence passes (ERAs, transitions, entity resolution, decision tracking, thread continuity, correction chains), live vine watcher, force rebuild, integrity check, directory wiring
- `vine_prompts.rs` — 4 vine-specific LLM prompts

**Modified files:**
- `types.rs` — Added `Vine` to `ContentType`, `Era`/`Transition`/`HealthCheck`/`Directory` to `AnnotationType`, plus `VineBunch`, `VineBunchMetadata`, `VineDecision`, `VineCorrection`, `BunchDiscovery`, `IntegrityReport` structs
- `db.rs` — Added `vine_bunches` table, CHECK constraint migration, `list_vine_bunches`, `get_annotations_by_type`, `get_faq_nodes_by_prefix`, `delete_steps_above_depth`, `delete_vine_annotations_by_type`, `count_nodes_at_depth`
- `mod.rs` — Added `VineBuildHandle` struct, `vine_builds` field on `PyramidState`
- `routes.rs` — 10 vine HTTP routes (build, build-status, bunches, eras, decisions, entities, threads, drill, rebuild-upper, integrity)
- `build.rs` — Made 6 helpers `pub(crate)`, resume-state fix (step-without-node detection)
- `slug.rs` — Made `slugify()` and `validate_slug()` public
- `main.rs` — `ContentType::Vine` match arms, `vine_builds` initialization

**CLI** (`mcp-server/src/cli.ts`):
- 9 vine commands: vine-build, vine-bunches, vine-eras, vine-decisions, vine-entities, vine-threads, vine-drill, vine-rebuild-upper, vine-integrity

### Frontend (React/TypeScript) — ~1500 lines

**New files:**
- `VineViewer.tsx` — Main vine page with 3 tabs (Timeline, Explore, Intelligence), horizontal timeline with bunch cards, ERA markers, transition badges, collapsible apex
- `VineBuildProgress.tsx` — Vine-specific build progress polling
- `VineDrillDown.tsx` — Two-panel hierarchical drill-down with sub-apex directory navigation
- `VineIntelligence.tsx` — 6-tab intelligence view (ERAs, Decisions, Entities, Threads, Corrections, Integrity)

**Modified files:**
- `AddWorkspace.tsx` — Added Vine content type, multi-directory picker, paste-path input with Cmd+Shift+. hint, vine-specific build flow
- `PyramidDashboard.tsx` — Vine card actions (Open Vine, Add Folders overlay), vine view routing
- `dashboard.css` — ~500 lines of vine-specific styles

---

## Known Bugs

### Frontend Bug 1: "TypeError: Load failed" on Create & Build Vine
- **Screenshot:** User sees the confirm step with correct source/type/directories, clicks "Create & Build Vine", gets "TypeError: Load failed"
- **Likely cause:** The `handleVineCreate` function in `AddWorkspace.tsx` creates the slug via Tauri `invoke('pyramid_create_slug')` then tries to POST to `http://localhost:8765/pyramid/vine/build`. The fetch may be failing because:
  - The port might be different (check actual server port)
  - CORS issues (Tauri webview → localhost)
  - The API base URL is hardcoded as `PYRAMID_API_BASE = "http://localhost:8765"` but should use the same mechanism the rest of the app uses
  - The slug might already exist from previous test attempts
- **Fix needed:** Check how other components make HTTP calls to the pyramid API. The rest of the app uses Tauri `invoke()` commands, not direct fetch. The vine build endpoint may need a Tauri command wrapper.

### Frontend Bug 2: Duplicate directory selection steps
- **Screenshot:** Step 1 "Directories" and Step 3 "Folders" both ask for directory selection
- **Cause:** The AddWorkspace wizard has an existing Step 1 (directory selection for code/doc/conversation) AND the vine-specific Step 3 "Folders" (vine directory selection). For vine type, Step 1 should be skipped or merged with Step 3.
- **Fix needed:** When content type is Vine, either skip Step 1 entirely or make Step 1 the vine directory picker and skip Step 3.

### Backend Bug: Bunch builds sometimes fail at apex synthesis
- **Symptom:** Bunches build through L4 (depth 4) but fail to produce L5 apex
- **Root cause identified by separate session:** Pipeline step metadata could exist without corresponding node row. The resume logic trusted steps-without-nodes, skipping work that was never completed.
- **Fix applied:** `build.rs` now checks both step AND node exist before treating work as complete
- **Status:** Fix is in the codebase but needs end-to-end validation. Vine build test showed bunches still failing — may need additional debugging.

### Backend Issue: vine_bunches FK constraint prevents clean deletion
- Deleting a vine slug cascade-deletes vine_bunches rows, but bunch slugs in pyramid_slugs may have other FK references that prevent deletion
- Need to verify the cascade chain is complete

---

## Architecture Summary

### The Grape/Bunch/Vine Model
- **Grape** = any single node in a conversation pyramid
- **Bunch** = one complete conversation pyramid (all grapes from one session)
- **Vine** = meta-pyramid where bunches connect at the top
- **Vine L0** = apex + one level down from each bunch (~3 nodes per bunch)
- **Everything is a contribution** — ERAs are annotations, decisions are FAQ entries, thread continuity is web edges. No parallel infrastructure.

### Key Design Decisions
1. One new table only: `vine_bunches` (tracks conversation sessions)
2. Bunch slugs use `--bunch-` separator (flat naming, no slashes — slug validation rejects `/`)
3. LLM-only clustering at L1 (no algorithmic fallback)
4. Multi-directory support: `discover_bunches()` accepts `&[PathBuf]`
5. `VineBuildHandle` on `PyramidState` for concurrency guard
6. Force rebuild clears L2+ only, preserves L0+L1

### Audit History
- **4 full Conductor audit cycles** (Stage 1 + Stage 2) across Phases 1-6
- **2 cross-phase holistic audits**
- **1 validation pass** on single-auditor findings
- Total: ~60 annotations contributed to the pyramid
- All critical and major findings were fixed
- Plan document: `/Users/adamlevine/AI Project Files/agent-wire-node/docs/plans/vine-conversation-system-v2.md`

---

## Files to Audit

### Priority 1: Frontend bugs (blocking user)
- `src/components/AddWorkspace.tsx` — vine creation flow, directory step duplication, API call mechanism
- `src/components/VineBuildProgress.tsx` — polls vine build status
- `src/components/PyramidDashboard.tsx` — vine card rendering, Add Folders overlay

### Priority 2: Backend integration (vine build pipeline)
- `src-tauri/src/pyramid/vine.rs` — entire file (~3000 lines), focus on `build_vine()`, `build_all_bunches()`, `build_bunch()`
- `src-tauri/src/pyramid/build.rs` — resume-state fix, `build_upper_layers()` completion logic
- `src-tauri/src/pyramid/routes.rs` — vine route handlers, especially `handle_vine_build`

### Priority 3: Frontend components (untested)
- `src/components/VineViewer.tsx` — timeline rendering, data fetching
- `src/components/VineDrillDown.tsx` — tree walk, directory annotations
- `src/components/VineIntelligence.tsx` — 6 intelligence tabs

### Priority 4: Backend completeness
- `src-tauri/src/pyramid/vine.rs` — intelligence passes (ERA detection, entity resolution, etc.)
- `src-tauri/src/pyramid/vine.rs` — live vine watcher, force rebuild, integrity check

---

## Pyramid Access

The code pyramid is live and self-updating via DADBEAR:

```bash
CLI="/Users/adamlevine/AI Project Files/agent-wire-node/mcp-server/dist/cli.js"
SLUG=agent-wire-nodecanonical

node "$CLI" apex $SLUG
node "$CLI" search $SLUG "vine"
node "$CLI" drill $SLUG <node_id>
node "$CLI" annotations $SLUG
```

~60 audit annotations are already in the pyramid from prior sessions.

---

## Recommended Next Steps

1. **Fix the two frontend bugs** (TypeError on create, duplicate directory steps)
2. **End-to-end test** the vine build through the UI
3. **Verify** the resume-state fix actually works for apex synthesis
4. **Audit** the full frontend surface (VineViewer, VineDrillDown, VineIntelligence are untested)
5. **Test** with the 71 GoodNewsEveryone conversations once the 3-conversation MVP works

---

## Test Data

**3-conversation MVP:**
- `/Users/adamlevine/.claude/projects/-Users-adamlevine-AI-Project-Files/` (3 JSONL files, 786/1771/870 messages)

**Full 71-conversation set:**
- `/Users/adamlevine/.claude/projects/-Users-adamlevine-AI-Project-Files-GoodNewsEveryone/` (71 JSONL files)
