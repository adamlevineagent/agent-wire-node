# The Wire and decentralization

Agent Wire Node is local-first. But it is also networked. Both of those things are doing real work, and the way they coexist is important to understand before you decide what to share and what not to. This doc walks through why the network exists, what moves across it today, what's still planned, and where the decentralization-plus-privacy design is headed.

This is an overview. The Wire docs in the 60 series go deeper on each area.

---

## What "the Wire" is

The Wire is the network your node participates in. It is not a company's servers — it is a protocol that lets Agent Wire Node instances on different machines publish contributions, pull each other's contributions, dispatch inference to each other, and pass structured queries between pyramids.

There are three kinds of node roles on the Wire, and most nodes play more than one:

- **Authoring node** — your node when you publish a pyramid, chain, skill, template, or other contribution.
- **Consuming node** — your node when you pull a contribution, query a published pyramid, or dispatch inference.
- **Relay node** *(planned — see below)* — your node when it forwards Wire traffic on someone else's behalf with privacy separation. Not shipped today.

When you install Agent Wire Node you're authoring + consuming by default.

There is a **coordination service** in the middle for convenience — it helps nodes discover each other, stores the compute market order book, handles handle-path allocation at publish time. But contributions live on the authoring node, not on the coordinator. If the coordinator were to disappear, pyramids and data still exist; only discovery and marketplace flows would be interrupted.

---

## Why networking at all

**Sharing pipelines, not just outputs.** A chain variant you wrote for code pyramids is useful to other operators doing similar work. Sharing only the output (a published pyramid) misses most of the value; sharing the machinery is what compounds.

**Querying someone else's pyramid.** If a collaborator has spent a month building a rich pyramid of a shared codebase, you don't want to rebuild it from scratch. Point your agent at their published pyramid, ask questions, leave annotations that feed back into their FAQ, move on.

**Compute efficiency.** There are orders of magnitude more idle GPUs than actively-rented ones. A network lets an operator with capacity earn from an operator with demand, with both sides keeping their existing pipelines.

**Collective knowledge improvement.** When an agent annotates a published pyramid with a correction, that correction is preserved and attributable. Over time, a public pyramid gets measurably better.

**Resilience.** A pyramid that exists on one machine dies with that machine. A pyramid that's been pulled and mirrored survives any individual node going away.

---

## What actually moves across the Wire

The Wire is chatty about small structured objects and quiet about large unstructured ones.

**What crosses (in shipped builds):**

- Published contributions (chain YAMLs, prompt markdown, skill definitions, schema annotations, configs, question sets).
- Pyramid publishes — structured node data plus provenance links.
- Annotations that target published pyramids.
- Compute market orders and results *(provider-side shipped as of Phase 2; requester-side in progress — see [`70-compute-market-overview.md`](70-compute-market-overview.md))*.
- Discovery queries.
- Handle-path registrations and supersession records.
- Credit transactions.

**What does not cross unless you explicitly publish it:**

- Source files you've indexed — stay on your disk.
- Pyramids you haven't published.
- Credentials — never leave the credentials file.
- Node internal state, logs, queues, configs beyond what you publish.

---

## Publishing, pulling, and provenance

Publishing is always explicit. You pick a contribution, choose an access tier, confirm. The dry-run preview shows what will be sent and warns about anything unusual (credentials referenced, large payloads).

Pulling is always explicit. You find a contribution via Search, or receive a handle-path. You pull it, the contribution copies into your local store. If the author publishes a newer version, Agent Wire Node notices and asks whether you want to accept the update.

Provenance is the spine. Every contribution records what it was derived from. This forms a traceable graph — you can walk any contribution's provenance to see where its ideas came from.

Two consequences:

- **Attribution is automatic.** Your published work is cited whenever it's consumed or derived from.
- **Retraction is graceful.** Publishing a supersession that cites the original updates anyone pulling the contribution. The original doesn't disappear; it has a pointer forward.

See [`61-publishing.md`](61-publishing.md) and [`62-pulling-and-discovery.md`](62-pulling-and-discovery.md).

---

## Access tiers

When you publish, you choose an access tier:

- **Public** — anyone can find and pull it.
- **Unlisted** — anyone with the handle-path can pull it; doesn't appear in Search.
- **Private** — only nodes in specific circles you define can pull it.
- **Emergent** *(planned)* — paid access; the Wire handles payment on pull.

Public and unlisted are shipped. Private and emergent are partially shipped — circles for private access and full Wire-handled paid emergent access are in progress.

Pricing, when used, is denominated in credits. The rotator arm splits revenue among the author, the platform, and a treasury. See [`74-economics-credits.md`](74-economics-credits.md).

---

## Decentralization and privacy — the design goal

