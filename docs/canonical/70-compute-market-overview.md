# The compute market

The **compute market** is the Wire-wide order book for inference. Operators with idle GPU capacity publish offers to serve specific models at specific prices. Operators with work to do dispatch inference to the market and let the cheapest qualified offer win. Payment flows in credits; outcomes are logged and attributed.

This makes the idle capacity on thousands of operator machines available as a pooled inference resource — and makes the cost of running Wire Node builds lower than paying OpenRouter directly for anyone willing to use the market.

---

## Current shipping state

- **Phase 2 (provider side)** — shipped. You can opt your node in as a compute market **provider**, publish rate cards, serve inference jobs from other operators, earn credits.
- **Phase 3 (requester side)** — in progress. Dispatching your own builds' inference to the market (instead of hitting OpenRouter directly) is landing piece by piece. Some paths work; others are still being wired through.
- **Cross-market integration** — partial. Using market inference for your own pyramid builds works via the `compute_market.rs` and `compute_requester.rs` paths but is best used for one-shot invocations today, not for driving full builds end-to-end.
- **UI for the requester flow** — partial. Market surface visibility (prices, available models) works; full "route my build through the market" toggle is landing.

See **Market** mode in the sidebar for the live market view, **Operations → Queue** for inflight jobs, **Settings → Compute Participation Policy** for the coarse dials.

---

## The two roles

You can participate as:

- **Provider** — you serve inference for others. Opt in, publish offers per model at a price/rate you set, serve jobs that route to you, earn credits. See [`71-compute-market-provider.md`](71-compute-market-provider.md).
- **Requester** — you buy inference from the market instead of calling cloud providers directly. Set a budget, dispatch, get results. See [`72-compute-market-requester.md`](72-compute-market-requester.md).

Most nodes will do both. A **Hybrid** compute participation policy (see [`73-participation-policy.md`](73-participation-policy.md)) is the common setup — serve when idle, dispatch when busy.

---

## How a job flows through the market

Provider side (shipped):

1. You publish an offer: *"I'll serve `gemma3:27b` at 3 credits per 1K output tokens, up to 2 concurrent jobs."*
2. Wire Node registers the offer with the coordinator's order book.
3. A requester on the Wire dispatches a job matching your offer.
4. Coordinator selects your offer (cheapest qualified, or based on requester's policy).
5. Coordinator push-delivers the job to your Wire Node.
6. Your node's compute queue for `gemma3:27b` picks up the job (same FIFO queue as your local builds use).
7. Your local Ollama runs the inference.
8. Result posts back to the requester (via the Wire).
9. Credits settle — rotator arm splits your earnings between you, the platform, the treasury, and reserved roles.
10. **Broadcast** confirms the settlement — this is the integrity check that catches orphan billings and credential leakage.

Requester side (partial):

1. You call inference via `compute_requester::MarketInferenceRequest` with a model and prompt and budget.
2. Coordinator broadcasts your job to registered providers.
3. Providers offer; coordinator selects the best match.
4. Job is push-delivered to the winning provider.
5. Provider runs the inference.
6. Result returns to you.
7. Credits debit from your balance, rotator arm splits.

Today the requester side works for one-shot market calls (e.g. you explicitly dispatch a single inference to the market via the CLI or API). Wiring the market into the default chain dispatch path — so every LLM call in a build could flow through it transparently — is near-term.

---

## Why this exists

Three reasons:

**Idle capacity is waste.** There are orders of magnitude more idle GPUs than actively-rented ones. Most operator hardware sits cold most of the time. The market lets that capacity become useful.

**Lower inference cost for requesters.** Market prices are typically below OpenRouter's retail margin because there's no datacenter middleman. Operators set their own rates based on their actual cost (electricity + hardware amortization), not a commercial margin on top.

**Decentralized resilience.** If one provider disappears, other providers still serve the same model. Cloud outages that today affect everyone get localized to "providers in that region" instead.

---

## The key economic primitives

- **Offers.** A provider's commitment to serve a specific model at a specific rate. Includes capacity (concurrent jobs), price per unit, any preferences (which requesters to accept, hours of operation, quality tier).
- **Orders.** A requester's demand. Includes model, budget, latency requirements, quality filters.
- **Matching.** Coordinator pairs offers with orders. Can be auction-style (cheapest wins), policy-driven (requester's preferences + provider's preferences), or reputation-weighted.
- **Settlements.** Credits flow from requester to provider on job completion. Rotator arm splits.
- **Disputes and challenges.** A requester who believes an answer was bad can file a flag; reputation takes a hit proportional to confirmed quality issues.

See [`74-economics-credits.md`](74-economics-credits.md) for the full economics.

---

## Privacy considerations

Today, market jobs are **attributed** — the provider sees the requester's handle and can observe the prompt payload. This is partly inherent (the provider runs the LLM on your prompt; it sees the prompt) and partly a property of the current build before relays ship.

Once relays are shipped (see [`63-relays-and-privacy.md`](63-relays-and-privacy.md)), market flows can be relay-routed: the provider runs your inference without knowing it was you, and the settlement fires back through the same relay path. Payload is necessarily visible to the provider who runs the computation; unlinkability of requester identity is what relays provide.

For high-sensitivity work, use your own cloud API directly. The market is built around the common case where "my inference is cheap and not deeply sensitive" is fine.

---

## Quality assurance

Market participation includes a reputation layer:

- **Speed** — providers that consistently answer within their stated latency range accrue a speed signal.
- **Flag rate** — requesters can flag suspicious or clearly wrong outputs. High flag rate drops the provider's reputation.
- **Challenge assessment** — disputed outputs get reviewed by other operators; review outcomes feed back into both provider and reviewer reputation.

The coordinator weights reputation into its matching algorithm. Low-reputation providers get fewer jobs until they rebuild trust.

---

## What the market looks like in the UI

**Sidebar → Market** mode:

- **Queue tab** — live view of which models are executing or queued, by source (local build, fleet peer, market).
- **Compute tab** — the network status hero card (your node's role), plus an Advanced drawer with offer management, rate card view, policy matrix, raw event ledger.
- **Hosting tab** — fleet peers hosting info.
- **Chronicle tab** — activity feed of compute market events.

**Settings → Compute Participation Policy** — coarse mode picker (Coordinator / Hybrid / Worker) plus an advanced breakdown of the 8 individual toggles the preset maps to.

**Network status sidebar item** — your credit balance and tunnel status, always visible.

---

## Where to go next

- [`71-compute-market-provider.md`](71-compute-market-provider.md) — opting in as provider.
- [`72-compute-market-requester.md`](72-compute-market-requester.md) — dispatching to the market.
- [`73-participation-policy.md`](73-participation-policy.md) — Coordinator/Hybrid/Worker modes.
- [`74-economics-credits.md`](74-economics-credits.md) — credits, rotator arm, settlements.
- [`29-fleet.md`](29-fleet.md) → Queue tab — inflight view.
