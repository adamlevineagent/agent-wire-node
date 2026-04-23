// pyramid/pyramid_scheduler.rs — Phase 9b-1: periodic tick scheduler.
//
// Emits two pyramid-wide observation events on a regular cadence:
//   - `accretion_tick` (default 30min): every active slug gets an
//     accretion_handler run via its role binding. The event is
//     map_event_to_primitive → role_bound, role_for_event →
//     `accretion_handler`, so it flows through the standard
//     compiler → supervisor → execute_chain_for_target path with no
//     special-case dispatch.
//   - `sweep_tick` (default 6h): every active slug gets a sweep run
//     via its role binding (same routing through role_for_event →
//     `sweep`).
//
// Periods and thresholds are operator-editable via a
// `scheduler_parameters` config contribution (one active row, global
// scope). Defaults are seeded on first boot and loud-resurfaced any
// time the scheduler misreads the row.
//
// Why not a queue: we don't want to persist tick rows — ticks are
// transient clock pulses. The scheduler just sleeps and emits
// observation events; the existing compile/dispatch loop consumes
// them like any other event. This keeps the scheduler orthogonal to
// the rest of the pipeline (a scheduler outage doesn't corrupt
// durable state; it just means slugs stop getting their periodic
// handler runs until the scheduler is restarted).
//
// Why global (not per-slug) scheduler: the per-slug loop inside the
// compiler already fans work out per-slug. The scheduler's job is to
// produce the upstream tick; we iterate slugs once at emit time so a
// slug added between ticks is picked up on the next cycle without
// any per-slug scheduler state. The observation event's `slug` field
// carries the per-slug binding — no shared work item state across
// slugs.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use super::config_contributions;
use super::observation_events;

/// schema_type used to store the scheduler's operator-editable
/// parameters in `pyramid_config_contributions`. Single-row (global
/// scope, slug=NULL) — one active entry per installation.
pub const SCHEDULER_CONFIG_SCHEMA_TYPE: &str = "scheduler_parameters";

/// Minimum interval. Clamped at load-time so a misconfigured YAML
/// body can't turn the scheduler into a hot loop. Loud-logs when a
/// clamp fires so the operator sees the misconfiguration.
const MIN_INTERVAL_SECS: u64 = 30;

/// Ceiling on interval — long enough for any reasonable periodic
/// cadence, short enough that misparse into `u64::MAX` doesn't mean
/// "effectively never". 30 days.
const MAX_INTERVAL_SECS: u64 = 30 * 24 * 60 * 60;

/// Genesis defaults. Seeded into `pyramid_config_contributions` on
/// first boot so operators can supersede the row to change values.
/// Per feedback_pillar37_no_hedging these values live in the DB
/// (editable), not in Rust — but the GENESIS seed has to come from
/// somewhere. This is the one place.
pub const DEFAULT_ACCRETION_INTERVAL_SECS: u64 = 30 * 60;   // 30 minutes
pub const DEFAULT_SWEEP_INTERVAL_SECS: u64 = 6 * 60 * 60;   // 6 hours
pub const DEFAULT_ACCRETION_THRESHOLD: u64 = 50;            // 50 annotations
pub const DEFAULT_SWEEP_STALE_DAYS: u64 = 7;                // failed WI older than 7d
pub const DEFAULT_SWEEP_RETENTION_DAYS: u64 = 30;           // archive after 30d

