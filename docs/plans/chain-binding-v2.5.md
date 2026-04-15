# chain-binding-v2.5 — Recover the Chronological Pipeline + Make Chain Binding Real

> **Status:** v2.5, written from a full-source-read pass on 2026-04-07. Supersedes v2.4.
>
> Lineage: v1 → 4-agent audit → v2.0 → Stage 1 informed audit → v2.1 → MPS audit → v2.2 → Round 1 discovery audit → v2.3 → Round 2 discovery audit → v2.4 → Round 3 discovery audit → **plan author read implicated source files end-to-end** → v2.5.
>
> Each fact in this plan is grounded in a verified file path + line range. No memory-based claims. No invented APIs.
>
> **Single-session shipping convention:** all phases plus all phases of `recursive-vine-v2-design.md` land in one session today. No rollback plans, no migration sequencing for "users on older binaries," no "ship X first as a safety beachhead."

---

## Section 0 — Verified facts (from source reads)

These are the load-bearing facts the plan rests on. Every one was verified by reading the cited file/line range.

### 0.1 Annotations are already supersession-safe

- `db.rs:481-482` says (verbatim): `// NOTE: fk_cascade_annotations_on_node_delete deliberately removed — supersession replaces deletion, annotations survive on superseded nodes.`
- `db.rs:485` actively `DROP TRIGGER IF EXISTS fk_cascade_annotations_on_node_delete` runs at every boot.
- `db.rs:141-142` defines `live_pyramid_nodes` view: `SELECT * FROM pyramid_nodes WHERE build_version > 0 AND superseded_by IS NULL`.
- `db.rs:2007-2013` `delete_nodes_above` is `#[deprecated(note = "Use supersede_nodes_above instead — delete_nodes_above destroys contributions")]`.
- `db.rs:2019-2031` `supersede_nodes_above` is the production pattern: `UPDATE pyramid_nodes SET superseded_by = ?3 WHERE slug = ?1 AND depth > ?2 AND superseded_by IS NULL`.
- The only `DELETE FROM pyramid_nodes` in source is `db.rs:2009` (deprecated) and `parity.rs:543-547` (test parity rebuild only).
- The user's main rebuild path uses supersession; annotations FK CASCADE never fires.

**Implication:** Phase 1.0 from earlier plan versions is genuinely a no-op. Drop entirely.

### 0.2 `build_conversation` is reachable and working

- Definition: `build.rs:684-1113`. Forward pass + reverse pass + combine into L0 + L1 pairing + L2 thread clustering + L3+ upper layers. Fully implemented.
- Reachable today via `vine.rs:569-586` for vine bunches: `ContentType::Conversation => build::build_conversation(reader, &write_tx, llm_config, slug, cancel, &progress_tx).await`.
- Reachable from `build_runner.rs::run_legacy_build:646-657` if `run_legacy_build` were called for a Conversation slug — but `run_build_from:237` short-circuits Conversation to `run_decomposed_build` before `run_legacy_build` is reached. So **the user's main create flow does not reach `build_conversation` today**, only the vine bunch path does.
- Function signature: `pub async fn build_conversation(db: Arc<Mutex<Connection>>, writer_tx: &mpsc::Sender<WriteOp>, llm_config: &LlmConfig, slug: &str, cancel: &CancellationToken, progress_tx: &mpsc::Sender<BuildProgress>) -> Result<i32>`.
- L0 node IDs use `format!("L0-{ci:03}")` (zero-padded 3-digit).
- Resume keys: `db::step_exists(conn, slug, "forward"|"reverse"|"combine", chunk_index, -1|0, ""|node_id)`.

### 0.3 Pillar 37 violations in `build.rs` prompt constants

Confirmed violations in inline `pub const *_PROMPT` raw strings:
- `:104` `"Target: 10-15% of input length."` — FORWARD_PROMPT JSON schema description
- `:108` `"1-2 sentences: what the conversation now knows..."` — FORWARD_PROMPT
- `:130` `"1-2 sentences: looking backward..."` — REVERSE_PROMPT
- `:147` `"2-6 word chunk name..."` — COMBINE_PROMPT
- `:163` `"3-6 coherent topics"` — DISTILL_PROMPT
- `:169` `"1-2 sentences explaining what this topic IS..."` — DISTILL_PROMPT
- `:173` `"2-6 word label..."` — DISTILL_PROMPT
- `:194` `"6-12 coherent THREADS"` — THREAD_CLUSTER_PROMPT
- `:202` `"6-12 threads total"` — THREAD_CLUSTER_PROMPT
- `:234` `"1-2 sentences"` — THREAD_NARRATIVE_PROMPT
- `:240` `"2-6 word thread label"` — THREAD_NARRATIVE_PROMPT
- `:241` `"1-2 sentences..."` — THREAD_NARRATIVE_PROMPT
- `:246` `"1-2 sentences"` — THREAD_NARRATIVE_PROMPT
- `:263` `"3-5 most complex functions"` — CODE_EXTRACT_PROMPT
- `:270` `"2-6 word file label"` — CODE_EXTRACT_PROMPT
- `:291` `"2-6 word config label"` — CONFIG_EXTRACT_PROMPT
- And more in CODE_GROUP_PROMPT and below.

Plus the matching files in `chains/prompts/conversation-chronological/{forward,reverse,combine}.md` exist on disk but **no code reads them** — they're orphaned design drafts. Source-of-truth for vine bunches and any chronological binding is the inline `build.rs` constants.

### 0.4 The UTF-8 panic site

`chain_executor.rs:6960-6964`:
```rust
let truncated = if new_val.len() > max_chars {
    new_val[..max_chars].to_string()
} else { new_val };
```
Confirmed exact. `new_val.len()` is byte length; the slice panics on multi-byte chars at the wrong boundary.

### 0.5 Dead `instruction_map: content_type:` config

- `chains/defaults/question.yaml:28` declares `content_type:conversation: "$prompts/conversation/source_extract_v2.md"`.
- `chain_executor.rs:1034-1070` (`instruction_map_prompt`) only handles prefixes `type:` (line 1052), `language:` (line 1056), `extension:` (line 1061), and `type:frontend` (line 1065). The `content_type:` prefix is never matched.
- The function takes `(step: &ChainStep, resolved_input: &Value)` — no slug, no content_type in scope. Implementing the matcher requires plumbing.

### 0.6 Dead `generate_extraction_schema()`

`extraction_schema.rs:40` defines `pub async fn generate_extraction_schema(...)`. Grep across `src-tauri/` for callers returns only the file's own internal references. Zero external callers. Safe to delete.

### 0.7 `chunk_transcript` boundary heuristic

`ingest.rs:240-282`. The trigger is `let at_boundary = line.starts_with("--- ") && current_count >= soft_threshold;` where `soft_threshold = (chunk_target_lines() as f64 * 0.7) as usize`. A markdown horizontal rule (`---`) doesn't trigger if it falls in the first 70% of a chunk, but DOES trigger in the back 30%. Real bug, narrower than v2.2 implied.

### 0.8 Existing CHECK migration pattern

- `db.rs:1018-1083` `migrate_slugs_check_constraint` adds `'vine'` to the CHECK clause. Pattern: `unchecked_transaction()` + `execute_batch` with hardcoded column list, wrapped in `PRAGMA foreign_keys=OFF/ON`. Idempotency via `sql.contains("CHECK") && !sql.contains("vine")`.
- `db.rs:1087-1148` `migrate_slugs_check_question` adds `'question'` to the CHECK clause. Same pattern, takes the hardcoded column list one step further.
- Both migrations called sequentially from `init_pyramid_db` at lines 540 and 739.
- These two migrations RUN BEFORE most of the ALTER-added columns. After they run, lines 815 onward call `migrate_online_push_columns` which ALTER-adds 12 more columns. The CHECK migrations succeed because they rebuild the table at a point when only the original columns exist.
- **Implication for v2.5:** the new "drop CHECK entirely" migration MUST run AFTER `migrate_online_push_columns` (so it sees the full accumulated schema) and MUST use a column list that matches what's actually present at that point. The cleanest approach: `PRAGMA table_info` introspection at runtime, OR a hardcoded full column list that mirrors the post-`migrate_online_push_columns` shape.

Verified accumulated ALTER columns on `pyramid_slugs` (from grep):
- :716 `archived_at` (NOT NULL: no, default: NULL)
- :853 `updated_at` (NOT NULL: yes, default: `(datetime('now'))`)
- :869 `last_published_build_id` (NOT NULL: no, default: NULL)
- :875 `pinned` (NOT NULL: yes, default: 0)
- :879 `source_tunnel_url` (NOT NULL: no, default: NULL)
- :885 `access_tier` (NOT NULL: yes, default: `'public'`)
- :889 `access_price` (NOT NULL: no, default: NULL)
- :893 `allowed_circles` (NOT NULL: no, default: NULL)
- :899 `metadata_contribution_id` (NOT NULL: no, default: NULL)
- :905 `absorption_mode` (NOT NULL: yes, default: `'open'`)
- :909 `absorption_chain_id` (NOT NULL: no, default: NULL)
- :915 `cached_emergent_price` (NOT NULL: no, default: NULL)

Plus 7 base columns from the original CREATE TABLE (`slug`, `content_type`, `source_path`, `created_at`, `last_built_at`, `node_count`, `max_depth`).

Total: 19 columns post-migration. The hardcoded column list approach is fine — there are no unknowns.

`pyramid_slugs` has **3 AFTER DELETE triggers** at `db.rs:487-505`:
- `fk_cascade_faq_on_slug_delete` → `DELETE FROM pyramid_faq_nodes WHERE slug = OLD.slug`
- `fk_cascade_cost_on_slug_delete` → `DELETE FROM pyramid_cost_log WHERE slug = OLD.slug`
- `fk_cascade_usage_on_slug_delete` → `DELETE FROM pyramid_usage_log WHERE slug = OLD.slug`

