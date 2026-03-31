# Sprint 1.5c — Vocabulary-Native Execution

## Context

The planner LLM reads the vocabulary and produces plans using vocabulary-native command names (`wire_api_call`, `list_my_corpora`, `operator_api_call`). The executor rejects these as "Command not allowed" because it only knows 7 hardcoded commands and expects a separate `api_call` step type that the vocabulary doesn't describe.

The problem isn't the LLM inventing commands. The problem is the executor speaking a different language than the vocabulary.

## The Fix

**One language throughout:** The vocabulary defines what's available. The LLM produces plans using vocabulary names. The executor accepts vocabulary names. No translation layer, no separate step types.

### 1. Eliminate the `api_call` step type

The `api_call` step type was an intermediate format we invented. The vocabulary describes Wire API operations as either:
- Direct Tauri commands (`pyramid_build`, `sync_content`, etc.)
- API calls via `operator_api_call` or `wire_api_call` Tauri commands with `{ method, path, body }` as args

Both are just `command` + `args`. The LLM should produce:
```json
{
  "command": "operator_api_call",
  "args": { "method": "POST", "path": "/api/v1/wire/agents/archive", "body": { "agent_id": "..." } }
}
```

Not the separate `api_call` step type. This matches how the vocabulary documents these operations.

### 2. Expand ALLOWED_COMMANDS to match the vocabulary

The allowlist should contain every Tauri command that appears in the vocabulary files. Derive it from the actual vocabulary, not a hardcoded list of 7.

**All commands from the vocabulary:**
- `pyramid_build`, `pyramid_create_slug`, `pyramid_build_cancel`, `pyramid_list_slugs`, `pyramid_build_force_reset`
- `pyramid_apex`, `pyramid_node`, `pyramid_tree`, `pyramid_drill`, `pyramid_search`
- `pyramid_get_references`, `pyramid_get_composed_view`, `pyramid_list_question_overlays`
- `pyramid_get_publication_status`, `pyramid_cost_summary`, `pyramid_stale_log`
- `pyramid_annotations_recent`, `pyramid_faq_directory`, `pyramid_faq_category_drill`
- `pyramid_archive_slug`, `pyramid_delete_slug`, `pyramid_purge_slug`
- `pyramid_publish`, `pyramid_publish_question_set`, `pyramid_characterize`, `pyramid_check_staleness`
- `pyramid_auto_update_config_get`, `pyramid_auto_update_config_set`
- `pyramid_auto_update_freeze`, `pyramid_auto_update_unfreeze`, `pyramid_auto_update_status`, `pyramid_auto_update_run_now`, `pyramid_auto_update_l0_sweep`
- `pyramid_breaker_resume`, `pyramid_breaker_archive_and_rebuild`
- `pyramid_set_config`, `pyramid_get_config`, `pyramid_test_api_key`
- `pyramid_set_access_tier`, `pyramid_get_access_tier`
- `pyramid_set_absorption_mode`, `pyramid_get_absorption_config`
- `pyramid_question_build`, `pyramid_question_preview`
- `pyramid_vine_build`, `pyramid_vine_build_status`
- `link_folder`, `unlink_folder`, `sync_content`, `get_sync_status`, `set_auto_sync`
- `list_my_corpora`, `list_public_corpora`, `create_corpus`
- `fetch_document_versions`, `compute_diff`, `pin_version`, `update_document_status`, `bulk_publish`, `open_file`
- `get_credits`, `get_market_surface`
- `get_messages`, `send_message`, `dismiss_message`
- `get_logs`, `get_health_status`, `check_for_update`
- `get_config`, `set_config`, `get_node_name`, `is_onboarded`
- `get_tunnel_status`, `retry_tunnel`
- `save_compose_draft`, `get_compose_drafts`, `delete_compose_draft`
- `cache_wire_handles`, `get_cached_wire_handles`
- `operator_api_call` — for all operator-auth Wire API calls (path + method + body as args)
- `wire_api_call` — for all wire-scoped Wire API calls (path + method + body + headers as args)

**Explicitly blocked (dangerous, not in vocabulary):**
```typescript
const BLOCKED_COMMANDS = new Set([
    // System lifecycle — never planner-invocable
    'logout', 'install_update', 'save_onboarding',
    // Recursive — would call itself
    'planner_call',
    // Auth flow — could trigger emails or change session
    'send_magic_link', 'verify_magic_link', 'verify_otp', 'login', 'auth_complete_ipc',
    // Session/system exposure
    'get_operator_session', 'get_home_dir',
    // Internal build operations — not user-facing
    'pyramid_ingest', 'pyramid_parity_run', 'pyramid_meta_run',
    'pyramid_crystallize', 'pyramid_chain_import',
    // Partner system — planner should not impersonate
    'partner_send_message', 'partner_session_new',
    // Destructive vine operation
    'pyramid_vine_rebuild_upper',
]);
```

