# Compute Chronicle — Persistent Observability Layer

**Date:** 2026-04-14
**Scope:** The node's persistent compute autobiography. Every LLM call that touches any compute resource gets recorded. The operator sees one unified history with source as a dimension.
**Prerequisite:** Phase 1 (compute queue, GPU loop, Market tab) is shipped. DADBEAR canonical architecture (Phases 1-7) ships BEFORE the Chronicle. The Chronicle builds on top of DADBEAR's completed infrastructure — work items, QueueEntry extensions, semantic path IDs, and the GPU loop callback are all in place.
**Blocking:** Must ship BEFORE resuming market phases 2-9. The chronicle captures events from all future phases without schema migration.
**Coordination:** DADBEAR is fully implemented first. See Section XII (DADBEAR Coordination) for the integration contract.

---

## I. Design Principles

1. **Append-only, immutable.** Events never update or delete. Law 3 compliant (audit log exception to the one-contribution-store rule).
2. **Source-agnostic schema.** The same table holds local, fleet, market, and cloud events. Source is a column, not a table.
3. **JSONB metadata.** Each event type carries its own fields in a metadata column. Avoids schema sprawl as new event types are added.
4. **Zero-cost when not observed.** The frontend is optional. Events write whether or not anyone is watching.
5. **Future-proof for market phases.** The schema accommodates settlement events, credit flows, market observations without migration. The JSONB metadata pattern handles this.
6. **Pillar 37 compliant.** No hardcoded retention periods, aggregation windows, or display limits. These are contribution-driven or at minimum operator-configurable via the existing generative config system.
7. **StepContext integration.** The chronicle captures what StepContext knows (step_name, build_id, slug, cost) plus what only the queue/dispatch layer knows (queue depth, wait time, source, peer).
8. **Relationship to pyramid_cost_log.** The chronicle does NOT replace `pyramid_cost_log`. That table serves the cost observatory (Phase 11 reconciliation, broadcast confirmation, leak detection). The chronicle is a separate event log with a different shape: lifecycle events with JSONB metadata, not per-call cost rows. They share some data (tokens, cost, latency) but serve different consumers. The cost log is per-LLM-call with reconciliation state. The chronicle is per-lifecycle-event with source provenance.

---

## II. Event Sources

Six sources, all recorded in the same `pyramid_compute_events` table:

| Source | Description | When it exists |
|---|---|---|
| `local` | This node's GPU processed a job from its own pyramid build or stale check | Phase 1 (now) |
| `fleet` | A same-operator fleet peer processed this node's job (free, no credits) | Phase 1 (now) |
| `cloud` | OpenRouter or direct API processed it (dollars) | Phase 1 (now) |
| `fleet_received` | Someone else's work that THIS node's GPU processed via fleet dispatch | Phase 1 (now) |
| `market` | A network market provider processed it (credits) | Future (Phase 2+) |
| `market_received` | Someone else's work that THIS node's GPU processed via market | Future (Phase 5+) |

---

## III. Layer 1 — Event Log Schema

### Task Context: The Narrative Layer

The chronicle isn't a performance log — it's a compute autobiography. Each entry tells you not just WHAT ran and HOW LONG, but WHY and FOR WHAT PURPOSE. This requires threading context from the chain executor level down to the write point.

**Context available from StepContext** (already at every LLM call):
- `slug` — which pyramid
- `build_id` — which build run
- `step_name` — chain step identifier (e.g., "extract_l0", "summarize_cluster")
- `primitive` — chain primitive ("single", "for_each", "recursive_cluster", "recursive_pair")
- `depth` — pyramid layer (0 = L0, 1 = L1, etc.)
- `chunk_index` — which chunk within a for_each
- `model_tier` — routing tier (e.g., "primary")
- `resolved_model_id` / `resolved_provider_id` — what actually executed

**Context available from ChainContext/ExecutionPlan** (needs to be threaded to StepContext):
- `chain_name` — which chain strategy ("code-mechanical", "conversation-episodic", "question-decomposed")
- `content_type` — "code", "document", "conversation"
- `apex_question` — the driving question (from `initial_params.$apex_question`)

**Threading the context:** Add three fields to `StepContext`:
```rust
// In StepContext (step_context.rs):
pub chain_name: String,       // from ExecutionPlan.source_chain_id
pub content_type: String,     // from ChainContext.content_type
pub task_label: String,       // human-readable: if chain_name set -> "{step_name} depth {depth} ({chain_name})", else -> "{step_name} depth {depth}"
```

`task_label` is a computed human-readable description. Derived at StepContext construction time from the existing fields. NOT LLM-generated — purely mechanical string formatting. **Conditional derivation:** if `chain_name` is non-empty, format as `"{step_name} depth {depth} ({chain_name})"` (e.g., "extract_l0 depth 0 (code-mechanical)"). If `chain_name` is empty (stale checks, tests, other non-chain-build paths), format as `"{step_name} depth {depth}"` (e.g., "stale_check depth 2"). This prevents garbled empty parenthetical like "stale_check depth 2 ()".

Populate these in `make_step_ctx_from_llm_config` (step_context.rs ~line 489) from the `CacheAccess` struct on LlmConfig. The CacheAccess already carries `slug` and `build_id` — extend it with `chain_name: Option<String>` and `content_type: Option<String>` (default `None`, set to `Some` only in the chain executor via `.with_chain_context()`). When `None`, StepContext fields default to empty string.

### SQLite Table: `pyramid_compute_events`

Append-only. No UPDATE or DELETE statements anywhere in the codebase. Created in `init_pyramid_db` in `src-tauri/src/pyramid/db.rs` using the existing `CREATE TABLE IF NOT EXISTS` pattern.

```sql
CREATE TABLE IF NOT EXISTS pyramid_compute_events (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    job_path       TEXT NOT NULL,          -- semantic path grouping lifecycle events (DADBEAR work_item_id or generated path)
    event_type     TEXT NOT NULL,          -- enum: see Event Types below
    timestamp      TEXT NOT NULL,          -- ISO 8601, passed explicitly from ChronicleEventContext (app-level chrono::Utc::now())
    -- Technical addressing
    model_id       TEXT,                   -- resolved model (e.g. "deepseek-r1:32b")
    source         TEXT NOT NULL,          -- enum: local, fleet, cloud, fleet_received, market, market_received
    -- Pyramid context (WHERE in the knowledge structure)
    slug           TEXT,                   -- pyramid slug, NULL for fleet_received / market_received
    build_id       TEXT,                   -- build_id from StepContext, NULL when no ctx
    -- Task context (WHAT the compute was doing)
    chain_name     TEXT,                   -- chain strategy: "code-mechanical", "conversation-episodic", etc.
    content_type   TEXT,                   -- "code", "document", "conversation"
    step_name      TEXT,                   -- chain step: "extract_l0", "summarize_cluster", etc.
    primitive      TEXT,                   -- chain primitive: "single", "for_each", "recursive_cluster"
    depth          INTEGER,               -- pyramid layer: 0=L0, 1=L1, etc.
    task_label     TEXT,                   -- human-readable: "L1 Clustering (code-mechanical)"
    -- Event-specific data
    metadata       TEXT                    -- JSON object, fields vary per event_type
);

-- Composite indexes covering primary query patterns.
-- Composites are more efficient than single-column indexes: fewer indexes to maintain,
-- and they support the actual multi-column WHERE clauses the IPC commands generate.

-- Per-pyramid drill-down: "all compute for opt-025 build X, ordered by time"
CREATE INDEX IF NOT EXISTS idx_compute_events_pyramid
    ON pyramid_compute_events(slug, build_id, timestamp);

-- Source-filtered queries: "all fleet events of type fleet_returned, ordered by time"
CREATE INDEX IF NOT EXISTS idx_compute_events_source_type
    ON pyramid_compute_events(source, event_type, timestamp);

-- Model analysis: "all events for deepseek-r1:32b, ordered by time"
CREATE INDEX IF NOT EXISTS idx_compute_events_model
    ON pyramid_compute_events(model_id, timestamp);

-- Layer breakdown: "L0 extraction vs L1 clustering compute time for a given chain"
CREATE INDEX IF NOT EXISTS idx_compute_events_layer
    ON pyramid_compute_events(chain_name, depth, timestamp);

-- Job lifecycle grouping: all events for one queue item (grouped by directly)
CREATE INDEX IF NOT EXISTS idx_compute_events_job_path
    ON pyramid_compute_events(job_path);
```

### Filtering Dimensions

Every indexed column is a filterable dimension in the frontend. The operator can combine any of these:

| Dimension | Column | Example queries |
|---|---|---|
| **By pyramid** | `slug` | "All compute for opt-025" |
| **By build** | `build_id` | "All compute for this specific build run" |
| **By layer** | `depth` | "L0 extraction vs L1 clustering vs L2 synthesis compute time" |
| **By chain strategy** | `chain_name` | "Code builds vs document builds vs question builds" |
| **By content type** | `content_type` | "How much compute goes to code vs documents?" |
| **By step type** | `step_name` | "Which chain steps are most expensive?" |
| **By primitive** | `primitive` | "for_each (parallel) vs recursive_cluster (convergent) cost" |
| **By model** | `model_id` | "gemma4:26b vs deepseek-r1:32b throughput comparison" |
| **By source** | `source` | "Local GPU vs fleet vs cloud — cost and latency comparison" |
| **By time** | `timestamp` | "Last hour", "today", "this week" |
| **By event type** | `event_type` | "Show me only failures", "Show me only fleet dispatches" |
| **Combined** | multiple | "Fleet-dispatched L0 extraction steps for opt-025 this week" |

### Event Types

Each event type has a fixed set of metadata fields in the `metadata` JSON column. The column is TEXT (SQLite has no native JSONB), queried with `json_extract()`.

| event_type | When emitted | metadata fields |
|---|---|---|
| `enqueued` | Job enters the compute queue | `queue_depth`, `queue_model_depth` |
| `started` | GPU loop picks up the job | `queue_wait_ms` (time from enqueue to start) |
| `completed` | GPU loop finishes the job successfully | `latency_ms`, `tokens_prompt`, `tokens_completion`, `cost_usd`, `generation_id` |
| `failed` | GPU loop job errors (after all retries) | `error`, `latency_ms` |
| `fleet_dispatched` | This node dispatched a job to a fleet peer | `peer_id`, `peer_name` (from `FleetPeer.name`), `rule_name`, `timeout_secs` |
| `fleet_returned` | Fleet dispatch returned a result | `peer_id`, `peer_name`, `peer_model`, `latency_ms`, `tokens_prompt`, `tokens_completion` |
| `fleet_dispatch_failed` | Fleet dispatch to a peer failed (peer removed from live set) | `peer_id`, `peer_name`, `error`, `latency_ms` |
| `fleet_received` | This node received a fleet job from a peer | `requester_node_id`, `rule_name`, `resolved_model` |
| `cloud_returned` | Cloud API returned a result | `provider_id`, `latency_ms`, `tokens_prompt`, `tokens_completion`, `cost_usd`, `generation_id`, `actual_cost_usd` |
| `market_matched` | Exchange matched this node's job to a provider | `provider_node_id`, `reservation_fee_credits`, `matched_rate_in_per_m`, `matched_rate_out_per_m`, `queue_discount_bps` |
| `market_settled` | Settlement completed for a market job | `actual_credits`, `refund_credits`, `settlement_latency_ms` |
| `market_received` | This node received a market job via exchange | `requester_job_path`, `matched_rate_in_per_m`, `matched_rate_out_per_m` |