These are created via `CREATE TRIGGER IF NOT EXISTS`. **After a table-recreate of `pyramid_slugs`, SQLite drops the triggers along with the old table. They MUST be recreated.** The plan handles this by either (a) running the new migration BEFORE the trigger creates so the next boot's `IF NOT EXISTS` re-creates them, or (b) re-creating them explicitly inside the migration.

### 0.9 `ContentType` enum and its 13 reference files

Definition: `types.rs:29-37`:
```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentType {
    Code,
    Conversation,
    Document,
    Vine,
    Question,
}
```

5 variants (not 4 as v2.2 claimed). `Vine` already exists. `'vine'` is already in the CHECK at `db.rs:56`.

`from_str` at `types.rs:52` is strict — returns None for unknown values, with a `tracing::warn!`.

**Match-arm sites that need an `Other(_)` arm** (verified by reading each site):
- `main.rs:3144-3207` — exhaustive match on `&content_type` (Code / Document / Conversation|Vine|Question). Needs `Other(_) => ("[]".to_string(), "[]".to_string())`.
- `main.rs:3848-3867` — exhaustive match in ingest dispatch. Needs `Other(_) => Err("custom content_type does not yet have an ingest handler")`.
- `routes.rs:2391-2412` — exhaustive match in HTTP /ingest. Same fix as main.rs:3848.
- `vine.rs:569-586` — exhaustive match in vine bunch dispatch. Needs `Other(_) => Err(anyhow!("vine bunches do not yet support custom content_type"))`.
- `slug.rs:168-190` (`resolve_validated_source_paths`) — exhaustive match for source-path validation. Needs `Other(_) => { /* no validation; chain decides */ Ok(()) }` semantics (just don't error).
- `build_runner.rs:646-688` (`run_legacy_build`) — exhaustive match for legacy build dispatch. Needs `Other(_) => return Err(anyhow!("legacy build does not support custom content_type"))`.

**`matches!()` sites that need `ContentType::Other(_)` added** (compile cleanly without it but cause silent fall-through):
- `main.rs:3227` — `matches!(ct, ContentType::Conversation | ContentType::Vine)` controls `backfill_node_ids` skipping. Add `| ContentType::Other(_)` to skip for custom types too (safe default).
- `main.rs:3240` — `matches!(content_type, ContentType::Conversation | ContentType::Vine)` controls stale engine + watcher skipping. Add `| ContentType::Other(_)`.
- `slug.rs:134` — `matches!(content_type, ContentType::Question)` early-returns for question pyramids. No change needed (Other should fall through to the validated-paths logic).
- `slug.rs:204` — same, `normalize_and_validate_source_path`. No change needed.

**Sites that DON'T need code changes** (already compatible):
- `chain_executor.rs:4688` — `match info.content_type { ContentType::Question => ..., _ => ... }` already has `_` catchall.
- `public_html/routes_read.rs:738, 1685` — equality comparisons (`== ContentType::Question`), not match arms. Compile cleanly with new variant.
- `public_html/routes_ask.rs:648` — equality comparison. Same.
- `build.rs:3370` — equality comparison. Same.
- `ingest.rs:363, 442, 602` — `&ContentType::Conversation/Code/Document` constructions. Compile cleanly.
- `parity.rs:922, 1557` — match on STRING `&str` content_type, not enum. No change.
- `routes.rs:3489` — match on STRING `&str`. No change.
- `evidence_answering.rs:181, 411, 844` — match on `source_content_type: &str`. No change.
- `question_decomposition.rs:1392, 1430, 1477, 1709` — string-based dispatch. No change.

**`from_str` callers that need to switch to `from_str_open`** for the open-string path:
- `db.rs:1345` (in `get_slug`)
- `db.rs:1394` (in `list_slugs_filtered`)
- `db.rs:1486` (in `get_questions_referencing`)
- `main.rs:3965` (in `pyramid_create_slug`) — STAYS STRICT. The wizard validation gate must reject empty/garbage. The wizard's "advanced custom content_type" path goes through a separate IPC command that uses `from_str_open`.

### 0.10 The build dispatch graph in `build_runner.rs`

- `run_build` at :158 → `run_build_from` at :170.
- `run_build_from`:
  - Returns early if `Vine` (line 190).
  - Returns early via `run_decomposed_build` if `Question` (line 199).
  - **Returns early via `run_decomposed_build` if `Conversation` (line 237).** This is where the chronological binding hooks in.
  - Falls through to `run_ir_build` if `use_ir` flag is set (line 283).
  - Falls through to `run_chain_build` if `use_chain` flag is set (line 295).
  - Falls through to `run_legacy_build` for everything else (line 314).
- `run_chain_build` at :502 — has its OWN inline resolver at :516-528 (calls `chain_registry::get_assignment` + `default_chain_id`). Phase 2.4 must update this.
- `run_ir_build` at :562 — has its OWN inline resolver at :573-586. Same. Phase 2.4 must update this.
- `run_decomposed_build` at :702 — calls `chain_registry::default_chain_id(ct_str)` directly at :804. Phase 2.4 must update this.
- `run_legacy_build` at :622-691 — exhaustive match on `&ContentType` at :646. Already routes Conversation→build_conversation but unreachable for Conversation (because the early return at :237).

**Function signature for `run_legacy_build`** (verified `:622-628`):
```rust
async fn run_legacy_build(
    state: &PyramidState,
    slug_name: &str,
    content_type: &ContentType,
    cancel: &CancellationToken,
    progress_tx: Option<mpsc::Sender<BuildProgress>>,
    write_tx: &mpsc::Sender<WriteOp>,
) -> Result<(String, i32)>
```

**`run_build_from` parameters** (`:170-180`): `state`, `slug_name`, `from_depth`, `stop_after`, `force_from`, `cancel`, `progress_tx`, `write_tx`, `layer_tx`. **`write_tx` IS in scope** at the dispatch site. No plumbing needed.

`run_legacy_build` returns `Result<(String, i32)>` (2-tuple). `run_build_from` returns `Result<(String, i32, Vec<StepActivity>)>` (3-tuple). The existing call site at :314 wraps via `.map(|(apex, failures)| (apex, failures, vec![]))`. The new dispatch path needs the same wrapper.

The `from_depth > 0` gate at :309-312 only fires for the legacy path branch. If we add a new dispatch path for `conversation-legacy-chronological` at line 237 (BEFORE the use_ir / use_chain branching), we need to either accept `from_depth > 0` for it or add a check.

### 0.11 Staleness entry points

Reading the 5 stale_*.rs files for actual staleness propagation entry points (not file-watcher dispatchers):

- `staleness.rs:126` `propagate_staleness(conn, slug, deltas, threshold)` — the canonical staleness propagation. Has both `conn` and `slug` in scope. Walks evidence KEEP links. **Single check here suffices for "skip if chain produces no KEEP links."**
- `staleness_bridge.rs:65` `run_staleness_check(conn, slug, ...)` — calls `propagate_staleness` internally. Doesn't need a duplicate check.
- `stale_engine.rs:547` `drain_and_dispatch(slug, ...)` — file-watcher pipeline. Conversation-legacy-chronological pyramids don't have file watchers (single .jsonl source), so this path never fires for them. No check needed.
- `stale_helpers.rs` and `stale_helpers_upper.rs` `dispatch_*` functions — sub-functions of the file-watcher pipeline. Not staleness propagation entry points; not relevant to chain-chain interactions.

**Implication for Phase 2.6:** ONE check, in `propagate_staleness`. Not 14.

### 0.12 L0 Topic parse sites

`grep parse_topics|Vec<Topic>|from_str.*Topic|deserialize.*Topic`:
- `chain_dispatch.rs:360` — `let topics: Vec<Topic> = output.get("topics").and_then(|t| t.as_array()).map(|arr| arr.iter().filter_map(|t| serde_json::from_value(t.clone()).ok()).collect())` — silently filters out any Topic that fails to parse.
- `build.rs:534` — same pattern. Used by build_conversation/build_code/build_docs.
- `delta.rs:694` — same pattern. Used by delta processing.
- `types.rs:75` — type definition only.

Phase 3.3 enforcement adds a helper `parse_topics_with_required_fields(value: &Value, required: Option<&[String]>) -> Vec<Topic>` and replaces these 3 sites with calls to it.

### 0.13 `chain_loader.rs` bootstrap

371 lines total:
- `load_chain` at :17 — reads YAML, parses to ChainDefinition, resolves `$prompts/...` refs to file contents, validates.
- `discover_chains` at :108 — walks `chains/defaults` and `chains/variants` directories.
- `ensure_default_chains` at :202 — bootstrap. Two tiers:
  - **Tier 1** (`source_chains_dir` is set, dev mode): `copy_dir_recursive(src, chains_dir); return Ok(())`. Unconditional overwrite.
  - **Tier 2** (no source tree): hardcoded `if !path.exists() { write }` for ~12 files (5 default chains, 1 planner prompt, 4 question prompts, 2 shared prompts).
- `copy_dir_recursive` at :298-318 — recursive `std::fs::write` (overwrite).
- 3 placeholder constants `DEFAULT_CONVERSATION_CHAIN` / `DEFAULT_CODE_CHAIN` / `DEFAULT_DOCUMENT_CHAIN` at :322-371. Each one is a stub with a `placeholder` step.

**Total prompt files in `chains/prompts/`**: 92 (verified by `find chains/prompts -type f | wc -l`). Tier 2 bundles only 7 of them. Most prompts are missing from a release standalone install.

### 0.14 Existing `src-tauri/build.rs`

80 lines. Uses `sha2::Sha256` + `include_bytes!` to bundle frontend assets (`assets/*.css`, `*.js`, `*.woff2`, `favicon.ico`). Generates `OUT_DIR/asset_manifest.rs` at compile time. Calls `tauri_build::build()` at the end. Easy to extend with chain manifest generation.

### 0.15 `Cargo.toml` deps

No `include_dir`. Adding `include_dir = "0.7"` to `[dependencies]` is the only change needed for Phase 4.

### 0.16 `ChainDefinition` schema

`chain_engine.rs:89-102`. Required fields (no `#[serde(default)]`): `schema_version`, `id`, `name`, `description`, `content_type`, `version`, `author`, `defaults`, `steps`. Optional: `post_build`. **Cannot add a YAML chain that omits any of the required fields without first adding `#[serde(default)]` annotations.** The validator at `chain_engine.rs:366` requires at least 1 step (`def.steps.is_empty()` errors). The validator restricts `content_type` to `["conversation", "code", "document", "question"]` (no 'vine').

**Implication for Phase 2.4:** The "register conversation-legacy-chronological as a YAML chain" approach from v2.3 was infeasible because of the required-fields constraint and the empty-steps check. Drop the YAML registration approach. Use a hardcoded magic string in the dispatch path.

---

## Section 1 — Premise

After reading the source end-to-end, the work breaks down into:

1. **Phase 0 — P0 fixes** (small, focused, no architecture).
2. **Phase 1 — Audit + verification** (no code changes; confirms what's already true).
3. **Phase 1F — Frontend constants centralization + wizard UI shell.**
4. **Phase 2 — Chain binding override layer** (`pyramid_chain_defaults` table + resolver + IPC + dispatch fix in 4 sites + magic string for chronological binding).
5. **Phase 2.5 — Open `ContentType` with `Other(String)` variant** (newtype-wrapped enum with manual serde, 6 match-arm sites + 2 matches!() updates + 3 from_str-call-site swaps + CHECK drop migration).
6. **Phase 2.6 — Stale propagation skip for chronological chain** (one check at `staleness.rs::propagate_staleness`).
7. **Phase 3 — Temporal first-class data** (chunks columns + Topic typed fields + `required_topic_fields` validator at 3 parse sites + re-ingest hash invalidation).
8. **Phase 4 — Bootstrap fixes** (`include_dir` for Tier 2, extend `build.rs` manifest, settings panel "re-sync" IPC, delete placeholder constants).
9. **Phase 5 — Documentation** (against the now-real architecture).

The plan is grounded in verified code locations. There are no "find this site at implementation time" hand-waves.

---

## Section 2 — Phase 0: P0 fixes

### 0.1 UTF-8 panic in `update_accumulators`

**File:** `chain_executor.rs:6960-6964`

```rust
// Replace:
let truncated = if new_val.len() > max_chars {
    new_val[..max_chars].to_string()
} else { new_val };

// With (max_chars now interpreted as char count, not byte count):
let truncated = new_val
    .char_indices()
    .nth(max_chars)
    .map(|(i, _)| new_val[..i].to_string())
    .unwrap_or(new_val);
```

**Sub-tasks:**
- Grep `chains/` for `max_chars` usage. Verify no chain treats it as bytes. (Quick search; expected: zero.)
- Add a unit test in `chain_executor.rs` that exercises em-dash, smart quote, CJK char, emoji at boundary positions. 6 cases.

### 0.2 Delete dead `instruction_map: content_type:` config

**File:** `chains/defaults/question.yaml:28`

Delete the line `      content_type:conversation: "$prompts/conversation/source_extract_v2.md"`. Add a one-line comment pointing at Phase 2 (which subsumes per-content-type prompt routing via the resolver).

The matcher in `chain_executor.rs:1034-1070` only handles `type:`/`language:`/`extension:`/`type:frontend` prefixes. The `content_type:` key is silently ignored today.

### 0.3 Delete `generate_extraction_schema()` (function only, not the whole file)

**File:** `extraction_schema.rs:40` only.

`generate_extraction_schema` (the async LLM-based schema generator at :40) is dead — zero callers in `src-tauri/`. **However, the file also contains `generate_synthesis_prompts` (called from `chain_executor.rs:4738`) and the `ExtractionSchema` struct used by other code paths. The file as a whole is NOT dead.**

Action: delete ONLY the `generate_extraction_schema` function and any helpers used solely by it (`parse_extraction_schema_response`, the `extraction_schema_temperature`/`max_tokens` config-tier reads inside that function). Keep `generate_synthesis_prompts`, `ExtractionSchema` struct, `collect_leaf_questions`, and the test module. Run `cargo check` to confirm nothing breaks.

### 0.4 Tighten `chunk_transcript` regex

**File:** `ingest.rs:257`

```rust
// Replace:
let at_boundary = line.starts_with("--- ") && current_count >= soft_threshold;

// With:
let at_boundary = is_speaker_boundary(line) && current_count >= soft_threshold;

// New helper above the function:
fn is_speaker_boundary(line: &str) -> bool {
    if !line.starts_with("--- ") {
        return false;
    }
    // Require at least one ASCII uppercase letter (A-Z) immediately after `--- `
    line.as_bytes().get(4).map(|c| c.is_ascii_uppercase()).unwrap_or(false)
}
```

This rejects markdown horizontal rules (`---` followed by nothing or whitespace) but accepts speaker labels like `--- PLAYFUL`, `--- CONDUCTOR`, `--- ALICE`.

### 0.5 Pillar 37 sweep on `build.rs` prompts

**Files:** `build.rs:90-300` (FORWARD_PROMPT, REVERSE_PROMPT, COMBINE_PROMPT, DISTILL_PROMPT, THREAD_CLUSTER_PROMPT, THREAD_NARRATIVE_PROMPT, CODE_EXTRACT_PROMPT, CONFIG_EXTRACT_PROMPT, CODE_GROUP_PROMPT and the rest)

Verified violations (Section 0.3 above):
- `:104` `"Target: 10-15% of input length."` → `"The chunk compressed to maximum density. Every decision, name, mechanism, correction preserved."`
- `:108` `"1-2 sentences: what the conversation now knows that it didn't before"` → `"What the conversation now knows that it didn't before."`
- `:130` `"1-2 sentences: looking backward from the end, what in this chunk matters?"` → `"Looking backward from the end, what in this chunk matters."`
- `:147` `"2-6 word chunk name that helps a human recognize this chunk later"` → `"A label that helps a human recognize this chunk later."`
- `:163` `"3-6 coherent topics"` → `"the coherent topics"`
- `:169` `"1-2 sentences explaining what this topic IS right now"` → `"What this topic IS right now."`
- `:173` `"2-6 word label for the parent node itself. Concrete and human-friendly."` → `"A concrete, human-friendly label for the parent node."`
- `:194` `"6-12 coherent THREADS"` → `"the coherent THREADS"`
- `:202` `"6-12 threads total. Fewer is better if the coverage is complete."` → `"Use as few threads as cover the material completely."`
- `:234` `"1-2 sentences"` → remove the word-count bound
- `:240` `"2-6 word thread label"` → `"A concrete, human-friendly thread label."`
- `:241` `"1-2 sentences..."` → remove
- `:246` `"1-2 sentences"` → remove
- `:263` `"For the 3-5 most complex functions, describe..."` → `"For the most complex functions, describe..."`
- `:270` `"2-6 word file label"` → `"A concrete file label."`
- `:291` `"2-6 word config label"` → same

Plus any others found by grep `at least|minimum|maximum|exactly N|\d+%|\d+-\d+ ` in `build.rs:90-300`.

**Tag:** the commit message must explicitly note "vine bunches share these constants — verifying via test rebuild." After landing, rebuild any existing vine slug as a smoke check.

### 0.6 Phase 0 done criteria

- [ ] `update_accumulators` no longer panics on multi-byte input. Test passes.
- [ ] `instruction_map: content_type:` removed from `chains/defaults/question.yaml`.
- [ ] `extraction_schema.rs` deleted, `cargo check` clean.
- [ ] `chunk_transcript` no longer false-triggers on markdown `---` rules.
- [ ] `build.rs` prompt constants Pillar-37-clean.
- [ ] Test vine rebuild succeeds with the scrubbed prompts.

Ship as 5 small commits.

---

## Section 3 — Phase 1: Audit + verification (no code changes)

### 1.0 Annotations (no work)

Verified in Section 0.1 that annotations are already supersession-safe. Production rebuild uses `supersede_nodes_above` (`db.rs:2019`); no DELETE FROM pyramid_nodes ever fires in the user path. The trigger `fk_cascade_annotations_on_node_delete` is explicitly DROPPED at `db.rs:485`. **Phase 1.0 from earlier plan versions is dropped entirely.**

### 1.1 `build_conversation` audit

Verified at `build.rs:684-1113`:
- Forward pass walks chunks 0..N, calls `FORWARD_PROMPT`, persists via `send_save_step` keyed on `(slug, "forward", ci, -1, "")`.
- Reverse pass walks N..0, calls `REVERSE_PROMPT`, persists via `(slug, "reverse", ci, -1, "")`.
- Combine pass walks 0..N, joins forward + reverse JSON, calls `COMBINE_PROMPT`, builds L0 node via `node_from_analysis`, persists via `send_save_node`. Resume keys: `(slug, "combine", ci, 0, "L0-{ci:03}")`.
- L1 pairing: `build_l1_pairing` (separate function).
- L2 thread clustering: `build_threads_layer`.
- L3+ upper layers: `build_upper_layers`.
- Cancellation handling: checked at top of each loop iteration.
- Resume: each pass restores running_context from prior step output and skips already-completed chunks.

**No surprises.** The function works.

### 1.2 Pillar 37 verification post Phase 0.5

After Phase 0.5 lands, re-read FORWARD_PROMPT, REVERSE_PROMPT, COMBINE_PROMPT, DISTILL_PROMPT and confirm no prescriptive output sizing remains. This is a verification step, not a code change.

### 1.3 Consumer surface inventory (free baseline)

Vine bunches (`vine.rs:571`) ALREADY produce structurally chronological pyramids (no question tree, no evidence verdicts, no FAQ). The consumer surface ALREADY handles them — find vine-built slug behavior and confirm.

**Inventory:**
- `live_pyramid_nodes` view (db.rs:142) — works on supersession; serves both shapes.
- Web/MCP read paths read from `live_pyramid_nodes` — work on both shapes.
- FAQ generation (`faq.rs`) — only fires when question tree exists; quietly skips for non-question chains.
- Stale propagation (`staleness.rs:propagate_staleness`) — silently no-ops when no KEEP links exist. Phase 2.6 turns the silent no-op into a logged skip.
- Web edges (`webbing.rs`) — only generated by question pipeline. Vine bunches and chronological pyramids don't have them; consumers handle absence.

**Conclusion:** the consumer surface is already chronological-pyramid-compatible because vines have been using `build_conversation` all along. Phase 1 is a verification step, not a remediation step.

### 1.4 Phase 1 done criteria

- [ ] Annotations no-op verified.
- [ ] `build_conversation` audited.
- [ ] Pillar 37 clean post Phase 0.5.
- [ ] Consumer surface confirmed (no new code; just inspection).

---

## Section 4 — Phase 1F: Frontend centralization + wizard

### 1F.1 Centralize content_type list

**New file:** `src/lib/contentTypes.ts`

```typescript
export const WELL_KNOWN_CONTENT_TYPES = [
  'code',
  'document',
  'conversation',
  'question',
  'vine',
] as const;

export type WellKnownContentType = typeof WELL_KNOWN_CONTENT_TYPES[number];

// After Phase 2.5 ships, ContentType becomes a free string at the type level.
export type ContentType = string;

export const CONTENT_TYPE_CONFIG: Record<string, { label: string; color: string; icon: string }> = {
  code: { label: 'Code', color: 'blue', icon: 'code' },
  document: { label: 'Document', color: 'green', icon: 'document' },
  conversation: { label: 'Conversation', color: 'purple', icon: 'chat' },
  question: { label: 'Question', color: 'amber', icon: 'question' },
  vine: { label: 'Vine', color: 'rose', icon: 'vine' },
};

export function getContentTypeConfig(ct: string) {
  return CONTENT_TYPE_CONFIG[ct] ?? {
    label: ct,
    color: 'gray',
    icon: 'question-mark',
  };
}
```

### 1F.2 Update existing call sites

Grep `src/` for `content_type` and update each to import from the central file. Replace direct `CONTENT_TYPE_CONFIG[x]` lookups with `getContentTypeConfig(x)`.

### 1F.3 Wizard UI shell

In `AddWorkspace.tsx`, add a chain selector below the content_type dropdown that appears for `conversation`. Default option: "Question pipeline (default)". Second option: "Chronological / forward+reverse+combine". The selector saves the operator's choice as a chain id ('question-pipeline' or 'conversation-legacy-chronological') to be persisted via Phase 1F-post (after Phase 2.3 IPC).

### 1F-post (after Phase 2.3 ships)

Wire the wizard chain selector to call the new IPC commands `set_chain_default_cmd(content_type, chain_id)` (for the default) or `assign_chain_to_slug_cmd(slug, chain_id)` (for per-slug). Add a settings panel button "Re-sync chain bundles" that calls `repair_chains_cmd`.

### 1F done criteria

- [ ] `src/lib/contentTypes.ts` exists.
- [ ] All `src/` content_type references go through it.
- [ ] All `CONTENT_TYPE_CONFIG[x]` lookups have a fallback.
- [ ] Wizard chain selector renders for conversation.
- [ ] Wizard chain selection persists via IPC after Phase 2.3.
- [ ] Settings panel "Re-sync chains" button works after Phase 4.3.

---

## Section 5 — Phase 2: Chain binding override layer

### 2.1 New `pyramid_chain_defaults` table

**File:** `db.rs::init_pyramid_db`, near the existing `chain_registry::init_chain_tables(conn)?` call at `:543`.

Use the existing `let _ = conn.execute(...)` pattern (no parallel migration runner needed):

```rust
let _ = conn.execute(
    "CREATE TABLE IF NOT EXISTS pyramid_chain_defaults (
        content_type TEXT PRIMARY KEY,
        chain_id TEXT NOT NULL,
        updated_at TEXT NOT NULL DEFAULT (datetime('now'))
    )",
    [],
);
```

Idempotent. Safe.

### 2.2 `resolve_chain_for_slug` resolver

**File:** `chain_registry.rs`, alongside the existing `default_chain_id`.

```rust
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

/// Resolve the chain ID for a slug build, consulting overrides in this order:
///   1. per-slug assignment (highest priority)
///   2. per-content-type default override
///   3. canonical default (`default_chain_id`, currently always `"question-pipeline"`)
pub fn resolve_chain_for_slug(
    conn: &Connection,
    slug: &str,
    content_type: &str,
) -> Result<String> {
    if let Some((chain_id, _)) = get_assignment(conn, slug)? {
        return Ok(chain_id);
    }
    if let Some(chain_id) = get_chain_default(conn, content_type)? {
        return Ok(chain_id);
    }
    Ok(default_chain_id(content_type).to_string())
}
```

**Update the doc-comment on `default_chain_id`** at `:79-99` to add a final paragraph:

```
/// **As of v2.5:** per-content-type *overrides* exist via `pyramid_chain_defaults`
/// (resolved by `resolve_chain_for_slug`). Operators can set an override per
/// content_type, which is consulted before this canonical default. Per-slug
/// assignments still take highest priority. The canonical default here is the
/// bottom fallback when neither override is set.
```

### 2.3 IPC commands

Add to `main.rs` near the other `pyramid_*` commands:

```rust
#[tauri::command]
async fn pyramid_set_chain_default(
    state: tauri::State<'_, SharedState>,
    content_type: String,
    chain_id: String,
) -> Result<(), String> {
    let conn = state.pyramid.writer.lock().await;
    wire_node_lib::pyramid::chain_registry::set_chain_default(&conn, &content_type, &chain_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn pyramid_get_chain_default(
    state: tauri::State<'_, SharedState>,
    content_type: String,
) -> Result<Option<String>, String> {
    let conn = state.pyramid.reader.lock().await;
    wire_node_lib::pyramid::chain_registry::get_chain_default(&conn, &content_type)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn pyramid_list_available_chains(
    state: tauri::State<'_, SharedState>,
) -> Result<Vec<ChainSummary>, String> {
    // Hardcoded list: well-known intrinsic + discovered YAML chains.
    let mut chains = vec![
        ChainSummary {
            id: "question-pipeline".to_string(),
            description: "Question decomposition + evidence loop (default for all content types)".to_string(),
            kind: "yaml".to_string(),
        },
        ChainSummary {
            id: "conversation-legacy-chronological".to_string(),
            description: "Forward + reverse + combine chronological pipeline (Rust intrinsic)".to_string(),
            kind: "rust_intrinsic".to_string(),
        },
    ];
    // Append discovered YAML chains
    if let Ok(discovered) = wire_node_lib::pyramid::chain_loader::discover_chains(&state.pyramid.chains_dir) {
        for meta in discovered {
            if !chains.iter().any(|c| c.id == meta.id) {
                chains.push(ChainSummary {
                    id: meta.id,
                    description: meta.name,
                    kind: "yaml".to_string(),
                });
            }
        }
    }
    Ok(chains)
}

#[derive(Serialize)]
struct ChainSummary {
    id: String,
    description: String,
    kind: String,
}
```

Register in `tauri::Builder::default().invoke_handler(tauri::generate_handler![..., pyramid_set_chain_default, pyramid_get_chain_default, pyramid_list_available_chains])`.

### 2.4 Dispatch fix at 4 sites

#### 2.4.1 The Conversation block at `build_runner.rs:237`

Replace the existing block with:

```rust
// ── Conversation dispatch ──────────────────────────────────────────
if content_type == ContentType::Conversation {
    // Check chain assignment for chronological binding override
    let chain_id = {
        let conn = state.reader.lock().await;
        chain_registry::resolve_chain_for_slug(&conn, slug_name, "conversation")
            .unwrap_or_else(|_| "question-pipeline".to_string())
    };

    if chain_id == "conversation-legacy-chronological" {
        // Chronological binding: dispatch directly to build::build_conversation.
        // This is reachable through vine.rs:571 today; this fix adds the user's
        // main conversation-pyramid create flow as a second caller.
        info!(slug = slug_name, "using conversation-legacy-chronological binding");

        // build_conversation does not support partial rebuild parameters.
        // Reject from_depth>0, stop_after, and force_from to avoid silent
        // full-rebuild surprises when the user requested partial rebuild.
        if from_depth > 0 {
            return Err(anyhow!(
                "conversation-legacy-chronological does not support from_depth > 0; \
                 partial rebuild is only available via the question pipeline"
            ));
        }
        if stop_after.is_some() || force_from.is_some() {
            return Err(anyhow!(
                "conversation-legacy-chronological does not support stop_after / force_from; \
                 these are only available via the chain-engine path"
            ));
        }

        let llm_config = state.config.read().await.clone();

        // build_conversation requires &Sender, but our parameter is Option<Sender>.
        // Mirror the run_legacy_build pattern: create an owned drain channel if None.
        let owned_tx;
        let ptx: &mpsc::Sender<BuildProgress> = match progress_tx {
            Some(ref tx) => tx,
            None => {
                let (tx, mut rx) = mpsc::channel::<BuildProgress>(16);
                tokio::spawn(async move { while rx.recv().await.is_some() {} });
                owned_tx = tx;
                &owned_tx
            }
        };

        let failures = build::build_conversation(
            state.reader.clone(),
            write_tx,
            &llm_config,
            slug_name,
            cancel,
            ptx,
        )
        .await?;

        return Ok(("legacy-chronological".to_string(), failures, vec![]));
    }

    // ── Default Conversation path: question pipeline ────────────
    // (existing run_decomposed_build call follows unchanged)
    let (apex_question, stored_granularity, stored_max_depth) = {
        let conn = state.reader.lock().await;
        match db::get_question_tree(&conn, slug_name)? {
            // ... existing code from :241-262 ...
        }
    };
    return Box::pin(run_decomposed_build(/* ... existing args ... */)).await;
}
```

#### 2.4.2 `run_chain_build` inline resolver at `:516-528`

Replace:

```rust
let chain_id = {
    let conn = state.reader.lock().await;
    match chain_registry::get_assignment(&conn, slug_name)? {
        Some((id, _file)) => { info!(...); id }
        None => {
            let default_id = chain_registry::default_chain_id(ct_str).to_string();
            info!(...); default_id
        }
    }
};
```

with:

```rust
let chain_id = {
    let conn = state.reader.lock().await;
    chain_registry::resolve_chain_for_slug(&conn, slug_name, ct_str)?
};
info!(slug = slug_name, chain_id = %chain_id, "resolved chain");
```

#### 2.4.3 `run_ir_build` inline resolver at `:573-586`

Same replacement pattern.

#### 2.4.4 `run_decomposed_build` at `:804`

Replace:

```rust
let default_chain_id = chain_registry::default_chain_id(ct_str);
```

with:

```rust
let default_chain_id_string = {
    let conn = state.reader.lock().await;
    chain_registry::resolve_chain_for_slug(&conn, slug_name, ct_str)?
};
let default_chain_id: &str = &default_chain_id_string;
```

#### 2.4.5 Hardcoded magic-string protection

`run_chain_build`'s `discover_chains` lookup at `:534-545` will fail if `chain_id == "conversation-legacy-chronological"` because no YAML defines it. Guard against this: at the top of `run_chain_build`, after resolving `chain_id`, return an error early if the chain is the chronological intrinsic but the slug isn't `Conversation`:

```rust
if chain_id == "conversation-legacy-chronological" {
    return Err(anyhow!(
        "chain '{}' is only supported for Conversation content_type — \
         currently routes through the dispatch fix in run_build_from",
        chain_id
    ));
}
```

(In practice this check is unreachable because the Conversation dispatch fix at 2.4.1 short-circuits before `run_chain_build` is reached. The guard is defense-in-depth for non-Conversation slugs that someone manually assigns the chronological chain to.)

Same guard in `run_ir_build`.

### 2.5 Phase 2 done criteria

- [ ] `pyramid_chain_defaults` table exists.
- [ ] `resolve_chain_for_slug` resolves per-slug → per-content-type → canonical fallback.
- [ ] IPC commands registered.
- [ ] `build_runner.rs:237` Conversation block routes to `build::build_conversation` when assigned chain is `conversation-legacy-chronological`.
- [ ] `run_chain_build`, `run_ir_build`, `run_decomposed_build` all use `resolve_chain_for_slug`.
- [ ] Magic-string guard in non-Conversation paths returns clear error.
- [ ] Test: build a conversation pyramid with `pyramid_chain_defaults[conversation] = 'conversation-legacy-chronological'`, confirm it executes `build::build_conversation`.
- [ ] Test: build a conversation pyramid with default chain, confirm it executes `run_decomposed_build`.

---

## Section 6 — Phase 2.5: Open `ContentType` with `Other(String)` variant

### 2.5.1 Enum + manual serde

**File:** `types.rs:29-65`

Replace the existing enum + impl with:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ContentType {
    Code,
    Conversation,
    Document,
    Vine,
    Question,
    Other(String),
}

impl ContentType {
    /// Convert to the lowercase string stored in SQLite.
    pub fn as_str(&self) -> &str {
        match self {
            ContentType::Code => "code",
            ContentType::Conversation => "conversation",
            ContentType::Document => "document",
            ContentType::Vine => "vine",
            ContentType::Question => "question",
            ContentType::Other(s) => s.as_str(),
        }
    }

    /// STRICT: returns None for unknown values. Used by validation gates that
    /// must reject empty/garbage strings (e.g., main.rs:3965 wizard create).
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "code" => Some(ContentType::Code),
            "conversation" => Some(ContentType::Conversation),
            "document" => Some(ContentType::Document),
            "vine" => Some(ContentType::Vine),
            "question" => Some(ContentType::Question),
            other => {
                tracing::warn!("Unknown content type via strict from_str: '{other}'");
                None
            }
        }
    }

    /// OPEN: maps unknown values to Other(s). Used by:
    ///  - DB read sites (db.rs:1345/1394/1486) — accept any stored value
    ///  - IPC paths that intentionally accept custom content_types
    /// Empty/whitespace strings still return None.
    pub fn from_str_open(s: &str) -> Option<Self> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return None;
        }
        Some(Self::from_str(trimmed).unwrap_or_else(|| ContentType::Other(trimmed.to_string())))
    }

    pub fn is_well_known(&self) -> bool {
        !matches!(self, ContentType::Other(_))
    }
}

