# DADBEAR: Pipeline Completion + UI Refactor + FAQ Accretion

## Status: Plan Audit Complete (Informed + Discovery) — Ready for Implementation

### What's Already Shipped (commit 278a6cd)
- Startup reconciliation (disk vs file_hashes comparison on boot)
- Backfill file_hashes from chunk headers for pre-tracking pyramids
- All propagation via evidence KEEP links (not parent_id)
- Absolute path normalization everywhere + startup migration
- Delta writing to upstream threads via evidence KEEP links
- Tree building + apex filtering via evidence links for all content types
- Node counts via pyramid_build_live_nodes (all live nodes, not tree-reachable)
- Stale check log extended: new_file/deleted/renamed with colored badges
- L0 sweep includes reconciliation for new file discovery

---

## Phase 1: This Build

### Fix 2: Skipped node cleanup

**Problem:** Nodes that don't map to live threads get silently abandoned in stale checks.

**Scope (narrowed per audit):** Focus on case (a) — evidence links pointing to superseded nodes. The `live_pyramid_evidence` view already filters these, so "cleanup" means periodic pruning of the raw `pyramid_evidence` table to remove links where both sides are superseded. Case (b) — live nodes with no thread — deferred pending investigation of why `ensure_thread_target` returns None for non-standard ID formats.

**Observability:** Add stale value 5 ("skipped") to distinguish "checked and not stale" from "skipped due to structural reason" in the stale check log. **Write-site:** In `stale_helpers_upper.rs`, the `SkippedNode`-to-`StaleCheckResult` conversion (lines 576-596) currently sets `stale: false` (value 0). Change to `stale: 5` for skipped nodes so `log_stale_results` writes the correct value. Add `stale-skipped` badge (gray) to frontend. Add `5 => "skipped"` to `db.rs` value mapping.

**Files:** `stale_helpers_upper.rs` (write stale=5 for skipped), `db.rs` (value mapping + pruning query), `DADBEARPanel.tsx` (badge), `dashboard.css` (badge style)

### Schema prep: faq_synthesis_pass column + backfill

**Discovery audit correction:** The acute FAQ path IS already wired at `routes.rs:2816-2818`. No wiring fix needed.

**What IS needed for Phase 2:** Add `faq_synthesis_pass TEXT DEFAULT NULL` column to `pyramid_annotations` (migration via ALTER TABLE). Backfill: set `faq_synthesis_pass = 'ACUTE'` on all existing annotations that have `question_context IS NOT NULL` so passive accretion doesn't double-count them.

**Files:** `db.rs` (migration + backfill query)

### UI Refactor: DADBEAR Panel Layout

#### Bar chart labels — node counts + debounce ratio

Labels show **total live nodes** normally (699, 40, 5, 1). When mutations are pending, switch to **pending/total** (e.g., "4/699"). Remove separate "L3: 1 L2: 5..." line.

Data sources: `nodeCounts` from `pyramid_build_live_nodes` (already loaded), `status.pending_mutations_by_layer` (already loaded). Both available in render context.

#### Evidence density module — new sidebar card

New card showing evidence interconnection with scrollable node list ranked by link count.

**Data contract:**
```typescript
interface EvidenceDensity {
  per_layer: { layer: number; keep_count: number }[];
  top_nodes: { node_id: string; headline: string; depth: number; inbound_links: number }[];
}
```

**Query:** Use `live_pyramid_evidence` view JOINed to `live_pyramid_nodes` for depth (the view has no depth column). Per-layer: count KEEP links grouped by target node's depth. Per-node: count inbound KEEP links per target, ORDER BY count DESC LIMIT 50.

```sql
-- Per layer
SELECT pn.depth, COUNT(*) as keep_count
FROM live_pyramid_evidence pe
JOIN live_pyramid_nodes pn ON pe.target_node_id = pn.id AND pe.slug = pn.slug
WHERE pe.slug = ?1
GROUP BY pn.depth;

-- Top nodes by inbound links
SELECT pe.target_node_id, pn.headline, pn.depth, COUNT(*) as inbound_links
FROM live_pyramid_evidence pe
JOIN live_pyramid_nodes pn ON pe.target_node_id = pn.id AND pe.slug = pn.slug
WHERE pe.slug = ?1
GROUP BY pe.target_node_id ORDER BY inbound_links DESC LIMIT 50;
```