**Cloud lifecycle note:** Cloud calls are return-only in v1. There is no `cloud_dispatched` event — only `cloud_returned` (on success) and `failed` (on error). Cloud calls bypass the compute queue entirely (they go through the provider pool), so there is no enqueue/started lifecycle. A `cloud_dispatched` event (emitted before the HTTP call) would give cloud calls the same pre/post visibility as fleet dispatches, but this requires instrumenting the retry loop entry point. Deferred to a follow-up when the cloud dispatch path is consolidated.

### Job ID Generation

`job_path` is a semantic path string generated at the earliest lifecycle point via `generate_job_path()`. For DADBEAR work: the work item's semantic path (`{slug}:{epoch}:{primitive}:{layer}:{target_id}`) is used directly. For non-DADBEAR local calls: derived from StepContext (`{slug}:{build_short}:{step_name}:d{depth}`). For fleet_received: generated ONCE in `handle_fleet_dispatch` (server.rs, WP-7) and passed through to the QueueEntry as `chronicle_job_path: Some(job_path)` so that WP-1/2/3/4 all share the same job_path. For fleet dispatch: generated at the fleet dispatch site (WP-5). For cloud: generated at the cloud return site (WP-8), or threaded from the queue via `LlmCallOptions.chronicle_job_path`. No UUIDs — all paths are human-readable.

The `job_path` is added to `QueueEntry` as a new field so it flows through the GPU loop and back to the completed/failed events. The `chronicle_job_path: Option<String>` field on QueueEntry allows pre-assignment of job_path from upstream handlers (fleet_received), preventing one logical job from fragmenting into multiple unrelated job_paths.

### Write Helper

New module: `src-tauri/src/pyramid/compute_chronicle.rs`

```rust
use anyhow::Result;
use rusqlite::{params, Connection};
use serde_json::Value as JsonValue;

/// All context needed to record a chronicle event.
/// Constructed from StepContext (when available) + queue/dispatch metadata.
pub struct ChronicleEventContext {
    pub job_path: String,
    pub event_type: String,
    pub timestamp: String,             // ISO 8601, captured at the event site via chrono::Utc::now()
    pub source: String,                // local, fleet, cloud, fleet_received, market, market_received
    // Technical
    pub model_id: Option<String>,
    // Pyramid context
    pub slug: Option<String>,
    pub build_id: Option<String>,
    // Task context (from StepContext + chain executor)
    pub chain_name: Option<String>,    // "code-mechanical", "conversation-episodic"
    pub content_type: Option<String>,  // "code", "document", "conversation"
    pub step_name: Option<String>,     // "extract_l0", "summarize_cluster"
    pub primitive: Option<String>,     // "single", "for_each", "recursive_cluster"
    pub depth: Option<i64>,            // pyramid layer: 0=L0, 1=L1
    pub task_label: Option<String>,    // "L1 Clustering (code-mechanical)"
    // Event-specific
    pub metadata: Option<serde_json::Value>,
}

impl ChronicleEventContext {
    /// Build from a StepContext (local builds, fleet dispatches — rich context).
    /// Captures `chrono::Utc::now()` at construction time so the timestamp reflects
    /// actual event time, not async write execution time.
    pub fn from_step_ctx(ctx: &StepContext, job_path: &str, event_type: &str, source: &str) -> Self {
        Self {
            job_path: job_path.to_string(),
            event_type: event_type.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            source: source.to_string(),
            model_id: ctx.resolved_model_id.clone(),
            slug: Some(ctx.slug.clone()),
            build_id: Some(ctx.build_id.clone()),
            chain_name: Some(ctx.chain_name.clone()),
            content_type: Some(ctx.content_type.clone()),
            step_name: Some(ctx.step_name.clone()),
            primitive: Some(ctx.primitive.clone()),
            depth: Some(ctx.depth),
            task_label: Some(ctx.task_label.clone()),
            metadata: None,
        }
    }

    /// Build with minimal context (fleet-received, market-received — no local build ctx).
    /// Captures `chrono::Utc::now()` at construction time.
    pub fn minimal(job_path: &str, event_type: &str, source: &str) -> Self {
        Self {
            job_path: job_path.to_string(),
            event_type: event_type.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            source: source.to_string(),
            model_id: None, slug: None, build_id: None,
            chain_name: None, content_type: None, step_name: None,
            primitive: None, depth: None, task_label: None,
            metadata: None,
        }
    }

    pub fn with_model_id(mut self, model_id: String) -> Self {
        self.model_id = Some(model_id);
        self
    }

    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

/// Record a single compute event. Append-only — never updates or deletes.
/// Generate a semantic job_path from available context.
/// For DADBEAR work: uses entry.work_item_id (already a semantic path).
/// For non-DADBEAR work: derives from StepContext or queue entry metadata.
/// NO UUIDs — paths are human-readable and LLM-parseable.
pub fn generate_job_path(ctx: Option<&StepContext>, entry: &QueueEntry) -> String {
    // DADBEAR already set a semantic path
    if let Some(ref wid) = entry.work_item_id {
        return wid.clone();
    }
    // Derive from StepContext (local builds, stale checks)
    if let Some(c) = ctx {
        let build_short = if c.build_id.len() > 8 { &c.build_id[..8] } else { &c.build_id };
        return format!("{}:{}:{}:d{}", c.slug, build_short, c.step_name, c.depth);
    }
    // Fleet received (no ctx, no work_item_id)
    if entry.source == "fleet_received" {
        let ts = chrono::Utc::now().timestamp();
        return format!("fleet-recv:{}:{}", entry.model_id, ts);
    }
    // Fallback: model + timestamp (always readable)
    let ts = chrono::Utc::now().timestamp();
    format!("anon:{}:{}", entry.model_id, ts)
}

/// Takes `&ChronicleEventContext` only — no positional parameters.
/// Timestamp is passed explicitly from the context (captured at event site),
/// NOT relying on SQLite DEFAULT, to ensure it reflects actual event time.
pub fn record_event(conn: &Connection, ctx: &ChronicleEventContext) -> Result<i64> {
    conn.execute(
        "INSERT INTO pyramid_compute_events
            (job_path, event_type, timestamp, model_id, source, slug, build_id,
             chain_name, content_type, step_name, primitive, depth, task_label, metadata)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            ctx.job_path,
            ctx.event_type,
            ctx.timestamp,
            ctx.model_id,
            ctx.source,
            ctx.slug,
            ctx.build_id,
            ctx.chain_name,
            ctx.content_type,
            ctx.step_name,
            ctx.primitive,
            ctx.depth,
            ctx.task_label,
            ctx.metadata.as_ref().map(|m| m.to_string()),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Query filters — every indexed column is a filterable dimension.
pub struct ChronicleQueryFilters {
    pub slug: Option<String>,
    pub build_id: Option<String>,
    pub chain_name: Option<String>,
    pub content_type: Option<String>,
    pub step_name: Option<String>,
    pub primitive: Option<String>,
    pub depth: Option<i64>,
    pub model_id: Option<String>,
    pub source: Option<String>,
    pub event_type: Option<String>,
    pub after: Option<String>,       // ISO timestamp
    pub before: Option<String>,      // ISO timestamp
    pub limit: i64,
    pub offset: i64,
}

/// Query compute events with optional filters. Every field is a filterable
/// dimension — combine any subset for precise queries like "fleet-dispatched
/// L0 extraction steps for opt-025 this week."
pub fn query_events(
    conn: &Connection,
    filters: &ChronicleQueryFilters,
) -> Result<Vec<ComputeEvent>> {
    let mut sql = String::from(
        "SELECT id, job_path, event_type, timestamp, model_id, slug,
                build_id, chain_name, content_type, step_name, primitive,
                depth, task_label, source, metadata
         FROM pyramid_compute_events WHERE 1=1"
    );
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut param_idx = 1;

    // Macro for optional filter columns
    macro_rules! add_filter {
        ($field:expr, $col:expr) => {
            if let Some(ref val) = $field {
                sql.push_str(&format!(" AND {} = ?{}", $col, param_idx));
                param_values.push(Box::new(val.clone()));
                param_idx += 1;
            }
        };
    }

    add_filter!(filters.slug, "slug");
    add_filter!(filters.build_id, "build_id");
    add_filter!(filters.chain_name, "chain_name");
    add_filter!(filters.content_type, "content_type");
    add_filter!(filters.step_name, "step_name");
    add_filter!(filters.primitive, "primitive");
    add_filter!(filters.model_id, "model_id");
    add_filter!(filters.source, "source");
    add_filter!(filters.event_type, "event_type");

    if let Some(ref d) = filters.depth {
        sql.push_str(&format!(" AND depth = ?{}", param_idx));
        param_values.push(Box::new(*d));
        param_idx += 1;
    }
    if let Some(ref a) = filters.after {
        sql.push_str(&format!(" AND timestamp >= ?{}", param_idx));
        param_values.push(Box::new(a.clone()));
        param_idx += 1;
    }
    if let Some(ref b) = filters.before {
        sql.push_str(&format!(" AND timestamp <= ?{}", param_idx));
        param_values.push(Box::new(b.clone()));
        param_idx += 1;
    }

    sql.push_str(" ORDER BY timestamp DESC");
    sql.push_str(&format!(" LIMIT ?{} OFFSET ?{}", param_idx, param_idx + 1));
    param_values.push(Box::new(filters.limit));
    param_values.push(Box::new(filters.offset));

    let params_refs: Vec<&dyn rusqlite::types::ToSql> =
        param_values.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&sql)?;
    // Use named column access (r.get::<_, T>("col")) to avoid index fragility.
    // All 15 columns are read — matches the SELECT list and ComputeEvent struct.
    let rows = stmt.query_map(params_refs.as_slice(), |r| {
        Ok(ComputeEvent {
            id: r.get("id")?,
            job_path: r.get("job_path")?,
            event_type: r.get("event_type")?,
            timestamp: r.get("timestamp")?,
            model_id: r.get("model_id")?,
            source: r.get("source")?,
            slug: r.get("slug")?,
            build_id: r.get("build_id")?,
            chain_name: r.get("chain_name")?,
            content_type: r.get("content_type")?,
            step_name: r.get("step_name")?,
            primitive: r.get("primitive")?,
            depth: r.get("depth")?,
            task_label: r.get("task_label")?,
            metadata: r.get::<_, Option<String>>("metadata")?
                .and_then(|s| serde_json::from_str(&s).ok()),
        })
    })?;

    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Summary query: aggregated stats for a time period grouped by a dimension.
pub fn query_summary(
    conn: &Connection,
    period_start: &str,
    period_end: &str,
    group_by: &str,   // "model", "source", "slug", "hour"
) -> Result<Vec<ComputeSummary>> {
    // Implementation uses json_extract for metadata fields.
    // Each group_by dimension produces a different SQL query.
    // See IPC Commands section for the full contract.
    todo!("Implementation in Step 9")
}

/// Timeline query: bucketed event counts for visualization.
pub fn query_timeline(
    conn: &Connection,
    start: &str,
    end: &str,
    bucket_size_minutes: i64,
) -> Result<Vec<TimelineBucket>> {
    // Uses strftime to bucket timestamps, counts events per bucket,
    // extracts latency_ms and cost_usd from metadata for aggregation.
    todo!("Implementation in Step 8")
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ComputeEvent {
    pub id: i64,
    pub job_path: String,
    pub event_type: String,
    pub timestamp: String,
    pub model_id: Option<String>,
    pub source: String,
    pub slug: Option<String>,
    pub build_id: Option<String>,
    pub chain_name: Option<String>,
    pub content_type: Option<String>,
    pub step_name: Option<String>,
    pub primitive: Option<String>,
    pub depth: Option<i64>,
    pub task_label: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ComputeSummary {
    pub group_key: String,
    pub total_events: i64,
    pub completed_count: i64,
    pub failed_count: i64,
    pub total_latency_ms: i64,
    pub avg_latency_ms: f64,
    pub p95_latency_ms: i64,          // Computed in Rust (sort + index), not SQL — SQLite has no native percentile
    pub total_tokens_prompt: i64,
    pub total_tokens_completion: i64,
    pub total_cost_usd: f64,
    pub fleet_count: i64,
    pub local_count: i64,
    pub cloud_count: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TimelineBucket {
    pub bucket_start: String,
    pub bucket_end: String,
    pub event_count: i64,
    pub completed_count: i64,
    pub avg_latency_ms: f64,
    pub total_cost_usd: f64,
    pub by_source: std::collections::HashMap<String, i64>,
}
```

