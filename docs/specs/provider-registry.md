# Provider Registry Specification

**Version:** 1.0
**Date:** 2026-04-09
**Status:** Design — pre-implementation
**Depends on:** Nothing (foundational)
**Unblocks:** Per-step model routing, local compute mode, OpenRouter Broadcast, evidence triage, YAML-to-UI renderer dynamic options
**Authors:** Adam Levine, Claude (session design partner)

---

## Overview

Replace the hardcoded OpenRouter URL in `llm.rs` with a **provider registry** — a table of compute backends (OpenRouter, Ollama, any OpenAI-compatible API) each with their own base URL, auth, and capabilities. The existing `model_aliases` HashMap becomes a **tier routing table** that maps each tier to a provider+model pair.

Chain YAMLs already declare `model_tier` per step. The YAML doesn't change. What changes is that the tier resolves through the provider registry instead of a hardcoded switch statement.

---

## Current State

### What's Hardcoded (llm.rs)

| Location | What | Value |
|----------|------|-------|
| `llm.rs:312` | API URL | `"https://openrouter.ai/api/v1/chat/completions"` |
| `llm.rs:354` | Auth header | `Bearer {api_key}` |
| `llm.rs:355-356` | Custom headers | `HTTP-Referer: newsbleach.com`, `X-Title: Wire Pyramid Engine` (note: `X-Title` is an alias; the canonical current name is `X-OpenRouter-Title`. There is also `X-OpenRouter-Categories` for marketplace listing. Both are attribution-only, no rate-limit tier impact.) |
| `llm.rs:119-121` | Default models | `inception/mercury-2`, `qwen/qwen3.5-flash-02-23`, `x-ai/grok-4.20-beta` — **these may be stale** (Mercury 2.7 is the current known version; the Qwen slug and Grok date-suffix are suspicious). **MUST verify against `GET /api/v1/models` before shipping.** Current strong candidates for knowledge extraction: `google/gemini-2.5-flash` (fast/cheap/smart), `anthropic/claude-sonnet-4-5` (quality), `deepseek/deepseek-chat-v3-0324` (cost-efficient long context). Always pull live from the models endpoint rather than hardcoding. |
| `llm.rs:212-260` | Response parsing | `parse_openrouter_response_body()` — OpenRouter JSON envelope |
| `llm.rs:885` | Direct call URL | Same hardcoded URL |

### What's Already Parameterized

| Location | What | How |
|----------|------|-----|
| `llm.rs:111` | `model_aliases` | `HashMap<String, String>` — defined but never used |
| `llm.rs:119-121` | Model cascade | `primary_model`, `fallback_model_1`, `fallback_model_2` — configurable |
| `llm.rs:122-123` | Context limits | `primary_context_limit`, `fallback_1_context_limit` — per-model |
| `chain_engine.rs:149` | Per-step tier | `model_tier: Option<String>` on `ChainStep` |
| `chain_engine.rs:113` | Chain defaults | `model_tier: String` on `ChainDefaults` |

---

## Architecture

### Three Levels of Model Resolution

```
Chain YAML step          Tier Routing Table         Provider Registry
┌─────────────────┐     ┌───────────────────────┐  ┌──────────────────────┐
│ model_tier: web  │ ──► │ web → openrouter /     │ ─► │ openrouter:           │
│                  │     │       mercury-2         │  │   base_url: openrout..│
│                  │     │                         │  │   api_key: sk-or-...  │
│                  │     │ synth_heavy → openrout..│  │   type: openrouter    │
│                  │     │              / m2.7     │  │                       │
│                  │     │                         │  │ ollama-local:         │
│                  │     │ stale_local → ollama /  │  │   base_url: localho.. │
│                  │     │              gemma3:27b │  │   type: openai_compat │
└─────────────────┘     └───────────────────────┘  └──────────────────────┘
```

1. **Step declares `model_tier`** (or inherits from chain defaults)
2. **Tier routing table** maps tier name → provider name + model ID
3. **Provider registry** maps provider name → base URL, auth, capabilities, response format

### Per-Step Override

Users can override the tier for any individual step via the YAML-to-UI renderer (stored in DB). Resolution order:

1. Per-step DB override (user explicitly set this step to a specific provider+model)
2. Tier routing table (tier name → provider+model)
3. Chain defaults
4. Global defaults

---

## Provider Trait

```rust
/// A compute backend that can handle LLM inference requests.
pub trait LlmProvider: Send + Sync {
    /// Provider display name (e.g., "OpenRouter", "Ollama")
    fn name(&self) -> &str;

    /// Full chat completions endpoint URL
    fn chat_completions_url(&self) -> String;

    /// Build HTTP headers for authentication and provider-specific requirements
    fn prepare_headers(&self, api_key: &str) -> Vec<(String, String)>;

    /// Parse the provider's response body into a unified format.
    /// Returns (content, token_usage, generation_id)
    fn parse_response(&self, body: &str) -> Result<(String, TokenUsage, Option<String>)>;

    /// Whether this provider supports `response_format` in the request body
    fn supports_response_format(&self) -> bool;

    /// Whether this provider supports streaming (SSE)
    fn supports_streaming(&self) -> bool;

    /// Auto-detect context window for a model (e.g., Ollama /api/show)
    /// Returns None if detection is not supported.
    async fn detect_context_window(&self, model: &str) -> Option<usize>;

    /// Provider-specific request body modifications (e.g., OpenRouter trace metadata)
    fn augment_request_body(&self, body: &mut serde_json::Value, metadata: &RequestMetadata);
}
```

### Built-in Implementations

#### OpenRouterProvider
- `chat_completions_url()`: `"{base_url}/chat/completions"` (confirmed in OpenRouter quickstart)
- Headers: `Authorization: Bearer {key}`, plus optional attribution headers:
  - `X-OpenRouter-Title: <app name>` — canonical current name per OpenRouter quickstart. `X-Title` is an accepted alias for backward compatibility but new code should use the canonical name.
  - `X-OpenRouter-Categories: <category>` — optional, for marketplace listing
  - `HTTP-Referer: <app URL>` — optional, still supported per the quickstart
  - All three are attribution-only (leaderboard rankings on openrouter.ai). None affect rate-limit tier assignment.
