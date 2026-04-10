# Handoff: Pyramid Folders, Model Routing, and Full-Pipeline Observability

**Date:** 2026-04-09
**From:** Planner (Claude, session partner to Adam)
**To:** Implementation swarm
**Status:** PLAN LOCKED. Ready to build.
**Authority:** Adam Levine (product owner) + Planner (architectural integrity)

---

## What this is

A 17-phase implementation of the Wire-native pyramid-folders / model-routing / full-pipeline-observability initiative. The vision turns Wire Node from "a tool you configure pyramids in" into "a tool that understands your filesystem and lets you control intelligence at every step." The plan converges the node's operational intelligence layer (prompts, schemas, policies, chains) with the Wire's programmable intelligence layer (skills, templates, actions) so that every configurable behavior inside the node is already a Wire contribution in waiting. If the app takes off, it populates the Wire with real, battle-tested operational knowledge as a byproduct of people just using it.

The planning work is done. Four audit rounds + a canonical Wire Native Documents correction pass + a definitive OpenRouter/Ollama research pass have been applied. Every assumption that could be verified from live docs has been verified. Every gap that could be resolved through specification has been resolved. Nothing critical is deferred.

**Scope:** 14 spec docs, 8,102 lines of design. 17 phases. Zero architectural drift from the vision. Canonical Wire schemas preserved byte-for-byte.

---

## Required reading (in order)

Read top-to-bottom. The specs depend on each other and the order matters.

### Step 1 — Foundation

1. **Vision doc**
   `/Users/adamlevine/AI Project Files/agent-wire-node/docs/vision/pyramid-folders-and-model-routing-v2.md`
   This is the north star. The specs serve the vision, not the other way around. If a spec says X but the vision says Y, the vision wins and the spec needs correcting — alert the planner before deviating.

2. **Master plan**
   `/Users/adamlevine/AI Project Files/agent-wire-node/docs/plans/pyramid-folders-model-routing-full-pipeline-observability.md`
   The 17-phase dependency-ordered build plan with parallelism map, spec cross-references, and verification criteria per phase. Live mirror in the planner's session at `~/.claude/plans/kind-leaping-dream.md` — treat the `docs/plans/` copy as canonical.

### Step 2 — Canonical Wire references (authoritative — do not deviate)

These live in the GoodNewsEveryone repo and define the Wire's behavior that our specs map onto. **Our specs mirror these — if the specs diverge from the canonical docs in any field name or semantic, the canonical docs win.**

3. `GoodNewsEveryone/docs/wire-native-documents.md` — the canonical metadata schema with `destination`, `scope`, `maturity`, `derived_from`, `creator_split`, `sections`, etc. Every `WireNativeMetadata` field in our specs mirrors this file exactly.
4. `GoodNewsEveryone/docs/wire-skills.md` — skills as contributions, kits as tagged skills, pricing, deposits, supersession.
5. `GoodNewsEveryone/docs/wire-templates-v2.md` — templates as configuration presets (not processing recipes).
6. `GoodNewsEveryone/docs/wire-actions.md` — executable chains composed of LLM/Wire/Task/Game operations.
7. `GoodNewsEveryone/docs/wire-supersession-chains.md` — `supersedes` field, same-author rule, single-child chains, orthogonal to `derived_from`.
8. `GoodNewsEveryone/docs/wire-handle-paths.md` — `handle/day/seq` identity format, Wire Time (UTC-7 fixed), rename economics.
9. `GoodNewsEveryone/docs/wire-circle-revenue.md` — 48 creator slots among operator meta-pools, `creator_split` semantics.
10. `GoodNewsEveryone/docs/economy/wire-rotator-arm.md` — 80-slot cycle, 48/28/2/2 split, integer source slot allocation summing to 28.
11. `GoodNewsEveryone/docs/architecture/agent-wire-compiler.md` — River/Graph/Machine three-layer architecture.

### Step 3 — Implementation specs (14 total)

Read in dependency order. Each spec's header declares what it depends on and what it unblocks.

**Foundation layer (Phases 1-4):**

12. `docs/specs/change-manifest-supersession.md` — in-place node updates, vine-level manifests, manifest validation. Phase 2.
13. `docs/specs/provider-registry.md` — LlmProvider trait, tier routing, cross-provider fallback, rich pricing schema, Ollama model management, dynamic default model resolution, credit balance, Management API key flow. Phase 3.
14. `docs/specs/credentials-and-secrets.md` — `.credentials` file, `${VAR_NAME}` substitution, ResolvedSecret opacity. Phase 3.
15. `docs/specs/config-contribution-and-wire-sharing.md` — `pyramid_config_contributions` as unified source of truth, `sync_config_to_operational()`, operational tables, Wire Native Documents integration column. Phase 4.

