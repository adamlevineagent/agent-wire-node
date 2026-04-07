# Audit Synthesis: chain-binding-and-triple-pass.md

Conductor audit pass run 2026-04-07. Two stages, four independent agents:
- **Stage 1 — Informed:** auditor-A and auditor-B, both given the full plan.
- **Stage 2 — Discovery:** auditor-C and auditor-D, given only a 2-sentence purpose statement plus a known-issues list from Stage 1, sent into the codebase to find new failures.

All four reached convergent conclusions: the plan as written cannot be implemented. The most consequential discoveries are summarized first; full per-finding lists follow.

---

## The four bombshells

### 1. The chronological conversation pipeline already exists (auditor-C M12)

`src-tauri/src/pyramid/build.rs:684+` contains a fully implemented `build_conversation` function with forward pass, reverse pass, combine into L0, L1 thread pairing, and L2 thread synthesis. It is exactly what the plan calls a "triple-pass chronological variant."

It is unreachable because `src-tauri/src/pyramid/build_runner.rs:237` routes `ContentType::Conversation` directly to `run_decomposed_build`, which never reaches `run_legacy_build` (which is the only caller of `build_conversation`). `run_legacy_build` is only invoked when both `use_chain_engine` and `use_ir_executor` are false — and `use_chain_engine` is defaulted on in production.

**Implication:** the plan was about to spend Phase 3a writing executor extensions to make `chains/questions/conversation-chronological.yaml` work in a DSL that production doesn't run, when the code that actually does forward/reverse/combine is sitting in the tree, dead. Recover or re-route, do not rebuild.

### 2. The plan targets the wrong DSL (auditor-A C1, auditor-B context)

There are TWO chain DSLs in the codebase:
- **Production DSL (legacy `ChainStep`):** `chains/defaults/question.yaml`, loaded by `chain_loader.rs`, executed by `chain_executor.rs`. This is what every real build runs.
- **Validation-only DSL (v3 question):** `chains/questions/*.yaml`, loaded by `question_loader.rs` and `question_compiler.rs`. **Only `parity.rs` consumes it.** Not in any production build path.

The plan's design-spec (`chains/questions/conversation-chronological.yaml`) is written in DSL #2. Phase 3a's "extend `question_loader.rs:158` to accept `direction: reverse`" / "add `zip_steps` to `StepInput`" / "add `enforce_topic_fields:` to chain YAML DSL" all target DSL #2. Implementing them as written would touch only the parity validator, not the production executor — `conversation-chronological.yaml` could pass `validate_question_compilation` and still never run.

Worse, the design-spec uses `creates: forward_view` / `creates: reverse_view` which would be **rejected** by the existing `is_recognized_creates` allow-list (`question_yaml.rs:171-186`). The about-clause "each chunk individually, processed in reverse order" is not in `RECOGNIZED_SCOPES`. The spec doesn't even validate against today's DSL #2 loader.

### 3. Multiple "new features" the plan proposes already exist in the production executor (auditor-A M3/M4)

