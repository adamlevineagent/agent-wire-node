# The Self-Describing Filesystem

**Status:** Vision — April 2026, post-current-initiative target architecture
**Depends on:** Pyramid folders + model routing + full-pipeline observability initiative completing (current 17-phase plan)
**Authors:** Adam Levine, Claude (session partner)
**Relationship to current plan:** NOT a change to the current 17-phase initiative. This is the architecture the current work makes possible and which the NEXT major initiative should build toward.

**Naming note:** The platform's formal name is **Agent Wire** (canonical domain: `agent-wire.com`). This document uses "Wire" and "the Wire" as colloquial shorthand consistent with other internal engineering docs. In external/legal/marketing contexts, "Agent Wire" is the authoritative form. See `GoodNewsEveryone/docs/wire-ip-and-licensing-strategy.md` for the trademark and naming conventions.

---

## The Thesis

Once you have a local pyramid that's alive and maintained, **the filesystem stops being "where files live" and becomes "where understanding accumulates."** Files are still there — they're just no longer the primary interface. The pyramid is.

If this thesis holds, the storage architecture that makes the most sense is one where:

1. Every folder describes itself — what's in it, what's understood about it, what the system knows and doesn't know.
2. Understanding travels with the files it describes. Move a folder, its understanding moves. Share a folder, its understanding shares. Archive a folder, its understanding archives.
3. The filesystem IS the pyramid, not "has a pyramid on top of it as a separate service."

This document is the vision for that target architecture. None of it is in the current 17-phase plan. The 17-phase plan is what makes reaching this feasible.

---

## Where We Are Today

The current pyramid architecture:

- All pyramid state lives in a central SQLite database at `~/Library/Application Support/wire-node/pyramid.db`
- Pyramid nodes have IDs like `L3-S000` that are meaningful only within that database
- Evidence links, supersession chains, build history, cost logs are all central tables
- Files on disk are referenced by absolute path; the pyramid's knowledge of them is divorced from the files themselves

This works and is the right architecture for the current scope. Its failure modes are specific:

- **Portability broken.** Move a folder, the path references become wrong.
- **Sharing is all-or-nothing.** You share the whole pyramid database or none of it.
- **Inspection requires the app.** You can't `cat` a node or `grep` a supersession chain. You need SQL or the CLI.
- **No git integration.** Your code has version control; your understanding of your code doesn't.
- **Obsolescence risk.** SQLite databases are readable as long as Wire Node exists. Plain files are readable forever.
- **Silent coverage gaps.** If a folder has 100 files and 30 are pyramidable, the other 70 are invisible. The system doesn't acknowledge their existence.

---

## Folder Nodes — The Immediate Bridge

The first step toward the self-describing filesystem is additive and fits inside the current architecture: **introduce `folder` as a new pyramid content type that represents folders directly, with a filemap payload describing everything in the folder.**

### What a folder node contains

```yaml
# Folder node: /Users/adam/AI Project Files/agent-wire-node/src-tauri/src/
id: F-L0-042
depth: 0                                    # folders live as L0 bedrock nodes inside a parent vine
content_type: folder
headline: "Wire Node Rust source (src-tauri/src/)"
distilled: |
  Core Rust implementation of Wire Node's pyramid engine, Tauri commands,
  and HTTP surface. Contains the pyramid module (engine, builders, chains,
  stale engine, DADBEAR, vine composition), the auth module, the main.rs
  entry point, and the vocabulary system.

filemap:
  covered:
    - path: "main.rs"
      content_type: code
      pyramid_node_id: C-L0-001
      last_ingested: "2026-04-10T08:15:00Z"
      source_hash: "sha256:abc123..."
    - path: "pyramid/chain_executor.rs"
      content_type: code
      pyramid_node_id: C-L0-023
      last_ingested: "2026-04-10T08:15:00Z"
      source_hash: "sha256:def456..."
    # ... more covered entries ...

  uncovered:
    - path: "target/"
      reason: excluded_by_pattern
      pattern: "target/"
    - path: "screenshot-auth-flow.png"
      reason: unsupported_content_type
      type: "image/png"
      size_bytes: 421_330
      mtime: "2026-03-01T14:22:00Z"
    - path: "build.log"
      reason: excluded_by_size
      size_bytes: 842_117
      mtime: "2026-04-09T22:30:00Z"
    - path: "Cargo.lock"
      reason: excluded_by_pattern
      pattern: "*.lock"

  deleted:
    - path: "pyramid/legacy_stale.rs"
      last_pyramid_node_id: C-L0-019
      deleted_at: "2026-03-22T11:00:00Z"
      tombstone_reason: "removed during chain binding v2.6 refactor"

coverage_ratio: 0.78                        # 78% of seen files are pyramidable and ingested
parent_folder_id: F-L0-041                  # the containing folder's node
child_folder_ids:
  - F-L0-043                                # pyramid/
  - F-L0-044                                # public_html/
```

