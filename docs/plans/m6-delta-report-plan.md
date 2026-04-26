# Mission #6-A — Delta Report: v2 Spec vs. agent-wire-node

**Plan type:** mission-decomposition | **Author:** deepseek-peterman | **Date:** 2026-04-26
**Target branch:** `puddy/m6-delta-report-plan`
**Card UUID:** `a355bf6e-939f-4f5d-aeb5-5b436ebeea29` (playful/115/39)
**Anchor corpus:** pyramid-app-v2/ (36 docs, draft 0.5, READ-ONLY)
**Comparison code:** agent-wire-node/ (READ-ONLY)
**Annotation pyramid:** alldocs-test2
**Pre-read anchors:** 00-plan.md, 10-noun-kernel.md, 11-verb-kernel.md, 80-kernel-closure-v1.md, contributory-vocabulary-and-dictionaries.md

---

## Goal

Produce a Delta Report — a structured Wire contribution comparing the v2 spec (pyramid-app-v2, draft 0.5) against the live agent-wire-node codebase. Six delta axes: parity, intentional-drop, intentional-change, net-new, smuggling-risk, spec-gap. Every finding is a typed annotation on the alldocs-test2 pyramid, citing `vocab/playful/vocabulary_entry/v1` and conforming to `dict/playful/master/v1`. Five new vocabulary entries (delta-finding, smuggling-risk, pillar-violation, parity-axis, positive-observation) are published as independent Wire contributions citing meta-bootstrap, NOT bundled at end. Minimum 10 substantive annotations. This is **plan-only** — jackie audits this plan first.

---

## 1. Card-chain shape

### Default (recommended) — 10 cards: 5 bania + 5 jackie

```
Card 6-A.1 (bania): Read ~8 v2 kernel docs, produce 6-12 annotations on alldocs-test2
       ↓
Card 6-A.2 (jackie): Audit 6-A.1 — verify vocab/dict citations, flag missing axes, per-annotation verdict
       ↓
Card 6-A.3 (bania): Read ~8 v2 data/genesis docs, produce 6-12 annotations
       ↓
Card 6-A.4 (jackie): Audit 6-A.3 — cross-check duplicates with 6-A.1
       ↓
Card 6-A.5 (bania): Cross-codebase diff — read agent-wire-node at key comparison points, 8-16 annotations (smuggling-risk + spec-gap focus)
       ↓
Card 6-A.6 (jackie): Audit cross-codebase — verify legacy citations, flag over-claims, produce axis-count summary
       ↓
Card 6-A.7 (bania): Publish 5 new vocabulary entries as independent Wire contributions
       ↓
Card 6-A.8 (jackie): Audit vocab entries — meta-bootstrap citation, dict conformance, resolvability
       ↓
Card 6-A.9 (bania): Final synthesis — Delta Report contribution citing all annotations, ≥10 substantive
       ↓
Card 6-A.10 (jackie): Final verdict — APPROVE / APPROVE_WITH_FIXES / REJECT
```

### Argued rationale

**Why batch-of-8 per bania doc-read card:** 36 total docs. Kernel docs (00-16) are denser than starter docs (32-43). Splitting at the kernel boundary (~8 dense docs per batch) balances card weight. Cross-codebase card (6-A.5) handles all 36 docs from legacy-side only, without per-doc annotation overhead.

**Rejected alternatives:**
- **Single bania card for all 36 docs:** Bania quality degrades on sessions exceeding ~12 distinct doc reads. Batch-of-8 matches the attention window.
- **Annotate THEN cross-codebase in same pass:** V2 spec must be understood on its own terms first. Only after v2-side annotation is complete does the cross-codebase diff have a stable baseline. Reversing the order biases annotations toward "what's different from legacy" rather than "what does v2 intend."
- **Single terminal jackie audit:** Batched audits provide progressive quality gates. A single terminal audit accumulates too many findings and forces rework across all prior cards.

---

## 2. Delta axes (six, each with one-line decision criterion)

### 2.1 Parity
**Criterion:** V2 spec mechanism matches agent-wire-node implementation in shape, intent, and operational semantics. Naming/schema differences OK if behavior is preserved.

### 2.2 Intentional-drop
**Criterion:** Capability present in agent-wire-node is explicitly excluded by v2 spec text (e.g., "no backward compatibility," "deferred to Vibesmithy").

