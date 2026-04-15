# Goal 1 Punch List — Make Every Feature Path Work

**Purpose:** Systematic end-to-end test of every feature, fix wiring gaps, get to a known-good baseline before SDFS migration.

**Process:** For each item: test it, record pass/fail, fix if broken, re-test. Mark done when confirmed working.

---

## Priority Key

| Priority | Meaning |
|---|---|
| **P0** | Blocks Goal 2 (passive pyramid maintenance on local hardware) |
| **P1** | Core feature that users/agents will hit immediately |
| **P2** | Important feature that works in isolation but may have integration issues |
| **P3** | Feature exists, needs verification, not blocking |

---

## P0 — Blocks passive local maintenance

### P0-1: Ollama local builds fail immediately

**Root cause identified:** `dispatch_ir_llm` in `chain_dispatch.rs:1198` calls `resolve_ir_model()` which uses hardcoded tier mappings (returns `config.primary_model` e.g. "openrouter/claude-3.5-sonnet") instead of consulting the provider registry's `tier_routing` table. When Ollama is enabled, `local_mode.rs` correctly updates `tier_routing` to point all tiers at `ollama-local` + the local model — but the dispatch path never reads it.

**Failure chain:**
```
Build starts → dispatch_ir_llm → resolve_ir_model returns old OpenRouter model name
→ build_call_provider returns OpenRouterProvider → prepare_headers needs API key
→ API key missing (user removed it when switching to Ollama) → credential error → build halts
```

**Fix:** Replace `resolve_ir_model()` call in `dispatch_ir_llm` with `registry.resolve_tier()` so tier_routing is actually consulted. Alternatively, make `call_model_unified` always check the registry before falling back to hardcoded config.

**Files:** `chain_dispatch.rs:1198-1230` (resolve_ir_model), `llm.rs:338-351` (build_call_provider), `provider.rs:898` (resolve_tier)
**Test:** Enable Ollama local mode, remove OpenRouter key, start code pyramid build. Should use Ollama, not error on missing OpenRouter credential.

---

### P0-2: `use_chain_engine` defaults to `false`

**Problem:** `mod.rs:532` defaults `use_chain_engine: false`. Adam's config overrides this to `true`, but any fresh install or config reset falls back to the legacy pipeline (`build_conversation`, `build_code`, `build_docs` in `build.rs`).

**Fix:** Change default to `true` in `mod.rs:532`. The chain executor is the production path. Legacy exists only for parity testing.

**Files:** `mod.rs:532`
**Test:** Delete `pyramid_config.json`, restart app, confirm chain executor is used for a code build (check `pipeline_steps` rows reference chain step names, not legacy step names).

---

### P0-3: DADBEAR auto-update enables and runs on watched folders

**What to test:** Point DADBEAR at a code repo, edit a file, confirm the recursive function fires: hash change detected → L0 stale check → confirmed stale → node rewritten → L1 check triggered → cascade completes.

**Test steps:**
1. Create a code pyramid from a small test repo
2. Enable auto-update (`pyramid_auto_update_config_set`)
3. Edit a source file
4. Watch stale log (`pyramid_stale_log`) for mutation detection, helper dispatch, and completion
5. Confirm the affected L0 node was superseded and L1+ cascade ran

**Files:** `watcher.rs`, `stale_engine.rs`, `stale_helpers.rs`, `stale_helpers_upper.rs`

---

### P0-4: DADBEAR Pipeline B — new file discovery and auto-ingest

**What to test:** Drop a new source file into a watched folder. DADBEAR Pipeline B should detect it, create an ingest record, and fire a chain build for the new content.

**Test steps:**
1. Have a code pyramid with DADBEAR enabled
2. Add a new `.rs` file to the source directory
3. Confirm `pyramid_ingest_records` gets a new row
4. Confirm `dispatch_pending_ingests` fires and a chain build runs
5. Confirm the new file appears as an L0 node

**Files:** `dadbear_extend.rs`, `folder_ingestion.rs`

---

### P0-5: OpenRouter builds work end-to-end (regression baseline)

**What to test:** Standard build with OpenRouter as provider. This is the known-working path — confirm it still works before fixing Ollama.

