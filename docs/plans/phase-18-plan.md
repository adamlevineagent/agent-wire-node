# Phase 18 Plan — Dropped-Handoff Fix Bundle

**Version:** 1.0
**Date:** 2026-04-11
**Authors:** Adam Levine + Claude (conductor)
**Depends on:** Phases 0b through 17 shipped and merged to main
**Unblocks:** Ouro test (requires Local Mode working end-to-end), general usability
**Status:** In progress

---

## Genesis

After the 17-phase pyramid-folders/model-routing/observability initiative merged to main and Adam started using the shipped app, he asked where the Local Mode toggle was. It didn't exist. That prompted an audit of every cross-phase deferral I wrote as conductor across the 17 workstream prompts.

**Finding:** 9 of 11 cross-phase deferrals dropped silently. 6 landed on Phase 10 alone. The receiving phase prompts were written against their own specs in isolation rather than against a deferral ledger, so deferred items fell through the cracks between prompts.

The full audit + meta-lesson is in:
- `docs/plans/deferral-ledger.md` — retroactive inventory of the 9 dropped + 2 picked-up deferrals
- `feedback_concurrent_phase_swarms.md` memory — the new conductor discipline

Phase 18 claims all 9 dropped items as a single coordinated fix bundle, exercising the new discipline (parallel workstreams + deferral ledger + UI-and-backend-in-same-phase).

---

## The 9 items

See `deferral-ledger.md` for full details. Summary:

| L# | Item | Impact |
|---|---|---|
| L1 | Local Mode toggle in Settings.tsx | Blocks Ouro test; user can't flip the app to Ollama |
| L2 | Credential warnings UI in ToolsMode | Silent breakage when a pulled contribution references an undefined `${VAR}` |
| L3 | OllamaCloudProvider backend variant | Remote Ollama behind nginx not supported (lower priority) |
| L4 | Cache-publish privacy opt-in checkbox | `export_cache_manifest` ships default-OFF with no way to flip it on from UI |
| L5 | Ollama `/api/tags` model list fetch | ModelSelectorWidget can't populate Ollama model names |
| L6 | Schema migration UI | `needs_migration = 1` rows are invisible and un-actionable |
| L7 | `search_hit` demand signal recording | Drills-from-search don't count toward demand propagation |
| L8 | `call_model_audited` cache retrofit | Audited LLM calls still burn tokens on repeat |
| L9 | Folder/circle scoped pause-all DADBEAR | "Pause work pyramids while in personal" not possible |

---

## Workstream split

Four parallel workstreams, each on its own branch, each with the full implementer → verifier → wanderer ceremony. Merges are serialized into main after all four wander clean.

### 18a — Local Mode + Provider Management

**Claims:** L1, L2, L3, L5

**Branch:** `phase-18a-local-mode-providers`

**Scope summary:** Ship the Local Mode toggle in Settings.tsx, with a Settings section for Ollama endpoint + model picker. Add `/api/tags` fetch for the ModelSelectorWidget. Add credential warnings in ToolsMode when pulled contributions reference undefined vars. `OllamaCloudProvider` is a nice-to-have — ship only if scope doesn't bloat.

**Why bundled:** Everything in this workstream is user-facing provider management. All four items share the same UI surface (Settings → Local LLM section + ToolsMode credential warnings) and the same backend plumbing (provider registry + tier_routing contribution supersession). One implementer can hold it coherently.

### 18b — Cache Integrity Retrofit

**Claims:** L7, L8

**Branch:** `phase-18b-cache-integrity`

**Scope summary:** Thread the cache-aware StepContext through `call_model_audited` sites in `evidence_answering.rs` (4 sites) and `chain_dispatch.rs` (1 site). Add `search_hit` demand signal recording path in `routes.rs::handle_search` → `handle_drill` linkage.

**Why bundled:** Both items are backend-only cache/observability work with no UI surface. Both are the kind of "production wiring gap" that wanderers catch in other phases. Both touch tests but not UI. One backend implementer is the right shape.

### 18c — Privacy Opt-in + Pause-all Scoping

**Claims:** L4, L9

**Branch:** `phase-18c-privacy-pause-all`

**Scope summary:** Add the cache-publish privacy opt-in checkbox in ToolsMode's publish preview modal (Phase 10 PublishPreviewModal). Extend `pyramid_pause_dadbear_all` / `pyramid_resume_dadbear_all` IPCs to support `scope: "folder"` and `scope: "circle"`, plus the frontend scope picker in CrossPyramidTimeline's Pause All button.

