# MPS: Live Pyramid Theatre

## Vision

Replace the current build progress view (colored grid squares + log panel) with a **live spatial visualization** where users watch their pyramid materialize in real-time. Every node is inspectable — click to see the prompt sent, the response received, and navigate the entire structure with bumpers.

Three layers of experience:
1. **The Stage** — spatial pyramid growing live on canvas
2. **The Inspector** — click any node for prompt/response modal with navigation
3. **The Timeline** — pipeline stage awareness bar

---

## Architecture

```
PyramidTheatre (replaces PyramidBuildViz via BuildProgress.tsx re-export)
 +-- PipelineTimeline (horizontal stage chips)
 |    +-- StageChip (one per pipeline phase)
 +-- LivePyramidStage (canvas-rendered spatial viz)
 |    +-- <canvas> (nodes + edges, animated)
 |    +-- LayerLabels (HTML overlay)
 |    +-- HoverTooltip
 +-- NodeInspectorModal (overlay, on node click)
 |    +-- ModalHeader (headline, depth badge, close)
 |    +-- NavigationBumpers (left/right siblings, up/down parent/child)
 |    +-- TabBar (Prompt | Response | Details)
 |    +-- PromptTab (system + user prompt)
 |    +-- ResponseTab (structured parsed view + raw toggle)
 |    +-- DetailsTab (evidence, gaps, web edges, metadata)
 +-- ActivityLog (collapsible bottom rail, extracted from PyramidBuildViz)
 +-- BuildControls (cancel, force-reset)
```

**Integration point:** `BuildProgress.tsx` currently re-exports `PyramidBuildViz`. To activate Theatre, swap the re-export to `PyramidTheatre`. This preserves the `onComplete`/`onClose`/`onRetry` contract used by PyramidDashboard, AddWorkspace, PyramidFirstRun, and AskQuestion.

---

## Data Model: New Table

```sql
CREATE TABLE IF NOT EXISTS pyramid_llm_audit (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    slug TEXT NOT NULL,
    build_id TEXT NOT NULL,
    node_id TEXT,                    -- NULL for non-node calls (pre-map, clustering)
    step_name TEXT NOT NULL,         -- "source_extract", "evidence_loop", etc.
    call_purpose TEXT NOT NULL,      -- "extract", "pre_map", "answer", "web", "synthesize"
    depth INTEGER,
    model TEXT NOT NULL,
    system_prompt TEXT NOT NULL,
    user_prompt TEXT NOT NULL,
    raw_response TEXT,               -- NULL while in-flight
    parsed_ok INTEGER DEFAULT 0,
    prompt_tokens INTEGER DEFAULT 0,
    completion_tokens INTEGER DEFAULT 0,
    latency_ms INTEGER,
    generation_id TEXT,              -- OpenRouter generation ID
    status TEXT NOT NULL DEFAULT 'pending',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    completed_at TEXT
);
```

**Relationship to `pyramid_cost_log`:** The existing `pyramid_cost_log` table already tracks model, tokens, costs, layer, step_name, generation_id, and latency_ms for aggregate cost reporting. `pyramid_llm_audit` is intentionally separate because it stores full prompt/response text (potentially large), is scoped to individual builds, and serves a different purpose (inspection vs. cost accounting). The cost_log continues to serve cost dashboards; the audit table serves the Inspector. Both are populated from `call_model_audited` — one insert each.

---

## Backend Changes

### 1. LLM Audit Hook (`llm.rs`)

New `call_model_audited()` wrapper around existing `call_model_unified()`:
- Inserts pending audit row BEFORE the LLM call
- Calls existing `call_model_unified` (signature: `config, system_prompt, user_prompt, temperature, max_tokens, response_format`)
- Updates row with response + metrics AFTER
- Also inserts into `pyramid_cost_log` (preserving existing cost tracking)
- Non-breaking: existing callers continue without audit

