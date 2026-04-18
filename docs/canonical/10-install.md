# Installing Wire Node

This doc covers getting Wire Node onto your machine, whether you got a pre-built `.dmg` from the alpha channel or you are building from source. It also covers what to expect from the installer, what gets put where on disk, and how to confirm the install worked.

**System requirements (alpha):**

- macOS 11 or later (Big Sur+). Both Apple Silicon (arm64) and Intel (x86_64) builds exist.
- At least 4 GB of free disk space for the app and initial data.
- Substantially more disk if you plan to build large pyramids or run local LLMs via Ollama — 50 GB is a reasonable working allocation for a serious user.
- An internet connection for first-run login and for anything Wire-related. Once set up, building pyramids with cloud LLMs needs the network; building with Ollama locally does not.

Linux and Windows are **not** supported in the alpha. Support is on the roadmap but not dated.

---

## Installing from a pre-built `.dmg` (the common path)

1. Obtain the `.dmg` from the alpha channel you were given access to. The filename looks like `Wire Node_0.3.0_aarch64.dmg` or `Wire Node_0.3.0_x64.dmg` — pick the one that matches your Mac's architecture. If you don't know, Apple Silicon Macs (M1 and later) want aarch64; older Macs want x64.

2. Double-click the `.dmg` to mount it. macOS opens a Finder window showing **Wire Node.app** and a shortcut to `/Applications`.

3. Drag **Wire Node.app** onto the `/Applications` shortcut. Wait for the copy to complete.

4. Eject the `.dmg` by dragging it to the trash in Dock (or right-click → Eject).

5. Open `/Applications` in Finder. Right-click (or Control-click) **Wire Node** and choose **Open**. Use this right-click-Open path the first time — it tells macOS Gatekeeper that you intend to run this app. Plain double-click on a freshly-downloaded app may be blocked.

6. macOS will prompt: *"Wire Node is from an identified developer. Are you sure you want to open it?"* Click **Open**.

7. The loading screen appears with a tunneling "W" logo. After a few seconds the login screen loads.

If macOS refuses to let you run it with a message like *"Wire Node cannot be opened because it is from an unidentified developer"* or *"Apple could not verify Wire Node is free of malware"*, you are on a build with a different signing state than expected. See Troubleshooting below.

## Building from source

If you want to run the latest development build, or contribute changes back, you can build from source.

**Prerequisites:**