impl serde::Serialize for ContentType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where S: serde::Serializer {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for ContentType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where D: serde::Deserializer<'de> {
        let s = String::deserialize(deserializer)?;
        ContentType::from_str_open(&s)
            .ok_or_else(|| serde::de::Error::custom("content_type must be a non-empty string"))
    }
}

impl std::fmt::Display for ContentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}
```

**Wire compatibility:**
- The manual `Serialize` produces a bare lowercase string (`"code"`, `"conversation"`, etc.) — identical to the current `#[serde(rename_all = "lowercase")]` enum output.
- The manual `Deserialize` accepts any non-empty string. Well-known strings parse to their enum variant; unknown strings parse to `Other(s)`.
- Tauri IPC payloads continue to round-trip cleanly.

### 2.5.2 Update `from_str` callers in `db.rs`

**Files:** `db.rs:1345`, `:1394`, `:1486`

For `:1345` and `:1394` (which currently default to `Document`), replace:
```rust
ContentType::from_str(&ct_str).unwrap_or_else(|| {
    tracing::warn!("Unknown content_type '{ct_str}' for slug, defaulting to Document");
    ContentType::Document
})
```
with:
```rust
ContentType::from_str_open(&ct_str).unwrap_or(ContentType::Document)
```

For `:1486` (which currently defaults to `Question` because the SQL filter is `s.content_type = 'question'`), replace:
```rust
ContentType::from_str(&ct_str).unwrap_or(ContentType::Question)
```
with:
```rust
ContentType::from_str_open(&ct_str).unwrap_or(ContentType::Question)
```

