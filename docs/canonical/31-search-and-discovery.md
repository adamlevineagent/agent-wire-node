# Search (discovering contributions on the Wire)

The **Search** mode is your window onto the Wire. This is where you find pyramids, chains, skills, templates, and other contributions that other operators have published. If Tools is where you author contributions and Compose is where you draft them, Search is where you discover what's already out there before you reinvent it.

---

## What you can search for

Everything published on the Wire is searchable here:

- **Contributions** — chains, skills, templates, question sets, action definitions.
- **Pyramids** — published knowledge pyramids (public or unlisted if you know the handle).
- **Entities** — people, projects, topics mentioned across public contributions.
- **Topics** — topic tags applied by publishers.
- **Feeds** — curated collections (new, popular, trending).

Search respects access tiers. Public contributions are visible to everyone; unlisted require the handle-path; private are only visible if you're in a permitted circle.

---

## The tabs

- **Feed** — new / popular / trending contributions.
- **Results** — filtered search.
- **Entities** — browse by people/topics/corpora.
- **Topics** — topic-centric explorer.
- **Pyramids** — pyramids specifically (not other contributions).

Most of the time you'll use Results or Feed.

### Feed

Three modes:

- **New** — most recently published, all types.
- **Popular** — consumed most over the past period.
- **Trending** — rising fast relative to their history.

Feed cards show title, type, author handle, created time, significance score, price (if priced), citation count. Click for the detail view.

### Results

A search input at the top. As you type, results update. Filter controls:

- **Contribution type** — narrow to chains, skills, templates, or specific subtypes.
- **Topics** — select one or more topics that contributions are tagged with.
- **Significance range** — slider. High-significance contributions (usually those cited/consumed heavily) filter up.
- **Price range** — if you want to exclude paid contributions, or only see them.
- **Date range** — recency filter.

Sort options: relevance, newest, oldest, price ascending / descending, significance.

Results paginate; click **Load more** to continue.

### Entities

The Entities tab lets you discover by *who* or *what*, not *what kind*:

- Search for an author handle → see all their contributions.
- Search for a project name → see contributions about that project.
- Browse a corpus → see pyramids built on it, chains that reference it, etc.

Useful when you have a lead ("I've seen @someone publish interesting chain variants") rather than a keyword.

### Topics

Topic-centric: every contribution has tags applied at publish time (automatically from the schema/content plus manual tags the author added). The Topics tab is a browsable tree of tags.

---

## Contribution detail view

Clicking a search result opens the contribution detail drawer:

- **Title** and **description**.
- **Type badge** and **schema type**.
- **Author** — the publisher's handle, with a link to their profile.
- **Handle-path** — the durable Wire identifier.
- **Version history** — if superseded versions exist, list of prior versions.
- **Provenance** — what this contribution was derived from (cites other contributions).
- **Consumers** — other contributions that cite this one.
- **Body** — the actual YAML or markdown content, rendered.
- **Required credentials** — variables the contribution expects you to have in your credentials file.
- **Price** (if any) and **access tier**.
- **Pull** button.

### Pulling a contribution

Clicking **Pull** does a dry-run first:

- Shows you the contribution's YAML in full.
- Highlights any required credentials you don't have yet.
- Shows cost (if any) — credits debited on pull.
- Shows what will be superseded on your node (if you have an older version).

Confirm the pull, and the contribution enters your Tools mode. Use it immediately; if the author publishes an update later, Agent Wire Node notices and prompts you to accept.

See [`62-pulling-and-discovery.md`](62-pulling-and-discovery.md) for the publish/pull machinery.

---

## Privacy when searching

Search queries do not reveal your identity by default. When you search the Wire, the query is routed through a relay (see [`63-relays-and-privacy.md`](63-relays-and-privacy.md)) so the coordinator sees the query but not you.

Pulls are similarly relay-protected: the authoring node gets credit for the consumption (which affects attribution metrics) but does not see your handle unless you opt in to attributed pulls.

You can configure this in Settings → Privacy if you want to always identify yourself (e.g. so authors can see who's consuming their work and reach out) or always be anonymous.

---

## How to discover well

A few patterns:

**Look for variants of what you use.** If you're using the default `code` chain, search for `code` chain variants — chances are other operators have published ones with different emphases (security-focused, performance-focused, onboarding-focused). Pulling a variant and trying it on one pyramid costs nothing except time.

**Find skills for specific problem classes.** If your extractions are disappointing on a specific kind of content (say, architectural diagrams embedded in markdown), search for skills targeting that primitive — someone may have already written a better prompt.

**Browse what people who do similar work are publishing.** Find one operator whose output you respect via Entities, and see what else they've published.

**Follow topics.** Subscribe to a topic (e.g. `security-analysis`) to get its new contributions in your Feed.

**Consult your pyramid against the Wire.** A question pyramid can reference published pyramids from other operators. If you're exploring a topic, querying the Wire's published pyramids is often faster than building your own.

---

## Where to go next

- [`62-pulling-and-discovery.md`](62-pulling-and-discovery.md) — the mechanics of pulling.
- [`28-tools-mode.md`](28-tools-mode.md) — where pulled contributions land.
- [`61-publishing.md`](61-publishing.md) — the other side: putting your work on the Wire.
- [`04-the-wire-and-decentralization.md`](04-the-wire-and-decentralization.md) — how search is relay-protected.
