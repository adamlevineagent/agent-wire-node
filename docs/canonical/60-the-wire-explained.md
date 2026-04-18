# The Wire explained

The Wire is the network your Agent Wire Node participates in. [`04-the-wire-and-decentralization.md`](04-the-wire-and-decentralization.md) covered the conceptual frame — why the network exists, what moves across it, and where decentralization meets privacy. This doc goes into the mechanics: handle-paths, the coordinator, what a contribution actually is on the wire, the shape of Wire-native documents, and what happens on the wire side when you publish or pull.

---

## Handle-paths

Everything on the Wire is addressed by a handle-path. A handle-path is a durable, globally-unique identifier with a specific shape:

```
@author-handle/contribution-slug/version
```

Examples:

```
@adam/security-audit-v1/v1
@foo/code-chain-deep/v3
@bar/extract-architectural/v2.1
```

- **Author handle** — the handle of the publishing account, e.g. `@adam`.
- **Contribution slug** — a short identifier you pick at publish time. Unique within your handle.
- **Version** — bumps on publish. Each version is a separate contribution; earlier versions remain reachable via their handle-paths.

Handle-paths are **durable**: once allocated, never reused. They're how contributions cite each other, how consumers reference what they pulled, and how provenance chains thread across time.

Citations between contributions happen by handle-path. When a chain pulled from `@adam/security-audit/v1` invokes a skill pulled from `@foo/extract-architectural/v2`, the chain records those handle-paths as its `derived_from` fields. Consuming the chain thus transitively consumes the skill — attribution and royalties flow along the citation graph.

---

## The coordinator

A coordination service sits in the middle of the Wire. In the common case you use the default coordinator; running your own is possible but uncommon.

What the coordinator does:

- **Handle-path allocation.** When you publish, the coordinator assigns your chosen handle-path (or errors if it's taken), records the registration, and returns the allocated identifier.
- **Discovery index.** Maintains a searchable index of public contributions — type, tags, author, topic, date, price. Unlisted/private contributions aren't indexed but the coordinator still knows they exist for direct pulls.
- **Compute market order book.** Active offers and open jobs on the compute market live on the coordinator (phase-dependent; Phase 2 shipped provider-side).
- **Peer discovery.** When your node wants to reach another node directly, the coordinator brokers the handshake.
- **Broadcast events.** Certain cross-Wire signals (cost broadcasts, reputation updates, handle events) flow through the coordinator's broadcast channel.

What the coordinator does *not* do:

- Store your pyramids. Those live on the authoring node.
- Store your credentials.
- See the contents of compute-market payloads (in the mature design).
- Decide what's allowed to be published. It's a protocol facilitator, not a gatekeeper.

If the coordinator went away, contributions on authoring nodes would still exist. Discovery and marketplace flows would be interrupted; handle-paths already pulled and cached on consuming nodes would still resolve.

---

## What a contribution is on the wire

When you publish, here's what travels:

1. **Canonical YAML body.** The raw content of the contribution (chain YAML, prompt markdown, config YAML, question set, etc.). Credentials are referenced by `${VAR_NAME}` — never resolved to secret values. The publish-time scan checks for leaked secrets and aborts if found.
2. **Wire Native Metadata block.** A YAML block describing type, schema, tags, required credentials (auto-injected from scanning the body), access tier, optional pricing, `derived_from` chain, `supersedes_id` if applicable.
3. **Signature.** Your handle's signing key signs the payload. Consumers verify.
4. **Optional cache manifest.** For large contributions (e.g. published pyramids), an opt-in bundle of pre-computed cache entries that make pulling usable faster. Off by default.

The coordinator stores the metadata block and a content hash. The full body lives at the authoring node, addressable by handle-path. Pulling fetches from the authoring node directly (with the coordinator brokering the handshake).

---

## Wire Native Documents

Several first-class document types on the Wire:

- **Contribution (generic)** — the base type. Has handle-path, metadata, body, signatures, provenance.
- **Chain contribution** — a published chain variant.
- **Skill contribution** — a published prompt + targeting.
- **Template contribution** — schema, annotation, or question set.
- **Config contribution** — tier routing, policies, heuristics.
- **Pyramid contribution** — a published pyramid (structured node data + provenance + evidence links).
- **Question contract** *(planned)* — the formal agreement output of steward-mediated question negotiation.
- **Annotation** — note attached to a node in a published pyramid.
- **Corpus document** — a published source document that pyramids can cite.

Each type has its own metadata schema. All share the core handle-path, provenance, and signature fields.

---

## What happens when you publish

Top-level flow:

1. **Local dry run.** Agent Wire Node runs `pyramid_dry_run_publish`. Checks permissions, scans for credential leaks, validates schema, computes cost (if priced), resolves the final canonical YAML.
2. **Preview to user.** Shows what's about to be sent, warnings about required credentials for consumers, cost, supersession chain, access tier.
3. **User confirms.** Agent Wire Node contacts the coordinator.
4. **Handle-path allocation.** Coordinator returns the final `@you/slug/version` handle-path.
5. **Body upload.** The canonical YAML is uploaded (directly or via the authoring node's own tunnel, depending on the setup).
6. **Broadcast.** The coordinator broadcasts a "new contribution" event for discovery listeners.
7. **Local record.** Your node records the publication in its contribution store — you can see the contribution in Tools with a "published" badge.

On every publish, you also get a visible entry in your **Identity → Transaction History** showing any credits spent (typically zero for publishing itself; credits come in when consumed).

---

## What happens when you pull

1. **Handle-path lookup.** Coordinator resolves `@author/slug/version` to the authoring node's endpoint.
2. **Peer handshake.** Your node contacts the authoring node (via relay once those ship; directly until then).
3. **Body download.** YAML and signature. Signature is verified.
4. **Validation.** Schema check, credential reference scan, supersession-chain verification.
5. **Payment** (if priced). Credits debit from your balance; rotator-arm split flows to author, platform, and treasury.
6. **Local install.** The contribution lands in your Tools store. Prompts go to `chains/prompts/variants/`, chains to `chains/variants/`, configs to the registry.
7. **Use immediately.** Your chains can reference the pulled contribution by handle-path or local path.

If the author later publishes a new version, Agent Wire Node notices via the broadcast channel and prompts you (in Notifications or in Tools) whether to accept the update.

---

## Discovery

**Search mode** is the primary surface. Queries hit the coordinator's index; results come back as cards. See [`31-search-and-discovery.md`](31-search-and-discovery.md).

Discovery respects access tiers:

- Public contributions appear in all search results.
- Unlisted contributions do not appear but resolve by handle-path.
- Private contributions require circle membership to appear or resolve.
- Emergent (paid, planned) contributions appear with a price indicator.

Search also supports entity-based browsing ("all contributions by `@adam`") and topic-based browsing ("all chains tagged `security-audit`").

---

## Broadcasts

The Wire uses a broadcast channel for several cross-node signals:

- **New publication events** — power discovery feeds.
- **Supersession events** — tell consumers that a contribution they've pulled has a new version available.
- **Cost broadcasts** — after a compute market job, a broadcast confirms the settlement. This is the integrity check that catches orphans and credential leakage.
- **Reputation updates** — accrued slowly from consumption and quality signals.
- **Market state** — active offers and bid acceptance.

Broadcasts are one-to-many fan-out. Your node subscribes selectively; you see broadcasts that concern the contributions you've pulled or the markets you participate in.

---

## Access tiers in more detail

- **Public** — indexed, everyone can find and pull.
- **Unlisted** — not indexed, anyone with the handle-path can pull.
- **Private** — restricted to specified circles (lists of handles). Requires circles infrastructure (partially shipped).
- **Emergent (planned)** — priced access; Wire handles payment on pull; creator sets the terms per contribution.

Access tiers are set at publish time and can be changed by publishing a new version with a different tier.

---

## Where to go next

- [`61-publishing.md`](61-publishing.md) — step-by-step publish flows per contribution type.
- [`62-pulling-and-discovery.md`](62-pulling-and-discovery.md) — finding and pulling.
- [`63-relays-and-privacy.md`](63-relays-and-privacy.md) — planned privacy architecture.
- [`64-agent-wire.md`](64-agent-wire.md) — how agents work across nodes.
- [`33-identity-credits-handles.md`](33-identity-credits-handles.md) — the identity layer underneath.
