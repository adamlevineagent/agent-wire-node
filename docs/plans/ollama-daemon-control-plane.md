# Ollama Daemon Control Plane

**Date:** 2026-04-12
**Scope:** Replace the Phase 18a Local LLM toggle with a full daemon control surface — model portfolio, hot-swap, context/concurrency control, pull/delete, config versioning, experimental territory markers.
**Framing:** This is the owner's interface to Layer 0 (the mechanical daemon) per `wire-node-steward-daemon.md`. Every surface built here is something the sentinel/steward will eventually read and write through the same contribution pattern.
**Audit:** Stage 1 informed audit applied 2026-04-12. Stage 2 discovery audit applied 2026-04-12. All critical/major findings corrected inline.

---

## Current State (what's broken / limited)

1. **Model dropdown disabled when enabled.** The `<select>` has `disabled={localMode.status?.enabled || ...}` — once local mode is on, the user can't change models without disable → change → re-enable (full tier-routing round-trip).

2. **No auto-probe.** The model list only populates after manually clicking "Test connection." If the user opens Settings with local mode already enabled, the dropdown shows models from the status refresh, but if disabled it shows nothing.

3. **No model details.** The dropdown shows bare model names. No size, quantization, parameter count, context window, or architecture info.

4. **No context control.** Context window is auto-detected and frozen. No way to see it prominently or override it (some models support more than their default via `num_ctx`).

5. **No concurrency control.** Hardcoded to 1 with a warning banner. No way for users with capable hardware to increase it.

6. **No model management.** Can't pull new models or delete old ones from the UI. Requires terminal `ollama pull`.

7. **Config history invisible.** The tier_routing and build_strategy contributions form a supersession chain, but the UI hides this entirely. No rollback, no diff, no "what changed."

8. **No experimental territory.** No way to mark which config dimensions are locked vs optimizable for future steward use.

---

## Architecture Decisions

### AD-1: Hot-swap via dedicated IPC, not disable/enable cycle
A new `pyramid_switch_local_model` command that:
- Finds the currently-active tier_routing contribution via `load_active_config_contribution(conn, "tier_routing", None)` — NOT from the state row's `restore_from_contribution_id` (that points at the PRE-local-mode contribution for the disable path)
- Probes `/api/show` for the new model's context window
- Supersedes the active tier_routing contribution with the new model
- Read-modify-writes the `pyramid_local_mode_state` row, updating ONLY `ollama_model` and `detected_context_limit` while preserving all other fields including `restore_from_contribution_id` and `restore_build_strategy_contribution_id`
- Refreshes the in-memory ProviderRegistry via `registry.load_from_db(conn)`
- Calls `rebuild_cascade_from_registry` after dropping the writer lock (required for builds to route to the new model)
- Does NOT touch `build_strategy` or the restore columns
- Returns updated `LocalModeStatus`

**Active build guard:** Hot-swap, disable, and enable all check the `active_build` map before proceeding. If any build is in progress, the IPC returns an error: "Cannot change model routing while a build is in progress — wait for it to complete or cancel it." This prevents half-and-half builds where early layers use one model and later layers use another. The guard applies to `pyramid_switch_local_model`, `pyramid_enable_local_mode`, and `pyramid_disable_local_mode`.

This keeps the enable/disable flow intact for the full toggle, and adds a lightweight model-switch path that only touches what changes.

### AD-2: Rich model data from existing Ollama APIs
Ollama's `/api/tags` returns more than just `name` — it includes `size` (bytes), `details` (family, families, parameter_size, quantization_level, format), and `modified_at`. We already call `/api/show` for context detection; it also returns `model_info` with architecture and parameter counts.

New struct: `OllamaModelInfo` carrying name, size_bytes, family, families, parameter_size, quantization_level, context_window, architecture. Populated by combining `/api/tags` entry + `/api/show` per model.

**IPC contract preservation:** Phase 2 adds a NEW field `available_model_details: Vec<OllamaModelInfo>` alongside the existing `available_models: Vec<String>`. The string list stays for backward compatibility and Phase 1 consumers. Phase 2 frontend reads `available_model_details` for the card UI.

**Cost concern:** `/api/show` per model is a POST that may be slow with many models. Solution: fetch `/api/tags` for the list (fast, single call), then lazy-load `/api/show` details only for the selected model or on explicit "show details" click. Don't block the model list on N serial show calls.

### AD-3: Pull progress via existing BuildEventBus
Ollama's `POST /api/pull` returns chunked JSON progress. We already have `BuildEventBus` with `broadcast::Sender<TaggedBuildEvent>`. Add a new `TaggedKind::OllamaPull` variant carrying model name, status string, downloaded/total bytes.

**Event envelope:** The outer `TaggedBuildEvent.slug` is set to `"__ollama__"` (a reserved non-pyramid slug) for pull events. Do NOT use empty string `""` — empty-slug events pollute the `useCrossPyramidTimeline` hook's `bySlug` Map, creating a phantom timeline entry for a non-existent pyramid. The serde tag serializes as `"ollama_pull"` via the existing `#[serde(tag = "type", rename_all = "snake_case")]` attribute.

**Frontend safety:** The frontend event handler must filter out `__ollama__` slug events from pyramid timeline rendering, and route them to the Ollama pull progress UI instead. Add a default/fallback case for unknown event types if not present.

**Cancellation:** New `pyramid_ollama_cancel_pull` IPC that sets an `AtomicBool` flag. The pull chunk-read loop checks the flag between chunks and drops the reqwest response stream if set. The frontend shows a "Cancel" button during pull.

**Concurrent pull guard:** A `Mutex<Option<String>>` (or `AtomicBool`) prevents multiple simultaneous pulls. The IPC refuses a second pull while one is active. The frontend disables the pull button while a pull is in progress.

