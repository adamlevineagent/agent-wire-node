# Pyramid Folders + Model Routing — Friction Log

**Plan:** `docs/plans/pyramid-folders-model-routing-full-pipeline-observability.md`
**Implementation log:** `docs/plans/pyramid-folders-model-routing-implementation-log.md`

---

## Purpose

Running log of friction points encountered during implementation of this 17-phase initiative. Keep this updated live so that if we need to hard-reset a phase (or the whole cycle), the surprises, dead ends, and lessons are all in one place. This is scoped to THIS plan — cross-session friction lives in the `project_friction_log` memory.

Each entry should have:
- **Date**
- **Phase / workstream**
- **What hit friction** (2-3 sentences)
- **Root cause** (if known)
- **What we did about it** (correction, deviation protocol, workaround, etc.)
- **Lesson for future phases** (if any)

---

## Entries

### 2026-04-09 — Planning: original handoff attributed Pipeline A symptoms to Pipeline B

**Phase / workstream:** Phase 1 (pre-implementation review)

**What hit friction:** The original handoff's Phase 1 spec attributed three symptoms (duplicate WAL entries in `pyramid_pending_mutations`, stacked stale checks, 200→528 L0 blowup) to `dadbear_extend.rs` tick re-entrancy. An implementer review against the current tree caught that those symptoms live in a different pipeline (`stale_helpers_upper.rs::execute_supersession`, Phase 2's target), and that `dadbear_extend.rs`'s dispatch was stubbed at lines 401-408 awaiting WS-EM-CHAIN. Phase 1 couldn't fix the symptoms it claimed to fix.

**Root cause:** The original plan was written from pyramid drills and grep output, not from full reads of the functions being changed. The two-pipeline architecture ("A = maintenance, B = creation/extension") was not recognized as two-pipeline until full-file reads happened.

**What we did about it:** Planner issued addendum-01 documenting the two pipelines explicitly, expanding Phase 0 into 0a (clippy) + 0b (finish Pipeline B), correcting Phase 1's symptom attribution, adding a Phase 2 scope boundary (only `stale_helpers_upper.rs::execute_supersession` is rewritten, not the two wholesale-rebuild callers), and noting Phase 17's dependency on 0b. Specs and master plan updated in place (commit `adc664b` / `ce7b62b`).

**Lesson for future phases:** Read every function you plan to change in full before planning the change. Pyramid drills and grep output are fine for navigation, not for architectural decisions. The `feedback_read_canonical_in_full` memory has been updated to include codebase reads, not just canonical spec reads.

---

### 2026-04-09 — Phase 0b wanderer caught a chunk-collision bug on second dispatch

**Phase / workstream:** Phase 0b (wanderer pass on `fire_ingest_chain`)

**What hit friction:** The implementer and verifier both signed off on `fire_ingest_chain` with tests that only exercised the FIRST dispatch. The wanderer caught that `ingest_conversation` always inserts chunks with `chunk_index` starting at 0, and `pyramid_chunks` has a `UNIQUE(slug, chunk_index)` constraint (`db.rs:107`). On the SECOND dispatch for the same slug (e.g., the next file drop after the initial build populated chunks), the chunking step would fail with `UNIQUE constraint failed: pyramid_chunks.slug, pyramid_chunks.chunk_index`. The ingest record would be marked `failed` and the build would never fire. Pipeline B was effectively broken end-to-end on any slug that already had chunks from a prior build.

**Root cause:** The punch-list verification checked lock ordering, variant coverage, error propagation, and the `chain_executor.rs:3804` zero-chunks guard, but the success path of the existing test (`test_fire_ingest_chain_chunks_conversation_before_dispatch`) only calls `fire_ingest_chain` once. The equivalent wizard path at `routes.rs:3431` does an explicit `db::clear_chunks(&conn, slug)?` before re-ingesting for exactly this reason ("repeated ingest calls append duplicate copies of the same source files") — but that pattern did not get copied into `fire_ingest_chain`. The six-spec punch list had no "idempotency under re-dispatch" check, and the wizard's defensive clear wasn't mentioned in the phase-0b prompt or addendum.

**What we did about it:** Wanderer added `test_fire_ingest_chain_second_dispatch_collision_repro` which calls `fire_ingest_chain` twice in a row on the same slug+file, asserts the second call does not surface a `UNIQUE` constraint error. That test failed against the committed implementer code, confirming the bug. Fix: added `db::clear_chunks(&conn, slug_owned)?` inside the chunking `spawn_blocking` block, before the `for path_str in &paths_owned` loop, mirroring `routes.rs:3431`. All 11 dadbear_extend tests now pass. Committed on branch `phase-0b-pipeline-b-dispatch`.

**Lesson for future phases:** (1) When the wizard/legacy path has a defensive operation before re-ingest (clear_chunks, delete_existing, truncate, etc.), the equivalent DADBEAR Pipeline B path needs the same operation. The "canonical build dispatch block" at `main.rs:3566-3730` that Phase 0b was patterned after is a FIRST-time build code path — it does not exercise the second-dispatch lifecycle that Pipeline B specifically fires. (2) Wanderer-style "does this work end-to-end, including the second and third time" testing catches things that punch-list verification misses. (3) For any future Pipeline B work, the test skeleton should include N>=2 sequential dispatches, not just one. Schema UNIQUE constraints across `chunk_index`, `batch_id`, and similar file-lifecycle keys are the first thing to check.

---

### 2026-04-10 — Pre-existing: release-mode chain bootstrap gap (conversation-episodic not embedded)

**Phase / workstream:** Phase 0b wanderer flagged during end-to-end trace (pre-existing, NOT a Phase 0b regression)

**What hit friction:** `chain_loader::ensure_default_chains` (~lines 241-247) embeds only 5 fallback chain YAML files via `include_str!`: `conversation.yaml` (placeholder with `id: conversation-default`), `code.yaml`, `document.yaml`, `question.yaml`, `extract-only.yaml`. The bootstrap path is taken ONLY when `env!("CARGO_MANIFEST_DIR").join("../chains")` does not exist at runtime. Since `CARGO_MANIFEST_DIR` is resolved at compile time, the path only exists on the build machine's filesystem. The `default_chain_id_for_mode` now routes conversations to `conversation-episodic`, which is NOT in the embedded list and has no embedded fallback. `tauri.conf.json` `bundle` section has no `resources` array, so chains aren't packed into the `.app`.

