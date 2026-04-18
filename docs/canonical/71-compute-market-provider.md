# Compute market — provider side

This doc covers opting your node in as a **compute market provider** — publishing offers to serve inference for other Agent Wire Node operators and earning credits per job completed.

**Status: shipped (Phase 2).** The provider side works. You can opt in, publish offers, serve jobs, earn credits today.

---

## Prerequisites

- Agent Wire Node is running and registered (tunnel online).
- You have at least one model ready to serve — typically via local Ollama (see [`51-local-mode-ollama.md`](51-local-mode-ollama.md)).
- Your compute participation policy allows serving (not Coordinator-only mode).

---

## Opting in

### Minimal path

1. Open **Market** mode.
2. Compute tab → the network status hero card. If it says "Marketplace disabled," click **Enable**.
3. Agent Wire Node registers your offers with the coordinator (default offers are seeded from your installed Ollama models).
4. The hero card now says "Idle, helping the network" (when nothing's running) or "Active, serving jobs" (when jobs are in flight).

That's it. Default offers are conservative (low concurrency, moderate pricing). Tune from the Advanced drawer if you want more control.

### Advanced path

From the Compute tab → **Advanced drawer** → **Offer Manager**:

- **Per-model offer** — which models you're serving.
- **Rate** — credits per 1K input tokens, per 1K output tokens, optional per-request fee.
- **Capacity** — max concurrent jobs for this model.
- **Schedule** — hours during which this offer is active (optional; useful if you don't want to serve during your work hours).
- **Quality tier** — "best-effort" or "priority"; priority costs more but gets matched more aggressively.
- **Acceptance rules** — which requesters you'll accept from (all, trusted-only, specific handles, non-banned).

Each offer is versioned via the contribution store — your rate card history is visible and auditable.

---

## Pricing your offers

Two approaches:

**Market-driven.** Look at the rate card for the model you're serving (Market tab → Advanced → Market surface). See what other providers are charging. Set your rate at or slightly below median for volume, or above for premium positioning.

**Cost-driven.** Compute your actual cost per 1K tokens based on electricity + hardware amortization. Price above that. The steward vision (when shipped) will automate this; today it's manual.

Common starting points (rough, check current market):

- Cheap small models on commodity hardware: ~1-3 credits per 1K output tokens.
- Heavier open-source models on prosumer GPUs: ~3-8 credits per 1K output tokens.
- Large models on datacenter-class hardware: ~10+ credits per 1K output tokens.

Start conservative. You can always raise rates once you've got usage history. Dropping rates kills reputation momentum — don't over-price initially.

---

## What happens when a job arrives

1. Coordinator push-delivers the job to your node (HTTP POST with the request payload and a settlement envelope).
2. Your Agent Wire Node validates the job against your active offer and acceptance rules.
3. If valid, the job enters your compute queue for the specific model — same FIFO queue your local builds use. This is important: **market jobs don't jump ahead of your own work.**
4. When the queue worker pulls the job, it calls Ollama (or whichever provider the model is routed to).
5. The result is sent back through the Wire's response path.
6. The settlement fires: credits debit from requester, rotator-arm splits to you + platform + treasury + reserved.
7. A broadcast confirms the settlement. Your node's cost accounting picks up the broadcast; if no broadcast arrives within the grace window, a leak-detection alert fires (integrity check).

All of this is visible in real time:

- **Operations → Queue** shows your live queue depths per model.
- **Market → Chronicle** shows the event stream.
- **Network** item in sidebar shows your balance ticking up.

---

## Reputation signals

Your provider reputation is tracked per-model. Three signals feed it:

- **Speed** — your median time-to-completion on jobs of this model, relative to peers.
- **Flag rate** — how often requesters flag your outputs as bad. A high flag rate tanks matching priority.
- **Uptime** — how often your offer is active when the coordinator has a matching job to dispatch. Going offline during high-demand periods hurts.

Reputation compounds. A new provider gets lower-priority matching for the first few jobs; as outcomes accumulate, matching gets better. Providers whose reputation consistently climbs become trusted sources for high-value jobs.

---

## Protecting your local work

The biggest fear most operators have: "if I enable the market, will my own builds get starved?"

The design prevents this:

- **Shared FIFO queue.** Market jobs enter the same queue as local builds. Local builds are not down-prioritized.
- **Per-model queues.** A flood of market jobs for `gemma3:27b` doesn't block your local `qwen2.5-coder:32b` build — different queues.
- **Concurrency caps.** You set the max concurrent jobs per model in your offer. Set it low enough that there's always capacity for your own work.
- **Schedule gates.** Set "only serve during X-Y hours" to keep the market out of your working hours.
- **Policy presets.** Set **Worker** mode and the market fills your capacity; set **Hybrid** for balanced; set **Coordinator** to dispatch only and never serve. See [`73-participation-policy.md`](73-participation-policy.md).

If your own work is impacted, dial back your offers. Earnings fall; your experience is restored. The tradeoff is yours.

---

## Quality control on your side

You can:

- **Refuse jobs** from requesters below a reputation threshold.
- **Flag suspicious requests** — if a prompt looks like an abuse pattern, flag and refuse. The coordinator's abuse-detection layer aggregates flags.
- **Test your own outputs** — sample your completions periodically and compare against a known-good baseline. Drift triggers investigation.

Market participation isn't commit-and-forget. Your offer's reputation is your asset; protect it.

---

## Earnings and settlement

**Every settled job** flows credits to your balance. The breakdown is visible in **Identity → Transaction History**.

Settlement timing:

- Synchronous cost is recorded at job completion (the inference's cost is known immediately).
- The broadcast arrives within a grace window (typically seconds to a couple of minutes). The settlement is fully confirmed when the broadcast arrives.
- Orphan broadcasts (broadcast with no matching inflight request) trigger an integrity alert — uncommon, usually benign.
- Missing broadcasts (inflight with no confirming broadcast) trigger leak detection.

**Annual equivalent estimate** — the sidebar's Network item shows a rough annualization of your current rate of income. A node running modestly in Hybrid mode might see anywhere from single-digit to low-hundred-dollar equivalent per month depending on hardware and uptime; serious provider nodes with good hardware and high uptime can do significantly more.

---

## Temporarily disabling

- **Pause market** (soft disable): in the Compute tab, click **Disable**. Offers are withdrawn; in-flight jobs complete.
- **Specific offer pause**: Advanced → Offer Manager → disable one offer while keeping others.
- **Global kill** in case of emergency: Settings → Compute Participation Policy → set to Coordinator (no serving at all).

Re-enabling republishes your offers with the coordinator; you become matchable again.

---

## Common questions

**"Can I run both a local build and serve market jobs at the same time?"** Yes. That's the Hybrid mode (the default in most setups). Your local build queues alongside market jobs; concurrency caps ensure the market can't starve local work.

**"What if my model is deprecated on Ollama?"** Update your Ollama, pull the new model, update your offer. Stale offers (for models you no longer have) are auto-disabled by the registry.

**"Can I serve an OpenRouter model?"** No — the provider side is specifically for serving local compute. An OpenRouter-powered node is a requester, not a provider.

**"What happens if I lose network during a job?"** The in-flight job gets orphaned; the coordinator detects the timeout and refunds the requester. You don't get credits for incomplete work. Your reputation takes a small hit if this happens often.

**"Can I set per-requester pricing?"** Not in the shipped build. Reputation-based matching exists; per-requester price discrimination is a planned extension.

---

## Where to go next

- [`70-compute-market-overview.md`](70-compute-market-overview.md) — the full market context.
- [`72-compute-market-requester.md`](72-compute-market-requester.md) — the other side.
- [`73-participation-policy.md`](73-participation-policy.md) — Coordinator / Hybrid / Worker.
- [`74-economics-credits.md`](74-economics-credits.md) — how credits flow.
- [`29-fleet.md`](29-fleet.md) — queue view.
