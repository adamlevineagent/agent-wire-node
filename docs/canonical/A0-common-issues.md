# Common issues

The grab-bag of things that surprise new users. Check here before digging into specifics.

For active known issues the team is tracking, see [`docs/PUNCHLIST.md`](../PUNCHLIST.md).

---

## Installation

### "Wire Node cannot be opened because Apple could not verify it"

Right-click the app in `/Applications`, choose **Open**, confirm. Bypasses Gatekeeper once. Future launches work normally.

If that doesn't work: `sudo xattr -rd com.apple.quarantine "/Applications/Wire Node.app"`.

### "Wire Node is damaged and can't be opened"

The `.dmg` downloaded truncated. Re-download. If the issue persists, quarantine-removal as above.

### App launches but window never opens

Backend crashed before the UI attached. Launch from terminal to see stderr:

```bash
"/Applications/Wire Node.app/Contents/MacOS/Wire Node"
```

Common causes: port 8765 in use by another process (`lsof -i :8765`), corrupt data directory (rare — see [`92-backup-reset-migrate.md`](92-backup-reset-migrate.md)), missing dependencies (uncommon with the bundled build).

### Port 8765 already in use

Another app is bound to 8765. Find and stop it:

```bash
lsof -i :8765
# shows the offending PID; kill it or use a different app that claimed it.
```

Wire Node's port is currently hardcoded. Changing it requires a source build.

---

## Login and onboarding

### Magic link email doesn't arrive

Check spam. Wait 60s and try again. If repeated attempts fail, check the alpha channel for service outages.

### Magic link opens browser but Wire Node doesn't launch

Deep link handler didn't register. Usually self-heals on second launch. If not, paste the URL into the "Paste magic link" text box on the login screen.

### Onboarding wizard reappears after completing it

`onboarding.json` didn't write. Check disk space and permissions on the data directory.

### "Node not registered" after login

Your login succeeded with Supabase but Wire Node couldn't register with the Wire coordinator. Check tunnel status (Settings). If the tunnel is offline, registration can't complete. Retry the tunnel; restart the app; if persistent, check the alpha channel.

---

## Credentials

### "Credentials file has unsafe permissions"

Wire Node refuses to read `.credentials` if its permissions are wider than 0600. Click **Fix permissions** in Settings → Credentials, or:

```bash
chmod 0600 "$HOME/Library/Application Support/wire-node/.credentials"
```

### "Variable `${OPENROUTER_KEY}` is not defined"

A config references a credential variable you haven't set. Open Settings → Credentials → Add credential → name it `OPENROUTER_KEY`.

### Provider test fails: 401 Unauthorized

Credential exists but is wrong (typo during paste, key revoked, wrong provider). Regenerate at the provider's dashboard and update.

### Provider test fails: insufficient credits

You have a key but no funds at the provider. Top up. If you configured OpenRouter's management key (`OPENROUTER_MANAGEMENT_KEY`), the oversight panel will warn you proactively when balance is low.

---

## Build failures

See [`A1-build-stuck-or-failed.md`](A1-build-stuck-or-failed.md).

Quick checks:

- Builds failing with "no OpenRouter key" when Ollama is on: P0-1 wiring gap. See [`51-local-mode-ollama.md`](51-local-mode-ollama.md).
- Builds timing out repeatedly: provider issues. Check **Settings → Providers → Test**.
- Builds stuck at 0%: port conflict on 8765 or the backend is frozen. Force-quit and restart.

---

## Cost surprises

### Build cost more than expected

Open Understanding → Oversight → Cost Rollup. Breakdown by source and phase. If extraction is dominating, route the `extractor` tier to a cheaper model. If synthesis dominated on a large pyramid, that's normal — it fires once at apex per build.

### DADBEAR cost keeps climbing

Check the DADBEAR panel for the pyramid in question (Understanding → pyramid detail drawer → DADBEAR). Look for:

- High debounce minutes (check more often = more LLM calls).
- Low `min_changed_files` threshold.
- Staleness evaluation tier routed to an expensive model (use a cheaper tier for stale checks).

Archive pyramids you aren't using — archived pyramids don't run DADBEAR.

### Spend higher than expected without active builds

Check the recent-calls table in Cost Rollup. If you see many calls you didn't trigger:

- DADBEAR running on pyramids you forgot about.
- Compute market jobs being served (check Operations → Queue).
- An agent session running a script against your node.

---

## Market and network

See [`A2-provider-and-network-errors.md`](A2-provider-and-network-errors.md).

### Tunnel dropped

Settings → Wire Node Settings → Tunnel Status → Retry. Usually self-heals. If persistent, check your network (Cloudflare blocks? VPN interference?).

### Compute market jobs not arriving

- Check your node is Hybrid or Worker mode.
- Check your offers are active (Market → Compute → Advanced → Offer Manager).
- Check tunnel is online.
- Check reputation isn't tanked.

---

## UI quirks

### Pyramid Surface window is slow or stuttering

Large pyramids (tens of thousands of nodes) strain the visualization. Switch to density layout, turn off expensive overlays (web edges, provenance), use search instead of visual scanning. For extreme cases, use CLI or MCP tools — not bounded by visualization.

### Notifications missing or stuck unread

Quit and restart Wire Node. Transient UI state sometimes gets confused.

### Tool panel doesn't refresh after pulling a contribution

Click refresh explicitly, or restart the app. Contribution store updates should propagate but occasionally don't until refresh.

---

## Things that look like bugs but aren't

### "I edited a chain YAML but builds still use the old behavior"

`use_chain_engine` is probably false on your install. Check `pyramid_config.json` — the field must be `true` for chain YAMLs to be consulted. See [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md).

### "My pyramid says it's 'complete' but nothing was extracted"

Build succeeded but found no extractable content. Source files may all be ignored (binaries, lockfiles, etc.) or may all be below the min-file-count threshold. Check the ingest preview when creating the pyramid.

### "I annotated a node but nothing changed in the FAQ"

FAQ processing runs asynchronously. The annotation is saved immediately; the FAQ entry may take a minute to materialize. Refresh the FAQ directory after a minute.

### "My pyramid built twice"

Check Understanding → Builds. If you see two completed builds for the same slug, DADBEAR or a test trigger kicked a rebuild. Check DADBEAR's debounce and runaway-threshold settings.

---

## When to ask for help

If none of the above covers it:

1. Capture the last few hundred lines of `wire-node.log`.
2. Note your version (Settings → About).
3. Describe what you did, what you expected, what you got.
4. Ask in the alpha channel.

The more specific, the faster the fix.

---

## Where to go next

- [`A1-build-stuck-or-failed.md`](A1-build-stuck-or-failed.md) — build-specific.
- [`A2-provider-and-network-errors.md`](A2-provider-and-network-errors.md) — provider and network.
- [`A3-staleness-and-breakers.md`](A3-staleness-and-breakers.md) — DADBEAR issues.
- [`91-logs-and-diagnostics.md`](91-logs-and-diagnostics.md) — diagnostic surfaces.
- [`docs/PUNCHLIST.md`](../PUNCHLIST.md) — authoritative known-issue list.
