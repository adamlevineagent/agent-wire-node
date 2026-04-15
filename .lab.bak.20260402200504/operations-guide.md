# Wire Node Pyramid Operations Guide

## Quick Reference

### Database & Config Paths
- **Database:** `~/Library/Application Support/wire-node/pyramid.db`
- **Config:** `~/Library/Application Support/wire-node/pyramid_config.json`
- **Chains:** `chains/defaults/` (YAML), `chains/prompts/` (markdown)
- **Vocabulary:** `chains/vocabulary_yaml/`

---

## Triggering Builds

### Via Tauri IPC (Desktop App)

Builds are triggered through the desktop app's command system:

| Command | Args | Purpose |
|---------|------|---------|
| `pyramid_create_slug` | `slug, content_type, source_path` | Create namespace |
| `pyramid_build` | `slug` | Start mechanical build |
| `pyramid_question_build` | `slug, question, granularity?, maxDepth?` | Start question build |
| `pyramid_build_cancel` | `slug` | Cancel running build |
| `pyramid_build_status` | `slug` | Poll progress |

### Via HTTP API (requires running app)

```bash
# Create slug
curl -X POST http://localhost:PORT/pyramid \
  -H "Authorization: Bearer TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"slug": "NAME", "content_type": "code", "source_path": "/path"}'

# Start build
curl -X POST http://localhost:PORT/pyramid/NAME/build \
  -H "Authorization: Bearer TOKEN"

# Check status
curl http://localhost:PORT/pyramid/NAME/status \
  -H "Authorization: Bearer TOKEN"
```

### Content Types
- `code` — source code files (uses `code.yaml` chain)
- `document` — markdown, design docs (uses `document.yaml` chain)
- `conversation` — chat logs (uses `conversation.yaml` chain)
- `question` — question-driven pyramids

---

## Inspecting Results

### Query Commands

| Command | Returns |
|---------|---------|
| `pyramid_list_slugs` | All pyramids with metadata |
| `pyramid_apex` | Root node + web edges |
| `pyramid_node` | Specific node by ID |
| `pyramid_tree` | Flat list of all nodes in depth order |
| `pyramid_drill` | Node + children + evidence |
| `pyramid_search` | Full-text search across nodes |
| `pyramid_cost_summary` | LLM costs per operation |

### Direct Database Queries

```sql
-- List active pyramids
SELECT slug, content_type, node_count, max_depth, last_built_at
FROM pyramid_slugs WHERE archived_at IS NULL;

-- Node counts per layer
SELECT slug, depth, COUNT(*) FROM pyramid_nodes
WHERE superseded_by IS NULL AND build_version > 0
GROUP BY slug, depth ORDER BY slug, depth;

-- Check pipeline steps for a build
SELECT step_type, COUNT(*), AVG(elapsed_seconds)
FROM pyramid_pipeline_steps WHERE slug = 'NAME'
GROUP BY step_type;

-- Get apex node content
SELECT id, headline, distilled, topics
FROM pyramid_nodes WHERE slug = 'NAME'
AND depth = (SELECT MAX(depth) FROM pyramid_nodes WHERE slug = 'NAME');
```

---

## Build Pipeline Architecture

### Mechanical Build Flow

```
Source Files → Chunking → L0 Extraction → Thread Clustering → L1 Synthesis
    → L1 Webbing → Upper Layer Convergence (recursive cluster) → Apex
```

### Per Content Type

| Type | L0 Extraction | Clustering | Synthesis | Upper Layers |
|------|--------------|------------|-----------|-------------|
| Code | Per-file analysis | Semantic threads (6-12) | Per-thread dev briefing | Recursive cluster → apex |
| Document | Per-doc type-aware extraction | Concept areas | Temporally-ordered per-area | Recursive cluster → apex |
| Conversation | Forward + reverse + combine | Subject-based threads | Per-thread with temporal authority | Recursive cluster → apex |

### Chain Execution (YAML-driven)

Chain definitions in `chains/defaults/{code,document,conversation}.yaml` drive the build.
Each step references a prompt in `chains/prompts/{type}/{name}.md`.

**Key primitives:**
- `extract` — per-item LLM analysis → L0 nodes
- `compress` — sequential with context accumulation
- `fuse` — combine multiple perspectives (zip_steps)
- `classify` — single LLM call grouping items
- `synthesize` — per-group synthesis → higher-layer nodes
- `web` — edge discovery between siblings
- `recursive_cluster` — cluster → synthesize loop until apex

### Upper Layer Convergence (recursive_cluster)

