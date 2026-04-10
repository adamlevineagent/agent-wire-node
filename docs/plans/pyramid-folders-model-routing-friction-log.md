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
