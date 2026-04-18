# Publishing

Publishing turns a local contribution into a Wire-addressable artifact with a durable handle-path. This doc walks through the publish flow for each contribution type, the preview and confirmation steps, pricing, and what changes on your node after a successful publish.

---

## The general publish flow

Every publish goes through the same steps:

1. **Pick the contribution.** In Tools mode, select one of your local contributions (draft or an updated version of something previously published).
2. **Pick access tier.** `public`, `circle-scoped`, `priced`, or `embargoed`.
3. **Set pricing** (if `priced`). Price in credits; optional floor, optional promotional discount.
4. **Set circles** (if `circle-scoped`). Which handles or groups can pull.
5. **Review the publish preview.** Dry run — what will be sent, warnings, cost to consumers, supersession chain.
6. **Confirm.** Agent Wire Node contacts the coordinator. Handle-path is allocated. Body is uploaded.
7. **Done.** Your contribution appears in Tools with a "published" badge and a handle-path. It's now discoverable (per access tier) and pullable.

The publish preview is load-bearing — it's your last chance to catch issues. It explicitly lists:

- Required credentials (auto-detected from the body's `${VAR_NAME}` references).
- Cost estimate to consumers.
- Supersession chain.
- Warnings (unusual size, missing metadata, questionable licensing, detected credential leaks).

If the preview shows something wrong, back out, fix locally, re-preview.

---

## Publishing a chain

From Tools → Contributions → your chain variant → **Publish**:

- Confirm the chain YAML is what you want published. Credential references are intact (`${OPENROUTER_KEY}`, etc.); no values leaked.
- Pick access tier.
- Describe what the chain does (required).
- Tag for discoverability (recommended).
- Preview. Check that required credentials are the ones you expect consumers to need.
- Confirm.

After publish, consumers can pull your chain and assign it to their pyramids. Rotator arm royalties flow to you on each consumption.

Chains that reference other contributions (skills, prompts, other chains) carry those as `derived_from`. Your published chain's provenance citesthe chain's dependencies, and royalty flows split accordingly.

---

## Publishing a skill

From Tools → Contributions → your skill → **Publish**:

- Confirm the prompt markdown and the output schema.
- Set the target primitive (`extract`, `classify`, `synthesize`, `web`, `compress`, `fuse`).
- Set a default tier hint (optional).
- Tag.
- Preview. Skills often have very small bodies — the preview shows the entire prompt.
- Confirm.

See [`44-authoring-skills.md`](44-authoring-skills.md).

---

## Publishing a question set

From Tools → Contributions → your question set → **Publish**:

- Confirm the decomposition tree.
- Set default granularity and max depth.
- Apex question text (required, unique within your handle).
- Tag.
- Preview. Shows the full tree structure.
- Confirm.

See [`45-question-sets.md`](45-question-sets.md).

---

## Publishing a config

From Tools → Contributions → your config → **Publish**:

- Confirm the config YAML.
- Declare the schema type (usually auto-detected from the contribution's schema annotation).
- Tag.
- Preview.
- Confirm.

Configs for profiles ("OpenRouter-heavy tier routing", "aggressive DADBEAR") are valuable. An operator with a well-tuned config for a common hardware class can publish it and earn rotator-arm royalties when others adopt.

See [`46-config-contributions.md`](46-config-contributions.md).

---

## Publishing a pyramid

Pyramids are larger than chains/skills/configs and have their own publish flow:

1. **From Understanding → detail drawer → Publish Now**.
2. **Access tier.** `public` / `circle-scoped` / `priced` / `embargoed`.
3. **Pricing** (if `priced`). Because pyramids are large, `priced` is the most common paid tier for pyramids.
4. **Absorption config.** How the pyramid responds to incoming questions. Mode (`open`, `absorb-all`, `absorb-selective`), chain for absorption.
5. **Cache manifest toggle.** Opt in to include pre-computed cache entries so consumers get usable speed immediately on pull. Recommended for published pyramids you expect to be queried heavily.
6. **Dry run.** Agent Wire Node runs a preview — structured node data, size, provenance, supersession chain, cost (if emergent).
7. **Confirm.**

Published pyramids are queryable by anyone with access. Queries happen against your running Agent Wire Node through the coordinator. Today those queries are **attributed** (the authoring node sees requester identity); when relays ship, unlinkable queries become possible.

---

## Publishing a composed contribution

From Compose mode → your draft → **Publish**:

- Title, body (markdown), type, topics, tags, target.
- Advanced options (granularity, max depth, manual reference overrides).
- Preview. Shows the canonical text and provenance.
- Confirm.

Composed contributions are first-class — they appear in Search, can be cited, rotator-arm royalties flow on consumption. See [`32-compose.md`](32-compose.md).

---

## Pricing

For contributions you set a price on (`priced` tier):

- **Price in credits.** You pick the amount.
- **Floor price.** Optional — disallow discounts below this.
- **Promotional discount.** Optional — temporary reduction.
- **Bundle pricing.** Multi-contribution packages (planned).

Prices can be changed by publishing a new version with a different price. Existing pulls at the old price remain (contributions are immutable once pulled).

The rotator arm splits each paid pull:

- 76% to creator (you).
- 2% to platform.
- 2% to treasury.
- Remainder reserved for roles like relays once shipped.

Defaults are configurable per contribution at publish time (some authors waive the platform/treasury cut for public-good contributions).

See [`74-economics-credits.md`](74-economics-credits.md).

---

## Supersession (publishing an update)

When you publish an updated version of a previously-published contribution:

1. Agent Wire Node detects the supersession chain (your local version supersedes a previously-published contribution).
2. The publish preview shows the chain: "This supersedes `@you/slug/v2`."
3. On confirm, the new version is published with `supersedes_id` set to the previous handle-path.
4. Consumers who pulled v2 see a notification that v3 is available.

Versioning convention: bump major version for breaking changes (different shape, removed fields), minor for non-breaking refinements. You choose the bump; it's not auto-computed.

---

## Retracting

From the contribution's detail view → **Retract**:

- Marks as retracted on the Wire.
- Doesn't delete — contributions are never hard-deleted. Prior pulls stay valid on consumer nodes.
- A retraction notice appears to anyone who has pulled the contribution.
- Retraction is versioned — you can un-retract by publishing a new version.

Retract when a contribution turns out to be wrong enough that you don't want it propagating. For smaller fixes, publish a correction via supersession.

---

## Dry-run publish without actually publishing

You can run the dry-run preview without committing to publish. From Tools → your contribution → **Preview publish** (or `pyramid-cli dry-run-publish`). Shows everything the real publish would show. Useful for checking credential warnings or cost estimates before deciding.

---

## Things to be careful about

**Credentials in the body.** Agent Wire Node scans for `${VAR_NAME}` references (fine) and for raw credential values (aborts). Still, check the preview. Any raw secret you paste into a YAML will be caught by the scan, but build the habit of using variables.

**Overbroad tags.** Tagging a skill with every conceivable topic doesn't help discovery; it just dilutes signal. Pick tags that describe what the contribution actually does.

**Big pyramids.** Published pyramids can be multi-hundred-megabyte artifacts. The cache manifest option makes them usable faster but adds substantially to publish size. Use it for pyramids you expect heavy queries on; skip for archival publications.

**Wire-facing attribution.** Your handle appears on everything you publish. Think about whether you want this under your real handle or a pseudonym.

**Breaking changes.** Pulling consumers depend on the shape of what they pulled. Breaking changes without a major version bump are hostile. When in doubt, bump major.

---

## Where to go next

- [`62-pulling-and-discovery.md`](62-pulling-and-discovery.md) — the other side of the flow.
- [`60-the-wire-explained.md`](60-the-wire-explained.md) — mechanics underneath.
- [`74-economics-credits.md`](74-economics-credits.md) — rotator arm and royalties.
- [`33-identity-credits-handles.md`](33-identity-credits-handles.md) — the handle that publishes under.
