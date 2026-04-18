# Provider and network errors

When something outside your node is the problem — the LLM provider, the Wire coordinator, the tunnel, the compute market — the signal shows up across several places. This doc covers diagnostics and recovery.

---

## Provider errors

### 401 Unauthorized

The credential you're sending is wrong or revoked.

Fix: regenerate the key at the provider's dashboard, update `.credentials`, test via Settings → Providers → Test.

### 429 Rate limited

You're hitting the provider's rate limits.

Short-term: Agent Wire Node's retry backoff handles transient 429s. You'll see "retry 1", "retry 2" in the activity log. Usually self-heals.

Sustained: bump to a higher tier on the provider's plan, or spread load across multiple providers via tier routing with fallback chains.

### 402 Payment required / insufficient credits

Your provider account is out of funds.

Fix: top up at the provider's dashboard. If you configured OpenRouter's management key (`OPENROUTER_MANAGEMENT_KEY`), Agent Wire Node proactively warns before builds fail.

### 5xx errors

Provider-side outage. No user fix — wait for the provider to recover. If you have fallback chains configured, Agent Wire Node routes around to the next provider automatically (when fallback chains are wired; partial shipping state — see [`50-model-routing.md`](50-model-routing.md)).

### Timeouts

Call took too long. Possible causes:

- Provider is slow (real congestion).
- Your network is flaky.
- The model is loaded cold (first call after idle can be slow, especially on Ollama).

Retry logic handles most timeouts. Sustained timeouts = bigger problem; check provider status.

---

## Tunnel errors

### Tunnel shows "Offline"

Settings → Agent Wire Node Settings → Tunnel Status → **Retry**. Most common fix.

### Tunnel keeps reconnecting

Local network issue. Check:

- Your internet is up.
- Your firewall isn't blocking outbound to Cloudflare.
- A VPN isn't interfering.
- A secondary network interface isn't claiming traffic unexpectedly.

If the tunnel can't establish at all, check the log's `tunnel::*` namespace for specifics.

### Tunnel connected but other nodes can't reach me

Usually a firewall issue on the other side, not yours. Have the other node verify their own tunnel.

---

## Wire coordination errors

### "Cannot reach Wire coordinator"

The coordinator is down or your network can't reach it.

Temporary: wait and retry. Coordinator has its own uptime.

Persistent: check the alpha channel for outage reports. Your local node continues to work; only cross-Wire operations (publish, pull, compute market dispatch) are blocked.

### Publish fails with "handle-path conflict"

The handle-path you're trying to use is taken. Pick a different slug or bump the version number.

### Pull fails with "contribution not found"

- Typo in the handle-path.
- The author retracted the contribution.
- The contribution is private and you don't have access.

Double-check the handle-path spelling. If correct and the contribution exists, the author may have retracted or made it private.

### Broadcast orphans

**Understanding → Oversight → Orphan Broadcasts panel** surfaces broadcasts that arrived without a matching in-flight request. Usually benign (a retry after timeout that eventually succeeded). A large accumulation over a short period suggests a bug or a misconfigured webhook secret — worth investigating.

Click the notification to see specifics. Acknowledge dismisses for the session.

---

## Compute market errors

### My offers aren't getting any jobs

Check:

- **Offer active?** Market → Compute → Advanced → Offer Manager. Stale offers (for models you no longer have installed) auto-disable.
- **Capacity set too low?** If capacity is 0, nothing routes.
- **Policy mode?** Coordinator mode doesn't accept jobs. Switch to Hybrid or Worker.
- **Tunnel online?** The coordinator needs to reach your node.
- **Reputation tanked?** High flag rate or poor uptime de-prioritizes you.

### Market dispatch returns NoMatch

No qualifying offers. Either:

- Your budget is too low for the model.
- No providers are currently serving that model.
- Your filters (reputation threshold, latency SLA) are too restrictive.

Relax filters or increase budget. Check the market surface (`GET /pyramid/compute/market/surface?model_id=...`) to see what's available.

### Market job came back with bad output

Flag the provider. From the transaction entry, **Flag** → enter reason. A sustained flag provides reputation feedback to the provider. If the dispute is sustained, you may get a partial or full refund.

### Job timeout during market call

Provider went dark mid-job. Coordinator detects and refunds. Retry or fall back to direct cloud.

---

## Fleet / peer errors

### Fleet peer shows as offline

The peer's tunnel dropped, their node stopped, or they're genuinely offline. Not your problem to fix. They'll reconnect when their tunnel recovers.

### Fleet dispatch fails

- **No fleet peers matching the job's needs.** The peer you expected to route to isn't serving the model.
- **Fleet auth mismatch.** Your fleet peering agreement expired or was revoked. Re-establish via the fleet coordination panel.
- **Network issue between you and the peer.** Cloudflare tunnel's state on either side.

---

## DADBEAR-specific network errors

DADBEAR ticks make LLM calls like regular builds. The same provider/network diagnostics apply. One twist:

### Leak detection alerts

The evidence-triage system expects each synchronous cost reconciliation to have a matching async broadcast confirming it. Missing broadcasts trigger leak detection alerts (potential billing integrity issue).

If you see leak detection alerts in Oversight:

- **Check broadcast configuration.** OpenRouter's broadcast webhook needs to be configured with the shared secret Agent Wire Node expects.
- **Check tunnel.** Broadcasts come in through your tunnel; if it's flaky, broadcasts arrive late or not at all.
- **Check the grace window.** Alerts fire after a grace period. If you're just seeing late-but-arriving broadcasts, extend the grace window in the evidence triage policy.

---

## General recovery patterns

**When a whole area is broken:** pause anything that depends on it. Market disabled, DADBEAR paused, tunnel retry. Let things stabilize. Re-enable once diagnostics are clean.

**When one provider is problematic:** route away from it via tier routing. Fallback chains (when fully shipped) automate this; manual routing works today.

**When your network is the problem:** check basic internet connectivity before debugging Agent Wire Node. `ping 8.8.8.8`, `curl https://openrouter.ai/api/v1/auth/key -H "Authorization: Bearer $KEY"`.

---

## Where to go next

- [`A0-common-issues.md`](A0-common-issues.md) — grab bag.
- [`A1-build-stuck-or-failed.md`](A1-build-stuck-or-failed.md) — build-specific.
- [`A3-staleness-and-breakers.md`](A3-staleness-and-breakers.md) — DADBEAR.
- [`91-logs-and-diagnostics.md`](91-logs-and-diagnostics.md) — diagnostic surfaces.
