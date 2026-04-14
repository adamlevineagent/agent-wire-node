# Pyramid Surface — Polish Fixes

**Date:** 2026-04-13
**Context:** Post-Sprint-2 testing identified 4 UX issues that need systemic fixes.
**Audit:** 3 cycles applied. All critical/major findings corrected.

---

## Fix A: Chain-driven expected depths (nodes at top during webbing)

**Problem:** During early build phases, only L0 and bedrock exist. The layout computes L0's normalized depth as 1.0 (top of canvas) because it doesn't know higher layers are coming. When webbing starts, L0 nodes float at the top instead of near the bottom.

**Root cause:** The layout engine computes position from the data it has, but during builds the data is incomplete. The chain definition declares the expected shape — the layout should use it.

**Fix:** `computeLayout` accepts an optional `expectedMaxDepth` parameter. When provided, the depth range uses `Math.max(actualMaxDepth, expectedMaxDepth)` instead of just the actual max depth. This ensures L0 is positioned near the bottom from the start.

The expected max depth comes from the build's `$max_depth` configuration parameter — NOT from counting chain steps (which doesn't work for question pipelines where depth is dynamic). Extend `pyramid_get_build_chain` IPC to also return resolved build parameters including `max_depth`. The frontend reads it directly.

**Files:**
- `src/components/pyramid-surface/useUnifiedLayout.ts` — `computeLayout` accepts `expectedMaxDepth?: number`, uses `Math.max(actualMaxDepth, expectedMaxDepth)` for depth range
- `src/components/pyramid-surface/useVizMapping.ts` — read `max_depth` from chain response build params, return as `expectedMaxDepth`
- `src/components/pyramid-surface/usePyramidData.ts` — pass `expectedMaxDepth` through to `computeLayout`
- `src-tauri/src/main.rs` — extend `pyramid_get_build_chain` response to include `max_depth` from build config

**Acceptance:** During source_extract, L0 nodes appear near the bottom with empty bands above for L1/L2/apex. As higher layers fill in, the layout adjusts smoothly.

---

## Fix B: Chronicle as bottom overlay (not side squish)

**Problem:** Opening the chronicle takes horizontal space from the pyramid canvas, pushing the UI off-screen. Users can't close it because the close button scrolls out of view.

**Root cause:** The chronicle is a flex sibling that competes for horizontal space with the canvas.

**Fix:** The chronicle renders as a bottom panel overlaying the lower portion of the canvas. `position: absolute; bottom: 0` within `.ps-full` (which already has `position: relative`). Max-height: 40% of container. Scrollable. Semi-transparent background so the pyramid is visible behind it. Close button always visible at top-right of the panel.

Note: nodes beneath the overlay panel will be unclickable. Acceptable tradeoff — the top portion of the canvas (where apex and higher layers are) stays interactive.

**Files:**
- `src/components/pyramid-surface/PyramidSurface.tsx` — move chronicle inside the canvas container div with absolute positioning
- `src/styles/dashboard.css` — chronicle as absolute-positioned bottom overlay with semi-transparent background

**Acceptance:** Opening chronicle doesn't resize the canvas. The pyramid remains visible above the chronicle panel. Close button always accessible.

---

## Fix C: Wizard collapse when build starts

**Problem:** In AddWorkspace and AskQuestion flows, wizard chrome stays visible after a build starts, constraining the build view to a tiny area.

**Root cause:** Parent components don't yield their space when the build begins.

**Fix:** `PyramidTheatre` accepts a `requestFullScreen?: (active: boolean) => void` callback. When `isRunning` becomes true, it calls `requestFullScreen(true)`. When the build completes/fails/cancels, it calls `requestFullScreen(false)`.

**Corrected parent list (from audit cycle 1):**
- `AddWorkspace` — collapses wizard step bar + step content, shows minimal "Building: slug" header with "Back" button. Also handles `VineBuildProgress` for vine builds.
- `AskQuestion` — collapses dialog chrome when build starts
- `PyramidDashboard` — already switches to full-screen build view via `view === 'building'` conditional. No change needed.
- `PyramidFirstRun` — does not render `BuildProgress`/`PyramidTheatre` directly (embeds `AddWorkspace` which handles it). No change needed.

**Files:**
- `src/components/PyramidTheatre.tsx` — add `requestFullScreen` prop, call on build state changes
- `src/components/AddWorkspace.tsx` — implement collapse (wizard chrome hidden when requestFullScreen(true))
- `src/components/AskQuestion.tsx` — implement collapse
- `src/components/VineBuildProgress.tsx` — add same `requestFullScreen` prop for vine builds