### AD-4: Context override stored in local_mode_state
New nullable column `context_override INTEGER` on `pyramid_local_mode_state`. When set, the tier_routing contribution uses this value instead of the auto-detected one. The UI shows both: "Detected: 128K" with an override input. Setting the override supersedes the active tier_routing contribution (same as hot-swap).

**Precedence rule:** `context_override` always wins over `detected_context_limit` when set. Model switching never clears the override — only the explicit "Reset to auto-detect" action clears it. Model switch updates `detected_context_limit` in the state row but leaves `context_override` unchanged.

**Ollama `num_ctx` pass-through (CRITICAL):** The `context_limit` in tier_routing only controls Wire Node's dehydration/truncation — it does NOT tell Ollama to allocate more context. Without passing `num_ctx` to Ollama, the override is a lie: Wire Node sends more tokens, Ollama silently truncates. When `context_override` is set (or even when auto-detected context differs from Ollama's default), the enable/switch flow must store `num_ctx` in the `ollama-local` provider row's `config_json`. The LLM call layer injects `{"options": {"num_ctx": N}}` into Ollama request bodies when the provider is `ollama-local`. This ensures Ollama actually allocates the context window Wire Node expects.

**Persistence rule:** Overrides survive disable/enable cycles. The disable flow preserves override values in the state row. Re-enable uses the override if set. The `LocalModeStateRow` struct, `save_local_mode_state`, and `load_local_mode_state` all include the override columns. All code paths that construct a `LocalModeStateRow` (enable, disable, switch) must read-modify-write to preserve fields they don't intend to change.

### AD-5: Concurrency override — dual-axis (build_strategy + provider pool)
New nullable column `concurrency_override INTEGER` on `pyramid_local_mode_state`. The UI shows an incrementor (1-12) with warning text: "Most home users should leave this on 1 to prevent issues."

**Dual concurrency axes (CRITICAL):** There are TWO concurrency controls that must move in lockstep:
1. `build_strategy.concurrency` — controls how many chain steps run in parallel (the `for_each` / `join_set` cap in the chain executor)
2. `dispatch_policy.provider_pools.ollama-local.concurrency` — controls how many HTTP requests hit Ollama simultaneously (the `ProviderPools` semaphore)

Setting build_strategy concurrency to 4 without updating the provider pool gives you 4 parallel steps all serializing through a 1-permit semaphore. Useless. The concurrency override IPC MUST update BOTH: supersede the `build_strategy` contribution AND supersede the `dispatch_policy` contribution's `provider_pools` section to match. Both supersessions happen atomically in the same writer lock.

Additionally, the static `LOCAL_PROVIDER_SEMAPHORE` in `llm.rs` (line 51, `Semaphore::new(1)`) must either be removed in favor of the `ProviderPools` semaphore or dynamically resized when concurrency changes.

**Warning text:** "Higher concurrency increases memory pressure on your GPU and may slow individual requests. Ollama queues concurrent requests against the same loaded model — it does not load multiple instances."

**Non-interaction invariant:** Model switching (AD-1) never touches `build_strategy` or `dispatch_policy`. Concurrency is an independent axis.

### AD-8: Stale engine deferral during builds + provider_pools wiring

**The contention problem:** When local mode is enabled, ALL LLM calls (build steps AND stale engine helpers) serialize through `LOCAL_PROVIDER_SEMAPHORE(1)` in `llm.rs:51`. A layer 1 build with 800 nodes spawns 50+ forEach workers, all queuing behind 1 HTTP slot. Meanwhile, the stale engine runs concurrently with up to 8 helper tasks (`concurrent_helpers` semaphore), each also competing for the same single slot. Result: 58+ tasks blocked on 1 permit. Progressive slowdown is inevitable.

**Three-part systemic fix:**

**Part 1: Wire `provider_pools` for Ollama in Phase 1 (not Phase 3).**
The `provider_pools` system already exists in `provider_pools.rs` with per-provider semaphores. It's not used for Ollama because local mode doesn't create a `dispatch_policy` contribution with pool configs. The `call_model_unified_and_ctx` path (llm.rs:702-747) checks for provider_pools first; only when absent does it fall back to `LOCAL_PROVIDER_SEMAPHORE`. Three separate LLM dispatch paths exist (main at line 876, registry at line 2053, direct at line 2586) — all check provider_pools and fall back identically.

Fix: When `commit_enable_local_mode` runs, also create a `dispatch_policy` contribution that includes `provider_pools.ollama-local.concurrency = 1`. The pool key MUST match `OLLAMA_LOCAL_PROVIDER_ID` (`"ollama-local"`) exactly — a mismatch causes silent fallback to `LOCAL_PROVIDER_SEMAPHORE` via `.ok()` on the acquire failure.

**ConfigSynced listener (CRITICAL prerequisite):** The in-memory `LlmConfig.provider_pools` is only set at boot (`main.rs:10282`). There is NO runtime listener that rebuilds pools when `dispatch_policy` changes. Creating the contribution writes YAML to the operational table but the live `LlmConfig.provider_pools` stays `None`. The entire pool wiring is dead on arrival without this.

Fix: Build a `ConfigSynced` bus subscriber (in `main.rs` setup or a dedicated module) that on `schema_type == "dispatch_policy"`: reads the new YAML from the operational table, constructs `ProviderPools::new(&policy)`, and writes both `dispatch_policy` and `provider_pools` onto the live `pyramid_state.config` write lock. This listener must exist before the enable flow can activate pools.

