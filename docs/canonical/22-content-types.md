# Content types

When you create a pyramid, you pick a **content type**. The choice drives which prompts are used and what kind of evidence gets extracted. Content types are not categories — they are different preset apex questions with different frozen decomposition strategies.

> **Note on current state.** As of 2026-04, all content types route through the canonical `question-pipeline` chain (`chains/defaults/question.yaml`). The per-content-type default chains (`code.yaml`, `document.yaml`, `conversation.yaml`) are deprecated but still load correctly if explicitly assigned for parity testing. Content-type-specific behavior today is driven primarily by the prompts selected via `instruction_map` keys (e.g. `content_type:conversation`) rather than by a separate chain per content type.

The four built-in types cover the common cases. You can author your own (see [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md) for how chain variants work).

---

## Code

Use `code` for a folder of source files (or a whole repo).

**The preset apex question:** *"What is this codebase and how is it organized?"*

**What the decomposition looks for:**

- Modules and their responsibilities.
- Architectural decisions (how layers communicate, what invariants are enforced where).
- Cross-cutting concerns (auth, logging, caching, error handling).
- Conventions (naming, idioms, formatting rules the code seems to follow).
- Hot spots (modules with dense inbound references).
- Dead or rarely-used code.

**Strong fit:**

- A codebase you want to onboard onto, own, or audit.
- A codebase you already know that you want to share with a collaborator via a published pyramid.
- A codebase whose architecture has drifted from its docs.

**Weak fit:**

- A monorepo of many unrelated projects — use `vine` instead.
- A single file or a tiny folder — too small to benefit from the scaffolding.
- A project whose "source" is mostly generated (the pyramid can see generation results but not generation intent, which is usually what you care about).

**Tips:**

- Run `code` with the default question first, then ask follow-up questions. Targeted follow-ups get better answers than a narrow first question, because the L0 evidence from the broad first pass is broader.
- If your language has generated files (e.g. protobuf output, build artifacts), make sure `.gitignore` covers them or the chunker will waste budget on them.

## Document

Use `document` for PDFs, markdown, plain text, and similar prose.

**The preset apex question:** *"What is this material about and what does it claim?"*

**What the decomposition looks for:**

- Key terms and their definitions (glossary).
- Claims and their evidence.
- Structural sections (chapters, subsections, digressions).
- Entities (people, organizations, systems mentioned).
- Corrections the pyramid made to the source (places where the source contradicts itself, or mis-states something — logged in the corrections output).

**Strong fit:**

- A stack of PDFs from a research area you're coming up to speed on.
- Meeting notes or design docs accumulated over months.
- A book, a paper, a long-form article you need to interrogate.
- A specification you need to reason about.

**Weak fit:**

- Very short documents (a single memo) — overhead exceeds benefit.
- Documents with lots of critical visual content (diagrams, screenshots) — the pyramid sees the alt-text and caption, not the image itself.
- Highly tabular data — use a different approach.

**Tips:**

- Group related documents into one folder before building. A pyramid per document is possible but tends to be thin.
- If the documents have a natural hierarchy (folder per author, folder per year), consider `vine` instead — you get per-section pyramids plus a cross-cutting synthesis.

## Conversation

Use `conversation` for JSONL files of structured chat transcripts — Claude Code conversations, exported ChatGPT transcripts, Slack exports processed into JSONL, etc.

**The preset apex question:** *"What happened in these conversations and what are the important threads?"*

**What the decomposition looks for:**

- **Decisions** — moments where something was chosen, with the alternatives considered and the reason.
- **Entities** — people, projects, artifacts mentioned, resolved across conversations (the same name often refers to different things in different threads; the pyramid does the resolution).
- **Episodic threads** — sequences of messages that form a coherent arc (one problem getting solved across many messages).
- **ERAs** — Event-Response-Action triplets that are the common structure of technical conversations.
- **Speaker-specific views** — what did *this speaker* say, decide, or change their mind about.

**Strong fit:**

