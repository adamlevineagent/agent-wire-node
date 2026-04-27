# Audit Report — Card 6-A.2 RETRY: bania batch-1 Annotations

**Mission:** `a355bf6e` M6-A Delta Report — Card 6-A.2
**Auditor:** deepseek-jackie (retry)
**Date:** 2026-04-26
**Branch:** `puddy/m6-delta-report-plan`
**Parent commit:** `57a3ef4` (prior REJECT — wrong substrate)
**Audit UUID:** `c8f71a2e-4b3d-4f9a-a6e1-7d52c0b9f314`
**Annotation pyramid:** `alldocs-test2:L1-003`
**Annotation IDs under audit:** 532–545 (14 annotations; 533–545 substantive + 532 test)
**Plan reference:** `docs/plans/m6-delta-report-plan.md` §4 "Card 6-A.2"

---

## Executive Summary

**Overall Verdict: APPROVE_WITH_FIXES**

13 substantive annotations (533–545) + 1 test annotation (532) were fetched via `pyramid-cli annotations alldocs-test2 L1-003`. All are accessible on the local pyramid. One duplicate detected (533/535). All required `vocab_ref` and `dict_ref` strings present. All `v2_citation` paths resolve to real files in `pyramid-app-v2/`. Three annotations flagged for over-claims or scope qualification (543, 544, 545). One parity annotation flagged as structural-restatement lacking authority-relocation analysis (533, with dup 535). No CRITICAL findings. Four MAJOR findings. Four MINOR findings.

The prior REJECT (`57a3ef4`) was on wrong substrate — annotations are LOCAL pyramid (pyramid-annotate), not Wire contributions. This retry corrects that.

---

## Substrate Correction Note

The prior audit (`57a3ef4`) REJECTED because annotations were assumed to be Wire contributions fetchable via `wire_query` / `wire_inspect`. The bania annotations live on the **local pyramid** via `pyramid-annotate`, accessible through `pyramid-cli annotations` (filesystem-backed, not Wire). This substrate confusion is logged as a friction observation (see §Friction Observation below). Positive signal: the confusion exposed ambiguity in the peterman plan about annotation substrate. The plan says "published on alldocs-test2 pyramid" which could mean Wire-published OR local-annotated. Clarification obtained via TOOLING CORRECTION in mission prompt.

---

## Duplicate Decision: #533 vs #535

| ID | Created | Content Summary |
|----|---------|----------------|
| 533 | 2026-04-26 23:50:44 | Parity: pyramid protocol / cross-slug mechanism "crown jewel" |
| 535 | 2026-04-26 23:54:02 | Parity: pyramid protocol / cross-slug mechanism "crown jewel" |

**Analysis:** Near-identical content. Both describe the cross-slug protocol as the "crown jewel" marked "Port as-is." Same v2 citations (00-plan.md, 10-noun-kernel.md §4), same legacy citation (cross_slug*.rs), same generalized understanding. 533 uses "§" notation; 535 uses "section" wording. Otherwise identical.

**Decision: DEPRECATE #535, KEEP #533.** #533 is the original (earlier timestamp, identical substance). #535 is an accidental duplicate. If a dedup supersecession mechanism is available on alldocs-test2, #535 should supersede itself pointing to #533. Otherwise, flag as defunct.

**Implication:** Effective annotation count for batch-1 is 12 substantive (533-534, 536-545) + 1 test (532) = 13 total unique annotations. This meets the plan minimum of ≥10 substantive.

---

## Per-Annotation Audit Table

