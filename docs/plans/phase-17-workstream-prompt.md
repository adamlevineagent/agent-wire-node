# Workstream: Phase 17 — Recursive Folder Ingestion (+ Claude Code Auto-Include)

## Who you are

You are an implementer joining an active 17-phase initiative. Phases 0a, 0b, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16 are shipped. You are the implementer of Phase 17 — the capstone: recursive folder ingestion that walks a folder, detects content types, auto-creates pyramids and topical vines, wires DADBEAR configs, AND the Claude Code conversation auto-include enhancement Adam added mid-run.

Phase 17 depends on Phase 16 (vine-of-vines) which you just shipped. Phase 16's topical vine chain YAML + recursive propagation is what makes Phase 17 possible.

## Context

Phases 0b (Pipeline B), 4 (folder_ingestion_heuristics schema type), 16 (vine-of-vines + topical vine recipe) are shipped. Phase 17 uses all three:
- Pipeline B handles new files appearing in the watched folders
- `folder_ingestion_heuristics` is already a valid schema_type in `config_contributions.rs:692` with `db::upsert_folder_ingestion_heuristics`
- Topical vines compose children from the folder hierarchy via Phase 16

What Phase 17 adds:
- **`folder_ingestion.rs`** — new module with content type detection, folder walk algorithm, slug generation, ignore pattern handling, `.pyramid-ignore` + `.gitignore` parsing
- **Claude Code conversation auto-include** (the mid-run enhancement)
- **New IPCs**: `pyramid_ingest_folder` (the main entry point), `pyramid_find_claude_code_conversations` (pre-flight for the wizard)
- **Extended `folder_ingestion_heuristics` schema** fields: `claude_code_auto_include`, `claude_code_conversation_path`, `min_files_for_pyramid`, `max_recursion_depth`, etc.
- **Bundled seed contribution** for `folder_ingestion_heuristics`
- **`AddWorkspace.tsx` extension** — new "Point at folder" mode with wizard UI showing detected Claude Code conversations

## Required reading (in order)

### Spec docs

1. `docs/handoffs/handoff-2026-04-09-pyramid-folders-model-routing.md` — deviation protocol.
2. **`docs/specs/vine-of-vines-and-folder-ingestion.md` Part 2 (lines 108-363) in full** — primary implementation contract. Pay special attention to the Claude Code auto-include section (lines 229-344).
3. `docs/specs/config-contribution-and-wire-sharing.md` — quick scan for how `folder_ingestion_heuristics` contributions are edited via the generative flow.
4. `docs/plans/pyramid-folders-model-routing-full-pipeline-observability.md` — Phase 17 section.
5. `docs/plans/pyramid-folders-model-routing-implementation-log.md` — scan Phase 16 entries so you know the exact helpers for vine-of-vines composition.

### Code reading