/// Operator-editable scheduler configuration. Persisted as a single
/// active `scheduler_parameters` contribution (YAML body). All
/// fields tunable without a code deploy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SchedulerConfig {
    /// Accretion tick interval in seconds. Every N seconds the
    /// scheduler emits `accretion_tick` for every active slug.
    pub accretion_interval_secs: u64,
    /// Sweep tick interval in seconds.
    pub sweep_interval_secs: u64,
    /// Volume-threshold K: when a slug's pending annotation count
    /// (since last accretion_cursor) reaches this, the annotation
    /// hook emits an immediate `accretion_threshold_hit` event
    /// instead of waiting for the next accretion tick.
    pub accretion_threshold: u64,
    /// "Stale" failed-work-item age cutoff in days. Rows in
    /// state='failed' older than this are candidates for archival
    /// (and counted by the sweep chronicle step).
    pub sweep_stale_days: u64,
    /// Retention period for soft-archived rows in days. Once a
    /// `dadbear_work_items` row's state_changed_at is older than
    /// (now - sweep_stale_days - sweep_retention_days), sweep marks
    /// `archived_at`. Two-stage so operators can still recover rows
    /// within the retention window.
    pub sweep_retention_days: u64,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            accretion_interval_secs: DEFAULT_ACCRETION_INTERVAL_SECS,
            sweep_interval_secs: DEFAULT_SWEEP_INTERVAL_SECS,
            accretion_threshold: DEFAULT_ACCRETION_THRESHOLD,
            sweep_stale_days: DEFAULT_SWEEP_STALE_DAYS,
            sweep_retention_days: DEFAULT_SWEEP_RETENTION_DAYS,
        }
    }
}

impl SchedulerConfig {
    fn clamped(mut self) -> Self {
        if self.accretion_interval_secs < MIN_INTERVAL_SECS {
            tracing::warn!(
                configured = self.accretion_interval_secs,
                floor = MIN_INTERVAL_SECS,
                "pyramid_scheduler: accretion_interval_secs below floor — clamping"
            );
            self.accretion_interval_secs = MIN_INTERVAL_SECS;
        }
        if self.accretion_interval_secs > MAX_INTERVAL_SECS {
            tracing::warn!(
                configured = self.accretion_interval_secs,
                ceiling = MAX_INTERVAL_SECS,
                "pyramid_scheduler: accretion_interval_secs above ceiling — clamping"
            );
            self.accretion_interval_secs = MAX_INTERVAL_SECS;
        }
        if self.sweep_interval_secs < MIN_INTERVAL_SECS {
            tracing::warn!(
                configured = self.sweep_interval_secs,
                floor = MIN_INTERVAL_SECS,
                "pyramid_scheduler: sweep_interval_secs below floor — clamping"
            );
            self.sweep_interval_secs = MIN_INTERVAL_SECS;
        }
        if self.sweep_interval_secs > MAX_INTERVAL_SECS {
            tracing::warn!(
                configured = self.sweep_interval_secs,
                ceiling = MAX_INTERVAL_SECS,
                "pyramid_scheduler: sweep_interval_secs above ceiling — clamping"
            );
            self.sweep_interval_secs = MAX_INTERVAL_SECS;
        }
        if self.accretion_threshold == 0 {
            tracing::warn!(
                "pyramid_scheduler: accretion_threshold=0 disables the volume-threshold path; \
                 keeping 0 (tick-only) per operator intent"
            );
        }
        self
    }
}

/// Seed the genesis `scheduler_parameters` row if none exists.
/// Idempotent — subsequent boots leave the row alone so operator
/// supersessions persist.
pub fn seed_scheduler_defaults(conn: &Connection) -> Result<()> {
    let existing =
        config_contributions::load_active_config_contribution(conn, SCHEDULER_CONFIG_SCHEMA_TYPE, None)?;
    if existing.is_some() {
        return Ok(());
    }
    let body = SchedulerConfig::default();
    let yaml = serde_yaml::to_string(&body)
        .context("pyramid_scheduler: failed to serialize default SchedulerConfig to YAML")?;
    let _ = config_contributions::create_config_contribution(
        conn,
        SCHEDULER_CONFIG_SCHEMA_TYPE,
        None,
        &yaml,
        Some("pyramid_scheduler genesis defaults"),
        "bundled",
        Some("genesis"),
        "active",
    )
    .context("pyramid_scheduler: failed to seed SchedulerConfig defaults")?;
    tracing::info!("[pyramid_scheduler] seeded genesis scheduler_parameters row");
    Ok(())
}

