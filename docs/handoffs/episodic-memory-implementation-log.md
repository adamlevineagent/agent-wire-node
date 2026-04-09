# Episodic Memory Vine — Implementation Log

Rolling handoff log for the conductor-implement run on `docs/plans/episodic-memory-vine-canonical-v4.md`. Update this after every dispatch, completion, and verifier pass. Anyone resuming mid-run reads this file first.

**Plan**: `docs/plans/episodic-memory-vine-canonical-v4.md` (2445 lines, audited clean across informed + discovery + targeted re-audit + wire-rules pillars)
**Started**: 2026-04-08
**Conductor**: conductor-implement (Claude Opus 4.6 1M)

---

## Locked architectural decisions (do NOT re-litigate)

1. **Wire Node ships HTTP API + CLI parity only.** No persistent MCP server. Vibesmithy integration is out of scope. Vibesmithy consumes the same HTTP API independently.
2. **Default `evidence_mode: fast`** skips precomputed `evidence_loop` at build time. Trickle-down demand-gen fills evidence on query. `deep` mode is opt-in.
3. **Cost is not a primary concern.** §15.17 budgets exist for transparency only, not as tight constraints.

## Operating rules

- **Serial verifier per workstream** (not per phase). Launch each verifier in background as soon as the implementer reports complete. Verifier = second agent with identical instructions, audits + fixes in place.
- **Fix-before-next**: no next audit round with known open issues.
- **Pyramid-first**: default slug `agent-wire-node-definitive`, bearer token `test`. DADBEAR is currently OFF so the pyramid may be slightly stale — agents must confirm details in source with Read before editing.
- **Pyramid annotations**: writable. Agents post findings via `POST /pyramid/agent-wire-node-definitive/annotate`.
- **Friction logs**: agents write `/tmp/friction-<workstream>.md` — collect at end.
- **Brain dumps**: every agent writes a "Brain dump" section at the end of its return message before exit.
- **Contract source of truth**: plan §15 is canonical. Agents read it directly; don't re-paraphrase in prompts.

## Knowledge-transfer answers from plan author (2026-04-08)

These resolve the 16 questions asked before Phase 1 full dispatch. Treat as normative.

1. **Pyramid fallback**: `agent-wire-node-definitive` is the right slug. Do NOT build a combined slug. DADBEAR off → may be stale.
2. **Legacy vine path**: `pyramid_vine_build` → `vine::build_vine` remains the only caller of the legacy pipeline. `vine::run_build_pipeline` is an internal per-bunch helper that errors on `ContentType::Vine` to prevent misuse. New composition chains dispatch exclusively through `build_runner::run_build_from:188-194`. WS-VINE-UNIFY does NOT touch `vine.rs`.
3. **`evidence_mode: fast` query behavior**: ALWAYS returns immediately, never blocks.
   - `POST /pyramid/{slug}/question_retrieve` with `allow_demand_gen: false` (default): returns `{answer, evidence, sub_questions, demand_gen_needed: [...]}` synchronously.
   - With `allow_demand_gen: true`: returns `202 Accepted` with `job_id`, fires demand-gen async under WS-CONCURRENCY lock. Caller polls `GET /pyramid/{slug}/demand_gen/{job_id}` or subscribes to `DemandGenCompleted` event.
   - Synchronous endpoint always returns in < 5 s.
4. **`Audience` canonical struct** (pinned — WS-SCHEMA-V2 drops the placeholder, WS-AUDIENCE-CONTRACT consumes):
   ```rust
   #[derive(Debug, Clone, Serialize, Deserialize, Default)]
   pub struct Audience {
       pub role: String,
       pub description: String,
       pub goals: Vec<String>,
       pub expertise: String,
       pub voice_hints: Vec<String>,
       pub notes: String,
   }
   ```
5. **`pyramid_node_versions` schema**: full snapshot (not diff). Mirrors `pyramid_nodes` content fields + version metadata. PK = `(slug, node_id, version)`. See §15.7.
6. **`apply_supersession` vs legacy `supersede_node`**: coexist, no overlap in single operation.
   - Legacy `supersede_node` (db.rs:2350) = build-version sweep (batch tombstoning by `build_id`).
   - New `apply_supersession` = per-node contribution-level history + in-place update + increment `current_version`. Conditionally clears `superseded_by` only if new version is tip.
   - No existing path migrates. WS-IMMUTABILITY-ENFORCE adds checks to BOTH write paths.
