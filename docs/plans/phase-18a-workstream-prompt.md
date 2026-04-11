# Workstream: Phase 18a — Local Mode + Provider Management

## Who you are

You are an implementer joining a coordinated fix-pass across the pyramid-folders/model-routing/observability initiative. The original 17 phases shipped to main. Phase 18 reclaims 9 dropped cross-phase handoffs that were punted from earlier phases into later phases but never threaded through the receiving workstream prompts. You are implementing workstream **18a**, claiming ledger entries **L1, L2, L3, L5** from `docs/plans/deferral-ledger.md`.

Three other Phase 18 workstreams (18b/18c/18d) run in parallel on their own branches. Do not touch files outside your scope. Your commits land on branch `phase-18a-local-mode-providers`.

## Context

Adam has been unable to test with local Ollama because there is no toggle. He asked "where do I turn on local mode?" during first real use of the shipped app — the Ouro test (comparing local vs cloud output) is blocked on your workstream shipping cleanly. The implementer ceremony (implementer → verifier → wanderer) has been running at 17/17 wanderer-catch rate across the initiative, so expect a wanderer to trace your work end-to-end on a real-ish launch. If your build crashes at startup or fails to flip a real pyramid's routing on toggle, the wanderer will find it.

## Ledger entries you claim

| L# | Item | Source spec |
|---|---|---|
| **L1** | **Local Mode toggle in Settings.tsx** — "Use local models (Ollama)" switch per `provider-registry.md` §382–395, with backend IPCs for enable/disable/status. | `docs/specs/provider-registry.md` lines 382–395 + 559–561 |
| **L2** | **Credential warnings UI** — surface missing `${VAR}` credential references when a pulled contribution needs a variable the user hasn't defined. | `docs/specs/credentials-and-secrets.md` + `provider-registry.md` §437 |
| **L3** | **OllamaCloudProvider** — optional backend variant for remote Ollama behind nginx. Punted from Phase 3 as optional; ship only if scope allows. | `docs/specs/provider-registry.md` §529 |
| **L5** | **Ollama `/api/tags` model list fetch** — Phase 8's `ModelSelectorWidget` + YamlConfigRenderer `options_from: model_list:{provider_id}` currently only returns models from `tier_routing` entries. For Ollama providers, fetch the real list from `GET {base_url}/api/tags`. | `docs/specs/yaml-to-ui-renderer.md` model_list section + `provider-registry.md` §388 |

## Required reading (in order)

1. `docs/plans/phase-18-plan.md` — overall Phase 18 structure; skim.
2. `docs/plans/deferral-ledger.md` — entries L1, L2, L3, L5 in full.
3. `docs/plans/phase-3-workstream-prompt.md` lines 208–215 and 254 — the exact phrasing where I (conductor) punted these items. Gives you context on why they didn't ship in Phase 3 and what was assumed.
4. **`docs/specs/provider-registry.md` in full** — sections §382–395 (Local Compute Mode), §397–438 (Credential Variable References + Wire-shareable providers + Provider test endpoint), §559–561 (IPC surface). Primary spec.
5. `docs/specs/credentials-and-secrets.md` — `CredentialStore`, `ResolvedSecret` opacity semantics. You extend the credentials resolution path to surface missing-variable errors as user-visible warnings.
6. `docs/specs/yaml-to-ui-renderer.md` — the `model_list:{provider_id}` option source contract. L5 extends this.
7. **`src-tauri/src/pyramid/provider.rs`** — full read. Existing `Provider` struct, `ProviderType` enum (Openrouter + OpenaiCompat), `LlmProvider` trait, `OllamaLocalProvider` (if separate) or how Ollama is currently handled via `OpenaiCompat`. Understand `detect_context_window` and how it hits `POST /api/show`.
8. **`src-tauri/src/pyramid/credentials.rs`** — `CredentialStore::collect_references`, `resolve_var`, error types. You'll surface these errors to the UI.
9. **`src-tauri/src/pyramid/db.rs` lines ~1530–1620 and ~12380–12470** — `pyramid_providers` table + `save_provider` + `list_providers`. You'll upsert an `ollama-local` row when the user enables local mode.
10. **`src-tauri/src/pyramid/config_contributions.rs`** — `create_config_contribution`, `supersede_config_contribution`, `load_active_config_contribution`, the tier_routing branch in `sync_config_to_operational`. L1's toggle-on supersedes the active tier_routing contribution; toggle-off restores the prior one.
11. **`src-tauri/src/pyramid/db.rs` `TierRoutingYaml` struct (~line 14002)**. Note the mismatch between the struct field name (`tiers`) and the bundled YAML field name (`entries`). `config_contributions.rs:677` calls `serde_yaml::from_str::<TierRoutingYaml>` directly on `yaml_content`. Verify which form actually parses — the bundled seed may be broken silently or the struct may use serde renames not visible in the short read. **Read the bundled seed in `src-tauri/assets/bundled_contributions.json` and trace a round-trip through the sync dispatcher before assuming anything.** If the bundled seed is broken, fix it in your commit and note it as a side-effect in the implementation log.
12. `src-tauri/src/pyramid/yaml_renderer.rs` — where `model_list:{provider_id}` options are resolved. L5 extends the resolver for Ollama providers.
13. **`src/components/Settings.tsx`** in full (~273 lines) — existing sections (Health, Node Info, Storage Cap, Mesh Hosting, Auto-Update, Save). You add a new "Local LLM (Ollama)" section between Mesh Hosting and Auto-Update.
14. **`src/components/modes/SettingsMode.tsx`** — 13-line wrapper that mounts `PyramidSettings` + `Settings`. Confirm it doesn't need changes.
15. `src/components/modes/ToolsMode.tsx` around the pull flow — L2 adds credential-warning surfacing when pulling a Wire contribution whose YAML references undefined `${VAR}` patterns. Grep for `pyramid_pull_wire_config` to find the current pull call site.
16. `src/components/yaml-renderer/widgets/ModelSelectorWidget.tsx` — the widget that consumes L5's fetched model list. Understand its current `optionSources` contract.
17. `src/hooks/useYamlRendererSources.ts` — where dynamic option sources are resolved; you extend the `model_list:` resolver for Ollama.

