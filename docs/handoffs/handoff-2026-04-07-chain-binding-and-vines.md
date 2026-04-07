# Handoff — 2026-04-07 — Chain Binding + Recursive Vines

> **TL;DR for the next session:** Two build plans need shipping today, in this order: (1) `docs/plans/chain-binding-v2.md` first — small, foundational, includes critical bug fixes. (2) `docs/plans/recursive-vine-v2.md` second — depends on chain-binding-v2 finishing. The previous plan `chain-binding-and-triple-pass.md` is INVALIDATED by a 4-agent audit; do not implement it. Read the audit synthesis at `chain-binding-and-triple-pass.audit.md` for the receipts.

---

## 1. Current state (in three sentences)

We spent this session iterating on conversation pyramid quality through 4 test runs, found and patched several real bugs, then wrote a plan to make the chain pipeline truly composable per content type — but the plan got eviscerated by a conductor audit that found the work targets the wrong DSL, that a chronological pipeline already exists in `build.rs:684+` but is unreachable, and that there's a critical UTF-8 panic in the accumulator code. We rewrote the plan as `chain-binding-v2.md` starting from the audit findings, and you (Adam) brought me a separate `recursive-vine-v2` design that sits on top of it. **The next session ships both plans, in order, today.**

---

## 2. Recommended sequencing for the next session

```
TODAY
═══════════════════════════════════════════════════════════════
Phase 0 of chain-binding-v2          ┐
   0.1 UTF-8 panic fix               │  ship as 4 small commits,
   0.2 instruction_map content_type: │  zero design risk,
   0.3 generate_extraction_schema    │  do this first
   0.4 chunk_transcript --- regex    ┘

Phase 1 of chain-binding-v2          ┐
   1.3 audit build.rs:684+           │  the gate — don't skip,
       (the build_conversation       │  this is what makes the
        function that already exists)│  rest of the plan possible
   1.4 decide route-vs-port          │
   1.5 ship the dispatch fix         ┘

Phase 2 of chain-binding-v2 (chain binding schema)
Phase 3 of chain-binding-v2 (temporal first-class)
Phase 4 of chain-binding-v2 (bootstrap/auto-update)
─────── chain-binding-v2 done ───────

Lifted-from-deferred quick wins (also today):
   - Annotations FK CASCADE → SET NULL migration
   - stale_engine supports_staleness capability flag
   - Closed-ContentType-enum work as its own quick plan-and-do

recursive-vine-v2 Phase 1 (pyramid evidence provider)
recursive-vine-v2 Phase 2 (gap-to-ask escalation)
recursive-vine-v2 Phase 3 (domain vine UX)
recursive-vine-v2 Phase 4 (cross-operator vines)
─────── recursive-vine-v2 done ───────

Phase 5 of chain-binding-v2 (docs) — last, against the now-real architecture
```

The reason for this order:
- **Phase 0 fixes are independent and ship safely.** The UTF-8 panic crashes any forward pass on em-dashes — this needs to land regardless of any other work.
- **Phase 1 of chain-binding-v2 has a hard audit gate.** `build.rs:684+` (`build_conversation`) was found to be a fully implemented forward/reverse/combine pipeline that's just unreachable. Read it before deciding whether to route to it verbatim or port its shape into `chains/defaults/question.yaml`.
- **chain-binding-v2 Phase 2 is a prerequisite for vine work.** Recursive vines need real per-content-type chain selection.
- **chain-binding-v2 Phase 3 (temporal first-class) is a prerequisite for vine queries against sequential sources.** Without `Topic.speaker` and `Topic.at` as queryable Rust fields, vine evidence escalation can't do chronological reasoning.
- **The closed-enum plan is the bridge.** chain-binding-v2 doesn't fix the closed `ContentType` enum, but vines can't add new sub-types (`ConvoVine`, `CodeVine`, `MeVine`) without it. That's the "lifted-from-deferred" middle slot. Possibly ship this as part of vine Phase 1.

---

## 3. Backstory: how we got here

### 3.1 Where we started this session (carried over from prior context)

The previous session was a multi-day push on the **post-agents-retro web surface** for `agent-wire-node` — public HTML routes, Cloudflare tunnel, OTP session bridge, conductor audit cycles, fast-follow private-tier work, several bug fixes (CSRF placeholder, ETag staleness, slug-stats apex bug, web UI redesign). All of that is in git history with commits like `2dc2a40`, `3073ad7`, `e9c9c7f`, `b3e42dd`, `48cd70b`. None of it is what this handoff is about, but it's the substrate.

### 3.2 What this session was actually about

The user wanted to test conversation pyramids built from a single Claude Code session `.jsonl`. We did 4 test runs, each one teaching us something about why the question pipeline (which is supposed to handle "any content type") doesn't actually handle sequential transcripts well.

