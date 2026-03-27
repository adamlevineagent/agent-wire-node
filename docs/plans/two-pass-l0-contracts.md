# Two-Pass L0 Architecture: Contracts

## Overview

Two-pass L0 extraction separates generic understanding from question-shaped extraction:
- **Canonical L0**: Generic, comprehensive, question-independent. Done once per corpus.
- **Question L0**: Question-shaped, built FROM canonical L0. Done per question build.

## Node Identity

Both canonical and question L0 nodes live in `pyramid_nodes` with `depth = 0`:
- **Canonical L0 IDs**: `C-L0-{index:03}` (e.g., `C-L0-005`) — deterministic, matches source file order
- **Question L0 IDs**: `L0-{uuid}` (e.g., `L0-a1b2c3d4-...`) — generated per question build
- **Canonical L0 `self_prompt`**: `NULL` or empty string (no question bias)
- **Question L0 `self_prompt`**: The question text that shaped the extraction

## DB Helpers Contract (WS-A)

### New functions in `db.rs`:

```rust
/// Check if canonical L0 exists for a slug (any node matching C-L0-% pattern)
pub fn has_canonical_l0(conn: &Connection, slug: &str) -> Result<bool>

/// Get all canonical L0 nodes for a slug
pub fn get_canonical_l0_nodes(conn: &Connection, slug: &str) -> Result<Vec<PyramidNode>>

/// Build a summary of canonical L0 for decomposition context
/// Returns: Vec of (node_id, headline, distilled_truncated_to_300_chars)
pub fn get_canonical_l0_summaries(conn: &Connection, slug: &str) -> Result<Vec<(String, String, String)>>

/// Delete only canonical L0 nodes (for re-extraction when source files change)
pub fn clear_canonical_l0(conn: &Connection, slug: &str) -> Result<usize>

/// Delete only question L0 nodes (for rebuild with different question)
pub fn clear_question_l0(conn: &Connection, slug: &str) -> Result<usize>
```

All functions use SQL WHERE clauses on node ID prefix:
- Canonical: `id LIKE 'C-L0-%'`
- Question: `id LIKE 'L0-%' AND id NOT LIKE 'C-L0-%'`

### No schema changes required.

## Canonical L0 Extraction Contract (WS-A)

### New module: `canonical_l0.rs`

```rust
/// Extract canonical L0 from source material.
/// Uses the existing chain executor with a GENERIC (non-question-shaped) extraction prompt.
/// The prompt says: "Describe what this file/document contains comprehensively.
/// Cover all major concepts, systems, decisions, and relationships."
///
/// Returns the count of canonical L0 nodes created.
pub async fn extract_canonical_l0(
    state: &Arc<PyramidState>,
    slug: &str,
    cancel: &CancellationToken,
    progress_tx: Option<tokio::sync::watch::Sender<BuildProgress>>,
) -> Result<i32>
```

**Behavior:**
1. Check if canonical L0 already exists via `has_canonical_l0()` — if yes, return early (reuse)
2. Build a generic extraction ExecutionPlan (uses defaults_adapter or a minimal plan)
3. The extraction prompt is NOT question-shaped — it's "what's in this file?"
4. Node IDs use pattern `C-L0-{index:03}`
5. Nodes are saved with `self_prompt = ""` (empty, not null — for serde compat)
6. File hashes are updated in `pyramid_file_hashes` referencing `C-L0-*` node IDs

**Model selection:** Mercury-2 for all extraction calls (small context, fast).

## Question L0 Contract (WS-B)

### New module: `question_l0.rs`

```rust
/// Generate question-shaped L0 nodes from canonical L0 nodes.
/// Reads canonical L0 summaries, applies question + audience framing,
/// and produces question-L0 nodes that feed into the evidence loop.
///
/// This is an LLM pass that reads canonical L0 distilled text (not raw files)
/// and reshapes it for the specific question being asked.
pub async fn generate_question_l0(
    canonical_nodes: &[PyramidNode],
    question_tree: &QuestionTree,
    extraction_schema: &ExtractionSchema,
    audience: Option<&str>,
    llm_config: &LlmConfig,
    conn: &Connection,
    slug: &str,
) -> Result<Vec<PyramidNode>>
```