## What to build

### 1. Backend: Local Mode state storage

The spec says "When toggled off, restores the previous tier routing (stored before toggle was activated)." You need a place to remember the pre-toggle tier_routing contribution ID so restoration is genuine, not reset-to-defaults.

Two options. Both are acceptable — pick one and document the choice in the implementation log:

**Option A: single-row state table.** Add `pyramid_local_mode_state(enabled BOOLEAN NOT NULL, restore_from_contribution_id TEXT, ollama_base_url TEXT, ollama_model TEXT, updated_at TEXT DEFAULT datetime('now'))` to `db.rs` as a new table. Key it on an implicit single row (id=1 PRIMARY KEY). Idempotent migration.

**Option B: derive from contribution chain.** Query for "most recent active tier_routing contribution whose `triggering_note` does NOT match `local_mode%`" to find the pre-toggle state. Store the ollama_base_url + model on the new contribution's `triggering_note` so status query can parse them back out.

Option A is cleaner and I recommend it. Option B avoids a schema migration but parses structured data out of a free-text field.

### 2. Backend: three IPC commands

Per spec §559–561:

```rust
#[tauri::command]
async fn pyramid_get_local_mode_status(...) -> Result<LocalModeStatus, String>

#[tauri::command]
async fn pyramid_enable_local_mode(
    state: tauri::State<'_, SharedState>,
    base_url: String,
    model: Option<String>,  // None → auto-pick first model from /api/tags
) -> Result<LocalModeStatus, String>

#[tauri::command]
async fn pyramid_disable_local_mode(
    state: tauri::State<'_, SharedState>,
) -> Result<LocalModeStatus, String>
```

`LocalModeStatus` shape:

```rust
#[derive(Serialize)]
struct LocalModeStatus {
    enabled: bool,
    base_url: Option<String>,
    model: Option<String>,
    detected_context_limit: Option<usize>,
    available_models: Vec<String>,         // populated from /api/tags on enable; cached
    reachable: bool,                       // last reachability check
    reachability_error: Option<String>,    // explanation if reachable == false
    ollama_provider_id: String,            // "ollama-local" conventionally
    prior_tier_routing_contribution_id: Option<String>,  // what we'll restore on disable
}
```

**`pyramid_enable_local_mode` behavior** (per spec §384–391 — all six steps are required):

