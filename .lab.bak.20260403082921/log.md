# Experiment Log — Vibesmithy Code Pyramids

## Restart
Branch: research/vibesmithy-code-pyramids / Type: thought / Parent: -
Hypothesis: A fresh lab grounded only in the rebuilt YAML system will produce cleaner research decisions than trying to inherit stale conclusions.
Changes: Archived prior `.lab`, created a new research branch, initialized a fresh `.lab/` for YAML-only code pyramid work on `vibesmithy`.
Result: Fresh research series initialized.
Duration: n/a
Status: thought
Insight: Prior notes remain useful as idea fodder, but not as evidence. The current experimental surface is `chains/defaults/code.yaml` plus `chains/prompts/code/*.md`.

## Experiment 0 — Existing Baseline Read
Branch: research/vibesmithy-code-pyramids / Type: thought / Parent: -
Hypothesis: The current `vibesmithy5` build will expose the dominant failure mode clearly enough to guide the first prompt change.
Changes: Measured the existing `vibesmithy5` code pyramid via SQLite and live CLI reads without changing any repo files.
Result: The build completes and produces one apex, but the pyramid shape is 34 L0 -> 34 L1 -> 1 L2. Every sampled L1 node has exactly one child, so the system is converging numerically without discovering architectural threads. The apex reads like a stitched catalog of file descriptions rather than a true subsystem map.
Duration: ~10m
Status: interesting
Insight: The primary defect is representational, not convergence. The first experiments should target how frontend files are extracted and clustered so L1 becomes multi-file subsystem nodes instead of renamed file summaries.

## THINK — before Experiment 1
Convergence signals: Baseline already shows the core issue. The apex exists, but thread coherence and drill usefulness are poor because L1 is an identity transform over L0. No evidence yet that recursive clustering is the immediate bottleneck.
Untested assumptions: I am assuming the extraction prompt is the highest-leverage problem, especially for `.tsx` files. I have not yet tested whether clustering alone could recover better threads from the current L0 outputs. I also have not tested the opposite framing: less user-experience narration and more subsystem-role extraction.
Invalidation risk: Recent system rebuilds invalidate prior findings, so only the current `vibesmithy5` artifact counts. Any changes made by others to the live build path could affect comparisons, so each experiment should use a fresh slug and record shape plus content deltas.
Next hypothesis: Rewrite the frontend extraction prompt so it describes each file primarily as a subsystem participant: its architectural role, collaboration points, owned state, and user-visible surface only as supporting context. If L0 stops over-indexing on isolated user-experience summaries, thread clustering should be more likely to form real multi-file architectural groups.

## Friction — IPC Build Trigger
Branch: research/vibesmithy-code-pyramids / Type: thought / Parent: #1
Hypothesis: The fastest path to productive prompt iteration is a scriptable local build trigger outside the desktop UI.
Changes: Investigated the failed HTTP mutation path after `POST /pyramid/slugs` and `POST /pyramid/<slug>/build` returned `moved to IPC`.
Result: Confirmed that create/build are Tauri IPC commands only, while HTTP remains read-only. No existing shell-side harness has been found yet.
Duration: ~10m
Status: thought
Insight: The current bottleneck is not prompt design alone; it is the missing automation path for rebuilds. This belongs in the friction log because it materially slows future research sessions.

## Crash Note — Experiment 1 first run
Branch: research/vibesmithy-code-pyramids / Type: thought / Parent: #0
Hypothesis: The new CLI mutation path would support an immediate create -> ingest -> build experiment.
Changes: Triggered `create-slug`, `ingest`, and `build` for `vibesmithy-exp1`.
Result: False-start crash. Build fired before ingest completed, so the log reports `No chunks found for slug 'vibesmithy-exp1'`. This is an orchestration mistake, not evidence about prompt quality.
Duration: ~5m
Status: thought
Insight: The new mutation path works, but it must be driven serially. Re-run Experiment #1 in strict sequence before drawing any conclusions.

## Experiment 1 — Reframe Frontend Extraction Around Subsystem Role
Branch: research/vibesmithy-code-pyramids / Type: real / Parent: #0
Hypothesis: If frontend files are extracted by subsystem role instead of isolated user-experience narration, the code pyramid will form more meaningful architectural groupings.
Changes: Rewrote `chains/prompts/code/code_extract_frontend.md` so headlines, orientation, and topics prioritize subsystem role, owned responsibility, state/control flow, and integration points over per-file visual description.
Result: `vibesmithy-exp1` completed in `complete_with_errors` state with 1 node failure. Shape changed from baseline `34 L0 -> 34 L1 -> 1 L2` to `34 L0 -> 1 L1`. The resulting top node is materially more architectural: it identifies application structure, pyramid exploration, chat/session management, and data/configuration as subsystems instead of restating 34 individual files. However, it over-collapsed and removed drillable intermediate structure.
Duration: ~102s
Status: keep
Insight: The direction is correct. Subsystem-role framing helps the model synthesize architecture instead of file catalogs. The next problem is no longer basic extraction framing; it is preserving multiple top-level architectural domains rather than collapsing the whole app into one node.

