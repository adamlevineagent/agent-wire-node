# 🔄 The Refine Cycle: Pyramid Folio Generator

## 1. Analyze (Surface vs. Underlying Need)

**Surface Request:** Add a CLI command that exports a pyramid as a readable document, with depth control, question overlays, and per-section timestamps.

**Underlying Need:** Bridge the impedance mismatch between agent-optimized knowledge graphs and human-readable briefing documents. This should:
- Give humans a **point-in-time snapshot** they can audit, share, print, or read offline
- Make the snapshot **citable** — every section traces back to a specific node at a specific version
- Allow **precision extraction** — not just "dump everything" but "give me exactly this slice, through this lens"
- Be a **first-class export**, not a CLI-only afterthought — it should be invocable from the GUI/Tauri shell too

### Key Findings from Research

The data model supports this well:
- **Pyramids carry:** `slug`, `content_type`, `node_count`, `max_depth`, `last_built_at`, `created_at`
- **Nodes carry:** `id`, `depth`, `headline`, `distilled`, `topics[]`, `corrections[]`, `decisions[]`, `terms[]`
- **DADBEAR carries:** `last_check_at`, `pending_mutations_by_layer`, staleness per-node via `stale-log`
- **Missing:** No per-node `updated_at` timestamp. The closest proxy is the `checked_at` from the stale-log. The Folio generator will need to derive "current as of" from either the pyramid's `last_built_at` or the most recent stale-log entry touching that node.
- **Question pyramid overlay mapping:** The `composed` view shows that question pyramids create `Q-L0-*` nodes that reference back to source pyramid nodes, giving us the explicit link graph needed for strict overlay filtering.

> [!IMPORTANT]
> Nodes do NOT currently carry individual `updated_at` or `version` fields. The build agent must either:
> (a) Add `updated_at` to the node schema (recommended — small migration), or
> (b) Derive it from the stale-log's `checked_at` for the most recent entry touching that `target_id`

---

## 2. Diverge (20 Ideas)

| # | Idea | Value | Effort |
|---|------|-------|--------|
| 1 | Simple tree-walk concatenation: apex → children → grandchildren, headers by depth | Medium — works but reads like a dictionary | Low |
| 2 | **Depth-controlled export** (`--depth N`): only emit nodes down to layer N | High — precision control over detail level | Low |
| 3 | **Question overlay filter** (`--overlay <slug>`): traverse only nodes that map to a question pyramid's decomposition | Very High — turns a sprawling pyramid into a focused briefing | Medium |
| 4 | **Multiple overlays** (`--overlay A --overlay B`): union or intersection of multiple question lenses | Very High — composable, like database views | Medium |
| 5 | Overlay mode selector (`--overlay-mode union|intersect`): control whether multiple overlays combine inclusively or exclusively | High — prevents ambiguity | Low |
| 6 | **Per-section provenance block**: render `node_id`, `current_as_of`, `pyramid_version` as a small metadata footer per section | Very High — makes every section citable and auditable | Medium |
| 7 | **Staleness warnings**: render `> [!WARNING]` alerts on sections sourced from stale nodes | High — immediately flags what's trustworthy | Low |
| 8 | Auto-generated Table of Contents from breadcrumbs | High — navigability for large folios | Low |
| 9 | **Dual surface: CLI + export API**: expose as both `pyramid folio <slug>` and a REST endpoint / Tauri command so the GUI can offer "Export as Folio" | Very High — not CLI-only, first-class feature | Medium |
| 10 | Glossary appendix: auto-collect all `terms[]` from included nodes and render as an alphabetized glossary at the end | High — immense readability boost | Low |
| 11 | Evidence appendix: collect all `evidence[]` citations and render as numbered endnotes | Medium — useful for academic/audit use | Medium |
| 12 | **Output format flag** (`--format md|html|pdf`): start with markdown, but allow HTML or PDF via pandoc | Medium — markdown is the real output; HTML/PDF is nice-to-have | Medium-High |
| 13 | Annotation inclusion (`--include-annotations`): optionally inline agent annotations as blockquotes | Medium — useful for debugging, noisy for reading | Low |
| 14 | Corrections section: if any included nodes have corrections, render them as a dedicated "Errata" section | High — transparency | Low |
| 15 | Cross-reference links: within the folio, make section headers linkable via markdown anchors so the TOC actually works | High — basic usability | Low |
| 16 | **Folio header block**: render a structured header with pyramid slug, content type, node count, depth range, generation timestamp, and DADBEAR status | Very High — the "cover page" | Low |
| 17 | Decision log appendix: collect all `decisions[]` from included nodes into a "Design Decisions" appendix | Medium — useful for architecture reviews | Low |
| 18 | Diff-since mode (`--since <timestamp>`): only include nodes that changed since a given date | Medium — useful for "what's new" briefings | Medium |
| 19 | Side-by-side overlay comparison: generate two columns showing how the same pyramid reads through two different question lenses | Low — cool but complex formatting | High |
| 20 | **Composable with Vines**: if source is a Vine (conversation pyramid), render ERA annotations and decision FAQs as first-class sections instead of ignoring them | High — makes Folio work across all pyramid types | Medium |

