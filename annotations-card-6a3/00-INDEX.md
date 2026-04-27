# Card 6-A.3 — Second v2 Doc Batch Annotations

**Builder:** deepseek-bania
**Branch:** puddy/m6-delta-report-plan @ 223d63b
**Target pyramid:** alldocs-test2
**Date:** 2026-04-26

## Annotation count: 10

| # | Axis | Target Doc | Title |
|---|------|-----------|-------|
| 1 | intentional-change | 17-identity | Counter-based node_id replaces UUIDs |
| 2 | intentional-change | 30-genesis-catalog | Genesis vocabulary catalog as shipped binary contributions |
| 3 | intentional-change | 31-genesis-role-binding | Role-binding as supersedable vocabulary contribution |
| 4 | intentional-change | 32-cascade-handler | DADBEAR re-expressed as YAML cascade-handler chain |
| 5 | intentional-change | 22 + 20 | folder_node as 5th explicit shape living inside folder |
| 6 | net-new | 33-judge | Three-tier judge architecture |
| 7 | net-new | 22 | debate_node as explicit first-class epistemic shape |
| 8 | parity | 21 + 30 | Evidence verdicts KEEP/DISCONNECT/MISSING preserved |
| 9 | parity | 21 | Pyramid protocol layer-by-layer structure preserved |
| 10 | parity | 21 + 17 | Cross-slug evidence links and vine/counter-pyramid preserved |

## Axis distribution

| Axis | Count | Annotation #s |
|------|-------|--------------|
| intentional-change | 5 | #1, #2, #3, #4, #5 |
| net-new | 2 | #6, #7 |
| parity | 3 | #8, #9, #10 |
| intentional-drop | 0 | — |
| smuggling-risk | 0 | — (see deviations) |
| spec-gap | 0 | — (requires legacy-side, Card 6-A.5) |

## Acceptance gates

- [x] All 8 docs read in full
- [x] 10 annotations produced (≥6 min, ≤12 max)
- [x] ≥2 annotations with axis: intentional-change (5 provided)
- [x] Every annotation carries vocab_ref and dict_ref
- [x] Every v2_citation resolves to real file path + section
- [x] No purely editorial annotations — each names a substantive delta

## Deviations

- **No smuggling-risk annotations:** Cross-doc evidence sweep (Jackie Batch-1 fix) resolved the one candidate (30-genesis entry body schemas deferred to "genesis tree"). 16-bootstrap § Embedded genesis tree (L65-102) specifies app bundle ships full contribution copies. Catalog = summary, embedded tree = bodies. Documentation separation, not smuggling risk.
- **No intentional-drop found:** These 8 docs redesign rather than drop. "No UUIDs" is part of the intentional-change identity model, not standalone drop.
- **No spec-gap:** Per plan, requires legacy-side comparison (Card 6-A.5).

## Doc completeness flags

**Deferred Phase 0 closure verification (structural pattern):**
- 32-cascade-handler (L396-400): legacy code path mapping deferred to 80-kernel-closure audit
- 33-judge (L461-463): gating decision mapping deferred to closure audit
- 22-epistemic-shapes (L673-675): node type mapping deferred to closure audit
- 30-genesis-catalog (L473-490): enum-to-entry mapping deferred to closure audit

Each doc asserts Phase 0 closure coverage but defers verification to 80-kernel-closure-v1.md. Coverage claims are conditional on audit passing.

**Genuinely complete:** 17-identity (self-contained, invariants, failure modes, edge cases), 20-folder-layout (900+ lines, full physical layout spec), 21-pyramid-protocol (complete protocol spec).

## North Star alignment

5 intentional-change annotations address "where semantic authority currently lives vs where v2 requires it":
- #1: UUID allocation → counter manifest contribution
- #2: LLM-extracted → shipped contribution set
- #3: hardcoded Rust dispatch → vocabulary contribution with supersession
- #4: Rust code (dadbear_compiler.rs etc.) → YAML chain spec
- #5: SQLite operational state → contribution with handle-path identity