### The five categories of `uncovered`

A bounded, closed enum. No "other." Every uncovered file falls into exactly one:

1. **`excluded_by_pattern`** — matched a `.gitignore`, `.pyramidignore`, or folder_ingestion_heuristics ignore rule.
2. **`excluded_by_size`** — exceeds the configured max file size threshold.
3. **`excluded_by_type`** — known binary/system file types we never want to ingest (`*.bin`, `*.exe`, `*.dylib`, `.DS_Store`).
4. **`unsupported_content_type`** — we could theoretically ingest this but we don't have an extractor for this type yet (images without a vision extractor, audio without transcription, spreadsheets without a structured parser).
5. **`failed_extraction`** — we tried, the extractor ran, and it failed. The reason field captures the failure.

The `unsupported_content_type` category is the one that matters most strategically. It's the TODO list for expanding coverage. When you add a new extractor, you scan folder nodes for `uncovered` entries with matching `unsupported_content_type` and incrementally ingest them.

The `failed_extraction` category is the one that fixes a current silent failure mode. Today a failed extraction is a log line that scrolls away. With folder nodes, it's a persistent record that surfaces in the oversight UI.

The `deleted` array is a tombstone log. If a file is deleted, the folder node retains a record of what was there and which pyramid node described it. You can answer "what did I delete last week" without digging through git history.

### Why this is additive and non-disruptive

- Folder nodes are a new `content_type` value, slotted into the existing pyramid architecture
- They can live in the existing `pyramid_nodes` SQLite table with a new content_type discriminator
- They don't change how other nodes work — they're a parallel type that coexists with code/document/conversation/question nodes
- DADBEAR already has scanners that know how to walk directories; those scanners become the ingesters for folder nodes
- Phase 17 (folder ingestion) in the current plan is the natural place to introduce folder nodes as its canonical output — each ingested folder becomes a folder node whose filemap describes what was found

This means **folder nodes can land as part of Phase 17 without disrupting the rest of the 17-phase plan.** They don't require the full `.understanding/`-per-folder migration; they're a new node type in the existing central store.

### What they buy you immediately

1. **Visibility of absence.** Users can see what the system isn't covering.
2. **Migration guidance.** Adding a new extractor becomes a targeted operation against `unsupported_content_type` entries.
3. **Tombstone history.** Deleted files leave a record.
4. **Coverage metrics.** Per-folder coverage ratio enables "show me areas of my filesystem that are well-understood vs mostly invisible."
5. **Hierarchical navigation.** Folder nodes nest into vines; the filesystem tree becomes queryable as a pyramid hierarchy.
6. **Whole-disk feasibility.** Even if most files aren't pyramidable today, you can still create folder nodes for every folder and get a structural index of the filesystem. The pyramid grows incrementally as extractors expand.

---

## `.understanding/`-per-folder — The Target Storage Architecture

Folder nodes are a data structure innovation, still stored centrally. The next architectural step is **storage co-location**: each folder contains its own understanding in a hidden `.understanding/` subdirectory.

### What a folder looks like