### 2.3 Intentional-change
**Criterion:** Intent matches legacy but structure, data model, or operational path is deliberately different (e.g., UUID→counter identity, SQLite→`.understanding/` canonical, hardcoded-enum→vocabulary entry).

### 2.4 Net-new
**Criterion:** Mechanism in v2 spec with no counterpart in agent-wire-node, and not a rename/refactor of existing capability. Genuinely new substrate.

### 2.5 Smuggling-risk (HIGHEST VALUE)
**Criterion:** V2 spec is silent or under-specified at a point where an implementer would reflexively reach for legacy agent-wire-node shape. These are invisible regression vectors — the spec looks complete but silently depends on implementer legacy knowledge. Highest-value findings.

### 2.6 Spec-gap
**Criterion:** Capability present in agent-wire-node that v2 spec does not address at all — neither dropped nor reimagined. Blind spots distinct from smuggling-risk in that the spec doesn't even create a surface where guessing would apply.


---

## 3. Vocab citation

### 3.1 Annotation body shape

Every annotation on alldocs-test2 carries:
```yaml
contribution_type: annotation
annotation_verb: delta-finding | positive-observation
target: <handle-path of v2 spec doc or section>
body:
  axis: parity | intentional-drop | intentional-change | net-new | smuggling-risk | spec-gap
  finding: <substantive one-paragraph description>
  evidence:
    v2_citation: <doc path + section/line range>
    legacy_citation: <file path + symbol, if cross-codebase>
  vocab_ref: vocab/playful/vocabulary_entry/v1
  dict_ref: dict/playful/master/v1
```

### 3.2 New vocabulary entries (independent contributions, NOT bundled)

| Handle-path | entry_type | Purpose |
|-------------|------------|---------|
| `vocab/playful/delta-finding/v1` | annotation_verb | Annotation verb for delta findings |
| `vocab/playful/positive-observation/v1` | annotation_verb | Surfaces load-bearing structure |
| `vocab/playful/smuggling-risk/v1` | observation_axis | Classifies smuggling-risk findings |
| `vocab/playful/pillar-violation/v1` | observation_axis | Tags Wire-pillar violations |
| `vocab/playful/parity-axis/v1` | vocabulary_entry_type | Declares six delta axes as category |

Each entry: standalone `POST /api/v1/contribute`, `derived_from: [{source: meta-bootstrap, weight: 1.0, justification: "New vocabulary entry for Mission #6-A Delta Report"}]`, body conforms to `dict/playful/master/v1` vocabulary_entry schema. Published in Card 6-A.7, audited in Card 6-A.8, cited in final synthesis Card 6-A.9.

### 3.3 Dict conformance

All annotations conform to `dict/playful/master/v1`:
- `annotation_verb` resolves against dict's registered verbs; new verbs extend via supersession
- Target handle-paths resolvable on Wire
- Vocabulary entry bodies cite `@genesis/meta-schema/vocabulary_entry/1` as schema reference

---

## 4. Per-card acceptance gates

### Card 6-A.1 — bania: First v2 doc batch

**Docs:** 00-plan.md, 10-noun-kernel.md, 11-verb-kernel.md, 12-vocabulary-as-contribution.md, 13-role-binding.md, 14-observation-event-routing.md, 15-purpose-as-contribution.md, 16-bootstrap-and-genesis-governance.md

- [ ] All 8 docs read in full
- [ ] 6-12 annotations published on alldocs-test2 pyramid
- [ ] Every annotation carries `vocab_ref: vocab/playful/vocabulary_entry/v1` and `dict_ref: dict/playful/master/v1`
- [ ] At least one annotation per axis (spec-gap excepted — requires legacy-side)
- [ ] Every `v2_citation` resolves to real file path + section in pyramid-app-v2/
- [ ] No purely editorial annotations — each names a substantive delta finding

### Card 6-A.2 — jackie: Audit first batch

- [ ] Every annotation's target handle-path resolves on Wire
- [ ] `vocab_ref` and `dict_ref` are valid and fetchable
- [ ] No annotation claims `axis: parity` without checking legacy code (flag as "unverified")
- [ ] Multi-axis annotations flagged unless justified by distinct findings
- [ ] Verdict per annotation: CONFIRMED / NEEDS_CLARIFICATION / REJECTED (with reason)
- [ ] Produce audit report contribution citing each audited annotation handle-path

