# Plan v2: Recover the Chronological Pipeline + Make Chain Binding Real

> **Status:** rough draft, unaudited. Supersedes `chain-binding-and-triple-pass.md` (invalidated by 4-agent audit on 2026-04-07; see `chain-binding-and-triple-pass.audit.md`).
>
> Built directly from the audit findings. Each phase is anchored to specific code locations the audit verified, not to a mental model.

## Premise

The audit overturned three assumptions the v1 plan was built on:

1. **A working forward/reverse/combine conversation pipeline already exists** at `src-tauri/src/pyramid/build.rs:684+` (`build_conversation`). It is unreachable in default config because `build_runner.rs:237` routes Conversation to `run_decomposed_build` and never reaches `run_legacy_build`.
2. **The plan's target DSL (`chains/questions/*.yaml` v3) is not in production.** Production runs `chains/defaults/question.yaml` (legacy `ChainStep` DSL) via `chain_executor.rs`. The v3 DSL is consumed only by `parity.rs` for validation.
3. **`ContentType` is a closed Rust enum dispatched by exhaustive `match` in 5+ files** (`main.rs`, `build_runner.rs`, `vine.rs`, IPC, wizard UI). "Config-driven, no recompile" is impossible without moving content_type to a free string + chain_id-based dispatch.

The new plan starts from those facts.

## Goals (revised, smaller, real)

1. **Stop forward-pass crashes and dead-config drift.** Ship the P0 bugs the audit found independently of any architectural work.
2. **Make the existing chronological conversation pipeline reachable.** `build_conversation` is already in the tree. Route to it via chain assignment.
3. **Make chain selection a per-content-type operator decision** without recompiling Rust. Smallest viable change.
4. **Persist temporal anchors as first-class data** so chronological reasoning doesn't depend on the LLM remembering JSON fields it serializes through `#[serde(flatten)] extra`.
5. **Fix the auto-update story** so end users actually receive new chains and prompts.
6. **Make the system honest about what it can and can't ingest.** Today it claims "domain-neutral conversation" but only handles Claude Code JSONL.

## Non-goals

- Rewriting the v3 question DSL or `parity.rs`. Either kill DSL #2 in a follow-up or leave it alone.
- Adding new transcript parsers beyond Claude Code JSONL in this plan. Phase 3 calls out the parser registry as a placeholder; the actual parsers come later.
- Meta-pyramid / cross-session grounding. Out of scope, as before.
- Changing the synthesis prompts or the answer.md abstain rule that were already shipped (commit `b3e42dd`). Those landed correctly and are working.

---

## Phase 0 — P0 fixes that ship independently

These are bugs the audit found that exist regardless of any binding/dispatch work. Ship them first; they have no design risk.

### 0.1 Fix the UTF-8 panic in `update_accumulators`

**File:** `src-tauri/src/pyramid/chain_executor.rs:6960-6964`

Today:
```rust
let truncated = if new_val.len() > max_chars {
    new_val[..max_chars].to_string()
} else { new_val };
```

`new_val.len()` is byte length. The slice panics on any non-ASCII character at the wrong byte boundary. Em-dashes, smart quotes, accents, CJK, emoji all crash the forward pass.

**Fix:** char-aware truncation. The same file already has `truncate_for_webbing` at `:1553`. Use that, or:

```rust
let truncated = new_val
    .char_indices()
    .nth(max_chars)
    .map(|(i, _)| new_val[..i].to_string())
    .unwrap_or(new_val);
```

Ship as a standalone commit. Add a test with a string containing an em-dash at the truncation boundary.

### 0.2 Resolve dead `instruction_map: content_type:` config

**Files:** `src-tauri/src/pyramid/chain_executor.rs:1034-1070`, `chains/defaults/question.yaml:27-28`

The chain YAML declares `instruction_map: content_type:conversation: $prompts/conversation/source_extract_v2.md`, but the matcher only handles keys with `type:`, `language:`, `extension:`, `type:frontend` prefixes. The `content_type:` key is never matched. Conversation builds silently use the generic prompt.

**Two options, pick one:**
- **(A)** Implement the missing `content_type:` arm in `instruction_map_prompt()`. Cheap. Brings the existing declaration to life.
- **(B)** Delete the dead key from `chains/defaults/question.yaml` so the next reader doesn't trust it.

