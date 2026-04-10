# Pyramid Folders, Model Routing & Full-Pipeline Observability — Build Plan

## Context

The vision doc (`docs/vision/pyramid-folders-and-model-routing-v2.md`) defines 11 interlocking capabilities. Through four audit rounds + the Wire Native Documents canonical-correction pass, the plan now includes 14 spec docs that align with the canonical Wire architecture (Skills/Templates/Actions, Wire Native Documents, Supersession Chains, Handle Paths, Circle Revenue, Rotator Arm).

**Pre-work**: 14 Rust files have uncommitted clippy cleanup — commit first.

---

## Spec Docs (14 total)

### Core infrastructure (original 7)

| File | Covers |
|------|--------|
| `yaml-to-ui-renderer.md` | Schema annotation model, 10 widget types, 3 visibility levels, dynamic options, notes paradigm, IPC contracts |
| `provider-registry.md` | `LlmProvider` trait, provider tables, tier routing, local mode, cost estimation data flow, cross-provider fallback, credential variable references |
| `change-manifest-supersession.md` | In-place node updates, `pyramid_change_manifests` with `note` column, vine-level manifests, scope separation from config supersession |
| `llm-output-cache.md` | Content-addressable cache, `pyramid_step_cache`, `StepContext` (canonical), prompt hash computation |
| `generative-config-pattern.md` | Schema registry, generation prompts, refinement loop, all config types, seed defaults as bundled contributions |
| `evidence-triage-and-dadbear.md` | In-flight lock, triage policy, demand signals with propagation, `pyramid_deferred_questions`, `pyramid_cost_log`, fail-loud cost reconciliation, provider health |
| `vine-of-vines-and-folder-ingestion.md` | `child_type` column, topical vine YAML, folder walk driven by `folder_ingestion_heuristics` config |

### Second-round additions (5)

| File | Covers |
|------|--------|
| `config-contribution-and-wire-sharing.md` | Unified `pyramid_config_contributions` table, supersession chains, operational sync for all 8 schema types, custom chain bundle serialization, notes capture lifecycle, Wire publication wiring |
| `build-viz-expansion.md` | Extended `TaggedKind` events, event emission point map, step timeline UI, cost accumulator, reroll-with-notes IPC, cross-pyramid timeline reference |
| `cache-warming-and-import.md` | Importing pyramids with content-addressable cache validation, source staleness check, DADBEAR auto-integration |
| `cross-pyramid-observability.md` | Cross-pyramid build timeline, cost rollup, pause-all (scope: all/folder/circle), cross-pyramid reroll |
| `credentials-and-secrets.md` | `.credentials` file, `${VAR_NAME}` substitution, 0600 permission enforcement, Wire-share safety, never-log rule |

### Wire integration (2)

| File | Covers |
|------|--------|
| `wire-contribution-mapping.md` | **Canonical Wire Native Documents schema** mirror, mapping from local `schema_type` → Wire `skill`/`template`/`action`, `WireNativeMetadata` struct matching canonical exactly, `derived_from` 28-slot rotator arm allocation, sections-based decomposition, supersession chain carryover with path references, prepare LLM skill-based enrichment, one-click publish |
| `wire-discovery-ranking.md` | Ranking signals (rating, adoption, freshness, chain length, reputation, challenge rate, internalization), composite score via `wire_discovery_weights` template contribution, recommendations engine, supersession notifications, quality badges |

---

## Canonical Alignment (Wire Native Documents)

The `wire-contribution-mapping.md` spec mirrors the canonical Wire Native Documents schema from `GoodNewsEveryone/docs/wire-native-documents.md` exactly:

- **Routing**: `destination` (corpus/contribution/both), `corpus`, `contribution_type` (skill/template/action/analysis/...), `scope` (unscoped/fleet/circle:<name>)
- **Identity**: `topics`, structured `entities` ({name, type, role}), `maturity` (draft/design/canon/deprecated)
- **Relationships**: `derived_from` + `supersedes` + `related` using canonical `ref:` / `doc:` / `corpus:` reference formats (paths/handle-paths, NOT resolved UUIDs — resolution happens at sync time)
- **Claims**: `trackable` assertions with optional `end_date`
- **Economics**: `price` OR `pricing_curve` (mutually exclusive), `embargo_until`
- **Distribution**: `pin_to_lists`, `notify_subscribers`
- **Circle splits**: `creator_split` summing to 48 slots, operator meta-pools
- **Lifecycle**: `auto_supersede`, `sync_mode` (auto/review/manual)
- **Decomposition**: `sections` map — one source contribution produces multiple Wire contributions (used for custom chain bundles that publish inline skills alongside the action)

