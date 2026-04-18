# Provider registry

A **provider** is a compute backend Agent Wire Node can call for inference — OpenRouter, Ollama, a self-hosted OpenAI-compatible endpoint, or anything that speaks that API. The provider registry is the set of configured providers on your node. Tier routing maps tiers to `(provider, model)` pairs; the provider registry is what resolves `provider` to a real URL + auth.

This doc covers what a provider definition looks like, how to add a new provider, and the common configurations you'll want.

---

## What a provider definition contains

Each provider is a row with:

- **ID** — e.g. `openrouter`, `ollama-local`, `anthropic-direct`, `my-private-gateway`. Used in tier routing.
- **Display name** — for the UI.
- **Type** — `openrouter` or `openai_compat`. (These are the two shipped types; `openai_compat` is what you use for any OpenAI-compatible endpoint, including Ollama.)
- **Base URL** — the endpoint to call. May contain `${VAR_NAME}` references (e.g. `${OLLAMA_PROD_URL}` for a URL that's an environment-specific secret).
- **API key ref** — the credentials file variable that holds the auth token. Null for providers without auth (local Ollama).
- **Auto-detect context** — whether Agent Wire Node should query the provider for context-window metadata on model selection.
- **Extra headers** — custom headers (e.g. for a proxy gateway's auth).
- **Enabled** — can be toggled off without removing the row.

The full schema is in the provider spec (`docs/specs/provider-registry.md`).

---

## Shipped provider types

**`openrouter`** — calls `https://openrouter.ai/api/v1/chat/completions` with the `OPENROUTER_KEY` credential. Returns detailed token counts + real USD cost in `usage.cost`. Supports `response_format` per model's capabilities, `tools`, `structured_outputs`, `reasoning`, and OpenRouter-specific extensions (trace metadata, intra-provider fallback, session_id, plugins).

**`openai_compat`** — generic OpenAI-compatible endpoint. Works for:

- Local Ollama (`http://localhost:11434/v1`).
- Self-hosted Ollama behind an auth proxy.
- Any OpenAI-style API.

Supports standard `chat/completions` path, optional bearer auth, structured output when the model supports it.

**Ollama setups** — configure as `openai_compat` with base URL `http://localhost:11434/v1`. Ollama's cloud-hosted models (e.g. `gpt-oss:120b-cloud`) use the same `openai_compat` type pointed at `https://ollama.com/api` with `OLLAMA_API_KEY` as the auth variable.

Forward-looking variants (`ollama_native` for per-request `num_ctx` control, dedicated cloud-specific provider types) are on the roadmap but not yet shipped — the shipped `openai_compat` covers the common cases today.

---

## Adding a provider

### Anthropic direct

```yaml
schema_type: provider
id: anthropic
display_name: Anthropic (direct)
type: openai_compat                    # Anthropic's OAI-compat endpoint
base_url: https://api.anthropic.com/v1
api_key_ref: ANTHROPIC_KEY
auto_detect_context: false
enabled: true
```

Ensure `ANTHROPIC_KEY` is in your credentials file. See [`12-credentials-and-keys.md`](12-credentials-and-keys.md).

### Self-hosted Ollama with basic auth

```yaml
schema_type: provider
id: ollama-prod
display_name: Production Ollama
type: openai_compat
base_url: https://ollama.internal.example.com/v1
api_key_ref: null
extra_headers:
  Authorization: "Basic ${OLLAMA_PROD_BASIC_AUTH}"
auto_detect_context: true
enabled: true
```

Where `OLLAMA_PROD_BASIC_AUTH` is a pre-encoded `base64(user:pass)` in your credentials file.

### API gateway with custom header

```yaml
schema_type: provider
id: my-gateway
display_name: Custom Gateway
type: openai_compat
base_url: https://inference.example.com/v1
extra_headers:
  X-Api-Key: "${GATEWAY_KEY}"
  X-Project-Id: "wire-node-alpha"
auto_detect_context: false
enabled: true
```

---

## Provider-specific knobs (OpenRouter)

OpenRouter supports more than the OpenAI baseline. If you're using `type: openrouter`, the executor can (optionally) inject:

- **`session_id: "<slug>/<build_id>"`** — groups related requests for per-build sampling.
- **`trace: { build_id, step_name, slug, depth }`** — custom metadata passed through to observability destinations.
- **`user: <node_identity>`** — per-node analytics.
- **`models: [primary, fallback1, fallback2]` + `route: "fallback"`** — OpenRouter's own intra-provider fallback.
- **`provider: ProviderPreferences`** — fine-grained upstream routing (which OpenRouter-behind-the-scenes provider to prefer).
- **`plugins: [...]`** — web search, response healing, context compression.

These are tuned via the provider's `config_json` field or at the tier routing level. Default behavior is "inject trace + session_id, leave the rest alone."

---

## Response parsing

Each provider type knows how to parse its response body:

- **OpenRouter** — `choices[0].message.content`, `usage.prompt_tokens`, `usage.completion_tokens`, `usage.cost` (USD, authoritative), `id` (generation ID).
- **OpenAI-compat** — same shape minus `usage.cost` (which isn't standard OAI). Cost is computed from tier routing's pricing table.
- **Ollama** — OAI-compat path, cost = 0 (local), token counts still recorded.
- **Ollama native** — different shape; the native provider type knows how to read it.

You don't usually think about this; it's what makes tier routing work uniformly across providers.

---

## Provider capabilities

Each provider advertises what it supports:

- **`supports_response_format`** — can the provider enforce a JSON output schema? Per-model for OpenRouter (check the model's `supported_parameters`).
- **`supports_streaming`** — Server-Sent Events for streaming responses.
- **`supports_tools`** — function calling.

Chains don't usually set per-capability requirements; they set `response_schema` and assume the tier routing has picked a compatible provider+model. If the model doesn't support `response_format`, OpenRouter silently ignores the field (it doesn't error). For strict JSON, OpenRouter's `response-healing` plugin or a healing prompt on the step handles recovery.

---

## Testing a provider

In **Settings → Providers**, each provider has a **Test** button. Test sends a trivial prompt ("Reply with the word OK") and reports:

- HTTP status.
- Latency.
- Token counts.
- Cost (if the provider reports it).
- Generation ID (for audit).
- Any errors with specifics ("credential variable not defined", "model not found", "rate limited").

Use test after any credential change, after adding a new provider, or when debugging a build that's failing on provider calls.

---

## Fallback between providers (planned)

When OpenRouter is down, fall back to Anthropic direct. When Anthropic is down, fall back to local Ollama. The provider registry and tier routing support declaring a fallback chain per tier; the walker is partially shipped. Credential-aware — entries without defined credentials get skipped with a logged warning.

See [`50-model-routing.md`](50-model-routing.md) for the fallback chain shape.

**Status:** defined in the spec, partially wired through the dispatch path. The intra-provider fallback (OpenRouter's own `route: "fallback"`) works today. Cross-provider fallback at Agent Wire Node level is landing.

---

## OpenRouter account state (optional monitoring)

If you're using OpenRouter, Agent Wire Node can optionally monitor your credit balance. Requires a separate **Management API Key** (distinct from inference keys). Setup:

1. Go to https://openrouter.ai/settings/keys, create a Management key (it's a different key type).
2. Put it in your credentials file as `OPENROUTER_MANAGEMENT_KEY`.
3. In **Settings → Providers → OpenRouter → Credits**, enable the balance widget.

Agent Wire Node periodically queries `/api/v1/credits` with the management key and shows your remaining balance + recent spend. Helpful for catching "about to exhaust credits" before builds start failing.

---

## Where to go next

- [`50-model-routing.md`](50-model-routing.md) — tier routing that sits on top of the provider registry.
- [`51-local-mode-ollama.md`](51-local-mode-ollama.md) — the Ollama-specific provider setup.
- [`12-credentials-and-keys.md`](12-credentials-and-keys.md) — managing the auth variables providers reference.
- [`docs/specs/provider-registry.md`](../specs/provider-registry.md) — authoritative spec with full field list.