**Recommendation:** (A). It's a few lines and unblocks per-content-type prompt overrides immediately, before any of Phase 1 lands. Some of the per-content-type binding work the rest of the plan describes could ride this primitive instead of inventing a new layer.

### 0.3 Decide the fate of `generate_extraction_schema()`

**File:** `src-tauri/src/pyramid/extraction_schema.rs:40`

Defined, exported, fully implemented with tests. Never called from anywhere in `src-tauri/src/`. The L0 schema in production comes from the static `chains/prompts/question/source_extract.md`.

**Two options:**
- **(A)** Wire it into the build pipeline at the L0 schema-materialization site (wherever that actually is — needs investigation; the v1 plan got this wrong).
- **(B)** Delete it.

**Recommendation:** (B) for Phase 0. Deleting dead code is safe. If we want LLM-generated L0 schemas later we can revive it from git history. Carrying dead code creates exactly the kind of confusion the v1 plan fell into.

### 0.4 Fix or document the chunk-boundary heuristic

**File:** `src-tauri/src/pyramid/ingest.rs:244-282`

`chunk_transcript` splits on any line beginning `--- `. Markdown horizontal rules trigger false speaker boundaries. Code-block separators do too.

**Fix:** require `--- (PLAYFUL|CONDUCTOR)` (or whatever the future label set is) prefix. Or store boundary metadata structurally instead of parsing it back out of text. The structural fix lands in Phase 3; Phase 0 just tightens the regex.

### 0.5 Phase 0 done criteria

- [ ] `update_accumulators` no longer panics on multi-byte UTF-8 input. Test with em-dash, accent, CJK, emoji.
- [ ] `instruction_map: content_type:` either works or doesn't exist in the YAML.
- [ ] `generate_extraction_schema` either runs or is deleted.
- [ ] `chunk_transcript` does not false-trigger on markdown `---` rules.

Phase 0 is independent of Phases 1-5. Ship as 4 small commits, not one.

---

## Phase 1 — Recover the existing chronological pipeline by fixing dispatch

Instead of building a new triple-pass pipeline, make `build_conversation` reachable.

### 1.1 The dispatch problem

**File:** `src-tauri/src/pyramid/build_runner.rs:237`

Today, `run_build_from` checks `content_type == ContentType::Conversation` and routes directly to `run_decomposed_build`, which calls `chain_registry::default_chain_id` (always returns `"question-pipeline"`). It never reaches `run_legacy_build`, which is the only caller of `build_conversation`.

`build_conversation` (`src-tauri/src/pyramid/build.rs:684+`) is a fully implemented forward-pass + reverse-pass + combine-into-L0 + L1 thread pairing + L2 thread synthesis pipeline. It is what the v1 plan called the "triple-pass chronological variant." It already exists.

### 1.2 The fix

Make the dispatch consult `pyramid_chain_assignments` before defaulting:

```rust
// inside run_build_from, for ContentType::Conversation:
let assignment = chain_registry::get_assignment(&conn, slug)?;
let chain_id = assignment.unwrap_or_else(|| chain_registry::default_for(content_type));

if chain_id == "conversation-legacy-chronological" {
    return run_legacy_build(state, slug, /* ... */).await;
}
return run_decomposed_build(state, slug, /* ... */).await;
```