/// Load the active `SchedulerConfig`, applying clamps + falling back
/// to defaults if the row is missing or malformed. Loud-logs when
/// fallback fires so operators see the drift.
pub fn load_config(conn: &Connection) -> SchedulerConfig {
    let row = match config_contributions::load_active_config_contribution(
        conn,
        SCHEDULER_CONFIG_SCHEMA_TYPE,
        None,
    ) {
        Ok(Some(r)) => r,
        Ok(None) => {
            tracing::warn!(
                "pyramid_scheduler: no active scheduler_parameters row — using defaults \
                 (seed_scheduler_defaults should have run; check init_pyramid_db ordering)"
            );
            return SchedulerConfig::default();
        }
        Err(e) => {
            tracing::error!(
                error = %e,
                "pyramid_scheduler: failed to load scheduler_parameters; using defaults"
            );
            return SchedulerConfig::default();
        }
    };
    match serde_yaml::from_str::<SchedulerConfig>(&row.yaml_content) {
        Ok(cfg) => cfg.clamped(),
        Err(e) => {
            tracing::error!(
                error = %e,
                contribution_id = %row.contribution_id,
                "pyramid_scheduler: scheduler_parameters yaml_content failed to parse; \
                 using defaults — operator should fix the row"
            );
            SchedulerConfig::default()
        }
    }
}

/// List every active (non-archived) slug. Ticks fan out to all of
/// them — an archived slug gets no scheduler attention.
fn list_active_slugs(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT slug FROM pyramid_slugs WHERE archived_at IS NULL ORDER BY slug",
    )?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Emit `accretion_tick` for every active slug.
///
/// Returns the number of events written. Per-slug write failures are
/// logged and skipped — a bad slug shouldn't fail the whole tick.
///
/// Metadata carries the accretion-related SchedulerConfig fields so
/// the supervisor's role_bound dispatch can splat them into the
/// accretion chain's initial input envelope. The accretion chain
/// itself reads `window_n` (LLM-context cap) from the envelope — we
/// send an aggressive default that operators override via a
/// chain-level supersession. The count + cursor are NOT pre-computed
/// at tick time (they'd be stale by dispatch) — the accretion chain
/// reads them freshly in `load_recent_annotations_for_slug`.
pub fn emit_accretion_tick(conn: &Connection) -> Result<usize> {
    let cfg = load_config(conn);
    let slugs = list_active_slugs(conn)?;
    let mut written = 0usize;
    for slug in &slugs {
        let metadata = serde_json::json!({
            "trigger": "scheduler",
            "tick_kind": "accretion",
            // Starter-accretion-handler reads `window_n` as required
            // field. Tie the scheduler default to the threshold so
            // tick-dispatched accretions load at least `K` annotations
            // of recent window — matches the threshold-hit semantics.
            // Operators override per-slug via chain-level YAML.
            "window_n": cfg.accretion_threshold.max(20) as i64,
        })
        .to_string();
        match observation_events::write_observation_event(
            conn,
            slug,
            "scheduler",
            "accretion_tick",
            None,
            None,
            None,
            None,
            None,
            None,
            Some(&metadata),
        ) {
            Ok(_) => written += 1,
            Err(e) => tracing::warn!(
                slug = %slug,
                error = %e,
                "pyramid_scheduler: failed to emit accretion_tick"
            ),
        }
    }
    Ok(written)
}

/// Emit `sweep_tick` for every active slug.
///
/// Metadata carries the sweep policy knobs (stale_days +
/// retention_days + contribution_retention_days) so the sweep chain's
/// mechanicals receive them via the supervisor's metadata splat.
pub fn emit_sweep_tick(conn: &Connection) -> Result<usize> {
    let cfg = load_config(conn);
    let slugs = list_active_slugs(conn)?;
    let mut written = 0usize;
    for slug in &slugs {
        let metadata = serde_json::json!({
            "trigger": "scheduler",
            "tick_kind": "sweep",
            "stale_days": cfg.sweep_stale_days as i64,
            "retention_days": cfg.sweep_retention_days as i64,
            "contribution_retention_days": cfg.sweep_retention_days as i64,
        })
        .to_string();
        match observation_events::write_observation_event(
            conn,
            slug,
            "scheduler",
            "sweep_tick",
            None,
            None,
            None,
            None,
            None,
            None,
            Some(&metadata),
        ) {
            Ok(_) => written += 1,
            Err(e) => tracing::warn!(
                slug = %slug,
                error = %e,
                "pyramid_scheduler: failed to emit sweep_tick"
            ),
        }
    }
    Ok(written)
}

