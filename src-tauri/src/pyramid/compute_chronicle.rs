// pyramid/compute_chronicle.rs — Persistent compute observability layer.
//
// Every LLM call that touches any compute resource gets recorded in
// `pyramid_compute_events`. The operator sees one unified history with
// source as a dimension. Append-only, immutable — events never update
// or delete.
//
// This module provides:
//   - `ChronicleEventContext` — struct carrying all fields for a single event
//   - `record_event` — append-only INSERT
//   - `generate_job_path` — semantic path generation (NO UUIDs)
//   - `query_events` — filterable query with all 11 dimensions
//   - `query_summary` — grouped aggregation for stats dashboard
//   - `query_timeline` — bucketed data for timeline visualization
//   - `query_distinct_dimensions` — distinct values for filter dropdowns
//   - Market stubs (called when market phases ship)

use anyhow::Result;
use rusqlite::{params, Connection};
use std::collections::HashMap;

use super::step_context::StepContext;
use crate::compute_queue::QueueEntry;

// ── Canonical source constants ────────────────────────────────────────────
// The `source` column records which compute lane produced the event.
// Dispatcher-side fleet events use SOURCE_FLEET.
// Peer-side (received-from-peer) events and queue entries use SOURCE_FLEET_RECEIVED.
pub const SOURCE_FLEET: &str = "fleet";
pub const SOURCE_FLEET_RECEIVED: &str = "fleet_received";

// ── Canonical event_type constants ────────────────────────────────────────
// These are the canonical event_type string values emitted by the async
// fleet dispatch path. Later workstreams migrate their emission sites to
// use these constants instead of raw string literals.

// Dispatcher-side events (source='fleet') — the node that sent the job.
pub const EVENT_FLEET_DISPATCHED_ASYNC: &str = "fleet_dispatched_async";
pub const EVENT_FLEET_DISPATCH_FAILED: &str = "fleet_dispatch_failed";
pub const EVENT_FLEET_PEER_OVERLOADED: &str = "fleet_peer_overloaded";
pub const EVENT_FLEET_DISPATCH_TIMEOUT: &str = "fleet_dispatch_timeout";
pub const EVENT_FLEET_RESULT_RECEIVED: &str = "fleet_result_received";
pub const EVENT_FLEET_RESULT_FAILED: &str = "fleet_result_failed";
pub const EVENT_FLEET_RESULT_ORPHANED: &str = "fleet_result_orphaned";
pub const EVENT_FLEET_RESULT_FORGERY_ATTEMPT: &str = "fleet_result_forgery_attempt";
pub const EVENT_FLEET_PENDING_ORPHANED: &str = "fleet_pending_orphaned";

// Peer-side events (source='fleet_received') — the node that received the job.
pub const EVENT_FLEET_JOB_ACCEPTED: &str = "fleet_job_accepted";
pub const EVENT_FLEET_ADMISSION_REJECTED: &str = "fleet_admission_rejected";
pub const EVENT_FLEET_JOB_COMPLETED: &str = "fleet_job_completed";
pub const EVENT_FLEET_CALLBACK_DELIVERED: &str = "fleet_callback_delivered";
pub const EVENT_FLEET_CALLBACK_FAILED: &str = "fleet_callback_failed";
pub const EVENT_FLEET_CALLBACK_EXHAUSTED: &str = "fleet_callback_exhausted";
pub const EVENT_FLEET_WORKER_HEARTBEAT_LOST: &str = "fleet_worker_heartbeat_lost";
pub const EVENT_FLEET_WORKER_SWEEP_LOST: &str = "fleet_worker_sweep_lost";
pub const EVENT_FLEET_DELIVERY_CAS_LOST: &str = "fleet_delivery_cas_lost";

// ── Event context ─────────────────────────────────────────────────────────

/// All context needed to record a chronicle event.
/// Constructed from StepContext (when available) + queue/dispatch metadata.
#[derive(Debug, Clone)]
pub struct ChronicleEventContext {
    pub job_path: String,
    pub event_type: String,
    pub timestamp: String,
    pub source: String,
    pub model_id: Option<String>,
    pub slug: Option<String>,
    pub build_id: Option<String>,
    pub chain_name: Option<String>,
    pub content_type: Option<String>,
    pub step_name: Option<String>,
    pub primitive: Option<String>,
    pub depth: Option<i64>,
    pub task_label: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub work_item_id: Option<String>,
    pub attempt_id: Option<String>,
}

