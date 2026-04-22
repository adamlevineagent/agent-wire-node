# Walker v3 consumer inventory

Created: 2026-04-21
Purpose: satisfy `walker-provider-configs-and-slot-policy-v3.md` §6 Phase 0a-1 pre-flight by producing an authoritative starting inventory of legacy routing/model consumers before Phase 1 edits begin.

## Source command

```bash
cd agent-wire-node
rg -n "primary_model|fallback_model_1|fallback_model_2|pyramid_tier_routing|RouteEntry|resolve_ir_model" src-tauri/src
```

Verified on 2026-04-21 from `/Users/adamlevine/AI Project Files/agent-wire-node`.

## Raw count summary

- Raw grep hits: 277
- Files hit: 27
- Exact `INSERT INTO pyramid_config_contributions` call sites for the Phase 0a envelope-writer refactor: 35 across 9 files

These raw hits intentionally include tests, comments, and type definitions. The table below is the implementation-planning inventory used to sort the hits into Phase 1 work buckets.

## File inventory

| File | Raw hits | Bucket | Notes |
|---|---:|---|---|
| `src-tauri/src/pyramid/llm.rs` | 72 | Mixed: `reads Decision` + `retires` + `test fixture` | Largest migration surface. Contains active fallback logic against `config.primary_model` / `fallback_model_*`, `RouteEntry` handling, and test fixtures/types that also reference the legacy model fields. |
| `src-tauri/src/pyramid/chain_dispatch.rs` | 35 | Mixed: `reads Decision` + `retires` + `test fixture` | Contains `resolve_ir_model` and tier-to-model fallback logic that Phase 1 is expected to delete/replace with `DispatchDecision`. Includes unit tests for the retiring helper. |
| `src-tauri/src/main.rs` | 22 | `retires` | Legacy config plumbing and IPC/http-adjacent state flow that still references model fields. Also the current real onboarding save owner (`save_onboarding`) relevant to walker/onboarding coordination. |
| `src-tauri/src/pyramid/chain_executor.rs` | 20 | `reads Decision` | Core executor reads `dispatch_ctx.config.primary_model` and calls `resolve_ir_model`; these are direct Phase 1 migration targets. |
| `src-tauri/src/pyramid/routes.rs` | 19 | Mixed: `reads synthetic Decision` + `retires` | Route handlers expose/update legacy config model fields and preview-ish flows. Needs split treatment so live-build paths migrate while legacy settings payloads retire cleanly. |
| `src-tauri/src/pyramid/mod.rs` | 16 | `retires` | `PyramidConfig` struct/default definitions for the legacy model fields and `use_chain_engine`. Struct-field deletion lands here in the total migration. |
| `src-tauri/src/pyramid/dispatch_policy.rs` | 12 | `retires` | `RouteEntry`/routing-rule definitions are part of the surface walker v3 replaces. |
| `src-tauri/src/pyramid/db.rs` | 12 | `retires` | `pyramid_tier_routing` schema + helpers. Migration and cleanup work retire these reads/writes once walker contributions take over. |
| `src-tauri/src/pyramid/build.rs` | 11 | `retires` | Legacy build pipeline. These references disappear when `use_chain_engine: false` is no longer a supported steady-state path. |
| `src-tauri/src/server.rs` | 8 | `reads synthetic Decision` | Boot/server-side flows with legacy config references that need verification during startup/preview migration. |
| `src-tauri/src/pyramid/characterize.rs` | 7 | `reads Decision` | Uses legacy model selection in active path; migrate to `DispatchDecision`. |
| `src-tauri/src/pyramid/local_mode.rs` | 6 | `retires` | Legacy/local-mode config references; expected to shrink or disappear with the new resolver-driven path. |
| `src-tauri/src/pyramid/wire_discovery.rs` | 4 | `reads synthetic Decision` | Preview/discovery surface; validate whether these are true synthetic-decision reads or can retire outright during implementation. |
| `src-tauri/src/pyramid/fleet_mps.rs` | 4 | `reads synthetic Decision` | Non-build evaluation/planning path; expected to consume synthetic decisions. |
| `src-tauri/src/pyramid/evidence_answering.rs` | 4 | `reads Decision` | Active question-system logic; Phase 1 consumer. |
| `src-tauri/src/pyramid/config_helper.rs` | 4 | `retires` | Legacy config helper surface. |
| `src-tauri/src/pyramid/stale_engine.rs` | 3 | `reads synthetic Decision` | DADBEAR/staleness-adjacent callers should use synthetic Decision construction where they do not enter a live chain step. |
| `src-tauri/src/pyramid/provider_pools.rs` | 3 | `reads Decision` | Provider-selection path; migrate to the resolved decision/provider params. |
| `src-tauri/src/pyramid/generative_config.rs` | 3 | `retires` | Legacy config-generation references. |
| `src-tauri/src/pyramid/vine.rs` | 2 | `reads Decision` | Active path, low-count consumer. |
| `src-tauri/src/pyramid/step_context.rs` | 2 | `reads Decision` | Low-count core path; verify during executor migration. |
| `src-tauri/src/pyramid/migration_config.rs` | 2 | `retires` | Legacy migration config surface. |
| `src-tauri/src/pyramid/defaults_adapter.rs` | 2 | `reads Decision` | Active adapter path; migrate with executor/dispatch changes. |
| `src-tauri/src/pyramid/yaml_renderer.rs` | 1 | `reads synthetic Decision` | Preview/render path; verify synthetic-decision usage. |
| `src-tauri/src/pyramid/provider.rs` | 1 | `retires` | Legacy provider registry/routing surface. |
| `src-tauri/src/pyramid/config_contributions.rs` | 1 | `reads Decision` | Contribution store touches a legacy reference; verify during resolver integration. |
| `src-tauri/src/fleet.rs` | 1 | `reads Decision` | Fleet contract touchpoint; verify when replacing legacy routing identifiers with walker Decision data. |