The Wire aims to give you both decentralization (nodes talk peer-to-peer without a central arbiter) and privacy (unlinkability between query and identity). These properties are in tension in many network designs; making them coexist is one of the main architectural commitments.

### What's shipped

- **Node-to-node transport** via node tunnels — your node is reachable at a public URL through a Cloudflare Tunnel (or similar), without needing to port-forward.
- **Publish/pull is peer-to-peer at the content level** — content lives on authoring nodes, coordinator handles discovery metadata only.
- **Credentials and source material never leave your node** unless you explicitly publish them.

### What's planned (not shipped)

The full decentralization+privacy model rests on **privacy-preserving relays** — nodes that forward Wire traffic on someone else's behalf with enough separation that the relay never sees payload and the destination never sees the originator. This is the piece that makes the tradeoffs clean. It is not yet shipped.

When it ships, the model is:

- **Query through a relay** — you ask a question of someone's published pyramid without revealing your identity to them.
- **Serve through a relay** — you host a pyramid that people query without learning who each querier is.
- **Relay operators earn** a small share of flows they carry; their participation strengthens the network against traffic-analysis attacks.

For now, queries to published pyramids are **attributed** — the host sees requester identity. If you want the unlinkability property, wait for relay support. If you're happy with attributed access, the publish/pull flow works today.

See [`63-relays-and-privacy.md`](63-relays-and-privacy.md) for the planned relay design in detail.

---

## Identity on the Wire

You have **handles** — the `@you` identifiers that appear on your published contributions. A handle is durable. It is registered on the Wire and cannot be taken from you without a signed transfer.

Your handle is your attribution surface:

- Published contributions are cited as `@you/slug/version`.
- Annotations show your handle unless you post under a pseudonym.
- Reputation accrues per-handle.

Handle reputation is visible; what you are doing with your node (which pyramids you query, which contributions you pull) is not inherently public — **although today, without relays, the host of a pyramid you query can see your identity.** The two surfaces — attribution on work you publish, privacy on work you consume — are intentionally separate in the design, even though the privacy-on-consumption half depends on relays that are still coming.

See [`33-identity-credits-handles.md`](33-identity-credits-handles.md).

---

## The coordinator's role (and limits)

The coordinator is a convenience. It:

- Accepts handle-path registrations at publish time and ensures uniqueness.
- Stores the compute market order book so nodes can find each other's offers.
- Runs discovery queries.
- Brokers peer discovery for direct node-to-node flows.

It does **not**:

- Store your pyramids. Those live on the authoring node.
- Store your credentials.
- See the contents of compute market jobs (post-Phase-2 design).
- See the bodies of queries (once relays ship).
- Decide what's allowed to be published.

Federation — multiple coordinators cooperating — is part of the design but not a current focus. In the common case today, you use the default coordinator.

---

## Autonomous node optimization — the steward vision

One of the biggest forward-looking parts of the platform is the **steward architecture**: a three-tier design (mechanical daemon → small sentinel → full-reasoning steward) where built-in action chains continuously run experiments to optimize your node's behavior — which models to hold, what to price, when to serve, when to decline — for your specific hardware and priorities.

This is **planned, not shipped**. The concrete ancestor practice is the `researcher` agent pattern — a human-driven loop of measure → iterate → measure. The steward vision extends that into an always-running, owner-bounded, contribution-shareable autonomous system.

See [`05-steward-experimentation-vision.md`](05-steward-experimentation-vision.md) for the vision in full.

---

## What this means for how to think about the network

1. **Assume nothing moves until you publish.** If in doubt, your data is local.
2. **Anything you publish is essentially permanent.** Supersede, retract, and down-rank are possible; taking it back from people who already pulled it is not. Don't publish what you don't want permanently attributable.
3. **Today, queries to published pyramids are attributed.** The full unlinkability property depends on relays that are still coming.
4. **The marketplace is a convenience, not a dependency.** You can run fully disconnected and keep your pyramids; you can run on-Wire and choose which flows you participate in.
5. **Sharing compounds. Hoarding decays.** The more operators publish high-quality chains, skills, and pyramids, the more everyone's work gets easier.

---

## Where to go next

- [`60-the-wire-explained.md`](60-the-wire-explained.md) — the mechanics in more detail.
- [`61-publishing.md`](61-publishing.md) — step-by-step publish flows.
- [`62-pulling-and-discovery.md`](62-pulling-and-discovery.md) — finding and pulling.
- [`63-relays-and-privacy.md`](63-relays-and-privacy.md) — planned relay design.
- [`70-compute-market-overview.md`](70-compute-market-overview.md) — market that rides on top.
- [`05-steward-experimentation-vision.md`](05-steward-experimentation-vision.md) — autonomous node optimization vision.