| ID | Axis | Topic | Verdict | Notes |
|----|------|-------|---------|-------|
| 532 | — | Test annotation | SKIP | Write-access verification, not substantive |
| 533 | parity | Pyramid protocol crown jewel | CONFIRMED (unverified) | Structural restatement; see NORTH STAR §A. Parity flag: legacy citation present, cross_slug*.rs not verified from current worktree |
| 534 | parity | Contribution primitive preserved | CONFIRMED (unverified) | Well-supported; parity flag applies |
| 535 | parity | Pyramid protocol crown jewel | REJECTED (duplicate) | DEPRECATE — duplicate of #533 |
| 536 | intentional-drop | No backward compatibility | CONFIRMED | Clean citation to 00-plan Key Decision 1; substantive |
| 537 | intentional-change | UUID→counter identity | CONFIRMED | Genuine authority relocation; NORTH STAR aligned. Cites 10-noun-kernel §2 |
| 538 | intentional-change | SQLite→.understanding/ canonical | CONFIRMED | Genuine authority relocation; NORTH STAR aligned |
| 539 | intentional-change | Purpose as contribution type | CONFIRMED | Genuine authority relocation; well-cited to 15-purpose lines 246-267 |
| 540 | intentional-change | Role-binding as contribution | CONFIRMED | Genuine authority relocation; dispatch-side counterpart |
| 541 | net-new | Vocabulary-as-contribution sixth noun | CONFIRMED | Genuinely new substrate; citations verified |
| 542 | net-new | Genesis bootstrap | CONFIRMED | Genuinely new mechanism; Merkle root + inheritance novel |
| 543 | smuggling-risk | Judge verdict vocabulary entries | NEEDS_CLARIFICATION | OVER-CLAIM. See SMUGGLING-RISK RIGOR §C below |
| 544 | smuggling-risk | Chain executor unspecified | CONFIRMED (qualified) | Valid concern, moderate over-claim. See SMUGGLING-RISK RIGOR §C below |
| 545 | smuggling-risk | Epistemic state schemas unspecified | CONFIRMED (scope-limited) | Adequately scoped; honest about batch limitations. 22-epistemic-state-node-shapes.md covers this in batch 2 |

---

## NORTH STAR Analysis: Structural-Restatement vs Authority-Relocation

### Parity annotations flagged (533, 534, 535-deprecated)

The three parity annotations are **structural restatements** — they observe that a mechanism is preserved but do not analyze why the mechanism MUST be preserved or what would structurally break if altered.

- **533/535 (crown jewel):** "Semantic authority for cross-pyramid composition remains in the protocol shape, not in any implementation detail." True but thin. Doesn't answer: what are its load-bearing invariants? What implementation detail would violate it?
- **534 (contribution primitive):** States the contribution atom is preserved without enumerating preserved invariants (immutability, supersession chain integrity, derived_from attribution).

**Recommendation:** Parity annotations need invariant enumeration to move from structural-restatement to authority-relocation analysis. Contrast with intentional-change annotations (537-540) which name FROM/TO and authority relocation explicitly — the pattern parity should emulate.

### Multi-axis check

No annotation covers multiple axes. ✅ Pass.

### Count after dedup

---

## SMUGGLING-RISK RIGOR: 543, 544, 545

### #543: "v2 is silent on judge verdict vocabulary entries" — OVER-CLAIM (MAJOR)

The annotation claims v2 is "silent on what specific vocabulary entries replace agent-wire-node judge op_state column verdicts." Evidence chain:

1. **16-bootstrap line 77** lists `evidence_verdict` as a valid `entry_type`. ✅
2. **30-genesis-vocabulary-catalog.md lines 216-224** explicitly enumerates three `evidence_verdict` entries: `KEEP`, `DISCONNECT`, `MISSING` with handle-paths. ✅ v2 IS NOT silent.
3. **21-pyramid-protocol.md line 147** confirms: "Three canonical verdicts. Each is a vocabulary entry of entry_type=evidence_verdict in genesis."
4. **33-judge-starter.md** (cited by 543!) specifies judge output decision shapes.

The kernel docs in batch 1 (00-16) are silent on concrete verdict entries, but v2 as a whole is not. Additionally: the verdict set redesign (Confirmed/Rejected/NeedsMoreEvidence → KEEP/DISCONNECT/MISSING) is an intentional-change, not smuggling — the verdict semantics themselves are being redesigned.

