# Audit: Mission #3 Plan — Recursive Folders + Vines (RETRY of card 5e9dc)

**Audit type:** plan-only gate | **Auditor:** deepseek-jackie | **Date:** 2026-04-26
**Subject:** `docs/plans/m3-folders-vines-plan.md` committed at `dc50a42` by deepseek-peterman
**Branch:** `puddy/m3-folders-vines-plan`
**Anchor:** `docs/handoffs/handoff-2026-04-11-folder-nodes-as-checklists.md`
**Wire contribution:** `04fe5ef6-c55c-434a-9f80-d1e2664fc86d` (peterman, 2026-04-26T03:37:50Z)

---

## Verdict: APPROVE_WITH_NITS

The plan covers all four of Adam's framing steps, cites the anchor handoff doc, decomposes into 10 sub-cards each with WHAT/WHERE/ACCEPTANCE-shape (acceptance implicit in some — see nits), and contains zero implementation code. Fit for bania to execute. Five non-blocking nits below; puddy may fold back or bania resolves during implementation.

---

## Acceptance Trace

### 1. Step 1 — 65-FAILED diagnosis ✓ PASS (lines 19–63)

Peterman provides a full UI-to-dispatch trace (lines 23–29), root-cause diagnosis (lines 31–35): `spawn_initial_builds` routes every leaf through `spawn_question_build` → `run_decomposed_build`, but Code/Document leaves have no `QuestionTree` and `run_decomposed_build` fails before step 0. Evidence with source line citations (lines 37–42). Proposed fix (lines 44–47): mirror `build_runner::run_build_from` dispatch. Acceptance: `cargo check`, zero-`Err` dispatch, step ≥ 1, fix isolation. Risk mitigation via Card 3.1 repro. **Concrete and falsifiable.** ✓

### 2. Step 2 — filemap.yaml schema + scan/curate/build split + idempotency ✓ PASS (lines 66–99)

All eight anchor decisions encoded. File location: `.understanding/filemap.yaml` (line 71). Top-level keys: `scan:`, `entries:`, `deleted:` (line 72). Scanner vs user field ownership (lines 73–74). Idempotency explicit (line 75, line 91): "field-level merge, scanner overwrites scanner columns; user columns preserved." New files → `user_included: null` (line 76). Deleted files → `deleted:` tombstone (line 77). Six `detected_inclusion` values per anchor (line 78). Scan/curate/build decoupling: `plan_recursive` split into scan-writes-filemaps + plan-reads-filemaps (line 80). Acceptance: depth ≥ 3, field completeness, exclusion patterns, idempotent re-scan, add/delete lifecycle, `cargo check`. **Comprehensive.** ✓

### 3. Step 3 — User curation: editor-native + UI ✓ PASS (lines 102–135)

Two-phase design. Phase 3a Editor-native (lines 106–109): document YAML format with inline comments, no code changes, users edit in any editor. Phase 3b UI (lines 111–116): checklist component reads via `pyramid_read_filemap`, writes via `pyramid_write_filemap_entries`, filesystem-is-the-model (no local UI state). Specific frontend files: `FilemapCuration.tsx`, `FilemapSummary.tsx`, `useFilemap.ts`, `AddWorkspace.tsx`. Backend: two Tauri commands in `main.rs`. Acceptance: editor readability, build-after-edit, UI load, 500ms toggle, disk persistence, dual `cargo check` + `npm run build`. **Editor-native is concrete ("the format IS the interface").** ✓

### 4. Step 4 — Build-from-checklists + migration story ✓ PASS (lines 138–180)

Seven-step new flow (lines 142–149): walk → collect `user_included: true` → group → `IngestionPlan` → `execute_plan` → write post-build fields. Topological build ordering (line 151). Inheritance: `children_default: unchecked | include | skip` with explicit semantics (lines 153–158). Migration: Q3 (line 236) recommends forward-only — new scans use new model, existing pyramids stay on old. Acceptance: exact build count, skip `false`/`null`, post-build writes to filemap, inheritance, idempotent re-run. **Full pipeline specified; migration delegated to Partner as open question.** ✓

### 5. Anchor doc cited explicitly ✓ PASS

Line 5: `**Anchor:** docs/handoffs/handoff-2026-04-11-folder-nodes-as-checklists.md`. Line 244 in References. Decisions 1–8 (lines 70–78) trace directly to handoff §55–71. Inheritance (lines 153–158) from handoff Q4. Migration Q3 (line 236) from handoff Q6.

### 6. Ten sub-cards with WHAT/WHERE/ACCEPTANCE-shape ✓ PASS (with nits)

All 10 present (lines 185–214: 3.1–3.10). Each has `**WHAT:**`. WHERE embedded in descriptions or parent step sections. ACCEPTANCE-shape quality:

| Card | WHAT explicit | WHERE explicit | ACCEPTANCE explicit |
|------|:---:|:---:|:---:|
| 3.1 — Reproduce 65-FAILED | ✓ | ✓ (diagnosis path) | ✓ (capture error + write diagnosis) |
| 3.2 — Fix dispatch routing | ✓ | ✓ (`folder_ingestion.rs`) | ✓ (regression test step ≥ 1) |
| 3.3 — filemap.rs module | ✓ | ✓ (`src-tauri/src/pyramid/filemap.rs`) | ✓ (5 test cases listed) |
| 3.4 — scan→filemap wrapper | ✓ | ✓ (`folder_ingestion.rs`) | Partial — delegates to Step 2 |
| 3.5 — read/write IPCs | ✓ | ✓ (`main.rs`) | Partial — delegates to Step 3 |
| 3.6 — Curation UI | ✓ | ✓ (4 component files) | Partial — delegates to Step 3 |
| 3.7 — build_plan_from_filemaps | ✓ | ✓ (`filemap.rs`) | Partial — delegates to Step 4 |
| 3.8 — pyramid_build_from_filemaps | ✓ | ✓ (`main.rs`) | Partial — delegates to Step 4 |
| 3.9 — Integration test | ✓ | ✓ (`folder_ingestion.rs` tests) | ✓ (asserts only checked built) |
| 3.10 — Docs & handoff | ✓ | ✓ (`docs/handoffs/`) | Partial — summary topics listed |

**Nit:** Cards 3.4–3.8, 3.10 delegate acceptance to parent step sections. Bania cross-references upward. Workable but not fully self-contained.

### 7. No implementation code ✓ PASS

Pure markdown. No Rust, TypeScript, YAML, or other code blocks. All line numbers reference existing source.

---

## Class Scans

**Phase-9 scope creep:** ✗ CLEAN. Non-goal line 221: "Do NOT pre-decompose Phase 9 from Mission #2 (still on hold)."

**New dependencies not in anchor:** ✗ CLEAN. `serde_yaml` implied by line 98 (YAML round-trip risk) — natural consequence of `.yaml` format choice; anchor left format open. Frontend components are first-order Step 3 consequences. No unexpected external deps.

**TODO placeholders:** ✗ CLEAN. No "TODO", "FIXME", or "TKTK" found. Open questions are explicit and delegated to Partner (lines 230–238).

---

## Findings (non-blocking nits)

### N1: "Five detected_inclusion values" lists six (line 78)
Says "Five" but enumerates: `included`, `excluded_by_pattern`, `excluded_by_size`, `excluded_by_type`, `unsupported_content_type`, `failed_extraction` — that's six. The anchor handoff references "five uncovered categories" (the exclusion reasons); `included` is a sixth positive value. Fix: change "Five" to "Six" or restructure as "One positive plus five exclusion categories."

### N2: Sub-card ACCEPTANCE criteria implicit for cards 3.4–3.8, 3.10
These cards delegate to parent step acceptance sections. Bania must cross-reference upward. Add a one-line `**ACCEPTANCE:**` to each for self-contained one-shot execution.

### N3: CLI shorthand in Tauri context (line 167)
"`wire-node build <root>` with 10/50 `user_included: true`" — the app is Tauri desktop; `wire-node` CLI may not exist. Should reference `pyramid_build_from_filemaps(root)` (Card 3.8).

### N4: Card 3.1 WHERE is inline, not a separate line
Diagnosis path `docs/plans/m3-65-failed-diagnosis.md` mentioned inline but could be pulled to a `**WHERE:**` for consistency with other sub-cards.

### N5: `serde_yaml` crate not declared as new dependency
YAML round-trip risk at line 98 implies `serde_yaml`. Not listed in dependencies. Bania discovers during Card 3.3. Minor — add to Step 2 files/dependencies list.

---

## Wire Contribution Cross-Check

Contribution `04fe5ef6-c55c-434a-9f80-d1e2664fc86d` is a verbatim copy of the plan, published 2026-04-26T03:37:50Z under pseudonym `wire_agent_e0511fff` (peterman). Type: `mission`, significance: 0.3. No entities/topics/claims. Consistent with plan-only pass — provenance recorded; substance in committed file.

---

## Summary

| Criterion | Result |
|-----------|--------|
| Step 1 (65-FAILED diagnosis) | ✓ PASS |
| Step 2 (filemap.yaml + idempotency) | ✓ PASS |
| Step 3 (editor-native + UI curation) | ✓ PASS |
| Step 4 (build-from-checklists migration) | ✓ PASS |
| Anchor doc cited | ✓ PASS |
| 10 sub-cards WHAT/WHERE/ACCEPTANCE | ✓ PASS (5 nits) |
| No implementation code | ✓ PASS |
| Phase-9 scope creep | ✗ CLEAN |
| New deps not in anchor | ✗ CLEAN |
| TODO placeholders | ✗ CLEAN |

**Verdict: APPROVE_WITH_NITS** — 5 nits, none blocking.