1. Validate `base_url` is a reasonable URL (starts with `http://` or `https://`, trimmed).
2. Reachability check: `GET {base_url}/api/tags`. If unreachable, return `Err` with a clear message (do NOT proceed to half-configured state).
3. Parse `/api/tags` response → `Vec<String>` of model names. If `model` param is `None`, pick the first (stable — sort the list for determinism).
4. Auto-detect context window: `GET {base_url}/api/show` with the selected model. Use the existing `detect_context_window` helper from `provider.rs`. On failure, fall back to `128000` (reasonable Ollama default) with a warning captured in the log.
5. Upsert `pyramid_providers` row with `id = "ollama-local"`, `provider_type = OpenaiCompat`, `base_url` = user input, `api_key_ref = None`, `auto_detect_context = true`, `supports_broadcast = false`, `enabled = true`. Use `db::save_provider`.
6. Snapshot the current active `tier_routing` contribution's `contribution_id`. Write it to `pyramid_local_mode_state.restore_from_contribution_id`. Set `enabled = true`, `ollama_base_url`, `ollama_model` in the same row.
7. Build a new `tier_routing` YAML: every tier (`fast_extract`, `synth_heavy`, `stale_local`, `web`, `stale_remote`, whatever tiers the current active contribution defines) points at `provider_id: ollama-local`, `model_id: <selected>`, with `context_limit` set to the detected value. Do NOT add new tiers — copy the tier names from the prior contribution so existing chain steps don't hit missing-tier errors.
8. Call `supersede_config_contribution(conn, prior_contribution_id, new_yaml, "local mode enabled", "local_mode_toggle", Some("user"))`. This supersedes the prior → active, and because Phase 4's dispatcher handles the tier_routing branch, `sync_config_to_operational` runs automatically.
9. Per spec §390: **derive dehydration budgets from detected context limit**. Look at `OperationalConfig` and the Tier2 config — there are hardcoded budget values (`answer_prompt_budget`, `pre_map_prompt_budget`, etc.) that assume large context windows. When local mode is enabled, scale these down to fit the detected context limit. If this turns out to be a rabbit hole (the scaling logic isn't trivial), document in the implementation log as a known limitation — the local mode still works, budgets just aren't auto-tuned.
10. Per spec §391: **set concurrency to 1**. This lives in `build_strategy` contribution, not `tier_routing`. Supersede the active `build_strategy` contribution too, setting `initial_build.concurrency = 1` and `maintenance.concurrency = 1`. Snapshot the prior `build_strategy` contribution_id in the state table so we can restore it. (You'll need a second restore column: `restore_build_strategy_contribution_id`.)
11. Return the new `LocalModeStatus`.

**`pyramid_disable_local_mode` behavior:**

1. Load `pyramid_local_mode_state`. If `enabled = false`, return the current status unchanged (idempotent).
2. Load the contribution identified by `restore_from_contribution_id`. If it still exists and is parseable, copy its YAML into a new "restore" contribution that supersedes the currently-active local-mode contribution. `triggering_note = "local mode disabled — restoring prior tier_routing"`.
3. Same for `restore_build_strategy_contribution_id`.
4. Update `pyramid_local_mode_state.enabled = false`. Leave the `ollama_base_url` / `ollama_model` fields populated so the UI remembers the last values for next time.
5. Return updated `LocalModeStatus`.

**`pyramid_get_local_mode_status` behavior:**

1. Read `pyramid_local_mode_state` row.
2. If `enabled = true`: run a reachability check on the stored base_url, refresh `available_models` from `/api/tags`. Return full status.
3. If `enabled = false`: return `{ enabled: false, base_url: None, model: None, reachable: false, ... }`.

### 3. Frontend: Settings.tsx Local LLM section (L1 — the big one)

Add a new `<div className="settings-section">` between the Mesh Hosting section and the Auto-Update section in `Settings.tsx`. This is **the component Adam will test by feel**. It must look like a settings control, not a form submission.

Components:
- **Header:** "Local LLM (Ollama)"
- **Section description** (short): "Route all tiers through a local Ollama instance. When enabled, every build uses local models instead of cloud providers."
- **Toggle:** `<input type="checkbox">` labeled "Use local models (Ollama)"
  - Initial state: `status.enabled` from `pyramid_get_local_mode_status` IPC called on mount
  - onChange: call `pyramid_enable_local_mode` or `pyramid_disable_local_mode`
  - Disabled while a request is in-flight, with a small loading indicator
- **Base URL field:** `<input type="text">` with default `http://localhost:11434/v1`
  - Editable when toggle is off; read-only (greyed) when toggle is on (you can't change URL while enabled — disable first, change, re-enable)
  - Validation: must start with `http://` or `https://`
- **Model dropdown:** `<select>` populated from `status.available_models` (when enabled) or pre-fetched via a "Test connection" button (when disabled)
  - "Test connection" button next to the URL field — calls `pyramid_get_local_mode_status` with a one-shot reachability probe (you'll need to extend the IPC OR add a `pyramid_probe_ollama(base_url) -> { reachable, models }` helper IPC)
  - When the toggle is on, this dropdown reflects the current model; changing it requires disabling + re-enabling
- **Status line:** below the controls, shows one of:
  - Green checkmark + "Enabled — routing N tiers through {model} on {base_url}, context limit {N}K tokens"
  - Red X + "Cannot reach Ollama at {base_url}: {error}"
  - Grey "Disabled — builds use cloud providers (OpenRouter)"
- **Warning banner (orange)** when enabled: "Local mode sets concurrency to 1 (home hardware constraint). Builds will be slower but run entirely on your machine."
- **Anti-fat-finger guard** when disabling: "Disable local mode? This will restore your previous tier routing." Confirm modal.

Match the existing CSS conventions from other settings sections (`settings-section`, `settings-section-header`, `settings-section-desc`, `settings-toggle`). Do NOT introduce new styling systems.

### 4. Frontend: useLocalMode hook (optional but cleaner)

Extract the status fetch + toggle handlers into `src/hooks/useLocalMode.ts`. Returns `{ status, loading, error, enable, disable, testConnection }`. Keeps `Settings.tsx` clean. Optional but recommended.

### 5. Backend + frontend: Ollama `/api/tags` model list fetch (L5)

Extend `yaml_renderer.rs` (or wherever `model_list:{provider_id}` is resolved) so that when the provider_id refers to a provider with `provider_type = OpenaiCompat` AND the `base_url` points at an Ollama-shaped endpoint, the resolver hits `GET {base_url}/api/tags` and returns the model names from the response JSON.

`/api/tags` response shape:
```json
{
  "models": [
    {"name": "llama3.2:latest", "modified_at": "...", "size": 1234},
    {"name": "gemma3:27b", ...},
    ...
  ]
}
```

Extract `models[].name` into `Vec<OptionValue { value: name, label: name }>`.

**Detection heuristic for "is this Ollama?":** simplest — provider_type = OpenaiCompat AND base_url contains `11434` (Ollama default port) OR the provider `id` starts with `ollama`. Document the heuristic in code; the user can override via tier_routing edits if they have a non-standard setup.

**Caching:** cache the result per-provider for 30 seconds in a `Mutex<HashMap<String, (Vec<OptionValue>, Instant)>>` at the yaml_renderer module level. Don't spam `/api/tags` on every widget render.

**Failure mode:** if `/api/tags` errors (unreachable, parse failure), return an empty list with a log warning. The widget should handle empty lists gracefully (it already does for other empty option sources per Phase 8).

### 6. Frontend: Credential warnings in ToolsMode (L2)

When a user clicks Pull on a contribution in the Discover tab, call a new IPC `pyramid_preview_pull_contribution(wire_contribution_id) -> { yaml, required_credentials: Vec<String>, missing_credentials: Vec<String> }` that:

1. Fetches the contribution from Wire (via existing `PyramidPublisher::fetch_contribution`)
2. Scans the YAML for `${VAR}` patterns (use existing `CredentialStore::collect_references` or equivalent)
3. For each variable found, checks whether it's defined in the local `.credentials` file
4. Returns the list of required vars + the subset that are missing

In `ToolsMode.tsx`'s `DiscoverPanel` (Phase 14's rewrite), when Pull is clicked:
- If `missing_credentials` is empty: proceed with the normal pull flow.
- If non-empty: show a warning modal listing the missing variables with a message like "This contribution requires credentials you haven't set: `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`. Set them in Settings → Credentials, or pull anyway (the contribution will be inactive until credentials are provided)."
- User can cancel, set credentials first, or pull anyway.

Phase 3's `credentials.rs` should have a helper for listing defined credentials — use it to populate the check. If it doesn't, add one (`CredentialStore::list_defined_names() -> Vec<String>`).

### 7. Backend: OllamaCloudProvider (L3, optional)

If you have scope room, add a new `ProviderType::OllamaCloud` variant for remote Ollama instances (behind nginx basic auth, or behind a bearer token). The difference from `OpenaiCompat` is:
- Must send `Authorization` header (`api_key_ref` required)
- Still uses `/api/tags` for model listing
- `/api/show` for context detection

If scope pressure is real, document in the implementation log as deferred again to "18a+1 fix pass" and ship the other four items cleanly. Local Mode works with `OpenaiCompat` + localhost Ollama today; OllamaCloud is a sharpening.

### 8. Tests

Rust tests:
- `pyramid_local_mode_state` table migration is idempotent
- `pyramid_enable_local_mode` with a mocked HTTP reachability: builds the expected tier_routing + build_strategy supersession + state row
- `pyramid_disable_local_mode` restores the prior contributions
- `pyramid_get_local_mode_status` with enabled=true and enabled=false cases
- `/api/tags` parser handles normal response + empty models array + malformed JSON (returns empty list, no crash)
- `detect_context_window` integration (may already be tested in Phase 3)
- Credential scan returns the right missing-variables list for a YAML with `${DEFINED}` + `${UNDEFINED}`

Frontend tests (only if a runner exists — check `package.json`):
- `useLocalMode` hook's state transitions
- `Settings.tsx` Local LLM section rendering in enabled/disabled/error states
- Credential warning modal renders the missing list

If no frontend runner, document manual verification steps in the implementation log (see "Verification" below).

## Scope boundaries

**In scope:**
- `pyramid_local_mode_state` table + migration
- Three IPCs: `pyramid_get_local_mode_status`, `pyramid_enable_local_mode`, `pyramid_disable_local_mode`
- Optional helper IPC: `pyramid_probe_ollama` (you may fold it into `pyramid_get_local_mode_status` if cleaner)
- One new IPC: `pyramid_preview_pull_contribution` for L2
- Settings.tsx "Local LLM (Ollama)" section with toggle + URL + model picker + status + warning banner + confirm modal
- Optional `useLocalMode.ts` hook
- Ollama `/api/tags` resolver in `yaml_renderer.rs` with 30s per-provider cache
- Credential warning modal in `ToolsMode.tsx::DiscoverPanel` pull flow
- OllamaCloudProvider backend if scope allows; otherwise document as deferred
- Rust tests for the IPC contract + the /api/tags parser
- Manual verification steps in the implementation log

**Out of scope (belongs to other Phase 18 workstreams):**
- `call_model_audited` cache retrofit — 18b
- `search_hit` demand signal recording — 18b
- Cache-publish privacy opt-in UI — 18c
- Folder/circle pause-all scopes — 18c
- Schema migration UI — 18d
- CC memory subfolder ingestion — 18e (separate workstream Adam adds later)
- Anything in `routes.rs::handle_search`, `chain_dispatch.rs`, `evidence_answering.rs`, or the schema migration code

**Out of scope permanently (not this phase):**
- Multiple concurrent local providers (one ollama-local row is sufficient)
- Persistent reachability monitoring (status refresh on mount is enough)
- Model download / pull UI (`/api/pull` is a whole other feature)
- Load balancing across local instances
- Streaming responses from Ollama (non-streaming is fine)

## Verification criteria

**Every item is a hard requirement. If you can't check one off, the phase is not done — escalate in the log, don't silently skip.**

1. **Rust clean:** `cargo check --lib` from `src-tauri/` — zero new warnings (3 pre-existing are allowed).
2. **Test count:** `cargo test --lib pyramid` — prior count (~1238) + new Phase 18a tests. Same 7 pre-existing failures throughout.
3. **Frontend build:** `npm run build` from repo root — clean, no new TypeScript errors.
4. **IPC registration:** `grep -c "pyramid_enable_local_mode\|pyramid_disable_local_mode\|pyramid_get_local_mode_status\|pyramid_preview_pull_contribution" src-tauri/src/main.rs` should be at least 8 (4 function defs + 4 invoke_handler entries).
5. **Settings section renders:** document in implementation log that on a freshly-built app, the Settings tab shows a "Local LLM (Ollama)" section between Mesh Hosting and Auto-Update.
6. **Toggle-on end-to-end (the load-bearing one):** manual verification path documented:
   - Launch built app
   - Settings → Local LLM → toggle ON with `http://localhost:11434/v1` and a valid model
   - Expected: toggle flips green, status line shows "Enabled — routing N tiers through {model}"
   - Then: go to any pyramid, trigger a rebuild, observe log output showing the chain executor routing to `ollama-local` for every LLM call
   - Then: toggle OFF
   - Expected: prior tier_routing contribution restored, next build goes back to OpenRouter
7. **Credential warning renders:** documented manual path — pull a contribution whose YAML contains `${NONEXISTENT_VAR}`, observe the warning modal listing it.
8. **`/api/tags` model fetch works:** when toggled on, the model dropdown in Settings populates from a real `/api/tags` call (or gracefully degrades if no Ollama is running on the test machine — the dropdown should show "No models found" with a clear reason, not crash).

## Deviation protocol

Standard. Most likely deviations:

- **`TierRoutingYaml` struct vs bundled seed mismatch** — if the bundled `entries:` YAML really doesn't parse into the `tiers:` struct, Phase 5's seed is broken. Fix it in your branch and note it as a Phase 5 side-effect in the implementation log; don't leave it broken because it's "not your scope."
- **Dehydration budget scaling** — if the budget-scaling logic is a rabbit hole, skip it and document the limitation. Local mode still works, it just uses the default (possibly too-large) budgets.
- **OllamaCloudProvider** — explicit deferral permitted if scope pressure. Document in log.
- **Restore chain walking** — if Option B (walk contribution chain) turns out cleaner than Option A (state table), use B. Both are acceptable.
- **`pyramid_probe_ollama` vs extending `pyramid_get_local_mode_status`** — pick whichever feels less awkward. The goal is that the user can "test connection" from the disabled state and see available models before committing to enable.
- **Bundled `tier_routing` seed** — per Adam's original direction, `stale_local` was supposed to be NOT seeded (Option A from the planning session). Phase 5 seeded it anyway pointing at openrouter. You can fix this (drop `stale_local` from the bundled seed) as a side-effect, or leave it and document. Your call — but if you fix it, be aware that existing installs with that row already in their DB won't be affected until next bundle migration.

## Mandate

- **Settings section is the gate.** `feedback_always_scope_frontend.md`: Adam tests by feel. If the toggle isn't visible and clickable in the built app, the phase failed regardless of how clean the backend is. This is the rule Phase 3's workstream prompt violated and Phase 18a exists to repair.
- **No Pillar 37 violations.** Don't hardcode the Ollama default model name, don't hardcode a model list, don't hardcode dehydration budgets. All user-tunable values flow from contributions or UI inputs.
- **Reversibility.** Toggle-off must actually restore the prior state. A half-restored state where the provider row exists but tier_routing was reset to defaults is a bug.
- **Match existing frontend conventions.** Look at the Mesh Hosting section in `Settings.tsx` for tone, structure, CSS class names. Do not introduce a new styling system.
- **Fix bugs found during the sweep.** Standard repo rule. The `TierRoutingYaml` struct mismatch is the most likely one you'll trip over; fix it.

## Commit format

Single commit on `phase-18a-local-mode-providers` with message:

```
phase-18a: local mode toggle + provider management

<8-12 line body summarizing:
- pyramid_local_mode_state table + three IPC commands
- Settings.tsx Local LLM (Ollama) section with toggle + URL + model + status
- Ollama /api/tags resolver in yaml_renderer with per-provider cache
- ToolsMode credential warning modal on pull
- OllamaCloudProvider (shipped / deferred)
- Claims L1, L2, L3 (maybe), L5 from deferral-ledger.md>
```

Do not amend. Do not push. Do not merge.

## Implementation log

Append a Phase 18a entry to `docs/plans/pyramid-folders-model-routing-implementation-log.md`:
1. The three new IPCs + their shapes
2. The state table schema + migration note
3. The `TierRoutingYaml` mismatch finding (if applicable) + fix
4. The dehydration budget scaling decision (did you or not)
5. Frontend components added + their mount points
6. Tests added + counts
7. Manual verification steps (the 6 and 7 and 8 items from Verification above)
8. Deviations with rationale
9. Status: `awaiting-verification`

## End state

Phase 18a is complete when every item in "Verification criteria" is checked off in the log, the commit is on branch `phase-18a-local-mode-providers` (not pushed, not merged), and the implementation log entry names Adam's Ouro-readiness criterion: "after this merges, Adam can flip Local Mode on and run a build entirely through localhost Ollama without editing YAML by hand or inserting SQLite rows manually."

Begin with the spec + existing `Settings.tsx` + the credentials + provider modules. Then the backend IPCs. Then the frontend section. Then the credential warning surface. Then wire `/api/tags`. Then tests. Do not skip the "Settings section renders" acceptance check.

Good luck. This is the toggle that unblocks the Ouro test.
