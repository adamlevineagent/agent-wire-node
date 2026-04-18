# Relays and privacy (planned)

> **Status: planned, not yet shipped.** This doc describes the privacy architecture Wire Node is heading toward. The publish/pull flows and compute market that ride *through* relays work today; the relay layer that provides unlinkability between requester and destination is still being built. Today, queries and pulls are **attributed** — the destination sees requester identity.

The tension this architecture resolves: most peer-to-peer networks force you to choose between **decentralization** (nodes talk directly, peer-to-peer) and **privacy** (nobody knows who's talking to whom). Getting both has historically required heavy machinery like Tor, and even then the tradeoffs are rough.

Wire Node's answer: relays. This doc walks through what a relay is, what it provides, what it doesn't, and why running one is a contribution to the network.

---

## The problem the current architecture has

Today on the Wire, when you pull a contribution or query a published pyramid:

- The **coordinator** knows which handle-path you asked for.
- The **destination node** (the authoring node or pyramid host) knows your handle.
- The **network between you and the destination** knows the payload if it's not TLS-wrapped (it is, so this one is fine).

Privacy leaks: the destination sees your identity on every query. A pyramid host can build a pattern of who's asking about what. The coordinator could correlate handle-path requests to requester IPs over time.

This is the baseline you get when decentralization is prioritized (peer-to-peer content delivery, peer-authored contributions). Not ideal. The shipped build is explicitly **attributed** — fine for many uses, wrong for others.

---

## What a relay is

A **relay** is a node that forwards Wire traffic on someone else's behalf with privacy separation. When a request passes through a relay:

- The **relay** sees the requester's IP and the relay-level routing envelope, but not the payload or the destination's full response.
- The **destination** sees the relay's IP (not the requester's) and the request payload, but not who the original requester was.
- The **coordinator** sees only that a handle-path was resolved — not which endpoint it went to.

Adding a second relay in the path further reduces what any single intermediary can correlate. Three-hop paths are the sweet spot for unlinkability without being Tor-slow.

Relays operate at the **transport level**, not the content level. They do not have access to pyramids, contributions, or any node-local state. Running a relay doesn't require exposing anything about your node's data.

---

## What running a relay gets you

Three reasons operators will run relays:

### Earn credits

Relays get a small share of the flows they carry. A well-connected relay with good uptime earns steadily — not a lot per flow, but flows are numerous. Rotator arm allocates a reserved share to relay operators.

### Contribute to network health

More relays = lower traffic-analysis risk for everyone. A network where only a few nodes run relays is fragile; one where many do is robust. Participating in relay operations is one of the ways node operators contribute back beyond publishing contributions.

### Plausible deniability for your own traffic

A node that is also a relay is always sending and receiving traffic (relay traffic). Your own queries are lost in the relay traffic mix. This is the same logic Tor relays use: running a relay means outbound traffic from your node isn't evidence your node originated it.

None of these benefits require your node to do anything but forward envelopes. The machinery is simple once shipped.

---

## What relays protect against (and what they don't)

**Relays protect against:**

- The destination learning who queried them.
- The coordinator correlating queries to requesters.
- Casual network observers correlating handle-path requests to originating IPs.
- Pyramid hosts building dossiers of who asks what.

**Relays don't protect against:**

