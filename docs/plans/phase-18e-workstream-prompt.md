# Workstream: Phase 18e — CC Memory Subfolder Pickup + CC Dir as Vine

## Who you are

You are an implementer joining a coordinated fix-pass across the pyramid-folders/model-routing/observability initiative. Phase 18 reclaims 9 dropped cross-phase handoffs PLUS one scope gap discovered during Adam's first real use of the shipped app. You are implementing workstream **18e**, claiming discovery entry **D1** from `docs/plans/deferral-ledger.md`.

Four other Phase 18 workstreams (18a/18b/18c/18d) run in parallel on their own branches — they claim the 9 ledger entries. You do NOT touch files outside your scope. Your commits land on branch `phase-18e-cc-memory-subfolder`.

## Context

When Phase 17's recursive folder ingestion attaches a Claude Code conversation directory (e.g., `~/.claude/projects/-Users-adam-AI-Project-Files-agent-wire-node`), it currently:

1. Emits a single `RegisterClaudeCodePyramid` op per CC dir
2. The op becomes a bedrock child of the target folder's top-level vine (via Phase 17's `spawn_initial_builds` path that partitions leaves and vines)
3. During first-build pre-populate, it runs `ingest_conversation` on only the most recently modified `.jsonl` file in the CC dir (workaround for Phase 0b chunk-collision, documented in the Phase 17 implementation log)

**What's wrong:** a typical Claude Code conversation directory also contains a `memory/` subfolder with `.md` files — Claude's persistent project memory. Those files contain load-bearing project knowledge (architecture decisions, user preferences, recurring context) that belongs in the pyramid graph. Today they're silently ignored because the CC dir is treated as a single conversation pyramid and the conversation ingest chain only consumes jsonl.

**What Adam wants (from the conductor thread):** restructure the CC dir so that:

- Each CC dir becomes a **vine** (not a single pyramid)
- The jsonl conversations in the CC dir become a **conversation bedrock** child of that vine
- IF a `memory/` subfolder exists with `.md` files, they become a **document bedrock** child of that same vine
- The CC vine then attaches to the target folder's top-level vine as a **vine child** (per Phase 16's `child_type='vine'` composition)
- The result: the CC vine acts "as if it were another folder/pyramid in the root `/agent-wire-node` directory" — peer to real folder children like `src-tauri`, `src`, `docs`

Phase 16 shipped the `child_type='vine'` composition pattern and the topical vine chain for exactly this case. You are using it.

## Ledger entry you claim

| ID | Item | Source |
|---|---|---|
| **D1** | **Claude Code `memory/*.md` subfolder pickup + CC dir → vine restructure** — each CC dir emits a mini-subplan (create CC vine + create conversation bedrock + optionally create memory doc bedrock) instead of a single `RegisterClaudeCodePyramid` op. | `docs/plans/deferral-ledger.md` Discovered-by-use section; Adam's conductor-thread refinement |

## Required reading (in order)

1. `docs/plans/phase-18-plan.md` — overall Phase 18 structure; skim.
2. `docs/plans/deferral-ledger.md` — D1 entry + the "Discovered-by-use" framing.
3. **`docs/specs/vine-of-vines-and-folder-ingestion.md`** — Part 2 (lines 108-363) + especially the Claude Code auto-include enhancement section (~line 229). The enhancement shipped in Phase 17 as a single-pyramid model; D1 restructures it to the vine-containing-two-bedrocks model.
4. `docs/plans/phase-16-workstream-prompt.md` — the vine-of-vines foundation. `insert_vine_composition(child_type='vine')`, `notify_vine_of_child_completion`, topical vine chain `topical-vine.yaml`.
5. `docs/plans/phase-17-workstream-prompt.md` — the original Phase 17 scope. Understand what Phase 17 built and what it deliberately didn't build.

### Code reading

