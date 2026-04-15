# chain-binding-v2 → v2.3 — Discovery Audit Corrections

> **Audit trail.** This document captures the deltas applied to `chain-binding-v2.md` (v2.2) after a Stage 2 discovery audit (auditors C and D, blind, 2026-04-07) found multiple critical issues that needed fixing before implementation.
>
> **The new canonical plan is `chain-binding-v2.3.md`.** This file is the receipts.
>
> Audit reports: `/tmp/discovery-audit-C.md`, `/tmp/discovery-audit-D.md`.

## Summary of damage

v2.2 was the result of: v1 → 4-agent audit → v2.0 → Stage 1 informed audit → v2.1 → MPS audit → v2.2. The v2.2 plan was significantly better than v1 but the Stage 2 discovery audit still found:

- 5 critical issues from auditor D (D-1, D-2, D-3, D-4, D-6)
- 7 critical + 6 major + 9 minor from auditor C
- Several "stale snapshot" claims in v2.2 about production code that no longer match reality
- Multiple cross-phase coupling failures of the same class that killed v1
- Phase 2.5 (free-string ContentType) was massively underscoped
- Phase 4 missed half the bootstrap surface (Tier 1 `copy_dir_recursive`)
- Migration ordering, transactions, and `PRAGMA foreign_keys` were unspecified anywhere

The pattern: most criticals were integration/contract failures that look fine in isolation but break when phases interact. Same failure mode as v1.

## Verified-true (v2.2 was right)

These v2.2 claims passed source verification and survive into v2.3:

- UTF-8 panic site at `chain_executor.rs:6960-6964` exists exactly as described (verified)
- `generate_extraction_schema` is genuinely dead — zero callers in `src-tauri/` (verified)
- `instruction_map: content_type:` in `chains/defaults/question.yaml` is dead config — matcher at `chain_executor.rs:1034-1070` does not handle it (verified)
- `zip_steps` exists at `chain_executor.rs:1997-2070` (verified)
- `StorageKind::StepOnly` exists at `execution_plan.rs:367` (verified)
- `build_conversation` exists at `build.rs:684+` and is structurally complete (verified, but see C6/D-1 below — it's NOT unreachable)
- Dispatch routing at `build_runner.rs:237` is exactly as described (verified)
- `pyramid_chain_assignments` table at `chain_registry.rs:5-17` matches the description

## Verified-false (v2.2 was wrong)

### CRITICAL — verified-false claims

#### V2.2-FALSE-1 — `build_conversation` is NOT unreachable (C6 + D-1)

**v2.2 said:** "It is unreachable in default config because `build_runner.rs:237` routes Conversation to `run_decomposed_build` and never reaches `run_legacy_build`."

**Reality:** `vine.rs:569-586` already calls `build::build_conversation` directly for conversation-typed vine bunches. The function is reachable today via the vine path. It's not dead code waiting to be wired up — it's *production code* for vine bunches. Any change to `build_conversation` (Pillar 37 sweep, prompt edits, structural fix) immediately changes vine bunch behavior.

**Implications for v2.3:**
- Phase 1 framing rewritten: "make `build_conversation` reachable from the user's main conversation-pyramid create flow (currently routes through `run_decomposed_build`)" — not "make it reachable at all"
- Phase 1.3.5 consumer audit MUST list `vine.rs::run_build_pipeline` as an existing consumer
- Existing vine-built slugs ALREADY have `L0-{ci}` ID shape — Phase 1.0 annotations FK migration runs against real data, not hypothetical
- Stale engine is ALREADY silently no-oping on existing vine-built pyramids (not just hypothetically post-Phase 1)

#### V2.2-FALSE-2 — FORWARD/REVERSE/COMBINE prompts are inline `pub const`s, not file references (C2 + D-2)

**v2.2 said:** Phase 1.3.0 — locate the prompt constants, "if broken: fix the references to point at `chains/prompts/conversation-chronological/{forward,reverse,combine}.md`. Run a Pillar 37 audit on those three files BEFORE Phase 1 routes through them."

**Reality:** The constants are inline raw-string literals at `build.rs:90`, `:113`, `:135`:
```rust
pub const FORWARD_PROMPT: &str = r#"You are a distillation engine. ..."#;
pub const REVERSE_PROMPT: &str = r#"..."#;
pub const COMBINE_PROMPT: &str = r#"..."#;
```
There are no `include_str!` calls. The files in `chains/prompts/conversation-chronological/{forward,reverse,combine}.md` exist on disk but no code path reads them. The "fix references" task is meaningless; the Pillar 37 target is wrong (audit must read `build.rs` source, not the on-disk files).

**Confirmed Pillar 37 violation in production:** `build.rs:104` says `"Target: 10-15% of input length"` — that's a prescriptive output size, exactly what Pillar 37 forbids. Real bug, real fix needed.

**Implications for v2.3:**
- Phase 1.3.0 rewritten: audit the inline constants in `build.rs:90,113,135` (and `DISTILL_PROMPT` at `:157`, and any other prompt constants in `build.rs`)
- Decide whether to (a) edit them in place, or (b) extract to `chains/prompts/conversation-chronological/*.md` with `include_str!` (Phase 4's `include_dir!` makes this cheap, but not blocking)
- Sweep Pillar 37 across all of `build.rs:90-300`. Confirmed violation at `:104`; check the rest
- Note that any change here also changes vine bunch behavior

#### V2.2-FALSE-3 — Phase 1.2 dispatch pseudocode doesn't compile (C3 + D-11)

**v2.2 said:**
```rust
let assignment = chain_registry::get_assignment(&conn, slug)?;
let chain_id = assignment.unwrap_or_else(|| chain_registry::default_for(content_type));
```

**Reality:** Three signature errors in two lines:
1. `get_assignment` returns `Result<Option<(String, Option<String>)>>` — a TUPLE of `(chain_id, chain_file)`, not a bare `String`. `unwrap_or_else(|| String)` is a type error.
2. `chain_registry::default_for` does not exist anywhere in the codebase.
3. Phase 2.1 introduces `resolve_chain_for_slug(conn, slug, content_type)` as the resolver, but Phase 1.2 doesn't call it.

**Implications for v2.3:**
- Rewrite the dispatch snippet to compile against the actual signature
- Destructure the tuple: `assignment.map(|(id, _)| id).unwrap_or_else(|| resolve_chain_for_slug(...))`
- Drop the imaginary `default_for`; collapse to a single resolver name (`resolve_chain_for_slug`)
- Phase 1.2 dispatch must sequence AFTER Phase 2.2 introduces the resolver — otherwise the call site has nothing to call

#### V2.2-FALSE-4 — Phase 2.5 free-string ContentType breaks Tauri IPC and frontend Record lookups (D-3)

**v2.2 said:** "Replace `enum ContentType { Code, Document, Conversation, Question }` with `pub struct ContentType(pub String)` (newtype) ... gives `Display`/`FromStr`/`Serialize` impls without locking the value space."

**Reality (two missed problems):**
1. **Serde shape change.** A bare tuple newtype `pub struct ContentType(pub String)` serializes by default as `["code"]` (a one-element array), not `"code"`. Every Tauri IPC payload that returns `content_type` breaks. The fix is `#[serde(transparent)]`. v2.2 didn't say this.
2. **Frontend `Record<ContentType, ...>` lookups.** `CONTENT_TYPE_CONFIG[slug.content_type]` is used at `src/components/PyramidRow.tsx:30`, `src/components/PyramidDetailDrawer.tsx:297`, etc. After 2.5 the backend can return `"vine.conversation"` or `"transcript.otter"` and the lookup returns `undefined`. Subsequent `config.label` access either crashes the renderer or shows an empty pill. v2.2 only said the constant "exports the well-known names for autocomplete" — it left every existing `Record<>` consumer broken.

**Implications for v2.3:**
- Add `#[serde(transparent)]` explicitly
- Add fallback objects for unknown content_types in every `CONTENT_TYPE_CONFIG[x]` consumer (`{label: x, color: gray, icon: question-mark}`)
- Audit frontend rendering surfaces explicitly
- Check for any `tauri-bindgen` / `specta` / `ts-rs` codegen step (likely none, but worth verifying)

#### V2.2-FALSE-5 — Phase 2.5 "drop CHECK constraint" never specifies the SQLite migration (D-4)

**v2.2 said:** "Drop the CHECK entirely. Free-string means the value space is open. Migration for existing rows. No data change required — content_type is already stored as TEXT in `pyramid_slugs`."

**Reality:** `CREATE TABLE IF NOT EXISTS` is a no-op for an existing DB — the original CHECK is baked into the schema. Dropping a CHECK requires the same `CREATE NEW TABLE / INSERT / DROP / RENAME` ritual the plan called out for the annotations FK in 1.0. AND `pyramid_slugs` is the FK target for many other tables (`pyramid_batches`, `pyramid_chunks`, `pyramid_nodes`, `pyramid_chain_assignments`, the new `pyramid_chain_defaults`, `pyramid_annotations` after 1.0, `pyramid_cost_log`, etc.). Renaming `pyramid_slugs` without disabling FKs will cascade-delete every dependent row.

**Implications for v2.3:**
- Add an explicit table-recreate ritual to 2.5 mirroring 1.0
- Wrap in `PRAGMA foreign_keys=OFF; BEGIN; ...; COMMIT; PRAGMA foreign_keys=ON`
- Recreate every dependent index after the swap
- Phase 2.7 done criteria: verify on an existing DB (not a fresh one) that all dependent tables survive

#### V2.2-FALSE-6 — Plan invented `recursive-vine-v2 §5.5` and a vine namespace the vine doc doesn't ask for (D-6)

**v2.2 said:** "vines (recursive-vine-v2 §5.5) need runtime-writable defaults that YAML can't provide" and "vines no longer need a `Vine` enum variant. They just declare their content_type as e.g. `vine.conversation`, `vine.code`, `vine.me`, ..."

**Reality:** The vine plan (`docs/recursive-vine-v2-design.md`) has no §5.5. Its sections jump from §5 (Recursive Stack) to §6 (Staleness Propagation). The vine plan EXPLICITLY retains `ContentType::Vine` — its appendix says: "`ContentType::Vine` | Already exists in the enum | Wire to new evidence provider." Vine plan §10 builds on the existing `ContentType::Vine` with no namespace and no chain_id-driven dispatch. The vine doc says nothing about needing free-string content_types, runtime-writable defaults, or `supports_staleness` flags.

**Implications for v2.3:**
- Drop the false "vines need this" justification for Phase 2.5 free-string ContentType
- Drop the invented `vine.conversation` / `vine.code` namespace
- Phase 2.5 stands on its OWN merit: "adding a new content_type today requires a Rust recompile + 13 file edits + a DB migration. That's the wall every future content_type hits, vines or not. Open the type to remove the wall." That's the genuine pain point, and it's enough.
- Vines continue to use `ContentType::Vine` as the vine doc requires
- The "supports_staleness" flag (Phase 2.6) is still needed but for a different reason: the user's MAIN conversation-pyramid binding (Phase 1) creates pyramids that don't fit the question-chain shape. Vine doc §6 says vines need build_id propagation, not capability flags — different mechanism.

#### V2.2-FALSE-7 — `ContentType::Vine` already exists; `'vine'` already in CHECK (C5)

**v2.2 said (Phase 2.5 step 1):** "Replace `enum ContentType { Code, Document, Conversation, Question }` with `pub struct ContentType(pub String)`"

**Reality:** `types.rs:31-37` has 5 variants:
```rust
pub enum ContentType {
    Code,
    Conversation,
    Document,
    Vine,        // already here
    Question,
}
```
And `db.rs:56` already has `'vine'` in the CHECK constraint.

**Implications for v2.3:**
- Update Phase 2.5 to reflect the actual 5-variant starting state
- Remove the stale Phase 2.7 done criterion "`ContentType::Vine` variant added"
- Other claims in the plan that touched `types.rs` may also be stale — re-verify

#### V2.2-FALSE-8 — Phase 1.0 annotations FK shape is wrong (C1 + D-5)

**v2.2 said:** "Today: `pyramid_annotations` has `FOREIGN KEY (node_id) REFERENCES pyramid_nodes(id) ON DELETE CASCADE`."

**Reality (`db.rs:228-238`):**
```sql
CREATE TABLE IF NOT EXISTS pyramid_annotations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    slug TEXT NOT NULL,
    node_id TEXT NOT NULL,
    annotation_type TEXT NOT NULL DEFAULT 'observation',
    content TEXT NOT NULL,
    question_context TEXT,
    author TEXT NOT NULL DEFAULT 'system',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (slug, node_id) REFERENCES pyramid_nodes(slug, id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_annotations_node ON pyramid_annotations(slug, node_id);
```

The FK is composite `(slug, node_id)`, both columns NOT NULL. `ON DELETE SET NULL` requires both columns to be nullable in the child. The proposed migration would crash at table-create time.

**Implications for v2.3:** Pick one of three options, not the SET NULL written in v2.2:

- **Option A:** Drop NOT NULL on both `slug` and `node_id`. Migrate to `ON DELETE SET NULL`. Annotations whose target node disappears become orphans (NULL slug + NULL node_id). Web UI/MCP need a "view orphaned annotations" surface. Most preserves data. Most invasive schema change.
- **Option B:** Change to `ON DELETE NO ACTION` (or a deferred constraint that the rebuild path handles). The rebuild path that deletes nodes also archives annotations to a `pyramid_orphaned_annotations` table before the node delete fires. Less destructive at the schema level. Adds a sibling table.
- **Option C:** Keep `ON DELETE CASCADE` and just snapshot annotations BEFORE rebuild, restore them AFTER rebuild keyed on a stable identifier (e.g., the topic name + chunk index instead of the synthetic node id). Avoids the migration entirely. Requires picking a stable identity.

**v2.3 picks Option B** (orphaned-annotations archive) — preserves the data, doesn't require a stable cross-rebuild identity, and avoids the schema-shape gymnastics. The orphan archive is a small new table; the rebuild path inserts into it before deletion.

### MAJOR — verified-false claims

#### V2.2-FALSE-9 — Phase 2.5 ContentType refactor is 13 files / 79 references, not "5+" (C4)

**v2.2 said:** "ContentType is dispatched by exhaustive match in 5+ files (main.rs, build_runner.rs, vine.rs, IPC, wizard UI)."

**Reality (verified by grep):** ContentType references appear in **13 source files**, **79 total references**:
- `src-tauri/src/main.rs` (10)
- `src-tauri/src/pyramid/build_runner.rs` (8)
- `src-tauri/src/pyramid/slug.rs` (7)
- `src-tauri/src/pyramid/db.rs` (16)
- `src-tauri/src/pyramid/ingest.rs` (3)
- `src-tauri/src/pyramid/routes.rs` (7)
- `src-tauri/src/pyramid/vine.rs` (9)
- `src-tauri/src/pyramid/chain_executor.rs` (4)
- `src-tauri/src/pyramid/parity.rs` (1)
- `src-tauri/src/pyramid/types.rs` (10)
- `src-tauri/src/pyramid/public_html/routes_read.rs` (2)
- `src-tauri/src/pyramid/build.rs` (1)
- `src-tauri/src/pyramid/public_html/routes_ask.rs` (1)

Plus `defaults_adapter.rs:119, 709, 812, 935` (D-22) which writes hardcoded `content_type:` strings into compiled chain definitions — also needs updating.

**Implications for v2.3:**
- Phase 2.5 enumerates all 13 files explicitly
- Decision: keep the newtype `ContentType(String)` everywhere (preserves type safety, lower diff, plays nice with existing call sites that use `ContentType::Code`-style construction via well-known constants) OR do full free-string conversion
- v2.3 picks: newtype with `#[serde(transparent)]` + named constants (`pub const CODE: ContentType = ...` style). Preserves call-site readability, opens the value space, single line of serde annotation.

#### V2.2-FALSE-10 — main.rs match arms are file-watcher capability checks, not chain dispatch (D-9)

**v2.2 said:** "Replace exhaustive matches with dispatch by `chain_id`. Most of these match arms collapse entirely."

**Reality:** The `match content_type` blocks in `main.rs:3145-3245` (and similar sites) are NOT chain-dispatch. They do:
```rust
// Code: walks source paths, hashes files, populates pyramid_file_hashes
// Document: same with doc_extensions
// Conversation | Vine | Question: skips file watching entirely

if matches!(ct, ContentType::Conversation | ContentType::Vine) {
    return Ok::<(), String>(());  // skip backfill_node_ids
}
if matches!(content_type, ContentType::Conversation | ContentType::Vine) {
    return Ok(());  // skip stale engine + watcher
}
```

These are *capability* checks: "does this content type have files to watch on disk?" "does this content type need stale-engine subscription?" "does this content type use the `pyramid_file_hashes` delta path?" After Phase 2.5, when an operator creates a slug with content_type `transcript.otter` or some other free-string value, *what answers these questions*?

**Implications for v2.3:**
- Add capability flags to chain definition (or to a sibling `ContentTypeCapabilities` resolver):
  - `wants_file_watcher: bool`
  - `wants_filesystem_hashing: bool`
  - `has_filesystem_sources: bool`
  - `wants_node_id_backfill: bool`
- Replace the main.rs matches with capability lookups via the resolver
- Default values need to be safe ("don't do filesystem things" by default; opt in via chain definition)
- Without this, the very first non-built-in content_type added after 2.5 either crashes the post-build seeding or silently gets the wrong watcher behavior

#### V2.2-FALSE-11 — New chain YAML fields silently dropped without struct field additions (D-8)

**v2.2 said (Phase 2.6):** "Add a `supports_staleness: bool` field to the chain YAML schema." (Phase 3.3): "Rust enforces 'every emitted topic has a non-empty value for every named field; otherwise reject and retry/fail this iteration.'"

**Reality:** `ChainDefinition` and `ChainStep` (in `chain_engine.rs:89-200` per discovery audit) are plain serde structs without `#[serde(deny_unknown_fields)]`. Adding `supports_staleness:` to a chain YAML or `required_topic_fields:` to a step YAML will parse without error, but serde drops the value on the floor. The runtime can't read what the parser didn't bind to a field.

**Implications for v2.3:**
- Phase 2.6 sub-task: "Add `pub supports_staleness: bool` field with `#[serde(default)]` to `ChainDefinition` in `chain_engine.rs`. Verify `defaults_adapter.rs:119` (and the other 3 sites) populate it on the legacy DSL → ChainDefinition conversion path."
- Phase 3.3 sub-task: "Add `pub required_topic_fields: Option<Vec<String>>` to `ChainStep`. Same defaults_adapter coverage."
- Without these, both phases are non-functional

#### V2.2-FALSE-12 — Phase 4 misses Tier 1 `copy_dir_recursive` and existing `src-tauri/build.rs` (D-10)

**v2.2 said:** Phase 4.1 — "Switch from `include_str!` per-file to `include_dir!`." Phase 4 talks about Tier 2 only.

**Reality:** `chain_loader.rs` has TWO tiers:
- **Tier 1** (source tree present, dev mode) does `copy_dir_recursive` at `:226-235` which UNCONDITIONALLY OVERWRITES all chain files on every app start. Dev users *already* lose any local edits to `chains/*` on every launch.
- **Tier 2** (release standalone) hard-codes ~12 bundled files via `if !path.exists() { write }`. **Most prompts simply do not exist on disk** in release standalone — `chains/prompts/conversation/*`, `chains/prompts/code/*`, `chains/prompts/document/*`, `chains/prompts/conversation-chronological/*` are all missing entirely.

The plan's "DADBEAR auto-update never overwrites" framing is only true for Tier 2 and understates the bug — the truer statement is "release standalone is missing 80%+ of prompt files."

ALSO: `src-tauri/build.rs` already exists with a working asset-manifest pattern (sha256 + `include_bytes!` for the existing assets pipeline). Phase 4 should *extend* this, not "add a `build.rs`."

ALSO: `include_dir = "0.7"` is not in `Cargo.toml`'s `[dependencies]` — needs an explicit add.

**Implications for v2.3:**
- Phase 4 must replace BOTH tiers with manifest-aware sync
- Tier 1: replace `copy_dir_recursive` with hash-aware sync (don't blow away dev edits)
- Tier 2: replace the per-file `if !exists { write }` with the same hash-aware sync, sourced from `include_dir!`
- Extend existing `src-tauri/build.rs` to template a chains manifest after the existing assets manifest pattern
- Add `include_dir = "0.7"` to `Cargo.toml`
- 4.5 first-run baseline: on the FIRST startup of the new binary, the on-disk file is whatever the OLD binary wrote (possibly user-edited since). The new manifest is the new bundled hash. Naive hash-mismatch would flag every file as user-edited. Fix: ship a "first-run reconciliation" mode that also hashes against a known set of historical bundled hashes — if the on-disk file matches ANY historical bundled hash for that path, treat as bundled-pristine and overwrite. Otherwise treat as user-edited.

#### V2.2-FALSE-13 — Phase 3.4 chunk_index reuse silently corrupts resume keys on insertion (D-13)

**v2.2 said:** "(a) match by content_hash, reuse the existing `chunk_index` and skip re-inserting. Only new content gets new indices. Resume keys remain valid across re-ingestion. Or: (b) invalidate `pyramid_pipeline_steps` for any chunk whose hash doesn't match its prior version."

**Reality:** The forward/reverse/combine resume path keys steps on `(slug, primitive, chunk_index, ...)`. Option (a) fails when source content gets *inserted* between two existing chunks. If chunk 5 (existing) gets a new chunk inserted before it, what's the new chunk_index of the new content? If "5" (pushing the old to "6"), every existing forward-pass step keyed on `chunk_index=5` now points at the wrong content. Option (a) is silently corrupting on any insertion.

**Implications for v2.3:**
- Pick option (b) exclusively: "invalidate `pyramid_pipeline_steps` for any chunk whose `content_hash` differs from the prior version (or whose `chunk_index` shifted)."
- Drop option (a) entirely

#### V2.2-FALSE-14 — Migration ordering and atomicity unspecified across phases (D-14)

**v2.2 said:** Nothing about migration ordering. Each phase introduces a schema change in isolation.

**Reality:** Phases 1.0, 2.1, 2.5, 3.1 all introduce migrations that must run in order at first launch. There is no `pyramid_schema_version` table currently in `db.rs`. Without a version mechanism, every boot re-attempts the migrations and the table-recreate idempotency depends on transient state (`CREATE TABLE IF NOT EXISTS` is a no-op, but `RENAME TABLE` is not).

**Implications for v2.3:**
- Add a new Phase 0.0 or 1.0-prerequisite: introduce `pyramid_schema_version (version INTEGER NOT NULL)` table tracking applied migrations
- All migrations check the version before running; insert their version after success
- Each migration wrapped in its own transaction
- Table-recreate steps wrap in `PRAGMA foreign_keys=OFF` / `PRAGMA foreign_keys=ON`
- Spell out the migration order: schema version → 1.0 annotations FK → 2.1 chain_defaults table → 2.5 pyramid_slugs CHECK drop → 3.1 chunks columns → done
- Partial-failure handling: rollback on any migration's failure, halt boot, log the error

#### V2.2-FALSE-15 — `conversation-legacy-chronological` chain_id has no YAML record anywhere (D-20)

**v2.2 said:** Phase 1.4 — verbatim routing keyed on the literal `chain_id == "conversation-legacy-chronological"`.

**Reality:** No `chains/defaults/conversation-legacy-chronological.yaml` file. The dispatch fix will key on a magic string with no schema record. Phase 2.6's `supports_staleness` flag has no chain definition file to declare in. Phase 1F wizard has nothing to enumerate.

**Implications for v2.3:**
- Phase 1 must register the chain_id somewhere the rest of the system can see it
- Two options:
  - **Option A:** Create `chains/defaults/conversation-legacy-chronological.yaml` as a stub with `pipeline: rust_intrinsic` (a new field) and a `target_function: build_conversation` reference. ChainStep gets a new variant or the ChainDefinition gets a "rust intrinsic" mode. More invasive but unifies all chains under one schema.
  - **Option B:** Hard-code the registration in `chain_loader.rs::list_intrinsic_chains()` (new function). The intrinsic chains are listed alongside YAML chains for enumeration but don't go through the YAML parser. Less invasive, two parallel registration paths.
- v2.3 picks Option A. The "rust intrinsic" pipeline mode is a future-proofing hook anyway: any chain that's hand-coded in Rust today (build_code, build_docs, build_conversation, the question pipeline itself) can be expressed as an intrinsic chain entry. Unifies the registry.

#### V2.2-FALSE-16 — `default_chain_id` has a 17-line canonical doc-comment that v2.2 reverses without acknowledgment (D-21)

**v2.2 said:** Phase 2.2 — "becomes a real function that consults the new table."

**Reality:** The current `default_chain_id` is not a stub. Its 17-line doc comment in `chain_registry.rs:79-99` explicitly says routing every content type through `question-pipeline` is *canonical and intentional*, that legacy `code-default` / `document-default` / `conversation-default` chains are deprecated, and that operators wanting legacy behavior must opt in via the assignment table. Phase 2.2 was about to silently reverse this without updating the doc.

**Implications for v2.3:**
- Phase 2.2 explicitly acknowledges the existing canonical design
- Decision: per-content-type defaults are an OVERRIDE LAYER, not a replacement. `question-pipeline` remains the bottom fallback when no override is set
- Update the doc comment in `chain_registry.rs:79-99` to reflect the new resolver path
- Phase 2.5 free-string ContentType doesn't change this: well-known content_types still default to question-pipeline; new content_types (transcripts, etc.) need explicit chain bindings

### MINOR — verified-false claims

#### V2.2-FALSE-17 — Phase 0.4 chunk-boundary fix is narrower than described (M2)

`chunk_transcript`'s boundary trigger is `line.starts_with("--- ") && current_count >= soft_threshold`. A stray markdown `---` near the start of a chunk does NOT trigger a boundary. The bug surface is "stray `--- ` rules in the back half of a chunk," not "any markdown HR." Real bug, smaller than v2.2 claimed.

**v2.3 implication:** Restate the bug honestly. The proposed regex tightening is still fine.

#### V2.2-FALSE-18 — L0 schema parse site strong-guess is wrong (C7 + D-24)

**v2.2 said (Phase 3.0):** "Strong guess: `chain_executor.rs` around `:3806`, in the `extract` primitive's parse/heal path."

**Reality:** `chain_executor.rs:3800-3829` is layered-rebuild SKIP logic for steps below `from_depth`. Topics flow as `Vec<Value>` (untyped JSON) through most of the executor; `Topic` strongly-typed only at storage write time elsewhere. The validator chokepoint is whatever step writes the typed Value to the chunk-output store, AND there are likely multiple parse sites (extract, extract_with_schema, heal_json retry path).

**v2.3 implication:** Drop the wrong guess. Phase 3.0 says: grep for the actual extract-step output handler — the call site that takes the LLM JSON and stuffs it into `step_outputs`. Expect multiple sites; size accordingly.

#### V2.2-FALSE-19 — `default_chain_id` callers not enumerated (M3)

**v2.2 said:** "All callers updated."

**v2.3 implication:** Phase 2.2 enumerates every caller of `default_chain_id` before changing the signature. Spell out which callers gain a `&Connection` arg, which become async, which collapse to call sites of `resolve_chain_for_slug` directly.

#### V2.2-FALSE-20 — Phase 2.6 hook architecture hand-waved (M4)

**v2.2 said:** "stale_engine reads the flag at the propagation site and either runs the KEEP-link logic or dispatches to a chain-specific propagation function or no-ops with a logged warning."

**Reality:** stale_engine has no hook system today. Adding one is real work. v2.2 implied both options were available; only "log warning + no-op" is available cheaply.

**v2.3 implication:** Phase 2.6 picks "log warning + no-op for non-supporting chains." Hook system is a follow-up plan if needed. Vine staleness is handled separately by build_id propagation per the vine doc §6, NOT by the supports_staleness flag.

#### V2.2-FALSE-21 — Phase 1F sequencing contradiction (M5)

**v2.2 said:** "Phase 1F can run parallel" but "1F.4 persists chain selection via the IPC command added in Phase 2."

**v2.3 implication:** Split 1F into 1F-pre (constants centralization, dropdown UI shell) and 1F-post (IPC wiring). 1F-post sequences after Phase 2.

#### V2.2-FALSE-22 — Phase 4.4 malformed YAML fallback masks bugs (N6)

**v2.2 said:** "Hard error only in dev mode."

**v2.3 implication:** Hard error always. Provide a `--repair-chains` CLI flag that re-bootstraps from the bundled tree. There's one user; they can be told what's wrong.

#### V2.2-FALSE-23 — Phase 0.1 test scope inflated (N1)

**v2.2 said:** "every byte boundary in a 4KB window."

**v2.3 implication:** Trim to meaningful repros: em-dash at `max_chars - 1` and `max_chars - 2`, single emoji (4-byte), single CJK char (3-byte). 6 test cases, not 4096.

#### V2.2-FALSE-24 — Phase 2.5.4 frontend "free-text input" is hostile UX (N9)

**v2.2 said:** "AddWorkspace.tsx accepts free-text input as well as the dropdown options."

**v2.3 implication:** Default to dropdown of well-known content_types. Free-text behind an "advanced: type custom" toggle.

#### V2.2-FALSE-25 — Phase 0.2 line numbers off by one (N5)

**v2.2 said:** `chains/defaults/question.yaml:27-28`. Actual: line 28. Cosmetic but indicative.

#### V2.2-FALSE-26 — Phase 1.3.5 consumer audit list is incomplete (D-7)

**v2.2 listed:** routes_read.rs, render.rs, MCP server, IPC commands in main.rs, stale_engine.rs, wizard UI.

**Missed:** vine.rs::run_build_pipeline, stale_helpers.rs, stale_helpers_upper.rs, staleness.rs, staleness_bridge.rs, publication.rs, wire_publish.rs, wire_import.rs, faq.rs, webbing.rs, reconciliation.rs, partner/ module.

**v2.3 implication:** Extend the audit list to ~15 files. Note that vine.rs already produces these "structurally divergent" pyramids, so the consumer surface already has SOME handling — inventory it as a free regression baseline.

#### V2.2-FALSE-27 — Phase 4.5 first-run baseline reconciliation missing (D-25)

Discussed under V2.2-FALSE-12 above.

#### V2.2-FALSE-28 — Phase 0.1 char-truncation downstream coupling (D-12)

**v2.2 said:** "the prior shape was already broken on multi-byte input so any new invariant is fine as long as it's documented."

**v2.3 implication:** Add a sub-task: grep all chain YAMLs for `max_chars`, list call sites, confirm none are byte-budgeted. Most likely fine; quick verification.

#### V2.2-FALSE-29 — Topic.extra alongside typed fields creates two-path-rot (D-17)

**v2.2 said:** "Keep `#[serde(flatten)] extra` for forward compatibility."

**v2.3 decision:** Topic gets typed `speaker: Option<String>` and `at: Option<String>`. The `extra` flatten stays for OTHER pass-through fields the LLM emits, but a custom deserialize_with check errors if `extra` contains the keys `"speaker"` or `"at"` (defensive — prevents the LLM from accidentally putting them in the wrong place).

#### V2.2-FALSE-30 — defaults_adapter.rs hardcoded content_type strings not in 2.5 scope (D-22)

`defaults_adapter.rs:119, 709, 812, 935` write hardcoded `content_type: "code".to_string()` etc. into compiled chain definitions. Phase 2.5 needs to update these to use the named constants.

## Decisions made for v2.3 (summary)

### Architecture
- **Annotations FK migration option B**: orphaned-annotations archive table; rebuild path inserts before deletion. (Not SET NULL; not stable cross-rebuild identity.)
- **Phase 1.4 routing**: verbatim to `build_conversation` via Phase 1.4's dispatch fix. Acknowledge that vine.rs is a co-caller.
- **Phase 1.4 chain registration**: create `chains/defaults/conversation-legacy-chronological.yaml` with a new `pipeline: rust_intrinsic` mode and `target_function: build_conversation`. Unifies all chains (including hand-coded Rust ones) under one registry.
- **Phase 2.5 ContentType**: newtype `pub struct ContentType(pub String);` with `#[serde(transparent)]`. Named constants for well-known types. Capability flags on chain definition replace the main.rs match-arm checks.
- **Phase 2.5 vine justification**: dropped. Phase 2.5 stands on its own merit (recompile pain). Vines keep `ContentType::Vine`.
- **Phase 2.6 supports_staleness**: simple flag, log warning + no-op for non-supporting chains. No hook system.
- **Phase 2.6 vine staleness**: handled by build_id propagation per vine doc §6, separate mechanism from supports_staleness flag.
- **Phase 3.2 Topic**: typed `Option<String>` speaker/at + custom deserialize_with that errors if `extra` contains those keys.
- **Phase 3.4 idempotency**: invalidate steps on hash mismatch (option b). Drop chunk_index reuse (option a).
- **Phase 4 bootstrap**: hash-aware sync for BOTH Tier 1 and Tier 2; extend existing `src-tauri/build.rs`; first-run reconciliation against historical hash set.

### Foundation (NEW phases / sections in v2.3)
- **Phase 0.0**: introduce `pyramid_schema_version` table and migration runner. All migrations route through this.
- **Phase 0.6**: Pillar 37 sweep across `build.rs:90-300` prompt constants. Confirmed violation at `:104` ("Target: 10-15% of input length") + others to find.
- **Migration Atomicity section**: spell out ordering, transaction wrapping, `PRAGMA foreign_keys=OFF`, partial-failure handling.

### Plan structure
- v2.3 is a clean rewrite of v2.2 with all of the above baked in
- This delta paper preserves the audit trail
- v2.2 (`chain-binding-v2.md`) gets a SUPERSEDED banner pointing at v2.3
