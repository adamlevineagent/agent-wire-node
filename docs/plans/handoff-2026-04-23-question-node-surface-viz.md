# Handoff: Question Nodes as First-Class Pyramid Surface Objects

**Date:** 2026-04-23  
**Worktree:** `/Users/adamlevine/AI Project Files/agent-wire-node-walker`  
**Branch observed:** `walker-v3-shipping`  
**Context:** finishing tester version of `agent-wire-node` with Walker v3 / pyramid system and compute-market routing.

This doc exists to stop re-cycling the same context. It captures the live debugging state, the Pyramid Surface contract, what has already been changed, what is known about the question-node data model, and the implementation direction for making the visual surface show the user everything.

## Latest Status: 2026-04-24 Stage-Manager Pass

The older “incomplete DAG patch” section below is now historical context. The combined worker output has been integrated and validated.

Current state:

- Fresh no-overlay runtime decomposition uses a layer-wise frontier DAG:
  - apex is persisted first
  - each parent frontier is decomposed as a whole layer
  - child questions can name multiple `parent_ids`
  - canonical edges are persisted to `pyramid_question_edges`
- Resume no longer falls back to the old recursive single-parent incremental builder. If partial question rows exist, runtime reconstructs `QuestionDagDraft` from `pyramid_question_nodes` + `pyramid_question_edges`, finds unfinished branch nodes as a frontier, and continues layer-wise.
- `chains/prompts/question/decompose_frontier.md` now exists; the new DAG-defining prompt is externalized instead of running from inline Rust fallback.
- Empty parsed frontier responses retry instead of silently marking a whole frontier as leaves.
- Dead recursive incremental resume helpers were removed.
- Question DAG tests now include resume-frontier reconstruction and multi-parent child preservation.

Residuals:

- Preview (`decompose_question`) is still a preview-only recursive tree builder.
- Delta (`decompose_question_delta`) is still tree-shaped and only used when `load_prior_state.has_overlay == true`.
- The YAML `instruction` field still does not directly select the frontier prompt; the primitive selects `decompose_frontier.md` internally. The prompt content is externalized, but prompt selection is still primitive-coded.
- Real-slug runtime validation should inspect `pyramid_question_edges`, multi-parent rows, rendered surface edges, and answer linkage through `source_question_id`.

Validation from the latest pass:

- `cargo fmt --check`
- `git diff --check`
- `cargo check --bin wire-node-desktop`
- `cargo test --lib pyramid::question_decomposition::tests -- --nocapture` passed 31/31
- Reported DB fallout tests passed:
  - `pyramid::chain_dispatch::tests::test_w1b_build_step_dispatch_decision_empty_db_returns_some_default`
  - `pyramid::manifest::tests::test_hydrate_returns_node_content`
  - `pyramid::query::tests::test_drill_includes_web_edges_and_empty_when_no_thread`
- `answered_node_outbox` passed
- `is_sqlite_busy` passed 4/4
- `npm run build` passed with the existing Vite large chunk warning

## Implementation Status Update

As of the latest pass, the first read-model/UI implementation is in place:

- `question.yaml` declares `decompose` / `decompose_delta` as structural `node_fill` visualization sourced from `question_nodes`.
- Backend live/tree/drill read models expose `pyramid_question_nodes` as typed question objects with normalized visual depth and best-effort linked answer metadata.
- The Pyramid Surface preserves question metadata through layout, merges live question nodes during builds, and suppresses duplicate upper answer nodes when a question node owns the linked answer.
- The inspector can open question nodes through `pyramid_drill` and shows the question, answer linkage, evidence, gaps, children, and stored record data.
- Validation passed: `cargo fmt`, `cargo test --bin wire-node-desktop --no-run`, and `npm run build`.

Remaining architectural work is called out in the companion implementation log: durable question-to-answer foreign key, truly incremental question-node persistence/events, and full consumption of non-`type` viz metadata.

## Follow-Up Status Update

The first structural follow-up pass has now been completed:

- Answer nodes now carry `source_question_id` from the source `LayerQuestion.question_id`; legacy text/depth matching remains as fallback.
- Question decomposition persists finalized question subtrees during recursion and emits question-node `NodeProduced` events for newly-created rows.
- The frontend consumes full YAML `viz` metadata as `VizStepConfig`, including `source` and `node_kind`.
- Folding answer nodes into question nodes is DAG-aware: edges through hidden answer nodes are rewritten through the owning question id, then exact duplicate edges are deduped.
- Validation passed again: `cargo fmt`, `cargo test --bin wire-node-desktop --no-run`, `npm run build`, and `git diff --check`.

## Pre-Compaction Alert: Maximal Question DAG Patch Incomplete

The user set aside evidence-loop externalization and asked to make question decomposition the maximal solution.

The current source tree now contains an **incomplete, unvalidated** mid-patch toward that model. Do not assume it compiles until checked.

Read the companion log section `Maximal Question DAG Work: Pre-Compaction Checkpoint` before continuing:

- `docs/plans/impl-log-2026-04-23-question-node-surface-viz.md`

Current target:

- Layer-wise/frontier question decomposition, not recursive depth-first branches.
- Canonical question DAG, not a single-parent tree.
- One canonical child question can connect to multiple parent questions.
- Answers/evidence/gaps/provenance accrue onto stable question IDs.

Mid-patch already started:

- Added `parent_ids` fields to backend question/read-model types.
- Added `pyramid_question_edges` schema and DB helpers.
- Began converting live/tree/drill read models to derive parents/children from canonical question edges.

Still required before validation:

- Finish Rust read-model conversion and all remaining struct literals.
- Update TypeScript mirror types and inspector/surface navigation for `parent_ids`.
- Then replace depth-first `build_subtree_incremental` with a layer-wise DAG builder.
- Runtime test14 also failed locally on `database is locked` while saving `L1-000` during `evidence_loop`; see implementation log for exact DB evidence.

## User Direction

The user wants question pyramids to expose the full reasoning object:

- A node should start as the **question**.
- As answers arrive, the answer, evidence verdicts, gaps, provenance, and model/audit record should accrue onto that same visible object.
- Questions are not fake placeholders. They are first-class, useful nodes.
- The UI currently hides too much and wastes space. The node surface and inspector should show substantially more of the actual record.
- The system is intentionally YAML/contribution-driven. Do not hardcode frontend behavior for specific steps like `decompose`; chain YAML should declare visualization behavior and the generic Pyramid Surface should render it.

Important phrasing from the user:

> The node should start off as the questions, and then as the answers come in we should add the answers to them because that is truly the understanding, both parts.

> I want the nodes to show the user everything, right now we hold back a lot and make poor use of space.

> Importantly, this is all controlled within the yaml.

## Current Live Symptoms

During a 5-document question build:

- L0 nodes appeared after extraction.
- Long-running steps like `decompose`, `extraction_schema`, and `evidence_loop` left the main pyramid visualization mostly static.
- Chronicle showed activity, but the graph did not show the intermediate question structure.
- The UI hid many LLM/audit events as “mechanical hidden”; some of those were actually LLM calls, not mechanical work.
- The inspector showed only a subset of node data and often emphasized answers while never making the questions themselves visible as navigable objects.

## Viz Contract From Existing Docs

The relevant docs are:

- `docs/plans/pyramid-surface.md`
- `docs/plans/pyramid-surface-sprint2.md`
- `docs/specs/build-viz-expansion.md`
- `docs/question-pyramid-architecture.md`
- `docs/question-pipeline-guide.md`
- `docs/plans/pyramid-surface-visual-encoding.md`

The important contract:

1. **AD-1: chain YAML drives visualization.** The frontend is a renderer for viz primitives, not a special-case handler for named steps.
2. `useVizMapping.ts` loads the active chain and maps each step to a viz primitive via explicit `step.viz.type` or by primitive inference.
3. Existing viz primitives are:
   - `node_fill`
   - `edge_draw`
   - `cluster_form`
   - `verdict_mark`
   - `progress_only`