**Test steps:**
1. Confirm `pyramid_config.json` has valid `openrouter_api_key` and `use_chain_engine: true`
2. Create a code pyramid from `agent-wire-node` source
3. Start build, confirm progress events fire, nodes are created, apex is reached
4. Confirm cost log has entries

---

## P1 — Core features agents/users hit immediately

### P1-1: Build progress visualization (frontend)

**What to test:** Start a build, confirm the frontend shows live progress (layer-by-layer, step names, node counts).

**Components:** `BuildProgress.tsx`, `PyramidBuildViz.tsx`
**IPC:** `pyramid_build_progress_v2` returns `BuildLayerState`
**Backend:** `event_bus.rs` emits `LayerEvent` variants, tee'd through `tee_build_progress_to_bus`

---

### P1-2: Question pyramid builds work

**What to test:** Ask a question of an existing pyramid ("What are the security concerns?"), confirm decomposition → extraction → answering → synthesis → apex.

**IPC:** `pyramid_question_build(slug, intent)`
**Backend:** `question_build.rs` → `build_runner::run_decomposed_build` → chain executor

---

### P1-3: Vine composition — conversation sessions

**What to test:** Build two conversation pyramids, compose them into a vine, confirm the vine apex synthesizes from both.

**IPC:** `pyramid_vine_build`, `pyramid_vine_bunches`, `pyramid_vine_drill`
**Backend:** `vine_composition.rs`

---

### P1-4: Annotations and FAQ generation

**What to test:** Annotate a node with a `question_context`, confirm FAQ is auto-generated or matched.

**IPC:** Annotation via HTTP `POST /pyramid/:slug/annotate`
**Backend:** `faq.rs::process_annotation` called after every annotation save
**Read:** `pyramid_faq_directory`, `pyramid_faq_category_drill`

---

### P1-5: Provider health monitoring

**What to test:** Provider goes down (stop Ollama), confirm health alert appears. Provider comes back, confirm alert clears.

**IPC:** `pyramid_provider_health`, `pyramid_acknowledge_provider_health`
**Frontend:** `ProviderHealthBanner.tsx`

---

### P1-6: Settings — credential management

**What to test:** Add/remove OpenRouter key, add/remove Ollama URL, confirm provider registry updates correctly and builds route to the right provider.

**IPC:** `pyramid_set_credential`, `pyramid_list_credentials`, `pyramid_list_providers`, `pyramid_test_provider`
**Frontend:** Settings page credential + provider sections

---

## P2 — Important features, may have integration issues

### P2-1: Folder ingestion wizard

**What to test:** Point at a folder, confirm scan returns a checklist of files/subfolders with proposed content types. Start build from checklist.

**Known gap:** Selective inclusion not wired — currently all-or-nothing. Frontend component `AddWorkspace.tsx` is a placeholder.

**IPC:** `pyramid_ingest_folder`, `pyramid_find_claude_code_conversations`

---

### P2-2: Generative config flow (intent → YAML → UI → accept)

**What to test:** Type an intent ("Add Ollama with fast extraction tier"), confirm YAML is generated, rendered in the YAML renderer with widgets, and accepted config syncs to operational tables.

**IPC:** `pyramid_generate_config`, `pyramid_refine_config`, `pyramid_accept_config`
**Frontend:** `YamlConfigRenderer.tsx`, `IntentBar.tsx`

---

### P2-3: Reading modes

**What to test:** For a built pyramid, confirm all six reading modes return meaningful content: memoir, walk, thread, decisions, speaker, search.

**IPC:** `/pyramid/:slug/reading/{mode}` routes
**Backend:** `reading_modes.rs`

---

### P2-4: Chain management — fork, import, publish, propose

**What to test:** Fork a default chain, modify it, assign to a pyramid, build with the variant. Propose a chain improvement, accept it.

**IPC:** `pyramid_chain_import`, chain publish/fork/propose routes
**Backend:** `chain_publish.rs`, `chain_proposal.rs`

---

### P2-5: Recovery operations

**What to test:** Force a build failure (kill mid-build), confirm recovery status shows the stuck state, use recovery operations to rerun/reingest/collapse.

**IPC:** `pyramid_recovery_*` commands
**Routes:** `/pyramid/:slug/recovery/*`
**Backend:** `recovery.rs`

