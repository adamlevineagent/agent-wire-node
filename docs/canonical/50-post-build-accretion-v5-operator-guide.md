# Post-build Accretion v5 — Operator Guide

This is the ship-level reference for everything that lives inside post-build accretion v5. It covers the architecture, the annotation vocabulary, the chains, the HTTP surface, the scheduler, the extensibility paths, and the troubleshooting paths. If you're debugging why an annotation didn't produce an effect, or adding a new annotation type without a code deploy, this is where to look.

Scope: everything shipped across Phases 1 through 9d on the `post-build-accretion-v5` branch. The full in-process test suite is `cargo test --lib post_build_tests` (223 tests). The external smoke suite is `cargo test --test phase9d_smoke` (11 tests).

---

## 1. Architecture overview

### The frame: everything is a contribution

Post-build accretion v5 rests on a single architectural commitment: the pipeline's vocabulary, routing, and configuration are all contribution rows, not Rust constants. Three orthogonal registries, all held in `pyramid_config_contributions`:

1. **Vocabulary (v5 Phase 6c-A)** — three namespaces: `annotation_type`, `node_shape`, `role_name`. Each row is a `vocabulary_entry:<kind>:<name>` schema_type. Adding a new annotation type or node shape is a contribution publish, not a code deploy.
2. **Role bindings (v5 Phase 1)** — per-slug bindings from role name (e.g. `cascade_handler`, `debate_steward`) to starter chain id (e.g. `starter-cascade-judge-gated`). Stored in `pyramid_role_bindings`. Operators can supersede a binding to swap the chain that fires on a role's events.
3. **Scheduler parameters (v5 Phase 9b-1)** — a single `scheduler_parameters` row holds the interval knobs, thresholds, and cooldowns. Editable without a code deploy.

### Cascade flow

```
HTTP POST /pyramid/:slug/annotate
    │
    ▼
save_annotation  →  pyramid_annotations row
    │
    ├───► emit_annotation_observation_events
    │        walks parent_id chain, writes one annotation_written
    │        (or annotation_superseded for correction) per ancestor
    │
    ├───► reactive?  if vocab.reactive  →  annotation_reacted on target node
    │
    ├───► creates_delta?  create thread delta
    │
    └───► threshold?  count_annotations_since_cursor >= K → accretion_threshold_hit

dadbear_compiler::run_compilation_for_slug
    │
    ▼
map_event_to_primitive  →  (primitive, step_name, tier)
    │
    │  "annotation_written" / "annotation_superseded" → role_bound annotation_cascade
    │  "annotation_reacted" → role_bound cascade_reacted (resolves vocab handler_chain_id)
    │  "accretion_tick" / "accretion_threshold_hit" → role_bound accretion_*_dispatch
    │  "sweep_tick" → role_bound sweep_tick_dispatch
    │  "gap_detected" → log-only (observability)
    │  "gap_resolved" → role_bound oracle_gap_resolved
    │  "purpose_shifted" / "meta_layer_crystallized" → role_bound oracle_*
    │  "debate_spawned" / "debate_collapsed" / "debate_reopened" → role_bound debate_*
    │
    ▼
dadbear_work_items row (state=compiled, resolved_chain_id stamped)

dadbear_supervisor tick
    │
    ▼
dispatch_item  →  prompt_materializer  →  provider  →  LLM (or mechanical)
    │
    ▼
handle_completion  →  execute_supersession (for re_distill)
    │                  pyramid_nodes.distilled / headline / build_version updated
    │
    └─  or for role_bound: execute_chain_for_target
        (full starter chain runs: LLM + mechanical steps)
```

### Shape nodes

Nodes carry a `node_shape` column (default `'scaffolding'`) with a matching `shape_payload_json` when applicable. Shipped shapes:

- **scaffolding** — default. `shape_payload_json IS NULL`. The canonical node carrying `distilled / topics / entities / decisions / terms`.
- **debate** — holds `DebateTopic { concern, positions: [DebatePosition], cross_refs, vote_lean }`. Created by `append_annotation_to_debate_node` on the first `steel_man` / `red_team` arriving at a Scaffolding node.
- **meta_layer** — holds `MetaLayerTopic { purpose_question, parent_meta_layer_id, covered_substrate_nodes, topics }`. Created by `starter-synthesizer` when the oracle detects crystallizable substrate.
- **gap** — holds `GapTopic { concern, description, demand_state, candidate_resolutions, evidence_anchors, source_annotation_ids }`. Created by `starter-gap-dispatcher` on a `gap` annotation.

Shape handlers live in a contribution-driven registry (Phase 9c-2-1). Each typed handler deserializes a canonical payload; shapes registered in vocab but without a typed handler fall through to `ShapePayload::Raw(serde_json::Value)` so agent-published shapes are at least queryable.

---

## 2. Annotation types (16 genesis)

Every type is a `vocabulary_entry:annotation_type:<name>` row. Fields: `description`, `handler_chain_id` (optional), `reactive` (bool), `creates_delta` (bool), `include_in_cascade_prompt` (bool), `event_type_on_emit` (optional string — overrides the default `annotation_written` event name on save; `correction` genesis uses `"annotation_superseded"`). Adding a new type is a `publish_vocabulary_entry` call.

### Narrative types (`include_in_cascade_prompt=true`)

| Type | Handler | Reactive | Delta | Purpose |
|------|---------|:-:|:-:|---------|
| `observation` | — | n | n | Neutral fact pinned to a node. Most common. |
| `correction` | — | n | **y** | Marks an inaccuracy. Creates a thread delta; emitted as `annotation_superseded`. |
| `question` | — | n | n | Open question; candidate for FAQ / evidence loop. |
| `friction` | — | n | n | Learning-curve moment; input to prompt/doc improvement. |
| `idea` | — | n | n | Speculative proposal. |
| `era` | — | n | n | Vine-intelligence temporal era marker. |
| `transition` | — | n | n | Vine-intelligence phase-shift marker. |
| `health_check` | — | n | n | Self-applied health check result. |
| `directory` | — | n | n | Folder-scope annotation (not a single file). |
| `steel_man` | `starter-debate-steward` | **y** | n | Good-faith reconstruction of an opposing position. Triggers debate steward. |
| `red_team` | `starter-debate-steward` | **y** | n | Adversarial challenge. Triggers debate steward. |
| `hypothesis` | `starter-debate-steward` | **y** | n | Proposed causal claim awaiting evidence. Triggers debate steward. |

### Operational directives (`include_in_cascade_prompt=false`)

| Type | Handler | Reactive | Delta | Purpose |
|------|---------|:-:|:-:|---------|
| `gap` | `starter-gap-dispatcher` | **y** | n | Missing-evidence signal. Creates a Gap node with demand state. |
| `purpose_declaration` | `starter-meta-layer-oracle` | **y** | n | Declares intended purpose; may trigger crystallization. |
| `purpose_shift` | `starter-meta-layer-oracle` | **y** | n | Explicit purpose change; oracle re-evaluates meta-layer coverage. |
| `debate_collapse` | `starter-debate-collapse` | **y** | n | Collapse a Debate node back to Scaffolding. Finalizes + emits `debate_collapsed`. |

`include_in_cascade_prompt=false` means the annotation content is operational chatter (routing metadata, finalize reasons) — it should not pollute the ancestor re-distill LLM prompt's `cascade_annotations` section. Narrative types DO flow into that prompt (Phase 9c-2-2).

### Example HTTP call

```bash
curl -X POST http://localhost:8765/pyramid/my-slug/annotate \
  -H "Authorization: Bearer $LOCAL_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "node_id": "L2-auth-flow",
    "annotation_type": "correction",
    "content": "This says the cache TTL is 60s but the code says 120s.",
    "author": "adam"
  }'
```