4. `pyramid_viz_config` is a contribution that controls rendering settings and overlays; no operational table should be added for user-facing viz settings.
5. Build-time detail should arrive through generic event-bus artifacts such as `ChainStepStarted`, `NodeProduced`, `EdgeCreated`, `VerdictProduced`, `EvidenceProcessing`, `TriageDecision`, `LlmCallStarted`, `LlmCallCompleted`, `StepError`.
6. The inspector is supposed to be a full record. `pyramid-surface.md` explicitly says every non-null node field should be visible.
7. Question architecture docs say everything above L0 exists because a question was asked. The rendered pyramid is a materialized view over a question-answer graph, not merely an answer tree.

Do not violate this by making React say “if step name equals decompose, render question nodes.” The correct path is YAML declaration plus generic node/artifact ingestion.

## Relevant Current Code

### Frontend Surface

- `src/components/pyramid-surface/useVizMapping.ts`
  - Loads chain via `pyramid_get_build_chain`.
  - Maps `step.name` to `VizPrimitive`.
  - Explicit `viz.type` in YAML wins.
  - Currently maps `recursive_decompose` to `progress_only` by default.

- `src/components/pyramid-surface/usePyramidData.ts`
  - Loads finished nodes via `pyramid_tree`.
  - During builds, polls `pyramid_build_live_nodes`.
  - Accumulates build viz state for verdicts, clusters, and new edges from event bus.
  - Currently does **not** fetch or merge `pyramid_question_nodes`.

- `src/components/pyramid-surface/types.ts`
  - `SurfaceNode` currently represents rendered nodes.
  - No explicit `kind: question | answer | source` field yet.
  - `BuildVizState` already supports verdict maps and new edges.

- `src/components/pyramid-surface/PyramidSurface.tsx`
  - Calls `useVizMapping`, `usePyramidData`, `useVisualEncoding`, and renderers.
  - Sends active viz primitive and build viz state into renderer.

- `src/components/pyramid-surface/CanvasRenderer.ts` and `GpuRenderer.ts`
  - Render viz primitives.
  - Recent local changes already added/adjusted `progress_only` pulse behavior and source-target verdict rendering.

### Frontend Inspector

- `src/components/theatre/NodeInspectorModal.tsx`
  - Fetches `pyramid_node_audit` and `pyramid_drill`.
  - Uses `allNodes` from `pyramid_build_live_nodes` for navigation.
  - If the surface starts rendering question ids, this modal will not navigate them unless it receives question-node info too.

- `src/components/theatre/DetailsTab.tsx`
  - Shows some fields: distilled, topics, corrections, decisions, terms, evidence, gaps, children, web edges, self_prompt, basic LLM metadata.
  - Does not yet expose the full `PyramidNode` payload or first-class question-node fields.

- `src/components/theatre/ResponseTab.tsx`
  - If audit exists, shows raw/structured LLM response.
  - If audit does not exist, builds a limited object from node fields.

- `src/components/theatre/inspector-types.ts`
  - Already contains a richer `PyramidNodeFull` / `DrillResultFull` shape matching Rust better than the current tabs actually render.

### Backend Question Nodes

- `src-tauri/src/pyramid/db.rs`
  - Existing table: `pyramid_question_nodes`.
  - Columns include `slug`, `question_id`, `parent_id`, `depth`, `question`, `about`, `creates`, `prompt_hint`, `is_leaf`, `children_json`, `build_id`.
  - Existing CRUD:
    - `save_question_node`
    - `save_question_node_with_build_id`
    - `load_question_nodes_as_tree`
    - `reconstruct_question_tree`
    - `count_question_nodes`
    - `clear_question_nodes`

- `src-tauri/src/pyramid/question_decomposition.rs`
  - `QuestionNode` has `id`, `question`, `about`, `creates`, `prompt_hint`, `children`, `is_leaf`.
  - `assign_question_ids` creates deterministic ids like `q-{hash}`.
  - Display depth assignment: leaves are depth 1, root/apex is max depth + 1. Layer 0 is reserved for source/extraction nodes.
  - Important current gap: `build_subtree_incremental` is named incremental, but fresh decomposition currently builds an in-memory tree first, assigns ids at the end, then `save_tree_nodes_to_db` persists all nodes in a batch. That means the graph cannot show each question layer as soon as each subcall completes until this is changed or events are emitted before persistence.

