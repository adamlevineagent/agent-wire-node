# v5 Accretions Live Test - architectureconcurrencytest3

Tested by: codex-newman
Date: 2026-04-25
Target slug: `architectureconcurrencytest3`
Branch/worktree: `post-build-accretion-v5` in `/Users/adamlevine/AI Project Files/agent-wire-node`
Runtime: local Wire Node at `127.0.0.1:8765`, SQLite DB at `~/Library/Application Support/wire-node/pyramid.db`

## Stop Condition

This is a live research snapshot, not a fix pass. Adam notified Newman that Elaine was actively fixing known issues, so testing stopped after the paths below to avoid overlapping code changes. No source code was edited by Newman.

## Executive Summary

The v5 reactive surface is partially alive. `steel_man`, `gap`, and `correction`/delta each produced durable state with clear provenance. Several other advertised paths triggered observation events but failed to commit their intended shape/state:

- Repeated same-target reactions are dropped by target-scoped role-bound work item identity. This blocked `red_team` append and `debate_collapse` on `L1-000`.
- `hypothesis` routes into `starter-debate-steward` but the work item failed and left the node scaffolding.
- `purpose_shift` routes into the meta-layer oracle and emits `synthesizer_invoked`, but the work item remains `previewed` and no `meta_layer` node is created.
- Scheduler accretion ticks exist, and DADBEAR compiled past them, but no `starter-accretion-handler` work item was found and `pyramid_slugs.accretion_cursor` remains `0`.
- The live annotation vocabulary rejects `claim`, `bug`, and `decision`, so bug records had to be encoded as `annotation_type=friction` with a YAML header of `type: bug`.

## Tested Paths

| Path | Result | Evidence |
| --- | --- | --- |
| typed `claim` annotation | Fail | API rejected `claim` as unknown. Runtime valid types were `correction`, `debate_collapse`, `directory`, `era`, `friction`, `gap`, `health_check`, `hypothesis`, `idea`, `observation`, `purpose_declaration`, `purpose_shift`, `question`, `red_team`, `steel_man`, `test_v5_custom_type`, `transition`. Recorded friction annotation #433. |
| `steel_man` on `L1-000` | Pass | Annotation #434 emitted `annotation_reacted` #129152, `debate_steward_invoked` #129159, and `debate_spawned` #129160. Work item `architectureconcurrencytest3:00000000:role_bound:0:L1-000` applied. Node `L1-000` became `node_shape=debate`. |
| `red_team` on same `L1-000` | Fail | Annotation #435 emitted `annotation_reacted` #129171, but no new role-bound work item appeared. Existing role-bound item for `L1-000` stayed tied to #129152. Debate payload still had one position and an empty `red_teams` array. Recorded bug-shaped friction #436. |
| `hypothesis` on `L1-001` | Fail | Annotation #437 emitted `annotation_reacted` #129211 and `debate_steward_invoked` #129212. Work item `architectureconcurrencytest3:00000000:role_bound:0:L1-001` failed and the node stayed scaffolding. Recorded bug-shaped friction #438. |
| `gap` on `L1-002` | Pass | Annotation #439 emitted `annotation_reacted` #129217, `gap_dispatcher_invoked` #129218, and `gap_detected` #129219. Work item applied. Node `L1-002` became `node_shape=gap` with `demand_state=open` and `source_annotation_ids=["annotation#439"]`. |
| `debate_collapse` on `L1-000` | Fail | Annotation #440 emitted `annotation_reacted` #129222 with handler `starter-debate-collapse`, but no new work item, no `debate_collapse_invoked`, and no `debate_collapsed` event appeared. Node stayed `node_shape=debate`. Recorded bug-shaped friction #441. |
| `purpose_shift` on `L2-000` | Fail/stuck | Annotation #442 emitted `annotation_reacted` #129226, `meta_layer_oracle_invoked` #129227, and `synthesizer_invoked` #129228. Work item `architectureconcurrencytest3:00000000:role_bound:0:L2-000` remains `previewed`; `node_shape=meta_layer` count remains `0`. Recorded bug-shaped friction #460. |
| `correction` on `Q-L0-000` | Pass | Annotation #461 emitted `annotation_superseded` #129290/#129291/#129292 to ancestors. Delta #1839 was inserted on thread `Q-L0-000`, sequence `1`, source_node_id `Q-L0-000`, relevance `low`; thread delta_count became `1`. |
| scheduler accretion tick | Fail/not confirmed | `accretion_tick` events #129114 and #129185 exist. `dadbear_compilation_state.last_compiled_observation_id` reached #129292, but no work item with `starter-accretion-handler`, accretion step name, or those observation IDs was found. `pyramid_slugs.accretion_cursor` remains `0`. |