`default_for(content_type)` is a new function (not `default_chain_id`'s wildcard). It reads the registry (Phase 2) or falls back to `"question-pipeline"`.

### 1.3 Verify `build_conversation` actually does what we want

**Critical:** before relying on it, audit `build_conversation` carefully. The v1 plan died because of unverified assumptions.

- Read `build.rs:684+` end to end.
- Confirm it produces L0/L1/L2 nodes with the same shape downstream consumers expect.
- Confirm it handles cancellation correctly.
- Confirm it persists step output in a way that survives crash + resume.
- Test it on the same `.jsonl` we used for Runs 1-4.
- Document any surprises in this doc before proceeding.

This audit step is half of Phase 1's value. The other half is the dispatch fix.

### 1.4 What we lose by routing to `build_conversation`

`build_conversation` is the legacy path. Things `run_decomposed_build` does that it does not:
- The question decomposition tree (decompose.md → sub-questions). `build_conversation` doesn't take a question; it produces a topic-clustered pyramid.
- The verdict-based synthesis with the abstain rule we just shipped in `answer.md`.
- The web edges, glossary, FAQ, and other artifacts the question pipeline produces.

This means a chronological conversation pyramid built with `build_conversation` would be **structurally different** from a regular conversation pyramid. Not better or worse — different. The user has to choose at create time which they want.

**Option for Phase 1.5:** instead of routing to legacy, **port the forward/reverse/combine ideas from `build.rs:684+` into the `chains/defaults/question.yaml` chain** as new steps that the chain executor already knows how to run (using the existing `save_as: step_only` and `zip_steps` primitives). This keeps the question pipeline's benefits (decomposition, verdicts, abstain, web edges) and adds the chronological L0 pass on top.

This is the right answer if it's tractable. The audit confirmed both `step_only` and `zip_steps` already work in the legacy chain executor — and `chains/defaults/question.yaml` is the production chain. Adding three new steps to it (forward pass, reverse pass, combine) might be ~50-100 lines of YAML + 0 lines of Rust if everything we need is already in the executor.

**Recommended:** start by reading `build.rs:684+` carefully (1.3 audit), then decide between (a) routing to the legacy build verbatim, or (b) porting its forward/reverse/combine shape into question.yaml as additional steps. The audit didn't dig into whether (b) is fully expressible in the legacy DSL; that's a Phase 1 spike.

### 1.5 Phase 1 done criteria

- [ ] `build_conversation` audited end-to-end and any surprises documented.
- [ ] Decision made: route to legacy verbatim, or port forward/reverse/combine into question.yaml.
- [ ] If routing: dispatch fix lands, a test pyramid built with `chain_id = "conversation-legacy-chronological"` actually executes `build_conversation` and produces nodes.
- [ ] If porting: question.yaml has new forward/reverse/combine steps using existing `save_as: step_only` and `zip_steps`. Test pyramid produces L0 nodes with combined context.
- [ ] Existing conversation pyramids (default chain) continue to build successfully — non-regression.

---

## Phase 2 — Make chain binding real (per-content-type, schema-supported)

The audit found that `pyramid_chain_assignments` has no `content_type` column, `default_chain_id` ignores its content_type parameter, and there is no per-content-type default mechanism. Phase 2 fixes that.

### 2.1 Schema change

Add a `content_type` column to `pyramid_chain_assignments`, OR add a new table:

```sql
CREATE TABLE IF NOT EXISTS pyramid_chain_defaults (
    content_type TEXT PRIMARY KEY,
    chain_id TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

Either way, the resolver becomes:

```rust
pub fn resolve_chain_for_slug(conn: &Connection, slug: &str, content_type: &str) -> Result<String> {
    // 1. per-slug override
    if let Some(override_chain) = get_assignment(conn, slug)? {
        return Ok(override_chain);
    }
    // 2. per-content-type default
    if let Some(default_chain) = get_default_for_content_type(conn, content_type)? {
        return Ok(default_chain);
    }
    // 3. hardcoded fallback
    Ok("question-pipeline".to_string())
}
```

### 2.2 Replace `chain_registry::default_chain_id`

**File:** `src-tauri/src/pyramid/chain_registry.rs:97`

```rust
pub fn default_chain_id(_content_type: &str) -> &'static str {
    "question-pipeline"
}
```

becomes a real function that consults the new table. All callers updated.

### 2.3 Operator surface

Surface a way to set per-content-type defaults from the desktop UI or via IPC. Two options:

- **(A)** Settings panel: dropdown per content_type → list of available chain_ids → save.
- **(B)** YAML config file at `chains/defaults.yaml` that the build_runner reads at startup and syncs to the table.

**Recommendation:** (B) for Phase 2 (reproducible across installs, version-controllable, no UI work). (A) as a follow-up.

### 2.4 What this does NOT fix

This phase does not solve the closed `ContentType` enum problem. Adding `conversation-chronological` as a new variant still requires the enum + DB CHECK migration + 5+ Rust match arms + 8 frontend files. That's Phase 3.

This phase only makes it possible to swap which chain a content_type uses by editing one row in `pyramid_chain_defaults` (or one line in `chains/defaults.yaml`). The chain itself is still picked from the existing chain set.

### 2.5 Phase 2 done criteria

- [ ] `pyramid_chain_defaults` table exists (or `pyramid_chain_assignments` has a `content_type` column).
- [ ] `resolve_chain_for_slug` consults per-slug → per-content-type → hardcoded fallback in that order.
- [ ] Setting `pyramid_chain_defaults[conversation] = 'conversation-legacy-chronological'` causes new conversation builds to route to the legacy build (assuming Phase 1 landed routing).
- [ ] Existing slugs with no override continue to use the question-pipeline by default.
- [ ] Migration is reversible (can drop the table, system falls back to today's behavior).

---

## Phase 3 — Persist temporal anchors as first-class data

The audit found that even after the chronological pipeline runs, temporal data lives in `#[serde(flatten)] extra` on `Topic` — Rust can't sort by it, can't filter by it, can't validate it. And `pyramid_chunks` has no `first_ts`/`last_ts` columns at all. And re-ingestion shuffles `chunk_index` so resume keys hit the wrong content.