**Note:** `call_model_unified` already returns `LlmResponse { content, usage: TokenUsage { prompt_tokens, completion_tokens }, generation_id: Option<String> }`. The generation_id extraction from OpenRouter is already implemented — no new parsing needed.

```rust
pub struct AuditContext {
    pub conn: Arc<Mutex<Connection>>,
    pub slug: String,
    pub build_id: String,
    pub node_id: Option<String>,
    pub step_name: String,
    pub call_purpose: String,
    pub depth: Option<i64>,
}
```

### 2. Evidence Answering (`evidence_answering.rs`)

Four LLM call sites to migrate (all currently call `llm::call_model_unified` directly):

| Location | Function | Purpose | Line (approx) |
|----------|----------|---------|------|
| 1 | `pre_map_layer()` | "pre_map" | ~247 |
| 2 | Two-stage pre_map refinement (within `pre_map_layer` variant) | "pre_map_refinement" | ~404 |
| 3 | `answer_questions()` / `answer_single_question()` | "answer" | ~848 |
| 4 | `targeted_reexamination()` | "gap_answer" | ~1167 |

Each call currently follows the pattern:
```rust
let resp = llm::call_model_unified(llm_config, system, user, temp, max_tokens, None).await?;
```
Migrate to:
```rust
let resp = llm::call_model_audited(llm_config, system, user, temp, max_tokens, None, &audit_ctx).await?;
```

### 3. Chain Executor (`chain_executor.rs`)

Thread `Option<AuditContext>` through `execute_chain_from` (current signature already takes `layer_tx: Option<mpsc::Sender<LayerEvent>>`).

**New `LayerEvent` variant** (currently missing — must be added to the enum in `types.rs`):
```rust
NodeStarted { depth: i64, step_name: String, node_id: String, audit_id: Option<i64> }
```

Existing variants for reference: `Discovered`, `NodeCompleted`, `NodeFailed`, `LayerCompleted`, `StepStarted`, `Log`. The new `NodeStarted` fills the gap between `Discovered` (pre-estimated count) and `NodeCompleted`/`NodeFailed` (outcome), enabling the "pulsing in-flight node" UX.

### 4. Build Runner (`build_runner.rs`)

Thread audit connection through `run_build` and `run_build_from`. Current state threading pattern:
- `&PyramidState` — immutable config/reader/writer
- `mpsc::Sender<WriteOp>` — DB write channel
- `mpsc::Sender<BuildProgress>` — overall progress
- `mpsc::Sender<LayerEvent>` — layer visibility

Add: `Option<Arc<Mutex<Connection>>>` for audit writes (or bundle into `AuditContext`).

### 5. New IPC Commands (`main.rs`)

Follow existing pattern (e.g., `pyramid_build_status`, `pyramid_cost_summary`):

| Command | Returns | When |
|---------|---------|------|
| `pyramid_build_live_nodes` | All nodes in active build with parent_id/children | Polled every 3s during build |
| `pyramid_node_audit` | LLM audit records for a node | On inspector open |
| `pyramid_audit_by_id` | Single audit record | For in-flight prompt viewing |
| `pyramid_audit_cleanup` | Deletes non-latest build audits | Manual cleanup |

**Note on `pyramid_build_live_nodes`:** The existing `LayerProgress.nodes` field is null for layers >50 nodes. This new command must query the DB directly (not the in-memory `BuildLayerState`) to guarantee full node coverage for large pyramids.

---

## Frontend Components

### Polling: `useBuildPolling` hook

Extract polling logic from PyramidBuildViz's inline `useEffect` into a reusable hook:
```typescript
function useBuildPolling(slug: string): {
    status: BuildStatus | null;
    progress: BuildProgressV2 | null;
    liveNodes: LiveNode[] | null;  // from new IPC
    isActive: boolean;
}
```
This replaces the 40-line inline polling block in PyramidBuildViz and is consumed by PyramidTheatre.

### LivePyramidStage

Canvas-rendered spatial pyramid using incremental trapezoid band layout.