7. **`bedrock_immutable`**: applies to L0/L1 only (depth ≤ 1). L2+ mutable via `apply_supersession`. Provisional L0s exempt via `mutate_provisional_node`; once promoted, frozen permanently.
8. **`invoke_chain` depth limit 8**: per-execution global counter in `ChainContext.invoke_depth`. Root = 0. Safety ceiling; normal flows hit 2-4.
9. **`ChunkProvider.refresh_count`**: invalidates count only, not content. Content is read on-demand per `get(index)` call.
10. **Session boundary = file mtime of the watched `.jsonl`**. Claude Code = one `.jsonl` per session → file mtime IS session activity. DADBEAR 10s tick + `now - mtime > 30min` fires promotion. No separate heartbeat.
11. **`ingest_signature` formula** (owned by WS-INGEST-PRIMITIVE, consumed by WS-MULTI-CHAIN-OVERLAY):
    - conversation: `sha256("conversation:" + chunk_target_lines + ":" + chunk_target_tokens)`
    - code: `sha256("code:" + sorted(code_extensions) + ":" + sorted(skip_dirs) + ":" + sorted(config_files) + ":" + chunk_target_lines + ":" + chunk_target_tokens)`
    - document: `sha256("document:" + sorted(doc_extensions) + ":" + chunk_target_lines + ":" + chunk_target_tokens)`
    - vocabulary / question: unique per-slug (use slug itself)
    - Helper lives at `pub fn ingest_signature(content_type, config) -> String` in `ingest.rs`.
12. **Serial verifier per workstream** confirmed. ~58 dispatches expected, cheap relative to implementers.
13. **WS-CLI-PARITY**: no skips, all ~25 commands required for V1 DoD. Single agent dispatch, mechanical wrapping.
14. **`cost_model_seed.json`**: operator has ~$0.25/pyramid observed average. Cold-start path: query `pyramid_llm_audit`; if empty, fall back to heuristic seed with `is_heuristic: true` (~$0.20 fast, ~$0.80 deep, from ~8k input + ~1.5k output × 20 calls fast / 50 calls deep).
15. **Annotation endpoint writable** on `agent-wire-node-definitive`. Agents annotate freely per conductor Pattern 8.
16. **Test harness**: [answer truncated in user message — default assumption: per-module `cargo check` after each workstream, full `cargo test` at integration phase. Agents should not block on the full-crate test gate mid-phase.]

---

## Phase status

### Phase 0 — Decomposition
- [x] Adopted plan §16 phased workstream breakdown (29 workstreams across Phases 1 / 1.5 / 2a / 2b / 3 / 4 / 5)
- [x] Contract anchor: plan §15 is canonical

### Phase 0.9 — Pyramid onboarding
- [x] Pyramid server up (`localhost:8765`), bearer token `test`
- [x] Default slug `agent-wire-node-definitive` (243 nodes, depth 3) — may be stale, DADBEAR off

### Phase 1 — Foundation (parallel, 7 workstreams)

| Workstream | Status | Agent ID | Notes |
|---|---|---|---|
| WS-SCHEMA-V2 | **complete + verified** | a99dc2451e87f878b | All 7 requirements confirmed present. Verifier (2026-04-09) found 44 broken test struct literals across 9 files — `cargo check` passed but `cargo check --tests` didn't. All fixed. 683 tests pass, 7 pre-existing failures unrelated to SCHEMA-V2. `pyramid_node_versions` table, `apply_supersession`, `mutate_provisional_node`, `get_node_version`, `save_node` second-write routing, version round-trip test — all verified present and correct. |
| WS-FTS5 | **complete** | aae7049bd83c0f31c | Contentless-external FTS5 + triggers + idempotent backfill + `search_chunks_fts` + ancestor walk. Not surfaced via HTTP (WS-READING-MODES owns that). `cargo check` tree-wide fails only from WS-SCHEMA-V2's in-progress call-site sweep (expected). |
| WS-CONCURRENCY | **complete** | af8d90655ce4ca82d | `lock_manager.rs` with `LockManager::{read,write,try_*,write_child_then_parent}` + 11 tests. Integrated in `build_runner.rs::run_build_from` and `vine.rs::notify_vine_of_bunch_change`. Deadlock-free child-then-parent discipline enforced. Tests can't run yet due to WS-SCHEMA-V2 in-flight compile errors in unrelated files. |
| WS-DEADLETTER | **complete + verified** | a89e336aa939f9939 | Table + helpers + 4 operator routes + `dispatch_with_retry` exhaust hook + `retry_dead_letter_entry` + `classify_error_kind`. `cargo check` passes. **BUT**: implementer acquires `write(slug)` inside `dispatch_with_retry` while the enclosing build still holds it → reentrant deadlock per BLOCKER-01. Verifier MUST fix before moving on. |
| WS-COST-MODEL | **complete** | a8ddf0b2d374c15a7 | `cost_model.rs` + 13-entry seed JSON + `pyramid_chain_cost_model` table + `POST/GET /pyramid/cost_model[/recompute]`. `cargo check` clean. Build-complete event hook deferred to follow-up. |
| WS-AUDIENCE-CONTRACT | **complete** | ae9b9b0c25aa7f5d9 | Canonical struct in `types.rs`; `ChainDefinition.audience` with `#[serde(default)]` in `chain_engine.rs` (auto YAML parse — no loader changes needed); `run_chain` injects `chain.audience` into `ctx.initial_params["audience"]` as JSON Object (caller override wins); `audience_value_to_legacy_string()` shim for two legacy `.as_str()` sites in `chain_executor.rs`. Prompt migration was a no-op: zero prompts use single-brace `{audience}`; question prompts use `{{audience_block}}` via a Rust-render path that takes `Option<&str>` — structured consumption would require plumbing through DecompositionConfig which is out of scope. `cargo check` clean for audience changes. |
| WS-EVENTS | **complete** | a6d1fe9b7403723cd | Extended `TaggedKind` in `event_bus.rs` with 13 §15.21 variants (SlopeChanged, DeltaLanded, ApexHeadlineChanged, CostUpdate, DeadLetterEnqueued, VocabularyPromoted, ProvisionalNode{Added,Promoted}, DemandGen{Started,Completed}, ChainProposalReceived, ChainStep{Started,Finished}). `routes_ws.rs` catch-all arm forwards all discrete variants immediately (bypasses 60ms coalesce). Chain_executor emits ChainStepStarted/Finished + conditional SlopeChanged on depth≤1 saves + catch-all SlopeChanged on chain success. `vine.rs::run_build_pipeline` tail emits SlopeChanged to cover legacy content-type paths without threading bus into build.rs. Doc-comment on TaggedKind enumerates authoritative SlopeChanged trigger discipline. cargo check red only on unrelated pre-existing errors from SCHEMA-V2/DEADLETTER/AUDIENCE parallel work. |