---

## IV. Layer 2 — Materialized Summaries

SQLite views built on top of `pyramid_compute_events` using `json_extract()`. Views are lazy (computed on query), not materialized tables. If query performance degrades at scale, the views can be replaced with periodically-rebuilt summary tables using the same schema.

### View: Per-model hourly stats

```sql
CREATE VIEW IF NOT EXISTS v_compute_hourly_by_model AS
SELECT
    strftime('%Y-%m-%d %H:00:00', timestamp) AS hour,
    model_id,
    source,
    COUNT(*) FILTER (WHERE event_type = 'completed') AS completed,
    COUNT(*) FILTER (WHERE event_type = 'failed') AS failed,
    AVG(CAST(json_extract(metadata, '$.latency_ms') AS REAL))
        FILTER (WHERE event_type = 'completed') AS avg_latency_ms,
    SUM(CAST(json_extract(metadata, '$.tokens_prompt') AS INTEGER))
        FILTER (WHERE event_type = 'completed') AS total_tokens_in,
    SUM(CAST(json_extract(metadata, '$.tokens_completion') AS INTEGER))
        FILTER (WHERE event_type = 'completed') AS total_tokens_out,
    SUM(CAST(json_extract(metadata, '$.cost_usd') AS REAL))
        FILTER (WHERE event_type IN ('completed', 'cloud_returned')) AS total_cost_usd
FROM pyramid_compute_events
GROUP BY hour, model_id, source;
```

**Note on SQLite FILTER clause:** SQLite 3.30+ supports `FILTER (WHERE ...)` on aggregates. The `rusqlite` bundled SQLite version satisfies this. If not, replace with `CASE WHEN ... END` inside the aggregate.

### View: Per-pyramid build stats

```sql
CREATE VIEW IF NOT EXISTS v_compute_by_build AS
SELECT
    slug,
    build_id,
    chain_name,
    content_type,
    COUNT(*) FILTER (WHERE event_type = 'completed') AS total_calls,
    SUM(CAST(json_extract(metadata, '$.latency_ms') AS INTEGER))
        FILTER (WHERE event_type = 'completed') AS total_gpu_ms,
    AVG(CAST(json_extract(metadata, '$.queue_wait_ms') AS REAL))
        FILTER (WHERE event_type = 'started') AS avg_queue_wait_ms,
    COUNT(*) FILTER (WHERE source = 'fleet') AS fleet_steps,
    COUNT(*) FILTER (WHERE source = 'local') AS local_steps,
    COUNT(*) FILTER (WHERE source = 'cloud') AS cloud_steps,
    GROUP_CONCAT(DISTINCT model_id) AS models_used,
    SUM(CAST(json_extract(metadata, '$.cost_usd') AS REAL))
        FILTER (WHERE event_type IN ('completed', 'cloud_returned')) AS total_cost_usd,
    MIN(timestamp) AS started_at,
    MAX(timestamp) AS finished_at
FROM pyramid_compute_events
WHERE slug IS NOT NULL AND build_id IS NOT NULL
GROUP BY slug, build_id, chain_name, content_type;
```

### View: Per-layer depth breakdown

```sql
CREATE VIEW IF NOT EXISTS v_compute_by_depth AS
SELECT
    slug,
    build_id,
    depth,
    primitive,
    COUNT(*) FILTER (WHERE event_type = 'completed') AS step_count,
    SUM(CAST(json_extract(metadata, '$.latency_ms') AS INTEGER))
        FILTER (WHERE event_type = 'completed') AS total_gpu_ms,
    AVG(CAST(json_extract(metadata, '$.latency_ms') AS REAL))
        FILTER (WHERE event_type = 'completed') AS avg_latency_ms,
    SUM(CAST(json_extract(metadata, '$.tokens_completion') AS INTEGER))
        FILTER (WHERE event_type = 'completed') AS total_tokens_out,
    GROUP_CONCAT(DISTINCT source) AS sources_used
FROM pyramid_compute_events
WHERE slug IS NOT NULL AND depth IS NOT NULL
GROUP BY slug, build_id, depth, primitive;
```

This tells you: "L0 extraction: 150 steps, 45 min total GPU, all local. L1 clustering: 12 steps, 8 min, 50% fleet."

### View: Per-peer fleet stats

```sql
CREATE VIEW IF NOT EXISTS v_compute_fleet_peers AS
SELECT
    json_extract(metadata, '$.peer_id') AS peer_id,
    COUNT(*) FILTER (WHERE event_type = 'fleet_dispatched') AS dispatch_count,
    COUNT(*) FILTER (WHERE event_type = 'fleet_returned') AS success_count,
    COUNT(*) FILTER (WHERE event_type = 'fleet_dispatch_failed') AS failed_count,
    ROUND(
        CAST(COUNT(*) FILTER (WHERE event_type = 'fleet_returned') AS REAL) /
        NULLIF(COUNT(*) FILTER (WHERE event_type = 'fleet_dispatched'), 0) * 100,
        1
    ) AS success_rate_pct,
    AVG(CAST(json_extract(metadata, '$.latency_ms') AS REAL))
        FILTER (WHERE event_type = 'fleet_returned') AS avg_round_trip_ms,
    GROUP_CONCAT(DISTINCT model_id) AS models_served
FROM pyramid_compute_events
WHERE source = 'fleet'
GROUP BY peer_id;
```

### View: Per-source aggregates

```sql
CREATE VIEW IF NOT EXISTS v_compute_by_source AS
SELECT
    source,
    COUNT(*) FILTER (WHERE event_type IN ('completed', 'fleet_returned', 'cloud_returned')) AS total_completed,
    SUM(CAST(json_extract(metadata, '$.cost_usd') AS REAL))
        FILTER (WHERE event_type IN ('completed', 'cloud_returned')) AS total_cost_usd,
    AVG(CAST(json_extract(metadata, '$.latency_ms') AS REAL))
        FILTER (WHERE event_type IN ('completed', 'fleet_returned', 'cloud_returned')) AS avg_latency_ms,
    SUM(CAST(json_extract(metadata, '$.tokens_prompt') AS INTEGER))
        FILTER (WHERE event_type IN ('completed', 'fleet_returned', 'cloud_returned')) AS total_tokens_in,
    SUM(CAST(json_extract(metadata, '$.tokens_completion') AS INTEGER))
        FILTER (WHERE event_type IN ('completed', 'fleet_returned', 'cloud_returned')) AS total_tokens_out
FROM pyramid_compute_events
GROUP BY source;
```

**Creation:** All four views are created in `init_pyramid_db` (`db.rs`) after the table creation, using the same `CREATE VIEW IF NOT EXISTS` pattern.

---

## V. Write Points — Where Events Are Emitted

Each write point opens a fresh SQLite connection to the pyramid DB (same pattern as the cost log: `Connection::open(db_path)`). The chronicle writes are non-blocking to the build pipeline — they happen as fire-and-forget inserts. Failures are logged via `tracing::warn!` but never propagate errors to callers.

### WP-1: Enqueue (source: local)

**File:** `src-tauri/src/pyramid/llm.rs`
**Function:** `call_model_unified_with_options_and_ctx`
**Location:** Inside the `if let Some(ref queue_handle) = config.compute_queue` block, after `q.enqueue_local(...)` returns (line ~667-695 region).
**Trigger:** Every local LLM call that enters the compute queue.

```rust
// After enqueue_local returns, before dropping the queue lock:
// Use the pre-set chronicle_job_path if present (fleet_received path),
// otherwise generate a semantic path (local enqueue path).
let job_path = entry.chronicle_job_path.clone()
    .unwrap_or_else(|| generate_job_path(ctx, &entry));
let depth = q.queue_depth(&queue_model_id);
// Drop queue lock, then record via spawn_blocking (no conn in scope):
let db_path = ctx.as_ref().map(|c| c.db_path.clone())
    .or_else(|| config.cache_access.as_ref().map(|ca| ca.db_path.to_string()));
let chronicle_ctx = if let Some(ref sc) = ctx {
    ChronicleEventContext::from_step_ctx(sc, &job_path, "enqueued", "local")
} else {
    ChronicleEventContext::minimal(&job_path, "enqueued", "local")
        .with_model_id(queue_model_id.clone())
};
let chronicle_ctx = chronicle_ctx.with_metadata(serde_json::json!({
    "queue_depth": depth,
    "queue_model_depth": depth,
}));
if let Some(db_path) = db_path {
    tokio::task::spawn_blocking(move || {
        if let Ok(conn) = rusqlite::Connection::open(&db_path) {
            let _ = compute_chronicle::record_event(&conn, &chronicle_ctx);
        }
    });
}
```