The fallback only fires for empty strings, which shouldn't occur in production. Open content_types round-trip as `Other(s)` rather than being silently coerced.

`main.rs:3965` (`pyramid_create_slug`) keeps the strict `from_str`. The wizard's "advanced custom content_type" toggle goes through a separate IPC command (`pyramid_create_slug_open` or similar) that uses `from_str_open` and validates the string is non-empty + matches a permitted charset.

#### 2.5.2a New IPC command for open content_types (Phase 1F-post integration)

```rust
#[tauri::command]
async fn pyramid_create_slug_open(
    state: tauri::State<'_, SharedState>,
    slug: String,
    content_type: String,
    source_path: String,
    referenced_slugs: Option<Vec<String>>,
) -> Result<SlugInfo, String> {
    // Validate the open content_type string
    let trimmed = content_type.trim();
    if trimmed.is_empty() {
        return Err("content_type cannot be empty".to_string());
    }
    if trimmed.len() > 64 {
        return Err("content_type too long (max 64 chars)".to_string());
    }
    if !trimmed.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_') {
        return Err("content_type contains invalid characters (only a-z, 0-9, -, ., _ allowed)".to_string());
    }
    let ct = ContentType::from_str_open(trimmed).ok_or_else(|| "invalid content_type".to_string())?;
    // Rest mirrors pyramid_create_slug
    let normalized_source_path = wire_node_lib::pyramid::slug::normalize_and_validate_source_path(
        &source_path, &ct, state.pyramid.data_dir.as_deref(),
    ).map_err(|e| e.to_string())?;
    let conn = state.pyramid.writer.lock().await;
    let info = wire_node_lib::pyramid::slug::create_slug(&conn, &slug, &ct, &normalized_source_path)
        .map_err(|e| e.to_string())?;
    if let Some(refs) = &referenced_slugs {
        if !refs.is_empty() {
            let _ = wire_node_lib::pyramid::db::save_slug_references(&conn, &info.slug, refs);
        }
    }
    Ok(info)
}
```