**Behavior:**
1. For each canonical L0 node, ask the LLM:
   "Given this source material summary and these questions, extract the evidence
   relevant to answering these questions. Write for {audience}. Cross-check your
   extraction against the canonical summary — do not add claims not supported by it."
2. Skip canonical L0 nodes with no relevance to any leaf question (pre-filter by keyword overlap)
3. Generate question-L0 nodes with IDs `L0-{uuid}`
4. Each question-L0 node's `self_prompt` = the leaf question it's most relevant to
5. Save nodes via `db::save_node()`
6. Return the full list of question-L0 nodes

**Concurrency:** Use `tokio::spawn` with Semaphore(8) for parallel LLM calls.

**Model selection:** Mercury-2 (canonical L0 summaries are small, question-L0 outputs are small).

## Build Runner Rewiring Contract (WS-C)

### Changes to `run_decomposed_build` in `build_runner.rs`:

**New flow:**
```
1. Characterize (existing)
2. Canonical L0 extraction (NEW — calls canonical_l0::extract_canonical_l0)
   - Skips if canonical L0 already exists for this slug
   - Uses generic prompt, not question-shaped
3. Build canonical L0 summary for decomposition context
   - Calls db::get_canonical_l0_summaries()
   - Formats as text block for the decomposer
4. Decompose question (MODIFIED — passes canonical L0 summaries instead of folder_map)
   - The decomposer now sees actual extracted content, not just file names
   - Makes informed leaf/branch decisions based on material breadth
5. Generate extraction schema (existing, for question-shaping)
6. Question L0 pass (NEW — calls question_l0::generate_question_l0)
   - Reads canonical L0, shapes for question + audience
7. Evidence loop L1+ (existing — now reads question-L0 nodes instead of executor-created L0)
8. Update slug stats, complete build
```

**Key changes:**
- Remove the `execute_plan` call for L0 extraction — canonical_l0 handles it
- Remove the "delete upper-layer nodes" step — no longer needed since executor doesn't create them
- The evidence loop reads question-L0 nodes (`depth = 0 AND id NOT LIKE 'C-L0-%'`)
- The decomposition config gets `canonical_l0_summary: Option<String>` instead of `folder_map`

### Decomposition Context Change

In `question_decomposition.rs`, the `call_decomposition_llm` function currently receives `folder_map` (file listing). Change to receive canonical L0 summaries:

```
Instead of:
  "Source material context: [file listing]"

Now:
  "Source material (extracted summaries of {N} documents):
   - C-L0-001: {headline} — {distilled_truncated}
   - C-L0-002: {headline} — {distilled_truncated}
   ..."
```

This gives the decomposer actual content knowledge, not just file names.

## Acceptance Criteria

### WS-A (Canonical L0 + DB):
- [ ] `has_canonical_l0` returns true after canonical extraction, false before
- [ ] `get_canonical_l0_nodes` returns all C-L0-* nodes
- [ ] `get_canonical_l0_summaries` returns (id, headline, distilled) tuples
- [ ] Canonical extraction creates nodes with C-L0-* IDs and empty self_prompt
- [ ] Re-running canonical extraction on same slug skips (returns early)
- [ ] `clear_canonical_l0` removes only C-L0-* nodes
- [ ] `cargo check` passes

### WS-B (Question L0):
- [ ] Reads canonical L0 nodes and produces question-shaped L0 nodes
- [ ] Question L0 IDs use L0-{uuid} pattern
- [ ] Question L0 self_prompt contains the relevant leaf question
- [ ] Audience framing appears in the question L0 extraction prompt
- [ ] Cross-checks against canonical summary (prompt instructs this)
- [ ] Parallel execution with Semaphore(8)
- [ ] `cargo check` passes

### WS-C (Build Runner):
- [ ] Canonical L0 extracted first (or reused if exists)
- [ ] Decomposer receives canonical L0 summaries, not folder_map
- [ ] Question L0 pass runs after decomposition
- [ ] Evidence loop reads question-L0 nodes
- [ ] Mechanical (non-question) builds still work (no regression)
- [ ] Build completes end-to-end with correct pyramid structure
- [ ] `cargo check` passes
