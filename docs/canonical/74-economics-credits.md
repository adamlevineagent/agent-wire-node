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

### The mechanism

Every paid contribution (and the compute market) has a **rotator arm**: a wheel of **80 slots**, each worth one unit (1.25% of a cycle). Each slot is configured to pay a specific recipient role — provider, creator, platform, treasury, a cited ancestor contribution, a reviewer, a relay, whatever the contribution's author directed at publish time.

When a credit comes in, the arm pays that credit to the role assigned to the current slot, then advances one slot forward. The next credit goes to the next slot's role. After 80 credits, the arm has been all the way around once and every recipient has received exactly their configured slot count.

This gives a **directable** split without any fractional credit accounting — every credit is atomic and goes to exactly one recipient. Over long-run volume, the distribution converges exactly on the directive. Short-run volume may not exercise all 80 slots, but the split is honored in expectation.

### Two flow classes — contributions vs. services

**Contributions.** Every contribution — chains, skills, templates, question sets, configs, pyramid nodes, annotations, supersessions, composed prose, everything with a `derived_from` lineage — is governed by **UFF**. When any paid or citation-triggered payout flows against a contribution, its UFF-compliant slot directive is what the rotator arm uses.

**Services.** The compute market is the service exception. Buying inference is a service purchase, not a contribution — nothing is published, nothing enters the supersession graph, no `derived_from` chain is involved. UFF does not apply. The directive is compact:

- **76 slots** → the provider serving the job.
- **2 slots** → the platform.
- **2 slots** → the treasury.

Every 80 credits of market revenue pays the provider 76, platform 2, treasury 2.

### UFF (the universal sourcing-share rule)

UFF covers every contribution. Under UFF, **28 of the 80 slots must be allocated to sourcing** — the prior contributions the current work derives from. This is a floor, not a suggestion: every contribution's directive commits 28 slots to the authors whose work it builds on. This is true for every contribution type — a chain YAML, a prompt markdown, a config, a pyramid node, a supersession of a prior config, a composed analysis, anything.

A typical UFF-compliant directive:

- **48 slots** → the creator (you).
- **28 slots** → sourcing (distributed across cited ancestors).
- **2 slots** → platform.
- **2 slots** → treasury.

The 28 sourcing slots are partitioned across the cited ancestors at the author's discretion. Each partition carries a **reason** — a written justification for why that particular ancestor gets that particular slot count. For example, for a chain that builds on three prior works:

- **12 slots** → `@alice/architecture-chain/v2` — "primary structural pattern borrowed wholesale."
- **10 slots** → `@bob/extraction-skill/v1` — "extraction prompt adapted with modifications."
- **6 slots** → `@carol/webbing-skill/v3` — "one step uses this skill directly."

The same rule applies to every contribution type. A supersession of a config directs sourcing slots to the superseded version (with reasons for what changed and what's preserved). A pyramid node sourced from evidence directs slots to those L0 nodes and to the source document/chunk. An annotation on a published pyramid sources slots to the node being annotated. A composed analysis sources slots to the contributions it critiques, steelmans, or builds on.

Every sourcing reason is published alongside the contribution and visible to anyone who pulls or queries it. This is where the "directable as directed" part of the rotator arm matters: the allocations are the author's judgment, but the judgment is **transparent and challengeable**.

### Firsts: self-sourcing with reason

A "first" is a contribution with no prior sources — nothing external it derives from. UFF still applies: the 28 sourcing slots are mandatory. The author allocates them **to themselves**, with a reason.

Typical self-sourcing reason: "no prior art identified; this contribution is a primary origin in its domain." The transparency matters — self-sourcing is legitimate only when the reasoning holds up. If a critic can point at prior work the author missed or ignored, the self-sourcing is challengeable.

### Challenge

Any operator who believes a sourcing allocation is wrong can **file a challenge** against the contribution. A challenge names the disputed allocation, proposes a different one, and gives a reason. The author can respond — adjusting the allocation (publishing a new version), defending the original, or conceding. Challenges become part of the contribution's public record.

Well-reasoned sourcing + responsive authors produce high-reputation contributions. Dodged or stale sourcing produces low-reputation ones. Over time the ecosystem rewards authors who cite carefully.

### Why this shape

- **No decimals.** Every credit is atomic, so no rounding drift over time.
- **Arbitrarily complex splits.** Any allocation that fits in 80 integer slots is expressible.
- **Citation chains pay back automatically.** UFF means derivative consumption feeds the lineage without separate accounting logic — it's baked into every citation-bearing directive.
- **Audit-friendly.** Each credit has exactly one recipient, one slot, and (for sourcing) one reason. The ledger replays cleanly and the reasoning is legible.
- **Socially enforceable.** Transparent reasons + challenge capability make sourcing a visible public process, not hidden math.

### Per-contribution directive

Within UFF's 28-slot sourcing floor, the author sets the remaining 52-slot directive when publishing — typically creator gets 48 with platform/treasury at 2 each, but the remaining slots are redirectable. Public-good contributions often push platform/treasury slots to other roles; collaborative contributions spread creator slots across multiple authors.

The directive and every sourcing reason are visible in the publish preview and in every transaction that pays out against it.

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

Common for users who just want to use Agent Wire Node for their own pyramids and don't care about market participation.

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