**No prior dispatch_policy:** Most users will have no existing `dispatch_policy` contribution (fresh install). The enable path must use `create_config_contribution` (not `supersede_config_contribution`) when no active dispatch_policy exists. The disable path, when `restore_dispatch_policy_contribution_id` is `None`, must find the now-active dispatch_policy contribution and supersede it with an empty/default policy (or mark it inactive), restoring the "no policy" state.

**Part 2: Default `defer_maintenance_during_build = true` for local mode.**
The stale engine already has a `defer_maintenance_during_build` flag in `dispatch_policy.build_coordination` (defaults to `false`). When local mode is enabled, the dispatch_policy contribution created in Part 1 also sets `build_coordination.defer_maintenance_during_build = true`.

**Hot-reload requirement:** The `defer_maintenance_during_build` bool is baked into the stale engine at construction time (`stale_engine.rs:113`) and never re-read. Changing the dispatch_policy at runtime does NOT update already-running stale engines. Fix: change `defer_maintenance_during_build` from a plain `bool` to an `Arc<AtomicBool>` on the engine. The `ConfigSynced` listener (from Part 1) also updates this atomic when the dispatch_policy changes. This is cheaper than reconstructing the engine.

**Part 3: Extend deferral to debounce timers.**
The current deferral only covers the 60s poll loop. Debounce timers spawned by `notify_mutation` (stale_engine.rs:449-484) fire regardless of active builds.

**Parameter threading requirement:** `drain_and_dispatch` is a free function that currently has no `active_build` or `defer_maintenance` parameters. `start_timer` (which spawns debounce tasks that call `drain_and_dispatch`) does not clone these handles. Fix: add `active_build: Arc<RwLock<HashMap<...>>>` and `defer_maintenance: Arc<AtomicBool>` parameters to `drain_and_dispatch`. Update ALL 6 call sites:
1. Poll loop — `stale_engine.rs:288`
2. `start_timer` debounce — `stale_engine.rs:534`
3. `run_manual` method — `stale_engine.rs:623`
4. `pyramid_force_stale_check` IPC — `main.rs:7554`
5. `pyramid_auto_update_l0_sweep` follow-up — `main.rs:7620`
6. HTTP route `stale_drain` — `routes.rs:5886`

Sites 4-6 are external callers that extract engine fields under a brief lock. The `defer_maintenance` `Arc<AtomicBool>` must be exposed as a public field on the engine struct (or stored on `PyramidState` directly) so external callers can access it without holding the engine lock — same pattern as `active_build` on `PyramidState`.

At the top of `drain_and_dispatch`, check: if `defer_maintenance.load(Ordering::Relaxed) && !active_build.read().await.is_empty()` → return early (reschedule). Mutations accumulate during a build and drain in a single batch after the build completes.

**In-flight stale tasks (known limitation):** Once `drain_and_dispatch` is entered and helpers are spawned, they cannot be cancelled if a build starts mid-flight. Each in-flight helper holds the provider pool permit for one LLM call (potentially 30-120s). This is acceptable for Phase 1 — stale helpers are short-lived and the window is small.

