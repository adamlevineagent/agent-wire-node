# Pyramid Surface

**Date:** 2026-04-13
**Scope:** Replace three disconnected visualizations (PyramidBuildViz, PyramidVisualization, ComposedView) with one unified Pyramid Surface component. Add full node inspection, navigable build chronicle, pyramid grid mission control, multi-window support, and rendering tier progression (Canvas2D → WebGL2 → WebGPU).
**Framing:** The build process generates ~45 event types across triage decisions, evidence verdicts, edge creation, cache hits, gap identification, and reconciliation. The user currently sees dots filling in a grid. Everything else is invisible.
**Audit:** Three audit cycles applied 2026-04-13. Cycle 1: 4 critical, 7 major — all corrected (Phase 2 split, contribution registration, visual encoding phase assignment, Chronicle persistence). Cycle 2: 2 critical, 4 major — all corrected (Pillar 37 violations, seed file paths, IPC shape, dependency graph). Cycle 3: clean.

---

## Current State

### Three Disconnected Visualizations

| Component | Tech | Shows | Where |
|-----------|------|-------|-------|
| `PyramidBuildViz.tsx` | JSX/CSS grid | Nodes completing in layers during build | Dashboard |
| `PyramidVisualization.tsx` | Canvas 2D | Finished pyramid structure + stale detection | DADBEAR / Oversight |
| `ComposedView.tsx` | Canvas 2D force-directed | Web of connections (evidence/child/web edges) | Question pyramids |

They share no code, no rendering path, no interaction model. The build viz polls `BuildProgressV2` (layer grid) and consumes 17 `TaggedKind` event types in a separate Step Timeline Panel. The structural viz consumes `pyramid_tree` once. The composed viz runs its own physics simulation.

### Rich Event Bus, Invisible to Structural Viz

The `BuildEventBus` already emits ~45 `TaggedKind` variants including:
- `CacheHit`, `CacheMiss`, `CacheHitVerificationFailed`
- `WebEdgeStarted`, `WebEdgeCompleted`
- `EvidenceProcessing`, `TriageDecision`
- `GapProcessing`, `ClusterAssignment`
- `LlmCallStarted`, `LlmCallCompleted`
- `ChainStepStarted`, `ChainStepFinished`
- `StepRetry`, `StepError`
- `NodeRerolled`, `CacheInvalidated`, `ManifestGenerated`
- `CostUpdate`, `DeltaLanded`, `SlopeChanged`

The Step Timeline Panel (`useBuildRowState.ts`) already handles 18 of these. But the structural pyramid viz (the layer grid) only consumes the polling system — it never sees the event bus. The two views sit side by side, disconnected.

### Node Inspector Shows ~30% of Node Data

`NodeInspectorModal.tsx` has three tabs (Prompt/Response/Details). The `pyramid_drill` IPC returns the full `PyramidNode` struct. The Details tab renders: self_prompt (L0 only), dead_ends, evidence links, gaps, children, web edges, metadata. It ignores: corrections, decisions, terms, narrative (multi-zoom), entities, key_quotes, transitions, weight, provisional, time_range, version history.

### Single Window, Mode-Based Routing

One Tauri window. `ModeRouter.tsx` switches on `state.activeMode` (pyramids, knowledge, settings, etc.). No URL-based routing. No multi-window support. Tauri 2 makes adding windows straightforward via `WebviewWindowBuilder`.

---

## Architecture Decisions

### AD-1: Viz behavior declared in chain YAML, not hardcoded in frontend

The frontend is a **renderer for viz primitives**, not a handler for specific event types. Each chain step's visualization is determined by its primitive type, with optional explicit overrides in the chain YAML.

**Viz primitive vocabulary:**

| Primitive | Renders As | Default For |
|-----------|-----------|-------------|
| `node_fill` | Dots appearing in a layer band | `for_each`, `pair_adjacent`, `single` |
| `edge_draw` | Lines forming between existing nodes | `web` |
| `cluster_form` | Nodes visually grouping, then parent appearing | `recursive_cluster` |
| `verdict_mark` | Decision indicators on nodes (keep/disconnect/missing) | `evidence_loop` |
| `progress_only` | Status text indicator, no structural change | Recipe primitives |

**Default inference:** The executor already emits `ChainStepStarted { step_name, primitive, depth }`. The frontend looks up the chain definition (loaded at build start via a new IPC) and maps `primitive` → viz behavior. No explicit `viz:` section needed for standard primitives.

**Explicit override for custom behavior:**
```yaml
- name: my_custom_analysis
  primitive: for_each
  viz:
    type: node_fill
    glow_on_complete: true
    label_format: "{headline}"
```

**Why:** New chain types from the Wire market or agents automatically get visualization. Viz behavior travels with the chain contribution. The frontend doesn't need code changes for new step types. Aligns with "can an agent improve this?"

### AD-2: One component, three deployment modes

The Pyramid Surface is one React component rendered at different scales:

| Mode | Where | What |
|------|-------|------|
| **Nested** (default) | Embedded in any page | Mini-viz of current pyramid. Compact. Click to expand or pop out. |
| **Popup** | Dedicated Tauri window | Full rendering. Navigate between pyramids. Grid View as home. |
| **Ticker** | Sidebar, DADBEAR, Dashboard | Row of dots per layer. Pulse with activity, fade over time. "Open Pyramid" button. |

All three are the same component at different detail levels — not three implementations. The `mode` prop controls rendering budget and interaction depth.