**Impact:** If this app is ever shipped to a user whose filesystem doesn't have `/Users/adamlevine/AI Project Files/agent-wire-node/chains`, conversation (and code and doc) builds will fail with "chain not found in chains directory". For Adam's personal build-and-run workflow this is fine; for any distribution (dogfood, alpha, etc.) it's a ticking bomb.

**Status:** Not blocking Phase 0b (Adam's dev workflow works because the chains dir exists on disk). Needs a dedicated phase or workstream before any distribution milestone. Candidate approach: embed the full `chains/**/*.yaml` tree via `rust-embed` or a build script, OR add a Tauri `resources` entry to `tauri.conf.json` so the chains directory is packaged alongside the app.

**Lesson:** Any content-type routing change that adds a new chain ID must verify the chain is reachable in both dev-build and distribution scenarios.

---

### 2026-04-10 — Pre-existing: DADBEAR config CHECK constraint excludes `vine` but `main.rs:3249` tries to create one

**Phase / workstream:** Phase 0b wanderer flagged during end-to-end trace (pre-existing, NOT a Phase 0b regression)

**What hit friction:** The `pyramid_dadbear_config` CHECK constraint at `db.rs:1085` allows only `('code', 'conversation', 'document')` for `content_type`. But `main.rs:3249` does `matches!(content_type, ContentType::Conversation | ContentType::Vine)` and attempts to save a `pyramid_dadbear_config` row with `content_type = "vine"`. Every vine-slug auto-DADBEAR attempt fails the CHECK silently (logged as a warning, not propagated). `fire_ingest_chain`'s `ContentType::Vine | ContentType::Question` guard arm would never actually be reachable in production because the config write upstream fails first.

**Impact:** No user-visible effect today because vine DADBEAR configs never get created. Future phases that want vine-level DADBEAR (e.g., Phase 17 recursive folder ingestion where a folder = a vine) need to either widen the CHECK constraint or change the main.rs:3249 match.

**Status:** Not blocking Phase 0b (vine DADBEAR was never functional). Fix when Phase 17 gets to vine folder ingestion — that phase will need to widen the CHECK anyway.

---

### 2026-04-10 — Latent in Phase 0b: multi-file batch chunk collision when `batch_size > 1`

**Phase / workstream:** Phase 0b wanderer flagged (NEW code path introduced by Phase 0b's claim-once batch dispatch)

**What hit friction:** Phase 0b's `fire_ingest_chain` clears chunks ONCE before iterating over claimed source files. For `batch_size = 1` (the default in `main.rs:3269`) this is correct — one file per dispatch, one `ingest_conversation` call, no collision. For `batch_size > 1`, the for-loop calls `ingest_conversation` N times; each call starts chunk_index at 0; the second file collides with the first file on the `UNIQUE(slug, chunk_index)` constraint. `ingest_conversation` does not take a chunk_offset parameter the way `ingest_code` effectively does.

**Impact:** Latent until a user manually sets `batch_size > 1` in their DADBEAR config. Default path is safe. The wanderer noted this exists in the existing wizard path too (`routes.rs:3435` / `main.rs:3901`) so it's not exclusively a Phase 0b regression — but Phase 0b's claim-once batch dispatch is the first code path that CAN exercise it via the batch_size config.

**Status:** Not blocking Phase 0b ship. Proper fix requires either:
- (a) Extending `ingest_conversation` to accept a `chunk_offset: i64` parameter and computing it from `db::count_chunks(conn, slug)` before each call, mirroring `ingest_code`'s implicit offset pattern
- (b) Keeping chunks cleared once and only allowing `batch_size = 1` for Pipeline B with a hard error otherwise
- (c) Refactoring `ingest_conversation` and `ingest_code` to share a common chunk-offset-aware primitive

Phase 17 (folder ingestion) is the right phase to resolve this because it introduces multi-file ingestion as a first-class concern. Until then, the default `batch_size = 1` keeps it dormant.

---

### 2026-04-10 — Observation: `fire_ingest_chain` writer drain task is dead code on the conversation path

**Phase / workstream:** Phase 0b wanderer flagged during end-to-end trace

**What hit friction:** `fire_ingest_chain` (dadbear_extend.rs:641-695) spawns a full writer drain task covering every `WriteOp` variant. The implementer mirrored the canonical block at `main.rs:3585-3631` exactly. But that canonical block feeds the **legacy build path** — the conversation path goes through `run_build_from` → `run_decomposed_build` → `execute_chain_from`, and `execute_chain_from` spawns its OWN internal write drain via `spawn_write_drain(state.writer.clone())` at `chain_executor.rs:3949`. Every `WriteOp` actually produced by the chain goes through the internal drain; the drain task in `fire_ingest_chain` sits idle until `write_tx` is dropped.

**Impact:** No defect — all writes still happen correctly via the internal drain. But ~50 lines of well-commented code in `fire_ingest_chain` is dead on the conversation path. On a future legacy-build code path (if one ever routes through `fire_ingest_chain`), the drain would become live again. Not worth tearing out pre-ship.

**Status:** Informational. Not blocking. Could be cleaned up in a future refactoring phase if Pipeline B remains chain-only. Documented here so future readers don't chase the drain looking for why it appears unused.

---

### 2026-04-10 — Phase 1 wanderer: in-flight flag fires on a race that doesn't exist in the current code structure

**Phase / workstream:** Phase 1 (wanderer pass on `phase-1-dadbear-inflight-lock`)

**What hit friction:** The Phase 1 spec (`evidence-triage-and-dadbear.md` Part 1, and the addendum-01 "symptom attribution corrected" section) claims the `HashMap<i64, Arc<AtomicBool>>` in-flight flag guards against "the next 1-second base tick starting a concurrent dispatch for the same config" while a slow chain is running. That race is structurally impossible in the current tick loop shape. The loop is a single `tokio::spawn` around `async move { loop { sleep(1s); for cfg in cfgs { run_tick_for_config(...).await; } } }`. When the `.await` is pending — because `fire_ingest_chain` → `run_build_from` is in its multi-minute work — the spawned future is suspended. Tokio does not re-enter a spawned future while it is suspended at an await. The outer loop does not advance. There is no "next tick" until `run_tick_for_config` returns, at which point `_guard` drops and the flag is already cleared. The in-flight skip branch (`dadbear_extend.rs:170-176`) is never reachable from the tick loop's own iteration.

The other (only) production caller of `run_tick_for_config` is `trigger_for_slug` (called via the HTTP POST `/pyramid/:slug/dadbear/trigger` route and its MCP equivalent). That caller **does not check the flag** — `in_flight` is a local variable inside `start_dadbear_extend_loop`'s `tokio::spawn` closure and is inaccessible from the HTTP handler. So a concurrent HTTP trigger fired during a slow tick is the ONLY race that could in principle produce two concurrent `run_tick_for_config` calls for the same config, and the flag does not cover it either.

Net: the flag is a no-op in the current code. It fires on nothing.

**Root cause:** The spec was written from a mental model of the tick loop that treats "every 1-second base tick" as an independent schedulable unit rather than as the next iteration of a single spawned future. The implementer's verification test (`test_in_flight_guard_skip_and_panic_safety`) exercises the `HashMap<i64, Arc<AtomicBool>>` primitive in isolation (set flag, drop guard, catch panic) but does not instantiate the tick loop and observe the skip branch firing. A test that actually drives the loop and counts dispatches would have caught that the skip branch is unreachable from within the tick loop's own flow.

The `InFlightGuard::drop`'s panic-safety guarantee is also half-true in the current structure: yes, the guard drops on panic (standard Rust unwind semantics across async `.await` boundaries run drops on locals), but the panic also kills the `tokio::spawn`ed tick loop task — there is no `catch_unwind` or respawn — so the flag clearing is academic. After a panic, no subsequent tick runs for ANY config until the process restarts.

**What we did about it:** Added two wanderer tests to `dadbear_extend.rs`:
1. `test_tick_loop_is_serial_within_single_task` — empirically mirrors the tick loop shape (single `tokio::spawn` around `loop { sleep; for cfg in cfgs { dispatch.await } }`) with a 500ms "dispatch" and a 50ms base tick, and an `unreachable!()` in the skip branch. Runs for 1.2 seconds; observes at most 2–3 dispatch starts and zero skip branches hit. Proves the scheduler does not advance the outer loop while an inner await is pending.
2. `test_trigger_for_slug_does_not_see_in_flight_flag` — documentation test asserting the structural fact that `in_flight` is a local variable inside `start_dadbear_extend_loop`'s spawned closure and cannot be observed by `trigger_for_slug` or any other caller. The test body is a comment; its value is pinning the claim in source so a future restructuring that hoists the HashMap has to update the test.

**Escalation:** Writing a deviation protocol block for the planner in this log entry (below) rather than committing a revert or rewrite. The spec's race claim is wrong, but the flag is cheap, correct (drops on guard drop, retain cleanup mirrors tickers), and harmless. It will become load-bearing if any of these changes happen later:
- The tick loop is restructured to `tokio::spawn` per-config sub-tasks (so iterations can genuinely overlap for the same config if cleanup is sloppy)
- The flag is promoted to `PyramidState` / `DadbearState` and `trigger_for_slug` (plus any future caller) is taught to consult it, gating concurrent manual triggers during a running auto-dispatch
- A future phase adds retries that respawn the tick loop after panics and wants a stable "config X currently in-flight" signal

> [For the planner]
>
> **Phase 1 deviation note:** the in-flight flag landed per spec, but the race the spec describes does not exist in the current `dadbear_extend.rs` structure. The tick loop is a single spawned future whose outer `loop { }` cannot advance while a prior iteration's `.await` is pending; the only other caller of `run_tick_for_config` is `trigger_for_slug`, which does not consult the flag (the HashMap is a local variable inside `start_dadbear_extend_loop`'s closure). Net: the flag is a no-op in the current code.
>
> I did NOT rip it out because (a) it's cheap, (b) it's correct in isolation, (c) the RAII pattern is the right shape for the race once it becomes real, and (d) removing it invites a future "reintroduce this" pass. I added two wanderer tests that document the current structural facts and empirically verify serial execution of the outer loop.
>
> **Decision points for the planner:**
>
> 1. **Is the spec's framing still correct for future phases?** If Phase 17 (recursive folder ingestion) restructures the tick loop to spawn per-config sub-tasks, the flag becomes live at that point — that's fine, but the Phase 1 spec should say "forward-looking guard for Phase 17" rather than "guard against a live race in the current tree."
> 2. **Should `trigger_for_slug` consult the flag?** Today, an HTTP trigger fired during an auto-dispatch races into the same code path and only serializes at the `LockManager::global().write(slug)` call inside `fire_ingest_chain` / `run_build_from`. Two concurrent `run_tick_for_config` calls for the same slug will both scan, both detect changes, both upsert ingest records under the lock (idempotent because `UNIQUE(slug, source_path, ingest_signature)`), and then both hit `dispatch_pending_ingests` sequentially via the lock. The second one's `fire_ingest_chain` runs a second build after the first completes — not a data-corruption race, but a "double work" race. If we want manual triggers to skip while auto is running, the flag needs to live on shared state and `trigger_for_slug` needs to consult it.
> 3. **Is "the tick loop dies on panic" a real concern that deserves its own fix?** `run_tick_for_config` panicking (LLM parse failure, DB corruption, etc.) will unwind through the `.await` at line 200, drop `_guard`, exit the for-loop via unwind, exit the outer `loop`, and tokio will catch the panic at the task boundary. The task terminates with `JoinError::Panic`. Nothing respawns it. DADBEAR is silently dead for every config from that point until the app restarts. The `InFlightGuard`'s panic-safety is load-bearing in a world where the loop restarts after a tick failure; it is not load-bearing in the current world because the loop is gone. This is a real operational gap worth a separate workstream (catch the panic in a `join!` or `tokio::task::Builder`-style harness, log, and respawn) and the implementation log should note it.
>
> **My proposed correction:** treat the Phase 1 work as "landed, no-op in current code, becomes live in Phase 17+." Leave the code as-is. Update `evidence-triage-and-dadbear.md` Part 1 and `handoff-2026-04-09-pyramid-folders-model-routing-addendum-01.md`'s "Phase 1's symptom attribution has been corrected" section to replace the "next 1-second tick would start a concurrent dispatch" framing with the honest framing: "the flag is in place for the future tick loop restructuring (per-config `tokio::spawn` or equivalent) that Phase 17's folder-recursion work may introduce; under the current shape, it is structurally unobservable but does not add cost or risk." File a separate follow-up ticket for (a) `trigger_for_slug` flag consultation if we decide manual triggers should skip during auto dispatch, and (b) tick loop panic-recovery if we want DADBEAR to survive a panicking `run_tick_for_config`.
>
> If instead you want the flag to actually guard a real race today, the smallest change is to hoist `in_flight` into `PyramidState` (as `dadbear_in_flight: Arc<DashMap<i64, Arc<AtomicBool>>>` or similar) and have `trigger_for_slug` consult it before calling `run_tick_for_config`. That's maybe 15 lines. I can do that if you'd rather ship a flag that fires on the HTTP-trigger race than wait for a future restructuring.

**Lesson for future phases:** When a spec says "this guards against race X," the verification must include a test that instantiates the minimal racing configuration and observes the guard firing — not just a test that exercises the primitive in isolation. The existing `test_in_flight_guard_skip_and_panic_safety` test sets the flag by hand, checks it by hand, and drops the guard by hand; it never instantiates the tick loop or observes the tick loop's for-loop reaching the skip branch. A test that runs the actual loop shape (as `test_tick_loop_is_serial_within_single_task` now does) catches the structural fact that the skip branch is unreachable from the only code path that was supposed to fire it. For every future "race guard" workstream, require the verification test to drive the surrounding scheduler / task / loop, not just the primitive.

**Resolution (fix pass, 2026-04-10):** hoisted `in_flight` to `PyramidState::dadbear_in_flight` (`Arc<std::sync::Mutex<HashMap<i64, Arc<AtomicBool>>>>`) and taught `trigger_for_slug` to consult it. The flag now guards the real race: two concurrent `run_tick_for_config` calls from (a) the auto tick loop and (b) an HTTP/CLI manual trigger for the same config. Under the old code, the second caller's `fire_ingest_chain` would run a full redundant chain build after the first completes — a "double work" race that burned LLM budget and wall-clock time. Under the new code, the second caller observes the flag set and skips with a `"skipped: dispatch in-flight"` JSON note; the HTTP caller gets a fast response instead of queuing a duplicate full-pipeline dispatch.

The scheduler re-entrancy race the original Phase 1 spec described remains structurally impossible, and that's fine — it would become relevant if Phase 17 spawns per-config sub-tasks, and the shared primitive is already the right shape for that. The panic-safety `InFlightGuard` primitive is unchanged and load-bearing in both call sites (tick loop AND trigger_for_slug). Both call sites use the RAII guard verbatim; there is no second cleanup path.

Files touched: `pyramid/mod.rs` (new field + `with_build_reader` clone), `pyramid/dadbear_extend.rs` (tick loop + `trigger_for_slug` + new test `test_tick_loop_and_trigger_race_skip` + rewrite of the stale wanderer doc test as `test_trigger_for_slug_respects_shared_in_flight_flag` + `make_test_state` fixture init), `main.rs` (3 `PyramidState { }` sites), `pyramid/vine.rs` (1 site), `pyramid/chain_executor.rs` (4 test fixtures). Tests: 15/15 passing in `pyramid::dadbear_extend`, 10/10 passing in `pyramid::chain_executor::tests::integration*`, 795/802 passing across the full lib (the 7 failures are pre-existing schema-drift failures in `staleness`/`db`/`defaults_adapter` tests, reproduced on pre-fix stashed state, and all in files untouched by this fix pass).

**Out-of-scope items still open after the fix pass:**
- **Tick loop panic recovery** — `run_tick_for_config` panicking still kills the spawned task with no respawn. DADBEAR silently dies until app restart. Not part of Phase 1 fix pass; deserves its own workstream.
- **Spec doc correction** — `evidence-triage-and-dadbear.md` Part 1 and `handoff-2026-04-09-pyramid-folders-model-routing-addendum-01.md` still describe the guard as covering the scheduler re-entrancy race. They should be updated to say "guards the HTTP/CLI-trigger-vs-auto-dispatch race today; becomes the scheduler guard automatically if Phase 17 restructures the tick loop into per-config sub-tasks." Planner approval required for spec doc edits; this fix pass deliberately does not touch the spec.

---

### 2026-04-09 — Phase 2 wanderer: L0 file_change path silently de-contentifies L0 nodes

**Phase / workstream:** Phase 2 (wanderer pass on `phase-2-change-manifest-supersession`)

**What hit friction:** Phase 2's rewrite of `execute_supersession` only considered the upper-layer (L1+) stale-update path the spec discusses. In reality, `execute_supersession` has TWO callers in `stale_engine.rs`, not one: (a) the L1+ confirmed_stale path at line 968, and (b) the L0 file_change path at line 838 (inside `file_batches`' `if result.stale == 1` branch, after resolving file path → L0 node IDs via `pyramid_file_hashes.node_ids`). The L0 caller is load-bearing for the primary DADBEAR use case: "user edits a file on disk, pyramid L0 node needs to reflect the new content."