The full arc is captured in `docs/conversation-pyramid-testing-state.md`. Short version:

| Run | What happened |
|---|---|
| 1 | Crashed on launch — `build_folder_map` only handled directories, not single .jsonl files. Fixed in commit `48cd70b`. |
| 2 | Built clean, scored 8/10 by haiku tester. Used the legacy default question. |
| 3 | Rebuild produced a hallucinated meta-node ("Purpose and Value of This Chat Session") with all-DISCONNECT verdicts and generic copy. Two compounding bugs: (a) the decompose step asks meta-questions about the artifact itself for sequential transcripts; (b) the answer step had no all-disconnect guard. |
| 4 | After prompt edits in commit `b3e42dd`: abstain rule worked, meta-questions gone, apex narratively chronological. **But the temporal field capture didn't land** — L0 nodes still missing `speaker`/`at` fields because the meta-prompted directive in `extraction_schema.md` leaked. Scored 6/10 (judged against the higher bar of the four claimed fixes). |

### 3.3 The shipped work that's still in place

These commits all stand and are working:

| Commit | What |
|---|---|
| `48cd70b` | `build_folder_map` handles single-file sources |
| `844ad25` | Stop truncating headlines on save |
| `a3056bc` | Web home: collapse Topic structure by default |
| `a7d8a50` | Backup conversation v1 + add chronological design-spec |
| `b3e42dd` | abstain on empty evidence + temporal-aware extraction directives |
| `e9c9c7f` | Fork `chains/prompts/question/` → `chains/prompts/question-conversation/` |
| `3073ad7` | docs/conversation-pyramid-testing-state.md |

The last three are the real artifacts of this session's prompt-only work. They sit in the repo waiting for the Rust changes that would make them load-bearing. The fork in `e9c9c7f` is **not yet wired into the executor** — every build still loads from `prompts/question/`.

### 3.4 The plan that died

After Run 4, I wrote `docs/plans/chain-binding-and-triple-pass.md`. It proposed:
- Phase 1: a `chains/registry.yaml` config with a Rust loader/resolver to make per-content-type prompts swappable
- Phase 2: a `docs/chain-development/` doc tree
- Phase 3: executor extensions (`sequential_context.direction: "reverse"`, `save_as: step_only`, `input.zip_steps`, `enforce_topic_fields:`) to make the v3 question DSL execute the chronological design-spec at `chains/questions/conversation-chronological.yaml`

The user asked for a conductor audit. I ran 4 agents (2 informed Stage 1 + 2 discovery Stage 2). They came back with **devastating, convergent findings**.

### 3.5 The audit findings (in order of severity)

Full audit at `docs/plans/chain-binding-and-triple-pass.audit.md`. The four bombshells:

1. **`src-tauri/src/pyramid/build.rs:684+` already contains a fully implemented `build_conversation` function** with forward pass + reverse pass + combine into L0 + L1 thread pairing + L2 thread synthesis. It is exactly the "triple-pass chronological variant" the plan wanted to build. It is unreachable because `build_runner.rs:237` routes Conversation directly to `run_decomposed_build` which never reaches `run_legacy_build` (the only caller of `build_conversation`). **The plan was about to rebuild what's already in the tree.**

2. **The plan targets the wrong DSL.** Production runs `chains/defaults/question.yaml` (legacy `ChainStep` DSL) via `chain_executor.rs`. The plan extends `chains/questions/*.yaml` (v3 DSL) which is consumed only by `parity.rs` for validation. Implementing the plan would change a validator that no production build depends on.

3. **`save_as: step_only` and `zip_steps` already exist in production.** `execution_plan.rs:370` has `StorageKind::StepOnly`. `chain_executor.rs:1997-2070` has `zip_steps` with `step` / `reverse: true` syntax. `chains/defaults/question.yaml` uses both already. The plan's Phase 3a.2 and 3a.3 collapse to "nothing to do."

4. **Critical UTF-8 panic in `update_accumulators`.** `chain_executor.rs:6960-6964` byte-slices a String at `max_chars`. Any non-ASCII character at the wrong byte boundary panics — em-dashes, smart quotes, accents, CJK, emoji. **Every Claude Code session contains em-dashes.** P0 regardless.

Beyond the four bombshells, the audit found 25+ major findings and many minor ones. Highlights:

