# Local Mode (Ollama)

Local Mode routes all LLM calls to a local [Ollama](https://ollama.com/) instance instead of OpenRouter or any other cloud provider. Free, private, works offline. Trade-offs: slower than cloud APIs, constrained by your hardware's RAM and GPU, one-model-at-a-time for most setups.

The tier-routing wiring gap that blocked pure-Ollama builds (PUNCHLIST P0-1) was fixed on 2026-04-11. Pure Ollama is first-class today; mixed cloud+Ollama routing also works.

---

## Prerequisites

1. **Install Ollama.** On macOS: `brew install ollama` then `brew services start ollama`. Default endpoint: `http://localhost:11434`.
2. **Pull at least one model.** `ollama pull gemma3:27b` (or smaller if your machine won't fit it). For a code-focused workflow: `ollama pull qwen2.5-coder:32b`. For a document-focused workflow: `ollama pull llama3.1:70b` if you have the hardware, or `gemma3:12b` on a smaller box.
3. **Confirm reachable.** `curl http://localhost:11434/api/tags` — should list your installed models.

Agent Wire Node doesn't install Ollama for you. Once Ollama is running and has a model, Agent Wire Node can discover and use it.

---

## Enabling Local Mode

### Via Settings UI

1. Open **Settings → Agent Wire Node Settings → Local Mode**.
2. Toggle **Enable local mode**.
3. Agent Wire Node probes your Ollama URL, lists installed models.
4. Pick a model for the default tier routing. You can pick different models for different tiers, or assign one model globally.
5. Save. Agent Wire Node writes the new routing atomically, storing the old routing for fallback.

### Via provider config

You can also set up Local Mode manually without the one-click toggle:

1. Add a provider in **Settings → Providers**:
   - ID: `ollama-local`
   - Type: `openai_compat`
   - Base URL: `http://localhost:11434/v1`
   - Auth: none (or set an API key variable if your Ollama is behind an auth proxy — see [`52-provider-registry.md`](52-provider-registry.md)).
2. Edit **Settings → Tier Routing**. Point whichever tiers you want at `ollama-local` with the model of your choice.
3. Save.

This path lets you selectively route some tiers to Ollama (e.g. cheap staleness checks on local, heavy synthesis on cloud).

---

## Model management inside Agent Wire Node

Once Local Mode is enabled, the Local Mode settings panel exposes Ollama management:

- **Installed models list** — from Ollama's `/api/tags`. Each row shows name, size on disk, family, parameter count, quantization, capabilities (from `/api/show`), last used.
- **Pull model** — form with model name, optional autocomplete. Agent Wire Node invokes `/api/pull` with streaming progress.
- **Delete model** — reclaims disk space (confirmation required — destructive).
- **Unload** — `/api/generate` with `keep_alive: 0`. Frees VRAM without deleting the model.
- **Keep-alive** — how long Ollama holds a model in VRAM between calls. Short keep-alive saves memory; long keep-alive keeps calls fast.

Disk management banner: if your Ollama models exceed a configurable threshold of disk usage, Agent Wire Node surfaces a warning.

---

## Context windows and detection

Ollama models advertise a context window in their metadata. Agent Wire Node detects it via `/api/show` when you pick a model:

1. Read `model_info["general.architecture"]` → e.g. `gemma3`.
2. Read `model_info["<arch>.context_length"]` → e.g. `131072`.
3. Fallback: scan keys for any ending in `.context_length`.

Detected context window populates the tier routing entry's `context_limit` automatically. If auto-detection fails, you can set it manually.

**Note on setting context per-request:** the OpenAI-compatible path (`/v1/chat/completions`) uses whatever context window the model was loaded with — you cannot override per-request. To override, you'd need to use Ollama's native `/api/chat` endpoint with `options: {num_ctx: ...}`. Agent Wire Node's default provider path is OAI-compat; an alternate Ollama native provider exists for setups that need per-request context override but isn't the default.

---

## Cloud models (ollama.com hosted)

Ollama also offers cloud-hosted models (e.g. `gpt-oss:120b-cloud`). Using these requires:

- Sign in to ollama.com (via `ollama signin` in your terminal), OR
- Set `OLLAMA_API_KEY` in your credentials file and configure it as the auth variable on your Ollama provider.

Cloud models show a lock icon in the model list. Agent Wire Node checks for credentials before offering to use them.

---

## Hardware notes

- **RAM requirement.** Roughly the model's parameter count in bytes (Q4 quantization), plus context. A 27B Q4 model needs ~18 GB; 70B Q4 needs ~42 GB. If your machine can't fit the model, Ollama errors on load.
- **Concurrency.** Local Mode defaults concurrency to 1 per model (home hardware constraint). Ollama can queue additional requests but serves them sequentially. Higher concurrency on your Ollama provider entry means more parallel queueing; it doesn't mean parallel inference unless your model supports it and your hardware can handle it.
- **Speed.** Local inference is typically slower than cloud APIs for similar model sizes (cloud uses datacenter-class GPUs). A 27B model on an M-series Mac is usable for extraction and staleness checks; you won't want it for apex synthesis on a large pyramid.

---

## Mixed setups (often what you want)

A common and pragmatic setup:

- **Cloud** (OpenRouter) for `extractor`, `web`, `mid`, `synth_heavy` — fast, parallel, reasoning-heavy work.
- **Local Ollama** for cheap staleness checks and other high-volume low-complexity tiers.

This gives you cheap, fast staleness checks on local hardware (DADBEAR loops over source changes — the cost adds up if every check hits a cloud API) while keeping heavy build work on cloud models where speed matters.

Set up the provider and tier routing manually in Settings → Tier Routing if you want to mix. The Enable Local Mode toggle moves everything to Ollama at once, which is also valid for operators with capable local hardware.

---

## Troubleshooting

**"Provider test failed: connection refused"** — Ollama isn't running, or it's listening on a non-default address. `brew services start ollama` and retry.

**"Model not found"** — the model wasn't pulled yet. Pull via the Settings UI or `ollama pull <name>` in terminal.

**"Context window detection failed"** — `/api/show` didn't return usable metadata. Try pulling a fresher version of the model, or set context_limit manually in the tier routing.

**Build errors with "no OpenRouter key" when Ollama is on** — the older wiring gap that caused this was fixed 2026-04-11. If you're still seeing it, you may be on an older build; update the app and retry.

**Slow inference** — check model size vs your RAM. If Ollama is swapping to disk, performance drops dramatically. Pull a smaller quantization (Q4 instead of Q8) or a smaller parameter count.

**VRAM exhausted on GPU** — keep only one large model loaded at a time. Use Agent Wire Node's Unload action after a big build to free the model.

---

## Where to go next

- [`50-model-routing.md`](50-model-routing.md) — tier routing in general.
- [`52-provider-registry.md`](52-provider-registry.md) — adding Ollama or other providers manually.
- [`34-settings.md`](34-settings.md) — Local Mode in the Settings UI.
- [`12-credentials-and-keys.md`](12-credentials-and-keys.md) — credentials if your Ollama is behind auth.
