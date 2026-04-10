# Vine-of-Vines & Recursive Folder Ingestion Specification

**Version:** 1.0
**Date:** 2026-04-09
**Status:** Design — pre-implementation
**Depends on:** Provider registry (for routing), generative config pattern (for heuristics YAML)
**Unblocks:** Folder ingestion (capstone)
**Authors:** Adam Levine, Claude (session design partner)

---

## Part 1: Vine-of-Vines

### Current State

- `vine_composition.rs` handles vine → bedrock composition via the chain executor
- `pyramid_vine_compositions` table tracks bedrock membership in vines
- `vine.rs:599` explicitly rejects `ContentType::Vine` in `run_build_pipeline`
- Temporal vine recipe exists (for conversation sessions)
- Bedrocks are shared, not owned — a bedrock in one vine is reusable by another

### What's Missing

Vines cannot compose other vines. A parent folder is conceptually a vine whose children include both file-derived pyramids (bedrocks) and sub-folder vines (child vines). The `pyramid_vine_compositions` table only tracks `child_slug`, not child vine slugs.

### Changes

1. **Extend `pyramid_vine_compositions`** — Add `child_type` column:
   ```sql
   ALTER TABLE pyramid_vine_compositions ADD COLUMN child_type TEXT DEFAULT 'bedrock';
   -- child_type: 'bedrock' or 'vine'
   ```
   When `child_type = 'vine'`, `child_slug` references a child vine slug rather than a bedrock.

2. **Allow vine content type in vine composition** — Remove the rejection in `vine.rs:599` (or route vine-of-vine through the composition path)

