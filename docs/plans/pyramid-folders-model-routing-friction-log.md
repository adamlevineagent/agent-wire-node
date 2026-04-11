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

### 2026-04-10 — Phase 4 wanderer: sync_config_to_operational was never called from production (BLOCKING, fixed in-place)

**Phase / workstream:** Phase 4 (wanderer pass, post-verifier)

**What hit friction:** The Phase 4 spec's central invariant says "Write path: always write to `pyramid_config_contributions` first, then sync to operational tables." The verifier pass reported clean against its punch list (schema + CRUD + dispatcher shape), but an end-to-end trace revealed that **none of the 9 new IPC handlers in `main.rs` actually called `sync_config_to_operational`**. The dispatcher existed only as dead code reachable from tests. Every production write path (`pyramid_create_config_contribution`, `pyramid_supersede_config`, `pyramid_accept_proposal`, `pyramid_rollback_config`) inserted a row into `pyramid_config_contributions` and returned — leaving operational tables (`pyramid_dadbear_config`, the 4 new tables, `pyramid_tier_routing`, `pyramid_step_overrides`) unchanged. A user saving a new DADBEAR policy through Phase 4's contribution IPC would create an audit row but the executor would keep reading the prior (stale) operational value indefinitely.

`grep -rn "sync_config_to_operational" src-tauri/src` returned 4 hits total: 3 in `config_contributions.rs` itself (one definition, two test call sites) and 1 in a comment in `db.rs`. Zero production call sites.

**Root cause:** The Phase 4 workstream prompt described the dispatcher as a piece of functionality to build but didn't explicitly call out "and wire it into every IPC handler that produces an `active` contribution". The verifier punch list matched the workstream prompt and checked "does `sync_config_to_operational` exist with all 14 branches" — true — rather than "does it fire on every write path". The implementer built the dispatcher to spec, the verifier checked it against the spec, and the spec's invariant-phrasing ("Write path: always write to `pyramid_config_contributions` first, then sync") lived several sections away from the IPC endpoint list, so it never made it onto the punch list.

**What we did about it:** Wanderer added the missing sync call to all 4 write-path IPC handlers (`pyramid_create_config_contribution`, `pyramid_supersede_config`, `pyramid_accept_proposal`, `pyramid_rollback_config`). The pattern is uniform: after the CRUD call, re-load the contribution by id, then call `sync_config_to_operational(&writer, &state.pyramid.build_event_bus, &contribution)`. `pyramid_propose_config` and `pyramid_reject_proposal` do NOT sync (a proposal is `status = 'proposed'`, not active; a rejection stays non-active). `pyramid_accept_proposal` gained a sync call because accept promotes a proposal to active. The fix is on branch `phase-4-config-contributions`.

**Lesson for future phases:** Write-path invariants deserve their own punch-list line, separate from "does the helper function exist". The verifier should trace at least one example write path end-to-end from the IPC boundary to the operational table and confirm every step is wired. "Dispatcher exists" and "dispatcher has all 14 branches" are not the same as "dispatcher is reachable from production code". This is exactly the class of bug the wanderer protocol was designed to catch — the punch list verifier was reading the same map the implementer was writing from; only a wanderer with no map asks "okay but does this actually run?"

Relatedly: the spec should have an explicit "Every IPC handler that creates or activates a contribution calls `sync_config_to_operational` before returning" requirement, not buried in a paragraph. Adding to the "Operational Table Sync" section of `config-contribution-and-wire-sharing.md` is a follow-up for the planner.

---

### 2026-04-10 — Phase 4 wanderer: six legacy bypass paths still write directly to operational tables (NON-BLOCKING, deferred)

**Phase / workstream:** Phase 4 (wanderer pass, scope-boundary concern)

**What hit friction:** Even with the Phase 4 IPC handlers fixed to call the dispatcher, multiple **pre-existing** write paths still write directly to operational tables without creating contributions. This violates the spec's invariant in spirit, but Phase 4's scope boundary explicitly said "do NOT modify Phase 3's provider registry CRUD except to call into it from the sync dispatcher," and similar pre-existing paths for DADBEAR were never called out as Phase 4 scope.

Direct-write paths still open after Phase 4:

1. `src-tauri/src/pyramid/routes.rs:8057` — `POST /pyramid/:slug/dadbear/watch` HTTP handler calls `db::save_dadbear_config` directly. The new row lands with `contribution_id = NULL`.
2. `src-tauri/src/pyramid/routes.rs:9452` — another HTTP DADBEAR save handler, same pattern.
3. `src-tauri/src/pyramid/routes.rs:3288` — inside the async build orchestrator, DADBEAR config is created directly from post-build metadata. `contribution_id = NULL` on the resulting row.
4. `src-tauri/src/main.rs:3274` — same post-build DADBEAR config creation inside `pyramid_continue_build`.
5. `src-tauri/src/pyramid/routes.rs:8107`, `:8129` — `enable_dadbear_for_slug` / `disable_dadbear_for_slug` HTTP handlers UPDATE the operational table directly for on/off toggles.
6. `src-tauri/src/main.rs:6688` — `pyramid_save_tier_routing` IPC (Phase 3) writes `pyramid_tier_routing` directly.
7. `src-tauri/src/main.rs:6731` — `pyramid_save_step_override` IPC (Phase 3) writes `pyramid_step_overrides` directly.
8. `src-tauri/src/pyramid/db.rs:10863..10898` — `seed_default_provider_registry` writes 4 tier_routing rows on first boot via `save_tier_routing` (i.e., first-boot seed is a bypass).

The bootstrap migration's idempotency guards (sentinel marker + per-row `contribution_id IS NULL` check) are sequenced so the marker gets written during `init_pyramid_db` on first boot; from that point forward, any direct-write bypass that lands a `contribution_id = NULL` row WILL NEVER get migrated, because the sentinel marker guard short-circuits `migrate_legacy_dadbear_to_contributions` on every subsequent call. There is no per-run "catch up any orphaned rows" pass.

**Root cause:** The spec's invariant is aspirational for Phase 4 — it assumed the Phase 4 IPC handlers would be the ONLY write path. It didn't model the pre-existing HTTP routes or the Phase 3 IPC commands as callers that also need to be migrated. Phase 4's workstream prompt explicitly excluded Phase 3's provider registry from modification, which makes sense for Phase 4 scope, but also means those IPC paths continue to write operational rows without contribution provenance.

**What we did about it:** Flagged as non-blocking because (a) the 4 new operational tables (`pyramid_evidence_policy`, `pyramid_build_strategy`, `pyramid_custom_prompts`, `pyramid_folder_ingestion_heuristics`) have no direct-write callers, so they're clean; (b) Phase 3's tier_routing and step_overrides IPCs were explicitly out-of-scope; (c) the DADBEAR direct-write paths are pre-existing and don't regress Phase 4. The escalation is described in the deviation block below.

**Lesson for future phases:** When introducing a "source of truth" invariant, the migration plan needs to include either (a) a deprecation pass that removes all legacy direct writers in the same phase, or (b) an explicit "coexistence" design where legacy writers shim into contribution creation. Phase 4 did neither — it introduced the new path but left all old paths running in parallel. Phase 5/6/9/10 will each need to handle "which legacy path am I replacing?" for their own schema types.

> [For the planner]
>
> Phase 4's central invariant (every config change flows through `pyramid_config_contributions` first, then syncs to operational tables) is architecturally sound but is contradicted by six pre-existing direct-write paths that Phase 4 did not touch:
>
> 1. **DADBEAR HTTP routes** (`POST /pyramid/:slug/dadbear/watch`, `POST /pyramid/:slug/dadbear/enable|disable`, `POST /pyramid/:slug/dadbear/commit`) — still write `pyramid_dadbear_config` directly via `save_dadbear_config` / `enable_dadbear_for_slug` / `disable_dadbear_for_slug`.
> 2. **Post-build DADBEAR creation** (`main.rs:3274`, `routes.rs:3288`) — called automatically at the end of each build to seed a DADBEAR watch config from the source path. Direct `save_dadbear_config` write.
> 3. **Phase 3 tier routing / step override IPCs** (`pyramid_save_tier_routing`, `pyramid_save_step_override`) — Phase 3 provider registry writes, still reachable from the frontend after Phase 4.
> 4. **`seed_default_provider_registry`** (`db.rs:10817`) — seeds 4 default tiers on first boot via direct `save_tier_routing` calls. First-boot state has no `tier_routing` contributions.
>
> **Question for planner:** How should Phase 5/6/9 handle these? Three options come to mind:
>
> a. **Shim legacy writers into contributions.** Wrap each direct-write call site with a "create contribution then sync" shim. Lowest friction for callers, introduces no new IPC commands, keeps the frontend stable. Cost: every old call site grows by 5 lines and has to handle the extra error path.
>
> b. **Deprecate legacy writers and force all writes through Phase 4 IPCs.** Remove `pyramid_save_tier_routing`, `POST /pyramid/:slug/dadbear/watch`, etc. Force the frontend to call `pyramid_create_config_contribution` with `schema_type = "tier_routing"` / `"dadbear_policy"`. Highest architectural purity, highest frontend churn.
>
> c. **Defer: accept the coexistence and document that legacy writers produce operational rows with `contribution_id = NULL`.** Add a runtime migration that re-runs per boot and retroactively creates contributions for orphaned operational rows (removing the sentinel-marker short-circuit and replacing it with per-row guards).
>
> Option (c) is the path of least resistance but punts the invariant enforcement forever. Option (a) is probably right for Phase 4.5 / Phase 9. Option (b) is the right end-state but needs coordinated frontend work.
>
> Separately: **`seed_default_provider_registry` should either seed via `bundled` contributions or be explicitly documented as a pre-contribution initial state.** The first boot of a fresh node currently has 4 tier_routing rows with NO contribution provenance. If the "contribution-first" invariant is taken seriously, seed data should itself be a `source = 'bundled'` contribution that syncs on first boot. This matches the Phase 4 bootstrap migration pattern and gives seed data the same audit trail as user-created data.
>
> Finally: the Phase 4 bootstrap migration's idempotency guard (sentinel marker) is a one-way latch. Once the marker exists, any direct-write DADBEAR row that lands afterward never gets a contribution. If Phase 4.5/9 adopts option (c) above, the marker needs to be per-row rather than global.

---

## Phase 5 wanderer pass — 2026-04-10

**Context:** Joined phase-5-wire-contribution-mapping after 45b440a + d18a495 (implementer + verifier commits) to trace end-to-end execution with fresh eyes and catch anything a punch-list verifier would miss. Phase 4's wanderer caught `sync_config_to_operational` being dead code (9 IPC handlers, zero callers); the assignment was to look for the same pattern in Phase 5.

### Finding A — CRITICAL: `PromptCache` was entirely dead code (FIXED)

**What it was:** Phase 5 shipped four new modules, ~4,300 lines of code, 65 new tests. The stated goal per the workstream prompt:

> "The existing `chain_loader::load_prompt` path should transparently hit the cache first, falling back to disk for files not yet migrated (rare — should only happen for chains that land AFTER first-run migration)."

**Reality:** `src-tauri/src/pyramid/chain_loader.rs:55` (`resolve_prompt_refs`) still read prompts straight from disk via `std::fs::read_to_string(&prompt_path)`, unchanged since before Phase 5. The `PromptCache` module existed, had tests, was invalidated by the dispatcher on skill contribution sync — but zero production code ever called `PromptCache::get()` or `resolve_prompt_from_store()`. A repo-wide grep for `prompt_cache::` turned up only:

- `config_contributions.rs:775` — the `invalidate_prompt_cache()` hook (which cleared a cache nobody read from)
- `prompt_cache.rs` itself and its tests
- `wire_migration.rs:51` — a `use` statement pulling in `normalize_prompt_path`

The chain executor, chain loader, build runner, parity module, preview module, and chain publish module all called `chain_loader::load_chain()` which called `resolve_prompt_refs()` which hit disk. The Phase 5 migration successfully wrote `skill` contributions to `pyramid_config_contributions` on first run, and those rows were completely unused by the runtime. The entire 4,300-line Phase 5 effort was cosmetic from the chain execution perspective.

**How this happened:** The implementer wrote `PromptCache`, wrote the migration that populates it, wrote the dispatcher invalidation hook, wrote comprehensive tests for each piece, and wrote their own wanderer prompt that said "populate the prompt cache on first lookup, serve a skill contribution's body through the chain loader". They never actually modified `chain_loader::resolve_prompt_refs` to consult the cache. The punch-list verifier focused on spec compliance (canonical parity, round-trip tests, 28-slot allocation edge cases, migration idempotency) and didn't trace the hot path: "what line of code reads a prompt on chain execution?"

**Root cause:** Same Phase 4 wanderer-caught pattern. Helper function exists and is tested → punch list passes → helper never gets called from production → entire phase is cosmetic. The verifier validated every module in isolation but not the boundary between them.

**Fix:** Added `set_global_prompt_cache_db_path()` + `resolve_prompt_global()` to `prompt_cache.rs`. The global resolver opens ephemeral reader connections from a stashed `pyramid.db` path on cache miss, warms the cache, and returns the body. This keeps the chain loader signature unchanged (no `&Connection` threading through 9+ call sites). `main.rs` stashes the path once during app setup after `init_pyramid_db`. `chain_loader::resolve_prompt_refs` now consults the global resolver first and falls back to disk on not-found/error. Added 2 new tests: `global_resolver_returns_none_when_path_unset` and `global_resolver_hits_stashed_db_when_set`.

**Detection method:** Grep for `prompt_cache` across all Rust files in `src-tauri/src/pyramid/`. Expected: chain_executor and chain_loader would import prompt_cache. Actual: neither file contained the string "prompt_cache". That was the smoking gun.

**Lesson for future phases:** When a phase ships a new cache or lookup layer, the verifier MUST grep for imports of the new module in every plausible hot path file. If the new module is only imported by its own tests and one dispatcher hook, it's dead code. A "does it test cleanly in isolation" pass does not catch this.

### Finding B — MEDIUM: Phase 4 DADBEAR bootstrap migration wrote `'{}'` metadata (FIXED)

**What it was:** `src-tauri/src/pyramid/db.rs:1543` — the Phase 4 DADBEAR migration path that converts legacy `pyramid_dadbear_config` rows to `pyramid_config_contributions` rows — hardcoded `wire_native_metadata_json = '{}'` in a direct SQL INSERT. This bypassed the `config_contributions.rs` helpers that Phase 5 updated to write canonical metadata.

**Spec violation:** `docs/specs/wire-contribution-mapping.md` → Creation-Time Capture table, line 361:

> "Bootstrap migration from legacy tables | Empty defaults. `maturity` = `canon`. `description` via prepare LLM on first publish."

Phase 5's implementation log claimed:

> "Phase 4's creation paths in `config_contributions.rs` now populate `wire_native_metadata_json` with schema-type-appropriate canonical defaults instead of the `'{}'` stub."

The claim is half-true: the helpers in `config_contributions.rs` were updated, but `db.rs:1543` (which predates the helpers and is a direct INSERT) was never touched. The Phase 5 implementer missed this because they only searched for `'{}'` inside `config_contributions.rs`, not the whole repo.

**Effect at runtime:** Legacy DADBEAR rows migrated during Phase 4 bootstrap land in `pyramid_config_contributions` with placeholder metadata. `WireNativeMetadata::from_json("{}")` gracefully falls back to `default_wire_native_metadata()` which sets `maturity: Draft`. As a result, `pyramid_publish_to_wire` refuses to publish these rows with the error "contribution maturity is `draft` — promote to design/canon before publishing". The degradation is graceful (no panics, no data loss) but the spec's intent is violated — bootstrap-migrated contributions should be immediately publishable with `maturity: canon`.

**Fix:** Updated `migrate_legacy_dadbear_to_contributions` to build a canonical `WireNativeMetadata` via `default_wire_native_metadata("dadbear_policy", Some(slug))`, override `maturity` to `Canon`, serialize to JSON, and use the serialized JSON in the INSERT. Added a test `phase5_dadbear_migration_writes_canonical_metadata_not_empty_json` that inserts a legacy DADBEAR row, runs the migration, and asserts the resulting contribution has canonical metadata with `maturity: Canon` and `contribution_type: Template`.