- `src-tauri/src/pyramid/routes.rs`
  - HTTP `GET /pyramid/:slug/question-tree` already returns question rows when present.
  - It returns `source: "nodes"`, counts, and a lightweight `nodes` array.
  - There is no equivalent Tauri IPC currently used by the Pyramid Surface.

### Backend Answer Nodes

- `src-tauri/src/pyramid/evidence_answering.rs`
  - `LayerQuestion.question_id` is a q-hash question identity.
  - Answer nodes currently get ids like `L{layer}-{seq}`.
  - Answer node `self_prompt` is set to `question.question_text`.
  - Evidence rows target the answer node id, not the q-hash question id.
  - This means question identity and answer identity are currently separate. The UI can correlate by question text/self_prompt as a bridge, but the durable structural fix is to explicitly persist the question id on the answer or make the answer materialize onto the question id.

- `src-tauri/src/pyramid/types.rs`
  - `PyramidNode` has many fields the current inspector underuses: `narrative`, `entities`, `key_quotes`, `transitions`, `time_range`, `weight`, `provisional`, `promoted_from`, `current_version`, `current_version_chain_phase`, etc.

## Recent Local Changes Already Made

Do not revert these unless intentionally revising them:

- `chains/defaults/question.yaml`
  - Added/adjusted step `viz` declarations:
    - `enhance_question`: `progress_only`
    - `decompose`: `progress_only`
    - `decompose_delta`: `progress_only`
    - `extraction_schema`: `progress_only`
    - `evidence_loop`: `verdict_mark`
  - Earlier debugging also changed several tiers/concurrency settings.

- `src/components/pyramid-surface/usePyramidData.ts`
  - `verdict_produced` handling stores both target-node and source-node verdicts.

- `src/components/pyramid-surface/types.ts`
  - `BuildVizState` includes `verdictsBySource`.

- `src/components/pyramid-surface/CanvasRenderer.ts`
  - `progress_only` draws a non-structural pulse around visible nodes.
  - `verdict_mark` combines source and target verdict maps.

- `src/components/pyramid-surface/GpuRenderer.ts`
  - Same conceptual changes as CanvasRenderer.

- `src/components/pyramid-surface/useChronicleStream.ts` and `Chronicle.tsx`
  - LLM calls are no longer casually hidden as “mechanical” in the same misleading way; label changed toward background/hidden.

- Backend changes from earlier in the debug session include:
  - Externalized model tiers for decompose/schema/question-ish steps.
  - Evidence answer concurrency now respects step/provider constraints more than before.
  - Some error swallowing in evidence loop was converted toward fail-loud behavior.
  - Model/provider labeling was improved so UI should not blindly display configured tier label as if it were actual serving model.

Validation already run earlier:

- `npm run build` passed after the visual primitive changes.
- A targeted Rust test around max depth recursion passed.
- `cargo fmt` was run.

## Current Architectural Gap

The current renderer is technically YAML-driven, but the data source is still answer-node-driven:

- The surface renders `pyramid_tree` / `pyramid_build_live_nodes`.
- `pyramid_question_nodes` are persisted but not surfaced in the Tauri frontend data path.
- `recursive_decompose` is declared `progress_only`, so even when it generates a meaningful tree, the current visual surface only pulses existing nodes.
- Evidence answers create separate answer nodes, so the UI visually treats “question” and “answer” as different hidden/visible concepts instead of one accumulating understanding object.

The user’s desired model is:

```text
question node appears
  -> answer fields attach
  -> evidence verdicts attach
  -> gaps attach
  -> audit/provenance attaches
```

not:

```text
question hidden in question table
answer node later appears as unrelated L1/L2 object
```

## Recommended Implementation Direction

### Phase A: Stop Re-Deriving, Expose Question Nodes

Add a Tauri IPC that mirrors and enriches the existing HTTP question-tree route.

Proposed command:

```text
pyramid_question_tree_state(slug) -> {
  slug,
  source,
  total_nodes,
  leaf_nodes,
  is_complete,
  nodes: [...]
}
```

Each node should include:

- `question_id`
- `parent_id`
- `depth`
- `display_depth`
- `question`
- `about`
- `creates`
- `prompt_hint`
- `is_leaf`
- `children`
- `build_id`
- best-effort attached answer:
  - `answer_node_id`
  - `answer_headline`
  - `answer_distilled`
  - `answered`

For the first pass, answer attachment can be best-effort by matching:

- same slug
- answer depth equals question display depth
- answer `self_prompt == question`

This is not the final identity model, but it avoids schema churn while proving the UI model.

### Phase B: Make YAML Declare Question-Node Visualization

Update `chains/defaults/question.yaml` so `decompose` and `decompose_delta` do not merely say `progress_only`.

Preferred minimal extension:

```yaml
viz:
  type: node_fill
  node_source: question_tree
```

or a semantically cleaner primitive if implemented:

```yaml
viz:
  type: question_node_fill
```

The better long-term design is likely to keep the primitive vocabulary small and add source metadata:

```yaml
viz:
  type: node_fill
  source: question_nodes
```

Then `useVizMapping` can still return a primitive, while a later typed viz config can carry source/options. If implementing immediately, extend the frontend chain step type to preserve `viz` options, not only `viz.type`.

### Phase C: Merge Question Nodes Into `usePyramidData`

In `usePyramidData.ts`:

- Poll `pyramid_question_tree_state` during builds and probably on slug load for question slugs.
- Convert question rows into `SurfaceNode`s.
- Use `question_id` as the rendered id.
- Use `display_depth`, not raw DB tree depth, for pyramid placement.
- Keep L0 source/extraction nodes visible.
- For upper layers, prefer question nodes as the canonical visible object. If an attached answer exists, show answered state and answer fields on the same surface node.
- Avoid duplicate answer nodes at depth > 0 when they are already attached to a question node.

`SurfaceNode` should gain optional fields:

```ts
kind?: 'knowledge' | 'question' | 'source';
question?: string;
questionAbout?: string;
questionCreates?: string;
questionPromptHint?: string;
answerNodeId?: string | null;
answerHeadline?: string | null;
answerDistilled?: string | null;
answered?: boolean;
```

This lets renderers stay generic while tooltips/inspector can show more.

### Phase D: Make Inspector Accept Question Nodes

The current modal only knows `pyramid_drill` for `pyramid_nodes`.

Minimal path:

- Add IPC `pyramid_question_node_drill(slug, question_id)` or make `pyramid_drill` fall back to question nodes when no `pyramid_nodes` row exists.
- Return a shape compatible with `DrillResultFull`, plus a `question_node` object.
- Include attached answer node and all answer/evidence/gap/audit data available.

Frontend:

- `NodeInspectorModal.tsx` should fetch question drill fallback if `pyramid_drill` fails or if node id starts with `q-`.
- `DetailsTab.tsx` should render a `Question` section first:
  - question
  - about
  - creates
  - prompt hint
  - is leaf
  - answer node id / headline / distilled when answered
- `ResponseTab.tsx` fallback should show all non-null node/question fields, not a handpicked tiny subset.

This is also where “show everything” starts paying off. At minimum expose:

- all `PyramidNodeFull` fields when present
- question node fields when present
- evidence rows with reasons
- gaps
- web edges
- model/provider/tokens/latency/cache/generation id from audit
- raw prompt and response

### Phase E: Identity Fix, Later but Important

The maximal data-model fix is to stop correlating question and answer by text.

Options:

1. Add `question_id` to answer nodes / structured data and persist it.
2. Make answer node id equal `question_id`, with display aliases for L1-000 style labels.
3. Add a mapping table or contribution linking `question_id -> answer_node_id`.

Given existing comments about short `L1-003` ids being easier for LLMs to reproduce, option 1 or 3 is likely safer than immediately replacing answer ids. The UI can still render `question_id` as canonical while showing `answer_node_id` as attached materialization.

## Important Risks

- Fresh decomposition does not currently persist question nodes until after the full in-memory tree is built and ids assigned. That means live per-layer question appearance requires either:
  - emitting provisional question-node events before final persistence, or
  - assigning deterministic ids earlier and persisting each subtree as it completes.