### AD-3: Miniature pyramid rendering for Grid View and Ticker

**Self-adjusting dot count:** Collapse algorithm driven by `pyramid_viz_config.rendering.max_dots_per_layer` (default from seed, not hardcoded):

```
max_dots = viz_config.rendering.max_dots_per_layer  // from contribution
rendered_count = min(actual_count, max_dots)
nodes_per_dot = ceil(actual_count / rendered_count)
```

Assignment is rotator-arm (round-robin): node i → dot (i % rendered_count). Activity intensity per dot = count of recent events across its assigned nodes. When `force_all_nodes: true`, collapse is disabled entirely.

**Visual treatment:**
- Dots are 1-4px depending on available space
- White normally; glows neon (cool-to-hot color ramp) based on activity recency
- Pack tighter as collapse ratio increases so pyramid shape always emerges cleanly
- Apex is always 1 dot (no collapse needed)

### AD-4: Viz config is a contribution

The rendering behavior is a `pyramid_viz_config` contribution — a YAML file that controls abstraction level, rendering tier, overlay defaults, and collapse thresholds. Registered as a schema type, visible and editable in the Tools tab, supersedable.

```yaml
schema_type: pyramid_viz_config
rendering:
  tier: auto              # auto | minimal | standard | rich
  max_dots_per_layer: 10  # collapse ceiling for miniature/collapsed rendering
  always_collapse: false  # true = always show minimap-style regardless of size
  force_all_nodes: false  # true = render every node, no collapse (supercomputer mode)
overlays:
  structure: true
  web_edges: true
  staleness: true
  provenance: true
  weight_intensity: true  # central node glow from weight maps
chronicle:
  show_mechanical_ops: false  # hide cache hits, step starts in chronicle
  auto_expand_decisions: true # intelligence decisions expand by default
ticker:
  enabled: true
  position: bottom        # bottom | top
window:
  auto_pop_on_build: true # auto-open pyramid window when a build starts
```

**Why a contribution:** Users can tune their viz experience and it persists, supersedes, versions, and eventually the steward can optimize it. A user with a supercomputer sets `force_all_nodes: true` and sees all 5,000 nodes with every connection. A user on a laptop sets `always_collapse: true` and gets the minimap-style abstracted view at full canvas size. The default (`auto`) uses the self-adjusting collapse algorithm.

**Per-pyramid override:** The contribution can be global (no slug) or per-pyramid (with slug). Per-pyramid overrides supersede global. A user might want full detail on their 50-node pyramid but collapsed view on the 5,000-node one.

### AD-5: Rendering tier progression (driven by viz config contribution)

Feature-detect and auto-select. User overrides via the viz config contribution, not a separate Settings toggle.

| Tier | Tech | What You Get |
|------|------|-------------|
| **Minimal** | DOM/CSS | Chronicle text view + CSS ticker. Accessibility. Oldest hardware. |
| **Standard** | Canvas 2D | Current-quality rendering. Smooth animations. Default fallback. |
| **Rich** | WebGPU (→ WebGL2 fallback) | GPU compute for force simulation. Smooth edge animation. Particle effects on stale propagation. |

**Detection:** Check `navigator.gpu` for WebGPU. Check `WebGL2RenderingContext` for WebGL2. Optionally benchmark (render 1000 circles, measure fps) to estimate GPU capability.

**WebGPU status:** Shipping in Safari 26+ on Apple platforms. WKWebView support should be feature-detected, not assumed. Pixi.js remains a valid intermediate step if WebGPU detection fails.

**Renderer abstraction:**
```
PyramidRenderer (interface)
  ├── render(nodes, edges, overlays)
  ├── hitTest(x, y) → node?
  ├── animate(operation)
  └── setTier(minimal | standard | rich)

Implementations:
  ├── DomRenderer (Minimal tier)
  ├── CanvasRenderer (Standard tier)
  └── GpuRenderer (Rich tier, WebGPU → WebGL2)
```

### AD-6: Chronicle as structured operation stream

The Step Timeline Panel already handles 17 event types. The Chronicle is its evolution — a scrollable, navigable, persistent stream where:

- **Intelligence decisions** (triage, verdicts, reconciliation, gap identification) are first-class, interactive, clickable records. They persist to the audit trail.
- **Mechanical operations** (cache hit, step start/complete) are non-interactive log lines. In-memory only.
- Every artifact (node, edge, verdict, gap) is clickable → navigates the structural viz + opens inspector.

The Event Ticker is the Chronicle in single-line mode — scrolls along the bottom of the visual pyramid, expands when no competing event, clickable.

### AD-7: Node Inspector as full record

Single scrollable view with collapsible sections (reuse `AccordionSection` from Ollama Phase 0). Replaces the three-tab modal.

**Categories:**

| Category | Fields |
|----------|--------|
| **Content** | headline, distilled, narrative (each zoom level), topics (with nested corrections/decisions), terms, key_quotes, dead_ends |
| **Structure** | children, evidence links (verdict + weight + reason), web edges (relationship + strength), transitions (prior/next question) |
| **Episodic** | entities (name, role, importance, liveness), time_range, weight, provisional, promoted_from |
| **Provenance** | self_prompt, build_id, created_at, version history (current_version + chain_phase), superseded_by |
| **LLM Record** | system prompt, user prompt, raw response, model, tokens, latency, cache_hit, step_name, generation_id |

Sections auto-collapse if empty. Expand all / collapse all toggle. Slides in as a panel alongside the pyramid, not as a modal overlay.

