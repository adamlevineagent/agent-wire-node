# Compute market — requester side

This doc covers using the **compute market** as a requester — dispatching inference to the market instead of calling OpenRouter (or another cloud provider) directly.

**Status: partially shipped (Phase 3 in progress).** The requester path is landing piece by piece. One-shot market calls work via the API and CLI; the full "route my build's inference through the market" toggle is still landing.

---

## What works today

- **One-shot market calls.** You can dispatch a single inference to the market via the `compute_market-call` HTTP route or via CLI/IPC. You specify model, prompt, budget; the market returns a result.
- **Market surface visibility.** In Market → Compute → Advanced drawer, you can see live rate cards for available models, peer offerings, and your recent market activity.
- **Credit settlement.** When you buy a market inference, credits debit from your balance with the rotator-arm split applied.

---

## What's in progress

- **Full build routing.** Transparently routing every LLM call inside a pyramid build through the market (instead of hitting the provider registry's configured cloud provider). The wiring is partial; the queue infrastructure is there but the routing switch is being completed.
- **Dispatch UI.** A "dispatch this build through the market" button in the Pyramid detail drawer that lets you choose market-vs-direct per build.
- **Policy-driven routing.** Per-tier or per-step routing — "extraction goes to market, synthesis stays on OpenRouter because I want the heavier model" — is planned.
- **Cost tracking across providers.** The existing cost rollup shows per-provider spend; the market is tracked as its own provider. Unified views across market + direct are landing.

---

## Why use the market instead of direct

- **Lower cost.** Market prices tend to be below cloud provider retail margins because there's no datacenter middleman — just operators pricing to their own cost.
- **Use your own credits.** If you're also serving as provider, your earnings offset your requester spend. Net cost for a hybrid node can approach zero over time.
- **Decentralized resilience.** If OpenRouter is down, the market is still running. If Anthropic's API is having issues, market providers serving open-weight models keep working.
- **Support the network.** Using market inference pushes credits through the network, strengthening the ecosystem that benefits you as provider too.

Why you might not:

- **Latency sensitivity.** Market adds a routing step. For latency-critical interactive work, direct cloud APIs can be faster.
- **Specific model requirements.** The market has whatever the provider ecosystem runs. If you need Claude Opus 4.5 specifically and no provider serves it, you go direct.
- **Data sensitivity.** The market provider sees your prompt. Until relays ship, attribution is visible too. For sensitive work, direct cloud APIs can be more appropriate (OpenRouter also sees your prompt, but it's a single trusted commercial entity rather than a distributed set of operators).

---

## One-shot market call today

Via the `pyramid-cli` (the most reliable path while the full UI lands):

```bash
# Via compute_market-call route
curl -s -H "Authorization: Bearer $(cat ~/Library/Application\ Support/wire-node/pyramid_config.json | jq -r .auth_token)" \
     -H "Content-Type: application/json" \
     -X POST "http://localhost:8765/pyramid/compute/market-call" \
     -d '{
       "model": "gemma3:27b",
       "prompt": "Summarize this code chunk in 3 sentences: ...",
       "max_tokens": 500,
       "budget_credits": 10
     }'
```

The request:

1. Dispatches to the coordinator with your budget and model requirement.
2. Coordinator selects a matching offer.
3. Job push-delivered to the winning provider.
4. Provider runs the inference.
5. Result returns.
6. Settlement fires, credits debit from your balance with rotator-arm split.

You see the transaction in **Identity → Transaction History** immediately.

---

## Checking the market before committing

Before dispatching, preview the market for the model you need:

- **Market surface** — `GET /pyramid/compute/market/surface?model_id=gemma3:27b` returns current offers, median price, and peer availability.
- **In the UI** — Market → Compute → Advanced drawer → Market surface — same data rendered.

Useful for sanity-checking price and capacity before a large spend.

---

## Budget and filters

When you dispatch a market call, you can specify:

- **`budget_credits`** — the max you're willing to spend on this single inference. Dispatch fails if no qualifying offer is within budget.
- **`min_reputation`** — skip providers below a reputation threshold. Default is permissive; raise for quality-sensitive work.
- **`max_latency_ms`** — required latency SLA. Providers whose offers commit to this or better are preferred.
- **`exclude_handles`** — skip specific provider handles. Useful after a bad experience.
- **`prefer_handles`** — bump specific providers in matching.

Reasonable defaults exist for all of these — you only set what matters for your use case.

---

## Failure handling

A dispatched market call can fail in several ways:

- **No qualifying offers** (budget too low, model not available) — returns `NoMatch` with context.
- **Provider timed out or dropped** — the coordinator detects and refunds you automatically; you can re-dispatch or fall back to direct.
- **Provider returned bad output** — parse or schema failures return a `BadOutput` status. You can dispute by flagging; the provider takes a reputation hit. You're refunded if the dispute is sustained.
- **Credential or permission issues** — your balance is insufficient, your node is not in a state that allows market requests, etc. Surfaced as clear errors.

The retry strategy in chains (and in the dispatch path generally) tries the market first if configured, falls back to direct if market fails, and records both attempts in cost logs.

---

## Patterns that work well today

**"Use the market for cheap, parallelizable work; direct for heavy, latency-critical work."** Route tier `extractor` (which is cheap and high-volume) through the market; keep `synth_heavy` on direct cloud providers for speed.

**"Build up a credit buffer before dispatching big jobs."** If you're running as provider, accumulate credits over a few days. Then dispatch a big build — much or all of it fund by earnings.

**"Use market for experimentation."** Trying a new chain variant or a new prompt? Run it through the market first. If it works, you can promote to direct for production use.

**"Route sensitive work direct; route routine work through market."** Two-path strategy. Tier routing with per-step overrides lets you do this.

---

## When full requester support ships

The UX will shift from one-shot calls to "click a toggle and every LLM call in this build goes through the market." Tier routing will include a "via market" option per tier. Cost estimation will incorporate current market prices before you confirm a build.

For now, treat market requester as **one tool in the inference toolbox** — usable for specific calls, with the full transparent-routing integration still landing.

---

## Where to go next

- [`70-compute-market-overview.md`](70-compute-market-overview.md) — the full market context.
- [`71-compute-market-provider.md`](71-compute-market-provider.md) — the other side.
- [`74-economics-credits.md`](74-economics-credits.md) — credits and settlement.
- [`50-model-routing.md`](50-model-routing.md) — where "market" eventually becomes a provider type in tier routing.
