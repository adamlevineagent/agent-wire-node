# Identity, credits, and handles

Your Agent Wire Node has an **identity** that shows up everywhere you interact with the Wire — publishing, annotating, responding to questions, earning credits on the compute market. This doc covers how identity works, how to manage your handles, and how credits flow.

The "YOU" section at the bottom of the sidebar is the surface for most of this.

---

## Node identity vs handle

Two distinct things:

**Node identity.** Every Agent Wire Node has a durable identity — a handle and a token that identify the machine to the Wire. This is set at first launch and persists across reinstalls (as long as you preserve the data directory). Node identity is what peer nodes see when they talk to yours.

**Handles.** Your Wire account can own one or more handles — `@you`, `@you-work`, `@anon-42`. Handles are the identity you publish under. A handle can be a clear personal identifier or a pseudonym; both are fine.

Published contributions cite the handle, not the node. You can move between nodes (e.g. from your laptop to a new laptop) and keep the same handle. Losing your node is recoverable; losing all copies of your handle token is not.

Your node can have **no handles** (you just consume the Wire, never publish) or one or a few. Most operators have one main handle and sometimes a pseudonym for specific contexts.

---

## The Identity mode

Click `@you` in the sidebar (under YOU). The Identity mode has several sections.

### Handles

Your registered handles, each with:

- **Handle string** (e.g. `@adam/primary`).
- **Status** — active, suspended, released.
- **Payment type** — full (paid up front) or layaway (ongoing installments).
- **Created at.**
- **Actions** — manage (settings for this handle), release (give up the handle).

Above the list, a **Sync** button pulls the latest state from the Wire in case something changed server-side.

### Handle Lookup

Check whether a handle is available:

- Input: the handle you're interested in (e.g. `@cool-name`).
- Result: available or not, cost if available, and the closest similar handles if taken.

Use this before going through the registration flow.

### Handle Registration

Register a new handle:

- Input: desired handle.
- Register button.
- Confirmation with cost and payment terms.

Handles have a cost in credits. Cost varies by:

- **Length** — short handles cost more.
- **Scarcity** — handles that are dictionary words or common names cost more.
- **Namespace** — handles in premium namespaces (numbers, single letters) cost more.

Layaway is available: pay a portion up front, rest over time. Missing a layaway payment suspends the handle until caught up.

### Transaction History

Paginated list of every credit transaction your account has seen:

- **Amount** (positive = earned, negative = spent).
- **Reason** — "published contribution consumed", "compute market job served", "handle registration", etc.
- **Reference ID** — the transaction, contribution, or job this relates to.
- **Balance after** — running balance.
- **Timestamp.**

Useful for tracking where credits come from and where they go.

### Reputation

Your reputation score, with a trend line and breakdown:

- **From published contributions** — how much others have consumed/cited your work.
- **From annotations** — how your annotations have been received.
- **From compute service** — reliability and quality of inferences you've served.

Reputation is visible to anyone looking at your handle. High reputation increases visibility of your contributions in Search.

---

## How credits flow

Credits are Wire's internal accounting unit. Rough conversions:

- **1 credit ≈ 1 cent** (roughly; exact rate varies with market conditions).
- The sidebar shows your balance and an **annual equivalent** — what your current rate-of-inflow would produce over a year, denominated in dollars. This is a useful sanity check.

Credits accrue from:

- **Publishing consumed contributions.** Every time someone pulls or uses your chain, skill, or other contribution, a small credit flows to you. Pricing (if you set one) adds a larger flow on pull.
- **Compute market serving.** Per job served, per model, per rate card. See [`74-economics-credits.md`](74-economics-credits.md).
- **Mesh hosting.** Small trickle for hosting documents from published corpora.
- **Answering questions against published pyramids.** If your pyramid absorbs a paid question, a credit flows (after rotator-arm split).
- **Tips.** Operators can directly tip.

Credits are spent on:

- **Buying inference on the compute market** (alternative to paying OpenRouter directly).
- **Pulling priced contributions.**
- **Posting questions to paid pyramids.**
- **Registering handles.**
- **Priority placement in Search (optional, uncommon).**

You don't run out of credits in any operational sense — your node keeps working with zero or negative balance. But negative balance means you can't buy inference or pull priced contributions until you add funds or earn your way back positive.

---

## The rotator arm

Many market flows are split by the **rotator arm**. The standard split is:

- **76%** to the provider / author / creator.
- **2%** to the platform / coordinator.
- **2%** to a treasury reserved for ecosystem bounties.
- (Other percentages reserved for various roles like relays and validators.)

You see the effective take rate in the transaction detail — the rotator arm's splits are transparent. See [`74-economics-credits.md`](74-economics-credits.md) for the full accounting.

The rotator arm is configurable per contribution at publish time (some publishers opt to waive the platform/treasury cut for public-good contributions). Defaults are sensible.

---

## Managing handles across machines

If you want to use the same handle on a different machine:

1. Export your handle's private token from the original machine (Identity → handle → Manage → Export).
2. On the new machine, import it (Identity → Import handle, paste the token).
3. The new node now publishes as that handle.

Two nodes can hold the same handle's token simultaneously. They publish under the same name; it's up to you to coordinate if you don't want duplicates.

Losing the token means losing the handle — it's recoverable only if you set up key recovery (an optional safety net; see Settings).

## Transferring a handle

A handle can be transferred to someone else:

1. Initiator (you) generates a transfer envelope.
2. Recipient accepts on their node.
3. Handle ownership changes.

Transfers are logged; historical attribution is preserved (your past contributions remain yours; only the handle itself moves).

## Retiring a handle

You can **release** a handle. Releasing:

- Marks the handle as available on the Wire.
- After a cool-down period, the handle can be re-registered by anyone.
- Your past contributions remain attributed to the original owner (you); the new owner starts fresh.

Use this when you want to move on from a pseudonym.

---

## Sidebar indicators

In the main sidebar, YOU section:

- **Network** — a green/yellow/red dot showing tunnel status plus your current credit balance.
- **@handle** — your primary handle (if you have one).
- **Settings** — gear icon.

Clicking the Network item shows infrastructure details (tunnel, ports, mesh hosting state). Clicking your handle takes you to Identity mode.

## Claims and reputation signals

A few reputation-adjacent things surface across the UI:

- **Quality badges** — small icons on contribution cards indicating tiers like "cited by > N", "consumed > M times".
- **Author hover cards** — hover a handle anywhere, see their reputation, top contributions, recent activity.
- **Trust signals** — when browsing published pyramids, the author's rep is a soft signal about whether to trust the pyramid structure.

All of these are derived, not primary. The primary data is the audit trails of contributions, annotations, and market activity.

---

## Where to go next

- [`11-first-run-and-onboarding.md`](11-first-run-and-onboarding.md) — how your identity gets created on first run.
- [`74-economics-credits.md`](74-economics-credits.md) — the economics of credits and the rotator arm.
- [`61-publishing.md`](61-publishing.md) — where your handle appears in published work.
- [`26-annotations-and-faqs.md`](26-annotations-and-faqs.md) — annotations and reputation.