**Text engine: native `ctx.measureText()` + manual truncation.** This avoids adding `@chenglou/pretext` as a dependency. Canvas text handling:
- Node labels: `ctx.measureText()` for width, truncate with ellipsis if exceeding node radius
- Layer labels: static positioned text, no complex layout needed
- Hover tooltips: HTML overlay (single positioned div), not canvas-rendered — simpler and supports text selection

**Spatial layout:**
- Nodes grouped by depth, each depth gets a horizontal band
- Band width narrows as depth increases (pyramid shape)
- Y position: L0 at bottom, apex at top
- New nodes animate in: spring from parent position to computed slot (300ms)
- In-flight nodes: pulsing outlined circles (no fill)
- Complete nodes: solid filled circles
- Failed nodes: red X overlay
- Edges: quadratic bezier curves from parent to child
- Layer labels rendered as HTML overlay (positioned absolutely)

**Existing canvas infrastructure:** `src/components/pyramid-viz/useCanvasSetup.ts` and `usePyramidLayout.ts` provide canvas DPI/resize handling and layout computation. Reuse where applicable.

Node count handling:
- ≤200 nodes: individual circles with truncated labels, all clickable
- 200-500: L0 uses 2-row stagger
- 500+: L0 uses summary rectangles (10 nodes per rect)

### NodeInspectorModal

```
+--------------------------------------------------+
| [<] [>] siblings    NODE HEADLINE     L2    [X]   |
| [^] parent  [v] child                            |
+--------------------------------------------------+
| [Prompt] [Response] [Details]                     |
+--------------------------------------------------+
|                                                    |
|  Scrollable tab content                           |
|                                                    |
+--------------------------------------------------+
| Model: mercury-2 | 1,247 in / 892 out | 3.2s     |
+--------------------------------------------------+
```

**Prompt Tab:**
- System prompt: collapsible (collapsed by default)
- User prompt: full display, monospace
- In-flight: shows prompt, response area = "Waiting for LLM..."

**Response Tab:**
- Structured view (default): headline, distilled, topics, verdicts, corrections
- Raw toggle: syntax-highlighted JSON
- In-flight: spinner

**Details Tab:**
- Evidence links with KEEP/DISCONNECT verdicts
- Gaps list
- Children (clickable, navigates inspector)
- Web edges
- Metadata: model, tokens, latency

**Navigation Bumpers:**
- `[<]` `[>]` — siblings (same parent, same depth). Arrow keys L/R.
- `[^]` — parent. Arrow key Up.
- `[v]` — child (dropdown if multiple). Arrow key Down.

### PipelineTimeline

Horizontal bar of chips, one per pipeline phase:

```
[Load State] [Extract] [L0 Web] [Refresh] [Enhance Q] [Decompose] [Schema] [Evidence] [Gaps] [L1 Web] [L2 Web]
```

Chip states: pending (gray) → active (pulsing accent) → complete (solid + check) → skipped (dim + strikethrough)

Driven by `current_step` from `BuildProgressV2` and `StepStarted` / `LayerCompleted` events.

---

## Polling Strategy

| Endpoint | Interval | Notes |
|----------|----------|-------|
| `pyramid_build_status` | 2s | Existing, unchanged |
| `pyramid_build_progress_v2` | 2s | Existing, unchanged |
| `pyramid_build_live_nodes` | 3s | New, queries DB directly (not in-memory LayerProgress) |
| `pyramid_node_audit` | On-demand | When inspector opens |
| `pyramid_drill` | On-demand | When inspector opens |

Canvas only redraws when state changes. When all animations settle, RAF loop stops.

---

## Implementation Phases

### Phase 1: Audit Trail Infrastructure (Backend)
- Add `pyramid_llm_audit` table in `db.rs`
- Create `call_model_audited()` wrapper in `llm.rs` (dual-writes to audit + cost_log)
- Add `NodeStarted` variant to `LayerEvent` in `types.rs`
- Migrate 4 `evidence_answering.rs` call sites
- Thread `AuditContext` through `build_runner.rs` → `chain_executor.rs`
- Add 4 new IPC commands in `main.rs`
- **Test:** verify audit rows populated after a build