`derived_from` uses **float weights** at authoring time, converted to **28-slot integer allocation** at publish time via largest-remainder method (per `economy/wire-rotator-arm.md`). Minimum 1 slot per source, maximum 28 sources.

Every local contribution carries canonical metadata from the moment of creation. Publishing is a button click — no re-entry of metadata, no separate publish form.

---

## User Directives Incorporated

From the session where Adam reviewed deferrals:

| Directive | How it lands |
|---|---|
| "Skills = .md prompts (Wire terminology)" | All prompts (generation, extraction, merge, heal, prepare, change manifest) become `skill` contributions. Runtime lookups resolve through a prompt cache indexed by legacy paths. |
| "Schema annotations = templates" | `schema_annotation` and `schema_definition` schema types both map to Wire `template` with `applies_to: "ui_annotation"` / `"validation"` respectively. |
| "Cross-provider fallback with site-specific overrides" | `provider-registry.md` cross-provider fallback section. Pipeline-wide fallback chains in `tier_routing` contributions, per-step overrides via `step_overrides` contributions. |
| "Cache warming on import (staleness check against source files)" | `cache-warming-and-import.md`. Imported cache entries are content-addressable — if the local source file hash matches, the entry is valid. |
| "Credentials in .credentials file, composable variables" | `credentials-and-secrets.md`. `${VAR_NAME}` substitution preserves references when Wire-publishing; variable names are portable, values stay on-device. |
| "Specify schema versioning maximally (another contribution type)" | Schemas are `template` contributions with `applies_to: "validation"`. Superseding a schema flags existing configs as needing migration — migration is an LLM-assisted refinement producing a new config contribution. |
| "Cost reconciliation should fail loudly" | `evidence-triage-and-dadbear.md` provider health alerting. No self-correction. 10% discrepancy threshold, 3+ discrepancies in 10 min → `provider_health = degraded`, manual acknowledge required. |
| "Wire discovery full spec" | `wire-discovery-ranking.md`. Composite ranking, recommendations, auto-update notifications, quality badges. |
| "Capture Wire Native metadata at creation, one-click publish" | `wire-contribution-mapping.md`. Every contribution creation path initializes canonical metadata; publishing is confirm-and-write. |
| "Custom chain validation: punt test workflow for now" | Validation is structural only (YAML parses, prompts resolve). Test workflow deferred. |
| "Cross-pyramid views (build viz, cost rollup, pause-all)" | `cross-pyramid-observability.md`. |
| "Reroll extended to clustering/intermediate outputs" | `build-viz-expansion.md` Node Reroll section. Any cached LLM output can be rerolled with notes. |
| "Demand signals propagate with attenuation" | `evidence-triage-and-dadbear.md`. 0.5 per layer, floor 0.1, max_depth 6 (all configurable in `evidence_policy`). |
| "Deferred questions re-evaluate on policy change" | `evidence-triage-and-dadbear.md`. Contribution supersession handler calls `reevaluate_deferred_questions()`. |
| "Seed defaults as bundled contributions, not hardcoded" | `generative-config-pattern.md` Seed Defaults Architecture. Bundled = starting point, not absolute standard. |

---

## Phase 0: Clippy Cleanup + Finish Pipeline B (fire_ingest_chain wiring)

### 0a — Commit clippy cleanup

Commit the 14 modified Rust files as a clean starting point. See handoff doc for the file list.

### 0b — Finish Pipeline B

**Background — the DADBEAR subsystem has two pipelines with different responsibilities:**

- **Pipeline A (`watcher.rs`, 2026-03-23)** — maintenance of already-ingested files. fs-notify events → `pyramid_pending_mutations` → `stale_engine.rs` polls + debounces → `stale_helpers_upper.rs::execute_supersession`. This pipeline is live, has its own guards (`start_timer` debounce at line 328, `check_runaway` breaker at line 612), and handles "file I know about changed, re-sync affected nodes."
- **Pipeline B (`dadbear_extend.rs`, 2026-04-08 — shipped one day before this plan was written)** — creation/extension. Polling scanner → `pyramid_ingest_records` → `dispatch_pending_ingests` → `fire_ingest_chain` (CURRENTLY STUBBED at lines 401-408) → should run a content-type chain to ingest new files into the pyramid. Header comment: "Extends DADBEAR from maintenance-only to also handling CREATION of pyramids."