---

## 3. Converge (The Recommendation)

The best Folio is a synthesis of ideas **2, 3, 4, 5, 6, 7, 8, 9, 10, 14, 15, 16, and 20**.

### Resolved Decisions

- **Overlay strictness:** User-definable via `--overlay-match strict|fuzzy` (default: `strict`). Strict = only nodes explicitly referenced by the question pyramid's `Q-L0-*` answer nodes. Fuzzy = semantic headline/topic matching against the question decomposition.
- **Depth semantics:** `--depth` always refers to depth in the **source pyramid** being exported. Overlays filter *which* branches to include; depth controls *how deep* to go in those branches.

### Architecture: Two Surfaces, One Engine

The Folio generator should be built as a **core function** (not a CLI-only script) that both the CLI and the Tauri/GUI can call:

```
┌─────────────────────────────────────┐
│       FolioEngine (core lib)        │
│  - takes: slug, depth, overlays[]   │
│  - returns: structured FolioDoc     │
├──────────────┬──────────────────────┤
│  CLI surface │  Tauri/API surface   │
│  `folio`     │  `export_pyramid`    │
└──────────────┴──────────────────────┘
```

### Revised Command Syntax

```bash
# Basic: full pyramid, 2 levels deep
pyramid-cli folio <slug> --depth 2

# Focused: through a question lens
pyramid-cli folio <slug> --depth 3 --overlay how-build-pipeline-works

# Multi-lens: intersection of two questions, fuzzy matching
pyramid-cli folio <slug> --overlay question-a --overlay question-b --overlay-mode intersect --overlay-match fuzzy

# Output control
pyramid-cli folio <slug> --depth 2 --out ./briefing.md
pyramid-cli folio <slug> --depth 2 --format html --out ./briefing.html
```

### Per-Section Provenance via Handle Paths

Every pyramid node already has a native **handle path**: `slug/depth/node_id` (e.g., `agent-wire-node-definitive/2/L2-S000`). These are used throughout the system for cross-pyramid references, remote web edges, and Wire publication. Additionally, the node schema already has a `build_version` counter (auto-incremented on every upsert) and a `created_at` timestamp.

**No schema migration is needed.** The build agent just needs to expose `build_version` and `created_at` in the node query response (currently omitted by the select query).

Every section in the rendered Folio must include a small, unobtrusive metadata block using the handle path as the canonical address. Example rendering:

```markdown
## Agent-wire-node Runtime Services

The node runs five core services...

---
<sub>agent-wire-node-definitive/2/L2-S000 · v3 · 2026-04-06T03:51:17Z</sub>
```

The fields:
- **Handle path** (`slug/depth/node_id`) — the fully resolvable address for this section's source node. Any agent can `drill <slug> <node_id>` to get back to the live data.
- **Version** (`v3`) — the node's `build_version` from the schema, incrementing on each update.
- **Timestamp** — the node's `created_at` (or a future `updated_at` if added).

### Document Structure

```
┌────────────────────────────────────────┐
│ FOLIO HEADER                           │
│ - Pyramid: slug, type, node_count      │
│ - Generated: ISO timestamp             │
│ - Depth: 0..N                          │
│ - Overlay(s): slug(s) or "none"        │
│ - Overlay Match: strict/fuzzy          │
│ - DADBEAR Status: last_check, pending  │
│ - Stale sections: count                │
├────────────────────────────────────────┤
│ TABLE OF CONTENTS                      │
│ (auto-generated, linked anchors)       │
├────────────────────────────────────────┤
│ BODY                                   │
│ # Apex headline                        │
│   > apex distilled text                │
│   <provenance footer>                  │
│                                        │
│ ## L2 child headline                   │
│   > distilled text                     │
│   > [!WARNING] Stale section           │
│   <provenance footer>                  │
│                                        │
│ ### L1 grandchild headline             │
│   ...                                  │
├────────────────────────────────────────┤
│ APPENDICES                             │
│ A. Glossary (all terms[], alphabetized)│
│ B. Errata (corrections[] if any)       │
│ C. Decision Log (decisions[] if any)   │
└────────────────────────────────────────┘
```