## THINK — before Experiment 2
Convergence signals: Experiment 1 improved architecture-level understanding but overshot into a single-node collapse. This suggests extraction framing was indeed a major lever. The next bottleneck is likely clustering or upper-layer convergence instructions.
Untested assumptions: I have not yet tested whether clustering is receiving enough distinct signals to keep multiple threads apart. I also have not tested whether the recluster/apex-readiness rules are too eager to treat a small codebase as already top-level complete.
Invalidation risk: The 1-node failure in Experiment 1 may hide a secondary issue, but the structural shift is too large to dismiss as noise. The next change should isolate layer preservation rather than continuing to broaden extraction changes.
Next hypothesis: Tighten `code_cluster.md` and/or `code_recluster.md` so small codebases preserve multiple distinct architectural domains when those domains are still genuinely useful to explore separately. The goal is to keep the subsystem framing win from Experiment 1 while preventing total collapse into one architecture node.

## Experiment 2 — Prevent One-Thread Code Clustering
Branch: research/vibesmithy-code-pyramids / Type: real / Parent: #1
Hypothesis: Stronger clustering guidance will preserve multiple explorable architectural domains instead of collapsing the entire app into one thread.
Changes: Tightened `chains/prompts/code/code_cluster.md` to explicitly resist single-thread output for application-sized codebases and to preserve distinct product areas such as navigation, exploration, chat, settings, shared data/client utilities, and rendering.
Result: `vibesmithy-exp2` completed in `complete_with_errors` state with 2 node failures. It did restore a multi-node upper structure (`13 L0 -> 13 L1 -> 1 L2`), but coverage regressed severely because only 13 of 34 ingested files became L0 nodes. The final apex is more drillable than Experiment 1, but it is missing too much of the codebase to count as an improvement.
Duration: ~68s
Status: discard
Insight: The clustering direction is promising in principle, but this specific prompt push is not safe. It appears to trade coverage for structure, which is the wrong bargain at this stage. Revert to Experiment 1 and look for a narrower way to preserve multiple domains without dropping files.

## THINK — after Experiment 2 discard
Convergence signals: We now have one clear keep and one clear discard. Extraction framing is a real lever; aggressive clustering changes can destabilize coverage.
Untested assumptions: I still have not isolated whether the coverage loss came from the clustering prompt itself, from interaction with the merge step, or from node failures elsewhere in the chain.
Invalidation risk: Because Experiment 2 dropped so much content, its structural shape is not trustworthy as a target. The safe baseline for future work is still Experiment 1.
Next hypothesis: Preserve Experiment 1's extraction framing and move to a narrower intervention, likely in `code_recluster.md` or the merge/convergence layer, where we can encourage multiple top-level domains without disturbing initial file coverage.

## Operator Correction — after Experiment 2
Operator reviewed cross-pipeline analysis (code vs document vs conversation) and identified root cause:
- `code.yaml` `merge_clusters` step uses `$prompts/document/doc_cluster_merge.md` — a document-pipeline prompt
- That prompt uses `D-L0-XXX` ID examples (code uses `C-L0-XXX`), talks about "documents" not files, and explicitly allows an `unassigned` output array
- Files placed in `unassigned` by the merge step silently drop from thread coverage — this is why exp2 went from 34 to 13 L0 nodes
- Exp1's collapse to 1 L1 is also downstream: the document merge prompt likely merged all threads into one "concept" because it doesn't understand architectural subsystem grouping
- The exp1 extraction direction is correct and should be kept. The merge bug is the primary fix needed.
Action: Create `chains/prompts/code/code_cluster_merge.md` with code-specific semantics (C-L0-XXX IDs, ZERO ORPHANS, subsystem framing, no unassigned). Update `code.yaml` to reference it.