- Claude Code conversation logs from a long-running project.
- Meeting transcripts (auto-transcribed; quality varies with transcription).
- Long-running design discussions in chat.
- Post-incident threads where you want to reconstruct decisions.

**Weak fit:**

- Single conversations — the value comes from cross-conversation entity resolution and thread continuity.
- Conversations with lots of embedded media that's load-bearing.

**Tips:**

- For Claude Code logs, use the preset for CC directly — Agent Wire Node knows the JSONL shape and auto-discovers the right directories.
- Conversations benefit disproportionately from DADBEAR running over time, because new conversations can extend existing threads or supersede existing decisions.

## Vine

Use `vine` for a folder of folders — typically a directory where each subfolder is its own coherent pyramid candidate.

**The preset apex question:** *"What is this collection and how do its parts relate?"*

**What a vine build does:**

1. Scans the root directory.
2. For each subfolder, classifies whether it's a pyramid candidate (mostly homogeneous content, above a minimum file count) or a vine candidate (has its own subfolders).
3. Creates a bedrock pyramid per pyramid candidate subfolder, using the right content type for each.
4. Creates a nested vine per vine-candidate subfolder.
5. Synthesizes a cross-cutting pyramid at the root, with evidence links to each bedrock.

A vine pyramid is a pyramid whose children are pyramids. The recursion is exact — vines of vines of vines if your directory is that deep.

**Strong fit:**

- A monorepo with multiple distinct projects.
- A `/docs` folder with multiple books-worth of material organized by topic.
- A research archive with a folder per topic or year.
- A team's shared drive-equivalent — lots of coherent sub-collections.

**Weak fit:**

- A flat folder with no meaningful subdivision — just use `code` or `document`.
- A folder where everything is tightly coupled and the divisions are artificial — you'll get a vine, but the cross-cutting synthesis will be thin.

**Tips:**

- Vines rely on heuristics (minimum file count, homogeneity threshold) to decide what's a pyramid vs what's included as loose files. Defaults are sensible but configurable; see [`46-config-contributions.md`](46-config-contributions.md).
- When you add a new subfolder later, DADBEAR's Pipeline B loop notices and folds it into the vine. You don't have to rebuild.
- Vines can take a while on first build — they are building many pyramids sequentially (or in parallel, depending on concurrency). Large vines are multi-hour builds; plan accordingly.

---

## Question pyramids (a fifth content type, kind of)

A **question pyramid** is what you get when you click **Ask question** on an existing pyramid. It doesn't take source files directly — it takes an apex question and one or more source pyramid slugs.

You don't pick `question` as a content type from the Add Workspace wizard; you create a question pyramid by asking a question. But it's worth knowing it's the same fundamental shape — just sourced from other pyramids' L0/L1 instead of from files.

See [`24-asking-questions.md`](24-asking-questions.md).

---

## Which content type should I use?

Decision tree:

1. **Is it one folder of one kind of material?**
   - Code → `code`.
   - Prose → `document`.
   - JSONL transcripts → `conversation`.
2. **Is it a folder of folders, each its own coherent thing?** → `vine`.
3. **Are you asking a question about existing pyramids?** → question pyramid (via Ask question, not Add Workspace).

If in doubt, start with `code` or `document`. You can always archive and rebuild as a different type — no data is lost, and DADBEAR's evidence is reusable.

## Authoring your own content type

The content types above are shipped chains. Authoring your own is a matter of:

1. Copy the relevant default chain (e.g. `chains/defaults/code.yaml`) to `chains/variants/my-code.yaml`.
2. Edit it — change primitives, iteration modes, prompts.
3. Either assign it per-slug (in the detail drawer), or publish it as a contribution and let any of your pyramids use it.

See [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md) and [`43-assembling-action-chains.md`](43-assembling-action-chains.md).

---

## Where to go next

- [`21-building-your-first-pyramid.md`](21-building-your-first-pyramid.md) — walkthrough with content type in context.
- [`24-asking-questions.md`](24-asking-questions.md) — question pyramids.
- [`41-editing-chain-yamls.md`](41-editing-chain-yamls.md) — when defaults aren't what you want.