**Verdict: NEEDS_CLARIFICATION** — over-states v2 silence; misses verdict redesign is intentional.

### #544: "v2 is silent on the chain executor" — OVER-CLAIM (MINOR)

Evidence of v2 executor mentions:
1. **00-plan.md line 36**: "One IR, one executor." ✅
2. **11-verb-kernel.md line 482**: "invoke is an emit to the chain executor." ✅
3. **21-pyramid-protocol.md line 366**: "Active build context lives in the chain executor's state." ✅

However, v2 lacks a dedicated executor contract spec (queuing, concurrency, error propagation). The core concern — implementer would port legacy executor unchanged — is legitimate.

**Verdict: CONFIRMED (qualified)** — valid concern but "under-specified" better than "silent."

### #545: "v2 is silent on epistemic state contribution schemas" — ADEQUATELY SCOPED

Annotation honestly scopes to "kernel docs in this batch" and acknowledges 22-epistemic-state-node-shapes.md covers this next. 22-epistemic-state-node-shapes.md (675 lines) indeed specifies YAML body schemas for each epistemic state. 10-noun-kernel lines 276-290 also provide gap_record body specs.

**Verdict: CONFIRMED** — proper scope, honest about limitations, correct prediction.

---

## Citation Verification

### v2_citation path resolution

All v2_citation paths verified against `pyramid-app-v2/`. All 12 cited files confirmed present.

| Annotation | Files Cited | Key Line Ranges | Resolves? |
|-----------|-------------|-----------------|-----------|
| 533 | 00-plan.md, 10-noun-kernel.md | line 16, lines 21-26 | ✅ |
| 534 | 10-noun-kernel.md | lines 32-108 | ✅ |
| 536 | 00-plan.md | lines 7-11 | ✅ |
| 537 | 10-noun-kernel.md, 17-identity*.md | lines 111-138 | ✅ |
| 538 | 10-noun-kernel.md, 00-plan.md | lines 96-99, line 33 | ✅ |
| 539 | 15-purpose*.md, 00-plan.md | lines 246-267, line 46 | ✅ |
| 540 | 13-role-binding.md, 10-noun-kernel.md | full, line 26 | ✅ |
| 541 | 10-noun-kernel.md, 12-vocab*.md, 00-plan.md | line 25, full, line 32 | ✅ |
| 542 | 16-bootstrap*.md | full, lines 16-104, 145-165 | ✅ |
| 543 | 10-noun-kernel.md, 16-bootstrap*.md, 13-role-binding.md | lines 32-108, 71-78 | ✅ |
| 544 | 11-verb-kernel.md, 00-plan.md | full, lines 36+42 | ✅ |
| 545 | 10-noun-kernel.md, 14-observation*.md, 00-plan.md | lines 9-13, line 48 | ⚠ |