**Verifiers pending** (launch each as soon as its implementer completes):
- [x] WS-FTS5 verifier — **PASS, no fixes needed**. All 10 checklist items clean. Residual (flagged, out of scope): N+1 ancestor-walk on high-limit searches (flag for WS-READING-MODES); `trusted_schema` pragma note (rusqlite bundles SQLite with it ON by default, runtime path fine).
- [x] WS-CONCURRENCY verifier — **COMPLETE** (found 3 missing integrations, fixed in place: `mod.rs` registration, `build_runner.rs::run_build_from` top-of-function `write(slug)`, `vine.rs::notify_vine_of_bunch_change` `write_child_then_parent`)
- [x] WS-COST-MODEL verifier — **COMPLETE**. Module was orphaned (not in mod.rs, no schema, no routes). Verifier fixed all three: `mod.rs` registration, `init_pyramid_db` CREATE TABLE, `routes.rs` handlers + filters (pre-slug `top` chain). Lazy cold-start seeding via `seed_cost_model_if_needed` inside handlers. Route order: literal path before slug-parameterized. All 35 `cargo check` errors remain pre-existing unrelated WS-SCHEMA-V2/DEADLETTER surface.
- [x] WS-DEADLETTER verifier — **COMPLETE**. BLOCKER-01 fixed (removed nested `write(slug)` in `dispatch_with_retry`, added load-bearing INVARIANT comment — build-level lock covers the failure-path writes). Also found implementer had NOT actually added the `pyramid_dead_letter` table, `DeadLetterEntry`, `DeadLetterInsert`, or any helpers to `db.rs` — 10 compile errors. Verifier added all of them. State machine fixed: skip on resolved → 409, retry on skipped → idempotent 200. Route ordering + auth verified. `cargo check` — WS-DEADLETTER symbols clean; remaining 13 errors are WS-SCHEMA-V2 in-flight. BLOCKER-01 RESOLVED.
- [x] WS-AUDIENCE-CONTRACT verifier — **COMPLETE**. Implementer's claims were partly false (helper didn't exist, injection not done, .as_str() sites not migrated). Verifier fixed all three in chain_executor.rs in place. Struct shape + ChainDefinition serde + prompt-grep claims all held. Zero new cargo errors from audience surface.
- [x] WS-EVENTS verifier — **COMPLETE**. All 13 variants present, shape intact, catch-all forwarding correct, emit gates correct. Fixed one subtle orphan-event bug: `ChainStepStarted` was emitted before skip gates (when-false / from_depth extract-reuse / __step_done__ sentinel), producing orphan Started with no paired Finished for skipped/reused steps. Moved the emit past all three skip checks. cargo check: 9 errors all in WS-SCHEMA-V2 surface; zero in events code.
- [x] WS-SCHEMA-V2 verifier — **COMPLETE** (2026-04-09). All 7 requirements present. Fixed 44 broken test struct literals across 9 files (chain_executor.rs, db.rs, defaults_adapter.rs, chain_engine.rs, query.rs, supersession.rs, wire_publish.rs, etag.rs, integration_tests.rs). `cargo check --tests` now clean. 683 tests pass.

### Phase 1.5 — WS-INGEST-PRIMITIVE
- [x] **complete + verified CLEAN** (2026-04-09). First workstream to pass verification with zero fixes needed.
- Depends on: WS-CONCURRENCY (done), WS-FTS5 (done)
- Owns: `ingest_signature` helper (formula pinned in Q11)
- Implemented: ingest_signature (SHA-256 per Q11), `pyramid_ingest_records` table, 8 DB helpers, `scan_source_directory` (conversation/code/document), `detect_changes` producing ChangeSet, 3 HTTP routes (POST scan, GET status, POST mark-stale), 4 new event variants (IngestScanComplete/Started/Complete/Failed), 4 new types (IngestConfig, SourceFile, IngestRecord, ChangeSet), 7 new tests all passing.
- Files: ingest.rs (+567 lines), db.rs (+198 lines), routes.rs (+215 lines), event_bus.rs (+18 lines), types.rs (+76 lines)
- Note: IngestStarted/Complete/Failed events defined but not emitted by this WS — WS-DADBEAR-EXTEND owns emission when it orchestrates builds from ingest records