### AD-8: Chain definition loaded at build start for viz mapping

New IPC: `pyramid_get_build_chain(slug)` → returns the chain YAML (or compiled IR) for the active build. The frontend parses it to build a `step_name → viz_primitive` map. This map drives how `ChainStepStarted` events render in the structural view.

For completed pyramids (no active build), the chain definition is loaded from the slug's `chain_id` reference in the contribution store.

---

## Phase Plan

### Phase 1: Node Inspector Rewrite

**Goal:** Show everything the node contains. No Rust backend changes — frontend type fixes + new UI.

**What changes:**
- **TypeScript type update (critical):** The frontend `DrillResult` type in `PyramidVisualization.tsx` silently drops fields the Rust backend returns: narrative, entities, key_quotes, transitions, weight, provisional, promoted_from, time_range. Update the TypeScript interface to match the full Rust `PyramidNode` shape. The backend already returns all fields — the frontend just isn't typed to receive them.
- New `NodeInspectorPanel.tsx` replacing `NodeInspectorModal.tsx` — panel instead of modal, single scrollable view instead of tabs
- Reuse existing `AccordionSection` component for collapsible categories
- Render ALL fields from `DrillResult.node`: corrections, decisions, terms, narrative, entities, key_quotes, transitions, weight, time_range, provisional, promoted_from, version info
- Keep existing data fetching (`pyramid_node_audit` + `pyramid_drill`) — no Rust changes needed
- Arrow key navigation preserved (siblings, parent, child)

**Files:**
- `src/components/pyramid-viz/types.ts` or inline in `PyramidVisualization.tsx` — update `DrillResult` / `PyramidNode` TypeScript interface to include all fields from Rust
- `src/components/theatre/NodeInspectorPanel.tsx` — new component replacing NodeInspectorModal
- `src/components/theatre/ContentSection.tsx` — narrative, topics, terms, quotes, dead_ends
- `src/components/theatre/StructureSection.tsx` — children, evidence, web edges, transitions
- `src/components/theatre/EpisodicSection.tsx` — entities, time_range, weight, provisional
- `src/components/theatre/ProvenanceSection.tsx` — self_prompt, build_id, versions, created_at
- `src/components/theatre/LlmRecordSection.tsx` — prompts, response, model, tokens, cache
- `src/components/PyramidBuildViz.tsx` — wire to new inspector
- `src/components/PyramidVisualization.tsx` — wire to new inspector

**Acceptance:**
- Every non-null field on the node is visible in the inspector
- Sections collapse/expand; empty sections auto-collapsed
- Inspector slides in as panel, doesn't obscure pyramid
- Existing navigation (arrow keys, click) works

---

### Phase 2a: Viz Config Contribution

**Goal:** Register `pyramid_viz_config` as a first-class contribution type. Standalone backend + frontend task with no viz code.

**Rust backend:**
- `schema_definition` contribution YAML for `pyramid_viz_config` — defines the schema structure (rendering, overlays, chronicle, ticker sections)
- `schema_annotation` contribution YAML — provides field descriptions for the YAML-to-UI renderer in the Tools tab
- Seed contribution YAML with defaults (auto tier, auto collapse, all overlays on, `auto_pop_on_build: true`)
- Add `pyramid_viz_config` branch to `sync_config_to_operational` dispatcher in `config_contributions.rs`
- Frontend reload mechanism: `ConfigSynced` listener (same pattern as Ollama dispatch_policy) updates in-memory viz config when contribution changes
- No operational table needed — viz config is read directly from the active contribution YAML

**Frontend:**
- `useVizConfig.ts` hook — loads active `pyramid_viz_config` contribution on mount, subscribes to `ConfigSynced` events for live reload
- Visible and editable in Tools tab (schema_annotation drives the UI)
- Per-pyramid override support: contribution with slug supersedes global

**Files:**
- `src-tauri/src/pyramid/config_contributions.rs` — dispatcher branch + `seed_pyramid_viz_config()` function (existing pattern: seeds are inserted from Rust code at startup via `seed_config_contribution`, not standalone YAML files on disk)
- `src-tauri/src/pyramid/viz_config.rs` — schema_definition, schema_annotation, and seed YAML as embedded string constants (same pattern as existing schema types in `yaml_renderer.rs` / `schema_registry.rs`)
- `src/hooks/useVizConfig.ts` — config hook with ConfigSynced subscription

**Acceptance:**
- `pyramid_viz_config` appears in Tools tab as editable config
- Seed contribution created on first run
- Changes to config trigger ConfigSynced → live reload in any active viz
- Per-pyramid override works (slug-scoped contribution supersedes global)

---

### Phase 2b: Renderer Interface + PyramidSurface Shell

**Goal:** `PyramidSurface` component with pluggable renderer consuming static tree data. The structural foundation.

**What changes:**

**Renderer interface:**
- `PyramidRenderer` abstract interface: `render()`, `hitTest()`, `animate()`, `resize()`, `destroy()`, `setNodeEncoding(nodeId, brightness, saturation, borderThickness)`
- `CanvasRenderer` implementation — port existing Canvas 2D logic from PyramidVisualization
- `DomRenderer` implementation — simplified DOM-based rendering for Minimal tier
- Renderer reads `pyramid_viz_config` (from Phase 2a hook) to determine tier and rendering options

