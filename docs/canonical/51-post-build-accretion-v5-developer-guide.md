# Post-build Accretion v5 — Developer Guide

Supplement to the [operator guide](./50-post-build-accretion-v5-operator-guide.md). This doc is for engineers touching the v5 code: where each module lives, what contracts they hold, the test patterns, and the extensibility surface.

---

## 1. Module map

### New in v5 (directly owned by the post-build accretion branch)

| Module | Path | Purpose |
|--------|------|---------|
| `vocab_genesis` | `src-tauri/src/pyramid/vocab_genesis.rs` | The ONLY hardcoded source of vocab rows. Three `const &[(…)]` slices for annotation_type, node_shape, role_name. Seeded by `seed_genesis_vocabulary`. |
| `vocab_entries` | `src-tauri/src/pyramid/vocab_entries.rs` | CRUD + cache + HTTP-shape for `vocabulary_entry:*` contribution rows. Process-wide cache with cross-process coherence (Phase 9c-3-1). |
| `role_binding` | `src-tauri/src/pyramid/role_binding.rs` | Per-slug role→chain bindings in `pyramid_role_bindings`. `resolve_binding` raises `UnresolvedBinding` loud; callers MUST NOT silently skip. |
| `dadbear_compiler` | `src-tauri/src/pyramid/dadbear_compiler.rs` | Observation events → work items. `map_event_to_primitive` is the canonical routing table; `run_compilation_for_slug` is the public entry. |
| `dadbear_supervisor` | `src-tauri/src/pyramid/dadbear_supervisor.rs` | Work items → dispatch → apply. Owns the JoinSet loop, crash recovery, and the per-primitive dispatch arms (re_distill, role_bound, extract, stale_check, etc.). |
| `stale_helpers_upper` | `src-tauri/src/pyramid/stale_helpers_upper.rs` | `execute_supersession` (the canonical re_distill apply path) + cascade_annotations loader. Phase 9c-3-2 added `assert_write_lock_held`. |
| `chain_dispatch` | `src-tauri/src/pyramid/chain_dispatch.rs` | Mechanical primitive implementations. Every `rust_function: foo` referenced by a starter chain resolves through `is_known_mechanical_function` + the match arms in this module. |
| `pyramid_scheduler` | `src-tauri/src/pyramid/pyramid_scheduler.rs` | Periodic tick emitters (accretion / sweep) + threshold emit. Config-driven via `scheduler_parameters` contribution. |
| `observation_events` | `src-tauri/src/pyramid/observation_events.rs` | Canonical append-only observation stream (`dadbear_observation_events`). `write_observation_event` is the only writer. |
| `lock_manager` | `src-tauri/src/pyramid/lock_manager.rs` | Global per-slug Read/Write lock. `assert_write_lock_held` (9c-3-2) is the contract guard `execute_supersession` enforces. |

### Relevant pre-v5 modules that v5 consumes heavily

| Module | Relevance |
|--------|-----------|
| `config_contributions` | Contribution envelope + supersede dance. Vocab + scheduler_parameters both ride on this. |
| `chain_loader` + `chain_registry` | YAML parse + Tier 2 include_str! bundle. `load_chain_by_id` is what the supervisor calls for role_bound primitives. |
| `chain_executor` | Starter chain runner. `execute_chain_for_target` is the public entry for library-called chains. |
| `types` | `AnnotationType`, `NodeShape`, `ShapePayload`, `DebateTopic`, `GapTopic`, `MetaLayerTopic`, `shape_handler_registry` (Phase 9c-2-1). |
| `db` | The single place `init_pyramid_db` sets up the schema + seeds vocab + seeds scheduler defaults + runs migrations. Every `phase*_post_build_tests` module lives here. |
| `routes` | Warp filters for every pyramid HTTP endpoint. `handle_annotate` is the canonical v5 HTTP entry; `process_annotation_hook` is the private post-save hook it spawns. |

---

