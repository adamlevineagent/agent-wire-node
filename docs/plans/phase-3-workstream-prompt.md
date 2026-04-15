# Workstream: Phase 3 — Provider Registry + Credentials

## Who you are

You are an implementer joining an active 17-phase initiative. Phases 0a, 0b, 1, and 2 are shipped on their feature branches. You are the implementer of Phase 3, which refactors LLM calls from a hardcoded OpenRouter URL to a pluggable provider registry backed by a credentials file for secrets. This is the foundation for local compute mode, per-step model routing, and the rest of the plan.

Phase 3 is substantial — it touches `llm.rs` (the LLM call surface), adds two new modules (`credentials.rs`, `provider.rs`), and extends the DB schema with three new tables. Take the time you need. Do not cut corners on credential safety — the `ResolvedSecret` opacity pattern is load-bearing.

## Context: what Phase 3 replaces

Current `llm.rs` hardcodes `https://openrouter.ai/api/v1/chat/completions` and reads the API key from a single field in `LlmConfig`. Every LLM call in the codebase goes through `call_model_with_usage` or similar helpers that assume OpenRouter's request/response format. The `model_aliases: HashMap<String, String>` field in `LlmConfig` is defined but unused.

Phase 3 makes this pluggable:
- **Providers** become first-class rows in a new `pyramid_providers` table, each with a type (openrouter / openai_compat), base URL, auth key reference, and config JSON.
- **Tier routing** (`pyramid_tier_routing` table) maps tier names like `fast_extract`, `web`, `synth_heavy`, `stale_remote`, `stale_local` to a provider+model pair.
- **Per-step overrides** (`pyramid_step_overrides` table) let users override any specific chain step's model or parameters without touching the tier routing.
- **Credentials** live in a `.credentials` YAML file on disk with 0600 perms. Configs reference secrets as `${VAR_NAME}` and the resolver substitutes them at runtime only. `ResolvedSecret` is an opaque wrapper with no Debug/Display/Serialize to prevent leaks.
- **An `LlmProvider` trait** abstracts request building and response parsing. OpenRouter and OpenAI-compatible providers (including Ollama local) are the v1 implementations.
- **`llm.rs` is refactored** to call through the provider trait instead of using hardcoded URLs/headers/parsing.

## Required reading (in order, in full unless noted)

### Handoff + spec docs

1. `docs/handoffs/handoff-2026-04-09-pyramid-folders-model-routing.md` — original handoff, deviation protocol.
2. `docs/handoffs/handoff-2026-04-09-pyramid-folders-model-routing-addendum-01.md` — just the scope-boundary framing, not critical for Phase 3 specifically.
3. **`docs/specs/provider-registry.md` — read in full, end-to-end.** This is your primary implementation contract for the provider side. Pay particular attention to: the `LlmProvider` trait definition, the table schemas (`pyramid_providers`, `pyramid_tier_routing`, `pyramid_step_overrides`), the `pricing_json` schema with the OpenRouter string-encoded prices gotcha, `supported_parameters_json`, and the Default Seeding section.
4. **`docs/specs/credentials-and-secrets.md` — read in full, end-to-end.** This is your implementation contract for the credential side. Pay particular attention to: the `ResolvedSecret` opacity contract (no Debug/Display/Serialize, type-system enforcement), file permissions (0600 on Unix, refuse to read if wider), atomic writes, the `${VAR_NAME}` substitution rules including escaping with `$$`, the never-log rule, and the Wire-share safety constraints. The spec's "Implementation Order" section at the bottom is a good sequencing guide.
5. `docs/plans/pyramid-folders-model-routing-full-pipeline-observability.md` — Phase 3 section + the parallelism map to understand what Phase 3 unblocks.
6. `docs/plans/pyramid-folders-model-routing-implementation-log.md` — scan Phase 0b, 1, 2 entries to see the patterns previous phases used for tests and logs.

### Code reading

