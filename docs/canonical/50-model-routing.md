# Model routing (the AI Registry)

Agent Wire Node does not hardcode which LLM to call for each step. Each chain step declares a **model tier** (a string like `extractor`, `synth_heavy`, `stale_local`, `web`, `mid`). A **tier routing table** maps each tier to a specific `(provider, model)` pair. A **provider registry** describes how to reach each provider.

This three-level indirection — step → tier → provider+model — is the **AI Registry**. Changing which model runs a step means editing tier routing, not the chain.

---

## Why indirect

Without indirection, chains bake in specific model names (`inception/mercury-2`, `anthropic/claude-sonnet-4-5`). That's fine until the model is deprecated, or you want to switch to Ollama, or you want to try a cheaper model for a specific step — and suddenly you're editing every chain that touches that model.

With tier routing, chains declare intent (`model_tier: extractor` = "use whatever's currently best for extraction on my node"). Switching models is one edit to the tier routing table.

---

## How resolution works

When the executor dispatches a step:

1. Step declares `model_tier: extractor`.
2. Executor looks up `extractor` in the active tier routing config.
3. Tier routing says `extractor → (provider: openrouter, model: inception/mercury-2)`.
4. Executor looks up provider `openrouter` in the provider registry.
5. Provider says base URL, auth variable, response format, capabilities.
6. Executor builds the HTTP call, resolves credentials, sends.

Resolution order for a step (narrowest wins):

1. **Per-step override** — the step has its own `(provider, model)` in a `pyramid_step_overrides` contribution.
2. **Step's `model` field** — explicit slug in the chain YAML.
3. **Step's `model_tier` field** — tier name.
4. **Chain defaults** — chain-wide `model_tier` or `model`.
5. **Global default** — `primary_model` in `pyramid_config.json`.

---

## The shipped tier names

These are the tier names used across shipped chains. The set is **not fixed** — you can name tiers whatever you want. These are the conventions:

- **`extractor`** — per-chunk extraction. Fast, cheap, good at structured output.
- **`web`** — cross-reference edge generation. Similar requirements to extractor.
- **`mid`** — general-purpose mid-tier. Default for most steps without stronger requirements.
- **`synth_heavy`** — apex and high-depth synthesis. Needs reasoning capacity.
- **`stale_local`** *(planned workflow)* — staleness checks. Ideal target for a local Ollama model.
- **`triage`** *(planned workflow)* — evidence pre-mapping. Cheaper tier.
- **`low`, `high`, `max`** — legacy tier names from early chain versions, still present as aliases in some deprecated chains.

The legacy `pyramid_config.json` also exposes three slots — `primary_model`, `fallback_model_1`, `fallback_model_2` — which older chains use as their only tier abstraction. Newer chains use tier routing through a proper registry. Both paths coexist in the shipped build.

---

## Editing tier routing

**Via the Settings UI** (recommended):

Go to **Settings → Tier Routing**. Each row is a tier; edit provider and model inline. Save creates a new superseding contribution.

**Via direct YAML edit:**

Tier routing is a config contribution. Open it in Tools mode → edit the YAML directly.

```yaml
schema_type: tier_routing
version: 3
tiers:
  extractor:
    provider: openrouter
    model: inception/mercury-2
    context_limit: 120000
  web:
    provider: openrouter
    model: inception/mercury-2
  synth_heavy:
    provider: openrouter
    model: anthropic/claude-sonnet-4-5
    context_limit: 200000
  stale_local:
    provider: ollama-local
    model: gemma3:27b
    context_limit: 131072
```

Save, and the next build uses the new routing.

---

## Per-step overrides

If one specific step in one specific pyramid needs a different model, create a per-step override:

```yaml
schema_type: step_override
slug: my-pyramid
chain_id: question-pipeline
step_name: deep_synthesis
field_name: model
value: anthropic/claude-opus-4-5
```

The override applies only to that `(slug, chain_id, step_name)` triplet. Everything else continues to use the normal tier routing.

This is narrow-by-design; use it when a tier-level change would be too broad.

---

## Fallback chains (planned)

A tier routing entry can define a fallback chain — other `(provider, model)` pairs to try if the primary fails. When OpenRouter is down, fall back to Anthropic direct; if that also fails, fall back to local Ollama.

```yaml
tiers:
  synth_heavy:
    primary: { provider: openrouter, model: inception/mercury-2 }
    fallback_chain:
      - { provider: anthropic, model: claude-sonnet-4-5 }
      - { provider: ollama-local, model: gemma3:27b }
```

The executor walks the chain on failure. Credential-aware — a fallback entry is skipped if its provider's auth variable isn't in the credentials file.

**Status:** fallback chains are defined in the provider registry spec but the walker is still being wired through the shipped dispatch path. Near-term. Today, fallback is primarily handled at the intra-provider level via OpenRouter's own routing.

---

## Local mode: the one-switch Ollama route

If you set up Ollama locally, there's a shortcut in **Settings → Local Mode**:

- Toggle **Enable local mode**.
- Agent Wire Node probes your Ollama, lists installed models.
- Pick a model; Agent Wire Node writes it to every tier in the routing table.
- Concurrency is set to 1 (home hardware constraint) and tier routing is swapped over atomically.

Toggling off restores the previous tier routing (stored before activation).

> **Status: known issue.** As of this writing, Local Mode has a wiring gap (P0-1 in `PUNCHLIST.md`): the dispatch path doesn't consistently consult the tier routing table when the chain engine is off. Mixed OpenRouter/Ollama routing works; pure Ollama routing for all tiers may error on the missing OpenRouter key. Fix is in progress. See [`51-local-mode-ollama.md`](51-local-mode-ollama.md).

---

## Cost accounting

Every LLM call is tagged with its resolved `(tier, provider, model)` and its cost. The cost rollup in **Understanding → Oversight** breaks spend down by provider, by model, by tier, by pyramid, by operation type. If a tier is surprisingly expensive, this is where you see it.

OpenRouter returns `usage.cost` synchronously in the response body — reconciled cost is available the moment the call returns. For local Ollama, cost is recorded as $0 but token counts are still tracked for observability.

---

## Dynamic defaults from the models endpoint (planned)

A planned enhancement: on first-run provider setup or on user-triggered refresh, Agent Wire Node queries OpenRouter's `/api/v1/models` endpoint and seeds tier defaults by ranking models on (context length, price, capability). This avoids hardcoded defaults that decay — the rankings are computed fresh against the current market.

**Status:** the ranking logic is designed (see `docs/specs/provider-registry.md` §Dynamic Default Model Selection). Not yet wired through the shipped first-run path. For now, tier defaults are seeded from `pyramid_config.json` and adjusted by user edits.

---

## Where to go next

- [`51-local-mode-ollama.md`](51-local-mode-ollama.md) — Ollama in detail.
- [`52-provider-registry.md`](52-provider-registry.md) — provider definitions.
- [`12-credentials-and-keys.md`](12-credentials-and-keys.md) — the credential variables providers reference.
- [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md) — how chains use tier names.