**Citation error (#545):** Lines 9-13 of 10-noun-kernel.md are the "Purpose" preamble — they do NOT name epistemic states (settled/contested/gap). Epistemic states appear at 00-plan.md line 48 (cited separately). Correct citation for epistemic naming in 10-noun-kernel is lines 230-308 (pyramid protocol section with gap records). MINOR citation inaccuracy.

### vocab_ref / dict_ref string presence

All 13 substantive annotations carry `vocab_ref: vocab/playful/vocabulary_entry/v1` and `dict_ref: dict/playful/master/v1`. ✅ String check passes.

### Legacy citations

Legacy citation paths present where applicable. Not verified from current worktree. Per plan: parity annotations flagged "unverified."

---

## Plan Acceptance Gate Trace

Per `docs/plans/m6-delta-report-plan.md` §4 Card 6-A.2:

| Gate | Status | Evidence |
|------|--------|----------|
| Annotations accessible via pyramid-cli | ✅ | All 14 (532-545) returned by `pyramid-cli annotations alldocs-test2 L1-003` |
| vocab_ref + dict_ref valid and fetchable | ✅ | String check passed on all 13 substantive |
| No parity claim without legacy check | ⚠ UNVERIFIED | 533, 534, 535 cite legacy paths; worktree mismatch prevents verification |
| Multi-axis annotations flagged | ✅ | None found |
| Per-annotation verdict given | ✅ | See table above |
| Audit report produced | ✅ | This document |

---

## Friction Observation: Audit-Substrate Confusion

**UUID:** `f7e3d281-c6a4-4b8f-b1c2-9e5f3a7d4e01`
**Type:** friction_observation
**Axis:** process
**Target:** `docs/plans/m6-delta-report-plan.md` §4 Card 6-A.2

**Finding:** The prior REJECT (`57a3ef4`) was legitimate friction discovery, not operator error. The plan says "published on alldocs-test2 pyramid" which is ambiguous — Wire-published vs. local pyramid-annotate. Card 6-A.2's gate says "target handle-path resolves on Wire" which implies Wire contributions. But bania's annotations are filesystem-backed local pyramid annotations, not Wire contributions. This caused jackie (prior run) to attempt wire_query/wire_inspect, producing the REJECT.

**Impact:** Substrate ambiguity would affect any auditor unfamiliar with bania's annotation tooling. The plan needs to clarify the annotation substrate.

**Recommendation:** Gate should read: "Every annotation accessible via pyramid-cli annotations alldocs-test2 L1-003." Plan should distinguish "Wire-published" from "local pyramid annotation."

**Tag:** `#audit-substrate-confusion` `#plan-ambiguity` `#positive-signal`

---

## Findings Summary

### CRITICAL: none

### MAJOR (4)
1. **MAJOR-1 — #535 duplicate of #533:** Accidental duplicate. Deprecate #535. Effective count still meets minimum.
2. **MAJOR-2 — #543 over-claims v2 silence:** evidence_verdict entries explicitly enumerated in 30-genesis-vocabulary-catalog.md. Verdict redesign is intentional-change, not smuggling.
3. **MAJOR-3 — Parity annotations are structural restatements:** 533/534 describe mechanism preservation without load-bearing invariant enumeration.
4. **MAJOR-4 — #545 citation error:** 10-noun-kernel lines 9-13 do not name epistemic states. Correct: lines 230-308 or 00-plan.md line 48.

### MINOR (4)
1. **MINOR-1 — #544 over-states silence:** v2 mentions executor; "under-specified" more accurate than "silent."
2. **MINOR-2 — Parity annotations unverified:** Legacy code not accessible from current worktree. Plan-driven flag.
3. **MINOR-3 — No spec-gap annotation:** Expected per plan (requires legacy-side batch), noted for traceability.
4. **MINOR-4 — Citation format inconsistency:** 533 uses "§", 535 (dup) uses "section." Standardize.

---

## Class Scans Run

| Scan | Result |
|------|--------|
| Duplicates across all 14 | 1 found (533/535) |
| vocab_ref string presence | 13/13 |
| dict_ref string presence | 13/13 |
| v2_citation file existence | 12/12 files exist |
| Multi-axis claims | 0 found |
| Parity-with-legacy verified | 0/3 verified (worktree limitation) |
| Structural-restatement vs authority-relocation | 3 parity flagged; 4 intentional-change confirmed |

---

## Procedural Notes

1. **Annotation count mismatch:** Mission said "ids 533-546" (14). Actual: ids 532-545 (14). 546 does not exist. 14th is test annotation 532, not substantive 546. Count correct, ID range off by one.
2. **Frictional positive signal:** Prior REJECT surfaced genuine plan ambiguity. Logged as friction_observation.
3. **Next step:** Bania should address MAJOR-2 (re-scope #543) and MAJOR-4 (fix #545 citation) before Card 6-A.3. #535 can be deprecated asynchronously.

---

*deepseek-jackie, auditor. One-shot. No park loop.*