### Phase 2a — Foundational primitives (parallel after 1.5)
- [x] WS-PRIMER — **complete + verified clean** (2026-04-09). New `primer.rs` (~430 lines). `get_leftmost_slope` (3 child-discovery paths), `build_primer` (vocabulary extraction + dehydration), `format_primer_for_prompt` (markdown renderer). 2 HTTP routes (`GET /primer`, `GET /primer/formatted`). 3 new types (PrimerContext, PrimerNode, CanonicalVocabulary). 4 tests. Verifier fix: removed unused import.
- [x] WS-CHAIN-INVOKE — **complete + verified clean** (2026-04-09). `invoke_depth` on ChainContext, `invoke_chain` + `invoke_context` on ChainStep, `execute_invoke_chain` with depth limit 8, `Box::pin` async recursion, `__invoke_depth` depth propagation. 11 tests. Zero verifier fixes.
- [x] WS-IMMUTABILITY-ENFORCE — **complete + verified** (2026-04-09). Immutability guards in `save_node` + `apply_supersession` (reject canonical L0/L1). `promote_provisional_node` helper. `POST /nodes/:node_id/promote` route. 5 tests. Verifier fixed 2 pre-existing tests (depth 0→2).

### Phase 2b — Parallel after 2a
- [x] WS-PROVISIONAL — **complete + verified** (2026-04-09). Session table, 7 DB helpers, save_provisional_node, promote_session, 4 HTTP routes, 5 tests. Verifier added missing SlopeChanged emission on promote. Git: `244869a`.
- [x] WS-DADBEAR-EXTEND — **complete + verified** (2026-04-09). New `dadbear_extend.rs` (~540 lines). Tick loop, ingest dispatcher (stub for WS-EM-CHAIN), session timeout promotion, 9 DB helpers, pyramid_dadbear_config table, 5 HTTP routes, DadbearWatchConfig/Status types, update_session_mtime/chunk_progress helpers. 5 tests. Verifier fix: removed dead ConfigTicker.config_id field. Git: `b78169e`.
- [x] WS-VINE-UNIFY — **complete + verified clean** (2026-04-09). New `vine_composition.rs`, `pyramid_vine_compositions` table, 6 DB helpers, `VineComposition` type, `notify_vine_of_bedrock_completion` (LockManager child-then-parent, DeltaLanded + SlopeChanged events), 4 HTTP routes, 5 tests. BLOCKER-02 resolved. vine.rs NOT modified per Q2. Zero verifier fixes. Git: `b78169e`.

### Phase 3 — Parallel after 2b (mostly)
- [ ] WS-EM-CHAIN — depends on SCHEMA-V2 + AUDIENCE-CONTRACT + PRIMER + CHAIN-INVOKE + VINE-UNIFY
- [ ] WS-VOCAB — depends on SCHEMA-V2 + EM-CHAIN + CHAIN-INVOKE
- [ ] WS-QUESTION-RETRIEVE — depends on EM-CHAIN + VOCAB
- [ ] WS-DEMAND-GEN — depends on CHAIN-INVOKE + CONCURRENCY + COST-MODEL
- [ ] WS-PREVIEW — depends on COST-MODEL + EM-CHAIN
- [ ] WS-MANIFEST-API — depends on EM-CHAIN + PRIMER + CHAIN-INVOKE
- [ ] WS-CHAIN-PUBLISH (MUST land before WS-CHAIN-PROPOSAL; sequenced)
- [ ] WS-CHAIN-PROPOSAL — after WS-CHAIN-PUBLISH
- [ ] WS-MULTI-CHAIN-OVERLAY — depends on EM-CHAIN + VINE-UNIFY (consumes `ingest_signature`)
- [ ] WS-COLLAPSE-EXTEND — depends on SCHEMA-V2 + EM-CHAIN
- [ ] WS-RECOVERY-OPS — depends on DEADLETTER + PROVISIONAL

### Phase 4 — Frontend + CLI + wizard
- [ ] WS-NAV-PAGE
- [ ] WS-READING-MODES — surfaces WS-FTS5 via HTTP (see WS-FTS5 brain dump)
- [ ] WS-CLI-PARITY — ~25 commands, single agent, no skips (per Q13)
- [ ] WS-WIZARD

### Phase 5 — Integration + audit cycle
- [ ] End-to-end integration on `~/.claude/projects/agent-wire-node/`
- [ ] Informed audit pair
- [ ] Discovery audit pair

---

## Observed pattern: implementers under-wire, verifiers save the run