The wizard's advanced toggle calls this IPC; the basic flow keeps using `pyramid_create_slug` with strict validation.

### 2.5.3 Match arm sites — add `Other(_)` arm

For each of the 6 verified exhaustive match sites, add an `Other(_)` arm with the indicated semantics:

#### `main.rs:3144-3207` (post-build seeding)
```rust
ContentType::Conversation | ContentType::Vine | ContentType::Question => {
    ("[]".to_string(), "[]".to_string())
}
ContentType::Other(_) => {
    // Custom content_types don't seed file-watcher state by default.
    ("[]".to_string(), "[]".to_string())
}
```

#### `main.rs:3848-3867` (ingest dispatch)
```rust
ContentType::Other(_) => {
    return Err("Custom content_type does not yet have an ingest handler".to_string());
}
```

#### `routes.rs:2391-2412` (HTTP /ingest dispatch)
```rust
ContentType::Other(_) => {
    return Err(anyhow::anyhow!("Custom content_type does not yet have an ingest handler"));
}
```

#### `vine.rs:569-586` (vine bunch dispatch)
```rust
ContentType::Other(_) => Err(anyhow!(
    "Vine bunches do not yet support custom content_type"
)),
```

#### `slug.rs:168-190` (`resolve_validated_source_paths`)
```rust
ContentType::Other(_) => {
    // Custom content_types: no source-path shape validation. The chain
    // implementation owns its own validation. We do still enforce the
    // sandbox checks above (canonical, allowed roots, sensitive paths).
}
```

#### `build_runner.rs:646-688` (`run_legacy_build`)
```rust
ContentType::Other(_) => {
    return Err(anyhow!(
        "Legacy build does not support custom content_type — assign a chain via pyramid_chain_assignments"
    ));
}
```

### 2.5.4 `matches!()` sites — add `Other(_)` to safe defaults

#### `main.rs:3227`
```rust
if matches!(ct, ContentType::Conversation | ContentType::Vine | ContentType::Other(_)) {
    return Ok::<(), String>(()); // skip backfill for non-filesystem content types
}
```

#### `main.rs:3240`
```rust
if matches!(content_type, ContentType::Conversation | ContentType::Vine | ContentType::Other(_)) {
    return Ok(()); // skip stale engine + watcher for non-filesystem content types
}
```

