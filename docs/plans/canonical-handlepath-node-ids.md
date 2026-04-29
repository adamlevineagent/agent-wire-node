# Canonical Handle-Path Node IDs

> **Status:** Draft for review
> **Origin:** Triage of `who-is-building-this` failure (2026-04-28). What looked like cross-slug evidence contamination turned out to be the system's two coexisting ID forms making provenance ambiguous from the row alone. See sibling plan `evidence-loop-resilience-and-capture.md` (I10) — that plan adds a runtime guardrail; this plan removes the underlying ambiguity at the data layer.
> **Owner:** TBD
> **Branch:** `feat/canonical-handlepath-ids` (proposed)
> **Backcompat:** None. Adam directive 2026-04-28 — no migration shims, no dual-form support during transition. Pyramids rebuild on the new schema.

---

## 1. The Problem

Pyramid node IDs today exist in two coexisting forms, mixed in the same SQL columns:

| Form | Example | Meaning | Where used |
|---|---|---|---|
| Bare | `L0-001`, `Q-L0-005`, `L1-003` | Only meaningful inside an implicit `(slug, build)` scope | Most of `pyramid_nodes.id` and `pyramid_evidence.{source,target}_node_id` |
| Partial qualified | `docpyramidv1/0/L0-002` | `slug/depth/id` — emitted only when crossing slug boundaries | Cross-slug rows synthesized by `evidence_answering.rs` |

Consequences:

1. **Ambiguous from the row.** You can't tell whether `L0-001` is a same-slug local ref or contamination without looking up the surrounding context. (Today's `who-is-building-this` triage misread declared cross-slug evidence as a buffer-leak bug for exactly this reason.)
2. **Lossy across builds.** Bare `L0-001` carries no `build_id`. Two builds of the same slug both have `L0-001` rows; nothing structurally prevents a stale evidence row from a prior build pointing at the wrong layer.
3. **Non-canonical across operators.** Owner is absent from the ID. Two operators with the same slug name collide in any cross-pyramid composition or import.
4. **Asymmetric to the Wire.** Wire contributions live at handle paths like `playful/117/2`. Pyramid nodes live at `L0-001`. Two ID systems for the same conceptual object — a citable artifact — inside the same product.
5. **Costs runtime translation.** Code in `evidence_answering.rs`, `chain_executor.rs`, `build_runner.rs` rewrites bare IDs to qualified handle paths at the slug boundary. Every cross-slug code path branches on "is this local or qualified."

## 2. Desired End State

Every pyramid node has exactly one canonical ID, fully qualified, of shape:

```
{owner}/{slug}/{build_id}/{depth}/{node_num}
```

Stored as the actual primary key. No bare form anywhere in storage or the wire format. Bare display names (`L0-001`) become a **UX-layer rendering** of the local part (`L0-001` ≡ depth=0, node_num=1), not a storage primitive.

Same shape for:
- Same-slug refs (`playful/who-is-building-this/qb-79ab9ffa/0/1`)
- Cross-slug refs (`playful/docpyramidv1/qb-9d3e2a01/0/2`)
- Cross-pyramid composed views
- Cross-operator imports
- FAQ, annotations, supersession chains

A pyramid node IS a contribution. The Wire's existing handle-path machinery (resolve, cite, supersede, rate, flag) works on pyramid nodes natively, no special composition layer.

## 3. Design Invariants

| ID | Invariant |
|----|-----------|
| **C1** | Every node ID stored anywhere in the pyramid schema is a fully-qualified handle path. No bare IDs in any column, anywhere. |
| **C2** | Node IDs are content-addressed by `(owner, slug, build_id, depth, node_num)`. The tuple is the identity; the string form is a canonical encoding of the tuple. |
| **C3** | Evidence rows reference node IDs by their canonical form only. Same-slug, cross-slug, cross-operator — all the same column shape. The "is this local or qualified" branch in code is deleted, not replaced. |
| **C4** | Bare display IDs (`L0-001`) exist only at the UI rendering layer, derived on read from the canonical tuple. The user-facing local form is unchanged. |
| **C5** | Cross-build references are structurally impossible to confuse: a row pointing at `playful/who-is-building-this/qb-79ab9ffa/1/3` cannot be mistaken for a row pointing at `playful/who-is-building-this/qb-OLDER/1/3`. They are distinct IDs. |
| **C6** | A pyramid node IS a Wire contribution. The pyramid schema is folded into the contribution schema entirely (a node is a contribution row with `type='pyramid_node'` or similar). All node operations (read, cite, supersede, rate) go through the existing contribution machinery. Pyramid-specific fields (`depth`, `distilled`, `headline`, evidence verdict) become contribution metadata or sibling rows. *(Path B selected by Adam directive 2026-04-28 via Partner-prime DM `msg/partner-prime/2026-04-28/dm-f9bad3` — canonical per architectural lens, no-backcompat already in flight, absorb the surrounding-work cost now rather than build the thin-shadow stepping stone and redo later.)* |
| **C7** | `pyramid_slug_references` becomes redundant for evidence-resolution scoping (the handle-path itself carries the source slug). It may persist as an explicit dependency declaration for build planning, but not as a security gate for evidence rows. |
| **C8** | No migration shim. Existing pyramids are rebuilt on the new schema, not converted. Adam directive: no backcompat. |