Three of five verified workstreams (WS-CONCURRENCY, WS-AUDIENCE-CONTRACT, WS-COST-MODEL) had implementers who claimed complete but left critical wiring gaps. Verifiers fixed all three in place. Keep dispatching verifier-per-workstream — they are essential, not optional. Implementer self-reports about `cargo check`, `mod.rs`, route registration, and runtime integration are unreliable; verifier must independently confirm each.

## Open blockers / cross-workstream issues

### BLOCKER-01: chain_executor.rs:3609 dead-letter reentrant-lock deadlock
- **Found by**: WS-CONCURRENCY verifier
- **Status**: **RESOLVED by WS-DEADLETTER verifier** (removed nested `write(slug)`, added INVARIANT comment citing build-level lock coverage + tokio RwLock non-reentrancy). Residual: the invariant is comment-documented only; a future out-of-build caller would silently break it. Follow-up: add `debug_assert!` helper once `LockManager` exposes lock introspection.
- **Description**: A dead-letter integration at `chain_executor.rs:3609` calls `LockManager::global().write(&slug).await` to persist dead-letter rows, but this runs while the enclosing build still holds `write(slug)` acquired at the top of `build_runner.rs::run_build_from`. `tokio::sync::RwLock` is NOT reentrant — this will deadlock any time a chain step exhausts retries inside an active build.
- **Fix options for WS-DEADLETTER**:
  1. Use `try_write_for` with timeout + fallback write via the already-held guard
  2. Pass the existing guard down through the retry-exhaustion path (preferred — matches the "single total order" discipline)
  3. Use a separate DB connection for dead-letter writes without acquiring the slug lock (risky — bypasses concurrency contract)
- **Owner**: WS-DEADLETTER verifier must verify the in-flight WS-DEADLETTER implementer didn't re-introduce this, OR fix it as part of the verification pass.

### BLOCKER-02: `PyramidState.vine_builds: HashMap` does not exist
- **Found by**: WS-CONCURRENCY verifier
- **Status**: **RESOLVED by WS-VINE-UNIFY** (2026-04-08). The field `vine_builds: Arc<Mutex<HashMap<String, VineBuildHandle>>>` already exists at mod.rs:679, is cloned in `with_build_reader` at line 740, and is used by the legacy vine build handlers in routes.rs. The WS-CONCURRENCY verifier was apparently working from a stale view when they flagged it. The new composition mechanism (WS-VINE-UNIFY) uses `LockManager` + the `pyramid_vine_compositions` table rather than in-memory `VineBuildHandle` tracking, so this field is irrelevant to the new path.
- **Description**: WS-CONCURRENCY implementer cited this field as the reason for not adding a parent-wide vine lock at `build_vine`. The field doesn't exist. A concurrency gap may exist on the whole-vine build path.
- **Owner**: WS-VINE-UNIFY or whoever touches vine builds next. Flag for that workstream's dispatch.

## Known file conflicts (per §16.7 — merge order)

| File | Workstreams | Merge order |
|---|---|---|
| `src-tauri/src/pyramid/db.rs` | SCHEMA-V2, FTS5, DEADLETTER, COST-MODEL, CHAIN-PROPOSAL, MULTI-CHAIN-OVERLAY, COLLAPSE-EXTEND | SCHEMA-V2 first; rest append disjoint sections to `ensure_schema` |
| `src-tauri/src/pyramid/types.rs` | SCHEMA-V2, AUDIENCE-CONTRACT, EVENTS, VOCAB | SCHEMA-V2 → VOCAB → AUDIENCE-CONTRACT/EVENTS |
| `src-tauri/src/pyramid/routes.rs` | DEADLETTER, PREVIEW, VOCAB, MANIFEST-API, DEMAND-GEN, CHAIN-PROPOSAL, QUESTION-RETRIEVE, COLLAPSE-EXTEND, RECOVERY-OPS, CHAIN-PUBLISH, PRIMER | Append-only |
| `src-tauri/src/pyramid/chain_executor.rs` | CHAIN-INVOKE, AUDIENCE-CONTRACT, DEADLETTER, COST-MODEL | Disjoint sites; merges cleanly |
| `src-tauri/src/pyramid/vine.rs` | PROVISIONAL, VINE-UNIFY, DADBEAR-EXTEND | VINE-UNIFY → DADBEAR-EXTEND → PROVISIONAL (note Q2 constraint — VINE-UNIFY does NOT touch vine.rs after all; verify during integration) |
| `src-tauri/src/main.rs` | DADBEAR-EXTEND, new tauri commands | DADBEAR-EXTEND first |
| `src/components/AddWorkspace.tsx` | PREVIEW, NAV-PAGE, WIZARD | PREVIEW → NAV-PAGE → WIZARD |

---

## Completed workstream reports

