# Compose (drafting contributions to the Wire)

The **Compose** mode is where you draft contributions in long-form — commentary, analysis, corrections, and other contribution types that are primarily prose rather than configuration. Tools is for authoring chains and skills and templates. Compose is for authoring the discussion around them: the analysis of someone else's contribution, the correction someone else's chain needs, the commentary on a trend.

---

## What a composed contribution is

A composed contribution is a piece of prose, typed in Compose, that becomes a Wire contribution when published. It has:

- **Title.**
- **Body** — markdown, with full formatting support.
- **Type** — analysis, commentary, correction, rebuttal, steelman, review, timeline, diff, etc.
- **Topics** — tags that categorize it.
- **Target** — the specific contribution (or pyramid, or entity) this composed piece is about.
- **References** — other contributions it cites.
- **Granularity and max_depth** (advanced) — if the contribution will be treated as material for further pyramid building.

Composed contributions are first-class on the Wire. They are publishable, pullable, citable, priceable. They show up in Search. They can be part of provenance chains — your analysis of someone else's chain becomes a reference in that chain's consumers' discovery views.

This is different from annotations (which are pinned to pyramid nodes and feed FAQs; see [`26-annotations-and-faqs.md`](26-annotations-and-faqs.md)). Annotations are small, local, attached. Composed contributions are long-form, shareable, independent artifacts.

---

## The two tabs

- **Contributions** — your drafts (any state).
- **Review Feed** — contributions waiting for your attention (in grace period, flagged, or settled).

### Contributions tab

Filter: all / flagged / grace / settled.

Each draft card shows:

- Title.
- Type + target (what it's about).
- Saved time.
- Status (draft / published / flagged / retracted).
- Delete button.

Click a draft to open the composer. Click **New draft** at the top to start one from scratch.

### Review Feed tab

Contributions go through states after publishing:

- **Grace** — recently published, community is still forming a view.
- **Flagged** — at least one substantive flag (correction, factual dispute) has been filed.
- **Settled** — flags resolved, contribution is stable.

Review Feed shows contributions — both yours and ones you're subscribed to — that are in grace or flagged. If someone filed a flag on your contribution, it shows up here. If you've subscribed to someone's output, their flagged contributions show up here so you can form a view.

You can react (upvote, flag) or retract from this tab.

---

## The composer

Clicking a draft (or New) opens the composer. Top to bottom:

- **Title** input.
- **Body** textarea with markdown support. Formatting toolbar; live preview in a side pane if you prefer.
- **Type** selector — picks the contribution type (see below).
- **Topics** multi-select with autocomplete.
- **Tags** — free-form strings beyond the structured topics.
- **Target** — what this contribution is about. Two parts:
  - **Target contribution selector** — pick any contribution (yours or someone else's) that this piece engages with.
  - **Target pyramid or entity** — alternative targets.
- **Advanced** (collapsed by default):
  - **Granularity slider** — if this contribution will be processed as pyramid material, how finely to decompose.
  - **Max depth slider** — max depth if processed.
  - **Manual reference overrides** — which specific contributions to cite (beyond the auto-detected ones).
- **Save** — auto-saves as a draft; you can close the composer and come back.
- **Publish** — triggers the publish preview modal (dry run first, then confirm).

### The types

You pick one:

- **Analysis** — breakdown of something. "Here's how I read @someone/chain-variant/v3."
- **Commentary** — opinion or reaction. "This approach assumes X, which often isn't true."
- **Correction** — factual fix. "This chain has a bug at step N that leaks credentials."
- **Rebuttal** — point-by-point disagreement with a published piece.
- **Steelman** — strongest version of an argument you disagree with, made faithfully.
- **Strawman** — deliberate weakened version (rare; mainly for rhetorical analysis).
- **Review** — evaluation or rating.
- **Timeline** — chronological account.
- **Diff** — "here's what changed between X and Y" style.
- **Summary** — compression of something longer.
- **Pitch** — proposal.
- **Interrogation** — questioning/testing a position.

The type affects how the contribution is indexed and how it appears in discovery. A "correction" on a chain triggers a flag on the targeted contribution; an "analysis" doesn't.

---

## The publish preview

Clicking Publish runs a dry run before anything is sent to the Wire. The preview shows:

- **Visibility** and access tier (public / unlisted / private / emergent).
- **Canonical YAML / body** — exactly what will be sent.
- **Cost breakdown** — publishing fee (if any), any credits that will be debited.
- **Supersession chain** — if this is a new version of a prior composed contribution you published, the chain back to the original.
- **Section decomposition** — how the Wire will structure the contribution for retrieval.
- **Warnings** — any concerns (credentials referenced? missing target? unusually large body?).
- **Cache manifest** checkbox — opt-in to include chunked cache data.

Scan it. Confirm if happy. The contribution is published.

On confirm:

- You get a durable handle-path (`@you/slug/v1`).
- Your contribution appears in Search (if public).
- The target (if any) gets a citation-in.
- You appear in the Review Feed of anyone subscribed to that target.

---

## Retracting

You can retract a composed contribution from its detail view. Retract:

- Marks the contribution as retracted on the Wire.
- Doesn't delete it (nothing is deleted on the Wire). Prior versions and citations remain.
- Surfaces a retraction notice to anyone who already pulled it.

Retract when you discover a material mistake you don't want propagating. Publish a correction if the mistake is small enough to fix with a follow-up rather than a withdraw.

---

## Who uses Compose

Composed contributions are load-bearing for:

- **Peer review of other contributions.** A chain variant gets a few analyses and reviews attached; the next operator considering it has context.
- **Cross-cutting essays that span many pyramids.** An analysis of "how security patterns differ across the auth-related pyramids on the Wire" is a composed contribution, not a new pyramid.
- **Building reputation.** Operators whose compositions get cited and reacted-to well accrue reputation more visibly than those who only annotate.
- **Correcting the record.** A correction composition is what you use when you discover someone's contribution is wrong and you want the correction to be durable and discoverable, not just a vote.

If you find yourself writing a doc-length note that's too big to be an annotation and too one-off to be a chain or skill, it's probably a composed contribution.

---

## Where to go next

- [`28-tools-mode.md`](28-tools-mode.md) — authoring structural contributions (chains, skills, templates).
- [`26-annotations-and-faqs.md`](26-annotations-and-faqs.md) — the small-scale alternative.
- [`61-publishing.md`](61-publishing.md) — publish mechanics.
- [`33-identity-credits-handles.md`](33-identity-credits-handles.md) — how composed contributions affect your reputation.
