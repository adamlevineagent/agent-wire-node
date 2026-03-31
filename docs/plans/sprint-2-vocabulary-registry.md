# Vocabulary as Wire Contributions — Command Registry

## Context

The planner's vocabulary system is broken at the design level. 64 of ~140 operations require the LLM to construct raw HTTP requests (method, path, auth type, body schema). The LLM keeps hallucinating paths (`/agents` instead of `/api/v1/operator/agents`), using wrong auth types (`wire_api_call` instead of `operator_api_call`), and inventing endpoints that don't exist (`/api/v1/agents/batch-archive`).

Meanwhile, the other ~53 operations are direct Tauri commands where the LLM just says the name (`pyramid_build`) and the executor invokes it. These work perfectly.

The fix: give EVERY operation a simple named command. The vocabulary that defines these names is a Wire contribution — published on the Wire, synced locally, cached for offline. One source of truth that generates both the LLM prompt and the executor's dispatch table.

## Design

### Vocabulary Contribution Format

Each domain (fleet_manage, wire_search, etc.) is a Wire contribution with YAML content. Each command has two halves: a **prompt half** (name, description, params) the LLM sees, and a **dispatch half** (type, method, path) the executor uses.

```yaml
domain: fleet_manage
version: 1
description: "Commands for managing agents in your fleet"
commands:
  # POST body example
  - name: archive_agent
    description: "Archive an agent (soft-delete). Agent is hidden but data is retained."
    params:
      - name: agent_id
        type: string
        required: true
        description: "UUID of the agent to archive"
    dispatch:
      type: wire_api
      method: POST
      path: "/api/v1/wire/agents/archive"
      body_map:
        agent_id: "{{agent_id}}"

  # GET with no params
  - name: list_operator_agents
    description: "List all agents belonging to the operator."
    params: []
    dispatch:
      type: operator_api
      method: GET
      path: "/api/v1/operator/agents"

  # Path parameter interpolation
  - name: get_operator_agent
    description: "Get details for a specific agent by ID."
    params:
      - name: agent_id
        type: string
        required: true
    dispatch:
      type: operator_api
      method: GET
      path: "/api/v1/operator/agents/{{agent_id}}"

  # GET with query parameters
  - name: wire_query
    description: "Query the Wire intelligence graph."
    params:
      - name: q
        type: string
        required: true
        description: "Search query text"
      - name: limit
        type: integer
        required: false
        description: "Max results to return"
      - name: sort
        type: string
        required: false
        description: "Sort order (relevance, recent)"
    dispatch:
      type: wire_api
      method: GET
      path: "/api/v1/wire/query"
      query_map:
        q: "{{q}}"
        limit: "{{limit}}"
        sort: "{{sort}}"

  # Custom headers (context-injected, not user params)
  - name: mesh_read_board
    description: "Read the shared blackboard for the current thread."
    params:
      - name: thread_id
        type: string
        required: true
        source: context
        description: "Thread ID (auto-injected from app context)"
    dispatch:
      type: wire_api
      method: GET
      path: "/api/v1/mesh/board"
      headers:
        X-Wire-Thread: "{{thread_id}}"

  # Tauri direct command (included for prompt visibility)
  - name: pyramid_build
    description: "Build a knowledge pyramid from a linked corpus."
    params:
      - name: slug
        type: string
        required: true
        description: "Pyramid slug to build"
    dispatch:
      type: tauri

  # Navigate command (static)
  - name: go_to_fleet
    description: "Open the Fleet management view."
    params: []
    dispatch:
      type: navigate
      mode: fleet

  # Navigate command with dynamic props
  - name: go_to_search
    description: "Open Wire Search with an optional pre-filled query."
    params:
      - name: query
        type: string
        required: false
        description: "Search query to pre-fill"
    dispatch:
      type: navigate
      mode: search
      props_map:
        query: "{{query}}"
```

Five dispatch types:
| Type | Executor Action |
|---|---|
| `tauri` | `invoke(name, args)` directly |
| `wire_api` | Build request → `invoke("wire_api_call", { method, path, body, headers })` |
| `operator_api` | Build request → `invoke("operator_api_call", { method, path, body })` |
| `navigate` | `setMode()` / `navigateView()` |