Verified call sites:
- **`save_as: step_only`** — `StorageKind::StepOnly` exists in `execution_plan.rs:370`. `chains/defaults/question.yaml` uses it at lines 18, 100, 115, 129, 152, 165, 180, 187. The legacy chain executor honors it. Phase 3a.2 collapses to "nothing to do."
- **`zip_steps`** — `chain_executor.rs:1997-2070` implements `zip_steps` with `step` / `reverse: true` syntax that exactly matches what the plan proposes. Phase 3a.3 collapses to "reuse existing implementation."
- **`instruction_map: content_type:`** — `chains/defaults/question.yaml:27-28` declares `content_type:conversation: $prompts/conversation/source_extract_v2.md`. The plan does not mention this primitive form of content-type-aware prompt routing. **(BUT see auditor-C #2 below for the catch.)**

### 4. Critical UTF-8 panic in the accumulator code (auditor-D #1)

`src-tauri/src/pyramid/chain_executor.rs:6960-6964`:

```rust
let truncated = if new_val.len() > max_chars {
    new_val[..max_chars].to_string()
} else { new_val };
```

`new_val.len()` is byte length. `new_val[..max_chars]` is a byte slice. If the byte at `max_chars` falls inside a multi-byte UTF-8 codepoint (any non-ASCII: smart quotes, em-dash, accented Latin, CJK, emoji), this **panics** with `byte index N is not a char boundary`. A char-aware helper exists in the same file (`truncate_for_webbing` at 1553) and is not used here.

**Implication:** the very forward-pass mechanism the plan wants to extend is broken on the first transcript with a non-ASCII character at the wrong byte. This is not an edge case — every Claude Code session contains em-dashes. Independent of the rest of the audit, this is a P0 fix.

---

## Critical findings (full list)

| # | Auditor | Finding | Location |
|---|---------|---------|----------|
| C1 | A | Plan targets DSL #2 (parity-only); production runs DSL #1 (legacy ChainStep). Phase 3a entirely mistargeted. | `build_runner.rs:803-820`, `chain_registry.rs:97`, `parity.rs` |
| C2 | A | The hardcoded `prompts/question/X.md` strings the plan wants to replace are in legacy executor primitives, not v3 DSL. Phase 1 reframing required. | `evidence_answering.rs`, `extraction_schema.rs`, `characterize.rs`, `question_decomposition.rs`, `chain_loader.rs:267-271` |
| C3 | A | `enforce_topic_fields` Phase 3b targets `extraction_schema.rs::generate_synthesis_prompts`, but that function generates UPPER-LAYER synthesis prompts, not L0 schemas. Phase 3b would not fix Run 4 even after implementation. The L0 schema is set elsewhere (the static `chains/prompts/question/source_extract.md`). | `extraction_schema.rs:201`, `chain_executor.rs:4738` |
| BC1 | B | Prompts are bundled via `include_str!` in `chain_loader.rs:267-279`. Adding new prompt directories requires Rust bootstrap changes. Auto-update users would silently miss new prompts. | `chain_loader.rs:267-279` |
| BC2 | B | `ContentType` enum + `pyramid_slugs.content_type` CHECK constraint must be migrated to add `conversation-chronological`. Plan straddles "first-class" and "virtual" without committing. | `types.rs:31-65`, `db.rs:56`, `db.rs:1116` |
| BC3 | B | Frontend `AddWorkspace.tsx` hardcodes content type union as `'code' \| 'document' \| 'conversation' \| 'vine'` (literally missing `'question'` despite backend support). 8+ files reference it. Plan does not mention UI work. | `src/components/AddWorkspace.tsx:41,168`, others |
| C-#1 | C | `Conversation` and `Question` dispatch in `build_runner.rs:199-277` bypasses `pyramid_chain_assignments` lookup entirely. `run_decomposed_build` calls `chain_registry::default_chain_id` directly with no assignment check. The "swap chains per content type" foundation does not exist for the only content types that go through the question pipeline. | `build_runner.rs:199-277`, `build_runner.rs:802-817` |
| C-#2 | C | `instruction_map: content_type:conversation:` is **dead config**. `chain_executor.rs:1034-1070` only matches keys with `type:`, `language:`, `extension:`, `type:frontend` prefixes. **No code path matches `content_type:` keys.** The conversation-tuned extractor declared in `chains/defaults/question.yaml:27-28` silently never executes. Production runs the generic prompt. Stage 1 known-issue #9 was incorrect — this mechanism is dead. | `chain_executor.rs:1034-1070`, `chains/defaults/question.yaml:27-28` |
| C-#3 | C | `extraction_schema.rs::generate_extraction_schema()` is **dead code**. Defined, exported, fully implemented with tests, **never called from anywhere** in `src-tauri/src/`. The L0 schema in production is the static `source_extract.md` for every build. | `extraction_schema.rs:40` |
| C-#4 | C | Speaker/timestamp ingest only works for Claude Code JSONL. `parse_conversation_messages` only accepts `type: user\|assistant`, hardcodes labels to `PLAYFUL`/`CONDUCTOR`. Any other transcript source (Zoom, Otter, Granola, Meet, Slack, podcast) is silently dropped via `continue`. The plan's claim of "domain-neutral conversation/meeting/interview" is false. | `ingest.rs:171-237` |
| C-#5 | C | Chunk transcript splits on any line beginning `--- ` — markdown horizontal rules and code-block separators trigger false speaker boundaries mid-message and corrupt chunk alignment. | `ingest.rs:244-282` |
| C-#6 | C | Chunked conversations lose speaker context across boundaries. A long monologue split mid-utterance leaves chunk N+1 starting mid-sentence with no `---` header. | `ingest.rs:244-282` |
| C-#11 | C | `stale_engine` hardcodes the question-chain shape: "All pyramids now use the question chain regardless of content_type. Propagation always follows evidence KEEP links." A non-question chain (chronological variant or any future custom chain) won't produce KEEP links → **stale propagation silently no-ops** with no warning. | `stale_engine.rs:1406-1407` |
| C-#12 | C | **The legacy `build_conversation` (forward/reverse/combine) is unreachable in default config.** `build.rs:684+` has the chronological pipeline implemented. `build_runner.rs:237` routes Conversation to `run_decomposed_build`, never reaches `run_legacy_build`. The user already has a chronological pipeline; it is just bypassed. | `build.rs:684+`, `build_runner.rs:237`, `build_runner.rs:622` |
| D-#1 | D | **UTF-8 panic in `update_accumulators`.** Byte-slice at `max_chars` panics on first non-ASCII character at the wrong boundary. Forward pass crashes on any transcript with smart quotes, em-dashes, accented Latin, CJK, emoji. P0 regardless of plan. | `chain_executor.rs:6960-6964` |
| D-#3 | D | Concurrent `for_each` silently breaks sequential semantics. A chronological chain authored without `sequential: true` runs in parallel; `update_accumulators` is never called between iterations; each iteration sees initial accumulator state. No runtime guard. | `chain_executor.rs:5639-5661`, `execute_for_each_concurrent` |

## Major findings

| # | Auditor | Finding | Location |
|---|---------|---------|----------|
| A-M1 | A | Composability claim overstated. New chain authors who forget to declare temporal fields silently lose temporal capture. Need a validator that warns when ingest provides markers no schema field captures. | Phase 3b |
| A-M2 | A | `direction: reverse` interaction with chunk batching, parallel mode, dispatch_order, and storage indexing not analyzed. Recommend `$chunks_reversed` input expression instead of inventing a `direction:` flag — one branch in `resolve_input_reference`, reuses existing zip_steps. | Phase 3a.1 |
| A-M5 | A | Phase 1 call-site table is mixed accuracy. `evidence_answering.rs` already has `source_content_type` in scope. `extraction_schema.rs::generate_extraction_schema` is dead. `characterize.rs` runs BEFORE content_type is fully known and cannot be content-type-conditional. `chain_loader.rs:267-279` (the include_str! bootstrap) is NOT in the plan's call-site list and must be. | Phase 1 |
| A-M6 | A | Phase 1 effort estimate too low. Missing: ContentType enum changes if first-class, DB CHECK migration, prompt-bootstrap path, UI dropdown. Realistic: ~600-900 lines + migration + UI change. | Phase 1 |
| A-M7 | A | Parity validator (`validate_question_compilation`) is currently load-bearing for `chains/questions/conversation.yaml`. The chronological design-spec fails its validation today. Plan must decide: kill DSL #2 entirely or update validator. | `parity.rs:854` |
| A-M8 | A | Test plan non-falsifiable. The 8/10 haiku score is a free-form judgment that drifts run-to-run (Run 2-4 already showed ±2 noise on same source, same evaluator). Replace with truth conditions: SQL assertion that L0 nodes have populated `speaker` and `at` fields; no all-DISCONNECT L1s; apex contains chronological framing markers. | Phase 3e |
| B-M1 | B | `save_as: step_only` per-build hashmap loses crash-resume — long builds (hours for ~100 chunks) cannot resume mid-combine. Need transient `pyramid_step_outputs` table. | Phase 3a.2 |
| B-M2 | B | Cancellation token plumbing for new reverse-iteration loop unaddressed. Cancel mid-reverse-pass leaves zombie LLM call queue draining. | Phase 3a.1 |
| B-M3 | B | Build progress / instrumentation / cost log not addressed for forward/reverse/combine step types. Progress bar would freeze during 100-chunk reverse pass. Cost log dimension list needs new step names. | Phase 3a |
| B-M4 | B | `parity.rs` already loads `chains/questions/*.yaml` for dual validation — Phase 1 must update parity to consume `ChainRegistry::resolve()` or accept silent drift. | Phase 1 |
| B-M5 | B | Schema-field injection has no de-duplication policy. What if LLM generates `speaker_name` and chain enforces `speaker`? What if descriptions conflict? Specify dedupe by case-insensitive name match, enforced wins on description, hard error on type conflict. | Phase 3b |
| B-M6 | B | Plan internally contradicts itself: Risk #4 says "the default for any binding flagged as `temporal: true`..." but Phase 3b says "Rust contains zero references to temporal." Risk #4 reintroduces what 3b removes. Delete Risk #4. | Plan internal |
| B-M7 | B | `zip_steps` ordering semantics under-specified. Storage layer determines ordering, not the reverse flag at zip time. Specify: per-chunk hashmap always indexed by absolute chunk index regardless of pass direction; `zip_steps` always pairs by absolute index; `reverse:` flag is no-op at zip time. | Phase 3a.3 |
| B-M8 | B | Phase 1 default registry pointing at `prompts/question-conversation/` is dead-on-arrival on any installation that didn't pre-bundle the fork. Combined with C1, Phase 1 hard-errors at startup on a fresh install. | Phase 1 rollout |
| C-#7 | C | Tier 2 bootstrap writes broken stub conversation/code/document YAMLs (`chain_loader.rs:322-371`) — `placeholder` step with `compress`/`extract` primitive. The "real" YAMLs only land via Tier 1 (source-tree sync, dev-only). Release standalone users get broken stubs. | `chain_loader.rs:322-371` |
| C-#8 | C | DADBEAR auto-update never refreshes existing chain or prompt files. Tier 2 bootstrap loops do `if !path.exists() { write }`. Once a user has `defaults/question.yaml`, an auto-update will not overwrite. Auto-updated end users **never** receive new prompts, new chain YAMLs, or new chain files. | `chain_loader.rs:202-296` |
| C-#9 | C | `conversation-chronological.yaml` references prompts that don't exist by those names. The yaml says `prompts/conversation/cluster.md`, `thread.md`, `recluster.md`, `web.md`, `distill.md`. The actual files are prefixed `conv_*.md`. Even after executor support lands, the file as-written would fail prompt resolution. | `chains/questions/conversation-chronological.yaml:90,103,111,120,130` |
| C-#10 | C | `Topic.speaker` / `Topic.at` survive only via `#[serde(flatten)] extra`. No Rust code can sort topics chronologically by `at`, no Rust code can filter by `speaker`. A chronological pipeline relying on these fields silently degrades to non-chronological whenever the LLM omits or mis-formats them, with no error. | `types.rs:90-108` |
| D-#2 | D | Accumulator semantics are REPLACE not APPEND. The accumulator is overwritten on every iteration. A "forward pass with accumulating context" cannot be expressed with the existing primitive — the LLM has to be prompt-engineered to extend each turn. Need explicit `accumulate.mode: append\|replace\|fold` knob. | `chain_executor.rs:6948-6967` |
| D-#4 | D | Sequential `for_each` `break`s on cancel without persisting partial accumulator state or returning a typed cancellation error. Cancel collapses to "successful partial result" that downstream steps process as if complete. | `chain_executor.rs:5663-5666` |
| D-#5 | D | `for_each` `error_strategy::Abort` returns `Err(...)` immediately without saving accumulator state. On resume, accumulator HashMap is rebuilt from scratch. **Investigate:** is `state.accumulators` actually persisted to disk? If not, forward-pass resume is broken. | `chain_executor.rs:6125-6128, 6442-6452, 9085, 9156` |
| D-#6 | D | Chunk model has no temporal ordering guarantee, only insertion order. `chunk_index` is just an integer assigned by the ingester. No constraint, comment, or test asserting chunk_index 0 = chronologically first. No `first_ts`/`last_ts` columns on `pyramid_chunks`. A chronological chain processing `for_each: $chunks` iterates in insertion order, which depends on the source iterator. For Slack export, that may be reverse-chronological. | `db.rs:99-108, 5634-5649`, `ingest.rs:363` |
| D-#7 | D | Re-ingestion shuffles `chunk_index`, breaking idempotency and resume. `clear_chunks` does hard DELETE then re-assigns indices from 0. Resume keys in `pyramid_pipeline_steps` are `(slug, step_type, chunk_index, depth, node_id)` — they hit on the wrong content after re-ingestion. Forward-pass running summary computed for old chunk 47 reused as iteration 47's resume state even though chunk 47 is now a different message. | `db.rs:1698-1700`, `ingest.rs` |
| D-#8 | D | Annotations FK CASCADE: `pyramid_annotations` foreign key is `ON DELETE CASCADE`. A chain swap producing nodes with different IDs silently drops all annotations. The schema comment claims "annotations survive on superseded nodes" but the FK contradicts it. As soon as content-type-aware routing lands, every existing conversation slug will swap chains and lose annotations on first rebuild. | `db.rs:228-238` |
| D-#9 | D | `pyramid_chain_assignments` table has no `content_type` column. Per-content-type defaults cannot be added without joining `pyramid_slugs` for content_type at every default lookup. Schema does not support the plan's foundational requirement. | `chain_registry.rs:5-17` |
| D-#10 | D | MCP server has zero schema awareness of new node fields. Generic node interface will not surface `speaker`/`at` to external agents. Chronological structure invisible to consumers. | `mcp-server/src/lib.ts` |
| D-#11 | D | Wire publish layer transmits `content_type` as free-form string. Downstream consumer (Vibesmithy, marketplace) likely has its own enum or CHECK that will reject unknown values, or silently coerce back to `conversation` and drop temporal metadata. | `wire_publish.rs:54, 454, 587, 685` |
| D-#12 | D | Vine pipeline has hard `match` on `ContentType` with no fallthrough at 5+ sites. Adding `ConversationChronological` is a recompile change in `main.rs:3145`, `main.rs:3849`, `build_runner.rs:190, 199, 237, 647`, `vine.rs:569`. **The user's stated goal — swappable per content type via config — cannot be met by adding more enum variants.** It requires moving content_type to a free string and dispatching by chain_id. | `main.rs:3145-3240, 3849-3866`, `build_runner.rs:190-237, 647-683`, `vine.rs:569-586` |

## Minor / investigate findings

(Listed in the original auditor outputs; not enumerated here for brevity. See agent transcripts for: bootstrap path strictness, malformed-yaml fallback, registry vs deprecated chain assignment, brittle CHECK migration substring matching, ISO-19-char timestamp truncation, `status: design-spec` field rejection, `running_context` reference may be aspirational, duplicate ContentType dispatch in main.rs vs build_runner.rs, `effective_l0_slug` interaction.)

---

## Cross-cutting themes

1. **Rust dispatch is structurally hostile to "config-driven."** ContentType is a closed enum with exhaustive matches in 5+ files (`vine.rs`, `build_runner.rs`, `main.rs`, IPC layer, the wizard UI). Adding any variant is a recompile in many places. The user's goal requires moving content_type to a free string and dispatching by `chain_id`, not adding more enum variants. This is the largest single rewrite the plan does not contemplate.

2. **The ingest layer is Claude-Code-specific.** "Domain-neutral conversation/meeting/interview" is currently false. `parse_conversation_messages` only handles Claude Code JSONL; everything else is silently dropped. A real chronological pipeline for "any sequential transcript" needs a transcript-parser registry (Claude JSONL, Otter, Zoom VTT, Granola, plain `Speaker [HH:MM]:`).

3. **Temporal anchors are not first-class data.** `Topic.speaker` and `Topic.at` only survive via `#[serde(flatten)] extra` — Rust cannot sort, filter, or validate them. `pyramid_chunks` has no `first_ts`/`last_ts` columns. Re-ingestion shuffles chunk_index without preserving content. Chronological reasoning is structurally impossible to enforce in code.

4. **The bootstrap and auto-update story is broken.** Tier 2 stubs are placeholders. Tier 1 is dev-only. DADBEAR auto-update never overwrites existing chain/prompt files. Even if everything else were fixed, end users would not receive the changes.

5. **Stage 1 known-issue #9 was incorrect.** `instruction_map: content_type:` looks like a content-type-aware routing primitive in `chains/defaults/question.yaml`, but `chain_executor.rs:1034-1070` does not match `content_type:` keys. The mechanism is dead config. The actual primitive form of registry binding does not exist in production.

6. **Run 4's failure mode (L0 missing speaker/at) can't be fixed where the plan aimed.** The plan targets `extraction_schema.rs::generate_synthesis_prompts`, which generates upper-layer synthesis prompts, not L0 schemas. The L0 schema is set by the static `source_extract.md`. The fix has to land at a different site entirely.

---

## Recommended path forward

The plan should be **archived, not amended.** A new plan should be written from these findings. Suggested shape:

### Phase 0 — P0 fixes that ship independently

These are bugs that exist regardless of the chain-binding work and should land first:

- Fix the UTF-8 panic in `update_accumulators` (D-#1) — replace byte-slice with char-aware truncation.
- Fix the `instruction_map: content_type:` matcher (auditor-C #2) — either implement the missing key handler or delete the dead key from question.yaml. Decide.
- Fix or delete `generate_extraction_schema()` (auditor-C #3) — wire it into the build pipeline or remove it.
- Fix the `--- ` chunk-boundary heuristic (C-#5) — replace text-parsing with structural metadata.

### Phase 1 — Recover the existing chronological pipeline

Instead of building anything new, fix the dispatch:

- `build_runner.rs:237` currently routes Conversation to `run_decomposed_build` unconditionally. Make it consult `pyramid_chain_assignments` and the chain registry. If the assignment selects a chain with `engine: legacy`, route to `run_legacy_build` instead. This makes `build_conversation` (already in `build.rs:684+`) reachable.
- Add a content_type-aware default lookup (replacing `chain_registry::default_chain_id`'s wildcard). This is the actual "config-driven binding" the plan wanted, in a much smaller package.
- Schema: add `content_type` column to `pyramid_chain_assignments` OR a new `chain_defaults_by_content_type` table. Pick one.

### Phase 2 — Make new content_types dispatchable without recompiles

Replace the closed `ContentType` enum with a free string + a dispatch table keyed by chain_id (not content_type). This is the structural change that turns the plan's "config-driven" goal from impossible to possible. Touches main.rs, build_runner.rs, vine.rs, the IPC layer, and the wizard UI. Big.

### Phase 3 — Persist temporal anchors as first-class data

- Add `first_ts`, `last_ts` columns to `pyramid_chunks`.
- Add `speaker` and `at` to the `Topic` struct as first-class fields, not flattened extras.
- Migrate the L0 schema-generation site (wherever it actually lives — needs investigation per finding A-C3) to honor a `required_fields:` declaration from the chain config.
- Build a transcript-parser registry to replace the Claude-Code-only ingest path.

### Phase 4 — Bootstrap and auto-update

- Switch Tier 2 bootstrap from `if !exists { write }` to version-stamped sync that overwrites on auto-update.
- Use `include_dir!` to bundle the entire `chains/` tree, not file-by-file `include_str!`.
- Make malformed `registry.yaml` fall back to defaults with a logged warning (don't brick the user's install).

### Phase 5 — Documentation

Phase 2 of the original plan (`docs/chain-development/`) — but written against the new architecture, not the original mental model.

---

## What the plan got right

- The composability principle in (revised) Phase 3b — "the chain config decides; Rust enforces what the config says" — is correct and worth preserving. The execution surface around it just needed to land somewhere real.
- The decision to fork `chains/prompts/question/` → `chains/prompts/question-conversation/` is sound; it just hasn't been wired to anything.
- The recognition that Run 4's failure mode comes from meta-prompting drift in schema generation is correct, even though the planned fix targeted the wrong file.
- The done-criteria checklist format is good; the criteria themselves need to be replaced with truth conditions, not haiku scores.

---

## Files referenced (for the next plan author)

| Concern | File |
|---|---|
| Existing chronological implementation | `src-tauri/src/pyramid/build.rs:684+` (`build_conversation`) |
| Dispatch site that bypasses it | `src-tauri/src/pyramid/build_runner.rs:237` |
| Chain registry stub | `src-tauri/src/pyramid/chain_registry.rs:97` (`default_chain_id`) |
| Production chain executor | `src-tauri/src/pyramid/chain_executor.rs` |
| UTF-8 panic site | `src-tauri/src/pyramid/chain_executor.rs:6960-6964` |
| zip_steps existing impl | `src-tauri/src/pyramid/chain_executor.rs:1997-2070` |
| `instruction_map` matcher (missing `content_type:` arm) | `src-tauri/src/pyramid/chain_executor.rs:1034-1070` |
| Dead `generate_extraction_schema` | `src-tauri/src/pyramid/extraction_schema.rs:40` |
| Static L0 schema source | `chains/prompts/question/source_extract.md` |
| Conversation ingest (Claude-Code-only) | `src-tauri/src/pyramid/ingest.rs:171-237` |
| Chunk schema | `src-tauri/src/pyramid/db.rs:99-108` |
| Annotations FK CASCADE | `src-tauri/src/pyramid/db.rs:228-238` |
| ContentType enum | `src-tauri/src/pyramid/types.rs:31-65` |
| ContentType CHECK | `src-tauri/src/pyramid/db.rs:56` |
| Frontend content_type union | `src/components/AddWorkspace.tsx:41,168` (and 7 other files) |
| Bootstrap stubs | `src-tauri/src/pyramid/chain_loader.rs:322-371` |
| `include_str!` bootstrap | `src-tauri/src/pyramid/chain_loader.rs:267-279` |
| stale_engine question-chain assumption | `src-tauri/src/pyramid/stale_engine.rs:1406-1407` |
| Wire publish content_type field | `src-tauri/src/pyramid/wire_publish.rs:54, 454, 587, 685` |
| MCP server (no temporal awareness) | `mcp-server/src/lib.ts` |
| Vine ContentType dispatch | `src-tauri/src/pyramid/vine.rs:569-586` |
| parity validator | `src-tauri/src/pyramid/parity.rs:854` |

---

## Net assessment

The original plan should not be implemented in any form. The work the user wants is real and valuable, but the plan was written against a mental model of the system that does not match what is in the code. Two of the most important findings — that a chronological pipeline already exists in `build.rs` and that ContentType is a closed enum dispatched by exhaustive matches in 5+ files — completely reshape what "config-driven chain binding" has to mean.

A new plan should start from these findings and the recommended path forward. The audit findings here should be the input to that planning, not a list of edits to the original document.