**Why bundled:** Both items are small — a single checkbox (L4) and a bulk SQL extension + scope picker dropdown (L9). Neither warrants its own workstream, but they share the pattern of "Phase 10/13 shipped the default-most-restrictive path and punted the real UX."

### 18d — Schema Migration UI

**Claims:** L6

**Branch:** `phase-18d-schema-migration-ui`

**Scope summary:** Add `pyramid_list_configs_needing_migration` IPC. Add `pyramid_migrate_config(contribution_id, target_schema_id, note)` IPC that runs an LLM-assisted migration (reverse of the Phase 9 generative flow — given an old YAML + old schema + new schema, produce a new YAML). Add a ToolsMode "Needs Migration" badge/section that surfaces flagged configs.

**Why standalone:** Schema migration is the largest single item — it's effectively a mini-Phase-9 for the reverse direction (schema → config) rather than intent → config. Its LLM flow is non-trivial. Bundling it with other items risks underscoping it.

---

## Execution plan

1. Write `phase-18-plan.md` + `deferral-ledger.md`. ✓
2. Create four branches from main:
   - `phase-18a-local-mode-providers`
   - `phase-18b-cache-integrity`
   - `phase-18c-privacy-pause-all`
   - `phase-18d-schema-migration-ui`
3. Write four workstream prompts (one per branch).
4. Dispatch four implementers in parallel (single message, 4 Agent calls).
5. Wait for all four to finish.
6. Dispatch four verifiers in parallel.
7. Dispatch four wanderers in parallel (no punch list, just "does this work?").
8. Merge via PRs in order 18a → 18b → 18c → 18d. Resolve conflicts on each merge.

### Conflict hot-spots (expected)

- `main.rs` invoke_handler list — 18a, 18c, 18d all register new IPCs
- `src/components/modes/ToolsMode.tsx` — 18a (credential warnings), 18c (privacy checkbox), 18d (migration section)
- `src/components/Settings.tsx` — 18a (Local LLM section) — only one workstream touches this, safe
- `docs/plans/pyramid-folders-model-routing-implementation-log.md` — all four append Phase 18 entries

Merges are serialized specifically because of the main.rs + ToolsMode.tsx overlap. Git's 3-way merge handles non-overlapping additions cleanly; the conductor resolves any true conflicts at merge time.

---

## Acceptance (aggregate)

Phase 18 is complete when:

1. Each of 18a/18b/18c/18d has its own commit sequence (implementer + verifier fix pass if needed + wanderer fix pass if needed) on its branch.
2. Each branch compiles clean (`cargo check --lib`, 3 pre-existing warnings only).
3. Each branch's test count is ≥ prior count + new Phase 18 tests. Same 7 pre-existing failures throughout.
4. `npm run build` clean for branches that touch frontend.
5. All four PRs merged into main in order.
6. Post-merge: `cargo check --lib` clean, `cargo test --lib pyramid` passing, `npm run build` clean.
7. Manual verification paths documented in each workstream's implementation log entry.

### Ouro-readiness check (the reason Phase 18 exists)

After 18a merges, the Ouro test should be runnable: Adam flips Local Mode on in Settings, points the app at a local Ollama instance, builds a pyramid, compares output to a cloud-routed build. If any step requires editing YAML by hand or inserting SQLite rows manually, 18a failed.

---

## Discipline exercised

This phase applies `feedback_concurrent_phase_swarms.md`:

1. **Parallel workstreams split** — 4 instead of 1 big phase. ✓
2. **Deferral ledger maintained** — `deferral-ledger.md` created, all 9 entries claimed. ✓
3. **UI + backend in same phase** — every item that has a user-facing surface ships UI and backend in the same workstream. ✓
4. **Integration-first framing** — each workstream prompt references the source phase's deferred items explicitly, grounding the implementer in the original spec. Required in the workstream prompts below.

Each workstream prompt must:
- Cite the deferral ledger entry number(s) it claims
- Cite the source spec section (e.g., "provider-registry.md §382–395")
- Explicitly scope frontend deliverables as named .tsx components (not "settings UI")
- Define an acceptance criterion that requires visible, clickable functionality (Adam tests by feel)
- Include an `feedback_always_scope_frontend.md` check in the "Mandate" section

If a workstream prompt draft doesn't include all four, it's wrong and rewritten.
