# chain-binding-v2.3 — Recover the Chronological Pipeline + Make Chain Binding Real

> **🚫 SUPERSEDED by `chain-binding-v2.4.md` on 2026-04-07.**
>
> Round 2 of Stage 2 discovery audit found 9 critical issues clustered around v2.3's additive architecture choices (rust_intrinsic chain mode, ContentType newtype, schema_version migration runner, supports_staleness flag plumbing, manifest-aware bootstrap). v2.4 simplifies aggressively. Deltas in `chain-binding-v2.4.deltas.md`.
>
> Do not implement from this file. Preserved for the audit trail only.
>
> ---
>
> **Original status:** v2.3, post Stage-2 discovery audit. Supersedes `chain-binding-v2.md` (v2.2).
>
> Lineage: v1 (`chain-binding-and-triple-pass.md`, INVALIDATED) → 4-agent audit (`chain-binding-and-triple-pass.audit.md`) → v2.0 → Stage 1 informed audit → v2.1 → MPS audit → v2.2 → Stage 2 discovery audit (`/tmp/discovery-audit-C.md`, `/tmp/discovery-audit-D.md`) → v2.3.
>
> **Audit trail:** what changed from v2.2 to v2.3 and why is documented in `chain-binding-v2.discovery-corrections.md`. Read that for the receipts.
>
> **Shipping convention:** all phases of this plan plus all phases of `recursive-vine-v2-design.md` land in a single session today. Phases are not temporally separated. No rollback plans, no "users on older binaries" guards, no "ship X first as a safety beachhead" framings — there is no window between phases.

## Premise

The discovery audit overturned several v2.2 assumptions:

1. **`build_conversation` is reachable today via the vine bunch path** (`vine.rs:569-586`). It is *production code* for vine bunches, not dead code. Phase 1's job is to make it reachable from the user's main conversation-pyramid create flow as well — currently routed to `run_decomposed_build` via `build_runner.rs:237`. Touching `build_conversation` changes vine bunch behavior too; that's a feature, not a bug, but it has to be planned for.
2. **The chronological prompt constants are inline `pub const` raw strings in `build.rs:90,113,135`**, not file-loaded from `chains/prompts/conversation-chronological/`. The on-disk prompt files exist but no code reads them. There's a Pillar 37 violation in production at `build.rs:104` (`"Target: 10-15% of input length"`) that needs scrubbing regardless.
3. **`ContentType::Vine` already exists** in the enum (5 variants total), and `'vine'` is already in the `pyramid_slugs` CHECK constraint. The vine-related work is much smaller than v2.2 implied.
4. **The vine plan retains `ContentType::Vine`** and never asks for free-string content_types or `supports_staleness` flags. v2.2 invented a `recursive-vine-v2 §5.5` reference that doesn't exist. Phase 2.5 free-string ContentType stands on its OWN merit (recompile pain), not as a vine prerequisite.
5. **`pyramid_annotations.node_id` is `NOT NULL` and the FK is composite `(slug, node_id)`**. The v2.2 SET NULL migration would crash. v2.3 ships an orphaned-annotations archive table instead.
6. **Phase 2.5 is 13 files / 79 references**, not "5+ files." Migration ordering, transactions, and `PRAGMA foreign_keys=OFF` were unspecified across all phases. Schema version table doesn't exist.
7. **`default_chain_id` has a 17-line canonical doc-comment** declaring "every content type routes to question-pipeline" as intentional. Phase 2.2 must reconcile, not silently reverse.
8. **`build.rs` already exists** with a working sha256 + `include_bytes!` asset manifest. Phase 4 extends it; doesn't add a new one. `chain_loader.rs` Tier 1 (`copy_dir_recursive`) unconditionally overwrites — opposite bug from Tier 2.

The new plan starts from these facts.

## Goals

1. **Stop forward-pass crashes and dead-config drift.** P0 bugs ship independently of any architectural work.
2. **Make the chronological conversation pipeline reachable from the main user create flow.** `build_conversation` exists; route to it via chain assignment; verify its prompts are Pillar-37-clean first.
3. **Make chain selection a per-content-type operator decision** without recompiling Rust. DB-driven, IPC-exposed, surfaced in the wizard.
4. **Open the `ContentType` value space** so adding new content_types is a config change, not 13 file edits and a recompile.
5. **Persist temporal anchors as first-class data** so chronological reasoning doesn't depend on the LLM remembering JSON fields it serializes through `#[serde(flatten)] extra`.
6. **Fix the bootstrap story for both Tier 1 and Tier 2** so dev and release installs both get all bundled chains with hash-aware sync.
7. **Introduce a schema_version table and migration runner** so multi-phase schema changes ship safely in one session.

## Non-goals

- Rewriting the v3 question DSL or `parity.rs`. Either kill DSL #2 in a follow-up or leave it alone.
- Adding new transcript parsers (Otter, Zoom, Granola, Slack) in this plan. Phase 2.5 unblocks them; the actual parsers are a follow-up.
- Meta-pyramid / cross-session grounding. Out of scope.
- Changing the synthesis prompts or `answer.md` abstain rule shipped in commit `b3e42dd`. Working correctly; do not revert.
- A hook system for `stale_engine`. Phase 2.6 ships the flag + log-warning behavior; chain-specific propagation hooks are a follow-up if ever needed.

---

## Phase 0 — P0 fixes that ship independently

These are bugs the audit found that exist regardless of any binding/dispatch work. Ship them as small commits.

### 0.0 Schema version table + migration runner (NEW, prerequisite for Phases 1.0, 2.1, 2.5, 3.1)

**File:** `src-tauri/src/pyramid/db.rs`

The codebase has no `pyramid_schema_version` table today. Without one, every boot re-attempts schema changes and table-recreate idempotency depends on transient state. Multiple phases in this plan introduce schema changes that need to run exactly once and in order.

**Action:**
1. Add a new table:
```sql
CREATE TABLE IF NOT EXISTS pyramid_schema_version (
    version INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL DEFAULT (datetime('now')),
    description TEXT NOT NULL
);
```
2. Add a `migrations.rs` module with:
```rust
pub struct Migration {
    pub version: i64,
    pub description: &'static str,
    pub up: fn(&Connection) -> Result<()>,
}

pub fn run_migrations(conn: &Connection) -> Result<()> {
    let applied = get_applied_versions(conn)?;
    for migration in MIGRATIONS {
        if applied.contains(&migration.version) { continue; }
        conn.execute("BEGIN", [])?;
        match (migration.up)(conn) {
            Ok(()) => {
                conn.execute(
                    "INSERT INTO pyramid_schema_version (version, description) VALUES (?1, ?2)",
                    rusqlite::params![migration.version, migration.description],
                )?;
                conn.execute("COMMIT", [])?;
            }
            Err(e) => {
                conn.execute("ROLLBACK", [])?;
                return Err(anyhow::anyhow!("migration {} failed: {}", migration.version, e));
            }
        }
    }
    Ok(())
}
```
3. Wire `run_migrations` into the existing `init_pyramid_db()` path, after `CREATE TABLE IF NOT EXISTS` runs but before any other init work.
4. Migrations that need to disable foreign keys do so inside their own `up` function with `PRAGMA foreign_keys=OFF` ... `PRAGMA foreign_keys=ON`.
5. Schema version numbers used in this plan: 1 = annotations FK (Phase 1.0), 2 = chain_defaults table (Phase 2.1), 3 = pyramid_slugs CHECK drop (Phase 2.5), 4 = chunks temporal columns (Phase 3.1), 5 = chunks content_hash backfill (Phase 3.4).

### 0.1 UTF-8 panic in `update_accumulators`

