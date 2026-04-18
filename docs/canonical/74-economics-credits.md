# Economics (credits and the rotator arm)

Credits are Wire's internal accounting unit. They accrue when you do work that benefits others (serving compute, publishing consumed contributions, getting tipped); they spend when you benefit from others' work (dispatching inference to the market, pulling priced contributions, registering handles). The **rotator arm** is the split function that distributes credits on paid flows.

This doc covers how credits move, what the rotator arm's standard splits are, how they can be tuned per contribution, and what the net flow typically looks like for different operator roles.

---

## Credits, in brief

Roughly: **1 credit ≈ 1 cent USD** (the exchange rate is not fixed and moves with the Wire's internal economy, but this is the useful mental model).

Your balance is visible in the sidebar's Network item, alongside an **annual equivalent** — the dollar value of your current rate-of-inflow projected across a year. A useful sanity check.

You hold balances in your Wire account (not per-node). A multi-node operator has one pool.

---

## What produces credits (inflow)

- **Publishing consumed contributions.** Every time someone pulls or uses your chain, skill, config, or other contribution, a small credit flows to you. Priced contributions flow more on each pull.
- **Compute market serving.** Per job served. Highest-volume inflow source for provider nodes.
- **Mesh hosting.** Small trickle for hosting documents from public corpora (opt-in, see [`27-knowledge-corpora.md`](27-knowledge-corpora.md)).
- **Absorbing questions on published pyramids.** If your pyramid is published and operators query it with paid synthesis (emergent tier), credits flow to you.
- **Relay traffic** *(planned)*. Credits flow to relay operators for carrying traffic.
- **Tips.** Operators can directly tip you; your handle has a tipping address.
- **Bounties and challenge assessment** *(planned)*. Wire occasionally has bounties for reviewing contributions, flagging bad content, or doing challenge assessment for disputes.

---

## What spends credits (outflow)

- **Buying inference on the compute market** (alternative to paying your cloud provider directly).
- **Pulling priced contributions** (emergent tier).
- **Posting questions to paid pyramids** (emergent tier).
- **Registering new handles.** Handle cost varies by length/scarcity/namespace.
- **Priority placement** *(optional, uncommon)* in Search or on the market.

Your node continues to function with zero or negative balance. Negative means you can't spend until you earn back; your local pyramids, builds using direct cloud APIs, and other non-market operations keep working.

---

## The rotator arm — default split

When credits flow on a paid Wire transaction (pull, market job, paid query), they're split:

- **76%** to the creator / provider (the principal value contributor).
- **2%** to the platform (Wire coordination service).
- **2%** to the treasury (reserved for ecosystem bounties, grants, incentives).
- The remainder is reserved for roles like **relays** and (planned) **validators** that support the network beyond direct contribution authorship.

The name "rotator arm" comes from the split's visual metaphor — a spinning arm that distributes the pie in fixed percentages regardless of transaction size.

The split is **per-transaction, not per-source**. Every paid pull rotates. Every market job rotates. Every paid query rotates.

---

## Per-contribution overrides

Authors can adjust the split when publishing a contribution:

- **Creator-favored** — reduce platform/treasury shares (e.g. 90/0/10) for public-good contributions where the author wants the contribution to be essentially free to circulate.
- **Treasury-favored** — increase treasury share (e.g. 60/2/38) for contributions designed to fund network development.
- **Role-emphasized** — increase relay share for privacy-sensitive contributions, rewarding relay operators more.

Defaults are sensible; overrides are for authors with specific intent.

---

## Flow shapes by operator role

### Pure consumer

You pull contributions, query paid pyramids, dispatch to the market. You don't serve or publish. Your credit flow is purely outbound.

- Inflow: near-zero.
- Outflow: moderate (cost of your inference and pulled contributions).
- Net: negative balance unless you buy credits.

Common for users who just want to use Wire Node for their own pyramids and don't care about market participation.

### Hybrid operator

You serve and dispatch both. You've published some contributions; others consume them.

- Inflow: market serving earnings + trickle from published contributions.
- Outflow: market dispatches + pull costs.
- Net: typically neutral to slightly positive for engaged operators. Hybrid is the common case and the design target.

### Worker / dedicated provider

You serve heavily, rarely dispatch. Low activity as a contribution author.

- Inflow: high (market serving on good hardware).
- Outflow: low.
- Net: sustainably positive. This is the "spare server earning credits" scenario.

### Content-heavy publisher

You publish chains, skills, pyramids at high volume. The market is secondary for you.

- Inflow: rotator-arm royalties from widely-consumed contributions.
- Outflow: usually low — you're the one producing the things others pull.
- Net: depends heavily on how adopted your contributions are. A high-reputation author publishing quality work can see meaningful income; a low-adoption author sees trickle.

### Steward-as-a-service operator (planned)

When stewards ship, operating stewards on behalf of other principals becomes a role. You run a "privacy-Steward-for-medical-data" service; many principals subscribe; each subscription generates ongoing credits.

---

## Cost estimation and transparency

Every planned paid action shows you the cost before you commit:

- **Pull preview** — shows purchase price, rotator-arm split.
- **Market dispatch preview** — shows budget cap, expected cost range.
- **Build preview** — shows estimated total LLM cost for the build (sum of per-step estimates based on current tier routing and average chunk sizes).

Cost accounting logs every transaction with:

- Timestamp.
- Counterparty (whose contribution, which provider).
- Amount.
- Split (how the rotator arm distributed).
- Balance after.

**Understanding → Oversight** aggregates costs across all pyramids. The per-pyramid detail drawer breaks down costs by phase (extraction, answering, synthesis) and by source (DADBEAR checks, build passes, absorption queries).

---

## Dispute and reversal

If a pull or market job turns out to have been fraudulent or seriously mis-delivered, disputes can flow:

- **Flag** the bad transaction.
- **Challenge assessment** — other operators review.
- **If sustained,** credits can be clawed back from the counterparty and refunded to you.

Disputes are uncommon and take time. For day-to-day transactions, assume settlement is final.

---

## Deflationary vs inflationary behavior

Credits follow a deflationary design — credits are issued against productive work, destroyed against consumption, and the ratio keeps overall purchasing power roughly stable. Periods of high demand (many operators running builds, market activity spiking) can see credit prices rise; periods of high supply (many operators serving, few consuming) see them settle.

The design is opinionated: **credits represent real work that has real value.** The platform does not aim to create speculative token markets; it aims to keep inference + knowledge work compensated in a medium that the participants themselves generate.

---

## Moving balances between machines

Your Wire account holds credits. Your individual nodes don't hold separate balances — they draw against your account. Switching to a new machine or running multiple machines on one account doesn't split your credits.

Withdrawal to external currencies is **planned** but not in the current scope. The credit economy today is closed — earn-and-spend within the Wire.

---

## Where to go next

- [`33-identity-credits-handles.md`](33-identity-credits-handles.md) — where credits and handles live in the UI.
- [`70-compute-market-overview.md`](70-compute-market-overview.md) — the main flow.
- [`61-publishing.md`](61-publishing.md) — publishing as an inflow source.
- [`62-pulling-and-discovery.md`](62-pulling-and-discovery.md) — pulls as outflow.
- [`05-steward-experimentation-vision.md`](05-steward-experimentation-vision.md) — where steward-as-a-service economics land.