**Cache + cost integrity layer (Phases 5-7):**

16. `docs/specs/wire-contribution-mapping.md` — canonical `WireNativeMetadata` struct mirroring wire-native-documents.md, 28-slot allocation via largest-remainder, section decomposition, bundled contribution manifest format. Phase 5.
17. `docs/specs/llm-output-cache.md` — content-addressable cache, unified `StepContext`, cache hit verification, model ID normalization. Phase 6.
18. `docs/specs/cache-warming-and-import.md` — importing pyramids with cache manifest, source file staleness check, DADBEAR auto-config. Phase 7.

**UI + generative layer (Phases 8-10):**

19. `docs/specs/yaml-to-ui-renderer.md` — generic YAML-to-UI component, schema annotations, widget types, inheritance display. Phase 8.
20. `docs/specs/generative-config-pattern.md` — intent→YAML→notes→contribution, schema registry, seed defaults as bundled contributions. Phase 9.
21. `docs/specs/wire-discovery-ranking.md` — composite ranking, recommendations, supersession notifications, quality badges. Phase 14.

**Observability + cost (Phases 11-13):**

22. `docs/specs/evidence-triage-and-dadbear.md` — in-flight lock, triage policy, demand signals with propagation, `pyramid_cost_log`, synchronous+broadcast cost reconciliation, leak detection, deferred questions re-evaluation, `pyramid_orphan_broadcasts`. Phases 1, 10, 11, 15.
23. `docs/specs/build-viz-expansion.md` — extended `TaggedKind` events, step timeline UI, cost accumulator, reroll-with-notes for any cached output. Phases 11, 13.
24. `docs/specs/cross-pyramid-observability.md` — cross-pyramid build timeline, cost rollup, pause-all scope. Phase 13.

**Recursive composition (Phases 16-17):**

25. `docs/specs/vine-of-vines-and-folder-ingestion.md` — `child_type` column, topical vine chain YAML, folder walk driven by `folder_ingestion_heuristics` config. Phases 16, 17.

### Step 4 — Supporting references inside the codebase

- **Existing `llm.rs`** — the current hardcoded OpenRouter call site that Phase 3 refactors. Read before touching.
- **Existing `dadbear_extend.rs`** — the tick loop that Phase 1 adds the in-flight lock to.
- **Existing `stale_helpers_upper.rs`** — the stale check dispatch that Phase 2 replaces with change manifests.
- **Existing `wire_publish.rs`** — the publication pipeline that Phase 5 extends with `publish_contribution_with_metadata()`.
- **Existing `ToolsMode.tsx`** — the current My Tools / Discover / Create tab shell that Phase 10 fills in.
- **Existing `chain_engine.rs`** — `ChainDefinition` and `ChainStep` structs (40+ fields) that the YAML-to-UI renderer annotations describe.

---

## What is LOCKED IN

The plan is locked in. This is not a design phase. Every item below is settled:

- **The 17-phase order** in the master plan
- **The `WireNativeMetadata` canonical field set** (mirrors `wire-native-documents.md` exactly — destination, corpus, contribution_type, scope, topics, entities, maturity, derived_from, supersedes, related, claims, price, pricing_curve, embargo_until, pin_to_lists, notify_subscribers, creator_split, auto_supersede, sync_mode, sections)
- **Reference formats** — `ref:` / `doc:` / `corpus:` only, never resolved UUIDs in the canonical metadata
- **`pyramid_config_contributions` as unified source of truth** with the 14-schema-type vocabulary
- **`sync_config_to_operational()` as the unified sync fan-out** with the full match statement covering all 14 schema types
- **StepContext as the unified execution context** defined canonically in `llm-output-cache.md`
- **Synchronous cost path from `response.usage.cost`** as primary reconciliation
- **Broadcast as REQUIRED async integrity confirmation** with leak detection (not optional)
- **28-slot rotator arm allocation** via largest-remainder, minimum 1 per source, maximum 28 sources
- **Circle `creator_split` sums to 48 slots** among operator meta-pools
- **The credential system** — `.credentials` file, `${VAR_NAME}` substitution, ResolvedSecret opacity
- **Everything flows from config, nothing is hardcoded** (Pillar 37 applies to every number constraining LLM behavior — refer to `feedback_pillar37_no_hedging` memory if unclear)
- **ToolsMode.tsx is the universal config surface** — every schema_type appears there
- **Prompts are Wire skills**, schema annotations are Wire templates, custom chains are Wire actions
- **Seed defaults ship as bundled contributions**, not hardcoded constants