6. **`src-tauri/src/pyramid/vine_composition.rs`** (post-Phase 16) — `insert_vine_composition`, `update_child_apex`, `get_parent_vines_recursive`, `notify_vine_of_child_completion`. Phase 17 calls these to register folder → vine composition.
7. **`src-tauri/src/pyramid/db.rs`** — find `upsert_folder_ingestion_heuristics` and the `pyramid_folder_ingestion_heuristics` table. Phase 17 extends the YAML schema (add new fields) and the operational table.
8. **`src-tauri/src/pyramid/config_contributions.rs:692`** — the folder_ingestion_heuristics sync branch. You'll extend the YAML struct.
9. **`src-tauri/src/pyramid/dadbear_extend.rs`** — understand how DADBEAR configs are wired. Phase 17 auto-creates DADBEAR configs for ingested pyramids.
10. **`src-tauri/src/pyramid/build_runner.rs`** — how new pyramids get their first build triggered after creation.
11. `src-tauri/src/pyramid/chain_executor.rs` — content type → chain mapping (verify your folder walker's created pyramids use the right chain).
12. `src-tauri/src/main.rs` — find the `invoke_handler!` list + existing `pyramid_create_slug` / `pyramid_add_workspace` IPCs for the pattern. Phase 17 adds `pyramid_ingest_folder` and `pyramid_find_claude_code_conversations`.
13. **`src/components/AddWorkspace.tsx` in full** (~1213 lines). Understand the existing slug creation flow. Phase 17 extends this with a "Point at folder" mode + wizard.
14. `src-tauri/assets/bundled_contributions.json` — add the `folder_ingestion_heuristics` bundled seed.

## What to build

### 1. Backend: `folder_ingestion.rs` module

New module: `src-tauri/src/pyramid/folder_ingestion.rs`.

```rust
pub struct FolderIngestionConfig {
    pub min_files_for_pyramid: usize,          // seed default: 3
    pub max_recursion_depth: usize,            // seed default: 10
    pub max_file_size_bytes: u64,              // seed default: 10_485_760 (10 MiB)
    pub default_scan_interval_secs: u64,       // seed default: 30
    pub code_extensions: Vec<String>,          // .rs, .ts, .py, etc.
    pub document_extensions: Vec<String>,      // .md, .txt, .pdf, etc.
    pub ignore_patterns: Vec<String>,          // node_modules/, target/, .git/, *.lock, *.bin, *.exe, .DS_Store
    pub claude_code_auto_include: bool,        // default true
    pub claude_code_conversation_path: String, // default "~/.claude/projects"
}

pub struct IngestionPlan {
    pub operations: Vec<IngestionOperation>,
    pub root_slug: String,
}

pub enum IngestionOperation {
    CreatePyramid {
        slug: String,
        content_type: ContentType,
        source_path: PathBuf,
    },
    CreateVine {
        slug: String,
        source_path: PathBuf,  // for bookkeeping; vines don't ingest directly
    },
    AddChildToVine {
        vine_slug: String,
        child_slug: String,
        child_type: String,  // "bedrock" | "vine"
    },
    RegisterDadbearConfig {
        slug: String,
        source_path: PathBuf,
        scan_interval_secs: u64,
    },
    RegisterClaudeCodePyramid {
        slug: String,
        source_path: PathBuf,  // the Claude Code project directory
        is_main: bool,
        is_worktree: bool,
    },
}

pub fn detect_content_type(files: &[PathBuf], config: &FolderIngestionConfig) -> Option<ContentType>

pub fn scan_folder(path: &Path, config: &FolderIngestionConfig) -> Result<ScanResult>

pub struct ScanResult {
    pub subfolders: Vec<PathBuf>,
    pub files: Vec<PathBuf>,
    pub ignored_count: usize,
}

pub fn generate_slug(path: &Path, existing_slugs: &HashSet<String>) -> String

pub fn is_homogeneous(files: &[PathBuf], config: &FolderIngestionConfig) -> bool

pub fn plan_ingestion(
    target_folder: &Path,
    config: &FolderIngestionConfig,
    include_claude_code: bool,
) -> Result<IngestionPlan>

pub fn find_claude_code_conversation_dirs(target_folder: &Path) -> Vec<PathBuf>

pub fn encode_path_for_claude_code(path: &Path) -> String
```

Key algorithmic details:
- **Content type detection**: group files by extension, pick the majority. If mixed, return None (force a vine).
- **Homogeneity check**: all files share the same detected content type.
- **Slug generation**: take last 2-3 path segments, kebab-case, lowercase, append suffixes on collision.
- **Ignore patterns**: parse `.pyramid-ignore` if present, plus `.gitignore`, plus the bundled defaults.
- **Recursion termination**: at `max_recursion_depth` OR when homogeneous + above threshold.
- **Claude Code scan**: only at `is_top_level_call` (spec line 294-296). The `starts_with(encoded_target + "-")` prefix match handles subfolders.
- **Plan vs execute**: `plan_ingestion` is a DRY RUN that returns all operations. `execute_plan` then runs them. This separation lets the UI show the user a preview before committing.

### 2. Backend: plan execution

Add to `folder_ingestion.rs`:

```rust
pub async fn execute_plan(
    state: &PyramidState,
    plan: IngestionPlan,
) -> Result<IngestionResult>

pub struct IngestionResult {
    pub pyramids_created: Vec<String>,
    pub vines_created: Vec<String>,
    pub dadbear_configs: Vec<String>,
    pub claude_code_pyramids: Vec<String>,
    pub root_slug: String,
}
```

Implementation:
1. For each `CreatePyramid` op: call the existing slug creation flow (likely via `pyramid_create_slug` or a lower-level helper). Content type determines which chain will run.
2. For each `CreateVine` op: create a slug with `content_type = "vine"`. The topical vine chain (Phase 16) runs on it.
3. For each `AddChildToVine` op: `db::insert_vine_composition(conn, vine_slug, child_slug, position, child_type)` — Phase 16 helper.
4. For each `RegisterDadbearConfig` op: write a `pyramid_dadbear_config` row via `db::save_dadbear_config`. Pipeline B's polling scanner will pick up the files.
5. For each `RegisterClaudeCodePyramid` op: same as CreatePyramid with content_type=Conversation + source_path=the Claude Code dir + a `cc-` slug prefix + a DADBEAR config on the cc dir.
6. Return the IngestionResult summary.

**Transaction boundaries**: wrap the whole plan execution in a transaction so if anything fails mid-plan, nothing is left half-committed. Or: execute each op atomically and log failures — recommend the latter for better UX (partial plans are still useful).

### 3. Backend: IPC commands

Add to `main.rs`:

```rust
#[derive(Deserialize)]
struct IngestFolderInput {
    target_folder: String,
    include_claude_code: bool,
    dry_run: bool,  // if true, return the plan without executing
}

#[derive(Serialize)]
struct IngestFolderOutput {
    plan: IngestionPlan,   // always returned
    result: Option<IngestionResult>,  // only when dry_run = false
}

#[tauri::command]
async fn pyramid_ingest_folder(
    input: IngestFolderInput,
    state: tauri::State<'_, SharedState>,
) -> Result<IngestFolderOutput, String>

#[derive(Serialize)]
struct ClaudeCodeConversationDir {
    encoded_path: String,
    absolute_path: String,
    jsonl_count: usize,
    earliest_mtime: Option<String>,
    latest_mtime: Option<String>,
    is_main: bool,
    is_worktree: bool,
}

#[tauri::command]
async fn pyramid_find_claude_code_conversations(
    target_folder: String,
) -> Result<Vec<ClaudeCodeConversationDir>, String>
```

Register in `invoke_handler!`.

### 4. Backend: extend `folder_ingestion_heuristics` YAML schema

Current Phase 4 shape in `db::upsert_folder_ingestion_heuristics` is minimal. Extend `FolderIngestionHeuristicsYaml` (find in `db.rs`) to include:

```rust
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct FolderIngestionHeuristicsYaml {
    #[serde(default)]
    pub min_files_for_pyramid: Option<usize>,
    #[serde(default)]
    pub max_recursion_depth: Option<usize>,
    #[serde(default)]
    pub max_file_size_bytes: Option<u64>,
    #[serde(default)]
    pub default_scan_interval_secs: Option<u64>,
    #[serde(default)]
    pub code_extensions: Option<Vec<String>>,
    #[serde(default)]
    pub document_extensions: Option<Vec<String>>,
    #[serde(default)]
    pub ignore_patterns: Option<Vec<String>>,
    #[serde(default)]
    pub claude_code_auto_include: Option<bool>,
    #[serde(default)]
    pub claude_code_conversation_path: Option<String>,
}
```

Add a loader: `pub fn load_active_folder_ingestion_heuristics(conn: &Connection) -> Result<FolderIngestionConfig>` that reads the active contribution and fills defaults for any missing fields.

### 5. Backend: bundled seed contribution

Add to `src-tauri/assets/bundled_contributions.json`:

```json
{
  "contribution_id": "bundled-folder_ingestion_heuristics-default-v1",
  "schema_type": "folder_ingestion_heuristics",
  "slug": null,
  "yaml_content": "schema_type: folder_ingestion_heuristics\nmin_files_for_pyramid: 3\nmax_recursion_depth: 10\nmax_file_size_bytes: 10485760\ndefault_scan_interval_secs: 30\ncode_extensions: [\".rs\", \".ts\", \".tsx\", \".py\", \".go\", \".js\", \".jsx\", \".java\", \".rb\", \".c\", \".cpp\", \".h\", \".hpp\", \".cs\", \".swift\", \".kt\"]\ndocument_extensions: [\".md\", \".txt\", \".pdf\", \".doc\", \".docx\", \".rst\", \".org\"]\nignore_patterns:\n  - \"node_modules/\"\n  - \"target/\"\n  - \".git/\"\n  - \"*.lock\"\n  - \"*.bin\"\n  - \"*.exe\"\n  - \"*.dylib\"\n  - \".DS_Store\"\n  - \"__pycache__/\"\n  - \"dist/\"\n  - \"build/\"\n  - \".venv/\"\n  - \"venv/\"\nclaude_code_auto_include: true\nclaude_code_conversation_path: \"~/.claude/projects\"\n",
  "source": "bundled",
  "status": "active"
  ...
}
```

Extend the migration path that loads bundled contributions to handle this one (should work automatically via the existing dispatcher).

### 6. Frontend: AddWorkspace "Point at folder" mode

Extend `src/components/AddWorkspace.tsx`:

Add a new mode alongside the existing ones: "Point at folder (recursive)". When selected:

1. **Folder picker**: native dialog → user selects a folder.
2. **Claude Code detection**: on selection, calls `invoke('pyramid_find_claude_code_conversations', { target_folder })`.
   - If the result is non-empty: show the checkbox "Include Claude Code conversations related to this folder" with the list of matching directories below it. Default ON.
   - If empty: hide or grey out the checkbox.
3. **Preview (dry run)**: "Next" button calls `invoke('pyramid_ingest_folder', { target_folder, include_claude_code, dry_run: true })`. Shows the returned plan:
   - Tree view of the proposed vine/pyramid hierarchy
   - Count of pyramids + vines + files that will be ingested
   - Estimated scan interval + max recursion depth (editable via a small settings expander)
4. **Commit**: "Start ingestion" button calls the same IPC with `dry_run: false`. On success, shows a toast "Created N pyramids, M vines" and closes the modal.

UI components:
- `FolderIngestionWizard.tsx` — multi-step wizard (select folder → confirm → preview → start).
- `IngestionPlanPreview.tsx` — renders the plan as a tree.
- `ClaudeCodeConversationList.tsx` — renders the detected CC dirs with icons for main/worktree/subfolder.

Match existing `AddWorkspace.tsx` styling conventions.

### 7. Tests

Rust tests:
- `folder_ingestion.rs` phase17_tests:
  - `test_detect_content_type_homogeneous_code`
  - `test_detect_content_type_homogeneous_document`
  - `test_detect_content_type_mixed_returns_none`
  - `test_scan_folder_respects_ignore_patterns`
  - `test_scan_folder_respects_gitignore`
  - `test_generate_slug_kebab_cases_path_segments`
  - `test_generate_slug_handles_collision_with_suffix`
  - `test_encode_path_for_claude_code`
  - `test_find_claude_code_conversation_dirs_matches_encoded_target`
  - `test_find_claude_code_conversation_dirs_matches_subfolders_via_prefix`
  - `test_plan_ingestion_single_level_homogeneous`
  - `test_plan_ingestion_mixed_folder_creates_vine`
  - `test_plan_ingestion_recursive_multi_level`
  - `test_plan_ingestion_with_claude_code_attaches_cc_pyramids`
  - `test_plan_ingestion_respects_max_recursion_depth`
  - `test_plan_ingestion_skips_below_threshold_files`
- `db.rs` phase17_tests:
  - `test_folder_ingestion_heuristics_yaml_roundtrip_with_new_fields`
  - `test_load_active_folder_ingestion_heuristics_defaults`
- `config_contributions.rs`:
  - `test_sync_folder_ingestion_heuristics_with_new_fields`

**Use temp dirs + synthetic files** for folder walk tests. Don't hit the real filesystem except through a temp directory.

### 8. Implementation log

Append Phase 17 entry. Include:
1. New module: `folder_ingestion.rs`
2. Content type detection logic
3. Folder walk algorithm
4. Claude Code auto-include implementation
5. New IPC commands
6. Bundled seed contribution
7. Frontend wizard components
8. Manual verification steps
9. Status: `awaiting-verification`

## Scope boundaries

**In scope:**
- `folder_ingestion.rs` module with scan, detect, plan, execute
- Claude Code conversation auto-include (find + attach)
- New IPCs: `pyramid_ingest_folder`, `pyramid_find_claude_code_conversations`
- Extended `folder_ingestion_heuristics` YAML schema + loader
- Bundled seed contribution for defaults
- AddWorkspace "Point at folder" mode + wizard components
- Rust tests using temp dirs
- Frontend tests if runner exists
- Implementation log

**Out of scope:**
- Detection of Cursor/other IDE conversation caches beyond the configurable path
- Automatic vine collapse (the "single-child vines collapse into parent" open question — defer)
- Folder depth limit UI — just use the config field
- Background monitoring for new subfolders (DADBEAR handles new files; new subfolders are a follow-up)
- Migrate existing standalone pyramids into a folder-ingested hierarchy (separate flow)
- Frontend tests if no runner
- CSS overhaul — match existing AddWorkspace conventions
- The 7 pre-existing unrelated Rust test failures

## Verification criteria

1. **Rust clean:** `cargo check --lib`, `cargo build --lib` from `src-tauri/` — zero new warnings.
2. **Test count:** `cargo test --lib pyramid` — Phase 16 count (1205) + new Phase 17 tests. Same 7 pre-existing failures.
3. **Frontend build:** `npm run build` — clean.
4. **Bundled contribution present:** grep `bundled_contributions.json` for `folder_ingestion_heuristics`.
5. **IPC registration:** grep `main.rs` for both new IPCs — each should appear in function definition + `invoke_handler!`.
6. **Manual verification path** documented:
   - Launch dev, open AddWorkspace, pick "Point at folder" mode
   - Select a real folder (e.g., the agent-wire-node repo)
   - Verify Claude Code conversations are detected if the folder has any
   - Dry-run shows the proposed plan
   - Start ingestion creates the expected pyramid/vine hierarchy
   - Verify DADBEAR configs are auto-created
   - Verify builds start on the new pyramids

## Deviation protocol

Standard. Most likely deviations:

- **`ContentType` variants**: the content type enum (`Code`, `Document`, `Conversation`, `Vine`, `Question`) may not match your detection logic exactly. Use what's there; don't add new variants.
- **Slug creation path**: `pyramid_create_slug` may not directly accept a source path + content type for programmatic creation. If it's too UI-coupled, use a lower-level helper like `db::upsert_pyramid_slug` or similar. Document the choice.
- **DADBEAR config auto-creation during plan execution**: if the DADBEAR config helpers require more state than what `folder_ingestion.rs` has, thread the state through. Don't duplicate logic.
- **Claude Code path detection on Windows**: the spec shows Unix paths. On Windows, `~/.claude/projects` is `%USERPROFILE%\.claude\projects`. Use `home_dir()` crate to handle platform differences.
- **Recursion depth tracking**: the walk needs to track current depth. Pass it as a parameter to the recursive function.
- **Existing slug conflict**: if a folder would generate a slug that collides with an already-ingested slug (from a prior run or another workspace), resolve via suffix. Log the collision.

## Implementation log protocol

Append Phase 17 entry. Include:
1. Module creation + content
2. Algorithm pseudocode with file:line references
3. IPC shape
4. Bundled contribution
5. Frontend component structure
6. Manual verification steps
7. Any deviations
8. Status: `awaiting-verification`

## Mandate

- **Phase 17 is the capstone.** Deliver end-to-end: a user can point at a folder and get a full ingested hierarchy with Claude Code conversations attached.
- **Dry run first.** The user always sees the plan before committing. No surprise pyramid creation.
- **Claude Code default is ON when matches found.** The spec is explicit.
- **Fix all bugs found during the sweep.** Standard.
- **Match existing frontend conventions.** AddWorkspace.tsx is the pattern.
- **Commit when done.** Single commit with message `phase-17: recursive folder ingestion + claude code auto-include`. Body: 6-10 lines summarizing folder_ingestion module, detection + walk + plan + execute, Claude Code auto-include, new IPCs, bundled seed, AddWorkspace wizard. Do not amend. Do not push.

## End state

Phase 17 is complete when:

1. `src-tauri/src/pyramid/folder_ingestion.rs` exists with scan/detect/plan/execute/Claude-Code-integration.
2. `pyramid_ingest_folder` + `pyramid_find_claude_code_conversations` IPCs registered.
3. `folder_ingestion_heuristics` YAML schema extended with the new fields.
4. Bundled seed contribution in `bundled_contributions.json`.
5. AddWorkspace.tsx has a "Point at folder" mode with wizard UI + preview.
6. Claude Code conversation detection + auto-include works.
7. `cargo check --lib` + `cargo build --lib` + `npm run build` clean.
8. `cargo test --lib pyramid` at Phase 16 count + new Phase 17 tests. Same 7 pre-existing failures.
9. Implementation log Phase 17 entry complete.
10. Single commit on branch `phase-17-recursive-folder-ingestion`.

Begin with the spec (Part 2 in full) + existing AddWorkspace + existing folder_ingestion_heuristics code. Then build.

Good luck. Build carefully. This is the last phase — deliver the capstone cleanly.