### 3.1 Add temporal columns to `pyramid_chunks`

**File:** `src-tauri/src/pyramid/db.rs:99-108`

```sql
ALTER TABLE pyramid_chunks ADD COLUMN first_ts TEXT DEFAULT NULL;
ALTER TABLE pyramid_chunks ADD COLUMN last_ts TEXT DEFAULT NULL;
ALTER TABLE pyramid_chunks ADD COLUMN content_hash TEXT DEFAULT NULL;
```

`first_ts` / `last_ts`: ISO timestamps if the source provides them. NULL otherwise.
`content_hash`: SHA-256 (or shorter) of chunk content. This is what resume keys should use, not `chunk_index`.

Populate during ingest. Backfill is N/A for new pyramids; existing ones keep working with NULL temporal data and existing chunk_index resume keys.

### 3.2 Make `Topic.speaker` and `Topic.at` first-class

**File:** `src-tauri/src/pyramid/types.rs:90-108`

Add named fields to the `Topic` struct alongside `name`, `current`, `entities`, etc. Keep `#[serde(flatten)] extra` for forward compatibility, but the temporal fields are no longer extras — they have getters, they're queryable, they're sortable.

Add validation: a `Topic` extracted from a sequential-source chunk must have `speaker` and `at` populated. The L0 extractor's JSON-parse step rejects topics that omit them and triggers the existing parse-retry path.

### 3.3 Generic `required_fields` validation in the L0 extract primitive

**File:** wherever the L0 source_extract output is parsed (the v1 plan got this wrong; needs investigation in Phase 3 itself)

Don't hardcode "speaker" and "at" in Rust — that's the composability mistake. Instead:

```yaml
- name: source_extract
  primitive: extract
  instruction: $prompts/question/source_extract.md
  for_each: $chunks
  required_topic_fields:
    - speaker
    - at
```

The chain YAML declares the required fields. Rust enforces "every emitted topic has a non-empty value for every named field; otherwise reject and retry/fail this iteration." Rust knows nothing about temporal semantics.

This is the corrected version of v1's Phase 3b, landed at the right code site.

### 3.4 Re-ingestion idempotency

**File:** `src-tauri/src/pyramid/db.rs:1698-1700` (`clear_chunks`) and `src-tauri/src/pyramid/ingest.rs`

Today: `clear_chunks` hard-deletes, re-ingest assigns fresh `chunk_index` from 0. Resume keys hit the wrong content if source iteration order changed.

Fix: re-ingest matches existing chunks by `content_hash`. If the hash matches, reuse the existing `chunk_index` and skip re-inserting. Only new content gets new indices. Resume keys remain valid across re-ingestion.

Or: invalidate `pyramid_pipeline_steps` for any chunk whose hash doesn't match its prior version. Less efficient but simpler.

### 3.5 Phase 3 done criteria