**New Tauri command:** `pyramid_evidence_density` in `main.rs`, backed by query in `db.rs`. **Not polled on 10s interval** — loaded once on mount and on manual refresh to avoid performance concerns.

**Navigation:** Clicking a node navigates to the pyramid visualization (calls `onNavigateToSlug` prop).

#### Stale log + cost — full-width bottom section

Move from sidebar to full-width section below the 2-column layout. CSS grid: top row = viz + sidebar, bottom row = full-width audit trail (stale log | cost observatory | contributions side by side).

**Files:** `DADBEARPanel.tsx` (layout restructure), `dashboard.css` (grid layout)

---

## Phase 2: FAQ Knowledge Accretion (after Phase 1 ships)

### Overview

Extend DADBEAR to drive passive FAQ accretion from annotations. Depends on Phase 1 (acute path wired, schema migration applied).

### How It Plugs Into DADBEAR

```
Annotation saved → WAL entry: mutation_type = "faq_accretion", target_ref = annotation ID
  → stale engine picks up in poll loop
  → threshold check inside dispatch handler (not at routing level)
  → if threshold met: drain → cluster → synthesize → create/update FAQs
  → if FAQ count crosses category threshold: emit faq_category_stale
```

**Audit fix — target_ref:** Use annotation ID (unique per annotation) to avoid dedup collisions in the WAL.

**Audit fix — dual-path:** Filter out annotations where `faq_synthesis_pass IS NOT NULL` (already processed by acute path or previous synthesis pass).

### Clustering Approach (per audit recommendation)

**Node-ID-based grouping** as first pass:
1. Group annotations by their `node_id`
2. Merge groups whose nodes share a parent (sibling nodes)
3. Merge groups whose nodes share topic names
4. Result: clusters of topically related annotations without LLM calls or embeddings

### Schema Changes

1. `faq_synthesis_pass TEXT DEFAULT NULL` on `pyramid_annotations` (migration via ALTER TABLE with error swallowing, matching existing pattern at db.rs:722)
2. New `pyramid_faq_synthesis_log` table (id, slug, annotation_count, clusters_found, faqs_created, faqs_updated, triggers_generated, model, input_tokens, output_tokens, elapsed_seconds, created_at)

### Core Functions (faq.rs)

All use `db_path: &str` pattern (matching existing dispatch functions, NOT `Arc<Mutex<Connection>>`).

1. `run_synthesis_pass()` — Load unprocessed annotations → cluster → LLM synthesis → create/update FAQs → mark processed → log to synthesis_log → emit category mutations if threshold crossed
2. `count_unprocessed_annotations()` — Simple COUNT query
3. `generate_match_surface()` — LLM call to expand FAQ match triggers

### Stale Engine Integration

Add `faq_accretion` to mutation type routing in `drain_and_dispatch` (line ~749). **Sequential with other drain_and_dispatch changes** to avoid merge conflicts.

**DEPLOYMENT ORDER CONSTRAINT:** The `faq_accretion` routing in `drain_and_dispatch` MUST ship BEFORE the annotation hook that emits `faq_accretion` mutations. If the hook ships first, unknown mutation types fall through to `node_stale` (line 760-762), triggering full stale checks instead of FAQ synthesis. Both changes go in the same commit.

**Dispatch handler:** `faq_accretion` batches call `run_synthesis_pass()` via `spawn_blocking` (matching pattern of other dispatch handlers). Acquires semaphore. Threshold check inside the handler — if `count_unprocessed_annotations()` < threshold, no-op. Accepts `db_path: &str` (not `Arc<Mutex<Connection>>`), matching all other dispatch functions.

### Configuration (Tier2Config)

- `faq_accretion_threshold: usize` (default: 10)
- `faq_accretion_debounce_mins: i32` (default: 5)
- `faq_match_surface_size: usize` (default: 10)

### Open Questions (resolved per audit)