**Changes to QueueEntry** (in `compute_queue.rs`):
- Add `pub job_path: String` — generated as `generate_job_path(ctx, &entry)` at enqueue time.
- Add `pub source: String` — set explicitly at the enqueue site. Values: `"local"` at the enqueue site in `llm.rs`, `"fleet_received"` in `handle_fleet_dispatch` (server.rs), future `"market_received"` in market handler. The GPU loop reads `entry.source` directly instead of inferring source from `step_ctx.is_some()`.
- Add `pub chronicle_job_path: Option<String>` — when `Some`, the enqueue site (WP-1) uses this value instead of generating a new path. This is how fleet_received jobs keep a single job_path across the entire lifecycle: the fleet dispatch handler (server.rs) generates the job_path once, records the `fleet_received` event (WP-7), then passes it as `chronicle_job_path: Some(job_path)` on the QueueEntry. WP-1 checks `entry.chronicle_job_path.clone().unwrap_or_else(|| generate_job_path(ctx, &entry))`. Local enqueue sites set `chronicle_job_path: None` (WP-1 generates a semantic path via `generate_job_path` as before).

### WP-2: Started (source: local, fleet_received)

**File:** `src-tauri/src/main.rs`
**Function:** GPU processing loop (line ~11430-11444 region)
**Location:** After `QueueJobStarted` event emission, before `call_model_unified_with_audit_and_ctx`.
**Trigger:** Every queue item picked up by the GPU loop.

```rust
// After QueueJobStarted event, before LLM call:
let queue_wait_ms = entry.enqueued_at.elapsed().as_millis() as u64;
let source = entry.source.as_str(); // explicit source field on QueueEntry
// Record via spawn_blocking to avoid blocking the GPU loop:
let db_path = pyramid_db_path_clone.clone();
let chronicle_ctx = if let Some(ref sc) = entry.step_ctx {
    ChronicleEventContext::from_step_ctx(sc, &entry.job_path, "started", source)
} else {
    ChronicleEventContext::minimal(&entry.job_path, "started", source)
        .with_model_id(model_id.clone())
};
let chronicle_ctx = chronicle_ctx.with_metadata(serde_json::json!({
    "queue_wait_ms": queue_wait_ms,
}));
tokio::task::spawn_blocking(move || {
    if let Ok(conn) = rusqlite::Connection::open(&db_path) {
        let _ = compute_chronicle::record_event(&conn, &chronicle_ctx);
    }
});
```

### WP-3: Completed (source: local, fleet_received)

**File:** `src-tauri/src/main.rs`
**Function:** GPU processing loop (line ~11481-11490 region)
**Location:** After `QueueJobCompleted` event emission, before `entry.result_tx.send(result)`.
**Trigger:** Successful completion of a queue item.

The GPU loop currently only has `model_id` and `latency_ms` at this point. To record tokens and cost, the `result` must be inspected. The result is `anyhow::Result<LlmResponse>`.

```rust
// After QueueJobCompleted, before result_tx.send:
if let Ok(ref response) = result {
    let source = entry.source.as_str(); // explicit source field on QueueEntry
    let db_path = pyramid_db_path_clone.clone();
    let chronicle_ctx = if let Some(ref sc) = entry.step_ctx {
        ChronicleEventContext::from_step_ctx(sc, &entry.job_path, "completed", source)
    } else {
        ChronicleEventContext::minimal(&entry.job_path, "completed", source)
            .with_model_id(model_id.clone())
    };
    let chronicle_ctx = chronicle_ctx.with_metadata(serde_json::json!({
        "latency_ms": elapsed_ms,
        "tokens_prompt": response.usage.prompt_tokens,
        "tokens_completion": response.usage.completion_tokens,
        "cost_usd": response.actual_cost_usd,
        "generation_id": response.generation_id,
    }));
    tokio::task::spawn_blocking(move || {
        if let Ok(conn) = rusqlite::Connection::open(&db_path) {
            let _ = compute_chronicle::record_event(&conn, &chronicle_ctx);
        }
    });
}
```

### WP-4: Failed (source: local, fleet_received)

**File:** `src-tauri/src/main.rs`
**Function:** GPU processing loop, same location as WP-3.
**Trigger:** LLM call error or panic in the GPU loop.

```rust
if let Err(ref e) = result {
    let source = entry.source.as_str(); // explicit source field on QueueEntry
    let db_path = pyramid_db_path_clone.clone();
    let chronicle_ctx = if let Some(ref sc) = entry.step_ctx {
        ChronicleEventContext::from_step_ctx(sc, &entry.job_path, "failed", source)
    } else {
        ChronicleEventContext::minimal(&entry.job_path, "failed", source)
            .with_model_id(model_id.clone())
    };
    let chronicle_ctx = chronicle_ctx.with_metadata(serde_json::json!({
        "error": e.to_string(),
        "latency_ms": elapsed_ms,
    }));
    tokio::task::spawn_blocking(move || {
        if let Ok(conn) = rusqlite::Connection::open(&db_path) {
            let _ = compute_chronicle::record_event(&conn, &chronicle_ctx);
        }
    });
}
```

### WP-5: Fleet Dispatched (source: fleet)

**File:** `src-tauri/src/pyramid/llm.rs`
**Function:** `call_model_unified_with_options_and_ctx`, Phase A fleet handling
**Location:** Just before the `fleet_dispatch_by_rule` call (line ~819 region).
**Trigger:** Every fleet dispatch attempt.

```rust
// Before fleet_dispatch_by_rule:
let job_path = generate_job_path(ctx, &entry);
// Derive db_path from ctx or config.cache_access (no conn variable in scope in llm.rs):
let db_path = ctx.as_ref().map(|c| c.db_path.clone())
    .or_else(|| config.cache_access.as_ref().map(|ca| ca.db_path.to_string()));
let chronicle_ctx = if let Some(ref sc) = ctx {
    ChronicleEventContext::from_step_ctx(sc, &job_path, "fleet_dispatched", "fleet")
} else {
    ChronicleEventContext::minimal(&job_path, "fleet_dispatched", "fleet")
        .with_model_id(model_id.clone())
};
let chronicle_ctx = chronicle_ctx.with_metadata(serde_json::json!({
    "peer_id": peer_clone.node_id,
    "peer_name": peer_clone.name,
    "rule_name": rule_name,
    "timeout_secs": fleet_timeout_secs,
}));
if let Some(db_path) = db_path.clone() {
    let chronicle_ctx = chronicle_ctx.clone();
    tokio::task::spawn_blocking(move || {
        if let Ok(conn) = rusqlite::Connection::open(&db_path) {
            let _ = compute_chronicle::record_event(&conn, &chronicle_ctx);
        }
    });
}
```

### WP-6: Fleet Returned (source: fleet)

**File:** `src-tauri/src/pyramid/llm.rs`
**Function:** `call_model_unified_with_options_and_ctx`, Phase A fleet handling
**Location:** Inside the `Ok(fleet_resp)` branch of the fleet dispatch match (line ~836 region).
**Trigger:** Successful fleet dispatch response.

```rust
// Inside Ok(fleet_resp):
// db_path already derived above (before fleet_dispatch_by_rule) via ctx or config.cache_access
let chronicle_ctx = if let Some(ref sc) = ctx {
    ChronicleEventContext::from_step_ctx(sc, &job_path, "fleet_returned", "fleet")
} else {
    ChronicleEventContext::minimal(&job_path, "fleet_returned", "fleet")
        .with_model_id(model_id.clone())
};
let chronicle_ctx = chronicle_ctx.with_metadata(serde_json::json!({
    "peer_id": peer_clone.node_id,
    "peer_name": peer_clone.name,
    "peer_model": fleet_resp.peer_model,
    "latency_ms": fleet_start.elapsed().as_millis() as u64,
    "tokens_prompt": fleet_resp.prompt_tokens.unwrap_or(0),
    "tokens_completion": fleet_resp.completion_tokens.unwrap_or(0),
}));
if let Some(ref db_path) = db_path {
    let db_path = db_path.clone();
    tokio::task::spawn_blocking(move || {
        if let Ok(conn) = rusqlite::Connection::open(&db_path) {
            let _ = compute_chronicle::record_event(&conn, &chronicle_ctx);
        }
    });
}
```

**Note:** `fleet_start` is a new `Instant::now()` captured just before `fleet_dispatch_by_rule`. Token counts from fleet responses may be `None` (peer did not report them); use `.unwrap_or(0)` to avoid null metadata fields. The `db_path` is derived once before the dispatch call (from ctx or config.cache_access) and reused for both WP-5 and WP-6.

### WP-7: Fleet Received (source: fleet_received)

**File:** `src-tauri/src/server.rs`
**Function:** `handle_fleet_dispatch` (line ~1474)
**Location:** After request parsing succeeds, before enqueue to the compute queue (line ~1588).
**Trigger:** Every fleet dispatch received from a peer.

```rust
// After request validation, before enqueue to compute queue:
// Generate the job_path ONCE here. This same job_path flows through the QueueEntry
// (via chronicle_job_path) to WP-1/2/3/4, so one logical fleet job has one job_path
// across its entire lifecycle (fleet_received -> enqueued -> started -> completed).
let job_path = generate_job_path(ctx, &entry);
let db_path = state.pyramid.data_dir.as_ref().map(|d| d.join("pyramid.db"));
if let Some(ref db_path) = db_path {
    let chronicle_ctx = ChronicleEventContext::minimal(&job_path, "fleet_received", "fleet_received")
        .with_model_id(resolved_model.clone())
        .with_metadata(serde_json::json!({
            "requester_node_id": claims.sub.unwrap_or_default(),
            "rule_name": rule_name,
            "resolved_model": resolved_model,
        }));
    let db_path_clone = db_path.clone();
    tokio::task::spawn_blocking(move || {
        if let Ok(conn) = rusqlite::Connection::open(&db_path_clone) {
            let _ = compute_chronicle::record_event(&conn, &chronicle_ctx);
        }
    });
}
// Pass job_path through to the QueueEntry so GPU loop events share the same job_path:
// Set chronicle_job_path: Some(job_path.clone()) on the QueueEntry.
```

**Note:** The fleet handler currently passes `None` for StepContext (line 1592: `None, // Fleet jobs have no StepContext`). The `fleet_received` source confirms this node did work for a peer. The enqueued/started/completed events for this job are recorded by WP-2/WP-3/WP-4 in the GPU loop with `source: "fleet_received"` (read from the explicit `entry.source` field on `QueueEntry`, set at enqueue time in `handle_fleet_dispatch`). The job_path is preserved because `chronicle_job_path: Some(job_path)` is set on the QueueEntry, and WP-1 uses it instead of generating a new path.