**File:** `src-tauri/src/pyramid/chain_executor.rs:6960-6964`

Today:
```rust
let truncated = if new_val.len() > max_chars {
    new_val[..max_chars].to_string()
} else { new_val };
```

`new_val.len()` is byte length. The slice panics on any non-ASCII character at the wrong byte boundary. Em-dashes, smart quotes, accents, CJK, emoji all crash.

**Fix — char-aware truncation, inline:**
```rust
// max_chars is interpreted as a CHARACTER count here, not bytes.
// Deliberate semantic shift from the prior byte budget; the prior shape was
// already broken on multi-byte input so any new invariant is fine as long as
// it's documented. (verified no chain YAML byte-budgets max_chars)
let truncated = new_val
    .char_indices()
    .nth(max_chars)
    .map(|(i, _)| new_val[..i].to_string())
    .unwrap_or(new_val);
```

**DO NOT swap in `truncate_for_webbing` at `:1553`** — that helper trims and appends `...`, semantically wrong for accumulators.

**Sub-tasks:**
- Verify no chain YAML treats `max_chars` as bytes: `grep -rn 'max_chars' chains/`. Document findings inline below.
- Add a unit test with em-dash at `max_chars - 1` and `max_chars - 2`, single emoji (4-byte), single CJK char (3-byte). 6 test cases.

### 0.2 Delete dead `instruction_map: content_type:` config

**Files:** `chains/defaults/question.yaml` (line 28), with explanatory comment

The chain YAML declares `instruction_map: content_type:conversation: $prompts/conversation/source_extract_v2.md`, but the matcher in `chain_executor.rs:1034-1070` only handles `type:`, `language:`, `extension:`, `type:frontend` prefixes. The `content_type:` key is silently ignored.

**Action:** Delete the dead key. Add a one-line comment pointing at Phase 2 (which subsumes the use case via the resolver). Implementing the matcher would require plumbing `ChainContext` through `instruction_map_prompt`'s call sites, all of which gets thrown away by Phase 2.

### 0.3 Delete `generate_extraction_schema()`

**File:** `src-tauri/src/pyramid/extraction_schema.rs:40`

Defined, exported, fully implemented with tests. Zero callers in `src-tauri/`. The L0 schema in production comes from the static `chains/prompts/question/source_extract.md`.

**Action:** Delete. If we want LLM-generated L0 schemas later, we revive from git history.

### 0.4 Tighten `chunk_transcript` boundary regex

**File:** `src-tauri/src/pyramid/ingest.rs:244-282`

`chunk_transcript`'s boundary trigger is `line.starts_with("--- ") && current_count >= soft_threshold`. A stray markdown `---` near the *start* of a chunk doesn't trigger (the soft threshold gates it), but a stray `--- ` rule in the *back half* of a chunk does. Real bug, narrower than v2.2 implied.

**Fix:** require a label after `--- `: `--- (PLAYFUL|CONDUCTOR|USER|ASSISTANT|...)`. Or, more permissive: `--- [A-Z]+`. The structural fix (per-chunk speaker/timestamp metadata) lands in Phase 3 alongside the temporal columns.

### 0.5 Pillar 37 sweep across `build.rs` prompt constants

**Files:** `src-tauri/src/pyramid/build.rs:90-300` (FORWARD_PROMPT, REVERSE_PROMPT, COMBINE_PROMPT, DISTILL_PROMPT, and any other prompt constants)

Pillar 37 forbids prescriptive output sizes in prompts. The discovery audit confirmed `build.rs:104` says `"Target: 10-15% of input length"` — direct violation in production. Likely more violations in the same file.

**Action:**
1. Read all `pub const *_PROMPT` declarations in `build.rs`.
2. Grep each for: `at least`, `between`, `minimum`, `maximum`, `at most`, `no more than`, `target:`, `\d+%`, `\d+-\d+`, `exactly N`, `between N and M words`.
3. Replace each violation with a truth condition or removal. Example for `:104`: replace `"Target: 10-15% of input length."` with `"Compress to maximum density without losing information."` (truth condition: density preserved).
4. Document the diff in the commit message.

### 0.6 Phase 0 done criteria

- [ ] `pyramid_schema_version` table exists and `run_migrations` runs at boot.
- [ ] `update_accumulators` no longer panics on multi-byte UTF-8 input. Test passes.
- [ ] `instruction_map: content_type:` removed from `chains/defaults/question.yaml`.
- [ ] `generate_extraction_schema` deleted.
- [ ] `chunk_transcript` does not false-trigger on stray `---` rules.
- [ ] `build.rs` prompt constants pass Pillar 37 audit.

Ship as 6 small commits.

---

## Phase 1 — Recover the chronological pipeline by fixing dispatch from the main create flow

`build_conversation` is reachable via vine.rs today but NOT from the user's main conversation-pyramid create flow (`build_runner.rs:237` routes Conversation to `run_decomposed_build`). Phase 1 makes the main flow able to reach it via chain assignment.

### 1.0 Annotations FK — orphaned-annotations archive (NEW migration shape)

**File:** `src-tauri/src/pyramid/db.rs:228-238`

Today:
```sql
CREATE TABLE IF NOT EXISTS pyramid_annotations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    slug TEXT NOT NULL,
    node_id TEXT NOT NULL,
    ...
    FOREIGN KEY (slug, node_id) REFERENCES pyramid_nodes(slug, id) ON DELETE CASCADE
);
```

The FK is composite `(slug, node_id)`, both NOT NULL. SET NULL is impossible without dropping NOT NULL on both columns, which is a bigger schema change. Cleaner: keep CASCADE, but archive annotations BEFORE the rebuild path deletes their target nodes.

**Migration (schema version 1):**
1. Create a new table:
```sql
CREATE TABLE IF NOT EXISTS pyramid_orphaned_annotations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    original_id INTEGER NOT NULL,
    slug TEXT NOT NULL,
    original_node_id TEXT NOT NULL,
    annotation_type TEXT NOT NULL,
    content TEXT NOT NULL,
    question_context TEXT,
    author TEXT NOT NULL,
    created_at TEXT NOT NULL,
    archived_at TEXT NOT NULL DEFAULT (datetime('now')),
    archive_reason TEXT NOT NULL DEFAULT 'rebuild'
);
CREATE INDEX IF NOT EXISTS idx_orphaned_annotations_slug ON pyramid_orphaned_annotations(slug);
```

2. Add `archive_annotations_for_slug(conn, slug, reason)` to `db.rs`. Called by the rebuild path BEFORE node deletion. Copies all annotations for the slug into `pyramid_orphaned_annotations`, then lets CASCADE delete them.

3. Wire `archive_annotations_for_slug` into the rebuild paths in `build_runner.rs` (and any vine rebuild path in `vine.rs`) before they delete pyramid_nodes rows for the slug.

4. Add a read-only "view orphaned annotations for slug" surface to MCP/web (Phase 5 docs follow-up — not Phase 1).

**Why this approach:** preserves data without schema gymnastics. Doesn't need a stable cross-rebuild identity. Doesn't change the FK shape. Vine bunches that already build with `L0-{ci}` IDs are also covered automatically since archive runs before deletion regardless of caller.

### 1.1 The dispatch problem

**File:** `src-tauri/src/pyramid/build_runner.rs:237`

Today, `run_build_from` checks `content_type == ContentType::Conversation` and routes directly to `run_decomposed_build`, which calls `chain_registry::default_chain_id` (always returns `"question-pipeline"`). It never reaches `run_legacy_build`. The user's main create flow has no way to reach `build_conversation` even though the function works.

(Vines reach it via a separate path in `vine.rs:569`. After Phase 1, both call sites coexist.)

### 1.2 The dispatch fix (sequence after Phase 2.2 introduces resolver)

