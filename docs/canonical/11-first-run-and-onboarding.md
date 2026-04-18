# First run and onboarding

This doc covers what happens the first time you launch Agent Wire Node: signing in, the onboarding wizard, generating your node identity, and landing on the main app. It assumes you have already installed the app (see [`10-install.md`](10-install.md)).

Set aside about 5 minutes. The one task with external dependencies is waiting for the magic link email to arrive.

---

## Step 1: Launch and loading screen

Open `/Applications/Agent Wire Node.app`. You see the loading screen — a "W" logo over a tunneling animation. The backend is starting up:

- Opening the SQLite database (creates it on first run).
- Generating a node identity if one doesn't already exist — a durable handle and a random token, stored in `node_identity.json`.
- Starting the HTTP server on `localhost:8765`.
- Starting the file watcher for any folders you'll later link.

This takes a few seconds. The first time, it can take up to 20 seconds while it does first-run setup.

When ready, the loading screen fades into the login screen.

## Step 2: Sign in

You need a Wire account to use the app. If you haven't been given an account yet, pause here and get one from whoever invited you to the alpha.

The login screen has two modes:

- **Magic link** (default) — enter your email, click Send. Wire sends a one-time link to your inbox; you click it, and macOS opens Agent Wire Node and completes sign-in automatically. If automatic deep-linking doesn't work (some email clients break it), you can paste the URL into the text area the screen provides, or paste the six-digit OTP code.
- **Password** — if you have set a password on your account, you can use it directly. New accounts generally start magic-link-only.

Magic link is recommended. It keeps passwords out of your head and out of leak lists.

**If the magic link email doesn't arrive:**

- Check spam.
- Wait 60 seconds and try again (the first attempt may have been rate-limited silently).
- If you keep getting nothing, the Wire service is having trouble. Check the alpha channel for outage notices.

Once sign-in succeeds, Agent Wire Node registers your node with the Wire coordinator: the coordinator learns your node's identity and returns an `api_token` for subsequent requests. You don't see this step happen — it's automatic — but it's why you need to be online for first-run login.

## Step 3: The onboarding wizard

First-time users get a four-step wizard. Existing users skip this entirely.

### Step 3a: Welcome and node name

Pick a human-readable name for your node. This is what appears in fleet lists, in compute market chronicle events, and in the system tray tooltip. Something descriptive is better than something cute: `adam-laptop`, `studio-m2`, `homeserver-beefy`. You can change this later in **Settings → Agent Wire Node Settings → Node name**, so don't agonize.

### Step 3b: Storage allocation

Choose how much disk Agent Wire Node is allowed to use for cached documents and mesh hosting. Options: 10 GB, 40 GB, 100 GB, or custom.

- **10 GB** — good if you're just kicking the tires. Enough for a small codebase pyramid and a few documents.
- **40 GB** (the default) — enough for serious use without thinking about it.
- **100 GB** — comfortable if you plan to run Ollama locally with large models, or host public pyramids for the mesh.
- **Custom** — enter anything between 1 and 1000 GB.

This cap is on *cached / hosted* content. Your own pyramids and source material are separate. See [`90-data-layout.md`](90-data-layout.md) for the full breakdown of what lives where.

### Step 3c: Link a first folder (optional)

This is where you point Agent Wire Node at something to build a pyramid over. You can pick:

- A codebase (a repo root, or any folder of source files).
- A folder of documents (PDFs, Markdown, text files).
- A folder of conversation transcripts (JSONL files).
- Nothing — click **Skip** and link folders later from **Understanding → Add Workspace**.

If you pick a folder now, the wizard doesn't start a build. It just registers the folder so you know where to come back when you want to build. You'll pick content type and configure the build when you actually create a pyramid on it.

### Step 3d: Mesh hosting (optional toggle)

A single toggle: **Participate in mesh hosting.** Off by default. When on, your node can host documents and published pyramids for other operators, which lets them pull faster and earns you mesh-hosting credits. This is independent of the compute market (which is about serving LLM inference — you can do either, both, or neither).

You can change this later in Settings. Leaving it off for now is fine; you can flip it on once you've got a feel for the app.

### Step 3e: Done

Click **Finish**. The wizard saves your choices to `onboarding.json` and lands you on the main app.

## Step 4: The main app

After onboarding, you see the sidebar on the left and the Understanding mode open on the right. The sidebar is organized in four sections:

- **YOUR WORLD** — Understanding (pyramids), Knowledge (docs/corpora), Tools (contributions).
- **IN MOTION** — Fleet (agents), Operations (notifications/jobs), Market (compute).
- **THE WIRE** — Search (discover contributions), Compose (draft contributions).
- **YOU** — Network (tunnel + credits), your handle, Settings (gear).

Each item shows a live summary: how many pyramids you have, how many are building, online fleet members, unread notifications, compute credits. Glowing items are things that need attention.

Your node isn't fully useful until you've set up credentials (step 5) and built something (step 6). Keep going.

## Step 5: Set up credentials

By default, Agent Wire Node can log you in and register your node, but it can't yet run any pyramid builds because it has no way to call an LLM. You need either:

- An **OpenRouter API key**, for cloud LLMs. Recommended for getting started because you don't need any local setup and you pay only for what you use.
- A **local Ollama instance** with at least one model pulled, for fully offline operation. Slower, but free and private.

Go to **Settings → Agent Wire Node Settings → Credentials** (or **Settings → Pyramid Settings** for the quick API-key shortcut). Paste in your OpenRouter key. Test it. Save.

Full walkthrough in [`12-credentials-and-keys.md`](12-credentials-and-keys.md). Ollama-specific setup in [`51-local-mode-ollama.md`](51-local-mode-ollama.md).

## Step 6: Build your first pyramid

Go to **Understanding** in the sidebar. Click **Add Workspace**. Pick a folder — one you feel comfortable experimenting with (a small repo you know well is ideal). Pick a content type (probably `code` to start). Optionally enter an apex question (the preset question "What is this codebase and how is it organized?" is fine for a first build).

Confirm and watch it run. The Pyramid Surface renders nodes as they appear. A small codebase might take 2-5 minutes on OpenRouter; a larger one can take an hour or more, and Ollama is slower.

If anything goes wrong, see [`A1-build-stuck-or-failed.md`](A1-build-stuck-or-failed.md).

## Step 7 (optional): Hook up an agent

Once you have a pyramid, you can point Claude (or any MCP-capable agent) at it.

Add this to your `~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "wire-node-pyramid": {
      "command": "node",
      "args": ["/absolute/path/to/agent-wire-node/mcp-server/dist/index.js"],
      "env": {
        "PYRAMID_AUTH_TOKEN": "your-auth-token-here"
      }
    }
  }
}
```

Get your auth token from `~/Library/Application Support/wire-node/pyramid_config.json` (the `auth_token` field). Restart Claude Desktop. In a new Claude session, your pyramid tools are available.

Full walkthrough in [`81-mcp-server.md`](81-mcp-server.md).

---

## What you've done, and what changed on disk

After a successful first run:

- `onboarding.json` has your node name and storage cap.
- `node_identity.json` has your durable node handle and token. **Back this up.** If you lose it, your node becomes a new node on the Wire.
- `session.json` has your current login tokens. This refreshes itself.
- `pyramid.db` exists but is mostly empty (just setup scaffolding).
- `.credentials` exists if you saved any API keys, locked to 0600.
- `wire-node.log` has a few hundred lines of boot-up activity.

You're ready to actually use the app.

## Troubleshooting first-run specifically

### The magic link opens my browser but Agent Wire Node doesn't launch

The deep link handler isn't registered. Usually this self-heals on second launch. If it doesn't, copy the URL from the email and paste it into the "Paste magic link" box on the login screen.

### I completed onboarding but the wizard re-appears on next launch

`onboarding.json` wasn't written. Check for disk space issues or permissions problems on `~/Library/Application Support/wire-node/`. The log file is truncated on each boot, so capture it immediately after the issue.

### Sign-in succeeds but my node doesn't register

The Wire coordinator is reachable but something is rejecting the registration. Most often this is because a node with the same handle already exists (rare — happens if you nuked data and a stale record is still on the server). Check **Settings → Agent Wire Node Settings → Node ID** — if it's blank, registration didn't complete. Try logging out and back in; if it still fails, get help from the alpha channel.

### Onboarding choices are wrong

You can change all of them later in **Settings → Agent Wire Node Settings**. Node name, storage cap, and mesh hosting are all editable. The only thing that's sticky is your node identity, which is tied to the `node_identity.json` file — overwriting it effectively creates a new node.

---

## Where to go next

- [`12-credentials-and-keys.md`](12-credentials-and-keys.md) — set up your API keys.
- [`20-pyramids.md`](20-pyramids.md) — understand Understanding mode before building.
- [`21-building-your-first-pyramid.md`](21-building-your-first-pyramid.md) — the walkthrough for your first real build.
- [`34-settings.md`](34-settings.md) — tour of every settings panel.