### WP-8: Cloud Returned (source: cloud)

**File:** `src-tauri/src/pyramid/llm.rs`
**Function:** `call_model_unified_with_options_and_ctx`
**Location:** After `build_call_provider` returns and before the HTTP POST (the retry loop entry, line ~1038 region). Also in `call_model_via_registry` (line ~2222 region) and `call_model_direct` (line ~2757 region).

**Strategy:** Cloud calls are return-only in v1 (see Cloud lifecycle note in Event Types). Record `cloud_returned` in the success path adjacent to the existing `emit_llm_call_completed` call. Since ALL non-fleet non-local calls go through `build_call_provider` which resolves to either OpenRouter or a registered provider, and since the `LlmResponse` already carries `provider_id` and `actual_cost_usd`, the cloud events can be recorded in the SAME success/failure path. Use the `is_local` flag from `RouteEntry` to determine if this is a cloud call.

**Concrete location (primary path):** `src-tauri/src/pyramid/llm.rs`, line ~1395, after `emit_llm_call_completed`. Check `!route_entry.is_local` and record a `cloud_returned` event.

**Concrete location (registry path):** `src-tauri/src/pyramid/llm.rs`, line ~2457, same pattern.

**Excluded: direct path** (`call_model_direct`, line ~2857 region) — returns `String` not `LlmResponse`, so it lacks structured usage/provider fields. Excluded from WP-8 until migrated.

```rust
// After emit_llm_call_completed, in the success path:
// Cloud detection: use RouteEntry.is_local flag rather than negative string matching.
// is_local is true for Ollama and local GPU; false for OpenRouter and registered cloud providers.
if !route_entry.is_local {
    // Use pre-assigned job_path from the queue (via LlmCallOptions) if available.
    // This prevents a queued job that falls through to cloud from generating
    // a fresh job_path, which would break lifecycle grouping with the queue events.
    let job_path = options.chronicle_job_path.clone()
        .unwrap_or_else(|| generate_job_path(ctx, &entry));
    let chronicle_ctx = if let Some(ref sc) = ctx {
        ChronicleEventContext::from_step_ctx(sc, &job_path, "cloud_returned", "cloud")
    } else {
        ChronicleEventContext::minimal(&job_path, "cloud_returned", "cloud")
            .with_model_id(use_model.clone())
    };
    let chronicle_ctx = chronicle_ctx.with_metadata(serde_json::json!({
        "provider_id": response.provider_id,
        "latency_ms": latency_ms,
        "tokens_prompt": response.usage.prompt_tokens,
        "tokens_completion": response.usage.completion_tokens,
        "cost_usd": cost_usd,
        "generation_id": response.generation_id,
        "actual_cost_usd": response.actual_cost_usd,
    }));
    let db_path = ctx.as_ref().map(|c| c.db_path.clone())
        .or_else(|| config.cache_access.as_ref().map(|ca| ca.db_path.to_string()));
    if let Some(db_path) = db_path {
        tokio::task::spawn_blocking(move || {
            if let Ok(conn) = rusqlite::Connection::open(&db_path) {
                let _ = compute_chronicle::record_event(&conn, &chronicle_ctx);
            }
        });
    }
}
```

**Job ID for cloud events:** Cloud events may originate from the compute queue (a queued job that falls through to OpenRouter because the local GPU is busy or unsupported) or from standalone cloud calls. To preserve lifecycle grouping:
- **Queue-originated cloud calls:** The GPU loop sets `options.chronicle_job_path = Some(entry.job_path)` before calling the LLM function. WP-8 uses this value, keeping all events (enqueued, started, cloud_returned) under the same job_path.
- **Standalone cloud calls (no queue):** `options.chronicle_job_path` is `None`. WP-8 generates a semantic path via `generate_job_path`. This is acceptable because standalone cloud calls don't have the enqueue/started lifecycle.

**Changes to LlmCallOptions** (in `llm.rs`):
- Add `pub chronicle_job_path: Option<String>` — when `Some`, WP-8 uses this value instead of generating a new path. The GPU loop sets this from `entry.job_path` before calling the LLM function. All other callers leave it as `None` (default).

**Note on `call_model_direct`:** This function returns `String` (not `LlmResponse`), so it lacks the structured `usage` and `provider_id` fields needed for chronicle recording. Exclude `call_model_direct` from WP-8 in this phase. When `call_model_direct` is migrated to return `LlmResponse` (future cleanup), add chronicle recording at that time.

### WP-9: Market events (STUBS ONLY)

**File:** `src-tauri/src/pyramid/compute_chronicle.rs`
**Status:** Stub functions. Not called from anywhere yet. Built when market phases ship.

```rust
/// Stub: record market_matched event. Called from market exchange client (Phase 2).
/// Callers construct ChronicleEventContext::from_step_ctx(...) with event_type "market_matched"
/// and source "market", then .with_metadata(json!({ ... market match fields ... })).
pub fn record_market_matched(conn: &Connection, ctx: &ChronicleEventContext) -> Result<i64> {
    record_event(conn, ctx)
}

/// Stub: record market_settled event. Called from settlement handler (Phase 3).
pub fn record_market_settled(conn: &Connection, ctx: &ChronicleEventContext) -> Result<i64> {
    record_event(conn, ctx)
}

/// Stub: record market_received event. Called from market job handler (Phase 5).
/// Uses ChronicleEventContext::minimal(...) since received jobs have no local build context.
pub fn record_market_received(conn: &Connection, ctx: &ChronicleEventContext) -> Result<i64> {
    record_event(conn, ctx)
}
```

### DB Path Threading

The chronicle needs a `db_path` to open a connection. In the GPU loop (main.rs), `pyramid_db_path` is already in scope. In llm.rs, the `StepContext.db_path` field carries the path, or `config.cache_access.db_path` as fallback. For fleet_received in server.rs, derive from `state.pyramid.data_dir.as_ref().map(|d| d.join("pyramid.db"))` (the actual field is `data_dir: Option<PathBuf>`, NOT `db_path`). For IPC commands, same derivation from `state.pyramid.data_dir`. Every write point has a DB path already in scope — no new threading is needed, but the `data_dir` → `pyramid.db` join must be explicit and the `None` case must be handled (early return with error).

---

## VI. IPC Commands

Four Tauri IPC commands registered in `main.rs`. Each opens a read-only SQLite connection to the pyramid DB.

### `get_compute_events`

Exposes all 11 filter dimensions from `ChronicleQueryFilters`. The frontend can combine any subset for precise drill-downs like "fleet-dispatched L0 extraction steps for opt-025 this week."

```rust
#[tauri::command]
async fn get_compute_events(
    state: tauri::State<'_, SharedState>,
    // All 11 filter dimensions:
    slug: Option<String>,
    build_id: Option<String>,
    chain_name: Option<String>,
    content_type: Option<String>,
    step_name: Option<String>,
    primitive: Option<String>,
    depth: Option<i64>,
    model_id: Option<String>,
    source: Option<String>,
    event_type: Option<String>,
    after: Option<String>,
    before: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<compute_chronicle::ComputeEvent>, String> {
    let db_path = state.pyramid.data_dir.as_ref()
        .map(|d| d.join("pyramid.db"))
        .ok_or_else(|| "No pyramid data_dir configured".to_string())?;
    tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open(&db_path)
            .map_err(|e| e.to_string())?;
        let filters = compute_chronicle::ChronicleQueryFilters {
            slug,
            build_id,
            chain_name,
            content_type,
            step_name,
            primitive,
            depth,
            model_id,
            source,
            event_type,
            after,
            before,
            limit: limit.unwrap_or(100),
            offset: offset.unwrap_or(0),
        };
        compute_chronicle::query_events(&conn, &filters)
            .map_err(|e| e.to_string())
    }).await.map_err(|e| e.to_string())?
}
```

### `get_compute_summary`

```rust
#[tauri::command]
async fn get_compute_summary(
    state: tauri::State<'_, SharedState>,
    period_start: String,
    period_end: String,
    group_by: String,  // "model" | "source" | "slug" | "hour"
) -> Result<Vec<compute_chronicle::ComputeSummary>, String> {
    let db_path = state.pyramid.data_dir.as_ref()
        .map(|d| d.join("pyramid.db"))
        .ok_or_else(|| "No pyramid data_dir configured".to_string())?;
    tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open(&db_path)
            .map_err(|e| e.to_string())?;
        compute_chronicle::query_summary(
            &conn, &period_start, &period_end, &group_by,
        ).map_err(|e| e.to_string())
    }).await.map_err(|e| e.to_string())?
}
```

### `get_compute_timeline`

`bucket_size_minutes` is required — no hardcoded default. The frontend derives bucket size from the visible time range (e.g., 1h range -> 1min buckets, 24h range -> 15min buckets, 7d range -> 1h buckets). This avoids a Pillar 37 violation from baking in a 60-minute default.

```rust
#[tauri::command]
async fn get_compute_timeline(
    state: tauri::State<'_, SharedState>,
    start: String,
    end: String,
    bucket_size_minutes: i64,  // Required — derived from time range by the frontend
) -> Result<Vec<compute_chronicle::TimelineBucket>, String> {
    let db_path = state.pyramid.data_dir.as_ref()
        .map(|d| d.join("pyramid.db"))
        .ok_or_else(|| "No pyramid data_dir configured".to_string())?;
    tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open(&db_path)
            .map_err(|e| e.to_string())?;
        compute_chronicle::query_timeline(
            &conn, &start, &end, bucket_size_minutes,
        ).map_err(|e| e.to_string())
    }).await.map_err(|e| e.to_string())?
}
```

### `get_chronicle_dimensions`

Returns the set of distinct values for each filterable dimension. The frontend uses this to populate filter dropdowns (pyramid slugs, models, sources, chain_names, event_types) so the operator sees only values that actually exist in their data.

```rust
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChronicleDimensions {
    pub slugs: Vec<String>,
    pub models: Vec<String>,
    pub sources: Vec<String>,
    pub chain_names: Vec<String>,
    pub event_types: Vec<String>,
}

#[tauri::command]
async fn get_chronicle_dimensions(
    state: tauri::State<'_, SharedState>,
) -> Result<ChronicleDimensions, String> {
    let db_path = state.pyramid.data_dir.as_ref()
        .map(|d| d.join("pyramid.db"))
        .ok_or_else(|| "No pyramid data_dir configured".to_string())?;
    tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open(&db_path)
            .map_err(|e| e.to_string())?;
        // One query with multiple SELECT DISTINCT subqueries for efficiency.
        let slugs = query_distinct(&conn, "slug")?;
        let models = query_distinct(&conn, "model_id")?;
        let sources = query_distinct(&conn, "source")?;
        let chain_names = query_distinct(&conn, "chain_name")?;
        let event_types = query_distinct(&conn, "event_type")?;
        Ok(ChronicleDimensions { slugs, models, sources, chain_names, event_types })
    }).await.map_err(|e| e.to_string())?
}

/// Helper: SELECT DISTINCT non-null values for a column.
fn query_distinct(conn: &Connection, column: &str) -> Result<Vec<String>, String> {
    let sql = format!(
        "SELECT DISTINCT {} FROM pyramid_compute_events WHERE {} IS NOT NULL ORDER BY {}",
        column, column, column
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}
```

