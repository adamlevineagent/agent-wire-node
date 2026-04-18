# Pulling and discovery

Pulling is how you bring a Wire contribution into your local store — a chain variant, a skill, a config, a published pyramid — and use it in your own work. Discovery is how you find things worth pulling.

This doc covers the pull flow end-to-end, how updates to pulled contributions reach you, and patterns for discovering what's on the Wire.

---

## Discovery

### Search mode

Search is the primary surface for finding contributions. See [`31-search-and-discovery.md`](31-search-and-discovery.md) for the full UI tour.

Quick pointers:

- **Feed** — new / popular / trending contributions. Glance once a day for serendipity.
- **Results** — filtered search. Start with a keyword, narrow by type/tags/price/date.
- **Entities** — browse by author handle. Find operators whose output you respect and see what else they publish.
- **Topics** — topic-centric browsing. Good for exploring an area.

### By handle-path

If someone sends you a handle-path directly — `@adam/security-audit-v3/v1` — you can pull without discovery. Paste into the search bar or use `pyramid-cli pull <handle-path>` (planned — see Integration docs).

### Via agents

Claude (or another MCP agent) can discover on your behalf. Ask: *"Find me a chain variant for code pyramids focused on security analysis."* The agent searches the Wire, evaluates candidates, recommends. You confirm the pull.

### Via entities and provenance

When you're reading a pyramid (published or local) and you notice it was built with a chain you don't have, that chain's handle-path is right there in the pyramid's metadata. One click to pull the chain.

---

## The pull flow

Step by step:

1. **Open the contribution detail** from search results or a handle-path. You see type, author, description, body preview, tags, required credentials, price (if any), supersession chain, consumers count, reactions.

2. **Click Pull.** The pull preview runs:

   - Full body fetched from the authoring node (via relay once they ship; direct for now).
   - Signature verified against the author's handle.
   - Schema validated.
   - Required credentials checked against your local credentials file — you get a list of which variables are missing.
   - Cost checked against your balance (if priced).
   - Supersession chain shown (if you have an older version, what this replaces).

3. **Confirm.** Wire Node:

   - Pays any pull cost (rotator-arm split flows automatically).
   - Writes the contribution to your local store.
   - Registers it with the relevant system (chains with the chain registry, skills with the skill registry, etc.).
   - Logs the transaction in your Identity → Transaction History.

4. **Use immediately.** The pulled contribution is available:

   - Chains appear in Tools → My Tools → Chains, assignable to pyramids.
   - Skills appear under Skills, referenceable from chains.
   - Configs appear under Templates; you can set them active for the appropriate scope.
   - Pyramids appear in Understanding as "pulled" (queryable; not modifiable).

---

## Dry-run pull

You can preview a pull without committing. Useful for checking what would change on your node before confirming. Available from the pull button as **Preview pull**.

The preview is the same as the real pull's first-few-steps, minus the install. You see exactly what you'd get.

---

## Updates to pulled contributions

When the author publishes a new version of something you've pulled, Wire Node notices via the broadcast channel. You see:

- A **notification** in Operations.
- An **update available** badge on the contribution card in Tools.
- Optionally an automatic prompt to review on next use.

Click the badge or the notification → **update review** modal:

- Shows the new version's body + diff against what you have.
- Lists any new required credentials.
- Lists any new price changes.
- Shows what downstream in your setup depends on this (e.g. three pyramids use this chain).

Accept (pull the new version, supersede your local), decline (stay on the version you have), or postpone (decide later).

Accepting is the common case. Declining is usually right when:

- The diff doesn't affect how you use the contribution.
- The new version has a dependency you don't want.
- You've customized your pulled version and the update would clobber your changes (note: custom-on-top-of-pulled is a forkable pattern — you can fork rather than accept).

### Forking a pulled contribution

If a pulled contribution is close to what you want but not quite, you can **fork**:

1. In Tools, right-click the pulled contribution → **Fork**.
2. A local copy is created under your own handle (or unassigned, if you haven't picked one).
3. Edit freely. Your fork's provenance still cites the original — rotator-arm royalties still flow to the original author when your fork gets consumed.
4. Publish as your own contribution.

Forking preserves attribution. You don't get to claim you wrote it from scratch; you built on the original and the chain of provenance makes that explicit.

---

## Pulled pyramids

Pulling a pyramid is slightly different from other contributions:

- You don't get a local copy of the full pyramid structure by default.
- What you get is a **queryable reference** — you can call `apex`, `search`, `drill`, `faq` against the pyramid via the authoring node (through the coordinator).
- If you want a full local copy (for offline use, for archival, for cross-referencing in your own question pyramids), pull with **cache manifest** included (if the author published one).

Queries against pulled pyramids are billed per-query-with-synthesis if the author has emergent pricing, free otherwise.

Pulled pyramids can be **referenced by your own question pyramids** — you create a question pyramid that references `@someone/their-pyramid/v2`, and your question pyramid pulls evidence from their published L0/L1 layers via the Wire.

---

## Who sees what you pull

Today: **the authoring node sees requester identity on pulls.** Your handle is attached to the pull transaction. If you want a pull visible to the author so they get attribution metrics, that's the default. If you want the pull to be unlinkable (author doesn't know it was you), that property depends on relays which are still coming — see [`63-relays-and-privacy.md`](63-relays-and-privacy.md).

Some operators set **Always identify on pull** in Settings → Privacy (when shipped) so authors can reach out; others will want anonymous. The choice is yours once the machinery supports it. For now, all pulls are attributed.

---

## The pull cost breakdown

Free contributions: 0 credits.

Emergent (paid) contributions:

- **Purchase price.** Set by the author.
- **Rotator-arm split.** Of the price: 76% to author, 2% platform, 2% treasury, remainder to reserved roles. You see the breakdown in the pull preview and in your transaction log.

Some contributions come with **subscription-style pricing** — pull is free, but running the contribution (e.g. invoking a published chain) has a per-use fee. Rare but supported.

---

## Common discovery patterns

**"Find better defaults for my hardware."** Search → filter by type "config" and tag matching your hardware class (e.g. "apple-silicon-m2"). Pull the top-rated config, apply as your baseline, tune from there.

**"Find a skill for this specific extraction."** Search → filter by type "skill" and tag matching your goal (e.g. "security-extraction"). Read a few, pick one whose prompt looks good to you, pull, try.

**"Explore what a specific operator publishes."** Entities → search for their handle → browse all their contributions. Useful for finding adjacent work after you've liked one thing from someone.

**"Pull a pyramid a collaborator built."** Direct handle-path → pull with cache manifest → query freely.

**"Subscribe to a topic."** (Planned) Subscribe a topic (e.g. `security-analysis`); new contributions of that topic show up in your Feed without you searching.

---

## Where to go next

- [`31-search-and-discovery.md`](31-search-and-discovery.md) — Search UI in detail.
- [`61-publishing.md`](61-publishing.md) — the other side of the flow.
- [`28-tools-mode.md`](28-tools-mode.md) — where pulled contributions live.
- [`63-relays-and-privacy.md`](63-relays-and-privacy.md) — planned privacy over pulls.