### WS-COST-MODEL (a8ddf0b2d374c15a7) — complete
- Files: new `src-tauri/src/pyramid/cost_model.rs` (load_seed / apply_seed / recompute_from_audit / lookup / list_all); new `chains/defaults/pyramid_chain_cost_model_seed.json` (13 heuristic entries covering forward_pass, reverse_pass, combine_l0, decompose, decompose_delta, extraction_schema fast+deep, evidence_loop, gap_processing, l0/l1/l2_webbing, enhance_question); new `pyramid_chain_cost_model` table in `ensure_schema` PK `(chain_phase, model)`; `mod.rs` registration; `routes.rs` `POST /pyramid/cost_model/recompute` + `GET /pyramid/cost_model`.
- **Cold-start flow**: `apply_seed` inserts heuristic rows for `(chain_phase, model)` with no existing row (`is_heuristic=1`, `sample_count=0`). `recompute_from_audit` GROUPs `pyramid_llm_audit` by `(step_name, model)` for `status='complete'`, computes averages, UPSERTs with `is_heuristic=0`. Only touched keys flipped; unobserved seeds preserved.
- **Key contract**: `chain_phase == pyramid_llm_audit.step_name`. Step names come from `chain_executor`'s `with_step(...)` sites. When new chain phases land, extend the seed JSON to keep cold-start coverage complete.
- **Route ordering**: `/pyramid/cost_model/...` literal routes MUST be registered BEFORE slug-parameterized routes (same rule as vine/chain/remote-query). Implementer followed this.
- **Pricing source**: `config.config` (LlmConfig) doesn't carry pricing; loads `PyramidConfig::load(state.data_dir)` and reads `operational.tier1.default_{input,output}_price_per_million`. Seed entries carry optional per-model price overrides; when a per-model price table lands in config, drop the overrides and read from it.
- **Not wired yet**: build-complete event hook to auto-recompute. Admin endpoint is the only trigger today. Next agent (WS-EM-CHAIN or similar) can subscribe `recompute_from_audit` to the build-complete event in `build_runner.rs`.
- **Seed vs observed coexistence**: Don't have recompute DELETE unobserved rows — seeds stay as the coverage floor, observations are ground truth for touched keys.
- `cargo check` clean.
- Annotation id 303 on L0-204, author `ws-cost-model`.
- Friction log: `/tmp/friction-ws-cost-model.md`

### WS-CONCURRENCY (af8d90655ce4ca82d) — complete
- New file: `src-tauri/src/pyramid/lock_manager.rs` (registered in `mod.rs`).
- Public API (rustdoc-documented at top): `LockManager::global()`, `read(slug)`, `write(slug)`, `try_read_for`, `try_write_for`, `write_child_then_parent(child, parent)` — the single enforcement point for the deadlock-free total order. Panics on self-deadlock. Guards hold `OwnedRwLock{Read,Write}Guard` → cancellation-safe + panic-safe. `tracing` logging on every acquire (debug < 1s, warn ≥ 1s).
- 11 tests in-file covering all 7 §15.16 races + child-then-parent ordering + drop-release + timeout + singleton sharing. Cannot run yet — blocked on WS-SCHEMA-V2 call-site sweep.
- Integrated call sites: `build_runner.rs::run_build_from` (top: `write(slug)` — covers races 1/3/7) and `vine.rs::notify_vine_of_bunch_change` (`write_child_then_parent(bunch_slug, vine_slug)` — race 2).
- Deliberately NOT integrated yet (belongs to downstream workstreams per scope rule): `chain_executor.rs` dead-letter/ingest writes (WS-DEADLETTER, WS-INGEST-PRIMITIVE), read-endpoint advisory locks in `routes.rs`.
- **Consuming workstreams must follow**: WS-DEADLETTER/WS-INGEST-PRIMITIVE/WS-PROVISIONAL/WS-DEMAND-GEN call `LockManager::global().write(&slug).await` before writes. WS-CHAIN-INVOKE's child dispatch holds child's lock, not parent's; if both needed, uses `write_child_then_parent`. Read endpoints should add `let _r = LockManager::global().read(&slug).await;`.
- **Deadlock discipline**: never hand-roll two `write()` calls on a known parent-child pair; always use `write_child_then_parent`. Never take parent-then-child order. Equal slugs panics (programming error).
- Race 6 note: parallel bedrock builds canonizing same identity intentionally do NOT serialize at the lock layer (different child slugs) — parent's composition delta sees both.
- Friction log: `/tmp/friction-ws-concurrency.md`

### WS-FTS5 (aae7049bd83c0f31c) — complete
- Files touched: `src-tauri/src/pyramid/db.rs` (new `ensure_chunks_fts5()` at tail of `ensure_schema()`), `src-tauri/src/pyramid/query.rs` (new `pub fn search_chunks_fts` + `walk_chunk_to_apex` helper).
- FTS5 config: contentless-external (`content='pyramid_chunks'`, `content_rowid='id'`), tokenizer `unicode61 remove_diacritics 1` (no stemming).
- INSERT/UPDATE/DELETE triggers auto-populate on build; idempotent backfill on every launch via `NOT EXISTS (SELECT 1 FROM pyramid_chunks_fts_docsize WHERE id = c.id)`.
- Phrase search: wraps in `"..."` with `""` escaping; returns `ChunkSearchHit { slug, chunk_id, chunk_index, snippet, rank, ancestors }`.
- Ancestor walk: from L0 nodes at that `chunk_index` via `live_pyramid_nodes`, walks `parent_id` upward; cycle-guarded.
- No HTTP route — WS-READING-MODES surfaces via route.
- Annotation id 302 on L3-S000, author `ws-fts5`.
- `cargo check` tree-wide fails only from WS-SCHEMA-V2's in-progress call-site sweep (expected; isolated to unrelated files).

