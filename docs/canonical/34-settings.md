# Settings (every panel, what it's for, what to tune)

**Settings** is the gear icon at the bottom of the sidebar. It holds every configuration surface that isn't a contribution — node-level preferences, credentials, providers, tier routing, local mode, auto-update, health.

This doc tours each panel in order.

---

## Pyramid Settings (API key quick setup)

The top panel is a shortcut for the most common first-run task: getting an OpenRouter key into the credentials file.

- **Configuration Status** — shows at a glance: OpenRouter API key (set / unset), auth token (set / unset), primary model.
- **Primary Model** — the default model for tier routes that don't specify one. Input field + a link to openrouter.ai/models.
- **Test API Key** — sends a test prompt to OpenRouter via your key; shows token counts and cost on success, or the specific error.
- **Auto-Execute toggle** — whether Wire Node automatically runs pyramid builds triggered by external events (absorption, DADBEAR, etc.). Off means builds need manual confirmation.

For anything beyond the basic OpenRouter key, use the panels below.

---

## Wire Node Settings

The main Settings body, several sub-sections.

### Network Configuration

- **Node name** — the human-readable name for your node. Shown in fleet, chronicle, tray tooltip. Change anytime.
- **Node ID** (read-only) — your durable identity. Don't change; if you want a fresh identity, nuke the data directory.
- **Storage cap (GB)** — slider, 1-1000. How much disk Wire Node can use for cached documents and mesh hosting. Does not limit your own pyramid data.
- **Mesh hosting toggle** — whether your node hosts documents from published corpora for the Wire.

### Health Status

- **Overall status** — green / yellow / red.
- **Per-check status** — individual checks (database, providers, tunnel, disk). Name, status, message.

If any check is failing, this panel tells you what and suggests remediation.

### Auto-Update (for the app itself)

- **Enable toggle** — auto-update the Wire Node binary when updates are available.
- **Update available** banner — appears when an update is ready. Shows version, release notes link.
- **Install update** button — downloads, verifies signature, installs, restarts.

Turning auto-update off pins you to a version. You can manually check for updates any time.

### Tunnel Status

- **Current status** — Connected / Connecting / Offline / Error.
- **Tunnel endpoint** — the public URL your node is reachable at (via Cloudflare Tunnel).
- **Retry** button — reconnect the tunnel if disconnected.

Tunnel goes down occasionally (transient network issues). Usually self-heals; the retry button is for when it doesn't.

### Compute Participation Policy

Three preset modes (pick one):

- **Coordinator** — dispatch-only. Your node plans builds but doesn't serve inference itself.
- **Hybrid** — full participation. Your node both serves inference and dispatches it.
- **Worker** — serve-only. Your node accepts inference jobs but doesn't dispatch its own out.

Each mode has a one-line description visible in the UI. Advanced users can expand to see the 8 individual toggle fields that each preset maps to (for fine-grained control).

See [`73-participation-policy.md`](73-participation-policy.md).

### Local Mode (Ollama)

- **Enable/disable toggle** — flip everything to Ollama or back to OpenRouter. Toggling on:
  - Probes your configured Ollama URL.
  - Lists installed models.
  - Asks you to pick a model for tier routing.
  - Applies to all tiers by default; you can adjust per-tier below.
- **URL input** — the Ollama base URL. Defaults to `http://localhost:11434`. Read-only when enabled (change only via the toggle flow).
- **Model selector** — dropdown of installed models, auto-populated.
- **Probe** — test connectivity without enabling.
- **Pull model** — form to pull a new model; progress bar during pull.
- **Available models** list — everything installed locally, with size, family, parameter count, quantization, last-used, delete button.

See [`51-local-mode-ollama.md`](51-local-mode-ollama.md).

### Config History

An accordion of past versions of your node-level configs. Timestamps + values. If you change a setting and regret it, you can review the prior values here.

---

## Credentials (Credentials panel)

Distinct from Pyramid Settings. This is the full credentials manager.