```
agent-wire-node/
├── src-tauri/
├── src/
├── chains/
├── docs/
├── package.json
├── Cargo.toml
└── .understanding/
    ├── folder.md                          # the folder node itself (canonical, git-friendly)
    ├── nodes/
    │   ├── F-L0-042/                      # folder node with version history
    │   │   ├── v1.md
    │   │   ├── v2.md
    │   │   ├── v3.md
    │   │   ├── current → v3.md            # symlink
    │   │   └── notes/
    │   │       ├── v1-to-v2.md            # the refinement note that drove v2
    │   │       └── v2-to-v3.md
    │   ├── C-L0-023/                      # a code bedrock node
    │   │   ├── v1.md
    │   │   └── current → v1.md
    │   ...
    ├── edges/
    │   └── web-edges.jsonl                # web edges for this folder's nodes
    ├── evidence/
    │   └── links.jsonl                    # evidence links
    ├── configs/
    │   ├── evidence_policy/               # per-folder config contributions
    │   ├── dadbear_policy/
    │   └── ...
    ├── conversations/                     # Claude Code conversations (see "Conversation Co-Location")
    │   └── 2026-04-09-morning.jsonl
    └── cache/
        └── llm-outputs/                   # content-addressable step cache
```

### What this solves

Every problem listed in "Where We Are Today":

- **Portability.** Move the folder, `.understanding/` goes with it. rsync it, archive it, AirDrop it — understanding travels as normal filesystem content.
- **Granular sharing.** Share one subfolder, share its understanding. Keep another private, keep its understanding private. Filesystem permissions do the work.
- **Inspection with standard tools.** `ls .understanding/nodes/`, `cat .understanding/nodes/F-L0-042/current`, `diff v1.md v3.md`. No SQL, no CLI.
- **Git integration is free.** Check `.understanding/` into git. Your understanding gets versioned alongside your code. `git log`, `git blame`, `git diff`, `git bisect` all work on understanding the same way they work on source.
- **Obsolescence resistance.** Plain markdown and JSONL files are readable in 2050 without any specific tool.
- **Format transparency.** Anyone can inspect how the pyramid represents knowledge. Contributors can reason about it without reverse-engineering SQLite schemas.

### Supersession as versioned directories

The supersession chain pattern makes version history legible as directory structure:

```
.understanding/nodes/L3-S000/
├── v1.md                                  # original synthesis
├── v2.md                                  # refined after "less cloud dependency" note
├── v3.md                                  # refined after "demand-driven maintenance" note
├── current → v3.md                        # symlink to active version
└── notes/
    ├── v1-to-v2.md                        # the note that drove v2
    └── v2-to-v3.md                        # the note that drove v3
```

You can `cd` into any node directory and inspect its history. Any version is readable by any tool. The notes that drove each transition are plain markdown files. Six months later, someone (human or agent) can read the version chain and understand not just what the node says but how it got there.

### The SQLite cache becomes a derivative index

`.understanding/` files are canonical. The central SQLite database becomes a rebuildable query cache — derived from the canonical files, not the source of truth. Queries hit SQLite for speed; writes go to the files; the cache gets invalidated and rebuilt when files change.

This is the standard "source of truth vs fast lookup" split. The files are canonical; SQLite is a pure optimization.

### What's hard about this

**Performance.** Filesystem walks over `.understanding/` in a large tree are slow. The SQLite cache handles query performance. The cache is rebuildable at any time from the canonical files.

**Concurrency.** Multiple processes writing to the same `.understanding/` need coordination. Use write-to-temp-then-rename, lockfiles in `.understanding/.lock/`, or git-like conflict resolution for multi-machine cases.

**Schema evolution.** When the node format changes, existing `.understanding/` folders need migration. Include a schema version in each file; migration pass on app upgrade. Standard file-format evolution story.

**Binary files.** Some content (embeddings, images, compact indices) doesn't fit well in markdown/JSON. Use binary files inside `.understanding/` with well-documented formats. The directory structure is the index.

**Cross-machine sync.** Git handles this well. rsync and iCloud don't. Users who need multi-machine sync should use git on `.understanding/` specifically. This is a known limitation.

**Discovery.** How do you find all `.understanding/` directories in a large filesystem? A registry of top-level directories that are pyramid roots, plus walking from those roots. Same mechanism a build system uses for finding project roots.

---

## Historical Framing

The ideas in this document have been around for a long time and have never worked. Understanding why helps clarify why they might work now.

**Memex (Vannevar Bush, 1945).** A device that would compile a trail of associations through all your personal documents. Imagined as mechanical microfilm with mechanical linkages. Never built because the compute and storage weren't there.

**Xanadu (Ted Nelson, 1960s).** Everything-links-to-everything, transclusion, stable addressable content, versioning of documents. Sketched in detail, implemented in fragments, never shipped at scale. The idea was right; the infrastructure to realize it didn't exist.