- [Rust 1.75+](https://rustup.rs/): `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- [Node.js 20+](https://nodejs.org/): `brew install node` or use your preferred version manager.
- Tauri CLI v2: `cargo install tauri-cli --version "^2.0"`
- Xcode Command Line Tools: `xcode-select --install`

**Build steps:**

```bash
git clone <the wire-node repo>
cd agent-wire-node
npm install
cargo tauri build
```

This produces a `.dmg` and `.app` under `src-tauri/target/release/bundle/`. Install the `.dmg` the same way as a pre-built one, or drag the `.app` directly from the build output into `/Applications`.

**For development with hot-reload:**

```bash
cargo tauri dev
```

This runs the frontend via Vite dev server and the backend in debug mode. Window opens; changes to React code hot-reload; Rust changes trigger a rebuild. Use this if you are tinkering with the app itself, not if you are just using it.

---

## What gets put where

Wire Node puts user data in one well-defined directory so you can back it up, move it between machines, or reset from scratch cleanly. Installing the app itself adds one `.app` bundle and nothing else outside user data.

**The application:**

```
/Applications/Wire Node.app
```

**Your data:**

```
~/Library/Application Support/wire-node/
├── pyramid.db                    — your pyramids and all their state
├── .credentials                  — API keys (permissions locked to 0600)
├── session.json                  — current login session (refreshed on login)
├── node_identity.json            — durable node identity
├── onboarding.json               — onboarding choices (node name, storage cap, toggles)
├── wire-node.log                 — application log (truncated on app restart)
├── compute_market_state.json     — live compute market state
├── chains/                       — chain variants and prompts you've edited or pulled
├── documents/                    — cached documents from mesh hosting (if enabled)
└── builds/                       — per-build caches and intermediate artifacts
```

This directory is the source of truth for your node. The app binary in `/Applications` is replaceable; the data directory is not.

For full detail on every file and when you'd touch it, see [`90-data-layout.md`](90-data-layout.md).

---

## Confirming the install

After the first launch:

1. **The loading screen** shows a "W" and a spinner while the backend boots. This is normal the first time — it's doing first-run setup of the database, node identity, and log file.
2. **The login screen** appears. If it doesn't, see Troubleshooting below.
3. **Open a terminal** and run: `curl http://localhost:8765/health`
4. You should see a JSON response like `{"status":"ok","version":"0.3.0",...}`. This confirms the HTTP server is up and you can reach it from tools like `pyramid-cli`.

If `/health` responds, the install is fine. Proceed to [`11-first-run-and-onboarding.md`](11-first-run-and-onboarding.md).

---

## Troubleshooting installation

### "Wire Node cannot be opened because Apple could not verify it"

The build you got is signed but not notarized, or notarization is still in progress. Two options:

1. Right-click the app in `/Applications`, choose **Open**, then confirm in the dialog. This bypasses the check once; future launches are fine.
2. From a terminal: `sudo xattr -rd com.apple.quarantine "/Applications/Wire Node.app"` then launch normally.

### "Wire Node is damaged and can't be opened"

The `.dmg` probably downloaded truncated. Re-download. If it persists, you may need to remove the quarantine flag as above.

### The app starts, but the window never opens

Check `~/Library/Application Support/wire-node/wire-node.log`. If it's empty or missing, the backend crashed before it could log. Start the app from Terminal to see stderr:

```bash
"/Applications/Wire Node.app/Contents/MacOS/Wire Node"
```

Common causes: another process is already bound to port 8765 (`lsof -i :8765` to find it), or the data directory is corrupt (rare; nuke and restart — see [`94-uninstall.md`](94-uninstall.md)).

### `/health` returns a connection error

The HTTP server didn't bind. Same causes as above: port conflict, backend crash, or the app isn't actually running. Quit the app fully (menu bar → Quit Wire Node), check Activity Monitor for lingering `Wire Node` processes and kill them, then relaunch.

### Port 8765 is already in use

Wire Node currently hardcodes port 8765 for the HTTP server. If that port is taken by another app, Wire Node will fail to start. Identify and stop the other process:

```bash
lsof -i :8765
# Shows the process holding the port. Decide whether to stop it.
```

Changing Wire Node's port is not exposed in the UI yet.

### Clean reinstall

If the install is wedged and you want to start completely fresh:

```bash
rm -rf /Applications/Wire\ Node.app
rm -rf ~/Library/Application\ Support/wire-node
# re-install the .dmg as in the main flow
```

This wipes everything, including your node identity, your pyramids, and your credentials. Make sure you have a backup if you care. See [`92-backup-reset-migrate.md`](92-backup-reset-migrate.md).

---

## Updating the app

Wire Node has a built-in updater (DADBEAR for the app itself). When an update is available, you see a banner in **Settings → Wire Node Settings → Auto-Update**. You click **Install update** and the app downloads, verifies the signature, replaces itself, and restarts.

User data is preserved across updates; onboarding does not re-run.

You can turn off auto-update from the same Settings panel if you want to pin a version. See [`93-updates-and-dadbear-app.md`](93-updates-and-dadbear-app.md).

---

## Where to go next

- [`11-first-run-and-onboarding.md`](11-first-run-and-onboarding.md) — first launch, login, and the onboarding wizard.
- [`12-credentials-and-keys.md`](12-credentials-and-keys.md) — setting up your API keys.
- [`20-pyramids.md`](20-pyramids.md) — understand the main mode before building your first pyramid.