**Registration:** Add all four to the `.invoke_handler(tauri::generate_handler![...])` list in `main.rs` (line ~12498 region).

---

## VII. Layer 3 — Frontend: The Chronicle

### Component Structure

```
src/components/
  ComputeChronicle.tsx          — Main chronicle container (sub-tab of Market)
  ComputeChronicleTimeline.tsx  — Horizontal zoomable timeline
  ComputeChronicleTable.tsx     — Sortable, filterable, paginated event table
  ComputeChronicleStats.tsx     — Stats dashboard (throughput, cost, fleet savings)
  ComputeChronicleFilters.tsx   — Filter bar (pyramid, model, source, time range)
  ComputeFleetAnalytics.tsx     — Fleet dispatch analytics panel
```

### Main Chronicle View

Lives as a new sub-tab alongside the existing QueueLiveView on the Market tab. The MarketDashboard.tsx (or MarketMode.tsx) gets a tab switcher:

| Tab | Component | Description |
|---|---|---|
| Queue | `QueueLiveView` | Existing real-time queue state (ephemeral) |
| Chronicle | `ComputeChronicle` | Persistent compute history (this plan) |
| Fleet | `ComputeFleetAnalytics` | Fleet dispatch analytics |

### Timeline Visualization (`ComputeChronicleTimeline.tsx`)

Horizontal timeline. Zoomable via mouse wheel or preset buttons (hour / day / week / month). Each completed job renders as a horizontal bar:

- **Width** = GPU time (latency_ms), scaled to the time axis
- **Color by source:**
  - Cyan (`#00D4FF`) = local
  - Purple (`#A855F7`) = fleet
  - Gold (`#EAB308`) = market
  - Grey (`#6B7280`) = cloud
  - Dark cyan (`#0891B2`) = fleet_received
  - Dark gold (`#CA8A04`) = market_received
- **Vertical axis** = model_id (one lane per model)
- **Hover** shows: job_path, model, source, latency, tokens, cost, step_name, slug

Data fetched via `get_compute_timeline` IPC (bucketed for the zoom level). On zoom-in past a threshold, switches to `get_compute_events` for individual job bars.

### Stats Dashboard (`ComputeChronicleStats.tsx`)

Four stat cards at the top of the chronicle view:

1. **Throughput** — completed jobs per hour (last 24h trend sparkline)
2. **Utilization** — % of time the GPU was busy (computed from started→completed spans vs wall clock)
3. **Cost** — total dollars spent (cloud) + credits spent (market, future) in selected period
4. **Fleet Savings** — estimated dollars saved by fleet routing (fleet job count * average cloud cost for that model)

All computed client-side from `get_compute_summary` response.

### History Table (`ComputeChronicleTable.tsx`)

Sortable table showing individual events. Columns:

| Column | Sortable | Source |
|---|---|---|
| Timestamp | yes | `timestamp` |
| Job | no | truncated `job_path` (hover for full) |
| Type | yes | `event_type` with colored badge |
| Model | yes | `model_id` |
| Source | yes | `source` with colored dot |
| Pyramid | yes | `slug` |
| Step | yes | `step_name` |
| Latency | yes | `metadata.latency_ms` (ms) |
| Tokens | no | `metadata.tokens_prompt` + `metadata.tokens_completion` |
| Cost | yes | `metadata.cost_usd` |

Pagination: configurable page size (operator preference, stored in localStorage). Default 50 rows.

### Filters (`ComputeChronicleFilters.tsx`)

Filter bar above the timeline and table:

- **Pyramid** — dropdown of all slugs with events (populated from `get_chronicle_dimensions` IPC)
- **Model** — dropdown of all model_ids (populated from `get_chronicle_dimensions` IPC)
- **Source** — multi-select checkboxes (local, fleet, cloud, fleet_received, market, market_received)
- **Time range** — preset buttons (1h, 6h, 24h, 7d, 30d) + custom date picker
- **Event type** — multi-select (all event types)

Filters are passed as parameters to all three IPC commands.

### Pyramid Drill-Down

Clicking a pyramid name in the table or filter opens a drill-down panel showing all compute events for that pyramid's builds. Groups events by `build_id` with collapsible sections. Each build shows:
- Total GPU time, total cost, models used, source breakdown
- Individual events as a mini-timeline within the build

### Fleet Analytics Panel (`ComputeFleetAnalytics.tsx`)

Dedicated view for fleet routing health:

- **Dispatch success rate** — pie chart (dispatched vs returned)
- **Latency comparison** — bar chart: fleet avg latency vs local avg latency vs cloud avg latency (per model)
- **Per-peer health** — table of fleet peers with columns: peer_id, dispatches, successes, success_rate, avg_round_trip_ms, models_served
- **Fleet savings calculator** — estimated USD saved = (fleet completed count per model) * (avg cloud cost per call for same model). **Calculation method:** The frontend queries `cloud_returned` events grouped by `model_id` via `get_compute_events` (filter: `event_type = "cloud_returned"`) to compute average `cost_usd` per model from the metadata. Then multiplies by the count of `fleet_returned` events for each model. This gives a per-model savings estimate without needing a dedicated backend query. If no cloud calls exist for a model (pure fleet routing), the savings cannot be estimated and the UI shows "N/A" for that model.

Data from `v_compute_fleet_peers` view via `get_compute_summary` with `group_by: "source"`. Fleet savings computation is client-side using data from `get_compute_events`.

---

## VIII. Integration Points Summary

| System | Event | Write Point | Source |
|---|---|---|---|
| Compute queue (llm.rs) | enqueued | WP-1 | local |
| GPU loop (main.rs) | started | WP-2 | local, fleet_received |
| GPU loop (main.rs) | completed | WP-3 | local, fleet_received |
| GPU loop (main.rs) | failed | WP-4 | local, fleet_received |
| Fleet dispatch (llm.rs Phase A) | fleet_dispatched | WP-5 | fleet |
| Fleet dispatch (llm.rs Phase A) | fleet_returned | WP-6 | fleet |
| Fleet dispatch (llm.rs Phase A) | fleet_dispatch_failed | WP-6 Err branch | fleet |
| Fleet receive (server.rs) | fleet_received | WP-7 | fleet_received |
| Cloud return (llm.rs) | cloud_returned | WP-8 | cloud |
| Market exchange (future) | market_matched | WP-9 stub | market |
| Market settlement (future) | market_settled | WP-9 stub | market |
| Market receive (future) | market_received | WP-9 stub | market_received |

---

## IX. Implementation Steps

### Step 0: CacheAccess → StepContext Context Threading

**Prerequisite for all other steps.** The chronicle's narrative value depends on `chain_name`, `content_type`, and `task_label` being available at every write point. These fields exist in the chain executor (`ExecutionPlan.source_chain_id`, `ChainContext.content_type`) but are not threaded through to `StepContext` today. This step closes that gap.

**Files to modify:**

1. **`src-tauri/src/pyramid/llm.rs` — `CacheAccess` struct:**
   - Add `pub chain_name: Option<String>` and `pub content_type: Option<String>` fields. **These are Optional, not required.** Default to `None`. Only the chain executor path sets them to `Some`.
   - The `LlmConfig::clone_with_cache_access()` signature stays unchanged — `chain_name` and `content_type` default to `None` on the `CacheAccess`. Add a separate builder method `.with_chain_context(chain_name: String, content_type: String)` on `CacheAccess` that sets both fields. Only the chain executor call sites use this builder.

2. **`src-tauri/src/pyramid/mod.rs` and ALL callers of `clone_with_cache_access`:**
   - There are 5+ call sites, not 2: `mod.rs:923`, `mod.rs:951`, `evidence_answering.rs:980`, and stale engine paths in `main.rs` and `server.rs`.
   - **Only mod.rs:923 and mod.rs:951** (the chain executor) call `.with_chain_context()` to set `chain_name` (from `ExecutionPlan.source_chain_id`) and `content_type` (from `ChainContext.content_type`).
   - All other callers (`evidence_answering.rs`, `main.rs` stale paths, `server.rs`) pass `None` unchanged — they don't have `ChainContext` in scope and the Optional fields default to `None`.

3. **`src-tauri/src/step_context.rs` — `StepContext` struct:**
   - Add `pub chain_name: String`, `pub content_type: String`, `pub task_label: String` fields.
   - **All three fields default to empty string `"".to_string()`.** The `StepContext::new()` signature stays unchanged — the 3 new fields are initialized to empty strings automatically. This means all 30+ existing `StepContext::new()` call sites (across stale_helpers, chain_dispatch, evidence_answering, llm tests, routes, provider, generative_config, migration_config, reroll) compile unchanged with empty defaults.
   - Add builder method `.with_chain_context(chain_name: String, content_type: String)` that sets both fields and derives `task_label` mechanically. **Only `chain_dispatch.rs` and `evidence_answering.rs`** (which have `ChainContext` in scope) need to call `.with_chain_context()`. All other sites get empty defaults and don't call this builder.

   **Note:** This refers to `pyramid::step_context::StepContext` (build metadata), NOT `pyramid::chain_dispatch::StepContext` (dispatch context with DB handles). These are two different structs with the same name.

4. **`src-tauri/src/step_context.rs` — `make_step_ctx_from_llm_config` (~line 489):**
   - Populate `chain_name` and `content_type` from `CacheAccess` on the `LlmConfig`.
   - Derive `task_label` from `chain_name` + `step_name` + `depth` using conditional formatting: if `chain_name` is empty, format as `"{step_name} depth {depth}"`. If `chain_name` is set, format as `"{step_name} depth {depth} ({chain_name})"`. This prevents garbled empty parenthetical when chain_name is empty (stale checks, tests, etc.).
   - **Critical site:** `chain_dispatch.rs` is where real non-empty values must be set. This is the chain executor entry point that has `ExecutionPlan.source_chain_id` and `ChainContext.content_type` in scope.

5. **Stale engine call sites** (files that create `StepContext` for stale checks, not pyramid builds):
   - No changes needed. The new fields default to empty string `""` via `StepContext::new()`. This is correct — stale checks are not chain builds and should not carry chain context. The task_label derivation handles the empty chain_name case (see Step 4 above).