6. **`src-tauri/src/pyramid/folder_ingestion.rs`** in full (~2000 lines). The module you are modifying. Pay particular attention to:
   - `IngestionOperation::RegisterClaudeCodePyramid` — the variant you're replacing / augmenting
   - `plan_ingestion` + `plan_recursive` — the walk algorithm
   - `execute_plan` — the execution path (lines ~970-1120)
   - `spawn_initial_builds` + `extract_build_dispatches` — the Phase 17 wanderer's first-build dispatch helper
   - `prepopulate_chunks_for` — the per-content-type ingest pre-pop function
   - `find_claude_code_conversation_dirs` — the scanner (line ~456)
   - `describe_claude_code_dirs` — the UI-facing IPC helper (line ~499)
7. **`src-tauri/src/pyramid/vine_composition.rs`** — `insert_vine_composition`, `notify_vine_of_child_completion`, `get_vines_for_child`. Phase 16 primitives you consume.
8. **`src-tauri/src/pyramid/db.rs` line ~1405** — `pyramid_vine_compositions` table with `child_type` column. Understand the schema.
9. **`src-tauri/src/pyramid/build.rs::build_topical_vine`** — the Phase 16 function that dispatches `topical-vine.yaml` on vine content_type builds. You'll need first-build triggering for the new CC vines.
10. `chains/defaults/topical-vine.yaml` — the Phase 16 chain. Verify its `cross_build_input` primitive handles vines with mixed bedrock children (one conversation, one document).
11. `src-tauri/src/pyramid/build.rs::ingest_conversation`, `ingest_docs` — the pre-pop helpers. The conversation pyramid pre-pops via `ingest_conversation`; the memory doc pyramid pre-pops via `ingest_docs`.
12. `src-tauri/src/pyramid/chain_executor.rs::execute_chain_from` — the vine build entry. Phase 16 wanderer fixed the `num_chunks==0` gate to accept vines. Verify.
13. **`src/components/AddWorkspace.tsx` folder ingestion wizard (~lines 700-1200)** — the preview modal. You extend the preview to show "CC vines: N" and "CC memory doc bedrocks: M" distinct from the old "CC pyramids: N" count.

## What to build

### 1. Detect `memory/` subfolders in the scanner

Extend `find_claude_code_conversation_dirs` (or add a sibling helper) so that for each matching CC dir, the return value also carries metadata about whether a `memory/` subfolder exists and, if so, the count of `.md` files in it.

Extend `ClaudeCodeConversationDir`:

```rust
pub struct ClaudeCodeConversationDir {
    pub encoded_path: String,
    pub absolute_path: String,
    pub jsonl_count: usize,
    pub earliest_mtime: Option<String>,
    pub latest_mtime: Option<String>,
    pub is_main: bool,
    pub is_worktree: bool,
    // NEW:
    pub has_memory_subfolder: bool,
    pub memory_md_count: usize,
    pub memory_subfolder_path: Option<String>,
}
```

In `describe_claude_code_dirs`, populate the new fields by checking `{cc_dir}/memory/` and counting `.md` files if present.

### 2. Restructure the ingestion plan for each CC dir

Replace the single `RegisterClaudeCodePyramid` op with a mini-subplan. Each CC dir with at least one `.jsonl` file (AND optionally a `memory/` subfolder with `.md` files) emits:

**Op 1: Create the CC vine**
```
CreateVine {
    slug: "{cc_slug_prefix}-{encoded_segment}",
    source_path: cc_dir_absolute_path,
}
```
Slug naming: keep the Phase 17 cc slug prefix convention (e.g., `{target_folder_slug}-cc-{encoded_segment}`) but treat the result as the VINE slug, not the conversation pyramid's slug.

**Op 2: Create the conversation bedrock**
```
CreatePyramid {
    slug: "{cc_vine_slug}-conversations",
    content_type: ContentType::Conversation,
    source_path: cc_dir_absolute_path,  // same as vine, but this one actually ingests jsonls
}
```

**Op 3: Add conversation pyramid as bedrock child of CC vine**
```
AddChildToVine {
    vine_slug: "{cc_vine_slug}",
    child_slug: "{cc_vine_slug}-conversations",
    position: 0,
    child_type: "bedrock",
}
```

