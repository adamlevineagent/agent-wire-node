# Understanding (Pyramids mode)

Understanding is the mode where most of your work happens. It is the list, dashboard, and control panel for every pyramid on your node. From here you create new pyramids, watch them build, drill into them, configure DADBEAR, publish them, or retire them. This doc is the orientation; the subsequent mode docs go deep on each sub-area.

---

## What you see

Open Wire Node → click **Understanding** in the sidebar (the first item under YOUR WORLD). The main area shows four tabs:

- **Dashboard** — the list of all pyramids, filtered and sorted.
- **Grid** — a visual grid of pyramids, density-encoded so you can see at a glance which ones are rich and which are thin.
- **Builds** — a cross-pyramid timeline of every build, past and running.
- **Oversight** — DADBEAR oversight across all pyramids.

If you have no pyramids yet, the Dashboard is replaced with a wizard that walks you through creating one. After your first pyramid, the Dashboard is what appears by default.

## Dashboard tab

The Dashboard is a list of your pyramids, one row per slug. Each row shows:

- **Slug** — the pyramid's identifier (click to open the detail drawer).
- **Content type** — code, document, conversation, vine, or question.
- **Node count** — how many nodes the pyramid has. "empty" if it hasn't been built yet.
- **Publication status** — a colored dot. Green = published and current. Yellow = published but stale. Spinning = currently publishing. Hidden = unpublished.
- **Last built** — relative time since the most recent build completed.
- **Active build progress** — if a build is running, a live progress bar with the current step and elapsed time.

Above the list, filter controls: search by slug, filter by content type, filter by publication status. Sort by node count, last built, or creation order. You can also collapse the list by content type if you have many pyramids.

### Detail drawer

Clicking a pyramid row opens a detail drawer on the right. The drawer has:

- **Header** — slug, content type, summary counts.
- **Publication control** — access tier (public/unlisted/private/emergent), price, circles filter, publish button.
- **Absorption config** — how the pyramid responds to incoming questions from the Wire. Mode (open, rate-limited, daily cap), chain selector, rate limits.
- **Actions** — rebuild, ask a question (creates a derivative question pyramid), open DADBEAR panel, open FAQ directory, open vine viewer, open in Vibesmithy, delete.
- **Metadata** — node count, max depth, created/built timestamps, list of referenced slugs.

The detail drawer is where you do almost everything on a specific pyramid. Most other operations (asking a question, publishing, configuring absorption) are shortcuts to specific sections of the drawer.

## Grid tab

The Grid is a visual representation of all your pyramids as cards in a density-aware grid. Each card is colored by content type, sized by node count, and shows the slug. Click a card to open the pyramid in a dedicated **Pyramid Surface** window (the full visualization — see [`23-pyramid-surface.md`](23-pyramid-surface.md)).

This view is most useful when you have many pyramids and want a scannable overview rather than a sortable list.

## Builds tab

The Builds tab is a timeline of every build that has ever run on your node, across all pyramids. Each row is a build:

- **Pyramid slug** — which pyramid was built.
- **Status** — running, complete, failed, cancelled.
- **Progress bar** — live for running builds.
- **Elapsed time** and **node counts**.
- **Actions** — cancel a running build, retry a failed one, reroll specific nodes.

If multiple pyramids are building concurrently (which is allowed and common), you see them here as concurrent rows. The per-pyramid detail drawer shows only its own builds; the Builds tab shows the whole fleet.

## Oversight tab

Oversight is DADBEAR's view across all pyramids. This is where you see:

- Which pyramids have pending staleness work.
- Which have tripped a breaker (too much stale at once — needs manual attention).
- Cost spend by pyramid, by source, by operation type.
- Orphan broadcasts (LLM responses that arrived without a matching in-flight request — usually benign, sometimes indicates a bug).

Each pyramid appears as a card with status, pipeline counts, cost, and pause/resume controls. You can dive into any pyramid's activity log from here.

See [`25-dadbear-oversight.md`](25-dadbear-oversight.md) for how to read and act on this.

---

## Creating a new pyramid

The **Add Workspace** button is how you create. Clicking it opens a wizard:

1. **Pick a directory.** File picker. Select the folder you want to build over. The folder must be readable and must contain files.
2. **Pick content type.** The wizard scans the folder and suggests one (code, document, conversation, vine). You can override. If you pick `conversation`, the wizard asks for a preset (Claude Code, chat.openai.com dumps, generic JSONL). If you pick `vine`, it asks for a vine root directory.
3. **Configure absorption and rate limits.** How eager to accept incoming questions if this pyramid ends up published.
4. **Optionally ask a question.** If you leave this blank, the default preset question for the content type is used ("What is this codebase and how is it organized?", etc.). If you provide a question, the build is shaped by your question from the start.
5. **Preview.** The wizard shows the ingestion plan: operations it will perform, file counts, any errors (unreadable files, unsupported formats, etc.).
6. **Confirm and build.** The build starts; the Pyramid Surface opens live.

You can also create a pyramid programmatically via the HTTP API or `pyramid-cli`, but most users use the UI.

See [`21-building-your-first-pyramid.md`](21-building-your-first-pyramid.md) for a walked-through example.

## Content types

The four built-in content types handle the common cases:

- **Code** — source code folders. Optimized for extracting architecture, module relationships, decisions, and flows.
- **Document** — PDFs, markdown, text files. Optimized for extracting terms, entities, claims, and structural summaries.
- **Conversation** — JSONL chat transcripts. Optimized for extracting decisions, threads, entity resolution, and speaker-specific views.
- **Vine** — folders-of-folders. Builds a vine pyramid over the containing directory, with per-subfolder bedrock pyramids underneath.

See [`22-content-types.md`](22-content-types.md) for what each content type actually does and how they differ.

## Asking questions

A question against an existing pyramid creates a derivative **question pyramid**. The question can cite multiple source pyramids. The derivative is itself queryable, and the next question you ask can cite *it* — questions chain.

From any pyramid's detail drawer, click **Ask question**. Enter the question, give it a slug, confirm. See [`24-asking-questions.md`](24-asking-questions.md).

## Viewing a pyramid

Clicking into a built pyramid opens it in the Pyramid Surface — the live visualization. You can drill nodes, hover for tooltips, click to open a node inspector with the full prompt and response, search, and toggle overlays (structure, web edges, staleness, provenance, build progress).

See [`23-pyramid-surface.md`](23-pyramid-surface.md).

## Publishing a pyramid

In the detail drawer, the publication control lets you:

- Choose an access tier (public, unlisted, private, emergent).
- Set a price (for emergent).
- Restrict to specific circles (for private).
- Click **Publish**.

The publish preview runs a dry-run that shows what will be sent, how much it costs, and any warnings (e.g. credentials referenced in a chain variant). You confirm, and the pyramid gets a Wire handle-path and appears in the Wire's search index (if public).

See [`61-publishing.md`](61-publishing.md).

## Retiring a pyramid

Pyramids are never hard-deleted — they're archived. The detail drawer has an **Archive** action. Archiving removes the pyramid from the active Dashboard and stops DADBEAR from running on it, but preserves all nodes, history, and annotations. You can unarchive at any time.

If you published the pyramid, archiving it locally does not automatically retract it from the Wire — you retract separately through the publication control if you want to.

---

## Keyboard shortcuts (in Understanding mode)

- `/` — focus the search box.
- `esc` — close detail drawer or modal.
- Arrow keys — navigate the list.
- `enter` — open the highlighted pyramid's detail drawer.

(Full keyboard map in [`Z1-quick-reference.md`](Z1-quick-reference.md).)

---

## Where to go next

- [`21-building-your-first-pyramid.md`](21-building-your-first-pyramid.md) — walkthrough.
- [`22-content-types.md`](22-content-types.md) — pick the right content type.
- [`23-pyramid-surface.md`](23-pyramid-surface.md) — drill a pyramid once it's built.
- [`24-asking-questions.md`](24-asking-questions.md) — derivative question pyramids.
- [`25-dadbear-oversight.md`](25-dadbear-oversight.md) — keep pyramids current.