**The problem**: Pipeline B's chain dispatch was left stubbed with `format!("dadbear-ingest-{}-{}", slug, uuid::Uuid::new_v4())` awaiting WS-EM-CHAIN, which never landed. Today `dispatch_pending_ingests` marks records "complete" immediately with a placeholder build_id and nothing downstream reads `pyramid_ingest_records`. Pipeline B is effectively dead code.

**What 0b does**: replace the stub with a real `fire_ingest_chain` helper that:

1. Resolves the active chain definition for the ingest record's `content_type` via the existing chain registry
2. Constructs a `ChainContext` with the new source file as the ingest input (via the existing `cross_build_input` primitive or the equivalent ingest entry point)
3. Calls into `build_runner::run_build_from()` / `chain_executor::invoke_chain()` — whichever is the correct entry for firing an ingest chain against a single source file
4. Captures the returned `build_id` and returns it so the ingest record can be marked complete with the real build_id
5. On chain failure, returns an error that `dispatch_pending_ingests` translates into `mark_ingest_failed` + `IngestFailed` event emission (the existing code path)
6. Holds LockManager write locks correctly during dispatch so concurrent DADBEAR tick cycles can't race the same record (works together with Phase 1's in-flight flag as defense in depth)

**What 0b does NOT do**:
- Does not obsolete Pipeline A. Pipeline A continues to handle maintenance of already-ingested files via fs-notify → stale engine.
- Does not merge the two pipelines. They remain complementary by design: Pipeline B = "new file to ingest", Pipeline A = "known file changed, re-sync affected nodes."
- Does not change `execute_supersession` behavior (that's Phase 2's change-manifest work, independent of this).

**Files**: `dadbear_extend.rs` (replace stub at 401-408 + new `fire_ingest_chain` helper), possibly `ingest.rs` (if a new per-file ingest entry point is needed), `event_bus.rs` (no changes expected; existing `IngestStarted`/`IngestComplete`/`IngestFailed` variants cover it).

**Verification**: enable DADBEAR on a test folder, drop a new source file in, observe: (1) ingest record created, (2) real chain builds, (3) new L0 node appears in the pyramid, (4) ingest record marked complete with the real `build_id`. Repeat with a failing chain to verify the `mark_ingest_failed` + `IngestFailed` event path works.

**Why this is Phase 0 and not a later phase**: Phase 1 (in-flight lock) becomes meaningful only when Pipeline B actually does work per tick. Phase 17 (folder ingestion) creates DADBEAR configs expecting them to drive pyramid creation — which requires Pipeline B to be live. Doing 0b up front avoids building against a dead pipeline that later needs to be rewired.

---

## Phase 1: DADBEAR In-Flight Lock (Pipeline B tick guard)

**Spec**: `evidence-triage-and-dadbear.md` Part 1

**What**: `HashMap<i64, Arc<AtomicBool>>` per config.id in the tick loop. Before calling `run_tick_for_config`, check the flag — if set, skip this tick for this config. Set `true` before calling, `false` on return.

**Scope clarification (corrected from the original spec draft)**:

- This lock guards **Pipeline B only** (the `dadbear_extend.rs` tick loop). It has nothing to do with Pipeline A's stale engine, which has its own guards (`start_timer` debounce at `stale_engine.rs:328`, `check_runaway` breaker at `stale_engine.rs:612`).
- The symptom "200 files → 528 L0 blowup" is NOT caused by tick re-entrancy. It's caused by Pipeline A's `execute_supersession` creating new nodes per stale check + cross-thread propagation in `stale_helpers_upper.rs:1327` writing more `confirmed_stale` mutations, producing a cascade. **Phase 2 (change-manifest) is what fixes that symptom.** This lock does not fix it.
- This lock exists because once Phase 0b wires real chain dispatch, a tick's `dispatch_pending_ingests` can take minutes. Without the lock, the next 1-second tick would start a second concurrent dispatch for the same config and race on ingest record state transitions. The lock is the right guard for that specific race.