**Detection method:** Repo-wide grep for `INSERT INTO pyramid_config_contributions`, then read each match and check whether it writes `wire_native_metadata_json = '{}'`. Expected: zero matches. Actual: 2 matches outside test code — one in `wire_migration.rs:269` (the Phase 5 sentinel write, which is correct — sentinel rows don't need metadata) and one in `db.rs:1543` (the Phase 4 DADBEAR migration, which is wrong).

**Lesson for future phases:** When a phase says "every creation path now writes canonical metadata", the verifier must grep for ALL direct `INSERT INTO pyramid_config_contributions` statements, not just check the helpers in the expected file. Legacy direct-insert paths are the places where half-fixes hide.

### Finding C — NON-BLOCKING: Spec file still has old incorrect struct definitions

**What it is:** `docs/specs/wire-contribution-mapping.md` has Rust struct definitions (e.g., `WireScope` as tagged enum, `WireRef` with a tagged enum of reference kinds, `supersedes: Option<WireRefKey>`) that diverge from the canonical YAML schema in `GoodNewsEveryone/docs/wire-native-documents.md`. The Phase 5 implementer correctly flagged three divergences in the implementation log and implemented the canonical-correct shapes in Rust, but left the spec file unchanged.

**Risk:** A future implementer who reads `docs/specs/wire-contribution-mapping.md` instead of the canonical will reintroduce the drift. The spec-as-source-of-truth assumption is broken for these struct shapes.

**Severity:** Non-blocking for runtime, blocking for future implementation correctness. Not fixed in this wanderer pass because the spec correction is a standalone editing task (no code change, no test) and the implementer explicitly flagged it for a follow-up pass.

**Recommendation:** A small spec correction PR should update the three struct definitions in the spec file to match the canonical + the Rust implementation. The impl log already contains the canonical-wins rationale for each divergence (log lines 968-977).

### What I did not fix

1. **Section decomposition publish path** — Phase 5 ships the `WireSectionOverride` type and the dry-run preview, but `publish_contribution_with_metadata` only publishes the top-level contribution. The impl log flagged this for a Phase 5.5 / Phase 9 follow-up. Since the spec explicitly carves this out for later and the economic graph is correct today (separate skill contributions + custom_chain with derived_from pointing at them), I left it alone.

2. **Live path→UUID resolution at publish time** — Phase 5's `resolve_derived_from_preview` computes the 28-slot allocation but marks every entry as `resolved: false`. The live path→UUID map is Phase 10's Wire discovery scope per the impl log. Left alone.

3. **JSON Schema validation of metadata** — Phase 5's `validate()` checks structural invariants (price/curve exclusion, 28-source cap, etc.) but does NOT run a JSON Schema check against the `schema_definition` contribution. Phase 9 provides the schemas per impl log. Left alone.

4. **Other legacy direct-insert paths** — Phase 4's wanderer pass (entry above) already enumerated 8 other direct-write paths that bypass the contribution table entirely. Those are out of Phase 5 scope and belong to Phase 4.5 / Phase 9 per the planner escalation.

### Commit

Single commit on `phase-5-wire-contribution-mapping` with message `phase-5: wanderer fix — PromptCache wire-up + DADBEAR canonical metadata`. Modifies 4 files (`chain_loader.rs`, `prompt_cache.rs`, `main.rs`, `db.rs`) and adds 3 new tests. No other changes.

### Verification after fix

- `cargo check --lib` — clean, 3 pre-existing warnings.
- `cargo test --lib pyramid::prompt_cache` — 8/8 passing (6 existing + 2 new).
- `cargo test --lib pyramid::wire_migration` — 6/6 passing (unchanged).
- `cargo test --lib pyramid::db::provider_registry_tests::phase5_dadbear_migration_writes_canonical_metadata_not_empty_json` — 1/1 passing (new).
- `cargo test --lib pyramid` — **923 passed, 7 failed** (same 7 pre-existing failures documented in Phase 2/3/4). Phase 5 implementer reported 919 passing post-implementation (854 → 919, +65 tests); verifier commit d18a495 added 1 test (920); wanderer fix adds 3 new tests (923). Zero regressions.

---

## Phase 6 wanderer pass — 2026-04-10

**Context:** Joined `phase-6-llm-output-cache` after 4d812a1 (implementer commit) to trace end-to-end execution with fresh eyes and catch anything a punch-list verifier would miss. Phase 4's wanderer caught `sync_config_to_operational` being dead code. Phase 5's wanderer caught `PromptCache` being dead code. The assignment was to look for the same pattern in Phase 6: "helper exists, tested in isolation, not reached by any production path."

### Finding A — EXPECTED (per brief, not a bug): chain_executor / chain_dispatch pipeline bypasses the cache entirely

**What it is:** Phase 6's `call_model_unified_with_options_and_ctx` is the cache-aware entry point. Its cache lookup gate is:

```rust
match ctx {
    Some(sc) if sc.cache_is_usable() => { /* cache read + write */ }
    _ => None, // cache bypassed
}
```

The ONLY production call site that constructs a fully-populated StepContext (resolved_model_id + non-empty prompt_hash) is `stale_helpers_upper::execute_supersession` → `generate_change_manifest`. That function is the Phase 2 retrofit proof-of-concept explicitly called out by the workstream prompt.

Every OTHER production LLM call site still goes through the legacy shim `call_model_unified_with_options` (line 396 of `llm.rs`), which delegates to `call_model_unified_with_options_and_ctx(config, None, ...)` — bypassing the cache entirely. The bypassed call sites include:

1. `chain_dispatch::dispatch_llm` (line 121) — the primary chain step dispatcher used for legacy v2 chains. Calls `call_model`, `call_model_audited`, `call_model_structured` — all legacy paths with no ctx.
2. `chain_dispatch::dispatch_ir_llm` (line 920) — the IR chain dispatcher used for v3 chains. Calls `call_model_unified_with_options(config_ref, system_prompt, ...)` and `call_model_audited(...)` — both legacy with no ctx.
3. `chain_dispatch::dispatch_ir_step` (line 877) → `dispatch_ir_llm` — this is the IR runtime's LLM step handler, reached from `chain_executor::dispatch_with_retry`.
4. `chain_executor` itself has zero direct `call_model_*` calls — it routes everything through `chain_dispatch`.
5. `call_model_via_registry` (line 1108) — Phase 3's registry-aware entry point with its OWN HTTP retry loop. Does NOT delegate to `call_model_unified_with_options_and_ctx`. The cache hook is not reachable from this function at all, regardless of whether the caller has a StepContext.
6. `evidence_answering.rs` — 4 call sites (`call_model_audited`, `call_model_unified`). None thread a StepContext.
7. `webbing.rs`, `delta.rs`, `meta.rs`, `faq.rs` — 11 call sites total across these files. All use `call_model`/`call_model_with_usage` legacy paths.
8. `characterize.rs`, `extraction_schema.rs`, `question_decomposition.rs`, `supersession.rs` — 7 call sites. All legacy.
9. `stale_helpers.rs`, `stale_helpers_upper.rs` (L0/edge/node stale check paths OTHER than `generate_change_manifest`) — 10 call sites. All legacy.
10. `build.rs`, `public_html/*`, `main.rs`, `routes.rs` — 6 call sites. All legacy.

The aggregate: roughly 50+ production LLM call sites, of which exactly ONE (`generate_change_manifest`) threads a cache-usable StepContext. Every other call — extraction, synthesis, webbing, evidence answering, recursive pairing, faq, delta, meta, supersession, etc. — costs real tokens every single time, even on identical re-runs of the same build.

**Why it's not a bug (scope-wise):** The phase-6-workstream-prompt.md explicitly says:

> "Retrofitting every other LLM call site (evidence triage, FAQ, delta, webbing, meta) — Phase 12 and later. Phase 6 only retrofits `generate_change_manifest` as the proof-of-concept."

And:

> "**No new scope.** Phase 6 is the cache primitive + the one StepContext retrofit proof-of-concept. Other retrofits are later phases."

The implementer did exactly what the workstream prompt said. The cache primitive is correctly implemented, the hook point is in the right place, the one retrofit works. Phase 6 shipping is spec-compliant with the brief.

**Why the spec and the workstream prompt disagree:** `docs/specs/llm-output-cache.md` frames the cache as universally applicable:

> "Crash recovery IS a cache hit — completed steps have valid cached outputs"
> "Re-running the same build with no changes: every step is a cache hit"
> "StepContext is the single context object threaded through all LLM-calling code paths."

Those spec guarantees ONLY hold for call sites that thread a StepContext. Post-Phase-6, exactly one site does. The spec-level guarantees are unreachable today for ~95%+ of production LLM calls. The brief's "gated per-phase" framing is the ground truth — the spec describes the eventual end state (post-Phase-12+), not the Phase 6 ship state.

**What I did:** Nothing — the scope boundary was explicit and the implementer respected it. Flagged for the planner below (see deviation block).

### Finding B — EXPECTED (per brief): `ChainContext.prompt_hashes` + `resolved_models` + the lazy helpers are dead code

**What it is:** Phase 6 added to `ChainContext`:

- `prompt_hashes: HashMap<String, String>` field
- `resolved_models: HashMap<String, String>` field
- `get_or_compute_prompt_hash(&mut self, path, body_provider)` lazy getter
- `cache_resolved_model(&mut self, tier, model_id)` setter
- `get_resolved_model(&self, tier)` getter

These match the spec's "Model ID Normalization" section exactly. The fields are initialized to empty HashMaps in `ChainContext::new()` and the helpers have 5 unit tests that exercise them in isolation.

A repo-wide grep for `get_or_compute_prompt_hash`, `cache_resolved_model`, `get_resolved_model`, `prompt_hashes.insert`, `prompt_hashes.get`, `resolved_models.insert`, and `resolved_models.get` returns only hits inside `chain_resolve.rs` itself (the struct/method definitions) and the 5 unit tests. **Zero production callers.** No chain executor step ever populates these caches; no LLM call site ever reads them.

**Additionally missing:** the spec calls out a `resolve_model_for_tier(ctx, tier_name)` helper that consults the provider registry on cache miss and writes the resolved id back to `ctx.resolved_models`. Phase 6 did NOT add this helper. It only added `cache_resolved_model(tier, model_id)` which takes BOTH the tier name and the already-resolved id as parameters — so a caller would need to call `ProviderRegistry::resolve_tier()` AND `ctx.cache_resolved_model()` separately. The "prevent drift mid-build" guarantee the spec promises isn't achievable with Phase 6's primitives alone; it requires the `resolve_model_for_tier` helper to be added later.

**Why it's not a bug (scope-wise):** The workstream prompt's "In scope" list says:

> "`ChainContext.prompt_hashes` + `ChainContext.resolved_models`"

...but says nothing about them being PRODUCTION-CALLED in Phase 6. The implementer added the storage primitive per the brief. Phase 12's per-call-site retrofits will wire up the callers.

**However:** Unlike Phase 4 and Phase 5's dead code (which were load-bearing for the phase's stated goal and the wanderer fixed in place), Phase 6's ChainContext dead code is a *forward-looking scaffold* with no Phase 6 user. It's cheap to carry (two empty HashMaps per ChainContext clone — ChainContext clones via Arc-counted fields, so it's really two Arc bumps). It's not a bug today; it becomes a bug only if Phase 12 lands without wiring them up.

**What I did:** Nothing. The pattern is consistent with the brief's "primitive today, callers in Phase 12+" framing. Flagged here so future-me (Phase 12 planner) remembers the scaffold exists.

### Finding C — MINOR: `resolve_model_for_tier` helper is missing from the spec's pattern

**What it is:** The spec at `llm-output-cache.md:178-187` shows:

```rust
fn resolve_model_for_tier(ctx: &mut ChainContext, tier_name: &str) -> Result<String> {
    if let Some(cached) = ctx.resolved_models.get(tier_name) {
        return Ok(cached.clone());
    }
    let model_id = provider_resolver::resolve_tier(tier_name)?;
    ctx.resolved_models.insert(tier_name.to_string(), model_id.clone());
    Ok(model_id)
}
```

This is the helper that actually makes the "resolved once per build, consistent across all cache writes" guarantee work. Phase 6 added `cache_resolved_model(tier, model_id)` (the writer side) and `get_resolved_model(tier)` (the reader side), but NO helper that goes through the provider registry and writes back on miss. A future retrofit that calls `cache_resolved_model` explicitly will work; a retrofit that assumes "the build-scoped resolution is transparent, I just call cache_resolved_model by hand and it happens to be consistent" can still drift if the caller forgets.

**Severity:** Low. The drift only happens if two sites in the same build resolve the same tier independently — possible but unusual. Current state: zero callers exist, so zero drift risk today.

**Impact when Phase 12 lands:** Phase 12's first retrofit will need either (a) to also add `resolve_model_for_tier` and use it consistently, or (b) to manually call `ProviderRegistry::resolve_tier` and `ctx.cache_resolved_model` in sequence at every site. Option (a) is cheaper and matches the spec's intent — worth adding to the Phase 12 scope explicitly.

**What I did:** Noted here. The helper addition is trivial (~10 lines) but would touch `chain_resolve.rs` which means declaring a circular dependency on `provider.rs` or requiring the caller to pass `&ProviderRegistry` into the helper. Phase 12 can design this properly when it has a concrete retrofit call site in hand. Not worth a blind scaffold in Phase 6.

### Finding D — INFORMATIONAL: `cache_build_id` format uses current build_version; can desync with the executing build

**What it is:** The retrofit in `execute_supersession` builds `cache_build_id` as `format!("stale-{node_id}-{build_version}")` where `build_version` is the node's CURRENT version (the pre-stale state). When the stale check runs, the new version that will eventually be written is `current_build_version + 1`. The `cache_build_id` column on the cache row will reflect the PRE-stale version, not the POST-stale version. The cache_key itself is content-addressable (unaffected by build_id) so the cache hit/miss behavior is correct, but the provenance column logs the "incoming" version.

**Impact:** None on correctness. Potential confusion for Phase 13's oversight UI when it groups cache rows by build_id: a stale check's cache row will be grouped under the pre-stale build rather than the post-stale one. Phase 13 scope, not Phase 6.

**What I did:** Noted here as a Phase 13 awareness item.

### Finding E — INFORMATIONAL: `load_change_manifest_prompt_body` reads from disk at call time with CWD-relative paths

**What it is:** `stale_helpers_upper::load_change_manifest_prompt_body()` tries `"chains/prompts/shared/change_manifest.md"` then `"../chains/prompts/shared/change_manifest.md"` at call time and falls back to the static string constant on failure. This means:

1. **CWD dependence:** The function's return value depends on the current working directory when it runs. In a dev tree run from repo root, it finds the file. In a production Tauri app bundled with no `chains/` resource, it falls back to the static string. Both return the SAME content WITHIN a run (the function is called twice per supersession: once by `execute_supersession` to compute the hash, once by `generate_change_manifest` to build the user prompt — both calls are microseconds apart so CWD/filesystem state is stable).
2. **Cross-process cache invalidation on dev→prod transition:** If the same `.db` file moves from dev to prod (e.g., the user runs the bundled app after running `cargo tauri dev`), the prompt_hash in `pyramid_step_cache` rows from the dev run will no longer match the current prompt_hash in the prod run (file was read vs. static fallback). Every dev-era cache row becomes a `MismatchPrompt` verification failure on first prod hit → gets deleted, re-run, re-stored. Functionally correct, just wastes the old rows.
3. **Phase 0b friction log already flagged the broader missing-chains-in-bundle issue:** see entry "2026-04-10 — Pre-existing: release-mode chain bootstrap gap (conversation-episodic not embedded)". This Phase 6 concern is the cache-side manifestation of the same problem.

**Impact:** Low. Cache is self-correcting via verification. The only measurable cost is one extra HTTP call per previously-cached change-manifest row on first run after a dev→prod transition.

**What I did:** Noted here. Fix belongs with the Phase 0b distribution-bundling follow-up.

### Finding F — INFORMATIONAL: `block_in_place` is safe under Tauri's multi-threaded runtime but not under `current_thread` runtimes

**What it is:** `call_model_unified_with_options_and_ctx` uses `tokio::task::block_in_place` for all cache read/write DB operations (four call sites in `llm.rs`). `block_in_place` is documented as only valid on a multi-threaded Tokio runtime — calling it on a `current_thread` runtime panics. Tauri's `async_runtime::spawn` uses the multi-threaded variant by default, so production is fine.

**Risk in tests:** Two tests in the repo build their own `current_thread` runtime (`dadbear_extend.rs::test_*` at lines 1747/1983 and `public_html/integration_tests.rs:27`). None of them call the cache code path, but a future test that bridges a `current_thread` runtime with the cache will panic at the first `block_in_place`. The Phase 6 tests correctly use `#[tokio::test(flavor = "multi_thread", worker_threads = 2)]`. Any new cache test that forgets this will panic at runtime rather than fail cleanly.

**Impact:** Zero today. Trap for future test writers.

**What I did:** Noted here. The better long-term pattern is `spawn_blocking` rather than `block_in_place`, but that changes the call shape (returns a JoinHandle, needs to await, doesn't nest cleanly inside the existing match arms). Not worth refactoring pre-ship.

### Finding G — INFORMATIONAL: Archive key format is collision-safe; supersede path handles no-prior-entry case cleanly

**What it was checked:** The `supersede_cache_entry` path moves a prior row's `cache_key` from the content-addressable form (64-char SHA-256 hex) to `archived:{id}:{orig_key}`. The concern was whether:

1. A content-addressable lookup could accidentally return an archived row. **No** — `check_cache` does an exact match (`WHERE slug = ?1 AND cache_key = ?2`), and a real cache_key is never prefixed with `archived:`. The `idx_step_cache_key` index on `cache_key` is a btree so prefix-based lookups don't match archived rows either.
2. The no-prior-entry case panics or misbehaves. **No** — lines 5935-5949 of `db.rs` handle it cleanly: `prior_row` is `None`, `(prior_id, archival_cache_key)` is `(None, None)`, the new entry is stored directly without an archival mutation. Tested in `db::step_cache_tests::test_supersede_with_no_prior_entry` (I confirmed the test exists).
3. The archive rollback on store failure leaves the prior row in a valid state. **Yes** — lines 5959-5968 of `db.rs` check for store errors and re-UPDATE the prior row's cache_key back to its original. The rollback runs even on transient SQL errors.

**What I did:** Just verified the implementer's existing test covered it. No friction; this path is solid.

### Verification

- `cargo check --lib` — clean (same 3 pre-existing warnings — deprecated evidence helpers + LayerCollectResult visibility).
- `cargo test --lib pyramid::step_context` — 15/15 pass.
- `cargo test --lib pyramid::llm::tests` — 14/14 pass (10 existing + 4 new Phase 6 cache tests).
- `cargo test --lib pyramid::stale_helpers_upper::tests` — 11/11 pass (10 Phase 2 + 1 new Phase 6 retrofit compile-check).
- `cargo test --lib pyramid::chain_resolve::tests` — 38/38 pass (33 existing + 5 new Phase 6 — all of them exercise the dead-code helpers in isolation).
- `cargo test --lib pyramid::db::step_cache_tests` — 13/13 pass.
- `cargo test --lib pyramid::step_context::tests::test_compute_cache_key_stable_across_runs` — verified deterministic across 3 separate runs. SHA-256 stability holds.
- Grep audit: `grep -rn "step_context" src-tauri/src/pyramid/chain_executor.rs src-tauri/src/pyramid/chain_dispatch.rs src-tauri/src/pyramid/chain_engine.rs` returned ZERO hits. Chain executor has no knowledge of Phase 6's StepContext type.

### What I did not fix

Nothing. All seven findings are either (a) expected per the brief's scope boundary (A, B, C) or (b) informational awareness items for future phases (D, E, F, G). The implementer's Phase 6 work is correct against the workstream prompt as written. The concern is whether the workstream prompt's scope boundary is correct against the spec.

Zero code changes from this wanderer pass. Zero new tests. The friction log entry + deviation block below is the artifact.

### Deviation block — Phase 6 scope boundary question

> [For the planner]
>
> Phase 6 shipped the cache primitive (table, CRUD, StepContext, hook, verify_cache_hit, supersede) and retrofitted exactly ONE call site (`generate_change_manifest`) per the workstream prompt. The implementer respected the scope boundary and the code is correct. I am not escalating a bug — I am escalating a scope-boundary decision that needs planner direction before Phase 12.
>
> **The question:** The spec (`docs/specs/llm-output-cache.md`) promises the cache works for "every step" and "every LLM call" — "Crash recovery IS a cache hit", "Re-running the same build with no changes: every step is a cache hit." Those promises are UNREACHABLE today because ~50+ production LLM call sites still use the legacy shim path that passes `None` for the ctx. The cache is demonstrably reachable end-to-end for exactly one path (stale-engine → execute_supersession → generate_change_manifest → call_model_unified_with_options_and_ctx with a cache-usable ctx), which the Phase 6 tests prove rigorously.
>
> **Concrete breakdown of current cache coverage:**
>
> - **Cache-reachable today (1 path):** Change-manifest generation inside DADBEAR stale-check supersession. Fires once per L1+ node per confirmed stale event, maybe 10-100 times per build cycle depending on staleness churn.
> - **Cache-unreachable but conceptually easy to retrofit (~5 paths):** `chain_dispatch::dispatch_ir_llm`, `chain_dispatch::dispatch_llm`, `evidence_answering`, `webbing`, `meta`. These have a well-defined step name, chunk index, and build_id in scope at the call site; threading a StepContext through them is mostly signature churn. Aggregate: the vast majority of production LLM traffic during a fresh build.
> - **Cache-unreachable and harder to retrofit (~20 paths):** `stale_helpers.rs` / `stale_helpers_upper.rs` (the L0 and edge stale-check paths that aren't `generate_change_manifest`), `faq.rs`, `delta.rs`, `characterize.rs`, `extraction_schema.rs`, `question_decomposition.rs`, `supersession.rs`. These are deeper in the call graph and many don't have a clean "this is the step_name / chunk_index" concept. They're also less hot-path — they run on specific triggers, not every build step.
> - **Intentionally bypassed forever (~5 paths):** `call_model_direct` (diagnostics), `public_html` routes (free-form ask), `routes.rs` semantic search path, `build.rs` legacy path. These are not "steps" and the cache shape doesn't fit them.
>
> **What the brief says:** Phase 12 owns the "Retrofitting every other LLM call site (evidence triage, FAQ, delta, webbing, meta)". So the sequencing is: Phase 6 builds the primitive, Phase 12 sweeps all call sites through the cache.
>
> **What I think the planner should consider:**
>
> 1. **Is the Phase 6 → Phase 12 distance acceptable?** Phase 12 is 6 phases away. During phases 7/8/9/10/11, the node is building with a cache that's 95% cold even on re-runs. If a user kicks off a fresh build, crashes mid-way, and restarts, every extraction / synthesis / web step re-runs with real token cost — the spec's "crash recovery IS a cache hit" promise doesn't hold yet. That's fine IF the user expectation is "the cache goes live in Phase 12" but it's a gap vs the spec text.
> 2. **Should Phase 6.5 retrofit the IR chain dispatcher as a second proof-of-concept?** `dispatch_ir_llm` is the ONE function in `chain_dispatch.rs` whose caller chain (chain_executor → dispatch_with_retry → dispatch_step → dispatch_ir_step → dispatch_ir_llm) would transparently pick up cache coverage for ~90% of production LLM traffic during build execution. The retrofit would be:
>    - Add `cache_ctx: Option<Arc<pyramid::step_context::StepContext>>` to `chain_dispatch::StepContext` (the EXISTING dispatch-context struct — not a rename, a new field).
>    - `chain_executor::execute_chain_from` builds one `pyramid::step_context::StepContext` per build entry and clones-with-overrides for each step (swapping step_name, depth, chunk_index).
>    - `dispatch_ir_llm` and `dispatch_llm` thread the optional ctx through to `call_model_unified_with_options_and_ctx` instead of the legacy shim.
>    - One new test that runs a two-step IR chain twice and asserts the second run is a cache hit.
>    - Phase 9's eventual config-contributions work for temperature/max_tokens is orthogonal and doesn't conflict.
>    
>    Cost estimate: ~200 LOC across `chain_dispatch.rs`, `chain_executor.rs`, `chain_resolve.rs` (to actually populate `prompt_hashes` + `resolved_models` on chain step entry — which BTW validates Finding B's dead-code concern by finally wiring the helpers).
>
> 3. **Should the spec be amended to reflect the phased rollout?** Currently the spec reads as if the cache just works. If the planner's intent is "Phase 12 is where it goes live", the spec's "Crash recovery IS a cache hit" paragraph should be moved to a "Phase 12 end-state" section, or the spec should be labeled as "describes the post-Phase-12 state, not the Phase 6 ship state". Readers (including the next wanderer and future me) shouldn't have to reconcile the conflict themselves.
>
> 4. **Should Phase 6 add the `resolve_model_for_tier` helper (Finding C)?** The current primitives (`cache_resolved_model` + `get_resolved_model`) store and retrieve but don't enforce "go through provider registry on cache miss". Adding a `resolve_model_for_tier(&mut ChainContext, &ProviderRegistry, tier_name) -> Result<String>` helper is ~10 LOC and makes the drift-prevention guarantee achievable. Phase 12's retrofits will need to call it; adding it now means the Phase 12 prompt can say "use this helper at every LLM call site" rather than "implement this helper THEN use it". Low cost, high clarity dividend.
>
> **My inclination (NOT a recommendation — the planner decides):**
>
> - **Option A (minimal):** Ship Phase 6 as-is. Phase 12 does the full sweep. Spec gets a "Phase 6 → Phase 12 rollout" section explaining the gap. Zero code changes in this wanderer pass.
> - **Option B (proof-of-concept expansion):** Phase 6.5 retrofits `dispatch_ir_llm` as a second POC, adds `resolve_model_for_tier`, populates `ChainContext.prompt_hashes` / `resolved_models` at chain step entry. Validates the full cache plumbing end-to-end with the most common production chain step type. Everything else still waits for Phase 12. ~200 LOC + 3-5 new tests.
> - **Option C (accept the scope boundary but tighten the spec):** Ship Phase 6 as-is, no code change, but update `llm-output-cache.md` to label the universally-applicable guarantees as "Phase 12+ end state" and the current-state guarantees as "only for generate_change_manifest in Phase 6". Spec-only change, no code.
>
> Option A is the default per the brief. Option B is the Phase 4/5 wanderer pattern applied to Phase 6. Option C is the documentation-only fix.
>
> I did NOT apply any of these because all three cross the "new scope" line in the workstream prompt. The Phase 4 wanderer fix was different — it was fixing a spec-stated invariant that was broken in code; the code had to match the spec. Phase 6's case is different: the spec describes an end state, the workstream prompt scopes the current state, and the gap between them is INTENTIONAL per the brief. Closing the gap is a planner decision, not a wanderer decision.
>
> **What I did do:** Documented findings A-G in this log entry. No commit. The Phase 6 implementer's work stands.
>
> Separately: whichever option the planner picks, **Phase 12's workstream prompt should explicitly require the implementer to grep for every `call_model_*` call site in the repo and thread a StepContext through it, and the verifier should grep for `call_model_unified_with_options` vs `call_model_unified_with_options_and_ctx` to confirm the ratio flips.** The Phase 6 wanderer protocol (me) is looking at the problem one phase too late — the fix belongs at Phase 12 planning time, not as a reactive sweep.

### Commit

None. Zero code changes from this wanderer pass itself.

### Verification after fix

N/A — no fix applied by the wanderer.

### Conductor follow-up (2026-04-10) — Option B applied

After consulting the `feedback_no_integrity_demotion` memory ("Never demote security/integrity features just because a primary path exists") and Adam's explicit "avoid temptation to say 'Well this is most of the work, we'll defer it' — temptation from the old world" framing, the conductor dispatched a fix agent with the scope: **Option B — retrofit `dispatch_ir_llm` to actually reach the cache**.

The fix landed across three files:

- **`src-tauri/src/pyramid/chain_dispatch.rs`** — added a `CacheDispatchBase` struct (per-build shared cache state: db_path, build_id, bus, lazy `prompt_hashes` + `resolved_models` HashMaps) and `cache_base: Option<Arc<CacheDispatchBase>>` field on the existing dispatch `StepContext`. `dispatch_ir_llm` now constructs a per-call `pyramid::step_context::StepContext` from `ctx.cache_base` and threads it through to `call_model_unified_with_options_and_ctx`. `dispatch_llm` (legacy v2 path) is intentionally left at `cache_base: None` per the scope boundary.
- **`src-tauri/src/pyramid/llm.rs`** — `call_model_via_registry` now routes through the ctx-aware variant when a StepContext is present, so the Phase 3 registry-aware path gets the same cache coverage as the shim path.
- **`src-tauri/src/pyramid/chain_executor.rs`** — the three `chain_dispatch::StepContext` initializer sites (main `execute_chain_from`, IR `execute_plan`, and the dead-letter retry path) now construct a `CacheDispatchBase` via `state.data_dir.as_ref().map(|dir| Arc::new(CacheDispatchBase::new(dir.join("pyramid.db")..., build_id.clone(), Some(state.build_event_bus.clone()))))`. Tests with `data_dir: None` cleanly bypass the cache. The dead-letter retry path uses `cache_base: None` explicitly — dead-letter retries shouldn't cache-hit prior failed attempts.

Also fixed: two test fixtures in `chain_dispatch.rs` (`test_dispatch_ir_mechanical_routes_correctly`, `test_dispatch_ir_step_mechanical_routes`) missing the new `cache_base` field.

**Net effect:** production chain execution (both the v2 chain engine path and the IR executor path) now builds with the cache live. A re-run of the same chain with unchanged inputs produces cache hits on every step that goes through `dispatch_ir_llm`. The spec's "Re-running the same build with no changes: every step is a cache hit" promise now holds for the primary production path.

**Verification:**
- `cargo check --lib` — clean, 3 pre-existing warnings.
- `cargo test --lib pyramid` — 961 passed, 7 failed (same 7 pre-existing failures: `test_evidence_pk_cross_slug_coexistence`, `real_yaml_thread_clustering_preserves_response_schema`, 5× `staleness::tests::*`). Zero new failures.

**Out of scope (still deferred to Phase 12):** `dispatch_llm` (legacy v2 chain path), `evidence_answering`, `faq`, `delta`, `webbing`, `meta`, `characterize` — these still call the legacy shim. Phase 12's workstream prompt will explicitly require the implementer to grep every `call_model_*` call site and thread a StepContext.

**Commit:** (below — conductor is about to `git add` and commit this block together with the code changes)

---

## Phase 7 wanderer pass — 2026-04-10

**Context:** Joined `phase-7-cache-warming-import` after `51eff38` (implementer commit) + `566f3f4` (verifier fix for `INSERT OR IGNORE`) to trace end-to-end execution with fresh eyes. Phase 4's wanderer caught `sync_config_to_operational` being dead code; Phase 5's wanderer caught `PromptCache` being dead code; Phase 6's wanderer caught the cache being unreachable from `dispatch_ir_llm`. Looking for the same class of bug: "the helper exists, the test in isolation passes, but the production wiring is wrong or absent."

### Finding A — BLOCKING (FIXED): re-import creates duplicate active dadbear_policy contributions

**What it is:** The implementer's `enable_dadbear_via_contribution` calls `create_config_contribution_with_metadata("dadbear_policy", Some(slug), ..., "active", ...)` unconditionally on every import. The contributions table has no UNIQUE constraint preventing two `status='active'` rows for the same `(slug, schema_type)`, so a second call lands a SECOND active contribution. The `sync_config_to_operational` dispatcher then overwrites `pyramid_dadbear_config.contribution_id` with the new contribution_id, leaving the prior active contribution row dangling — still `status='active'` but no operational row points at it. `load_active_config_contribution` returns the most recent (`ORDER BY created_at DESC LIMIT 1`) so subsequent reads still work, but the audit trail and supersession chain are silently broken.

The implementer's `test_import_pyramid_resume_same_pyramid_succeeds` test was the closest existing coverage; it asserted the cache row count was unchanged after the second import but did NOT count active dadbear_policy contribution rows. The bug slipped through because the contributions table doesn't enforce the invariant at the schema level — it has to be enforced at the call site.

This is the same class of bug Phase 4's wanderer caught: a downstream invariant ("there is at most one active contribution per schema_type+slug at any time") that nobody enforces from the ingress point because the schema doesn't require it. Phase 4's wanderer flagged six bypass paths in the friction log under "non-blocking, deferred"; Phase 7's import path was almost a SEVENTH bypass — except the bypass was self-inflicted by the resume idempotency model rather than legacy behavior.

**Detection method:** Wrote a wanderer regression test (`test_import_pyramid_resume_does_not_duplicate_dadbear_contributions`) that:
1. Calls `import_pyramid` once → counts active `dadbear_policy` rows for the slug → asserts 1.
2. Calls `import_pyramid` again with the same slug + same wire_pyramid_id (the spec's "resume" path) → counts active rows → asserts 1.

The test FAILED against the as-shipped Phase 7 code with `expected 1, got 2` confirming the bug. With the fix in place, the test passes.

**What I did about it:** Modified `enable_dadbear_via_contribution` to first check `load_active_config_contribution(conn, "dadbear_policy", Some(target_slug))` and, if an active row already exists, re-sync it through `sync_config_to_operational` instead of creating a fresh contribution. This is the "check before insert" pattern Phase 4's wanderer fix applied to its 4 IPC handlers, adapted for the Phase 7 import path. The re-sync re-asserts the operational row's `contribution_id` FK so it stays consistent with the active contribution. Re-import is now genuinely idempotent: the contributions table sees one active row regardless of how many times the slug is imported.

**Lesson:** The "everything is contribution" pattern needs the same defensive check at every callsite that creates an active row. The contributions table doesn't enforce single-active-per-slug, so it has to be enforced by the writer. Phase 4's wanderer caught this for IPC handlers; Phase 7's wanderer caught it for the import path. Phase 9/10/11 implementers should grep for `create_config_contribution_with_metadata.*"active"` in any new code path and confirm there's a check-before-insert wrapper, or the bug pattern will recur.

### Finding B — BLOCKING (FIXED): cancel does not roll back partial cache rows

**What it is:** The spec at `docs/specs/cache-warming-and-import.md` "Cleanup" section ~line 345 is explicit:

> "On explicit user cancel, the row is deleted along with any partially inserted cache entries and the target slug's DB rows."

The implementer's `pyramid_import_cancel` IPC handler in `main.rs` only called `db::delete_import_state`, with a comment that explicitly contradicted the spec:

```rust
//   pyramid_import_cancel(target_slug)
//     Deletes the in-flight import state row. Does NOT touch the
//     populated cache — idempotent cache rows remain valid even if the
//     user cancels mid-way (they're still content-addressable).
```

The implementer's reasoning ("cache rows are content-addressable, so they don't need cleanup") was wrong: even if the rows are valid, leaving them behind contradicts the user's "cancel" intent. A user who cancels mid-import expects the slug to return to its pre-import state. Having cache rows linger means a subsequent import of a DIFFERENT pyramid into the same slug would observe unexpected hit rates from the cancelled prior import (since the cache rows are slug-scoped, not pyramid-id-scoped). It also means "cancel" leaves a long tail of orphaned data on disk that the user has no UI to clean up.

The impl log's "Spec adherence" section claims `✅` against every spec contract, including "IPC contract". The cancel deviation was not flagged as a deferred concern.

**Detection method:** Read the spec's "Cleanup" section and the IPC handler comment side-by-side. The handler's stated behavior literally contradicts the spec text. The implementer's `ImportCancelResponse` struct also only had a `cancelled: bool` field — the spec's IPC contract section listed `partial_rollback: bool` as part of the response, which the response struct didn't include. Two corroborating tells.

**What I did about it:** Added a proper `cancel_pyramid_import` function to `pyramid_import.rs` that:

1. Loads the import state row to confirm it exists (recorded in the report so the IPC handler can distinguish "cancelled in-flight import" from "no-op cancel of a slug never imported").
2. Queries `pyramid_step_cache` for distinct `build_id` values starting with `import:` for the target slug.
3. Deletes every cache row matching those build_ids.
4. Deletes the import state row.

The cancel filter is **build_id-scoped** (`WHERE build_id LIKE 'import:%'`), NOT slug-wide — this preserves any cache rows that local LLM calls or rerolls wrote between import attempts. A regression test (`test_cancel_pyramid_import_preserves_non_import_cache_rows`) plants a "local-build-7" row alongside the imported rows, calls cancel, and asserts the local row survives.

The IPC handler now returns `ImportCancelResponse { cancelled, state_row_existed, cache_rows_rolled_back }` so the frontend can confirm both the deletion count and whether the cancel was a no-op.

**What I did NOT touch:** the DADBEAR contribution that the import created. Deleting a contribution from outside the contribution path bypasses the pattern, and Phase 4's wanderer findings explicitly flagged direct contribution writes as the anti-pattern. The user can disable DADBEAR through the existing oversight UI which creates a properly-superseded contribution. Documented in the function header.

### Finding C — INFORMATIONAL: progress polling is binary (0% then 100%), not incremental

**What it is:** The spec at line 405 says "`pyramid_import_progress` is polled by the frontend during the import" with a weighted progress formula:

```
progress = (nodes_processed / nodes_total) * 0.5 + (cache_entries_validated / cache_entries_total) * 0.5
```

The implementation initializes `nodes_processed` and `cache_entries_validated` to 0 in `create_import_state`, then bumps them to `nodes_total` / `cache_entries_total` in a SINGLE `db::update_import_state` call AFTER `populate_from_import` finishes. There is no incremental update inside `populate_from_import` — the loop runs all three passes and only writes the counters at the end.

In production, this means:
- Frontend polls `pyramid_import_progress` every 500ms.
- For the entire duration of the populate pass (could be seconds for a 100+-L0 pyramid), the response shows `status="validating_sources"`, `progress=0.0`.
- A single tick later, the response shows `status="populating_cache"`, `progress=1.0`.
- Then `status="complete"`.

The user sees a stuck "0% — validating sources" for the import duration, then a flash to 100%. Functionally the import works; the progress IPC is just non-functional for its stated purpose.

**Severity:** Informational — the import is correct, the IPC just doesn't surface useful data. Phase 10's wizard will need this fixed before the import-progress UI is meaningful.

**What I did about it:** Did NOT fix in this wanderer pass. The fix requires plumbing periodic `db::update_import_state` calls into the inner loops of `populate_from_import` (probably every N nodes), which is invasive enough that it deserves its own focused pass with the Phase 10 wizard's polling cadence in view. Flagged here so Phase 10 implementer (or a Phase 7.5 follow-up) knows the IPC plumbing exists but reports stale data.

### Finding D — INFORMATIONAL: content_type defaults to 'document' regardless of source pyramid type

**What it is:** `enable_dadbear_via_contribution` hardcodes `content_type: document` in the YAML it builds. The `pyramid_dadbear_config.content_type` column has a CHECK constraint `IN ('code', 'conversation', 'document')`. If the imported pyramid is actually a `code` pyramid or a `conversation` pyramid, the imported DADBEAR config will mis-classify it and DADBEAR's tick loop will use document-flavored heuristics for code/conversation files.

The implementer documented this in a code comment:

```rust
// `content_type` is required by the operational table but not part of
// the spec's auto-enable shape — we default to `document` since the
// manifest doesn't carry the source's declared content type. Phase 10's
// wizard can override.
```

The cache manifest format in the spec (~line 151) doesn't carry a top-level `content_type` field. The source pyramid's content_type IS knowable to the publisher (it lives on `pyramid_slugs.content_type`) but the publisher's `build_cache_manifest` doesn't currently emit it. Phase 10's wizard could ask the user, but for an auto-enable post-import, the right fix is for the manifest to carry the source pyramid's content_type and for the importer to use it.

**Severity:** Informational — the workaround is for the user to manually fix the dadbear config after import. Not a Phase 7 blocker but a quality-of-import issue.

**What I did about it:** Did NOT fix. The fix requires:
1. Adding `content_type` to the `CacheManifest` struct (additive — backwards compatible).
2. Updating `build_cache_manifest` to populate it from `pyramid_slugs.content_type`.
3. Updating `enable_dadbear_via_contribution` to read it from the manifest instead of hardcoding `"document"`.

That's a 3-file change that's worth doing in a focused follow-up rather than a wanderer fix-pass. Flagged for the Phase 7.5 / Phase 10 implementer.

### Finding E — INFORMATIONAL: import state row is left behind on populate failure with no GC

**What it is:** When `populate_from_import` returns an error mid-import, the importer updates the state row to `status='failed'` with the error message and leaves it behind. The spec's resume contract says a subsequent call with the same slug + same wire_pyramid_id picks up where it left off. But there's no garbage collector for `failed` state rows that the user never retries — they accumulate indefinitely as silent debt.

A `pyramid_import_state` with `status='failed'` for slug X also blocks importing a different pyramid into slug X (the `wire_pyramid_id != existing.wire_pyramid_id` check refuses). The user has to call `pyramid_import_cancel` first.

**Severity:** Low. Modest debt accumulation, manual resolution path exists. Worth a Phase 10 implementer being aware of.

**What I did about it:** Did NOT fix. The fix is either (a) auto-cancel `failed` state rows older than N days on app launch, or (b) a "List failed imports" admin UI in Phase 10. Defer to the planner.

### Finding F — INFORMATIONAL: import does not create the pyramid_slugs row

**What it is:** `import_pyramid` writes to `pyramid_step_cache`, `pyramid_dadbear_config`, and `pyramid_config_contributions` for the target slug, but does NOT create the `pyramid_slugs` row itself. The test fixture creates it manually (`mem_conn` inserts a slug row before each test). In production, if a user calls `pyramid_import_pyramid` for a slug that doesn't already exist in `pyramid_slugs`, the cache rows / dadbear / contribution rows all land successfully (no FK enforcement to `pyramid_slugs`), but DADBEAR's tick loop will operate on a slug that doesn't appear in the slug list. Whether that produces a usable build state depends on what other parts of the system assume `pyramid_slugs` is canonical.

The implementer's intention (per scope decisions in the impl log) is that Phase 10's frontend wizard creates the slug row before calling the import IPC. That's a valid scope boundary, but the IPC contract should document it explicitly so a future caller (testing harness, CLI tool, scripted import) doesn't trip over the implicit precondition.

**Severity:** Low. Phase 10 will fix this by construction. Worth a doc note on the IPC handler.

**What I did about it:** Did NOT fix. Adding a `pyramid_slugs` row inside `import_pyramid` would require choosing a `content_type` (which suffers from Finding D's same problem) and a `source_path` (which the import does have). This is the kind of "import wizard sets up the slug, IPC fills in the cache" split the implementer described in the impl log; hardcoding a default content_type at the IPC entry point would conflict with Phase 10's wizard. Flagged here for Phase 10.

### What I did fix

Both blocking findings (A, B) plus 3 new tests:

1. `enable_dadbear_via_contribution` now checks for an existing active dadbear_policy contribution and re-syncs it instead of creating a duplicate.
2. New `cancel_pyramid_import` function in `pyramid_import.rs` rolls back imported cache rows + state row, scoped by `build_id LIKE 'import:%'` to preserve local writes.
3. `pyramid_import_cancel` IPC handler in `main.rs` calls the new function and returns `state_row_existed` + `cache_rows_rolled_back` in the response.
4. `ImportCancelResponse` struct extended with the two new fields per the spec's IPC contract.

Tests added:
- `test_import_pyramid_resume_does_not_duplicate_dadbear_contributions` — repros the duplicate-contribution bug, then validates the fix.
- `test_cancel_pyramid_import_rolls_back_cache_rows` — pins the rollback contract.
- `test_cancel_pyramid_import_preserves_non_import_cache_rows` — pins the build_id-scoped filter (locally-built rows survive).

### Verification after fix

- `cargo check --lib` — clean, same 3 pre-existing warnings.
- `cargo build` (binary) — clean, only the pre-existing tauri-plugin-shell deprecation warning.
- `cargo test --lib pyramid::pyramid_import` — **18/18 passing** (15 original + 3 new wanderer tests).
- `cargo test --lib pyramid` — **992 passed, 7 failed** (same 7 pre-existing failures: `test_evidence_pk_cross_slug_coexistence`, `real_yaml_thread_clustering_preserves_response_schema`, 5× `staleness::tests::*`). Phase 7 verifier ended at 989; wanderer added 3 → 992. Zero regressions.

### End-to-end trace

**Scenario A — happy path import with matching L0 sources:**
1. Frontend (or test) calls `pyramid_import_pyramid` with a manifest covering 3 L0s + 2 upper-layer nodes.
2. `import_pyramid` validates inputs, creates `pyramid_import_state` row with `status='downloading_manifest'`.
3. Updates state to `status='validating_sources'` with `nodes_total=5`, `cache_entries_total=5`.
4. Calls `populate_from_import` → Pass 1 hashes 3 L0 source files, all match → inserts 3 cache rows. Pass 2 BFS finds nothing stale (frontier is empty) → no propagation. Pass 3 inserts 2 upper-layer cache rows.
5. Updates state to `status='populating_cache'` with `nodes_processed=5`, `cache_entries_validated=5`, `cache_entries_inserted=5`.
6. Calls `enable_dadbear_via_contribution` → no existing active dadbear_policy contribution → creates new one with `source='import'`, maturity=`Canon`. `sync_config_to_operational` upserts `pyramid_dadbear_config` with the contribution_id FK.
7. Updates state to `status='complete'`.
8. Returns `ImportReport { cache_entries_valid: 5, cache_entries_stale: 0, nodes_needing_rebuild: 0, nodes_with_valid_cache: 5 }`.

**Result:** End-to-end works. Cache rows land, contribution lands, operational row lands with FK populated. ✅

**Scenario B — cancel mid-import (after fix):**
1. Test calls `import_pyramid` with the same manifest → 5 cache rows + state row + dadbear contribution all land.
2. User clicks Cancel in the frontend → `pyramid_import_cancel` IPC fires → `cancel_pyramid_import` runs.
3. Function loads import state row (exists) → finds `build_id='import:wire:test-pyramid'` in cache → deletes 5 rows under that build_id → deletes import state row.
4. Returns `ImportCancelReport { state_row_existed: true, cache_rows_rolled_back: 5 }`.

**Result (POST-FIX):** All 5 imported cache rows are gone. State row is gone. DADBEAR contribution + operational row are intentionally left intact (they'll be cleaned via the DADBEAR oversight UI per the contribution path). ✅

**Result (PRE-FIX):** State row deleted. **5 cache rows orphaned.** ❌

**Scenario C — DADBEAR auto-enable after a successful import (after fix):**
1. `enable_dadbear_via_contribution` checks `load_active_config_contribution("dadbear_policy", "test-import")`.
2. First call: returns None → builds canonical YAML with `content_type=document`, `source_path={local_root}`, `enabled=true` → calls `create_config_contribution_with_metadata("dadbear_policy", Some("test-import"), ..., source="import", maturity=Canon)`.
3. Re-loads the contribution → calls `sync_config_to_operational` → dispatcher's `dadbear_policy` branch parses the YAML into `DadbearPolicyYaml` → calls `db::upsert_dadbear_policy` → INSERT INTO `pyramid_dadbear_config` with `contribution_id` = new contribution_id. Triggers `dadbear_reload` event.
4. Second call (resume): `load_active_config_contribution` returns the existing row → re-syncs through dispatcher → no new contribution → idempotent.

**Result (POST-FIX):** Exactly one active dadbear_policy contribution per slug regardless of resume count. Operational row's contribution_id matches. ✅

**Result (PRE-FIX):** Two active contributions on second import. Operational row's FK points at the newer one; older one is dangling-active. ❌

### Commit

Single commit on `phase-7-cache-warming-import` with message `phase-7: wanderer fix — duplicate contribution + cancel rollback`. 3 files modified, 3 new tests, all passing. No deviation from scope (the cancel fix is implementing what the spec already specified; the duplicate-contribution fix is closing the same loophole Phase 4's wanderer closed for IPC handlers).

### Post-fix re-trace summary

| Path | Pre-fix | Post-fix |
|------|---------|----------|
| Single import → cache | ✅ works | ✅ works |
| Re-import (resume) → cache idempotency | ✅ works (verifier fix) | ✅ works |
| Re-import → contribution count | ❌ duplicates | ✅ singleton |
| Cancel mid-import → state row | ✅ deleted | ✅ deleted |
| Cancel mid-import → cache rows | ❌ orphaned | ✅ rolled back |
| Cancel preserves local rerolls | n/a (cancel didn't touch cache) | ✅ build_id-scoped |
| DADBEAR contribution → operational row FK | ✅ works | ✅ works |
| Manifest version > 1 → loud reject | ✅ works | ✅ works |
| `derived_from` cycles in BFS | ✅ HashSet visited gate handles it | ✅ unchanged |
| `derived_from` dangling refs | ✅ silently skipped | ✅ unchanged |
| Privacy gate: `export_cache_manifest` opt-in | ✅ default OFF, no production callers | ✅ unchanged |
| `store_cache` vs `store_cache_if_absent` | ✅ verifier fix correct, only `llm.rs` uses `store_cache` for fresh writes | ✅ unchanged |

---

## Phase 8 wanderer pass — 2026-04-10

**Branch:** `phase-8-yaml-to-ui-renderer`
**Commit:** `24f1091 phase-8: yaml-to-ui renderer` (implementer) + wanderer fix commit below

Phase 8 shipped the `YamlConfigRenderer` primitive — the first pure-frontend phase of the initiative. No production caller mounts it yet (that's Phase 10), so the wanderer question is not "does it run in production?" but "will Phase 10 break on first real-data wiring?" Four distinct bugs found.

### Finding A — MEDIUM: seed `chain-step.schema.yaml` described `dehydrate` with the wrong shape (FIXED)

**What:** The Phase 8 seed annotation file described `dehydrate` as a `list` widget with `item_widget: select` and `item_options_from: node_fields`. The real `ChainStep.dehydrate` field in `chain_engine.rs` is `Option<Vec<DehydrateStep>>` where `DehydrateStep { drop: String }` — a list of OBJECTS each containing a `drop: path.to.field` key, NOT a list of scalar strings.

Grep on real chain YAMLs confirmed the shape in production:
```yaml
dehydrate:
  - drop: "topics.current"
  - drop: "topics.entities"
  - drop: "topics.summary"
  - drop: "topics"
```

(see `chains/defaults/document.yaml`).

**Phase 10 impact:** When ToolsMode wires `YamlConfigRenderer` to a real chain step, the `ListWidget` iterates each item via `String(item)` — an object becomes `"[object Object]"`. The nested `<select>` then renders the stringified object as the current value and finds no matching option (since `node_fields` options are scalar strings like `"headline"` / `"distilled"`). The user would see a broken list of garbled entries they cannot edit. Worse, the `item_options_from: node_fields` list has only top-level fields — real dehydrate rules commonly reference sub-paths (`topics.current`). The annotation is structurally unfit for the real data even with the object-vs-string fix.

**Fix:** Replaced the `dehydrate` field annotation with `widget: readonly` and a help text note that Phase 10 is responsible for the structured editor. This shows the current rules as compact JSON so users can see what's there without being able to (incorrectly) edit them. Phase 10 will need to add either (a) a new composite widget that understands lists of objects, or (b) an expanded schema annotation shape that declares nested `fields:` sub-maps per list item. Both are out of Phase 8 scope.

### Finding B — MEDIUM: seed `dadbear.schema.yaml` had mostly-wrong field names (FIXED)

**What:** The Phase 8 seed DADBEAR annotation defined four fields: `enabled`, `scan_interval_secs`, `max_concurrent_ingests`, `content_type`. The real `DadbearPolicyYaml` struct in `pyramid/db.rs` has seven fields: `source_path`, `content_type`, `scan_interval_secs`, `debounce_secs`, `session_timeout_secs`, `batch_size`, `enabled`.

Mismatches:
- `max_concurrent_ingests` does NOT exist on `DadbearPolicyYaml` (closest field: `batch_size`, described in the struct as "pending ingests per tick")
- Missing: `source_path` (REQUIRED in real YAML), `debounce_secs`, `session_timeout_secs`, `batch_size`

**Phase 10 impact:** When ToolsMode wires the renderer to a real DADBEAR config, `max_concurrent_ingests` would show as an empty editor (no such key in the real YAML). The four missing fields would be invisible entirely — the user would edit four config values and hit Accept, but `source_path` / `debounce_secs` / `session_timeout_secs` / `batch_size` would silently retain their prior values because the renderer never surfaced them.

**Fix:** Rewrote `chains/schemas/dadbear.schema.yaml` to match `DadbearPolicyYaml` 1:1. Added `source_path` (text, basic), `debounce_secs` (number, advanced), `session_timeout_secs` (number, advanced), `batch_size` (number, advanced). Dropped `max_concurrent_ingests` entirely. Added new Rust test `test_seed_dadbear_annotation_matches_real_policy_fields` that parses the seed file and asserts every field key is a real `DadbearPolicyYaml` property + no stale unknowns. Also added `test_seed_chain_step_annotation_fields_exist_on_chain_step` to lock in the chain-step annotation against `ChainStep` drift.

### Finding C — LOW/CORRECTNESS: inheritance indicator shows "← default" when both value and default are undefined (FIXED)

**What:** `YamlConfigRenderer.FieldRow` uses `valuesEqual(value, resolvedDefault)` to decide whether to show `← {inherits_from} default`. `valuesEqual(undefined, undefined) === true`, so when a field has `inherits_from: defaults.xxx` set, the current value is missing, AND the resolved default (via `readPath(defaults, "defaults.xxx")`) is also `undefined`, the indicator renders — implying the field is inheriting when in fact there is nothing to inherit from.

Phase 10 will typically pass a real `defaults` object, but edge cases surface this: rendering a chain step in isolation without its parent defaults block, rendering during the loading window before defaults resolve, rendering a simpler config type (DADBEAR policy) where `inherits_from` is never set but a bug elsewhere could generate it. The indicator is a correctness signal; showing it wrongly erodes user trust.

The implementer's own log entry (Phase 8, Scope decisions, "Inherited-from-default indicator compares current vs resolved default, not vs 'absent'") acknowledged the behavior and reasoned "no override means we use the default" — but that argument only holds when the default actually exists. When both are undefined, nothing is being inherited.

**Fix:** Added `shouldShowInheritanceIndicator(annotation, value, resolvedDefault)` helper next to `valuesEqual`. Guards against the false positive by requiring `resolvedDefault !== undefined` before running the equality check. `FieldRow` now calls the helper instead of the bare `valuesEqual`. The fix is narrow and preserves the original semantics in all other cases — if the resolved default is present (even as `null` or an empty string), the equality check still runs.

### Finding D — LOW/PERFORMANCE: cost-estimate effect refetches on every keystroke (FIXED)

**What:** `useYamlRendererSources` runs a second `useEffect` for `show_cost: true` fields with deps `[schema, costFieldPaths, values, optionSources]`. The `values` entry in the deps array triggers the effect on every prop update — including keystrokes in unrelated fields (e.g. user types a number in `temperature`, cost effect refires and makes a fresh IPC round trip for every cost-annotated path). The pattern produces N IPC roundtrips per keystroke where N is the number of cost-annotated fields.

Not a correctness bug but a real footgun once the renderer is wired in Phase 10 — the default schema has `show_cost: true` on `model_tier`, so every keystroke in `temperature` / `concurrency` / `max_input_tokens` / etc. fires an `invoke('yaml_renderer_estimate_cost', ...)`. The user sees a visible delay on fast-typed inputs.

**Fix:** Extracted a memoized `costPathValues` string that serializes only the values at the cost-annotated paths (e.g. `"model_tier=synth_heavy"`). The cost effect now depends on `costPathValues` instead of the full `values` object, so it only re-runs when a cost-annotated field's value actually changes. Unrelated keystrokes are silent.

### Finding E — INFORMATIONAL: `model_selector` widget silently hides a set value during the option-resolution loading window

**What:** When `optionSources.tier_registry` hasn't resolved yet (Tauri IPC roundtrip in flight), `ModelSelectorWidget` renders with an empty options list. If the parent already passed `value="synth_heavy"`, the native `<select>` doesn't find a matching `<option>` and either shows nothing or picks the first empty option. When the options arrive, React re-renders and the real value appears.

**Verdict:** Not fixed. This is a Phase 10 concern — the parent component is responsible for the loading state (e.g. wrap in a `loading` spinner until `useYamlRendererSources.loading === false`). Phase 8's hook already exposes a `loading` field; Phase 10 just has to use it.

### Finding F — INFORMATIONAL: `condition` field is on the type but not evaluated

**What:** Both Rust `FieldAnnotation` and TypeScript `FieldAnnotation` carry a `condition` string property. The renderer does not evaluate it. The implementer documented this explicitly as deferred to Phase 10. If someone ships a schema annotation with a `condition` field (e.g. `"split_strategy != null"`), the field will ALWAYS render regardless of the expression's truth value — effectively a silent "always show" override of whatever the schema author intended.

**Verdict:** Not fixed. Documented as Phase 10 scope. The friction is that Phase 10 MUST implement this before any schema annotation uses `condition` — otherwise a seed with conditional fields ships broken. Adding a spec note or renderer warning ("condition evaluation is not implemented") would be nice-to-have but feels out of Phase 8 scope.

### What I did not fix

- **Finding E** — loading state is Phase 10's responsibility (the hook already exposes `loading`).
- **Finding F** — `condition` evaluation is explicitly deferred to Phase 10.
- **`valuesEqual` JSON.stringify key-order brittleness** — low-severity correctness issue where `{a:1, b:2}` and `{b:2, a:1}` JSON-serialize to different strings and compare as unequal. Unlikely to surface with server-serialized defaults (serde produces deterministic key order) but worth noting. No fix — the risk is narrow and object defaults are rare in practice.
- **`list`/`group`/`code` widget advanced features** — Phase 3 (per spec) deferred features are acceptable per the workstream brief. The widgets exist as minimum viable implementations.

### End-to-end scenario traces (post-fix)

**(a) Phase 10 loads `chain-step.schema.yaml` annotation and renders it with real chain step values.**

1. Phase 10 calls `invoke('pyramid_get_schema_annotation', { schemaType: 'chain_step_config' })` → Rust returns `SchemaAnnotation` JSON via `load_schema_annotation_for` (direct slug lookup hits `slug = "chain_step_config"`, body deserializes into `SchemaAnnotation`).
2. Tauri v2 serde handles camelCase→snake_case auto-conversion on the arg side; response is already snake_case JSON and TS types match 1:1.
3. Phase 10 loads the real chain step YAML (e.g. `source_extract` step from `document.yaml`) and passes it as `values`.
4. Phase 10 calls `useYamlRendererSources(schema, values)` → the hook walks the annotation's `options_from` / `item_options_from` (now just `tier_registry` since the list widget was removed from `dehydrate`), calls `yaml_renderer_resolve_options` once per unique source, caches results.
5. Phase 10 mounts `YamlConfigRenderer` with `{ schema, values, optionSources, costEstimates }`.
6. Renderer sorts the annotation's fields by `(order, key)`, splits by visibility, groups by `group`. Basic fields render inline, Advanced fields sit under a collapsed `▶ Advanced` section.
7. `model_tier` select renders with `tier_registry` options; `temperature` slider renders 0.3; `concurrency` number renders 10; `on_error` static select renders "retry(3)"; `max_input_tokens` renders 50000 with "tokens" suffix under Token Budget group; `batch_size` renders 20; `split_strategy` shows "sections"; `dehydrate` now shows read-only compact JSON `[{"drop":"topics.current"},...]` (post-fix); `compact_inputs` toggle renders false.
8. Cost badge on `model_tier` shows the tier's estimated USD-per-call from the pricing_json lookup.
9. No crashes. All fields visible. `dehydrate` is read-only which is correct-but-limited for Phase 8 per the fix.

**(b) User edits a model_tier field via the select widget.**

1. User clicks the `model_tier` dropdown. `ModelSelectorWidget` renders the `tier_registry` options from `optionSources`, each with a rich `meta.provider_id`/`meta.model_id` badge and context window label.
2. User picks `fast_extract`. The native `<select>` fires `onChange(e)`, the widget calls `onChange(e.target.value)`.
3. `FieldRow`'s wrapped callback invokes `onChange("model_tier", "fast_extract")` on the renderer's parent.
4. Parent updates its `values` state.
5. React re-renders; `YamlConfigRenderer` receives new `values`.
6. `useYamlRendererSources`'s cost effect runs — post-fix, the dep is `costPathValues` which now changes (the model_tier value changed), so the effect fires. It reads the new tier's meta from the cached `tier_registry` options, calls `yaml_renderer_estimate_cost` with the new `(provider_id, model_id)` pair, sets the new cost on `costEstimates.model_tier`.
7. FieldRow shows the new cost badge, updated inheritance indicator (if `fast_extract` matches `defaults.model_tier` the `← default` label appears; otherwise it disappears).
8. No crashes. Post-fix: editing an unrelated field like `temperature` no longer triggers the cost effect, since `costPathValues` is unchanged.

**(c) User clicks Notes and provides a refinement note.**

1. User clicks the "Notes" button in the action bar. `setNotesOpen(true)`.
2. Inline textarea appears with placeholder text and a "Submit Notes" button (disabled while the textarea is empty).
3. User types "Use cheaper model for source_extract, bump batch size for merges". `setNotesText` updates on every keystroke.
4. Submit button enables.
5. User clicks "Submit Notes". The handler calls `onNotes("Use cheaper model for source_extract, bump batch size for merges")`, then clears the textarea and closes the notes section.
6. Phase 9 will own the LLM round trip — the renderer just emits the note via `onNotes`. Phase 8 is correctly passive here.

### Commit

Single wanderer-fix commit on `phase-8-yaml-to-ui-renderer` with message `phase-8: wanderer fix — inheritance + schema + cost effect`. Files modified:

- `chains/schemas/chain-step.schema.yaml` — `dehydrate` widget → `readonly`
- `chains/schemas/dadbear.schema.yaml` — rewrite to match `DadbearPolicyYaml`
- `src/components/YamlConfigRenderer.tsx` — add `shouldShowInheritanceIndicator` guard
- `src/hooks/useYamlRendererSources.ts` — memoize `costPathValues`, decouple cost effect from full `values`
- `src-tauri/src/pyramid/yaml_renderer.rs` — two new seed-file lock-in tests

### Verification after fix

- `cargo test --lib "pyramid::yaml_renderer"` — **14/14 passing** (12 pre-existing + 2 new lock-in tests)
- `cargo test --lib "pyramid::wire_migration"` — **12/12 passing** (unchanged; migration does not care about annotation field correctness, only YAML parseability)
- `npx tsc --noEmit` — clean, no new TypeScript errors
- `cargo check --lib` — clean

---

## Phase 9 wanderer pass — 2026-04-10

**Branch:** `phase-9-generative-config-pattern`
**Commit:** `5b9975a phase-9: generative config pattern` (implementer) + wanderer fix commit below

Phase 9 shipped the backend for the generative config loop: 6 IPC commands, the schema registry, the bundled contributions manifest walker, and the 3-phase load → LLM → persist pattern. 1044 tests passing, 16 tests for `generative_config`, 10 for `schema_registry`. Clean verifier pass. The wanderer found two bugs in the refine/accept lifecycle that map directly to the "helper exists but isn't called from production" pattern the brief flagged.

### Finding A — HIGH/CORRECTNESS: direct-YAML accept orphans the prior active contribution (FIXED)

**What:** `accept_config_draft` has two paths: (a) promote-latest-draft (used when the user doesn't pass a YAML payload), and (b) direct-YAML (used when the user edits the YAML in the renderer and saves directly). The promote path correctly supersedes the prior active via `promote_draft_to_active`. The direct-YAML path calls `create_config_contribution_with_metadata(..., status="active", ...)` in isolation — which creates a new row but does NOT touch any existing active row.

Result: every direct-YAML save accumulates a new active row without superseding the previous one. After N saves there are N+1 active rows for the same (schema_type, slug) pair. The schema registry's `find_bundled_default_id` and `load_active_config_contribution` queries `ORDER BY created_at DESC LIMIT 1` out of these, so the "most recent" wins — but the older rows are orphaned, and any code that does a COUNT(*) over active rows (or that assumes uniqueness) breaks.

Exactly the class of bug the Phase 7 wanderer caught, and exactly the "helper exists but isn't called from production" pattern the Phase 9 brief flagged: `supersede_config_contribution` exists and does the right thing, but the direct-YAML path doesn't call it.

**Reproduction:** Test `wanderer_accept_direct_yaml_does_not_orphan_prior_active` in `src-tauri/src/pyramid/test_phase9_wanderer.rs`. On a fresh DB with the bundled evidence_policy default active, calls `accept_config_draft(evidence_policy, yaml=<direct yaml>)`. Before fix: 2 active rows (bundled + new); bundled.status = "active" with superseded_by_id = None. After fix: 1 active row; bundled.status = "superseded" with superseded_by_id pointing at the new row.

**Phase 10 impact:** Every interaction pattern the spec describes ("user accepts the refined YAML from the renderer") flows through the direct-YAML path. Ship-blocker — after even a single accept the DB would have two active evidence_policy contributions, and subsequent `pyramid_active_config` calls would still return the bundled default (ORDER BY created_at DESC would put the new row first, but the orphan is a time bomb for any per-slug COUNT / dedup logic).

**Fix:** Rewrote the direct-YAML branch in `accept_config_draft` to run a transaction that (1) finds any existing active row for the (schema_type, slug) pair, (2) inserts the new row with `supersedes_id = prior_active_id`, and (3) marks the prior row as `superseded` with `superseded_by_id = new_id`. Matches the `supersede_config_contribution` semantics but honors the direct-YAML path's `source = "local"`, `created_by = "user"` metadata. When no prior active exists, the insert still runs (with `supersedes_id = NULL`) and no UPDATE fires.

### Finding B — HIGH/CORRECTNESS: refine of an active contribution wipes the active chain (FIXED)

**What:** `create_draft_supersession` (the refine path's backing helper) ran a two-statement transaction: INSERT the new draft with `supersedes_id = prior_id`, then UPDATE the prior row to set `superseded_by_id = new_id` and flip `status = 'active' → 'superseded'`. The problem is the status flip: when the user refines their currently-active config, the prior row becomes `superseded` and the new row is a `draft` — so there is NO row with `status = 'active' AND superseded_by_id IS NULL` for that (schema_type, slug) anymore.

Consequences:
1. `pyramid_active_config` returns `None` during the refine draft window — the UI loses its reference to the current policy while the user is still reviewing the draft.
2. Background readers (DADBEAR ticks, ongoing builds) that resolve via `load_active_config_contribution` also lose their reference.
3. `load_config_version_history` starts its chain walk from the active row, so it returns an empty `Vec` — and the refine response's version number (computed as `history.len() + 1`) is wrong (returns 1 when it should return 2).

**Reproduction:** Tests `wanderer_refine_active_returns_correct_version` and `wanderer_multi_refine_increments_version` in `test_phase9_wanderer.rs`. Both fail before the fix and pass after.

**Root cause:** The implementer's comment on `create_draft_supersession` says it exists "because `supersede_config_contribution` forces the new row to `active`, which is wrong for the Phase 9 draft flow." Correct diagnosis, but the fix went too far — it also inherited `supersede_config_contribution`'s "mark the prior as superseded" UPDATE, which is wrong for the draft flow too. The refinement draft is a PROPOSED successor, not an accepted one; the status transfer must wait until the user accepts.

**Fix:** Removed the UPDATE on the prior row entirely. `create_draft_supersession` now only INSERTs the new draft with `supersedes_id = prior_id` — the prior row stays untouched. The refinement chain is traced purely via `supersedes_id` backpointers until accept, at which point `promote_draft_to_active` walks the chain and handles the active-transfer transaction.

Also replaced the refine path's version computation: instead of calling `load_config_version_history` (which walks from the active and therefore can't see draft chains), the new `version_by_chain_walk` helper walks the `supersedes_id` chain backward from a given contribution_id and counts the depth. Handles cycle-safety with a HashSet visited set and a 10K chain-length cap.

### Findings that were NOT bugs

- **First-boot idempotency under `INSERT OR IGNORE`** — verified. The bundled walk runs on every boot (correctly not gated by the Phase 5 sentinel per the implementer's explicit comment), INSERT OR IGNORE skips existing rows including user supersessions, and the test `phase9_bundled_walk_skips_user_superseded` locks this in. The "new version of an existing contribution_id" case is intentionally NOT handled — app upgrades ship NEW `contribution_id` values (e.g. `bundled-evidence_policy-default-v2`), leaving the v1 in place. No change.
- **`synth_heavy` hardcoded tier** — the implementer noted this and has a fallback path. Verified the fallback: if `resolve_tier("synth_heavy", ...)` returns None, it falls back to `llm_config.primary_model` with provider_id="openrouter" (telemetry only — the actual HTTP call builds its provider from `config.provider_registry.get_provider("openrouter")` in `build_call_provider`, and falls back to a legacy `OpenRouterProvider` if no registry is attached). The hardcoded "openrouter" is consistent with the rest of the codebase's provider resolution pattern, not a new Phase 9 coupling. `with_model_resolution` + `with_provider` on StepContext are telemetry-only — the actual model used in `call_model_unified_with_options_and_ctx` line 490 is always `config.primary_model`, regardless of what the ctx says.
- **YAML extraction resilience** — verified all four documented cases (plain, fenced, prose-prefix, fence+prose) by reading `extract_yaml_body` + `extract_fenced_block`. The fenced-block regex is naive (first `\`\`\`` wins) but the Phase 9 prompts explicitly ask for YAML-only output, so the edge case of ``` in YAML comments is low-probability.
- **Prompt substitution `{if X}...{end}` blocks** — verified. Conditionals are processed BEFORE value substitution, so `{end}` inside a note/current_yaml value is safe. Nested conditionals are not supported (the Phase 9 prompts don't use them). Unclosed `{if X}` returns input unchanged.
- **3-phase Send-safety pattern** — verified. Each IPC handler drops the `tokio::sync::Mutex<Connection>` guard in a scoped block before the `.await` on the LLM call. `rusqlite::Connection` is `!Send`, so the block-scoped guard ensures it never crosses an await point. `cargo check --lib` is clean.
- **Notes enforcement at IPC boundary** — verified. `pyramid_refine_config` calls `validate_note(&note)?` before the `config.read().await` and before any DB work. Empty notes error immediately.
- **Bundled manifest `include_str!` path** — verified resolves correctly: `src-tauri/src/pyramid/wire_migration.rs` → `../../assets/bundled_contributions.json` → `src-tauri/assets/bundled_contributions.json`. Compile would fail if the path were wrong — `cargo check --lib` is clean.
- **Generation skill body extraction** — the manifest ships the skill body inlined under `yaml_content`, and `insert_bundled_contribution` writes it directly to the `yaml_content` column. `load_contribution_by_id` reads the same column. End-to-end consistent.
- **Schema registry loading under contention** — `SchemaRegistry` uses an `RwLock<HashMap<String, ConfigSchema>>` internally. Read locks are taken in `get` / `list` / `list_full` and released at method return. Write lock is taken only during `reload` / `invalidate`. An in-flight generation call that holds a `ConfigSchema` clone via `schema_registry.get(schema_type)?` will NOT see a mid-call invalidation because `get` clones the struct out of the read lock before returning — the clone is independent of the registry's state after that point.

### Commit

Single wanderer-fix commit on `phase-9-generative-config-pattern` with message `phase-9: wanderer fix — refine preserves active + direct-YAML accept supersedes`. Files modified:

- `src-tauri/src/pyramid/generative_config.rs` — removed prior-row UPDATE from `create_draft_supersession`; added `version_by_chain_walk` helper and used it in `persist_refined_draft`; rewrote the direct-YAML branch of `accept_config_draft` to wrap its INSERT + UPDATE in a transaction.
- `src-tauri/src/pyramid/test_phase9_wanderer.rs` — new test module with 4 tests covering the two fixed bugs + two sanity cases (direct-YAML with no prior, multi-refine version counting).
- `src-tauri/src/pyramid/mod.rs` — register the test module behind `#[cfg(test)]`.
- Updated `test_create_draft_supersession_marks_prior_superseded` → `test_create_draft_supersession_links_via_supersedes_id` to reflect the new correct behavior (prior stays active).

### Verification after fix

- `cargo test --lib "pyramid::generative_config"` — **16/16 passing** (pre-existing 16; one updated assertion reflects new correct semantics)
- `cargo test --lib "pyramid::schema_registry"` — **10/10 passing**
- `cargo test --lib "pyramid::wire_migration"` — **17/17 passing**
- `cargo test --lib "pyramid::config_contributions"` — **21/21 passing**
- `cargo test --lib "pyramid::test_phase9_wanderer"` — **4/4 passing** (new)
- `cargo test --lib "pyramid::"` — 1048 passing vs 1044 pre-fix, same 7 pre-existing failures (+4 wanderer tests, no regressions)
- `cargo check --lib` — clean

---

## 2026-04-10 — Phase 10 wanderer found 3 UI bugs in ToolsMode drawer + publish modal

**Phase / workstream:** Phase 10 wanderer pass (ToolsMode UI integration)

**What hit friction:** Phase 10 wired the drawer, publish modal, and Create wizard to the Phase 4/5/8/9 IPC. A fresh-eyes trace caught three non-obvious bugs that the punch-list verifier missed, all in the React layer (zero Rust changes).

### Finding A — HIGH/UX: `ContributionDetailDrawer` version history was rendered in reverse order, with inverted version labels, landing on the OLDEST row by default

**Where:** `src/components/ContributionDetailDrawer.tsx` — the lazy fetch effect (~line 118) and the `activeRow` memo (~line 143).

**Symptom:** User opens the drawer on a config with 3 refinements. Clicks "Version History" tab. Expects to see the CURRENT active YAML and a list with v3 (active) at the top, v1 (oldest) at the bottom. Instead:
1. The list renders v1 at the top labeled "v3", v3 at the bottom labeled "v1" — because the display index was `versions.length - i` on an already-oldest-to-newest list.
2. The default selection on tab-switch lands on `versions[0]`, which is the OLDEST row, so the renderer shows the yaml_content from v1 (not what the user was just looking at in the Details tab).
3. Every version-history-related number (`v{n}` badge, the `versionInfo.version` passed to the renderer, the "which row is selected" highlight) was inverted.

**Root cause:** The backend `load_config_version_history` helper in `config_contributions.rs` does `chain.reverse()` at the end of the walk and returns oldest-to-newest. The frontend drawer was written assuming the list came back newest-first — all the indexing math (`versions.length - i`, `versions.findIndex(...)`, default `versions[0]`) was correct for newest-first, wrong for oldest-to-newest. The drawer's comment even said "Default to the first version (latest chronologically — versions are returned in chain order by the Phase 9 IPC)" — the comment was wrong.

**How I found it:** Read the Rust helper (`config_contributions.rs:421`) explicitly after seeing the drawer comment. The existing Rust test (`test_load_config_version_history`) asserts the order: `ids == vec![v1, v2, v3]` with `history[0].status == "superseded"` and `history[2].status == "active"`. The drawer was off by one (or rather, flipped) against that contract.

**Fix:** In `ContributionDetailDrawer.tsx`, flip the list at the fetch boundary (`setVersions([...rows].reverse())`) so `versions[0]` IS the newest row through the rest of the component. That makes all the other indexing math correct by the existing code and also makes the default-selection land on the latest (which IS the row the drawer was opened with via `pyramid_active_config_contribution`). Comment updated.

**Lesson for future phases:** When a React component claims to know a backend helper's ordering, read the helper source and search for its tests — don't trust the component's own comment. Also: when frontend and backend both own the same list, the contract should be unidirectional (backend decides, frontend passively renders). Both places doing transformations (backend `.reverse()`, frontend `versions.length - i`) creates the exact kind of "double-invert" bug we had here.

### Finding B — MEDIUM/UX: `PublishPreviewModal` could be dismissed mid-publish, creating ghost publishes

**Where:** `src/components/PublishPreviewModal.tsx` — the overlay `onClick`, the `Escape` key handler, and the `✕` close button.

**Symptom:** User clicks Confirm & Publish. The button correctly disables during `publishing === true`. But the backdrop click (`onClick={onClose}`), the Escape key (`e.key === "Escape" && onClose()`), and the `✕` header button were all NOT gated on `publishing`. If the user clicked outside or hit Escape during the 2-10s publish round-trip, the modal would unmount while the publish was still in flight. The publish would still complete on the backend (writing `wire_publication_state_json`, returning a `wire_contribution_id`), but the user never sees the success confirmation — and they don't know the publish actually happened.

**Root cause:** The `publishing` state flag was used to disable the Confirm button but wasn't threaded through to any of the "close the modal" paths. An easy miss: three separate close triggers, each independently coded.

**Fix:** Added a `safeClose` callback that short-circuits when `publishing` is true. Wired all three close triggers (`overlay onClick`, Escape key handler, `✕` header button) to it. Also marked the `✕` button `disabled={publishing}` so the user sees the gating visually.

**Lesson for future phases:** When a modal has a mid-flight async operation, the "close" primitive should be factored into a single `safeClose` that knows about the in-flight state. Every close trigger calls `safeClose`, not `onClose` directly. This is a one-function-refactor pattern; the implementer landed the `publishing` disable on the button but forgot the cancel/Escape/✕ paths.

### Finding C — LOW/UX: `ContributionDetailDrawer` stayed open with stale data after a successful publish from its footer button

**Where:** `src/components/modes/ToolsMode.tsx` — the `publishClose` callback in `MyToolsPanel`.

**Symptom:** User opens a config's detail drawer. Clicks "Publish to Wire" in the drawer footer. `PublishPreviewModal` opens, user confirms, publish succeeds. The modal closes via `publishClose`, which calls `bumpRefresh()` (refetches My Tools configs and proposals). But the `detailContribution` state was unchanged — the drawer still showed the pre-publish `ConfigContribution` row with `wire_contribution_id: null`. The drawer's "Published" badge check (`{contribution.wire_contribution_id && ...}`) stayed false. User would need to close and reopen the drawer to see that the publish actually landed.

**Root cause:** The publish modal's success path called `onClose` (which bumps `refreshToken` in the parent), but `refreshToken` only drives the schema-list and proposals refetches — not the drawer's `detailContribution` state. The drawer state is independent of the refresh cycle.

**Fix:** Added a `handlePublishSuccess` callback wired to the modal's `onPublished` prop. On publish success, clear `detailContribution` (which unmounts the drawer). The user's next "View" click refetches the row via `pyramid_active_config_contribution`, which now returns the updated `wire_publication_state_json` / `wire_contribution_id`. The drawer reopens with fresh state.

**Lesson for future phases:** When a modal mutates a parent's data source, the parent needs an explicit success hook, not a generic close hook. The `onClose` callback is used for both "user cancelled" AND "user confirmed and closed" — those are different semantics from the parent's perspective. Two callbacks (`onClose` for cancel, `onPublished` for success) is cleaner than branching on internal state.

### Findings that were NOT bugs

- **IPC argument naming:** All 13 Phase 10 IPC calls use camelCase arg keys (e.g. `{ schemaType, slug }`), matching Tauri v2's default auto-conversion to snake_case on the Rust side. Confirmed against an existing working call (`yaml_renderer_estimate_cost` at `useYamlRendererSources.ts:161` passes `avgInputTokens` to Rust's `avg_input_tokens`). No mismatches.
- **Drawer re-open state reset:** The `[contribution?.contribution_id, initialTab]` dep on the reset effect correctly fires on close→reopen (null → uuid is a dep change), so internal state doesn't leak across open cycles.
- **`bundled` annotations without `condition` field:** None of the three bundled schema_annotations (evidence_policy, build_strategy, custom_prompts) use `condition`, so the deferred evaluator is not exercised in Phase 10's shipped surface.
- **Missing annotations for `dadbear_policy` / `tier_routing`:** These two schema types have a schema_definition + skill but no schema_annotation in `bundled_contributions.json`. The drawer and the Create wizard both have explicit fallback paths that render raw YAML with "No UI schema annotation available for ...". Confirmed to work without crashing.
- **`pyramid_active_config` vs `pyramid_active_config_contribution`:** Both commands are registered in the invoke_handler and return different shapes for different consumers. MyToolsPanel uses the former for ConfigCard metadata (version_chain_length, triggering_note) and the latter for drawer/publish loads (full row including wire metadata).

### Non-blocking concerns noted but not fixed

- **Draft contribution accumulation:** The `handleAccept` path in the Create wizard ALWAYS passes `yaml: state.values`, so it always hits the direct-YAML branch of `pyramid_accept_config`. The alternate "promote latest draft" branch is never reached from the Phase 10 UI. This means draft rows created by `pyramid_generate_config` and `pyramid_refine_config` are never promoted or cleaned up — they accumulate as stranded `status='draft'` rows in `pyramid_config_contributions`. No UI surfaces them, so the user never sees the clutter, but the DB grows monotonically on every generate/refine. Not a Phase 10 blocker (the accepted contribution is functionally correct), but a cleanup pass in a later phase is warranted.
- **`js-yaml` round-trip fidelity:** The `handleRefine` path serializes `state.values` to YAML via `yaml.dump({lineWidth: -1, noRefs: true})` to send to the backend for the refinement LLM call. Key ordering and comment preservation are NOT guaranteed, so the YAML sent to the refine call may not be byte-identical to the LLM's original output. Not a correctness bug — the backend parses it with `serde_yaml::from_str` which is order-independent — but it means the "what the user sees" and "what the LLM sees for refinement" may differ in layout. The refined LLM call is still correct because it operates on semantic content, not layout.
- **YAML object serialization in accept path:** `handleAccept` passes `yaml: state.values` as a JS object. The Rust side's `Option<serde_json::Value>` accepts it and re-serializes via `serde_yaml::to_string`. The stored `yaml_content` may therefore have a different layout than what the LLM generated — not a bug, just a minor wart where the DB-stored YAML differs textually from the LLM output.

### What I did fix

- `src/components/ContributionDetailDrawer.tsx` — reverse the versions list at fetch time; comments updated.
- `src/components/PublishPreviewModal.tsx` — add `safeClose`, wire it to overlay / Escape / `✕`; `✕` button gets `disabled={publishing}`.
- `src/components/modes/ToolsMode.tsx` — add `handlePublishSuccess` callback, wire it to `PublishPreviewModal.onPublished`.

### Verification after fix

- `npx tsc --noEmit` — clean (no type errors)
- `npm run build` — clean (131 modules transformed, frontend bundle builds)
- `cargo check --all-targets` — clean (warnings only, no new issues from Phase 10)
- Zero Rust changes.

### End-to-end scenarios traced post-fix

1. **Full generate → refine → accept → drawer → publish dry-run.**
   - Create tab: pick schema (with has_generation_skill=true) → intent → Generate → LLM round-trip → edit step with version=1, triggering_note=intent. Refine with a note → LLM round-trip → edit step with version=2, triggering_note=note. Accept → accept-success with version reflecting active chain length.
   - My Tools tab: MyToolsPanel remounts, refetches schemas + active configs via `pyramid_active_config`. ConfigCard shows the accepted version.
   - Click View → drawer opens with active row. Click "Version History" tab → fetch fires, versions reversed so `versions[0]` is the active (matches the drawer's `contribution` prop). Version labels (v3, v2, v1 top-to-bottom) now correct.
   - Click Publish from drawer → modal opens → dry-run fetches → user clicks Confirm → publish completes → modal success → Done → publishClose fires (refresh + handlePublishSuccess) → drawer unmounts → next View refetches and shows "Published" badge.
2. **Accept without refinement.**
   - Create tab: pick schema → intent → Generate → edit step → Accept immediately. `handleAccept` passes `state.values` (from the parsed generated YAML). Direct-YAML path in Rust creates a new active row with version=1 (or 2 if a prior active existed). Works.
3. **Open My Tools with pre-existing active config, open drawer, click through version history.**
   - Bundled defaults are inserted at `status='active'` on first run. MyToolsPanel's config cards show them. Click View → drawer opens with the active row. Click Version History → `pyramid_config_versions` returns just the one row. Drawer shows v1 labeled correctly. No crash even with versions.length === 1.

### Commit

- `2f77ffe phase-10: wanderer fix — drawer version order + publish race + drawer staleness` — three fixes squashed on branch `phase-10-toolsmode-ui`.

### What I did not fix

- Draft contribution accumulation (non-blocking; needs a dedicated cleanup pass or a UI surface for abandoned drafts).
- YAML round-trip fidelity (not a correctness bug; layout-only).

---

### 2026-04-10 — Phase 11 wanderer caught the health hook wired to dead code

**Phase / workstream:** Phase 11 (wanderer pass on `phase-11-openrouter-broadcast`)

**What hit friction:** The implementer wired `maybe_record_provider_error` into `call_model_via_registry` for both connection failures (→ `ConnectionFailure`) and HTTP ≥500 (→ `Http5xx`). The implementation log line 1824 asserts this is "the primary cost path". But `call_model_via_registry` is not called from anywhere outside `llm.rs` itself — it's a public function with zero external call sites. `chain_dispatch.rs::dispatch_ir_llm` uses `call_model_unified_with_options_and_ctx`, which had no health hooks wired in. Real production traffic would therefore never feed `Http5xx` or `ConnectionFailure` into the state machine. Only the broadcast webhook's `CostDiscrepancy` path would flow in. A provider outage — the exact signal the oversight UI is meant to surface — would be invisible to `pyramid_provider_health`.

Separately, the state machine's HTTP 5xx branch called `count_recent_cost_discrepancies` (wrong signal) and unconditionally flipped to `degraded` on every observation. The existing test `single_5xx_degrades_immediately` asserted the wrong behavior (spec says degrade only after 3+ in window). A single transient 5xx would have flagged a provider as degraded in the oversight UI and stayed there until the operator acknowledged.

**Root cause:** (1) Two parallel LLM entry points coexist in `llm.rs` — `call_model_unified_with_options_and_ctx` (the legacy path used by chain_dispatch, generative_config, and the Theatre audit path) and `call_model_via_registry` (a tier-routing path introduced by Phase 6 that hasn't been wired into production flows yet). The implementer added the hook to the wrong function based on naming ("via_registry" sounds like the canonical path) without verifying the call graph. (2) The state machine's Http5xx branch was written before the 5xx event log table existed, so the implementer reused `count_recent_cost_discrepancies` as a stand-in signal and then wired both branches of the `if` to the same `Degraded` outcome — at which point the count check was vestigial and degraded-on-first was effectively hardcoded. The comment in `llm.rs` at line 1331 said "threshold in `record_provider_error`" but no such threshold actually gated the decision.

**What we did about it:** Wanderer committed `phase-11: wanderer fix — wire health hooks into prod LLM path + 5xx rolling threshold` on branch `phase-11-openrouter-broadcast`.

Fix 1 (dead-code wiring): Added `maybe_record_provider_error` calls to `call_model_unified_with_options_and_ctx` at three error sites — (a) final connection-failure return after retries exhausted (→ `ConnectionFailure`), (b) the retryable-status-codes branch when `status >= 500` (→ `Http5xx`), (c) the terminal non-success branch when `status >= 500` (→ `Http5xx`). Non-5xx final errors (401/403/404) are intentionally NOT fed into the health hook — they indicate auth/config mistakes, not provider failure. The provider_id passed to the hook is computed once at the top of the function from `provider_type.as_str()`, which resolves against the seeded `openrouter` provider row for both the registry and transitional fallback paths in `build_call_provider`.

Fix 2 (5xx rolling threshold): Added a new `pyramid_provider_error_log` table (id, provider_id, error_kind, created_at) with a `(provider_id, error_kind, created_at)` index for the count query. Added `db::record_provider_error_event` + `db::count_recent_provider_errors` helpers. Rewrote `provider_health::record_provider_error`'s `Http5xx` branch to INSERT the event, COUNT recent 5xx rows inside `policy.provider_degrade_window_secs`, and only flip to `Degraded` when the count ≥ `policy.provider_degrade_count`. Below the threshold the observation is logged but the state machine returns early without flapping the flag. Updated the existing `single_5xx_degrades_immediately` test to `single_5xx_below_threshold_does_not_degrade` (now asserts `healthy` after one observation) and added `three_5xx_in_window_degrades` (asserts `degraded` after three observations, with reason mentioning "HTTP 5xx"). Cost discrepancies continue to use `pyramid_cost_log.reconciliation_status = 'discrepancy'` as their counter surface — the new event log is HTTP-specific.

**Verification:**
- `cargo check --lib` — clean, same 3 pre-existing warnings.
- `cargo test --lib pyramid::provider_health` — 7/7 pass (6 existing + 1 new `three_5xx_in_window_degrades`; the renamed `single_5xx_below_threshold_does_not_degrade` replaces the old assertion).
- `cargo test --lib pyramid::openrouter_webhook` — 16/16 pass.
- `cargo test --lib pyramid` — 1073 passed / 7 failed. Same 7 pre-existing failures (db evidence PK cross-slug, defaults_adapter thread clustering, 5 staleness tests querying a non-existent `pyramid_evidence.build_id` column). Net +1 from phase-11 baseline (added 2 health tests, removed the old assertion, same test count delta).

**Lesson for future phases:** When a phase adds a hook that "the LLM path fires", map ALL LLM entry points first, confirm which one the production flows actually hit, and assert the hook is reachable from `chain_dispatch.rs` specifically. `call_model_via_registry` looks like the canonical path from the name alone, but Phase 6's tier-routing retrofit never got wired into the main dispatch — it's a latent path waiting for a Phase 12+ migration. The dead-code-smell for any Rust function: `grep -c "fn_name" src-tauri/src/ | grep -v llm.rs | head` should show at least one external caller before you wire side effects into it. Phase 4's wanderer caught the same class of bug (`sync_config_to_operational` had no IPC caller) — this is a recurring trap for hooks added to phase-specific scaffolding that hasn't been wired into production yet.

### Non-blocking concerns surfaced by the wanderer (not fixed)

- **Leak sweep has no cancellation token** despite a code comment claiming "the same per-app cancellation pattern as the DADBEAR extend loop." `main.rs:8154-8185` is `tauri::async_runtime::spawn(async move { loop { sleep; run_leak_sweep } })` with no cancellation — on app shutdown the tauri runtime drops the task, so this is not a hard leak, but it's inconsistent with the documented pattern. Fix later by adopting the `CancellationToken` shape from `dadbear_extend::start_dadbear_extend_loop`.
- **Acknowledge-then-reoccur re-degrades immediately** with no grace period. If a provider is acknowledged back to healthy and the next HTTP call fails the same way, `record_provider_error` sees `healthy` and flips straight back. The operator sees the same alert they just dismissed. A 60-second "just acknowledged, give it a breather" window would let real remediations take effect before the UI re-alerts. Not a correctness bug; UX concern.
- **LLM-path health events pass `None` for the event bus** via the fire-and-forget `maybe_record_provider_error` helper's side connection. Only webhook-path degradations emit `ProviderHealthChanged` to the bus. Frontend subscribers (Phase 15 oversight UI) won't see live updates when an outage is detected from the LLM call path — they'll learn about it on the next IPC poll. Not blocking but worth revisiting when Phase 15 wires the event subscriber.
- **`resp.text().await` + `parse_response` mid-body failures are not fed into the health hook.** These are "connection started, server gave headers, then either the body read hung up or the JSON was garbage" cases — they probably shouldn't be classified as 5xx or `ConnectionFailure`, but they're also not a clean "provider is fine" outcome. A fourth `ProviderErrorKind::MalformedResponse` category could capture these. Not blocking.
- **Correlate-by-generation_id does not filter on `broadcast_confirmed_at IS NULL`.** A duplicate broadcast for the same gen_id would re-overwrite `broadcast_confirmed_at` + `broadcast_payload_json` + `broadcast_discrepancy_ratio`. The discrepancy-detection path stays consistent (ratio computation is deterministic) but the write is wasted and a discrepancy row could be RE-flipped via a spurious broadcast. Low frequency in practice; add the guard when Phase 15 tests catch a real duplicate.
- **`augment_request_body` writes BOTH flat trace keys AND nested `trace.metadata.*`** as belt-and-suspenders. OpenRouter's OTLP translator likely promotes both to `trace.metadata.*` attributes, which could result in duplicate attributes in the emitted OTLP span (e.g., both `trace.metadata.slug` and `trace.metadata.metadata.slug`). Doesn't break correlation (the webhook pulls from the nested form) but could be tidied to flat-only once live OpenRouter behavior is verified.
- **Fallback correlate uses (slug, step_name, model) instead of (slug, build_id, step_name).** Spec line 770 recommends `trace.metadata.build_id + trace.metadata.step_name`. The current implementation picks the OLDEST unconfirmed row for a given (slug, step) pair, which means two concurrent builds for the same slug with the same step_name could correlate broadcasts across builds. Low-probability in practice; would show up as false-positive confirmations in multi-build scenarios. Tighten to build_id once Phase 15 starts testing concurrent builds.

---

### 2026-04-10 — Phase 12 wanderer caught the ID-space mismatch + global-supersede deadzone

The Phase 12 verifier pass correctly diagnosed the "retrofit was dead code" cluster and wired cache_access through every production entry point. It also fixed 4 other blocking bugs including the `is_first_build` hardcode and the `block_in_place` runtime flavor panic. What the verifier did not catch: **the entire triage-gate demand signal machinery was joining two disjoint ID spaces**, and **global evidence_policy supersessions silently dropped every re-eval**. Both bugs were structural rather than wiring-level, so the cache-plumbing audit didn't surface them.

### Finding A — HIGH/CORRECTNESS: `question.question_id` is a q-hash, not a node_id (FIXED)

Five call sites tried to join `pyramid_demand_signals.node_id` against `LayerQuestion.question_id`:

1. `evidence_answering::run_triage_gate` (`sum_demand_weight(conn, slug, &question.question_id, ...)`) — the triage gate's `has_demand_signals` predicate.
2. `stale_engine`'s DADBEAR deferred-question scanner.
3. `config_contributions::reevaluate_deferred_questions` (on policy supersession).
4. `main.rs::pyramid_reevaluate_deferred_questions` IPC handler.
5. `demand_signal::record_demand_signal`'s on-demand reactivation hook via `list_deferred_by_question_target(conn, slug, drill_node_id)`.

The ID spaces never meet:
- `LayerQuestion.question_id` is a `q-{sha256_hex_first_12}` hash built by `question_decomposition::make_question_id(question, about, depth)` and assigned via `assign_question_ids` at decomposition time, **before** any answer exists.
- `pyramid_demand_signals.node_id` holds the answered pyramid node's `L{layer}-{seq:03}` id assigned by `answer_single_question` at line 652 of evidence_answering.rs, **after** the question has been answered.
- `pyramid_nodes` has no column that back-references a q-hash. No persistent q-hash → L-id mapping exists anywhere in the schema.

Consequences:
- The triage DSL's `has_demand_signals` condition always evaluated to false. The spec's canonical `"stale_check AND has_demand_signals → answer"` rule could never match.
- The on-demand reactivation hook for `never`/`on_demand` deferred questions was a no-op on every real drill event.
- Global/IPC/DADBEAR re-evaluation paths all had the same structural bug — they'd evaluate every question with `has_demand_signals = false`, so a demand-driven "please re-check" never took effect.

The verifier's earlier fix to `list_deferred_by_question_target` corrected a column-name bug (JSON `target_node_id` → `question_id` column) but preserved the fundamental mismatch — both versions demand `question_id = drill_node_id`, which is never true in the ID space we actually have.

**Fix:** switch all five sites to slug-level demand signal aggregation.

- Added `db::sum_slug_demand_weight(conn, slug, signal_type, window_modifier)` that drops the `node_id` filter and sums across the entire slug.
- Added `db::list_on_demand_deferred_for_slug(conn, slug)` that returns every `on_demand`/`never` deferred row on the slug (dropping the broken per-node join).
- All five sites now compute `slug_has_demand_signals` once per triage pass and apply that single boolean to every question in the batch.
- The `demand_signal::record_demand_signal` reactivation hook iterates the slug-scoped on-demand list, re-triages each with `has_demand_signals=true`, and removes rows whose decision flips to `Answer`.

Per-slug aggregation loses the spatial precision the spec implies, but that precision is unimplementable without a persistent q-hash → node-id mapping (Phase 13+ scope). Per-slug is the correct semantics inside the schema we have and matches the spec's intent ("demand drives re-check").

### Finding B — HIGH/CORRECTNESS: global evidence_policy supersession silently re-evaluated zero rows (FIXED)

`config_contributions::reevaluate_deferred_questions(conn, slug: Option<&str>)` wrote `let slug_str = slug.unwrap_or("");` and then called `list_all_deferred(conn, slug_str)`. For a **global** evidence_policy contribution (`contribution.slug = NULL`), `contribution.slug.as_deref()` at config_contributions.rs:669 passes `None`, which collapsed to `slug_str = ""`, which matched zero rows in the `WHERE slug = ''` query. Every global-policy supersession silently dropped every deferred row re-evaluation.

**Fix:** split the function into a global dispatcher and a per-slug worker. When `slug.is_none()`, the dispatcher walks `list_slugs_with_deferred_questions(conn)` (new helper) and recurses per-slug. The per-slug worker still loads policy via `load_active_evidence_policy(conn, Some(slug))` so per-slug overrides continue to win when they exist.

### Findings that were NOT bugs

Traced each of the 11 wanderer-focus questions end-to-end:

- **Cache retrofit reaches cache in production.** Spot-checked 5 paths (evidence_answering::answer_single_question, faq::process_annotation match path, meta::timeline_forward, stale_helpers::check_file_stale, stale_helpers_upper::dispatch_node_stale_check). All build a cache-usable StepContext with non-empty `resolved_model_id` and `prompt_hash` and route through `..._and_ctx`. The verifier's cache_access plumbing is complete.
- **No wiring gaps on cache_access clones/rebuilds.** Greped every `state.config.read().await.clone()` in the pyramid crate. Only 3 bare clones exist: (a) `chain_executor::retry_dead_letter_entry` (intentional no-cache), (b) `public_html/ascii_art.rs::ascii_handler` (intentional bypass), (c) `main.rs::get_config` (WireNodeConfig, not LlmConfig — different type). Every production LLM path goes through `llm_config_with_cache` or `attach_cache_access`.
- **`is_first_build` DB lookup correct.** Single atomic SELECT; no TOCTOU. Depth-0 filter matches spec. SQLite errors default to `false` (safer than spurious-match `true`).
- **DSL evaluator vocabulary complete.** Recursive-descent grammar with correct C-style precedence. `depth == N` handled specially. `evidence_question_trivial`/`_high_value` default to `false` when the classifier didn't run (safe fallback). `rule_to_decision` unknown actions fall through to Answer.
- **Deferred question data integrity.** UPSERT on `(slug, question_id)` prevents double-defer. SQLite writer lock serializes remove vs update races to zero.
- **Retrofit step metadata correct.** 3 spot-checked sites have distinct `(step_name, primitive, depth, chunk_index)`. Cache key is `(inputs_hash, prompt_hash, model_id)`, so two retrofit sites with identical content share a cache row — semantically correct.
- **`block_in_place` runtime-flavor wrap correct.** Both probe and store paths dispatch on `Handle::try_current()` → `runtime_flavor()`; MultiThread donates the thread, CurrentThread runs inline. DB open + SELECT is sub-ms so inline is safe.
- **DADBEAR scanner non-blocking.** Runs inside `spawn_blocking` with its own DB connection; doesn't hold the writer mutex.

### Non-blocking concerns surfaced (not fixed)

- **The `make_step_ctx_from_llm_config` helper hardcodes `with_model_resolution("primary", config.primary_model.clone())`** — every retrofit site using this helper records `tier_id = "primary"` regardless of what tier the call actually resolves through. Minor telemetry inaccuracy; doesn't affect cache correctness because `tier_id` isn't in the cache key. Fix later by threading the resolved tier through the helper.
- **`evidence_answering::answer_single_question` at line 904 hardcodes `with_model_resolution("fast_extract", ...)`** regardless of the actual tier resolved for the answering call. Same minor telemetry inaccuracy; same fix-later note.
- **`rule_to_decision`'s `TagForLog` trait is a no-op placeholder.** The implementer flagged it in their log as dead code. Cosmetic only; trait method is called but does nothing. Safe to delete in a cleanup pass.
- **Phase 12's retrofit table has a `search_hit` signal recording deferral** — Wire Node can't tell "drill came from a search" without a session tracking mechanism. Phase 13+ scope per the implementer's deviation note. Not a wanderer fix.
- **A persistent q-hash → node-id mapping would let the triage gate's demand signals go back to spatial precision** instead of slug aggregation. Proposal for Phase 13+: add `pyramid_question_node_map(slug, question_id, node_id)` populated at answer persistence time in `chain_executor.rs:5265` where `save_node` already knows both identities. Once that table exists, `sum_demand_weight` can replace `sum_slug_demand_weight` at every wanderer-fixed site, and `list_deferred_by_question_target` can do a real join through the map.

### Commit

`phase-12: wanderer fix — slug-level demand signal aggregation + global-policy reeval dispatcher`

### Verification after fix

- `cargo check --lib`: 3 pre-existing warnings only. No new warnings.
- `cargo test --lib pyramid`: **1101 passing / 7 failing** (baseline 1099/7). Delta: +2 new tests (`test_sum_slug_demand_weight_aggregates_across_nodes`, `test_list_on_demand_deferred_for_slug`). Same 7 pre-existing failures.

### Lesson for future phases

**When a phase introduces a new join across two subsystems, grep for the column names on both sides and verify they share an ID space.** Phase 12's implementer and verifier both assumed `question_id == target_node_id` because of comments in `question_decomposition.rs` that set `question_id = node.id`, where `node` is a QuestionNode (q-hash), not a PyramidNode (L-id). The same identifier name across two types masked the mismatch. A grep for `question_id` usages in both `question_decomposition.rs` and `pyramid_nodes` would have surfaced that `pyramid_nodes` has no such column — which is the tell that the join can't work. The wanderer's value here is stepping outside "does the wiring connect?" to ask "do the two ends of the wire carry the same thing?"

---

## Phase 13 — Build viz expansion + reroll + cross-pyramid

### Wanderer pass (2026-04-10)

Traced all 12 wanderer questions end-to-end. Found two real bugs the verifier's punch-list audit missed — one a subtle event-ordering issue in the React reducer, one a load-bearing cache-key invariant violation in the reroll backend that cascaded into four other visible features.

### Bugs fixed

**W1 — `derivedStepStatus` early-returns on `retrying`; retry loop pollutes `step.calls` (`src/hooks/useBuildRowState.ts:164-201`, `:266-289`)**

The reducer's aggregate-status helper had `if (step.status === 'failed' || step.status === 'retrying') return step.status;` at the top. Once `step_retry` flipped the step to `retrying`, no subsequent `llm_call_completed` could compute a terminal status — the helper short-circuited on the stale `retrying` string and the step was stuck forever.

Even with that guard removed, a secondary bug lurked: `llm.rs::call_model_unified_with_options_and_ctx` re-emits `LlmCallStarted` inside its retry loop on every attempt. The reducer's `llm_call_started` handler does `step.calls.push(...)` unconditionally, so a retried-then-succeeded step ends up with `step.calls = [{cacheKey, status: 'retrying'}, {cacheKey, status: 'completed'}]`. The aggregate logic walked BOTH entries and found `allCompleted = false` (first entry is `'retrying'`) and `anyFailed = false`, landing on `'running'` instead of `'completed'`. Retries silently left steps stuck at `running` in the UI.

**Fix:**
1. Drop the `'retrying'` early-return from `derivedStepStatus` (keep the `'failed'` early-return — that's still terminal).
2. Re-derive status from "last call per cache_key" so stale retry markers are ignored; the last attempt per key wins. Calls with an empty cache_key (e.g., the reroll's non-cache-aware path) get synthetic slots so they're considered independently.
3. In the `llm_call_started` handler, preserve `step.status === 'retrying'` when a retry's fresh attempt fires, so the UI keeps showing `retry N/M` until the attempt either completes or fails.

**W2 — Reroll cache-key mismatch breaks the ENTIRE reroll flow (`src/pyramid/reroll.rs:120-210`, `:378-465`)**

The reroll path threaded `with_prompt_hash(prior.prompt_hash)` onto the StepContext, making `cache_is_usable() = true`. `call_model_unified_with_options_and_ctx` then computed `cache_key = hash(hash(reroll_system, reroll_user), prior_prompt_hash, prior_model_id)` — **different** from `prior.cache_key` because `build_reroll_prompts` intentionally wraps the original output in a "rerolling a prior output" template. That different cache_key cascaded into five broken user-visible behaviors:

1. `supersede_cache_entry(slug, NEW_cache_key, entry)` found no prior row at the NEW key, so it inserted the rerolled row as a fresh entry with `supersedes_cache_id = NULL` and never archived the original.
2. The original row remained untouched at `prior.cache_key`.
3. `load_new_cache_row(slug, prior.cache_key)` loaded the UNTOUCHED original — the reroll IPC returned the old row's `id` as `new_cache_entry_id` to the frontend.
4. `apply_note_to_cache_row(new_cache_entry_id, note)` wrote the reroll note onto the ORIGINAL row, not the rerolled row.
5. `count_recent_rerolls` — which gates the anti-slot-machine rate limit on `WHERE supersedes_cache_id IS NOT NULL` — never counted the reroll. **The rate limit was effectively disabled.**
6. On subsequent normal builds, the lookup computed `prior.cache_key` from the ORIGINAL prompts and hit the UNTOUCHED original row, serving the pre-reroll content. **The reroll never took effect on future builds.**

The root invariant the implementer missed: `supersede_cache_entry` only works when the new row's cache_key matches the prior row's cache_key. The reroll wrapper prompts intentionally violate that invariant.

**Fix:** bypass the cache-aware path for the reroll and route the DB write manually via a new `write_reroll_cache_entry` helper. The helper:
- constructs the StepContext WITHOUT `with_prompt_hash`, so `cache_is_usable() = false` and the LLM path skips its automatic lookup/store entirely (events still fire because `ctx.bus.is_some()`),
- builds a `CacheEntry` with `cache_key = prior.cache_key`, `inputs_hash = prior.inputs_hash`, `prompt_hash = prior.prompt_hash`, `model_id = prior.model_id` so `verify_cache_hit` passes on read-back,
- calls `db::supersede_cache_entry` directly, which archives the prior row under `archived:{id}:{prior.cache_key}` and inserts the rerolled row at `prior.cache_key` with `supersedes_cache_id = prior_id` and `force_fresh = true`,
- persists the user's note via `CacheEntry.note` (no post-write UPDATE needed),
- returns the new row's id via a follow-up `check_cache_including_invalidated` read.

`load_new_cache_row` and `apply_note_to_cache_row` were deleted — they were only reachable from the broken auto-store path.

### Findings that were NOT bugs

Traced each of the 12 wanderer-focus questions end-to-end:

- **Q1/Q11 — step timeline event ordering.** `LlmCallStarted` is emitted INSIDE the retry loop after `try_cache_lookup_or_key` has already short-circuited cache hits. Cache hits fire `CacheHit` + `return Ok(response)` BEFORE `LlmCallStarted`. Cache misses fire `CacheMiss` and fall through to the loop. So the event sequence is always either `CacheHit` alone OR `(LlmCallStarted[, StepRetry]*, LlmCallCompleted | StepError)`. Never both. `llm.rs:537-538, 637, 963`.
- **Q2 — cache-hit savings accumulator.** Uses a heuristic `avgCost = step.totalCostUsd / max(1, step.cacheMisses)` because the Phase 6 `CacheHit` variant doesn't carry `original_cost_usd` (pre-dates Phase 13's expanded payloads). Savings are approximate — zero on the first call of a fully-cached step — but the implementer documented this as a deliberate deviation ("A future refinement can thread the original cost through the event"). Not a wanderer fix, just a known limitation.
- **Q6 — cross-pyramid timeline seeding.** `pyramid_active_builds` returns `Ok(Vec::new())` (not an error) when the active-build map is empty. The frontend hook seeds from the IPC, polls every 30s as a safety net, and subscribes to `cross-build-event`. Works as spec'd.
- **Q7 — cost rollup SQL + client-side pivot.** `GROUP BY slug, provider_id, operation`. Frontend iterates distinct `(slug, provider, operation)` triples and pivots into three views. No double-counting. `db.rs::cost_rollup:10918-10951`, `CostRollupSection.tsx:36-63`.
- **Q8 — Pause All respects `enabled = 0`.** `dadbear_extend.rs::dadbear_tick_loop` reloads configs every 1 second via `load_enabled_configs → get_enabled_dadbear_configs`, which filters on `enabled = 1`. Paused rows are skipped immediately on the next reload tick. `disable_dadbear_all` is idempotent (`WHERE enabled = 1`). `dadbear_extend.rs:139`, `db.rs:10845-10857, 10881-10901`.
- **Q10 — cross-pyramid router lifetime.** The forwarder task runs indefinitely — fine, it's a singleton. `active_slugs` lazy-populates inside the forwarder (F5 deviation documented by the verifier). Entries with `unregistered = true` are pruned after 60s, but nothing calls `unregister_slug`, so the map grows monotonically with every distinct slug ever seen. Slow unbounded growth, practically bounded by the user's pyramid set (~10s). Minor known leak, not a wanderer fix.
- **Q12 — downstream invalidation walker scope.** The implementer's log claims "single-level" but the code uses `SELECT ... WHERE depth > rerolled_depth` in `find_downstream_cache_keys`, which invalidates EVERY deeper row regardless of whether it actually depended on the rerolled content. The implementer's deviation #3 acknowledges this over-invalidation. The workstream prompt allowed it ("ship node-level invalidation only"). Not a bug, just a comment/log wording mismatch.

### Non-blocking concerns surfaced (not fixed)

- **Reroll wrapper template vs original prompt template.** The reroll sends a "you are rerolling a prior output" system prompt + a user prompt containing the prior output + the note. A future refinement could thread the ORIGINAL prompt template body through cache metadata and have the reroll replay the original prompts with the note injected — that would make the rerolled content match the original call's shape more closely. Out of scope for the wanderer fix; the current template is functionally correct because the resulting row stores `inputs_hash = prior.inputs_hash` (semantic lie but cache-correct).
- **Cache savings heuristic in `useBuildRowState.ts::cache_hit` handler.** Computes `avgCost = step.totalCostUsd / max(1, step.cacheMisses)` which is zero on the first call of a fully-cached step. A real fix would extend the `CacheHit` TaggedKind with `original_cost_usd` and `original_model_id` fields (per the Phase 13 spec's original intent), and the reducer would use the actual saved cost. That touches Phase 6's event shape — non-trivial blast radius. Wanderer left alone.
- **`CrossPyramidEventRouter::register_slug` / `unregister_slug` never called from production (F5 deviation documented by the verifier).** The router's explicit lifecycle hooks are dead. Runtime behavior matches the spec via lazy auto-population inside `spawn_tauri_forwarder`, but the 60-second grace window is effectively "forever" because nothing ever flips `unregistered = true`. Minor leak, not a wanderer fix.
- **Reroll's node→cache_key lookup is a text search (`lookup_cache_entry_for_node`).** `SELECT ... WHERE output_json LIKE '%{node_id}%'` — relies on the node_id appearing verbatim in the cache row's output_json, which holds for the current chains but is fragile against future prompt changes that wrap or reformat node ids. Implementer deviation #2 documented this; a cleaner path is a future schema refinement adding `cache_key` to `pyramid_nodes`. Not a wanderer fix.
- **Step timeline callIndex on empty cache_key.** The reroll's non-cache-aware ctx produces `LlmCallStarted` events with `cache_key = ""`. The reducer's `next.callIndex.set("", ...)` could collide with other events carrying empty keys, but in practice the reroll is a single one-off call and no concurrent call shares the empty-key slot. With the wanderer fix to `derivedStepStatus` using synthetic `__no_key_N` slots, the aggregate logic is already robust against the collision.

### Commits

1. `phase-13: wanderer fix — reroll cache key mismatch + retry status derivation`

### Verification after fix

- `cargo check --lib`: 3 pre-existing warnings only. No new warnings.
- `cargo test --lib pyramid`: **1137 passing / 7 failing** (baseline 1135 after verifier fix + 2 wanderer regression tests). Same 7 pre-existing failures (2 defaults_adapter + 5 staleness tests documented in every prior phase log).
- `cargo test --lib pyramid::reroll`: 9 passing / 0 failing (all reroll tests including the 2 new wanderer regression tests).
- `npm run build`: clean, 140 modules transformed, no new TypeScript errors.
- Code traces for Q1-Q12 recorded above; all 12 answered with file:line citations.

### Lesson for future phases

**When a reroll path uses `supersede_cache_entry`, the new row's cache_key MUST match the prior row's cache_key — otherwise the supersession chain silently detaches.** The invariant isn't documented on `supersede_cache_entry` itself — its signature just takes `(slug, prior_cache_key, new_entry)` and you'd reasonably assume the helper handles the key-matching internally. It doesn't. If `new_entry.cache_key != prior_cache_key`, the helper finds no prior row, skips the archival step, and inserts the new row at its own key — the supersession semantics only work when the content-addressable invariant (`cache_key = hash(inputs_hash, prompt_hash, model_id)`) stays the same on both sides. The reroll path violates this by construction (wrapper template produces different prompts → different inputs_hash → different cache_key), and the fix is to bypass the cache-aware write path entirely and build the `CacheEntry` manually with the prior cache_key.

**The wanderer's value on a retried-then-succeeded path was: trace what the reducer actually computes vs what the spec says should display.** The verifier checked that `StepRetry` events fire and the reducer has a `step_retry` case. The wanderer simulated the event sequence `LlmCallStarted → StepRetry → LlmCallStarted → LlmCallCompleted` in their head and traced through the aggregate logic line by line, finding two orthogonal bugs: the early-return guard and the stale retry-marker pollution of `step.calls`. Event reducers are tricky because the state is observable only by sending events through them — static analysis of the switch statements rarely catches the pathological sequences.

---

## Phase 14 — Wire discovery + ranking + recommendations + update polling

### Wanderer pass (2026-04-10)

Traced all 13 wanderer questions end-to-end against the implementer commit (`de464a1`) + verifier fix commit (`ea68bdb`). Found two real bugs the verifier's punch-list audit missed, both landing on the same code path in `src-tauri/src/pyramid/wire_pull.rs`. The two bugs share a fix — an atomic transaction-scoped resolution of the current active row — so they're addressed in a single commit.

### Bugs fixed

**W1 — `pyramid_pull_wire_config` with `activate=true` doesn't supersede existing active rows (`src-tauri/src/pyramid/wire_pull.rs:193-240`)**

The IPC takes a `wire_contribution_id + slug + activate` triple and passes `local_contribution_id_to_supersede: None` unconditionally. The implementer's `pull_wire_contribution` branched on the presence of that hint: with a hint, run `supersede_with_pulled`; without, run `insert_pulled_contribution` — which is a fresh insert with `status='active'`, `supersedes_id=NULL`. The fresh-insert path does not touch any existing active row.

Reproduction (no race required, hits on every Discover-tab "Pull and activate"):

1. Bundled manifest seeds `custom_prompts` with `contribution_id = bundled-custom_prompts-default-v1`, `status = 'active'`.
2. User opens ToolsMode → Discover, picks `custom_prompts`, finds a compelling Wire contribution, clicks the "Pull and activate" button in `DiscoveryDetailDrawer`.
3. Frontend calls `invoke('pyramid_pull_wire_config', { wireContributionId, slug: null, activate: true })` (`ToolsMode.tsx:2159-2166`).
4. Backend `pyramid_pull_wire_config` passes `local_contribution_id_to_supersede: None` (`main.rs:8129`).
5. `pull_wire_contribution` routes through the `else` arm at the old `wire_pull.rs:214-225` and calls `insert_pulled_contribution` with `status = "active"`.
6. A new row lands active. The bundled row is still active with `superseded_by_id IS NULL`. `load_active_config_contribution` now has **two candidates** for `(schema_type='custom_prompts', slug=NULL)`; the `LIMIT 1 ORDER BY created_at DESC, id DESC` tiebreaker returns the newest row so runtime behavior looks fine, but the history chain is silently corrupted and every subsequent pull accumulates another orphan active row.

Consequences beyond the surface symptom:

- `load_config_version_history` walks `supersedes_id` starting from the "newest active" and can never see the orphaned older active rows — they're invisible in the UI's version list.
- `pyramid_wire_update_poller::list_wire_tracked_contributions` returns every active row with a `wire_contribution_id`, including the orphans, wasting a Wire round-trip slot per orphan on every polling cycle.
- If a user pulls the SAME Wire contribution twice (duplicate-click, browser reload), two rows land with identical `wire_contribution_id` values. The poller's `check_supersessions` input list contains the dup.
- Rebuilds that source tier routing / evidence policy from the `pyramid_config_contributions` row (via the contribution_id FK on the operational table) silently bind to the LAST-inserted active row rather than the user's intended one.

This is the "Phase 10 stub alias shipped for real" path and it trips on the most common end-to-end Discover use case. The Phase 14 workstream prompt's `pyramid_pull_wire_config` line item just says "brand-new pull, not supersession" — the implementer encoded that literally, but the frontend always routes Discover "Pull and activate" through this IPC regardless of whether a local version already exists, because the UI has no way to know the local state of every schema type at drawer-open time.

**W2 — `supersede_with_pulled` has no idempotency guard; concurrent poller+user pull corrupts the chain (`src-tauri/src/pyramid/wire_pull.rs:313-361` pre-fix)**

The old `supersede_with_pulled` helper was passed a `prior: &ConfigContribution` loaded BEFORE the transaction opened, then ran an unconditional `UPDATE … SET status='superseded', superseded_by_id=?` against that prior's `contribution_id` inside the transaction. No predicate checked whether the prior was still active — unlike `supersede_config_contribution` in `config_contributions.rs:267-271`, which explicitly bails with `prior contribution X is already superseded — cannot supersede a non-active version`.

Reproduction (race between poller's auto-update path and user's manual pull):

1. User has local `L1` active, `wire_contribution_id = W1`. Wire has published `W2` superseding it. `wire_auto_update_settings.custom_prompts = true`.
2. Poller cycle starts, acquires writer, calls `try_auto_update` → `pull_wire_contribution(..., local_contribution_id_to_supersede: Some(L1), activate: true)`. Inside `supersede_with_pulled`: transaction opens, inserts `L2` active, UPDATEs `L1` → `status='superseded', superseded_by_id=L2`. Transaction commits. Writer released. Poller deletes the `pyramid_wire_update_cache` row for L1.
3. **Meanwhile**, the user had opened the My Tools tab BEFORE the poller ran, so the UI had a cached `WireUpdateEntry` with `local_contribution_id=L1`. They click "Pull latest" in the drawer just after the poller releases the writer.
4. `pyramid_wire_pull_latest(L1, W2)` acquires the writer, loads L1 via `load_contribution_by_id` — L1 exists but is now `status='superseded'`. The old code proceeds: `supersede_with_pulled(conn, prior=L1, …)` opens a new transaction and runs the unconditional UPDATE on L1.
5. Now L1's `superseded_by_id` is overwritten from `L2` to `L3`. The transaction inserts `L3` with `supersedes_id=L1, status='active'`.
6. Final state: L1 superseded (by L3 — L2 is orphaned from L1's perspective), L2 `status='active'` (still, because the UPDATE only touched L1), L3 `status='active'`. **Two active rows, corrupted supersession chain: L1→L3 but L2→? dangling.**

The reverse interleaving (user pull wins the writer, poller follows) hits the same bug via `try_auto_update` finding L1 still in `list_wire_tracked_contributions` (it is, because it has `wire_contribution_id != NULL`) and calling `pull_wire_contribution` with `Some(L1)`. The poller's pull then clobbers L1's supersession pointer the same way.

The race window is narrow (requires auto-update enabled AND the user holds a stale UI view), but the invariant break is permanent once it happens and there's no self-healing path — the orphan row lingers until the user manually deletes or supersedes it through another flow.

### Fix

Both W1 and W2 share a root cause: the pull flow captures an externally-supplied "which row to supersede" hint and trusts it without re-checking the real state at transaction time. The fix eliminates the hint entirely for the activate path and builds a new helper, `commit_pulled_active`, that:

- Takes `(schema_type, slug, yaml, note, metadata, wire_id)` — NO prior ID, ever.
- Opens a transaction.
- Resolves the CURRENT active row via the same predicate as `load_active_config_contribution` (`slug = ? AND schema_type = ? AND status = 'active' AND superseded_by_id IS NULL`), using the slug-branching idiom for NULL-safety.
- Inserts the new row with `supersedes_id = prior_active_id` (NULL when no prior exists — fresh-insert case still works).
- UPDATEs the prior row ONLY if `prior_active_id.is_some()`, with a predicate guard (`WHERE contribution_id = ? AND status = 'active' AND superseded_by_id IS NULL`) that no-ops if the row has been flipped by a racing writer.
- Commits.

The `insert_pulled_contribution` helper is preserved for the `activate=false` (proposed) path, where the row lands with `status='proposed'` and doesn't interact with the active-row invariant.

The `supersede_with_pulled` helper and the `pull_wire_contribution` branch on `options.local_contribution_id_to_supersede` are both deleted — the `local_contribution_id_to_supersede` field in `PullOptions` is still honored structurally (callers still pass it) but it's now ignored for correctness purposes. The in-transaction resolution is the authoritative source.

### Findings that were NOT bugs

Traced each of the 13 wanderer-focus questions end-to-end:

- **Q1 — Discovery search end-to-end.** `ToolsMode.tsx:2128-2151` calls `invoke('pyramid_wire_discover', { schemaType, query, tags, limit, sortBy })`; Tauri converts camelCase → snake_case; `main.rs:7817-7851` loads weights synchronously then awaits the HTTP fetch; `wire_discovery::discover` → `rank_raw_results` → sort + score + rationale. Frontend renders `DiscoveryResultCard` with `QualityBadges` and the rationale string at `ToolsMode.tsx:2484-2495`. Sort dropdown round-trips through `from_str_lax` which handles every documented value and falls back to `Score` for unknown strings with a warning log.
- **Q2 — Recommendations profile.** Built from REAL data: `source_type` from `pyramid_slugs.content_type` (NOT NULL CHECK-constrained at `db.rs:56`), `tier_routing_providers` from `pyramid_tier_routing.provider_id` distinct list. `build_pyramid_profile` at `wire_discovery.rs:667-702`. No placeholders.
- **Q4 — Update poller honors per-schema auto-update toggle.** `auto_update_settings.is_enabled(schema_type)` at `wire_update_poller.rs:322` gates `try_auto_update`. Settings are loaded once per cycle at `wire_update_poller.rs:265-268` via `load_auto_update_settings(reader)`, so supersessions of the settings contribution take effect on the next cycle without restart. The spec requires this and the implementation delivers.
- **Q5 — Update drawer end-to-end.** Badge renders when `wireUpdates.find(...)` matches a ConfigCard's schema_type AND (no active contribution OR active.contribution_id matches) at `ToolsMode.tsx:474-480`. Click opens `WireUpdateDrawer`. "Pull latest" button invokes `pyramid_wire_pull_latest` which DOES pass `local_contribution_id_to_supersede: Some(&local_contribution_id)` at `main.rs:8086` — so (before the wanderer fix) this path went through `supersede_with_pulled` which has the race issue but not the W1 always-duplicate issue. After the wanderer fix, the path routes through `commit_pulled_active` which resolves the prior inside the transaction. Badge disappears via `bumpRefresh()` refetching `pyramid_wire_update_available`, which filters by `acknowledged_at IS NULL` + checks the cache row was deleted by the pull flow.
- **Q6 — Auto-update toggle round-trip.** Modal reads via `pyramid_wire_auto_update_status`, flips via `pyramid_wire_auto_update_toggle` which constructs a new `wire_auto_update_settings` YAML, supersedes the prior contribution via `supersede_config_contribution` (not the wire_pull path — unrelated to W1/W2), then calls `sync_config_to_operational` which invalidates caches. Next poller tick re-reads. Clean.
- **Q7 — Weight redistribution math.** Manually walked the case: `adoption=50, freshness=30d, chain_length=2, rest None, max_adoption=100`. Normalized: `adoption ≈ 0.851, freshness = 0.833, chain = 0.2`. `present_weight_sum = 0.20+0.15+0.10 = 0.45`. Score = 0.851×(0.20/0.45) + 0.833×(0.15/0.45) + 0.2×(0.10/0.45) ≈ 0.378+0.277+0.044 ≈ 0.700. Matches the spec's "redistribute missing weights" requirement. Test `test_compute_score_with_redistributed_weights` covers the sparse-vs-full parity. Implementation caveat: `from_search_result` ALWAYS sets `adoption_count = Some(r.adoption_count)`, so the Wire's 0-adoption signal is `Some(0)`, not `None`. Brand-new-contribution redistribution only applies when the Wire actually sends NULL for a signal — a narrower case than the spec intent but documented in the struct comments.
- **Q8 — Credential safety gate edge cases.** `CredentialStore::collect_references` is a raw-byte scanner that (a) detects `${VAR}`, (b) handles `$${NOT_A_VAR}` escape by consuming the `$$...}` block without recording it, (c) does NOT YAML-parse — comment lines like `# ${OLD_VAR}` WILL match. The verifier pass documented this as a known tradeoff: "the safety-first position is any unresolved `${VAR_NAME}` in the YAML is a blocker". Confirmed behavior in `credentials.rs:420-458`.
- **Q9 — Concurrent poller+pull race.** See W2 above. Fixed.
- **Q10 — Bundled idempotency.** `walk_bundled_contributions_manifest` uses `INSERT OR IGNORE` keyed on the explicit `contribution_id`. If the user has refined a bundled default, the bundled row still exists (marked `superseded`) and the INSERT OR IGNORE is a no-op. User's refinement remains active. Verified at `wire_migration.rs:978-1013`.
- **Q11 — WireUpdatePoller sidecar lifetime.** `main.rs:9024` leaks the handle via `std::mem::forget`. The Drop impl of `WireUpdatePollerHandle` + the SidecarHandle's watchdog-clearing Drop are therefore dead code in production — the sidecar thread runs until process exit, which kills all threads. Not a leak in practice (OS cleanup), but the "graceful shutdown" machinery is effectively decorative for the production path. Documented intent in the `mem::forget` comment; not a wanderer fix.
- **Q12 — Weights cache invalidation.** `pyramid_accept_config` → `accept_config_draft` → `sync_config_to_operational_with_registry` → `wire_discovery_weights` branch at `config_contributions.rs:751-758` → `invalidate_wire_discovery_cache()` → `wire_discovery::invalidate_weights_cache()`. Next `load_ranking_weights` cache-miss re-reads from SQLite. No stale-weight window. `ToolsMode.tsx` Discover tab's weights are read per-search inside `pyramid_wire_discover`, so the fresh weights apply on the very next search after the supersession.
- **Q13 — Frontend error handling.** `DiscoverPanel` shows "No results. The Wire's discovery endpoint may not be live yet — …" when the IPC returns `[]` (`ToolsMode.tsx:2366-2377`). Errors surface via a red `<p>{error}</p>` banner. Pull errors detect credential-related strings and render a tailored "Pull refused — … Add the missing credentials in Settings → Credentials, then retry." message. `WireUpdateDrawer` renders errors in a red panel at `ToolsMode.tsx:1050-1062`.

### Non-blocking concerns surfaced (not fixed)

- **Writer lock held across HTTP in the pull path.** `pull_wire_contribution` takes `&mut Connection` and holds it across `publisher.fetch_contribution(...).await` because the Connection is borrowed from the caller's `writer.lock().await` MutexGuard. For a slow Wire response, the writer mutex is blocked for seconds, starving every other write IPC. The auto-update path in `try_auto_update` has the same pattern. A future refinement would split the fetch (no lock) from the commit (lock) and re-validate invariants before committing — shape matches the Q3 fix above but adds lock-free fetches. Out of scope for the wanderer fix.
- **Missing-signal redistribution ineffective for `adoption_count`, `chain_length`, `freshness_days=0`, `upheld/filed_rebuttals`.** `from_search_result` treats all of these as `Some(0)` rather than `None`, so brand-new contributions with zero adoption/no chain get concrete normalized values of 0.0 instead of triggering the redistribution path. The spec's fair-shot intent applies only to `rating`, `reputation`, `internalization` (when total_pullers=0), and `freshness_days == u32::MAX`. Documented in the `RankingSignals::from_search_result` struct comment as "conservative: treat 0 adoption as `Some(0)` (tracked)". Not a bug but worth calling out if Adam's test results show new contributions ranking too low.
- **`pyramid_pull_wire_config`'s `activate` option defaults to `false`** (`main.rs:8130`). The frontend passes `true` when the user clicks "Pull and activate" and `false` for "Pull as proposal", so the default only matters for programmatic callers. Current behavior is correct.
- **`WireUpdatePoller` reads the Wire auth token from `PyramidState.config.auth_token` or the `WIRE_AUTH_TOKEN` env var**, NOT the canonical `AuthState` held outside `PyramidState`. The implementer documented this as a coupling shortcut. Missing auth → poller skips cycles cleanly. Future wiring task: thread the real `AuthState` through.
- **`pyramid_wire_update_available` enriches cache rows by calling `load_contribution_by_id` per row** (`main.rs:7923-7949`). That's N+1 queries for N cache entries. Acceptable for tens of entries, gets expensive at hundreds. A single JOIN would be cleaner. Not a wanderer fix — Adam's use case has ~10s of entries per node.

### Commits

1. `phase-14: wanderer fix — atomic active-row resolution in wire pull flow`

### Verification after fix

- `cargo check --lib`: 3 pre-existing warnings only. No new warnings.
- `cargo test --lib pyramid`: **1170 passing / 7 failing** (baseline 1166 + 4 new wanderer regression tests). Same 7 pre-existing failures.
- `cargo test --lib pyramid::wire_pull`: **7 passing / 0 failing** (3 existing credential gate tests + 4 new wanderer regression tests: `test_commit_pulled_active_supersedes_existing_active`, `test_commit_pulled_active_ignores_stale_prior_hint`, `test_commit_pulled_active_inserts_fresh_when_no_prior`, `test_commit_pulled_active_isolates_by_slug`).
- `npm run build`: clean, 141 modules transformed, no new TypeScript errors.
- Code traces for Q1-Q13 recorded above with file:line citations.

### Lesson for future phases

**When an activate-path pull can land a row with `status='active'`, the supersession invariant (`exactly one active row per (schema_type, slug) pair`) has to be enforced INSIDE the transaction that inserts the row — never via a caller-provided "which row to supersede" hint.** Hints capture the state at the caller's call site, which may be several `.await` points ago; by the time the transaction opens, a racing writer can have flipped that exact row, and the unconditional UPDATE becomes a data-corruption primitive.

The fix is mechanical: take the hint as an optional UX preference but always re-resolve the authoritative current-active via the same predicate (`status='active' AND superseded_by_id IS NULL`) inside the transaction, with a `WHERE` guard on the UPDATE that no-ops if the row was concurrently mutated. This is the pattern `accept_config_draft` in `generative_config.rs:785-852` already uses — the Phase 9 wanderer fix retrofitted the direct-YAML accept path to do exactly this. Phase 14's `supersede_with_pulled` was a regression of the same anti-pattern; the wanderer fix brings it in line.

**The wanderer's value here was in seeing that the Phase 10 alias `pyramid_pull_wire_config` always passes `local_contribution_id_to_supersede: None` — the verifier's punch-list audit checked that the primary `pyramid_wire_pull_latest` IPC hit `supersede_with_pulled` (which it did), but never asked what the alias's fresh-insert branch does when an active row already exists.** Alias IPCs are a classic place for behavioral drift because the "alias" framing implies "it's just a name change" — in reality, the alias often hits a different code path with different assumptions, and that path needs its own end-to-end trace.

---

## Phase 15 — DADBEAR Oversight Page

### Deviation 1: In-flight stale check detection not from `pyramid_stale_check_log`

**Friction:** the workstream prompt specifies `COUNT(*) FROM pyramid_stale_check_log WHERE slug = ? AND completed_at IS NULL` for the Oversight page's `in_flight_stale_checks` field. The actual schema has no `completed_at` or `started_at` column on `pyramid_stale_check_log` — every row in that table is a completed check with `checked_at`, `stale`, and `reason` populated at insert time. The prompt anticipated this: "if there's no `completed_at` column, use a different signal".

**Resolution:** derive in-flight state from the existing `PyramidState::dadbear_in_flight` AtomicBool map, which is keyed by `pyramid_dadbear_config.id` and already tracks per-config dispatch state (set when the tick loop fires, cleared when it returns). This is the authoritative runtime signal — the same map the tick loop and the HTTP/CLI manual trigger already consult to avoid re-entrant dispatch. The IPC takes a snapshot of the flags, then per slug, filters the snapshot to the config ids belonging to that slug and counts the `true` entries.

**Why this is not a Pillar 37 violation:** the fix routes through an existing user/tick-loop-controlled state primitive rather than introducing a new number or hardcoded threshold. The count faithfully reflects "how many of this slug's DADBEAR configs are currently dispatching". No new constants, no magic windows, no silent fallback — just reusing the map that already exists for the same purpose.

### Deviation 2: per-pyramid `pyramid_dadbear_pause`/`_resume` IPCs did not exist

**Friction:** the prompt said to "check first" for these IPCs. At the start of Phase 15, only the Phase 13 `*_all` variants were registered in `main.rs`. The DB-level helpers (`enable_dadbear_for_slug`, `disable_dadbear_for_slug`) existed but were unreachable from the frontend.

**Resolution:** added two thin IPC wrappers (`pyramid_dadbear_pause`, `pyramid_dadbear_resume`) that call the existing DB helpers and return `{ ok: bool, affected: usize }`. No new DB code.

### Deviation 3: `pyramid_acknowledge_orphan_broadcast` IPC did not exist

**Friction:** prompt said "check first". Did not exist. Phase 11 shipped `pyramid_list_orphan_broadcasts` but not the ack counterpart.

**Resolution:** added the IPC. Updates `pyramid_orphan_broadcasts.acknowledged_at` + `acknowledgment_reason` with a predicate guard so re-acks are idempotent no-ops.

### Deviation 4: Page placed as a tab within `PyramidsMode`, not a new top-level Mode

**Friction:** the prompt says "new top-level mode/tab/route — recommend a new top-level page that shares the `useBuildRowState` hook". The existing `PyramidsMode` already has two tabs (Dashboard, Builds) that are conceptually adjacent to Oversight, and the sidebar already surfaces "Understanding" (pyramids).

**Resolution:** added a third tab "Oversight" within PyramidsMode. Keeps the sidebar uncluttered, puts the Oversight surface alongside the Builds timeline where operators already look for DADBEAR status. The prompt explicitly permitted either choice ("tab on the same page or create a new page").

### Deviation 5: `CostRollupSection` removed from `CrossPyramidTimeline` entirely

**Friction:** the prompt says "Remove the Phase 13 mount OR keep it in both places (a small duplication is fine). Document the choice."

**Resolution:** removed from CrossPyramidTimeline. Spec intent is that the Oversight page is the canonical home; having it in two places would split user expectation for where to find the spend pivot view. The compact live footer (`CrossPyramidCostFooter`) still lives on the Builds tab.

### Deviation 6: Preset bridge as module-level singleton + CustomEvent, not AppContext extension

**Friction:** the prompt says "Wire the button to open the Phase 9/10 `CreatePanel` workflow with those preset values. If that's not trivially possible due to how Phase 10 wired the CreatePanel, open a new modal that dispatches the same invoke calls."

**Resolution:** the Phase 10 CreatePanel already takes a `seed: CreateSeed | null` prop that drives an "edit existing" flow, but "Set Default Norms" doesn't need a seeded draft — it just needs to pre-select the `dadbear_policy` schema and jump to the intent step. Added a separate `preset` prop (`ToolsModePreset`, same shape as pick-schema arguments) with its own effect. The cross-component bridge is a module-level one-shot variable (`takeToolsModePreset`) + a `wire-node:tools-mode-preset` CustomEvent. Rationale: the preset is ephemeral cross-component handoff data (not persistent app state), so AppContext is the wrong place for it. This avoids polluting the reducer with a prop that's always empty except for one mode transition.

### Non-blocking concerns surfaced (not fixed)

- **`display_name` returned from `pyramid_dadbear_overview` is just the slug.** The database schema has no per-pyramid display_name column; a future phase can swap in a real display name when `pyramid_slugs` gains one. Not a correctness issue — the frontend still renders the slug in a monospace header which is what Adam already sees elsewhere.
- **`next_scan_at` is computed as `last_scan_at + scan_interval_secs` without accounting for the debounce period or the in-flight flag.** For most users this is accurate enough; the UI shows "due now" when the computed time is in the past.
- **`pyramid_dadbear_activity_log` merges the three sources in memory and then sorts + truncates.** At 500 rows the LIMIT cap this is a non-issue. A future optimization would push the union into SQL so the LIMIT applies before sort.
- **Polling intervals are hardcoded (10s for overview, 30s for provider health, 60s for orphans).** These should probably move to config contributions so operators can dial them, but that's out of scope for Phase 15 — the plan was explicit that Oversight is frontend assembly, not new configuration surface.

### Commits

1. `phase-15: dadbear oversight page`

### Verification after phase

- `cargo check --lib`: 3 pre-existing warnings only. No new warnings.
- `cargo check --bin wire-node-desktop`: 1 pre-existing binary warning.
- `cargo test --lib pyramid`: **1179 passing / 7 failing** (baseline 1170 + 9 new Phase 15 tests). Same 7 pre-existing failures.
- `cargo test --lib pyramid::db::phase15_tests`: **9 passing / 0 failing**.
- `npm run build`: clean, 150 modules transformed, no new TypeScript errors.

---

## Phase 15 — DADBEAR Oversight Page (wanderer pass, 2026-04-10)

Unguided end-to-end wanderer on the Phase 15 verifier commit. Verifier had flagged one known-issue and declared the rest clean. Wanderer caught three additional real bugs because the verifier's punch list didn't question the seed helpers.

### Friction 1: Tests seeded with a fictitious `'confirmed'` reconciliation status

**Friction:** the implementer's Phase 15 test helpers passed `Some("confirmed")` as `pyramid_cost_log.reconciliation_status`. No production writer ever stores `'confirmed'` — the writers use `'synchronous'`, `'synchronous_local'`, `'broadcast'`, `'broadcast_missing'`, `'discrepancy'`, and `'estimated'`. The tests passed only because the query's final `else → healthy` fallthrough accidentally caught the unknown value.

**Root cause:** no one checked the set of actual reconciliation_status values at write time against the test seeds. The tests were internally consistent (seed+query agreed on the fictional value) but untethered from production.

**Lesson:** when seeding enum-like columns in tests, the seed value list should be a literal reference to the writer's `match` arms, not an independent dictionary. For Phase 15 specifically, the writer's state graph is documented in `db::record_broadcast_confirmation` and `db::insert_cost_log_synchronous` — the seeds should have copy-pasted from there. Add to the implementer handoff prompt: "If your test seeds touch an enum-valued column, grep the writer paths for the literal string before coding the seed."

### Friction 2: Overview's "pending" bucket counts healthy broadcast-confirmed rows as pending

**Friction:** a pyramid with fully-reconciled synchronous rows (broadcast arrived, matched, healthy) renders `'pending'` on its Oversight card. The `pyramid_cost_log.reconciliation_status` column stays at `'synchronous'` even AFTER the broadcast confirms — the production contract in `record_broadcast_confirmation` only flips the status field on divergence; the success path stamps `broadcast_confirmed_at` and leaves the status alone. The overview query was using the status field as the sole signal and missed the `broadcast_confirmed_at` axis.

**Root cause:** two axes of truth (status + broadcast_confirmed_at) were collapsed into one in the query design. The rest of the codebase (e.g. `sweep_broadcast_missing` at `db.rs:13986`) already uses `broadcast_confirmed_at IS NULL` as the "still waiting" signal — the overview query was inconsistent with that convention.

**Lesson:** the overview is the UI surface for reconciliation state, so its query shape should match the axes the writers use. Add a test that explicitly covers: synchronous + not-confirmed → pending; synchronous + confirmed → healthy; synchronous_local → healthy; broadcast_missing → broadcast_missing; discrepancy → discrepancy. The wanderer added the first two.

### Friction 3: "Set Default Norms" silently corrupts pyramid_config_contributions

**Friction:** the verifier flagged this as "known issue — contribution lands as a draft and the accept errors out". Wanderer traced more carefully: the direct-YAML accept path at `generative_config.rs:785-854` commits the new row as `status = 'active'` (and supersedes the prior active) BEFORE calling `sync_config_to_operational`. When sync fails with `"dadbear_policy requires a slug (per-pyramid scope)"`, the transaction has already committed — the contribution table now has an orphaned active slug=NULL row with no operational mirror. Every retry layers another orphan row on top.

**Root cause 1:** the accept flow commits the contribution transaction before sync runs. This is intentional (the contribution is the source of truth regardless of whether sync succeeds) but it means any sync failure leaves observable state in the contribution table that no consumer can process.

**Root cause 2:** the `upsert_dadbear_policy` helper was written with a hard requirement on a non-null slug because the operational `pyramid_dadbear_config` table has a NOT NULL slug column. But the helper lives downstream of the contribution layer, which should be schema-agnostic about per-slug vs global.

**Resolution:** made `upsert_dadbear_policy(None)` a no-op instead of an error. The contribution still persists in `pyramid_config_contributions` (version history, Wire sharing). A future phase can implement a layered resolver that merges the active global `dadbear_policy` contribution with per-slug rows at `get_enabled_dadbear_configs` read time.

**Lesson:** sync dispatchers must never fail on structural expectations about the contribution shape that the contribution layer has no constraints on. If the operational table can't mirror a shape, that's a sync no-op, not a hard error. Add to the handoff prompt: "Sync dispatcher branches that reject valid contributions are bugs — if an operational table can't represent a shape, the dispatcher should log + no-op, not error."

### Friction 4: Provider health `'down'` state renders as grey unknown chip

**Friction:** the frontend `ProviderHealthBanner` maps `'healthy' | 'degraded' | 'alerting' | 'unhealthy'` to chip classes, but the backend emits `'down'` when connection/DNS/TLS failures hit. Down providers render with a grey `'provider-health-chip-unknown'` class and the raw `'down'` text as the label.

**Root cause:** the frontend author worked from the spec's provider health section which mentions `healthy | degraded | down`, but then wrote the map based on a different naming convention (alerting/unhealthy) and forgot to cross-reference against the backend's `ProviderHealth::Down` enum variant.

**Lesson:** enum union types in TypeScript should be copy-pasted from a single source of truth (e.g. a generated `.d.ts` or a hand-maintained comment pointing at the Rust file). The hook at `useProviderHealth.ts:14` already had `'alerting'` as a legacy fallback — that was a smell the wanderer should have caught on sight. Cheap fix: added `'down'` to the union + the two switch statements.

### Meta: wanderer caught what the verifier missed because the seed helpers lied

The verifier confidently declared Phase 15 clean on the reconciliation priority axis. Wanderer caught the bug because the wanderer questioned "what does `'confirmed'` mean here — is that a real state or a made-up one?" and grepped the writers. The verifier's punch list methodology was to check each spec bullet against the code; the wanderer's end-to-end trace methodology exposed a class of bug that existed BELOW the spec's specificity level (the status semantics).

Add to the "wanderers on built systems" memory: **When tests use enum-like values, the wanderer should always grep the writer paths for the literal values before trusting the test's assertions. A green test on a fictitious seed is worth less than no test at all.**

### Commits

1. `phase-15: wanderer fix — reconciliation pending bucket + default norms no-op + provider down state`

### Verification after wanderer pass

- `cargo check --lib`: 3 pre-existing warnings only.
- `cargo test --lib pyramid`: **1183 passing / 7 failing** (+4 new wanderer tests vs verifier baseline of 1179). Same 7 pre-existing failures.
- `cargo test --lib pyramid::db::phase15_tests`: **13 passing / 0 failing** (9 implementer + 4 wanderer).
- `npm run build`: clean, 150 modules, 779.37 kB bundle.

---

## 2026-04-10 — Phase 16 wanderer pass (vine-of-vines + topical vine recipe)

### Context

Phase 16 had already been through an implementer pass (commit `76740ca`) and a verifier fix pass (commit `203ff93`). The verifier flagged that `notify_vine_of_child_completion` had no production caller but deferred the wire-up, so the wanderer started there and then traced the full vine build flow end-to-end.

### Method

Traced each of the 12 flows the wanderer brief asked about, reading source files rather than trusting test output. Four flows surfaced bugs; the other eight were clean.

### Bugs found (4 criticals)

#### 1. `execute_chain_from` hard-rejected every vine build at line 3849 (chunk check)

**Symptom:** calling `build_topical_vine` (or any path that dispatches the topical-vine chain) on a vine slug would return an immediate error: `"No chunks found for slug 'X' — cannot run non-question pipeline with zero chunks"`.

**Root cause:** the chain executor counts chunks in `pyramid_chunks` at the top of `execute_chain_from` and errors out when 0. The implementer/verifier only exempted `content_type == "question"` from this rule. Vines never have chunks — they compose children via `pyramid_vine_compositions` + `cross_build_input` — so every vine build hit the rejection branch.

**Failure class:** same as the "legacy path still rejects ContentType::Vine" bug the verifier caught one layer up. The rejection was moved from the dispatch to the chain executor itself, and the chain executor's chunk check inherited the old assumption. Class: **pre-existing runtime guards that hard-code content-type assumptions keep resurfacing as new content types are added to the executor.**

**Fix:** extracted the logic into `content_type_allows_zero_chunks(&str) -> bool` helper at the top of `chain_executor.rs`. It returns true for `"question"` and `"vine"`. `execute_chain_from` now calls this instead of the inline check, so the next time a new zero-chunk content type lands the exemption is a one-line change in an obvious place.

**Regression test:** `tests::test_content_type_allows_zero_chunks_gate` asserts both exemptions AND that conversation/code/document/empty-string/unknown strings are rejected. Keeps the list explicit.

#### 2. `topical-vine.yaml::upper_synthesis.depth` was 2 (should be 1)

**Symptom:** even with Bug #1 fixed, a vine build would complete with an apex pointing at an L1 cluster node (via the defensive fallback at `execute_chain_from:4504`) rather than an apex that is a true upper-layer synthesis of those clusters. The recursive_pair loop exited immediately with no work done.

**Root cause:** in the chain YAML, `recursive_pair: true` steps use `depth:` as the **source** depth — the layer the loop reads nodes from and pairs upward. With `depth: 2`, the loop ran `get_nodes_at_depth(slug, 2)`, found 0 nodes (because `cluster_synthesis` writes L1 nodes at `depth: 1`), and returned an empty apex id immediately. The fallback at line 4504 then picked the highest-depth live node — one of the L1 cluster nodes — as the "apex".

Pattern confirmed by comparing with `conversation-episodic.yaml`'s `recursive_synthesis` step which uses `depth: 1` after its L1-producing step. The convention: recursive_pair reads from its declared depth and writes at depth+1.

**Failure class:** **semantic overload of the `depth` field across primitives.** for_each extraction uses `depth:` as the OUTPUT depth (where nodes are written). recursive_pair uses `depth:` as the SOURCE depth (where it starts reading). Both use the same YAML field. The implementer was clearly reasoning "cluster_synthesis writes L1, so upper_synthesis writes L2 and up" and set depth: 2 — treating it as output depth, not source depth. The YAML schema permits both interpretations and only the runtime semantics disambiguate.

**Fix:** changed to `depth: 1` with an expanded comment in the YAML explaining the starting_depth semantics. Plus a regression test.

**Regression test:** `phase16_tests::test_topical_vine_upper_synthesis_starts_from_depth_1` asserts the field is 1.

**Friction takeaway for the plan:** the chain YAML guide (docs/chain-system-reference.md) should call out this dual semantics explicitly or rename the field for recursive steps (`source_depth` vs `depth`). Adding to planner friction points.

#### 3. `cluster_synthesis` input block was missing `cluster: "$item"`

**Symptom:** even with Bugs #1 and #2 fixed, the per-cluster synthesis prompt would receive a payload of the shape `{children: [all_children_array]}` with no hint of which cluster was being synthesized. The LLM would either synthesize over all children (wrong output) or return garbage.

**Root cause:** `ctx.resolve_value(input)` only substitutes the `$refs` that appear in the input block. `for_each: "$cluster_children.clusters"` sets `ctx.current_item = cluster`, but without an explicit `cluster: "$item"` in the input block the current cluster is never surfaced to the prompt. The prompt template (`topical_synthesis.md`) explicitly documents "The input is a single cluster object, containing: name, reason, children" — so the prompt and the chain YAML disagreed on what was actually being passed.

Compare with working patterns in `conversation-episodic.yaml`'s `l1_classify` / other for_each steps: they either have no `input:` block (which triggers `enrich_group_item_input(item, ctx)` fallback that injects the whole item) or explicitly reference `$item.field_name`. Our `cluster_synthesis` had an input block that silently dropped the item.

**Failure class:** **implicit vs. explicit item injection asymmetry.** When a for_each step has no `input:` block, the item is auto-injected. When it has an `input:` block, the item must be explicitly named. The implementer wrote an input block (to pass `children`) and didn't realize that action alone removed the auto-injection fallback for the item itself.

**Fix:**
- chain YAML `cluster_synthesis.input` now carries both `cluster: "$item"` and `children: "$collect_children.children"`.
- `topical_synthesis.md` prompt updated to document the new input shape (`cluster` + `children`, plus explicit instruction to filter the `children` array by `cluster.child_slugs`).
- updated the "reuse the cluster reason" bullet to reference `cluster.reason` instead of the old top-level `reason`.

**Regression test:** `phase16_tests::test_topical_vine_cluster_synthesis_passes_cluster_via_item_ref` asserts the input map has `cluster: "$item"` AND `children: "$collect_children.children"`.

**Friction takeaway for the planner:** **the chain executor's input resolution should auto-inject `$item` by default unless the step explicitly opts out.** Add to friction points — the current behavior is a load-bearing footgun.

#### 4. `notify_vine_of_child_completion` had no production caller (verifier deferred)

**Symptom:** after a bedrock rebuild completed, no parent vines received a DeltaLanded event and no pending mutations were enqueued. Vine-of-vine updates were silently dropped unless an operator manually hit `POST /pyramid/:slug/vine/trigger-delta`.

**Root cause:** the implementer wrote `notify_vine_of_child_completion` with the correct BFS walk logic, cycle guard, and per-level DB writes. But the production build-completion paths in `build_runner::run_build_from_with_evidence_mode` only called `db::get_slug_referrers` (the older cross-slug referrer notification) and skipped the vine composition hook entirely. The verifier caught this but marked it as "deferred / not fixed (out of verifier scope)".

**Failure class:** **implementer built the mechanism, nobody wired the production caller.** Same failure class as Phase 4/6/12/13/14/15 — the mechanism works in unit tests that call it directly, but no production path actually invokes it. This is the class the "wanderer after verifier" memory was created to catch.

**Fix:**
1. Extracted the post-build notification block from `run_build_from_with_evidence_mode` into a new `run_post_build_hooks(state, slug_name, &result)` helper. The helper runs three things on successful builds: WS8-F cross-slug referrer notification, Phase 16 vine-of-vines propagation (`notify_vine_of_child_completion`), and WS-ONLINE-F remote web edge resolution.
2. The `run_build_from_with_evidence_mode` early-return for Question and Conversation slugs now captures the result, calls `run_post_build_hooks`, then returns. Previously these two paths skipped the referrer notification AND the vine propagation entirely, so a conversation or question pyramid being a child of a vine never triggered any upward update.
3. Added `use super::vine_composition;` to the build_runner imports.
4. Gated the vine propagation on `res.1 == 0 && !res.0.is_empty()` (zero failures AND non-empty apex), since the propagation walk uses the apex id as the "what changed" signal.

**Regression tests (2 async integration tests):**

- `test_phase16_notify_vine_of_child_completion_walks_two_levels`: creates a real `PyramidState` with an on-disk sqlite, wires a 2-level vine-of-vines (bedrock → v-mid → v-top), calls `notify_vine_of_child_completion` on the bedrock's apex, and asserts that (a) the direct-parent return list contains only v-mid, (b) v-mid's composition row was updated to point at the bedrock's apex, (c) v-top's composition row was updated to point at v-mid's own apex (this is the second recursive hop that was silently broken), and (d) the build event bus emitted DeltaLanded events for both v-mid AND v-top.
- `test_phase16_notify_vine_of_child_completion_handles_three_level_cycle`: creates a 3-node cycle v-a → v-b → v-c → v-a and asserts the walk terminates, returns v-b as the direct parent of v-a only, and leaves all composition rows intact. Exercises the async cycle guard end-to-end (vs. the existing tests that only exercise the DB-level cycle guard in `get_parent_vines_recursive`).

### Files changed by wanderer

- `src-tauri/src/pyramid/chain_executor.rs`
  - new `content_type_allows_zero_chunks` helper at line ~2988
  - `execute_chain_from` calls the helper instead of hard-coded content_type check
  - new `tests::test_content_type_allows_zero_chunks_gate` unit test
- `chains/defaults/topical-vine.yaml`
  - `upper_synthesis.depth: 2` → `depth: 1` with expanded comment
  - `cluster_synthesis.input` gains `cluster: "$item"` field with expanded comment
- `chains/prompts/vine/topical_synthesis.md`
  - INPUT SHAPE section rewritten to describe the two-field input (`cluster` + `children`) and the "filter `children` by `cluster.child_slugs`" instruction
  - "Reuse the cluster reason" bullet references `cluster.reason` instead of the old top-level `reason`
- `src-tauri/src/pyramid/build_runner.rs`
  - added `use super::vine_composition;`
  - extracted post-build hooks into `run_post_build_hooks(state, slug_name, &result)` helper
  - `run_build_from_with_evidence_mode` calls the helper at the end of the non-early-return path (replaces the inline referrer + web edge blocks)
  - Question slug early-return now captures the result, calls `run_post_build_hooks`, then returns (previously it skipped both hooks entirely)
  - Conversation slug early-return now captures the result, calls `run_post_build_hooks`, then returns (previously it skipped both hooks entirely)
  - Phase 16 hook: `run_post_build_hooks` calls `vine_composition::notify_vine_of_child_completion` on successful builds
- `src-tauri/src/pyramid/chain_loader.rs`
  - two new regression tests: `test_topical_vine_upper_synthesis_starts_from_depth_1` and `test_topical_vine_cluster_synthesis_passes_cluster_via_item_ref`
- `src-tauri/src/pyramid/vine_composition.rs`
  - two new async integration tests with a real PyramidState: `test_phase16_notify_vine_of_child_completion_walks_two_levels` and `test_phase16_notify_vine_of_child_completion_handles_three_level_cycle`
  - new `make_propagation_test_state()` helper (local to the tests module) to build a minimal PyramidState with an on-disk sqlite DB and event bus
  - new `install_vine_with_apex()` async helper to install a vine slug + apex node so the recursive walk can lift the parent apex on each hop

### Meta lessons

1. **When a verifier defers a "latent bug" because it's "out of scope," the wanderer always fixes it.** The Phase 16 verifier's own fix-pass log explicitly listed "Production wiring of `notify_vine_of_child_completion` to a build-completion hook" as deferred. That's the exact class of bug the "wanderer after verifier" memory was built to catch. The rule stays: anything in the verifier's "deferred" list that's a latent bug gets fixed in the wanderer pass.

2. **Chain YAML semantics are too subtle for punch-list verification.** The verifier's punch list was things like "does the YAML exist and validate?", which it does. But "does `depth: 2` on a recursive_pair step start from the right source layer?" requires tracing the chain executor's `execute_recursive_pair` to see that depth is the SOURCE depth. That's exactly the kind of semantic gap a wanderer pass catches by tracing end-to-end. Add a friction point to the planner: **recipe correctness lives below the validator.**

3. **"Prompt receives the item" is an implicit contract the for_each primitive doesn't enforce.** When a step has no input block, `enrich_group_item_input` injects the item automatically. When the step has an input block, the item is invisible unless the step explicitly references `$item`. The YAML doesn't flag this — you only notice when the LLM output is obviously wrong. Add to friction points: **the chain executor should either auto-inject $item in all paths or warn loudly when an input block is present without an $item ref on a for_each step.**

4. **The "wanderer catches what the verifier misses" dynamic is strongest at chain-execution-time.** Every wanderer find this session is a failure mode that would only surface the first time a human actually built a vine — the verifier's unit tests validate the YAML schema and the DB walk, but they never run `execute_chain_from` on a vine slug. Add to memory: **phase verification should include at least one end-to-end simulation of the feature's primary user flow, even if mocked, to catch exactly this class.**

### Commit

1. `phase-16: wanderer fix — chain executor vine gate + recursive_pair depth + cluster item ref + production propagation wiring`

### Verification after wanderer pass

- `cargo check --lib`: 3 pre-existing warnings only.
- `cargo test --lib phase16`: **21 passing / 0 failing** (17 pre-existing + 4 new wanderer regression tests: chunk-check gate in chain_executor, upper_synthesis depth assertion in chain_loader, cluster_synthesis item ref assertion in chain_loader, 2 async integration tests in vine_composition — one asserts the 2-level walk end-to-end, one asserts the cycle guard).
- `cargo test --lib pyramid`: **1205 passing / 7 failing** (+5 new wanderer tests vs verifier baseline of 1200). Same 7 pre-existing failures.
- `npm run build`: clean, 150 modules, 779.37 kB bundle.

---

## 2026-04-10 — Phase 17 implementer notes

### Deviations from the spec / workstream prompt

1. **Strict-majority content type detection** (spec says "majority extension
   wins", implementation uses "must be >= half AND strictly greater than
   runner-up"). The spec's plain majority rule would tie-break arbitrarily
   on a 2-code/2-doc folder. Stricter rule matches the spec's "mixed →
   vine" behavior more faithfully. Documented in `detect_content_type`
   doc comment.

2. **Top-level folder forced to vine when Claude Code matches exist.** The
   spec's pseudocode attaches CC pyramids "as bedrocks of the target
   folder's vine", but doesn't specify what to do when the target would
   otherwise become a single leaf pyramid (e.g. a folder with just .md
   files). Phase 17 forces a topical vine in that case so the CC children
   have a parent composition row — without this, the CC pyramids would be
   orphaned. Documented in `plan_recursive` comment block.

3. **`require_git(false)` on `ignore::WalkBuilder`.** Not in the spec, but
   required so `.gitignore` works on non-git folders the user points at.
   The `ignore` crate's default refuses to honor `.gitignore` unless the
   directory is inside a `.git/` directory, which is the wrong default
   for folder ingestion. Added as a comment on the builder configuration.

4. **Extension lists stored as `*_json` columns, not first-class columns.**
   `code_extensions_json` and `document_extensions_json` mirror the
   existing `content_type_rules_json` / `ignore_patterns_json` pattern
   on the same table. This keeps migrations small and avoids the
   schema-rigidity problem (spec may evolve to allow per-extension
   metadata later).

5. **No explicit first-build trigger after plan execution.** The spec
   doesn't require it, and Pipeline B's scanner will pick up the new
   DADBEAR configs on its next tick. Adding an explicit build-now hook
   would require holding the writer lock across long-running chain
   dispatch which is exactly what Phase 16's `run_post_build_hooks`
   refactor was trying to avoid. Manual verification step 9 confirms
   builds start automatically.

### Surprises

1. **`pyramid_config_contributions` column is `wire_native_metadata_json`,
   not `canonical_metadata`.** First pass at the db phase17 test helpers
   wrote `canonical_metadata` based on Phase 5 naming in the log. Only
   discovered at test runtime. The column was renamed in Phase 5 but the
   naming in commit messages is inconsistent. **Add to friction: when
   writing backend tests, grep for the actual `CREATE TABLE` before
   writing INSERT statements — memory is unreliable across renames.**

2. **Migration ordering matters.** First pass put the Phase 17 idempotent
   ALTER TABLE block at the top of `init_pyramid_db` near the Phase 16
   `child_type` migration. This runs BEFORE the `execute_batch` that
   creates `pyramid_folder_ingestion_heuristics`, so the ALTER failed on
   "no such table". Moved the block to after the execute_batch that
   creates the table. **Add to friction: in-place ALTER blocks must come
   after the CREATE TABLE in the same initialization path; the "migration
   marker" pattern used elsewhere in the codebase doesn't enforce this.**

3. **`tempfile::TempDir` paths on macOS go through `/private/var/...`
   via symlink.** When the test canonicalizes the target folder, the
   path changes from `/var/folders/...` to `/private/var/folders/...`.
   The first draft of `test_find_claude_code_conversation_dirs_matches_encoded_target`
   failed until I realized the test needed to canonicalize its own
   target to compute the expected encoded path — otherwise the encoded
   string in the `.claude/projects/` directory name didn't match the
   one the implementation computed. Fix was trivial once the cause was
   identified: `target.canonicalize().unwrap()` at the top of the test.

### Test count delta

- Phase 16 baseline: 1205 passing / 7 failing.
- Phase 17 total: **1233 passing / 7 failing** (+28 tests).
- New tests: 23 `folder_ingestion::phase17_tests` + 4 `db::phase17_tests`
  + 1 `config_contributions::tests::test_sync_folder_ingestion_heuristics_with_new_fields`.
- Pre-existing failures unchanged.

---

## 2026-04-10 — Phase 17 wanderer pass (capstone wanderer)

### The load-bearing trace that the punch-list audit missed

The verifier had confirmed that `execute_plan` correctly lands `db::create_slug`
+ `db::save_dadbear_config` + `db::insert_vine_composition` into the real
tables. That part is true. The deviation flagged in the implementation log
(#5, "No explicit first-build trigger after plan execution — Pipeline B
handles it") is NOT true, and the downstream of the IPC was never verified
beyond the DB writes. Three concrete breaks:

1. **Pipeline B explicitly rejects code and document content types.**
   `dadbear_extend.rs:742-748` returns
   `Err("Phase 0b: content_type '{}' is not yet supported by Pipeline B
   ingest; per-file code/doc ingest lands in Phase 17 (recursive folder
   ingestion). Record will be marked failed.")`. That rejection was
   delegated to Phase 17 to fix, but Phase 17 never touched
   `fire_ingest_chain`. A folder ingestion that creates a code pyramid
   writes an ingest record → DADBEAR dispatches → fire_ingest_chain
   returns Err → record is marked failed → no build ever runs. The user
   sees a pyramid slug with zero nodes and no explanation.

2. **Topical vines never get a first build.** Vines are not listed in
   `pyramid_dadbear_config` at all (they have no file source), so
   Pipeline B doesn't even scan them. The usual fallback path —
   `notify_vine_of_child_completion` in `vine_composition.rs` — ONLY
   enqueues change-manifest mutations against pre-existing vine apex
   nodes. A freshly-created vine has zero nodes, so
   `enqueue_vine_manifest_mutations` returns 0 affected rows and no
   pending mutations land. The stale engine has nothing to pick up.
   The vine sits at node_count=0 forever.

3. **DADBEAR extend loop may not even be running.** `main.rs:9390-9412`
   only starts the loop at boot when `get_enabled_dadbear_configs`
   already returns non-empty. For a fresh install where the user's first
   action is folder ingestion, there are zero configs at boot → the loop
   never starts → even the conversation pyramids (which Pipeline B CAN
   handle) don't get scanned. `main.rs:3420-3432` starts the loop during
   `post_build_seed` for conversation/vine slugs, but that only runs
   after a build completes — and builds don't start, so the loop never
   starts either. Chicken and egg.

The meta-lesson: the verifier's "end-to-end trace verified" claim stopped
at `insert_vine_composition` — the last DB write in execute_plan. Nobody
traced into Pipeline B's actual dispatch for the newly-created configs.
The friction log entries 218 and 219 from Phase 0b were flagged as
"latent bugs — fix when Phase 17 needs it" and Phase 17 did not revisit
them.

### The wanderer fix

Added `folder_ingestion::spawn_initial_builds(state, plan)` — a new helper
called from `pyramid_ingest_folder` immediately after `execute_plan`
returns on `dry_run: false`. It spawns a single background task that:

1. Starts the DADBEAR extend loop if it isn't already running (via a
   new `ensure_dadbear_loop_running` helper that mirrors the check in
   `main.rs:9402`).
2. Walks every non-vine leaf in plan order. For each leaf:
   - Runs the appropriate `ingest::ingest_code` / `ingest_docs` /
     `ingest_conversation` against the slug's source_path to populate
     `pyramid_chunks`. This is what the legacy AddWorkspace flow does
     between `pyramid_create_slug` and `pyramid_question_build`. Phase
     17 was missing this step.
   - Calls `question_build::spawn_question_build` with a content-type
     appropriate default apex question (matches the
     `AddWorkspace.tsx::DEFAULT_QUESTIONS` lookup).
3. Sleeps 2 seconds so leaf builds have a chance to start writing apex
   nodes, then walks every vine in plan order and calls
   `spawn_question_build` on each. The vine dispatches through the
   topical-vine chain's `cross_build_input` primitive which reads
   whatever apexes the children have produced; if some children are
   still mid-build the vine picks them up via the normal propagation
   cascade on subsequent child completions.

The dispatch runs entirely in a background tokio task so the IPC
round-trip stays fast (the writer lock is released before the task
starts). `spawn_question_build`'s own internal task-spawn pattern is
reused — no duplicated writer/progress/layer channel wiring.

### Subtle fix inside the fix: Claude Code conversation chunk collision

First draft of `prepopulate_chunks_for` called `ingest_conversation` per
`.jsonl` file in the CC directory. This hits the same latent bug called
out in the Phase 0b friction log at entry 219: `ingest_conversation` uses
chunk_index 0..N starting per file, and the `UNIQUE(slug, chunk_index)`
constraint on `pyramid_chunks` means the second file's chunk 0 collides
with the first file's chunk 0. Only the first file's chunks would land.

The correct fix at Phase 17 is the same fix called for by entry 219: a
per-file `chunk_offset` parameter on `ingest_conversation`. That's a
wider change than a wanderer pass should take on. The narrow fix: for a
Claude Code conversation pyramid's bootstrap, ingest only the MOST
RECENTLY MODIFIED jsonl session (the active one the user cares about).
Subsequent sessions show up through DADBEAR / Pipeline A on later ticks.
This matches the single-file contract `ingest_conversation` was
originally designed for, unbreaks the critical path for Phase 17, and
leaves the per-file chunk_offset refactor as a standalone follow-up.
Documented inline at `folder_ingestion.rs::prepopulate_chunks_for`.

### Non-issues confirmed (clean)

- **Q5 Claude Code path expansion:** `expand_claude_code_projects_root`
  correctly handles `~/` via `dirs::home_dir()` and falls back on bare
  `~` and raw paths. The encoded-path prefix match covers subfolders
  and Claude Code worktrees via `starts_with(encoded_target + "-")`.
  CC directories are passed to the DADBEAR config as absolute paths, so
  Pipeline B's scanner reads them directly regardless of where the
  ingested folder lives relative to `~/.claude/projects/`.
- **Q6 .git/ subdirectory filtering:** the bundled
  `default_ignore_patterns()` includes `.git/`, and
  `path_matches_any_ignore` matches any path component equal to `.git`,
  so both direct and nested `.git/` dirs are correctly skipped.
- **Q10 bundled heuristics fallback:** `FolderIngestionConfig::default()`
  field values match the bundled
  `bundled-folder_ingestion_heuristics-default-v1` YAML. When the
  operational row is absent on first boot (the seed isn't auto-synced),
  `load_active_folder_ingestion_heuristics` returns the default, which
  is equivalent to the bundled YAML. Verified by code inspection.
- **Q11 Pipeline B vine recognition:** vines intentionally don't get
  `pyramid_dadbear_config` rows — they compose children via
  `pyramid_vine_compositions`, which is what the topical-vine chain
  reads at build time. The wanderer fix triggers the vine's first build
  directly instead of routing it through DADBEAR.

### Non-issues deliberately NOT addressed in the wanderer pass

- **Slug collision against pre-existing database slugs.** `generate_slug`
  only dedupes against the per-plan HashSet; if a folder-generated slug
  matches a slug already in `pyramid_slugs`, `execute_plan` catches the
  "already exists" error and treats it as success, effectively co-opting
  the existing slug into the folder hierarchy. In practice users won't
  have pre-existing slugs that clash with folder-generated slugs because
  folder-generated slugs are compound (e.g. `agent-wire-node-src-tauri`)
  and the legacy AddWorkspace flow uses simpler slugs. Defer to a
  follow-up pass that queries the DB for existing slugs at plan time.
- **Empty vines at deep recursion.** If `max_recursion_depth` is hit
  mid-walk with a subfolder present but loose files below threshold,
  `plan_recursive` emits a CreateVine with zero children ops. The
  verifier's empty-vine guard only fires when `subfolders.is_empty()`,
  so this edge case still leaks. Minor cosmetic bug that would surface
  as "1 vine, 0 pyramids" in the wizard for an unusual folder shape.
  Defer.
- **Latent `ingest_conversation` chunk_offset bug (entry 219).** The
  wanderer fix works around it by only ingesting the newest jsonl per CC
  dir; the proper fix (extending `ingest_conversation` to accept a
  chunk_offset parameter) is called out in the Phase 0b friction log
  and remains a standalone follow-up.

### Files changed

- `src-tauri/src/pyramid/folder_ingestion.rs` — added `BuildDispatch`,
  `extract_build_dispatches`, `default_apex_question`,
  `prepopulate_chunks_for`, `ensure_dadbear_loop_running`, and the
  public `spawn_initial_builds`. Added `use std::sync::Arc` to the
  imports. Three new tests in `phase17_tests`:
  `test_extract_build_dispatches_partitions_leaves_and_vines`,
  `test_default_apex_question_non_empty_for_every_content_type`,
  `test_extract_build_dispatches_empty_for_plan_without_creates`.
- `src-tauri/src/main.rs` — `pyramid_ingest_folder` now calls
  `folder_ingestion::spawn_initial_builds(&state.pyramid, &plan)`
  after `execute_plan` succeeds on `dry_run: false`.
- `docs/plans/pyramid-folders-model-routing-implementation-log.md` —
  wanderer sub-entry under Phase 17.
- `docs/plans/pyramid-folders-model-routing-friction-log.md` — this
  entry.

### Test count delta (wanderer)

- Phase 17 verifier baseline: 1235 passing / 7 failing.
- After wanderer fix: **1238 passing / 7 failing** (+3 tests).
  Same 7 pre-existing unrelated failures.
- `cargo check --lib` clean (same 3 pre-existing warnings).
- `cargo check --bin wire-node-desktop` clean (same 1 pre-existing
  tauri_plugin_shell deprecation).
- `cargo test --lib 'pyramid::folder_ingestion::phase17_tests'` — **28
  passing** (25 from verifier + 3 new wanderer tests).
- `npm run build` clean, 150 modules, 788.39 kB bundle (unchanged —
  the fix is backend-only).

### Commit

1. `phase-17: wanderer fix — explicit first-build dispatch for every
   created slug`

### Surprises

1. **The verifier's "end-to-end trace verified" skipped the most
   important leg.** The claim was: "DB writes land correctly, IPCs
   wire up, plan → execute_plan → db::create_slug." What it didn't
   check: whether any of those created slugs ever produce a BUILD.
   Tracing into Pipeline B's actual dispatch would have revealed
   the `fire_ingest_chain` rejection for code/document immediately.
   The meta-pattern that played out on every prior phase — wanderers
   find 1-2 real production wiring gaps that the punch-list verifier's
   audit missed — held here at 200% strength: the ENTIRE feature was
   non-functional for non-conversation content types, and it would
   have shipped.

2. **The `fire_ingest_chain` error message points directly at the
   fix.** The Phase 0b error says "per-file code/doc ingest lands in
   Phase 17 (recursive folder ingestion)." This is a TODO embedded in
   the production error path, and Phase 17 implemented the recursive
   walker but not the Pipeline B extension. Shipping TODO messages as
   error text is a good habit — but only if every successor phase
   actually grepcs for them.

3. **The DADBEAR extend loop's boot-time guard is reasonable in
   isolation but silently breaks first-time-user flows.** The guard
   at `main.rs:9402` exists so idle apps don't spin a tick loop with
   nothing to do. But for a new install where folder ingestion is the
   user's first action, the guard defeats the whole first-build path
   for conversations. The wanderer fix starts the loop lazily in
   `spawn_initial_builds`, which is the right place for any "I just
   created configs, kick the loop" call.

---

### 2026-04-11 — Phase 18c: circle scope deferral + cache audit-count deferral

**Phase / workstream:** Phase 18c (privacy opt-in + pause-all
scoping). Claims L4 and L9 from the deferral ledger.

**What hit friction**

The L9 spec defines three scopes for `pyramid_pause_dadbear_all`:
`all`, `folder`, and `circle`. The `circle` scope is supposed to
pause every pyramid in a specific Wire circle via:

```sql
UPDATE pyramid_dadbear_config SET enabled = 0
WHERE enabled = 1 AND slug IN (
  SELECT slug FROM pyramid_metadata WHERE circle_id = ?1
);
```

But `pyramid_metadata` is not a real table in the local DB. Searching
the schema turned up no `circle_id` column on any pyramid table. The
only place `circle_id` lives is in the JWT claim layer
(`server.rs::JwtClaims`, `auth.rs::PublicAuthSource`, etc.) — Wire's
auth side knows what circle a request is from, but Wire Node has no
local per-pyramid record of "this pyramid was published into circle
X."

The L4 spec calls for an inline preview of "N L0 nodes / M reference
private corpus docs" alongside the cache opt-in checkbox. Computing
this would need a join against `wire_source_documents.visibility`,
which is not consistently populated for every L0 node's source
document on the local DB. The visibility field is part of the import
flow but not the publish flow, so the "audit count" preview would be
non-trivial backend work that didn't fit the L4 time budget.

**Root cause**

Two cases of the same shape: a spec field that depends on schema
that hasn't been built yet OR data that hasn't been routed to the
right place for the consumer. This isn't a Phase 18c bug — both
features are real follow-ups for whoever lands the
`pyramid_metadata` table or extends the publish path with source
visibility tracking.

**What we did about it**

Per the deviation protocol in the workstream prompt:

- **Circle scope:** ship `all` + `folder` only. The IPC returns an
  explicit error for `scope = "circle"` pointing at this friction
  log entry. The frontend renders the circle radio as disabled with
  a tooltip explaining the deferral. The `count_dadbear_scope`
  helper returns `Ok(0)` for circle so the UI can render the
  disabled state without crashing on a backend error.
- **Audit count:** ship the checkbox with the warning text only.
  The user must review their source visibility manually before
  checking the box. The warning text covers the privacy story; the
  audit count is documented as a follow-up here so it can be picked
  up alongside whatever lands `wire_source_documents.visibility` on
  the publish path.

**Lesson for future phases**

Two takeaways:

1. **Spec SQL that references tables-that-don't-exist is a flag.**
   When the spec writes a SQL statement that uses a column the
   schema doesn't have, that's a deferred dependency, not a missing
   detail. The Phase 18c workstream prompt called this out
   explicitly ("If `pyramid_metadata.circle_id` doesn't exist
   ... defer circle scope") which was the right move — naming the
   deferral upfront kept it from sliding into a half-built feature.

2. **The "deferred radio" pattern is genuinely useful.** Rendering
   the circle radio as disabled with "coming soon" instead of
   hiding it tells the user (a) the feature exists conceptually, and
   (b) it's a planned addition, not a bug. Compare with hiding it
   entirely, which would silently surprise the user when they find
   the spec mentioning circle scoping.

### Phase 18c — additional notes

**Folder canonicalization decision:** the Phase 18c db.rs helpers
use a simple lexical match: strip a single trailing slash from the
input, then SQL `(source_path = ?1 OR source_path LIKE ?1 || '/%')`.
No filesystem resolution. No symlink handling. The DB stores
whatever `source_path` the user originally configured, and we match
against that text. If a user has DADBEAR configs at both
`/home/user/project` and `/Users/user/project` because of
case-insensitive HFS+ vs case-sensitive APFS quirks, they're treated
as different folders. That's the correct behavior for the
"intentional groupings" use case the spec describes — users who
want to pause "all my work pyramids" should pick one canonical
prefix. Symlink resolution would be the wrong layer for this.

**Concurrent workstream contamination:** while implementing 18c,
the working tree picked up file modifications from concurrent
18a/18b/18d/18e workstreams that share the same git checkout
location. db.rs, event_bus.rs, mod.rs, llm.rs, folder_ingestion.rs,
yaml_renderer.rs, and bundled_contributions.json all picked up
spurious modifications at various points during the session,
forcing extra `git checkout HEAD --` cleanup. The 18c branch only
ships its own changes (main.rs, db.rs, wire_publish.rs,
PublishPreviewModal.tsx, CrossPyramidTimeline.tsx,
DadbearOversightPage.tsx, DadbearPauseScopeModal.tsx,
configContributions.ts) — but the cross-workstream interference was
the single biggest friction point in the session. **Lesson:**
parallel workstreams on the same checkout directory need either
git worktrees or sequential execution; git stash routinely picked
up changes from sister branches when the agent didn't intend it.

---

### 2026-04-11 — Phase 18c wanderer: UX papercuts surfaced by end-to-end trace

**Phase / workstream:** Phase 18c wanderer pass (no fixes, log
commit for traceability).

**What hit friction**

Six UX polish items surfaced while tracing the 12 questions in the
wanderer prompt end-to-end. None block the phase — every primary
flow works correctly — but they're the kind of edge-case behavior
a punch-list audit won't catch without actually walking the flow.

- **W1 — L4 silent fallback for slug=null contributions.**
  Publishing a global config contribution (no pyramid slug) with
  the cache manifest opt-in checked falls through a
  `tracing::warn!` path that leaves the user's opt-in silently
  dropped. The success state looks identical to not opting in.
- **W2 — DryRunReport lacks `slug`.** The frontend can't gate the
  cache-manifest opt-in on slug presence without an extra IPC.
  W1 and W2 would be fixed by the same change.
- **W3 — CrossPyramidTimeline banner Resume forces `scope="all"`.**
  Clicking Resume on the banner after a folder-scoped pause can
  re-enable other pyramids that were paused in a prior session.
  The implementer's in-code comment explicitly acknowledges this
  and recommends the DADBEAR Oversight page for scoped resume.
- **W4 — CrossPyramidTimeline doesn't refetch after pause/resume.**
  Only sets the banner + toast; the active-builds list stays
  stale until the hook polls. DadbearOversightPage correctly
  calls `refetchOverview()`.
- **W5 — Whitespace-only folder input reaches the count IPC.**
  The frontend short-circuits on empty string but not on
  whitespace. Not broken (the confirm button is disabled via
  `trim().length === 0`), just a wasted round trip.
- **W6 — Shared `datalist id="dadbear-source-paths"`.** Latent ID
  collision if the modal is ever reused in parallel. Fine today.

**Root cause**

Two patterns:

1. **W1, W2, W3** — the backend ships a correct default-safe path,
   but the frontend success state doesn't distinguish
   "completed as requested" from "silently adjusted". The user
   opts in or clicks Resume and gets a success toast that hides
   the behavior change.
2. **W4, W5, W6** — parity and hygiene gaps between the two
   consumer surfaces (CrossPyramidTimeline and
   DadbearOversightPage) because the shared component was
   introduced mid-phase and the host pages were refactored
   around it without a final diff review.

**What we did about it**

Nothing. The findings are logged here for a follow-up phase; none
break the end-to-end flow.

**Lesson for future phases**

- **Silent backend fallbacks need frontend-visible signals.** If
  the backend decides "I can't do what you asked but I won't
  error either," the user needs to see that. The pattern is
  `response_field: Option<T>` plus an optional
  `reason_skipped: Option<String>` that the UI surfaces when
  populated. Adding a warning field is cheap; carrying opaque
  silence forward isn't.
- **Reusable modals need parity tests on both hosts.** When
  18c introduced `DadbearPauseScopeModal` used by two different
  pages, the per-page follow-up behavior (refetching, banner
  state, scope memory) diverged. A mini parity checklist for
  "what does each host do after the IPC returns" would have
  caught W3 and W4 during the verifier pass.
- **DryRunReport is the right place for publish-time
  state the modal needs.** Adding fields like `slug` to the
  dry-run report is cheap and means the modal doesn't need
  extra IPC calls to make UX decisions about the publish.

---
## Phase 18b wanderer pass (2026-04-11)

1. **When the verifier can't run tests, the wanderer must be the
   guaranteed backstop.** Phase 18b's verifier explicitly punted
   runtime verification (`cargo test --lib pyramid` + `npm run build`)
   because 4 sibling Phase 18 worktrees had filled the shared Data
   volume. The verifier's static audit was thorough, but "pre-release
   test count matches a static code review" is a weaker guarantee than
   "I ran the tests and they pass." The wanderer started with runtime
   verification as job #1 before the 12 failure-mode trace. All three
   runtime checks passed cleanly on this worktree (111 GB free).

2. **Pre-existing test failures need a clear provenance trail.** The 7
   failures in `cargo test --lib pyramid` are all from schema drift in
   `pyramid::staleness::tests::*` where a local `setup_test_db` helper
   creates `pyramid_evidence` without the `build_id` column that
   production code expects. These have existed since Phase 17 (last
   touch on staleness.rs was `4177152`). The wanderer documented this
   chain of provenance in the log so a future reader doesn't think the
   count drift is a new regression.

3. **"Verifier clean" != "Wanderer clean" — but for 18b, both passed.**
   The meta-lesson from 17/18 prior phases is that wanderers catch
   bugs verifiers miss. Phase 18b is the exception: the implementer's
   work was tight enough that 12 deep failure-mode traces, an
   end-to-end runtime verification, and a lock-contention/deadlock
   analysis all cleared with zero code fixes required. The architecture
   is sound (Option A fully consummated, single-source-of-truth unified
   function), the retrofit is exhaustive (6/6 production sites flipped),
   the deprecation is explicit, the schema migration is idempotent, and
   the test bodies exercise production paths not shapes. One phase in
   eighteen is a believable rate for "wanderer finds nothing real."

4. **Cost-log double-counting on cache hits is a pre-existing design
   observation, NOT a Phase 18b bug.** Worth flagging for a future
   phase: `chain_executor.rs::execute_ir_single` unconditionally calls
   `log_cost_synchronous` with the cached `LlmResponse.actual_cost_usd`
   on every dispatch, including cache hits. Phase 12's non-audited
   retrofit had the same behavior. The user sees "second build cost
   $X" in the cost ledger even on a full cache-hit re-run. The fix is
   small but out of 18b scope — either filter by cache_hit at the
   ledger boundary, or zero out the cost when serving from cache. This
   is a dashboard-correctness issue, not a billing issue (OpenRouter
   only actually charges for wire calls).

5. **Cache-hit audit rows now show up in `cost_model.rs::recompute_
   from_audit` averages but the math works out.** Cache hit rows
   preserve the original call's `prompt_tokens` / `completion_tokens`,
   and `calls_per_conversation = total_calls / distinct_builds` stays
   stable across re-runs (2N/2 = N). Per-call cost estimate is
   unchanged. The cost model reports approximately the first build's
   actual cost, which IS what the user paid. Not a regression.

6. **No frontend UI surfaces `cache_hit` from the audit table yet.**
   Phase 13's build viz shows cache hits via the event bus path
   (`TaggedKind::CacheHit` → `useBuildRowState.ts:323`), not by
   querying `pyramid_llm_audit.cache_hit`. Phase 18b's contribution
   is the durable record — DADBEAR Oversight surfacing cache savings
   from the audit column is a separate workstream. This matches the
   workstream prompt's framing and isn't a gap.

---
## Phase 18a — Local Mode + Provider Management (2026-04-11)

### `TierRoutingYaml` field name silently broke since Phase 4

The Phase 4 dispatcher's `tier_routing` branch did:

```rust
let yaml: db::TierRoutingYaml = serde_yaml::from_str(&contribution.yaml_content)?;
db::upsert_tier_routing_from_contribution(conn, &yaml, ...)?;
```

…where `TierRoutingYaml` was declared as `pub tiers: Vec<...>`. The
bundled tier_routing JSON Schema (in
`assets/bundled_contributions.json`) and the bundled default
contribution both used `entries:`. There was no
`#[serde(deny_unknown_fields)]`, so deserialization succeeded silently
and produced an EMPTY `Vec`. The upsert helper iterated over zero
rows and the operational `pyramid_tier_routing` table never picked
up contribution-driven supersessions. The fact that nothing surfaced
this for 14 phases suggests every test that exercised tier_routing
either:
- Bypassed the contribution layer and called `save_tier_routing`
  directly (like `seed_default_provider_registry`).
- Used a hand-rolled `tiers: []` YAML fixture that matched the
  broken struct (like `test_global_config_with_null_slug`).

No test exercised the dispatcher's tier_routing branch end-to-end
against the canonical schema. The bug would have stayed silent
indefinitely.

**Lesson:** every dispatcher branch needs at least one test that
parses the canonical bundled-seed YAML. Add a guard test in a future
phase that walks every `bundled_contributions.json` entry and runs
`sync_config_to_operational` against a fresh DB. If any branch
silently produces empty/default state from the canonical seed, fail
loudly.

**Phase 18a fix:** struct renamed to `entries:` with
`#[serde(default, alias = "tiers")]`, plus three previously-unknown
fields added (`priority`, `prompt_price_per_token`,
`completion_price_per_token`) so the canonical schema parses. Plus
the upsert helper now DELETEs tier rows not present in the new
contribution — required for Local Mode reversibility AND consistent
with the rest of the dispatcher's "contribution is the source of
truth" model.

### Worktree contamination

The five Phase 18 sub-workstreams (18a/18b/18c/18d/18e) were
launched in parallel branches but only 18b/18d/18e got dedicated
worktrees in `/private/tmp/`. The 18a and 18c agents both ended up
operating in the main repo at different points. I (the 18a agent)
spent the first ~30 minutes editing files in
`/Users/adamlevine/AI Project Files/agent-wire-node` while the
checked-out branch was actually `phase-18c-privacy-pause-all` —
because no one had created a worktree for 18a yet. My edits to
db.rs, mod.rs, and main.rs landed in the 18c worktree and were
rolled back when 18c committed its own changes (the "modified
since read" errors I kept hitting were a symptom of the harness
detecting concurrent file mutation).

**Recovery:**
1. Created a worktree at `/private/tmp/agent-wire-phase-18a` and
   tried to redo the work there. Hit a separate disk-space wall
   on `/private/tmp` (the cargo target dir filled the partition).
2. Removed the `/private/tmp` worktree, waited for 18c to commit
   its work, then `git checkout phase-18a-local-mode-providers`
   inside the main repo. This reused the existing target dir
   and avoided the disk-space issue.
3. Re-applied all Phase 18a edits in the main repo on the 18a
   branch. Verified 18c's commit (`f674051`) was still intact
   on its own branch.

**Lesson:** when launching N parallel agents on related branches,
spin up a worktree per agent BEFORE the agents start work. Tell
each agent the absolute path to its worktree as the "working
directory" override. The "checked out in main repo" path is
fragile when multiple agents share the same repo state.

### Dehydration budget scaling — deferred

Spec §390 calls for deriving dehydration budgets from the detected
context limit when local mode is enabled. The relevant constants
live in `OperationalConfig::tier2`
(`pre_map_prompt_budget = 80_000`, `answer_prompt_budget`, etc.) and
are NOT currently surfaced as a contribution. They're hard-coded in
`src-tauri/src/pyramid/mod.rs`. Scaling them at toggle-on time would
require either:
- Threading a mutable handle to `OperationalConfig` into
  `local_mode::enable_local_mode` (the `OperationalConfig` is held
  behind `Arc<OperationalConfig>` today, not `Arc<RwLock<...>>`).
- Introducing a new `dehydration_budget` contribution schema_type
  with its own dispatcher branch and operational reader.

Both are beyond Phase 18a scope. **Deferred** with a comment in
`local_mode::enable_local_mode` pointing here. Local mode still
works against the default budgets; users running tiny-context
models (e.g., Llama 3.2:1b at 4k context) may need to manually
drop those budgets before their first build succeeds. Phase 19
candidate.

### `model_list:` resolver async refactor

Phase 8's `resolve_option_source` was sync. The Phase 18a (L5)
extension to hit Ollama's `/api/tags` for Ollama-shaped providers
needs network async, but `&Connection` (the rusqlite handle) is
not Send and can't cross an await point in a tauri command future.

**First attempt:** make `resolve_option_source` async. Failed —
the compiler saw `&conn` captured across the (only) await point
and rejected the future as non-Send.

**Working approach:** keep `resolve_option_source` synchronous,
add a separate `resolve_model_list_only(provider_registry,
provider_id) -> Vec<OptionValue>` async function that takes only
the registry. The IPC handler in `main.rs` checks the source
prefix and routes `model_list:` calls to the async path WITHOUT
holding the rusqlite lock. Other branches stay on the sync path
and hold the lock as before. This works because the model_list
branch never touches `&conn` — it only reads from the in-memory
registry + network.

The sync `resolve_option_source` for `model_list:` consults the
30-second cache (populated by the async path on the previous
call) and falls back to the Phase 8 tier-table view. This means
the renderer's mount-time fetch always returns immediately from
the cache or fallback, and the next IPC call refreshes the
cache. Acceptable trade-off — the user sees the previous probe
result for 30s, then a fresh probe.

### OllamaCloudProvider (L3) — re-deferred

Time/scope pressure from the TierRoutingYaml repair plus the
async resolver refactor consumed the Phase 18a headroom. The
local Ollama path (via `ProviderType::OpenaiCompat` + bare
localhost URL) already covers Adam's Ouro test use case
completely. L3 (remote Ollama behind nginx with bearer auth)
is a sharpening for users who run a hosted Ollama on a
separate machine. Recommend the conductor re-defer L3 to a
"Phase 18a+1 fix pass" or absorb it into Phase 19's planning.

### What worked

- The implementer pass caught the TierRoutingYaml mismatch
  immediately because the workstream prompt explicitly flagged
  it as a likely deviation. Wanderers on built systems would
  have caught it too, but the pre-warning let me find + fix it
  BEFORE the local mode IPC path hit it.
- The state-table approach (Option A) is much cleaner than the
  contribution-chain walking approach (Option B) for the same
  reason: explicit columns are easier to verify than parsed
  free-text triggering notes. Recommend always defaulting to
  Option A unless there's a reason not to add a schema migration.
- `feedback_always_scope_frontend.md` was load-bearing — the
  Settings.tsx Local LLM section is the gate. I built it before
  the OllamaCloudProvider deferral because Adam tests by feel
  and the toggle is what unblocks the Ouro test.

### Wanderer pass — reader lock held across `.await` in status IPC

**What the wanderer caught.** The verifier's 11-point checklist
explicitly noted at item 11 that `yaml_renderer_resolve_options`
at `main.rs:8810` correctly drops the rusqlite reader lock
BEFORE awaiting the async model-list resolver. But when the
same verifier looked at `pyramid_get_local_mode_status` at
`main.rs:7702` (item 6 in the IPC registration check), they
only verified registration, not the lock-release pattern. The
status IPC held the tokio reader Mutex across a 5-second
`probe_ollama().await`, which is async-safe but blocks every
other reader-bound IPC for the duration of the probe.

**Why the verifier missed it.** Verifier was working off a
punch list of "11 failure modes" that the workstream prompt
flagged. Lock-hold patterns across `.await` wasn't one of the
11 items. The verifier did a surface-level check that the 5
IPCs were registered (they were) and moved on. The wanderer,
following a different prompt about tracing end-to-end execution
and looking for wiring bugs, landed on the same code but asked
a different question: "how long can the reader lock be held
here, and does anything concurrent care?"

**The fix.** Split `get_local_mode_status` into
`load_status_snapshot` (sync, DB-only) and
`refresh_status_reachability` (async, no DB/mutex), then have
the IPC handler drop the reader lock between them via an
explicit scoped block. The legacy `get_local_mode_status`
wrapper stays for enable/disable return paths where the probe
latency is already baked into the writer lock's span.

**Lesson.** When a verifier punch list exists, wanderer passes
still need to ask "what ISN'T on this list?" Look for the
same anti-pattern the verifier explicitly called out elsewhere
and check it in every other IPC handler in the touched file.
If the verifier flagged "IPC X drops the lock correctly", the
wanderer should grep for every OTHER IPC that takes a reader
lock + awaits and confirm each one. The verifier's "good"
citation is a template for the wanderer's sweep.

**Meta-lesson.** Verifiers working from an explicit punch list
are necessary but not sufficient. The wanderer's job is
exactly to look at what the punch list doesn't cover. This
matches the 17/18 wanderer-catch rate from earlier phases —
the bug is always in the space the verifier didn't enumerate.

**Tests added.** 4 unit tests pinning the split-function
contract: snapshot is sync and DB-only, refresh is async and
no-op when disabled. Test count 1254 → 1258 passing. Same 7
pre-existing failures.