Three param-to-request maps:
| Map | Purpose | Example |
|---|---|---|
| `body_map` | POST/PATCH/PUT JSON body | `{ agent_id: "{{agent_id}}" }` |
| `query_map` | GET query string params | `?q={{q}}&limit={{limit}}` |
| `path` with `{{param}}` | Path parameter interpolation | `/api/v1/operator/agents/{{agent_id}}` |

Param sources:
| Source | Meaning |
|---|---|
| `args` (default) | LLM supplies this value |
| `context` | Executor injects from app state (e.g., thread_id) |

### What the LLM Sees (generated from YAML, dispatch block excluded)

```
### archive_agent
Archive an agent (soft-delete). Agent is hidden but data is retained.
Args: { agent_id: string (required) — UUID of the agent to archive }

### wire_query
Query the Wire intelligence graph.
Args: { q: string (required), limit: integer, sort: string }
```

### What the LLM Produces

```json
{ "command": "archive_agent", "args": { "agent_id": "scout-7-uuid" } }
{ "command": "wire_query", "args": { "q": "battery chemistry", "limit": 10 } }
```

No paths. No methods. No auth types. No query strings. The executor handles everything.

### Registry Contribution

A single registry contribution indexes all vocabulary domains:

```yaml
registry_version: 1
entries:
  - domain: fleet_manage
    contribution_id: "uuid-of-fleet-manage-vocab"
  - domain: wire_search
    contribution_id: "uuid-of-wire-search-vocab"
  # ... 13 more
```

The registry is discovered via a well-known **handle-path** (per Pillar 14), not a compiled-in UUID. This naturally follows supersession chains — the app fetches the latest contribution at that handle-path.

### Sync Flow

1. On startup after auth: fetch registry contribution from Wire via handle-path
2. For each entry: fetch vocabulary contribution, parse YAML, validate (see Security), cache to `{data_dir}/vocabulary/`
3. Build `VocabularyRegistry` (prompt text + dispatch table)
4. Three-tier fallback **per domain**: Wire API → local cache → bundled (compile-time)
5. If a single domain fails to parse, fall back for that domain only; log warning; other domains unaffected

### Security: Vocabulary Validation

Vocabulary fetched from the Wire is untrusted. Before accepting:
1. **Path prefix allowlist**: all `path` values must start with `/api/v1/`
2. **Method allowlist**: only GET, POST, PUT, PATCH, DELETE
3. **Dispatch type allowlist**: only `tauri`, `wire_api`, `operator_api`, `navigate`
4. **Domain name must match** what the registry entry declared
5. **All `{{param}}` tokens** in path/query_map/body_map/headers must map to a declared param
6. **No path traversal**: reject paths containing `..`, scheme prefixes, or encoded slashes
7. **Post-interpolation re-validation**: after `{{param}}` interpolation at execution time, re-check the resulting path against rules 1 and 6 (user-supplied param values could contain traversal sequences)

Bundled vocabulary is trusted (compiled into the binary). Cached vocabulary was validated when first synced.

### Named Commands (all API operations)

**fleet_manage (10 API + 1 navigate):** `register_agent`, `list_operator_agents`, `get_operator_agent`, `update_agent_status`, `update_agent_controls`, `regenerate_agent_token`, `archive_agent`, `unarchive_agent`, `merge_agents`, `cancel_agent_merge`, `go_to_fleet`

**fleet_tasks (4 API + 1 navigate):** `create_task`, `list_tasks`, `get_task`, `update_task`, `go_to_fleet_tasks`

**fleet_mesh (6 API + 1 navigate):** `mesh_status`, `mesh_read_board`, `mesh_write_board`, `mesh_list_intents`, `mesh_declare_intent`, `mesh_withdraw_intent`, `go_to_fleet_mesh`

**wire_search (9 API + 1 navigate):** `wire_query`, `wire_feed`, `wire_search`, `wire_list_entities`, `wire_get_entity`, `wire_list_topics`, `wire_get_topic`, `wire_discover_corpora`, `wire_pearl_dive`, `go_to_search`

**wire_compose (5 API + 3 Tauri + 1 navigate):** `wire_contribute`, `wire_contribute_human`, `wire_rate`, `wire_correct`, `wire_supersede`, `save_compose_draft`, `get_compose_drafts`, `delete_compose_draft`, `go_to_compose`