### Card 6-A.3 — bania: Second v2 doc batch

**Docs:** 17-identity-rename-move-portability.md, 20-understanding-folder-layout.md, 21-pyramid-protocol.md, 22-epistemic-state-node-shapes.md, 30-genesis-vocabulary-catalog.md, 31-genesis-role-binding.md, 32-cascade-handler-starter.md, 33-judge-starter.md

- [ ] All 8 docs read in full
- [ ] 6-12 annotations published on alldocs-test2
- [ ] ≥2 annotations with `axis: intentional-change` (storage + protocol + epistemic shapes — high change density)
- [ ] Flag any doc that appears incomplete or placeholder (per draft 0.5 status)
- [ ] Same vocab_ref/dict_ref/v2_citation constraints as Card 6-A.1

### Card 6-A.4 — jackie: Audit second batch

- [ ] Same audit constraints as Card 6-A.2
- [ ] Cross-check duplicates with 6-A.1 — flag for synthesis dedup in Card 6-A.9
- [ ] Verify ≥2 intentional-change annotations meet criterion (deliberate, not accidental drift)

### Card 6-A.5 — bania: Cross-codebase diff pass

- [ ] Read agent-wire-node at comparison points in Section 5
- [ ] 8-16 annotations covering smuggling-risk + spec-gap axes
- [ ] Every smuggling-risk cites concrete legacy code (file + function + line range) + explains why v2 silence creates risk
- [ ] Every spec-gap cites concrete legacy capability with no v2 counterpart
- [ ] ≥3 annotations reference SQL tables, Rust structs, or dispatch paths

### Card 6-A.6 — jackie: Audit cross-codebase

- [ ] Verify every legacy code citation resolves to real file + symbol
- [ ] For each smuggling-risk claim: is v2 truly silent? Flag over-claimed risks
- [ ] For each spec-gap claim: does v2 intentionally drop? Reclassify if yes
- [ ] Axis-count summary: parity=X, drop=Y, change=Z, new=W, smuggling=V, gap=U

### Card 6-A.7 — bania: Publish 5 new vocabulary entries

- [ ] Publish `vocab/playful/delta-finding/v1` as independent Wire contribution (annotation_verb)
- [ ] Publish `vocab/playful/positive-observation/v1` as independent Wire contribution (annotation_verb)
- [ ] Publish `vocab/playful/smuggling-risk/v1` as independent Wire contribution (observation_axis)
- [ ] Publish `vocab/playful/pillar-violation/v1` as independent Wire contribution (observation_axis)
- [ ] Publish `vocab/playful/parity-axis/v1` as independent Wire contribution (vocabulary_entry_type)
- [ ] Each entry cites `meta-bootstrap` in `derived_from` with justification
- [ ] Each entry body conforms to `dict/playful/master/v1` vocabulary_entry schema
- [ ] Five separate `POST /api/v1/contribute` calls — no bundling
- [ ] All five resolvable on Wire post-publication

### Card 6-A.8 — jackie: Audit vocabulary entries

- [ ] Each entry handle-path resolves on Wire
- [ ] Each `derived_from` includes `meta-bootstrap` source
- [ ] No body violates `dict/playful/master/v1` schema
- [ ] `parity-axis/v1` correctly enumerates all six delta axes as vocabulary_entry_type
- [ ] `delta-finding/v1` and `positive-observation/v1` schemas valid for annotation bodies
- [ ] All entries supersedable (playful namespace, not genesis pinned-by-hash)

### Card 6-A.9 — bania: Final synthesis — Delta Report contribution

- [ ] One Delta Report Wire contribution (contribution_type per dict)
- [ ] Cites all prior annotation contributions by handle-path
- [ ] Asserts count of substantive annotations — must be ≥10 (CONFIRMED by jackie)
- [ ] Findings grouped by delta axis with per-axis summary paragraph
- [ ] Friction observations (both directions) per Section 6
- [ ] Top-3 smuggling-risk findings flagged with rationale for "highest value"
- [ ] Report body is markdown; frontmatter carries structured counts + annotation refs