**Verification:** `cargo check` passes. A pyramid build's `StepContext` instances carry non-empty `chain_name` and `content_type` matching the selected chain strategy.

### Step 1: SQLite Table + Write Helper

**Files to modify:**
- `src-tauri/src/pyramid/db.rs` — Add `CREATE TABLE IF NOT EXISTS pyramid_compute_events` and all indexes to `init_pyramid_db`. Add all four `CREATE VIEW IF NOT EXISTS` statements.
- Create `src-tauri/src/pyramid/compute_chronicle.rs` — The `record_event`, `query_events` functions, and all struct definitions (`ComputeEvent`, `ComputeSummary`, `TimelineBucket`). `query_summary` and `query_timeline` can have `todo!()` bodies initially.
- `src-tauri/src/pyramid/mod.rs` — Add `pub mod compute_chronicle;`

**Verification:** `cargo check` passes. Opening the pyramid DB creates the new table and views.

### Step 2: QueueEntry job_path + source + Enqueue Event (WP-1)

**Files to modify:**
- `src-tauri/src/compute_queue.rs` — Add `pub job_path: String`, `pub source: String`, and `pub chronicle_job_path: Option<String>` to `QueueEntry`. When `chronicle_job_path` is `Some`, WP-1 uses that value as the job_path instead of generating a new path. This ensures fleet_received jobs keep a single job_path across their entire lifecycle.
- `src-tauri/src/pyramid/llm.rs` — In the compute queue transparent routing block (~line 652-695):
  - Set `chronicle_job_path: None` and `source: "local".to_string()` on the QueueEntry (WP-1 generates a semantic path via `generate_job_path`).
  - After `enqueue_local`, record the `enqueued` event (WP-1) using `entry.chronicle_job_path.unwrap_or_else(|| generate_job_path(ctx, &entry))`.
- `src-tauri/src/server.rs` — In `handle_fleet_dispatch`:
  - Generate `job_path` once (for WP-7 chronicle event).
  - Set `source: "fleet_received".to_string()` and `chronicle_job_path: Some(job_path.clone())` on the QueueEntry passed to the compute queue. This ensures WP-1/2/3/4 all use the same job_path as WP-7.
- `Cargo.toml` — Add `chrono = "0.4"` if not already present (uuid crate already in Cargo.toml but NOT used for job_path generation — semantic paths only).

**Verification:** `cargo check` passes. A local pyramid build populates `pyramid_compute_events` with `enqueued` rows. The GPU loop reads `entry.source` without inferring.

### Step 3: GPU Loop Events — Started, Completed, Failed (WP-2, WP-3, WP-4)

**Files to modify:**
- `src-tauri/src/main.rs` — GPU processing loop (~line 11416-11499):
  - After `QueueJobStarted` emission: record `started` event (WP-2) with `queue_wait_ms` from `entry.enqueued_at`.
  - After `QueueJobCompleted` emission, inside `Ok(ref response)`: record `completed` event (WP-3) with latency, tokens, cost, generation_id.
  - After `QueueJobCompleted` emission, inside `Err(ref e)`: record `failed` event (WP-4) with error and latency.
  - Read `entry.job_path` (added in Step 2) for all three events.
  - Clone `pyramid_db_path` into the GPU loop closure (it is already available as `pyramid_db_path` in the surrounding scope).
  - **Also update the existing `QueueJobStarted` / `QueueJobCompleted` event bus emissions** (~line 11437) to use `entry.source` instead of inferring source from `step_ctx.is_some()`. The existing event bus infers source from `step_ctx.is_some()` (true = local pyramid build, false = fleet_received), while the chronicle uses `entry.source`. These could diverge. Both must use the explicit `entry.source` field for consistency.

**Verification:** Run a local build. `pyramid_compute_events` has enqueued, started, completed rows for each LLM call. Latency and token counts are populated.

### Step 4: Fleet Dispatch Events (WP-5, WP-6)

