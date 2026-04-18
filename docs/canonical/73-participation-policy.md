# Compute participation policy

Your **compute participation policy** is the coarse dial that controls how your node engages with the compute market. The policy maps to a bundle of eight individual toggles that an advanced user can tune directly; most operators pick a preset and leave it alone.

Three presets: **Coordinator**, **Hybrid**, **Worker**.

---

## Where to set it

**Settings → Wire Node Settings → Compute Participation Policy.**

Three preset buttons, each with a one-line description. Pick one and the eight underlying toggles snap to the matching values. Click **Advanced** to expose the individual toggles if you want finer control.

---

## Coordinator mode

Your node **dispatches** work to the market but does not **serve** work for others.

**Best for:**

- Nodes without spare compute capacity.
- Machines where you want zero market interruption (your work laptop during work hours).
- Operators who want to use market inference without contributing back.

**What this means practically:**

- No offers published to the market.
- No jobs delivered to your node.
- You can dispatch inference to the market (subject to requester-side routing landing — see [`72-compute-market-requester.md`](72-compute-market-requester.md)).
- Your node earns no market credits.

Coordinator mode is the **shipped default** for nodes that haven't explicitly opted in to anything else. Fresh installs land here.

---

## Hybrid mode

Your node **both dispatches and serves** — the common case for engaged operators.

**Best for:**

- Most operators. Most of the time.
- Machines that have spare capacity sometimes but not always.
- Operators who want credits to offset their own market spending.

**What this means practically:**

- Offers are published for models you have locally.
- Jobs route to you subject to your capacity and acceptance rules.
- Your own builds still take priority in the shared queue.
- You earn credits when you serve and spend credits when you dispatch.
- Net balance depends on your hardware, uptime, and usage pattern.

Hybrid is the recommended mode for anyone with a capable machine that they aren't pinning at 100% on their own work.

---

## Worker mode

Your node **serves** but does not dispatch — or only dispatches in specific narrow cases.

**Best for:**

- Dedicated compute nodes (a spare machine, a server you're running specifically for market participation).
- Operators who want maximum earnings and don't intend to run pyramid builds themselves on this node.

**What this means practically:**

- Offers published aggressively for everything you can serve.
- Capacity set high.
- No market dispatch (except for specific dependency cases).
- Maximum exposure to market reputation risk and reward.

Worker mode is for the "I've got a GPU box that's otherwise idle; let it earn" scenario. Not the default.

---

## The eight underlying toggles (Advanced)

When you click **Advanced**, the preset expands into 8 toggles you can set independently:

1. **Publish offers** — do you publish rate cards? (Off in Coordinator, on in Hybrid and Worker.)
2. **Accept jobs** — do you serve delivered jobs? (Off in Coordinator, on in Hybrid and Worker.)
3. **Dispatch to market** — do you route your own inference through the market? (Off in Worker, on in Coordinator and Hybrid.)
4. **Schedule-gated serving** — do you respect hours-of-operation filters on your offers? (Optional in all modes.)
5. **Auto-withdraw on low balance** — stop serving if your credit buffer drops below a floor. (Off by default.)
6. **Reputation floor** — minimum requester reputation you'll accept jobs from. (Permissive by default.)
7. **Fleet preference** — prefer jobs from your own fleet members (other nodes on your account). (On if you have multiple nodes.)
8. **Quality tier** — commit to a priority tier (faster matching, higher rates, higher reputation stakes). (Best-effort by default.)

Editing these directly lets you construct custom policies — "Hybrid but never serve during my working hours", "Worker but only from requesters above reputation X", "Coordinator that also serves one specific model because my friend asked."

The advanced dials are saved as a config contribution (see [`46-config-contributions.md`](46-config-contributions.md)). You can publish a well-tuned policy as a shareable preset that other operators with similar profiles adopt.

---

## Changing modes

Switching between presets takes effect immediately:

- **To Coordinator** — offers are withdrawn (pending jobs complete, no new ones accepted).
- **To Hybrid or Worker** — offers are published; the market starts routing jobs to you.

The coordinator's order book reflects the new state within seconds. No restart required.

---

## Interactions with other settings

- **Storage cap** doesn't affect market participation directly — it's about document caching, not compute.
- **Local Mode (Ollama)** is strongly recommended if you want to serve — you serve local models, typically via Ollama.
- **Credentials** matter for the requester side — dispatching to the market doesn't require OpenRouter credentials, but falling back to direct cloud on market failure does.
- **Tunnel status** must be online for the market — the coordinator needs to reach your node to push-deliver jobs.

---

## Policy history

All policy changes are versioned. The current policy is a contribution in your store; editing creates a new superseding version. You can see the history in Settings → Wire Node Settings → Config History.

"Go back to yesterday's policy" is one click — select a prior version and apply. This becomes useful once the steward experimentation vision ships (see [`05-steward-experimentation-vision.md`](05-steward-experimentation-vision.md)) — autonomous policy tuning needs clean rollback.

---

## Where to go next

- [`70-compute-market-overview.md`](70-compute-market-overview.md) — the market these modes govern participation in.
- [`71-compute-market-provider.md`](71-compute-market-provider.md) — what "serving" does in detail.
- [`72-compute-market-requester.md`](72-compute-market-requester.md) — what "dispatching" does.
- [`34-settings.md`](34-settings.md) — the Settings UI.
