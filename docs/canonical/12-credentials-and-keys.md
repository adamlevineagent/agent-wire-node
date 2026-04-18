# Credentials and keys

Before Agent Wire Node can run any pyramid build, it needs a way to call an LLM. That means either:

- An **OpenRouter API key** (recommended to start — pay-as-you-go, no local setup).
- A local **Ollama** instance (free, private, slower, needs a capable machine).
- Both (common setup: cloud LLMs for heavy synthesis, local Ollama for cheap staleness checks).

This doc covers setting up credentials: where they live, how they're referenced from configs, how to keep them safe, and how to manage multiple keys.

---

## Where credentials live

Credentials live in a single file at:

```
~/Library/Application Support/wire-node/.credentials
```

The file is plain YAML. It looks like this:

```yaml
OPENROUTER_KEY: sk-or-v1-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
OPENROUTER_MANAGEMENT_KEY: sk-or-mgmt-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
OLLAMA_LOCAL_URL: http://localhost:11434
ANTHROPIC_KEY: sk-ant-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
```

Keys are uppercase SNAKE_CASE. Values are arbitrary strings. Agent Wire Node locks the file to `0600` (user read/write only) on every load. If permissions are wider than that, the app refuses to read the file and surfaces a blocking error in Settings — this is to protect you from accidentally leaking a key through a too-permissive backup or sync.

You can edit this file directly with any editor if you want to. The Settings UI is the convenient path.

## How configs reference credentials

Agent Wire Node keeps a hard wall between configs (which are shareable) and secrets (which are private). Configs reference credentials by variable name, not by value.

A provider config might look like:

```yaml
provider_id: openrouter
base_url: https://openrouter.ai/api/v1
api_key: ${OPENROUTER_KEY}
```

The string `${OPENROUTER_KEY}` stays as a literal in the config. When the runtime actually needs the key to make an HTTP call, it resolves the variable against your credentials file at that moment, builds the Authorization header, makes the call, and drops the resolved value. The resolved value never enters the config, never enters a log, never enters any file.

This pattern is what makes provider configs safe to publish on the Wire. When someone pulls your config, they get the YAML with `${OPENROUTER_KEY}` intact — and they resolve it against *their own* credentials file. Nothing secret moves.

---

## Setting up OpenRouter (the common path)