**wire_economics (8 API):** `wire_earnings`, `wire_my_contributions`, `wire_float`, `wire_payment_intent`, `wire_payment_redeem`, `wire_claim_bounty`, `wire_opportunities`, `wire_reputation`

**wire_games (8 API):** `wire_create_game`, `wire_list_games`, `wire_join_game`, `wire_pick_outcome`, `wire_resolve_game`, `wire_cancel_game`, `wire_ripe_predictions`, `wire_market_stake`

**wire_social (12 API):** `wire_send_message`, `wire_get_messages`, `wire_get_notifications`, `wire_mark_notifications_read`, `wire_create_circle`, `wire_list_circles`, `wire_create_list`, `wire_get_lists`, `wire_subscribe`, `wire_get_subscriptions`, `wire_roster`, `wire_pulse`

**Tauri-native domains (included in YAML for prompt visibility):**
- **knowledge_docs (9 Tauri):** `list_my_corpora`, `list_public_corpora`, `create_corpus`, `fetch_document_versions`, `compute_diff`, `pin_version`, `update_document_status`, `bulk_publish`, `open_file`
- **knowledge_sync (5 Tauri + 1 navigate):** `link_folder`, `unlink_folder`, `sync_content`, `get_sync_status`, `set_auto_sync`, `go_to_knowledge`
- **pyramid_build (10 Tauri):** `pyramid_create_slug`, `pyramid_build`, `pyramid_question_build`, `pyramid_build_cancel`, `pyramid_build_force_reset`, `pyramid_set_config`, `pyramid_get_config`, `pyramid_test_api_key`, `pyramid_set_access_tier`, `pyramid_set_absorption_mode`
- **pyramid_explore (15 Tauri):** `pyramid_list_slugs`, `pyramid_apex`, `pyramid_node`, `pyramid_tree`, `pyramid_drill`, `pyramid_search`, `pyramid_get_references`, `pyramid_get_composed_view`, `pyramid_list_question_overlays`, `pyramid_get_publication_status`, `pyramid_cost_summary`, `pyramid_stale_log`, `pyramid_annotations_recent`, `pyramid_faq_directory`, `pyramid_faq_category_drill`
- **pyramid_manage (16 Tauri):** `pyramid_archive_slug`, `pyramid_delete_slug`, `pyramid_purge_slug`, `pyramid_publish`, `pyramid_publish_question_set`, `pyramid_characterize`, `pyramid_check_staleness`, `pyramid_auto_update_config_get`, `pyramid_auto_update_config_set`, `pyramid_auto_update_freeze`, `pyramid_auto_update_unfreeze`, `pyramid_auto_update_status`, `pyramid_auto_update_run_now`, `pyramid_auto_update_l0_sweep`, `pyramid_breaker_resume`, `pyramid_breaker_archive_and_rebuild`
- **system (13 Tauri):** `get_config`, `set_config`, `get_auth_state`, `logout`, `get_health_status`, `check_for_update`, `install_update`, `get_logs`, `get_node_name`, `is_onboarded`, `get_credits`, `get_tunnel_status`, `retry_tunnel`
- **navigate (10 navigate):** `go_to_pyramids`, `go_to_knowledge`, `go_to_tools`, `go_to_fleet`, `go_to_operations`, `go_to_search`, `go_to_compose`, `go_to_dashboard`, `go_to_identity`, `go_to_settings`

**Total: ~150 operations across 15 YAML files. All included in vocabulary for prompt visibility.**

### Version Compatibility

- `version: 1` — additive-only changes (new optional fields) are backward-compatible within major version
- Breaking changes (removed/renamed fields) bump the major version
- Rust parser rejects vocabulary with a version higher than it supports, falls back to bundled for that domain
- `registry_version: 1` follows the same rules

## Pre-existing Bugs (fix independently, not blocked on vocabulary migration)

These were discovered during the audit and should be fixed as standalone patches:

1. **Widget values never applied** — `handleApprove` in IntentBar.tsx executes plan steps as-is. The `widgetValues` state (from corpus_selector, text_input, agent_selector, etc.) is collected but never merged into step args before execution. Widgets are cosmetic. Fix: merge `widgetValues` into matching step args by `field` key before executing.

2. **Navigate silently drops props** — `executeStep` requires BOTH `view` AND `props` (`if (step.navigate.view && step.navigate.props)`). Props without view are dropped. Fix: change to `||` and handle each case.