---

### P2-6: Demand generation and evidence triage

**What to test:** Confirm MISSING verdicts from question builds are recorded as demand signals. Confirm demand signals propagate with attenuation.

**IPC:** `/pyramid/:slug/demand-gen` routes
**Backend:** `demand_signal.rs`, `demand_gen.rs`, `triage.rs`

---

### P2-7: Cost observatory

**What to test:** Build a pyramid, confirm cost entries are logged per step with model, tokens, and cost. Confirm cost summary and rollup endpoints return data.

**IPC:** `pyramid_cost_summary`, `pyramid_cost_rollup`
**Backend:** `cost_model.rs`, `pyramid_cost_log` table

---

### P2-8: Breaker trip and recovery

**What to test:** Simulate a branch switch (change many files at once), confirm breaker trips at 75% threshold, confirm three options work: resume, build new pyramid, freeze.

**IPC:** `pyramid_breaker_resume`, `pyramid_breaker_archive_and_rebuild`, `pyramid_auto_update_freeze`
**Backend:** `stale_engine.rs` breaker logic

---

## P3 — Verify, not blocking

### P3-1: Wire publish/pull/discover

**Test:** Publish a pyramid to Wire, pull it on another node, discover it via search.
**Backend:** `wire_publish.rs`, `wire_pull.rs`, `wire_discovery.rs`, `publication.rs`

---

### P3-2: Remote query proxy

**Test:** Query a pyramid on a remote node via `POST /pyramid/remote-query`.
**Backend:** `routes.rs` remote_query_route, tunnel

---

### P3-3: Vocabulary system

**Test:** Build a pyramid, confirm vocabulary terms are extracted and recognizable. Drill into a term.
**IPC:** `pyramid_vocab_*` commands
**Backend:** `vocabulary.rs`

---

### P3-4: Primer generation

**Test:** Generate a primer for a pyramid, confirm it's usable as extraction context.
**Backend:** `primer.rs`
**Routes:** `/pyramid/:slug/primer/json`, `/pyramid/:slug/primer/formatted`

---

### P3-5: Manifest execution (agent cognition steering)

**Test:** Execute a manifest against a pyramid, confirm it steers the agent's next action.
**Backend:** `manifest.rs`
**Routes:** `POST /pyramid/:slug/manifest/exec`

---

### P3-6: Collapse and delta chain management

**Test:** Build, rebuild, rebuild — confirm delta chain accumulates. Collapse it. Confirm history preserved.
**Backend:** `collapse.rs`, `delta.rs`
**Routes:** `/pyramid/:slug/collapse/*`

---

### P3-7: Multi-chain overlay

**Test:** Create an overlay combining two question builds on the same pyramid. Confirm composed view works.
**Backend:** `multi_chain_overlay.rs`
**IPC:** `pyramid_list_question_overlays`, `pyramid_get_composed_view`

---

### P3-8: Crystallization extra passes

**Test:** Trigger crystallization on a pyramid, confirm delta extraction, belief tracing, and gap fill passes run.
**Backend:** `crystallization.rs`
**Routes:** `POST /pyramid/:slug/crystallize`

---

### P3-9: Partner (Dennis) integration

**Test:** Send a message via Partner IPC, confirm brain state updates and response returns.
**IPC:** `partner_send_message`, `partner_session_new`

---

### P3-10: Parity testing (legacy vs chain)

**Test:** Run a parity test, confirm legacy and chain executor produce structurally equivalent pyramids.
**IPC:** `pyramid_parity_run`
**Backend:** `parity.rs`

---

## Summary

| Priority | Count | Status |
|---|---|---|
| P0 | 5 | All must pass before Goal 2 |
| P1 | 6 | Core features, test during P0 work |
| P2 | 8 | Important, test after P0+P1 |
| P3 | 10 | Verify when time permits |
| **Total** | **29** | |

---

**Start with P0-5** (OpenRouter regression baseline) to confirm the happy path works, then **P0-1** (Ollama fix — root cause identified, fix is known), then **P0-2** (default flip), then **P0-3 and P0-4** (DADBEAR end-to-end).

**Last updated:** 2026-04-11