## 4. Architectural Changes

### 4.1 Schema

`pyramid_nodes`:
- `id` becomes `TEXT PRIMARY KEY` storing the canonical handle path string.
- Add columns `owner`, `slug`, `build_id`, `depth`, `node_num` as denormalized indexed projections of the ID for query efficiency. Constraint: `id == owner || '/' || slug || '/' || build_id || '/' || depth || '/' || node_num`.
- Drop any column that exists today only to disambiguate bare IDs.

`pyramid_evidence`:
- `source_node_id` and `target_node_id` become `TEXT` storing canonical handle paths.
- Drop any cross-slug-rewrite logic in code that produces partial qualified strings.

`pyramid_candidate_links`, `pyramid_pending_repairs`, `pyramid_evidence_diagnoses` (from sibling plan): same change to any node-ID column.

### 4.2 Code paths to delete

- The bare-to-qualified rewrite in `evidence_answering.rs` at the cross-slug boundary.
- Any branch in `chain_executor.rs` of the form `if id.contains('/') { /* cross-slug */ } else { /* local */ }`.
- The implicit `(slug, build_id)` resolution context that the bare-ID lookups depend on.

### 4.3 Code paths to add

- A `NodeHandle` type with parser + canonical-string encoder. Single source of truth.
- A UI rendering helper that takes a `NodeHandle` and returns the local display form (`L0-001`) when the surrounding context is the same `(owner, slug, build_id)`, falling back to the full handle path when crossing scopes.
- A startup invariant check: every row in `pyramid_evidence` must parse as `NodeHandle` for both source and target. Loud-fail on bare strings.

### 4.4 Wire integration — Path B (decided)

Adam directive 2026-04-28 via Partner-prime DM `msg/partner-prime/2026-04-28/dm-f9bad3`: **Path B**. Fold `pyramid_nodes` into the contributions schema entirely. A node IS a contribution row with `type='pyramid_node'` (exact type-string TBD in W2 design pass). All node operations (read, cite, supersede, rate, annotate) go through the existing contribution machinery (`wire_contribute`, `wire_inspect`, `wire_read`). Pyramid-specific fields (`depth`, `distilled`, `headline`, evidence verdict, build_id, layer-cluster membership) become contribution metadata or sibling rows referencing the contribution by handle path.

Reasoning Adam ratified: canonical per architectural lens, matches the "every internal artifact is a contribution" frame already shaping the Wire-side work, and the no-backcompat directive means we don't pay migration cost twice. Path A as a thin-shadow stepping stone was only justified if Path B's surrounding-work cost was prohibitive — Adam's call is to absorb that cost now rather than build A then redo as B.

Implementation surface (sketched, refined in W2):
- A `pyramid_node` contribution carries the node's content (`headline`, `distilled`, `topics`) in the contribution body.
- Evidence relationships become contribution `derived_from` rows: a layer-N node `derives_from` its layer-(N-1) sources with verdict (KEEP/DISCONNECT) carried in the citation justification or a sibling annotation.
- `pyramid_evidence` table either disappears (collapsed into `derived_from`) or stays as a denormalized projection for query speed — W2 design call.
- Annotations on nodes already use the annotation/FAQ contribution pattern; they collapse cleanly onto contribution annotations.
- Build-time intermediate state (`pyramid_candidate_links`, `pyramid_pending_repairs`, `pyramid_evidence_diagnoses` from sibling plan) stays as build-runtime tables; they're not contributions, they're build telemetry.

## 5. Phasing (rough)