Effects: annotation row saved, `annotation_superseded` events emitted on L3 / apex ancestors, thread delta created (correction is `creates_delta=true`), and the next compiler pass produces role_bound work items routed through `cascade_handler`.

---

## 3. Chains (13 starter chains)

All in `chains/defaults/starter/`. Each chain is a `ChainDefinition` YAML; the Tier 2 bundle (`chains/defaults/starter/mod.rs`'s `include_str!`s in `chain_loader`) ensures first-boot seeds reach the DB. Prompts live in `chains/prompts/` (not shown below).

### Event-triggered (dispatched by DADBEAR compiler from observation events)

| Chain | Role | Input event | Shape |
|-------|------|-------------|-------|
| `starter-cascade-immediate-redistill` | cascade_handler (backfilled existing slugs) | `annotation_written` / `annotation_superseded` | mechanical — queues re_distill directly |
| `starter-cascade-judge-gated` | cascade_handler (new slugs, default) | `annotation_written` / `annotation_superseded` | LLM judge → mechanical queue re_distill on approve |
| `starter-debate-steward` | debate_steward / handler for `steel_man`, `red_team`, `hypothesis` | `annotation_reacted` | mechanical — append to / create Debate node |
| `starter-debate-collapse` | handler for `debate_collapse` | `annotation_reacted` | mechanical — finalize debate, Debate→Scaffolding, emit `debate_collapsed` |
| `starter-gap-dispatcher` | gap_dispatcher / handler for `gap` | `annotation_reacted` | mechanical — create Gap node, emit `gap_detected` |
| `starter-meta-layer-oracle` | meta_layer_oracle / handler for `purpose_declaration`, `purpose_shift` | `annotation_reacted`, `purpose_shifted` | LLM — evaluate crystallizability; dispatches synthesizer |
| `starter-synthesizer` | synthesizer | `meta_layer_crystallized` | LLM + mechanical — build MetaLayer node, mark covered gaps resolved |
| `starter-accretion-handler` | accretion_handler | `accretion_tick`, `accretion_threshold_hit` | LLM — digest recent annotations |
| `starter-sweep` | sweep | `sweep_tick` | mechanical — archive stale failed work items + old contributions |

### Library-called (invoked from other chains or from code)

| Chain | Purpose |
|-------|---------|
| `starter-judge` | Debate judge — called by `cascade-judge-gated`. Decides redistill vs skip. |
| `starter-evidence-tester` | Runs evidence loops to verify / refute claims. |
| `starter-reconciler` | Reconciles conflicting contributions / orphaned nodes post-build. |
| `starter-authorize-question` | Gates Question-typed slug creation + propose_question HTTP route. |

Every starter chain is a first-class contribution — operators can publish a replacement chain YAML, supersede the role binding, and the new chain fires immediately on the next event.

---

## 4. HTTP endpoints

All endpoints live under the pyramid HTTP surface (normally `localhost:8765` for a desktop-run Wire Node). Authenticated endpoints take `Authorization: Bearer <token>`; local-only ones accept the local session token.

| Endpoint | Auth | Purpose |
|----------|------|---------|
| `POST /pyramid/:slug/annotate` | Local | Save an annotation; triggers the full cascade flow. |
| `POST /pyramid/:slug/propose_question` | Local | Propose a question slot on a question-typed slug; gated by `authorize_question` chain. |
| `POST /pyramid/:slug/debates/:node_id/collapse` | Local | Operator-driven Debate→Scaffolding transition. Queues a `debate_collapse` annotation + fires the starter-debate-collapse chain. |
| `POST /pyramid/:slug/debates/:node_id/reopen` | Local | Operator-driven re-open of a collapsed debate. Emits `debate_reopened`, bypasses the post-collapse cooldown on the next append. |
| `GET /vocabulary/:vocab_kind` | Public (no auth) | List active vocab entries for a kind (`annotation_type`, `node_shape`, `role_name`). Backs MCP + frontend vocab surfacing. |
| `GET /pyramid/:slug/annotations?node_id=…` | Dual auth | Read annotations for a node / slug. |
| `GET /pyramid/:slug/debates/:node_id` | Local | Introspection: current debate state — node_shape, DebateTopic payload (if present), recent spawn/collapse/reopen events, is_collapsed + cooldown_until. |
| `GET /pyramid/:slug/role_bindings` | Local | Introspection: all active role bindings for the slug (role_name → chain_id + created_at). |
| `GET /pyramid/:slug/synthesis_history/:node_id` | Local | Introspection: MetaLayer node's synthesis trail — shape_payload + re-distill history (build_version, applied_at) + most recent cascade_annotations loaded. |

The operator HTTP surface (registered in `src-tauri/src/pyramid/routes_operator.rs`) adds 25+ routes for compute market, system observability, local mode, and providers — see `docs/canonical/84-http-operator-api.md`.

---

## 5. MCP CLI usage

Every HTTP route above has a matching MCP tool. The canonical agent flow:

1. **Start a Wire node** — the desktop app or `pyramid-cli serve` brings up the HTTP surface.
2. **Publish a custom vocab entry** — `pyramid-cli vocab publish --kind annotation_type --name my_custom --description "..." --reactive true --handler-chain-id starter-debate-steward`. Returns the contribution id.
3. **Annotate a node** — `pyramid-cli annotate --slug my-slug --node L1-foo --type my_custom --content "..."`. The HTTP handler's `from_str_strict` validation now accepts `my_custom` because the vocab entry is live.
4. **Propose a question** — `pyramid-cli propose-question --slug my-slug --parent-node L2-topic --question "What is …?"`. Runs through the `authorize_question` chain.
5. **Collapse a debate** — `pyramid-cli debates collapse --slug my-slug --node L1-debate --reason "Pro side wins"`. Fires `starter-debate-collapse`.

`pyramid-cli` commands are self-describing (`--help`); full catalog at `docs/canonical/80-pyramid-cli.md` and `docs/canonical/81-mcp-server.md`.

---

## 6. Configuration

### `scheduler_parameters` contribution

One active row, global scope (slug=NULL). Seeded at first boot via `pyramid_scheduler::seed_scheduler_defaults`. Fields:

| Field | Default | Sane range | Purpose |
|-------|---------|------------|---------|
| `accretion_interval_secs` | 1800 (30 min) | 30 – 2592000 (30 days) | Interval between `accretion_tick` emits per active slug. |
| `sweep_interval_secs` | 21600 (6 h) | 30 – 2592000 | Interval between `sweep_tick` emits per active slug. |
| `accretion_threshold` | 50 | 0+ | K: annotations since cursor that triggers an immediate `accretion_threshold_hit`. 0 disables volume path. |
| `accretion_tick_window_n` | 50 | 1+ | `window_n` stamped into `accretion_tick` metadata; accretion chain uses it as LLM context cap. |
| `sweep_stale_days` | 7 | 1+ | Failed work items older than this are sweep candidates. |
| `sweep_retention_days` | 30 | 1+ | Archive window for soft-archived rows. |
| `collapse_cooldown_secs` | 600 (10 min) | 0+ | Post-collapse cooldown: steel_man / red_team arriving within this window is refused rather than resurrecting the Debate. 0 disables guard (loud warn). |

### Supersession workflow

```bash
# Read current active row
pyramid-cli config get scheduler_parameters
# or via HTTP operator API: GET /ops/contributions/active?schema_type=scheduler_parameters

# Supersede via contribution write
pyramid-cli config supersede scheduler_parameters \
    --yaml-file new-scheduler.yaml \
    --note "Bump accretion threshold to 100 for larger pyramids"
```

The active row is replaced atomically by `supersede_config_contribution` (Phase 0a-1 commit 5: `BEGIN IMMEDIATE` serializes concurrent supersessions).

Clamps: `accretion_interval_secs` and `sweep_interval_secs` are clamped to `[30s, 30d]` at load time (`MIN_INTERVAL_SECS` / `MAX_INTERVAL_SECS` in `pyramid_scheduler.rs`) with a loud `tracing::warn!` when clamping fires. The scheduler re-reads on every tick, so supersessions land within one period.

---

## 7. Adding a new annotation type (canonical extensibility path)

This is the "no code deploy" extensibility surface. Full flow:

1. **Publish a `vocabulary_entry:annotation_type:<name>` contribution.** Body is YAML:
   ```yaml
   vocab_kind: annotation_type
   name: my_reactive_type
   description: "Brief description shown in /vocabulary/annotation_type"
   handler_chain_id: starter-debate-steward   # optional; omit for non-reactive
   reactive: true                              # emits annotation_reacted → dispatches handler_chain_id
   creates_delta: false                        # true → creates a thread delta on save
   include_in_cascade_prompt: true             # true → annotation content flows into ancestor re-distill
   event_type_on_emit: annotation_written      # optional; override the default emit event name
                                                # (correction genesis uses "annotation_superseded")
   ```
2. **Process-wide cache invalidates.** `publish_vocabulary_entry` resets the atomic watermark; subsequent reads re-populate from SQL.
3. **Cross-process readers sync on next read.** The MCP server, Wire node, and CLI all hit `MAX(id)`-indexed check; peer writes are observed on the next read cycle (Phase 9c-3-1).
4. **HTTP write path accepts the new type immediately.** `AnnotationType::from_str_strict` consults the vocab registry at every POST.

How the four flags interact:

- `reactive=true` → arrival emits `annotation_reacted` against the target node. The compiler maps this to a role_bound work item whose `resolved_chain_id` = the vocab entry's `handler_chain_id`. If reactive but no handler_chain_id is set, the event is emitted but the compiler's role_bound resolution raises loud (`UnresolvedBinding`).
- `creates_delta=true` → save also creates a `pyramid_deltas` row on the matching thread (subject to thread match; falls back to log if no thread).
- `include_in_cascade_prompt=true` → the annotation's content is pulled into the `cascade_annotations` section of any ancestor re-distill prompt. False for operational directives (gap, purpose_*, debate_collapse).
- Any combination is valid; the flags are orthogonal.

---

## 8. Adding a new chain (code-deploy path)

This is NOT contribution-driven — chains carry mechanical primitives whose Rust function names must exist in `chain_dispatch.rs`. Flow:

1. **Write the YAML.** Put it in `chains/defaults/starter/starter-my-chain.yaml`. Schema: `ChainDefinition` (see `chains/CHAIN-DEVELOPER-GUIDE.md`).
2. **Register any new mechanical primitives.** If your chain calls `rust_function: my_new_fn`, implement `my_new_fn` in `chain_dispatch.rs` and add its name to `is_known_mechanical_function()`.
3. **Register any new prompts.** Prompts go in `chains/prompts/my-chain-step.md`; reference via `prompt: my-chain-step` on the step.
4. **Add to Tier 2 `include_str!` bundle.** In `chain_loader.rs`'s starter-chains list so a fresh install picks up the chain on first boot.
5. **If it's a new role:** add a `vocabulary_entry:role_name:my_role` to `vocab_genesis.rs::GENESIS_ROLE_NAMES` with `handler_chain_id: starter-my-chain`. Genesis role-binding initialization + startup backfill pick this up.
6. **If it's an existing role with a new default:** publish a `role_binding` supersession per-slug, or update `initialize_genesis_bindings` for fresh slugs.

The starter chain format is deliberately constrained (no nested invoke_chain, explicit `rust_function` references for all mechanicals, prompt refs by id). See `project_wire_canonical_vocabulary.md` in Adam's memory for the rationale.

---

## 9. Adding a new node shape (partial extensibility path)

Publishing a `vocabulary_entry:node_shape:<name>` row makes the shape accepted at ingress. The `NodeShape::from_db` read path accepts the name, and payloads deserialize as `ShapePayload::Raw(serde_json::Value)` — arbitrary JSON is preserved and queryable but not strongly typed.

For typed handling (validation, rendering, re-distill-prompt enrichment), a Rust code deploy is required:

1. Implement `ShapeHandler` for your new type in `types.rs`.
2. Register in `shape_handler_registry()` alongside `debate`, `meta_layer`, `gap`.
3. Add the `ShapePayload::YourType(YourTopic)` variant + `parse_shape_payload` match arm.

Vocab-only publish gives you "the shape is known"; the Rust deploy gives you "the shape is type-checked + renderable."

---

## 10. Known v5 limitations / v6 backlog

Shipping v5 as-is; none of these block the ship gate. Operator workarounds noted where they exist.

- **`emit_accretion_threshold_hit` called inline in the annotate hook, not batched.** If an operator POSTs N annotations rapidly past K, multiple threshold events emit. The work-item layer de-dups (distinct `step_name` / target), so this is not correctness-breaking — just a chattier event log than v6 will have. No operator workaround needed.
- **Scheduler intervals clamp floor is `30s` (not sub-second).** Sub-30s tick cadences are not supported — the floor is a safety guard against a YAML supersession turning the scheduler into a hot loop. A v6 item is "sub-second burst mode for test harnesses."
- **Starter chains register by string id, not contribution id.** If an operator supersedes a starter chain via a contribution, the in-process `chain_loader` load-by-id still hits the DB-backed row, so supersession works — but a chain id that's gone entirely (no DB row + no seed) raises at first dispatch. Operator workaround: don't delete starter chains; supersede them.
- **Gap nodes don't auto-close on evidence arrival.** `starter-synthesizer` marks covered gaps resolved when the MetaLayer covering them crystallizes (Phase 9b-5). Gaps not covered by a crystallized meta-layer remain open indefinitely. Operator workaround: manually annotate gap target with a resolution note, then mark resolved via `pyramid-cli gaps resolve`.
- **`debate_reopened` event is log-only at the compiler level.** It maps to no primitive; the guard in `append_annotation_to_debate_node` reads the event directly to bypass the cooldown. A v6 item is "debate_reopened role-dispatches to a `debate-reopen-validator` chain if operators want custom post-reopen logic."
- **Cross-pyramid cascades are not v5 scope.** An annotation on slug A never triggers work on slug B. Cross-pyramid event routing exists in the router but is not wired to annotation flows.
- **Force-triggering a cascade against a specific target from HTTP is not supported.** The supervisor-tick cadence is the production trigger path. Read-only introspection is available via the `GET /pyramid/:slug/debates/:node_id`, `/role_bindings`, and `/synthesis_history/:node_id` routes (Phase 9 close-3).

### Design decisions (not limitations)

- **`starter-debate-collapse` and `starter-debate-steward` are separate chains by design.** A single unified debate chain with a `collapse: true` step was considered but intentionally kept separate because the semantics are opposite (steward appends; collapser finalizes). Merging them would muddy the responsibility boundary; the split is architectural and is NOT scheduled to be merged in any roadmap milestone.

---

## 11. Troubleshooting

### "My annotation landed but nothing happened."

1. **Check the vocab entry.** `curl http://localhost:8765/vocabulary/annotation_type | jq '.entries[] | select(.name=="<your_type>")'`. If missing or `reactive=false`, that's expected — only reactive types fire chains. Non-reactive types still emit `annotation_written` on ancestors; check the observation events table.
2. **Check the observation events.** `sqlite3 pyramid.db "SELECT * FROM dadbear_observation_events WHERE slug='<slug>' ORDER BY id DESC LIMIT 10"`. If empty, the annotate hook raised; check logs for `[annotation] post-save hook failed`.
3. **Check the work items.** `sqlite3 pyramid.db "SELECT id, primitive, step_name, state, resolved_chain_id FROM dadbear_work_items WHERE slug='<slug>' ORDER BY id DESC LIMIT 10"`. If the compiler hasn't run, wait for the next supervisor tick (or trigger via the admin CLI).
4. **Check the role binding.** `sqlite3 pyramid.db "SELECT * FROM pyramid_role_bindings WHERE slug='<slug>' AND superseded_by IS NULL"`. Missing → role resolution raised; logs will have `UnresolvedBinding`.

### Stalled compile cursor

Symptom: observation events keep piling up but no new work items. Root cause is usually an `map_event_to_primitive` raise for an unknown event type. The compiler logs `map_event_to_primitive: unknown event_type '<x>' — skipping` loud. Fix: either the emitter is wrong, or a new event type needs to be added to `map_event_to_primitive`. Vocab cannot fix this — event types are NOT a contribution dimension in v5 (they'd need to be for v6).

### Dead re_distill work items

Symptom: work item state=`previewed` or `dispatched` but never applied. Check:

1. Is the supervisor running? `ps aux | grep wire-node-desktop`.
2. Is the prompt materializer raising? Logs will have `spawn_blocking join error for materialization`.
3. Is the LlmConfig set up? `sqlite3 pyramid.db "SELECT * FROM pyramid_providers WHERE enabled=1"`. No enabled providers → dispatch fails silently at the provider lookup.
4. Was the work item swept? `SELECT state, archived_at FROM dadbear_work_items WHERE id='<id>'`. Sweep archives failed items older than `sweep_stale_days`.

### Vocab cache stale

Cross-process cache coherence (Phase 9c-3-1) kicks in on the NEXT read after a peer write. If your MCP server still sees old vocab after the Wire node published a new entry, force a read (`curl /vocabulary/annotation_type` on the stale process). The watermark poll is `SELECT MAX(id) FROM pyramid_config_contributions WHERE schema_type LIKE 'vocabulary_entry:%'` — if this exceeds the atomic, the cache invalidates + repopulates.

### Debate resurrects after collapse

If a steel_man arrives immediately after `debate_collapse`, the append is REFUSED within `collapse_cooldown_secs` (default 10 min). The refusal raises loud in `append_annotation_to_debate_node`; the HTTP response is a 500 with the cooldown age in the body. Workaround: wait the cooldown, or POST `/pyramid/:slug/debates/:id/reopen` first — reopen emits `debate_reopened` which bypasses the cooldown for the next append (Phase 9c-3-3).

### Scheduler ticks not firing

1. The scheduler task must be spawned at boot. Check `[pyramid_scheduler] spawned accretion + sweep periodic tasks` in logs.
2. The scheduler re-reads the config on every tick — if you superseded `scheduler_parameters` with a huge interval, the next tick happens one period out, not immediately. The log line `pyramid_scheduler[accretion]: interval changed — re-tuning` fires when a supersession is picked up.
3. No active slugs → no ticks (scheduler iterates `pyramid_slugs` WHERE `archived_at IS NULL`).

---

## 12. Architecture diagrams

### Annotation flow

```
                        HTTP POST /pyramid/:slug/annotate
                                     │
                                     ▼
                            validate slug + node
                                     │
                                     ▼
                          AnnotationType::from_str_strict   ◄── vocab registry
                                     │
                                     ▼
                           db::save_annotation
                                     │
                                     ▼
        ┌───────────────────────────────────────────────┐
        │        process_annotation_hook (background)    │
        │                                                │
        │  ┌────────────────────┐                        │
        │  │ emit observation   │ one per ancestor       │
        │  │ events (ancestors) │ annotation_written or  │
        │  └────────────────────┘ annotation_superseded  │
        │                                                │
        │  ┌────────────────────┐                        │
        │  │ creates_delta?     │─► delta::create_delta  │
        │  └────────────────────┘                        │
        │                                                │
        │  ┌────────────────────┐                        │
        │  │ reactive?          │─► emit                 │
        │  │                    │   annotation_reacted   │
        │  └────────────────────┘                        │
        │                                                │
        │  ┌────────────────────┐                        │
        │  │ count >= K?        │─► emit                 │
        │  │                    │   accretion_threshold  │
        │  └────────────────────┘   _hit                 │
        └────────────────────────────────────────────────┘
                                     │
                                     ▼
                      dadbear_compiler next tick
                                     │
                                     ▼
                        map_event_to_primitive
                                     │
                                     ▼
                         work_items(role_bound)
                                     │
                                     ▼
                       dadbear_supervisor dispatches
                                     │
                                     ▼
                     ┌───────────────┴───────────────┐
                     ▼                               ▼
          execute_supersession            execute_chain_for_target
          (re_distill primitive)          (role_bound primitive)
                     │                               │
                     ▼                               ▼
           pyramid_nodes updated          starter chain runs
           (distilled/headline/bv)        (LLM + mechanicals)
```

### Event routing fan-out

```
                              observation_event
                                     │
                                     ▼
                        ┌──────────────────────────┐
                        │  map_event_to_primitive  │
                        └──────────────────────────┘
                                     │
          ┌────────────┬────────────┼──────────────┬──────────────┐
          ▼            ▼            ▼              ▼              ▼
    annotation_    annotation_  annotation_    accretion_     sweep_tick
    written        superseded   reacted        tick /            │
                                               threshold_hit     │
          │            │            │              │              │
          └────────────┴────────────┘              │              │
                    │                              │              │
                    ▼                              ▼              ▼
           role_bound                   role_bound         role_bound
           annotation_cascade           accretion_tick_    sweep_tick_
           step                         dispatch           dispatch
                    │                              │              │
                    ▼                              ▼              ▼
          resolve cascade_handler       resolve            resolve sweep
          binding                       accretion_handler  binding
                    │                   binding                   │
                    │                              │              │
                    ▼                              ▼              ▼
          starter-cascade-              starter-accretion-  starter-sweep
          {judge-gated|                 handler
          immediate-redistill}
```

### Role binding resolution

```
  annotation_reacted(target=N, handler_chain_id=C)
                     │
                     ▼
  map_event_to_primitive("annotation_reacted")
     => ("role_bound", "cascade_reacted", "stale_remote")
                     │
                     ▼
  Phase 8-1 audit 7a-gen: if handler_chain_id present in metadata,
  use it directly (vocab-stamped override).
  Else: role_for_event("annotation_reacted")
     => Some("debate_steward")   [pre-Phase-9c, legacy path]
                     │
                     ▼
  role_binding::resolve_binding(slug, "<role>")
                     │
                     ▼
  chain_id stamped onto work_item.resolved_chain_id
                     │
                     ▼
  supervisor: chain_loader::load_chain_by_id(chain_id, chains_dir)
                     │
                     ▼
  chain_executor::execute_chain_for_target(state, chain, slug, target, inputs)
```

---

## References

- **Canonical memory index**: `~/.claude/projects/.../memory/project_wire_canonical_vocabulary.md`
- **Phase-by-phase commit map**: `git log --oneline --grep 'post-build' post-build-accretion-v5`
- **Developer-focused deep dive**: [`51-post-build-accretion-v5-developer-guide.md`](./51-post-build-accretion-v5-developer-guide.md)
- **Unit test suite**: `src-tauri/src/pyramid/db.rs` — 20 `phase*_post_build_tests` modules, 223 tests total
- **Smoke suite**: `src-tauri/tests/phase9d_smoke.rs` — 11 real-HTTP + mockito scenarios