**Op 4: DADBEAR config for the conversation pyramid**
```
RegisterDadbearConfig {
    slug: "{cc_vine_slug}-conversations",
    source_path: cc_dir_absolute_path,
    content_type: "conversation",
    scan_interval_secs: config.default_scan_interval_secs,
}
```

**Op 5 (optional, only if memory subfolder has at least one `.md`): Create the memory doc bedrock**
```
CreatePyramid {
    slug: "{cc_vine_slug}-memory",
    content_type: ContentType::Document,
    source_path: "{cc_dir_absolute_path}/memory",
}
```

**Op 6 (optional): Add memory doc pyramid as bedrock child of CC vine**
```
AddChildToVine {
    vine_slug: "{cc_vine_slug}",
    child_slug: "{cc_vine_slug}-memory",
    position: 1,
    child_type: "bedrock",
}
```

**Op 7 (optional): DADBEAR config for the memory pyramid**
```
RegisterDadbearConfig {
    slug: "{cc_vine_slug}-memory",
    source_path: "{cc_dir_absolute_path}/memory",
    content_type: "document",
    scan_interval_secs: config.default_scan_interval_secs,
}
```

**Op 8: Attach the CC vine to the root target folder's vine**

The existing plan attaches the CC result (formerly the single RegisterClaudeCodePyramid slug) as a bedrock child of the root vine. Change this to attach the CC vine as a VINE child of the root vine — per Phase 16's `child_type='vine'` composition:
```
AddChildToVine {
    vine_slug: root_target_vine_slug,
    child_slug: cc_vine_slug,
    position: (next available),
    child_type: "vine",  // not "bedrock"
}
```

**Key invariant:** the CC vine sits alongside real folder children (`src`, `docs`, etc.) in the root vine, with `child_type='vine'`. Its own children (the conversation bedrock + optional memory bedrock) compose within it.

### 3. Retire or deprecate `RegisterClaudeCodePyramid`

You have two options:

**Option A:** Delete `IngestionOperation::RegisterClaudeCodePyramid` entirely. Replace with the mini-subplan pattern above. Cleaner long-term; larger diff.

**Option B:** Keep `RegisterClaudeCodePyramid` as a deprecation shim that, when encountered in `execute_plan`, expands into the new mini-subplan at execute time. Smaller diff; carries legacy name.

**Recommendation: Option A.** Phase 17 is <2 weeks old, no external callers. Delete the variant and update the 3-5 call sites.

If you pick A: update all tests that reference the variant, including the `extract_build_dispatches` function at `folder_ingestion.rs:1959` that partitions ops into leaves vs vines.

### 4. First-build dispatch for the new CC vines

Phase 17's wanderer fix (`spawn_initial_builds`) dispatches first builds for every non-vine leaf in the plan (via `prepopulate_chunks_for` + `spawn_question_build`), then dispatches first builds for vines after a 2-second settle delay.

Your restructure changes the inventory:
- Before: N CC pyramids (leaves) per target folder
- After: N CC vines (vines) + N conversation bedrocks (leaves) + M memory bedrocks (leaves)

Verify `extract_build_dispatches` correctly partitions the new ops into leaves vs vines so `spawn_initial_builds` dispatches each correctly:
- Conversation bedrocks → leaves → pre-pop via `ingest_conversation` on the jsonls → dispatch first build
- Memory doc bedrocks → leaves → pre-pop via `ingest_docs` on the markdown files → dispatch first build
- CC vines → vines → no pre-pop → dispatch topical vine chain after settle

**Conversation pre-pop still has the Phase 0b single-jsonl limitation.** That's NOT your scope to fix — Phase 0b's chunk-collision needs per-file `chunk_offset` threading. Document it as inherited from Phase 17 and move on. The memory doc bedrock IS additive value that lands today, even with the conversation pyramid still only consuming the latest jsonl.

### 5. Frontend: preview updates