The convergence system uses an **IR executor** that unrolls the cluster→synthesize loop:

1. **Shortcut check:** If ≤4 L1 nodes, directly synthesize to apex (skip clustering)
2. **Round N:** Classify current nodes into clusters → synthesize each cluster → next layer
3. **Repeat** until ≤4 nodes remain or max_rounds (8) exhausted
4. `shortcut_at: 4` — threshold for direct synthesis
5. Safety net: if clustering doesn't reduce, force-merge smallest clusters

**Each round produces 4 pipeline steps:**
- `upper_layer_synthesis_rN_classify` — LLM grouping
- `upper_layer_synthesis_rN_fallback` — positional groups if classify fails
- `upper_layer_synthesis_rN_repair` — reassign missing nodes
- `upper_layer_synthesis_rN_reduce` — per-cluster synthesis

---

## Configuration

### Operational Tiers

**Tier 1 (Operator):**
- `stale_max_concurrent_helpers`: 3
- `llm_max_retries`: 5
- `answer_concurrency`: 5
- Model selection, context limits

**Tier 2 (Tunable):**
- `staleness_threshold`: 0.3
- `chunk_target_lines`: 100
- `max_headline_chars`: 72

**Tier 3 (Expert):**
- `batch_cap_nodes`: 5
- `batch_cap_connections`: 20
- `staleness_max_propagation_depth`: 20

### Models
- Default: `inception/mercury-2` (mid-tier)
- Classification/clustering: `qwen/qwen3.5-flash-02-23`
- Large payloads: auto-escalate to qwen

---

## Auto-Update (DADBEAR)

After successful build, the system initializes:
1. **File watcher** — detects source file changes
2. **Stale engine** — per-layer debounce timers
3. **Staleness propagation** — weight-based: L0 changes propagate upward with attenuation

Controls:
- `pyramid_auto_update_config` / `pyramid_auto_update_freeze` / `pyramid_auto_update_unfreeze`
- Circuit breaker: pauses if LLM errors cascade
- `pyramid_auto_update_l0_sweep` — force re-check all L0 nodes

---

## Prompt Editing Guide

### File Locations
```
chains/prompts/
├── code/          (extract, cluster, thread, web, distill, recluster)
├── document/      (classify, taxonomy, extract, concept_areas, assign, thread, web, distill, recluster)
├── conversation/  (forward, reverse, combine, cluster, thread, web, distill, recluster)
├── question/      (characterize, decompose, enhance, extraction_schema, pre_map, answer, horizontal_review, synthesis_prompt)
└── planner/       (classifier-system, planner-system)
```

### Template Variables
- `{{variable}}` — reads from step's resolved input JSON
- Dot-path: `{{data.summary}}`
- Available vars depend on the step's input configuration in YAML

### Key Rules
- Prompts ending in `/no_think` skip LLM reasoning tokens
- JSON output format is specified in the prompt text
- Response schemas in YAML enforce structured output (but can cause token clipping)
- Steps without `response_schema` get free-form JSON + retry (more reliable for large outputs)

---

## Database Schema (Key Tables)

| Table | Purpose |
|-------|---------|
| `pyramid_slugs` | Pyramid namespaces (slug, content_type, source_path, node_count, max_depth) |
| `pyramid_nodes` | Knowledge nodes (id, slug, depth, headline, distilled, topics, parent_id, children) |
| `pyramid_pipeline_steps` | Build execution trace (step_type, output_json, model, elapsed_seconds) |
| `pyramid_web_edges` | Cross-thread relationships |
| `pyramid_file_hashes` | File→node mapping for auto-update |
| `pyramid_auto_update_config` | DADBEAR settings per slug |
| `pyramid_source_deltas` | File change audit trail |
| `pyramid_staleness_queue` | Pending re-answer queue |
| `pyramid_pending_mutations` | Batched mutations for dispatch |

---

## Known Issues

### Apex Convergence Gap (Active)
The converge expansion unrolls rounds with guards `count($prev) > 4`. When a round produces
exactly 2-4 nodes, the next round's guard is false but no shortcut fires (shortcut only checks
initial input). Result: build ends with 2-4 top-layer nodes and no apex.

### Workaround
Prompt the recluster step to be more aggressive on early rounds, targeting ≤4 clusters so the
shortcut fires before rounds begin, or target exactly 1 cluster to produce apex directly.

---

## Build Lifecycle

```
idle → running → complete | complete_with_errors | failed | cancelled
```

- `complete`: All nodes built (failures == 0)
- `complete_with_errors`: Most built, some failed
- `failed`: Fatal error
- `cancelled`: User cancelled