**Commands in neither vocab nor blocklist (decision needed at implementation):**
- Harmless reads: `get_wire_identity_status`, `get_app_version`, `get_work_stats`, `pyramid_build_status`, `test_remote_connection` — add to vocabulary if planner should use them, blocklist if not
- Vine read commands: `pyramid_vine_bunches/eras/decisions/entities/threads/drill/corrections/integrity` — add to `pyramid_explore.md` vocabulary if planner should use them, blocklist if not

### 3. Update PlanStep type

Remove `api_call`, `auth` fields. Everything is `command` + `args` or `navigate`.

```typescript
export interface PlanStep {
    id: string;
    description: string;
    estimated_cost: number | null;
    on_error?: 'abort' | 'continue';
    command?: string;
    args?: Record<string, unknown>;
    navigate?: { mode: string; view?: string; props?: Record<string, unknown> };
}
```

### 4. Update executor in IntentBar.tsx

Remove the `api_call` branch entirely. The executor becomes:

```typescript
if (step.command) {
    if (BLOCKED_COMMANDS.has(step.command)) {
        throw new Error(`Command blocked: ${step.command}`);
    }
    return invoke(step.command, step.args ?? {});
}

if (step.navigate) {
    setMode(step.navigate.mode);
    return { navigated: true };
}
```

No allowlist — a blocklist instead. Everything is allowed UNLESS explicitly blocked. The vocabulary IS the security boundary. If a command doesn't exist as a Tauri command, `invoke` returns an error naturally.

### 5. Update planner prompt

Remove all references to the `api_call` step type. Update examples to show:
- Wire API calls as `command: "operator_api_call"` or `command: "wire_api_call"` with the path/method/body in args
- Direct Tauri commands as `command: "pyramid_build"` etc.

The vocabulary files already describe operations this way — the prompt just needs to match.

### 6. Update prompt examples

Example 2 (agent archive) becomes:
```json
{
  "id": "step-1",
  "command": "operator_api_call",
  "args": { "method": "POST", "path": "/api/v1/wire/agents/archive", "body": { "agent_id": "scout-7" } },
  "description": "Archive agent scout-7 on the Wire",
  "estimated_cost": null
}
```

Not the old `api_call` step type format.

---

## Phases

| Phase | What | Size | Depends on |
|-------|------|------|-----------|
| 1 | Update PlanStep type (remove api_call/auth) | Tiny | — |
| 2 | Update executor (blocklist instead of allowlist, remove api_call branch) | Small | Phase 1 |
| 3 | Update planner prompt + examples | Small | — |
| 4 | Update planner_call fallback prompt in Rust | Tiny | Phase 3 |

Phases 1+3 parallel, then 2+4 parallel.

## Additional Changes from Audit

**OperationsMode.tsx** — `handleViewResult` (line 143) and `getResultLabel` (line 264) use `step0?.api_call` to route the "View Result" button. Replace with `step0?.command === 'operator_api_call' || step0?.command === 'wire_api_call'`.

**OPERATION_FORMAT_VERSION** — bump from 2 to 3 in `src/types/planner.ts` to clear stale operations with the old `api_call` shape.

**Prompt Phase 3 (critical path)** — must rewrite:
- Line 8 rule: "Each step must have exactly one of: `command` + `args`, or `navigate`" (remove `api_call` + `auth`)
- Lines 39-48: remove the api_call step example entirely
- Example 2: change from `api_call` format to `command: "operator_api_call"` format
- CRITICAL RULES section: remove any `api_call` mention

**Future: CI guard** — the union of vocabulary commands + blocklist should equal the full generate_handler list. Any new Tauri command forces an explicit decision: vocabulary or blocklist. Not required for Sprint 1.5c but should be added.

---

## Files

| File | Change |
|------|--------|
| `src/types/planner.ts` | Remove `api_call`, `auth` from PlanStep. Bump OPERATION_FORMAT_VERSION to 3. |
| `src/components/IntentBar.tsx` | Replace ALLOWED_COMMANDS allowlist + api_call branch with BLOCKED_COMMANDS blocklist |
| `chains/prompts/planner/planner-system.md` | Remove api_call step type, update rule + examples to command-only format |
| `src-tauri/src/main.rs` | Update PLANNER_FALLBACK_PROMPT to match |
| `src/components/modes/OperationsMode.tsx` | Replace `step0?.api_call` with command name checks |

## Verification

1. "Archive my agents with zero contributions" → planner produces `command: "operator_api_call"` steps → executor runs them
2. "Build a pyramid from my code" → planner produces `command: "pyramid_create_slug"` + `command: "pyramid_build"` → executor runs them
3. "Search the Wire for X" → planner produces `command: "wire_api_call"` with query path OR `navigate` step → works either way
4. Planner produces `command: "logout"` → executor rejects with "Command blocked"
5. Planner produces `command: "nonexistent_thing"` → Tauri invoke returns error naturally
6. `npx tsc --noEmit` + `cargo check` pass
