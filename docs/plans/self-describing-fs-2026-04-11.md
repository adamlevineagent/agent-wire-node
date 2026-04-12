# Self-Describing Filesystem — Maximal Build

**Date:** 2026-04-11
**Author:** Claude (session with Adam)
**Branch:** `stabilize-main` (continuing on the same branch; SDFS supersedes the stabilize-main patch approach)
**Checkpoint:** `a6eb1ae` on `stabilize-main-checkpoint-20260411-124944`
**Supersedes:** `docs/plans/stabilize-main-2026-04-11.md` committed at `efec5c0` (stabilize-main plan is preserved as historical context; its bug findings inform this plan but its fix approach is abandoned)
**Status:** Rev 3 — conversation pipeline sections rewritten after Adam corrected the "follow-up" punt. Conversation ingest is first-class MVP; `ingest_continuation` is wired up in Commit 8; filemap is the message-count cursor; orphan-bunch bug fixed as side effect. Ready for Cycle 1 audit.

---

## TL;DR

After three audit cycles and a diverging complexity curve, the stabilize-main patch approach was abandoned in favor of the architectural pivot Adam had been directionally committed to: the Self-Describing Filesystem. Files in `.understanding/` become the canonical store; SQLite becomes a pure derived cache rebuildable from the files at any time. Every document is a Wire Native Document with YAML rear-matter. Local documents cite each other via `{ doc: workspace-relative-path }`. Handle-paths are assigned exclusively at publication time by `insert_contribution_atomic()` per the canonical spec — there is no local allocator, no pre-allocation, no migration sweeper. DADBEAR the pattern is retargeted to watch source files, write updates to filemap scanner fields, and auto-fire rebuilds via the `stale_local` tier (local compute) after pure debounce — the expensive parts go through local Ollama by default.

MVP scope: eight commits that make folder ingest on `agent-wire-node/` produce a queryable pyramid via the new architecture. Publish pipeline, handle registration onboarding, and Wire-native distribution of bundled configs are all follow-ups after the local workflow is proven.

---

## Context — How We Got Here

Adam reported 65 failed folder ingest builds on `agent-wire-node/` this morning. I spent many hours running three audit cycles on a patch approach (`stabilize-main`) that grew from 3 bugs to 4 to 6, and each Cycle 3 finding was deeper architectural (dual resolver paths, decorative tier routing, `walk_bundled_contributions_manifest` not calling sync, nested `unchecked_transaction` failing at runtime, 5 conversation chain YAMLs not in the release bundle, etc.). Each patch revealed a deeper structural problem. The plan was diverging.

Adam's call: stop patching, pivot to the full Self-Describing Filesystem architecture. His rationale was short and correct — "it's already well broken now" and "the true goal is to get it to the point where I can use my local compute to process all my conversations and folders." The complexity of patching an SQLite-centric architecture while knowing you want files-as-canonical is worse than doing it right once.

Two subagents did research before drafting this plan:
- **DADBEAR pattern** — read `dadbear_extend.rs`, `stale_engine.rs`, docs, and grepped comments. DADBEAR = Detect/Accumulate/Debounce/Batch/Evaluate/Act/Recurse. Pipeline A (maintenance) and Pipeline B (creation) exist today. RAII guard prevents panic stickiness; claim semantics limit `batch_size` as a cap not a multiplier; lock ordering is load-bearing.
- **Wire Native Document format** — read `wire-native-documents.md`, `wire-handle-paths.md`, `wire_publish.rs`, and the handoffs. YAML rear-matter at the END (prose primacy), `{ ref / doc / corpus }` three-form references, `insert_contribution_atomic()` is the sole handle-path allocator, Wire Time = UTC-7 fixed no DST, epoch 2026-01-01 WT.

Adam then wrote a handback (`handoff-2026-04-11-handle-paths-publish-time-only.md`) correcting my pre-allocation framing: handle-paths are publish-time only, local docs cite each other by file path via the canonical `{ doc: relative-path }` form, no allocator is needed. I had been proposing to invent an endpoint and a table that the canonical spec already made unnecessary.

This plan absorbs all of that.

---

## Core Architectural Facts (verified)

1. **`insert_contribution_atomic()` is the sole handle-path allocator** (`GoodNewsEveryone/supabase/migrations/20260320100000_ux_pass_foundation.sql`). Handle-paths are computed at publication time via `generate_daily_seq(agent_id, epoch_day)` serialized by `pg_advisory_xact_lock(737, hashtext(...))`. The deprecated TypeScript `generateHandlePath()` at `src/lib/server/wire-handle-paths.ts:59` explicitly warns against client-side replication.

2. **Wire Time is UTC-7, fixed, no DST.** Epoch = 2026-01-01 00:00:00 WT = 2026-01-01T07:00:00Z UTC. Formula: `epoch_day = floor((now_utc_ms - WIRE_EPOCH_UTC_MS) / 86_400_000)`. Today (2026-04-11) is Wire epoch_day 100.

3. **Three legal reference forms** in `derived_from` per `wire-handle-paths.md:60-68`:
   - `{ ref: "nightingale/77/3" }` — handle-path (published contributions)
   - `{ doc: wire-actions.md }` — file path (local corpus docs, workspace-relative)
   - `{ corpus: "wire-docs/wire-actions.md" }` — corpus path (remote corpus docs)
   All three resolve to internal UUIDs at publish time. Nobody types a UUID.

4. **`handle_path` is NOT a rear-matter field** in `wire-native-documents.md`. Documents don't self-identify with a handle; Wire assigns one at insertion time. Local files remain without `handle_path`. After publication, a `local.published_as: { handle_path, published_at, published_by_build_id, signed_proof_hash }` field is added to the published version file so the publication is git-visible and queryable without touching Wire. Publication state lives in `local:` (not `wire:`) because `wire:` is what gets sent to the server on publish — putting `published_as` in `wire:` would be either a paradox or noise the server strips. `local:` is already the "operational state, stripped at publish" block by spec.