[OpenRouter](https://openrouter.ai/) is an LLM aggregator. One API key, many models (OpenAI, Anthropic, Google, Mistral, Qwen, Mercury, DeepSeek, Grok, open-source models, and more). You pay per-token; the pricing for each model is listed on OpenRouter's website.

### Getting a key

1. Go to https://openrouter.ai/.
2. Sign in (Google, GitHub, or email).
3. Go to **Keys** in the dashboard, create a new key.
4. Fund the account with a small amount ($5 is more than enough to start).
5. Copy the key. It looks like `sk-or-v1-...` and is 60-ish characters.

### Putting it in Agent Wire Node

Two paths:

**Via Settings UI (easy):**

1. Open Agent Wire Node → **Settings** (gear, bottom of sidebar).
2. Go to the **Credentials** panel (inside Agent Wire Node Settings, or via the Pyramid Settings shortcut).
3. Click **Add credential**.
4. Name: `OPENROUTER_KEY`. Value: paste your key.
5. Click **Save**. The app writes to the credentials file with correct permissions.
6. Click **Test** to confirm it works. Agent Wire Node sends a tiny test prompt to OpenRouter; if it comes back with token counts and a cost, you're good.

**Via direct file edit (for scripts or CI):**

```bash
chmod 0600 ~/Library/Application\ Support/wire-node/.credentials
cat > ~/Library/Application\ Support/wire-node/.credentials <<'EOF'
OPENROUTER_KEY: sk-or-v1-paste-your-key-here
EOF
chmod 0600 ~/Library/Application\ Support/wire-node/.credentials
```

Agent Wire Node picks up the new value on the next LLM call; you don't need to restart.

### The two OpenRouter key types

OpenRouter has two kinds of keys:

- **Inference keys** (`sk-or-v1-...`) — for making LLM calls. This is the one you need for builds.
- **Management keys** (`sk-or-mgmt-...`) — for reading account status, including credit balance. Optional. If you set `OPENROUTER_MANAGEMENT_KEY` as well, Agent Wire Node can show your remaining OpenRouter credits in the oversight panel and proactively switch providers when you're running low.

Start with just the inference key. Add the management key when you want the visibility.

---

## Setting up Ollama (the local path)

Ollama runs LLMs locally. It's free to run, keeps everything on your machine, and works offline. The tradeoffs are: you need a machine capable of running the model (e.g. 32+ GB RAM for 27B-class models), and local inference is generally slower than API calls.

### Installing Ollama

```bash
brew install ollama
brew services start ollama
# or run in foreground:
ollama serve
```

Confirm it's running:

```bash
curl http://localhost:11434/api/tags
# Should return {"models":[]} or a list of models you have pulled.
```

### Pointing Agent Wire Node at it

You don't need to set a credential for local Ollama — there's no API key. What you do need is to tell Agent Wire Node the URL.

**Easy path:** Go to **Settings → Agent Wire Node Settings → Local Mode**. Toggle Local Mode on. Agent Wire Node discovers your Ollama at `http://localhost:11434` by default, probes it, and shows the list of installed models. Pick one for each tier (extractor, synth_heavy, stale_local, etc.), or just set it globally. See [`51-local-mode-ollama.md`](51-local-mode-ollama.md) for details.

**Manual path:** add an entry to your credentials file so configs can reference a Wire-shareable Ollama URL:

```yaml
OLLAMA_LOCAL_URL: http://localhost:11434
```

This is useful if you're authoring provider configs that reference `${OLLAMA_LOCAL_URL}` explicitly.

### Pulling a model

Ollama ships with no models by default. Pull at least one before you expect Agent Wire Node to use it:

```bash
ollama pull gemma3:27b
# or smaller if your machine won't fit that:
ollama pull gemma3:12b
# or a coding-focused model:
ollama pull qwen2.5-coder:32b
```

The model file can be 10-50 GB. Pulls take a while on first run.

Agent Wire Node's **Settings → Local Mode** panel also has a "Pull model" button that wraps this. Use the terminal path if you want more control.

---

## Setting up other providers (Anthropic, OpenAI direct, custom)

You can use providers other than OpenRouter. The pattern is the same: put the key in `.credentials`, configure a provider that references it by variable name, point tier routing at it.

For Anthropic direct:

```yaml
ANTHROPIC_KEY: sk-ant-api03-xxxxxxxxxxxxxxxx
```

For a self-hosted Ollama behind an auth gateway:

```yaml
OLLAMA_GATEWAY_KEY: your-gateway-token
OLLAMA_GATEWAY_URL: https://ollama.internal.example.com/v1
```

Then configure a provider in **Settings → Providers** that uses `${OLLAMA_GATEWAY_KEY}` as its API key and `${OLLAMA_GATEWAY_URL}` as its base URL. See [`52-provider-registry.md`](52-provider-registry.md).

---

## Credential safety rules

Agent Wire Node is designed to make credential leaks hard, but it relies on a few things from you.

**Permissions on the credentials file must be `0600`.** If they are wider, Agent Wire Node refuses to read the file. If you see the "credentials file has unsafe permissions" error, use the **Fix permissions** button or run `chmod 0600 ~/Library/Application\ Support/wire-node/.credentials` and retry.

**Never paste a credential value into a config YAML directly.** Always use `${VAR_NAME}` references. If you paste a value into YAML and then publish that config, your key goes on the Wire permanently. Agent Wire Node tries to catch this at publish time with a pre-publish scan, but don't rely on that check — develop the habit.

**Don't commit `.credentials` to git.** It lives outside your repo; keep it that way. If you are syncing your home directory via iCloud or similar, consider excluding `~/Library/Application Support/wire-node/.credentials` or moving the whole data directory out of the sync path.

**Rotate keys you've mislaid.** If you think you pasted a key somewhere visible, go to the provider's dashboard (OpenRouter, Anthropic, etc.), revoke the key, and create a new one.

**Be careful with screenshots.** The Settings UI masks key values by default (shown as `••••••`), but the "reveal" toggle shows them in plain text. Don't screenshot with reveal on.

---

## What Agent Wire Node does for you automatically

- On every read of the credentials file, Agent Wire Node verifies the file exists, is regular (not a symlink to somewhere suspicious), and has 0600 permissions. If any check fails, it surfaces an error rather than silently using the file.
- The in-memory wrapper for resolved credentials has no Display, Debug, or Serialize implementations. Attempts to log or serialize a resolved credential fail at compile time. The only way a credential value exits the process is through the HTTP client, which masks the Authorization header in its request logs.
- At Wire publish time, the YAML is scanned for `${…}` patterns, and warnings are surfaced for each credential your config requires. The pre-publish preview also scans for raw credential values (from your file) and aborts if any are found.

You don't need to do anything for these — they just happen.

---

## Credential references dashboard

In **Settings → Credentials**, below the list of credentials, you'll see a cross-reference table: every credential that's defined (or referenced-but-missing), and which configs reference it.

```
OPENROUTER_KEY    Referenced by: tier_routing (global), code_pyramid (custom chain)   ✓ defined
ANTHROPIC_KEY     Referenced by: stale_local provider override                        ✗ missing
WIRE_AUTH_TOKEN   Referenced by: Wire publication                                     ✓ defined
```

Missing credentials are marked with a red X and a click-to-set button. Use this view to find holes before a build fails on you.

---

## Common credential-related issues

### "Variable `${OPENROUTER_KEY}` is not defined"

You have a provider or config that references `OPENROUTER_KEY` but your credentials file doesn't have it. Either add it (Settings → Credentials → Add) or change the provider to use a different variable name.

### "Provider test failed: 401 Unauthorized"

The credential exists but is wrong (revoked, truncated during paste, belongs to a different account, etc.). Regenerate at the provider's dashboard and update.

### "Provider test failed: insufficient credits"

Specific to paid providers. Top up the account. If you've added `OPENROUTER_MANAGEMENT_KEY`, you'll see this warning in the oversight panel before builds start failing.

### "Credentials file has unsafe permissions"

`chmod 0600 ~/Library/Application\ Support/wire-node/.credentials` or click the **Fix permissions** button in Settings → Credentials.

### A published config I pulled complains about a credential I don't have

Pulls show required credentials in their preview. You'll see something like "This config requires `OPENROUTER_KEY`, `ANTHROPIC_KEY`." Add the ones you have; the ones you don't will block specific functions of the config until you provide them.

### I rotated my key; do I need to restart the app?

No. Agent Wire Node reads the credentials file on each resolve, not at startup. Next LLM call uses the new value.

---

## Advanced: multiple keys per provider

You might want different OpenRouter keys for different purposes (dev vs prod, personal vs work). Credentials are flat — just use distinct variable names:

```yaml
OPENROUTER_KEY: sk-or-v1-personal...
OPENROUTER_KEY_WORK: sk-or-v1-work...
```

Then configure two providers, each using a different variable. Agent Wire Node's tier routing lets you mix and match per pyramid or per step.

---

## Where to go next

- [`50-model-routing.md`](50-model-routing.md) — pick which models get used for which steps.
- [`51-local-mode-ollama.md`](51-local-mode-ollama.md) — get Ollama running end-to-end.
- [`52-provider-registry.md`](52-provider-registry.md) — add custom providers.
- [`21-building-your-first-pyramid.md`](21-building-your-first-pyramid.md) — once credentials work, build something.
