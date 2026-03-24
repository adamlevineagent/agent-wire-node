# Vine Conversation System — Implementation Plan

> Status: SUPERSEDED — see vine-conversation-system-v2.md
> Date: 2026-03-23
> Target: agent-wire-node pyramid engine (Rust)
> Superseded by: vine-conversation-system-v2.md (2026-03-24)
> Reason: Incorrect grape/bunch/vine model + Stage 1 audit found 2 critical, 8 major issues

## Problem

The pyramid engine builds excellent per-conversation knowledge structures, but there's no way to query *across* conversation sessions. Questions like "when did we decide X?", "how did the project evolve?", or "which sessions discussed authentication?" require manually remembering which session to search. With 71+ conversation sessions for GoodNewsEveryone alone, this is untenable.

## Solution: The Vine

A **meta-pyramid** where each conversation session is a "grape" — a full pyramid built from one JSONL file. The vine connects grapes temporally, enabling cross-session queries, era detection, decision tracking, and entity resolution.

**Key principle: annotations-as-contributions.** One new table (`vine_grapes`). Everything else — ERAs, decisions, entities, thread continuity, corrections — expressed through existing pyramid infrastructure: annotations, FAQ entries, and web edges. No parallel systems.

## V1 Scope

- ✅ `vine_grapes` table + new `vine` content type
- ✅ Grape discovery + batch construction from JSONL files
- ✅ Vine L0/L1/L2+/apex build with temporal-topic affinity clustering
- ✅ Intelligence passes: ERAs, transitions, entity resolution, decisions, thread continuity, corrections
- ✅ Live Vine mode: DADBEAR per-bundle watching conversation pyramid changes
- ⏸️ Cross-project vines (V2)
- ⏸️ Vine merging (coop mode, later)
- ⏸️ Auto-source discovery (later)

## Architecture

### The Two-Tier Model

**Grape** = one conversation session = one JSONL file = one full pyramid (L0 chunks → L1 topics → L2 threads → apex). Grapes are immutable historical archives once the conversation ends. Active conversations produce provisional grapes that update via DADBEAR.

**Vine** = meta-pyramid of grapes. L0 nodes are grape apexes + L2 substance. Strict temporal ordering. Uses temporal-topic affinity for L1 clustering instead of positional pairing. Detects eras, transitions, decision evolution, thread continuity.

### Why Apex + L2/L3 (Not Just Apex)

The vine's staleness trigger watches the **apex chain + the L2/L3 nodes that changed**. The apex alone is too compressed — it may not reflect meaningful changes happening in the threads below. The L2/L3 layer carries the substance. Think of it as: apex is the alarm, the layer below is the evidence.

This means vine L0 nodes contain both:
- The grape's apex (compressed headline)
- The grape's L2 thread summaries (expanded detail)

---

## Phase 1: Database Schema

### 1a. New content type: `vine`

**File:** `src-tauri/src/pyramid/types.rs`
- Add `Vine` variant to `ContentType` enum
- Add `"vine"` to `as_str()` and `from_str()`

**File:** `src-tauri/src/pyramid/db.rs`
- Add `'vine'` to `pyramid_slugs.content_type` CHECK constraint

### 1b. New table: `vine_grapes`

```sql
CREATE TABLE IF NOT EXISTS vine_grapes (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    vine_slug       TEXT NOT NULL REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
    grape_slug      TEXT NOT NULL REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
    session_id      TEXT NOT NULL,
    jsonl_path      TEXT NOT NULL,
    grape_index     INTEGER NOT NULL,       -- temporal order within vine (0-based)
    first_ts        TEXT,                   -- ISO timestamp of first user message
    last_ts         TEXT,                   -- ISO timestamp of last message
    message_count   INTEGER,
    chunk_count     INTEGER,
    apex_node_id    TEXT,                   -- grape pyramid's apex node ID
    vine_l0_node_id TEXT,                   -- corresponding vine L0 node
    status          TEXT NOT NULL DEFAULT 'pending',  -- pending | building | built | error
    metadata        TEXT,                   -- JSON: VineGrapeMetadata
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at      TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(vine_slug, session_id)
);
CREATE INDEX IF NOT EXISTS idx_vine_grapes_vine ON vine_grapes(vine_slug);
CREATE INDEX IF NOT EXISTS idx_vine_grapes_order ON vine_grapes(vine_slug, grape_index);
```