### Card 6-A.10 — jackie: Final verdict

- [ ] ≥10 annotations survive audit (CONFIRMED, not REJECTED or NEEDS_CLARIFICATION)
- [ ] Delta Report contribution fetchable on Wire
- [ ] All 5 vocab entries resolve and pass audit
- [ ] Friction observations grounded in concrete annotation findings
- [ ] Verdict: APPROVE / APPROVE_WITH_FIXES / REJECT
- [ ] If APPROVE_WITH_FIXES: each fix listed as concrete action with target card

---

## 5. Comparison surface

### v2 spec docs — bania batch assignments

**Batch 1 (Card 6-A.1):**
00-plan.md, 10-noun-kernel.md, 11-verb-kernel.md, 12-vocabulary-as-contribution.md, 13-role-binding.md, 14-observation-event-routing.md, 15-purpose-as-contribution.md, 16-bootstrap-and-genesis-governance.md

**Batch 2 (Card 6-A.3):**
17-identity-rename-move-portability.md, 20-understanding-folder-layout.md, 21-pyramid-protocol.md, 22-epistemic-state-node-shapes.md, 30-genesis-vocabulary-catalog.md, 31-genesis-role-binding.md, 32-cascade-handler-starter.md, 33-judge-starter.md

**Remaining (cross-codebase pass only, Card 6-A.5, no per-doc annotation):**
34-43 starter docs, 50-52 observability, 60-63 Node Lite, 70-72 UI, 80-kernel-closure, 90-91 meta, PUNCHLIST.md

### agent-wire-node comparison points (Card 6-A.5)

| Focus area | agent-wire-node path(s) |
|------------|------------------------|
| Contribution / store | `src-tauri/src/pyramid/db.rs`, `src-tauri/src/pyramid/contribution*.rs` |
| Handle-path parsing | `src-tauri/src/db/` (parse_handle_path) |
| Vocabulary | `src-tauri/src/vocabulary.rs`, `src-tauri/src/pyramid/vocabulary.rs` |
| DADBEAR / cascade | `src-tauri/src/pyramid/dadbear/`, `src-tauri/src/pyramid/cascade*.rs` |
| Judge | `src-tauri/src/pyramid/judge*.rs` |
| Evidence / testing | `src-tauri/src/pyramid/evidence*.rs` |
| Chain executor | `src-tauri/src/pyramid/chain*.rs`, `src-tauri/src/pyramid/build_runner.rs` |
| Epistemic states | `src-tauri/src/pyramid/debate*.rs`, `src-tauri/src/pyramid/gap*.rs`, `src-tauri/src/pyramid/meta_layer*.rs` |
| Cross-slug / vines | `src-tauri/src/pyramid/cross_slug*.rs`, `src-tauri/src/pyramid/vine*.rs` |
| Dispatch (MCP/HTTP/CLI) | `mcp-server/src/`, `src-tauri/src/main.rs` |
| SQLite (`.understanding/` equiv) | `src-tauri/src/pyramid/db.rs` schema section |
| Supersession | `src-tauri/src/pyramid/supersed*.rs` |
| Prompts / chains | `chains/prompts/`, `chains/variants/` |

---

## 6. Friction observations

### Positive (surfaced via `positive-observation` verb)

1. **Vocabulary-as-contribution is the load-bearing insight.** Specs 10§5 + 12 make every enum-like concept a contribution. Agent-wire-node's genesis vocabulary catalog (`.lab/architecture/genesis-vocabulary-catalog.md`) validates the pattern is real and the mapping is direct. Positive observation on `12-vocabulary-as-contribution.md`.

2. **Cross-slug protocol survives intact.** Spec 21 (pyramid protocol) ports agent-wire-node's cross-slug mechanism — the "crown jewel" called out in 00-plan.md — as-is. This is the strongest parity anchor. Positive observation on `21-pyramid-protocol.md`.

3. **60% posture is honest about starter quality.** Spec 80 D4 enumerates which stock purposes are proof-vertical vs. starter-only. This self-awareness prevents implementers from treating all starters as equally validated. Positive observation on `80-kernel-closure-v1.md`.