3. **Model override active** — Line ~2837 of main.rs hardcodes `planner_config.primary_model = "qwen/qwen3.6-plus-preview:free"`. Fix: delete the override.

4. **wire_api_call/operator_api_call not blocked** — These raw commands are not in BLOCKED_COMMANDS, so the LLM can bypass any future translation layer. Fix: add them to BLOCKED_COMMANDS now.

5. **Auth-type inconsistency in prompt examples** — Example 2 vs Example 3b use different auth for the same endpoint. Fix: correct Example 3b to use `wire_api_call`.

6. **Vocabulary not bundled for release builds** — `ensure_default_chains` in chain_loader.rs doesn't write vocabulary files. Fresh installs get empty vocabulary. Fix: bundle vocabulary via `include_str!` in ensure_default_chains.

## Implementation Phases

### Phase 1: Structured Vocabulary + Rust Registry

**New file: `src-tauri/src/vocabulary.rs`**

Typed structs:
```rust
#[derive(Deserialize, Serialize, Clone)]
struct VocabularyDomain {
    domain: String,
    version: u32,
    description: String,
    commands: Vec<CommandDef>,
}

#[derive(Deserialize, Serialize, Clone)]
struct CommandDef {
    name: String,
    description: String,
    params: Vec<ParamDef>,
    dispatch: DispatchEntry,
}

#[derive(Deserialize, Serialize, Clone)]
struct ParamDef {
    name: String,
    #[serde(rename = "type")]
    param_type: String,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    description: Option<String>,
    #[serde(default = "default_source")]
    source: String,  // "args" (default) or "context"
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(tag = "type")]
enum DispatchEntry {
    #[serde(rename = "tauri")]
    Tauri,
    #[serde(rename = "wire_api")]
    WireApi {
        method: String,
        path: String,
        #[serde(default)]
        body_map: Option<HashMap<String, serde_json::Value>>,
        #[serde(default)]
        query_map: Option<HashMap<String, String>>,
        #[serde(default)]
        headers: Option<HashMap<String, String>>,
    },
    #[serde(rename = "operator_api")]
    OperatorApi {
        method: String,
        path: String,
        #[serde(default)]
        body_map: Option<HashMap<String, serde_json::Value>>,
        #[serde(default)]
        query_map: Option<HashMap<String, String>>,
    },
    #[serde(rename = "navigate")]
    Navigate {
        mode: String,
        #[serde(default)]
        view: Option<String>,
        #[serde(default)]
        props_map: Option<HashMap<String, String>>,  // {{param}} interpolation for dynamic nav props
    },
}
```

`VocabularyRegistry` methods:
- `to_prompt_text()` → renders name/description/params as LLM-friendly text (dispatch block excluded, context-source params excluded)
- `get_dispatch(command_name)` → returns `Option<&DispatchEntry>` for executor lookup
- `to_frontend_registry()` → serializes dispatch table as JSON for frontend
- `validate()` → checks path prefixes, methods, template tokens match params, no path traversal
- `load_bundled()` → `include_str!` the YAML files as compile-time fallback

All parsing via `serde_yml` (migrate from archived `serde_yaml` 0.9 → `serde_yml` as part of Phase 1 — mechanical find-replace, not tech debt).

**New directory: `chains/vocabulary_yaml/`**
- Convert all 15 markdown files to YAML format
- ALL operations included (Tauri, API, navigate) — the YAML IS the complete vocabulary
- These replace `chains/vocabulary/*.md` as source of truth
- Serve as both bundled fallback source AND Wire publication source

**Changes to `src-tauri/src/main.rs`:**
- Add `vocabulary: RwLock<VocabularyRegistry>` to `AppState`
- Initialize from bundled YAML during setup
- In `planner_call` (line ~2793): replace the .md file-reading loop with `vocab_registry.to_prompt_text()`
- **Remove model override** at line ~2837 (`planner_config.primary_model = "qwen/qwen3.6-plus-preview:free"`) — use user's configured model
- **Update PLANNER_FALLBACK_PROMPT** (line ~2746) — replace hardcoded string with `VocabularyRegistry::load_bundled().to_prompt_text()` so fallback stays in sync with vocabulary
- **Bundle vocabulary in ensure_default_chains** — write YAML files to `chains/vocabulary_yaml/` on fresh install via `include_str!`
- New Tauri command: `get_vocabulary_registry` → returns dispatch table JSON
- Register in `generate_handler![]`