- **List of credentials** — each with a masked preview (`sk-or-••••••xxxx`), reveal toggle, rotate, delete.
- **Add credential** — form with key name + value. Saves with `0600` permissions.
- **Cross-reference dashboard** — table of credentials vs which configs reference them. Missing credentials flagged with red X and click-to-set.
- **File permission** — current mode, Fix permissions button if wider than 0600.

See [`12-credentials-and-keys.md`](12-credentials-and-keys.md).

---

## Providers

The **Providers** panel is where you configure LLM providers beyond the default OpenRouter. Each provider has:

- **ID** (e.g. `openrouter`, `ollama-local`, `my-custom-oai`).
- **Display name.**
- **Type** — openrouter / openai_compat / ollama-native.
- **Base URL** — the endpoint to call.
- **Auth** — which credential variable to use as the bearer token (or none for local unauthenticated).
- **Auto-detect context** toggle — for providers that expose context-window metadata (Ollama), ask for it on model selection.
- **Enabled** toggle.

Actions per provider: edit, test (sends a trivial prompt to verify connectivity), remove.

**Add provider** button walks you through creating a new one.

See [`52-provider-registry.md`](52-provider-registry.md).

---

## Tier Routing

The **Tier Routing** panel maps tier names (`extractor`, `synth_heavy`, `stale_local`, `web`, `mid`, etc.) to `(provider, model)` pairs.

Each row:

- **Tier name.**
- **Provider** — dropdown, picks from your configured providers.
- **Model** — dropdown, picks from the provider's available models (auto-fetched).
- **Context limit** — total context budget. Auto-detected by default; overridable.
- **Max completion tokens** — hard output cap. Typically inherited from the model's metadata.
- **Pricing** — inherited from the provider's model list.
- **Fallback chain** (collapsed, advanced) — other (provider, model) pairs to try if the primary fails.

Edit a row, save, and future builds use the new routing. Existing builds don't re-route mid-flight.

See [`50-model-routing.md`](50-model-routing.md).

---

## Per-Step Overrides

Advanced panel (collapsed by default). For fine-grained control, you can override tier routing at the `(slug, chain_id, step_name)` level. Useful when one specific step on one specific pyramid needs a different model.

Rarely used; most users tune at the tier level.

---

## Privacy (if available in your build)

Controls for relay-based privacy:

- **Always identify on search** — off by default; if on, your handle is attached to search queries (so authors can see who's searching for their work).
- **Always identify on pull** — off by default; if on, pulls are attributed.
- **Run a relay on this node** — opt-in to carrying Wire traffic for other operators. Earns credits.

See [`63-relays-and-privacy.md`](63-relays-and-privacy.md).

---

## Notifications

Control which notification types surface:

- Per event type: on / muted / silent.
- Per source: on / muted.

Mute things you don't want to see (e.g. infrastructure noise if you're on a stable setup).

---

## About

Version, build hash, signatures, release notes link. Useful to include in bug reports.

---

## Keyboard shortcuts

A reference list of every shortcut across all modes. Comes from [`Z1-quick-reference.md`](Z1-quick-reference.md).

---

## What to change first, second, third

A rough order for a new operator:

1. **Credentials** — get OpenRouter and/or Ollama keys in.
2. **Node name** — make sure Fleet / chronicle attributions show something meaningful.
3. **Storage cap** — set appropriately for your disk.
4. **Tier routing** — once you've been building for a bit, tune which models handle which work. The default everything-on-Mercury-2 is fine but not optimal for cost.
5. **Compute participation policy** — decide whether your node serves inference for others.
6. **Local mode / providers** — if you want local Ollama alongside cloud.
7. **Auto-update** — usually leave on.

Everything else can wait until you hit a specific reason to change it.

---

## Where to go next

- [`12-credentials-and-keys.md`](12-credentials-and-keys.md) — credentials specifically.
- [`50-model-routing.md`](50-model-routing.md) — how tier routing works.
- [`51-local-mode-ollama.md`](51-local-mode-ollama.md) — Ollama setup.
- [`73-participation-policy.md`](73-participation-policy.md) — compute participation modes.
- [`93-updates-and-dadbear-app.md`](93-updates-and-dadbear-app.md) — auto-update of the app itself.