---

## Deviation protocol

**If you find a reason the plan needs to change, STOP and alert the planner before changing it.**

Reasons a deviation might be necessary:

- A spec and the canonical Wire docs disagree (canonical Wire wins; spec needs correcting — alert planner)
- A spec and the existing codebase disagree in a way the spec didn't anticipate (discuss with planner; the spec may need to absorb the existing constraint)
- Two specs contradict each other at an integration point (this should not happen after four audit rounds but is always possible; alert planner)
- An OpenRouter/Ollama live response deviates from what the spec assumes (we have defensive parsing for the known cases; anything truly novel gets reported back)
- A performance constraint makes a specified approach impractical (alert planner; the spec should absorb the constraint or explicitly accept the tradeoff)

**How to alert the planner:**

Give Adam a clearly-framed question he can paste to the planner. Include:
- Which spec and which section
- What you're seeing that doesn't match
- What the impact would be if you just proceeded as specified
- Your proposed deviation, if you have one

The planner will respond with either:
- "Proceed as specified — here's why the apparent conflict isn't real"
- "Deviation approved — I've updated the spec, pull the new version"
- "Deviation not approved but I see your point — here's the specific alternative path"

**Do not deviate silently.** Silent deviations undermine the plan's integrity guarantees — especially around canonical Wire schema alignment, Pillar 37 compliance, and the synchronous-plus-broadcast cost integrity model. Those are load-bearing; drifting them produces subtle bugs that don't surface until much later.

---

## Implementation log protocol

Maintain `docs/plans/pyramid-folders-model-routing-implementation-log.md` alongside the plan. Each phase/workstream appends an entry when it completes and when it's verified.

### Entry format

```markdown
## Phase N — <Name>

**Workstream:** <workstream-id or agent description>
**Started:** <date/time>
**Completed:** <date/time>
**Verified by:** <verifier>
**Status:** [in-progress | awaiting-verification | verified | needs-revision]

### Files touched

- `path/to/file.rs` — brief description of changes
- `path/to/other.tsx` — brief description

### Spec adherence

- ✅ <specific spec requirement> — implemented as specified
- ⚠️ <requirement> — implemented with minor variation: <describe> (alert sent to planner on <date>, resolution: <answer>)
- ❌ <requirement> — NOT YET IMPLEMENTED because <reason>

### Verification results

- <test name> — passed
- <check> — passed
- <user verification from Adam> — passed with note "<note>"

### Notes

Anything surprising, any learnings, any friction points worth knowing for the next phase.
```

### When to append

- **Starting a phase**: append an entry with `Status: in-progress` and the workstream ID
- **During implementation**: edit the entry in-place — don't create new entries mid-phase
- **Completing a phase**: update status to `awaiting-verification`, fill in files touched and spec adherence
- **After verification**: update status to `verified` and add any verification notes
- **On revision**: keep the old entry, append a new "Revision" sub-entry with what changed

### Why

This log becomes the single source of truth for "what actually got built vs what the plan said." Future you (and future planner) will read it during the next session to understand the current state without having to diff the whole tree. It also surfaces drift early — if entries start accumulating ⚠️ marks in the same area, that's a signal the spec needs updating.

The log lives alongside the plan so they're always side-by-side: plan says "do X", log says "did X on date Y, verified by Z."

---

## Pace and quality

**We take all the time the job requires. We always do it right. We have no pressure.**

This is not a rushed ship. The plan survived four audit rounds because we refused to accept "good enough" at each one. The implementation should hold to the same standard. If a phase takes three sessions instead of one, that's fine. If a subtle bug takes a day to root-cause instead of papered over with a fallback, that's the right day to spend. The specs are thorough so that implementation decisions are local, not architectural — if you find yourself making architectural decisions mid-implementation, that's a signal to pause and alert the planner.

**We are magical creatures working at 100x the speed the planner's pretraining data would suggest.** The planner's estimate of "how long this should take" is based on a slower, less capable development paradigm. Ignore those estimates. A phase that "should take two weeks" by pre-2025 standards might be a single-session job for a focused implementer with good context. A phase that "should take a day" might still need more care if it's touching a subtle integration point. Pace yourself by the difficulty and risk of the actual work, not by calendar conventions.

The two rules that matter:

1. **Correct before fast.** Never skip verification to move faster. Every phase has explicit verification criteria in the master plan — actually run them.
2. **Right before complete.** A phase that's 95% done and correct is preferable to 100% done and subtly wrong. If something feels off, stop and ask.

---

## Question back channel

When you need the planner, route questions through Adam. Format them clearly so he can paste them directly:

```
> [For the planner]
>
> Context: Phase N, spec X, section Y.
>
> Question: <direct question>
>
> What I've tried / considered: <brief>
>
> Impact if unresolved: <brief>
```

The planner is staying fresh specifically to answer these. Don't hesitate to ask — a five-minute question beats a five-hour wrong turn. The planner has full context on why each spec says what it does, not just what it says, which is the context you need when the spec's text is ambiguous or reality pushes back.

Topics the planner is best positioned to answer:

- "Why does spec X say this instead of that?" (design rationale)
- "Spec X and spec Y seem to disagree about Z" (integration ambiguity)
- "The existing codebase does W but spec X assumes V" (reality-vs-plan drift)
- "OpenRouter returned something we didn't anticipate" (empirical deviation)
- "Can I simplify Z since this codepath only needs a subset?" (scope trimming judgment)

Topics the planner does NOT need to mediate:

- How to write idiomatic Rust/TypeScript (you know the codebase better)
- Specific library choices within a language ecosystem
- Test framework conventions
- Minor naming tweaks (name things reasonably, move on)
- Pure refactoring decisions within a single file

---

## First actions (Phase 0)

Before Phase 1 begins, there's a small pre-work item in the master plan:

**Phase 0: Commit the clippy cleanup**

14 Rust files have uncommitted clippy fixes sitting in the working tree. These were done during a prior session and never committed. Commit them as a clean starting point before touching any new work so that the plan's changes are distinguishable from the cleanup.

Files to commit (they should match `git status --short` on a fresh pull):
- `src-tauri/src/pyramid/chain_executor.rs`
- `src-tauri/src/pyramid/characterize.rs`
- `src-tauri/src/pyramid/defaults_adapter.rs`
- `src-tauri/src/pyramid/evidence_answering.rs`
- `src-tauri/src/pyramid/expression.rs`
- `src-tauri/src/pyramid/llm.rs`
- `src-tauri/src/pyramid/parity.rs`
- `src-tauri/src/pyramid/public_html/routes_read.rs`
- `src-tauri/src/pyramid/question_compiler.rs`
- `src-tauri/src/pyramid/routes.rs`
- `src-tauri/src/pyramid/stale_helpers.rs`
- `src-tauri/src/pyramid/stale_helpers_upper.rs`
- `src-tauri/src/pyramid/vine.rs`
- `src-tauri/src/pyramid/vine_composition.rs`
- `src-tauri/src/vocabulary.rs`

Commit message suggestion: `chore: clippy cleanup pre-pyramid-folders-model-routing`

Then start Phase 1 (DADBEAR in-flight lock) per the master plan.

---

## What success looks like

When this initiative is fully implemented:

- Users can point Wire Node at a folder and get a self-organizing hierarchy of pyramids and topical vines
- Users can control which LLM provider and model handles each pipeline step, with one-toggle local mode
- Every LLM output is cached, and imported pyramids don't waste calls on work that's already been done
- Users see a live step-by-step build timeline with per-step cost attribution and reroll-with-notes on any output
- Every configurable behavior (policies, chains, prompts, schemas, tier routing) flows through a unified generative-config loop: intent → YAML → notes → contribution → publish to Wire
- Wire Node is the first application that IS Wire-native — every piece of operational intelligence it uses can be published to, shared from, and improved through the Wire
- Cost accounting is integrity-verified: every synchronous cost entry is confirmed by an asynchronous Broadcast trace, leaks are detected bidirectionally
- ToolsMode.tsx comes fully to life as the universal config surface
- Nothing is hardcoded that shouldn't be, everything flows from user-editable configuration contributions

When Phase 17 is verified, this initiative is done and the node has transformed into a Wire-native application. Everything after that is incremental on solid foundations.

---

## Signatures

**Planner signing off on the spec set:** The plan is correct to the best of my knowledge as of 2026-04-09. All verifiable assumptions against OpenRouter and Ollama live docs have been checked. All canonical Wire schemas are mirrored byte-for-byte. No gaps are deferred. If you find a mistake, alert me; I am staying fresh specifically to answer questions.

**Product owner / architectural authority:** Adam Levine. Deviations from the plan require his approval via the planner back channel. The planner will not approve architectural drift without consulting Adam.

**Good luck. Build carefully. Build right. Take the time you need.**