**Lifestreams (David Gelernter, 1990s).** All your personal data as a chronological stream, queryable as a unified corpus. Brilliant conceptually but text-only, no semantic layer, no way to extract meaning from documents automatically.

**WinFS (Microsoft, early 2000s).** A relational filesystem where files had structured metadata and could be queried with SQL. Killed because the engineering was too hard and the UX didn't materialize.

**Apple Spotlight / macOS Core Services.** What we actually got: an inverted-text index with some metadata awareness. Works for "find this filename" and "grep this content" but not for "what did I write about X" or "show me everything related to Y."

**Microsoft Recall, Google Drive intelligent search, Apple Intelligence.** Current attempts, all suffering from the same structural limitation: they're cloud-dependent (so privacy is compromised), they're centralized (so they serve the vendor's incentives, not yours), and they're closed (so your knowledge lives in a silo you can't move).

**The reason a local pyramid-backed self-describing filesystem might work now is specific:** for the first time in history, we have inference models small enough, fast enough, and cheap enough to run semantic extraction over personal-scale data on local hardware. A 27B parameter Gemma model on an M3 can extract useful understanding from a document in seconds for zero dollars. Ten years ago that cost $50 and required a datacenter. Five years ago it cost $5 and required a cloud API. Today it's $0 and runs on a laptop while the user sleeps.

The ideas were waiting for the compute. The compute arrived.

---

## Whole-Disk Pyramid — The Scale Target

Once the architecture supports folder nodes + `.understanding/`-per-folder, the path to whole-disk coverage becomes tractable:

- Every folder gets a folder node, whether or not its files are yet pyramidable
- Folder nodes nest hierarchically — the root directory is a vine containing subfolder vines containing bedrock pyramids at the leaves
- Coverage grows organically as new extractors are added (photos, audio, email, messages, calendar, browser history, spreadsheets, binary metadata)
- Each new extractor targets `unsupported_content_type` entries in existing folder nodes; no filesystem re-walk needed

### Content types required for whole-disk coverage

The current pyramid handles code, document, conversation, question. Whole-disk expansion requires:

| Content type | Extractor | Priority |
|---|---|---|
| Code | Already built | Done |
| Document (markdown, PDF, DOCX) | Already built; PDF layout parsing needed | High |
| Conversation (chat logs, JSONL) | Already built | Done |
| Email (mbox, maildir) | New — existing mail parser + LLM extraction | High |
| Calendar events (ics) | New — small, structured | Medium |
| Messages (iMessage, SMS) | New — privacy-sensitive, local-only | High |
| Browser history | New — small, structured | Medium |
| Images | New — vision model, CLIP embeddings, caption generation | High |
| Video | New — scene detection + transcript + key frame extraction | Low |
| Audio | New — transcription (Whisper) + summarization | Medium |
| Spreadsheets | New — structural extraction + cell-value understanding | Medium |
| Databases (SQLite files) | New — schema extraction + query-friendly description | Low |
| Binary metadata | New — file type classification, metadata extraction | Low |

That's 10+ new content types. Each needs its own extraction primitive, its own quality bar, its own prompt skill, its own test suite. This is not a trivial extension — probably a 2-3x increase in pyramid surface area over the current code/document/conversation/question scope.

Most of these can reuse the same chain infrastructure (YAML-defined chains, the same executor, the same output cache). The work is per-content-type prompt skills and small amounts of Rust for the non-LLM extraction (file parsing, metadata reading, structural analysis).

### Cost honesty

**Bootstrap inference cost.** A modern Mac has maybe 300K-2M user-meaningful files. At current OpenRouter rates for fast extraction models, a full-disk bootstrap costs roughly $150-600 one-time on cloud inference. With Ollama-only, it's $0 in dollars but 3-10 days of continuous local inference.

**Ongoing maintenance cost.** DADBEAR + change-manifest updates on incremental file changes are cheap per-file. A user modifying 50 files/day costs a few cents/day on OpenRouter or a few minutes of local GPU.

**Compute-during-use friction.** This is the real concern. Background Ollama inference at 30W continuous produces fan noise, battery drain, and thermal throttling of other tasks. A MacBook running full-disk extraction in the background is a different device than one mostly idle. This is the friction point that determines whether users adopt it.

**Storage.** Folder nodes are small. Pyramid nodes are small. A million nodes is ~1 GB on disk. Not a problem on modern storage.

**Privacy.** Whole-disk coverage essentially requires Ollama (or equivalent local inference). Cloud APIs like OpenRouter mean exfiltrating file contents to a third party, which is unacceptable for photos, source code with secrets, private messages, etc. The self-describing filesystem only works as a local-compute story; the cloud option only exists for user-initiated selective operations.

---

## Conversation Co-Location

A specific application of the `.understanding/`-per-folder architecture to an immediate friction point:

Claude Code stores conversations in `~/.claude/projects/<encoded-path>/`, keyed by the encoded absolute path of the project the conversation was about. The conversation-to-code linkage exists on disk but in a separate directory tree. This is a historical artifact — Claude Code started as a CLI that needed a central place to find conversations.

In the target architecture, **conversations should live in `.understanding/conversations/` inside the project folder they're about.** Move the project folder, conversations move with it. Share the project via git, conversations are co-located and version-controlled. Archive the project, conversations archive along with the code they reference.

Phase 17's Claude Code auto-include feature (already in the current plan) is the bridge to this state: it discovers conversations in `~/.claude/projects/` via the encoded-path match and pulls them into the folder ingestion. Once the self-describing filesystem architecture is live, the auto-include feature can additionally COPY (or symlink) conversations into `.understanding/conversations/` in the target folder at ingest time, giving users the co-located experience even if Claude Code itself doesn't adopt the convention.

**Future:** propose (to Anthropic, or as an open-source plugin) that Claude Code optionally store conversations in a project-local `.understanding/conversations/` directory instead of the central location. At that point the bridge becomes unnecessary and conversations live natively with the code they describe.

This principle generalizes beyond Claude Code. Any tool that records human-agent interaction can benefit from co-locating its records with the files those interactions were about. Cursor conversations, GitHub Copilot chat logs, ChatGPT project sessions, Notion AI threads — all of these could live in `.understanding/conversations/` subdirectories in the relevant project folder, making the project folder a complete record of "what was built and how it was built."

---

## Relationship to the Current 17-Phase Plan

**The current plan does not build this architecture.** The current plan builds:

- Unified config contributions (Phase 4)
- Canonical Wire Native Documents mirror (Phase 5)
- LLM output cache (Phase 6)
- Cache warming on import (Phase 7)
- YAML-to-UI renderer + generative config (Phases 8-9)
- ToolsMode universal config surface (Phase 10)
- Evidence triage + cost integrity + leak detection (Phases 11-12)
- Build viz + cross-pyramid observability (Phase 13)
- Wire discovery (Phase 14)
- Vine-of-vines + folder ingestion (Phases 16-17)

All of these stay in central SQLite. The current plan gets Wire Node to a shippable, Wire-native configuration surface with correct cost integrity and functional folder ingestion. That's the foundation. Shipping it is the prerequisite for any self-describing filesystem work — because without it, you don't have users who care about the target architecture.

**What the current plan can do to enable this future:**

**Phase 4 (config contributions) and Phase 7 (cache warming on import)** are the two places where forward compatibility with `.understanding/`-per-folder matters most. Both are about "what travels with a pyramid when it moves." The implementers of those phases should:

- Keep config data as serializable documents (YAML, JSON, markdown) from day one, even inside SQLite rows — so future migration to filesystem storage is "move the same documents to files," not "reshape the data structure"
- Design the cache manifest format as file-like (one manifest per node with clear fields), not as opaque SQL rows — so future migration exports cleanly to `.understanding/cache/` files
- Avoid baking in assumptions that "pyramid state = rows in a database" — write the code against traits/interfaces so the storage backend can change later

**Phase 17 (folder ingestion) is the natural place to introduce folder nodes** as the output of the folder walk. Each created pyramid gets a folder node at its root; each subfolder gets a child folder node; the resulting tree is a nested structure of folder nodes with bedrock pyramids at the covered leaves. This doesn't require `.understanding/`-per-folder storage — folder nodes can live in central SQLite like any other node — but it creates the data structure that later migrates cleanly.

If Phase 17 lands with folder nodes, a follow-up initiative can migrate central SQLite storage to `.understanding/`-per-folder without reshaping the data, only relocating it.

---

## What The Next Initiative Looks Like

After the 17-phase plan ships and the implementation has stabilized for a reasonable period (probably 1-3 months of real-world use), the next major initiative becomes:

**"Self-Describing Filesystem" — migrate from central SQLite to `.understanding/`-per-folder + expand content type coverage toward whole-disk.**

Rough phase outline (to be written properly when the time comes):

1. **Define the `.understanding/` directory layout spec.** File formats, naming conventions, version markers, inheritance rules.
2. **Build the read/write abstraction.** A trait-backed storage layer that can target either SQLite (current) or `.understanding/` (target), with a migration mode that reads from SQLite and writes to `.understanding/`.
3. **Rebuild the SQLite cache layer.** The cache becomes a pure derived index; writes go through to files first, then the cache is updated. Cache rebuilds from files are idempotent and fast.
4. **Migrate existing pyramids.** One-time operation per pyramid: read central state, write to `.understanding/`, verify, switch the storage backend. Old SQLite rows get archived, not deleted, for rollback safety.
5. **Git integration layer.** A `.gitattributes` template that sets up `.understanding/` for git-friendly diffs. Merge drivers for node files. Sync conflict resolution for the notes/versions directories.
6. **Expand content types.** Email, images, audio, messages, calendar, browser history, etc. Each gets its own extractor chain and skill contribution.
7. **Whole-disk scanner.** An opt-in mode that creates folder nodes for the entire user home directory (with sensible exclusions), then incrementally ingests content as extractors support it.
8. **Cross-device sync.** git-based or rsync-based sync of `.understanding/` directories for users who work across multiple machines.

Timeline estimate: 3-6 months after the current plan ships, assuming similar team size and pace.

---

## The Name Claim

Taken seriously, what we're describing has a specific name in the history of computing: **the Memex, finally.** Eighty years after Vannevar Bush described it, the compute arrived that makes it buildable.

Or from the other direction: **Plan 9's "everything is a file" principle extended to knowledge.** Your memory of a project is a file. The notes that drove each refinement are files. The evidence links are files. The version history is directories. Unix philosophy meets semantic compute.

Either framing is honest. Both are the same thing: personal knowledge infrastructure as a native property of the filesystem, not an app running on top of it.

---

## Open Questions

1. **Top-level exclusions for whole-disk mode.** Some directories should always be excluded (system files, caches, node_modules, build artifacts, mail stores the user doesn't want extracted). What's the canonical default exclusion list? How do users audit and override it?

2. **Git integration for multi-user `.understanding/`.** If two developers both refine the same node, merging is non-trivial. Use git merge drivers? Use a git hook that serializes refinements? Use CRDTs? Needs a design pass.

3. **Performance at scale.** A filesystem with 1 million nodes across 100K folders. SQLite cache works, but what's the cache rebuild time? Is incremental rebuild feasible? Benchmarks needed.

4. **Content type extractor maintenance.** Every new content type needs ongoing maintenance (prompt updates, model upgrades, format changes). At 15+ content types, the maintenance surface is significant. Who owns it? Is it a Wire-native contribution system where anyone can publish a content type extractor?

5. **Privacy vs observability.** Full-disk coverage means the pyramid knows a LOT about the user. What's the threat model if the laptop is stolen? What's the encryption story for `.understanding/` at rest? Disk encryption (FileVault) handles most cases but the pyramid's summaries may be more sensitive than the source files.

6. **Binary content storage.** Embeddings, cached LLM outputs, thumbnails. Where do these go in `.understanding/`? Git-LFS? Out-of-tree cache with file references? Needs a design pass.

---

## Cross-References

- `docs/plans/pyramid-folders-model-routing-full-pipeline-observability.md` — the current 17-phase plan that establishes the foundation
- `docs/specs/vine-of-vines-and-folder-ingestion.md` — where folder nodes would land as Phase 17 output
- `docs/specs/config-contribution-and-wire-sharing.md` — where forward-compat matters for config storage
- `docs/specs/llm-output-cache.md` — where forward-compat matters for cache manifest format
- `GoodNewsEveryone/docs/wire-strategy.md` — the big-picture Wire platform strategy this fits under
- `GoodNewsEveryone/docs/ownership-protocol.md` — the rent-to-own ownership model that governs how value flows through the platform
