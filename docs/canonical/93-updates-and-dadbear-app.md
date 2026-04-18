# Updates (the app, not DADBEAR for pyramids)

Agent Wire Node ships updates through a built-in updater — the same self-updating pattern the project calls DADBEAR, but applied to the binary rather than to pyramids. This doc covers how app updates work, how to pin a version if you need stability, and what to watch when an update lands.

Not to be confused with [DADBEAR for pyramids](25-dadbear-oversight.md), which is the system that keeps pyramid content current as source files change. Same mnemonic, different scope.

---

## How app updates work

On app startup, Agent Wire Node checks the update endpoint (`newsbleach.com/api/releases/wire-node/...`) for a newer version than what's running. If a newer version exists:

1. A notification appears — both in Operations and as a banner in Settings → Agent Wire Node Settings → Auto-Update.
2. The banner shows: version number, release notes link.
3. You click **Install update**. Agent Wire Node downloads the update (signed, verified), applies it, and restarts into the new binary.
4. Your data directory is preserved. Onboarding does not re-run.

Updates are **not** auto-applied unless you set them to be. The default is "notify on available, wait for confirm."

To change: **Settings → Agent Wire Node Settings → Auto-Update** has a toggle. With auto-apply on, updates install silently during an idle window. With it off (default), you get the banner and confirm manually.

---

## Pinning a version

Turn off auto-update. Agent Wire Node stops checking for updates; the current version stays put indefinitely.

Useful when:

- You've established a workflow you don't want disrupted mid-project.
- A recent update introduced a regression you want to wait on a fix for.
- You're running alongside a specific version of another tool that expects a specific Agent Wire Node API.

To re-enable updates, toggle back on. The next check picks up the latest available version.

Auto-update can also be disabled via `pyramid_config.json` (the `auto_update_enabled` field in `onboarding.json` mirrors the UI toggle).

---

## What gets updated

Everything in `/Applications/Agent Wire Node.app`:

- The Tauri binary (Rust backend + React frontend bundle).
- Bundled chain defaults under `chains/defaults/`.
- Bundled prompts under `chains/prompts/defaults/`.
- Other static assets.

What does **not** get updated by app updates:

- Your data directory (`~/Library/Application Support/wire-node/`).
- Your chain variants (`chains/variants/`).
- Your prompt variants (`chains/prompts/variants/`).
- Your credentials, session, or node identity.

Your customizations are safe across updates. The only time an update could affect them is if a schema migration applies to your contribution store — in which case you get a migration review modal, not a silent conversion.

---

## Breaking changes

Alpha means breaking changes happen. They fall into categories:

**App-binary breaking change.** Fresh install behavior changes. Updates that involve this usually come with migration logic that runs on first launch after update.

**Chain schema breaking change.** The shipped chains' YAML structure evolves. Your variants may need migration — Tools mode's **Needs Migration** tab flags this.

**Config schema breaking change.** A schema type's definition changes. Migration review modals surface this; you accept or postpone.

**Database schema breaking change.** The SQLite schema changes. Agent Wire Node runs migrations on launch; irreversible migrations are backed up first.

Release notes (linked from the update banner) call out breaking changes explicitly.

---

## Checking the current version

Three places:

- **Settings → About.** Version number, build hash.
- **Sidebar footer.** Small version text at the bottom.
- **CLI:** `pyramid-cli health` returns `{ "version": "0.3.0", ... }`.

Include the version in any bug report.

---

## Rollback

The built-in updater doesn't have a rollback button. If an update introduces a regression and you need the previous version:

1. Back up your data directory (just in case).
2. Download the previous version's `.dmg` from the alpha channel.
3. Drag Agent Wire Node out of `/Applications` to the trash.
4. Install the old version from its `.dmg`.
5. Launch. Your data directory is unchanged; the old binary reads it as before (assuming the DB schema is compatible — a schema that ran forward migrations can't go back easily).

If the updater introduces a DB schema migration that breaks rollback, get help in the alpha channel before attempting it.

---

## Update signing and verification

Updates are signed with the project's release key. The updater verifies the signature before installing; an unsigned or badly-signed update is refused.

This protects against a compromise of the update server itself — an attacker can't push malicious updates to alpha testers without also holding the release key.

If you see an update fail signature verification, stop and ask — don't manually bypass.

---

## What to do when an update arrives

The boring path that works: click **Install update**, let it restart, verify it still works.

The careful path for long-running work:

1. Note what you're in the middle of.
2. Back up your data directory.
3. Apply the update.
4. Launch. Check Health Status (Settings → Agent Wire Node Settings) — all green?
5. Open a pyramid you trust, run a small query. Does it behave?
6. If anything looks off, see rollback above or file in the alpha channel.

Updates usually take under a minute end-to-end. The restart is the biggest interruption.

---

## Where to go next

- [`25-dadbear-oversight.md`](25-dadbear-oversight.md) — DADBEAR for pyramids (the other "DADBEAR").
- [`92-backup-reset-migrate.md`](92-backup-reset-migrate.md) — back up before a big update.
- [`34-settings.md`](34-settings.md) → Auto-Update panel.