### Phase 2: Frontend Translation Layer

**New file: `src/utils/commandDispatch.ts`**
- `interpolate(template: string, args: Record<string, unknown>)` → mustache-style `{{param}}` substitution; throws if required placeholder has no value
- `buildApiRequest(dispatch: DispatchEntry, args: Record<string, unknown>, context: AppContext)` → returns `{ method, path, body?, headers?, queryParams? }`:
  - Interpolates `{{param}}` in path, body_map values, query_map values, header values
  - For params with `source: "context"`, pulls values from context instead of args
  - Strips undefined optional query params
  - Validates all required `{{param}}` tokens are satisfied
- `DispatchRegistry` type definition (mirrors Rust struct)

**Changes to `src/components/IntentBar.tsx`:**
- Load registry via `invoke('get_vocabulary_registry')` on mount
- **Fix widget values bug**: before executing, merge `widgetValues` into matching step args by `field` key
- **Allowlist security model**: executeStep checks if command is in the vocabulary registry. If not in registry → reject with error. No fallthrough to raw invoke. The vocabulary registry IS the allowlist.
  - `wire_api` → build request via `buildApiRequest`, call `invoke("wire_api_call", ...)`
  - `operator_api` → build request via `buildApiRequest`, call `invoke("operator_api_call", ...)`
  - `navigate` → interpolate `props_map` from args, call `setMode()` / `navigateView()`
  - `tauri` → `invoke(name, args)` directly
  - **Not in registry → error** (no fallthrough)
- **Consolidate navigate paths**: remove `step.navigate` field handling. All navigation goes through named commands (`go_to_fleet`, `go_to_search`, etc.) via the registry. The `PlanStep.navigate` field becomes deprecated.
- Add `wire_api_call` and `operator_api_call` to BLOCKED_COMMANDS as defense-in-depth
- Plan preview UI: show named command in step details, optionally show resolved HTTP details as expandable debug info

**Changes to `src/types/planner.ts`:**
- Bump `OPERATION_FORMAT_VERSION` to 4 (semantics changed: command is now a named action, not raw API call)
- Mark `navigate` field as deprecated (keep for backward compat but executor prefers `command` path)

**Changes to `src/components/modes/OperationsMode.tsx`:**
- Filter or mark operations with `format_version < OPERATION_FORMAT_VERSION` as stale/incompatible (the field was previously dead code — make it functional)

**Changes to `chains/prompts/planner/planner-system.md`:**
- Remove all HTTP construction rules, anti-patterns, path-copying warnings
- Remove `wire_api_call`/`operator_api_call` from examples — all examples use named commands
- Fix existing auth-type inconsistency in examples (archive uses `wire_api` not `operator_api`)
- Keep structural rules: no data flow between steps, navigate-vs-command decision, widget guidelines, error handling
- Simplify critical rules: "Use command names from the vocabulary. The executor handles all HTTP details. You never specify methods, paths, or URLs."

### Phase 3: Wire Publication + Sync

**Publish vocabulary to Wire:**
- Script/command to publish each YAML domain as an 'action' contribution
- Publish registry contribution at a well-known handle-path
- Handle-path compiled into app as constant (follows supersession naturally per Pillar 14)

**New in `vocabulary.rs`:**
- `sync_vocabulary(api_url, token)` → fetch registry from Wire via handle-path, fetch each domain, validate (path prefixes, methods, template tokens), cache to `{data_dir}/vocabulary/`, rebuild registry
- Three-tier fallback **per domain**: Wire API → cached YAML → bundled
- If a single domain's YAML fails to parse or validate, fall back for that domain only; log warning
- Version check: reject vocabulary with version higher than supported; fall back to bundled for that domain
- Supersession: new contributions supersede old; app follows the chain via handle-path on next sync

**Changes to `main.rs`:**
- After auth established: trigger vocabulary sync
- Update `AppState.vocabulary` with fresh data

### Phase 4: Cleanup

- Remove `chains/vocabulary/*.md` files (replaced by YAML — only after Phase 1 is fully deployed and old code path removed)
- Update handoff doc at `docs/handoffs/planner-prompt-iteration-handoff.md`

## Critical Files