| Wave | Scope | Notes |
|---|---|---|
| **W0** | ✅ DONE 2026-04-28 — Elaine empirical scan, contribution `playful/117/4`. Verdict: 0 unresolvable, 0 malformed; raw rebuild feasible; ~7 dense slugs warrant export-rebuild-import for annotation preservation. |
| **W1** | `NodeHandle` type, parser, canonical encoder, UI renderer. No schema change yet. Standalone of W2. | 1d |
| **W2** | Path B design pass: settle the contribution `type` string (`pyramid_node` vs `pyramid_node:l0` vs per-depth types), settle whether evidence collapses into `derived_from` or stays as a projection table, settle exact contribution-metadata layout for `headline`/`distilled`/`depth`/`build_id`. Output: a written sub-spec for W3 to implement. | 1–2d |
| **W3** | Schema migration: `pyramid_nodes` table folds into `contributions`. `pyramid_evidence` either folds into `derived_from` or stays as projection per W2. Existing pyramids invalidated; rebuild on demand. | 3–5d |
| **W4** | Delete cross-slug rewrite logic and local/qualified branching across `evidence_answering.rs`, `chain_executor.rs`, `build_runner.rs`. Save paths now write contributions via `wire_contribute` (or the in-process equivalent), not direct table inserts. | 2–3d |
| **W5** | Startup invariant check (C1, C3 enforcement). Any contribution-id-shaped column that doesn't parse as `NodeHandle` is a loud-fail. | 0.5d |
| **W6** | UI rendering: friendly local display for in-scope IDs, full handle path for cross-scope. Pyramid viewer reads from contributions schema, not pyramid_nodes (which no longer exists). | 1–2d |
| **W7** | Annotation/FAQ collapse: existing per-table annotation rows migrate to contribution-annotations on the new node-as-contribution rows. ~7 dense slugs from W0 get the export-rebuild-reattach treatment. | 1–2d |
| **W8** | Evidence-loop plan I10 is now enforced by data shape; remove the runtime check from that plan and replace with a "verified at parse time" note. | 0.25d |

## 6. Acceptance Gates

- A1: `grep -rE "L[0-9]+-[0-9]{3}" src-tauri/src/pyramid/` returns zero hits in storage code (display-layer hits are fine).
- A2: `pyramid_evidence` schema check returns ZERO rows where `source_node_id` does not parse as a canonical handle path.
- A3: Cross-slug evidence resolves through the same code path as same-slug evidence. No `if id.contains('/')` branches survive in the build path.
- A4: A node row is addressable via `wire_inspect <handle_path>` and returns its body. (Path A: via join. Path B: direct.)
- A5: A rebuild of `who-is-building-this` produces evidence rows where every source and target is a fully-qualified handle path resolving inside the declared evidence universe. The triage that produced the original "looks like contamination" misread is structurally impossible.

## 7. Open Questions

- ~~**Q1 (C6 decision):**~~ ✅ RESOLVED 2026-04-28 — Path B selected by Adam directive.
- **Q2:** Does `build_id` belong in the canonical form, or is it implicit in the slug+latest-build context? Putting it in is more provenance-explicit but inflates the ID; leaving it out reintroduces the cross-build ambiguity (C5). **Recommended default: include it.** Surface to Adam if W2 design pass finds a strong reason to exclude.
- **Q3:** What does supersession look like for a pyramid node? A new build's `qb-NEWER` produces a `qb-NEWER/1/3` that semantically supersedes `qb-OLDER/1/3`. Same Wire supersession primitive, new use case. W2 design pass settles whether each rebuild emits a fresh `supersedes` chain or whether build-rev is a contribution-internal field.
- **Q4:** ✅ Annotations on pyramid nodes already use the annotation/FAQ contribution pattern (per project memory `everything_is_contribution`); under Path B they collapse cleanly onto contribution-annotations on the node-as-contribution row. W7 handles the migration for the ~7 dense slugs flagged by W0.
- **Q5 (new):** Type-string for the contribution. `pyramid_node` (single type, depth as metadata) vs `pyramid_node:depth_N` (typed per layer) vs `pyramid_apex` / `pyramid_intermediate` / `pyramid_leaf`. W2 design call. Recommended default: single `pyramid_node` type, depth as metadata field — keeps the contribution-type vocabulary closed.

## 8. Relationship to Sibling Plan

`evidence-loop-resilience-and-capture.md` ships a runtime guardrail (I10) for the same class of bug this plan eliminates structurally. Sequencing:

- Evidence-loop plan ships first (faster, smaller, well-scoped). I10 acts as a runtime check.
- This plan ships next. Once the schema canonicalizes, I10's runtime check is replaced by a parse-time invariant. The two plans don't conflict; they're symptomatic + systemic responses to the same underlying issue.