## THINK — before Experiment 3
Branch: research/vibesmithy-code-pyramids / Type: THINK / Parent: #1
Convergence signals: Root cause is now identified by cross-system audit, not blind empiricism. The merge bug explains both symptoms: coverage collapse (unassigned files drop) and single-thread collapse (document semantics merge all code subsystems into one narrative concept).
Untested assumptions: I had not read `doc_cluster_merge.md` carefully or traced which file `code.yaml` referenced. The assumption that clustering was the bottleneck was wrong — clustering was likely producing reasonable threads that the wrong merge prompt then collapsed.
Invalidation risk: Exp1's single-L1 outcome may have had the document merge prompt collapsing multiple good threads from `batch_cluster` into one "vibesmithy frontend" concept thread. With the correct merge prompt this may resolve without any further changes to `code_extract_frontend.md`.
Next hypothesis: Replacing `doc_cluster_merge.md` with a code-specific `code_cluster_merge.md` (subsystem semantics, C-L0-XXX IDs, ZERO ORPHANS, no unassigned allowed) will restore full file coverage AND produce multiple meaningful L1 thread nodes, carrying forward exp1's improved extraction framing.

## Experiment 3 — Merge Bug Fix Verification
Branch: research/vibesmithy-code-pyramids / Type: real / Parent: #1
Hypothesis: With zero orphans enforced and the correct `C-L0-XXX` format in the new `code_cluster_merge.md`, file coverage drop will be solved.
Changes: Updated `code.yaml` to reference `code_cluster_merge.md`.
Result: `34 L0 -> 34 L1 -> 1 L2`. Coverage is fully restored to 34 files. However, the thread clustering is still 34 singletons. This confirms the merge prompt caused the coverage drop, but the 34 singletons are caused by the clustering prompt favoring splitting for small codebases.
Duration: ~110s
Status: keep
Insight: The sub-chain `batch_cluster` -> `merge_clusters` structure is correct, but for 34 files it runs as a single batch. Thus the LLM logic in `code_cluster.md` controls the final shape completely.

## Experiment 4 — Stripping Sizing/Splitting Constraints
Branch: research/vibesmithy-code-pyramids / Type: real / Parent: #3
Hypothesis: Stripping the legacy "Prefer splitting", "Balance", and "Max 12 files" rules from `code_cluster.md`—and allowing the token-aware hydration to naturally provide boundaries—will let the LLM organize files into a healthy 5-8 subsystems.
Changes: Removed explicit sizing numbers and splitting preference from `code_cluster.md`. Crucially, I accidentally deleted the `BAD`/`GOOD` JSON examples.
Result: The build aborted mid-flight. Status: `failed`.
Duration: ~190s
Status: discard
Insight: Without the trailing JSON examples, the `batch_cluster` output drifted from the strictly expected JSON string format, crashing the structured output parser (`No JSON found in: { ... }`). The JSON framing rules are load-bearing to get complete parsed output from the executor.

## Experiment 5 — Restoring Syntax, Relying on Natural Hydration
Branch: research/vibesmithy-code-pyramids / Type: real / Parent: #3
Hypothesis: Restoring the exact JSON rules/examples while keeping the legacy sizing rules removed will produce valid JSON and a natural macroscopic clustering.
Changes: Put the JSON generation guardrails (BAD/GOOD examples) back into `code_cluster.md`.
Result: `34 L0 -> 34 L1 -> 1 L2`. Completed successfully, but produced exactly 34 threads.
Duration: ~163s
Status: discard
Insight: When asked to group by "coherent architectural subsystem" without an explicit macro-granularity target, the LLM defaulted to microscopic precision, treating every single UI component or file as its own "subsystem" (e.g. a thread just for `Chat Panel UI`, another for `Chat Message Bubble`). Code resists being grouped without a specific target grain, unlike documents which clump semantically under broad concepts.

## THINK — after Experiment 5
Branch: research/vibesmithy-code-pyramids / Type: THINK / Parent: #5
Convergence signals: Bounding the LLM to output accurate clusters for code in a single prompt is fighting against the material. Forcing it to do it in "one shot" with token hydration produces either hyper-granularity or arbitrary clipping. We have two solid paths:
1. Provide a generic target layout ("aim for 4-8 macroscopic areas") in the `batch_cluster` prompt without hardcoding structure.
2. Advance `code.yaml` to match `document-v4-classified.yaml`: run a separate `concept_areas` identification pass over the codebase, and then map the files into it in parallel.
Untested assumptions: Both options still use the `for_each: $l0_doc_extract` mechanic under the hood, but separating definition from assignment solves LLM exhaustion. 
Invalidation risk: Proceeding to Option 2 without the User's explicit approval assumes abandoning the `batch_cluster` debug thread for a paradigm jump.
Next hypothesis: We proceed directly to implementing the V4 Document architecture, decoupling macro-architecture definition from per-file thread assignment.