/// Emit an immediate `accretion_threshold_hit` for a single slug.
/// Called from the annotation hook when a slug's pending annotation
/// count since last cursor reaches `accretion_threshold`. Carries
/// the count + cursor + triggering annotation id in metadata so a
/// chronicle reader can reconstruct why the immediate tick fired.
pub fn emit_accretion_threshold_hit(
    conn: &Connection,
    slug: &str,
    annotation_id: i64,
    count_since_cursor: i64,
    accretion_cursor: i64,
    threshold: u64,
) -> Result<i64> {
    let metadata = serde_json::json!({
        "trigger": "annotation_hook",
        "tick_kind": "accretion_threshold",
        "annotation_id": annotation_id,
        "count_since_cursor": count_since_cursor,
        "accretion_cursor": accretion_cursor,
        "threshold": threshold,
        // Same field the accretion chain's `load_recent_annotations_for_slug`
        // reads as required envelope field; tied to the threshold that
        // just crossed so the load window matches the cause.
        "window_n": count_since_cursor.max(threshold as i64),
    })
    .to_string();
    observation_events::write_observation_event(
        conn,
        slug,
        "scheduler",
        "accretion_threshold_hit",
        None,
        None,
        None,
        None,
        None,
        None,
        Some(&metadata),
    )
    .with_context(|| {
        format!(
            "pyramid_scheduler: failed to emit accretion_threshold_hit for slug '{slug}' \
             annotation #{annotation_id}"
        )
    })
}

/// Count annotations on a slug since its active `accretion_cursor`.
/// Used by the annotation hook to decide whether the slug has passed
/// the volume threshold.
///
/// Returns (count_since_cursor, cursor). If `pyramid_slugs` has no
/// row (slug missing), returns `(0, 0)` so callers don't raise.
pub fn count_annotations_since_cursor(conn: &Connection, slug: &str) -> Result<(i64, i64)> {
    let cursor: i64 = conn
        .query_row(
            "SELECT COALESCE(accretion_cursor, 0) FROM pyramid_slugs WHERE slug = ?1",
            rusqlite::params![slug],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .unwrap_or(0);
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pyramid_annotations WHERE slug = ?1 AND id > ?2",
            rusqlite::params![slug, cursor],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0);
    Ok((count, cursor))
}

/// Handle for a spawned scheduler task; dropping the handle does NOT
/// stop the loop — it runs for the process lifetime. Returned so the
/// caller can keep a reference (currently unused; future add:
/// cancellation token).
#[allow(dead_code)]
pub struct SchedulerHandle {
    pub accretion_task: tauri::async_runtime::JoinHandle<()>,
    pub sweep_task: tauri::async_runtime::JoinHandle<()>,
}