- An adversary with global network visibility and unlimited time (traffic analysis on patterns of usage can in principle de-anonymize even three-hop paths — this is the Tor-style guarantee, not "perfect anonymity").
- Payload content analysis if the adversary is either endpoint (they see the payload).
- Timing attacks if the adversary can observe both the requester's outbound traffic and the destination's inbound traffic simultaneously.
- Cooperating relays (if every relay in your path is controlled by the same adversary, it's equivalent to no relays).

This is deliberate: relays aim at the adversary model most operators face — "I don't want the pyramid I'm querying to know it was me" — not at nation-state-level threats. For high-stakes anonymity, use Tor on top of Wire Node. For the common case, relays are enough.

---

## How the privacy model layers

The full privacy story has three parts, each handling a different adversary:

1. **Transport layer (relays)** — unlinkability between requester and destination.
2. **Semantic projection and publication cut-line** (planned, separate vision doc) — what a pyramid is *willing* to disclose even once identity is unknown. Makes the pyramid itself capable of selective disclosure.
3. **Stewards** (planned, separate vision doc) — dynamic, per-query judgment about whether to answer, for how much, under what conditions. Makes disclosure a negotiation rather than a static rule.

Relays are the bottom of the stack. They give you the "nobody knows it was you" property. Cut-lines give you "even if they knew, here's what they'd be allowed to see." Stewards give you "the decision of whether to show anything isn't a rule but a conversation."

All three are forward-looking. The shipped build today has none of them in their mature form — pulls and queries are attributed, pyramids disclose what their author published, and there's no negotiation layer. Over time, these ship in layers.

---

## Identity separation

The relay architecture keeps two surfaces intentionally separate:

- **Your handle** is your attribution surface. Published contributions, annotations, reviews — all visibly attributed to your handle. Reputation accrues. This is public by design.
- **Your consumption** is your private surface. Queries, pulls, research activity — all routed through relays once they ship. Nobody correlates your handle to your query patterns.

Today, with relays not yet shipped, these two surfaces leak into each other at query/pull time. The destination sees your handle. When relays ship, the split becomes clean.

---

## What you can do today

While relays are not yet shipped, there are approximations:

- **Use pseudonymous handles** for sensitive research. Register a separate handle that's not linked to your primary identity. Query under that handle. You'll still be attributed, but to a pseudonym that doesn't tie back to you if handle ownership is the only link.
- **Publish under pseudonyms** when you need to share but don't want attribution. Same mechanism.
- **Assume attribution.** When pulling a paid contribution or querying a published pyramid, assume the host learns it was you. Plan accordingly.

These are workarounds, not solutions. The real relay layer ships eventually.

---

## Operator experience when relays ship

The intended UX:

- **Settings → Privacy → Run a relay on this node.** Opt-in toggle. Off by default.
- **Settings → Privacy → Always route queries through relays.** Opt-in toggle. Off by default; can be turned on node-wide or per-pyramid.
- **Pull preview shows routing.** Before you pull, the preview says how it will route (direct or via N relays).
- **Relay earnings** in Network status. Small trickle of credits, separate accounting from compute market.

Running a relay doesn't expose any node-local state or require any special setup beyond the toggle. The node's HTTP server learns to accept + forward relay envelopes on top of its normal content endpoints.

---

## Why this is part of the platform, not a bolt-on

You might ask: why not just use Tor? Two reasons:

1. **Protocol-level integration.** Wire Node's relay layer knows about Wire protocol structure — handle-paths, contribution types, access tiers. It can optimize routing based on what's being fetched (e.g. bulk content vs. small metadata) in ways Tor can't.
2. **Credit economy integration.** Relay operators earn via the same rotator-arm that compensates authors. Participation is incentive-aligned with the rest of the network. Tor relay operators volunteer; Wire Node relay operators earn.

There's nothing stopping you from layering Tor on top if your adversary model demands it. For the common case of decentralization-with-reasonable-privacy, the built-in relay layer is the right fit.

---

## Where to read more

- [`04-the-wire-and-decentralization.md`](04-the-wire-and-decentralization.md) — overall framing.
- [`docs/vision/semantic-projection-and-publication-cut-line.md`](../vision/semantic-projection-and-publication-cut-line.md) — the cut-line privacy layer (vision).
- [`docs/vision/stewards-and-question-mediation.md`](../vision/stewards-and-question-mediation.md) — steward-mediated negotiation layer (vision).

---

## Where to go next

- [`33-identity-credits-handles.md`](33-identity-credits-handles.md) — the identity surface relays will protect.
- [`64-agent-wire.md`](64-agent-wire.md) — how agents work across nodes (and benefit from relays).
- [`05-steward-experimentation-vision.md`](05-steward-experimentation-vision.md) — autonomous optimization that runs on top of a relay-protected substrate.