- **`instruction_map: content_type:conversation:` in `chains/defaults/question.yaml` is dead config.** The matcher in `chain_executor.rs:1034-1070` only handles `type:`, `language:`, `extension:`, `type:frontend` prefixes. The `content_type:` key is never matched. Conversation builds silently use the generic prompt.
- **`extraction_schema.rs::generate_extraction_schema()` is dead code.** Defined, exported, never called from anywhere in `src-tauri/src/`.
- **`pyramid_chain_assignments` has no `content_type` column.** Per-content-type defaults cannot be added without a schema change.
- **`ContentType` is a closed Rust enum dispatched by exhaustive `match` in 5+ files** (`main.rs`, `build_runner.rs`, `vine.rs`, IPC, wizard UI). "Config-driven, no recompile" is impossible without moving content_type to a free string and dispatching by `chain_id`.
- **Annotations FK is `ON DELETE CASCADE`.** Chain swap with different node IDs silently drops all annotations. As soon as content-type-aware routing lands, every existing conversation slug loses annotations on first rebuild.
- **DADBEAR auto-update never refreshes existing chain or prompt files.** `chain_loader.rs:202-296` does `if !path.exists() { write }`. Tier 2 bootstrap stubs are placeholder strings. Auto-updated end users never receive new prompts.
- **Conversation ingest only handles Claude Code JSONL.** `parse_conversation_messages` hardcodes `PLAYFUL`/`CONDUCTOR` labels and accepts `type: user|assistant`. Zoom, Otter, Granola, Slack, podcasts — all silently dropped via `continue`.
- **`pyramid_chunks` has no temporal ordering guarantee.** `chunk_index` is just an integer assigned in source-iteration order. No `first_ts`/`last_ts` columns. Re-ingestion shuffles indices, breaking idempotency and resume.
- **`Topic.speaker` and `Topic.at` survive only via `#[serde(flatten)] extra`.** No Rust code can sort, filter, or validate them.
- **`stale_engine` hardcodes the question-chain shape** and silently no-ops on chains that don't produce evidence KEEP links — a vine-killer.
- **The accumulator semantics are REPLACE not APPEND.** A "forward pass with growing summary" cannot be expressed with the existing primitive without prompt-engineering the LLM to extend each turn (which is unreliable).
- **Concurrent `for_each` silently breaks sequential semantics** if the chain author forgets `sequential: true`. No runtime guard.
- **The chronological design-spec at `chains/questions/conversation-chronological.yaml` references prompts that don't exist.** Says `prompts/conversation/cluster.md` etc; actual files are `conv_*.md`. Won't load even after executor support lands.
- **Frontend `AddWorkspace.tsx` content_type union is hardcoded** in 8+ files and is currently missing `'question'` despite backend support.

### 3.6 What I did with the audit findings

1. Added an INVALIDATED banner to the top of `chain-binding-and-triple-pass.md` with a pointer to the audit.
2. Wrote `docs/plans/chain-binding-and-triple-pass.audit.md` with the full per-finding tables, cross-cutting themes, and recommended path forward (commit `35da1c9`).
3. Wrote `docs/plans/chain-binding-v2.md` from scratch, anchored entirely to audit findings (commit `e426921`).
4. Discussed the user's separate `recursive-vine-v2` design and concluded it should be a sibling plan, not folded in.

---

## 4. The two plans, summarized

### 4.1 `docs/plans/chain-binding-v2.md`

#### Phase 0 — P0 fixes (independent, ship as 4 small commits)
- **0.1** UTF-8 panic fix in `update_accumulators` (`chain_executor.rs:6960-6964`). Use `truncate_for_webbing` pattern at `:1553`. **Ship first regardless of anything else.**
- **0.2** Resolve `instruction_map: content_type:` dead config. **Recommendation: implement the missing matcher arm** in `chain_executor.rs:1034-1070`. Brings the existing declaration to life.
- **0.3** Decide fate of `generate_extraction_schema()`. **Recommendation: delete.** It's dead, deleting is safe, can revive from git if wanted.
- **0.4** Fix `chunk_transcript` `--- ` false-trigger on markdown horizontal rules. Tighten the regex.

#### Phase 1 — Recover existing chronological pipeline
- **1.3 (the gate):** Read `src-tauri/src/pyramid/build.rs:684+` (`build_conversation`) end-to-end. Verify it actually works, document any surprises in the plan doc before proceeding.
- **1.4 Decide:** route to `build_conversation` verbatim via `build_runner.rs:237` dispatch fix, OR port forward/reverse/combine into `chains/defaults/question.yaml` as new steps (which can use the already-working `save_as: step_only` and `zip_steps`). Recommendation: spike both for an hour, pick the one that needs fewer surprises.
- **1.5** Ship the dispatch fix. New chain_id `conversation-legacy-chronological` (or similar) routes to the legacy build path.

#### Phase 2 — Real chain binding (per-content-type, schema-supported)
- Add a `content_type` column to `pyramid_chain_assignments` OR a new `pyramid_chain_defaults` table.
- Replace `chain_registry::default_chain_id`'s wildcard with a real resolver: per-slug → per-content-type → fallback.
- Surface a way to set per-content-type defaults: YAML config at `chains/defaults.yaml` (recommended) OR DB-driven settings panel.

