# Backup, reset, and migrate

Your Agent Wire Node data directory (`~/Library/Application Support/wire-node/`) is the source of truth for your node. This doc covers backing it up, resetting specific parts of it, and moving it between machines.

---

## What to back up

### Minimum (credentials + preferences)

- `onboarding.json` — node name, preferences.
- `.credentials` — API keys. **Do not** include in public backups.

Loss: you re-onboard and re-enter API keys. Pyramids and identity are lost.

### Standard (preserves node identity)

All of the minimum, plus:

- `node_identity.json` — durable handle + token.
- `pyramid_config.json` — operational config, auth token.
- `session.json` — current session (though this refreshes on next login).

Loss: you keep pyramids (they're in `pyramid.db` which needs separate handling) but might need to re-login or re-register your node.

### Full (everything)

All of the above plus:

- `pyramid.db` + `pyramid.db-shm` + `pyramid.db-wal` — all pyramid data.
- `chains/` — your chain variants and edited prompts.
- `compute_market_state.json` — live market state.
- `documents/`, `builds/` — caches (can be rebuilt but backing up saves time).

This is the "I could re-create my whole node" backup.

---

## Backup approaches

### Time Machine

macOS's Time Machine backs up the whole `~/Library/Application Support/wire-node/` directory with WAL-awareness handled by the system. This is the easiest path; if you have Time Machine configured, you're probably already backed up.

### Manual snapshot with Agent Wire Node stopped

```bash
# Stop Agent Wire Node from the menu bar or with:
pkill -x "Agent Wire Node"

# Copy the data directory
cp -R "$HOME/Library/Application Support/wire-node" \
      "$HOME/wire-node-backup-$(date +%Y%m%d)"

# Restart Agent Wire Node
open -a "Agent Wire Node"
```

Stopping Agent Wire Node ensures SQLite WAL state is consistent. If you can't stop the app, use the SQLite online backup approach below.

### SQLite online backup

```bash
sqlite3 "$HOME/Library/Application Support/wire-node/pyramid.db" \
  ".backup '$HOME/wire-node-db-backup-$(date +%Y%m%d).sqlite'"
```

This produces a clean SQLite snapshot without stopping Agent Wire Node. Combine with a copy of the config files for a minimum-downtime backup.

### rsync

```bash
rsync -a "$HOME/Library/Application Support/wire-node/" \
         "$HOME/wire-node-backup/"
```

Only safe if Agent Wire Node is stopped. Incremental after the first run.

---

## Restoring from backup

Same machine:

```bash
# Stop Agent Wire Node first
pkill -x "Agent Wire Node"

# Move current dir out of the way (or delete if confident)
mv "$HOME/Library/Application Support/wire-node" \
   "$HOME/Library/Application Support/wire-node.broken"

# Restore backup
cp -R "$HOME/wire-node-backup-YYYYMMDD" \
      "$HOME/Library/Application Support/wire-node"

# Restart
open -a "Agent Wire Node"
```

Everything from that point in time is restored.

---

## Migrating to a new machine

Goal: same node identity on new hardware.

1. **On the source machine:** full backup as above (stop Agent Wire Node first to ensure consistency).
2. **Install Agent Wire Node** on the new machine via the normal install.
3. **Before first launch:** don't launch yet. Replace the empty data directory with your backup:
   ```bash
   rm -rf "$HOME/Library/Application Support/wire-node"
   cp -R /path/to/backup "$HOME/Library/Application Support/wire-node"
   ```
4. **Launch Agent Wire Node.** It reads the existing `node_identity.json`, re-establishes the tunnel, reconnects to the Wire as the same node.
5. **Verify.** Check Settings → Agent Wire Node Settings → Node ID matches the old machine. Check a pyramid still opens correctly. Check `.credentials` still has your keys.

Your Wire-side reputation, handles, and pulled contributions are preserved.

**Note:** don't run two machines with the same `node_identity.json` simultaneously. The Wire will see conflicting sessions and behave unpredictably. If you want true multi-node operation (both machines online at once under one account), they need distinct node identities — see the multi-node fleet section in [`05-steward-experimentation-vision.md`](05-steward-experimentation-vision.md).

---

## Partial resets

### Reset pyramids only (keep node identity, credentials)

```bash
pkill -x "Agent Wire Node"
rm "$HOME/Library/Application Support/wire-node/pyramid.db"*
rm -rf "$HOME/Library/Application Support/wire-node/builds"
open -a "Agent Wire Node"
```

On next launch, Agent Wire Node creates a fresh empty database. Your node identity, API keys, and onboarding choices persist; all pyramids are gone.

### Reset credentials only

Edit or delete `.credentials` via Settings → Credentials, or directly:

```bash
rm "$HOME/Library/Application Support/wire-node/.credentials"
```

Re-enter keys via Settings on next launch.

### Reset node identity (become a new node on the Wire)

```bash
rm "$HOME/Library/Application Support/wire-node/node_identity.json"
```

On next launch, Agent Wire Node generates a new identity. From the Wire's perspective this is a fresh node — no reputation history, no published contributions visible under the new identity (your old ones remain under the old handle if you still have access to it).

This is irreversible: you can't recover the old identity unless you still have the `node_identity.json` file somewhere.

### Reset logs (small, rarely needed)

```bash
> "$HOME/Library/Application Support/wire-node/wire-node.log"
```

Truncate. Agent Wire Node keeps writing to the same file.

---

## Factory reset (complete wipe)

When you want a completely fresh start:

```bash
pkill -x "Agent Wire Node"
rm -rf "$HOME/Library/Application Support/wire-node"
open -a "Agent Wire Node"
```

Everything is gone — pyramids, identity, credentials, logs. Agent Wire Node boots into a fresh onboarding experience.

If you also want to wipe the app itself, see [`94-uninstall.md`](94-uninstall.md).

---

## Diffing between backups

Useful when debugging: what changed between two backups?

```bash
# Config diffs
diff ~/wire-node-backup-A/pyramid_config.json ~/wire-node-backup-B/pyramid_config.json

# Chain variant diffs
diff -r ~/wire-node-backup-A/chains/variants ~/wire-node-backup-B/chains/variants

# Database schema changes (if any)
sqlite3 ~/wire-node-backup-A/pyramid.db ".schema" > /tmp/schema-A.sql
sqlite3 ~/wire-node-backup-B/pyramid.db ".schema" > /tmp/schema-B.sql
diff /tmp/schema-A.sql /tmp/schema-B.sql
```

Schema changes usually mean an app update happened between backups.

---

## What's not backed up by backing up this directory

- **Ollama models.** They live in Ollama's own directory (`~/.ollama/models/`). Back up separately if you care (they're large).
- **Your published contributions on the Wire.** Published artifacts live on the Wire; backing up Agent Wire Node locally doesn't back up your publish history. Consumers who pulled your contributions have copies, and the coordinator has metadata, but the definitive copy is on the Wire.
- **External documents and corpora.** Only documents cached under `documents/` are in the backup. Source files you index (code repos, PDFs) live in their own locations and aren't Agent Wire Node's responsibility to back up.

---

## Where to go next

- [`90-data-layout.md`](90-data-layout.md) — what each file does.
- [`93-updates-and-dadbear-app.md`](93-updates-and-dadbear-app.md) — how app updates interact with your data.
- [`94-uninstall.md`](94-uninstall.md) — complete removal.