**Why this is systemic, not a band-aid:** The provider_pools path replaces the global bottleneck with a per-provider bottleneck that can be independently tuned (Phase 3's concurrency override). The stale engine deferral eliminates contention at the source — stale checks don't compete with builds for inference time, they accumulate and batch-drain afterward. Both changes are contribution-driven (dispatch_policy), so the steward can eventually optimize them.

### AD-6: Experimental territory as contribution (Phase 6 only)
Per-dimension lock/experimental/experimental-within-bounds stored as a policy contribution (`schema_type: "experimental_territory"`). The UI renders toggles per dimension. This is a new contribution type — it doesn't affect current daemon behavior, it's metadata for the future steward. Building the UI and persistence now means the steward has territory to read when it arrives.

**Dispatcher requirement:** Phase 6 MUST add an `"experimental_territory"` branch to the `sync_config_to_operational` dispatcher. It can be a no-op (no operational table needed), but it must exist to prevent `UnknownSchemaType` errors on every supersession.

### AD-7: Config versioning surfaces existing data
The supersession chain already exists in `pyramid_config_contributions`. The UI just needs to query it — no new backend tables. New IPC: `pyramid_get_config_history` wrapping the existing `load_config_version_history` function, adding a `limit` parameter (the existing function walks the entire chain with no limit). Returns entries most-recent-first.

**Rollback safety:** Rollback of tier_routing or build_strategy while local mode is enabled is BLOCKED with a warning: "Disable local mode before rolling back tier routing." This prevents state splits where `pyramid_local_mode_state` shows `enabled = true` but tier routing points at OpenRouter.

---

## Phase Plan

### Phase 0: Infrastructure Prerequisites

**Goal:** Shared components and fixes needed before Phases 1-6.

**Shared AccordionSection component:**
- New `<AccordionSection>` component used by all Phases 2-6
- Includes `aria-expanded`, `aria-controls`, `role="region"`, keyboard navigation (Enter/Space to toggle)
- Consistent expand/collapse animation matching existing patterns (FAQDirectory, ActivityFeed)
- Replace the current ad-hoc expand/collapse patterns where feasible

**Shared reqwest client:**
- Move `HTTP_CLIENT: LazyLock<reqwest::Client>` from `llm.rs` to a shared module (or make it `pub(crate)`)
- All Ollama API calls (`fetch_ollama_models`, `detect_ollama_context_window`, pull, delete) use this shared client instead of `reqwest::Client::new()` per call
- Eliminates redundant TCP connections and TLS negotiations

**SSRF warning on base_url:**
- When `base_url` is not `localhost` / `127.0.0.1` / `::1`, show a prominent warning in the UI: "You are pointing at a remote server. All prompts and build data will be sent there. Ollama does not use authentication."
- No hard block (legitimate use case: Ollama on another machine in the LAN), but make the risk visible

**Accessibility baseline:**
- Link standalone `<label>` elements to inputs via `htmlFor`/`id` in the Ollama section
- Add `aria-label` to the checkbox toggle

**Files:**
- `src/components/AccordionSection.tsx` — new shared component
- `src-tauri/src/pyramid/http_client.rs` (or add to existing shared module) — shared client
- `src/components/Settings.tsx` — SSRF warning, accessibility fixes
- `src/styles/dashboard.css` — accordion styles

---

### Phase 1: Unblock + Hot-Swap (immediate fix)

**Goal:** Fix the broken dropdown, auto-probe on mount, add model hot-swap.

**DB migration:**
- Add `context_override INTEGER` and `concurrency_override INTEGER` nullable columns to `pyramid_local_mode_state` via `ALTER TABLE ... ADD COLUMN` in `init_pyramid_db`. These columns are unused in Phase 1 but prevent forward-compatibility issues when Phase 3 code tries to read them. All existing code paths that write `LocalModeStateRow` must carry these new fields (defaulting to `None`).

**Rust backend — stale engine deferral + provider_pools (AD-8):**
- **ConfigSynced listener (prerequisite, build first):** Add a bus subscriber in `main.rs` setup that on `ConfigSynced { schema_type: "dispatch_policy" }`: reads the new operational YAML, constructs `ProviderPools::new(&policy)`, writes `dispatch_policy` + `provider_pools` onto the live `LlmConfig` write lock, and updates the stale engine's `defer_maintenance` `Arc<AtomicBool>`. Without this, all pool wiring is dead on arrival.
- **Stale engine hot-reload:** Change `defer_maintenance_during_build` from plain `bool` to `Arc<AtomicBool>` on the stale engine struct. The ConfigSynced listener updates it. Pass it (+ `active_build` clone) through `start_timer` into `drain_and_dispatch`.
- **`drain_and_dispatch` parameter threading:** Add `active_build: Arc<RwLock<HashMap<...>>>` and `defer_maintenance: Arc<AtomicBool>` parameters. Add early-return check at top. Update all 6 call sites (poll loop, start_timer, run_manual, pyramid_force_stale_check IPC, l0_sweep follow-up, stale_drain HTTP route). Expose `defer_maintenance` as public field on engine struct for external callers. Note: dispatch_policy handling in commit_disable is net-new logic, not modification of existing restore scaffolding.
- In `commit_enable_local_mode`: create a `dispatch_policy` contribution with `provider_pools.ollama-local.concurrency = 1` and `build_coordination.defer_maintenance_during_build = true`. Use `create_config_contribution` when no active dispatch_policy exists (most fresh installs), `supersede_config_contribution` when one does.
- Save the prior `dispatch_policy` contribution_id in `pyramid_local_mode_state` (new column `restore_dispatch_policy_contribution_id`). When prior is `None`, store `None`.
- `commit_disable_local_mode`: when `restore_dispatch_policy_contribution_id` is `Some`, restore it (same pattern as tier_routing/build_strategy). When `None`, supersede the current dispatch_policy with an empty/default policy to restore the "no policy" state.

**Rust backend — state row + hot-swap:**
- Update `LocalModeStateRow` struct and `save_local_mode_state`/`load_local_mode_state` to include `context_override: Option<i64>`, `concurrency_override: Option<i64>`, and `restore_dispatch_policy_contribution_id: Option<String>` (all passed through as None in Phase 1 except dispatch_policy restore which is populated by enable).
- New `pyramid_switch_local_model(model: String)` IPC in `main.rs` following the split-phase pattern:
  1. **Async prepare:** validate local mode is enabled (read state row under reader lock, drop lock), probe `/api/show` for context window on new model
  2. **Sync commit (writer lock):** check `active_build` map — refuse if any build is in progress. Find active tier_routing via `load_active_config_contribution(conn, "tier_routing", None)`. Build new tier_routing YAML with new model + context (respecting `context_override` from state row if set). Also update `config_json` on the `ollama-local` provider row with `{"num_ctx": N}` so Ollama actually allocates the context window. Supersede via `supersede_config_contribution`. Sync to operational via `sync_config_to_operational`. Read-modify-write state row (update `ollama_model` + `detected_context_limit`, preserve everything else). Refresh registry via `registry.load_from_db(conn)`. Drop writer lock.
  3. **Async follow-up:** `rebuild_cascade_from_registry(&state).await` to update live LlmConfig.
- In `local_mode.rs`: new `prepare_switch_local_model` + `commit_switch_local_model` fns mirroring the enable pattern.

**Frontend:**
- `useLocalMode.ts`: add `switchModel(model: string)` method wrapping the new IPC
- `Settings.tsx` — single atomic change to the `<select>` element:
  - Change disabled condition to `disabled={localMode.loading || availableModels.length === 0}` (remove `localMode.status?.enabled`)
  - Replace onChange with a handler that calls `switchModel(value)` when `localMode.status?.enabled`, or `setLocalModelChoice(value)` when disabled. These MUST change together as one atomic edit.
  - Auto-probe fires AFTER the initial status refresh completes (not on bare mount). Condition: `localMode.status !== null && !localMode.status.enabled && localMode.status.base_url` is set (not just the hardcoded default). This prevents probe errors for users who never configured Ollama.
  - Re-enable the "Test connection" / "Refresh models" button when local mode is on (remove `localMode.status?.enabled === true` from its disabled condition). Rename to "Refresh models" when enabled.

**Files:**
- `src-tauri/src/pyramid/db.rs` — add columns (context_override, concurrency_override, restore_dispatch_policy_contribution_id), update LocalModeStateRow struct + save/load fns
- `src-tauri/src/main.rs` — new `pyramid_switch_local_model` command handler
- `src-tauri/src/pyramid/local_mode.rs` — new `prepare_switch_local_model` + `commit_switch_local_model` fns, update all `LocalModeStateRow` construction sites, dispatch_policy creation in enable/disable flows
- `src-tauri/src/pyramid/stale_engine.rs` — change `defer_maintenance_during_build` to `Arc<AtomicBool>`, add params to `drain_and_dispatch` + `start_timer`, add early-return check
- `src-tauri/src/main.rs` (also) — add ConfigSynced bus subscriber for dispatch_policy → rebuild provider_pools + update defer_maintenance atomic
- `src/hooks/useLocalMode.ts` — add `switchModel` to hook
- `src/components/Settings.tsx` — fix disabled condition, auto-probe, wire onChange, rename button

**Acceptance:**
- User can change models while local mode is enabled, without toggling off
- Opening Settings with a previously-configured base_url auto-populates the model list
- Switching models supersedes the tier_routing contribution (visible in DB)
- Context window re-detected per model on switch
- `rebuild_cascade_from_registry` called — builds route to new model immediately
- `num_ctx` written to provider config_json — Ollama allocates the correct context window
- Disable/re-enable cycle still works correctly (restore columns preserved, dispatch_policy restored)
- Override columns exist in DB (nullable, unused in Phase 1)
- Switch/enable/disable blocked while a build is in progress (clear error message)
- Stale engine defers during active builds (both poll loop AND debounce timers)
- Ollama calls route through `provider_pools` semaphore, not `LOCAL_PROVIDER_SEMAPHORE`
- Stale checks batch-drain after build completes

---

### Phase 2: Model Portfolio with Details

**Goal:** Replace bare model names with rich info cards showing size, quant, context, architecture.

**Rust backend:**
- New struct `OllamaModelInfo` in `local_mode.rs`:
  ```
  name: String
  size_bytes: u64
  family: Option<String>
  families: Option<Vec<String>>
  parameter_size: Option<String>    // e.g. "27B"
  quantization_level: Option<String> // e.g. "Q4_K_M"
  context_window: Option<usize>
  architecture: Option<String>
  modified_at: Option<String>
  ```
- New `fetch_ollama_models_rich` returning `Vec<OllamaModelInfo>` extracting all available fields from `/api/tags` response. Existing `fetch_ollama_models` delegates to this and maps to `Vec<String>` — all existing callsites (probe, enable) continue working unchanged.
- New `pyramid_get_model_details(model: String)` IPC that calls `/api/show` and returns the full `OllamaModelInfo` with context_window and architecture filled in
- Add `available_model_details: Vec<OllamaModelInfo>` as a NEW field on `OllamaProbeResult` and `LocalModeStatus`, alongside the existing `available_models: Vec<String>`. No breaking IPC contract change.

**Frontend:**
- New `OllamaModelCard` component showing: name, parameter size, quantization, size on disk (human-readable), context window, "Active" badge if current
- Replace `<select>` with a scrollable model list of `OllamaModelCard` components inside a collapsible "Models" accordion section
- Click a card to select it (fires switchModel if enabled, or sets localModelChoice if disabled)
- Lazy-load context_window via `pyramid_get_model_details` when a card is focused/expanded (don't block list render on N serial calls)
- Show "Currently active" indicator on the running model

**Files:**
- `src-tauri/src/pyramid/local_mode.rs` — `OllamaModelInfo`, `fetch_ollama_models_rich`, model details IPC
- `src-tauri/src/main.rs` — register `pyramid_get_model_details` command
- `src/hooks/useLocalMode.ts` — update types (add `available_model_details` field)
- `src/components/Settings.tsx` — replace select with model card list in accordion
- `src/components/OllamaModelCard.tsx` — new component (or inline in Settings if small enough)

**Acceptance:**
- Each model shows name, parameter size, quantization, disk size
- Active model visually distinguished
- Context window shows on selected/expanded card
- Clicking a card switches the active model (when enabled)
- Existing `available_models: Vec<String>` still works for any consumers

---

### Phase 3: Context + Concurrency Control

**Goal:** Let users see and override context limits and build concurrency.

**DB migration:** None — columns already added in Phase 1.

**Rust backend — context override:**
- New `pyramid_set_context_override(limit: Option<usize>)` IPC:
  - When `Some(n)`: read-modify-write state row (set context_override), supersede tier_routing with the override value, rebuild cascade
  - When `None`: clear context_override, supersede tier_routing with auto-detected value from state row, rebuild cascade
  - Returns `LocalModeStatus`
- Extend `LocalModeStatus` with `context_override: Option<usize>` and `concurrency_override: Option<usize>`

**Rust backend — concurrency override:**
- New `pyramid_set_concurrency_override(concurrency: Option<usize>)` IPC:
  - When `Some(n)`: store override (clamp 1-12). In a single writer lock:
    1. Supersede `build_strategy` contribution with new concurrency cap
    2. Supersede `dispatch_policy` contribution's `provider_pools` section to set `ollama-local.concurrency = n`
    3. Trigger `ProviderPools` reload so the runtime semaphore matches
  - When `None`: clear override, restore both to defaults (concurrency 1)
  - The static `LOCAL_PROVIDER_SEMAPHORE` in `llm.rs` must be removed or bypassed in favor of the `ProviderPools` semaphore for Ollama calls
  - Also add a `MAX_CONCURRENCY = 12` constant used by BOTH the IPC clamp AND the chain executor's `read_build_strategy_concurrency` (defense in depth against direct YAML edits)
  - Returns `LocalModeStatus`

**Frontend:**
- Context section (collapsible accordion):
  - Shows "Detected: {N}K tokens" in secondary text
  - Input field for override, placeholder shows detected value
  - "Reset to auto-detect" button when override is set
  - Warning when override > detected ("Model may not support this context length — use at your own risk")
- Concurrency section (collapsible accordion):
  - Incrementor: 1 through 12
  - Warning text: "Most home users should leave this on 1 to prevent issues."
  - Dynamic sub-warning when >1: "Concurrency {n} requires sufficient VRAM for {n} simultaneous model instances."
  - "Reset to default (1)" when override is set
- Replace the static "Local mode sets concurrency to 1" warning banner with the dynamic version

**Files:**
- `src-tauri/src/pyramid/local_mode.rs` — override logic, new IPCs, status extension
- `src-tauri/src/main.rs` — register new commands
- `src/hooks/useLocalMode.ts` — add setContextOverride, setConcurrencyOverride methods + status fields
- `src/components/Settings.tsx` — context and concurrency accordion sections

**Acceptance:**
- Context override supersedes tier_routing contribution with custom value
- Clearing override restores auto-detected value
- Concurrency override supersedes build_strategy contribution (clamped 1-12)
- Both persist across app restart AND across disable/enable cycles
- Model switch respects active context override (override wins, detected updates silently)

---

### Phase 4: Model Pull + Delete from UI

**Goal:** Pull new Ollama models and delete existing ones without leaving the app.

**Rust backend — pull:**
- New `pyramid_ollama_pull_model(model: String)` IPC:
  - Concurrent pull guard: `Mutex<Option<String>>` in app state. Refuse if another pull is active.
  - Calls `POST {native_root}/api/pull` with `{"model": "..."}` and streaming response
  - Ollama returns chunked JSON in phases:
    1. `{"status": "pulling manifest"}` (no bytes)
    2. `{"status": "pulling digestname", "digest": "...", "total": N, "completed": N}` (progress)
    3. `{"status": "verifying sha256 digest"}` (no bytes)
    4. `{"status": "writing manifest"}` (no bytes)
    5. `{"status": "success"}` (complete)
  - Relay chunks as `TaggedBuildEvent` with slug `""` and `TaggedKind::OllamaPull` variant:
    ```
    OllamaPull {
        model: String,
        status: String,
        completed_bytes: Option<u64>,
        total_bytes: Option<u64>,
    }
    ```
  - Cancellation: check `AtomicBool` flag between chunks. New `pyramid_ollama_cancel_pull` IPC sets the flag; pull loop drops the reqwest response stream.
  - On completion, clear the pull guard and auto-refresh the model list
  - Returns success/error
- Note: `DELETE /api/delete` with JSON body is non-standard HTTP but matches Ollama's API and reqwest handles it correctly.

**Rust backend — delete:**
- New `pyramid_ollama_delete_model(model: String)` IPC:
  - Guard: refuse if model == currently active model in local_mode_state
  - Warning: deleting a model while a build is in progress may cause the build to fail (document as known limitation — checking in-flight build state adds disproportionate complexity)
  - Calls `DELETE {native_root}/api/delete` with `{"model": "..."}`
  - Returns success/error + refreshed model list

**Frontend:**
- Pull section (collapsible accordion):
  - Text input for model name (e.g. "llama3.2:latest")
  - Link to https://ollama.com/library opens browser for browsing (no in-app browsing — Ollama has no stable registry/search API)
  - "Pull Model" button (disabled while pull in progress)
  - "Cancel" button (visible during pull)
  - Progress bar fed by `OllamaPull` events from the event bus
  - Status text showing current pull phase (during non-progress phases like "verifying sha256 digest", show status string instead of progress bar)
  - Model list auto-refreshes on completion
- Delete:
  - Delete icon/button on each model card (Phase 2)
  - Confirmation dialog: "Delete {model}? This removes the model files from Ollama."
  - Disabled on the currently-active model
  - Model list auto-refreshes on completion
- Frontend event handler must have a default/fallback case for unknown TaggedKind types (add if not present)

**Files:**
- `src-tauri/src/pyramid/local_mode.rs` — pull_model (streaming + cancel), delete_model functions
- `src-tauri/src/pyramid/event_bus.rs` — add `TaggedKind::OllamaPull` variant
- `src-tauri/src/main.rs` — register new commands, add pull guard + cancel flag to app state
- `src/hooks/useLocalMode.ts` — add pullModel, cancelPull, deleteModel methods
- `src/components/Settings.tsx` — pull UI with progress + cancel, delete buttons on cards

**Acceptance:**
- User can type a model name and pull it with visible progress
- Progress bar shows downloaded/total bytes during download phases
- Status text shows phase name during non-progress phases
- Cancel button stops the download
- Can't start a second pull while one is active
- Model list refreshes automatically when pull completes
- User can delete non-active models with confirmation
- Can't delete the currently-active model

---

### Phase 5: Config History + Rollback

**Goal:** Surface the supersession chain so users can see what changed and roll back.

**Rust backend:**
- New `pyramid_get_config_history(schema_type: String, limit: usize)` IPC:
  - Do NOT use the existing `load_config_version_history` function (which walks the full chain via O(N) individual queries, returns oldest-first). Instead, use a single SQL query: `SELECT * FROM pyramid_config_contributions WHERE schema_type = ? AND slug IS NULL ORDER BY created_at DESC LIMIT ?`. This is O(1) regardless of chain length and avoids the N+1 query problem that makes the history view freeze on long chains.
  - Alternatively, use a recursive CTE with LIMIT for chain-walk ordering (SQLite handles this efficiently)
  - Returns `Vec<ConfigHistoryEntry>`:
    ```
    contribution_id: String
    yaml_content: String
    triggering_note: Option<String>
    created_by: Option<String>
    created_at: String
    superseded_by_id: Option<String>
    is_active: bool
    ```
  - Note: field names match existing DB columns (`created_by`, `superseded_by_id`, `schema_type`)
- New `pyramid_rollback_config(contribution_id: String)` IPC:
  - **Guard:** if local mode is enabled, REFUSE rollback of tier_routing or build_strategy with error: "Disable local mode before rolling back tier routing configuration." This prevents state splits.
  - **Schema validation:** before creating the rollback contribution, validate the target YAML parses correctly for its schema_type. If the schema has evolved since the target version (Phase 9 migration system), refuse with error: "Cannot roll back — configuration schema has changed since this version."
  - Loads the target contribution's yaml_content
  - Creates a new contribution superseding the current active one with `triggering_note: "manual rollback to {contribution_id}"`
  - Syncs to operational
  - Rebuilds cascade
  - Returns updated status

**Frontend:**
- New collapsible "Configuration History" accordion section in the Ollama panel
- Timeline view showing:
  - Each config change with timestamp, triggering note, created_by
  - "Active" badge on current
  - "Rollback to this" button on each historical entry (disabled when local mode is enabled, with tooltip explaining why)
  - Diff view (expandable) showing what changed vs the next version
- Rollback triggers confirmation: "Roll back tier routing to the version from {date}?"

**Files:**
- `src-tauri/src/pyramid/config_contributions.rs` — history query wrapper function
- `src-tauri/src/pyramid/local_mode.rs` — rollback function with local-mode guard
- `src-tauri/src/main.rs` — register commands
- `src/hooks/useLocalMode.ts` — add getConfigHistory, rollbackConfig methods
- `src/components/Settings.tsx` — config history timeline accordion

**Acceptance:**
- User can see full history of tier_routing changes with timestamps and notes
- Each entry shows what changed (triggering note)
- Rollback creates a new supersession (not destructive, adds to chain)
- Rollback blocked when local mode is enabled (clear error message)
- Post-rollback (when disabled), the active config reflects the rolled-back version

---

### Phase 6: Experimental Territory

**Goal:** Let users mark which config dimensions the future steward can optimize.

**Rust backend:**
- New contribution type: `experimental_territory`
- Add `"experimental_territory"` branch to `sync_config_to_operational` dispatcher — no-op (no operational table needed), but required to prevent `UnknownSchemaType` errors
- Schema:
  ```yaml
  schema_type: experimental_territory
  dimensions:
    model_selection:
      status: experimental  # locked | experimental | experimental_within_bounds
      bounds: null           # optional: constraints when status is experimental_within_bounds
    context_limit:
      status: locked
    concurrency:
      status: experimental_within_bounds
      bounds:
        min: 1
        max: 2
    # Future dimensions (compute market):
    # pricing: ...
    # job_acceptance: ...
    # scheduling: ...
  ```
- New `pyramid_get_experimental_territory` / `pyramid_set_experimental_territory` IPCs
- Territory persisted as a config contribution (supersedable, versionable, same chain)

**Frontend:**
- New "Optimization Territory" collapsible accordion section in Ollama panel
- Per-dimension row:
  - Dimension name + description
  - Three-state toggle: Locked / Experimental / Bounded
  - When "Bounded": shows min/max inputs
  - Visual indicator: lock icon (locked), open icon (experimental), constrained icon (bounded)
- Explanatory text: "When the steward arrives, it will only optimize dimensions you've marked as experimental. Locked dimensions are never touched."
- Changes persist as contribution supersessions

**Files:**
- `src-tauri/src/pyramid/local_mode.rs` — territory read/write functions
- `src-tauri/src/pyramid/config_contributions.rs` — add dispatcher branch
- `src-tauri/src/main.rs` — register commands
- `src/hooks/useLocalMode.ts` — territory methods
- `src/components/Settings.tsx` — territory accordion section

**Acceptance:**
- Each dimension shows current territory status
- Changing a dimension persists as a contribution supersession
- Territory is readable by future steward code via standard contribution query
- Locked dimensions have visual lock treatment
- No `UnknownSchemaType` errors logged on supersession

---

## Resolved Questions

1. **Model card layout:** Accordion/collapsible sections within the Ollama panel. Each concern (model portfolio, context/concurrency, pull, history, territory) is a collapsible section.

2. **Auto-probe frequency:** Probe on mount + manual refresh button. No periodic polling.

3. **Pull model input:** Free text input for model name. Link to https://ollama.com/library opens browser for browsing. No in-app browsing — Ollama has no stable registry/search API.

4. **Concurrency max:** Incrementor capped at 12, with warning text: "Most home users should leave this on 1 to prevent issues."

---

## Dependency Graph

```
Phase 0 (infrastructure prerequisites)
    ↓
Phase 1 (unblock + hot-swap)
    ↓
Phase 2 (model portfolio)    Phase 3 (context + concurrency)    Phase 5 (config history)    Phase 6 (experimental territory)
    ↓
Phase 4 (pull + delete)  ←── needs model cards from Phase 2
```

Phase 0 is prerequisite for all. Phases 2, 3, 5, 6 are independent of each other (all depend only on Phase 1). Phase 4 depends on Phase 2 for the card UI. The critical path is Phase 0 → Phase 1 → Phase 2 → Phase 4.

---

## DB Migration Summary

Phase 1: Add `context_override INTEGER`, `concurrency_override INTEGER`, and `restore_dispatch_policy_contribution_id TEXT` nullable columns to `pyramid_local_mode_state`. Update `LocalModeStateRow` struct and all construction sites.
Phase 6: New `experimental_territory` dispatcher branch in `sync_config_to_operational` (no-op, no new tables).

---

## Stage 1 Audit Corrections Applied

| Finding | Severity | Source | Correction |
|---------|----------|--------|------------|
| Missing `rebuild_cascade_from_registry` in hot-swap | Critical | A | Added as explicit step 3 in AD-1 and Phase 1 backend spec |
| Hot-swap must use `load_active_config_contribution`, not state row | Critical | B | Specified explicitly in AD-1 with explanatory comment |
| ProviderRegistry not refreshed in switchModel | Major | A | Added `registry.load_from_db` step to AD-1 |
| Forward-compat claim without mechanism for override columns | Major | A | Moved column migration to Phase 1 |
| No pull cancellation | Major | A | Added cancel IPC + AtomicBool to AD-3 and Phase 4 |
| No concurrent pull guard | Major | A | Added Mutex guard to AD-3 and Phase 4 |
| Rollback creates state split with local_mode_state | Major | A | Block rollback while local mode enabled (AD-7, Phase 5) |
| Breaking IPC contract Vec\<String\> → Vec\<OllamaModelInfo\> | Major | A | Added parallel field, keep backward compat (AD-2, Phase 2) |
| Hot-swap must preserve restore columns in state row | Major | B | Read-modify-write pattern specified in AD-1 |
| Context override + hot-swap interaction underspecified | Major | B | Precedence rules added to AD-4 |
| Override columns wiped on disable UPSERT | Major | B | Persistence rule added to AD-4 |
| No stable Ollama browse API | Major | B | Downgraded to link-out + free text (Resolved Q3, Phase 4) |
| Concurrency cap contradictions (4/8/12) | Minor | A+B | Unified to 12 everywhere |
| Dependency graph shows wrong Phase 5 dependency | Minor | A | Fixed diagram |
| Auto-probe fires on hardcoded default URL | Minor | A | Probe only when status.base_url is set |
| Event bus slug semantics undefined | Minor | A+B | Specified empty string convention in AD-3 |
| experimental_territory needs dispatcher branch | Minor/Major | A+B | Added to AD-6 and Phase 6 |
| Precedence rules not explicit | Minor | A | Added to AD-4 and AD-5 |
| config_type vs schema_type naming | Minor | B | Fixed to schema_type in Phase 5 |
| authored_by vs created_by naming | Minor | B | Fixed to created_by in Phase 5 |
| Test Connection button disabled when enabled | Minor | B | Re-enabled + renamed in Phase 1 frontend |
| DELETE with body non-standard | Info | A+B | Documented in Phase 4 |
| Pull stream non-progress phases undocumented | Minor | B | Full phase list added to Phase 4 |
| parse_tags callsite migration unclear | Minor | B | Specified delegation pattern in AD-2 |
| families field missing from OllamaModelInfo | Minor | B | Added to Phase 2 struct |

---

## Stage 2 Discovery Audit Corrections Applied

| Finding | Severity | Source | Correction |
|---------|----------|--------|------------|
| Concurrency override won't work — dual axis (build_strategy vs provider pool semaphore) | Major | B | Rewrote AD-5 to require both supersessions + pool reload + semaphore removal |
| Context override doesn't flow to Ollama `num_ctx` — silent truncation | Major | A | Added num_ctx pass-through requirement to AD-4 and Phase 1 commit step |
| Model switch/disable during active build corrupts routing mid-flight | Major | A+B | Added active build guard to AD-1, Phase 1 acceptance criteria |
| SSRF on base_url — no localhost restriction | Major | B | Added SSRF warning to Phase 0 |
| Config history O(N) chain walk with N+1 queries | Major | A+B | Replaced with single SQL query in Phase 5 |
| Rollback past schema migration boundary could fail | Major | A | Added schema validation guard to Phase 5 rollback |
| `save_local_mode_state` full-column UPSERT is structural landmine | Major | A+B | Documented read-modify-write requirement; consider field-level mutation API |
| Empty-slug pull events pollute cross-pyramid timeline | Minor | B | Changed slug convention from `""` to `"__ollama__"` in AD-3 |
| No shared accordion component in design system | Minor | B | Added Phase 0 with AccordionSection component |
| reqwest::Client::new() per call instead of shared client | Minor | B | Added shared HTTP client to Phase 0 |
| Zero accessibility markup | Minor | B | Added accessibility baseline to Phase 0 |
| Toggle checkbox race with rapid clicks | Minor | B | Noted; add `localMode.loading` to disable early-return |
| Confirmation dialog not dismissed on status change | Minor | A+B | Add useEffect to reset confirmingDisable on status flip |
| `rebuild_cascade_from_registry` doesn't write max tier context limit | Major | A | Pre-existing bug; fix alongside Phase 1 (add context_limit write for max tier) |
| `local_mode_toggle` source not in canonical vocabulary | Minor | A | Change to `"local"` source; use triggering_note for provenance |
| Auto-probe 5s timeout for previously-configured-then-disabled users | Minor | A | Suppress probe error display when auto-probing (silent failure to empty list) |
| Concurrency clamp only in IPC, not in executor | Minor | A | Add MAX_CONCURRENCY constant used by both IPC and executor |
| Delete model warning not in UI confirmation | Minor | A | Add build-in-progress warning to delete confirmation dialog |

### Accepted Risks (not corrected)

| Finding | Severity | Rationale |
|---------|----------|-----------|
| `LocalModeStatus` exposes internal fields to frontend | Minor | Not blocking; deferred to cleanup pass |
| `handleDisableLocalMode` stale-closure confirmation race | Minor | Edge case; reset-on-status-change useEffect is sufficient |