/// Spawn the two periodic loops. Both open their own read connection
/// each tick (short-lived, released immediately after emit) so the
/// scheduler never contends with the shared reader.
///
/// `db_path` is the pyramid.db file path; we open a fresh connection
/// per tick to avoid holding a shared Mutex across the long sleep.
pub fn spawn(db_path: PathBuf) -> SchedulerHandle {
    let db_path_accretion = db_path.clone();
    let accretion_task = tauri::async_runtime::spawn(async move {
        // Short stagger so scheduler doesn't collide with boot-time
        // DB init writes. 10s matches the pyramid sync timer pattern.
        tokio::time::sleep(Duration::from_secs(10)).await;

        // Load the initial interval. We re-read on every tick below,
        // so operator supersessions land within one period.
        let initial_interval = {
            match super::db::open_pyramid_connection(&db_path_accretion) {
                Ok(conn) => load_config(&conn).accretion_interval_secs,
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "pyramid_scheduler[accretion]: initial config load failed — using default"
                    );
                    DEFAULT_ACCRETION_INTERVAL_SECS
                }
            }
        };
        let mut interval = tokio::time::interval(Duration::from_secs(initial_interval));
        interval.tick().await; // consume immediate first tick; first emit is one period out

        loop {
            interval.tick().await;
            match super::db::open_pyramid_connection(&db_path_accretion) {
                Ok(conn) => {
                    match emit_accretion_tick(&conn) {
                        Ok(n) => tracing::debug!(
                            emitted = n,
                            "pyramid_scheduler[accretion]: tick emitted"
                        ),
                        Err(e) => tracing::warn!(
                            error = %e,
                            "pyramid_scheduler[accretion]: tick emit failed"
                        ),
                    }
                    // Rebuild the interval if the operator changed
                    // the period. tokio::time::interval doesn't
                    // dynamically re-tune; we swap it instead.
                    let now_interval = load_config(&conn).accretion_interval_secs;
                    if now_interval != initial_interval
                        && now_interval != interval.period().as_secs()
                    {
                        tracing::info!(
                            old_secs = interval.period().as_secs(),
                            new_secs = now_interval,
                            "pyramid_scheduler[accretion]: interval changed — re-tuning"
                        );
                        interval = tokio::time::interval(Duration::from_secs(now_interval));
                        interval.tick().await; // consume immediate tick
                    }
                }
                Err(e) => tracing::warn!(
                    error = %e,
                    "pyramid_scheduler[accretion]: could not open DB connection for tick"
                ),
            }
        }
    });

    let db_path_sweep = db_path.clone();
    let sweep_task = tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(20)).await;

        let initial_interval = {
            match super::db::open_pyramid_connection(&db_path_sweep) {
                Ok(conn) => load_config(&conn).sweep_interval_secs,
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "pyramid_scheduler[sweep]: initial config load failed — using default"
                    );
                    DEFAULT_SWEEP_INTERVAL_SECS
                }
            }
        };
        let mut interval = tokio::time::interval(Duration::from_secs(initial_interval));
        interval.tick().await;

        loop {
            interval.tick().await;
            match super::db::open_pyramid_connection(&db_path_sweep) {
                Ok(conn) => {
                    match emit_sweep_tick(&conn) {
                        Ok(n) => tracing::debug!(
                            emitted = n,
                            "pyramid_scheduler[sweep]: tick emitted"
                        ),
                        Err(e) => tracing::warn!(
                            error = %e,
                            "pyramid_scheduler[sweep]: tick emit failed"
                        ),
                    }
                    let now_interval = load_config(&conn).sweep_interval_secs;
                    if now_interval != initial_interval
                        && now_interval != interval.period().as_secs()
                    {
                        tracing::info!(
                            old_secs = interval.period().as_secs(),
                            new_secs = now_interval,
                            "pyramid_scheduler[sweep]: interval changed — re-tuning"
                        );
                        interval = tokio::time::interval(Duration::from_secs(now_interval));
                        interval.tick().await;
                    }
                }
                Err(e) => tracing::warn!(
                    error = %e,
                    "pyramid_scheduler[sweep]: could not open DB connection for tick"
                ),
            }
        }
    });

    tracing::info!("[pyramid_scheduler] spawned accretion + sweep periodic tasks");
    SchedulerHandle {
        accretion_task,
        sweep_task,
    }
}

// Suppress unused-import warning when no ambient `Arc<Mutex>`
// consumers need these — scheduled here so a future cancellation
// refactor can lean on them.
#[allow(dead_code)]
fn _unused_typing_ping(_a: Option<Arc<Mutex<Connection>>>) {}
