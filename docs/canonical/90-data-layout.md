# Data layout

Agent Wire Node keeps all your data in one well-defined directory. This doc is a map: what lives where, why, and what you'd touch it for.

The root is:

```
~/Library/Application Support/wire-node/
```

On macOS (the supported platform in the alpha). Derived from `dirs::data_local_dir()`.

This is the source of truth for your node. The binary in `/Applications` is replaceable; this directory is not.

---

## Top level

```
~/Library/Application Support/wire-node/
├── pyramid.db                   — SQLite; all pyramid metadata, nodes, edges, annotations
├── pyramid.db-shm, pyramid.db-wal — SQLite WAL companions; don't touch
├── pyramid_config.json          — node-wide config (auth token, model defaults, feature flags)
├── onboarding.json              — user onboarding choices (node name, storage cap, toggles)
├── session.json                 — current login tokens (refreshed on login)
├── node_identity.json           — durable node identity (handle + token)
├── .credentials                 — API keys, 0600 perms, YAML
├── compute_market_state.json    — live compute market state (offers, job counters)
├── wire-node.log                — application log (truncated on app restart)
├── chains/                      — chain variants + prompts you've edited or pulled
├── documents/                   — cached documents (mesh hosting; opt-in)
└── builds/                      — per-build caches and intermediate artifacts
```

---

## The SQLite database (`pyramid.db`)

The bulk of your state. Every pyramid you've built, every annotation, every FAQ entry, every cost log, every DADBEAR mutation, every contribution. Multiple gigabytes in active use.

You don't edit `pyramid.db` directly. Agent Wire Node's API surfaces (UI, CLI, MCP, HTTP) are the way in. If you need raw SQL for analysis, read-only access is fine (`sqlite3 pyramid.db ".tables"`), but write operations can corrupt state if they bypass the application's invariants.

**Backup regularly.** If you care about the pyramids you've built, back up `pyramid.db` + `pyramid.db-shm` + `pyramid.db-wal` together. An inconsistent WAL state after a crash mid-write is the rarest case of corruption; standard WAL-aware backup tools handle this correctly.

See [`92-backup-reset-migrate.md`](92-backup-reset-migrate.md) for procedures.

---

## Config files

### `pyramid_config.json`

Node-wide operational config. Fields:

- `auth_token` — your node's bearer token for local API access.
- `primary_model`, `fallback_model_1`, `fallback_model_2` — legacy model cascade.
- `primary_context_limit` — context window for primary model.
- `use_chain_engine` — feature flag (see [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md)).
- Other runtime knobs.

Safe to edit by hand for operational changes (toggle the chain engine, update models). Hot-reloaded on most LLM calls; some fields require restart.

### `onboarding.json`

User-chosen values from the onboarding wizard. Fields:

- `node_name` — human-readable name.
- `storage_cap_gb` — disk allocation for cached documents.
- `mesh_hosting_enabled` — toggle for mesh hosting.
- `auto_update_enabled` — toggle for app auto-update.
- `completed_at` — timestamp of onboarding completion.

Editable via Settings UI; rarely edited by hand.

### `session.json`

Current Supabase session — access_token, refresh_token, user_id, node_id, api_token. Refreshed on every login. Wiped on logout.

Don't hand-edit; let the app manage this.

### `node_identity.json`

**The important one to back up.** Contains:

- `node_handle` — your durable node identifier.
- `node_token` — random 32-byte token used by other nodes and the coordinator to verify your identity.

If you lose this, your node becomes a new node from the Wire's perspective — different handle, no reputation history, different registration. Back it up alongside `pyramid.db`.

### `.credentials`

API keys in YAML, 0600 permissions. See [`12-credentials-and-keys.md`](12-credentials-and-keys.md).

Never commit this to git. Never include it in a public backup. Loss means re-issuing keys from your providers; not catastrophic but annoying.

### `compute_market_state.json`

Live market state snapshot — active offers, in-flight jobs, counters. Updated frequently while the market is active. On a normal shutdown this is written cleanly; on a crash it may be stale, and Agent Wire Node reconciles on next start.

Don't hand-edit.

---

## Logs