7. **`src-tauri/src/pyramid/llm.rs` — read in full.** This is the file you refactor. Pay attention to:
   - `LlmConfig` struct (the current config with hardcoded model names and fallback chain)
   - `call_model_unified` / `call_model_with_usage` / `parse_openrouter_response_body` — the functions that currently contain hardcoded URL + OpenRouter response parsing
   - `resolve_model` / `resolve_ir_model` — current tier resolution logic
   - All existing constants (`X-Title` header, `HTTP-Referer`, etc.)
   - Line references in the spec (like `llm.rs:312` for the hardcoded URL) may have drifted — verify the actual line numbers when you read.

8. **`src-tauri/src/main.rs`** — targeted read. Find the `PyramidState` construction site (around line 6574+) where `LlmConfig` is built from disk. Your changes extend this path to load the credentials file and seed the provider registry on first run.

9. **`src-tauri/src/pyramid/db.rs`** — targeted read. Find `init_pyramid_db` (grep for it) — you'll add three new table creation statements there. Also find any existing `pyramid_llm_audit` or related tables to see the conventions.

10. **`src-tauri/src/pyramid/mod.rs`** — read the `PyramidState` struct definition around line 720. You'll add new fields (`providers: Arc<ProviderRegistry>`, `credentials: Arc<CredentialStore>`). Also update `with_build_reader` to clone them.

11. **`src-tauri/src/pyramid/types.rs`** — scan for existing types similar to what you'll add. You'll add new types for `Provider`, `TierRouting`, `StepOverride`, `ResolvedSecret`, etc.

12. **Existing call sites of `call_model_*` and `parse_openrouter_*`** — grep for them across `src-tauri/src/pyramid/`. The refactor touches every caller that passes an `LlmConfig`. Expect the grep to return 20-50+ results across `chain_executor.rs`, `stale_helpers_upper.rs`, `build.rs`, `evidence_answering.rs`, `characterize.rs`, etc. You do NOT have to rewrite each caller's LLM prompt logic — just thread the `ProviderRegistry` + `CredentialStore` references through so the underlying call goes via the new trait.

## Default model slugs (Adam provided these explicitly — use them, do not speculate)

On first run, seed `pyramid_tier_routing` with:

| Tier | Provider | Model slug | Rationale (from Adam) |
|---|---|---|---|
| `fast_extract` | `openrouter` | `inception/mercury-2` | Very fast, very cheap, smart enough for most extraction |
| `web` | `openrouter` | `x-ai/grok-4.1-fast` | 2M context window for whole-array relational work |
| `synth_heavy` | `openrouter` | `minimax/minimax-m2.7` | Near-frontier (very smart), relatively slow (40 tps), very inexpensive |
| `stale_remote` | `openrouter` | `minimax/minimax-m2.7` | Same quality profile for upper-layer stale checks |