### Vine Compatibility

If the source pyramid is a Vine (`content_type: vine`), the Folio should also render:
- **ERA annotations** as callout blocks within the relevant section
- **Decision FAQs** in the appendix alongside the regular decision log

---

## Proposed Changes

### Core Engine

#### [NEW] `mcp-server/src/folio/engine.ts` (or Rust equivalent in `src-tauri/src/pyramid/folio.rs`)
- `generateFolio(slug, options: FolioOptions): FolioDocument`
- `FolioOptions`: `{ depth: number, overlays: string[], overlayMode: 'union' | 'intersect', overlayMatch: 'strict' | 'fuzzy', includeAnnotations: boolean, format: 'md' | 'html' }`
- `FolioDocument`: structured intermediate representation (sections[], glossary, errata, decisions, header)

#### [NEW] `mcp-server/src/folio/renderer.ts`
- Takes `FolioDocument` → renders to markdown string
- Handles TOC generation, provenance footers, staleness warnings, appendices
- Future: HTML renderer using the same `FolioDocument` input

### CLI Surface

#### [MODIFY] CLI command registry
- Add `folio <slug>` command with flags: `--depth`, `--overlay` (repeatable), `--overlay-mode`, `--overlay-match`, `--out`, `--format`, `--include-annotations`

### API/Tauri Surface

#### [MODIFY] Tauri command registry or REST API routes
- Expose `export_pyramid` / `pyramid_folio` as an invocable command, returning the markdown string or writing to a path

### Schema

#### [MODIFY] Node query API
- Expose `build_version` and `created_at` in the node/drill JSON response (already in the DB schema, currently omitted from `NODE_SELECT_COLS` output)
- No schema migration needed — `build_version` auto-increments on every `save_node` upsert (see `db.rs:1761`)

---

## MPS Audit

**Evaluating:** The Pyramid Folio Generator plan — a new CLI command + export surface that flattens a Knowledge Pyramid into a structured, human-readable markdown document with depth control, composable question overlays, per-section provenance, and staleness indicators.

### Missing (belongs in v1)

- **Topic-based sections, not just node dumps.** Right now the plan renders each node's `distilled` text as a body paragraph. But nodes also carry a rich `topics[]` array where each topic has a `name`, `current` summary, `entities`, `corrections`, and `decisions`. A maximal Folio should render topics as sub-sections within each node's section. Otherwise a node with 7 topics (like L2-S000 with "Runtime Services", "Module Mapping", "Interaction Flow", etc.) gets flattened into one undifferentiated wall of text. **This is the difference between "dump" and "document."**

- **Web edge rendering.** The plan mentions nothing about `edges` (cross-node relationships from the webbing system). These are the "see also" links that connect siblings across the tree. A human reading a section about "Error Handling" needs to know that it connects to "LLM Client Resilience" in another branch. The Folio should render these as inline cross-references (`→ See also: agent-wire-node-definitive/1/L1-045 — LLM Client Resilience`) when both the source and target node are included in the export. Using handlepaths as the cross-reference target means they're resolvable, not just decorative.

- **Pending mutation count in header.** The DADBEAR status in the header should explicitly call out pending mutations, not just "last_check". If there are 80 pending L0 mutations (as the live pyramid shows now), the folio header should say: `> [!CAUTION] 80 pending mutations at L0 — this folio may not reflect the latest source changes.` This is the difference between "we checked" and "we're current."

### Over-engineered (cut or simplify)

- **HTML/PDF format in v1.** Markdown is the native format. HTML and PDF are rendering concerns that can be handled by any pandoc pipeline outside the tool. Shipping with `--format html` in v1 adds a dependency and testing surface that doesn't serve the core use case. **Cut for v1. Add as a separate `folio-render` command later if needed.**