#### Phase 3 — Temporal anchors as first-class data
- Add `first_ts`, `last_ts`, `content_hash` columns to `pyramid_chunks`.
- Add `speaker`, `at` as first-class fields on `Topic`, not flattened extras.
- Add `required_topic_fields:` to chain YAML — Rust enforces presence at L0 extract output, knows nothing about temporal semantics. The corrected version of v1's Phase 3b at the right code site.
- Re-ingestion preserves `chunk_index` for unchanged content (match by `content_hash`).

#### Phase 4 — Bootstrap and auto-update
- Switch from per-file `include_str!` to `include_dir!` for the whole `chains/` tree.
- Version-stamped sync that overwrites bundled files without losing user edits.
- Malformed YAML falls back to defaults with a logged warning (don't brick installs).

#### Phase 5 — Documentation
- `docs/chain-development/` tree, written against the now-real architecture. Defer until 1-4 land.

#### Out of scope (called out so they don't get lost)
- Closing `ContentType` enum (its own plan, see §5)
- Transcript parser registry
- Wire publish content_type contract
- MCP server temporal awareness

### 4.2 `docs/plans/recursive-vine-v2.md` (the spec the user brought)

The full spec is at `docs/plans/recursive-vine-v2.md` (you'll write this file from the user's design — see §6 below). Short summary:

**Premise:** there is only one thing — a pyramid. "Vine" is a relative label. A pyramid's sources can be raw files OR other pyramids. The recursive hierarchy (conversation → conversation-vine → me-vine → us-vine → metabrain) is built from one mechanism applied recursively.

**Key mechanism — the evidence escalation ladder:** when a pyramid's source is another pyramid, evidence is gathered in three stages with increasing cost and thoroughness:
- **Stage 1: Search** (fast, non-mutating) — fan-out search across source pyramids
- **Stage 2: Drill** (medium, non-mutating) — drill into promising matches for full content
- **Stage 3: Ask** (expensive, mutating, recursive) — when MISSING verdicts persist, trigger `_ask` on the source pyramid to enrich it permanently. The source pyramid runs its own decomposition → evidence loop → synthesis. Recursion bounded by accuracy threshold and depth limit.

**Hook point:** the existing `targeted_reexamination` / `resolve_files_for_gap` path in the question pipeline already handles MISSING verdicts → re-extract from source files. Add a sibling `resolve_pyramids_for_gap` that does search/drill/ask on source pyramids. Same control flow, same verdict system, different evidence provider.

**Phase 1:** Pyramid Evidence Provider — declare other pyramids as sources, search fan-out, evidence integration, build_id staleness tracking.
**Phase 2:** Gap-to-Ask Escalation — MISSING verdicts trigger `_ask` on source pyramids, configurable depth/accuracy.
**Phase 3:** Domain Vine UX — CLI/UI for creating vines, staleness propagation, delta re-query.
**Phase 4:** Cross-Operator Vines — published pyramids as vine sources, credit flow, access control.

**Sequential content:** the bidirectional distillation (forward → reverse → combine) is the generalized treatment for any ordered content (conversations, books, movies, git history, legislation, lectures). It applies at the bedrock level for sequential sources. The vine layer above doesn't need to know whether its sources used triple-pass — it just queries them. This is why chain-binding-v2 Phase 1 (recover/build the chronological pipeline) is a prerequisite for vine queries against sequential sources.

---

## 5. The interaction points (critical — read this before starting)

These are dependencies that bridge chain-binding-v2 and recursive-vine-v2. The next session needs to understand all of them.

### 5.1 The closed `ContentType` enum problem
- The audit found `ContentType` is a closed Rust enum with exhaustive `match` arms in 5+ files.
- chain-binding-v2 leaves this alone (Phase 2 binds chains to existing content_types via a separate table; doesn't add new types).
- **The vine spec needs new content_types** (`ConvoVine`, `CodeVine`, `MeVine`, etc.) OR all vines are `ContentType::Vine` differentiated only by `chain_id`.
- **Recommendation:** before shipping vine Phase 1, do a small dedicated plan to either (a) move ContentType to a free string with chain_id-based dispatch, or (b) commit to "all vines are `ContentType::Vine`, the differentiation lives in `chain_id`." Option (b) is faster; option (a) is correct long-term. Ship (b) today, plan (a) for next.

### 5.2 `stale_engine` silently no-ops on non-question chains
- `stale_engine.rs:1406-1407` says "All pyramids now use the question chain regardless of content_type. Propagation always follows evidence KEEP links."
- A non-question chain (the conversation-legacy-chronological path, or any vine chain) won't produce KEEP links → propagation silently no-ops.
- **The vine spec section 6 (Staleness Propagation) is the entire upward cascade.** It cannot work with the current `stale_engine`.
- **Action:** add a `supports_staleness` capability flag on chains, OR a chain-specific propagation function dispatched by chain_id. Schedule alongside chain-binding-v2 Phase 2 or as part of vine Phase 1. **Not in either plan currently — must be added.**

### 5.3 Annotations FK CASCADE → SET NULL
- `pyramid_annotations` has `ON DELETE CASCADE`.
- Any chain swap that produces nodes with different IDs silently drops every annotation on the affected nodes.
- chain-binding-v2 Phase 1 introduces chain swaps (the dispatch fix).
- **Action:** migrate annotations FK to SET NULL or add a node-supersession layer that preserves annotations. **Bump from chain-binding-v2's "deferred" list into Phase 1.**

### 5.4 chain-binding-v2 Phase 3 (temporal first-class) is a prerequisite for vine queries against sequential sources
- Without `Topic.speaker` and `Topic.at` as first-class Rust fields, vine evidence escalation can't sort, filter, or rank by temporal data — it can only rely on the LLM remembering JSON fields it serializes through `#[serde(flatten)] extra`.
- vine Phase 1 needs Phase 3 done before it can query a chronological conversation pyramid usefully.

### 5.5 The chain YAML for a vine is itself a chain that needs registry binding
- A vine chain (with `resolve_pyramids_for_gap` semantics) lives in `chains/defaults/*.yaml` and needs to be selectable for the vine content_type.
- chain-binding-v2 Phase 2 (real chain binding) is the mechanism that makes this selection possible.
- vine Phase 1 cannot ship until chain-binding-v2 Phase 2 ships.

### 5.6 The `_ask` endpoint already does cross-pyramid question creation
- vine Phase 2 (gap-to-ask escalation) reuses the existing `_ask` endpoint per the spec.
- chain-binding-v2 doesn't touch `_ask`. Clean separation.
- **But:** `_ask` creates question pyramids, and question pyramids go through `run_decomposed_build`. The recursion will hit chain-binding-v2's improved dispatch — verify the path works after chain-binding-v2 Phase 1 lands.

### 5.7 The `targeted_reexamination` / `resolve_files_for_gap` hook is the vine's hook
- chain-binding-v2 doesn't touch this. Should not.
- vine Phase 1 adds `resolve_pyramids_for_gap` as a sibling.
- The hook point is verified by the audit. Clean.

### 5.8 `pyramid_chunks` temporal columns also help vines
- chain-binding-v2 Phase 3 adds `first_ts`/`last_ts` to chunks.
- vine evidence provider can query chunks directly for temporal range during search ranking, not just the topic-level fields.
- Bonus, not a blocker.

### 5.9 The `instruction_map` mechanism (after Phase 0.2)
- After Phase 0.2 implements the `content_type:` matcher, a primitive form of per-content-type prompt routing exists in production.
- chain-binding-v2 Phase 2 generalizes this with the registry binding.
- vine work doesn't touch `instruction_map` directly but benefits from the cleanup.

---

## 6. Files to create / read / not touch

### 6.1 Files the next session should write
- `docs/plans/recursive-vine-v2.md` — the user's design verbatim (the spec content from the conversation), with a "Dependencies" section added at the top citing the chain-binding-v2 phases as prerequisites and the §5 interaction points.
- Whatever Rust files the implementation phases produce — see each plan for specifics.

### 6.2 Files the next session should READ before starting

**Plans and audits (10 minutes total):**
- `docs/plans/chain-binding-v2.md` — the plan to ship first
- `docs/plans/chain-binding-and-triple-pass.audit.md` — why v1 died, what to avoid
- `docs/conversation-pyramid-testing-state.md` — the test arc that motivated everything
- `docs/handoffs/handoff-2026-04-07-chain-binding-and-vines.md` — this file

**Code that the audit found is critical (15 minutes total):**
- `src-tauri/src/pyramid/build.rs:684+` — the `build_conversation` function. **Phase 1.3's gate.** Read this end-to-end before deciding route-vs-port.
- `src-tauri/src/pyramid/build_runner.rs:199-277` — the dispatch site that bypasses chain assignments today
- `src-tauri/src/pyramid/chain_executor.rs:6960-6964` — the UTF-8 panic site (Phase 0.1)
- `src-tauri/src/pyramid/chain_executor.rs:1034-1070` — the `instruction_map` matcher (Phase 0.2)
- `src-tauri/src/pyramid/chain_executor.rs:1997-2070` — the existing `zip_steps` implementation
- `src-tauri/src/pyramid/execution_plan.rs:370` — `StorageKind::StepOnly` confirmation
- `src-tauri/src/pyramid/extraction_schema.rs:40` — dead `generate_extraction_schema` (Phase 0.3)
- `src-tauri/src/pyramid/ingest.rs:171-282` — Claude-Code-only ingest + chunk_transcript regex
- `src-tauri/src/pyramid/types.rs:31-65, 90-108` — `ContentType` enum + `Topic` struct (the closed-enum problem and the flattened-extras problem)
- `src-tauri/src/pyramid/db.rs:56, 99-108, 228-238, 1116` — `pyramid_slugs` CHECK, `pyramid_chunks` schema, annotations FK, content_type migration pattern
- `src-tauri/src/pyramid/stale_engine.rs:1406-1407` — the question-chain hardcoding
- `src-tauri/src/pyramid/chain_loader.rs:202-296, 322-371` — bootstrap path with the `if !exists { write }` problem and the placeholder stubs
- `src-tauri/src/pyramid/chain_registry.rs:5-17, 97` — assignment table schema, `default_chain_id` wildcard
- `src-tauri/src/pyramid/parity.rs:854` — the validator that the v3 question DSL feeds, currently load-bearing for nothing in production
- `chains/defaults/question.yaml:27-28` — the dead `instruction_map: content_type:` declaration
- `chains/questions/conversation-chronological.yaml` — the v1 design-spec that references nonexistent prompts; either fix or delete
- `chains/prompts/question-conversation/` — the fork from commit `e9c9c7f`, not yet wired

**Files the next session should NOT touch:**
- Anything under `chains/prompts/conversation/` except as part of cleanup decisions in Phase 0.4
- The shipped prompt edits from commit `b3e42dd` (`answer.md` abstain rule, `decompose.md` transcript branch, `extraction_schema.md` temporal directive, `source_extract.md` fallback, `synthesis_prompt.md` chronological framing). These are correct and load-bearing — don't revert them.
- The web surface code (`routes_read.rs`, `render.rs`, `routes_login.rs`, anything under `public_html/`). That work is done and stable.

---

## 7. Loose ends and known issues NOT in either plan

These came up during the session or in the audit but aren't in chain-binding-v2 or recursive-vine-v2. The next session should be aware:

1. **Slug stats are stale on multiple slugs.** Earlier in the session I backfilled stats for ~8 slugs via direct SQL (`UPDATE pyramid_slugs SET node_count = ..., max_depth = ...`). The root cause of `update_slug_stats` not always firing is unknown. The web home now queries live nodes directly so this doesn't break the apex display, but the stats column is still stale on builds where the helper didn't run. Worth investigating once chain-binding-v2 ships.

2. **The `chains/registry.yaml` proposed in v1 was never created.** Don't create it as designed; create whatever Phase 2 of chain-binding-v2 actually settles on.

3. **The `chains/questions/conversation.questionpipeline-v1.yaml.bak` backup file** from commit `a7d8a50` is still in the repo. Decide whether to keep, move to an `_archived/` dir, or delete.

4. **The `chains/prompts/conversation-chronological/` directory** (created in commit `a7d8a50`) contains three drafted prompts (`forward.md`, `reverse.md`, `combine.md`). They are domain-neutral and well-written. They are NOT loaded by anything. Phase 1 of chain-binding-v2 should decide whether they relocate into a real prompts directory (if porting forward/reverse/combine into question.yaml) or get archived (if routing to `build_conversation` verbatim).

5. **The `chains/prompts/question-conversation/` fork** from commit `e9c9c7f` is also not loaded by anything yet. Phase 2 of chain-binding-v2 wires the registry binding. If chain-binding-v2 Phase 2 chooses YAML config at `chains/defaults.yaml` over a DB table, this is the file that points conversation builds at the fork.

6. **The L0 schema generation site is unknown.** The audit found that `generate_extraction_schema()` is dead and `generate_synthesis_prompts()` only handles upper-layer synthesis. **Where does the L0 topic schema actually get set in production?** The plan's Phase 3 hand-waves this with "needs investigation in Phase 3 itself." The next session needs to find this site before implementing `required_topic_fields:` enforcement. Strong guess: it's the JSON schema enforcement in the `extract` primitive in `chain_executor.rs`, but verify.

7. **The Run 4 pyramid (`claudeconvotest4temporallabelingupdate`) is in the DB.** It's the haiku-tested reference. After chain-binding-v2 Phase 1 ships, build a new pyramid from the same `.jsonl` to compare.

8. **The audit auditors annotated some findings to the `agent-wire-node-bigsmart-2` pyramid.** Worth running `node "$CLI" annotations agent-wire-node-bigsmart-2` to see what they wrote. Some of them may be about other parts of the codebase the next session also needs to know.

9. **`build_id` change detection on source pyramids** is referenced in the vine spec section 6. The audit didn't dig into how `build_id` actually surfaces through search/drill responses. Verify before vine Phase 1.

10. **The `conversation-default` chain (commit `a7d8a50`'s backup target)** is marked DEPRECATED in its own file header but still discoverable by `discover_chains`. It can be selected via `pyramid_chain_assignments` and silently overlay an opt-in user. There is no enforcement. Worth a flag check during chain-binding-v2 Phase 2.

11. **The audit found the `--- ` chunk-boundary heuristic is also weak in another way** (auditor C #6): chunks lose speaker context across boundaries. A long monologue split mid-utterance leaves chunk N+1 starting mid-sentence with no `---` header. Phase 0.4's regex tightening doesn't fix this — it only fixes false positives. Real fix is structural metadata, deferred to Phase 3.

12. **The Tauri bundle build has an "Error A public key has been found, but no private key" warning.** It's a `tauri_signing_private_key` env var thing for the auto-updater. Harmless for local dev, but if this session is the first to ship to actual users via auto-update, it needs resolution. Not in either plan.

13. **The `~/Library/Application Support/wire-node/pyramid.db` file is the dev database.** Lots of test slugs. The next session can query it freely but should not delete it without backup — there's real test history in there.

14. **The `bigsmart-2` pyramid is the audit reference.** 210 nodes, code-typed. Use it for any "what's in the codebase" questions during the next session — it's faster than grepping.

15. **The MCP server is at `/Users/adamlevine/AI Project Files/agent-wire-node/mcp-server/dist/cli.js`.** Use it via `node "$CLI" <command>` (the `node` binary lives at `/opt/homebrew/bin/node`, so prefix with `export PATH="/opt/homebrew/bin:$PATH"` if cargo isn't on PATH either).

---

## 8. The shape of the vine plan you'll write

The user has the spec ready (it's the "Recursive Vine Architecture — V2 Design" content from the session transcript). Your job: relocate it to `docs/plans/recursive-vine-v2.md` with the following modifications:

1. **Add a "Dependencies" section at the top** citing:
   - chain-binding-v2 Phases 0-4 must ship first
   - The closed-`ContentType`-enum decision (option (a) free string + chain_id, or option (b) "all vines are `ContentType::Vine`, differentiate by `chain_id`")
   - `stale_engine` capability flag (§5.2)
   - Annotations FK CASCADE → SET NULL migration (§5.3)

2. **In Section 5 (The Recursive Stack)**, flag that `vine.rs:569-586` has hard `match` arms — adding new vine sub-types as content_types is a recompile-touching-N-files change unless §5.1 gets resolved.

3. **In Section 6 (Staleness Propagation)**, flag the `stale_engine` dependency from §5.2.

4. **In Section 8 (Persistence & Identity)**, flag the annotations FK CASCADE issue from §5.3.

5. **In Section 7 (Sequential Content & Triple-Pass Chrono)**, flag that this section depends on chain-binding-v2 Phase 1 (recover the chronological pipeline) and Phase 3 (temporal first-class).

6. **The four build phases** stay as the user wrote them. They are well-scoped.

7. **Add the appendix table** the user provided — it's an honest list of what already exists.

The spec content itself is good. Don't rewrite it. Just relocate and annotate.

---

## 9. Tooling and conventions for the next session

### 9.1 Pyramid CLI (use this, not raw curl)
```
CLI="/Users/adamlevine/AI Project Files/agent-wire-node/mcp-server/dist/cli.js"
SLUG=agent-wire-node-bigsmart-2  # the audit reference

node "$CLI" apex $SLUG                       # System overview
node "$CLI" search $SLUG "query terms"       # Multi-word AND search
node "$CLI" drill $SLUG <node_id>            # Drill into detail
node "$CLI" faq $SLUG                        # Generalized knowledge
node "$CLI" annotations $SLUG [node_id]      # See annotations
node "$CLI" annotate $SLUG <node_id> "..." --question "..." --author "your-name" --type observation
```

Bash + node isn't on the default PATH from cargo's environment. Use:
```
export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$PATH"
```

### 9.2 Tauri rebuild
```
cd "/Users/adamlevine/AI Project Files/agent-wire-node/src-tauri"
export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$PATH"
cargo tauri build 2>&1 | tail -10
```

Bundle lands at `src-tauri/target/release/bundle/macos/Wire Node.app`. The signing-private-key warning is benign locally. Quit the running Wire Node before relaunching the new bundle.

### 9.3 Wire Node DB
```
sqlite3 "/Users/adamlevine/Library/Application Support/wire-node/pyramid.db"
```
Don't delete it. Real test history.

### 9.4 Conventions
- Pillar 37: never prescribe outputs to intelligence. No "at least N", no "between 3 and 7", no "minimum X". Use truth conditions.
- Always include frontend/UX workstreams alongside backend (per Adam's CLAUDE.md).
- For bug fixes, run a second fixer in series instead of a full audit (per Adam's CLAUDE.md).
- All data should use the annotation/FAQ contribution pattern, not separate tables.
- Call the AI partner "Partner" internally, "Dennis" externally (Adam's convention).
- Don't add docstrings, comments, or type annotations to code you didn't change.
- Don't create files unless necessary. Edit existing.
- Commit messages should explain WHY, not just WHAT.
- All commits go through the `Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>` footer.

### 9.5 The session's git state
The current branch is `main`, all recent commits are pushed to `origin/main`. Pull before starting. Recent commits relevant to this work (newest first):
```
e426921 — chain-binding-v2 plan, rough/unaudited
35da1c9 — audit pass invalidates chain-binding plan
26f9b47 — 3b: chain config declares enforced topic fields
27407ea — plan for config-driven chain binding + triple-pass pipeline (the v1 plan, INVALIDATED)
3073ad7 — state of conversation pyramid testing (4 runs)
e9c9c7f — fork question prompts → question-conversation
b3e42dd — abstain on empty evidence + temporal-aware extraction
a7d8a50 — backup conversation v1 + add chronological design-spec
48cd70b — build_folder_map handles single-file sources
844ad25 — stop truncating headlines on save
a3056bc — collapse Topic structure by default
```

---

## 10. The "what done looks like" picture

By end of next session, this should be true:

- [ ] Phase 0 of chain-binding-v2 shipped: forward pass doesn't crash on em-dashes; `instruction_map: content_type:` either lives or dies; `generate_extraction_schema` either lives or dies; chunk_transcript no longer false-triggers on markdown.
- [ ] `build_conversation` audited end-to-end and route-vs-port decision documented in chain-binding-v2.
- [ ] Phase 1 dispatch fix shipped: a conversation pyramid built with the chronological binding actually executes a chronological pipeline.
- [ ] Phase 2 chain binding schema shipped: per-content-type defaults work via config (DB or YAML).
- [ ] Phase 3 temporal first-class shipped: L0 nodes have queryable `speaker` and `at` fields.
- [ ] Phase 4 bootstrap shipped: fresh installs and auto-updates correctly receive all bundled chains.
- [ ] Annotations FK CASCADE → SET NULL migration done (lifted from deferred).
- [ ] `stale_engine` `supports_staleness` flag done (lifted from deferred).
- [ ] Closed-`ContentType`-enum decision made and either implemented or scoped as a fast follow.
- [ ] recursive-vine-v2 plan written from the user's spec with dependencies annotated.
- [ ] recursive-vine-v2 Phase 1 shipped: a test vine queries source pyramids via search and successfully answers questions.
- [ ] recursive-vine-v2 Phase 2 shipped: a vine question that can't be answered by search alone triggers `_ask` recursion.
- [ ] recursive-vine-v2 Phase 3 shipped: operator can create domain vines via CLI, staleness propagates.
- [ ] recursive-vine-v2 Phase 4 shipped: cross-operator vines work with credit flow.
- [ ] A test pyramid built from the same `.jsonl` we used for Runs 1-4, with the new chronological binding, has populated `speaker`/`at` fields, no meta-nodes, and reads chronologically. Haiku eval is sanity-check, not gate.
- [ ] No regressions on existing pyramids (code, document, default conversation, question).

That's a lot for one session. The user said "ship this all today." Trust them; they know the scope.

---

## 11. Final notes from this session's conductor

A few things I want to flag that aren't elsewhere:

- **The audit was the most valuable thing this session produced.** Not the fixes, not the plan rewrites — the audit. Every plan should get one before implementation, especially when the plan author hasn't fully traced the production codepath. The 4-agent pattern (2 informed + 2 discovery, with discovery getting a known-issues list to avoid re-finding Stage 1 bugs) is what surfaced the dead `instruction_map` and the existing `build_conversation` and the UTF-8 panic. None of those were in any single auditor's findings — they were in the convergence.

- **Run the audit on the v2 plan too.** I didn't, because the user explicitly asked for "rough unaudited form." But before any meaningful Rust changes start, the next session should run the same 4-agent audit on chain-binding-v2 to catch whatever I got wrong.

- **The `_ask` endpoint and the `targeted_reexamination` path are the two existing mechanisms the vine spec is built on.** They are real, they work, they were audited. The vine spec is unusually well-grounded for a design doc — the user's appendix table at the bottom is honest about what already exists. Trust it more than you'd trust a typical "here's how the system works" claim.

- **The user is fast.** They will iterate on builds while the next session is implementing. Be ready for the test pyramid output to change between when you run a build and when you read the result. Use slug names that indicate what they test.

- **If you find yourself rebuilding `build_conversation` from scratch instead of recovering it, stop and re-read this handoff.** That's exactly the loop the v1 plan died in.

- **The user values terse responses.** Don't summarize what you just did at the end of every response — the diff is self-explanatory. Lead with the answer, not the reasoning.

- **The user prefers single bundled PRs over many small ones for refactors in this area.** (From a confirmed-feedback memory.) For Phase 0 the small commits are correct because the fixes are independent. For Phase 1 onward, prefer fewer, larger commits.

Good luck. Ship it.

— Partner, 2026-04-07