**`stale_local` is NOT seeded** (Adam's explicit decision — Option A from the earlier conductor exchange). The tier will only exist once a user registers a local provider (e.g., Ollama). Do NOT insert a `stale_local` row pointing at anything; leave it absent.

Default fallback cascade (for context-limit overflows and primary failure): `x-ai/grok-4.1-fast` (2M context beats the current `fallback_1_context_limit: 900_000`).

The default `pyramid_providers` seed row is `openrouter` with `api_key_ref = "OPENROUTER_KEY"`.

## What to build

### 1. Credentials module (`src-tauri/src/pyramid/credentials.rs` — NEW)

Implement per the spec's "The `.credentials` File" and "Variable Substitution" sections:

- `CredentialStore` struct that owns the parsed credentials map + file path
- `CredentialStore::load(data_dir: &Path) -> Result<Self>` — reads the file, checks permissions (refuses if > 0600 on Unix), parses YAML
- `CredentialStore::save_atomic(&self) -> Result<()>` — writes to temp file, fsyncs, renames over original, ensures 0600
- `CredentialStore::resolve_var(&self, name: &str) -> Result<ResolvedSecret>` — lookup by key, returns `Err` with the spec's clear error message if missing
- `CredentialStore::substitute(&self, input: &str) -> Result<ResolvedSecret>` — walks `${VAR_NAME}` patterns, replaces each, handles the `$${X}` escape (first `$` is literal), builds a `ResolvedSecret`
- `ResolvedSecret` opaque struct:
  - `inner: String` field (private)
  - `as_bearer_header(&self) -> String` — `format!("Bearer {}", self.inner)`
  - `as_url(&self) -> String` — clone inner
  - **NO `Debug` impl, NO `Display` impl, NO `Serialize` impl, NO `Clone` impl**
  - A custom `Drop` impl that zeroizes the inner String on drop (best-effort — Rust's `String` doesn't guarantee zeroization, but `self.inner.clear()` is a minimum)
- The file path resolver uses the spec's platform table (`~/Library/Application Support/wire-node/.credentials` on macOS, etc.) — check `directories-next` or the existing app-data-dir logic in the repo for the helper
- Add tests: load/save round trip, permission refusal, missing-variable error, escape sequence, atomic write crash-safety (simulate by writing to temp and checking temp exists)

### 2. Provider module (`src-tauri/src/pyramid/provider.rs` — NEW)

Implement the `LlmProvider` trait from the spec exactly. Include these implementations:

- **`OpenRouterProvider`** — the existing behavior ported into the trait. Headers include `Authorization: Bearer {resolved}`, plus `X-OpenRouter-Title` (canonical name — `X-Title` is the legacy alias, use the canonical in new code), `X-OpenRouter-Categories`, `HTTP-Referer`. Response parser pulls out `id` (generation_id for cost tracking), `choices[0].message.content`, `usage.prompt_tokens`, `usage.completion_tokens`, `usage.total_tokens`, `usage.cost` (the authoritative synchronous cost field). `augment_request_body` adds the `trace` object with `build_id`/`step_name`/`slug`/`depth` plus `session_id` and `user` per the spec. `detect_context_window` returns `None` for OpenRouter (the context limit comes from the `/models` endpoint via `pyramid_tier_routing.context_limit`, not from a per-call detection).

- **`OpenAiCompatProvider`** — for Ollama local and any other OpenAI-compatible endpoint. Headers only include `Authorization` if `api_key_ref` is set (Ollama local has no auth; headers silently ignored). Response parser uses standard OpenAI format. `usage.cost` is NOT present (local = free) so the cost tracker records `actual_cost = 0.0` for this provider type. `detect_context_window` calls Ollama's `POST /api/show` with the model name per the spec's algorithm:
  1. Read `model_info["general.architecture"]` → e.g., `"gemma3"`
  2. Read `model_info["<arch>.context_length"]` → e.g., `131072`
  3. Fallback: scan `model_info` keys for any ending in `.context_length`, take the first match
  4. If all fail (model not pulled yet, network error), return `None` and let the user specify manually

- **`OllamaCloudProvider`** — OPTIONAL for Phase 3. Skip if scope pressure is real; document in the implementation log as "Phase 10 scope" per the spec's UI-focused Ollama management section.

The trait methods are: `name`, `chat_completions_url`, `prepare_headers(&self, secret: &ResolvedSecret)`, `parse_response(&self, body: &str)`, `supports_response_format`, `supports_streaming`, `detect_context_window`, `augment_request_body`.

### 3. Provider registry + schema (`src-tauri/src/pyramid/db.rs` extensions)

Add to `init_pyramid_db`:

- `pyramid_providers` table per the spec's schema
- `pyramid_tier_routing` table per the spec's schema — including the full `pricing_json` field with the OpenRouter string-encoded price format ("0.0000015" means $0.0000015 per token, per million divide)
- `pyramid_step_overrides` table per the spec's schema

Add helpers alongside:
- `get_provider(conn, id) -> Result<Option<Provider>>`
- `save_provider(conn, &Provider) -> Result<()>`
- `list_providers(conn) -> Result<Vec<Provider>>`
- `get_tier_routing(conn) -> Result<HashMap<String, TierRoutingEntry>>`
- `save_tier_routing(conn, &TierRoutingEntry) -> Result<()>`
- `get_step_override(conn, slug, chain_id, step_name, field_name) -> Result<Option<StepOverride>>`
- `save_step_override(conn, &StepOverride) -> Result<()>`

**Default seeding** runs on first init (check if `pyramid_providers` is empty; if so, insert the default OpenRouter row + the 4 tier routing entries listed above; do NOT seed `stale_local`).

### 4. `llm.rs` refactor

Replace the hardcoded URL + OpenRouter-specific parsing with provider-trait-based dispatch. The caller pattern changes from:

```rust
call_model_with_usage(&config, system_prompt, user_prompt, model, temperature, max_tokens).await
```

to something like:

```rust
call_model_via_registry(
    &registry,
    &credentials,
    tier_or_override,
    system_prompt,
    user_prompt,
    request_metadata,
).await
```

The registry resolves tier → provider + model + pricing + context_limit → builds headers via `credentials.resolve_var(provider.api_key_ref)` → calls the provider trait methods → parses the response uniformly.

**Keep `LlmConfig` for now** as a compatibility shim. Do NOT delete it. The refactor replaces the CALL path, not the config struct. Future phases (4, 6) may further refactor the struct. Your job is to make the call path provider-registry-aware without a full rewrite of every caller's argument list.

For the hardcoded temperature/max_tokens scattered throughout (`0.2, 4096`, `0.1, 2048`, etc.) — these stay for Phase 3. They're Pillar 37 violations but moving them to config flows is Phase 4/6 scope (config contributions + LLM output cache). Leave a comment at one representative call site noting the Phase 4 TODO.

### 5. Update existing call sites

Thread `ProviderRegistry` + `CredentialStore` references through the call chain so every call to `call_model_with_usage` (and similar) reaches the new provider-aware path. The call sites you'll touch include (grep to find current count):

- `chain_executor.rs` — the main chain execution engine; multiple call sites
- `stale_helpers_upper.rs` — Phase 2 just added `generate_change_manifest` which uses `call_model_with_usage`
- `build.rs` — legacy build path
- `evidence_answering.rs` — evidence loop
- `characterize.rs` — characterization step
- `question_compiler.rs` — question compilation
- `question_decomposition.rs` — question decomposition

**Architectural choice:** you probably want `LlmConfig` to carry an `Arc<ProviderRegistry>` and `Arc<CredentialStore>` so the call sites that already take `&LlmConfig` get the new capabilities "for free" without changing their signatures. If that's too invasive, an alternative is to add a new `LlmCtx` struct containing both and pass it alongside the existing `LlmConfig`. Pick the cleanest approach and document in the log.

### 6. IPC endpoints

Add to `main.rs` or `routes.rs` (match the pattern of existing pyramid IPC commands):

**Credentials:**
- `pyramid_list_credentials` — returns key list with masked previews (first 4 + last 4 chars visible, middle masked)
- `pyramid_set_credential` — writes a new credential or rotates an existing one
- `pyramid_delete_credential` — removes a credential, returns affected configs
- `pyramid_credentials_file_status` — returns path, exists, mode, safe
- `pyramid_fix_credentials_permissions` — chmod 600
- `pyramid_credential_references` — cross-reference which configs use which credentials

**Providers:**
- `pyramid_list_providers` — returns all rows from `pyramid_providers`
- `pyramid_save_provider` — insert/update
- `pyramid_delete_provider` — remove (with confirmation logic in caller)
- `pyramid_test_provider` — makes a minimal call to the provider's `/models` or equivalent, surfaces clear error if credential missing
- `pyramid_get_tier_routing` — returns all tier routing entries
- `pyramid_save_tier_routing` — update a tier entry
- `pyramid_get_step_overrides` — returns overrides for a (slug, chain_id)
- `pyramid_save_step_override` — upsert an override
- `pyramid_delete_step_override` — remove

### 7. Tests

- Credentials: `load_saves_round_trip`, `rejects_wide_permissions`, `substitutes_simple_var`, `substitutes_multiple_vars`, `handles_escape_sequence`, `missing_var_error`, `atomic_write_not_partial`, `resolved_secret_has_no_debug` (compile-time check via `::<dyn Debug>::new` pattern — or a `static_assertions` crate check; or just document that it's type-enforced and skip the runtime test)
- Provider: `openrouter_headers_include_bearer`, `openrouter_parses_usage_cost`, `openai_compat_no_auth_when_no_ref`, `ollama_detect_context_window_parses_arch_prefix`, `pricing_json_parses_string_values`, `request_metadata_augments_trace`
- Registry: `init_seeds_default_providers_on_empty_db`, `init_does_not_reseed_populated_db`, `tier_routing_overrides_chain_defaults`, `step_override_takes_precedence_over_tier`, `resolve_tier_missing_provider_error`
- End-to-end: `call_via_registry_uses_correct_provider` (mock the HTTP layer or use a local test server)

## Scope boundaries

**In scope:**
- `.credentials` file + `ResolvedSecret`
- Variable resolver with `${VAR_NAME}` and `$${X}` escape
- `LlmProvider` trait + OpenRouter + OpenAI-compat implementations
- Three new DB tables + CRUD helpers
- Default seeding with Adam's model slugs
- `llm.rs` refactor to provider-trait path
- IPC endpoints for credentials + providers + tier routing + step overrides
- Tests for all the above
- Implementation log entry

**Out of scope (do NOT touch):**
- Ollama model management UI (Phase 10 — ToolsMode integration)
- OllamaCloudProvider (optional; defer to Phase 10 if scope pressure)
- Pricing table prefetch from `/api/v1/models` (Phase 14 — wire discovery ranking)
- Model ID verification at seed time (Adam confirmed the slugs are correct; do NOT hit `/models` to validate them)
- Hardcoded temperature/max_tokens cleanup (Phase 4/6 scope)
- Dry-run publish credential scan (Phase 5 scope)
- ToolsMode credential warnings (Phase 10 scope)
- Settings.tsx UI (Phase 10 scope — keep IPC endpoints minimal, no React work)
- Any changes to Phase 0b/1/2 code unless strictly necessary for the refactor
- The existing 7 pre-existing test failures (`test_evidence_pk_cross_slug_coexistence`, etc.) — do NOT fix

## Verification criteria

1. `cargo check`, `cargo build` from `src-tauri/` — clean, zero new warnings in files you touched.
2. `cargo test --lib pyramid::credentials` — new test module, 8+ tests passing.
3. `cargo test --lib pyramid::provider` — new test module, 6+ tests passing.
4. `cargo test --lib pyramid::db::tests::provider_registry` — 4+ registry seeding/migration tests passing.
5. `cargo test --lib pyramid` — overall suite: existing 800 tests + your new Phase 3 tests all pass. Same 7 pre-existing failures as before. NO new failures.
6. `grep -n "https://openrouter.ai/api/v1/chat/completions"` in `src-tauri/src/` — should only match `provider.rs` (the one place that encodes the OpenRouter base URL as a trait impl default). Should NOT match `llm.rs` or any other file.
7. `grep -n "as_bearer_header\|ResolvedSecret" src-tauri/src/pyramid/credentials.rs` — confirms the opacity helpers exist.
8. Manual check: on your local dev run (if feasible), verify that `.credentials` file is created with 0600 mode on first run, and that the app starts without errors even if no credentials are defined (it should only fail at the point where a chain tries to make a call that needs a missing credential).

## Deviation protocol

Standard protocol — friction log + `> [For the planner]` block for anything the spec doesn't anticipate. The most likely deviation areas:

- **Arc threading depth.** If `Arc<ProviderRegistry>` needs to go through 10+ function signatures to reach deep call sites, that's a signal the wrapper struct approach (`LlmCtx`) is cleaner. Flag it, pick one, document.
- **Legacy `LlmConfig` field migration.** If the existing struct has fields that don't cleanly map to the new provider model, surface them. Do not silently drop any. Add a migration comment.
- **Request body format divergence.** If the OpenRouter response parser you port from `llm.rs` doesn't match the spec's pricing schema expectations (e.g., the `usage.cost` field is optional and missing on some responses), handle it defensively and log.
- **Default seed idempotency.** First-run seeding must not reseed on every startup. Use an explicit check (`COUNT(*) = 0` on `pyramid_providers`) or a seed marker row.

## Implementation log protocol

Append a Phase 3 entry in `docs/plans/pyramid-folders-model-routing-implementation-log.md`. Document:

- Started / Completed timestamps
- Files touched (new files + modified files with brief descriptions)
- Spec adherence per section: credentials file + ResolvedSecret, variable resolver, LlmProvider trait, OpenRouter impl, OpenAI-compat impl, schema, seeding, llm.rs refactor, IPC endpoints, tests
- Scope decisions: whether you shipped OllamaCloudProvider or deferred to Phase 10; whether you used Arc threading or LlmCtx wrapper; any other judgment calls
- Verification results
- Status: `awaiting-verification`

## Mandate

- **Correct before fast.** Credentials are security-critical — don't cut corners on the opacity contract, permission checks, or atomic writes.
- **Right before complete.** The provider trait MUST cleanly replace the hardcoded OpenRouter URL. If you find yourself writing "TODO: refactor this later" in `llm.rs`, stop and think — are you actually done with the refactor?
- **No new scope.** Ollama UI is Phase 10. Dry-run publish is Phase 5. Don't gold-plate.
- **Pillar 37 awareness.** Existing hardcoded temperature/max_tokens are pre-existing and stay for Phase 3 (Phase 4/6 fixes them uniformly). Your new code MUST NOT introduce any new LLM-constraining hardcoded numbers.
- **Fix all bugs found.** If you spot an adjacent bug, fix it per the repo convention and note in the friction log.
- **Commit when done.** Single commit with message `phase-3: provider registry + credentials`. Body: 5-7 lines summarizing the credential store, provider trait, schema additions, llm.rs refactor, IPC endpoints, tests. Do not amend. Do not push.

## End state

Phase 3 is complete when:

1. `src-tauri/src/pyramid/credentials.rs` exists with `CredentialStore`, `ResolvedSecret`, substitution, atomic writes, 0600 enforcement.
2. `src-tauri/src/pyramid/provider.rs` exists with `LlmProvider` trait + `OpenRouterProvider` + `OpenAiCompatProvider` implementations.
3. `pyramid_providers`, `pyramid_tier_routing`, `pyramid_step_overrides` tables exist in `init_pyramid_db`.
4. First-run seeding inserts the default OpenRouter provider row + 4 tier routing entries (using Adam's exact slugs) but NOT `stale_local`.
5. `llm.rs` no longer contains `https://openrouter.ai/api/v1/chat/completions` — the URL lives in `provider.rs` as part of `OpenRouterProvider`'s trait impl.
6. Every existing call site that previously used the hardcoded OpenRouter path now goes through the provider trait.
7. IPC endpoints for credentials + providers + tier routing + step overrides exist and are wired up.
8. `cargo check`, `cargo build`, `cargo test --lib pyramid` all pass with existing failures unchanged.
9. Implementation log Phase 3 entry is complete.
10. Single commit on branch `phase-3-provider-registry-credentials`.

Begin with the reading. The two specs are the implementation contract — read them end-to-end before touching code.

Good luck. Build carefully. Take the time you need.