| File | Change |
|------|--------|
| New: `src-tauri/src/vocabulary.rs` | Registry types, parsing, prompt generation, dispatch, validation |
| New: `chains/vocabulary_yaml/*.yaml` (15 files) | Structured vocabulary definitions (ALL operations) |
| New: `src/utils/commandDispatch.ts` | Path/query/body interpolation, API request building |
| `src-tauri/src/main.rs` | AppState + planner_call + get_vocabulary_registry + remove model override |
| `src/components/IntentBar.tsx` | Allowlist executeStep + registry loading |
| `chains/prompts/planner/planner-system.md` | Simplify to named commands, fix examples |
| `src/types/planner.ts` | Bump OPERATION_FORMAT_VERSION to 4, deprecate navigate field |
| `src/components/modes/OperationsMode.tsx` | Filter stale operations by format_version |
| `src-tauri/src/pyramid/chain_loader.rs` | Bundle vocabulary YAML in ensure_default_chains |

## Verification

1. `cargo check` passes with new vocabulary.rs module
2. All 15 YAML files parse without errors; `validate()` passes on all
3. Cross-reference: every API operation in old .md files has a corresponding named command in YAML with correct path, method, auth type
4. "Archive agent scout-7" → LLM produces `{ "command": "archive_agent", "args": { "agent_id": "scout-7" } }` → executor translates to `POST /api/v1/wire/agents/archive`
5. "Search the wire for battery chemistry" → LLM produces `{ "command": "wire_query", "args": { "q": "battery chemistry" } }` → executor builds `GET /api/v1/wire/query?q=battery+chemistry`
6. "Build a pyramid from my code" → LLM produces `{ "command": "pyramid_build", ... }` → executor invokes Tauri command directly
7. "Archive agents with zero contributions" → LLM produces `{ "navigate": ... }` or `{ "command": "go_to_fleet" }` (can't filter between steps)
8. LLM producing `wire_api_call` directly → BLOCKED by BLOCKED_COMMANDS
9. LLM producing unknown command → rejected by allowlist ("command not in vocabulary")
10. App works offline using bundled vocabulary (no Wire connection needed)
11. `npx tsc --noEmit` passes
12. Rebuild app, test full intent→plan→preview→execute flow

## Audit Trail

**Stage 1 findings applied (19 issues from 2 independent auditors):**
- C1+C2: Added `query_map` and path template interpolation with examples
- C3: Added Security section with path prefix allowlist, method validation, template token validation, path traversal rejection
- M1: Plan now explicitly removes model override line
- M2: Flipped to allowlist security model — registry IS the allowlist, no fallthrough
- M3: Added Rust struct definitions
- M4+M5: All operations now listed including Tauri and navigate
- M5(version): Added version compatibility section
- m1+m7(prompt): Plan explicitly fixes auth inconsistency and keeps structural rules
- m2: Added `source: context` for params the executor injects from app state
- m3: Added navigate YAML example
- m4: Changed from compiled UUID to handle-path for registry discovery
- m5: OPERATION_FORMAT_VERSION bumps to 4
- m6: Prompt simplification scoped — only HTTP rules removed, structural rules kept
- A-Issue13: Three-tier fallback is now per-domain, not all-or-nothing
- A-Issue15: Phase 4 cleanup explicitly conditional on Phase 1 deployment
- A-Issue17: Added cross-reference verification step
- A-Issue12: Migrating serde_yaml → serde_yml in Phase 1 (not deferred tech debt)

**Stage 2 findings applied (8 new issues from 2 independent discovery auditors):**
- CRITICAL: Widget values never applied to execution — added to pre-existing bugs list + Phase 2 fix
- CRITICAL: Vocabulary not bundled for release builds — added to pre-existing bugs + Phase 1 ensure_default_chains fix
- MAJOR: Navigate two-path ambiguity — consolidated: all navigation through named commands, `PlanStep.navigate` deprecated
- MAJOR: Navigate dispatch needs `props_map` for dynamic props (go_to_search with query) — added to Navigate variant
- MAJOR: Fallback prompt contradicts new vocabulary — updated to use `load_bundled().to_prompt_text()`
- MAJOR: FORMAT_VERSION field is dead code — OperationsMode.tsx now filters on it
- MINOR: body_map Value type vs string template — interpolate() recursively walks Value tree, interpolates string leaves
- MINOR: Post-interpolation path validation — added rule 7 to security section