`wire-node.log` is the application log. Truncated on each app start (the app currently rotates-by-truncation, not rolling files). The last ~500 lines are accessible via `pyramid_logs` CLI.

For longer-running diagnostics, redirect app stderr to a file yourself:

```bash
"/Applications/Agent Wire Node.app/Contents/MacOS/Agent Wire Node" 2>> ~/wire-node-extended.log
```

Launching from terminal captures stderr; launching from Finder does not. See [`91-logs-and-diagnostics.md`](91-logs-and-diagnostics.md).

---

## `chains/` — your chain variants and prompts

```
chains/
├── defaults/           — shipped chains + prompts (conceptually read-only; updated by app updates)
├── variants/           — your edits
└── prompts/
    ├── defaults/
    └── variants/
```

The `defaults/` subtree is managed by the app. Updating Agent Wire Node can overwrite it. If you want to persist your edits, put them in `variants/` with a distinct ID.

Chain variants are loaded on app start or when the chain registry is refreshed. Prompt changes are picked up lazily on next step dispatch.

---

## `documents/`

Cache for documents you host as mesh member, plus downloaded copies of documents referenced by pulled pyramids. Managed by the mesh sync worker. Disk usage capped by `storage_cap_gb`.

Safe to delete contents if you need disk space; Agent Wire Node will re-download on demand.

---

## `builds/`

Per-build cache directories, one per `(slug, build_id)`. Contains:

- Step outputs (JSON + intermediates).
- LLM call traces (for debugging).
- Token cost accounting details.

Bulk consumer of disk for active work. Can be cleaned up for completed builds without affecting pyramid state (the pyramid is in `pyramid.db`; the build cache is for diagnostic walk-back).

Agent Wire Node will clean old build directories on its own schedule. If disk is tight, deleting older build folders is safe.

---

## The tunnel directory (`cf-tunnel/`)

If Cloudflare Tunnel is configured:

```
cf-tunnel/
├── config.yaml
├── tunnel.json         — tunnel credentials
└── ...
```

Managed by the tunnel subsystem. Don't hand-edit unless you know what you're doing.

---

## What to back up

**Minimum (tests your setup but doesn't preserve data):**

- `onboarding.json`
- `.credentials`

**Standard (preserves node identity + keys):**

- `node_identity.json` + `onboarding.json` + `.credentials`

**Full (preserves pyramids and all work):**

- Everything in the data directory.

Backups should include WAL state (`pyramid.db-shm`, `pyramid.db-wal`) — either back up while Agent Wire Node isn't running, or use a backup tool that's WAL-aware (Time Machine is; most rsync invocations aren't unless scripted correctly).

See [`92-backup-reset-migrate.md`](92-backup-reset-migrate.md).

---

## Sizes, roughly

- **Base install** (empty node, just onboarded): ~10 MB.
- **One small code pyramid built**: +100-300 MB (DB grows fast with extraction).
- **A month of active use across several pyramids**: 2-5 GB.
- **With local Ollama models**: add the model sizes (several GB to tens of GB per model; Ollama's own storage is outside Agent Wire Node's data dir).
- **With mesh hosting**: up to your configured `storage_cap_gb`.

Most operators' data dir grows slowly after the first few builds. DADBEAR maintenance adds incrementally; new pyramids are the main jump factor.

---

## Migrating to a new machine

Stop Agent Wire Node. Copy the entire `~/Library/Application Support/wire-node/` directory to the new machine. Install Agent Wire Node there. Launch.

Your node identity, pyramids, annotations, credentials — all intact. Wire sees the node re-connect from a new IP but with the same handle and token; no re-registration required.

See [`92-backup-reset-migrate.md`](92-backup-reset-migrate.md) for full migration procedure.

---

## Where to go next

- [`91-logs-and-diagnostics.md`](91-logs-and-diagnostics.md) — log content and diagnostics.
- [`92-backup-reset-migrate.md`](92-backup-reset-migrate.md) — the procedures.
- [`94-uninstall.md`](94-uninstall.md) — clean removal.
- [`12-credentials-and-keys.md`](12-credentials-and-keys.md) — `.credentials` details.