impl ChronicleEventContext {
    /// Build from a StepContext (local builds, fleet dispatches — rich context).
    /// Captures `chrono::Utc::now()` at construction time so the timestamp
    /// reflects actual event time, not async write execution time.
    pub fn from_step_ctx(
        ctx: &StepContext,
        job_path: &str,
        event_type: &str,
        source: &str,
    ) -> Self {
        Self {
            job_path: job_path.to_string(),
            event_type: event_type.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            source: source.to_string(),
            model_id: ctx.resolved_model_id.clone(),
            slug: Some(ctx.slug.clone()),
            build_id: Some(ctx.build_id.clone()),
            chain_name: if ctx.chain_name.is_empty() {
                None
            } else {
                Some(ctx.chain_name.clone())
            },
            content_type: if ctx.content_type.is_empty() {
                None
            } else {
                Some(ctx.content_type.clone())
            },
            step_name: Some(ctx.step_name.clone()),
            primitive: Some(ctx.primitive.clone()),
            depth: Some(ctx.depth),
            task_label: if ctx.task_label.is_empty() {
                None
            } else {
                Some(ctx.task_label.clone())
            },
            metadata: None,
            work_item_id: None,
            attempt_id: None,
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
            model_id: None,
            slug: None,
            build_id: None,
            chain_name: None,
            content_type: None,
            step_name: None,
            primitive: None,
            depth: None,
            task_label: None,
            metadata: None,
            work_item_id: None,
            attempt_id: None,
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

    pub fn with_work_item(mut self, work_item_id: Option<String>, attempt_id: Option<String>) -> Self {
        self.work_item_id = work_item_id;
        self.attempt_id = attempt_id;
        self
    }
}

// ── Job path generation ───────────────────────────────────────────────────

/// Generate a semantic job_path from available context.
/// For DADBEAR work: uses entry.work_item_id (already a semantic path).
/// For non-DADBEAR work: derives from StepContext or queue entry metadata.
/// NO UUIDs — paths are human-readable and LLM-parseable.
pub fn generate_job_path(ctx: Option<&StepContext>, work_item_id: Option<&str>, model_id: &str, source: &str) -> String {
    // DADBEAR already set a semantic path
    if let Some(wid) = work_item_id {
        if !wid.is_empty() {
            return wid.to_string();
        }
    }
    // Derive from StepContext (local builds, stale checks)
    if let Some(c) = ctx {
        let build_short = if c.build_id.len() > 8 {
            &c.build_id[..8]
        } else {
            &c.build_id
        };
        return format!("{}:{}:{}:d{}", c.slug, build_short, c.step_name, c.depth);
    }
    // Fleet received (no ctx, no work_item_id)
    if source == "fleet_received" {
        let ts = chrono::Utc::now().timestamp();
        return format!("fleet-recv:{}:{}", model_id, ts);
    }
    // Fallback: model + timestamp (always readable)
    let ts = chrono::Utc::now().timestamp();
    format!("anon:{}:{}", model_id, ts)
}

/// Generate a job_path from a QueueEntry (convenience wrapper).
pub fn generate_job_path_from_entry(ctx: Option<&StepContext>, entry: &QueueEntry) -> String {
    generate_job_path(ctx, entry.work_item_id.as_deref(), &entry.model_id, &entry.source)
}

// ── Record (append-only write) ────────────────────────────────────────────

/// Record a single compute event. Append-only — never updates or deletes.
/// Timestamp is passed explicitly from the context (captured at event site),
/// NOT relying on SQLite DEFAULT.
pub fn record_event(conn: &Connection, ctx: &ChronicleEventContext) -> Result<i64> {
    conn.execute(
        "INSERT INTO pyramid_compute_events
            (job_path, event_type, timestamp, model_id, source, slug, build_id,
             chain_name, content_type, step_name, primitive, depth, task_label,
             metadata, work_item_id, attempt_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
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
            ctx.work_item_id,
            ctx.attempt_id,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

// ── Query types ───────────────────────────────────────────────────────────

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
    pub by_source: HashMap<String, i64>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ChronicleDimensions {
    pub slugs: Vec<String>,
    pub models: Vec<String>,
    pub sources: Vec<String>,
    pub chain_names: Vec<String>,
    pub event_types: Vec<String>,
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
    pub after: Option<String>,
    pub before: Option<String>,
    pub limit: i64,
    pub offset: i64,
}

// ── Query implementations ─────────────────────────────────────────────────

/// Query compute events with optional filters. Every field is a filterable
/// dimension — combine any subset for precise queries.
pub fn query_events(
    conn: &Connection,
    filters: &ChronicleQueryFilters,
) -> Result<Vec<ComputeEvent>> {
    let mut sql = String::from(
        "SELECT id, job_path, event_type, timestamp, model_id, slug,
                build_id, chain_name, content_type, step_name, primitive,
                depth, task_label, source, metadata
         FROM pyramid_compute_events WHERE 1=1",
    );
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut param_idx = 1;

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
            metadata: r
                .get::<_, Option<String>>("metadata")?
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
    group_by: &str,
) -> Result<Vec<ComputeSummary>> {
    // Determine the SQL column to GROUP BY based on the requested dimension.
    let group_col = match group_by {
        "model" => "model_id",
        "source" => "source",
        "slug" => "slug",
        "hour" => "strftime('%Y-%m-%d %H:00:00', timestamp)",
        _ => "source", // safe fallback
    };

    let sql = format!(
        "SELECT
            COALESCE({group_col}, 'unknown') AS group_key,
            COUNT(*) AS total_events,
            COUNT(CASE WHEN event_type = 'completed' THEN 1 END) AS completed_count,
            COUNT(CASE WHEN event_type = 'failed' THEN 1 END) AS failed_count,
            COALESCE(SUM(CASE WHEN event_type = 'completed' THEN CAST(json_extract(metadata, '$.latency_ms') AS INTEGER) ELSE 0 END), 0) AS total_latency_ms,
            COALESCE(AVG(CASE WHEN event_type = 'completed' THEN CAST(json_extract(metadata, '$.latency_ms') AS REAL) END), 0.0) AS avg_latency_ms,
            COALESCE(SUM(CASE WHEN event_type = 'completed' THEN CAST(json_extract(metadata, '$.tokens_prompt') AS INTEGER) ELSE 0 END), 0) AS total_tokens_prompt,
            COALESCE(SUM(CASE WHEN event_type = 'completed' THEN CAST(json_extract(metadata, '$.tokens_completion') AS INTEGER) ELSE 0 END), 0) AS total_tokens_completion,
            COALESCE(SUM(CASE WHEN event_type IN ('completed', 'cloud_returned') THEN CAST(json_extract(metadata, '$.cost_usd') AS REAL) ELSE 0.0 END), 0.0) AS total_cost_usd,
            COUNT(CASE WHEN source = 'fleet' THEN 1 END) AS fleet_count,
            COUNT(CASE WHEN source = 'local' THEN 1 END) AS local_count,
            COUNT(CASE WHEN source = 'cloud' THEN 1 END) AS cloud_count
         FROM pyramid_compute_events
         WHERE timestamp >= ?1 AND timestamp <= ?2
         GROUP BY {group_col}
         ORDER BY total_events DESC",
        group_col = group_col,
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![period_start, period_end], |r| {
        Ok(ComputeSummary {
            group_key: r.get("group_key")?,
            total_events: r.get("total_events")?,
            completed_count: r.get("completed_count")?,
            failed_count: r.get("failed_count")?,
            total_latency_ms: r.get("total_latency_ms")?,
            avg_latency_ms: r.get("avg_latency_ms")?,
            total_tokens_prompt: r.get("total_tokens_prompt")?,
            total_tokens_completion: r.get("total_tokens_completion")?,
            total_cost_usd: r.get("total_cost_usd")?,
            fleet_count: r.get("fleet_count")?,
            local_count: r.get("local_count")?,
            cloud_count: r.get("cloud_count")?,
        })
    })?;

    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Timeline query: bucketed event counts for visualization.
/// `bucket_size_minutes` is required — no hardcoded default (Pillar 37).
pub fn query_timeline(
    conn: &Connection,
    start: &str,
    end: &str,
    bucket_size_minutes: i64,
) -> Result<Vec<TimelineBucket>> {
    // Use strftime to bucket timestamps. SQLite doesn't have native bucket
    // functions, so we compute the bucket index from the Julian day offset.
    let bucket_secs = bucket_size_minutes * 60;

    let sql = format!(
        "SELECT
            strftime('%Y-%m-%dT%H:%M:%S', (CAST(strftime('%s', timestamp) AS INTEGER) / {bs}) * {bs}, 'unixepoch') AS bucket_start,
            strftime('%Y-%m-%dT%H:%M:%S', ((CAST(strftime('%s', timestamp) AS INTEGER) / {bs}) * {bs}) + {bs}, 'unixepoch') AS bucket_end,
            COUNT(*) AS event_count,
            COUNT(CASE WHEN event_type = 'completed' THEN 1 END) AS completed_count,
            COALESCE(AVG(CASE WHEN event_type = 'completed' THEN CAST(json_extract(metadata, '$.latency_ms') AS REAL) END), 0.0) AS avg_latency_ms,
            COALESCE(SUM(CASE WHEN event_type IN ('completed', 'cloud_returned') THEN CAST(json_extract(metadata, '$.cost_usd') AS REAL) ELSE 0.0 END), 0.0) AS total_cost_usd,
            COUNT(CASE WHEN source = 'local' THEN 1 END) AS local_count,
            COUNT(CASE WHEN source = 'fleet' THEN 1 END) AS fleet_count,
            COUNT(CASE WHEN source = 'cloud' THEN 1 END) AS cloud_count,
            COUNT(CASE WHEN source = 'fleet_received' THEN 1 END) AS fleet_received_count,
            COUNT(CASE WHEN source = 'market' THEN 1 END) AS market_count,
            COUNT(CASE WHEN source = 'market_received' THEN 1 END) AS market_received_count
         FROM pyramid_compute_events
         WHERE timestamp >= ?1 AND timestamp <= ?2
         GROUP BY bucket_start
         ORDER BY bucket_start ASC",
        bs = bucket_secs,
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![start, end], |r| {
        let mut by_source = HashMap::new();
        let local_count: i64 = r.get("local_count")?;
        let fleet_count: i64 = r.get("fleet_count")?;
        let cloud_count: i64 = r.get("cloud_count")?;
        let fleet_received_count: i64 = r.get("fleet_received_count")?;
        let market_count: i64 = r.get("market_count")?;
        let market_received_count: i64 = r.get("market_received_count")?;

        if local_count > 0 { by_source.insert("local".to_string(), local_count); }
        if fleet_count > 0 { by_source.insert("fleet".to_string(), fleet_count); }
        if cloud_count > 0 { by_source.insert("cloud".to_string(), cloud_count); }
        if fleet_received_count > 0 { by_source.insert("fleet_received".to_string(), fleet_received_count); }
        if market_count > 0 { by_source.insert("market".to_string(), market_count); }
        if market_received_count > 0 { by_source.insert("market_received".to_string(), market_received_count); }

        Ok(TimelineBucket {
            bucket_start: r.get("bucket_start")?,
            bucket_end: r.get("bucket_end")?,
            event_count: r.get("event_count")?,
            completed_count: r.get("completed_count")?,
            avg_latency_ms: r.get("avg_latency_ms")?,
            total_cost_usd: r.get("total_cost_usd")?,
            by_source,
        })
    })?;

    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// SELECT DISTINCT non-null values for a column (for filter dropdowns).
pub fn query_distinct(conn: &Connection, column: &str) -> Result<Vec<String>, String> {
    // Whitelist columns to prevent SQL injection
    let allowed = [
        "slug",
        "model_id",
        "source",
        "chain_name",
        "event_type",
        "step_name",
        "primitive",
    ];
    if !allowed.contains(&column) {
        return Err(format!("Invalid column for distinct query: {}", column));
    }
    let sql = format!(
        "SELECT DISTINCT {} FROM pyramid_compute_events WHERE {} IS NOT NULL ORDER BY {}",
        column, column, column
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

/// Query all distinct dimension values for populating filter dropdowns.
pub fn query_distinct_dimensions(conn: &Connection) -> Result<ChronicleDimensions, String> {
    let slugs = query_distinct(conn, "slug")?;
    let models = query_distinct(conn, "model_id")?;
    let sources = query_distinct(conn, "source")?;
    let chain_names = query_distinct(conn, "chain_name")?;
    let event_types = query_distinct(conn, "event_type")?;
    Ok(ChronicleDimensions {
        slugs,
        models,
        sources,
        chain_names,
        event_types,
    })
}

// ── Market stubs ──────────────────────────────────────────────────────────

/// Stub: record market_matched event. Called from market exchange client (Phase 2).
pub fn record_market_matched(conn: &Connection, ctx: &ChronicleEventContext) -> Result<i64> {
    record_event(conn, ctx)
}

/// Stub: record market_settled event. Called from settlement handler (Phase 3).
pub fn record_market_settled(conn: &Connection, ctx: &ChronicleEventContext) -> Result<i64> {
    record_event(conn, ctx)
}

/// Stub: record market_received event. Called from market job handler (Phase 5).
pub fn record_market_received(conn: &Connection, ctx: &ChronicleEventContext) -> Result<i64> {
    record_event(conn, ctx)
}