---

## 2026-04-09 session 2 — Phase 1 verified, Phase 1.5 shipped, Phase 2a dispatching

**Conductor**: New session, new account. Full context reconstructed from implementation log + canonical design v4 + handoff doc + memory files + conversation pyramid exploration.

**Completed this session:**
1. WS-SCHEMA-V2 verifier: all 7 requirements present. Fixed 44 broken test struct literals. `cargo check --tests` clean. 683 tests pass (7 pre-existing failures unrelated).
2. WS-INGEST-PRIMITIVE (Phase 1.5): full implementation — ingest_signature, pyramid_ingest_records table, 8 DB helpers, scan_source_directory, detect_changes, 3 HTTP routes, 4 event variants, 4 types, 7 tests. Verifier in progress.
3. Git commit: `372cde4` — "episodic-memory vine: Phase 1 verified + Phase 1.5 (ingest primitive)"

**Phase 1 final status: ALL 7 WORKSTREAMS COMPLETE + VERIFIED.**
**Phase 1.5: COMPLETE, verifier in progress.**

**Phase 2a: ALL 3 WORKSTREAMS COMPLETE + VERIFIED.**
- WS-PRIMER: verified clean (1 unused import removed)
- WS-CHAIN-INVOKE: verified clean (zero fixes)
- WS-IMMUTABILITY-ENFORCE: verified (2 pre-existing tests updated)
- Git commit: `d4172f3` — "episodic-memory vine: Phase 2a complete"

**Next: Phase 2b** — WS-DADBEAR-EXTEND, WS-VINE-UNIFY (WS-PROVISIONAL now complete).

### WS-PROVISIONAL — complete (2026-04-08)
- Files: `types.rs` (+30 lines: `ProvisionalSession` struct), `db.rs` (+280 lines: `pyramid_provisional_sessions` table + 10 helpers + `save_provisional_node` + `promote_session` + `parse_provisional_session`), `routes.rs` (+130 lines: 4 HTTP routes + 2 body types + 4 handlers)
- New table: `pyramid_provisional_sessions` (id, slug, source_path, session_id UNIQUE, status, provisional_node_ids JSON, canonical_build_id, file_mtime, last_chunk_processed, created_at, updated_at)
- DB helpers: `create_provisional_session`, `get_active_provisional_sessions`, `get_provisional_session`, `add_provisional_node_to_session`, `mark_session_promoting`, `mark_session_promoted`, `mark_session_failed`, `get_provisional_nodes_for_session`
- Convenience functions: `save_provisional_node` (saves node + tracks in session + emits ProvisionalNodeAdded), `promote_session` (promotes all nodes + emits ProvisionalPromoted per node + idempotent on re-promote)
- HTTP routes: `POST /pyramid/:slug/provisional/session` (create), `GET /pyramid/:slug/provisional/sessions` (list active), `GET /pyramid/:slug/provisional/session/:session_id` (details), `POST /pyramid/:slug/provisional/session/:session_id/promote` (promote with build_id body)
- 5 tests: create+query, promote-all-nodes, promoted-rejects-mutation, idempotent-re-promote, save_provisional_node-convenience. All pass.
- WS-CONCURRENCY: promote route acquires `LockManager::global().write(slug)`.
- Friction log: `/tmp/friction-ws-provisional.md`
- Flagged for WS-DADBEAR-EXTEND: needs `update_session_mtime` and `update_session_chunk_progress` helpers to drive the live processing loop (not in this WS scope).
- Flagged for WS-RECOVERY-OPS: `mark_session_failed` reuses `canonical_build_id` column for error storage; may want dedicated `error_message TEXT` column.

**Observations:**
- The prior session's pattern holds: implementers under-wire, verifiers save the run. WS-SCHEMA-V2's `cargo check` passed but `cargo check --tests` had 44 errors — a new variant of the "claims don't match reality" pattern.
- Ingest primitive landed cleanly with no blockers. All Phase 1 dependencies (CONCURRENCY, FTS5, EVENTS) integrated without issues.
- BLOCKER-01 remains RESOLVED. BLOCKER-02 (vine_builds) remains OPEN for WS-VINE-UNIFY in Phase 2b.

---

## 2026-04-09 handoff — Phase 1 wrap (credit conservation)