### 1c. New types

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VineGrape {
    pub id: i64,
    pub vine_slug: String,
    pub grape_slug: String,
    pub session_id: String,
    pub jsonl_path: String,
    pub grape_index: i64,
    pub first_ts: Option<String>,
    pub last_ts: Option<String>,
    pub message_count: Option<i64>,
    pub chunk_count: Option<i64>,
    pub apex_node_id: Option<String>,
    pub vine_l0_node_id: Option<String>,
    pub status: String,
    pub metadata: Option<VineGrapeMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VineGrapeMetadata {
    pub topics: Vec<String>,
    pub entities: Vec<String>,
    pub decisions: Vec<Decision>,
    pub corrections: Vec<Correction>,
    pub open_questions: Vec<String>,
    pub l2_summaries: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct GrapeDiscovery {
    pub session_id: String,
    pub jsonl_path: PathBuf,
    pub first_ts: String,
    pub last_ts: String,
    pub message_count: i64,
}
```

---

## Phase 2: Grape Discovery & Construction

### New file: `src-tauri/src/pyramid/vine.rs`

### 2a. Grape Discovery

```rust
pub async fn discover_grapes(jsonl_dir: &Path) -> Result<Vec<GrapeDiscovery>>
```

- Scan `jsonl_dir` for `*.jsonl` files (top-level only — skip subdirectories containing subagent files)
- For each file: read first `type: "user"` line → extract `timestamp` and `sessionId`
- Read last message line → `last_timestamp`
- Count user+assistant messages (skip `progress`, `queue-operation`, `system`, `last-prompt` types)
- Skip files with < 3 user messages (trivial/aborted sessions)
- Sort by `first_timestamp` ascending → defines `grape_index`

**Conversation source:** `/Users/adamlevine/.claude/projects/-Users-adamlevine-AI-Project-Files-GoodNewsEveryone/*.jsonl`

**JSONL format (Claude Code):**
```json
{"type": "user", "message": {"role": "user", "content": "..."}, "timestamp": "2026-03-20T15:05:19Z", "sessionId": "UUID", "uuid": "UUID"}
{"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": "..."}]}, "timestamp": "..."}
{"type": "progress", ...}  // skip
{"type": "queue-operation", ...}  // skip
{"type": "system", ...}  // skip
{"type": "last-prompt", ...}  // skip
```

Content can be string or array of blocks (text, tool_use, tool_result). Reuse existing JSONL parsing from `ingest.rs`.

### 2b. Build Each Grape

```rust
pub async fn build_grape(
    state: &PyramidState,
    vine_slug: &str,
    grape: &GrapeDiscovery,
    grape_index: i64,
    llm: &LlmConfig,
    cancel: &CancellationToken,
) -> Result<VineGrape>
```

1. Create grape slug: `{vine_slug}-grape-{grape_index:03}` with `content_type = "conversation"`
2. Call `ingest::ingest_conversation()` — existing function, unchanged
3. Call `build::build_conversation()` — existing function, unchanged
4. Read apex: `db::get_apex(conn, &grape_slug)`
5. Read L2 nodes: `SELECT * FROM live_pyramid_nodes WHERE slug=? AND depth=2`
6. Extract metadata mechanically (2c)
7. Insert into `vine_grapes`

### 2c. Metadata Extraction (Mechanical, No LLM)

```rust
fn extract_grape_metadata(apex: &PyramidNode, l2_nodes: &[PyramidNode]) -> VineGrapeMetadata
```

- **topics**: collect all `topic.name` from L2 nodes' `topics` vec
- **entities**: collect all `topic.entities` from L2 topics
- **decisions**: collect all decisions from L2 nodes
- **corrections**: collect all corrections from L2 nodes
- **open_questions**: parse from apex `self_prompt`
- **l2_summaries**: each L2 node's `distilled` text

### 2d. Batch Build (Crash-Safe)

```rust
pub async fn build_all_grapes(
    state: &PyramidState,
    vine_slug: &str,
    jsonl_dir: &Path,
    llm: &LlmConfig,
    cancel: &CancellationToken,
    progress_tx: Option<mpsc::Sender<BuildProgress>>,
) -> Result<Vec<VineGrape>>
```

- Sequential (LLM rate-limited)
- Skip grapes with `status = 'built'` (crash-safe resume)
- Progress via existing `mpsc` channel pattern from `build.rs`

---

## Phase 3: Vine Construction

### 3a. Vine L0 — Mechanical Assembly

**No LLM.** For each grape in temporal order, create a vine L0 node:

**Node ID:** `L0-{grape_index:03}` under the vine slug
**Content format:**
```
## Session [{grape_index}]: {apex_headline}
Date: {first_ts} → {last_ts} ({duration})
Messages: {message_count}
Topics: {comma-separated topic list}

### Summary
{apex_distilled}

### Thread Detail
{l2_summaries joined with headers}
```

**Topics JSON:** assembled from grape metadata (topics, entities, decisions, corrections)

### 3b. Vine L1 — Temporal-Topic Affinity Clustering

**For ≤20 grapes:** Use existing `THREAD_CLUSTER_PROMPT` with temporal ordering instructions prepended. Let LLM decide clusters.

**For >20 grapes:** Algorithmic pre-clustering:

```
affinity(i, j) = 0.40 × temporal_proximity(i, j)
               + 0.35 × topic_overlap(i, j)
               + 0.25 × entity_overlap(i, j)

where:
  temporal_proximity = 1.0 / (1 + |grape_index_i - grape_index_j|)
  topic_overlap      = jaccard(topics_i, topics_j)
  entity_overlap     = jaccard(entities_i, entities_j)
```

Constraints:
- Max cluster size: 4 grapes
- Contiguity: max 2-grape gap within a cluster
- Algorithm: greedy agglomerative, merge highest-affinity pairs first, stop below 0.15 threshold

Each cluster → one vine L1 node, distilled with existing `THREAD_NARRATIVE_PROMPT` using `[LATE — AUTHORITATIVE]` markers on more recent grapes.

### 3c. Vine L2+/Apex

Reuse `build_upper()` pattern: pair adjacent L1 nodes, distill with `DISTILL_PROMPT`, recurse to apex.

**All existing prompts reused unchanged:** `DISTILL_PROMPT`, `THREAD_CLUSTER_PROMPT`, `THREAD_NARRATIVE_PROMPT`

---

## Phase 4: Intelligence Passes

All outputs go through existing tables — no new infrastructure.

### 4a. ERA Detection

Three signals blended:

1. **Entity overlap sliding window** (mechanical): window=5 grapes, compute overlap ratio. Boundary candidate when overlap < 0.3.
2. **LLM phase classifier** (for ambiguous 0.3–0.6 zone): new `VINE_PHASE_CHECK_PROMPT` asks "same project phase or different?"
3. **Temporal gap reinforcement**: 3+ day gaps between grapes strengthen boundary signal

Output: annotations on vine L1/L2 nodes with `annotation_type = "era"` containing:
```json
{
  "era_number": 2,
  "label": "Authentication & Database Design",
  "date_range": ["2026-03-01", "2026-03-08"],
  "grape_ids": ["grape-005", "grape-006", "grape-007"],
  "dominant_topics": ["auth", "database schema"],
  "emergent_topics": ["RLS policies"],
  "fading_topics": ["initial setup"],
  "narrative_summary": "..."
}
```

### 4b. Transition Classification

For each pair of adjacent ERAs, classify via `VINE_TRANSITION_PROMPT`:
- **pivot**: fundamentally different focus
- **evolution**: same focus, deeper understanding
- **expansion**: same focus, broader scope
- **refinement**: same focus, tighter execution
- **return**: circling back to earlier era's concerns

Output: annotations with `annotation_type = "transition"`

### 4c. Entity Resolution

1. Collect all entities across all grape metadata
2. Cluster by fuzzy match (Levenshtein < 3 or substring containment)
3. LLM picks canonical name per cluster
4. Store as FAQ entries in `pyramid_faq_nodes` with `match_triggers` aliases in metadata

### 4d. Decision Tracking

1. Collect all decisions across grapes
2. Group by topic/entity overlap
3. Build evolution chains: proposed → decided → modified → replaced (with grape references)
4. Store as FAQ entries with evolution chain in metadata

### 4e. Thread Continuity

1. Match L2 thread names across grapes using entity resolution canonical names
2. Create web edges in `pyramid_web_edges` linking related vine L0 nodes
3. Track lifecycle: active / dormant / resolved / recurring

### 4f. Correction Chains

1. Match corrections across grapes where correction in grape N fixes something from grape M
2. Store as annotations with `annotation_type = "correction"` pointing to both grapes

---

## Phase 5: Live Vine Mode

### 5a. DADBEAR Integration

Reuses existing `PyramidStaleEngine` and `PyramidFileWatcher` from `stale_engine.rs`/`watcher.rs`.

The vine registers a **vine-level watcher** that:
- Monitors grape pyramid changes (not JSONL files directly)
- When a grape pyramid's L2+ nodes get superseded → vine receives notification
- Uses existing `pyramid_pending_mutations` WAL for crash safety

### 5b. Staleness Trigger: Apex + L2/L3

```rust
pub async fn check_grape_staleness(
    state: &PyramidState,
    vine_slug: &str,
    grape: &VineGrape,
) -> Result<bool>
```

- Read grape's current apex + L2 nodes
- Compare against stored `vine_grapes.metadata`
- Apex changed → definitely stale
- Only L2/L3 changed but apex didn't → **still stale** (apex too compressed to reflect the change)
- Update metadata with new state

### 5c. Propagation

When grape confirmed stale:
1. Re-extract metadata from updated grape pyramid
2. Supersede vine L0 node via existing `superseded_by` mechanism
3. Write mutation to `pyramid_pending_mutations` for vine's L1 layer
4. Vine's stale engine picks up → rebuilds L1 cluster → propagates to L2+/apex
5. Incremental intelligence passes around changed grape

### 5d. Active Conversation Detection

- Poll JSONL directory for `mtime` changes
- Active conversations: JSONL growing → grape rebuild → vine update
- Finished conversations: JSONL stops growing → final grape build → vine settles

---

## Phase 6: HTTP API Routes

New routes in `routes.rs`:

| Method | Path | Purpose |
|--------|------|---------|
| POST | `/pyramid/vine/build` | Build vine from JSONL directory |
| GET | `/pyramid/:slug/vine/grapes` | List all grapes with metadata |
| GET | `/pyramid/:slug/vine/eras` | List detected ERAs |
| GET | `/pyramid/:slug/vine/decisions` | Decision evolution chains |
| GET | `/pyramid/:slug/vine/entities` | Resolved entities with aliases |
| GET | `/pyramid/:slug/vine/threads` | Cross-grape thread continuity |
| GET | `/pyramid/:slug/vine/timeline` | Timeline data for visualization |

Existing routes work automatically on vine slugs: `/apex`, `/search`, `/tree`, `/drill`, `/annotations`, `/faq/*`

---

## Files

### New
| File | Est. Lines | Purpose |
|------|-----------|---------|
| `src-tauri/src/pyramid/vine.rs` | 800-1000 | Core vine: discovery, grape build, vine construction, intelligence, live mode |
| `src-tauri/src/pyramid/vine_prompts.rs` | 150 | Vine-specific LLM prompts |

### Modified
| File | Change |
|------|--------|
| `pyramid/mod.rs` | Add `pub mod vine; pub mod vine_prompts;` |
| `pyramid/types.rs` | Add `Vine` to ContentType, VineGrape + VineGrapeMetadata structs |
| `pyramid/db.rs` | Add vine_grapes table, update content_type CHECK, vine CRUD functions |
| `pyramid/routes.rs` | Add vine HTTP routes |

### Reused Unchanged
| File | What |
|------|------|
| `ingest.rs` | `ingest_conversation()` |
| `build.rs` | `build_conversation()`, all prompts, `build_upper()` pattern |
| `query.rs` | All query functions (work on vine nodes automatically) |
| `delta.rs` | Delta chains for vine L2+ |
| `webbing.rs` | Web edges for thread continuity |
| `faq.rs` | FAQ entries for decisions + entity aliases |
| `stale_engine.rs` | DADBEAR infrastructure |
| `llm.rs` | `call_model()`, `extract_json()`, model cascade |

---

## Cost Estimates

| Operation | Per grape | 71-grape total |
|-----------|-----------|----------------|
| Grape pyramid build | ~$0.02 | ~$1.42 |
| Metadata extraction | $0 (mechanical) | $0 |
| Vine L0 assembly | $0 (mechanical) | $0 |
| Vine L1 clustering + distillation | — | ~$0.10 |
| Vine L2+/apex | — | ~$0.05 |
| Intelligence passes (all) | — | ~$0.25 |
| **Total full build** | | **~$1.82** |
| **Incremental per new grape** | | **~$0.05** |

---

## Design Decisions Log

1. **One table, everything else is contributions** — No vine_eras table, no vine_decisions table. ERAs are annotations, decisions are FAQ, threads are web edges.

2. **Apex + L2/L3 trigger, not apex alone** — Apex too compressed. The layer below carries the substance. Vine L0 includes both.

3. **Temporal ordering is first-class** — Unlike code pyramids (no inherent order), vine L0 has strict total order by `first_ts`. Every algorithm respects this.

4. **Grapes are immutable archives** — Once conversation ends, grape pyramid doesn't change. DADBEAR applies to the vine, not finished grapes. Active conversations are the exception (Live Vine mode).

5. **Grape = full pyramid, not just apex** — Each grape IS a conversation pyramid. The vine's L0 node references the grape slug, letting users drill from vine → grape → individual messages.

6. **Reuse existing conversation pipeline unchanged** — `ingest_conversation()` and `build_conversation()` work as-is. No modifications to the battle-tested build pipeline.

7. **Sequential grape building** — LLM rate limits make parallelism counterproductive at the grape level. But vine L0 assembly is mechanical and instant.

8. **≤20 grapes: LLM clustering; >20: algorithmic** — Small vine? Let LLM decide clusters (it's good at this). Large vine? Algorithmic pre-clustering with affinity formula to keep costs bounded.