- [ ] `pyramid_chunks` has `first_ts`, `last_ts`, `content_hash` columns. Populated on ingest.
- [ ] `Topic` has first-class `speaker` and `at` fields, sortable from Rust.
- [ ] Chain YAML can declare `required_topic_fields:` and the L0 extract step enforces them.
- [ ] Re-ingestion preserves `chunk_index` for unchanged content.
- [ ] An L0 node from a sequential source has populated `speaker` and `at` fields, verifiable by SQL query.

---

## Phase 4 — Bootstrap and auto-update

The audit found that DADBEAR auto-update never overwrites existing chain or prompt files (`chain_loader.rs:202-296` does `if !path.exists() { write }`). End users on auto-update are frozen at whatever was bundled at first install.

### 4.1 Switch from `include_str!` per-file to `include_dir!` for the whole tree

**File:** `src-tauri/src/pyramid/chain_loader.rs:267-279`

Add the `include_dir` crate. Bundle the entire `chains/` tree:

```rust
use include_dir::{include_dir, Dir};
static CHAINS_DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/../chains");
```

The bootstrap then walks `CHAINS_DIR` recursively and writes every file. Future-proof: adding a new prompt directory or chain YAML requires zero changes to `chain_loader.rs`.

### 4.2 Version-stamped sync

Bootstrap currently uses `if !exists { write }`. Replace with: each file in `CHAINS_DIR` carries a version (computed from binary build time or a compile-time constant). Each file on disk carries its stamped version (sidecar file or extended attribute). On startup, sync overwrites disk versions older than bundled. Reverse direction is preserved: user-edited files newer than bundled stay user-edited.

Or simpler: a single `chains/.bundle_version` file. If it differs from the binary's bundled version, rsync the entire `chains/` tree (overwriting). Lose user edits but ensure correctness.

**Recommendation:** the per-file version approach. Slightly more code, much friendlier to users who tweak prompts.

### 4.3 Malformed YAML fallback

Audit finding: a typo in `registry.yaml` would brick a user's install. Fix: any malformed YAML at startup logs a warning and falls back to defaults. Hard error only in dev mode (env var or feature flag).

### 4.4 Phase 4 done criteria

- [ ] `include_dir!` bundles the full `chains/` tree.
- [ ] Auto-update on a user's machine pulls new prompts and chain YAMLs without losing user edits to existing files.
- [ ] Malformed `chains/registry.yaml` (or any other config) does not crash the app.
- [ ] Tests: simulate a fresh install, confirm all bundled files materialize. Simulate an update, confirm new files arrive and unchanged files stay.

---

## Phase 5 — Documentation

This is Phase 2 of the v1 plan, written against the v2 architecture. Defer until Phases 1-4 land.

### 5.1 New doc tree

```
docs/chain-development/
├── README.md
├── 01-architecture.md         — content_type → chain assignment → chain_id → executor
├── 02-chain-yaml-reference.md — schema for chains/defaults/*.yaml (the legacy DSL, since that's prod)
├── 03-prompt-anatomy.md       — chains/prompts/*/*.md, what each one does
├── 04-temporal-conventions.md — pyramid_chunks temporal columns, Topic.speaker/at, required_topic_fields
├── 05-pillar-37.md            — prompt discipline
├── 06-forking-a-chain.md      — recipe with `conversation-legacy-chronological` as worked example
├── 07-adding-a-content-type.md — recipe with the open question of enum-vs-string still flagged
├── 08-testing-a-chain.md      — build + drill + haiku eval pattern
└── 09-troubleshooting.md      — actual failure modes from Runs 1-4 and the audit
```

Worth writing only after the architecture is real.

---

## Deferred / out-of-scope

These came up in the audit and are real, but don't belong in this plan:

- **Closing the `ContentType` enum out** — moving content_type to a free string and dispatching by chain_id. The audit's biggest structural finding. Touches main.rs, build_runner.rs, vine.rs, IPC, 8 frontend files. Worth its own dedicated plan after this one lands.
- **Transcript parser registry** (Otter, Zoom, Granola, Slack, plain `Speaker [HH:MM]:`). Requires the dispatch-by-string change above to be useful.
- **Wire publish content_type contract** with the Vibesmithy / marketplace side. Cross-repo coordination.
- **MCP server temporal awareness.** New columns means new query surface. Out of scope until Phase 3 lands and there's data to surface.
- **Annotations FK CASCADE → SET NULL on chain swap.** The audit found this; the fix is a migration. Worth doing alongside the chain-swap work but easy to forget. **Calling it out here so it doesn't get lost.**
- **`stale_engine` capability flag.** Audit found stale propagation silently no-ops on non-question chains. Add a `supports_staleness` flag on chains. Small but easy to forget.
- **Killing or updating the v3 question DSL and `parity.rs`.** The v1 plan was going to rewrite this; v2 leaves it alone. Decide its fate in a follow-up.
- **`conversation-chronological.yaml` design-spec cleanup.** That file points at non-existent prompts (`prompts/conversation/cluster.md` etc that don't exist — actual files are `conv_*.md`). Either delete it or fix the references. Either way, not load-bearing for v2.

---

## Sequencing

```
Phase 0 (P0 fixes) — independent, ship first
   │
Phase 1 (recover dispatch)
   │
   ├─→ Phase 2 (chain binding schema)
   │
   ├─→ Phase 3 (temporal first-class)
   │
   └─→ Phase 4 (bootstrap/auto-update)
            │
            └─→ Phase 5 (docs)
```

Phases 2, 3, 4 can run in parallel after Phase 1 lands, if the team is split. Realistically Phase 1 → 2 → 3 → 4 → 5 in series, but each phase is small enough that parallelism is possible.

## Risks

1. **`build_conversation` may not be as ready-to-use as `build.rs:684+` looks.** Its behavior might depend on assumptions in `run_legacy_build` that are no longer valid. The 1.3 audit step is the gate.
2. **Porting forward/reverse/combine into `chains/defaults/question.yaml` may hit limitations the audit didn't surface.** The legacy chain executor's `save_as: step_only` and `zip_steps` are confirmed working but not necessarily for a sequential reverse pass with cross-chunk accumulator semantics. Phase 1.4's spike will show.
3. **Adding the `content_type` column or `pyramid_chain_defaults` table needs a migration that handles existing slugs gracefully.** The audit found brittle CHECK constraint patterns elsewhere; the new migration needs to be defensive.
4. **`include_dir!` may inflate the binary noticeably.** The full `chains/` tree is small (mostly markdown and YAML, no images), so probably fine, but worth measuring.
5. **Auto-update overwriting user-edited files is the wrong default.** Per-file versioning solves it; fallback to "single bundle version" is the brittler option.
6. **`Topic.speaker` and `Topic.at` becoming first-class fields means a SQLite migration on `pyramid_topics` (or wherever Topics are stored).** Need to grep for the actual storage shape; the audit didn't dig in.

## Open questions

- Phase 1: route to `build_conversation` verbatim, or port forward/reverse/combine into `chains/defaults/question.yaml`? The Phase 1.3 audit answers this.
- Phase 2: store per-content-type defaults in the DB or a YAML file? DB is more flexible; YAML is more reproducible. Pick one.
- Phase 3: are there other chain types that would benefit from `required_topic_fields:` enforcement (not just temporal)? Probably yes — generalize early.
- Phase 4: if `include_dir!` doesn't work cleanly with Tauri's bundling, fall back to manual `include_str!` enumeration of every chains file. Is this acceptable?
- Should Phase 0 ship as four small commits or one consolidated "P0 cleanup" commit? Recommend four — easier to revert individually if any one regresses.

## Done criteria (overall)

- [ ] Phase 0: forward pass doesn't crash on non-ASCII; dead config either lives or dies.
- [ ] Phase 1: a conversation pyramid built with the chronological assignment actually executes a chronological pipeline (verbatim or ported).
- [ ] Phase 2: an operator can swap which chain a content_type uses by editing config (DB or YAML), no Rust changes.
- [ ] Phase 3: an L0 node from a sequential source has populated `speaker` and `at` fields, queryable by SQL.
- [ ] Phase 4: a fresh install gets all bundled chains; an auto-update gets new ones without losing user edits.
- [ ] No regression on existing pyramids (code, document, conversation default, question).
- [ ] Test pyramid: build a conversation pyramid with the chronological binding from the same `.jsonl` we used for Runs 1-4. Verify L0 has temporal fields. Read the apex; confirm chronological framing. Score is sanity-check, not gate.