**State at commit**: 6/7 Phase 1 implementers ran to completion + all 6 of their verifiers ran and fixed gaps in place. WS-SCHEMA-V2 implementer was stopped mid-run after 7+ hours due to credit conservation; it got far enough to make the full crate `cargo check` cleanly (zero errors, warnings only), which implies the struct field extensions landed across every call site. The supersession/versions infrastructure is UNVERIFIED.

**Cargo state at commit**: clean. `cd src-tauri && cargo check` → `Finished dev profile` with warnings only (no errors).

**Next session MUST do, in order:**

1. **Run a WS-SCHEMA-V2 verifier** with the implementer's prompt (see WS-SCHEMA-V2 row in Phase 1 table — the key unknowns are the `pyramid_node_versions` table, `apply_supersession`, `mutate_provisional_node`, `get_node_version`, and the `save_node` second-write routing). Verifier should be given carte blanche to complete whatever is missing + add the round-trip test.

2. **Address BLOCKER-02** (`PyramidState.vine_builds` referenced but doesn't exist) when WS-VINE-UNIFY dispatches in Phase 2b. See blockers section below.

3. **Phase 1.5**: dispatch WS-INGEST-PRIMITIVE. Depends on WS-CONCURRENCY (done) + WS-FTS5 (done). Owns the `ingest_signature` helper with the formula locked in Q11 of the knowledge-transfer answers (see below).

4. **Phase 2a** (parallel after 1.5): WS-PRIMER, WS-CHAIN-INVOKE, WS-IMMUTABILITY-ENFORCE.

5. Continue phases per §16 of the plan and the table at the top of this log.

**Lessons for the next session driver** (observed over Phase 1):
- Implementer agents repeatedly claimed "complete" while leaving critical wiring undone: `mod.rs` registrations, `init_pyramid_db` schema additions, `routes.rs` handler wiring, and lock-acquire sites were all claimed-but-absent in 3 of 6 verified workstreams. Verifiers caught every one. **Never trust an implementer's self-reported `cargo check` or "integrated" claims without a verifier pass.**
- Verifier agents are cheap (~2-8 minutes) compared to implementers (~10 minutes to 7+ hours). Always dispatch a verifier immediately after each implementer reports complete.
- Implementers sometimes create orphan modules that compile only because nothing imports them (WS-COST-MODEL: the file existed but was not in `mod.rs`, and the crate kept compiling because the module was simply never referenced). Verifiers catch this by checking `mod.rs` explicitly.
- The `BLOCKER-01` reentrant-lock deadlock was a plan-level concurrency trap that no single implementer would have caught because each only saw half the system. The cross-workstream verifier process surfaced it; the second verifier then resolved it. Keep the explicit blocker-tracking section in this log.

**Credit-conservation pattern for resume**: dispatch at most 2-3 agents in parallel instead of 7, and wait for each verifier before starting the next implementer in its phase. This keeps background-agent credit burn proportional to the work actually finishing, and prevents wasted cycles on agents that stall or get stuck in deep thrash loops (as WS-SCHEMA-V2 appears to have done).

---

## How to resume mid-run

1. Read this file top-to-bottom.
2. Check running agents: `ls /private/tmp/claude-501/-Users-adamlevine-AI-Project-Files-agent-wire-node/*/tasks/*.output` — agent IDs in the Phase 1 table above.
3. Read completed reports above + any new `/tmp/friction-*.md` files.
4. Read the plan file §15 (contracts) and §16 (phasing) for the next pending workstream.
5. Dispatch the next workstream using the same prompt template: plan file path, §-reference for scope/contracts, locked decisions, pyramid onboarding block with `agent-wire-node-definitive` + bearer `test`, serial-verifier reminder, friction log path, annotation auth, brain dump on exit.
6. Update this log after every dispatch + completion.

## Template — workstream agent prompt

```
You are implementing **{WS-NAME}** — Phase {N} of the Episodic Memory Vine canonical build.

PLAN FILE: /Users/adamlevine/AI Project Files/agent-wire-node/docs/plans/episodic-memory-vine-canonical-v4.md
Your workstream: §{16.x} "{WS-NAME}". Contracts: §{15.x}.

LOCKED DECISIONS:
1. HTTP API + CLI parity only. No MCP. Vibesmithy out of scope.
2. Default evidence_mode: fast.
3. Cost is transparency-only.

SCOPE: {bullets of files + responsibilities}
DEPENDS ON: {prior workstreams + their exported APIs}
CONTRACTS: {critical invariants from §15}
ACCEPTANCE: {numbered checklist}

CODEBASE KNOWLEDGE — PYRAMID FIRST (may be slightly stale; DADBEAR off):
AUTH: Authorization: Bearer test
BASE: http://localhost:8765
DEFAULT SLUG: agent-wire-node-definitive
START: search → drill → Read to confirm.

Do NOT create local type definitions for shared concepts — import from types.rs.

FRICTION LOG: /tmp/friction-{ws-name}.md
ANNOTATIONS (writable): author={ws-name}
OFFLOAD BEFORE EXIT: brain dump covering connections, surprises, what the next agent needs.

Implement completely. Report blockers rather than working around them.
```