## 2. Test patterns

### Unit tests (in-process, lib target)

All 223 post_build unit tests live in `src-tauri/src/pyramid/db.rs` inside `phase*_post_build_tests` modules (20 total). Conventions:

- Module name is `phaseN[letter]_post_build_tests` (e.g. `phase9c3_post_build_tests`). Phase-per-module; never cross-cut.
- `post_build_test_support::test_lock` is a process-wide mutex; every test that touches the vocab cache or the lock manager MUST hold this (otherwise tests race each other's DB state through the static cache).
- `fresh_db` / `fresh_conn` helpers: open in-memory sqlite, `init_pyramid_db`, `invalidate_cache`. Every vocab-touching test starts here.
- LLM mocking uses `phase6_post_build_tests::mocked_llm_config(server.url())` + `openrouter_body(content)`. These are `pub(super)` so peer test modules in `db.rs` share the wiring — they're NOT visible to integration tests (see §4).

### Integration test (external, test target)

`src-tauri/tests/phase9d_smoke.rs`. Run with `cargo test --test phase9d_smoke`. Unlike unit tests, this test binary compiles without `cfg(test)` on the lib, so `pub(crate)` / `#[cfg(test)] pub(crate) mod test_hooks` items are NOT visible. The smoke test uses only `pub` items from `wire_node_lib::pyramid::*`.

Why a warp mini-server instead of driving the real `routes::pyramid_routes`: the real route filter requires JWT state, wire_auth, node_id, and the full Arc<PyramidState> (30+ fields). The mini-server wraps the same public building blocks (`db::save_annotation`, `observation_events::write_observation_event`, `pyramid_scheduler::*`) in a minimal warp filter — same transport, same DB semantics, no auth scaffolding.

### When to add which

- **New primitive or event type**: unit test in the relevant `phase*_post_build_tests` module. Drives `map_event_to_primitive` + supervisor arm directly.
- **New HTTP endpoint**: both a unit test (driving the handler's public internals) and a smoke-test scenario in `phase9d_smoke.rs` exercising real HTTP + reqwest.
- **New chain or mechanical primitive**: unit test invoking `chain_executor::execute_chain_for_target` with a programmatic `ChainDefinition`. For LLM steps, use `mocked_llm_config`.
- **Cross-process invariant**: unit test with `tempfile::TempDir` + two separate `Connection::open` calls. Phase 9c-3-1 covers the vocab coherence case.

---

## 3. Extending the system

### New event type

The compiler's `map_event_to_primitive` is authoritative. To add a new event type:

1. Edit `map_event_to_primitive` in `dadbear_compiler.rs`. Return `Some((primitive, step_name, tier))` for your event.
2. If routing is role-driven, also add an arm to `role_for_event`.
3. If the primitive is new (not role_bound / re_distill / extract), add a dispatch arm to `dadbear_supervisor::dispatch_item`.
4. Unit test in a new or existing `phase*_post_build_tests` module asserting the mapping + dispatch outcome.

Event types are NOT a contribution dimension in v5. Adding one is a code deploy. This is deliberate: the mapping is the canonical routing table, and drift in the routing table is catastrophic.

### New primitive

Primitives are the supervisor's dispatch arms. Shipped: `re_distill`, `role_bound`, `extract`, `tombstone`, `stale_check`. Adding one:

1. Add the arm in `dadbear_supervisor::dispatch_item`.
2. Add the arm in `dadbear_compiler::compile_observations` (maps event → primitive).
3. Ensure `mark_role_bound_failed` / `mark_primitive_failed` has a consistent failure-handling path.
4. Unit test in `phase*_post_build_tests` covering compile + dispatch + failure.

### New observation source

Observation event `source` column values: `"watcher"`, `"cascade"`, `"rescan"`, `"evidence"`, `"vine"`, `"annotation"`, `"purpose"`, `"dadbear"`, `"chain"`, `"vocabulary"`, `"scheduler"`, `"operator"`. Adding a new source is documentation + a write call to `observation_events::write_observation_event` with the new source string. No registry — sources are emitter-local labels. If a new source introduces a new event type, follow "New event type" above.

### New node shape (with typed handler)

The `types::ShapeHandler` trait:

```rust
pub trait ShapeHandler: Send + Sync {
    fn shape_name(&self) -> &'static str;
    fn parse_payload(&self, raw: &str) -> Result<ShapePayload>;
}
```

Implementation steps:

1. Add `YourTopic` struct + `ShapePayload::Your(YourTopic)` variant in `types.rs`.
2. Implement `ShapeHandler` for `YourHandler`; `parse_payload` deserializes the canonical JSON shape with LOUD error on mismatch (never fall through to `ShapePayload::Raw` — that's the unregistered-shape fallback).
3. Register in `shape_handler_registry()`:
   ```rust
   reg.insert("your_shape".into(), Arc::new(YourHandler));
   ```
4. Add a `vocabulary_entry:node_shape:your_shape` row to `GENESIS_NODE_SHAPES` so the vocab registry surfaces it.
5. Unit test matching the Phase 9c-2-1 test set (handler_parses_payload, handler_mismatch_raises_loud, Raw_fallback_for_unregistered).

---

## 4. LockManager contract

`LockManager::global()` is a process-wide per-slug `Arc<RwLock<()>>` table. Contracts:

- **`execute_supersession` MUST hold the slug write guard.** Phase 9c-3-2 added `assert_write_lock_held(slug, caller)` at the top of `execute_supersession`. Debug builds panic if the guard is not held; release builds log `tracing::error!` and bail. Test call sites must wrap:
   ```rust
   let _slug_lock = LockManager::global().write("my-slug").await;
   execute_supersession(...).await?;
   drop(_slug_lock);
   ```
- **Multiple readers can coexist.** `read()` increments a per-slug counter; the guard drops release the counter.
- **Writers are exclusive.** A `write()` call blocks until all readers + any prior writer release.
- **No lock upgrade.** A read guard cannot be promoted to a write guard; release the read first.
- **`is_write_locked` / `is_read_locked` are test-only observability.** Don't use them for runtime decisions — they're inherently racy.

Why the contract exists: pre-9c-3-2, a concurrent `execute_supersession` + annotation write could race the cascade_annotations load vs the node update. The write guard serializes the apply path.

---

## 5. Contribution supersession dance

Two supersession patterns coexist in v5:

### (A) `pyramid_config_contributions` rows

Via `config_contributions::supersede_config_contribution`:

1. Prior row's `status` flips from `active` to `superseded`.
2. New row inserts with `supersedes_id` → prior `contribution_id`.
3. Both rows carry a `wire_native_metadata_json` blob; the new row inherits from the prior with `maturity` reset to `Draft`.
4. `BEGIN IMMEDIATE` serializes concurrent supersessions. The unique-active index (`uq_config_contrib_active`) fails loud on conflict.

Used by: vocab entries, role bindings (indirectly via the `pyramid_role_bindings` table which has its own supersede chain), scheduler_parameters, purpose contributions, every `pyramid_config_contributions` consumer.

### (B) `pyramid_role_bindings` self-reference-then-fixup

Used only by `role_binding::set_binding`. The partial unique index on `(slug, role_name, scope) WHERE superseded_by IS NULL` prevents two active rows — but a naïve UPDATE + INSERT breaks it in the window between steps. The fix:

1. **Park** prior row: `UPDATE SET superseded_by = id WHERE id = prior`. Now prior's `superseded_by` is non-NULL, so it's outside the partial index.
2. **Insert** new active row.
3. **Redirect** prior's pointer: `UPDATE SET superseded_by = <new_id> WHERE id = prior`.

Three statements in a transaction. The intermediate "self-reference" state is semantically meaningless but structurally necessary — it lifts the prior out of the partial index so the new row can insert without collision.

Same pattern used in `purpose.rs::supersede_purpose`. Grep for "self-reference-then-fixup" to find the rationale docstring.

---

## 6. Cross-phase dependency map

Read this before making changes that touch multiple phases.

```
Phase 1 (role bindings)          ──┐
Phase 2 (shapes + vocab types)    ├─┬── Phase 6c-A (vocab contribution)
                                   │ │       │
Phase 3 (role-bound primitive)   ──┤ │       ├── Phase 6c-B (vocab-driven dispatch)
                                   │ │       │
Phase 4 (cascade chain YAML)     ──┘ │       ├── Phase 6c-C (frontend vocab surfacing)
                                     │       │
Phase 5 (annotation_cascade step)   ─┤       └── Phase 6c-D (role-vocab unification)
                                     │
Phase 7a-d (reactive annotations    ─┤
  + utility chains)                  │
                                     │
Phase 8 (annotation → re_distill    ─┤       Phase 9a (three architectural integrations)
  actually fires, the original bug)  │               │
                                     │               ├── Phase 9b (scheduler + accretion threshold + sweep)
                                     │               │
                                     │               ├── Phase 9c-1 (debate_collapse vocab + chain)
                                     │               │
                                     │               ├── Phase 9c-2 (shape handlers + cascade prompt filter + cooldown)
                                     │               │
                                     │               └── Phase 9c-3 (cross-process coherence + lock assertion + reopen)
                                     │
                                     └── Phase 9d (smoke + docs — this phase)
```

Key invariants preserved across phases:

- **Pre-v5 annotation cascade contract**: every annotation emits `annotation_written` on ancestors (Phase 8-1 flipped the routing from re_distill to role_bound cascade_handler, but the emission contract is unchanged).
- **Vocab registry is the single source of annotation types** post-6c-B. No Rust enum.
- **`execute_supersession` is the only pyramid_nodes writer for the cascade path.** Phase 8-2 shipped this.
- **Starter chains never mutate pyramid_nodes directly.** Mechanical primitives in `chain_dispatch.rs` write to observation events / work items / shape payloads; the actual `distilled / headline / build_version` update goes through `execute_supersession`.

---

## 7. Pointers for common tasks

| Task | Start here |
|------|-----------|
| "Add a new annotation type" | `vocab_genesis.rs::GENESIS_ANNOTATION_TYPES` for documentation + `publish_vocabulary_entry` for runtime publish. |
| "Change the cascade routing for an event" | `dadbear_compiler::map_event_to_primitive` + `role_for_event`. |
| "Add a mechanical primitive to a chain" | `chain_dispatch.rs` — find an existing primitive's `match` arm, copy the pattern, add to `is_known_mechanical_function`. |
| "Debug why a chain isn't firing" | Check `dadbear_work_items.state` + `resolved_chain_id`; if `resolved_chain_id` is NULL, role binding resolution raised. |
| "Bump the supervisor tick interval" | `dadbear_supervisor::TICK_INTERVAL_SECS`. Not contribution-driven — it's a code-deploy constant. |
| "Add a new HTTP route" | `routes.rs::pyramid_routes` — compose a new `route!(...)` arm, add the handler, wire into the `.or(...)` chain at the bottom. |
| "Lower the scheduler clamp floor for testing" | `pyramid_scheduler::MIN_INTERVAL_SECS`. Don't commit the lowered value; the clamp is there for a reason. |

---

## 8. Canonical references

- [Operator guide](./50-post-build-accretion-v5-operator-guide.md) — user-facing flow, troubleshooting, extensibility from outside.
- `CHAIN-DEVELOPER-GUIDE.md` — starter chain YAML format + mechanical primitive contract.
- `project_wire_canonical_vocabulary.md` in the memory index — the theoretical frame (IR, four contribution types, ten operation types).
- `project_convergence_decision.md` — the 2026-03-25 inflection when the pyramid executor converged with Wire action chain format.