5. **DADBEAR is a pattern, not a subsystem.** The existing `dadbear_extend.rs` implementation is Pipeline B of that pattern. Under SDFS, DADBEAR continues to watch source files (that's unchanged); what changes is that it writes filemap scanner fields instead of `pyramid_ingest_records`, and it reads policy from contribution files instead of a SQLite config table.

6. **Chain dispatch has two resolvers** — `resolve_ir_model` (IR path, `chain_dispatch.rs:1023`) called from 7 sites in `chain_executor.rs`, and `resolve_model` (legacy path, `chain_dispatch.rs:186`) called via `dispatch_llm → dispatch_step → dispatch_with_retry` with 29 occurrences. Adam's `use_ir_executor: false` puts him on the legacy path today. Both resolvers need to read tier routing from the cache in Commit 7.

7. **`pyramid_tier_routing` table is decorative today** because neither `resolve_ir_model` nor `resolve_model` consults it. Tier routing is therefore orphaned data. Under SDFS, tier routing lives in `.understanding/configs/tier_routing.md` as a contribution file, and the SQLite cache is hydrated from it on boot. Resolvers read the cache. The table stops being decorative and starts being an actual cache.

8. **`walk_bundled_contributions_manifest` inserts but never syncs** for six schema types (`build_strategy`, `dadbear_policy`, `evidence_policy`, `folder_ingestion_heuristics`, `tier_routing`, `custom_prompts`). Five of them actually reach operational tables via the `.understanding/` canonical file path; `dadbear_policy` is a global no-op and its bundled entry should either be fixed or dropped. Under SDFS this entire code path becomes a boot-time "read bundled config files, write to `.understanding/configs/` if not present" bootstrap — much simpler.

9. **Conversation ingest + chronological vines already exist, and are first-class.** Not a future extractor. Verified in the existing code:
   - `ingest::ingest_conversation` at `ingest.rs:350` parses JSONL, filters user/assistant messages, labels PLAYFUL/CONDUCTOR with timestamps, chunks at ~100 lines.
   - `pyramid_find_claude_code_conversations` at `folder_ingestion.rs:599+` auto-discovers `~/.claude/projects/<encoded-path>/` dirs via Pattern A (encoded-subdir match) + Pattern B (direct `.jsonl` folder), with the Phase 18a path-encoding fix so paths containing spaces work.
   - Folder ingestion Phase 17/18e creates a CC vine per CC dir with `CreateVine`, a conversation bedrock per dir with `CreatePyramid(content_type='conversation')`, optionally a memory bedrock from any `memory/` subfolder, and `RegisterDadbearConfig` for each.
   - `vine_bunches` table at `db.rs:779-799` stores the chronological structure: one row per session with `session_id`, `jsonl_path`, `bunch_index`, `first_ts`, `last_ts`, `message_count`, `chunk_count`, `apex_node_id`, `penultimate_node_ids`. Bunches are ordered by `first_ts ASC` with `session_id` as tiebreaker. Each bunch = one Claude Code session = one bedrock under the CC vine.
   - `vine.rs:877-1123` builds bunches: pre-creates the bunch slug, dispatches `ingest_conversation`, then `run_build_from_with_evidence_mode` via the conversation-episodic chain (5 prompts at `chains/prompts/conversation-episodic/`: chronological_decompose, combine_l0, forward, reverse, synthesize_recursive).
   - DADBEAR Pipeline B at `dadbear_extend.rs:702+` already handles `ContentType::Conversation` and dispatches `fire_ingest_chain` → `clear_chunks` → `ingest_conversation` → full chain rebuild.

   **SDFS's job is not to introduce conversation ingest.** It already works. SDFS's job is to hoist the vine-bunch state out of SQLite into `.understanding/folder.md.conversation_sources[].sessions[]`, close the incremental-append gap (below), and fix the orphan-bunch bug (below) as a side effect.

10. **`ingest_continuation` exists but has ZERO call sites.** `ingest.rs:384` defines the incremental-append primitive (skips the first N messages, writes a `"continuation"` batch with `chunk_offset = count_chunks(slug)`, appends chunks with offset). Every live call site — Pipeline B, the wizard, `vine.rs` bunch building — uses `ingest_conversation` instead, which means every conversation file change triggers a `clear_chunks + full re-ingest + full chain rebuild`. This is not an architectural choice; it's a Phase 0b shortcut. The `dadbear_extend.rs:731-734` comment names the reason: "Pipeline B's ingest record schema doesn't track per-file message offsets — the message count it would need for `skip_messages` isn't stored anywhere." The state gap is real: `pyramid_ingest_records` has file hash + mtime but no message count.

    **SDFS closes this gap for free.** The cursor lives in the filemap, not in a new SQLite table. Each `conversation_sources[].sessions[].ingested_message_count` IS the `skip_messages` value. DADBEAR reads it from the filemap, calls `ingest_continuation`, and writes the new total back to the filemap on success. No schema migration, no new table, no new code path — just wire the existing `ingest_continuation` primitive into the new flow.

11. **Orphan-bunch bug: new `.jsonl` in already-ingested CC dir doesn't auto-attach to the vine.** When a new Claude Code session writes a fresh `.jsonl` under a CC directory whose parent vine already exists, DADBEAR's `detect_changes` marks it as a new file and Pipeline B dispatches `fire_ingest_chain` for it, but the resulting pyramid is never attached to the existing CC vine via `AddChildToVine`. The user has to re-run the wizard for the orphan to get hooked in. SDFS fixes this: the scanner writes new sessions into `conversation_sources[].sessions[]` with `bunch_slug: null` and `ingested_message_count: 0`; Commit 5's builder sees `bunch_slug: null` on its next pass, creates the bunch, attaches it to the parent CC vine via the existing `AddChildToVine` op, and writes the assigned slug back into the filemap. Zero orphans.

---

## Core Architecture

### `.understanding/` layout (full spec surface)

Every folder being ingested gets a `.understanding/` subdirectory with this shape:

```
<folder>/.understanding/
├── folder.md                     # folder-level filemap (Wire Native Document shape)
├── nodes/
│   ├── F-L0-042/                 # folder-node directory with version history
│   │   ├── v1.md
│   │   ├── v2.md
│   │   ├── v3.md
│   │   ├── current.md            # physical copy of v3.md; scanner verifies sha256 parity
│   │   └── refinements/
│   │       ├── v1-to-v2.md
│   │       └── v2-to-v3.md
│   ├── C-L0-023/                 # code bedrock
│   │   ├── v1.md
│   │   └── current.md
│   └── ...
├── edges/
│   ├── <uuid-1>.md               # one edge per file, YAML rear-matter only
│   ├── <uuid-2>.md
│   └── ...
├── evidence/
│   ├── <uuid-1>.md               # one evidence link per file
│   └── ...
├── configs/
│   ├── dadbear_policy.md         # per-folder DADBEAR policy override (optional)
│   ├── folder_ingestion_heuristics.md  # per-folder ignore patterns, size caps, etc.
│   ├── tier_routing.md           # per-folder tier assignments (optional)
│   └── ...
└── cache/
    └── llm_outputs/
        ├── ab/
        │   └── abc123.../
        │       ├── call.md       # request metadata as rear-matter
        │       └── response.md   # raw LLM response body
        └── cd/...
```

**User-global layer** at `~/Library/Application Support/wire-node/.understanding/`:
- Same shape as per-folder.
- Holds user-level defaults for configs (copied from bundled app resources on first run).
- Inheritance: **folder > user-global > bundled**. Folder overrides user-global; user-global overrides bundled. First value found per-field wins (cascading merge).

**Bundled defaults** shipped in the app resource bundle at `src-tauri/resources/.understanding/`:
- Canonical defaults for all six config schema types.
- On first run, copied to user-global `~/Library/Application Support/wire-node/.understanding/configs/` if not present.
- Future updates via Wire-native distribution (v2 follow-up).

### Document format (Wire Native Document shape, uniform across all `.understanding/` files)

Body-first content, `---` fence, YAML rear-matter at the end split into `wire:` (canonical Wire Native Document fields) and `local:` (filesystem operational state). The `wire:` block is what gets published to Wire if the document is ever published; `local:` is stripped at publish time.

Full examples for each file type are in the **File Format Specifications** section below.

### Workspace-relative paths

Adam confirmed: all local `{ doc: path }` citations use **workspace-root-relative paths**. The workspace is the top-level folder the user pointed at for ingestion — e.g., `/Users/adamlevine/AI Project Files/agent-wire-node` is a workspace. A node at `src-tauri/.understanding/nodes/C-L0-023/v3.md` citing a node at `src/.understanding/nodes/D-L0-017/v1.md` uses the reference `{ doc: "src/.understanding/nodes/D-L0-017/v1.md", weight: 0.5 }`.

Workspace-root is determined once at ingest time and stored in the filemap's `local.workspace_root_path` field. All subsequent citations use that anchor. Re-anchoring (moving the workspace) is a one-shot rewrite pass; not MVP scope.

Cross-workspace citations are not in MVP. If needed later, they go through published handle-paths (publish the citing doc, cite the other workspace's doc via its handle-path).

### Handle-path model — publish-time only

Per Adam's handback:
- **Local documents never have handle-paths.** No `handle_path` field in the rear-matter, no local allocator, no pre-allocation.
- **Local documents cite each other via `{ doc: workspace-relative-path }`.** Canonical form per `wire-handle-paths.md:60-68`.
- **Handle-paths are assigned at publish time** by `insert_contribution_atomic()` on the Wire backend, deterministically from `generate_daily_seq(agent_id, epoch_day)`. The node never computes a handle-path locally.
- **Publish pipeline (future follow-up):** walks the `derived_from` graph of a batch of documents to be published, topologically sorts, publishes in dependency order, rewrites `{ doc: path }` references to `{ ref: "handle/day/seq" }` for docs published in the same batch as the handles are assigned. Documents referencing local-only docs (not in the publish batch) retain their `{ doc: path }` form — legal per the canonical spec, stored as corpus-doc references on Wire.
- **Onboarding handle registration** is a follow-up requirement: user must have a registered Wire handle before any publish is possible. MVP is local-only, so onboarding can ship later.

### DADBEAR retarget (file-centric)

**What watches:** source files in the user's workspace (unchanged from today).

**What writes:** `.understanding/folder.md` scanner fields in affected folders. Specifically the `files[].dadbear_stale`, `files[].mtime`, `files[].sha256`, and the folder-level `dadbear:` block (`last_change_detected_at`, `folder_quiet_since`).

**What triggers:** pure debounce — folder must be quiet for `debounce_quiet_secs` before firing. No `batch_threshold` counter-based override (per Adam's correction). During active editing, the timer keeps resetting; DADBEAR only fires after the user stops touching files for the full window.

**What fires:** the builder, in `auto_fire` mode by default. The builder dispatches chain builds for stale entries via the `stale_local` tier (local compute via Ollama) so the freshness work doesn't burn cloud credits.

**Policy source:** `.understanding/configs/dadbear_policy.md` — inheritance folder > user-global > bundled. Every field is overridable per-folder.

**Pattern stages mapped to SDFS code:**
- **Detect:** `scan_folder()` walks the source directory, computes hash/mtime, compares to `folder.md` scanner fields.
- **Accumulate:** rewrite `folder.md` with updated `files[]` and folder-level `dadbear:` state.
- **Debounce:** the tick loop checks `folder_quiet_since` vs `debounce_quiet_secs` on each scan; only fires when the quiet window has elapsed.
- **Batch:** collected set of `dadbear_stale: true` entries from the filemap form the batch; `max_files_per_rebuild` is a ceiling on batch size.
- **Evaluate:** policy says `auto_rebuild: auto_fire` (default) → dispatch. Alternative options `mark_stale_only` and `scheduled` remain available as per-folder overrides for users who want passive observation or cron-gated rebuilds.
- **Act:** invoke the builder, which reads the filemap and dispatches chain builds via the existing chain executor retargeted to write node files.
- **Recurse:** parent folder's `folder.md` has its own `dadbear:` block updated when a child folder's build completes. Upward vine-stale propagation is automatic via the filemap's `child_folder_node_ids` back-reference.

### SQLite as derived cache

The existing SQLite `pyramid_nodes`, `pyramid_edges`, `pyramid_evidence`, `pyramid_tier_routing`, and related tables are preserved but reclassified: they are **a cache**, not a source of truth. `.understanding/` is canonical.

**Cache invariants:**
- SQLite can be deleted at any time. On next boot, the system walks `.understanding/` directories for every registered workspace and rebuilds the cache in place.
- Writes fan out: every mutation goes to the file first, then to the SQLite cache. Cache is write-through. If a write succeeds to file but fails to cache, the cache can be rebuilt from the file on next boot.
- Queries hit the cache. Cache hits return fast; cache misses fall through to filesystem reads.
- On `.understanding/` file change (via DADBEAR or direct edit), the cache entry for that file is invalidated and refreshed from disk.

**Rebuild-from-filesystem:** a new `cache::rebuild_from_understanding(workspace_roots: &[Path]) -> Result<RebuildReport>` function walks each workspace's `.understanding/` trees, parses every document, and writes SQLite rows matching the current cache schema. Runs on boot if SQLite is missing or if a schema version mismatch is detected. Tests verify idempotency (rebuild twice, second is a no-op) and recovery (delete SQLite, rebuild produces the same query results).

### LLM output cache (content-addressable)

`.understanding/cache/llm_outputs/<sha256-prefix>/<sha256-full>/` — two-level prefix sharding on the request hash. Each cache entry:
- `call.md` — body is human-readable summary ("OpenRouter Mercury 2 call for fast_extract step of code-v2 chain"), rear-matter has structured metadata (request hash, model, tier, cost, duration, first_built_at, last_hit_at, hit_count, produced_by_node_ids).
- `response.md` — raw LLM response text (plaintext, no wrapping).

Request hash is a stable composition of `(model_id, tier_name, system_prompt_sha, user_prompt_sha, temperature, max_tokens)`. Any two nodes that make the same request hit the same cache entry.

Cache is purely additive; no GC in MVP. A separate sweep can later age out entries by `last_hit_at`.

---

## File Format Specifications

### `.understanding/folder.md` — filemap

```markdown
# src-tauri/

(AI-written observations. Humans don't edit this file directly in practice.
 This body is optional; the canonical folder state is in the rear-matter.)

This folder is the Rust backend for Wire Node. Key modules:
- `pyramid/db.rs` — all SQLite access (~15k lines, over-grown)
- `pyramid/chain_executor.rs` — chain dispatch, excluded until split

---
wire:
  destination: corpus
  corpus: local
  contribution_type: template
  scope: unscoped
  topics: [folder-state, wire-node-backend, rust]
  entities:
    - { name: "src-tauri/", type: folder, role: subject }
  maturity: canon
  sync_mode: manual
  derived_from: []

local:
  spec_version: 1
  schema_type: folder_filemap
  workspace_root_path: /Users/adamlevine/AI Project Files/agent-wire-node
  folder_relative_path: src-tauri
  folder_node_id: F-L0-042
  parent_folder_node_id: F-L0-041
  root_vine_node_id: V-L3-000
  scanned_at: 2026-04-11T20:30:00Z
  scanner_version: 1
  file_hash_algorithm: sha256

  coverage_ratio: 0.78
  child_folder_node_ids: [F-L0-043, F-L0-044]
  content_type_counts:
    code: 45
    document: 12
    conversation: 0
    uncovered: 7

  dadbear:
    last_change_detected_at: 2026-04-11T19:30:00Z
    folder_quiet_since: 2026-04-11T19:30:00Z
    last_build_triggered_at: 2026-04-11T18:00:00Z
    last_build_id: B-2026-04-11-123
    last_build_trigger_reason: "debounce_quiet_secs elapsed (300s)"

  files:
    - path: src/main.rs
      size_bytes: 45382
      sha256: abc123...
      mtime: 2026-04-10T08:15:00Z
      detected_content_type: code
      scanner_suggestion: include
      dadbear_stale: false
      last_built_at: 2026-04-11T18:45:00Z
      last_build_node_id: C-L0-001
      last_build_node_ref: "nodes/C-L0-001/v3.md"  # workspace-relative
      last_build_cost_usd: 0.0012
      last_build_status: ok
      user_included: true
      user_content_type_override: null
      user_notes: null

    - path: src/pyramid/db.rs
      size_bytes: 627391
      sha256: def456...
      mtime: 2026-04-11T18:12:00Z
      detected_content_type: code
      scanner_suggestion: include
      dadbear_stale: true                       # modified since last build
      last_built_at: 2026-04-11T10:00:00Z
      last_build_node_id: C-L0-023
      last_build_node_ref: "nodes/C-L0-023/v2.md"
      last_build_cost_usd: 0.0089
      last_build_status: ok
      user_included: true
      user_notes: "Most code touches this file; consider splitting."

  excluded_by_pattern:
    - { path: target/, pattern: "target/" }
    - { path: .git/, pattern: ".git/" }

  excluded_by_size:
    - { path: build.log, size_bytes: 8421170, max_allowed: 1048576 }

  unsupported_content_type:
    - { path: screenshot-auth-flow.png, detected_content_type: image/png, extractor_available: false }

  failed_extraction: []

  tombstoned:
    - path: src/pyramid/legacy_stale.rs
      deleted_at: 2026-03-22T11:00:00Z
      last_known_sha256: old123...
      last_build_node_id: C-L0-019

  conversation_sources:
    # Each source is a Claude Code `~/.claude/projects/<encoded-path>/` directory
    # discovered via pyramid_find_claude_code_conversations() against this folder's
    # encoded path (folder_ingestion.rs:599+). Discovered once at scan time, kept in
    # sync by DADBEAR.
    - canonical_path: /Users/adamlevine/.claude/projects/-Users-adamlevine-AI-Project-Files-agent-wire-node/
      source_type: claude_code_dir
      encoded_path: -Users-adamlevine-AI-Project-Files-agent-wire-node
      is_main: true
      is_worktree: false

      # CC vine structure for this source. Created by Commit 5 builder on first build.
      cc_vine_slug: agent-wire-node-cc-1
      memory_subfolder_path: /Users/adamlevine/.claude/projects/-Users-adamlevine-AI-Project-Files-agent-wire-node/memory
      memory_bedrock_slug: agent-wire-node-cc-1-memory   # null if memory/ is absent or empty

      # Per-session state. One entry per .jsonl file in the CC dir. Each session = one
      # conversation bunch = one bedrock under the CC vine. This is the authoritative
      # INCREMENTAL INGEST CURSOR — `ingested_message_count` is the skip_messages value
      # passed to ingest_continuation() on the next dispatch.
      sessions:
        - session_id: 01h8k2e3f4m5p6q7r8s9t0v1w2
          jsonl_path: 01h8k2e3f4m5p6q7r8s9t0v1w2.jsonl    # relative to canonical_path
          bunch_slug: agent-wire-node-cc-1--bunch-000
          bunch_index: 0
          first_ts: 2026-04-10T14:23:00Z
          last_ts: 2026-04-11T09:45:00Z

          # Change-detection fields (DADBEAR compares these against fresh filesystem reads)
          last_seen_mtime: 2026-04-11T09:45:00Z
          last_seen_sha256: abc123...
          last_seen_file_size: 145230

          # Incremental ingest cursor. On next dispatch, DADBEAR passes this as
          # skip_messages to ingest_continuation(). Updated atomically after each
          # successful continuation ingest. Starts at 0 for new sessions.
          ingested_message_count: 127
          ingested_chunk_count: 6

          # Build state for the bunch pyramid
          last_built_at: 2026-04-11T09:50:00Z
          last_build_node_id: B-2026-04-11-cc-001
          last_build_status: ok
          last_build_cost_usd: 0.0073

        # New session detected but not yet built — bunch_slug is null, cursor is 0.
        # Commit 5 builder sees this on next pass: creates bunch, attaches to CC vine
        # via AddChildToVine (existing op), runs episodic chain, writes bunch_slug
        # back into this entry. No orphan pyramid.
        - session_id: 01h8m3n4p5q6r7s8t9u0v1w2x3
          jsonl_path: 01h8m3n4p5q6r7s8t9u0v1w2x3.jsonl
          bunch_slug: null
          bunch_index: 1
          first_ts: 2026-04-11T20:10:00Z
          last_ts: 2026-04-11T20:15:00Z
          last_seen_mtime: 2026-04-11T20:15:00Z
          last_seen_sha256: def456...
          last_seen_file_size: 8420
          ingested_message_count: 0
          ingested_chunk_count: 0
          last_built_at: null

  build_history:
    - { built_at: 2026-04-11T20:45:00Z, build_version: 3, status: complete, cost_usd: 0.0421, duration_s: 142, nodes_updated: 23, trigger: dadbear_debounce_expired }

  refinement_log:
    - { from_version: 2, to_version: 3, refined_at: 2026-04-11T20:30:00Z, refined_by: "agent:dadbear", reason: "pyramid/db.rs modified, folder quiet >5min" }
---
```

### `.understanding/nodes/C-L0-023/v3.md` — node payload

```markdown
# SQLite writer module for Wire Node pyramid state

The `pyramid/db.rs` module serves as the authoritative SQLite write path for
all pyramid state in Wire Node. It holds the connection pool, schema migrations,
and the canonical CRUD helpers for pyramid_nodes, pyramid_edges, pyramid_evidence...

## Key invariants

- All writes pass through a single `writer` connection guarded by `Arc<Mutex<_>>`
- Schema migrations run on every boot; each migration has a `_migration_marker`
  sentinel row to guarantee one-shot execution
- ...

## Supersession

...

---
wire:
  destination: corpus
  corpus: local
  contribution_type: extraction
  scope: unscoped
  topics: [sqlite, pyramid_state, writer, wire-node, rust]
  entities:
    - { name: pyramid_nodes, type: table, role: subject }
    - { name: save_node, type: function, role: referenced }
    - { name: "Arc<Mutex<Connection>>", type: pattern, role: referenced }
  maturity: canon
  derived_from:
    - doc: "src-tauri/src/pyramid/db.rs"
      weight: 1.0
      justification: "source file (sha256:def456...)"
    - doc: "src-tauri/.understanding/nodes/C-L0-024/current.md"
      weight: 0.3
      justification: "dependent module"
  sync_mode: manual

local:
  spec_version: 1
  schema_type: pyramid_node
  node_id: C-L0-023
  depth: 0
  content_type: code
  headline: "SQLite writer module for Wire Node pyramid state"
  self_prompt: "What does this module do and how?"
  build_version: 3
  distilled_preview: "Handles all pyramid_nodes/edges/evidence writes..."

  source_refs:
    - type: source_file
      path: src-tauri/src/pyramid/db.rs      # workspace-relative
      sha256: def456...
      mtime: 2026-04-11T18:12:00Z
      chunk_range: "L1-L15000"

  provenance:
    model_tier: synth_heavy
    model_id: openrouter|minimax/minimax-m2.7
    provider_id: openrouter
    built_at: 2026-04-11T20:45:00Z
    build_id: B-2026-04-11-123
    cost_usd: 0.0089
    duration_s: 23.4
    extractor: code_v1
    chain_name: code-v2

  refinement:
    prior_version_path: "v2.md"              # sibling file
    refinement_note_path: "refinements/v2-to-v3.md"
    reason: "Added supersession subsystem"

  # published_as — absent on local-only docs. Populated by the publish pipeline
  # (future, out of MVP scope) after insert_contribution_atomic() assigns a handle-path.
  # When present, the v<N>.md file is considered immutable on Wire; new content
  # must go to v<N+1>.md and earn its own publish stamp.
  # Example after publish:
  # published_as:
  #   handle_path: playful/100/42
  #   published_at: 2026-04-11T21:00:00Z
  #   published_by_build_id: B-2026-04-11-publish-001
  #   signed_proof_hash: sha256:abc123...
---
```

`current.md` is a physical copy of `v3.md`; scanner verifies sha256 parity on boot and auto-repairs mismatches.

**Publication state is in `local:` not `wire:`.** The `wire:` block is what gets sent to the Wire backend on publish — putting `published_as` in `wire:` would be either a paradox (the contribution sending its own publication metadata back to Wire) or noise that the server has to strip. `local:` is already the "operational state, stripped at publish" block by spec, so publication metadata lives there.

### `.understanding/configs/dadbear_policy.md`

```markdown
# DADBEAR policy for this folder

Default: wait 5 minutes of folder-level quiet, then auto-rebuild via local compute.

(AI writes rationale here when a per-folder override is applied.)

---
wire:
  destination: corpus
  corpus: local
  contribution_type: template
  scope: unscoped
  topics: [dadbear, policy, freshness]
  maturity: canon
  sync_mode: manual

local:
  spec_version: 1
  schema_type: dadbear_policy

  # Cadence — how often DADBEAR wakes to check for changes
  scan_interval_secs: 30

  # Trigger — pure debounce, no batch threshold
  debounce_quiet_secs: 300                   # 5 min of folder quiet before firing

  # Dispatch
  max_files_per_rebuild: 25                  # ceiling on files processed per rebuild batch
  auto_rebuild: auto_fire                    # options: auto_fire | mark_stale_only | scheduled
  rebuild_tier: stale_local                  # local compute by default; override per folder

  # Vine hierarchy
  recurse_to_parent: true                    # upward stale propagation via parent folder's filemap
---
```

### `.understanding/configs/tier_routing.md`

```markdown
# Tier routing for this workspace

Maps tier names (used by chain YAMLs via `model_tier:`) to provider+model slugs.
Resolver reads this via the SQLite cache. Edit via the Settings UI (future) or
directly in this file.

---
wire:
  destination: corpus
  corpus: local
  contribution_type: template
  scope: unscoped
  topics: [tier-routing, provider, model]
  maturity: canon
  sync_mode: manual

local:
  spec_version: 1
  schema_type: tier_routing

  tiers:
    fast_extract:
      provider_id: openrouter
      model_id: inception/mercury-2
      context_limit: 120000
      notes: "Very fast, very cheap, smart enough for most extraction"

    web:
      provider_id: openrouter
      model_id: x-ai/grok-4.1-fast
      context_limit: 2000000
      notes: "2M context for whole-array relational work"

    synth_heavy:
      provider_id: openrouter
      model_id: minimax/minimax-m2.7
      context_limit: 200000
      notes: "Near-frontier, slow, inexpensive"

    stale_remote:
      provider_id: openrouter
      model_id: minimax/minimax-m2.7
      context_limit: 200000
      notes: "Same quality profile for upper-layer stale checks"

    stale_local:
      provider_id: ollama-local                # materializes when user enables local mode
      model_id: llama3.2:latest
      context_limit: 131072
      notes: "Local compute for DADBEAR freshness work (no cloud cost)"
---
```

### `.understanding/configs/folder_ingestion_heuristics.md`

Bundled defaults for ignore patterns, max file size, content type detection. Same shape as stabilize-main's `default_ignore_patterns()` but stored as a contribution file instead of Rust constant. User can override per-folder with additional patterns or different caps.

### `.understanding/edges/<uuid>.md` and `.understanding/evidence/<uuid>.md`

Slim individual YAML files, each ~20 lines of rear-matter only. Pure YAML (the body can be empty or a one-line description). Edge example:

```markdown
---
wire:
  destination: corpus
  corpus: local
  contribution_type: extraction
  scope: unscoped
  topics: [edge, web-link]
  maturity: canon
  sync_mode: manual

local:
  spec_version: 1
  schema_type: pyramid_edge
  edge_id: e-019abc...
  edge_type: web_edge
  from_node_ref: "nodes/C-L0-023/current.md"
  to_node_ref: "nodes/C-L0-024/current.md"
  weight: 0.7
  created_at: 2026-04-11T20:45:00Z
  built_by_build_id: B-2026-04-11-123
  rationale: "db.rs saves nodes that chain_executor produces"
---
```

Evidence files follow the same shape with `schema_type: evidence_link` and `claim_node_ref`/`source_node_ref` instead of from/to.

---

## Commit Structure (8 focused commits on `stabilize-main`)

Each commit is independently buildable and testable. Every commit advances the system state so bisect produces a clear story.

**Pre-flight (not a commit):**
- Back up `~/Library/Application Support/wire-node/pyramid.db` to `pyramid.db.pre-sdfs-backup-<timestamp>` and same for `.db-wal`/`.db-shm`. Full rollback path preserved. No other destructive action.

---

### Commit 1 — `sdfs: file format library (Wire Native Document parser + serializer + workspace-relative path helper)`

**Scope:** pure library. No runtime effect beyond making the format available to subsequent commits.

**Files:**
- `src-tauri/src/pyramid/understanding/mod.rs` — new module root.
- `src-tauri/src/pyramid/understanding/document.rs` — `WireNativeDocument` struct, parser (markdown body + YAML rear-matter), serializer.
- `src-tauri/src/pyramid/understanding/schema.rs` — enum + struct definitions for each `schema_type` (folder_filemap, pyramid_node, dadbear_policy, tier_routing, folder_ingestion_heuristics, pyramid_edge, evidence_link, llm_cache_entry).
- `src-tauri/src/pyramid/understanding/workspace_path.rs` — `WorkspaceRoot` type + helpers for computing relative paths and resolving `{ doc: path }` citations.
- `src-tauri/src/pyramid/understanding/version.rs` — current-file management: copy highest `v<N>.md` to `current.md`, verify sha256 parity, auto-repair.

**Tests:**
- Round-trip: parse a folder.md document, serialize, assert byte-equivalent.
- Schema-specific parsers reject malformed input with clear errors.
- Workspace-path helper: compute relative path between any two files, resolve `{ doc: rel-path }` back to absolute, handle nested `.understanding/` dirs.
- `current.md` parity check: creates + verifies + repairs.

**No LLM calls, no DB writes.** Foundation only.

---

### Commit 2 — `sdfs: scanner writes .understanding/folder.md for each scanned folder (code + docs + conversations)`

**Scope:** adapt `folder_ingestion::scan_folder` to write filemap files. Keep existing `folder_ingestion_heuristics` logic for pattern matching. Reuse the existing `pyramid_find_claude_code_conversations` + `describe_claude_code_dirs` path for conversation discovery — do NOT rewrite it. Idempotent — re-scanning an already-scanned folder merges scanner-owned fields into the existing filemap without touching user-owned fields.

**Files:**
- `src-tauri/src/pyramid/understanding/scanner.rs` — new `scan_and_write_filemap(workspace_root, folder_path, config)` function. Walks the folder, categorizes files, emits a `FolderFilemap` struct via the Commit 1 library, writes it to `<folder>/.understanding/folder.md`.
- `src-tauri/src/pyramid/understanding/merge.rs` — field-level merge of scanner-owned fields against an existing filemap, preserving user-owned fields.
- Tauri command `pyramid_sdfs_scan_folder(workspace_root, folder_path)` — IPC entry point so the UI can trigger a scan.

**Conversation-source population** (at top-level scan only, matching existing Phase 17/18e guard):
- Call `pyramid_find_claude_code_conversations(config, target_folder)` → `Vec<ClaudeCodeConversationDir>`.
- For each CC dir, compute the full set of `.jsonl` files and for each read its `session_id` + `first_ts` + `last_ts` + message count via the existing `scan_jsonl_metadata` (`vine.rs:251`) helper.
- Write to `conversation_sources[]` with one entry per CC dir and one `sessions[]` entry per `.jsonl`. New sessions (not yet in the filemap) get `bunch_slug: null` and `ingested_message_count: 0`. Existing sessions keep their `bunch_slug` + `ingested_message_count` intact — the scanner only refreshes the change-detection fields (`last_seen_mtime`, `last_seen_sha256`, `last_seen_file_size`). Builder (Commit 5) picks up the null-slug entries on its next pass.
- Memory subfolder detection: preserved from Phase 18e. If `<cc_dir>/memory/` exists with at least one `.md` file, write `memory_subfolder_path` + compute `memory_bedrock_slug` (leave null until builder creates it).
- Encoded path, `is_main`, `is_worktree` come directly from `ClaudeCodeConversationDir`.

**Tests:**
- Fresh scan on a test directory with a matching CC dir produces filemap with `conversation_sources[]` populated and `sessions[]` reflecting the jsonl files.
- Re-scanning after a new `.jsonl` appears adds a new `sessions[]` entry with `bunch_slug: null`; existing entries are unchanged.
- Re-scanning after an existing `.jsonl` is appended refreshes that session's `last_seen_mtime`/`sha256`/`file_size` but keeps `ingested_message_count` unchanged.
- Re-scanning after the user unchecks a file preserves `user_included: false`.
- Symlinks and hidden files behave per the ignore patterns.
- Fresh scan on a folder with NO matching CC dir produces filemap with empty `conversation_sources[]`.

**Still no LLM calls.** Scanning is free. `scan_jsonl_metadata` reads headers only; it does not parse the full conversation content.

---

### Commit 3 — `credentials: bootstrap from legacy pyramid_config.openrouter_api_key (Bug #1)`

**Scope:** carryover from stabilize-main. The credentials subsystem is orthogonal to the SDFS architecture — regardless of file-vs-SQLite, the user needs a working `.credentials` file before any LLM call succeeds. Landing this commit unblocks Commits 5 and 8 to actually build anything.

Reuses the stabilize-main plan's design for Bug #1: new `CredentialStore::load_with_bootstrap(path, data_dir) -> (Arc<Self>, BootstrapReport)` API with a minimal `BootstrapLegacyKey` serde struct. Retry for credential-failed ingest records runs from main.rs gated on `bootstrap_report.bootstrapped`.

**Files:**
- `src-tauri/src/pyramid/credentials.rs` — new bootstrap helper.
- `src-tauri/src/pyramid/db.rs` — new `retry_credential_failed_ingest_records` helper (will be called from main.rs after bootstrap).
- `src-tauri/src/main.rs` — replace `CredentialStore::load(...)` call at :9795 with `load_with_bootstrap(...)`. Post-load retry if bootstrapped.

**Tests:** 11 unit tests per the stabilize-main Bug #1 section (absent file + legacy key → bootstrap; present file → no-op; malformed legacy → WARN; whitespace/empty/short/quoted legacy → skip; failed-record retry fires only when bootstrapped).

---

### Commit 4 — `settings-ui: wire PyramidSettings + PyramidFirstRun to pyramid_set_credential (Bug #4)`

**Scope:** carryover from stabilize-main. Three sites call `pyramid_set_config` (legacy) but none call `pyramid_set_credential` (Phase 3 IPC). Wire them. Use `apiKey.trim()`. Add `autoExecute` to `handleSave` dep array. Add `credentialWriteFailed` state for partial-success UX.

**Files:**
- `src/components/PyramidSettings.tsx` — handleSave + handleTestApiKey.
- `src/components/PyramidFirstRun.tsx` — handleSaveApiKey.

**Tests:** manual via the UI round-trip in the verification checklist.

---

### Commit 5 — `sdfs: builder reads .understanding/folder.md, dispatches chain builds, writes node files (code, docs, conversation bedrocks)`

**Scope:** the biggest commit. Replaces the folder_ingestion → spawn_question_build path with a builder that reads curated filemaps and writes node files via a new writer. Handles three content types: code, document, and conversation. Conversation handling reuses the existing Phase 17/18e primitives (`CreateVine`, `CreatePyramid`, `AddChildToVine`, `RegisterDadbearConfig`) — the builder is wiring, not new logic.

**Files:**
- `src-tauri/src/pyramid/understanding/builder.rs` — `build_from_filemap(workspace_root, folder_path)` reads the filemap, collects user-included entries, dispatches:
  - **Code/document files** → chain executor via `run_build_from` with the resolved chain for the content type.
  - **Conversation sessions with `bunch_slug: null`** → for each CC dir, ensure a CC vine exists (create via existing `CreateVine` op if not), then for each null-slug session: pre-create the bunch slug (following the `{cc_vine_slug}--bunch-{idx:03d}` convention from `vine.rs:886-901`), call `ingest::ingest_conversation` for the initial full ingest (this is the ONE place full ingest is correct — no prior state exists), attach the bunch to the CC vine via `AddChildToVine(child_type='bedrock')`, run the conversation-episodic chain via `run_build_from_with_evidence_mode`, then write the resulting `bunch_slug` + total `message_count` + `chunk_count` back into the filemap's `sessions[]` entry.
  - **Memory bedrocks** (when `memory_subfolder_path` is present and `memory_bedrock_slug` is null) → same pattern as conversations: create bedrock, attach to CC vine, run document chain, write slug back to filemap.
- `src-tauri/src/pyramid/understanding/node_writer.rs` — new writer that takes chain executor output and writes `.understanding/nodes/<id>/v<N>.md` files with full Wire Native Document rear-matter including `derived_from` citations in `{ doc: workspace-relative-path }` form.
- `src-tauri/src/pyramid/chain_executor.rs` — modify the writer drain task to support a new `WriteOp::SaveNodeFile` variant alongside the existing `SaveNode` (SQLite). The chain executor writes both. Files are authoritative; SQLite rows are cache.
- `src-tauri/src/pyramid/build_runner.rs` — add `run_sdfs_build(filemap_path)` entry point that feeds the builder. Keeps `run_chain_build` / `run_ir_build` for backward compat until DADBEAR retarget replaces them.
- Tauri command `pyramid_sdfs_build_from_filemap(workspace_root, folder_path)`.

**Orphan-bunch fix lands here.** The "null bunch_slug → create and attach" loop means new sessions detected in later DADBEAR scans are automatically attached to the existing CC vine on the next build pass. The pre-existing bug (new `.jsonl` creates an orphan pyramid not hooked into the vine) is closed as a side effect of the filemap-driven build flow.

**Tests:**
- Builder on a test filemap with 3 user-included code files produces 3 code node files + SQLite cache rows.
- Builder on a test filemap with a CC dir containing 5 `.jsonl` files (all `bunch_slug: null`) creates the CC vine, creates 5 bunch bedrocks, attaches each to the vine, runs the episodic chain for each, and writes the assigned slugs + message counts back to the filemap.
- Builder re-run with no changes: all bunch_slugs already populated, no new builds triggered.
- Builder re-run after scanner adds a 6th null-slug session entry: only the 6th bunch gets built; existing 5 are untouched.
- Cross-file citations in `derived_from` use correct workspace-relative paths.
- Provenance metadata (model_tier, model_id, cost, duration) lands in the node file's `local.provenance` section.
- Supersession: second build of a code file produces `v2.md`, refreshes `current.md`, adds a refinement note file.
- Memory bedrock creation: filemap with `memory_subfolder_path` and null `memory_bedrock_slug` produces the memory pyramid attached to the CC vine.

**LLM calls start here.** Commit 3 must land first for builds to succeed.

---

### Commit 6 — `sdfs: SQLite as derived cache, rebuildable from .understanding/`

**Scope:** add the rebuild-from-filesystem path. SQLite becomes officially disposable; boot-time hydration from `.understanding/` produces a cache equivalent to what existed before.

**Files:**
- `src-tauri/src/pyramid/understanding/cache_rebuild.rs` — `rebuild_cache_from_understanding(workspace_roots) -> RebuildReport` walks each `.understanding/` tree, parses every document, writes SQLite rows.
- `src-tauri/src/pyramid/db.rs` — schema version marker + detection logic. On boot, if `pyramid.db` is missing OR schema version mismatches, trigger rebuild.
- `src-tauri/src/main.rs` — boot hook: after `init_pyramid_db`, check rebuild flag and call `rebuild_cache_from_understanding` if needed.
- Tauri command `pyramid_sdfs_rebuild_cache()` — manual trigger from UI.

**Tests:**
- Delete `pyramid.db`, boot, assert SQLite has node rows for every `.understanding/nodes/*/current.md`.
- Rebuild is idempotent (second run is a no-op).
- Cache queries match file state after rebuild.
- Schema version bump forces rebuild on next boot.

---

### Commit 7 — `sdfs: resolver reads tier_routing from cache, hydrated from .understanding/configs/tier_routing.md`

**Scope:** finally unblocks the "decorative tier routing" bug from Cycle 2. The fix has two halves:
1. Bundled `.understanding/configs/tier_routing.md` (shipped in app resources, copied to user-global on first run) is the source of truth for tier assignments.
2. Both `resolve_ir_model` AND legacy `resolve_model` in `chain_dispatch.rs` are rewritten to consult the cache via `provider_registry.resolve_tier()`. Legacy fallback for `low|mid|high|max` is preserved for backward compat.

**Files:**
- `src-tauri/resources/.understanding/configs/tier_routing.md` — bundled default.
- `src-tauri/src/pyramid/chain_dispatch.rs` — rewrite `resolve_ir_model` (at :1023) AND `resolve_model` (at :186) to try registry-first, fall back to legacy mapping, preserve `model_aliases` escape hatch, preserve `defaults.model` early-return. Same shape for `resolve_ir_context_limit` + `resolve_context_limit`.
- `src-tauri/src/pyramid/understanding/bundled.rs` — new `bootstrap_user_global_understanding(data_dir)` copies bundled configs to `~/Library/Application Support/wire-node/.understanding/configs/` on first run if not present. Called from main.rs after credential bootstrap.
- Tests for both resolvers: table-wins-over-legacy, aliases-win-over-table, fall-through-on-empty-cache, context-limit-reads-table.

**Why this is Commit 7 and not earlier:** the builder in Commit 5 can use the still-hardcoded resolver because Adam's `primary_model = inception/mercury-2` happens to be the right default for most tiers — a coincidence that made the stabilize-main debugging so confusing. Commit 7 formalizes the fix under the new architecture without depending on the coincidence.

---

### Commit 8 — `sdfs: DADBEAR retargeted at source files + conversation sessions, writes filemap, fires builder via debounce, uses ingest_continuation for incremental append`

**Scope:** replace `dadbear_extend.rs`'s tick loop. Pattern is unchanged; surface is swapped. Three important plumbing changes beyond the source-file retarget:

1. **Incremental conversation append via `ingest_continuation`.** Stop calling `clear_chunks + ingest_conversation` on conversation change. Read `ingested_message_count` from the filemap session entry, call `ingest::ingest_continuation(conn, slug, path, skip_messages=ingested_message_count)`, write the new total back to the filemap on success. The Phase 0b shortcut at `dadbear_extend.rs:731-734` is deleted with its comment.
2. **No new SQLite table for message-count state.** The filemap is the cursor. `.understanding/folder.md.conversation_sources[].sessions[*].ingested_message_count` is authoritative. This closes the exact schema gap the shortcut comment complained about — without adding schema.
3. **New-session detection goes through the builder.** DADBEAR detects a new `.jsonl` appearing (filesystem poll + sha256 against filemap), writes a new `sessions[]` entry with `bunch_slug: null` and `ingested_message_count: 0`, and lets Commit 5's builder pick it up on its next pass to create the bunch and attach to the CC vine. DADBEAR itself does not create bunches — it only updates the filemap.

The existing RAII `InFlightGuard` pattern is preserved for panic-safety. `batch_size` becomes `max_files_per_rebuild`. Policy source is `.understanding/configs/dadbear_policy.md`.

**Files:**
- `src-tauri/src/pyramid/understanding/dadbear.rs` — new tick loop. Reads policy from contribution file (via cache). Scans source directories AND CC dirs from the filemap's `conversation_sources[]`. Updates `folder.md` scanner fields. Computes folder-quiet timer. On debounce expiry:
  - For stale **code/document files**: fire builder's chain rebuild path.
  - For stale **conversation sessions** (mtime/sha256 differs from filemap): fire `ingest::ingest_continuation(conn, slug, path, skip_messages=ingested_message_count)`, run the episodic chain via `run_build_from_with_evidence_mode`, write the new `message_count` + `chunk_count` back to the filemap atomically with the build completion.
  - For **new conversation sessions** in a CC dir (filemap has no matching entry): append a new `sessions[]` entry with null slug and zero cursor, fire the builder which creates the bunch on its next tick.
- `src-tauri/src/pyramid/dadbear_extend.rs` — DELETE. Replaced by the new module.
- `src-tauri/src/pyramid/folder_ingestion.rs` — delete the `RegisterDadbearConfig` emission sites and the `spawn_initial_builds` wanderer-fix dispatch; these are superseded by the new scanner + builder + DADBEAR retarget. Keep the `pyramid_find_claude_code_conversations` + `describe_claude_code_dirs` + `encode_path_for_claude_code` helpers — Commit 2 reuses them.
- `src-tauri/src/main.rs` — boot hook: start the new DADBEAR tick loop after all bootstraps complete.

**Tests:**
- DADBEAR detects a code file modification and marks `dadbear_stale: true` in the filemap.
- Folder quiet for `debounce_quiet_secs` triggers builder invocation.
- Folder with continuing edits (timer keeps resetting) does NOT trigger until quiet.
- `auto_rebuild: mark_stale_only` option fires only the filemap update, not the builder.
- Recursive propagation: child folder build completes, parent folder's filemap shows stale marker.
- **Conversation append: existing `.jsonl` grows by 20 messages → DADBEAR detects via sha256, reads `ingested_message_count: 127`, calls `ingest_continuation` with `skip_messages=127`, ingests 20 new messages as a continuation batch with `chunk_offset=6`, updates filemap to `ingested_message_count: 147`. SQLite chunks for that slug go from 6 to ~7 (one new chunk holding 20 messages). No `clear_chunks` call.** This is THE test.
- **Conversation full re-ingest is never triggered.** Assert that on any conversation change, `clear_chunks` is not called and `ingest_continuation` is called instead.
- New session: fresh `.jsonl` appears in an already-ingested CC dir → DADBEAR adds null-slug session entry → builder tick creates bunch, attaches to vine, ingests full file (as initial ingest), writes slug back.

**End of MVP.** After Commit 8, folder ingest on `agent-wire-node/` works via the new architecture with incremental conversation append and orphan-free new-session handling.

---

## Verification

### Pre-build
1. `cargo check --manifest-path src-tauri/Cargo.toml` — clean, no new errors.
2. `cargo test -p wire_node_lib` — all tests pass including the new SDFS tests.

### Clean-boot SDFS verification
1. Back up pyramid.db per pre-flight.
2. Delete `pyramid.db` and all `.understanding/` directories in the test workspace.
3. Boot the rebuilt app. Verify:
   - Bundled configs are copied to `~/Library/Application Support/wire-node/.understanding/configs/`.
   - Credentials bootstrap log line appears (if legacy key is present).
   - No errors on boot.
4. Run `pyramid_sdfs_scan_folder` on `/Users/adamlevine/AI Project Files/agent-wire-node`. Verify:
   - `agent-wire-node/.understanding/folder.md` exists with expected scanner fields.
   - Subfolder filemaps exist (`src-tauri/.understanding/folder.md`, etc.).
   - No user-included entries yet (fresh scan has everything `user_included: false` by default).
5. Mark some files as included (AI/IPC or manual edit for test). Run `pyramid_sdfs_build_from_filemap` on one subfolder. Verify:
   - Chain executor runs without errors.
   - `.understanding/nodes/*/v1.md` files appear with full provenance.
   - SQLite cache rows appear in parallel.
6. Query a built node via CLI. Verify coherent output.

### Tier routing verification (from Commit 7)
1. Set `primary_model = "test-sentinel/not-real"` in `pyramid_config.json` (temp override).
2. Run a build step that uses `model_tier: web`.
3. Verify the DEBUG log shows `x-ai/grok-4.1-fast` (from the tier_routing contribution) was dispatched, not `test-sentinel/not-real`.
4. Restore `primary_model`.

### DADBEAR verification (from Commit 8)
1. Edit a file in an already-scanned folder.
2. Wait 5+ minutes without touching anything else.
3. Verify: filemap's `dadbear_stale: true` flipped → debounce window expired → builder fired → node file updated → SQLite cache refreshed.
4. During the 5-minute quiet window, verify no premature rebuild.
5. Cancel test: edit the file again at minute 3, wait, verify quiet timer reset and rebuild is deferred to 8 minutes after the second edit.

### Conversation incremental-append verification (from Commit 8)
1. Scan `agent-wire-node/`; wait for Commit 5 builder to build at least one CC bunch. Note the filemap entry for that session: `ingested_message_count: N`, `chunk_count: K`.
2. Open Claude Code, type a few new messages into the SAME session (appending to the existing `.jsonl`). Wait for the session `.jsonl` mtime to update.
3. Wait `debounce_quiet_secs` past the last edit.
4. Verify:
   - DADBEAR detects the change (sha256 differs from filemap).
   - `ingest_continuation` is called with `skip_messages=N` (check DEBUG log).
   - `clear_chunks` is NOT called. Assert via grep of the log.
   - The continuation batch's `source_path` ends with `:continuation:N+` (per `ingest.rs:406-410`).
   - Filemap session entry's `ingested_message_count` is now `N + (number of new messages)`.
   - SQLite chunk count for the slug is `K + (number of new chunks)`, not a full re-chunk.
5. Kill the node app, restart, verify filemap cursor survives restart and a second append round picks up from the new total.

### New conversation session verification (from Commit 5 + Commit 8 + orphan-bunch fix)
1. Start a fresh Claude Code session in `agent-wire-node/`. A new `.jsonl` file appears in `~/.claude/projects/-Users-...agent-wire-node/`.
2. DADBEAR detects the new file. Filemap scanner adds a new `sessions[]` entry with `bunch_slug: null, ingested_message_count: 0`.
3. On the next builder tick: verify the new bunch is created via `CreatePyramid`, attached to the existing CC vine via `AddChildToVine(child_type='bedrock')`, and the episodic chain runs. `bunch_slug` is written back to the filemap.
4. Query the CC vine via CLI: assert the new bunch is listed as a child, NOT an orphan pyramid.

### Cache-rebuild verification
1. Delete `pyramid.db` with the app running.
2. Restart the app.
3. Verify: boot hook detects missing DB, rebuilds from `.understanding/`, queries return correct results.
4. Assert query results match what they were before deletion.

### Settings UI round-trip (from Commit 4)
1. Open Settings, change OpenRouter key with a nonce suffix (`<real-key> x`).
2. Save. Quit the app. Reopen.
3. Verify `.credentials` contains the modified value AND `pyramid_config.json.openrouter_api_key` is also updated (legacy sync).
4. Restore the real key.

---

## Out of Scope

### Follow-ups after MVP

- **Publish pipeline.** Topological sort of `derived_from` graph, citation rewriting at publish time, `published_as` field population. Needs onboarding handle-registration first.
- **Onboarding handle registration UX.** Frontend workstream to require the user to register a Wire handle before first publish attempt. Flag in the Settings UI when a handle is absent.
- **Wire-native distribution of bundled config updates.** MVP bundles configs in the app resource directory; updates come via app update. Future: Wire pulls fresh config contributions and the user can supersede local configs with them.
- **Cross-workspace citations via handle-paths.** MVP only supports workspace-internal `{ doc: path }` citations. Cross-workspace requires publishing the cited doc first and citing by handle-path.
- **Schema migration for existing pyramid.db.** MVP backs up and blows away the existing DB. A one-shot migration from pyramid.db → `.understanding/` tree is possible but not in scope. Adam's existing pyramid content (`agent-wire-node-april9`, `goodnewseveryone-definitive`, etc.) is not preserved; he explicitly confirmed blow-away.
- **Refinement-as-supersession UX.** MVP writes `v1.md`, `v2.md`, `v3.md` on each rebuild. Deciding WHICH rebuilds constitute a refinement (vs just a re-run of the same chain) is a policy question. MVP assumes every rebuild produces a new version; deduplication/refinement-detection is a follow-up.

### Pre-existing known bugs (from stabilize-main catalog) — NOT fixed by SDFS

- **Bug #6** — Phase 17 CC auto-include pulled wrong directory. The new scanner replaces Phase 17's scan code entirely, so this is obsoleted. The stray CC-1 slugs in the current pyramid.db will be deleted as part of the backup-and-blow-away.
- **Bug #7** — `pyramid_test_api_key` reads legacy `config.api_key` not CredentialStore. Still broken under SDFS; follow-up.
- **Bug #8** — Partner `LlmConfig.api_key` cached at boot. Still broken under SDFS; follow-up.
- **Bug #10** — `sync.rs` near-miss pyramid.db POST. Security-critical; separate branch.
- **Bug #11** — `pyramid_config.json` 0644 plaintext key. Migrate to CredentialStore as follow-up.
- **Bug #13** — `substitute_to_string` UTF-8 corruption via byte-cast. Latent; fix when touched.
- **Bug #14** — `batch_size=1` pinned at multiple sites. Becomes obsolete under SDFS (DADBEAR uses `max_files_per_rebuild` from policy contribution).
- **Bug #20** — `ResolvedSecret::drop` zeroize claim overclaim. Comment fix.
- **Bug #21** — warp TRACE log noise. Log level tune.

The Cycle 1/2/3 findings from stabilize-main are preserved in the earlier plan document at `efec5c0`; they informed this one but don't need re-fixing since SDFS supersedes the broken code paths they were about.

### Architectural deferrals

- **Non-Claude-Code conversation sources** (ChatGPT exports, Cursor chats, Windsurf chats, GPT API usage). MVP handles Claude Code only via the existing `pyramid_find_claude_code_conversations` path. Other sources need per-source format adapters; the filemap's `conversation_sources[].source_type` field is designed to extend to new values (`chatgpt_export`, `cursor_session_dir`, etc.) without schema change.
- **Cross-folder conversation attribution.** A Claude Code session that touches multiple workspaces lives in the first-scanned workspace's filemap. True "this session belongs to workspaces A and B" needs cross-workspace citation via published handle-paths and is out of MVP.
- **`run_build_from` intelligent incremental rebuild.** This plan closes the incremental INGEST side (new messages only get chunked via `ingest_continuation`). Whether `run_build_from` then rebuilds only new chunks' nodes vs re-running the whole episodic chain for the whole slug is a separate question flagged in "Assumptions to verify" (#8). If the chain executor always rebuilds the full slug, a further optimization lands as a follow-up. The append is still correct; the LLM-cost savings are partial until that optimization ships.
- **Whole-disk scanning** from the spec (§260-301).
- **Git integration layer** (`.gitattributes`, merge drivers, etc.) from spec §366.
- **Cross-device sync** via git/rsync from spec §369.
- **Extractor expansion to email, images, audio, messages, calendar, browser history** from spec §270-290.

### Explicitly dropped (NOT preserved from current code)

- **`clear_chunks + full re-ingest` on conversation change.** The Phase 0b shortcut at `dadbear_extend.rs:712-735` is deleted in Commit 8. The comment at line 731-734 names the state gap that drove the shortcut; SDFS closes the gap by putting the cursor in the filemap. No fallback, no flag to restore the old behavior — the shortcut is wrong and the proper path is available.

---

## Assumptions (verified during planning)

1. `insert_contribution_atomic` is the sole handle-path allocator. Verified via Adam's handback.
2. `{ doc: relative-path }` is a legal `derived_from` form. Verified at `wire-handle-paths.md:60-68`.
3. Wire Time is UTC-7 fixed. Verified.
4. `resolve_ir_model` and `resolve_model` both exist and both need fixing. Verified.
5. `use_ir_executor: false` on Adam's config. Verified.
6. `unchecked_transaction()` nesting fails at runtime. Verified (but we're not using it in SDFS — the new flow doesn't need nested transactions).
7. `pyramid_tier_routing` is populated but unconsulted by chain execution today. Verified. Under SDFS, tier routing is hydrated into the cache from `.understanding/configs/tier_routing.md`, and both resolvers consult the cache.
8. The `.understanding/` subdirectory name doesn't collide with anything in the user's source trees. Probably safe; worth a grep at implementation time.
9. Workspace-root-relative paths are stable across git clones, rsync copies, and physical disk moves as long as the workspace is moved as a unit.
10. `ingest_continuation` has zero call sites today (verified via Explore agent grep). Wiring it up is core MVP scope; the function signature at `ingest.rs:384-427` is the contract.
11. `vine_bunches` table stores existing chronological vine state. SDFS hoists this into the filemap; the table becomes part of the derived cache rebuild path in Commit 6.
12. Phase 17/18e planner primitives (`CreateVine`, `CreatePyramid`, `AddChildToVine`, `RegisterDadbearConfig`) are reused by Commit 5 builder for new bunches. These are existing helpers in `folder_ingestion.rs`, not new code.
13. `encode_path_for_claude_code` at `folder_ingestion.rs:536` handles paths with spaces post-Phase 18a fix. MVP does not need to touch this.

## Assumptions to verify at implementation time

1. **Chain executor's writer drain task** can be extended with a `SaveNodeFile` variant without breaking the existing `SaveNode` SQLite path. Read `build_runner.rs:3740-3915` (the drain spawn region) before committing.
2. **`.understanding/` directory creation** doesn't accidentally trigger `.gitignore` patterns in the user's source tree. Test by adding `.understanding/` to a workspace that has `.git/info/exclude` rules.
3. **Bundled app resource distribution.** `tauri.conf.json` needs a `resources` block to bundle `src-tauri/resources/.understanding/` into the app. Verify before Commit 7.
4. **`rebuild_cache_from_understanding` schema version** must match the current pyramid.db schema or the cache rebuild produces wrong rows. Wire it to the existing `_migration_marker` pattern.
5. **DADBEAR tick loop startup** must happen after credential bootstrap, tier routing bootstrap, and bundled config bootstrap. Order in main.rs matters.
6. **`current.md` parity check** happens on every scanner pass (not just boot). If a user edits `v3.md` directly, the parity check auto-repairs `current.md`.
7. **Workspace root detection** — when the user points the app at `/Users/adamlevine/AI Project Files/agent-wire-node`, is that the workspace root automatically, or do we need a marker file? Probably automatic: the top-level scan target is the root.
8. **`run_build_from` incremental behavior for slugs with continuation batches.** Does it rebuild only nodes whose chunks are new (chunk_index ≥ chunk_offset), or does it re-run the whole episodic chain across every chunk? If the former, incremental ingest gives proportional LLM-cost savings. If the latter, the chain executor still re-runs everything and we only save on chunking, not on LLM work. Read `build.rs` + `chain_executor.rs` before Commit 8; if full re-run is the reality, Commit 8 should either add a `--from-chunk-offset` flag OR flag an explicit follow-up for the chain-executor side of the optimization.
9. **LockManager write-lock semantics for `ingest_continuation` vs `ingest_conversation`.** Pipeline B's current `fire_ingest_chain` takes `LockManager::global().write(slug)` around the ingest call. The incremental path needs the same lock discipline. Verify no ordering hazard exists when the DADBEAR tick loop dispatches both an `ingest_continuation` AND a chain rebuild in sequence while the write lock is held.
10. **`scan_jsonl_metadata` efficiency on large `.jsonl` files.** The existing helper reads the file to extract session_id, timestamps, and message count. On a 100MB session file, this is slow. Verify the cost on real files and, if needed, cache the session_id/mtime/sha256/file_size in the filemap so the scanner only re-parses when sha256 differs.
11. **`ingest_continuation` message count semantics.** The function's `skip_messages` parameter counts only user+assistant messages (filtering toolUseResult), matching the same filter in the initial `ingest_conversation`. Verify the filter is identical in both paths so the cursor stays consistent. If they diverge (e.g., continuation counts tool_use as a message but initial doesn't), the cursor will drift and re-dispatches will either skip real content or re-ingest already-processed content.
12. **`pyramid_batches.chunk_offset` persistence across app restarts.** The batch record stores the offset when the batch was created. On restart, a new continuation batch is created with a fresh `count_chunks(slug)` call — verify this returns the cumulative count (including all prior batches), not just the latest batch's chunks.

---

## File Surface

**New files:**
- `src-tauri/src/pyramid/understanding/mod.rs`
- `src-tauri/src/pyramid/understanding/document.rs`
- `src-tauri/src/pyramid/understanding/schema.rs`
- `src-tauri/src/pyramid/understanding/workspace_path.rs`
- `src-tauri/src/pyramid/understanding/version.rs`
- `src-tauri/src/pyramid/understanding/scanner.rs`
- `src-tauri/src/pyramid/understanding/merge.rs`
- `src-tauri/src/pyramid/understanding/builder.rs`
- `src-tauri/src/pyramid/understanding/node_writer.rs`
- `src-tauri/src/pyramid/understanding/dadbear.rs`
- `src-tauri/src/pyramid/understanding/cache_rebuild.rs`
- `src-tauri/src/pyramid/understanding/bundled.rs`
- `src-tauri/resources/.understanding/configs/dadbear_policy.md`
- `src-tauri/resources/.understanding/configs/tier_routing.md`
- `src-tauri/resources/.understanding/configs/folder_ingestion_heuristics.md`
- `src-tauri/resources/.understanding/configs/build_strategy.md`
- `src-tauri/resources/.understanding/configs/evidence_policy.md`
- `src-tauri/resources/.understanding/configs/custom_prompts.md`

**Modified:**
- `src-tauri/src/pyramid/credentials.rs` — Bug #1 bootstrap.
- `src-tauri/src/pyramid/db.rs` — cache schema version marker, `retry_credential_failed_ingest_records` helper, rebuild hook.
- `src-tauri/src/pyramid/chain_dispatch.rs` — both resolvers rewritten.
- `src-tauri/src/pyramid/chain_executor.rs` — writer drain task adds `SaveNodeFile` variant.
- `src-tauri/src/pyramid/build_runner.rs` — add `run_sdfs_build` entry point.
- `src-tauri/src/main.rs` — boot hooks for bootstraps + DADBEAR tick.
- `src-tauri/tauri.conf.json` — add `resources` block for bundled `.understanding/configs/`.
- `src/components/PyramidSettings.tsx` — Bug #4.
- `src/components/PyramidFirstRun.tsx` — Bug #4.

**Deleted:**
- `src-tauri/src/pyramid/dadbear_extend.rs` — replaced by `understanding/dadbear.rs`.
- `src-tauri/src/pyramid/folder_ingestion.rs` sections implementing the wanderer-fix dispatch and `RegisterDadbearConfig` emissions — the new scanner + builder + DADBEAR retarget replace them.

**Preserved but reclassified:**
- `pyramid.db` — now a cache, not the source of truth. Backed up before any SDFS code runs.

**Read (for understanding, not modified):**
- `GoodNewsEveryone/docs/wire-native-documents.md`
- `GoodNewsEveryone/docs/wire-handle-paths.md`
- `GoodNewsEveryone/supabase/migrations/20260320100000_ux_pass_foundation.sql`
- `agent-wire-node/docs/vision/self-describing-filesystem.md`
- `agent-wire-node/docs/handoffs/handoff-2026-04-11-handle-paths-publish-time-only.md`
- `agent-wire-node/docs/handoffs/handoff-2026-04-11-folder-nodes-as-checklists.md`

---

## Success Criteria

1. `cargo check` clean. `cargo test -p wire_node_lib` passes with new SDFS tests.
2. `cargo tauri build` produces a working `.app`. Binary version gate passes.
3. Clean-boot SDFS verification passes (all 6 steps).
4. Tier routing verification passes (resolver reads from contribution file, not hardcoded primary_model).
5. DADBEAR verification passes (debounce works, folder quiet triggers, active edits defer).
6. Cache-rebuild verification passes (delete pyramid.db, reboot, queries still work).
7. Settings UI round-trip confirms credential store writes.
8. End-to-end: Adam points the app at `agent-wire-node/`, scans, curates (or AI curates), builds, queries via CLI. Real pyramids, real nodes, real answers.
9. Existing pyramid.db is backed up (not destroyed) for rollback safety.
10. The branch `stabilize-main` has 8 focused commits on top of `efec5c0`; PR merges as one unit.

---

## Resolved Decisions (previously Open Questions)

Adam confirmed these during plan review. They are baked into the relevant sections above; this list exists as a quick-reference for future readers.

1. **`published_as` goes in `local:`, inline on the v<N>.md file.** NOT `wire:`, NOT a separate publication log.

   Reasoning: `wire:` is what gets sent to the Wire backend on publish, so `published_as` in `wire:` is either a paradox (contribution sends its own publication metadata back to Wire) or noise the backend has to strip. The `local:` block is already spec'd as "filesystem operational state, stripped at publish time" — publication state is exactly that.

   Each `v<N>.md` carries its own publication stamp for the version it represents: if v3.md publishes as `playful/100/42` and then the user edits + rebuilds to v4.md, v4.md starts with no `published_as` and gets its own stamp on its own publish. 1:1 mapping between published version and local version.

   Write semantics: publishing atomically rewrites the v<N>.md file to add `local.published_as: { handle_path, published_at, published_by_build_id, signed_proof_hash }`, then refreshes `current.md` to maintain sha256 parity. A v<N>.md that already carries `published_as` is immutable on Wire and cannot be re-published through the same file; new content goes to v<N+1>.md.

   Cache indexing: boot-time walk of `.understanding/nodes/**/v*.md` builds a `handle_path → node_id → version_path` index for fast lookups.

2. **Onboarding handle registration — follow-up, not MVP.** MVP is local-only, 8 commits, no scope bump. Onboarding lands when the publish pipeline does (its own dedicated sprint).

3. **Conversation ingest is first-class MVP, NOT a follow-up.** Adam corrected me on this. Previous draft said "conversation tail-follow is deferred because we'd need a new extractor." That was wrong on two counts: (a) the extractor already exists (conversation-episodic chain, 5 prompts under `chains/prompts/conversation-episodic/`), and (b) what I was calling "tail-follow" is just the combination of DADBEAR change detection (already exists for code/docs) plus `ingest_continuation` (already exists as a library function). The work is plumbing, not new primitives.

   The actual MVP scope for conversations:
   - **Commit 2 (scanner)** calls `pyramid_find_claude_code_conversations` and writes results into `folder.md.conversation_sources[].sessions[]` with per-session state (session_id, jsonl_path, mtime, sha256, message_count cursor).
   - **Commit 5 (builder)** sees `bunch_slug: null` entries in the filemap, creates bunches via the existing `CreateVine`/`CreatePyramid`/`AddChildToVine`/`RegisterDadbearConfig` ops (Phase 17/18e primitives), runs the episodic chain, writes bunch_slug + initial `ingested_message_count` back into the filemap.
   - **Commit 8 (DADBEAR retarget)** on detected change reads `ingested_message_count` from the filemap, calls `ingest_continuation(slug, path, skip_messages=ingested_message_count)` — NOT `clear_chunks + ingest_conversation` — then writes the new total back to the filemap. Full re-ingest on conversation change is deleted, not carried forward.

   The Phase 0b shortcut that currently re-ingests the whole JSONL on every change (flagged inline at `dadbear_extend.rs:731-734`) gets dropped. Both `ingest_continuation` (already built) and the filemap cursor (new in this plan) cover the state gap the shortcut was working around.

   **Explicitly deferred (genuine follow-ups, not MVP):**
   - Non-Claude-Code conversation sources (ChatGPT, Cursor, Windsurf). MVP handles Claude Code only. Other sources need per-source format adapters.
   - Cross-folder conversation attribution — a session that touches multiple workspaces lives in the first-scanned workspace's filemap.
   - `run_build_from` intelligent incremental rebuild. This plan closes the ingest side (new messages only get chunked). Whether the chain executor rebuilds only the new chunks' nodes vs re-running the whole episodic chain is a separate question that needs verification at Commit 8 implementation time — if `run_build_from` always rebuilds the whole pyramid, the LLM cost savings from incremental ingest are partially lost and a further fix is needed. Flagged in "Assumptions to verify."

4. **`chains/defaults/*.yaml` bundling — in scope for Commit 7.** Cycle 2 Stage 2 B found 5 conversation chain YAMLs aren't embedded via `include_str!` and `tauri.conf.json` has no `resources` block. Commit 7 must bundle all of `chains/defaults/` alongside `.understanding/configs/` or the fresh install is missing chains.

5. **Workspace root — first-scan-wins.** Whatever folder the user scanned first becomes the workspace root. Subsequent scans inside it reuse that root via the parent's `.understanding/folder.md` `local.workspace_root_path`. If the user scans `agent-wire-node/src-tauri` directly before ever scanning the parent, `src-tauri` becomes its own workspace root. Re-anchoring (promoting `agent-wire-node/` to root after `src-tauri/` was scanned first) is a one-shot rewrite pass and not MVP.