**PyramidSurface shell:**
- Single component with `mode` prop (full / nested / ticker)
- Consumes `pyramid_tree` for static rendering (build mode comes in Phase 2c)
- Trapezoid band layout from existing `usePyramidLayout.ts`
- Relationship density view as alternate layout mode (available on any pyramid type, not just questions)
- Bedrock layer: always one collapsed band (RD-5)

**Shared miniature renderer:**
- `MiniaturePyramid.tsx` — the self-adjusting dot renderer (AD-3 algorithm)
- Shared between Grid View (Phase 5), Minimap (Phase 4), and Ticker mode
- Built in this phase so downstream phases can consume it

**Files:**
- `src/components/pyramid-surface/PyramidSurface.tsx` — unified component shell
- `src/components/pyramid-surface/PyramidRenderer.ts` — interface definition
- `src/components/pyramid-surface/CanvasRenderer.ts` — Canvas 2D implementation
- `src/components/pyramid-surface/DomRenderer.ts` — DOM/CSS implementation
- `src/components/pyramid-surface/MiniaturePyramid.tsx` — self-adjusting dot renderer
- `src/components/pyramid-surface/useUnifiedLayout.ts` — trapezoid band layout (port from usePyramidLayout)
- `src/components/pyramid-surface/types.ts` — shared types

**Acceptance:**
- `PyramidSurface` renders a static pyramid on DADBEAR replacing old PyramidVisualization
- Structural layout matches existing quality (trapezoid bands, bedrock, edges)
- Renderer tier respects viz config (minimal/standard)
- MiniaturePyramid renders correctly at small sizes with self-adjusting collapse
- Relationship density view available as layout toggle
- `setNodeEncoding()` method implemented on CanvasRenderer and DomRenderer (can be no-op with logging until Phase 3b wires real data, but the plumbing must exist)

---

### Phase 2c: Unified Data Hook + Build Mode + Overlay Layers

**Goal:** PyramidSurface works during builds (replacing PyramidBuildViz) with composable overlay layers.

**Data sources unified:**
- `usePyramidData.ts` — single hook consuming:
  - Build mode: `BuildProgressV2` (polling) + `cross-build-event` (push via `useStepTimeline` / `useBuildRowState`)
  - Static mode: `pyramid_tree` + `pyramid_drill` on demand
  - Same component, same renderer, different data feeds

**Composable overlay layers:**
- Structure layer (nodes + parent→child edges) — always on
- Web layer (same-layer edges, cross-pyramid edges)
- Staleness layer (DADBEAR coloring, mutation indicators)
- Provenance layer (bedrock file connections)
- Build layer (progressive node fill, operation indicators) — auto-enabled during build
- Layer toggles driven by `pyramid_viz_config` overlay settings

**Files:**
- `src/components/pyramid-surface/usePyramidData.ts` — unified data hook
- `src/components/pyramid-surface/layers/StructureLayer.ts`
- `src/components/pyramid-surface/layers/WebLayer.ts`
- `src/components/pyramid-surface/layers/StalenessLayer.ts`
- `src/components/pyramid-surface/layers/ProvenanceLayer.ts`
- `src/components/pyramid-surface/layers/BuildLayer.ts`
- `src/components/PyramidBuildViz.tsx` — wire to new surface (parallel running period begins)

**Acceptance:**
- `PyramidSurface` renders on Dashboard during build, replacing PyramidBuildViz
- Layer toggles work (structure, web, staleness, provenance)
- Build mode shows progressive node fill with step indicators
- Event bus events drive build layer (existing 18 event types)
- Old components deprecated but not yet deleted (parallel running period)

---

### Phase 3a: Viz-from-YAML + Build Layer Enrichment

**Goal:** Chain YAML drives visualization. Build operations become visible in the structural view.

**Rust backend:**
- New IPC: `pyramid_get_build_chain(slug)` — returns the chain definition for the active or most recent build
- Extend `ChainStepStarted` event to include the step's viz primitive (inferred from the chain)
- New `TaggedKind` variants (relationship to existing events noted):
  - `EdgeCreated { slug, build_id, step_name, source_id, target_id, depth }` — **supplements** existing `WebEdgeStarted`/`WebEdgeCompleted` (which are per-batch start/end). EdgeCreated is per-edge granularity within the batch.
  - `VerdictProduced { slug, build_id, step_name, node_id, verdict, source_id, weight }` — **supplements** existing `EvidenceProcessing` (which is aggregate) and `TriageDecision` (which is per-question triage, not per-verdict). VerdictProduced fires per KEEP/DISCONNECT verdict during answering.
  - `NodeSkipped { slug, build_id, step_name, node_id, reason }` — genuinely new. Delta skip or sentinel match.
  - `ReconciliationEmitted { slug, build_id, orphan_count, central_count }` — genuinely new. Named to avoid collision with existing `ReconciliationResult` struct in types.rs.

**Frontend:**
- Load chain definition at build start via `pyramid_get_build_chain`
- Build `step_name → viz_primitive` map from chain definition
- When `ChainStepStarted` fires, look up viz primitive and activate the corresponding renderer:
  - `node_fill` → dots appearing in band (existing behavior, now data-driven)
  - `edge_draw` → lines animating between nodes during webbing
  - `cluster_form` → nodes visually grouping, parent node appearing
  - `verdict_mark` → KEEP/DISCONNECT/MISSING indicators on source nodes
  - `progress_only` → status text indicator
- Handle new event types in `useBuildRowState` reducer
- Chronicle distinguishes: `EvidenceProcessing` = "evidence answering started for N questions", `VerdictProduced` = individual verdict detail. `WebEdgeStarted` = "webbing batch began", `EdgeCreated` = individual edge. Both levels shown.