### Phase 2: Live Spatial Stage (Frontend)
- Create `src/components/theatre/` directory
- Extract polling into `useBuildPolling` hook
- Create `PyramidTheatre.tsx` (parent orchestrator, same props as PyramidBuildViz)
- Create `LivePyramidStage.tsx` (canvas, native measureText, reuse `useCanvasSetup`)
- Create `ActivityLog.tsx` (extracted from PyramidBuildViz lines 189-202)
- Swap `BuildProgress.tsx` re-export from PyramidBuildViz → PyramidTheatre
- **Test:** watch pyramid grow spatially during build

### Phase 3: Node Inspector Modal
- Create `NodeInspectorModal.tsx` with tabs
- Create `PromptTab.tsx`, `ResponseTab.tsx`, `DetailsTab.tsx`
- Fetch audit records via `pyramid_node_audit` IPC on open
- Structured response parsing for Response tab
- **Test:** click any node, see prompt/response

### Phase 4: Navigation + Timeline
- Create `NavigationBumpers.tsx` (sibling/parent/child)
- Add keyboard shortcuts (arrow keys)
- Create `PipelineTimeline.tsx` (stage chips driven by `current_step`)
- **Test:** navigate full pyramid tree via bumpers + keyboard

### Phase 5: Polish
- Migrate remaining LLM calls outside evidence_answering to audited
- System prompt deduplication (store once, reference by hash)
- Canvas optimization for 500+ node pyramids
- Audit cleanup command
- Remove old `pbv-*` CSS once PyramidBuildViz is fully retired

---

## Key Files

### Backend (modify)
| File | Change |
|------|--------|
| `src-tauri/src/pyramid/db.rs` | New `pyramid_llm_audit` table, CRUD functions |
| `src-tauri/src/pyramid/types.rs` | `LlmAuditRecord` struct, `NodeStarted` variant added to `LayerEvent` |
| `src-tauri/src/pyramid/llm.rs` | `AuditContext` struct, `call_model_audited()` wrapper |
| `src-tauri/src/pyramid/evidence_answering.rs` | Thread `AuditContext` through 4 call sites |
| `src-tauri/src/pyramid/build_runner.rs` | Thread audit connection through `run_build`/`run_build_from` |
| `src-tauri/src/pyramid/chain_executor.rs` | Thread audit, emit `NodeStarted` events |
| `src-tauri/src/main.rs` (or `routes.rs`) | 4 new IPC commands |

### Frontend (create)
| File | Purpose |
|------|---------|
| `src/components/PyramidTheatre.tsx` | Parent orchestrator (same props as PyramidBuildViz) |
| `src/components/theatre/LivePyramidStage.tsx` | Canvas spatial viz (native measureText, reuses useCanvasSetup) |
| `src/components/theatre/PipelineTimeline.tsx` | Stage chip bar |
| `src/components/theatre/NodeInspectorModal.tsx` | Modal with tabs |
| `src/components/theatre/PromptTab.tsx` | Prompt display |
| `src/components/theatre/ResponseTab.tsx` | Structured response |
| `src/components/theatre/DetailsTab.tsx` | Evidence, gaps, edges |
| `src/components/theatre/NavigationBumpers.tsx` | Sibling/parent/child nav |
| `src/components/theatre/ActivityLog.tsx` | Extracted from PyramidBuildViz |
| `src/components/theatre/types.ts` | Shared TypeScript types |

### Frontend (modify)
| File | Change |
|------|---------|
| `src/components/BuildProgress.tsx` | Swap re-export: PyramidBuildViz → PyramidTheatre |
| `src/hooks/useBuildPolling.ts` | New hook extracted from PyramidBuildViz inline polling |
