# Chain Optimization — Final Handoff

## Date: 2026-03-25
## Branch: research/chain-optimization (pushed to GitHub)
## Current Score: 85/100 blind tester average

---

## Priority 1: Surface Web Edges to Consumers

**Problem**: 26 web edges exist in `pyramid_web_edges` but the CLI `drill` command and MCP tools don't include them in output. Blind testers never see cross-cutting connections.

**Fix**: When returning a node via `drill`, `apex`, or MCP query, also return web edges where that node is source or target:
```sql
SELECT thread_a_id, thread_b_id, relationship, relevance
FROM pyramid_web_edges
WHERE slug = ?1 AND (thread_a_id = ?2 OR thread_b_id = ?2)
ORDER BY relevance DESC
```

Add to the drill response JSON as:
```json
{
  "id": "L1-002",
  "headline": "Pyramid Engine Core",
  "distilled": "...",
  "topics": [...],
  "children": ["C-L0-093", ...],
  "web_edges": [
    {
      "connected_to": "L1-009",
      "connected_headline": "Pyramid Query Layer",
      "relationship": "Both read/write pyramid_nodes table",
      "strength": 0.9
    }
  ]
}
```

**Impact**: +2-3 points on Q3 (frontend-backend), Q10 (bug fix confidence). This is the single highest-ROI change.

**Files**: `src-tauri/src/pyramid/db.rs` (query), `src-tauri/src/pyramid/routes.rs` (drill/apex response), `mcp-server/src/tools.ts` (MCP drill tool)

---

## Priority 2: Feed Web Edges into Synthesis Context

**Problem**: Thread synthesis and upper-layer synthesis don't know about cross-cutting connections. L1-002 (Pyramid Engine) doesn't mention that it shares `pyramid_nodes` table with L1-009 (Query Layer).

**Fix**: When building the user prompt for thread_narrative and upper_layer_synthesis, append web edges as context:

For thread_narrative (L1 synthesis):
```
## CONNECTIONS TO OTHER THREADS
- Files C-L0-093, C-L0-094 in this thread share pyramid_nodes table with C-L0-087 in thread "Query Layer"
- File C-L0-095 shares validate_token() with C-L0-063 in thread "Backend Core"
```

For upper_layer_synthesis (L2+ synthesis):
```
## CROSS-SUBSYSTEM CONNECTIONS
- L1-002 ↔ L1-009: Both read/write pyramid_nodes (strength 0.9)
- L1-004 ↔ L1-008: Frontend invokes Tauri commands implemented in backend (strength 0.95)
```

**Implementation**: In `chain_executor.rs` where the user prompt is assembled for forEach items, query `pyramid_web_edges` for edges touching nodes in the current thread/cluster and append formatted text.

**Impact**: +1-2 points on all relationship/integration questions. Makes cross-cutting concerns appear in node content, not just metadata.

---

## Priority 3: Enforce Max Thread Size at Rust Level

**Problem**: `code_cluster.md` says "max 12 files per thread" but qwen ignores it. L1-002 had 19 files, causing 305K char synthesis prompts that take 95+ seconds and risk token clipping.

**Fix**: After parsing the clustering JSON response, post-process:
```rust
for thread in &mut threads {
    if thread.assignments.len() > MAX_THREAD_SIZE {
        // Split into sub-threads of MAX_THREAD_SIZE
        let chunks: Vec<_> = thread.assignments.chunks(MAX_THREAD_SIZE).collect();
        // First chunk keeps the original thread name
        // Additional chunks get "{name} (Part 2)", "{name} (Part 3)"
        // Replace the single thread with multiple sub-threads
    }
}
```

`MAX_THREAD_SIZE` should be configurable in the YAML (default 12):
```yaml
- name: thread_clustering
  max_thread_size: 12
```

**Impact**: Prevents token clipping, faster builds, more granular L1 nodes.

---

## Priority 4: L3+ Headline Deduplication

**Problem**: Upper layer headlines are generic: "Desktop Runtime Integration Layer", "Integrated Knowledge Platform Backend". Both testers flag this every time (7/10).

**Fix**: Two approaches (do both):

**A. Banned words list** in `code_distill.md` and `code_recluster.md`:
```
Your headline MUST NOT contain these words: Integration, Layer, Platform,
Overview, System, Architecture, Suite, Stack, Core, Engine, Unified,
Comprehensive, Module. Use concrete nouns: "Auth & Token Validation",
"Pyramid Build Pipeline", "React Dashboard Components".
```

**B. Sibling headline injection** — when synthesizing a node, pass its sibling headlines:
```
## SIBLING NODES AT THIS DEPTH (your headline must differ from ALL of these)
- L2-000: "MCP Server & CLI"
- L2-002: "Frontend Components"
Your headline for L2-001 must describe a DIFFERENT architectural concern.
```