## Planning notes

- This inventory is a pre-flight planning artifact, not a claim that every raw grep hit is production logic. Tests, comments, and struct definitions are intentionally visible because they still affect Phase 1 scope and cleanup order.
- The most consequential active-path files are `llm.rs`, `chain_dispatch.rs`, `chain_executor.rs`, `routes.rs`, and `characterize.rs`.
- The most consequential retirement files are `mod.rs`, `dispatch_policy.rs`, `db.rs`, and `build.rs`.
- Any implementation branch should refresh this grep before Phase 1 starts if additional plan edits or unrelated code changes land first.

## Rev 1.0.2 additions (post-fresh-audit code verification)

Additional consumers surfaced by the 2026-04-22 pair audit against live code:

| File | Hits | Bucket | Notes |
|---|---|---|---|
| `src-tauri/src/pyramid/build_runner.rs` | 2 | `retires` / behavioral | `state.use_chain_engine.load(Ordering::Relaxed)` at :328 and `from_depth is only supported with the chain engine` error at :358. Direct user-visible signal for §5.6.3's chain-engine-enable-ack modal when `use_chain_engine: false`. Not covered by grep for `primary_model`/`fallback_model_*`/`pyramid_tier_routing`/`RouteEntry`/`resolve_ir_model` but directly relevant to walker v3's behavioral gate. |
| `src-tauri/src/pyramid/yaml_renderer.rs` (additional callers) | +2 | `retires` / replace | `:575` and `:749` call `registry.list_tier_routing()` alongside the :428 rewrite target. All three migrate together or UI tier surface silently drifts. |
| `src-tauri/src/pyramid/llm.rs:2216-2232` | — | `reads Decision` | Market dispatch retry loop reads `get_compute_participation_policy(&conn)` on a fresh SQLite connection. Decision-in-scope threading required; not just a substitution. |

### Code-reality corrections to rev 1.0.1 framing

- `src-tauri/src/pyramid/stale_engine.rs` is DECOMMISSIONED for dispatch (explicit "DECOMMISSIONED" comments at lines 180, 498, 602, 619-622; `drain_and_dispatch` is a no-op). The active DADBEAR dispatch path is `src-tauri/src/pyramid/dadbear_supervisor.rs:514 dispatch_materialized_item`. Any plan or inventory reference to stale_engine-as-dispatch-site is stale.
- `src-tauri/src/pyramid/ollama_probe.rs` does not exist. Ollama probing is `pub async fn probe_ollama(base_url: &str) -> OllamaProbeResult` at `src-tauri/src/pyramid/local_mode.rs:330` with callers at `:499`, `:570`, `:1204`.
- `resolve_ir_model` at `src-tauri/src/pyramid/chain_dispatch.rs:1059` is already partially migrated (Phase 3 fix-pass): line 1067 consults `provider_registry.resolve_tier()` as priority-2 before falling back to `primary_model`. Phase 1 subsumes this registry-consulting path; the `match tier` fallback at :1080-1088 retires.
- `INSERT INTO pyramid_config_contributions` hits exactly 35 sites across 9 files. Dropping the `pyramid_` prefix grep-matches 44 sites / 7 files on the different `config_contributions` table. CI deny-rule MUST use the full table name.
