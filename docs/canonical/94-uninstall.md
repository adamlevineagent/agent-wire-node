# Uninstall

Removing Wire Node from your machine. This doc covers full removal (app + data), keeping your data (in case you reinstall later), and clean Wire disconnection.

---

## What to think about before uninstalling

- **Published contributions on the Wire don't go away.** If you've published contributions, they remain on the Wire under your handle. Uninstalling your local node doesn't retract them. If you want to retract published work, do so from Tools mode before uninstalling.
- **Your Wire handle is durable.** You can delete the local node entirely and your handle still exists. Returning later with a backup of `node_identity.json` or creating a fresh node under the same handle (from a different machine) is fine.
- **Local pyramids go away with the data directory.** Unless you've published them or backed them up, local pyramids are lost when the data directory is deleted.

---

## Full uninstall

Wipes the app binary and all data.

```bash
# Stop the app
pkill -x "Wire Node"

# Remove the application
rm -rf "/Applications/Wire Node.app"

# Remove all data
rm -rf "$HOME/Library/Application Support/wire-node"

# Optional: clean Cloudflare Tunnel config if Wire Node added its own
# (Wire Node's tunnel config is inside the data dir above, but if you
# had cloudflared configured separately, that lives in ~/.cloudflared)
```

No other locations to clean — Wire Node is self-contained in the two paths above.

Your Wire account, published contributions, and handle reputation all persist on the Wire. A future reinstall under the same handle (using a backed-up `node_identity.json`) picks up where you left off, minus the local pyramids.

---

## Uninstall the app but keep data

When you're planning to reinstall or move to a new machine and want to keep your pyramids and identity:

```bash
# Stop the app
pkill -x "Wire Node"

# Remove just the binary
rm -rf "/Applications/Wire Node.app"

# Leave the data directory
# ~/Library/Application Support/wire-node/ stays intact
```

Reinstalling Wire Node later finds the existing data and continues from where it was. No re-onboarding, no re-registration.

---

## Uninstall the data but keep the app

The reverse — usually because you want to start over with a clean slate but don't want to re-download the app.

```bash
pkill -x "Wire Node"
rm -rf "$HOME/Library/Application Support/wire-node"
# Next launch will re-onboard you as a fresh node.
```

Note: this makes you a **new node from the Wire's perspective.** Your previous handle and contributions remain (on the Wire and in whatever backups you have), but this new install won't be connected to them.

---

## Clean Wire disconnection (without uninstalling)

If you want to disconnect from the Wire without removing the app (e.g. going offline for a while, or deciding to use Wire Node purely locally):

1. **Settings → Wire Node Settings → Compute Participation Policy → Coordinator.** Stops market participation.
2. **Settings → Wire Node Settings → Mesh hosting → off.** Stops hosting documents for others.
3. **Settings → Wire Node Settings → Tunnel → disable** (if offered). Your node is no longer reachable from the Wire.
4. **Retract published contributions** you no longer want visible. From Tools → your contribution → Retract.

The app still works locally. You can still build pyramids, query via local CLI/MCP, drive Claude via MCP against local pyramids. The Wire just doesn't know you're there.

Reversing: flip the toggles back on. Publish a fresh version of any contributions you retracted if you want them visible again.

---

## Release your handle

If you want your Wire handle available for someone else (or you're permanently winding down your participation):

**Identity mode → your handle → Release.**

- The handle is marked as available after a cool-down period.
- Your published contributions remain attributed to you (immutable) but the handle itself can be re-registered.

This is rare and usually irreversible once someone else registers the handle. Think before doing it.

---

## Transfer your handle

If you want to give your handle and its reputation to someone else:

**Identity mode → your handle → Transfer.**

- Generates a transfer envelope.
- Recipient accepts on their node.
- Handle ownership moves.
- Historical contributions remain attributed to the transition point; new contributions go under the new owner.

Transfers are logged publicly. They're for legitimate successions (account consolidation, handing a project to someone else, organizational transitions), not for trading handles casually.

---

## After full uninstall

If you decide you want Wire Node back:

- Re-install from the `.dmg`.
- Onboard fresh (or restore from backup — see [`92-backup-reset-migrate.md`](92-backup-reset-migrate.md)).

No residual state remains to confuse a fresh install.

---

## Where to go next

- [`90-data-layout.md`](90-data-layout.md) — the paths you'd delete.
- [`92-backup-reset-migrate.md`](92-backup-reset-migrate.md) — save what you want before you uninstall.
- [`33-identity-credits-handles.md`](33-identity-credits-handles.md) — handle management.