Pre-Phase-2, the body of `execute_supersession` (now preserved verbatim as `execute_supersession_identity_change`) had an explicit `if depth == 0 { read_source_file ... }` branch at lines 2551-2562 that loaded the actual file content, packed the first 400 lines / 20k chars into the LLM prompt as `source_snapshot`, and asked the LLM to rewrite the L0 headline + distilled to match the current file.

Post-Phase-2, the new body takes an entirely different code path that NEVER reads the source file. `load_supersession_node_context` (stale_helpers_upper.rs:2180) reads headline, distilled, topics, terms, decisions, dead_ends, and `pyramid_deltas` rows — but no source file. `build_changed_children_from_deltas` for an L0 node with no deltas returns a single synthetic `ChangedChild { child_id: format!("{parent}-children"), old_summary: distilled, new_summary: distilled }` — same string for OLD and NEW. The LLM gets "nothing changed" as input and (if it behaves) produces a near-empty manifest with `content_updates` all null. `update_node_in_place` applies essentially a no-op and just bumps build_version. **The L0 node's distilled text remains whatever it was before the file change.**

If the LLM hallucinates instead (produces a non-null distilled update based on the existing distilled alone), it's WORSE than a no-op — it's hallucinated content drift.

Compounding factors:
- `pyramid_file_hashes.hash` is never updated on file_change (only on initial ingest via `upsert_file_hash` at main.rs:3454). So the watcher re-fires file_change on every tick until the hash matches. Previously the identity-change path masked this because it created new node IDs and at least tried to update headline/distilled; now it just bumps build_version repeatedly while content stays static.
- `update_node_in_place` does NOT enforce WS-IMMUTABILITY-ENFORCE. Canonical L0 nodes (`depth <= 1 AND provisional = 0`) are supposed to be permanently immutable per `apply_supersession`'s check at db.rs:2481, but `update_node_in_place` has no such guard and will happily mutate an L0 canonical row. Pre-Phase-2, the identity-change path sidestepped this by writing a NEW node instead of mutating the existing L0 — which was in fact the entire cause of the viz orphaning bug, but it preserved the invariant.
- Neither the spec (`docs/specs/change-manifest-supersession.md` → "Integration with Stale Engine") nor the workstream prompt mentions the L0 file_change caller. The spec's Current/New flow diagrams only show `PendingMutation → dispatch_node_stale_check → stale=true → execute_supersession`, which is the L1+ path.

