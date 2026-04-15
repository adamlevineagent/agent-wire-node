# Gap Report: Understanding Web Build Validation

**Date:** 2026-04-04
**Binary:** Post-understanding-web-build (all 4 phases shipped)
**Test:** Fresh question pyramid on `core-selected-docs` (127 docs), apex question "What is this body of knowledge and how is it organized?"

## Result Summary

| Metric | Value | Assessment |
|--------|-------|------------|
| Build status | complete | PASS |
| Depth distribution | L0:127, L1:6, L2:6, L3:1 | PASS — 4-layer pyramid |
| Evidence KEEP | 437 | PASS |
| Evidence DISCONNECT | 163 | PASS — reasonable ratio |
| L0 nodes touched | 1971 (includes cross-build) | PASS |
| Gaps processed | 103 | PASS — pipeline ran |
| Targeted re-examinations | 0 | **ISSUE** — see Gap 1 |
| Apex endpoint | FAIL | **ISSUE** — see Gap 2 |
| Drill endpoint | PASS | Evidence links followed correctly |
| self_prompt on nodes | Populated | PASS |
| Build time | 167s | PASS |
| Failures | 0 | PASS |

## Gap 1: Targeted re-examination resolves zero files

**Severity:** High — Phase 2 (Grow It) is wired but not producing results

**What happens:** 103 MISSING verdicts were collected and fed into gap processing. The gap file resolution step ran for each one. Every single gap resolved to `candidates_scored=0, files_resolved=0`. No targeted L0 re-examinations were produced.

**Example from logs:**
```
gap=Agent Wire Compiler Architecture document – describes the compiler pipeline...
keywords=20 candidates_scored=0 files_resolved=0
→ no source files resolved for gap, marking resolved with no new evidence
```

**The irony:** The gap describes a document that literally exists in the source corpus (`Core Selected Docs/` contains the Agent Wire Compiler Architecture doc). The gap resolution can't find it.

**Likely cause:** The rule-based candidate resolution (keyword matching against source file paths/names?) isn't matching gap descriptions to actual source files. The gaps describe documents by their CONTENT ("describes the compiler pipeline") but the resolver is probably matching against FILE NAMES. The file might be named `agent-wire-compiler-architecture.md` but the gap says "Agent Wire Compiler Architecture document" — close but not a string match.

**Fix needed:** The gap → source file resolver needs access to the canonical L0 extractions (which contain headlines matching the gap descriptions), not just file paths. Match gap keywords against L0 node headlines/topics, find the source file for the matching L0 node, then re-examine that file.

## Gap 2: Apex endpoint still broken

**Severity:** High — blocks all API consumers from finding the entry point

**What happens:** `GET /pyramid/core-selected-docs/apex` returns:
```json
{"error": "No valid apex for slug 'core-selected-docs': multiple nodes at every depth (max depth 0, 127 nodes)"}
```

But the apex exists: `L3-491a10ef-4b59-4d1d-929d-08b55ac54018` at depth 3, `superseded_by IS NULL`, single node at that depth. Drill endpoint finds it fine when given the ID directly.

**What works as workaround:** Direct drill to the L3 node by ID returns correct results with 5 children found via evidence KEEP links.

**Likely cause:** The apex finder query is probably doing something like `SELECT * FROM pyramid_nodes WHERE slug=? AND superseded_by IS NULL ORDER BY depth DESC LIMIT 1` but with additional filters that exclude question pyramid nodes. Or it's computing max_depth from a subset of nodes (e.g., only nodes matching `L\d+-\d+` sequential ID pattern). The error message says "max depth 0, 127 nodes" which means it's only seeing the L0 layer — the L1/L2/L3 question pyramid nodes are invisible to it.

**Fix needed:** The apex query must find the single node at the highest depth among all non-superseded nodes, regardless of ID format. No filtering by ID pattern.

## Gap 3: Decomposition tree is shallow (11 leaves, 2 branches)

**Severity:** Medium — functional but less rich than previous builds

**What happened:** The previous build (experiment 4) produced 36 L1 nodes from 37 leaf questions. This fresh build produced only 6 L1 nodes from 11 leaves + 2 branches. The decomposition is shallower — fewer questions, less granular coverage.

**Likely cause:** Mercury 2 non-determinism. The same prompts, same corpus, different decomposition. This isn't a bug — it's the expected variability of LLM decomposition. The prompts say "produce the MINIMUM number of sub-questions needed" and Mercury sometimes interprets this more aggressively.

**Not a fix:** This is acceptable variability. The architecture handles it correctly regardless of decomposition breadth. Mentioning it for completeness.

## Gap 4: Progress reports 0/0 during setup phase (known)

**Severity:** Low — already logged in friction log

**What happens:** Build reports `done=0, total=0` for the first ~120 seconds while characterization, enhancement, decomposition, extraction schema, and synthesis prompt generation run. Progress only starts counting once the evidence loop begins.

**Already documented in:** `.lab/friction-log.md` item #2

## Gap 5: L0 touched count seems inflated (1971)

**Severity:** Low — cosmetic/diagnostic

**What happens:** The query `SELECT count(DISTINCT source_node_id) FROM pyramid_evidence WHERE source_node_id LIKE 'D-L0-%' OR source_node_id LIKE 'C-L0-%'` returns 1971, which exceeds 127 (the number of actual L0 nodes). This is because evidence links from MULTIPLE prior builds (different build_ids) all contribute distinct entries. The count includes cross-build evidence accumulation, which is correct behavior — but the diagnostic query should scope to the current build_id to show how many L0 nodes THIS build touched.

**Not a bug:** The evidence table correctly stores all historical evidence. The diagnostic query just needs build_id scoping for per-build analysis.

## What's Working Well

1. **Drill via evidence links** — Phase 1.2 works. Drilling from L3 apex returns 5 L2 children found through KEEP verdicts. Drilling L2 nodes returns L1 children. The full navigation path works.

2. **self_prompt populated** — Phase 1.3 works. Every L1+ node has its question in self_prompt. "What is the architectural design of the Wire knowledge base..." etc.

3. **Gap processing pipeline** — Phase 2.1 is wired end-to-end. 103 MISSING verdicts collected, fed through gap resolution, each gap processed with keyword extraction and candidate scoring. The pipeline runs; it just can't resolve candidates (Gap 1 above).

4. **Evidence verdicts** — 437 KEEP + 163 DISCONNECT across the build. Reasonable ratio. The answer prompt is correctly producing justified verdicts.

5. **Build completion** — Zero failures, 167 seconds, clean 4-layer pyramid. The core question pyramid pipeline is solid.

## Priority Fix Order

1. **Gap 2 (apex finder)** — blocks all API consumers. Single highest-priority fix.
2. **Gap 1 (gap file resolution)** — blocks the accretion engine. Phase 2 is wired but can't produce results until the resolver can match gaps to source files via L0 node metadata.
3. Gap 4 (progress) and Gap 5 (diagnostic) are low priority / known issues.