In `AddWorkspace.tsx` folder ingestion wizard, extend the preview modal's plan summary section. The current rows include "CC pyramids: N". Change to:

```
CC vines:            N
  Conversation beds: N
  Memory doc beds:   M  (if any memory subfolders were found)
```

And update the CC conversation directory list rendering (which shows per-dir metadata like jsonl count) to also show the memory md count when `has_memory_subfolder = true`:

```
- -Users-adam-foo-bar [main] (215 jsonl, 8 memory md)
```

### 6. Tests

Rust tests:
- `test_find_cc_dirs_populates_memory_subfolder_metadata` — temp dir with a memory/ subfolder containing .md files; assert the metadata fields populate correctly
- `test_find_cc_dirs_memory_absent_returns_false` — CC dir without memory/; assert `has_memory_subfolder = false`
- `test_plan_generates_cc_vine_plus_conversation_bedrock` — CC dir with jsonls only; assert the plan has CreateVine + CreatePyramid(conversation) + AddChildToVine(bedrock) + DADBEAR config, all attached to root vine as child_type=vine
- `test_plan_generates_cc_vine_plus_both_bedrocks_when_memory_present` — CC dir with jsonls + memory/*.md; assert the plan has CreateVine + 2 CreatePyramid + 2 AddChildToVine(bedrock) + 2 DADBEAR configs
- `test_plan_skips_memory_bedrock_when_no_md_files` — memory/ subfolder exists but empty; assert no memory bedrock created
- `test_extract_build_dispatches_partitions_cc_vines_and_bedrocks_correctly` — verify leaves/vines split for the new op shape

Frontend tests (if runner exists): wizard preview renders the new counts.

### 7. Ingestion prompts

Since the memory doc bedrock uses the existing document content-type ingest chain, no new prompts needed. Memory .md files flow through the same `ingest_docs` → document chain as any other doc bedrock.

The conversation bedrock is unchanged from Phase 17's behavior.

The CC vine uses Phase 16's `topical-vine.yaml` chain unchanged — it composes the conversation bedrock's apex + memory doc bedrock's apex into a single CC-scoped view. No new prompts.

## Scope boundaries

**In scope:**
- Extended `ClaudeCodeConversationDir` with memory metadata
- `find_claude_code_conversation_dirs` / `describe_claude_code_dirs` populate memory fields
- `plan_ingestion` / `plan_recursive` emit the mini-subplan per CC dir
- `IngestionOperation::RegisterClaudeCodePyramid` retired (Option A) or shimmed (Option B)
- `extract_build_dispatches` handles the new op shape
- `spawn_initial_builds` dispatches first builds for conversation + memory bedrocks + CC vines
- `AddWorkspace.tsx` wizard preview shows memory bedrock counts
- Rust tests for all of the above
- Implementation log entry

**Out of scope (other Phase 18 workstreams):**
- Local mode toggle (18a)
- Cache retrofits (18b)
- Privacy opt-in + pause-all scoping (18c)
- Schema migration UI (18d)

**Out of scope permanently (inherited from Phase 17):**
- Fixing the Phase 0b chunk-collision that limits conversation pyramids to the most recent jsonl. That's a separate fix at the `ingest_conversation` layer requiring a `chunk_offset` parameter. Document in the log as inherited limitation.
- Deep memory file scanning (e.g., reading each .md's frontmatter). The document chain handles whatever shape the user's memory .md files have; you don't need to parse them.
- Multi-level memory structures (e.g., `memory/<topic>/*.md` subfolders inside the memory/ folder). If a user has that, the document chain walks it naturally via the existing folder walk. Verify by reading `ingest_docs` briefly and documenting either "handles recursive memory/" or "reads memory/ top-level only."

## Verification criteria

1. **Rust clean:** `cargo check --lib` — 3 pre-existing warnings, zero new.
2. **Test count:** `cargo test --lib pyramid` at prior count + new Phase 18e tests.
3. **Frontend build:** `npm run build` clean.
4. **Plan shape verification:** document in the log a manual trace — given a target folder with one CC dir that has both jsonls and memory/*.md, list the exact operations the plan emits. Compare against the 7-op template in section 2 above.
5. **Manual verification path documented:**
   - Start the built app
   - Ingest a real folder that has Claude Code conversations AND the CC dir has a memory/ subfolder (Adam can use `/Users/adamlevine/AI Project Files/agent-wire-node` which likely has both)
   - Observe the wizard preview: should show "CC vines: N, Conversation beds: N, Memory doc beds: M"
   - Click Start Ingestion
   - Observe the resulting slugs: should see `{target}-cc-{encoded}`, `{target}-cc-{encoded}-conversations`, and `{target}-cc-{encoded}-memory`
   - Verify in SQLite that `pyramid_vine_compositions` has the right rows: root vine → CC vine (child_type='vine'), CC vine → conversation (child_type='bedrock'), CC vine → memory (child_type='bedrock')

## Deviation protocol

- **Option A vs B for retiring `RegisterClaudeCodePyramid`:** pick A (clean) unless scope pressure. Document.
- **`ingest_docs` on memory/ subfolder recursion:** verify once, document the answer.
- **Phase 0b chunk-collision inherited limitation:** explicitly documented as out of scope. Do NOT try to fix it.
- **Slug collision handling:** if `{cc_vine_slug}-conversations` collides with an existing slug (unlikely but possible), use the existing collision-suffix resolver.

## Mandate

- **`feedback_always_scope_frontend.md`:** the wizard preview must show the new memory bedrock count. Adam tests by feel — if the preview still says "CC pyramids: N" after your work, the phase failed visibly.
- **Use Phase 16's `child_type='vine'` composition.** Do NOT flatten the CC vine into the root vine's bedrock list. The CC vine must be a `child_type='vine'` child of the root vine, with its own bedrock children underneath.
- **Test every op shape.** Your new mini-subplan has 7 op types; every one of them should be exercised by at least one test.
- **No Pillar 37 violations.** No hardcoded memory file limits, no hardcoded file extensions beyond the existing heuristic config.

## Commit format

Single commit on `phase-18e-cc-memory-subfolder`:

```
phase-18e: CC dir → vine with conversation + memory bedrocks

<5-8 line body summarizing:
- ClaudeCodeConversationDir extended with memory metadata
- plan_ingestion emits mini-subplan per CC dir (CreateVine + 1-2 CreatePyramid + AddChildToVine)
- CC vine attaches to root vine as child_type='vine' (Phase 16 composition)
- RegisterClaudeCodePyramid retired / shimmed
- AddWorkspace wizard preview shows memory bedrock counts
- Claims D1 from deferral-ledger.md Discovered-by-use section>
```

Do NOT amend. Do NOT push. Do NOT merge.

## Implementation log

Append Phase 18e entry to `docs/plans/pyramid-folders-model-routing-implementation-log.md`:
1. New op shape (7 ops per CC dir)
2. Option A or B for RegisterClaudeCodePyramid retirement
3. Extract_build_dispatches change
4. Frontend preview updates
5. Tests added
6. Manual verification for the trace-through-plan test
7. Inherited Phase 0b single-jsonl limitation noted
8. Status: `awaiting-verification`

## End state

Phase 18e is complete when:
1. Each CC dir in a folder ingestion plan emits a vine + 1-2 bedrocks + their DADBEAR configs + root composition
2. Memory `.md` files are ingested as a document bedrock when present
3. The CC vine attaches to the root folder vine as `child_type='vine'`
4. First-build dispatch runs for each new pyramid + vine
5. `cargo check --lib` + `cargo test --lib pyramid` + `npm run build` clean
6. Single commit on branch `phase-18e-cc-memory-subfolder`

Begin with the spec Part 2 enhancement section + Phase 16 vine composition primitives + Phase 17's current CC handling code. Then restructure the plan emission. Then update execute + dispatch. Then frontend preview. Then tests.

Good luck. This is the one Adam discovered by using the app — ship it right.
