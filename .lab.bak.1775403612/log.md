# Experiment Log — Question Pyramid Prompt Tuning

## THINK — before Experiment 1

**Convergence signals:** Fresh start — baseline only. No prior trend.

**Baseline state:**
- Utilization: 61.7% (21/34 L0 nodes KEEP'd)
- Layer count: 2 (all leaves, no branches — flat pyramid)
- Verdict breakdown: 48 KEEP, 44 DISCONNECT, 0 MISSING
- Untouched: 13 nodes, ALL React UI components

**Root cause analysis:**
The 13 untouched nodes all appeared as DISCONNECT in pyramid_evidence — the pre-mapper found them, but the answer step correctly rejected them because NO QUESTION asks about UI components. The decompose produced 7 sub-questions, all structural: repo layout, config files, docs, public assets, stack, build workflow, app purpose. None asks "What are the UI components and what do they do?"

This is a decompose problem, not a pre-map problem. The decomposer marked everything is_leaf=true at depth 1. With granularity=3 max_depth=3 on a 34-file codebase, depth-1 nodes should be BRANCHES that further decompose into component-level leaf questions.

**Untested assumptions:**
- Is this caused by missing /no_think (Mercury not settling cleanly on branch/leaf)?
- Is the branch/leaf guidance in the prompt not explicit enough?
- I'm assuming branches at depth 1 would generate a "UI components" sub-question → haven't tested this.

**Hypothesis:** Adding /no_think to decompose.md AND strengthening the depth-1 branch guidance will cause the decomposer to produce BRANCH questions at depth 1 (major categorical areas including UI components), which will:
1. Increase layer count from 2 to 3-4
2. Generate questions that cover UI components
3. Increase utilization from 61.7% toward the target

**Combining with compliance fixes:** pre_map.md, answer.md, and extraction_schema.md are also missing /no_think — adding to all in this experiment. The decompose branch guidance is the core hypothesis; /no_think adds are compliance fixes bundled in.

**Next hypothesis if this fails:** The problem may be that pre_map needs to be more aggressive about mapping components to whatever "app purpose/overview" question exists. Could try broadening the app-purpose question mapping.

## Experiment 1 — Branch guidance + /no_think compliance
Branch: research/question-pyramid-tuning / Type: real / Parent: #0
Hypothesis: Depth-1 branch guidance + /no_think on all 4 prompts will produce branching tree and cover UI components
Changes: decompose.md depth-1 branch rule + /no_think; pre_map.md, answer.md, extraction_schema.md /no_think
Result: 100% utilization (34/34, was 21/34), 3 layers (was 2), L1=53, L2=8, L3=1, KEEP=146, DISCONNECT=95, MISSING=0
Duration: 555s (was 40s) — 14x slower
Status: keep*
Insight: Primary metric hit target. Branch guidance worked — decomposer produced 8 branches × ~6-7 leaves = 53 leaf questions, covering all 34 L0 nodes. Build time regression is severe: granularity=3 is passed to decompose but prompt has NO instruction about what granularity means or how to use it. 53 leaves for 34 files is ~1.5x over-decomposed. Apex content is much more specific (names component organization, build pipeline, naming conventions).

## THINK — before Experiment 2

**Convergence signals:** 1 keep* so far. Primary metric hit target (100%), secondary regression (build time 14x).

**Current state:**
- Utilization: 100% (all 34 nodes touched) ✅
- Layer count: 3 ✅
- Build time: 555s ❌ (target: ~2-3 min for tester UX)
- Question count: 53 leaves + 8 branches + 1 apex = 62 questions

**Root cause of build time:**
1. 53 leaf questions is too many for 34 files (ideally ~20-25)
2. `granularity: "$granularity"` is passed to decompose in the YAML input but decompose.md has NO instruction about what granularity means. The LLM ignores it.
3. `max_depth: "$max_depth"` is also passed but only implicitly handled — the Rust primitive stops recursive calls at max_depth, but the decomposer doesn't calibrate breadth to match the scale of the corpus.

**Untested assumption:** Does adding granularity guidance (explaining it as a scale) actually constrain the decomposer without Pillar 37 violation? Granularity is a user-provided parameter, not a hardcoded number. Telling the LLM what the scale means is fine — it's semantic guidance, not a number constraining output.

**Hypothesis:** Adding explanation of the granularity parameter to decompose.md — telling the LLM it's a scale from focused (1) to comprehensive (5) — will cause the decomposer to produce fewer, more focused sub-questions at granularity=3, reducing leaf count from ~53 to ~20-25 and cutting build time to 2-3 minutes while maintaining 100% utilization.

**Risk:** Fewer questions might drop utilization below 100% if some nodes relied on the extra leaf questions to get touched. Monitor carefully.

**Key constraint:** No Pillar 37. I'm explaining what the granularity scale MEANS semantically — not specifying counts or sentence lengths.

## Experiment 2 — Granularity guidance in decompose
Branch: research/question-pyramid-tuning / Type: real / Parent: #1
Hypothesis: Granularity scale explanation will reduce leaf count from ~53 to ~20-25, cutting build time while maintaining utilization
Changes: decompose.md — added granularity 1-5 scale guidance + "be aggressive about merging"
Result: 100% utilization maintained (34/34). Nodes: L1=11, L2=6, L3=1 (was L1=53, L2=8). Evidence_loop 143s (was 283s). BUT l0_extract still 124s. Total: 401s (was 555s).
Duration: 401s
Status: keep*
Insight: Granularity guidance worked — cut leaves from 53 to 11, halved evidence_loop time. l0_extract remains slow (~124s) because the generated extraction_prompt from extraction_schema is now more complex (based on deeper question tree), causing Mercury 2 to take ~40s per chunk instead of ~1.7s in baseline. Quality tradeoff: tune-2 L0 nodes average 1870 chars/topics vs tune-0's 1256 (49% richer). 

ROOT CAUSE of l0_extract slowness: extraction_schema generates a per-question extraction_prompt that lists directives for all questions separately. With 17 questions, this produces a long detailed prompt → slow Mercury calls. Fix: rewrite extraction_schema.md to generate a HOLISTIC extraction_prompt (consolidate directives across questions rather than listing per-question) — same coverage, much shorter prompt.

FRICTION LOG NOTE: source_file connection is broken for question pipeline (pyramid_file_hashes.node_ids empty). Rust gap — for_each executor doesn't update file_hashes when saving Q-L0-* nodes. Cannot fix with YAML/prompts.

## Checkpoint R — Rust Team Handback

Post-experiment-2, the Rust team audited and fixed all wiring bugs:

**Fixes applied:**
- `$decomposed_tree` canonical alias crash — both decompose paths now write to `ctx.step_outputs["decomposed_tree"]`
- `extraction_schema` received wrong inputs — input block corrected to include `question_tree: "$decomposed_tree"`
- `pyramid_file_hashes` WriteOp::UpdateFileHash variant added — for_each executor now updates file_hashes after saving Q-L0-* nodes (extracts `## FILE:` header)
- Global sliding-window rate limiter added: 4 req / 5s (`llm_rate_limit_max_requests`, `llm_rate_limit_window_secs` config keys)
- Jittered retry backoff: prevents thundering herd on 429 storms
- `l0_extract` concurrency reduced to 4 (rate limiter now controls throughput)
- `extraction_schema.md` rewritten: explicit rules that generated `extraction_prompt` MUST end with JSON output format spec + `/no_think` as absolute final characters

**Rust team test result:** 278s, 41 nodes (34 L0 → 6 L1 → 1 apex), 0 failures

**Layer regression noted:** Post-Rust test shows 2 layers (6 L1 + 1 apex) vs 3 layers in Exp 1-2. Both prompt files confirmed in sync with runtime. Likely cause: either (a) Rust team ran test before syncing tuned prompts, or (b) Mercury 2 variance. Tune-3 will determine which.

## THINK — before Experiment 3

**Convergence signals:** 2 keep* experiments, Rust fixes landed, layer count uncertain.

**Current state:**
- Utilization: unknown post-Rust (Rust test showed 2 layers which is suspicious)
- Build time: 278s (Rust test) — better than 401s but still above target
- l0_extract: was 124s in Exp 2; with rate limiter + concurrency=4 should be different
- extraction_schema.md: rewritten by Rust team with format rules

**Primary question for Exp 3:** Does the re-baselined system (Rust fixes + current prompts) still produce 3-layer pyramids and 100% utilization?

**Secondary question:** What does build time look like now that rate limiter controls concurrency?

**Hypothesis:** The Rust team's 2-layer result was variance or stale prompts. With tuned prompts synced, Exp 3 should reproduce 3 layers + 100% util at ~278s or better.

**Risk:** If still 2 layers, the `{{depth}}` template substitution in decompose.md may not be working (LLM sees "DEPTH {{depth}} RULES" as opaque heading), or Mercury is deciding vibesmithy is small enough for flat structure. Will need stronger language if so.

## Experiment 3 — Re-baseline with Rust fixes (tune-3)
Branch: research/question-pyramid-tuning / Type: real / Parent: R
Hypothesis: Rust fixes + synced tuned prompts will reproduce 3-layer pyramid and 100% util
Changes: None to prompts — pure re-baseline after Rust fixes
Result: 100% util (34/34 KEEP). BUT still 2 layers: depth 0=34, depth 1=6, depth 2=1. KEEP=77, DISCONNECT=5.
Duration: ~278s (per Rust team test; approx)
Status: keep* (utilization), regression (layers + question quality)
Insight: Layer regression confirmed real — not a Rust test artifact. Root cause identified: decompose.md reads source material summaries and produces FILE-LOCATION questions ("What's in the docs folder?") instead of CONCEPTUAL questions ("How does spatial exploration work?"). File-location questions are naturally narrow → marked as_leaf=true, correctly by the BRANCH criteria. Fix: reframe decompose.md to explicitly prohibit file-location questions and redirect toward purpose/capability/behavior framing.

## THINK — before Experiment 4

**Convergence signals:** 3 keep* (utilization), 1 open regression (layer count / question quality).

**Root cause analysis (layers):**
The decomposer receives source material summaries (file-level headlines like "this is a React component", "this is a config file") and interprets "major dimensions" as filesystem dimensions. It produces questions like "What's in the docs folder?" which are naturally narrow → is_leaf=true is CORRECT by the branch criteria. The fix is not to force branches — it's to produce questions that are conceptually broad, which will NATURALLY become branches because they span many files.

**What changed in decompose.md:**
- Added explicit BAD decomposition list (file-location questions)
- Added explicit GOOD decomposition examples (purpose, capabilities, architecture, data flow)
- Reframed HOW TO DECOMPOSE step 1: "What does this system DO?" instead of "What are the major DIMENSIONS?"
- Tightened depth-1 BRANCH rule: conceptual areas are "almost always BRANCHES"

**Hypothesis:** Conceptual question framing will produce questions like "What does Vibesmithy do?", "How does the spatial exploration work?", "What are the core data models?" — these span many files → BRANCH. Depth-1 items will mostly be branches → 3-layer pyramid restored. Quality of apex should improve dramatically (explains what Vibesmithy IS, not just where files live).

**Risk:** utilization could drop if conceptual questions don't map to all 34 source files. Some files (public/, docs/, config) might not be reached by capability questions. Acceptable tradeoff if apex quality improves.

## Experiment 4 — Conceptual decomposition framing (tune-4)
Branch: research/question-pyramid-tuning / Type: real / Parent: #3
Hypothesis: Conceptual framing in decompose.md will produce branch questions that span files → 3+ layers
Changes: decompose.md — added BAD/GOOD decomposition examples, purpose-first framing, depth-2 parent anchoring
Result: 94% util (32/34). 4 layers (L0=34, L1=7, L2=7, L3=1). BUT 6/7 L2 branches "no evidence" — extraction didn't capture purpose/config/docs topics.
Status: regression
Insight: Conceptual framing produced branches (4 layers ✓) but extraction_schema didn't generate directives for branch topics. L1 leaves were still src/-structure questions. Branch questions (purpose, config, docs) had no evidence → empty answers → apex generic.

## Experiment 5 — Extraction covers branches + no-doc-questions (tune-5)
Branch: research/question-pyramid-tuning / Type: real / Parent: #4
Hypothesis: extraction_schema covering branch+leaf questions + no-document-specific-questions will fill empty branches
Changes: extraction_schema.md — cover all tree levels. decompose.md — no document-specific questions, stay within corpus, calibrate to complexity
Result: 94% util. 4 layers (L0=34, L1=66, L2=9, L3=1). extraction_schema hit length limit (47k tokens, 2x retry). 20% empty nodes (15/76).
Status: regression
Insight: extraction_schema enumerated directives per-question → 47k output. 76 questions is over-decomposed. Even successful third attempt was 11,736 tokens. Empty nodes from questions about docs/testing/deployment absent from corpus.

## Experiment 6 — Consolidated extraction + corpus calibration (tune-6)
Branch: research/question-pyramid-tuning / Type: real / Parent: #5
Hypothesis: Consolidated extraction (4-8 themes), terse output, no Pillar 37 violations → fewer questions, fewer empties
Changes: extraction_schema.md — consolidated themes, brief output. decompose.md — STAY WITHIN CORPUS, CALIBRATE TO COMPLEXITY, no doc-specific questions
Result: 97% util (33/34). 4 layers (L0=34, L1=39, L2=7, L3=1). KEEP=206, DISCONNECT=59. Empty: 13/47 (28%).
Status: keep*
Insight: Best result on v1 pipeline. Apex names Vibesmithy correctly, describes three-tier architecture. No length errors. But 28% empty nodes persist — decompose still asks about testing, deployment, CLAUDE.md. Root cause identified: decompose runs BEFORE extraction, sees only $characterize, not real content. Prompt mitigations can't fully solve a data-availability problem.

Haiku blind assessment (tune-6):
- Assessor A: CONDITIONAL PASS (4,4,3,4,3). Strong: UI component naming, hook descriptions. Weak: 24% empty leaves.
- Assessor B: FAIL (3,2,2,2,—). "Museum tour not knowledge base." Depth quality weak, branches repeat apex.
- Shared diagnosis: questions about absent topics, structure over behavior.

## ARCHITECTURE PIVOT — Extract-First (v2 Pipeline)

Root cause analysis across experiments 0-6: decompose runs before L0 extraction. It sees only $characterize (one-paragraph corpus description) and generates speculative questions. Document.yaml solves this by extracting first, then clustering from real content.

v2 pipeline reorder: source_extract → l0_webbing → refresh_state → enhance → decompose → extraction_schema → evidence_loop → gap_processing → l1_webbing → l2_webbing

New prompt: source_extract.md — generic, content-type neutral, dehydration-friendly schema (headline/orientation/topics with summary/current/entities).

Spec: docs/plans/question-pipeline-v2-extract-first.md. Builder audit: PASS with fixes (#2 evidence_loop refs $refresh_state, #4 compact_inputs on webbing).

Rust fixes in parallel: unified ## FILE: chunk header for all content types (was ## DOCUMENT: for docs, breaking pyramid_file_hashes). All friction log items resolved.

## Experiment 7 — v2 pipeline first attempt (tune-7, INVALID)
Branch: research/question-pyramid-tuning / Type: real / Parent: #6
Changes: Full v2 pipeline. source_extract.md, question.yaml v2, l0_webbing with compact_inputs
Result: INVALID — l0_webbing hit length limit (309 input → 47,317 output). compact_inputs stripped L0 nodes to 309 tokens despite 128k context window available. Mercury generated exhaustive N×N edge pairs. Build continued but produced stacked overlays (2 question builds on same slug).
Root cause: compact_inputs is a blunt on/off switch that always strips, not token-aware. For 34 nodes the full content (~30-50k tokens) fits easily in 128k. No reason to dehydrate.
Fix: Removed compact_inputs from l0_webbing.

## Experiment 8 — v2 pipeline clean run (tune-8)
Branch: research/question-pyramid-tuning / Type: real / Parent: #7
Changes: Removed compact_inputs from l0_webbing. Fresh slug.
Result: 97% util (33/34). 4 layers (L0=34, L1=14, L2=8, L3=1). KEEP=99, DISCONNECT=19. Empty: 9/22 (41%).
Status: mixed
Insight: v2 extract-first architecture works — decompose sees real L0 content, produces fewer questions (22 vs 47). BUT empty nodes WORSE (41% vs 28%). Root cause shifted: generic extraction mentions entities (handoff guide, docs, contribution guidelines) without containing their content. Decompose sees mentions and asks about them. Also: source_extract.md too detailed — producing 4 topics per component with CSS classes and prop values. Needs tightening to role/purpose only, not implementation details.

source_extract.md rewritten: "Most sources have one topic. Complex sources have two." Explicit WHAT DOES NOT BELONG section (CSS, props, hook lifecycle, boilerplate). Reframed to "what role it plays in the system" not "what it contains."

## THINK — before Experiment 9

**Convergence signals:**
- 8 real experiments across v1 and v2 pipelines
- Utilization: SOLVED (97-100% since exp 1). Not the bottleneck.
- Layer depth: SOLVED (4 layers since exp 4, when conceptual framing was added)
- Empty nodes: UNSOLVED and WORSENING — 28% in best v1 (exp 6), 41% in v2 (exp 8)
- Global best (exp 6, 97% util) unchanged for 2 real experiments — not yet at plateau guardrail threshold (8+) but trending

**Root cause chain (empty nodes):**
1. Exp 0-3: decompose ran before extraction → speculative questions about absent topics
2. Exp 4-6: v1 mitigations (STAY WITHIN CORPUS, calibrate, conceptual framing) reduced empties from 28% but couldn't solve the data-availability problem
3. Exp 7-8: v2 pivot (extract-first) solved data availability but introduced NEW problem: source_extract.md was too detailed → L0 nodes contained entity mentions (handoff guide, CLAUDE.md, CSS classes, prop values) → decompose sees mentions, asks about them → evidence_loop can't find content for those entities → empty nodes
4. Root cause = extraction granularity too fine. source_extract.md rewritten but UNTESTED.

**What changed in source_extract.md (untested):**
- "Most sources have one topic. Complex sources have two." — caps topic proliferation
- WHAT DOES NOT BELONG section: CSS classes, prop values, function signatures, state variables, hook lifecycle, event handlers, render logic, boilerplate, imports, type declarations
- Reframed from "what it contains" to "what role it plays in the system"
- Topic names: "name the concept, not the file" — "Partner Chat Session" not "ChatLobbyPage"

**Untested assumptions:**
1. Fewer, coarser topics in L0 extraction will reduce entity mentions that trigger speculative decompose questions → fewer empty nodes. This is the core hypothesis.
2. I have NOT tested: what if decompose.md itself needs to be told "entities in L0 extractions are references, not topics to ask about." The handoff flagged this as a possible secondary fix.
3. I have NOT tried the opposite direction — what if MORE detailed extraction with a STRONGER decompose constraint works better? Unlikely given the 47k length-out in exp 5, but noted.

**Invalidation risk:**
- The v2 pipeline YAML also has uncommitted changes alongside source_extract.md. Need to verify what changed in question.yaml — if it's just the compact_inputs removal from exp 7-8, that's expected. If there are other changes, they confound this experiment.

**Next hypothesis:** Tightened source_extract.md (1-2 topics per source, role-not-implementation, explicit exclusions) will reduce empty nodes from 41% to under 20% while maintaining 97%+ utilization and 4-layer depth. If empties are ≤15%, proceed to Haiku assessors.

**If this fails:** Add explicit guidance to decompose.md: "Entity names in extracted summaries are cross-references, not topics. Do not ask questions about entities unless they represent major system components that multiple sources describe."

## Experiment 9 — Tightened source_extract.md (tune-9c)
Branch: research/question-pyramid-tuning / Type: real / Parent: #8
Hypothesis: Coarser L0 extraction (1-2 topics, role-not-implementation, explicit exclusion list) reduces entity-mention-driven speculative questions → fewer empty nodes
Changes: source_extract.md rewritten — "Most sources have one topic. Complex sources have two." + WHAT DOES NOT BELONG section (CSS, props, hooks, boilerplate). "What role it plays" not "what it contains."
Result: ~97% util. 4 layers (L0=34, total=63 nodes). Empty: 4/29 synthesis nodes (14%, was 41% in exp 8, 28% in exp 6). Build time ~280s.
Duration: ~280s
Status: keep

Haiku blind assessment (tune-9c):
- Assessor A (structural): 4,4,5,5,1 = 19/25. CONDITIONAL PASS. Specificity 5/5 — names concrete artifacts (env vars, path aliases, component hierarchies, exact config settings). Alignment 5/5 — every headline question directly answered. Empty 1/5 — 4 absent-topic nodes.
- Assessor B (user experience): 4,2,3,3,3 = 15/25. CONDITIONAL PASS. Apex 4/5 — names Vibesmithy, technologies, components. Coverage 2/5 — testing, CI/CD, docs, linting missing. Depth 3/5 — "mechanical not revelatory", L1 recites L2. Leaf usefulness 3/5 — "answers WHERE not WHAT or WHY."

Progress vs tune-6 (last assessed):
- Empty: 28% → 14% ✓
- Assessor A specificity: 3/5 → 5/5 ✓
- Assessor A alignment: 4/5 → 5/5 ✓
- Overall: COND PASS / FAIL → COND PASS / COND PASS ✓ (no FAIL)

Insight: source_extract.md tightening was the highest-impact change in this series. Three remaining issues:
1. **4 empty nodes from absent topics** — source_extract mentions testing/CI/docs/linting as MISSING, decompose sees mentions and asks about them. Fix: either (a) source_extract shouldn't mention absences, or (b) decompose should skip questions about topics flagged as absent.
2. **Depth quality** — leaves repeat branches rather than adding detail. This is an answer.md issue — synthesis at each layer needs to add specificity, not rephrase. The answer.md prompt currently says "every KEEP candidate representing a genuinely distinct dimension should be reflected" but doesn't say "add detail the parent doesn't have."
3. **Structure vs purpose** — code corpora naturally produce architecture descriptions. To get "what does it DO for users", decompose needs stronger purpose/UX priority. But this may be an inherent limitation of code-only corpora — you can't extract UX from source files that describe component APIs.

## THINK — before Experiment 10

**Re-validation guardrail (every 10th real experiment):** Due before running exp 10. However, the build I just ran IS on current HEAD (9c2c02e) — the assessors evaluated it. Current HEAD = exp 9 = global best. Re-running would produce an identical build (same prompts, same corpus). Re-validation is satisfied by the assessment just completed.

**Convergence signals:**
- 9 real experiments. 1 baseline, 3 keeps, 3 keep*, 1 mixed, 1 regression. Healthy distribution.
- Global best: exp 9 (14% empty, COND PASS / COND PASS). First clear keep since exp 2.
- Utilization: SOLVED. Not a factor anymore.
- Layers: SOLVED. 4 layers consistently since exp 4.
- Empty nodes: IMPROVED (41% → 14%) but not eliminated. 4 specific absent-topic empties remain.
- Depth quality: NEW ISSUE surfaced by assessors. Not yet addressed.
- Purpose vs structure: ONGOING concern from exp 6's FAIL assessor.

**Three improvement vectors (ranked by expected impact):**

1. **Fix empty nodes from absent-topic mentions (source_extract.md)** — The source_extract currently doesn't mention absences, but the EXTRACTION produces topics that mention other components. When decompose sees "references: handoff guide, CLAUDE.md" in L0 extractions, it asks about them. The fix is in source_extract.md: entities should only reference things that EXIST in the corpus (other source files), not external documentation. Change: "entities are cross-references to OTHER SOURCES IN THIS CORPUS, not external dependencies or documentation."

2. **Fix depth quality (answer.md)** — The answer prompt focuses on dimension coverage but doesn't instruct the LLM to ADD detail at each layer. A branch answer should synthesize its leaf answers into something that reveals patterns, connections, or insights the individual leaves don't. Change: add "your synthesis must reveal connections between the evidence that the individual pieces don't show on their own" to answer.md.

3. **Fix purpose vs structure (decompose.md)** — The prompt already has GOOD decomposition examples emphasizing purpose/capabilities, but the LLM still gravitates toward structural questions for code. This may be a code corpus limitation — but could try: "For code corpora: the first sub-question must always be about what the software DOES for its users, not how it's organized internally."

**Hypothesis for exp 10:** Fix #1 only (entities constraint in source_extract.md). Single-variable test. Expected: 4 empty nodes → 0-1 empty nodes. If successful, combine with #2 (answer.md depth) in exp 11.

**Risk:** The entity constraint might over-restrict extraction — L0 nodes that reference external systems (\"uses Supabase for auth\") would lose that context. Need to be precise: keep system/technology references, exclude references to documents or files not present in the corpus.

## Experiment 10 — Constrain entities to corpus-visible references (tune-10)
Branch: research/question-pyramid-tuning / Type: real / Parent: #9
Hypothesis: Restricting entities to corpus-present sources eliminates absent-topic decompose questions
Changes: source_extract.md — entities must reference "OTHER SOURCES IN THIS CORPUS", not external docs/infra
Result: 100% util. 4 layers (L0=34, L1=17, L2=5, L3=1, total=57). Empty: **8/23 (35%)** — WORSE than exp 9's 14%.
Duration: ~240s
Status: discard (reset to exp 9 HEAD)
Commit: c621518 (reset)

Insight: Entity constraint BACKFIRED. With fewer entities in L0 nodes, decompose had LESS information about what the corpus actually contains, and generated MORE speculative questions about absent topics (docs, testing, CI/CD, versioning, style assets). The empty nodes went from 4 specific absent-topic nodes to 8. The apex also regressed to file-layout description.

Root cause analysis: The empty node problem is NOT caused by over-rich entities in source_extract. It's caused by decompose.md generating questions about dimensions it believes a codebase SHOULD have (testing, docs, CI), regardless of whether the L0 summaries mention them. The solution is on the decompose side, not the extraction side: decompose needs to be told "ONLY ask about topics that appear in the L0 summaries you received."

## THINK — before Experiment 11

**Convergence signals:**
- 10 real experiments. Last was a discard. No consecutive discard streak yet (exp 9 was keep, exp 10 discard).
- Global best: exp 9 (14% empty, COND PASS / COND PASS).
- Pattern: extraction-side fixes (source_extract.md) have been the most impactful changes. But we just hit diminishing returns on extraction — further tightening made things worse.
- The problem has shifted from extraction quality to decompose accuracy.

**Untested assumptions:**
1. I assumed entities were the signal driving decompose to ask about absent topics. WRONG — exp 10 proved this. Decompose asks about absent topics based on its own priors about "what a codebase should have", not because entities mention them.
2. I haven't tested: what if decompose.md explicitly says "you can ONLY ask about topics that appear in the L0 summaries below — you may not infer topics that should exist but don't appear"?
3. I haven't tested the answer.md depth fix (vector #2 from exp 9 THINK) — this is independent of the empty node problem.

**Two independent paths:**
A. **Fix decompose for absent topics** — add constraint: "Every sub-question must be grounded in explicit content from the L0 summaries. Do not ask about conventional topics (testing, CI, documentation) unless the summaries contain evidence of them."
B. **Fix answer.md for depth quality** — add synthesis guidance: "Your answer must reveal connections between evidence that the individual pieces don't show on their own. Do not rephrase the same information at different verbosity."

These are independent changes to different files. Could test A alone (exp 11), then B alone (exp 12), then combine if both help.

**Hypothesis for exp 11:** Add explicit grounding constraint to decompose.md — sub-questions must reference specific content visible in L0 summaries. Expected: 4 empty absent-topic nodes → 0-1. This directly addresses the root cause (decompose generating from priors, not from data).

## Experiment 11 — Decompose Grounding Constraint (tune-11)
Branch: research/question-pyramid-tuning / Type: real / Parent: #9
Hypothesis: Explicitly telling decompose to ONLY ask about topics present in the L0 summaries will eliminate absent-topic questions.
Changes: Added Step 5 to decompose.md "GROUND CHECK: for each sub-question, verify that the source material summaries contain content that could answer it. If no source summary mentions the topic — drop the question..."
Result: 100% util. 3 layers (L0=34, L1=8, L2=1, total=43). Empty: **4/9 (44%)**.
Duration: ~300s
Status: discard (reset to exp 9 HEAD)
Commit: 63f3896 (reset)

Insight: The grounding constraint collapsed the tree entirely. By dropping so many questions, the pyramid flattened to 3 layers (no L3 apex). Yet even with this destructive constraint, it STILL produced empty nodes about testing, documentation, and static assets. The LLM simply ignores the grounding constraint when its priors about "codebases" override the prompt logic. 

## THINK — before Experiment 12

**Convergence signals:**
- 11 real experiments. Last two were discards (10 and 11). We've hit a wall on reducing the 4 empty nodes.
- Global best: exp 9 (14% empty, COND PASS / COND PASS).
- Both source_extract (exp 10) and decompose (exp 11) interventions backfired, causing regression.

**Strategic Pivot:**
The 4 remaining empty nodes (testing, CI/CD, docs, linting) represent ~14% of synthesis nodes. This achieved the ≤15% success metric set for this session (down from 41%). We shouldn't break the whole pyramid (like we did in 10 and 11) trying to chase these last 4 nodes, especially when they represent genuine absences in the codebase structure. A 14% empty node rate with 5/5 Specificity is an acceptable tradeoff for now.

Let's shift focus to the second major issue flagged by the assessors: **Depth Quality**.
Assessor B noted: "mechanical not revelatory", L1 recites L2. Leaves repeat branches instead of adding detail.

The issue lies in `chains/prompts/question/answer.md`. Currently, it says: "every KEEP candidate that represents a genuinely distinct dimension should be reflected." But it doesn't instruct the synthesis process to meaningfully elevate or combine details as it moves up the pyramid.

**Hypothesis for exp 12:** Update `answer.md` to explicitly demand that synthesis adds value. Specifically: "Your synthesis must reveal connections between the evidence that the individual pieces don't show on their own. Do not just rephrase the same information at different verbosity." By doing this, we expect the depth quality to jump from 3/5 to 4/5 or 5/5 in the next assessment.

## Experiment 12 — Invalid Run (tune-12)
Status: invalid
Reason: Forgot to sync the reverted `decompose.md` (which removed the destructive continuous ground check from Exp 11) to the Application Support config directory after running `git reset`. The backend executed tune-12 using the destructive Exp 11 `decompose.md` combined with the Exp 12 `answer.md`.
Result: Layers collapsed back down to 3, as expected from the Exp 11 bug. Discarded.

## Experiment 12 — Valid Run: Branch Synthesis Rules (tune-13)
Branch: research/question-pyramid-tuning / Type: real / Parent: #9
Hypothesis: Updating `answer.md` to explicitly demand value-adding synthesis for BRANCH nodes will improve the depth quality reported by assessors.
Changes: Added SYNTHESIS RULES to `answer.md` explicitly forbidding mechanical rephrasing and requiring the revelation of connections, patterns, or combined purpose that individual pieces don't show.
Result: 100% util. 3 layers (L0=34, L1=8, L2=1, total=43). Empty: **1/9 (11%)**.
Duration: ~200s
Status: mixed (keep for analysis)
Commit: 323629d
Assessors: FAIL (Assessor A) / CONDITIONAL PASS (Assessor B). Depth Quality dropped to 1/5!

Insight: The synthesis rules worked for content, but because `answer.md` handles both evidence acceptance (KEEP) and synthesis composition, the instructions to "not mechanics/concatenate" caused Mercury to DISCONNECT perfectly good additive evidence. Evidence acceptance is not a zero-sum game; we want all relevant evidence kept, even if it's just an additive detail. By disconnecting additive evidence, the tree collapsed entirely, resulting in a flat (3-layer) pyramid with no real branches, which is why Depth Quality and Alignment were heavily penalized.

## THINK — before Experiment 14

**Status:**
`tune-9c` remains the high-water mark. `tune-13` proved that the synthesis changes work for insight, but break the structure if they infect the evidence acceptance logic.

**Two Distinct Jobs:**
1. **Evidence Triage:** `KEEP` should be permissive. If it's factually related, keep it. Additive details are fine. The `weight` parameter is what handles profoundness (0.9 vs 0.3).
2. **Composition:** What we do with the kept evidence. This is where we demand "revelatory" connections.

**Hypothesis for exp 14:** Edit `answer.md` to cleanly decouple Triage from Composition. Explicitly instruct the LLM that `KEEP` is NOT a zero-sum game—keep all additive details. The demand for value-adding synthesis applies *only* to the composition step. We expect this to restore the 4 layers (from `tune-9c`) while maintaining the dense synthesis qualities (from `tune-13`), aiming for 5/5 Depth Quality arrays.

## Experiment 14 — Invalid Run (tune-14)
Status: invalid
Reason: The `source_extract.md` prompt in the backend was *still* the discarded version from Experiment 10 ("Entities: cross-references to OTHER SOURCES IN THIS CORPUS"). When I ran `git reset --hard HEAD~1` previously, I didn't recursively copy all prompts from the repo over to `Application Support/...`. Thus, tune-14 ran decoupled answers, but with severely restricted L0 entities that caused Decompose to generate 40 sub-questions to compensate, resulting in 18 empty nodes (37.5%).
Result: Discarded.

## Experiment 15 — Decoupled Triage & Synthesis (clean rerun) (tune-15)
Branch: research/question-pyramid-tuning / Type: real / Parent: #13
Hypothesis: Decoupling permissive evidence triage from demanding synthesis composition will restore the 4-layer structure while preserving high depth/insight.
Changes: Cleanly running the exact `answer.md` from Exp 14 but verifying we are using the exact `source_extract.md` and `decompose.md` from Exp 9.
Result: 100% util. 4 layers restored! (L0=34, L1=5, L2=6, L3=1, total=46). Empty nodes: 3/12 (25%).
Duration: ~300s
Status: mixed (COND PASS / FAIL)
Commit: a60b23a

Insight: Decoupling evidence triage from synthesis successfully restored the 4-layer structure! The apex score hit **5/5** for Assessor A for the first time. However, Assessor B failed it because 3 out of 6 L2 branches were empty (50%). The empty nodes are caused by two bugs:
1. `pre_map` miss: It's missing obvious L0 matches (e.g. config files) despite "over-include" instructions.
2. The Rust Executor gap: Zero-candidate sets from `pre_map` bypass `MISSING` verdicts and gap_processing. The node just gets an empty answer and dies.

## THINK — before next step (Rust Executor Fix)
To actually fix the empty node problem at the architectural level, we need to alter the `agent-wire-node` Rust backend:
- `pre_map`: Investigate why it's missing obvious L0 candidates (prompt tweaking or structure mismatch).
- `evidence_loop` primitive: If zero candidates are returned, the engine must auto-generate a `MISSING` synthetic verdict to trigger `gap_processing` and recover the branch instead of generating an empty answer.

Entering Planning Mode to formulate a fix for the Rust backend.

## THINK — before Experiment 16

**Status:** The Rust gap fix is now built and active.
**Convergence signals:** Empty branching in Exp 15 was caused by zero-candidate outcomes in `pre_map` arriving at the LLM, causing it to hallucinate. 
**Untested assumptions:** Let's verify that emitting synthetic `MISSING` verdicts directly from Rust correctly preserves the 4-layer structure and stops the LLM hallucination without breaking internal dependencies.
**Next hypothesis:** Running the exact decoupled prompt logic (from Exp 15) against the patched Rust boundary will yield identically robust synthesis but handle zero-candidate paths cleanly as gaps, proving the gap-processing pipeline is functionally wired.