`slug.rs:134` and `slug.rs:204` are early returns for `Question` only — no change needed (`Other` falls through to the validation logic which then routes to the new `Other(_)` arm in `resolve_validated_source_paths`).

### 2.5.5 Drop the CHECK constraint on `pyramid_slugs`

**File:** `db.rs`, new function `migrate_slugs_drop_check` modeled on `migrate_slugs_check_question` at `:1087-1148`.

```rust
/// Drop the `pyramid_slugs.content_type` CHECK constraint to allow open content_types
/// (introduced by Phase 2.5 of chain-binding-v2.5). Idempotent: skips if no CHECK present.
fn migrate_slugs_drop_check(conn: &Connection) -> Result<()> {
    let table_sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='pyramid_slugs'",
            [],
            |row| row.get(0),
        )
        .ok();

    let needs_migration = match &table_sql {
        Some(sql) => sql.contains("CHECK(content_type"),
        None => false,
    };

    if !needs_migration {
        return Ok(());
    }

    tracing::info!("Dropping pyramid_slugs CHECK constraint to allow open content_types...");

    conn.execute_batch("PRAGMA foreign_keys=OFF;")?;

    let result = (|| -> Result<()> {
        let tx = conn.unchecked_transaction()?;

        // Create new table with the FULL accumulated column list (verified
        // against db.rs lines 54-63 base + 716/853/869/875/879/885/889/893/899/905/909/915 ALTERs).
        // No CHECK on content_type.
        tx.execute_batch(
            "
            CREATE TABLE pyramid_slugs_new (
                slug TEXT PRIMARY KEY,
                content_type TEXT NOT NULL,
                source_path TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                last_built_at TEXT,
                node_count INTEGER NOT NULL DEFAULT 0,
                max_depth INTEGER NOT NULL DEFAULT 0,
                archived_at TEXT DEFAULT NULL,
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                last_published_build_id TEXT DEFAULT NULL,
                pinned INTEGER NOT NULL DEFAULT 0,
                source_tunnel_url TEXT DEFAULT NULL,
                access_tier TEXT NOT NULL DEFAULT 'public',
                access_price INTEGER DEFAULT NULL,
                allowed_circles TEXT DEFAULT NULL,
                metadata_contribution_id TEXT DEFAULT NULL,
                absorption_mode TEXT NOT NULL DEFAULT 'open',
                absorption_chain_id TEXT DEFAULT NULL,
                cached_emergent_price INTEGER DEFAULT NULL
            );
            INSERT INTO pyramid_slugs_new (
                slug, content_type, source_path, created_at, last_built_at,
                node_count, max_depth, archived_at, updated_at, last_published_build_id,
                pinned, source_tunnel_url, access_tier, access_price, allowed_circles,
                metadata_contribution_id, absorption_mode, absorption_chain_id, cached_emergent_price
            )
            SELECT
                slug, content_type, source_path, created_at, last_built_at,
                node_count, max_depth, archived_at, updated_at, last_published_build_id,
                pinned, source_tunnel_url, access_tier, access_price, allowed_circles,
                metadata_contribution_id, absorption_mode, absorption_chain_id, cached_emergent_price
            FROM pyramid_slugs;
            DROP TABLE pyramid_slugs;
            ALTER TABLE pyramid_slugs_new RENAME TO pyramid_slugs;

            -- Re-create the 3 AFTER DELETE triggers (db.rs:487-505) which were
            -- dropped along with the old table.
            CREATE TRIGGER IF NOT EXISTS fk_cascade_faq_on_slug_delete
            AFTER DELETE ON pyramid_slugs
            FOR EACH ROW BEGIN
                DELETE FROM pyramid_faq_nodes WHERE slug = OLD.slug;
            END;

            CREATE TRIGGER IF NOT EXISTS fk_cascade_cost_on_slug_delete
            AFTER DELETE ON pyramid_slugs
            FOR EACH ROW BEGIN
                DELETE FROM pyramid_cost_log WHERE slug = OLD.slug;
            END;

            CREATE TRIGGER IF NOT EXISTS fk_cascade_usage_on_slug_delete
            AFTER DELETE ON pyramid_slugs
            FOR EACH ROW BEGIN
                DELETE FROM pyramid_usage_log WHERE slug = OLD.slug;
            END;
            ",
        )?;

        tx.commit()?;
        Ok(())
    })();

    conn.execute_batch("PRAGMA foreign_keys=ON;")?;

    match result {
        Ok(()) => {
            tracing::info!("pyramid_slugs CHECK constraint dropped successfully.");
            Ok(())
        }
        Err(e) => {
            tracing::error!("pyramid_slugs CHECK drop failed (FK re-enabled): {e}");
            Err(e)
        }
    }
}
```