**Files**: `dadbear_extend.rs` (~20 lines)

**Verification**: mock `fire_ingest_chain` to block on a future for 30 seconds. Enable DADBEAR on a test folder with `scan_interval_secs: 1`. Drop a file. Assert: first tick starts dispatch, subsequent ticks log "DADBEAR: skipping tick, previous dispatch in-flight" instead of launching a second dispatch. When the 30-second mock completes, the next tick runs normally.

---

## Phase 2: Change-Manifest Supersession

**Spec**: `change-manifest-supersession.md`

**What**: In-place node updates via change manifests. Same ID, bumped `build_version`. `pyramid_change_manifests` with `note` column. Vine-level manifests for propagation through vine hierarchy.

**Files**: `stale_helpers_upper.rs`, `supersession.rs`, `db.rs`, `query.rs`, new prompt `chains/prompts/shared/change_manifest.md`

---

## Phase 3: Provider Registry + Credentials

**Specs**: `provider-registry.md`, `credentials-and-secrets.md`

**What**: `LlmProvider` trait, `pyramid_providers` + `pyramid_tier_routing` + `pyramid_step_overrides`. Local mode toggle. Cross-provider fallback. `.credentials` file + `${VAR_NAME}` substitution. Credential variable references in provider configs.

**Files**: `llm.rs`, new `provider.rs`, `db.rs`, `routes.rs`, new `credentials.rs`, settings UI

---

## Phase 4: Config Contributions Foundation

**Spec**: `config-contribution-and-wire-sharing.md`

**What**: `pyramid_config_contributions` as unified source of truth. `wire_native_metadata_json` + `wire_publication_state_json` columns. Operational sync for all 8 schema types. Bundled contribution bootstrap on first run. Migration of legacy tables.

**Files**: `db.rs`, new `config_contributions.rs`, new `bundled_contributions.rs`

---

## Phase 5: Wire Contribution Mapping (Canonical)

**Spec**: `wire-contribution-mapping.md`

**What**: Canonical `WireNativeMetadata` struct. Creation-time capture. Prompt + schema migration from disk to skill/template contributions. Prompt lookup cache for runtime resolution. Largest-remainder 28-slot allocation helper. Prepare LLM enrichment via bundled prepare skill.

**Depends on**: Phase 4

**Files**: `db.rs` (new columns), new `wire_native_metadata.rs`, new `rotator_allocation.rs`, new `wire_prepare.rs`, new `prompt_cache.rs`, migration path

---

## Phase 6: LLM Output Cache + StepContext

**Spec**: `llm-output-cache.md`

**What**: Content-addressable cache, `pyramid_step_cache`, unified `StepContext` struct threaded through `call_model_unified()`, prompt_hash computation cached in ChainContext.

**Files**: `db.rs`, `chain_executor.rs`, `llm.rs`

---

## Phase 7: Cache Warming on Import

**Spec**: `cache-warming-and-import.md`

**Depends on**: Phase 6, Phase 4

**What**: Import flow that populates local cache from source pyramid's cache manifest. Staleness check against local source file hashes. DADBEAR auto-configuration for imported pyramids.

**Files**: New `import.rs`, `db.rs` (cache manifest schema), `ToolsMode.tsx` (import UI)

---

## Phase 8: YAML-to-UI Renderer

**Spec**: `yaml-to-ui-renderer.md`

**What**: Generic `YamlConfigRenderer` React component. Schema annotations loaded from `schema_annotation` template contributions (not disk).

**Files**: New `YamlConfigRenderer.tsx`

---

## Phase 9: Generative Config Pattern

**Spec**: `generative-config-pattern.md`

**Depends on**: Phase 4, Phase 5, Phase 8

**What**: Intent → YAML → notes → contribution flow. Generation prompts loaded as skill contributions. Schemas loaded as template contributions. Notes capture lifecycle enforcement at IPC boundary.

**Files**: New `config_schema.rs`, IPC handlers

---

## Phase 10: ToolsMode UI Integration

**Spec**: `config-contribution-and-wire-sharing.md` → Frontend: ToolsMode.tsx + `wire-contribution-mapping.md` dry-run publish modal

**Depends on**: Phase 4, Phase 5, Phase 8, Phase 9