**File:** `src-tauri/src/pyramid/build_runner.rs:237`

The pseudocode below uses `chain_registry::resolve_chain_for_slug` introduced in Phase 2.2. Phase 1.2 sequences AFTER Phase 2.2 because the call site has nothing to call until then.

```rust
// inside run_build_from, for ContentType::Conversation:
let chain_id = chain_registry::resolve_chain_for_slug(
    &conn,
    slug,
    content_type.as_str(),
)?;

if chain_id == "conversation-legacy-chronological" {
    archive_annotations_for_slug(&conn, slug, "chain_swap")?;
    return run_legacy_build(state, slug, /* ... full args ... */).await;
}
return run_decomposed_build(state, slug, /* ... */).await;
```

**Notes:**
- `resolve_chain_for_slug` returns a plain `String` (not the `(String, Option<String>)` tuple of the lower-level `get_assignment`). Phase 2.2's wrapper handles the destructure.
- `run_legacy_build` and `run_decomposed_build` have different signatures — Phase 1.2 must thread `apex_question`, `granularity`, `max_depth`, and any other args correctly. Audit the existing `run_legacy_build` callers (the only one today is via the path that never fires for Conversation) to recover the signature.
- The `archive_annotations_for_slug` call is the chain-swap protection from Phase 1.0.

### 1.3 Verify `build_conversation` actually does what we want

**Critical:** before relying on it, audit `build_conversation` carefully.

- Read `build.rs:684+` end to end.
- Confirm it produces L0/L1/L2 nodes the parser actually accepts.
- Confirm it handles cancellation correctly.
- Confirm it persists step output in a way that survives crash + resume.
- Test it on the same `.jsonl` we used for Runs 1-4.
- Document any surprises in this doc before proceeding.

#### 1.3.0 Pillar 37 audit on inline prompt constants (not phantom files)

The chronological prompts are inline `pub const` raw strings in `build.rs:90` (`FORWARD_PROMPT`), `:113` (`REVERSE_PROMPT`), `:135` (`COMBINE_PROMPT`). They are NOT loaded from `chains/prompts/conversation-chronological/`. The on-disk files exist but no code reads them.

This was already covered by Phase 0.5's broader Pillar 37 sweep across `build.rs:90-300`. Phase 1.3.0 is the verification step that confirms 0.5 actually scrubbed all three constants.

**Action:**
1. After Phase 0.5 lands, re-read `FORWARD_PROMPT`, `REVERSE_PROMPT`, `COMBINE_PROMPT`.
2. Confirm no prescriptive output sizing remains.
3. Note that any change here also affects vine bunch builds (they use the same constants via the vine.rs → build_conversation path).

#### 1.3.5 Structural divergence consumer audit

`build_conversation` produces structurally different pyramids than `run_decomposed_build`:
- No question decomposition tree (no `Q-*` nodes)
- No evidence verdicts (CONNECT/DISCONNECT/MISSING)
- No FAQ generation
- No question-shape web edges
- No abstain rule from the new `answer.md`

Vine bunches built via `vine.rs:569` ALREADY produce these "missing-question-shape" pyramids. The consumer surface already handles them somehow (or fails silently). Inventory current behavior on a vine-built slug as a free regression baseline, then extend any gaps for the new chronological binding.

**Consumer surfaces to audit (extended list — 15 files):**
- `src-tauri/src/pyramid/build_runner.rs` — post-build seeding (file watcher, stale subscription, backfill_node_ids)
- `src-tauri/src/pyramid/vine.rs::run_build_pipeline` — already a co-caller of `build_conversation`
- `src-tauri/src/pyramid/stale_engine.rs` — propagation (covered by Phase 2.6)
- `src-tauri/src/pyramid/stale_helpers.rs`, `stale_helpers_upper.rs` — helper paths
- `src-tauri/src/pyramid/staleness.rs`, `staleness_bridge.rs` — bridge layer
- `src-tauri/src/pyramid/publication.rs` — Wire publish content_type contract
- `src-tauri/src/pyramid/wire_publish.rs`, `wire_import.rs` — cross-instance pyramid sync
- `src-tauri/src/pyramid/faq.rs` — FAQ generation (only fires for question-shape today; verify behavior on chronological)
- `src-tauri/src/pyramid/webbing.rs` — web edge generation
- `src-tauri/src/pyramid/reconciliation.rs` — node reconciliation across rebuilds
- `src-tauri/src/pyramid/partner/` — Partner/Dennis interactive layer
- `src-tauri/src/pyramid/public_html/routes_read.rs` — web read surface
- `src-tauri/src/pyramid/public_html/routes_ask.rs` — web ask surface
- `src-tauri/src/pyramid/render.rs` — HTML rendering
- `mcp-server/src/` — MCP apex/drill/search/faq endpoints

**For each:** confirm it degrades gracefully on missing question-shape data, OR fix it in this same session.

### 1.4 Route to `build_conversation` verbatim — register as a chain

#### 1.4.1 Register `conversation-legacy-chronological` as a chain (NEW — D-20 fix)

The dispatch fix in 1.2 keys on `chain_id == "conversation-legacy-chronological"`, but no YAML or registry record defines this chain ID. Without a record, Phase 2.6's `supports_staleness` flag has nothing to declare in, and Phase 1F's wizard has nothing to enumerate.

**Action — option A: rust_intrinsic chain mode.** Create `chains/defaults/conversation-legacy-chronological.yaml`:
```yaml
id: conversation-legacy-chronological
description: |
  Chronological forward/reverse/combine pipeline for sequential conversation
  sources. Implemented in Rust via build::build_conversation. Used by both
  the user's main conversation-pyramid create flow (when bound) and by vine
  bunches (always).
pipeline: rust_intrinsic
target_function: build_conversation
content_type: conversation
supports_staleness: false
capabilities:
  wants_file_watcher: false
  wants_filesystem_hashing: false
  has_filesystem_sources: true
  wants_node_id_backfill: false
```

Add a new `Pipeline::RustIntrinsic { target: String }` variant to `ChainDefinition` (in `chain_engine.rs`). Add `target_function` to the schema. The chain executor sees a `RustIntrinsic` chain and dispatches to a hardcoded function map keyed on the `target_function` string. The function map is in `chain_loader.rs::INTRINSIC_FUNCTIONS` and contains entries like `("build_conversation", build_conversation_dispatch_wrapper)`.

This unifies all chains (including hand-coded Rust builds) under one registry. Future-proofing: `build_code`, `build_docs`, the question pipeline itself, and vine builds can all become intrinsic chain entries in the same file later.

#### 1.4.2 Routing in `run_build_from`

Already covered by Phase 1.2. The dispatch checks the resolved chain_id; if it matches the registered intrinsic, it calls the intrinsic dispatcher.

### 1.5 Phase 1 done criteria

- [ ] Annotations migration (schema version 1) lands; `archive_annotations_for_slug` exists and is called by all rebuild paths (build_runner + vine).
- [ ] `build_conversation` audited end-to-end; surprises documented inline.
- [ ] Phase 0.5 Pillar 37 sweep verified clean for FORWARD/REVERSE/COMBINE constants (Phase 1.3.0 verification step).
- [ ] Structural divergence consumer audit complete; every breaking surface fixed in this session (Phase 1.3.5).
- [ ] `chains/defaults/conversation-legacy-chronological.yaml` exists; `Pipeline::RustIntrinsic` mode added to `ChainDefinition`; intrinsic dispatcher registered for `build_conversation`.
- [ ] Dispatch fix lands; test pyramid built with `chain_id = "conversation-legacy-chronological"` actually executes `build_conversation` and produces nodes the parser accepts.
- [ ] Run 4 reference rebuild: same `.jsonl` as `claudeconvotest4temporallabelingupdate`, new chronological binding, eyeball convergence.
- [ ] Existing slugs assigned to `question-pipeline` continue to build successfully.
- [ ] Existing vine bunches that route through `build_conversation` continue to build successfully.