- If React renders q-hash nodes but the inspector only receives answer nodes, clicking will break or show “Node not found.”
- Matching answers to questions by `self_prompt == question` is useful for a first pass but can collide if duplicate/near-duplicate questions exist.
- Do not add a new user-facing table for this. `pyramid_question_nodes` already exists.
- Do not make “question nodes” a frontend-only visual illusion. They are persistent data and should be inspectable.

## Immediate Next Patch Checklist

1. Add backend IPC `pyramid_question_tree_state`.
2. Enrich question rows with attached answer node by best-effort `self_prompt` match.
3. Register IPC in `main.rs` invoke handler.
4. Extend `SurfaceNode` with optional question/answer fields.
5. Update `usePyramidData` to fetch and merge question nodes.
6. Update `chains/defaults/question.yaml` to declare question-node visualization via `viz` metadata, ideally `node_fill` plus a source option.
7. Update tooltip to show question first and answer summary second.
8. Add question drill fallback or command.
9. Update inspector details/response fallback to show the full record.
10. Run `npm run build`.
11. Run targeted Rust compile/test if backend IPC is added.
12. Sync runtime YAML if testing the installed app uses Application Support chain files.

## Subagent Work In Flight

Two high-powered explorer subagents were spawned at user request:

- One auditing the Pyramid Surface / YAML viz contract.
- One auditing the question-node backend/frontend data path.

They were instructed not to edit files. Use their results to refine the checklist above, not to restart the investigation from zero.

## Subagent Finding: Viz Contract Audit

The first subagent audit completed and confirmed the central diagnosis:

- The intended contract is already YAML-driven. Chain YAML owns `viz.type`; `useVizMapping.ts` maps active step name to a viz primitive; `PyramidSurface.tsx` passes that primitive and accumulated build state into the renderer.
- The current frontend only renders `SurfaceNode` / `SurfaceEdge` data from `pyramid_tree`, `pyramid_build_live_nodes`, and event-derived overlays. It does not consume question-tree artifacts.
- `question.yaml` currently declares `decompose` and `decompose_delta` as:

```yaml
viz:
  type: progress_only
```

That means the chain itself is telling the surface that question decomposition is non-structural. For the user’s desired behavior, this is wrong.

The audit’s recommended minimal direction:

1. Change the chain declaration first so decomposition declares structural visualization, probably `viz.type: node_fill` plus generic metadata such as `source: question_nodes` or `source: decomposed_tree` and `node_kind: question`.
2. Add a generic data/event contract for provisional structural nodes rather than a React special case for `decompose`.
3. Have `recursive_decompose` emit or publish question-tree nodes as visible nodes when `$decomposed_tree` is created/evolved.
4. Extend `SurfaceNode` with generic role/state fields, not step-specific frontend logic.
5. Keep `useVizMapping` generic: explicit YAML `viz.type` wins.

Compact verdict from the audit:

> The YAML-driven generic renderer architecture is mostly in place, but the current question pipeline declares decomposition as invisible progress. The smallest correct fix is to make question nodes a generic structural artifact emitted by the decomposition primitive and declared in YAML, then let answers/evidence attach later.

## Subagent Finding: Question Node Data Path Audit

The second subagent audit completed and confirmed the read-model gap.

### What Already Exists

- `QuestionTree` / `QuestionNode` are real internal models in `question_decomposition.rs`. They have deterministic `q-*` ids, question text, `about`, `creates`, `prompt_hint`, children, and leaf status.
- The question system can extract those nodes into per-layer `LayerQuestion`s for answering.
- The database already stores:
  - full tree blobs in `pyramid_question_tree`
  - individual rows in `pyramid_question_nodes`
- CRUD already exists to save, load, and reconstruct question nodes.
- The chain executor already runs `recursive_decompose`, persists the tree JSON, then runs `evidence_loop`.
- Answer nodes are normal `pyramid_nodes`: `answer_single_question` creates an `L{layer}-NNN` node and stores the question text in `self_prompt`.

### Why They Are Invisible

Question nodes live outside `pyramid_nodes`, while the current surface/inspector APIs only read `pyramid_nodes` / `live_pyramid_nodes`.

Specific read paths:

- `pyramid_tree` delegates to `query::get_tree`, which starts from `live_pyramid_nodes`.
- `pyramid_build_live_nodes` calls `get_build_live_nodes`, which reads `pyramid_nodes`.
- `pyramid_drill` calls `db::get_live_node`, so a `q-*` id returns `Node not found`.
- `NodeInspectorModal` assumes every clicked id exists in `allNodes` from the live-node API and then calls `pyramid_drill`.
- `LiveNodeInfo` has no `kind` / `source` / `object_kind`.
- `DetailsTab` and `ResponseTab` render answer-node fields but no question-node shape.

### Exact Minimal Patch Points

Backend, read-model first:

1. In `db.rs`, add helpers to list/get `pyramid_question_nodes` as surface rows and synthetic drill nodes.
2. Normalize question-node depth. Stored question depth is root-down; visual pyramid depth must be layer-up:

```text
visual_depth = max_question_depth - row.depth + 1
```

3. Add optional `node_kind` / `object_kind` to `TreeNode` and `LiveNodeInfo`, defaulting to normal answer nodes. Use `"question"` for synthetic question nodes.
4. Merge question-node projections into `query::get_tree`, ideally as a separate question overlay so answer hierarchy stays intact.
5. Merge question projections into `get_build_live_nodes`:

```text
node_id   = question_id
headline  = question
parent_id = question parent_id
children  = children_json
```

6. Make `query::drill` branch before `get_live_node`: if `node_id` resolves in `pyramid_question_nodes`, return a synthetic `DrillResult` with:
   - question metadata
   - child question nodes
   - gaps by `question_id`
   - optional linked answer info

Backend, answer linkage:

1. Current bridge: answer nodes only store question text in `self_prompt`; no stable `question_id` exists on the answer node.
2. Minimal no-schema bridge: match `pyramid_nodes.self_prompt = question` plus depth.
3. Durable fix: add `source_question_id` / `question_id` to answer persistence or a narrow read-through mapping.
4. In `chain_executor.rs`, if adding stable answer mapping, stamp it before `save_node`.
5. Also stamp evidence links with `build_id`; currently answer nodes get `build_id`, but `EvidenceLink.build_id` remains `None`.

Frontend:

1. Extend theatre and inspector types with optional question metadata / `object_kind`.
2. `NodeInspectorModal.tsx`: support question nodes that are not normal answer nodes. Header/depth/navigation should use synthetic drill data or `LiveNodeInfo.object_kind`.
3. `DetailsTab.tsx`: add a `Question` section with question, about, creates, prompt hint, child questions, and linked answer.
4. `ResponseTab.tsx`: for question nodes with no audit, show structured question data rather than treating the fallback as stored answer data.

### Risks Confirmed

- **Depth mismatch:** question-node DB depth is root-down; surface depth is layer-up.
- **No stable question-to-answer key:** `self_prompt` matching works as a bridge but is text-fragile.
- **Build scoping:** `save_question_node` and `save_question_tree` use legacy unscoped paths; `pyramid_question_nodes` primary key is `(slug, question_id)`, not `(slug, build_id, question_id)`.
- **Not truly incremental:** ids are assigned after the full tree is built; DB write happens after full decomposition, not after each LLM subtree.
- **Double-count risk:** adding question nodes directly into existing surfaces can double apparent node count unless `node_kind` is explicit and renderers distinguish question edges from answer/evidence edges.

## What Not To Do

- Do not hardcode `decompose` behavior in React.
- Do not create a new question-node table.
- Do not hide LLM calls as mechanical just because they are background.
- Do not treat answer nodes as replacing question nodes.
- Do not revert unrelated dirty files in this worktree.
- Do not broad-search the entire repo again unless a specific unknown blocks implementation.

## Compact Mental Model

Question pyramid nodes should be rendered as:

```text
Question identity:
  q-id, question, about, creates, prompt_hint, parent/children

Answer materialization:
  answer_node_id, headline, distilled, topics, narrative, quotes, entities

Evidence:
  KEEP / DISCONNECT / MISSING, weights, reasons, source ids

Provenance:
  prompts, raw responses, model/provider, tokens, latency, cache, build_id
```

The Pyramid Surface should show the question object first, then light it up as answer/evidence/provenance arrives.