3. **Topical vine recipe** — New chain YAML for organizing bedrocks by topic and dependency rather than time:
   - Clustering uses import-graph signals from code bedrocks, entity-overlap from doc bedrocks
   - One recipe handles both code and document content (they don't differ at the composition level)
   - The temporal conversation vine remains the only special case

4. **Propagation through vine hierarchy** — When a bedrock updates, propagation walks up through:
   ```
   bedrock → parent vine → grandparent vine → ...
   ```
   Each level triggers a change-manifest update (not full rebuild), using the existing `notify_vine_of_bedrock_completion` pattern extended to vine parents.

### Topical Vine Chain YAML

```yaml
schema_version: 1
id: topical-vine
name: Topical Vine
description: "Organizes bedrocks by topic, dependency, and entity overlap. For folder composition."
content_type: vine
version: "1.0.0"
author: "wire-node"

defaults:
  model_tier: synth_heavy
  temperature: 0.3

steps:
  # Collect apex summaries from all child bedrocks/vines
  - name: collect_children
    primitive: cross_build_input
    save_as: step_only

  # Cluster children by topic using entity overlap and import graph signals
  - name: cluster_children
    primitive: extract
    instruction: "$prompts/vine/topical_cluster.md"
    input:
      children: "$collect_children"
    save_as: step_only
    model_tier: synth_heavy

  # Synthesize per-cluster summaries → L1 nodes
  - name: cluster_synthesis
    primitive: extract
    instruction: "$prompts/vine/topical_synthesis.md"
    for_each: "$cluster_children.clusters"
    # concurrency inherits from chain defaults; overridable via per-step overrides
    depth: 1
    save_as: node
    model_tier: synth_heavy

  # Web edges between clusters
  - name: l1_webbing
    primitive: web
    instruction: "$prompts/question/question_web.md"
    depth: 1
    save_as: web_edges
    model_tier: web

  # Recursive pairing to apex
  - name: upper_synthesis
    primitive: extract
    instruction: "$prompts/vine/topical_apex.md"
    recursive_pair: true
    depth: 2
    save_as: node
    model_tier: synth_heavy
```

---

## Part 2: Recursive Folder Ingestion

### Overview

User points at a folder. The system walks it recursively, detects content types, and creates a self-organizing hierarchy of pyramids and topical vines.

### Self-Organizing Rules

| Condition | Result |
|-----------|--------|
| Homogeneous folder with enough files (configurable via `folder_ingestion_heuristics.min_files_for_pyramid`, seed default: 3) | Pyramid of that content type |
| Mixed-content folder | Topical vine composing its children |
| Folder with subfolders | Topical vine where each subfolder becomes a bedrock or vine |
| Recursion terminates | When folder contents are homogeneous enough for a single pyramid |
| Files below `folder_ingestion_heuristics.min_files_for_pyramid` threshold | Include in parent, don't create a pyramid |
| Binary/large files | Skip (respect ignore patterns) |

### Content Type Detection

| Signal | Content Type |
|--------|-------------|
| `.rs`, `.ts`, `.tsx`, `.py`, `.go`, `.js`, `.java`, `.rb`, `.c`, `.cpp`, `.h` | code |
| `.md`, `.txt`, `.pdf`, `.doc`, `.docx`, `.rst` | document |
| `.json` with conversation structure (messages array) | conversation |
| `.yaml`/`.yml` with `schema_version` + `steps` | chain definition (skip, not content) |
| Everything else | skip |

Detection is a heuristic YAML (generative config pattern, `schema_type: folder_ingestion_heuristics`). The full set of folder ingestion heuristics is a generative config YAML -- user-customizable and Wire-shareable. Users who figure out good rules share them on the Wire.

### Folder Walk Algorithm

```
fn ingest_folder(path, parent_vine_slug):
    children = scan(path)
    subfolders = children.filter(is_dir)
    files = children.filter(is_file).filter(not_ignored)

    if files.is_empty() and subfolders.is_empty():
        return  # nothing here

    if is_homogeneous(files) and subfolders.is_empty() and files.len() >= config.min_files_for_pyramid:  # configurable, seed default: 3
        # Leaf: create a pyramid
        slug = generate_slug(path)
        create_pyramid(slug, content_type_of(files), path)
        if parent_vine_slug:
            add_bedrock_to_vine(parent_vine_slug, slug)
        return

    # Mixed or has subfolders: create a topical vine
    vine_slug = generate_slug(path)
    create_vine(vine_slug, path)
    if parent_vine_slug:
        add_child_vine_to_vine(parent_vine_slug, vine_slug)

    # Recurse into subfolders
    for subfolder in subfolders:
        ingest_folder(subfolder, vine_slug)

    # Handle remaining files
    if files.len() >= config.min_files_for_pyramid:  # configurable, seed default: 3
        file_pyramid_slug = generate_slug(path + "/files")
        create_pyramid(file_pyramid_slug, content_type_of(files), path)
        add_bedrock_to_vine(vine_slug, file_pyramid_slug)
    elif files.len() > 0 and parent_vine_slug:
        # Files below min_files_for_pyramid threshold: include in parent vine's file collection
        add_loose_files_to_vine(parent_vine_slug, files)
```

### Slug Generation

```
path: /Users/adam/AI Project Files/GoodNewsEveryone/src
slug: goodnewseveryone-src

path: /Users/adam/AI Project Files/GoodNewsEveryone/docs/architecture
slug: goodnewseveryone-docs-architecture
```

Rules:
- Take the last 2-3 path segments
- Kebab-case, lowercase
- Deduplicate against existing slugs (append `-2`, `-3` if needed)

### Ignore Patterns

Default `.pyramid-ignore` (like `.gitignore`):
```
node_modules/
target/
.git/
*.lock
*.bin
*.exe
*.dylib
.DS_Store
```

Plus: respect `.gitignore` if present. Max file size: configurable via `folder_ingestion_heuristics.max_file_size_bytes` (seed default: 10485760).

### DADBEAR Integration

**This spec depends on Phase 0b (finish Pipeline B).** Before Phase 0b lands, creating a `pyramid_dadbear_config` row does not drive ongoing pyramid updates because `dispatch_pending_ingests` is stubbed. After Phase 0b, the config drives real ingest chain dispatch for new files.

Each created pyramid gets a DADBEAR config:
- `source_path` = the folder/file path
- `scan_interval_secs` = from `folder_ingestion_heuristics.default_scan_interval_secs` (seed default: 30)
- `enabled` = true by default

**Two pipelines serve the ongoing-update responsibility**, with different domains:

- **Pipeline B (`dadbear_extend.rs`, wired by Phase 0b)** — handles **creation and extension**. The polling scanner notices new files that appeared in a watched folder, writes `pyramid_ingest_records`, and `dispatch_pending_ingests` runs the content-type-appropriate chain against the new file via `fire_ingest_chain`. This is the path that folder ingestion relies on for "a new file appeared in my watched folder, add it to the pyramid." Pipeline B is tick-based and can catch up to changes that happened while the app was offline.
- **Pipeline A (`watcher.rs`, 2026-03-23)** — handles **maintenance of already-ingested files**. fs-notify events on files that are already in `pyramid_file_hashes` → writes `pyramid_pending_mutations` → `stale_engine.rs` polls and debounces → stale checks run via `stale_helpers_upper.rs::execute_supersession` (rewritten by Phase 2 to use change-manifest in-place updates). This is the path that handles "a file I already know about changed, re-sync the affected nodes." Pipeline A is event-driven and fires in real time while the app is running.

The two pipelines are complementary, not overlapping: Pipeline B's detector key is "files in the scan result that are NOT in `pyramid_file_hashes` yet" (new files), and Pipeline A's trigger is fs-notify events on paths that ARE in `pyramid_file_hashes` (known files changing). A newly-ingested file transitions from Pipeline B's domain to Pipeline A's domain the moment `fire_ingest_chain` completes and the file is recorded in `pyramid_file_hashes`.

For folder ingestion, the DADBEAR config Wire Node writes activates both pipelines naturally:
- Pipeline B's polling scanner iterates the configured `source_path` on each tick, discovers any files that aren't yet tracked, and dispatches ingest chains for them.
- Pipeline A's watcher starts watching the same `source_path` on next build completion (when `pyramid_file_hashes` is populated), handling subsequent file edits via the fs-notify path.

Propagation of updates UP through the vine hierarchy happens on the Pipeline A side via change manifests (Phase 2) — when an ingested file changes and `execute_supersession` rewrites the affected L0 node in place, the change manifest propagates to parent vines via `vine_composition.rs::notify_vine_of_bedrock_completion`, which triggers vine-level manifest generation for each affected vine node (see `change-manifest-supersession.md` → Vine-Level Manifests section).

### Claude Code Conversation Auto-Include (enhancement 2026-04-10)

Recursive folder ingestion as described above expects conversation JSONLs to live co-located with the folder being ingested. For a user whose conversations about a project happen in Claude Code, that's wrong — Claude Code stores conversation history in a separate master directory (`~/.claude/projects/`), keyed by the encoded absolute path of the project the conversation was about. The conversation-to-code linkage exists on disk already; the import flow should surface it instead of requiring the user to hand-copy JSONL files.

**The feature:** when the user kicks off recursive folder ingestion on a target folder, they see a checkbox labeled something like:

> ☑ Include Claude Code conversations related to this folder and its subfolders
>
> Or pick a different conversation folder: [browse…]

When checked (default ON if Claude Code is detected), the folder walker ALSO discovers every Claude Code project directory under `~/.claude/projects/` whose encoded path matches the target folder's encoded path OR any of its subfolders' encoded paths. Every matching directory's `*.jsonl` files are attached to the ingestion graph alongside the folder's own content, producing conversation pyramids that sit inside the same vine hierarchy as the code/doc pyramids for that folder.

**Path encoding rule** (derived from observation of `~/.claude/projects/`):

```
/Users/adam/AI Project Files/agent-wire-node
                    ↓ absolute path, slashes → dashes, leading dash preserved
-Users-adam-AI-Project-Files-agent-wire-node
```

The encoded form is a simple `replace('/', '-')` on the absolute path. Spaces, case, and punctuation are preserved in the directory name. Subfolders, worktrees, and sub-project directories under the same parent each get their own encoded entry (e.g., `-Users-adam-AI-Project-Files-agent-wire-node--claude-worktrees-<name>` for a Claude Code worktree).

**Matching algorithm:**

```
fn find_claude_code_conversation_dirs(target_folder: &Path) -> Vec<PathBuf>:
    let claude_projects_root = home_dir().join(".claude").join("projects")
    if !claude_projects_root.exists():
        return vec![]  # Claude Code not installed or no conversations yet

    let encoded_target = encode_path_for_claude_code(target_folder)
    let mut matches = vec![]

    for entry in read_dir(claude_projects_root):
        let dir_name = entry.file_name().to_string_lossy()
        # Match if the entry's encoded path is exactly the target OR starts with
        # the target followed by a dash (indicating a subfolder of the target).
        if dir_name == encoded_target:
            matches.push(entry.path())
        else if dir_name.starts_with(&format!("{}-", encoded_target)):
            matches.push(entry.path())

    return matches

fn encode_path_for_claude_code(path: &Path) -> String:
    # Absolute path with `/` replaced by `-`. Leading `-` from root is preserved.
    path.to_string_lossy().replace('/', "-")
```

**Integration with the folder walk:**

The matching directories are treated as additional conversation sources attached to the target folder's topical vine. Each Claude Code project directory becomes its own conversation pyramid (slug: `{target-folder-slug}-cc-{encoded-subfolder-segment}`), and all of them are added as bedrocks of the target folder's vine.

Pseudocode extension to `ingest_folder`:

```
fn ingest_folder(path, parent_vine_slug, include_claude_code: bool):
    # ... existing walk ...

    if include_claude_code and is_top_level_call:
        let cc_dirs = find_claude_code_conversation_dirs(path)
        for cc_dir in cc_dirs:
            let cc_slug = generate_slug_for_claude_code_dir(path, cc_dir)
            create_pyramid(cc_slug, ContentType::Conversation, cc_dir.to_string())
            add_bedrock_to_vine(vine_slug_for(path), cc_slug)
```

The `is_top_level_call` guard ensures we only do the Claude Code scan at the top of the recursion, not on every subfolder — the top-level scan already matches subfolder-encoded directories via the `starts_with` prefix check.

**DADBEAR integration:** each Claude Code conversation pyramid gets its own DADBEAR config pointing at the Claude Code project directory. When the user continues their Claude Code session, new JSONL files appear in that directory, Pipeline B's scanner picks them up on the next tick, and the episodic memory pyramid grows automatically — the user doesn't have to re-import anything. Pipeline A continues to handle changes to existing JSONLs (Claude Code appends to the active session file).

**Privacy note:** Claude Code conversations may contain sensitive context (API keys pasted in, private business logic, etc.). The auto-include feature is opt-in via the checkbox. The default state is ON only when the folder has a clear Claude Code conversation match (i.e., `find_claude_code_conversation_dirs` returns at least one directory) AND the folder is local (not a network mount). Otherwise default OFF, user must explicitly opt in.

**UI surface (Phase 17 wizard):**

```
Ingest folder: AI Project Files/agent-wire-node

  Content:           [Auto-detect (recommended)]
  Recursion depth:   [No limit ▼]

  ☑ Include Claude Code conversations related to this folder
     Found 4 matching conversation directories in ~/.claude/projects:
      • -Users-adam-AI-Project-Files-agent-wire-node (main)
      • -Users-adam-AI-Project-Files-agent-wire-node-docs-pyramid-prototype
      • -Users-adam-AI-Project-Files-agent-wire-node--claude-worktrees-nervous-lichterman
      • -Users-adam-AI-Project-Files-agent-wire-node--claude-worktrees-wonderful-tesla

     Or pick a different folder: [browse…]

  [Cancel]  [Start ingestion]
```

**IPC surface addition** (for Phase 17 + Phase 10 wizard):

```
pyramid_find_claude_code_conversations(target_folder: String)
  → Vec<{
      encoded_path: String,
      absolute_path: String,
      jsonl_count: usize,
      earliest_mtime: String,
      latest_mtime: String,
      is_main: bool,         // true if encoded_path matches target exactly
      is_worktree: bool,     // true if encoded_path contains "--claude-worktrees-"
    }>
```

The UI calls this command on folder selection to populate the pre-flight list. If the returned list is empty, the checkbox can be hidden or greyed out.

**Configurable via heuristics:** the `folder_ingestion_heuristics` contribution (Phase 4) gains new fields:
- `claude_code_auto_include: bool` — default ON
- `claude_code_conversation_path: String` — default `"~/.claude/projects"`, user-overridable if Claude Code stores its conversations elsewhere or the user wants to point at a different source (e.g., a Cursor conversation cache)

Both fields flow through the normal contribution/refinement loop — a user who wants to import Cursor history instead of Claude Code history can generate a variant heuristics config that points at a different path.

### Example Output

```
AI Project Files/                         ← topical vine (apex of everything)
├── GoodNewsEveryone/                     ← topical vine
│   ├── src/                              ← code pyramid
│   ├── docs/                             ← topical vine
│   │   ├── architecture/                 ← document pyramid
│   │   └── plans/                        ← document pyramid
│   └── supabase/migrations/              ← code pyramid
├── agent-wire-node/                      ← topical vine
│   ├── src-tauri/src/                    ← code pyramid
│   ├── src/                              ← code pyramid (React)
│   ├── mcp-server/                       ← code pyramid
│   └── docs/                             ← document pyramid
└── vibesmithy/                           ← topical vine
```

---

## Build Viz Integration

The folder ingestion creates a tree of pyramids and vines. The build viz should show:
- The vine hierarchy (which pyramids compose into which vines)
- Build status per pyramid (building, complete, stale)
- Cost attribution (how much each pyramid/vine costs to maintain)
- Propagation flow (when a bedrock updates, show which vines are affected)

---

## Implementation Order

1. **Vine-of-vines** — Extend `pyramid_vine_compositions`, remove vine rejection, write topical vine chain YAML + prompts
2. **Content type detection** — Heuristic function + `.pyramid-ignore` support
3. **Folder walk algorithm** — Recursive scanner that creates pyramids and vines
4. **DADBEAR auto-config** — Auto-create DADBEAR configs for ingested pyramids
5. **Propagation through vine hierarchy** — Extend `notify_vine_of_bedrock_completion` for vine parents
6. **Folder ingestion UI** — "Point at folder" mode in AddWorkspace

### Files

| Item | Files |
|------|-------|
| Vine-of-vines | `db.rs`, `vine_composition.rs`, `vine.rs`, new chain YAML |
| Content detection | New `folder_ingestion.rs` |
| Folder walk | `folder_ingestion.rs` |
| DADBEAR auto-config | `dadbear_extend.rs` |
| Propagation | `vine_composition.rs` |
| UI | `AddWorkspace.tsx` |

---

## Open Questions

1. **Folder depth limit**: How deep to recurse? Configurable via `folder_ingestion_heuristics.max_recursion_depth` (seed default: 10). Most codebases don't go deeper.

2. **Incremental folder re-scan**: When a new subfolder appears, should the system auto-create a new pyramid/vine? Recommend: yes, DADBEAR detects new directories and triggers the folder walk for that subtree.

3. **Vine collapse**: If a vine has only one child (single subfolder), should it be collapsed into its parent? Recommend: yes, single-child vines add structure without value.