**What**: Extend existing `ToolsMode.tsx`:
- **My Tools** tab: all config contributions grouped by `schema_type`, with metadata review, dry-run publish, publish-to-Wire button
- **Discover** tab: Wire config browser with ranking (pointer to Phase 14)
- **Create** tab: generative config entry point

**Files**: `ToolsMode.tsx`

---

## Phase 11: OpenRouter Broadcast Webhook + Fail-Loud Reconciliation

**Spec**: `evidence-triage-and-dadbear.md` Parts 3/4 + provider health alerting

**Depends on**: Phase 3

**What**: Broadcast webhook receiver, cost reconciliation, fail-loud discrepancy handling, `provider_health` column and IPC.

**Files**: `server.rs` (webhook route), `provider.rs` (augment_request_body, health tracking)

---

## Phase 12: Evidence Triage + Demand Signal Propagation

**Spec**: `evidence-triage-and-dadbear.md` Part 2 + propagation

**Depends on**: Phase 3, Phase 4, Phase 9

**What**: Triage step. Demand signals with weighted propagation (attenuation factor 0.5, floor 0.1, max_depth 6, all configurable). Deferred questions re-evaluate on policy change. `pyramid_deferred_questions` table.

**Files**: `evidence_answering.rs`, new `triage.rs`, `db.rs`, MCP server + `query.rs` (demand signal recording)

---

## Phase 13: Build Viz Expansion + Reroll + Cross-Pyramid

**Specs**: `build-viz-expansion.md`, `cross-pyramid-observability.md`

**Depends on**: Phase 6

**What**: Extended events, step timeline, cost accumulator, reroll-with-notes for any cached output, cross-pyramid timeline view, cost rollup, pause-all.

**Files**: `event_bus.rs`, `PyramidBuildViz.tsx` refactor, new `CrossPyramidView.tsx`, new `CostRollup.tsx`

---

## Phase 14: Wire Discovery & Ranking

**Spec**: `wire-discovery-ranking.md`

**Depends on**: Phase 5 (wire-contribution-mapping), Phase 10 (ToolsMode)

**What**: Composite ranking, recommendations engine, supersession notifications, quality badges. `wire_discovery_weights` template contribution for configurable ranking algorithm.

**Files**: New `wire_discovery.rs`, `ToolsMode.tsx` Discover tab

---

## Phase 15: DADBEAR Oversight Page

**Spec**: `evidence-triage-and-dadbear.md` Part 3

**Depends on**: Phase 11, Phase 12, Phase 13

**What**: Frontend page assembling per-pyramid DADBEAR status, cost reconciliation, provider health alerts, pause/resume controls.

**Files**: New `DadbearOversight.tsx`

---

## Phase 16: Vine-of-Vines + Topical Vine Recipe

**Spec**: `vine-of-vines-and-folder-ingestion.md` Part 1

**Depends on**: Nothing (parallel-safe)

**What**: Extend `pyramid_vine_compositions` with `child_type` column. Topical vine chain YAML. Propagation through vine hierarchy using change manifests.

**Files**: `db.rs`, `vine_composition.rs`, `vine.rs`, new chain YAML + prompts

---

## Phase 17: Recursive Folder Ingestion

**Spec**: `vine-of-vines-and-folder-ingestion.md` Part 2

**Depends on**: Phase 16, Phase 9 (folder_ingestion_heuristics config), Phase 4

**What**: Folder walk driven by `folder_ingestion_heuristics` contribution YAML. Content detection, self-organizing rules.

**Files**: New `folder_ingestion.rs`, `AddWorkspace.tsx`

---

## Parallelism Map

```
Session 1: Phase 0 → Phase 1 → Phase 2
Session 2: Phase 3 (provider+credentials) → Phase 4 (contributions)
Session 3: Phase 5 (wire mapping) → Phase 6 (cache) → Phase 7 (import)
Session 4: Phase 8 (renderer) → Phase 9 (generative) → Phase 10 (tools UI)
Session 5: Phase 11 (broadcast) + Phase 16 (vine-of-vines) [parallel]
Session 6: Phase 12 (triage) → Phase 13 (build viz) → Phase 14 (discovery)
Session 7: Phase 15 (oversight) → Phase 17 (folder ingestion)
```

Phases 11 and 16 are independent and can run in any session after their deps.

---

## This Session Scope