**Root cause:** The spec author identified the `supersede_nodes_above()` three-caller framing (which is factually off anyway — it's two callers, neither is stale_helpers_upper) but not the `execute_supersession` two-caller framing. The implementer followed the spec literally and tested only upper-layer node paths (the new tests use `insert_upper_node` at depth 2/3, with an explicit code comment at line 2961 "depth 2 is safe — it's above the bedrock immutability cutoff"). The verifier's punch list matched the spec and also missed the L0 caller. Both were scoped to the upper-layer concern.

**Impact on the viz orphaning fix:** Phase 2 DOES fix the viz DAG coherence for the L1+ path (node IDs are stable, evidence links remain valid). It also accidentally "fixes" viz coherence for the L0 path in the sense that IDs are now stable there too. But it breaks the semantic correctness of L0 content sync: L0 nodes lose the ability to reflect file changes, and DADBEAR enters a loop where it keeps detecting the file as stale (because the hash never updates) and keeps firing no-op updates.

**What we did about it:** documented here; the wanderer's verdict is that Phase 2 is a **non-blocking concern** for the viz-orphaning mission but a **blocking regression** for the L0 content-sync mission. Planner escalation with proposed fix options is in the wanderer's summary (see report). No code changes applied — the right fix needs planner direction on whether to (a) add a depth==0 branch to the new execute_supersession that threads file content into the manifest input, (b) route L0 file_change calls back to `execute_supersession_identity_change` explicitly rather than falling through the Phase 2 path, (c) add a depth==0 guard to `update_node_in_place` that refuses and escalates to a rebuild, or (d) accept the regression and fix it in a Phase 2.1 focused on L0.

**Lesson for future phases:** Grep for EVERY caller of a function you're rewriting, not just the ones the spec describes. The spec in this case listed "three callers of supersede_nodes_above" but that's the wrong function — the actual thing being rewritten is `execute_supersession`, which has its own callers the spec didn't enumerate. Always grep the function name itself, not the function the spec mentions, when deciding scope.

---

### 2026-04-09 — Phase 2 wanderer: identity-change fallback reintroduces the orphaning bug verbatim

**Phase / workstream:** Phase 2 (wanderer pass on `phase-2-change-manifest-supersession`)

**What hit friction:** `stale_helpers_upper.rs::execute_supersession` falls back to `execute_supersession_identity_change` (the pre-Phase-2 body, preserved verbatim) in two situations: (1) when `generate_change_manifest` fails for any reason (LLM error, JSON parse failure, network blip), and (2) when the LLM returns `identity_changed: true`. The implementation log at `docs/plans/pyramid-folders-model-routing-implementation-log.md:487` frames this as "degrades gracefully rather than leaving a stale node un-updated." But `execute_supersession_identity_change` IS THE BUG Phase 2 was written to fix — it creates a new node ID (`next_sequential_node_id(..., "S")`), leaves all `pyramid_evidence` links pointing at the old ID, and marks the old node `superseded_by` so `live_pyramid_nodes` filters it out. That's exactly the pattern that produces the viz orphaning bug.

If the manifest LLM is flaky (bad JSON 5% of the time), 5% of stale checks will reintroduce the exact viz orphaning bug Phase 2 was supposed to eliminate. The bug will be intermittent and hard to debug because the "fix is landed" narrative masks it.

**Root cause:** The graceful-degradation tradeoff was well-intentioned but backwards — better to mark a failed manifest as failed and SKIP the update (leaving the node at its prior valid state) than to fall through to a path that corrupts the viz. The spec's "Failure handling" section at line 251 explicitly says "an unapplied manifest is visible and recoverable" but the implementation takes "fall back to new-id path" as the failure route, which is the worst of both worlds.

**What we did about it:** documented here; wanderer report recommends the manifest-gen-failure fallback be replaced with a "log failure manifest row + return early without update" path that leaves the node untouched. The identity-change path should ONLY run when the LLM explicitly sets `identity_changed = true` after a successful manifest generation. No code changes applied — planner direction needed.

---

### 2026-04-09 — Phase 2 wanderer: `build_id` parameter in `update_node_in_place` is dead

**Phase / workstream:** Phase 2 (wanderer pass on `phase-2-change-manifest-supersession`)

**What hit friction:** `db::update_node_in_place(..., build_id: &str, supersession_reason: &str)` takes a `build_id` parameter that is never written to any row. The snapshot INSERT at db.rs:2896 uses `snap.build_id.clone()` (the pre-update node's existing build_id), not the function parameter. Line 3018 has `let _ = build_id;` with a misleading comment claiming "carried into the snapshot above". The caller at stale_helpers_upper.rs:2090 passes the literal string `"stale_refresh"` as BOTH `build_id` and `supersession_reason`, which is fine functionally but makes the code lie about its shape.

**Impact:** No correctness bug — the snapshot preserves the node's original build_id, which is arguably the right semantic. But the API is misleading: future callers will think they're controlling the snapshot's build_id when they aren't.

**What we did about it:** documented here. Fix is trivial: either remove the unused parameter from the signature, or actually write it to the snapshot's build_id column. Wanderer did not apply because the fix touches the public API and deserves a planner-approved cleanup.

---

### 2026-04-10 — Phase 3 wanderer: `config_for_model` loses the provider registry, so stale engine + faq + delta + webbing + meta + stale_helpers* all hit the transitional fallback

**Phase / workstream:** Phase 3 (wanderer pass on `phase-3-provider-registry-credentials`)

**What hit friction:** The implementer's framing in the Phase 3 log says "production boots always attach a non-None registry via `PyramidConfig::to_llm_config_with_runtime`. The registry path is the canonical path, but legacy fields still drive the cascade when the registry isn't the per-call resolver." The phrasing "when the registry isn't the per-call resolver" is doing a lot of work — in reality, the registry IS the per-call resolver **only for code paths that hold an `LlmConfig` derived from `PyramidState.config`**. Every other code path in the repo that constructs a fresh `LlmConfig` via `config_helper::config_for_model(api_key, model)` gets `provider_registry: None` + `credential_store: None` (because `config_for_model` ends with `..Default::default()` and `Default::default()` explicitly zeroes both new fields). That means every LLM call from:

- `stale_engine.rs` / `stale_helpers.rs` / `stale_helpers_upper.rs` (L0 and L1+ stale dispatch, including `generate_change_manifest`, `dispatch_connection_check`, `dispatch_edge_stale_check`, `execute_supersession_identity_change`)
- `faq.rs` (query refinement, auto-annotate, FAQ generation — 6 call sites)
- `delta.rs` (delta generation — 4 call sites)
- `webbing.rs`, `meta.rs` (1 call each per file, 5 call sites total)
- `build.rs` (legacy build path's `call_and_parse`)

... goes into `call_model_with_usage` → `call_model_unified` → `call_model_unified_with_options` → `build_call_provider(config)`, which sees `provider_registry: None` and falls into the transitional fallback path:

```rust
// Transitional fallback path: no registry, no credential store.
let provider = OpenRouterProvider {
    id: "openrouter".into(),
    display_name: "OpenRouter".into(),
    base_url: "https://openrouter.ai/api/v1".into(),
    extra_headers: vec![],
};
let secret = if config.api_key.is_empty() {
    None
} else {
    Some(ResolvedSecret::new(config.api_key.clone()))
};
```

This path:
1. Uses the hardcoded `base_url` string literal in `build_call_provider` — **not the `pyramid_providers.base_url` column** — so a user who edits the provider row (e.g. for an OpenAI-compatible mirror) will be ignored on every stale/faq/delta/webbing/meta/build call.
2. Wraps `config.api_key` (which ultimately comes from the legacy `pyramid_config.openrouter_api_key` on-disk JSON field) into a `ResolvedSecret`. **The `.credentials` file's `OPENROUTER_KEY` entry is not consulted.**
3. Never reads any row from `pyramid_providers`, `pyramid_tier_routing`, or `pyramid_step_overrides` — so per-tier routing, per-step overrides, pricing, and `supported_parameters` gating are all silently skipped.

The ~22 `config_for_model` call sites are **more than half** the LLM call sites in the repo. The new provider registry and credential store only service the remaining call sites (chain executor, partner, public_html routes, routes.rs semantic search, evidence_answering, characterize, question_decomposition, extraction_schema, supersession, planner_call in main.rs) — roughly the "chain-driven build" path. The "DADBEAR maintenance + faq refresh + delta generation" paths silently bypass the whole Phase 3 infrastructure.

**What the user will observe:**

1. On first boot with a valid `OPENROUTER_KEY` in `.credentials` AND a non-empty legacy `pyramid_config.openrouter_api_key`, everything works. Chain executor calls pick up `.credentials`; stale engine calls pick up the legacy field. Both hit OpenRouter successfully.

2. On first boot with a valid `OPENROUTER_KEY` in `.credentials` and an EMPTY legacy `pyramid_config.openrouter_api_key`: chain executor builds still work (they resolve via `.credentials`). Stale engine / faq / delta / webbing / meta / build-path calls will all fail with the `OpenRouterProvider::prepare_headers` error:
   > `OpenRouter provider `openrouter` requires an api_key_ref but the credential resolved to None`
   because `build_call_provider` passes `None` for the secret when `config.api_key.is_empty()`. This is the "`.credentials` file is authoritative" scenario and it fails silently except via the error bubbling up as a stale-dispatch failure.

3. On rotation: if the user rotates `OPENROUTER_KEY` via `pyramid_set_credential`, the chain executor picks up the new key immediately (registry `instantiate_provider` re-resolves on every call). Stale engine does NOT pick it up — `PyramidStaleEngine` caches `api_key: String` at construction time (from `pyramid_state.config.api_key` at the moment `pyramid_start_file_watcher_stale_engine` fires), and propagates that cached string through every `drain_and_dispatch → dispatch_node_stale_check → config_for_model` call chain. The only way to refresh it is to restart the app or re-apply a profile (which re-reads `pyramid_state.config` — but `pyramid_state.config.api_key` still comes from the legacy `PyramidConfig.openrouter_api_key` field, not `.credentials`).

**Root cause:** the implementer's framing focused on "replacing the URL literal in llm.rs" and achieved that locally. But the repo already had a pattern (`config_for_model`) that constructs a throwaway `LlmConfig` for every LLM call from the maintenance pipelines. That pattern was invisible to the implementer's "grep for `call_model_*` and thread the registry through" search because the registry lives on the `LlmConfig` struct itself, not on the `call_model_*` arguments. The call sites DO thread through a working `LlmConfig` — it's just a fresh one that lost the registry fields on the way. The spec's "85+ call sites" framing described the count accurately but not the shape: the refactor needed to either (a) retire `config_for_model` and force every caller to pass the live `LlmConfig` through, OR (b) thread `Arc<ProviderRegistry>` + `Arc<CredentialStore>` into `config_for_model` as parameters. Neither was done.

**Impact:**

- **Not a build-breaking bug.** Builds still fire LLM calls successfully as long as the legacy `pyramid_config.openrouter_api_key` field is set.
- **Credential rotation is half-broken.** Rotating via Settings → Credentials only affects chain-executor calls. The maintenance subsystem keeps using the old key until restart.
- **Per-tier routing is silently ignored** on the entire maintenance subsystem. If the user assigns `stale_remote` to a different model in `pyramid_tier_routing`, it has zero effect on stale dispatch — that code path hardcodes `primary_model` from `LlmConfig` via `config_for_model`.
- **Provider registry's base URL is silently ignored** on the maintenance subsystem. A user who adds a self-hosted OpenAI-compatible provider as the default cannot use it for stale/faq/delta — those calls always go to `https://openrouter.ai/api/v1`.
- **`.credentials` file is effectively write-only for the maintenance subsystem** — nothing reads it.

**Status:** Non-blocking for Phase 3's literal scope (which was "replace the hardcoded URL + parser with a trait"). Blocking for the spec's intent ("`.credentials` is the one source of truth; the provider registry is the canonical resolver; per-tier routing works everywhere"). The implementer's log entry does partially disclose this ("legacy fields still drive the cascade in `call_model_unified` when the registry isn't the per-call resolver") but does not call out that ~22 call sites silently bypass the new infrastructure via `config_for_model`. The phrasing minimizes an architectural gap that will become load-bearing the moment Phase 4/6 tries to move temperature/max_tokens into contributions — those phases will need to either (a) retire `config_for_model` or (b) thread the registry into it. Either way, fixing it earlier is cheaper than later.

**Proposed fix options (planner decision required):**

1. **Thread the registry through `config_for_model`.** Change signature to `config_for_model(api_key: &str, model: &str, registry: Option<Arc<ProviderRegistry>>, creds: Option<Arc<CredentialStore>>) -> LlmConfig`. Update every `dispatch_node_stale_check`, `fire_webbing`, `generate_faq_*`, `generate_delta_*` function to accept and forward `Arc<ProviderRegistry>` + `Arc<CredentialStore>` from its caller. The caller chain terminates at `PyramidStaleEngine::new` / the HTTP route handlers / `dadbear_extend::run_tick_for_config` / etc., each of which already has a `PyramidState` in scope.

2. **Retire `config_for_model` in favor of `PyramidState.config.clone_with_model_override(model)`.** Add a helper on `LlmConfig` that clones and overrides just the `primary_model` field. Every caller passes the live `LlmConfig` from `state.config.read().await.clone()` and mutates the model in place. This preserves the registry fields by construction.

3. **Keep `config_for_model` but require `Arc<CredentialStore>` globally.** Store the credential store in a `OnceLock<Arc<CredentialStore>>` at boot time. `config_for_model` reads from the global. This is the lowest-churn fix but introduces a global singleton which is exactly the shape the Phase 3 spec was trying to avoid.

Option 2 is the cleanest and the one the implementer probably intended. Option 1 is a bigger surface-area change but matches the existing pattern of threading dependencies explicitly. Option 3 is the smallest change but worst architecturally.

**What the wanderer did:** documented here; did NOT apply a fix because the right fix requires a planner-level decision about which approach to take. All three options touch the same ~22 call sites and ~5 intermediate function signatures; the cleanup is straightforward in any direction but should be done once, not in dribs and drabs.

**Lesson for future phases:** when refactoring a shared config struct to add "optional runtime handles", grep for `..Default::default()` / `LlmConfig { .. }` struct literal expressions in production code, not just for the struct's construction sites via `new()` or equivalent. A helper that wraps `Default::default()` to build a fresh instance will silently zero any new optional fields — that's the exact scenario `config_for_model` hit. Also: if the refactor claim is "X gets carried through", the test suite should include at least one end-to-end test that exercises a call path terminating in `build_call_provider` and asserts that the `provider_registry` branch (not the fallback) was hit. None of the Phase 3 tests do this — they exercise the registry directly via `ProviderRegistry::instantiate_provider`, not via `call_model_unified` starting from `config_for_model`.

---

### 2026-04-10 — Phase 3 wanderer: credential store reads in-memory cache, not the file, so file-edits-outside-UI aren't picked up

**Phase / workstream:** Phase 3 (wanderer pass on `phase-3-provider-registry-credentials`)

**What hit friction:** The `credentials-and-secrets.md` spec's Open Questions section explicitly recommends "no file watcher in v1 — resolver reads the file on every resolve. This is slow but correct and simple." The implementation does the opposite: `CredentialStore::load` reads the file once at boot into a `BTreeMap<String, String>` guarded by an `RwLock`, and `resolve_var` / `substitute` walk that in-memory map. Subsequent edits via `pyramid_set_credential` → `store.set(key, value)` mutate the in-memory map and call `save_atomic` to flush to disk, so IPC-driven rotation works within the session. But **direct edits to `.credentials` with a text editor** (which is the whole point of a human-readable YAML file per the spec) are not observed by the running app — the user's `chmod` and `vim` edits sit unused until the next restart.

**Impact:** Not a correctness bug within the IPC surface — that's intentionally routed through the store. It is a spec deviation (the spec explicitly calls out "The resolver reads the file on every resolve" as the chosen v1 semantics) and a UX gotcha for users who follow the spec's "you can edit the file with your preferred editor" framing in the comments of the serialized YAML. The serialized YAML header reads:

```
# Wire Node credentials file — YAML, plain text, 0600 mode enforced.
# Managed by Wire Node. Edit via Settings → Credentials or in your preferred editor.
# Reference credentials in configs as ${VAR_NAME}.
```

The "or in your preferred editor" phrasing implies live reload, which isn't implemented.

**Status:** Non-blocking. Two reasonable fixes:

1. **Implement the spec's on-every-resolve read.** Change `resolve_var` / `substitute_to_string` to re-parse the file on each call. This is what the spec recommends. Slow in hot paths (LLM calls) but only a few ms per call. Acceptable for v1.
2. **Update the comment in `serialize_credentials_yaml`** to say "Edit via Settings → Credentials (live). Editing the file directly requires an app restart." This matches current behavior and preserves the in-memory fast path.

I lean toward option 2 for v1 — live reload adds complexity for a rare use case, and the comment clarification is a 2-line change. But the spec says option 1 is the chosen approach, so planner direction is required.

**Proposed action:** document via this friction log; planner decides option 1 vs option 2. Wanderer did not apply a fix.

---

### 2026-04-10 — Phase 3 wanderer: `pyramid_test_api_key` IPC endpoint still uses hardcoded URL + legacy api_key field, bypassing the new registry

**Phase / workstream:** Phase 3 (wanderer pass on `phase-3-provider-registry-credentials`)

**What hit friction:** The existing `pyramid_test_api_key` IPC command in `main.rs:5383` does a `GET https://openrouter.ai/api/v1/models` with `Authorization: Bearer {config.api_key}` hardcoded directly, bypassing both the provider registry and the credential store. Phase 3 added a separate `pyramid_test_provider` IPC command (which v1 just checks credential presence without making a real HTTP call), but the legacy `pyramid_test_api_key` remains wired up and unchanged. The frontend Settings page still calls it as the "Test API Key" button.

**Impact:**

- The test button reports success/failure based on the legacy `LlmConfig.api_key` field, not the `.credentials` file's `OPENROUTER_KEY`. A user who just rotated their key via Settings → Credentials will see the test button report **old** key's status because the button consults `config.api_key` which hasn't updated.
- The hardcoded URL means users with a self-hosted or OpenAI-compatible provider registered in `pyramid_providers` cannot test it via this button — it always hits openrouter.ai.
- The phase 3 implementer's "grep returns only two hits in provider.rs" verification claim is only true for the specific `openrouter.ai/api/v1/chat/completions` literal. The broader `openrouter.ai/api/v1` prefix appears in `main.rs`, `partner/conversation.rs`, `db.rs` (seed row), and `llm.rs` (transitional fallback) too. Three of those are legitimate (seed, transitional fallback, partner); `main.rs:5393` is a missed refactor.

**Status:** Non-blocking; the old button still works for the legacy path. Fix options:

1. **Route `pyramid_test_api_key` through `ProviderRegistry::instantiate_provider`** — resolve the `openrouter` row, build the provider impl, do a 1-ping `GET /models` using `provider.chat_completions_url().strip_suffix("/chat/completions") + "/models"` or add a `fn models_url(&self) -> String` method to the trait. Drop the hardcoded URL.
2. **Delete `pyramid_test_api_key` entirely** and migrate the frontend button to use `pyramid_test_provider` which was introduced in Phase 3.

Option 2 is cleaner (single test code path) but requires a frontend change. Option 1 is a 15-line backend fix that keeps the existing frontend button working. Either is fine; the current state is a gap between "old way" and "new way" that will confuse users during the migration window.

**What the wanderer did:** documented; did not delete the legacy command because it's load-bearing on the current frontend. Planner should decide option 1 vs option 2.

---

### 2026-04-10 — Phase 3 wanderer: `.credentials` atomic write doesn't fsync the parent directory

**Phase / workstream:** Phase 3 (wanderer pass on `phase-3-provider-registry-credentials`)

**What hit friction:** `CredentialStore::save_atomic` writes to a sibling temp file, fsyncs the file, renames over the original, and re-applies 0600 on the final. It does NOT fsync the containing directory after the rename. POSIX's full crash-safe rename pattern is:

1. Write temp file
2. fsync temp file
3. rename temp → target
4. **fsync containing directory** ← missing

Without step 4, a system crash immediately after step 3 can leave the filesystem directory block unflushed, meaning the rename may not be visible on the next boot — the user could see the OLD credentials file with stale content and NO temp file.

**Impact:** Very narrow. A `.credentials` file is typically written only when the user rotates a key via the IPC endpoint, which is rare enough that the probability of a system crash landing in the exact window between the rename and the next flush is effectively zero for a desktop app. Servers doing frequent writes would care about this; desktop Tauri apps basically don't.

**Status:** Informational. Not worth fixing unless the credentials file is ever written in a tight loop. The spec's "atomic writes" requirement is met in spirit (the rename itself is atomic); it's just not maximally crash-safe.

---

### 2026-04-10 — Phase 3 wanderer: `parse_openai_envelope` drops the control-char sanitize fallback that the old parser had

**Phase / workstream:** Phase 3 (wanderer pass on `phase-3-provider-registry-credentials`)

**What hit friction:** The pre-Phase 3 `parse_openrouter_response_body` in `llm.rs` had three levels of defensiveness:

1. Direct `serde_json::from_str`
2. SSE `data:` line extraction + parse
3. `{...}` substring extraction + parse
4. **`sanitize_json_candidate` fallback that strips non-whitespace control characters** then re-parses both the substring and the full trimmed body

The new `parse_openai_envelope` in `provider.rs::540+` has levels 1-3 but **not** level 4 (the control-char sanitize pass). The implementer's impl log says "The provider's test suite covers the same SSE / prefixed-json fixtures the old tests exercised" — which is true for the SSE and prefixed-json fixtures, but the old sanitize test case (`test_parse_openrouter_response_body_accepts_control_chars` or similar) is gone too.

**Impact:** Narrow. Some upstream LLM providers (particularly models behind aggressive streaming infrastructure) occasionally emit stray `\x01` / `\x02` control bytes in the response body. Pre-Phase 3 those would be stripped and the parse would succeed on retry. Post-Phase 3 those requests will fail the parse and go through the retry loop. The retry loop will probably succeed on the second try (streaming artifacts are non-deterministic), so the effective user-visible behavior is "one extra retry on corrupted responses." Not a correctness regression, but it does weaken the parser's resilience.

**Proposed fix:** port `sanitize_json_candidate` into `parse_openai_envelope` as the final fallback before returning `Err`. The old code was ~10 lines:

```rust
fn sanitize_json_candidate(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
        .collect()
}
```

Then add a final attempt in `parse_openai_envelope` that sanitizes and re-parses. Low-risk, low-cost, restores parity with the pre-refactor behavior.

**Status:** Non-blocking. Documented here. A wanderer could apply the fix inline (10 lines) or leave it as a planner decision.

---

### 2026-04-10 — Phase 3 wanderer: HTTP 400 error body is logged at WARN level including any API key echoed back by the provider

**Phase / workstream:** Phase 3 (wanderer pass on `phase-3-provider-registry-credentials`)

**What hit friction:** `call_model_unified_with_options` at `llm.rs:479` logs:

```rust
warn!(
    "[LLM] HTTP 400 from {} — body: {}",
    short_name(&use_model),
    &body_400[..body_400.len().min(500)],
);
```

The first 500 bytes of the response body go to the tracing fmt layer, which writes to both stdout and a log file. Most 400 responses from OpenRouter don't echo back the API key, but some error paths might (e.g., "authorization header malformed: Bearer sk-or-v1-..." in a debug-heavy backend). There is no explicit sanitization of the body before the warn! call.

This is **pre-existing behavior** (same warn! line existed before Phase 3) and not a regression, but Phase 3's emphasis on the never-log rule for credentials makes the gap worth flagging: the Phase 3 claim is "`ResolvedSecret` opacity prevents credentials from appearing in logs" — and that's true for the credential as it passes through the type system, but the HTTP response body is a separate channel that could leak via a misconfigured or verbose provider.

**Impact:** Near-zero in the common case. Worth a one-line sanitization pass on the logged body before the warn!:

```rust
let safe_body = redact_bearer_tokens(&body_400[..body_400.len().min(500)]);
warn!("[LLM] HTTP 400 from {} — body: {}", short_name(&use_model), safe_body);
```

Where `redact_bearer_tokens` runs a regex like `sk-or-[a-zA-Z0-9-]{20,}` → `sk-or-[redacted]`.

**Status:** Informational. Pre-existing, documented here for follow-up. Not a Phase 3 regression — this warn! line already existed.

---