**Files to modify:**
- `src-tauri/src/pyramid/llm.rs` — Phase A fleet handling (~line 800-850):
  - Before `fleet_dispatch_by_rule`: generate `fleet_job_path`, capture `fleet_start = Instant::now()`, derive `db_path` from ctx/config.cache_access, record `fleet_dispatched` event (WP-5) via `spawn_blocking`.
  - Inside `Ok(fleet_resp)`: record `fleet_returned` event (WP-6) with peer_model, latency, tokens via `spawn_blocking`.
  - Inside `Err(e)`: record `fleet_dispatch_failed` event (NOT `failed` — that's for GPU loop errors) with `{ peer_id, peer_name, error, latency_ms }` via `spawn_blocking`. This makes the fleet peer view's success rate calculation accurate: dispatched = returned + dispatch_failed. Without this event, fleet dispatch failures are invisible and the success rate appears higher than reality.

**DB path:** Derive from `ctx.as_ref().map(|c| c.db_path.clone()).or_else(|| config.cache_access.as_ref().map(|ca| ca.db_path.to_string()))`. There is no `conn` variable in scope in llm.rs at the fleet dispatch site. Use `tokio::task::spawn_blocking` with `Connection::open` (same pattern as WP-2/3/4). If db_path is None (no ctx and no cache_access), skip the chronicle write.

**Verification:** With fleet enabled and a peer connected, fleet dispatches produce `fleet_dispatched` + `fleet_returned` rows.

### Step 5: Fleet Received Event (WP-7)

**Files to modify:**
- `src-tauri/src/server.rs` — `handle_fleet_dispatch` (~line 1474-1621):
  - After request validation (line ~1528): record `fleet_received` event (WP-7).
  - DB path: `state.pyramid.data_dir.as_ref().map(|d| d.join("pyramid.db"))` (the actual field is `data_dir: Option<PathBuf>`, NOT `db_path`). Handle the `None` case by skipping the chronicle write with a `tracing::warn!`.

**Note:** The corresponding started/completed/failed events for this job fire from the GPU loop (Step 3) with `source: "fleet_received"` because the fleet handler passes `None` for StepContext.

**Verification:** When this node receives a fleet dispatch, `pyramid_compute_events` has `fleet_received`, `started`, and `completed` rows.

### Step 6: Cloud Return Events (WP-8)

**Files to modify:**
- `src-tauri/src/pyramid/llm.rs` — Two call sites where `emit_llm_call_completed` is called:
  - Primary path (~line 1395): After `emit_llm_call_completed`, check `!route_entry.is_local` and record `cloud_returned`.
  - Registry path (~line 2457): Same pattern.
  - `call_model_direct` (~line 2857) is EXCLUDED — it returns `String` not `LlmResponse`, so it lacks structured usage/provider fields. Add chronicle recording when it is migrated to return `LlmResponse`.

**Helper function:** Create `fn maybe_record_cloud_event(ctx, model_id, response, route_entry, latency_ms)` in `compute_chronicle.rs` that checks `!route_entry.is_local` and writes the event. Called from both sites.

**Cloud provider detection:** Use the `is_local` flag from `RouteEntry` rather than negative string matching against provider IDs. `is_local` is `true` for Ollama and local GPU routes, `false` for OpenRouter and registered cloud providers. This is more robust than checking `provider_id != "fleet" && !provider_id.starts_with("ollama")` because it handles future local providers without code changes.

**Verification:** Cloud calls (OpenRouter) produce `cloud_returned` rows with cost and latency.

### Step 7: IPC Commands

**Files to modify:**
- `src-tauri/src/main.rs` — Add four `#[tauri::command]` functions: `get_compute_events`, `get_compute_summary`, `get_compute_timeline`, `get_chronicle_dimensions`. Register them in `.invoke_handler(tauri::generate_handler![...])`.
- `src-tauri/src/pyramid/compute_chronicle.rs` — Implement `query_events` fully (the skeleton is in Step 1). Add `ChronicleDimensions` struct and `query_distinct` helper. `query_summary` and `query_timeline` can remain stubs until Steps 8-9.

**Verification:** From the Tauri dev console, `invoke("get_compute_events", { limit: 10 })` returns events.

### Step 8: Frontend — Basic Chronicle Component + Filters

**Files to create:**
- `src/components/ComputeChronicle.tsx` — Main container with sub-tabs (Timeline, Table, Fleet)
- `src/components/ComputeChronicleTable.tsx` — Sortable, filterable table
- `src/components/ComputeChronicleFilters.tsx` — Filter bar
- `src/components/ComputeChronicleStats.tsx` — Four stat cards

**Files to modify:**
- `src/components/MarketDashboard.tsx` (or `src/components/modes/MarketMode.tsx`) — Add Chronicle as a tab alongside QueueLiveView.

**Verification:** The Chronicle tab shows real event data from past builds in a filterable table.

### Step 9: Timeline Visualization

**Files to create or modify:**
- `src/components/ComputeChronicleTimeline.tsx` — Horizontal timeline with zoom, per-model lanes, color-coded bars.
- `src-tauri/src/pyramid/compute_chronicle.rs` — Implement `query_timeline` (bucketed aggregation using `strftime`).

**Verification:** The timeline renders job bars at the correct positions with correct colors. Zoom in/out works.

### Step 10: Summary Views + Fleet Analytics

**Files to create or modify:**
- `src/components/ComputeFleetAnalytics.tsx` — Fleet dispatch analytics panel.
- `src-tauri/src/pyramid/compute_chronicle.rs` — Implement `query_summary` (grouped aggregation).

**Verification:** Fleet analytics panel shows per-peer stats. Source breakdown shows local vs fleet vs cloud.

---

## X. What NOT to Build Yet

1. **Market event emission** — Stubs only (WP-9). Built when market phases 2+ ship. The schema supports market events already via the JSONB metadata pattern.
2. **Real-time streaming of chronicle to other nodes** — The chronicle is local-only. No Wire synchronization. No peer-to-peer event sharing.
3. **Chronicle data as Wire contributions** — The chronicle is an internal audit log, not a Wire contribution. It does not follow the contribution pattern (it is immutable, append-only, not supersedable).
4. **Automated alerting on anomalies** — Steward territory (Phase 7+). The chronicle provides the data; the steward-daemon provides the intelligence.
5. **Retention / compaction policies** — Pillar 37 forbids hardcoding these. When the table grows large enough to matter, add a contribution-driven retention policy. For now, append forever.
6. **GPU utilization percentage** — Requires instrumenting the GPU loop with idle-time tracking (started→completed spans vs wall clock). Defer to a follow-up; the stats card placeholder shows "coming soon".

---

## XI. Files Modified (Complete List)

| File | Change |
|---|---|
| `src-tauri/src/pyramid/llm.rs` | **Step 0:** Add `chain_name: Option<String>`, `content_type: Option<String>` to `CacheAccess` struct (default `None`). Add `.with_chain_context()` builder on `CacheAccess`. Signature of `clone_with_cache_access()` unchanged. **Step 2:** WP-1 (enqueue event, set `source: "local"` and `chronicle_job_path: None` on QueueEntry). **Step 4:** WP-5/6 (fleet dispatch/return events via spawn_blocking). **Step 6:** WP-8 (cloud events using `is_local` flag, threaded job_path via `LlmCallOptions.chronicle_job_path`). |
| `src-tauri/src/pyramid/mod.rs` | **Step 0:** Update chain executor callers of `clone_with_cache_access` (~lines 923, 951) to call `.with_chain_context()`. Other callers (evidence_answering.rs:980, main.rs stale paths, server.rs) unchanged — Optional fields default to `None`. **Step 1:** `pub mod compute_chronicle;` |
| `src-tauri/src/step_context.rs` | **Step 0:** Add `chain_name`, `content_type`, `task_label` to `StepContext`. Add `.with_chain_context()` builder. Update `make_step_ctx_from_llm_config` to populate from `CacheAccess`. |
| `src-tauri/src/pyramid/db.rs` | **Step 1:** CREATE TABLE pyramid_compute_events + composite indexes + 4 views in `init_pyramid_db` |
| `src-tauri/src/pyramid/compute_chronicle.rs` | **Step 1:** **NEW** — `ChronicleEventContext` (struct-only API), `record_event`, `query_events`, `query_summary`, `query_timeline`, `ComputeEvent` (15 fields), `ComputeSummary`, `TimelineBucket`, `ChronicleQueryFilters`, stub market helpers |
| `src-tauri/src/compute_queue.rs` | **Step 2:** Add `pub job_path: String`, `pub source: String`, and `pub chronicle_job_path: Option<String>` to `QueueEntry`. `chronicle_job_path` is the pre-assigned job_path from fleet_received (WP-7); WP-1 uses it when `Some`, generates semantic path via `generate_job_path` when `None`. |
| `src-tauri/src/main.rs` | **Step 3:** WP-2/3/4 (GPU loop started/completed/failed events reading `entry.source`). Also update existing event bus emissions to use `entry.source`. Set `options.chronicle_job_path = Some(entry.job_path)` before calling LLM function (for cloud fallthrough job_path threading). **Step 7:** Four IPC commands (`get_compute_events` with all 11 filter dimensions, `get_compute_summary`, `get_compute_timeline`, `get_chronicle_dimensions`). Clone db_path into GPU loop. |
| `src-tauri/src/server.rs` | **Step 2:** Set `source: "fleet_received"` on QueueEntry in `handle_fleet_dispatch`. **Step 5:** WP-7 (fleet_received event). |
| `src-tauri/Cargo.toml` | Add `chrono` crate if not present |
| `src/components/ComputeChronicle.tsx` | **Step 8:** **NEW** — Main chronicle container |
| `src/components/ComputeChronicleTimeline.tsx` | **Step 9:** **NEW** — Timeline visualization |
| `src/components/ComputeChronicleTable.tsx` | **Step 8:** **NEW** — Event history table |
| `src/components/ComputeChronicleStats.tsx` | **Step 8:** **NEW** — Stats dashboard cards |
| `src/components/ComputeChronicleFilters.tsx` | **Step 8:** **NEW** — Filter bar |
| `src/components/ComputeFleetAnalytics.tsx` | **Step 10:** **NEW** — Fleet analytics panel |
| `src/components/MarketDashboard.tsx` | **Step 8:** Add Chronicle tab alongside QueueLiveView |

---

## XII. DADBEAR Coordination

DADBEAR (Phases 1-7) ships BEFORE the Chronicle. When Chronicle implementation begins, the following are already in place:

### What DADBEAR Already Provides

**QueueEntry fields (added in DADBEAR Phase 3):**
```rust
pub work_item_id: Option<String>,   // Semantic path: "{slug}:{epoch}:{primitive}:{layer}:{target_id}"
pub attempt_id: Option<String>,     // Semantic path: "{work_item_id}:a{attempt_number}"
```
These are already on QueueEntry. Chronicle adds ONE field:
```rust
pub source: String,                 // "local", "fleet_received", "market_received"
```

**GPU loop callback (DADBEAR Phase 3+):** The GPU loop completion already writes to `dadbear_work_attempts` and `dadbear_work_items` (CAS state transition). Chronicle adds its `pyramid_compute_events` write to the same callback point.

**Semantic path IDs:** All DADBEAR identifiers are semantic paths, not UUIDs. The Chronicle's `job_path` follows the same convention.

### Chronicle's `job_path`: Semantic, Not UUID

For DADBEAR-dispatched work, `job_path = entry.work_item_id` (the semantic path DADBEAR already set).

For non-DADBEAR work (interactive queries, diagnostic calls, fleet-received jobs, stale engine calls that haven't been migrated to DADBEAR yet), the Chronicle generates a semantic path at enqueue time:
```
{slug}:{build_id_short}:{step_name}:d{depth}:{qualifier}
```
Fleet-received: `fleet-recv:{peer_id_short}:{rule_name}:{timestamp}`
Cloud standalone: `cloud:{provider}:{model_short}:{timestamp}`

No UUIDs anywhere an LLM or operator might see them.

### pyramid_compute_events: DADBEAR Correlation Columns

Add to the schema:
```sql
    work_item_id   TEXT,              -- DADBEAR work item ID (NULL for non-DADBEAR calls)
    attempt_id     TEXT,              -- DADBEAR attempt ID (NULL for non-DADBEAR calls)
```

Add index: `CREATE INDEX IF NOT EXISTS idx_compute_events_work_item ON pyramid_compute_events(work_item_id) WHERE work_item_id IS NOT NULL;`

This enables: "show me all chronicle events for DADBEAR work item X" — joining the compute execution history to the durable work item lifecycle.

### StepContext: DADBEAR Work Item Reconstruction

Chronicle adds `chain_name`, `content_type`, `task_label` to StepContext. DADBEAR reconstructs StepContext from durable work item fields for crash recovery.

**Resolution:** DADBEAR's `dadbear_work_items` table should also carry `chain_name TEXT` and `content_type TEXT` (alongside existing `step_name`, `primitive`, `layer`). These are materialized at compile time from the chain context. On crash recovery, StepContext reconstruction populates all fields including chronicle's additions.

`task_label` is derived (not stored) — reconstructed from `chain_name + step_name + depth` at StepContext construction time.

### GPU Loop Callback: Ordered Concerns

The GPU loop completion point handles both systems. Order:

1. **Chronicle write** — `record_event` with completed/failed status (spawn_blocking)
2. **DADBEAR work item update** — CAS state transition `dispatched → completed` (if work_item_id is set)
3. **DADBEAR attempt completion** — update `dadbear_work_attempts` row
4. **Event bus emission** — `QueueJobCompleted` (existing, for real-time frontend)
5. **Oneshot result send** — return result to waiting caller

Steps 1-3 are fire-and-forget writes. Steps 4-5 are the existing flow.

### Source Vocabulary

Chronicle: `source: 'local' | 'fleet' | 'cloud' | 'fleet_received' | 'market' | 'market_received'`
DADBEAR attempts: `routing: 'local' | 'cloud' | 'fleet' | 'market'`

Chronicle has finer granularity (sent vs received). DADBEAR's routing is from the dispatcher's perspective (it never receives — the receiving side is a separate node). No conflict. The dispatcher can read `entry.source` to populate `dadbear_work_attempts.routing` (mapping "local" → "local", "fleet" → fleet peer resolved the model, etc.).

### Cost Tracking: Three Stores, Three Consumers

| Store | Consumer | Purpose |
|---|---|---|
| `pyramid_cost_log` | Cost observatory, reconciliation, leak detection | Per-LLM-call, with broadcast confirmation state |
| `pyramid_compute_events` (chronicle) | Operator dashboard, fleet analytics, timeline | Per-lifecycle-event, with task context + source provenance |
| `dadbear_work_attempts` | Supervisor hot path, work item cost rollup | Per-attempt, with review/challenge state |

All three carry cost/token/latency data. Not duplication — different shapes for different consumers. `dadbear_work_attempts.cost_log_id` joins to `pyramid_cost_log`. `pyramid_compute_events.work_item_id` joins to `dadbear_work_items`. The three stores form a triangle that can be cross-referenced.

### Implementation Order

Since DADBEAR is being implemented first:
1. DADBEAR adds `work_item_id` + `attempt_id` to QueueEntry
2. Chronicle adds `source` to QueueEntry and the `work_item_id`/`attempt_id` columns to `pyramid_compute_events`
3. Chronicle's enqueue path generates a semantic path into `work_item_id` when DADBEAR hasn't set one (guard: `if entry.work_item_id.is_none()`)
4. The GPU loop callback writes chronicle events using `entry.work_item_id` as the correlation key

If Chronicle ships BEFORE DADBEAR finishes: `work_item_id` and `attempt_id` on QueueEntry are always None. Chronicle generates semantic path via `generate_job_path`s. When DADBEAR ships later, it populates these fields and chronicle events automatically correlate.

If DADBEAR ships BEFORE Chronicle: `work_item_id` and `attempt_id` exist on QueueEntry but chronicle doesn't exist yet. No harm — the fields are just unused until chronicle ships.

Either order works. No ordering dependency.

---

## XIII. Verification Checklist

After implementation, verify:

1. `cargo check` passes with no warnings related to compute_chronicle
2. Fresh DB creation includes `pyramid_compute_events` table and all views
3. Local pyramid build produces: enqueued → started → completed events for each LLM call
4. Each event has correct job_path grouping (all lifecycle events for one call share the same job_path, using work_item_id when set by DADBEAR)
5. StepContext fields (slug, build_id, step_name, chain_name, content_type) are populated on local build events
6. `queue_wait_ms` on started events is non-zero (measures actual queue wait)
7. `latency_ms`, `tokens_prompt`, `tokens_completion` on completed events match the LlmResponse
8. Fleet dispatch (if fleet is configured) produces fleet_dispatched + fleet_returned events
9. Fleet dispatch failure produces fleet_dispatch_failed event
10. Fleet receive produces fleet_received + started + completed events with source "fleet_received", same job_path across all
11. Cloud calls (OpenRouter) produce cloud_returned events with cost_usd populated
12. `get_compute_events` IPC returns correct data with all 11 filter dimensions working
13. `get_chronicle_dimensions` IPC returns distinct values for filter dropdowns
14. Chronicle tab renders in the Market section with real data
15. No regressions in build speed (chronicle writes are non-blocking via spawn_blocking)
16. Failed LLM calls produce failed events with error messages
17. The existing `pyramid_cost_log` and event bus events continue to work unchanged
18. `work_item_id` and `attempt_id` columns exist on pyramid_compute_events (NULL when DADBEAR hasn't set them)
19. QueueEntry has `work_item_id`, `attempt_id`, `source` fields (DADBEAR coordination)