### 1F Frontend workstream (split into 1F-pre and 1F-post)

#### 1F-pre — UI shell only (parallel to Phase 1, before Phase 2)

1. Grep `src/` for content_type unions and string literals.
2. Centralize in `src/lib/contentTypes.ts` (create if missing): single source `export const WELL_KNOWN_CONTENT_TYPES = ['code', 'document', 'conversation', 'question', 'vine'] as const; export type ContentType = string;` (free string after Phase 2.5).
3. Update `AddWorkspace.tsx` and the 6+ other call sites to import from the central list.
4. Add a chain selector UI shell to the wizard for conversation content_type. Dropdown of well-known chains (default: `question-pipeline`; option: `conversation-legacy-chronological`). The selector renders but persists nothing yet.
5. Audit every `CONTENT_TYPE_CONFIG[x]` consumer (`PyramidRow.tsx:30`, `PyramidDetailDrawer.tsx:297`, etc.) and add a fallback object: `const config = CONTENT_TYPE_CONFIG[x] ?? { label: x, color: 'gray', icon: 'question-mark' };`.

#### 1F-post — IPC wiring (after Phase 2)

6. Wire the wizard's chain selector to the IPC commands `set_chain_default` / `get_chain_default` (added in Phase 2.3).
7. Surface the active chain in the workspace settings panel.
8. Default the wizard dropdown to well-known content_types; add an "advanced: type custom" toggle for free-text input (gated behind a settings flag for power users).

#### 1F done criteria

- [ ] All content_type unions in `src/` import from a single constant.
- [ ] Every `CONTENT_TYPE_CONFIG[x]` lookup has a fallback object.
- [ ] AddWorkspace wizard exposes chain selection for conversation content_type, persisted via Phase 2.3 IPC.
- [ ] Workspace settings panel shows the active chain_id for each slug.
- [ ] Free-text custom content_type behind an advanced toggle.

---

## Phase 2 — Make chain binding real (per-content-type, schema-supported)

The audit found that `pyramid_chain_assignments` has no `content_type` column, `default_chain_id` is canonically hardcoded to `"question-pipeline"` per its 17-line doc-comment, and there is no per-content-type override mechanism. Phase 2 adds the override layer without reversing the canonical default.

### 2.1 Schema change — `pyramid_chain_defaults` table (schema version 2)

`pyramid_chain_assignments` is keyed on slug PK with FK to `pyramid_slugs`. Adding a column doesn't work for defaults — there's no slug to bind to. Different table.

**Migration (schema version 2):**
```sql
CREATE TABLE IF NOT EXISTS pyramid_chain_defaults (
    content_type TEXT PRIMARY KEY,
    chain_id TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

Simple `CREATE TABLE IF NOT EXISTS`. No FK gymnastics. Wraps in the standard migration transaction.

### 2.2 Add `resolve_chain_for_slug` (override layer over canonical default)

**File:** `src-tauri/src/pyramid/chain_registry.rs`

`default_chain_id` stays. Its 17-line doc-comment is the canonical design and v2.3 doesn't reverse it. v2.3 adds an OVERRIDE layer.

Add the new resolver alongside `default_chain_id`:
```rust
/// Resolve the chain ID for a slug build, consulting overrides in this order:
///   1. per-slug assignment (highest priority)
///   2. per-content-type default override
///   3. canonical default (`default_chain_id`, currently always `"question-pipeline"`)
pub fn resolve_chain_for_slug(
    conn: &Connection,
    slug: &str,
    content_type: &str,
) -> Result<String> {
    // 1. per-slug override
    if let Some((chain_id, _)) = get_assignment(conn, slug)? {
        return Ok(chain_id);
    }
    // 2. per-content-type default override
    if let Some(chain_id) = get_chain_default(conn, content_type)? {
        return Ok(chain_id);
    }
    // 3. canonical default
    Ok(default_chain_id(content_type).to_string())
}

