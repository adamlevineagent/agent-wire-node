# Logs and diagnostics

Wire Node writes operational signals to a log file and exposes health checks, cost rollups, and subsystem diagnostics through the UI. This doc covers what's available and when to reach for each.

---

## The log file

Location: `~/Library/Application Support/wire-node/wire-node.log`.

Rotation: **truncated on each app start** (no rolling files in the current build). If you want to capture logs across restarts, redirect stderr when launching from terminal:

```bash
"/Applications/Wire Node.app/Contents/MacOS/Wire Node" 2>> ~/wire-node-extended.log
```

Log level: `info` by default. To get more verbose diagnostics, set `RUST_LOG` before launch:

```bash
RUST_LOG=debug "/Applications/Wire Node.app/Contents/MacOS/Wire Node"
```

Common log namespaces:

- `pyramid::*` — pyramid builds + DADBEAR.
- `compute_market::*` — market offers/jobs.
- `fleet::*` — fleet dispatch events.
- `warp::*` — HTTP request/response traces.

Filter by namespace: `RUST_LOG=pyramid=debug,warp=info`.

The last ~500 lines of the log are also accessible via the `pyramid_logs` CLI or the **Settings → Logs** tab in the UI — no need to open the file for quick checks.

---

## Health checks

**Settings → Wire Node Settings → Health Status.**

Each check shows status (green/yellow/red) and a message:

- **Database** — can we open and read `pyramid.db`?
- **Providers** — are configured LLM providers reachable?
- **Tunnel** — is Cloudflare Tunnel connected?
- **Storage** — is disk usage under the cap?
- **Wire** — can we reach the coordinator?
- **Credentials** — are required credentials present for configured providers?

Each failing check expands to show the specific failure and any suggested remediation.

Also available via `GET /pyramid/system/health` (with bearer auth).

---

## Provider health

**Settings → Providers** lists each configured provider with a Test button. Test sends a tiny prompt and reports:

- HTTP status.
- Round-trip latency.
- Token counts.
- Cost (if the provider reports it).
- Generation ID (for audit trail).
- Specific error if it failed (missing credential, rate limited, model not found, etc.).

**Understanding → Oversight → Provider Health banner** appears when any provider has active incidents. Click to expand; acknowledge clears for the session.

---

## Cost diagnostics

**Understanding → Oversight → Cost Rollup.**

Aggregate spend across all pyramids with a window selector (all time / 7d / 30d). Breakdowns by:

- **Source** — extraction, answering, stale-check, characterization.
- **Operation type** — which primitive (extract, synthesize, web, etc.).
- **Layer** — L0 / L1+ / apex.
- **Pyramid** — sorted by spend.
- **Provider** — OpenRouter vs Ollama vs others.

Per-pyramid cost panels (in Understanding → pyramid detail drawer → DADBEAR panel → Cost Analysis) give the same breakdowns scoped to one pyramid.

The recent-calls table (top 10) is useful for spotting anomalies. If one call cost $5 when the average is $0.05, it's worth drilling.

Also available programmatically via `GET /pyramid/:slug/cost`.

---

## Build diagnostics

**Understanding → Builds tab** shows every build across all pyramids. Per build:

- Running / complete / failed / cancelled.
- Elapsed time.
- Step counts (done / total).
- Assigned models.
- Cancel / retry / force-reset buttons.

Clicking a build opens its **Pyramid Theatre** view with:

- Pipeline timeline (which phase is active).
- Activity log (detailed event stream).
- Live pyramid surface (nodes appearing as they're created).

For a failed build, the activity log has the error and usually enough context to diagnose. For a stuck build, the activity log's last event tells you which step is hung.

**Force reset** is available on builds that have been running >30 minutes without progress. Use when a worker has deadlocked and normal cancel doesn't clean up. Force reset is destructive to in-progress step state but preserves completed work.

---

## DADBEAR diagnostics

**Understanding → Oversight** tab → per-pyramid cards.

Each card surfaces:

- Pending mutation counts by layer.
- Cost spent on DADBEAR this window.
- Last check time.
- Current phase.
- Breaker state.

Drilling into a pyramid's full DADBEAR panel (from its detail drawer) gives the **stale log** — every evaluation with reason, cost, outcome. Filter by layer, by stale/keep status. This is where you see what DADBEAR has been thinking about.

See [`25-dadbear-oversight.md`](25-dadbear-oversight.md).

---

## Tunnel diagnostics

**Settings → Wire Node Settings → Tunnel Status.**

- Current state (Connected / Connecting / Offline / Error).
- Public endpoint URL (if connected).
- Retry button.

Tunnels disconnect occasionally (transient network issues). Most self-heal. The retry button forces a reconnect.

If the tunnel refuses to come up, the log (`warp::*` and `tunnel::*` namespaces) has specifics. Common causes: Cloudflare Tunnel credentials expired, conflict on the local port, firewall blocking outbound traffic to Cloudflare.

---

## Event chronicle

**Pyramid Surface → Chronicle panel** and **Market → Chronicle tab**.

Live event streams:

- Pyramid chronicle: build events, DADBEAR ticks, supersessions, cache hits, cost events per pyramid.
- Compute market chronicle: offer events, job dispatches, settlements, broadcasts.

Filter by type, time range, or specific pyramid / model. Click an event to jump to the affected node or job.

Also available programmatically via `GET /pyramid/system/compute/events` and `GET /pyramid/:slug/chronicle`.

---

## Credit-related diagnostics

**Identity → Transaction History** — every credit transaction with reason, reference ID, balance after, timestamp.

If your balance is moving in ways you don't expect, the transaction history is the forensic record.

---

## When things go wrong: a triage flow

1. **Check Operations → Notifications.** Most significant issues surface here as notifications.
2. **Check Settings → Wire Node Settings → Health Status.** Failing checks tell you which subsystem is the problem.
3. **For build issues:** Understanding → Builds → click the problem build → activity log.
4. **For cost anomalies:** Understanding → Oversight → Cost Rollup → drill into the anomalous pyramid or phase.
5. **For DADBEAR weirdness:** Understanding → Oversight → pyramid card → View activity or Configure.
6. **For provider issues:** Settings → Providers → Test button, then check the log's provider namespace.
7. **For tunnel issues:** Settings → Wire Node Settings → Tunnel Status → Retry, then check log.

If after all of that you still don't know what's wrong, capture the last section of the log + a screenshot of the relevant diagnostic panels and ask in the alpha channel.

---

## Debug builds and verbose logging

A debug build with `cargo tauri dev` runs with debug-level logging by default and logs to stdout (terminal-attached) in addition to the log file. Useful for reproducing specific issues.

For a production build, `RUST_LOG=debug` captures the same verbosity. Traces can be noisy — filter to specific namespaces unless you're casting a wide net.

---

## Where to go next

- [`70-common-issues.md`](A0-common-issues.md) — known patterns.
- [`A1-build-stuck-or-failed.md`](A1-build-stuck-or-failed.md) — build-specific.
- [`A2-provider-and-network-errors.md`](A2-provider-and-network-errors.md) — provider and network.
- [`A3-staleness-and-breakers.md`](A3-staleness-and-breakers.md) — DADBEAR.
- [`docs/PUNCHLIST.md`](../PUNCHLIST.md) — authoritative known-issue list.