- Q1: Threshold = "10 annotations OR 24h since last synthesis, whichever comes first"
- Q2: Annotations with question_context weighted higher in synthesis
- Q3: Yes, FAQ accretion shows as own phase ("faq_accretion" in current_phase Arc)

### CLI / MCP

- CLI: `faq-synthesize <slug>` command (manual trigger, skips threshold)
- MCP: `pyramid_faq_synthesize` tool
- HTTP: `POST /pyramid/:slug/faq/synthesize` (local auth only)

---

## Out of Scope: Build Pipeline Gaps (answer_single_question overflow, crash-safety)

Discovery audit noted that the gap report (docs/plans/gap-report-incremental-save-and-batching.md) documents critical build pipeline issues (token overflow in answer_single_question, crash-safety for evidence_loop, silent write drops). These are real but are build-time issues, not DADBEAR/stale-time issues. They belong in a separate plan focused on the build pipeline. This plan covers DADBEAR operational pipeline + UI + FAQ accretion only.

## Deferred: Evidence Re-evaluation for Disconnected L0s

**Problem:** When an L0 file changes but has no evidence KEEP links, the change dies at L0. No mechanism triggers re-evaluation of whether the L0 should now connect to L1 nodes.

**Why deferred:** Both auditors found this requires extracting the evidence evaluation logic from the chain executor (`evidence_answering.rs:1123-1177`) into a standalone function callable from the stale engine. The chain executor's `pre_map_layer()` requires a `CandidateMap`, `ExecutionState`, and step output context that don't exist outside the chain. This is a significant refactoring effort.

**Approach when tackled:**
1. Extract core evidence matching logic from `pre_map_layer()` into a standalone `evaluate_evidence_candidates()`
2. New mutation type `evidence_reevaluation` with dedicated handler
3. Trigger: emitted from `execute_supersession()` when `resolve_evidence_targets_for_node_ids()` returns empty
4. Pre-filter: parse L0 topics JSON, SQL query L1 nodes with overlapping topics, pass only candidates to evaluation
5. Key constraint: use topic/entity matching to narrow scope, don't brute-force all L1s

**Separate workstream.** Will be planned and audited independently.

---

## Files to Modify

### Phase 1
| File | Changes |
|------|---------|
| `src-tauri/src/pyramid/stale_helpers_upper.rs` | Skipped node stale value 5 |
| `src-tauri/src/pyramid/db.rs` | Stale value 5 mapping; evidence density query; faq_synthesis_pass migration + backfill; evidence pruning |
| `src-tauri/src/main.rs` | pyramid_evidence_density Tauri command |
| `src/components/DADBEARPanel.tsx` | Full layout refactor: bar labels, evidence card, bottom section, skipped badge |
| `src/styles/dashboard.css` | Grid layout, skipped badge style |

### Phase 2
| File | Changes |
|------|---------|
| `src-tauri/src/pyramid/stale_engine.rs` | faq_accretion mutation routing in drain_and_dispatch |
| `src-tauri/src/pyramid/faq.rs` | run_synthesis_pass, count_unprocessed, generate_match_surface |
| `src-tauri/src/pyramid/routes.rs` | Annotation hook emits faq_accretion mutation; synthesize endpoint |
| `src-tauri/src/main.rs` | pyramid_faq_synthesize Tauri command |
| `mcp-server/src/cli.ts` | faq-synthesize command |
| `mcp-server/src/index.ts` | pyramid_faq_synthesize MCP tool |

## Verification

### Phase 1
1. `cargo build` — compiles
2. DADBEAR panel shows total node counts in bar labels (699, 40, 5, 1)
3. When pending mutations exist, labels show "4/699" format
4. Evidence density card shows KEEP link counts and scrollable ranked node list
5. Stale log is full-width, readable without truncation
6. Skipped nodes show gray "Skip" badge in stale log
7. Acute FAQ creation works when annotation has question_context + "Generalized understanding:"
8. `cargo tauri build` succeeds

### Phase 2
1. Submit 10+ annotations across different nodes
2. Trigger synthesis: `pyramid-cli faq-synthesize <slug>`
3. Verify FAQ entries created with match triggers
4. Wait for DADBEAR threshold → verify automatic synthesis fires
5. DADBEAR panel shows "faq_accretion" phase during synthesis