**Batching strategy for edge events:**
- Webbing on 200 nodes could produce thousands of edges
- Backend batches `EdgeCreated` events. Batch size is a chain YAML parameter (`viz.edge_batch_size`) with no hardcoded default. If unspecified, the backend emits only `WebEdgeStarted` and `WebEdgeCompleted` (existing behavior) and the frontend renders edges on completion rather than progressively.
- Frontend accumulates batched events and renders in next animation frame
- `WebEdgeCompleted` (existing event) serves as the final flush

**Files:**
- `src-tauri/src/main.rs` — new `pyramid_get_build_chain` IPC
- `src-tauri/src/pyramid/chain_executor.rs` — emit new event types during webbing, evidence, reconciliation
- `src-tauri/src/pyramid/event_bus.rs` — new `TaggedKind` variants
- `src-tauri/src/pyramid/evidence_answering.rs` — emit `VerdictProduced` per verdict (multiple per question — one per KEEP/DISCONNECT)
- `src/components/pyramid-surface/useVizMapping.ts` — chain YAML → viz primitive mapping
- `src/components/pyramid-surface/layers/BuildLayer.ts` — extended to handle all viz primitives
- `src/hooks/useBuildRowState.ts` — handle new event types

**Acceptance:**
- During webbing: edges visibly draw between nodes in the structural view
- During evidence loop: verdict indicators appear on source nodes
- During clustering: nodes visually group before parent appears
- Cache hits show as skip indicators (different node color/opacity)
- New chain step types with standard primitives get automatic visualization
- Custom `viz:` section in chain YAML overrides defaults

---

### Phase 3b: Visual Encoding Implementation

**Goal:** Three-axis node encoding (brightness, saturation, border thickness) and link importance propagation rendered in the structural view. See `pyramid-surface-visual-encoding.md` for full spec.

**Rust backend:**
- New IPC: `pyramid_get_visual_encoding_data(slug)` → returns:
  - `nodes: Vec<{ node_id, depth, aggregate_keep_weight, web_edge_count }>` — per-node summary from bulk queries against `pyramid_evidence` and `pyramid_web_edges`
  - `evidence_links: Vec<{ source_id, target_id, weight }>` — all KEEP evidence links with per-link weights, needed for client-side propagation computation. This is distinct from structural parent-child edges in `pyramid_tree` — evidence links carry KEEP weights from the question system.
  - `apex_ids: Vec<String>` — node IDs at maximum depth (propagation start points)
- No new tables. All data from existing `pyramid_evidence` (KEEP verdicts) and `pyramid_web_edges`.

**Frontend:**
- `useVisualEncoding.ts` hook:
  - Fetches bulk encoding data on mount (static mode) or recomputes on `LayerComplete` events (build mode)
  - Computes propagated importance via BFS from apex(es) in reverse-depth order
  - For multi-apex pyramids: each apex starts at 1.0, propagated importance can exceed 1.0 for nodes on multiple apex paths. Renderer normalizes (clamp or log-scale) for visual output.
  - Maps raw values through power curve ramp to [0, 1] range for each axis
- Renderer interface already has `setNodeEncoding()` from Phase 2b — wire it up
- Link rendering: `link_visual_intensity = link_weight × upstream_node.propagated_importance`
- Aggregation rules per zoom level (see visual encoding spec)

**Files:**
- `src-tauri/src/main.rs` — new `pyramid_get_visual_encoding_data` IPC
- `src-tauri/src/pyramid/query.rs` — bulk query function for aggregate weights + web edge counts
- `src/components/pyramid-surface/useVisualEncoding.ts` — computation hook
- `src/components/pyramid-surface/CanvasRenderer.ts` — apply three-axis encoding to node rendering
- `src/components/pyramid-surface/DomRenderer.ts` — CSS-based encoding (opacity, saturate(), border-width)

**Acceptance:**
- Central nodes are visibly brighter than peripheral nodes (Axis 1: brightness)
- Nodes on critical paths from apex are more vivid (Axis 2: saturation)
- Web hubs have thicker inward borders (Axis 3: border thickness)
- Evidence links show "rivers of importance" — thick bright lines from apex path, thin faint lines from periphery
- Multi-apex pyramids render correctly (importance from multiple apexes accumulates)
- Build mode: encoding updates progressively as layers complete

---

### Phase 4: Chronicle + News Ticker

**Goal:** Navigable text stream of build operations. Headline scroll on visual mode.

**Chronicle component:**
- Scrollable virtualized list of operations
- Intelligence decisions (triage, verdicts, gaps, reconciliation) are first-class interactive items with full detail
- Mechanical operations (cache hit, step complete) are compact non-interactive log lines
- Every artifact reference is clickable → highlights in structural viz + opens inspector
- Minimap in corner: simplified pyramid rendering (AD-3 algorithm) with "you are here" indicator showing the node/layer the chronicle is currently focused on

**Event Ticker:**
- Single-line headline scroll along bottom of visual mode
- Shows most recent operation headline
- Expands to multi-line when no competing events (>2s since last event)
- Clickable — navigates to the operation in the chronicle or highlights in the viz
- Format examples:
  - "L1-003: KEEP(3) DISCONNECT(1) MISSING(1) — 'How are tokens validated?'"
  - "Webbing L0: 47 edges created across 23 nodes"
  - "Triage: 12 answered, 3 deferred, 1 skipped"
  - "Cache: 8/12 L0 nodes served from cache"