1. **Phase 0** — Commit clippy cleanup
2. **Phase 1** — DADBEAR in-flight lock fix
3. **Phase 2** — Start change-manifest supersession

---

## What's NOT Deferred Anymore

The round-2/3/4 audits and Adam's directives pulled these back into scope:

- ✅ Skills (prompts) as Wire contributions — replaces on-disk prompt files
- ✅ Schema annotations + definitions as Wire template contributions
- ✅ Wire Native Documents format captured at creation time
- ✅ Cross-provider fallback with site-specific overrides
- ✅ Cache warming on import with source staleness check
- ✅ Credentials file + variable substitution (Wire-share safe)
- ✅ Schema versioning (schemas ARE contributions)
- ✅ Cost reconciliation fail-loud (no self-correction)
- ✅ Wire discovery ranking + recommendations + notifications
- ✅ Agent proposal workflow (with Wire Native metadata at creation)
- ✅ Cross-pyramid views (build viz, cost rollup, pause-all)
- ✅ Reroll extended to clustering/intermediate outputs
- ✅ Demand signal propagation with attenuation
- ✅ Deferred questions re-evaluate on policy change
- ✅ Seed defaults as bundled contributions (not hardcoded)

## What's STILL Deferred (post-1.0)

- Custom chain test-build validation (structural validation only for v1)
- Cache pruning / size limits
- Automatic cache invalidation of prompt file edits mid-build (build-scoped is correct for v1)
- Client-side build viz event filtering
- Secondary market for configs (Wire-level, not in our scope)

## Canonical Schema Guardrail

**CRITICAL**: The `WireNativeMetadata` struct in `wire-contribution-mapping.md` must mirror the canonical Wire Native Documents schema from `GoodNewsEveryone/docs/wire-native-documents.md` exactly. Any future edits to either file must preserve field name parity. Divergence means our published contributions won't validate against Wire's schema.

Field names to preserve exactly: `destination`, `corpus`, `contribution_type`, `scope`, `topics`, `entities`, `maturity`, `derived_from`, `supersedes`, `related`, `claims`, `price`, `pricing_curve`, `embargo_until`, `pin_to_lists`, `notify_subscribers`, `creator_split`, `auto_supersede`, `sync_mode`, `sections`.

Reference formats to preserve: `ref:` (handle-path), `doc:` (local file path), `corpus:` (corpus-prefixed path). Never use resolved UUIDs in the canonical metadata — resolution happens at sync time via a separate `wire_publication_state` column.

---

## Verification

- **Phase 1**: Long DADBEAR dispatch → logs show "skipping tick, previous dispatch in-flight"
- **Phase 2**: Stale check → apex keeps its ID, viz tree renders correctly
- **Phase 3**: Configure Ollama provider + `${OLLAMA_LOCAL_URL}` credential → build uses local with no hardcoded URL
- **Phase 4**: Create DADBEAR policy → stored in `pyramid_config_contributions`, synced to operational table
- **Phase 5**: Publish a local contribution → Wire Native metadata serializes to canonical YAML format matching `wire-native-documents.md` byte-for-byte
- **Phase 6**: Kill app mid-build → restart → completed steps are cache hits
- **Phase 7**: Pull pyramid from Wire → unchanged source files → cache hit rate >90% on subsequent build
- **Phase 8**: Renderer shows a `WireNativeMetadata` form using a schema annotation contribution
- **Phase 9**: Intent → YAML → notes → refined YAML → accept creates a config contribution with canonical metadata
- **Phase 10**: ToolsMode shows publish-to-Wire button; clicking opens dry-run preview with resolved 28-slot allocation
- **Phase 11**: OpenRouter call → webhook → cost_log reconciled; 10% discrepancy → provider_health degraded + banner
- **Phase 12**: Agent queries an evidence node → demand signal propagates up; parent triage sees aggregated weight
- **Phase 13**: Build viz shows per-step timeline with cache hits; reroll button on any step with a cache entry
- **Phase 14**: Discover tab shows ranked Wire configs with quality badges + recommendations
- **Phase 15**: Oversight page shows provider health + cost reconciliation across all pyramids
- **Phase 16**: Vine containing another vine → propagation flows through both levels via change manifests
- **Phase 17**: Point at `AI Project Files/` → get tree of pyramids + vines matching folder structure