**Call site:** in `init_pyramid_db`, AFTER the `updated_at` ALTER at `db.rs:852-855` (so the `updated_at` column exists when the migration's hardcoded column list references it). The cleanest insertion point is at the end of `init_pyramid_db`, just before `Ok(())` at `db.rs:857`. Add:

```rust
// Drop CHECK constraint on pyramid_slugs.content_type for Phase 2.5 open content_types.
// Must run AFTER all ALTER TABLE pyramid_slugs ADD COLUMN calls (lines 716, 853, 869,
// 875, 879, 885, 889, 893, 899, 905, 909, 915) so the new-table schema in this
// migration sees the full accumulated column set.
migrate_slugs_drop_check(conn)?;
```

**Why not earlier:** the `updated_at` column is ALTER-added at `db.rs:853` (well after the `migrate_online_push_columns` call at `:814`). If `migrate_slugs_drop_check` runs immediately after `migrate_online_push_columns`, the migration's `INSERT INTO pyramid_slugs_new (..., updated_at, ...) SELECT ..., updated_at, ...` fails because the column doesn't exist on the source table yet. Running at the end of `init_pyramid_db` guarantees all ALTERs have completed.

**Update `db.rs:54-63`** (the `CREATE TABLE IF NOT EXISTS pyramid_slugs`) to remove the CHECK clause:
```rust
CREATE TABLE IF NOT EXISTS pyramid_slugs (
    slug TEXT PRIMARY KEY,
    content_type TEXT NOT NULL,
    source_path TEXT NOT NULL DEFAULT '',
    ...
)
```
This way, fresh installs don't reintroduce the CHECK on first boot.

The two existing migration functions (`migrate_slugs_check_constraint`, `migrate_slugs_check_question`) become inert on fresh installs (their idempotency check `sql.contains("CHECK") && !sql.contains("vine")` returns false because there's no CHECK at all). They remain in the file as dead code for backwards compat with very old DBs that haven't migrated past the original CHECK shape.

### 2.5.6 Phase 2.5 done criteria

- [ ] `ContentType::Other(String)` variant exists.
- [ ] Manual `Serialize`/`Deserialize` produces bare-string wire format.
- [ ] `from_str` (strict) and `from_str_open` (open) both implemented.
- [ ] All 3 db.rs from_str sites use `from_str_open`.
- [ ] `pyramid_create_slug` keeps strict `from_str`.
- [ ] New `pyramid_create_slug_open` IPC for advanced custom content_types.
- [ ] All 6 exhaustive match sites have `Other(_)` arms.
- [ ] Both `matches!()` sites in main.rs include `Other(_)`.
- [ ] `migrate_slugs_drop_check` runs once and drops the CHECK constraint cleanly.
- [ ] All 3 triggers re-created.
- [ ] `db.rs:54` `CREATE TABLE IF NOT EXISTS` updated to omit CHECK.
- [ ] `cargo check` clean.
- [ ] `cargo test` passes existing suite.
- [ ] Manual test: create a slug with content_type `"transcript.test"` via `pyramid_create_slug_open`, confirm it persists and reads back as `Other("transcript.test")`.

---

## Section 7 — Phase 2.6: Stale propagation skip for chronological chain

### 2.6.1 The single check

**File:** `staleness.rs:126-223` (`propagate_staleness`)

Insert at the very top of the function (before line 132):

```rust
pub fn propagate_staleness(
    conn: &Connection,
    slug: &str,
    deltas: &[SourceDelta],
    threshold: f64,
) -> Result<StalenessReport> {
    // Phase 2.6: skip stale propagation for chains that don't produce KEEP-link evidence.
    // Today the only such chain is the Rust intrinsic conversation-legacy-chronological,
    // which is reachable via per-slug assignment or per-content-type override.
    if let Ok(Some((chain_id, _))) = super::chain_registry::get_assignment(conn, slug) {
        if chain_id == "conversation-legacy-chronological" {
            warn!(
                slug,
                chain_id = %chain_id,
                "stale propagation skipped: chain produces no KEEP-link evidence"
            );
            return Ok(StalenessReport {
                affected_questions: vec![],
                max_depth_reached: 0,
                staleness_scores: Default::default(),
            });
        }
    }
    // Also check the per-content-type default for slugs without explicit assignment.
    // Less common path, but covers operators who set the default rather than per-slug.
    if let Ok(Some(slug_info)) = super::db::get_slug(conn, slug) {
        if let Ok(Some(default_chain)) = super::chain_registry::get_chain_default(conn, slug_info.content_type.as_str()) {
            if default_chain == "conversation-legacy-chronological" {
                warn!(
                    slug,
                    chain_id = %default_chain,
                    "stale propagation skipped: per-content-type default chain produces no KEEP-link evidence"
                );
                return Ok(StalenessReport {
                    affected_questions: vec![],
                    max_depth_reached: 0,
                    staleness_scores: Default::default(),
                });
            }
        }
    }

    // (existing logic from line 132 follows)
    let mut all_scores: HashMap<String, f64> = HashMap::new();
    // ...
}
```

### 2.6.2 Second propagation path: `delta.rs::propagate_staleness_parent_chain`

Round 4 audit found a second propagation path: `delta.rs::propagate_staleness_parent_chain` (called from delta.rs:387/851/944) does NOT route through `staleness::propagate_staleness`. Add the same check at the top of that function:

```rust
pub fn propagate_staleness_parent_chain(
    conn: &Connection,
    slug: &str,
    /* other args */
) -> Result<...> {
    if should_skip_chronological_staleness(conn, slug) {
        return Ok(/* empty result */);
    }
    // ... existing logic ...
}
```

Where `should_skip_chronological_staleness` is a small helper in `chain_registry.rs` shared between `staleness.rs::propagate_staleness` and `delta.rs::propagate_staleness_parent_chain`:

```rust
// chain_registry.rs
pub fn should_skip_chronological_staleness(conn: &Connection, slug: &str) -> bool {
    // Check per-slug assignment
    if let Ok(Some((chain_id, _))) = get_assignment(conn, slug) {
        if chain_id == "conversation-legacy-chronological" {
            return true;
        }
    }
    // Check per-content-type override via the resolver
    if let Ok(Some(slug_info)) = super::db::get_slug(conn, slug) {
        if let Ok(resolved) = resolve_chain_for_slug(conn, slug, slug_info.content_type.as_str()) {
            if resolved == "conversation-legacy-chronological" {
                return true;
            }
        }
    }
    false
}
```

Both `staleness.rs::propagate_staleness` and `delta.rs::propagate_staleness_parent_chain` call this helper. This avoids hand-rolling the check twice and keeps the chronological-skip logic in one place.

`staleness_bridge.rs::run_staleness_check` calls `propagate_staleness` internally, so it inherits the check; no separate work needed there.

The dispatch_* functions in `stale_helpers.rs` and `stale_helpers_upper.rs` are file-watcher pipeline sub-functions that don't apply to chronological pyramids (which have no file watcher). No check needed.

### 2.6.3 Phase 2.6 done criteria

- [ ] `propagate_staleness` skips with logged warning when slug is bound to `conversation-legacy-chronological`.
- [ ] Test: bind a slug to chronological chain, trigger staleness, observe the warning + empty report.

---

## Section 8 — Phase 3: Temporal first-class data

### 3.0 L0 Topic parse sites — verified (no spike needed)

3 sites, all using the same pattern `serde_json::from_value(t.clone()).ok()`:
- `chain_dispatch.rs:360`
- `build.rs:534`
- `delta.rs:694`

Phase 3.3 introduces a helper that all 3 call.

### 3.1 Add temporal columns to `pyramid_chunks`

**File:** `db.rs::init_pyramid_db`, new helper called from the existing init flow.

```rust
fn add_chunks_temporal_columns_if_missing(conn: &Connection) -> Result<()> {
    let columns: Vec<String> = conn
        .prepare("PRAGMA table_info(pyramid_chunks)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;

    if !columns.contains(&"first_ts".to_string()) {
        let _ = conn.execute("ALTER TABLE pyramid_chunks ADD COLUMN first_ts TEXT DEFAULT NULL", []);
    }
    if !columns.contains(&"last_ts".to_string()) {
        let _ = conn.execute("ALTER TABLE pyramid_chunks ADD COLUMN last_ts TEXT DEFAULT NULL", []);
    }
    if !columns.contains(&"content_hash".to_string()) {
        let _ = conn.execute("ALTER TABLE pyramid_chunks ADD COLUMN content_hash TEXT DEFAULT NULL", []);
    }
    Ok(())
}
```

Called from `init_pyramid_db` near the existing `let _ = conn.execute("ALTER TABLE pyramid_chunks ...")` patterns. PRAGMA pre-check is needed because `ALTER TABLE ADD COLUMN` doesn't support `IF NOT EXISTS`. The `let _ =` swallow on the ADD COLUMN handles edge cases where the column was added via a different path.

### 3.2 Make `Topic.speaker` and `Topic.at` first-class

**File:** `types.rs:89-108`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Topic {
    pub name: String,
    #[serde(default)]
    pub current: String,
    #[serde(default)]
    pub entities: Vec<String>,
    #[serde(default)]
    pub corrections: Vec<Correction>,
    #[serde(default)]
    pub decisions: Vec<Decision>,

    // ── Phase 3.2: first-class temporal anchors for sequential sources ──
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,

    // ── Pass-through: everything else the LLM produces ──
    #[serde(flatten, default)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}
```

**Important:** v2.4 proposed a custom `deserialize_with` that REJECTED `extra.speaker` / `extra.at`. v2.5 drops this — existing rows in `pyramid_nodes.topics` may have these keys in `extra` from old LLM output, and rejecting would crash `get_node`/`list_nodes`. Instead:

- The serde deserialize moves `speaker` and `at` from JSON into the typed fields if they're at the top level.
- If the JSON puts them inside extra (`{"name": "...", "extra": {"speaker": "..."}}` or via the flatten path with no speaker/at at top level), the serialized output WILL include them in `extra`. New code reads `topic.speaker` (typed); old data round-trips through `extra`.
- One-time migration helper (optional): a CLI script that walks `pyramid_nodes.topics`, parses each, moves speaker/at from extra to typed if present, re-saves. Ship as a `pyramid_migrate_topic_temporal_cmd` IPC for the user to run once.

### 3.3 Generic `required_topic_fields` validation

#### 3.3.1 New ChainStep field

**File:** `chain_engine.rs::ChainStep` (around line 122)

Add to the struct:
```rust
#[serde(default)]
pub required_topic_fields: Option<Vec<String>>,
```

And to the `Default` impl in `chain_engine.rs:265-325`:
```rust
required_topic_fields: None,
```

`#[serde(default)]` ensures the field is optional in YAML and defaults to None.

#### 3.3.2 Helper function

**File:** new module `pyramid/topic_validation.rs` (or inline in `chain_dispatch.rs`)

```rust
use serde_json::Value;
use super::types::Topic;

pub fn parse_topics_with_required_fields(
    value: Option<&Value>,
    required: Option<&[String]>,
) -> Vec<Topic> {
    let arr = match value.and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return Vec::new(),
    };

    arr.iter()
        .filter_map(|t| {
            let topic: Topic = serde_json::from_value(t.clone()).ok()?;
            if let Some(required_fields) = required {
                for field in required_fields {
                    let value = match field.as_str() {
                        "speaker" => topic.speaker.as_deref(),
                        "at" => topic.at.as_deref(),
                        other => topic.extra.get(other).and_then(|v| v.as_str()),
                    };
                    if value.map(|s| s.is_empty()).unwrap_or(true) {
                        return None;
                    }
                }
            }
            Some(topic)
        })
        .collect()
}
```

#### 3.3.3 Update the 3 parse sites

**`chain_dispatch.rs:360-368`** — replace the inline parsing with:
```rust
let required = step.required_topic_fields.as_deref();
let topics: Vec<Topic> = parse_topics_with_required_fields(output.get("topics"), required);
```

`step` is the `ChainStep` available at this site (verify in implementation; pass through if needed).

**`build.rs:534-542`** — same replacement. The legacy build path (build_conversation, build_code, build_docs) doesn't have a ChainStep in scope; pass `required: None` (no enforcement) for now. Phase 3 doesn't make build_conversation enforce required fields — that's a Phase 3.x follow-up.

**`delta.rs:694`** — same. Delta processing doesn't have required field metadata; pass `None`.

#### 3.3.4 Chain YAML usage

Operators add `required_topic_fields:` to their extract step:
```yaml
- name: source_extract
  primitive: extract
  instruction: $prompts/question/source_extract.md
  for_each: $chunks
  required_topic_fields:
    - speaker
    - at
```

After Phase 3.3 lands, only chain steps that opt in will enforce. The default remains "no enforcement, silent skip on parse failure" matching today's behavior.

### 3.4 Re-ingestion idempotency by `content_hash` invalidation

**File:** `ingest.rs` (in the chunk-write path) and `db.rs::clear_chunks`

#### 3.4.1 Compute content_hash on ingest

In each `ingest_*` function (`ingest_code`, `ingest_docs`, `ingest_conversation`), when persisting a chunk, compute its SHA-256 (truncated to 16 hex chars) and pass it to a new `db::insert_chunk_with_hash` function:

```rust
let hash = format!("{:x}", sha2::Sha256::digest(content.as_bytes()))[..16].to_string();
db::insert_chunk_with_hash(conn, slug, batch_id, chunk_index, content, line_count, char_count, &hash)?;
```

`db::insert_chunk_with_hash` is a new function that mirrors the existing chunk insert but populates `content_hash`.

#### 3.4.2 Hash-mismatch invalidation on re-ingest

Replace the existing `clear_chunks` flow in `routes.rs:2386-2389` and `main.rs` (anywhere `clear_chunks` is called before re-ingest) with a new function `db::reingest_chunks_with_hash_check`:

```rust
pub fn reingest_chunks_with_hash_check(
    conn: &Connection,
    slug: &str,
    new_chunks: &[(usize, String, String)], // (chunk_index, content, hash)
) -> Result<()> {
    // For each new chunk, compare hash to old hash at the same chunk_index.
    // If mismatch (or old hash is NULL), invalidate pipeline_steps for that index.
    let mut stmt = conn.prepare(
        "SELECT chunk_index, content_hash FROM pyramid_chunks WHERE slug = ?1 ORDER BY chunk_index"
    )?;
    let old_chunks: Vec<(i64, Option<String>)> = stmt
        .query_map(rusqlite::params![slug], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;

    for (idx, _content, new_hash) in new_chunks {
        let old_hash = old_chunks.iter().find(|(i, _)| *i == *idx as i64).and_then(|(_, h)| h.clone());
        if old_hash.as_deref() != Some(new_hash.as_str()) {
            // Hash mismatch (or old NULL): invalidate pipeline_steps for this chunk_index
            conn.execute(
                "DELETE FROM pyramid_pipeline_steps WHERE slug = ?1 AND chunk_index = ?2",
                rusqlite::params![slug, *idx as i64],
            )?;
        }
    }

    // Now clear and re-insert chunks
    db::clear_chunks(conn, slug)?;
    // ... insert new chunks with hashes ...
    Ok(())
}
```

**First post-Phase-3.1 re-ingest:** every existing chunk has `content_hash = NULL`. The hash comparison `old_hash.as_deref() != Some(new_hash)` evaluates to `None != Some(...)` = true → all pipeline_steps get invalidated. **Acknowledge:** the user's first re-ingest after Phase 3.1 ships will trigger a full rebuild for affected slugs. Document this in the commit message.

### 3.5 Phase 3 done criteria

- [ ] `pyramid_chunks` has `first_ts`, `last_ts`, `content_hash` columns.
- [ ] `Topic` has `Option<String>` `speaker` and `at` fields. Existing rows deserialize without error.
- [ ] `ChainStep` has `required_topic_fields: Option<Vec<String>>` field.
- [ ] `parse_topics_with_required_fields` helper exists.
- [ ] All 3 parse sites use the helper.
- [ ] Re-ingest hash-mismatch invalidation works.
- [ ] First post-upgrade re-ingest documented as a one-time mass rebuild.
- [ ] Test: chain YAML with `required_topic_fields: [speaker, at]` rejects a topic with empty speaker.

---

## Section 9 — Phase 4: Bootstrap fixes

### 4.1 Add `include_dir` dependency

**File:** `src-tauri/Cargo.toml`

Add to `[dependencies]`:
```toml
include_dir = "0.7"
```

### 4.2 Extend `src-tauri/build.rs` with chains rerun trigger

**File:** `src-tauri/build.rs:6` (after `cargo:rerun-if-changed=assets/`)

Add:
```rust
println!("cargo:rerun-if-changed=../chains");
```

This causes `cargo build` to rebuild the lib whenever any file in `chains/` changes, ensuring `include_dir!` picks up new prompts.

### 4.3 Replace Tier 2 hardcoded includes with `include_dir!`

**File:** `chain_loader.rs`

Add at the top:
```rust
use include_dir::{include_dir, Dir};
static CHAINS_DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/../chains");
```

Replace lines 238-293 (the Tier 2 hardcoded `if !path.exists() { write }` block) with:

```rust
// ── Tier 2: Embedded defaults via include_dir! (bootstrap only) ──────
// Bundles the entire chains/ tree at compile time. Any new file added
// to chains/ is automatically picked up on next cargo build.

/// Path filter — exclude archived/draft/dev-only directories from the bundle.
/// Anything matching these path-segments is skipped at bundling time.
fn should_bundle(rel_path: &Path) -> bool {
    let path_str = rel_path.to_string_lossy();
    !(path_str.contains("_archived/")
        || path_str.contains("/_archived")
        || path_str.starts_with("vocabulary")
        || path_str.contains("/vocabulary/")
        || path_str.ends_with("CHAIN-DEVELOPER-GUIDE.md"))
}

fn write_bundled_recursive(dir: &Dir, dst_root: &Path) -> Result<()> {
    for entry in dir.entries() {
        match entry {
            include_dir::DirEntry::Dir(subdir) => {
                if !should_bundle(subdir.path()) { continue; }
                let dst = dst_root.join(subdir.path());
                if !dst.exists() {
                    std::fs::create_dir_all(&dst)
                        .with_context(|| format!("failed to create dir: {}", dst.display()))?;
                }
                write_bundled_recursive(subdir, dst_root)?;
            }
            include_dir::DirEntry::File(file) => {
                if !should_bundle(file.path()) { continue; }
                let dst = dst_root.join(file.path());
                if !dst.exists() {
                    if let Some(parent) = dst.parent() {
                        std::fs::create_dir_all(parent).ok();
                    }
                    std::fs::write(&dst, file.contents())
                        .with_context(|| format!("failed to write bundled file: {}", dst.display()))?;
                    tracing::info!(path = %dst.display(), "bootstrapped bundled file");
                }
            }
        }
    }
    Ok(())
}

write_bundled_recursive(&CHAINS_DIR, chains_dir)?;
```

**Note on bundle size:** the binary inflates by roughly the size of `chains/prompts/` minus the filtered junk (archived files, vocabulary, dev guides). At ~85 prompt files of 1-15 KB each that's ~500 KB-1 MB of additional binary size. Acceptable.

### 4.4 Delete placeholder constants

Delete lines 322-371 (`DEFAULT_CONVERSATION_CHAIN`, `DEFAULT_CODE_CHAIN`, `DEFAULT_DOCUMENT_CHAIN` const strings). They're replaced by the `include_dir!` content.

Also delete the per-file `include_str!` lines at 243-244 (question.yaml, extract-only.yaml) and at 259, 267-271, 282-285 (planner-system, question prompts, shared prompts) — they're all subsumed by `write_bundled_recursive`.

### 4.5 Tier 1 keeps unconditional overwrite

Tier 1 (`copy_dir_recursive` at :298-318) **stays unchanged**. Dev users want source-tree edits to hot-reload to the runtime data dir on every app restart. The previous v2.4 plan's "skip overwrite" change was wrong for Tier 1 — it would break the dev workflow.

### 4.6 New `repair_chains_cmd` IPC

**File:** `main.rs`, near the other pyramid IPCs.

```rust
#[tauri::command]
async fn pyramid_repair_chains(
    state: tauri::State<'_, SharedState>,
) -> Result<(), String> {
    let chains_dir = state.pyramid.chains_dir.clone();
    // Force re-bootstrap by deleting the directory contents (carefully) and re-running ensure_default_chains.
    // Or simpler: call write_bundled_recursive directly with overwrite=true.
    wire_node_lib::pyramid::chain_loader::force_resync_chains(&chains_dir)
        .map_err(|e| e.to_string())
}
```

Add a sibling `force_resync_chains` function in `chain_loader.rs` that does the same `write_bundled_recursive` walk but unconditionally overwrites.

Settings panel button (frontend) wires to this IPC.

### 4.7 Phase 4 done criteria

- [ ] `include_dir = "0.7"` added to Cargo.toml.
- [ ] `src-tauri/build.rs` has `cargo:rerun-if-changed=../chains`.
- [ ] `CHAINS_DIR` static + `write_bundled_recursive` implemented.
- [ ] Tier 2 placeholder constants and per-file `include_str!` lines deleted.
- [ ] Tier 1 still overwrites (no change).
- [ ] `pyramid_repair_chains` IPC + `force_resync_chains` function exist.
- [ ] Frontend settings panel button calls the IPC.
- [ ] `cargo build` succeeds, binary contains all 92 prompt files via `include_dir!`.

---

## Section 10 — Phase 5: Documentation

```
docs/chain-development/
├── README.md
├── 01-architecture.md         — content_type → resolver → chain_id → executor
├── 02-chain-yaml-reference.md — schema + new fields (required_topic_fields)
├── 03-prompt-anatomy.md
├── 04-temporal-conventions.md — chunks columns, Topic.speaker/at, required_topic_fields
├── 05-pillar-37.md            — prompt discipline, with build.rs Pillar 37 sweep examples
├── 06-forking-a-chain.md      — recipe with conversation-legacy-chronological as worked example
├── 07-adding-a-content-type.md — using ContentType::Other
├── 08-testing-a-chain.md
└── 09-troubleshooting.md
```

---

## Sequencing

```
Phase 0   (P0 fixes — ships first, 5 commits)
   │
Phase 1F-pre  (frontend constants — parallel to Phase 1 backend)
Phase 1   (audit only, no code changes)
   │
Phase 2.1  (chain_defaults table)
Phase 2.2  (resolve_chain_for_slug + doc-comment update)
Phase 2.3  (IPC commands)
   │
Phase 2.5  (ContentType::Other variant + match arms + CHECK drop migration)
Phase 2.6  (stale propagation skip)
   │
Phase 2.4  (dispatch fix in build_runner.rs at 4 sites — uses 2.2 + 2.5)
Phase 1F-post  (wizard IPC wiring + settings panel)
   │
Phase 3.1  (chunks temporal columns)
Phase 3.2  (Topic typed fields)
Phase 3.3  (required_topic_fields ChainStep field + helper + 3 parse sites)
Phase 3.4  (re-ingest hash invalidation)
   │
Phase 4   (bootstrap include_dir + repair IPC)
   │
Phase 5   (docs)
   │
──── recursive-vine-v2 begins (sibling plan, same session) ────
```

## Risks

1. **`build_conversation` audit reveals subtle breakage.** Mitigation: vine.rs:571 already exercises it; if vines work, build_conversation works.
2. **Pillar 37 sweep changes vine bunch outputs.** Mitigation: rebuild a vine bunch as a smoke test in Phase 0 done criteria.
3. **`migrate_slugs_drop_check` misses an ALTER-added column.** Mitigation: the hardcoded list mirrors verified columns from db.rs:715-915. If the codebase grows new columns later, the migration will need updating — but it's a one-time runtime, idempotent, and the column count is fixed at the time the migration ships.
4. **First post-upgrade re-ingest mass-invalidates pipeline steps** (Phase 3.4). Acknowledged in commit message; user expects one-time rebuild.

## Done criteria (overall)

- [ ] Phase 0: 5 P0 fixes shipped; vine rebuild smoke test passes.
- [ ] Phase 1: audit complete; no code changes.
- [ ] Phase 1F: frontend centralized; fallback objects everywhere.
- [ ] Phase 2: chain_defaults table + IPC + resolver + 4-site dispatch fix; conversation pyramid bound to chronological chain executes `build_conversation`.
- [ ] Phase 2.5: ContentType::Other variant + 6 match arms + 2 matches!() + 3 from_str sites + CHECK drop migration + 3 trigger recreation.
- [ ] Phase 2.6: propagate_staleness skips chronological chain.
- [ ] Phase 3: chunks temporal columns, Topic typed fields, required_topic_fields enforcement at 3 parse sites, re-ingest hash invalidation.
- [ ] Phase 4: include_dir bundling, build.rs extended, repair IPC.
- [ ] Phase 5: doc tree.
- [ ] Manual smoke test: build any conversation .jsonl with chronological binding; verify L0 has populated speaker/at fields and the apex headline reads chronologically.
- [ ] `cargo build` clean. Existing `cargo test` suite passes.