- **Fuzzy overlay matching in v1.** Strict overlay matching is well-defined (follow the `Q-L0-*` → source node links from `composed`). Fuzzy matching requires semantic similarity scoring, which is an LLM call per node — expensive, non-deterministic, and hard to debug. **Ship strict-only in v1. Reserve `--overlay-match fuzzy` as a flag that's documented but not yet implemented, so the API surface is ready.**

### Right (already maximal)

- **Depth control (`--depth N`)** — Correctly maps layer depth to markdown header level. The tree structure from the API confirms children are explicit and ordered, so traversal is deterministic. This is exactly right.

- **Composable overlays (`--overlay` repeatable + `--overlay-mode`)** — The `composed` view proves the question pyramid → source pyramid link graph exists and is queryable. Multiple overlays with union/intersect is the correct abstraction. Making the strictness definable (per user's call) is the right long-term API.

- **Per-section provenance via handle paths** — Handle paths (`slug/depth/node_id`) are the system's native addressing scheme, already used for cross-pyramid references, remote web edges, and Wire publication. Using them as the Folio's section identifier means every section is directly resolvable — any agent can take the handle path and `drill` straight to the live node. Combined with `build_version` (already in the schema, auto-incrementing on every upsert) and `created_at`, this gives us **per-node versioning with zero migration cost**. This is better than what the original plan proposed.

- **Staleness warnings as GitHub alerts** — Directly leveraging `> [!WARNING]` is both human-readable and machine-parseable. The stale-log provides the data; the rendering is correct.

- **Dual surface architecture (CLI + Tauri/API)** — Building the engine as a core lib with thin surface wrappers is the correct architectural bet. It avoids the classic mistake of CLI-only features that have to be reverse-engineered into the GUI later.

- **Glossary appendix from `terms[]`** — Every node carries structured terms with definitions. Auto-collecting and deduplicating these into an alphabetized glossary is high value at near-zero effort. This is already maximal.

- **Errata/corrections appendix** — Nodes carry `corrections[]` with structured data. Rendering these as a dedicated section is the right transparency call.

### Verdict: **NO** → but close. 3 items remain.

The handlepath + build_version discovery resolved 2 of the original 5 gaps (section numbering and schema migration). Source path attribution is handled by the existing `source_path` field on L0 nodes, which can be appended to the handlepath provenance. 3 items remain:

#### Priority Punch List

1. **Add topic-based sub-sections** — When rendering a node, iterate `topics[]` and render each as a sub-heading within the node's section. Use the topic's `name` as the heading and `current` as the body. This is the single biggest readability improvement.

2. **Add web edge cross-references** — After rendering a section, check if any `edges` from the webbing system connect this node to another *included* node. If so, render `→ See also: <target_handlepath> — <headline>` links at the bottom of the section, before the provenance footer. Using handlepaths makes these resolvable, not just decorative.

3. **Add pending mutation warning to header** — If `pending_mutations_by_layer` has any non-zero values, render a `> [!CAUTION]` block in the folio header with the counts.

#### Additional provenance detail for L0 nodes
- Append `· Source: <relative_path>` to the provenance footer for L0 nodes (data already available via `source_path` field).

#### Items to defer from v1
- `--format html|pdf` → v2
- `--overlay-match fuzzy` → v2 (reserve the flag in the CLI parser but return "not yet implemented")

---

## Verification Plan

### Automated Tests
1. Generate a folio from `agent-wire-node-definitive` at `--depth 1` — verify it has exactly 1 apex + 9 L2 sections (matching the known children)
2. Verify L2-S000's section contains sub-headings for all 7 of its topics (Runtime Services, Module Mapping, etc.)
3. Generate a folio with `--overlay how-build-pipeline-works` — verify it only includes sections relevant to the build pipeline
4. Verify every section contains a provenance footer with handlepath (`slug/depth/node_id`), `build_version`, and timestamp
5. Verify stale nodes render with `> [!WARNING]` alerts
6. Verify the folio header includes pending mutation counts from DADBEAR
7. Verify TOC anchors resolve correctly
8. Verify L0 node provenance footers include `source_path`
9. Verify web edge cross-references render as "See also" links with handlepaths, only when both nodes are in scope

### Manual Verification
- Read the generated folio end-to-end as a human and confirm it tells a coherent narrative
- Compare a depth-1 vs depth-3 folio to verify the progressive detail expansion feels natural
- Verify section numbering is deterministic across repeated runs