**Persistence — no new tables (Law 3):**
- Intelligence decisions are **already persisted** in existing tables:
  - LLM calls: `pyramid_llm_audit`
  - Evidence verdicts: `pyramid_evidence`
  - Triage deferrals: `pyramid_deferred_questions`
  - Gap reports: `pyramid_gaps`
  - Demand signals: `pyramid_demand_signals`
- Reconciliation summaries (orphan counts, central nodes, weight maps) are currently computed and discarded. Persist as a contribution (`schema_type: reconciliation_result`, build_id scoped) so they're queryable post-build.
- Mechanical operations (cache hit, step start/complete) → in-memory event stream only, gone after build completes.
- Post-build chronicle review reads from the above existing tables. No new `pyramid_build_operations` table.

**Files:**
- `src/components/pyramid-surface/Chronicle.tsx` — main chronicle component
- `src/components/pyramid-surface/ChronicleItem.tsx` — per-operation renderer (interactive vs compact)
- `src/components/pyramid-surface/EventTicker.tsx` — headline scroll
- `src/components/pyramid-surface/Minimap.tsx` — simplified pyramid with "you are here"
- `src/components/pyramid-surface/useChronicleStream.ts` — consumes event bus, categorizes operations
- `src-tauri/src/pyramid/config_contributions.rs` — add `reconciliation_result` dispatcher branch (same pattern as Phase 2a's `pyramid_viz_config`), plus schema_definition and schema_annotation contributions for the type
- `src-tauri/src/pyramid/reconciliation.rs` — persist reconciliation summaries as contributions after evidence loop

**Acceptance:**
- Chronicle shows all build operations in chronological order
- Intelligence decisions are expandable with full detail
- Clicking an artifact navigates the structural viz
- Minimap updates as the user scrolls the chronicle
- News ticker scrolls operation headlines during build
- Post-build: chronicle can be reviewed (loads from persisted data)

---

### Phase 5: Grid View (Mission Control)

**Goal:** See all pyramids at a glance. Default view when opening Pyramid Window without a specific pyramid.

**Data source:**
- Reuse existing `pyramid_list_slugs()` → `Vec<SlugInfo>` (already has node_count, max_depth, content_type, last_built_at)
- Reuse existing `pyramid_get_publication_status()` → publication info per pyramid
- Reuse existing `pyramid_build_status()` polling for active builds
- `EnrichedSlug` type in `pyramid-types.ts` already merges these — Grid View consumes it directly

**Miniature pyramid rendering (AD-3):**
- Each pyramid rendered as a tiny self-adjusting pyramid (10 or fewer dots per layer)
- Activity glow: subscribe to `cross-build-event` for per-slug activity, apply cool-to-hot color ramp
- Click → navigate to Full View for that pyramid
- Sortable: by name, activity, node count, last build time

**Layout:**
- Responsive CSS grid
- Cards sized to fit miniature pyramid + slug name + key stats (node count, content type, build status)
- Active builds show animated dots
- Stale pyramids show stale indicator

**Files:**
- `src/components/pyramid-surface/GridView.tsx` — grid layout
- `src/components/pyramid-surface/PyramidCard.tsx` — per-pyramid miniature card
- `src/components/pyramid-surface/MiniaturePyramid.tsx` — the self-adjusting dot renderer (shared with Minimap)
- `src/components/pyramid-surface/useGridData.ts` — data hook (enriched slugs + activity events)

**Acceptance:**
- All pyramids visible as miniature cards in a scrollable grid
- Self-adjusting dot count (never more than 10 per layer)
- Activity glow shows which pyramids are currently active
- Click navigates to full pyramid view
- Building pyramids show animated progress
- Grid is responsive to window size

---

### Phase 6: Multi-Window + Nesting

**Goal:** Pyramid Surface can pop out as a dedicated OS-level window. Pages embed a nested mini-viz.

**Tauri window management:**
- New IPC: `pyramid_open_window(slug: Option<String>)` — creates a new Tauri window via `WebviewWindowBuilder`
  - If `slug` is Some: opens Full View for that pyramid
  - If `slug` is None: opens Grid View
  - Window label: `pyramid-surface-{uuid}` (allows multiple windows)
  - Loads same React frontend with window context (detected via Tauri window label)
- New IPC: `pyramid_close_window(label: String)` — closes a specific window
- Cross-window state: events flow through the Tauri event system (already global). Each window subscribes independently to `cross-build-event`.

**Frontend routing:**
- Detect window context on mount: if window label starts with `pyramid-surface-`, render `PyramidSurface` directly (skip ModeRouter)
- Query params or Tauri window data carry the initial slug
- Grid View is the default when no slug specified

**Nested mode:**
- `PyramidSurface mode="nested"` renders compact (ticker-style with dots) wherever embedded
- "Open Pyramid" button on nested view pops the full window
- DADBEAR page embeds nested mode for the current pyramid
- Dashboard embeds nested mode for actively building pyramids

**Event Ticker on pages:**
- Row of dots per layer that pulse with activity (CSS animation, no canvas)
- Color intensity fades over time (CSS transition)
- Prominent "Open" button → pops pyramid window
- Embedded in sidebar pyramid list items, DADBEAR header, Dashboard build section

**State model:** Multi-window means independent React trees with shared Rust state (`SharedState` in Tauri is shared across all windows) and shared Tauri events. Node inspector state is per-window (intentional — different windows can inspect different nodes). IPC calls work identically from any window.

**Auto-pop during builds:**
- When a build starts, check `pyramid_viz_config.auto_pop_on_build` (default: true)
- User can dismiss; build continues in background
- Nested ticker on Dashboard shows progress even when window dismissed

**Files:**
- `src-tauri/src/main.rs` — window management IPCs
- `src/main.tsx` — window context detection on mount
- `src/components/pyramid-surface/PyramidSurface.tsx` — mode="nested" rendering path
- `src/components/pyramid-surface/EventTicker.tsx` — compact pulsing dot row
- `src/components/Sidebar.tsx` — embed ticker per pyramid
- `src/components/DadbearMode.tsx` — embed nested surface, remove old PyramidVisualization

**Acceptance:**
- "Open Pyramid" button pops a new OS-level window
- Window shows Grid View (no slug) or Full View (with slug)
- Multiple pyramid windows can coexist
- Events flow to all open windows in real-time
- DADBEAR page shows nested mini-viz instead of the old full canvas
- Sidebar items show event ticker dots
- Build auto-pops window (configurable)

---

### Phase 7: Rich Rendering Tier (WebGPU/WebGL2)

**Goal:** GPU-accelerated rendering for users with capable hardware.

**GpuRenderer implementation:**
- Feature-detect WebGPU (`navigator.gpu`) → WebGL2 fallback → Canvas2D fallback
- Implement `PyramidRenderer` interface with GPU backend
- Node rendering: instanced circle geometry (one draw call for all nodes at a depth)
- Edge rendering: line geometry with smooth bezier interpolation
- Glow/bloom: post-processing shader pass
- Force simulation: WebGPU compute shader (if available) — O(n²) repulsion on GPU, 100x speedup for large webs

**Progressive enhancement:**
- Standard tier Canvas2D is always the baseline
- Rich tier adds: smooth edge animation during webbing, particle trails on stale propagation, glow effects, force sim at 60fps
- Auto-detection with user override via `pyramid_viz_config` contribution (AD-4)

**Performance budget:**
- Nested mode: <5ms per frame (minimal rendering)
- Standard mode: <16ms per frame (60fps)
- Rich mode: <8ms per frame (targeting 120fps on capable hardware)
- Grid View with 100+ pyramids: <16ms per frame total

**Files:**
- `src/components/pyramid-surface/GpuRenderer.ts` — WebGPU/WebGL2 renderer
- `src/components/pyramid-surface/shaders/` — WGSL shaders for node, edge, glow
- `src/components/pyramid-surface/useRenderTier.ts` — detection + preference hook

**Acceptance:**
- WebGPU detected and used when available
- Graceful fallback chain: WebGPU → WebGL2 → Canvas2D
- Edge animation smooth during webbing (thousands of edges at 60fps)
- Force simulation runs at 60fps for large webs
- User can override tier in Settings
- No visual regression on Standard tier

---

## Dependency Graph

```
Phase 1 (Node Inspector)
    ↓
Phase 2a (Viz Config Contribution)
    ↓
Phase 2b (Renderer + Surface Shell + MiniaturePyramid)
    ↓                        ↓
Phase 2c (Data + Build)    Phase 5 (Grid View)
    ↓
Phase 3a (Viz-from-YAML)    Phase 3b (Visual Encoding)
    ↓                            ↓
Phase 4 (Chronicle + Ticker)
                              Phase 6 (Multi-Window)
                                   ↓
                              Phase 7 (Rich Rendering)
```

Phase 1 is independent and immediately shippable. Phase 2a-2b-2c are sequential (each builds on the prior). Phase 2b produces `MiniaturePyramid.tsx` which is shared by Phase 4 (Minimap) and Phase 5 (Grid View). **Phase 5 depends on 2b (not 2c)** — Grid View uses MiniaturePyramid + existing IPC data, doesn't need build-mode data hook or overlays. Phase 5 can run in parallel with 2c. Phases 3a and 3b are independent of each other (both depend on 2c). Phase 4 depends on 3a (viz primitives drive chronicle items). Phase 6 needs Phase 5 (Grid View as default). Phase 7 is progressive enhancement.

Critical path: Phase 1 → 2a → 2b → 2c → 3a → Phase 4.

---

## Intersection with Ollama Daemon Control Plane

The Ollama plan (Phases 0-6, currently in progress) intersects at:

| Shared Surface | How |
|---|---|
| `AccordionSection` component (Ollama Phase 0) | Node Inspector reuses for collapsible sections |
| `BuildEventBus` + `TaggedKind` (event_bus.rs) | Our new event variants added alongside Ollama's `OllamaPull` |
| `ConfigSynced` listener pattern (Ollama Phase 1) | Rendering tier preference uses same contribution → live-reload pattern |
| `__ollama__` slug convention | Non-pyramid events follow same reserved-slug pattern |

No blockers. The two initiatives touch different files and different concerns. The shared event bus is additive on both sides.

---

## Existing Code to Reuse

| What | Where | Reuse For |
|------|-------|-----------|
| `useBuildRowState.ts` (18 event handlers) | `src/hooks/` | Chronicle event consumption — extend, don't replace |
| `useStepTimeline.ts` (seeds from cache, wires event listener) | `src/hooks/` | Chronicle data source — seeds + subscribes to cross-build-event |
| `usePyramidLayout.ts` (trapezoid bands) | `src/components/pyramid-viz/` | Structural layout in unified surface |
| `ComposedView.tsx` (force sim, edge drawing) | `src/components/pyramid-viz/` | Relationship density view algorithm reference |
| `useCanvasSetup.ts` (DPI scaling) | `src/components/pyramid-viz/` | Canvas2D renderer setup |
| `AccordionSection.tsx` | `src/components/` | Node Inspector collapsible sections |
| `EnrichedSlug` type | `src/components/pyramid-types.ts` | Grid View data model |
| `cross_pyramid_router.rs` | Rust | Automatic Tauri event forwarding (no changes needed) |
| `pyramid_list_slugs` + `pyramid_get_publication_status` | Rust `main.rs` | Grid View data source |
| `pyramid_drill` + `pyramid_node_audit` | Rust `main.rs` / `query.rs` | Node Inspector data (already returns full node) |
| `ConfigSynced` listener pattern | Rust `main.rs` | Viz config live-reload (same pattern as dispatch_policy) |

---

## What Gets Deleted

After Phase 6 stabilizes and the parallel running period confirms no regressions:

- `src/components/PyramidBuildViz.tsx` — replaced by PyramidSurface in build mode
- `src/components/PyramidVisualization.tsx` — replaced by PyramidSurface in static mode
- `src/components/pyramid-viz/ComposedView.tsx` — replaced by PyramidSurface web layer + force layout
- `src/components/theatre/NodeInspectorModal.tsx` — replaced by NodeInspectorPanel
- `src/components/theatre/PromptTab.tsx` — merged into LlmRecordSection
- `src/components/theatre/ResponseTab.tsx` — merged into ContentSection
- `src/components/theatre/DetailsTab.tsx` — distributed across all sections

---

## Resolved Decisions

### RD-1: Reconciliation data is first-class and visual

Reconciliation outputs (orphans, central nodes, weight maps) persist and are visually represented:

- **Orphans** — nodes created but not part of the final evidence structure. Surfaced as an "unused nodes" view accessible per pyramid. Not shown in the main structural viz (they'd clutter it), but available as an overlay or side panel for users who want to audit coverage gaps.
- **Central nodes** — heavily-cited, high-weight source nodes. Visually distinguished in the structural viz: in Rich rendering tier, central nodes glow brighter / larger based on aggregate weight. In Standard tier, distinct color intensity. This lets the user see at a glance what the load-bearing knowledge is.
- **Weight maps** — aggregate citation weight per source node. Drives the visual intensity of nodes in the structural view. Higher weight = more prominent rendering. This is the data-driven version of "which nodes matter most."

Persistence: reconciliation results stored per-build (build_id scoped) as contributions (`schema_type: reconciliation_result`). Weight maps drive node rendering intensity. Central node status is a derived property — the LLM or reconciliation logic determines which nodes are central based on the evidence graph, not a hardcoded threshold. Orphan status is the inverse (zero evidence references).

### RD-2: DADBEAR scope

DADBEAR becomes the deep-dive control panel for a specific pyramid: event ticker (nested surface), staleness controls, build history, cost tracking, recovery tools. Rich visualization moves to the Pyramid Surface. Scope can expand post-ship as needed.

### RD-3: Force-directed layout is a mode on any pyramid, redesigned

The current force-directed view (ComposedView) is too cramped and shows no meaningful data. The replacement is a **relationship density view** available on any pyramid type (not just questions):

- More like a weighted word cloud than a node graph
- Bond tightness weighted by actual relationship strength (web edge relevance, evidence weight)
- Spacing determined by semantic distance — tightly related nodes cluster naturally, loosely related ones drift apart
- Node sizing driven by weight maps (RD-1) — central nodes are larger
- Available as a layout toggle on the Pyramid Surface, not a separate component
- In Rich tier: smooth force simulation on GPU. In Standard tier: pre-computed layout, static.

### RD-4: Unknown viz primitives fallback

Wire market chains that declare an unrecognized viz primitive fall back to `progress_only` with a text label. Sufficient for now — the five core primitives cover all current and foreseeable chain step types.

### RD-5: Bedrock is always one collapsed band

Regardless of source material (files, docs, other pyramids via vine, chunks of larger documents), bedrock renders as a single band at the bottom of the pyramid. Same self-adjusting dot rules as every other layer. In the miniature, bedrock is just "the foundation." In the full view, bedrock dots expand to show their actual source (file path, vine reference, etc.). Clicking a bedrock dot that represents another pyramid navigates to that pyramid's surface.

## Resolved — Visual Encoding (see `pyramid-surface-visual-encoding.md`)

Three-axis node encoding system, link importance propagation, and aggregation rules are fully specified in the companion doc.

**Summary:**
- **Brightness** = direct citation (aggregate KEEP weight)
- **Color saturation** = propagated importance (apex importance flowing down through evidence links, attenuated by link weight per hop)
- **Border thickness (inward)** = lateral connectivity (web edge count)
- **Link intensity** = `link_weight × upstream_node.propagated_importance` — "rivers of importance" visible from apex to source material
- All three axes use power curve ramp (most visual range in top quartile)
- Aggregation rules for zoom levels and miniature rendering
- Per-rendering-tier adaptation (DOM/Canvas/GPU)
- Relationship density view uses same encoding with size replacing brightness

## Open Items

1. **Power curve exponent:** The visual encoding spec says "power curve" but doesn't pin the exponent. Gamma 2.2 (standard display) is a reasonable starting point but needs tuning against real pyramid data during implementation. Taste question for Adam.