pub fn get_chain_default(conn: &Connection, content_type: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT chain_id FROM pyramid_chain_defaults WHERE content_type = ?1",
    )?;
    let result = stmt.query_row(rusqlite::params![content_type], |row| row.get::<_, String>(0));
    match result {
        Ok(val) => Ok(Some(val)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn set_chain_default(conn: &Connection, content_type: &str, chain_id: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO pyramid_chain_defaults (content_type, chain_id) VALUES (?1, ?2)
         ON CONFLICT(content_type) DO UPDATE SET chain_id = excluded.chain_id, updated_at = datetime('now')",
        rusqlite::params![content_type, chain_id],
    )?;
    Ok(())
}
```

**Update the doc-comment on `default_chain_id`** to acknowledge the new override layer:
```rust
/// CANONICAL: every content type routes to `question-pipeline` by default.
/// (Existing doc-comment text...)
///
/// **As of v2.3:** per-content-type *overrides* exist via `pyramid_chain_defaults`
/// (resolved by `resolve_chain_for_slug`). Operators can set an override per
/// content_type, which is consulted before this canonical default. Per-slug
/// assignments still take highest priority. The canonical default here is the
/// bottom fallback when neither override is set.
pub fn default_chain_id(_content_type: &str) -> &'static str {
    "question-pipeline"
}
```

**Enumerate `default_chain_id` callers:**
```bash
grep -rn 'default_chain_id' src-tauri/src/
```
Each caller decides whether to migrate to `resolve_chain_for_slug` (when slug + content_type are in scope) or stay on `default_chain_id` (when only content_type is in scope, e.g., test fixtures or fallback paths).

### 2.3 IPC commands

Add Tauri IPC commands in `src-tauri/src/main.rs` (or wherever IPC commands live):
```rust
#[tauri::command]
async fn set_chain_default_cmd(content_type: String, chain_id: String, state: State<'_, AppState>) -> Result<(), String> {
    let conn = state.conn.lock().await;
    chain_registry::set_chain_default(&conn, &content_type, &chain_id).map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_chain_default_cmd(content_type: String, state: State<'_, AppState>) -> Result<Option<String>, String> {
    let conn = state.conn.lock().await;
    chain_registry::get_chain_default(&conn, &content_type).map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_available_chains_cmd(state: State<'_, AppState>) -> Result<Vec<ChainSummary>, String> {
    // returns id + description for every chain in the registry
}
```

Frontend (Phase 1F-post) calls these.

### 2.4 What this does NOT change

- `default_chain_id` stays canonical "everything routes to question-pipeline" by default
- Existing slugs with no override still use `question-pipeline`
- Phase 2 only adds a new layer above the canonical fallback

### 2.5 Open `ContentType` to free string (newtype + capability flags)

The largest workstream in the plan. The `ContentType` enum is closed by exhaustive `match` in 13 source files (79 references). Adding a new content_type today requires recompiling Rust + editing 13 files + a DB CHECK migration. That's the wall every future content_type hits, and it's the main motivation here. (Vines do NOT need this — vines retain `ContentType::Vine`.)

#### 2.5.1 Newtype with `#[serde(transparent)]`

**File:** `src-tauri/src/pyramid/types.rs:29-65`

Replace the closed enum with a transparent newtype:
```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContentType(pub String);

impl ContentType {
    pub fn new(s: impl Into<String>) -> Self { Self(s.into()) }
    pub fn as_str(&self) -> &str { &self.0 }
    pub fn from_str(s: &str) -> Option<Self> { Some(Self::new(s)) }  // back-compat
    pub fn is_well_known(&self) -> bool {
        matches!(self.0.as_str(), "code" | "conversation" | "document" | "vine" | "question")
    }
}

// Named constants for well-known types — preserves call-site readability.
// Code that previously wrote `ContentType::Code` now writes `ContentType::code()`.
impl ContentType {
    pub fn code() -> Self { Self::new("code") }
    pub fn conversation() -> Self { Self::new("conversation") }
    pub fn document() -> Self { Self::new("document") }
    pub fn vine() -> Self { Self::new("vine") }
    pub fn question() -> Self { Self::new("question") }
}

impl std::fmt::Display for ContentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
```

`#[serde(transparent)]` is REQUIRED — without it the newtype serializes as `["code"]`, breaking every Tauri IPC payload.

#### 2.5.2 Replace exhaustive matches with capability lookups

The `match content_type` arms in `main.rs:3145-3245` and similar sites are NOT chain dispatch — they're file-watcher capability checks. Phase 2.5 replaces them with capability lookups.

Add `ContentTypeCapabilities` to `ChainDefinition` in `chain_engine.rs`:
```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ContentTypeCapabilities {
    #[serde(default)]
    pub wants_file_watcher: bool,
    #[serde(default)]
    pub wants_filesystem_hashing: bool,
    #[serde(default)]
    pub has_filesystem_sources: bool,
    #[serde(default)]
    pub wants_node_id_backfill: bool,
}
```

Add a resolver:
```rust
pub fn resolve_capabilities(conn: &Connection, content_type: &str) -> ContentTypeCapabilities {
    // Resolve via the chain currently bound to this content_type.
    // If no chain bound, fall back to per-content-type defaults table.
    // If still nothing, return safe defaults (all false).
}
```

Replace the match arms in `main.rs` and elsewhere with `resolve_capabilities(...).wants_file_watcher` etc.

**Hardcoded built-in capabilities** (for the well-known content_types, written to `chains/defaults/*.yaml` files):
- `code`: `wants_file_watcher=true, wants_filesystem_hashing=true, has_filesystem_sources=true, wants_node_id_backfill=true`
- `document`: same as code
- `conversation`: `has_filesystem_sources=true` (single-file source); others false
- `question`: all false (no source files)
- `vine`: all false

#### 2.5.3 Drop the CHECK constraint via table-recreate (schema version 3)

`pyramid_slugs` has `content_type TEXT NOT NULL CHECK(content_type IN ('code', 'conversation', 'document', 'vine', 'question'))`. The CHECK is baked into the schema; `CREATE TABLE IF NOT EXISTS` won't change it. SQLite has no `ALTER TABLE DROP CONSTRAINT`. Required ritual:

```rust
fn migration_3_drop_slugs_check(conn: &Connection) -> Result<()> {
    conn.execute("PRAGMA foreign_keys = OFF", [])?;
    conn.execute_batch("
        CREATE TABLE pyramid_slugs_new (
            slug TEXT PRIMARY KEY,
            content_type TEXT NOT NULL,
            -- (all other columns from pyramid_slugs, no CHECK)
            ...
        );
        INSERT INTO pyramid_slugs_new SELECT * FROM pyramid_slugs;
        DROP TABLE pyramid_slugs;
        ALTER TABLE pyramid_slugs_new RENAME TO pyramid_slugs;
        -- Recreate every index that was on pyramid_slugs
        CREATE INDEX IF NOT EXISTS idx_slugs_xxx ON pyramid_slugs(...);
    ")?;
    conn.execute("PRAGMA foreign_keys = ON", [])?;
    Ok(())
}
```

**Risk:** `pyramid_slugs` is the FK target for many other tables. With `PRAGMA foreign_keys = OFF`, the rename is safe but indices and triggers must be recreated explicitly. Read `db.rs` end-to-end before writing the migration to enumerate every index/trigger that references `pyramid_slugs`.

#### 2.5.4 Update all 13 ContentType reference sites

Files (per discovery audit verification):
- `src-tauri/src/main.rs` (10 refs) — match arms become capability lookups
- `src-tauri/src/pyramid/build_runner.rs` (8 refs) — dispatch site (Phase 1.2) + others
- `src-tauri/src/pyramid/slug.rs` (7 refs)
- `src-tauri/src/pyramid/db.rs` (16 refs) — including the CHECK migration above
- `src-tauri/src/pyramid/ingest.rs` (3 refs)
- `src-tauri/src/pyramid/routes.rs` (7 refs)
- `src-tauri/src/pyramid/vine.rs` (9 refs) — dispatch in `run_build_pipeline`
- `src-tauri/src/pyramid/chain_executor.rs` (4 refs)
- `src-tauri/src/pyramid/parity.rs` (1 ref)
- `src-tauri/src/pyramid/types.rs` (10 refs) — the type definition itself
- `src-tauri/src/pyramid/public_html/routes_read.rs` (2 refs)
- `src-tauri/src/pyramid/build.rs` (1 ref)
- `src-tauri/src/pyramid/public_html/routes_ask.rs` (1 ref)

Plus `defaults_adapter.rs:119, 709, 812, 935` — replace hardcoded `"code".to_string()` etc. with `ContentType::code().0.clone()` or similar.

For each file:
- `match content_type { ContentType::Foo => ..., ... }` → either capability lookup OR string comparison via `as_str()` for the well-known cases
- `ContentType::Foo` constructors → `ContentType::foo()` named constructors
- Pattern matches that branch on enum variant → use `is_well_known()` + string match, OR use the resolver if it's a dispatch decision

#### 2.5.5 Frontend updates (handled in Phase 1F)

- `src/lib/contentTypes.ts` — change `type ContentType = ...union...` to `type ContentType = string` (with `WELL_KNOWN_CONTENT_TYPES` as a const for autocomplete)
- Every `Record<ContentType, ...>` lookup gets a fallback object (Phase 1F-pre step 5)
- AddWorkspace.tsx accepts dropdown by default + advanced free-text toggle

#### 2.5.6 Verification

After all 13 files are updated:
- `cargo check` passes
- Build a test pyramid with content_type `"conversation"` and verify it builds correctly
- Build a test pyramid with content_type `"transcript.test"` (a free-string value) and verify the resolver falls back to `question-pipeline` and the build runs

### 2.6 `supports_staleness` capability flag

**File:** `src-tauri/src/pyramid/chain_engine.rs` (struct field), `src-tauri/src/pyramid/stale_engine.rs:1406+` (consumer)

Stale propagation in `stale_engine.rs:1406` hardcodes the question-chain shape and silently no-ops on chains that don't produce KEEP-link evidence. Phase 1 ships chronological pyramids that don't fit. They become silently write-only.

**Action:**

1. Add a struct field to `ChainDefinition` in `chain_engine.rs`:
```rust
#[serde(default)]
pub supports_staleness: bool,
```

2. `chains/defaults/question.yaml` declares `supports_staleness: true`.
3. `chains/defaults/conversation-legacy-chronological.yaml` declares `supports_staleness: false` (already in 1.4.1).
4. `stale_engine.rs` reads the flag at the entry to its propagation loop. If false: log a warning naming the slug + chain_id, then return without doing work. No silent no-op.
5. Verify `defaults_adapter.rs` populates the new field on the legacy DSL → ChainDefinition conversion (4 sites: `:119, :709, :812, :935`).
6. Apply the same flag check to `stale_helpers.rs`, `stale_helpers_upper.rs`, `staleness.rs`, `staleness_bridge.rs`. These all currently assume question shape.

**Vine staleness is separate.** Vines need `build_id` propagation per vine doc §6, which is a different mechanism implemented in the recursive-vine-v2 plan. The `supports_staleness` flag is about whether the existing question-shape KEEP-link propagation runs; vine `build_id` propagation is a parallel mechanism that doesn't go through this code path.

### 2.7 Phase 2 done criteria

- [ ] `pyramid_chain_defaults` table exists (schema version 2 applied).
- [ ] `resolve_chain_for_slug` consults per-slug → per-content-type → canonical fallback in that order.
- [ ] IPC commands `set_chain_default` / `get_chain_default` / `list_available_chains` exposed and called by Phase 1F-post wizard.
- [ ] Setting `pyramid_chain_defaults[conversation] = 'conversation-legacy-chronological'` routes new conversation builds to `build_conversation`.
- [ ] `ContentType` is now a `#[serde(transparent)]` newtype `String`. All 13 reference files updated. `cargo check` passes.
- [ ] `pyramid_slugs` CHECK constraint dropped (schema version 3 applied) on an existing dev DB without losing dependent rows.
- [ ] `ContentTypeCapabilities` resolved via chain definition; main.rs match arms collapsed.
- [ ] `defaults_adapter.rs` literals replaced with named constants.
- [ ] `supports_staleness` flag honored by `stale_engine`, `stale_helpers`, `stale_helpers_upper`, `staleness`, `staleness_bridge`. Non-supporting chains log warning + return without work.
- [ ] `default_chain_id` doc-comment updated to reflect override layer.
- [ ] Frontend `Record<ContentType, ...>` consumers all have fallback objects.
- [ ] Test: build a pyramid with content_type `"transcript.test"` (free string), confirm fallback chain runs.

---

## Phase 3 — Persist temporal anchors as first-class data

Even after the chronological pipeline runs, temporal data lives in `#[serde(flatten)] extra` on `Topic` — Rust can't sort, filter, or validate. `pyramid_chunks` has no `first_ts`/`last_ts` columns. Re-ingestion shuffles `chunk_index` so resume keys hit the wrong content.

### 3.0 Locate the L0 schema parse site(s) — spike

**Goal:** find every site where extracted LLM JSON becomes a `Topic` (or whatever shape Rust validates against).

**Method:**
```bash
grep -rn 'serde_json::from_str.*Topic\|from_value.*Topic\|Vec<Topic>' src-tauri/src/pyramid/
grep -rn 'deserialize_with\|serde(deserialize_with' src-tauri/src/pyramid/types.rs
grep -rn 'step_outputs.*insert\|store_step_output' src-tauri/src/pyramid/chain_executor.rs
```

The discovery audit found:
- Topics flow as `Vec<Value>` (untyped JSON) through most of `chain_executor.rs`
- `Topic` is strongly-typed only at storage write time elsewhere (unknown specific site)
- Likely multiple parse sites: extract, extract_with_schema, heal_json retry path

**Output:** document inline below the actual parse sites found. Phase 3.3 hooks into them.

### 3.1 Add temporal columns to `pyramid_chunks` (schema version 4)

**File:** `src-tauri/src/pyramid/db.rs:99-108` (`pyramid_chunks` table)

```sql
ALTER TABLE pyramid_chunks ADD COLUMN first_ts TEXT DEFAULT NULL;
ALTER TABLE pyramid_chunks ADD COLUMN last_ts TEXT DEFAULT NULL;
ALTER TABLE pyramid_chunks ADD COLUMN content_hash TEXT DEFAULT NULL;
```

`ALTER TABLE ADD COLUMN` is safe in SQLite. No table-recreate needed.

`content_hash`: SHA-256 (or 16-byte truncation) of chunk content. Used by Phase 3.4 for resume key invalidation.

Populate during ingest. Backfill is optional for existing chunks (NULL content_hash means "treat as always-changed").

### 3.2 Make `Topic.speaker` and `Topic.at` first-class

**File:** `src-tauri/src/pyramid/types.rs:90-108`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Topic {
    // ── Load-bearing fields: Rust business logic reads these ──
    pub name: String,
    #[serde(default)]
    pub current: String,
    #[serde(default)]
    pub entities: Vec<String>,
    #[serde(default)]
    pub corrections: Vec<Correction>,
    #[serde(default)]
    pub decisions: Vec<Decision>,

    // ── New: temporal anchors for sequential sources ──
    #[serde(default)]
    pub speaker: Option<String>,
    #[serde(default)]
    pub at: Option<String>,

    // ── Pass-through: everything else the LLM produces ──
    #[serde(flatten, default, deserialize_with = "deserialize_extra_no_temporal")]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

fn deserialize_extra_no_temporal<'de, D>(deserializer: D) -> Result<serde_json::Map<String, serde_json::Value>, D::Error>
where D: serde::Deserializer<'de> {
    let map = serde_json::Map::deserialize(deserializer)?;
    if map.contains_key("speaker") || map.contains_key("at") {
        return Err(serde::de::Error::custom(
            "speaker/at must be top-level Topic fields, not in extra"
        ));
    }
    Ok(map)
}
```

This makes the temporal fields structurally available AND prevents the LLM from accidentally putting them in the wrong place. Existing rows with no temporal data deserialize fine (`Option::None`).

### 3.3 Generic `required_fields` validation in the L0 extract step

**File:** Phase 3.0 spike output (likely `chain_executor.rs` extract primitive)

Don't hardcode "speaker"/"at" in Rust. Instead:

1. Add a struct field to `ChainStep` in `chain_engine.rs`:
```rust
#[serde(default)]
pub required_topic_fields: Option<Vec<String>>,
```

2. Verify `defaults_adapter.rs` populates the field on legacy DSL → ChainDefinition conversion.

3. At every parse site identified by Phase 3.0, after deserializing a `Topic`:
```rust
if let Some(required) = &step.required_topic_fields {
    for field in required {
        let value = topic_field_value(&topic, field);
        if value.is_none() || value.unwrap().is_empty() {
            // Reject this topic; trigger the existing parse-retry path
            return Err(...);
        }
    }
}
```

4. Chain YAML usage:
```yaml
- name: source_extract
  primitive: extract
  instruction: $prompts/question/source_extract.md
  for_each: $chunks
  required_topic_fields:
    - speaker
    - at
```

Rust enforces presence per chain config. Rust knows nothing about temporal semantics.

### 3.4 Re-ingestion idempotency by `content_hash` invalidation only

**File:** `src-tauri/src/pyramid/db.rs:1698-1700` (`clear_chunks`) and `src-tauri/src/pyramid/ingest.rs`

Today: `clear_chunks` hard-deletes, re-ingest assigns fresh `chunk_index` from 0. Resume keys hit the wrong content if source iteration order changed.

**Fix (option b only — invalidate steps on hash mismatch):**

1. On re-ingest, compute `content_hash` for each new chunk.
2. For each chunk in `pyramid_chunks`, compare new hash to old hash. If different (or missing), invalidate `pyramid_pipeline_steps` entries keyed on that chunk's `chunk_index`.
3. Old chunks whose content matches new content keep their `chunk_index` and their pipeline_steps. Old chunks whose content differs lose their pipeline_steps and the new chunk takes the index.

**DO NOT do option (a) "preserve chunk_index by content_hash matching" — that silently corrupts resume keys when source content gets *inserted* between two existing chunks.**

Pseudocode:
```rust
fn reingest_with_hash_check(conn: &Connection, slug: &str, new_chunks: Vec<ChunkContent>) -> Result<()> {
    let old_chunks = db::list_chunks_with_hash(conn, slug)?;
    let new_with_hash: Vec<_> = new_chunks.into_iter().enumerate()
        .map(|(idx, c)| (idx, sha256(&c.content), c))
        .collect();

    // Identify chunks whose hash changed at the same chunk_index
    for (idx, new_hash, _) in &new_with_hash {
        if let Some(old) = old_chunks.iter().find(|c| c.chunk_index == *idx as i64) {
            if old.content_hash.as_deref() != Some(new_hash) {
                db::invalidate_pipeline_steps_for_chunk(conn, slug, *idx as i64)?;
            }
        }
    }
    db::clear_chunks(conn, slug)?;
    db::insert_chunks_with_hash(conn, slug, new_with_hash)?;
    Ok(())
}
```

### 3.5 Phase 3 done criteria

- [ ] L0 parse site(s) located and named (3.0 spike output documented inline).
- [ ] `pyramid_chunks` has `first_ts`, `last_ts`, `content_hash` columns (schema version 4 applied). Populated on ingest.
- [ ] `Topic` has `Option<String>` `speaker` and `at` fields. `extra` deserializer rejects those keys.
- [ ] Chain YAML can declare `required_topic_fields:` and every parse site enforces them.
- [ ] `defaults_adapter.rs` populates `required_topic_fields` and `supports_staleness` on legacy DSL conversion.
- [ ] Re-ingestion invalidates `pyramid_pipeline_steps` on hash mismatch.
- [ ] An L0 node from a sequential source has populated `speaker` and `at` fields, verifiable by SQL query.

---

## Phase 4 — Bootstrap and auto-update for both tiers

Tier 1 (`copy_dir_recursive` in dev) unconditionally overwrites — destructive. Tier 2 (release standalone) only bundles ~12 files via `if !path.exists() { write }` — most prompts simply don't exist on disk in a release standalone install. Both tiers need hash-aware sync.

### 4.1 Add `include_dir` and extend the existing `build.rs`

**File:** `src-tauri/Cargo.toml`, `src-tauri/build.rs`

`src-tauri/build.rs` already exists with a sha256 + `include_bytes!` asset manifest pattern. Phase 4 EXTENDS it (does not replace) with a chains manifest.

1. Add to `Cargo.toml`:
```toml
[dependencies]
include_dir = "0.7"
```

2. In `src-tauri/build.rs`, add a `cargo:rerun-if-changed=../chains` directive and a chains manifest generator that walks `chains/` at compile time and writes a constant table `chains_manifest.rs` (in `OUT_DIR`):
```rust
// chains_manifest.rs (generated by build.rs)
pub static CHAINS_MANIFEST: &[(&str, [u8; 32])] = &[
    ("defaults/question.yaml", [0x12, 0x34, ...]),
    ("prompts/question/source_extract.md", [...]),
    // ...
];
```

3. In `chain_loader.rs`, use `include_dir!` to bundle the tree:
```rust
use include_dir::{include_dir, Dir};
static CHAINS_DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/../chains");

include!(concat!(env!("OUT_DIR"), "/chains_manifest.rs"));
```

### 4.2 Hash-aware sync (replaces both Tier 1 and Tier 2 bootstrap)

**File:** `src-tauri/src/pyramid/chain_loader.rs:202-371`

Replace the entire two-tier bootstrap with one hash-aware sync that runs in both dev and release:

```rust
pub fn sync_chains(chains_dir: &Path) -> Result<()> {
    // 1. Ensure target dir exists
    std::fs::create_dir_all(chains_dir)?;

    // 2. Load on-disk previous manifest (if any)
    let manifest_path = chains_dir.join(".bundle_manifest");
    let previous_manifest: HashMap<String, [u8; 32]> = read_manifest_or_empty(&manifest_path);

    // 3. For each bundled file
    for (rel_path, bundled_hash) in CHAINS_MANIFEST {
        let dst = chains_dir.join(rel_path);
        let bundled_content = CHAINS_DIR
            .get_file(rel_path)
            .ok_or_else(|| anyhow!("missing bundled file: {}", rel_path))?
            .contents();

        if !dst.exists() {
            // Missing on disk -> write
            std::fs::create_dir_all(dst.parent().unwrap())?;
            std::fs::write(&dst, bundled_content)?;
            continue;
        }

        // Compare disk hash to PREVIOUS manifest hash. If they match, the user
        // hasn't touched this file -> overwrite with new bundled. If they don't
        // match, the user touched it -> leave alone with a logged warning.
        let disk_hash = sha256(std::fs::read(&dst)?);
        let prev_hash = previous_manifest.get(*rel_path).copied();

        if prev_hash == Some(disk_hash) {
            // Bundled-pristine: safe to overwrite
            std::fs::write(&dst, bundled_content)?;
        } else if prev_hash.is_none() {
            // First run after upgrade (or first run ever): no previous manifest.
            // Treat as bundled-pristine and overwrite. Rationale: on a fresh
            // install, the disk version is whatever the old binary wrote, and
            // the user hasn't had a chance to edit it in this version yet. On
            // a brand-new install, dst won't exist (handled above).
            std::fs::write(&dst, bundled_content)?;
        } else {
            // User-edited: leave alone, log warning
            tracing::warn!(
                "chain file {} differs from bundled version (user edit?). Leaving on-disk version unchanged. Run with --repair-chains to overwrite.",
                rel_path
            );
        }
    }

    // 4. Write the new manifest to disk
    write_manifest(&manifest_path, CHAINS_MANIFEST)?;

    Ok(())
}
```

### 4.3 Delete dead bootstrap code

**File:** `src-tauri/src/pyramid/chain_loader.rs:225-371`

Delete:
- `copy_dir_recursive` (replaced by `sync_chains`)
- The Tier 2 hardcoded `if !path.exists() { write }` writes
- `DEFAULT_QUESTION_CHAIN`, `DEFAULT_CODE_CHAIN`, `DEFAULT_DOCUMENT_CHAIN`, etc. placeholder constants

These were carried over from the old per-file `include_str!` approach. After 4.1+4.2, the manifest is the source of truth.

### 4.4 Malformed YAML — hard error always, with `--repair-chains` flag

Plan v2.2 said "fall back to defaults in production." That hides bugs. v2.3:

- Malformed YAML is a hard error at boot. Log the file path + parse error. Halt boot.
- Add a CLI flag `--repair-chains` to the binary that re-runs `sync_chains` with `force_overwrite: true`, ignoring user-edited status. Operator runs this when they've broken a file.

### 4.5 Phase 4 done criteria

- [ ] `include_dir = "0.7"` in `Cargo.toml`.
- [ ] `src-tauri/build.rs` extended with chains manifest generation + `cargo:rerun-if-changed=../chains`.
- [ ] `sync_chains` replaces both Tier 1 and Tier 2 bootstrap.
- [ ] `copy_dir_recursive`, Tier 2 hardcoded writes, and `DEFAULT_*_CHAIN` constants deleted.
- [ ] First-run reconciliation handles upgrade-from-old-binary correctly.
- [ ] User-edited files (hash mismatch vs previous manifest) are left alone with logged warning.
- [ ] Malformed YAML is a hard error; `--repair-chains` flag exists.

---

## Phase 5 — Documentation

### 5.1 New doc tree

```
docs/chain-development/
├── README.md
├── 01-architecture.md         — content_type → resolver → chain_id → executor
├── 02-chain-yaml-reference.md — schema for chains/defaults/*.yaml + new fields
├── 03-prompt-anatomy.md       — chains/prompts/*/*.md, what each one does
├── 04-temporal-conventions.md — chunks temporal columns, Topic.speaker/at, required_topic_fields
├── 05-pillar-37.md            — prompt discipline, with examples
├── 06-forking-a-chain.md      — recipe with `conversation-legacy-chronological` as worked example
├── 07-adding-a-content-type.md — recipe (now actually possible, post Phase 2.5)
├── 08-testing-a-chain.md      — build + drill + haiku eval pattern
├── 09-troubleshooting.md      — actual failure modes from Runs 1-4 and the audits
├── 10-rust-intrinsic-chains.md — the `pipeline: rust_intrinsic` mode and the function map
└── 11-capability-flags.md     — wants_file_watcher etc., when to set what
```

---

## Migration atomicity — cross-phase

All schema changes route through the migration runner introduced in Phase 0.0. Order:

| Version | Phase | Description |
|---------|-------|-------------|
| 1 | 1.0 | Create `pyramid_orphaned_annotations` table; wire archive into rebuild paths |
| 2 | 2.1 | Create `pyramid_chain_defaults` table |
| 3 | 2.5 | Drop CHECK constraint from `pyramid_slugs` (table-recreate ritual) |
| 4 | 3.1 | Add `first_ts`, `last_ts`, `content_hash` columns to `pyramid_chunks` |
| 5 | 3.4 | Backfill `content_hash` for existing chunks (one-time computation) |

Each migration:
- Wraps in its own transaction (`BEGIN; ...; COMMIT;` or `ROLLBACK;`)
- Records its version in `pyramid_schema_version` after success
- Migration 3 wraps in `PRAGMA foreign_keys=OFF` / `PRAGMA foreign_keys=ON`
- Partial failure: rollback the failing migration's transaction; halt boot; log the error; subsequent migrations don't run

The migrations only run if their version isn't already in `pyramid_schema_version`. Idempotent across boots.

---

## Deferred / out-of-scope

These came up in the audits and are real, but don't belong in this plan (or were resolved differently than v2.2 suggested):

- **Wire publish content_type contract** with the GoodNewsEveryone repo. Cross-repo coordination — handled there. Phase 2.5's free-string ContentType makes this trivial when it lands cross-repo.
- **Transcript parser registry** (Otter, Zoom, Granola, Slack, plain `Speaker [HH:MM]:`). Phase 2.5 unblocks the registry shape (free-string content_types like `transcript.otter` map to chains via the resolver). Adding the actual parsers is its own plan because each parser is real format-specific work.
- **MCP server temporal awareness.** Phase 3 adds the columns; this plan exposes them via Rust types. Adding MCP query verbs (`temporal_range`, `chronological_drill`) is its own follow-up since MCP server lives in `mcp-server/` with its own build cycle.
- **Killing or updating the v3 question DSL and `parity.rs`.** v1 was going to rewrite this; v2.3 leaves it alone. Decide its fate in a follow-up.
- **`conversation-chronological.yaml` (the OLD design-spec file).** That file points at non-existent prompts. Either delete or repurpose. Not load-bearing; minor cleanup.
- **`stale_engine` chain-specific propagation hook system.** Phase 2.6 ships flag + log-warning only. A real per-chain hook system is its own plan if ever needed; vine staleness uses build_id propagation instead.

---

## Sequencing (single session, dependencies only)

```
Phase 0  (P0 fixes — small commits, ship first)
   0.0 schema_version table + migration runner
   0.1 UTF-8 panic
   0.2 dead instruction_map key
   0.3 dead generate_extraction_schema
   0.4 chunk_transcript regex
   0.5 Pillar 37 sweep on build.rs prompts
   │
Phase 1F-pre  (frontend constants + UI shell — parallel to Phase 1 backend)
   │
Phase 1.0  (annotations migration + archive function + rebuild path wiring)
Phase 1.3  (build_conversation audit + 1.3.0 verify Pillar 37 + 1.3.5 consumer audit)
Phase 1.4.1  (register conversation-legacy-chronological as rust_intrinsic chain)
   │
Phase 2.1  (pyramid_chain_defaults table)
Phase 2.2  (resolve_chain_for_slug + doc-comment update + caller enumeration)
Phase 2.3  (IPC commands)
   │
Phase 1.2  (dispatch fix in build_runner.rs — uses Phase 2.2's resolver)
Phase 1F-post  (wizard IPC wiring)
   │
Phase 2.5  (free-string ContentType + capability flags + CHECK migration + 13 files)
Phase 2.6  (supports_staleness flag + 5 stale_helpers files)
   │
Phase 3.0  (L0 parse site spike)
Phase 3.1  (chunks temporal columns)
Phase 3.2  (Topic typed fields)
Phase 3.3  (required_topic_fields enforcement)
Phase 3.4  (re-ingest hash-mismatch invalidation)
   │
Phase 4  (bootstrap: include_dir, build.rs extend, hash-aware sync)
   │
Phase 5  (docs)
   │
──── recursive-vine-v2 begins (sibling plan, same session) ────
```

Phase 1.2 sequences AFTER Phase 2.2 because the dispatch call site needs `resolve_chain_for_slug`. Phase 1F-post sequences AFTER Phase 2.3 because the wizard needs the IPC commands. Everything else has the obvious local order.

## Risks

1. **`build_conversation` may not be as ready-to-use as `build.rs:684+` looks.** The 1.3 audit step is the gate. If the prompt constants are broken or the function diverges from its docstring, fall back to writing a fresh chronological chain in `chains/defaults/`.
2. **Vine bunches break from Phase 0.5 / 1.3.0 prompt edits.** Vine bunches share `FORWARD_PROMPT`/`REVERSE_PROMPT`/`COMBINE_PROMPT`. Edits to scrub Pillar 37 violations also change vine output. Verify by rebuilding a test vine before declaring Phase 1 done.
3. **Phase 2.5 pyramid_slugs CHECK migration may break dependent indices/triggers** if any are missed. Read `db.rs` end-to-end before writing the migration.
4. **The L0 schema parse site (Phase 3.0) may turn out to be split across multiple files** (extract, extract_with_schema, heal_json retry path). Validator hook may need to land in 2-3 places.
5. **`include_dir!` may inflate the binary noticeably.** The full `chains/` tree is mostly markdown and YAML; should be fine. Measure during Phase 4.
6. **First-run reconciliation in Phase 4 may incorrectly preserve a stale on-disk file** if the previous manifest is missing. Mitigation: treat missing manifest as "all bundled-pristine" so the new bundled wins. Operator can `--repair-chains` if the heuristic is wrong.

## Done criteria (overall)

- [ ] Phase 0: schema_version table + 5 P0 fixes shipped.
- [ ] Phase 1: annotations archive in place; build_conversation audited + Pillar-37 clean; consumer audit complete; conversation-legacy-chronological chain registered as rust_intrinsic; dispatch routes user's main create flow through it when bound.
- [ ] Phase 1F: frontend content_type centralized; wizard exposes chain selection wired to IPC; settings panel shows active chain; fallback objects on every Record consumer.
- [ ] Phase 2: chain_defaults table + IPC + resolver layered over canonical default; ContentType opened to free-string newtype with serde transparent; CHECK constraint dropped; capability flags resolved via chain definition; supports_staleness honored everywhere stale propagates.
- [ ] Phase 3: L0 parse sites named; chunks temporal columns; Topic typed speaker/at; required_topic_fields enforced; re-ingest hash-mismatch invalidation.
- [ ] Phase 4: include_dir + extended build.rs + hash-aware sync replaces both bootstrap tiers; placeholder constants deleted; --repair-chains flag.
- [ ] Phase 5: doc tree against the now-real architecture.
- [ ] Run 4 reference rebuild: same `.jsonl` as `claudeconvotest4temporallabelingupdate`, new chronological binding, L0 has populated speaker/at, apex reads chronologically, eyeball-pass against Run 4.
- [ ] Existing slugs assigned to `question-pipeline` continue to build successfully.
- [ ] Existing vine bunches that route through `build_conversation` continue to build successfully.
- [ ] `cargo build` clean. `cargo test` passes existing suite plus new UTF-8 boundary test.