**Acceptance:** Starting a build from AddWorkspace or AskQuestion gives the build view full screen. The wizard chrome collapses. A "Back" button remains to return to wizard state. User can resume the wizard view at any time.

---

## Fix D: Chronicle shows produced intelligence, not plumbing stats

**Problem:** The chronicle shows "LLM: gemma4:26b 5562tok 59115ms" — plumbing stats, not the intelligence that was produced.

**Root cause:** Events carry IDs and metadata but not the content that was produced. The content exists at the emission point in the Rust executor but isn't included in the event.

**Fix — maximal systemic:** The event stream becomes the content stream. Intelligence events carry what they produced, not just IDs.

**Key insight from audit cycle 2:** `LlmCallCompleted` fires from `llm.rs` BEFORE the response is parsed into a node — the headline doesn't exist yet at emission time. The fix: a NEW `NodeProduced` event emitted from `chain_dispatch.rs` AFTER the node is built. `LlmCallCompleted` stays as generic LLM telemetry.

**Rust changes:**

1. **New `NodeProduced` event** (emitted from `chain_dispatch.rs` after `build_node_from_output`):
   ```
   NodeProduced {
       slug: String,
       build_id: String,
       step_name: String,
       node_id: String,
       headline: String,
       depth: i64,
   }
   ```
   This is the PRIMARY intelligence event for extraction. Fires once per produced node, carrying the headline the LLM generated.

2. **Enrich `VerdictProduced`** (emitted from `evidence_answering.rs`):
   - Add `source_headline: Option<String>`, `target_headline: Option<String>`
   - At the emission site (line ~660), the source and target node data was loaded earlier for prompt construction. Build a headline lookup map from the `candidate_nodes` / `all_nodes` and use it at emission time.

3. **Enrich `EdgeCreated`** (emitted from `chain_executor.rs`):
   - Add `source_headline: Option<String>`, `target_headline: Option<String>`
   - The `headline_lookup` HashMap built from the nodes slice (around line 2301) is in scope. Use it to resolve IDs to headlines at emission time.

4. **`LlmCallCompleted` stays unchanged** — it fires at the right level (LLM telemetry) and the chronicle shows it as supplementary metadata when expanded.

**Frontend chronicle rendering (content-first):**

| Event Type | Headline |
|-----------|---------|
| `node_produced` | "Extracted: **{headline}** at L{depth}" |
| `verdict_produced` (with headlines) | "Evidence kept: **{source_headline}** supports **{target_headline}** (w={weight})" |
| `verdict_produced` (no headlines) | "Evidence {verdict}: {source_id} → {node_id} (w={weight})" (fallback) |
| `edge_created` (with headlines) | "Connected: **{source_headline}** ↔ **{target_headline}** at L{depth}" |
| `edge_created` (no headlines) | "Edge: {source_id} → {target_id} at L{depth}" (fallback) |
| `llm_call_completed` | "{step_name} — {model_id} {tokens}tok {latency}ms" (shown as expandable metadata) |
| `web_edge_completed` | "Discovered {edges_created} connections ({latency}ms)" |
| `chain_step_started` | "Beginning {step_name}..." |
| `reconciliation_emitted` | "Analysis: {central_count} central nodes, {orphan_count} unused" |

**Detail (expandable):** Model, tokens, latency, step_name, full node IDs, distilled text if available.

**Files:**
- `src-tauri/src/pyramid/event_bus.rs` — new `NodeProduced` variant + add optional headline fields to `VerdictProduced` and `EdgeCreated`
- `src-tauri/src/pyramid/chain_executor.rs` — emit `NodeProduced` after `build_node_from_output` at all 5 call sites (for_each container, split_merge, non-split, normal dispatch, parallel for_each). Also build node headline map from `nodes` Vec for `EdgeCreated` enrichment.
- `src-tauri/src/pyramid/evidence_answering.rs` — build headline lookup map from candidate_nodes, include in `VerdictProduced` emission
- `src-tauri/src/pyramid/llm.rs` — NO CHANGES (LlmCallCompleted stays as-is)
- `src/hooks/useBuildRowState.ts` — add `NodeProduced` to KnownTaggedKind, update types for enriched fields
- `src/components/pyramid-surface/useChronicleStream.ts` — new `node_produced` handler (primary intelligence entry), updated `verdict_produced`/`edge_created` handlers to use headline fields with fallback
- `src/components/pyramid-surface/usePyramidData.ts` — handle `node_produced` events to update buildVizState

**Acceptance:** During a build, the chronicle shows what intelligence PRODUCED — "Extracted: Agent Wire Compiler", "Evidence kept: X supports Y", "Connected: A ↔ B". LLM stats are expandable metadata. No frontend nodeMap needed — events carry their own content.