## UI/API Surface

The shape-aware intro endpoints expose reactive state:

- `GET /pyramid/architectureconcurrencytest3/debates/L1-000` returned `node_shape=debate` and the debate payload from annotation #434.
- `GET /pyramid/architectureconcurrencytest3/debates/L1-002` returned `node_shape=gap` and the gap payload from annotation #439.

The generic node endpoint did not expose shape fields:

- `GET /pyramid/architectureconcurrencytest3/node/L1-000`
- `GET /pyramid/architectureconcurrencytest3/node/L1-002`
- `GET /pyramid/architectureconcurrencytest3/node/L2-000`

Those responses included ordinary node content fields but not `node_shape` or `shape_payload_json`. If the main UI relies on generic node reads, v5 shapes can be durable in DB but invisible in that surface unless it calls the shape-aware endpoints.

I did not visually inspect the Tauri WebView after Adam asked Newman to wrap up because Elaine was actively fixing known issues.

## Primary Findings

1. **Target-scoped role-bound work item IDs suppress repeated reactions on the same node.** The clearest evidence is `L1-000`: `steel_man` worked, but later `red_team` and `debate_collapse` only emitted `annotation_reacted` and never got their own committed handler work. The work item identity appears to need an event/annotation/handler dimension, not only slug/epoch/primitive/target.

2. **The debate steward path does not successfully handle `hypothesis`.** The runtime vocab routes `hypothesis` to `starter-debate-steward`, but the work item failed immediately after invocation and no Debate shape was created.

3. **The meta-layer oracle path can preview without committing.** `purpose_shift` progressed through oracle and synthesizer observations, then stalled in `previewed` with no `meta_layer_crystallized` event and no meta-layer node.

4. **Scheduler accretion ticks are observable but not actionable.** Ticks were emitted and compiled past, but no accretion-handler work appeared and the slug cursor did not move.

5. **Annotation vocabulary and typed-annotation tooling are out of sync.** The `pyramid-annotate` workflow expects `claim`, `bug`, and `decision`; the live runtime rejected those types. Test bug records were therefore stored as `friction` annotations with structured YAML declaring `type: bug`.

## Bug Annotations Created

Because live `annotation_type=bug` is rejected, these were recorded as `annotation_type=friction` with YAML `type: bug`:

- #436 on `L1-000`: repeated `red_team` reaction dropped by target-scoped role-bound dedup.
- #438 on `L1-001`: `hypothesis` failed in `starter-debate-steward`.
- #441 on `L1-000`: `debate_collapse` reaction dropped after prior role-bound item.
- #460 on `L2-000`: `purpose_shift` stuck previewed with no meta-layer crystallization.

## Recommended Follow-up

After Elaine's fix pass lands, rerun the same slug with a small reset strategy:

1. Use fresh nodes or clear only the v5 test annotations/work items for this slug.
2. Retest same-target `steel_man -> red_team -> debate_collapse` as one sequence.
3. Retest `hypothesis` on a fresh scaffolding node.
4. Retest `purpose_shift` and require either `meta_layer_crystallized` or an explicit failed/blocked state with an error.
5. Let one scheduler tick pass and verify a `starter-accretion-handler` work item plus cursor movement.
6. Check the actual UI, not only the API endpoints.