- Response parsing: existing `parse_openrouter_response_body()` logic. **Key fields to extract:**
  - `id` — the generation ID (format: `gen-xxxxxxxxxxxxxx`), stored as `pyramid_cost_log.generation_id`
  - `choices[0].message.content` — completion text
  - `usage.prompt_tokens`, `usage.completion_tokens`, `usage.total_tokens` — token counts
  - **`usage.cost` — actual cost in USD (authoritative, available synchronously in the response)**
  - `usage.cost_details.upstream_inference_prompt_cost` and `usage.cost_details.upstream_inference_completions_cost` — optional breakdown
- `supports_response_format()`: per-model (check the model's `supported_parameters` array from `/models`). OpenRouter **silently ignores** `response_format` on models that don't support it — it does not error. The `response-healing` plugin can enforce valid JSON if needed.
- `detect_context_window()`: `None` (use `/models` endpoint data — `context_length` field reflects **total context budget (input + output combined)**, not just input)
- `augment_request_body()`: adds OpenRouter-specific fields:
  - `trace` object with custom metadata (build_id, step_name, slug, depth) — passed through to Broadcast destinations
  - `session_id: "{slug}/{build_id}"` — enables deterministic per-build sampling
  - `user: <node_identity>` — for per-node analytics in observability destinations
  - Optionally: `models: [<primary>, <fallback1>, <fallback2>]` and `route: "fallback"` — OpenRouter's own multi-model fallback (alternative to our cross-provider fallback for intra-provider failover)

**OpenRouter-specific request body extras (beyond OpenAI baseline)** that we can optionally use:

| Field | Purpose |
|---|---|
| `models: string[]` | Try models in order (OpenRouter's fallback, intra-provider) |
| `route: "fallback"` | Activate OpenRouter's fallback routing |
| `provider: ProviderPreferences` | Fine-grained upstream routing preferences |
| `plugins: Plugin[]` | Extend with web, file-parser, response-healing, context-compression |
| `session_id: string` | Group related requests (also accepted as `x-session-id` header) |
| `trace: object` | Arbitrary custom metadata for observability |
| `prediction: { type: "content", content: string }` | Predicted output for latency optimization |
| `top_k`, `min_p`, `top_a`, `repetition_penalty` | Extended LLM sampling parameters (beyond OAI baseline) |

#### OpenAiCompatProvider (Ollama, custom OAI endpoints)
- `chat_completions_url()`: `"{base_url}/chat/completions"` (works for Ollama at `/v1/chat/completions`, stable path)
- Headers: `Authorization: Bearer {key}` only if `api_key_ref` is set. Ollama local has no auth (header is silently ignored); production Ollama behind an nginx/Caddy reverse proxy may require a bearer token, so the spec supports an optional `OLLAMA_API_KEY` credential variable.
- Response parsing: standard OpenAI format — `choices[0].message.content`, `usage.prompt_tokens`, `usage.completion_tokens`. **Note**: for Ollama specifically, `usage.cost` is NOT present (local = free). Our cost tracking records `actual_cost = 0` for Ollama calls. Ollama's native metrics (`total_duration`, `eval_count`, etc.) are only returned by the native `/api/generate` endpoint, not the OAI-compat path.
- `supports_response_format()`: `true` for Ollama (confirmed in Ollama OAI-compat docs); configurable per-instance for custom providers.
- `detect_context_window()`:
  - For Ollama: `POST {base_url_without_/v1}/api/show` with body `{"model": "<model>"}`. The response contains a `model_info` object with architecture-prefixed keys. The detection algorithm:
    1. Read `model_info["general.architecture"]` → e.g., `"gemma3"`, `"llama"`, `"qwen2"`
    2. Read `model_info["<arch>.context_length"]` → e.g., `model_info["gemma3.context_length"] = 131072`
    3. Fallback: if step 2 fails, scan `model_info` keys for any ending in `.context_length` and take the first match
  - The response also includes a top-level `capabilities: string[]` array (e.g., `["completion", "vision"]`) which is Ollama's equivalent of OpenRouter's `supported_parameters`. Store this alongside the context length for model-capability gating.
  - Ollama's `/api/show` is a stable native endpoint (OpenAPI 3.1 spec in docs.ollama.com); this is the documented path.
  - Fallback if detection fails entirely (model not pulled yet, network error): prompt the user to enter a context limit manually during provider setup.
  - **Setting context size on Ollama calls**: the OpenAI-compat path (`/v1/chat/completions`) has no way to override the context window — it uses whatever the model was loaded with. To specify context size at request time, use the native `/api/chat` or `/api/generate` endpoint with `options: { num_ctx: <size> }`. Our `OpenAiCompatProvider` defaults to OAI-compat; an `OllamaNativeProvider` variant that uses the native endpoint is available when `num_ctx` override is needed.
- `augment_request_body()`: no-op for local Ollama. For custom OAI-compat providers, inject any provider-specific headers/fields configured in `config_json`.

#### OllamaCloudProvider
Variant of `OpenAiCompatProvider` for Ollama's hosted cloud (`https://ollama.com/api`):

- `chat_completions_url()`: `"https://ollama.com/api/v1/chat/completions"` (conventional; Ollama's cloud API at `ollama.com/api` mirrors the local API shape)
- Headers: `Authorization: Bearer ${OLLAMA_API_KEY}` — required
- Model IDs: append `-cloud` suffix (e.g., `gpt-oss:120b-cloud`) per Ollama's cloud model naming convention
- Auth-required for: running cloud models, publishing models, downloading private models
- API keys don't expire; revocable via Ollama dashboard

---

## Ollama Model Management

Wire Node provides a **first-class Ollama control surface** inside the app so users don't have to drop to terminal for model management. This fits the "ToolsMode is the universal config surface" philosophy: Ollama models become another configurable behavior that lives in the app.

### Capabilities

| Operation | Ollama endpoint | Wire Node surface |
|---|---|---|
| List installed models | `GET /api/tags` | Settings → Providers → Ollama → Installed Models panel |
| Show model details | `POST /api/show` | Click any installed model → detail drawer (context length, capabilities, quantization, family) |
| Pull a new model | `POST /api/pull` (streaming) | "Pull model" button → progress bar with streaming status |
| Unload a model | `POST /api/generate` with `keep_alive: 0` | "Unload" button next to each model |
| Keep-alive control | `options.keep_alive` on any request | Per-provider setting (e.g., "keep loaded for 10 minutes") |

### The Installed Models panel

When the user opens their Ollama provider in Settings, we show every model `/api/tags` returns with:

- **Name** (e.g., `gemma3:27b`)
- **Size on disk** (formatted: e.g., "18.2 GB")
- **Family** + **parameter size** (e.g., "gemma, 27B")
- **Quantization level** (e.g., "Q4_K_M")
- **Context length** (fetched via `/api/show` on first expand)
- **Capabilities** (from `/api/show`'s `capabilities` array: `completion`, `vision`, etc.)
- **Last used** (from our `pyramid_cost_log` history for this model)
- **Actions**: "Use in tier routing" (opens tier editor pre-filled), "Unload", "Delete" (calls `DELETE /api/delete`)

### The Pull Flow

When the user clicks "Pull model":

1. Show a text input for the model name (with autocomplete from Ollama's public library via the community API, if available)
2. On submit, call `POST /api/pull` with `{ "model": "<name>" }` in streaming mode
3. The response is NDJSON with `{ status, digest?, total?, completed? }` per chunk
4. Render a progress bar: `(completed / total) * 100` with the current `status` message ("pulling manifest", "downloading layer sha256:...", etc.)
5. On `status: "success"`, refresh the installed models list
6. The newly pulled model is immediately available for tier routing assignment

### IPC contract for Ollama management

```
GET pyramid_ollama_list_models
  Input: { provider_id: String }
  Output: { models: [{ name, model, modified_at, size, digest, details: { format, family, families, parameter_size, quantization_level } }] }

GET pyramid_ollama_show_model
  Input: { provider_id: String, model: String }
  Output: { parameters, license, modified_at, details, template, capabilities: [String], model_info: Map<String, Value> }

POST pyramid_ollama_pull_model
  Input: { provider_id: String, model: String }
  Output: stream of { status: String, digest?: String, total?: u64, completed?: u64 }
  Note: server-sent events or WebSocket to stream progress to the frontend

POST pyramid_ollama_unload_model
  Input: { provider_id: String, model: String }
  Output: { ok: bool }

POST pyramid_ollama_delete_model
  Input: { provider_id: String, model: String }
  Output: { ok: bool }
  Note: requires confirmation modal in UI (destructive action, reclaims disk space)

POST pyramid_ollama_set_keep_alive
  Input: { provider_id: String, duration_secs: u64 }
  Output: { ok: bool }
  Note: sets a default keep_alive for all requests to this provider (passed as options.keep_alive on each request)
```

### Recommendations engine (optional Phase enhancement)

The Ollama management panel can surface **recommended models** based on:

- The user's evidence_policy (cost-sensitive users see small-quant suggestions; quality-sensitive users see larger models)
- The user's hardware (if we can detect it via OS introspection: RAM, VRAM)
- The tiers their tier_routing references (if tier routing asks for a "synth_heavy" model but none is installed, recommend matching models)

This is a Phase 3+ enhancement; v1 just shows installed + allows pull.

### Cloud model gating

Models with the `-cloud` suffix (e.g., `gpt-oss:120b-cloud`) require authentication to `ollama.com`. The management UI:

- Shows a lock icon next to cloud models
- On "Use in tier routing", checks if `OLLAMA_API_KEY` is set in the credentials file
- If not, prompts: "This is a cloud model. You need to sign in to ollama.com or provide an API key in Settings → Credentials."
- Runs `ollama signin` through a terminal session if the user prefers that path (documented but not automated)

### Disk management

Ollama models can be large (10-50GB each). The management panel shows:

- Total disk used by Ollama models (sum of `size` across installed models)
- A warning banner if disk usage exceeds a configurable threshold (e.g., 80% of available disk)
- Per-model "Delete" actions with confirmation

This gives users the same disk-management visibility they'd have from `ollama list` in the terminal, inside the app.

---

## Storage

### pyramid_providers Table

```sql
CREATE TABLE IF NOT EXISTS pyramid_providers (
    id TEXT PRIMARY KEY,                      -- "openrouter", "ollama-local", "custom-1"
    display_name TEXT NOT NULL,               -- "OpenRouter", "Local Ollama"
    provider_type TEXT NOT NULL,              -- "openrouter", "openai_compat"
    base_url TEXT NOT NULL,                   -- "https://openrouter.ai/api/v1" (may contain ${VAR_NAME})
    api_key_ref TEXT,                         -- null = no auth, otherwise credential variable name (e.g. "OPENROUTER_KEY") — see credentials-and-secrets.md
    auto_detect_context INTEGER DEFAULT 0,    -- 1 = call detect_context_window on model changes
    supports_broadcast INTEGER DEFAULT 0,    -- 1 = provider supports cost broadcast webhooks
    broadcast_config_json TEXT,              -- broadcast-specific config (webhook URL, format)
    config_json TEXT DEFAULT '{}',            -- provider-specific config (custom headers, etc.)
    enabled INTEGER DEFAULT 1,
    created_at TEXT DEFAULT (datetime('now')),
    updated_at TEXT DEFAULT (datetime('now'))
);
```

### pyramid_tier_routing Table

```sql
CREATE TABLE IF NOT EXISTS pyramid_tier_routing (
    tier_name TEXT PRIMARY KEY,              -- "extractor", "web", "synth_heavy", "stale_local"
    provider_id TEXT NOT NULL REFERENCES pyramid_providers(id),
    model_id TEXT NOT NULL,                  -- "inception/mercury-2.7-preview-03", "gemma3:27b" (canonical ID, NOT our internal alias)
    context_limit INTEGER,                   -- total budget (input + output combined); null = auto-detect
    max_completion_tokens INTEGER,           -- from top_provider.max_completion_tokens; output budget cap
    pricing_json TEXT NOT NULL DEFAULT '{}', -- full OpenRouter pricing object (see below)
    supported_parameters_json TEXT,          -- JSON array of supported params (tools, response_format, structured_outputs, etc.)
    notes TEXT,                              -- user notes on why this routing was chosen
    created_at TEXT DEFAULT (datetime('now')),
    updated_at TEXT DEFAULT (datetime('now'))
);
```

**`pricing_json` schema** (mirrors OpenRouter's Models API pricing object):

```json
{
  "prompt": "0.0000015",              // USD per input token (string!)
  "completion": "0.0000060",          // USD per output token (string!)
  "request": "0",                      // fixed cost per API request
  "image": "0.00264",                  // per image input
  "web_search": "0",                   // per web search operation (for models with web plugin)
  "internal_reasoning": "0.0000060",  // for reasoning tokens (reasoning-mode models)
  "input_cache_read": "0.00000015",   // per cached input token read (prompt caching)
  "input_cache_write": "0.0000020"    // per cached input token write
}
```

**Critical parsing notes**:
1. **All values are strings**, not numbers. Use `parseFloat()` / `.parse::<f64>()` before arithmetic.
2. **Per-token, not per-1K-tokens.** A `prompt` of `"0.0000015"` means $0.0000015 per input token (i.e., $1.50 per million).
3. `"0"` means free (local models, promotional models, etc.).
4. `context_length` from the models endpoint is the **total budget** (input + output combined), not just input. When computing `max_tokens` for a call, subtract estimated input from `context_length` to get the available output budget.
5. `top_provider.max_completion_tokens` provides a HARD CAP on output tokens that may be lower than the remaining context budget — respect it.

**`supported_parameters_json` schema** (from OpenRouter's Models API):

```json
["tools", "tool_choice", "max_tokens", "temperature", "top_p", "reasoning", "include_reasoning",
 "structured_outputs", "response_format", "stop", "frequency_penalty", "presence_penalty", "seed"]
```

**Usage**: before setting `response_format`, `tools`, or any extended parameter on a request, check if the model's `supported_parameters_json` includes it. For `response_format` on unsupported models: OpenRouter silently ignores the field (does not error). If strict JSON is required, activate the `response-healing` plugin instead.

### pyramid_step_overrides Table

```sql
CREATE TABLE IF NOT EXISTS pyramid_step_overrides (
    slug TEXT NOT NULL,                       -- pyramid slug
    chain_id TEXT NOT NULL,                   -- chain definition ID
    step_name TEXT NOT NULL,                  -- chain step name
    field_name TEXT NOT NULL,                 -- "model_tier", "temperature", "concurrency", etc.
    value_json TEXT NOT NULL,                 -- JSON-encoded override value
    created_at TEXT DEFAULT (datetime('now')),
    PRIMARY KEY (slug, chain_id, step_name, field_name)
);
```

---

## Default Seeding

On first run (or upgrade), seed the provider registry with the existing configuration:

```sql
-- Default provider (existing behavior)
-- api_key_ref = "OPENROUTER_KEY" references the credentials file (see credentials-and-secrets.md)
INSERT INTO pyramid_providers VALUES (
    'openrouter', 'OpenRouter', 'openrouter',
    'https://openrouter.ai/api/v1', 'OPENROUTER_KEY', 0, '{}', 1, ...
);

-- Default tier routing (matches current model_aliases behavior)
INSERT INTO pyramid_tier_routing VALUES ('extractor', 'openrouter', 'inception/mercury-2', 120000, ...);
INSERT INTO pyramid_tier_routing VALUES ('web', 'openrouter', 'inception/mercury-2', 120000, ...);
INSERT INTO pyramid_tier_routing VALUES ('synth_heavy', 'openrouter', 'inception/mercury-2', 120000, ...);
INSERT INTO pyramid_tier_routing VALUES ('mid', 'openrouter', 'inception/mercury-2', 120000, ...);
```

The existing `LlmConfig` fields (`primary_model`, `fallback_model_1`, etc.) become the fallback cascade within a single provider. The tier routing table replaces `model_aliases`.

---

## Local Compute Mode

A single toggle: "Use local models (Ollama)" that:

1. Checks Ollama is reachable at the configured base_url
2. Lists available models via `GET {base_url}/api/tags`
3. Auto-detects context window for the selected model via `GET {base_url}/api/show`
4. Sets ALL tier routing entries to the local provider + detected model
5. Derives dehydration budgets from detected context limit
6. Sets concurrency to 1 (home hardware constraint)

When toggled off, restores the previous tier routing (stored before toggle was activated).

If auto-detection fails, fall back to user-specified context limit with a warning.

---

## Credential Variable References

The `api_key_ref` column in `pyramid_providers` stores a **credential variable name** (e.g., `"OPENROUTER_KEY"`), not a literal key or sentinel value. The provider resolver looks up the credential at runtime from the credentials store defined in `credentials-and-secrets.md`.

### Runtime resolution

```rust
fn build_provider_headers(provider: &Provider, creds: &CredentialStore) -> Result<Vec<Header>> {
    let mut headers = vec![];
    if let Some(key_ref) = &provider.api_key_ref {
        let secret = creds.resolve_var(key_ref)?;   // ResolvedSecret
        headers.push(("Authorization", secret.as_bearer_header()));
    }
    Ok(headers)
}
```

`resolve_var()` walks the local `.credentials` file, returns a `ResolvedSecret` on success, and returns a clear "variable not defined" error on failure. The `ResolvedSecret` wrapper prevents the credential value from appearing in logs, debug dumps, or any serialization path — see `credentials-and-secrets.md` for the opacity semantics.

The provider's `base_url` field may also contain `${VAR_NAME}` patterns for environments where the URL itself is a secret (for example, a self-hosted Ollama endpoint at a private address). The resolver walks any string-valued provider field that may contain `${...}` and substitutes variables the same way.

### Wire-shareable providers

Because `api_key_ref` is a variable name rather than a literal secret, stored provider configs are portable. A shared provider config says "auth from `OPENROUTER_KEY`"; every recipient resolves that variable against their own credentials file. No secret ever leaves the authoring user's machine. The `pyramid_dry_run_publish` flow (see `wire-contribution-mapping.md`) scans the config YAML for `${...}` patterns and warns the publisher which credentials the recipient will need.

### Migration of legacy `"settings"` sentinel

Legacy rows where `api_key_ref = "settings"` (pre-credentials-spec) are migrated on first run after this spec ships:

1. Read the API key from the legacy `LlmConfig` on-disk settings file
2. Write the key to `.credentials` under `OPENROUTER_KEY` (or the provider-appropriate name derived from `provider.id`)
3. Rewrite the provider's `api_key_ref` column to the new variable name
4. Clear the key field from the legacy settings file

After migration, no provider row should carry the literal `"settings"` value.

### Provider test endpoint

`POST /api/providers/{id}/test` resolves credentials at test time. A failed test because the credential variable isn't defined surfaces the specific missing-credential error ("Provider references `${OPENROUTER_KEY}` but your credentials file doesn't define it — set it in Settings → Credentials") rather than a generic "auth failed", so the user knows where to fix it.

---

## Cross-Provider Fallback

When a provider fails — for example, OpenRouter is down, or the user's OpenRouter credit balance is exhausted — the executor should fall back to another provider and model, not just to another model within the same provider. This spec defines pipeline-wide fallback chains with site-specific overrides.

### Fallback criteria

A provider call is considered to have failed (and triggers fallback) on any of:

- HTTP 5xx responses from the provider
- Connection errors (DNS, TCP, TLS)
- Rate limit responses (HTTP 429) after exhausting the per-provider retry budget
- Auth failures (HTTP 401/403) — these trigger immediate fallback, no retry
- Response parse errors when the provider returns malformed content after the retry budget

Transient errors (timeouts with retries remaining) do not trigger fallback — they retry within the current provider first.

### Pipeline-wide fallback chains

A `tier_routing` contribution can specify a `fallback_chain` field per tier, listing provider+model pairs to try in order when the primary fails:

```yaml
tier_routing:
  synth_heavy:
    primary: { provider: openrouter, model: m2.7 }
    fallback_chain:
      - { provider: anthropic, model: claude-opus-4-6 }
      - { provider: ollama-local, model: gemma3:27b }
```

The executor walks the chain on failure: primary -> fallback[0] -> fallback[1] -> ... until one succeeds or the chain is exhausted. If the chain is exhausted the chain step fails with a `FallbackExhausted` error, and the chain executor applies its normal step-failure handling (retry, abort, emit `NodeFailed`).

### Site-specific overrides

`pyramid_step_overrides` contributions can override the fallback chain for specific `(slug, chain_id, step_name)` scopes. The override structure mirrors the tier_routing entry:

```yaml
step_overrides:
  - slug: my-pyramid
    chain_id: code_pyramid
    step_name: deep_synthesis
    field_name: fallback_chain
    value:
      - { provider: anthropic, model: claude-opus-4-6 }
```

Resolution order for the fallback chain at call time:

1. Per-step override (site-specific) if present
2. Tier routing `fallback_chain` for the step's tier
3. Empty chain (no fallback — the primary failure surfaces as-is)

### Cost tracking attribution

When a fallback cascade is triggered, cost tracking attributes the final successful call's token usage and cost to the chain step, not the failed attempts. The failed attempts are logged separately in `pyramid_cost_log` with `status = "failed"` and zero cost (assuming the provider doesn't bill failed requests). For providers that do bill failed requests (e.g., rate-limit responses with partial charge), the cost log records the full attempt history.

### Build visualization event

When a fallback triggers, the executor emits a `ProviderFailover` event into the `LayerEvent` channel. The event payload:

```rust
ProviderFailover {
    build_id: String,
    step_name: String,
    failed_provider: String,
    failed_model: String,
    failure_reason: String,        // "http_502", "timeout", "auth_failed", etc.
    next_provider: String,
    next_model: String,
    chain_position: u32,           // 0 = first fallback, 1 = second, etc.
}
```

The frontend `PyramidBuildViz.tsx` subscribes to this event and renders a visual indicator on the affected step — a small "failover" badge with the failed provider crossed out and the successor shown. The build log records the event with the failure reason so users can diagnose provider issues after the fact. See `build-viz-expansion.md` for the full event channel definition — the `ProviderFailover` variant is added to the `LayerEvent` enum alongside the existing node-level events.

### Credential-aware fallback

Fallback respects credential availability: a fallback entry is skipped if its provider's `api_key_ref` is not defined in the user's credentials file, moving to the next chain entry without attempting the call. A warning is logged so the user knows the fallback chain is partially unavailable. If every fallback entry is unavailable due to missing credentials, the error surfaces as "fallback chain exhausted (credentials missing for: anthropic, azure-openai)".

---

## Resolution Flow in call_model_unified

Current flow:
```
model_tier (from step/defaults) → hardcoded switch → model name → hardcoded URL → call
```

New flow:
```
model_tier (from step/defaults)
    → check pyramid_step_overrides (per-slug, per-step)
    → resolve via pyramid_tier_routing → (provider_id, model_id)
    → load provider from pyramid_providers
    → provider.chat_completions_url() → URL
    → provider.prepare_headers() → headers
    → provider.augment_request_body() → trace metadata
    → HTTP call
    → provider.parse_response() → unified LlmResponse
```

The cascade logic (primary → fallback_1 → fallback_2 based on context limits) stays within a provider. Context limits come from tier routing table or auto-detection.

---

## API Endpoints

```
GET  /api/providers                    — list all providers
POST /api/providers                    — add a provider
PUT  /api/providers/{id}               — update a provider
DELETE /api/providers/{id}             — remove a provider

GET  /api/tier-routing                 — list all tier routes
PUT  /api/tier-routing/{tier}          — set tier routing

POST /api/providers/{id}/test          — test connectivity (send a trivial prompt)
POST /api/providers/{id}/detect-models — list available models (Ollama: /api/tags)

POST /api/local-mode/enable            — toggle local mode on
POST /api/local-mode/disable           — toggle local mode off
GET  /api/local-mode/status            — current local mode state
```

---

## Cost Estimation and Tracking Data Flow

**Critical finding**: OpenRouter returns actual cost directly in the chat completions response body at `usage.cost` (USD). This changes our architecture — we do NOT depend on async Broadcast for primary cost tracking. The response body is authoritative and available synchronously.

### Primary cost path (synchronous, in-response)

```
Build starts
  -> Chain executor resolves model_tier for each step via tier routing
  -> Tier routing provides (provider, model, pricing_json, supported_parameters_json)
  -> Before LLM call: compute ESTIMATE via
       prompt_cost  = parseFloat(pricing.prompt) * estimated_input_tokens
       completion_cost = parseFloat(pricing.completion) * estimated_output_tokens
       estimate = prompt_cost + completion_cost + parseFloat(pricing.request)
  -> Write INSERT to pyramid_cost_log:
       { estimated_cost: <estimate>, generation_id: NULL, actual_cost: NULL }
  -> Make LLM call (synchronous)
  -> Parse response body:
       generation_id = response.id             // "gen-xxxxxxxxxxxxxx"
       actual_cost   = response.usage.cost      // number, USD — AUTHORITATIVE
       prompt_tokens = response.usage.prompt_tokens
       completion_tokens = response.usage.completion_tokens
  -> UPDATE pyramid_cost_log:
       { generation_id, actual_cost, actual_tokens_in, actual_tokens_out,
         reconciled_at: now(), reconciliation_status: "synchronous" }
  -> If abs(actual_cost - estimated_cost) / estimated_cost > policy.discrepancy_ratio:
       emit CostReconciliationDiscrepancy event (see evidence-triage-and-dadbear.md fail-loud rules)
```

This means every OpenRouter call has reconciled cost before the LLM response is even handed back to the caller. The `reconciliation_status` column distinguishes `"synchronous"` (from response body), `"broadcast"` (from webhook), `"generation_api"` (from `/api/v1/generation`), and `"estimated"` (never reconciled).

### Secondary cost paths

- **`GET /api/v1/generation?id=<gen-id>`** — OpenRouter has a dedicated endpoint that returns `native_tokens_prompt`, `native_tokens_completion`, `total_cost`, and more for any past generation. Use this path when:
  - The response body parse failed mid-stream and we only captured the generation_id
  - An audit job needs to re-verify historical rows against the provider's view
  - Community observation: available within seconds of completion; implement brief polling retry (e.g., 3 attempts at 1-second intervals) before accepting the row as unreconcilable
  - This is a confirmed endpoint, not async pushed

- **Broadcast** — **required** async integrity confirmation layer (see `evidence-triage-and-dadbear.md` Part 4). Every synchronous reconciliation expects a matching broadcast within the grace period. Missing confirmations trigger leak detection alerts. Broadcast is NOT optional — it's how we detect credential compromise and provider-side accounting drift. Users who explicitly opt out (`dadbear_policy.cost_reconciliation.broadcast_required: false`) see a persistent "Leak detection disabled" banner in the oversight page.

### For the YAML-to-UI renderer cost display

```
Renderer calls yaml_renderer_estimate_cost(tier_name, step_name)
  -> Backend resolves tier -> (provider, model, pricing_json)
  -> Parse pricing.prompt and pricing.completion as f64
  -> Compute avg_input_tokens and avg_output_tokens from historical pyramid_cost_log
     for this (slug, step_name) over the last 10 completed builds
  -> If no history: fall back to seed defaults (2000 input, 1000 output)
  -> Return estimated cost per call = prompt_rate * avg_in + completion_rate * avg_out + request_fee
```

### For Ollama / local

```
-> pricing = { "prompt": "0", "completion": "0", "request": "0", ... }
-> Actual cost: 0 (local compute)
-> pyramid_cost_log.actual_cost = 0, reconciliation_status = "synchronous_local"
-> Cost display shows "$0.00 (local)" badge
-> Token counts from response are still recorded for observability even though cost is zero
```

### Pricing data sourcing

- **Seeded from `GET /api/v1/models`** on first provider setup. Full `pricing` object for every model cached in `pyramid_tier_routing.pricing_json`.
- **Refreshed on user action** ("refresh models" button in Settings) or on a daily schedule (no pricing-change webhooks exist on OpenRouter's side).
- **Custom OAI-compat providers**: user enters pricing manually during provider setup, or leaves blank for zero (treated as local).
- **Ollama**: all pricing fields default to `"0"`.

---

## OpenRouter Account State

### Credit balance

Endpoint: `GET https://openrouter.ai/api/v1/credits`

**Important**: this endpoint requires a **Management API key**, which is a different key type from the standard inference `sk-or-v1-...` keys. Users must create a separate Management API key in the OpenRouter dashboard and store it in `.credentials` as a separate variable (e.g., `OPENROUTER_MANAGEMENT_KEY`).

Response shape (inferred from community docs, verify before relying):

```json
{
  "data": {
    "total_credits": <number>,
    "total_usage": <number>
  }
}
```

Used by the provider resolver to gate the "credits exhausted → cross-provider fallback" path. If the Management API key isn't configured, the balance check is skipped and we fall back on 4xx responses from inference calls instead (less proactive but still works).

### Rate limit handling

OpenRouter does **not** publicly document specific `X-RateLimit-*` response headers. Our strategy:

- Watch for HTTP 429 responses
- Exponential backoff per the existing `call_model_unified` retry logic
- After exhausting retries within a provider, trigger cross-provider fallback (see "Cross-Provider Fallback" section above)
- Do NOT pre-emptively read rate limit headers as a primary throttling strategy — rely on reactive 429 handling

---

## Prompt Hash Computation

For cache key computation (see llm-output-cache spec), `prompt_hash` is computed as SHA-256 of the instruction file's content at build start time. File content is read and hashed once per build, cached in `ChainContext.prompt_hashes: HashMap<String, String>` for the duration of the build. Changes to prompt files between builds produce different hashes (cache miss). Changes to prompt files mid-build have no effect until the next build (build-scoped).

---

## Migration Path

1. Add `pyramid_providers` and `pyramid_tier_routing` tables
2. Initialize the credentials store and migrate any legacy `LlmConfig` API key into `.credentials` under an appropriate variable name
3. Seed with default OpenRouter provider referencing `api_key_ref = "OPENROUTER_KEY"` and tier mappings from current defaults
4. Add `ProviderResolver` that loads provider + tier routing at startup and resolves credentials via the credentials store
5. Refactor `call_model_unified` to use `ProviderResolver` instead of hardcoded URL
6. Add provider management UI (or reuse `PyramidSettings.tsx` with new sections)
7. Remove hardcoded URL and provider-specific logic from `llm.rs`
8. Add cross-provider fallback support to `call_model_unified` — walk the `fallback_chain` on failure, emit `ProviderFailover` events

### Backward Compatibility

- Existing API keys in `LlmConfig` are migrated into `.credentials` on first run
- Existing `model_aliases` entries are migrated to `pyramid_tier_routing`
- If no provider registry exists (fresh install), the bootstrap path seeds the default OpenRouter provider + tier mappings with bundled contributions

---

## Open Questions

1. **API key storage**: RESOLVED. Provider API keys live in the `.credentials` file (see `credentials-and-secrets.md`), referenced by variable name in the `api_key_ref` column. This keeps secrets out of SQLite and makes provider configs Wire-shareable.

2. **Fallback cascade across providers**: RESOLVED. Pipeline-wide fallback chains with site-specific overrides are defined in the "Cross-Provider Fallback" section above. The v1 scope includes provider-level fallback with credential-aware skipping.

3. **Model list caching**: For providers that support model listing (Ollama /api/tags, OpenRouter /api/v1/models), how often to refresh? Recommend: on-demand when the user opens the model selector, cached for the session. Daily background refresh for pricing data.

---

## Response Body Cost Extraction (Gap 2 resolved)

OpenRouter returns actual cost directly in the chat completions response body. Full extraction:

```rust
pub struct OpenRouterUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub cost: f64,                                      // USD, authoritative
    pub cost_details: Option<OpenRouterCostDetails>,    // optional breakdown
}

pub struct OpenRouterCostDetails {
    pub upstream_inference_prompt_cost: Option<f64>,
    pub upstream_inference_completions_cost: Option<f64>,
    // Additional fields may appear (caching breakdown, reasoning cost, etc.);
    // we parse defensively with serde(default)
}
```

**Parsing strategy**:

```rust
fn extract_cost(response_body: &Value) -> Result<ExtractedCost, ExtractParseError> {
    let usage = response_body.get("usage")
        .ok_or(ExtractParseError::MissingUsage)?;

    let cost = usage.get("cost")
        .and_then(|v| v.as_f64())
        .ok_or(ExtractParseError::MissingCostField)?;

    let prompt_tokens = usage.get("prompt_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let completion_tokens = usage.get("completion_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let generation_id = response_body.get("id")
        .and_then(|v| v.as_str())
        .ok_or(ExtractParseError::MissingId)?;

    // Defensive: if the shape changes, log WARN but still return what we have
    if !generation_id.starts_with("gen-") {
        warn!("unexpected generation_id format: {}", generation_id);
    }

    // cost_details is optional and best-effort
    let cost_details = usage.get("cost_details").cloned();

    Ok(ExtractedCost {
        generation_id: generation_id.to_string(),
        cost,
        prompt_tokens,
        completion_tokens,
        cost_details,
    })
}
```

**When `usage.cost` is missing or non-numeric** (unexpected provider behavior): fall back to computing cost from `pricing_json` using the reported token counts, set `reconciliation_status = "estimated"`, and log a WARN with the full response body for investigation.

---

## OTLP Cost Attribute Extraction (Gap 1 resolved)

The GenAI semantic conventions at OpenTelemetry (https://github.com/open-telemetry/semantic-conventions) define the standard attribute naming. For cost, the primary key is:

**`gen_ai.usage.cost`** — numeric attribute, USD per call

Our webhook handler extracts this via a defensive cascade:

```rust
fn extract_otlp_cost(span_attributes: &[OtlpAttribute]) -> Option<f64> {
    // Primary: gen_ai.usage.cost (GenAI semantic convention)
    if let Some(cost) = find_numeric_attribute(span_attributes, "gen_ai.usage.cost") {
        return Some(cost);
    }

    // Fallback 1: scan for any gen_ai.* key containing "cost"
    for attr in span_attributes {
        if attr.key.starts_with("gen_ai.") && attr.key.contains("cost") {
            if let Some(v) = attr.value.as_numeric() {
                debug!("using fallback OTLP cost attribute: {}", attr.key);
                return Some(v);
            }
        }
    }

    // Fallback 2: any attribute key ending in ".cost"
    for attr in span_attributes {
        if attr.key.ends_with(".cost") {
            if let Some(v) = attr.value.as_numeric() {
                debug!("using last-resort OTLP cost attribute: {}", attr.key);
                return Some(v);
            }
        }
    }

    None
}

fn find_numeric_attribute(attrs: &[OtlpAttribute], key: &str) -> Option<f64> {
    attrs.iter().find(|a| a.key == key).and_then(|a| a.value.as_numeric())
}
```

Token counts use the same defensive pattern against standard keys:
- `gen_ai.usage.prompt_tokens` → `intValue`
- `gen_ai.usage.completion_tokens` → `intValue`
- `gen_ai.usage.total_tokens` → `intValue`

If the OpenTelemetry GenAI conventions evolve (e.g., rename to `gen_ai.usage.cost_usd`), the fallback cascade catches the new key automatically. We log which key matched at DEBUG level so the first call after a convention update is visible.

---

## Credits Endpoint Parsing (Gap 3 resolved)

`GET https://openrouter.ai/api/v1/credits` (requires Management API key). Defensive parsing handles both likely response shapes:

```rust
pub struct CreditsResponse {
    pub total_credits: f64,
    pub total_usage: f64,
    pub remaining: f64,
}

fn parse_credits_response(body: &Value) -> Result<CreditsResponse, CreditsParseError> {
    // Try wrapped form: { "data": { ... } }
    let data = body.get("data").unwrap_or(body);

    // Field name cascade — different versions may use different names
    let total = find_number(data, &["total_credits", "credits_total", "purchased", "limit"])
        .ok_or(CreditsParseError::MissingTotal)?;

    let used = find_number(data, &["total_usage", "credits_used", "usage", "spent"])
        .ok_or(CreditsParseError::MissingUsage)?;

    let remaining = find_number(data, &["remaining_credits", "credits_remaining", "remaining", "balance"])
        .unwrap_or(total - used);

    Ok(CreditsResponse { total_credits: total, total_usage: used, remaining })
}

fn find_number(obj: &Value, keys: &[&str]) -> Option<f64> {
    for key in keys {
        if let Some(v) = obj.get(*key).and_then(|v| v.as_f64()) {
            return Some(v);
        }
    }
    None
}
```

**First-call learning**: on the first successful credits query, log the full response body at INFO level so subsequent spec updates can codify the exact observed shape. This is a one-time observation (per provider) stored in `pyramid_providers.observed_credits_shape_json` for future reference.

---

## Dynamic Default Model Selection (Gap 4 resolved)

Rather than hardcoding default model IDs that decay, we resolve defaults dynamically on first provider setup (and on user-triggered "refresh models"):

```rust
pub async fn select_default_tier_models(
    provider: &Provider,
    models_list: &[ModelEntry],
) -> HashMap<String, String> {
    let mut defaults = HashMap::new();

    // Filter to usable text-output models with tool and response_format support
    let usable: Vec<&ModelEntry> = models_list.iter()
        .filter(|m| m.architecture.output_modalities.contains(&"text".to_string()))
        .filter(|m| m.supported_parameters.iter().any(|p| p == "response_format" || p == "structured_outputs"))
        .filter(|m| m.expiration_date.is_none())   // not deprecated
        .collect();

    // EXTRACTOR tier: cheapest model with context_length >= 100k
    let extractor = usable.iter()
        .filter(|m| m.context_length >= 100_000)
        .min_by(|a, b| cmp_price(&a.pricing.prompt, &b.pricing.prompt))
        .map(|m| m.id.clone());
    if let Some(id) = extractor { defaults.insert("extractor".into(), id); }

    // WEB tier: same criteria as extractor (used for web/edge generation)
    if let Some(id) = defaults.get("extractor").cloned() {
        defaults.insert("web".into(), id);
    }

    // SYNTH_HEAVY tier: highest-rated model with context_length >= 200k and tool support
    let synth_heavy = usable.iter()
        .filter(|m| m.context_length >= 200_000)
        .filter(|m| m.supported_parameters.iter().any(|p| p == "tools"))
        .min_by(|a, b| cmp_price(&b.pricing.completion, &a.pricing.completion))  // MAX completion price as proxy for quality
        .map(|m| m.id.clone())
        .or_else(|| {
            // Fallback: any model with context >= 200k
            usable.iter().filter(|m| m.context_length >= 200_000).next().map(|m| m.id.clone())
        });
    if let Some(id) = synth_heavy { defaults.insert("synth_heavy".into(), id); }

    // MID tier: mid-priced model with context >= 120k
    let mid = usable.iter()
        .filter(|m| m.context_length >= 120_000)
        .map(|m| (m, parse_price(&m.pricing.prompt)))
        .filter(|(_, p)| *p > 0.0)
        .collect::<Vec<_>>();
    // Sort by price, take the median
    let mut sorted = mid;
    sorted.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    if !sorted.is_empty() {
        let median_idx = sorted.len() / 2;
        defaults.insert("mid".into(), sorted[median_idx].0.id.clone());
    }

    // STALE_LOCAL: not resolved from OpenRouter, picked from Ollama on local provider setup
    // (handled separately when the Ollama provider is added)

    defaults
}

fn cmp_price(a: &str, b: &str) -> std::cmp::Ordering {
    let pa = a.parse::<f64>().unwrap_or(f64::MAX);
    let pb = b.parse::<f64>().unwrap_or(f64::MAX);
    pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
}
```

This selection algorithm is **itself a generative config** (`schema_type: tier_routing_defaults_heuristic`) so users can refine it via notes: "prefer models that support streaming", "prefer models under $3/M tokens", "prefer models from specific providers only", etc. The refined heuristic is stored as a contribution and re-applied on the next refresh.

**Fallback if the models endpoint is unreachable on first run**: seed with a minimal safe list of known-good model families (not specific versions) and surface a "Model selection pending — refresh models to populate tier defaults" banner.

**Legacy hardcoded defaults removed**: on first run after this spec ships, the old `inception/mercury-2` / `qwen/qwen3.5-flash-02-23` / `x-ai/grok-4.20-beta` defaults are discarded and replaced with the dynamically-selected set. No user action required.

---

## Management API Key Provisioning (Gap 5 resolved)

`GET /api/v1/credits` requires a Management API key (a separate key type from inference keys). The provisioning flow:

### UI flow (Settings → Providers → OpenRouter → Credits)

1. User clicks "Enable credit balance monitoring"
2. Wire Node opens a modal with:
   - Brief explanation: "Credit balance monitoring requires a separate Management API key. Click the button below to create one in the OpenRouter dashboard, then paste it here."
   - "Open OpenRouter Dashboard" button → opens `https://openrouter.ai/settings/keys` in the user's default browser
   - Text input for the new key
   - "Save" button
3. On save, the key is stored in `.credentials` under `OPENROUTER_MANAGEMENT_KEY`
4. Wire Node calls `GET /api/v1/credits` immediately with the new key to verify it works
5. On success: enables the credit balance widget in the Oversight page
6. On failure: displays the parse error and keeps the modal open

### Credential storage

```
# ~/Library/Application Support/wire-node/.credentials
OPENROUTER_KEY: sk-or-v1-...              # inference key
OPENROUTER_MANAGEMENT_KEY: sk-or-mgmt-...  # management key for /credits
OPENROUTER_BROADCAST_SECRET: <random>      # webhook shared secret
```

### Fallback if Management Key isn't configured

The provider registry works without a Management Key — we just skip the balance check. The oversight page shows "Credit balance: not monitored (add Management API Key to enable)" with a link to the setup flow. Credit-exhaustion fallback still works via reactive 4xx detection on inference calls.

---

## Ollama Reverse-Proxy Auth (Gap 6 resolved)

Production Ollama deployments commonly sit behind nginx, Caddy, or an API gateway with authentication. Our existing credential system covers these cases via `api_key_ref` + `config_json.extra_headers`:

```sql
-- Example: Ollama behind nginx basic auth
INSERT INTO pyramid_providers VALUES (
    'ollama-prod', 'Production Ollama', 'openai_compat',
    'https://ollama.internal.example.com/v1',
    NULL,                                    -- api_key_ref unused for basic auth
    1,                                        -- auto_detect_context
    0, NULL,                                  -- supports_broadcast
    '{
      "extra_headers": {
        "Authorization": "Basic ${OLLAMA_PROD_BASIC_AUTH}"
      }
    }',                                       -- config_json with custom header
    1, ...
);
```

The `config_json.extra_headers` field accepts arbitrary HTTP headers with `${VAR_NAME}` substitution. The provider resolver merges these with the standard bearer auth (if `api_key_ref` is set). Supported patterns:

| Auth scheme | Configuration |
|---|---|
| No auth (local) | `api_key_ref = NULL`, no extra headers |
| Bearer token | `api_key_ref = "OLLAMA_API_KEY"` → adds `Authorization: Bearer $OLLAMA_API_KEY` |
| HTTP basic auth | `config_json.extra_headers = { "Authorization": "Basic ${OLLAMA_BASIC_AUTH}" }` where `OLLAMA_BASIC_AUTH` is pre-encoded base64(user:pass) |
| API gateway key | `config_json.extra_headers = { "X-Api-Key": "${OLLAMA_GATEWAY_KEY}" }` |
| Multi-header | Combination of the above, all merged at request time |

No code changes beyond the existing credential + header merge logic. The spec already supports this; the gap was documentation, not design.

---

## Webhook Authentication (Gap 7 resolved)

See `evidence-triage-and-dadbear.md` Part 4 → "Webhook authentication" for the full scheme:

- **Primary**: shared secret header `X-Webhook-Secret: ${OPENROUTER_BROADCAST_SECRET}` (32-byte random, generated by Wire Node on provider setup, copied into OpenRouter dashboard)
- **Opportunistic**: source IP tracking for observational security review
- **Future-proof**: HMAC verification on `X-Signature` header IF OpenRouter adds it later (additive, not replacing the shared secret)
- **Rate-limit**: 100 broadcasts/second per IP to contain abuse if the shared secret leaks

The shared secret approach works today and requires no assumptions about undocumented features. HMAC support is a future-compatibility hook, not a gap.