4. **No UUIDs is genuine simplification.** Pinned decision D7 replaces UUIDs with handle-paths-as-primary-key. Agent-wire-node's internal UUID usage carries friction the codebase works around; v2's move is cleaner. Positive observation on `17-identity-rename-move-portability.md`.

5. **Meta-schema recursion closure correctly self-describing.** Spec 12's vocab entry type recursion bottoms at `vocabulary_entry_type` whose own entry_type is itself. Spec 30§1 enumerates 15 entry_type categories cleanly. Avoids infinite-regress bugs. Positive observation on `12-vocabulary-as-contribution.md` + `30-genesis-vocabulary-catalog.md`.

### Negative (surfaced via `delta-finding` verb)

1. **Spec 80 is a template, not a finished audit.** The closure spec defers concrete enumeration to "a focused audit session between plan completion and implementation start." This creates a timing gap — the Delta Report is partially that audit, but the spec's own gate criteria require the audit to pass before Phase 1 begins. Chicken-and-egg risk. Friction on `80-kernel-closure-v1.md`.

2. **`contributory-vocabulary-and-dictionaries.md` not found at stated path.** The mission's pre-read anchor references `agent-wire-project-docs/working-drafts/cross-project/2026-04-25-contributory-vocabulary-and-dictionaries.md` which does not exist in the agent-wire-node repo. Bania must resolve: unpublished draft, mis-path, or doc that should exist but doesn't yet. Friction on mission pre-read list.

3. **Draft 0.5 status creates annotation instability.** Several v2 docs are explicitly drafts with deferred content. Annotations against e.g., 80-kernel-closure-v1.md must distinguish "finding about what's written" from "finding about what's conspicuously absent per the template." The spec's own disclaimer creates a moving target.

---

## 7. Open questions

- [ ] **Q1:** Does `contributory-vocabulary-and-dictionaries.md` exist under a different path, or should bania create it as scaffolding before Card 6-A.1?
- [ ] **Q2:** Should the Delta Report contribution supersede the mission pre-read anchor (if located), or is it a net-new contribution with its own handle-path?
- [ ] **Q3:** Are starter-chain docs (32-43) in-scope for per-doc annotation, or is cross-codebase coverage sufficient given "60% directional" per spec 00?
- [ ] **Q4:** Should jackie verify each annotation's target handle-path corresponds to a v2 doc on Wire, or are file-path-only citations acceptable?
- [ ] **Q5:** Does the Delta Report contribute to alldocs-test2 pyramid directly, or is it a standalone Wire contribution that merely cites alldocs-test2 annotations?

---

## 8. Non-goals

- **No annotation of agent-wire-node docs.** Comparison target is agent-wire-node code, not its documentation.
- **No implementation work.** Plan-only. No Rust/TS files touched.
- **No bania dispatch.** Cards are described but not triggered by this plan. Jackie audits first.
- **No parity harness execution.** Spec 52 parity testing is a separate mission.
- **No Phase 0 go/no-go decision.** The Delta Report informs Phase 0; it does not make the decision.

---

## 9. Risks

| Risk | Mitigation |
|------|-----------|
| `dict/playful/master/v1` may not exist on Wire in annotation-ready form | Card 6-A.7 extends dict via supersession; if missing entirely, bania pauses and escalates to puddy |
| alldocs-test2 pyramid may not be writable by deepseek-peterman | Card 6-A.1 pre-flight: bania verifies write access before annotating |
| Spec draft 0.5 docs may change between plan and execution (spec drift) | Pin v2 spec to git commit hash at Card 6-A.1 start; annotations cite pinned version |
| 10 cards may exceed fleet capacity | Cards are serially dependent (bania→jackie→...) so only one runs at a time; two slots sufficient |
| Spec-gap vs. intentional-drop classification requires judgment | Card 6-A.6 jackie has final reclassification authority |
| `contributory-vocabulary` doc missing creates boot ambiguity | Q1 resolution before Card 6-A.1 |

---

## 10. Deliverable

One Delta Report Wire contribution (handle-path TBD at Card 6-A.9) citing:
- ≥10 substantive, jackie-CONFIRMED annotations on alldocs-test2 pyramid
- 5 independent vocabulary entry contributions
- Per-axis summary counts with top-3 smuggling-risk findings
- Friction observations (positive + negative)
- Full annotation index (handle-paths to each annotation contribution)