This requires the executor to gather sibling headlines before dispatching each synthesis call. Small change in `execute_recursive_cluster()` — after clustering but before synthesizing each cluster, collect all cluster names and pass them as context.

**Impact**: +1-2 points on Q9 (headline distinctness).

---

## Priority 5: L0 Webbing Before Clustering

**Problem**: Currently webbing happens at L1 and L2 only. L0 webbing would tell the clustering step which files share resources, producing better thread groupings.

**Fix**: Add a web step after L0 extract and before clustering in `code.yaml`:
```yaml
- name: l0_webbing
  primitive: web
  instruction: "$prompts/code/code_web.md"
  depth: 0
  save_as: web_edges
  model_tier: mid
  temperature: 0.2
  on_error: skip
```

Then pass L0 web edges to the clustering prompt:
```
## FILE-LEVEL CONNECTIONS (from automated analysis)
- C-L0-093 ↔ C-L0-087: both write pyramid_nodes table
- C-L0-041 ↔ C-L0-063: both call validate_token()
Files with strong connections should be in the SAME thread.
```

**Concern**: 112 L0 nodes = large prompt for webbing. Could batch or use qwen for L0 webbing. Or only send entity lists (not full orientations) to keep prompt small.

**Impact**: Better clustering → better L1 nodes → +1-2 points across the board.

---

## Priority 6: Frontend-Specific Extract Prompt

**Problem**: React/TSX files are underrepresented. Testers score "React component architecture" at 7/10. The generic `code_extract.md` doesn't capture component hierarchy, props, hooks, or state patterns.

**Fix**: Create `code_extract_frontend.md` with additional topic categories:
- "Component Hierarchy" — parent/child relationships, what renders what
- "Props & State" — what props this component accepts, what state it manages
- "Hooks & Effects" — useEffect dependencies, custom hooks, side effects
- "User Interactions" — what events this component handles, where they dispatch to

In `code.yaml`, add a conditional:
```yaml
- name: l0_code_extract
  instruction: "$prompts/code/code_extract.md"
  # Future: when chunk.file_ext in ['.tsx', '.jsx']:
  #   instruction: "$prompts/code/code_extract_frontend.md"
```

**Impact**: +1 point on Q2 (subsystems — frontend modules), Q3 (frontend-backend relationship).

---

## Priority 7: Algorithm Detail in Extract Prompt

**Problem**: Testers score "confidence making a bug fix" at 7-8/10. Key algorithms (warm_pass, crystallization, stale detection, delta creation) are named but not explained.

**Fix**: Add to `code_extract.md`:
```
- "Algorithm & Decision Logic" — for the 1-2 most complex algorithms in this
  file (not just functions — algorithms that make decisions), describe:
  What triggers it? What conditions does it check? What are the possible
  outcomes? What side effects does each outcome produce? Think of this as
  a decision tree, not a function signature.
```

**Impact**: +1 point on Q10 (bug fix confidence).

---

## Priority 8: Sub-Pyramid Splitting for Large Threads

**Problem**: When a thread has >12 files and the Rust-level split (Priority 3) produces "{name} (Part 2)", those parts are positional, not semantic.

**Fix**: Instead of positional splitting, run a mini-clustering step within the oversized thread:
1. Take the 19 files in the oversized thread
2. Send their headlines + entities to mercury-2 with a "split into 2-3 sub-groups" prompt
3. Each sub-group becomes its own L1 node
4. All sub-group L1 nodes get the same parent at L2

This is the "mini-pyramid within a thread" pattern. It reuses the existing clustering infrastructure — just applied at a smaller scale.

**Impact**: Better L1 quality for large subsystems. Not urgent if max_thread_size enforcement (Priority 3) is implemented first.

---

## Summary — Priority Order

| # | Change | Effort | Score Impact | Dependencies |
|---|--------|--------|-------------|--------------|
| 1 | Surface web edges in drill/MCP | Small | +2-3 pts | None |
| 2 | Feed web edges into synthesis | Medium | +1-2 pts | #1 helps but not required |
| 3 | Enforce max thread size (Rust) | Small | Reliability | None |
| 4 | L3+ headline dedup | Small | +1-2 pts | None |
| 5 | L0 webbing before clustering | Medium | +1-2 pts | Web primitive exists |
| 6 | Frontend extract prompt | Small | +1 pt | None |
| 7 | Algorithm detail in extract | Small | +1 pt | None |
| 8 | Sub-pyramid splitting | Large | Quality | #3 first |

Priorities 1-4 are high-ROI, small effort. Should push scores to 88-90.
Priorities 5-7 are incremental. Push toward 92-93.
Priority 8 is architectural. Matters more at scale (500+ files).
