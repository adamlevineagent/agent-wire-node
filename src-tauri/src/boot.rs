// Walker v3 Phase 0a-2 WS5 — canonical boot coordinator (§2.17).
//
// The production `main.rs` still owns all the surrounding Tauri/Warp/HTTP
// wiring; this module scopes the PART of boot that Phase 0a-2 formalizes:
// the ArcSwap ScopeCache + scope_cache_reloader supervisor + ConfigSynced
// trigger bridge + AppMode transition gate. These are steps 3, 6, 7 of
// §2.17, and they are the steps the walker-v3 runtime depends on for
// correctness. Steps 1-2 (open DB, load bundled manifest via the envelope
// writer) + 4-5 (migration + post-migration rebuild) + 8 (stale-engine
// rehydrate) + 10-11 (HTTP listeners, DADBEAR, chain executor) already
// exist in main.rs and are wired to run in the order §2.17 specifies.
//
// The coordinator is deliberately callable from integration tests without
// a live Tauri runtime: `run_walker_cache_boot` is an async function that
// takes just the DB path + AppMode handle + build_event_bus and returns
// the spawned handles the caller must keep alive for the process lifetime.
//
// §2.17.3 boot-aborts-to-known-states: every failable step in this
// coordinator that cannot recover leaves AppMode in `Quarantined` and
// logs `boot_aborted` before returning. The caller in main.rs uses the
// returned `BootResult` to gate downstream steps (HTTP listeners, chain
// executor spawn).

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use arc_swap::ArcSwap;
use rusqlite::Connection;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::app_mode::{transition_to, AppMode};
use crate::pyramid::event_bus::{BuildEventBus, TaggedKind};
use crate::pyramid::v3_migration::{
    self, V3MigrationError, V3MigrationReport, V3PhaseBError, V3PhaseBReport,
};
use crate::pyramid::walker_cache::{
    spawn_scope_cache_reloader, AppModeTransition, RebuildTrigger, ScopeCache,
};

/// Internal: outcome of boot step 4's migration attempt.
enum MigrationStepOutcome {
    /// Phase A executed and committed. `report` carries the details
    /// logged for operator visibility.
    Ran(V3MigrationReport),
    /// Marker body indicated migration is not needed (`v3`, post-Phase A,
    /// or an unknown marker body this boot doesn't recognize — logged
    /// but not escalated).
    NoOp { marker: String },
}

/// Read the active `migration_marker` contribution's inner `body:` field,
/// returning `None` if no active marker exists or if the body doesn't
/// parse as YAML with a `body:` string. Used by boot step 4 to decide
/// whether Phase A migration should run.
fn read_marker_body(conn: &Connection) -> std::result::Result<Option<String>, rusqlite::Error> {
    use rusqlite::OptionalExtension;
    let yaml: Option<String> = conn
        .query_row(
            "SELECT yaml_content FROM pyramid_config_contributions \
             WHERE schema_type = 'migration_marker' \
               AND status = 'active' \
               AND superseded_by_id IS NULL \
             ORDER BY created_at DESC, id DESC \
             LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let Some(yaml) = yaml else {
        return Ok(None);
    };
    // Parse: try YAML-with-body-field first; fall back to trimmed whole.
    let trimmed: Option<String> = serde_yaml::from_str::<serde_yaml::Value>(&yaml)
        .ok()
        .and_then(|v| {
            v.get("body")
                .and_then(|b| b.as_str())
                .map(|s| s.trim().to_string())
        })
        .or_else(|| Some(yaml.trim().to_string()));
    Ok(trimmed.filter(|s| !s.is_empty()))
}

/// Handles + channels produced by `run_walker_cache_boot`. main.rs must
/// keep these alive for the process lifetime; they are NOT owned by
/// AppState because AppState pre-dates walker v3 and the plan explicitly
/// frames the reloader as an external-to-AppState background supervisor.
#[allow(dead_code)]
pub struct BootHandles {
    /// The ArcSwap the resolver (Phase 0b) will read via `.load_full()`.
    /// main.rs should stash this somewhere it can hand to resolver
    /// readers — for Phase 0a-2 there are no resolver readers yet, so
    /// main.rs just holds the handle.
    pub cache_writer: Arc<ArcSwap<ScopeCache>>,
    /// Sender half of the reloader trigger channel. Cloned to the
    /// ConfigSynced listener and any admin "force rebuild" endpoint.
    pub trigger_tx: mpsc::Sender<RebuildTrigger>,
    /// JoinHandle of the scope_cache_reloader task itself. Kept so a
    /// future clean-shutdown path can `.abort()` it.
    pub reloader_handle: JoinHandle<()>,
    /// JoinHandle of the AppMode transition relay task. Consumes
    /// AppModeTransition::Quarantined messages from the reloader and
    /// flips the global AppMode.
    pub mode_relay_handle: JoinHandle<()>,
    /// JoinHandle of the ConfigSynced→RebuildTrigger bridge. Converts
    /// BuildEventBus ConfigSynced events into reloader triggers.
    pub config_sync_bridge_handle: JoinHandle<()>,
}

/// Boot coordinator result. `ok` = AppMode is Ready at return.
/// `quarantined` = a boot-time check failed and AppMode was flipped to
/// Quarantined; caller should skip spawning build-starters but still
/// bring up HTTP listeners so the operator can see the failure in UI.
pub enum BootResult {
    Ok(BootHandles),
    /// Fatal boot-abort: DB missing, in-progress builds blocking a v3
    /// migration, or an unknown provider_id preventing a clean route.
    /// Caller logs `boot_aborted` and refuses to come up. The message
    /// is operator-visible — it drives the modal copy in the Phase-6 UI
    /// (§2.17.3 boot-aborts-to-known-states).
    Aborted(String),
}

/// Run the walker-v3-owned portion of the canonical §2.17 boot sequence:
/// steps 3, 6, 7, and the final `transition_to(Ready)` on step 9.
///
/// Prerequisites (§2.17 steps 1-2, 4-5, 8 — NOT run by this function):
/// - Connection to the pyramid DB has been opened + initialized.
/// - `migrate_prompts_and_chains_to_contributions` has run (bundled
///   manifest walked via the envelope writer in `BundledBootSkipOnFail`
///   mode).
/// - Provider/schema registries are hydrated.
/// - `stale_engine` rehydration has run or is queued.
///
/// Steps this function implements:
/// 3. Build initial ScopeCache from active contributions → ArcSwap::store.
///    Phase 0b WS-E: calls `walker_resolver::build_scope_cache(&conn)`
///    against a fresh connection. If that fails (malformed walker_*
///    body, etc.), we log at warn and fall back to
///    `ScopeCache::new_empty()` so the ArcSwap is populated and the
///    resolver can still serve defaults — one bad contribution body
///    must not brick boot.
/// 4. Migration phase — reads the active `migration_marker` body. If
///    "v2", logs "v3 migration pending (not yet implemented)" and
///    continues. No destructive DDL in Phase 0a-2.
/// 5. Rebuild ScopeCache post-migration — no-op in Phase 0a-2 (no
///    migration ran). Kept as an explicit step so Phase 0b can fill in.
/// 6. Spawn `scope_cache_reloader` (the supervisor from WS3).
/// 7. Wire ConfigSynced → RebuildTrigger bridge so supersession events
///    drive a rebuild through the reloader.
/// 9. `transition_to(AppMode::Ready)`.
///
/// On any failure in steps 3-7, this function flips AppMode to
/// `Quarantined` and returns `BootResult::Ok(BootHandles)` anyway IF
/// the reloader + relay are up (so the operator can still interact with
/// the node). Truly fatal failures (DB path cannot be used at all)
/// surface as `BootResult::Aborted`.
pub async fn run_walker_cache_boot(
    db_path: String,
    app_mode: Arc<tokio::sync::RwLock<AppMode>>,
    build_event_bus: Arc<BuildEventBus>,
) -> BootResult {
    // §2.17.3: verify the DB path can be opened. If not, this is the
    // boot_aborted state — main.rs should refuse to come up.
    {
        let probe_path = db_path.clone();
        let probe = tokio::task::spawn_blocking(move || Connection::open(&probe_path)).await;
        match probe {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                tracing::error!(
                    event = "boot_aborted",
                    step = "walker_cache_boot.db_probe",
                    error = %e,
                    "boot aborted: pyramid DB cannot be opened"
                );
                return BootResult::Aborted(format!("db open failed: {e}"));
            }
            Err(e) => {
                tracing::error!(
                    event = "boot_aborted",
                    step = "walker_cache_boot.db_probe",
                    error = %e,
                    "boot aborted: db probe task failed"
                );
                return BootResult::Aborted(format!("db probe join failed: {e}"));
            }
        }
    }

    // Step 3: initial ScopeCache → ArcSwap. Phase 0b WS-E builds the real
    // cache via `walker_resolver::build_scope_cache`. If the build fails
    // (malformed walker_* body, for example), we fall back to an empty
    // cache and log at warn — boot MUST NOT brick on a single bad
    // contribution body. The reloader + ConfigSynced bridge wired in
    // steps 6-7 will pick up a corrected supersession and recover the
    // cache to a non-empty state without a restart.
    let initial_cache = {
        let probe_path = db_path.clone();
        let build_result = tokio::task::spawn_blocking(move || -> Result<ScopeCache> {
            let conn = Connection::open(&probe_path)?;
            crate::pyramid::walker_resolver::build_scope_cache(&conn)
        })
        .await;

        match build_result {
            Ok(Ok(cache)) => cache,
            Ok(Err(e)) => {
                tracing::warn!(
                    event = "boot_step_warn",
                    step = 3,
                    name = "scope_cache_initial",
                    error = %e,
                    "build_scope_cache failed at boot; storing empty ScopeCache \
                     and continuing — ConfigSynced-driven rebuild will recover \
                     on the next supersession"
                );
                ScopeCache::new_empty()
            }
            Err(join_err) => {
                tracing::warn!(
                    event = "boot_step_warn",
                    step = 3,
                    name = "scope_cache_initial",
                    error = %join_err,
                    "build_scope_cache task failed at boot; storing empty \
                     ScopeCache and continuing"
                );
                ScopeCache::new_empty()
            }
        }
    };
    let source_count = initial_cache.source_contribution_ids.len();
    let cache_writer = Arc::new(ArcSwap::from_pointee(initial_cache));
    tracing::info!(
        event = "boot_step",
        step = 3,
        name = "scope_cache_initial",
        source_contributions = source_count,
        "ScopeCache populated with {} contributions",
        source_count
    );

    // Step 4: migration phase. W1a wires the real §5.3 Phase A migration.
    //
    // Sequence: probe the active `migration_marker`. If it says `v2` (or
    // missing, per §2.17 / §5.3 "missing = v2"), run
    // `v3_migration::run_v3_phase_a_migration` inside a single SQL
    // transaction. On the known error classes that have operator-visible
    // recovery modals (in-flight builds, unknown provider_ids) we return
    // `BootResult::Aborted` with a precise message — per §2.17.3
    // boot-aborts-to-known-states. On success we log the report and fall
    // through to step 5 (post-migration cache rebuild).
    //
    // The config-file rewrite side of migration (marker → `v3`, removing
    // `primary_model` / `fallback_model_{1,2}` from pyramid_config.json)
    // is W4; §5.3 Phase B. Phase A leaves the marker at
    // `v3-db-migrated-config-pending` deliberately.
    {
        let db_probe = db_path.clone();
        let marker_and_migration = tokio::task::spawn_blocking(
            move || -> std::result::Result<MigrationStepOutcome, V3MigrationError> {
                let mut conn = Connection::open(&db_probe)?;
                let marker_body = read_marker_body(&conn)?;
                match marker_body.as_deref() {
                    // v3 / v3-db-migrated-config-pending: skip.
                    Some("v3") => Ok(MigrationStepOutcome::NoOp {
                        marker: "v3".to_string(),
                    }),
                    Some("v3-db-migrated-config-pending") => Ok(MigrationStepOutcome::NoOp {
                        marker: "v3-db-migrated-config-pending".to_string(),
                    }),
                    // v2 or missing: run Phase A.
                    Some("v2") | None | Some("") => {
                        let data_dir = v3_migration::default_data_dir();
                        let report =
                            v3_migration::run_v3_phase_a_migration(&mut conn, data_dir.as_deref())?;
                        Ok(MigrationStepOutcome::Ran(report))
                    }
                    // Unknown marker body — log, skip migration, continue.
                    Some(other) => Ok(MigrationStepOutcome::NoOp {
                        marker: other.to_string(),
                    }),
                }
            },
        )
        .await;

        match marker_and_migration {
            Ok(Ok(MigrationStepOutcome::Ran(report))) => {
                tracing::info!(
                    event = "boot_step",
                    step = 4,
                    name = "migration_phase",
                    marker_from = %report.marker_transitioned_from,
                    marker_to = %report.marker_transitioned_to,
                    walker_provider_writes = report.walker_provider_contributions_written.len(),
                    walker_call_order_written = report.walker_call_order_written.is_some(),
                    snapshot_rows = report.snapshot_rows_dumped,
                    "v3 migration Phase A committed"
                );
            }
            Ok(Ok(MigrationStepOutcome::NoOp { marker })) => {
                tracing::debug!(
                    event = "boot_step",
                    step = 4,
                    name = "migration_phase",
                    marker = %marker,
                    "v3 migration already applied or not required for this marker body"
                );
            }
            Ok(Err(V3MigrationError::InProgressBuildsBlock(slugs))) => {
                let msg = format!(
                    "Upgrade to walker v3 requires in-progress builds to finish or be marked failed \
                     (blocking slugs: {slugs:?}). [Resume] / [Mark failed] / [Rollback to v2]"
                );
                tracing::error!(
                    event = "boot_aborted",
                    step = 4,
                    name = "migration_phase",
                    reason = "in_progress_builds_block",
                    slugs = ?slugs,
                    "{}", msg
                );
                return BootResult::Aborted(msg);
            }
            Ok(Err(V3MigrationError::UnknownProviderIds { ids })) => {
                let msg = format!(
                    "Upgrade to walker v3 encountered unknown provider_ids in pyramid_tier_routing: {ids:?}. \
                     Investigate and either rename to one of [openrouter, ollama, ollama-local, fleet, market] \
                     or acknowledge via the unknown-provider modal (Phase 6 UI)."
                );
                tracing::error!(
                    event = "boot_aborted",
                    step = 4,
                    name = "migration_phase",
                    reason = "unknown_provider_ids",
                    ids = ?ids,
                    "{}", msg
                );
                return BootResult::Aborted(msg);
            }
            Ok(Err(V3MigrationError::AlreadyMigrated { body })) => {
                tracing::debug!(
                    event = "boot_step",
                    step = 4,
                    name = "migration_phase",
                    marker = %body,
                    "AlreadyMigrated — rerun elided"
                );
            }
            Ok(Err(e)) => {
                let msg = format!("v3 migration failed: {e}");
                tracing::error!(
                    event = "boot_aborted",
                    step = 4,
                    name = "migration_phase",
                    error = %e,
                    "{}", msg
                );
                return BootResult::Aborted(msg);
            }
            Err(join_err) => {
                let msg = format!("migration task failed: {join_err}");
                tracing::error!(
                    event = "boot_aborted",
                    step = 4,
                    name = "migration_phase",
                    error = %join_err,
                    "{}", msg
                );
                return BootResult::Aborted(msg);
            }
        }
    }

    // Step 4.5: Phase B — rewrite pyramid_config.json + supersede
    // marker `v3-db-migrated-config-pending` → `v3`. Runs after Phase A
    // commits and before the final cache rebuild.
    //
    // Idempotent: when the active marker is already `v3`, Phase B
    // returns `AlreadyMigrated` and we log at debug. `PhaseANotRun` is
    // warn-only (shouldn't happen in production; Phase A runs above).
    // Any other error aborts boot with `v3_phase_b_failed` — the temp
    // file (`pyramid_config.json.walker_v3_tmp`) stays on disk for
    // forensics; next boot retries from the marker's pending state.
    {
        let db_probe = db_path.clone();
        let phase_b_result = tokio::task::spawn_blocking(
            move || -> std::result::Result<Option<V3PhaseBReport>, V3PhaseBError> {
                let mut conn = Connection::open(&db_probe)?;
                let Some(data_dir) = v3_migration::default_data_dir() else {
                    tracing::debug!(
                        event = "boot_step",
                        step = 4.5,
                        name = "phase_b_skipped_no_data_dir",
                        "platform data_dir unresolved; Phase B cannot locate \
                         pyramid_config.json — skipping (test mode)"
                    );
                    return Ok(None);
                };
                let report = v3_migration::run_v3_phase_b_migration(&mut conn, &data_dir)?;
                Ok(Some(report))
            },
        )
        .await;

        match phase_b_result {
            Ok(Ok(Some(report))) => {
                tracing::info!(
                    event = "v3_phase_b_complete",
                    bytes_before = report.bytes_before,
                    bytes_after = report.bytes_after,
                    snapshot_id = %report.snapshot_id,
                    marker_transition = %report.marker_transition,
                    "v3 migration Phase B committed"
                );
            }
            Ok(Ok(None)) => { /* test path — no data_dir */ }
            Ok(Err(V3PhaseBError::AlreadyMigrated { body })) => {
                tracing::debug!(
                    event = "v3_phase_b_already_migrated",
                    marker_body = %body,
                    "Phase B skipped — marker already v3"
                );
            }
            Ok(Err(V3PhaseBError::PhaseANotRun)) => {
                tracing::warn!(
                    event = "v3_phase_b_skipped_phase_a_not_run",
                    "Phase B reached with marker pre-migration — Phase A should \
                     have run above; this indicates a boot-order bug, skipping"
                );
            }
            Ok(Err(other)) => {
                let msg = format!(
                    "Phase B failed: {other:?} — pyramid_config.json may still \
                     contain legacy fields; next boot will retry from pending marker"
                );
                tracing::error!(
                    event = "v3_phase_b_failed",
                    error = ?other,
                    "{}", msg
                );
                return BootResult::Aborted(msg);
            }
            Err(join_err) => {
                let msg = format!("Phase B task failed: {join_err}");
                tracing::error!(
                    event = "v3_phase_b_failed",
                    error = %join_err,
                    "{}", msg
                );
                return BootResult::Aborted(msg);
            }
        }
    }

    // Step 5: post-migration rebuild. Phase 0a-2 no-op (step 3 already
    // stored an initial cache; no migration ran). Kept as an explicit log
    // marker so Phase 0b can wire `build_scope_cache` here in one place.
    tracing::debug!(
        event = "boot_step",
        step = 5,
        name = "scope_cache_rebuild_post_migration",
        "Phase 0a-2 no-op (no migration ran)"
    );

    // Step 6: spawn the scope_cache_reloader.
    let (trigger_tx, trigger_rx) = mpsc::channel::<RebuildTrigger>(16);
    let (app_mode_tx, app_mode_rx) = mpsc::channel::<AppModeTransition>(4);

    // Phase 0b WS-E: real rebuild_fn. Plugs `walker_resolver::build_scope_cache`
    // into the reloader. Signature matches the `Fn(&Connection) -> Result<ScopeCache>`
    // bound on `spawn_scope_cache_reloader` exactly. Malformed walker_*
    // bodies surface as `Err(_)` which the reloader logs as non-panic
    // failure (does not burn restart budget). Rust-level panics inside
    // the parser (shouldn't happen in practice) are caught by the
    // reloader's catch_unwind and drive the quarantine path.
    let rebuild_fn = |conn: &Connection| -> Result<ScopeCache> {
        crate::pyramid::walker_resolver::build_scope_cache(conn)
    };

    // Event emitter: fire-and-forget tracing::warn. Phase 0b swaps in a
    // real BuildEventBus emit once the chronicle integration lands.
    let event_emitter = |name: &str, payload: serde_json::Value| {
        tracing::warn!(
            event = name,
            payload = %payload,
            "scope_cache reloader event"
        );
    };

    let reloader_handle = spawn_scope_cache_reloader(
        Arc::clone(&cache_writer),
        trigger_rx,
        rebuild_fn,
        db_path.clone(),
        event_emitter,
        app_mode_tx,
    );
    tracing::info!(
        event = "boot_step",
        step = 6,
        name = "scope_cache_reloader_spawned",
        "scope_cache_reloader task is up"
    );

    // AppMode quarantine relay — the sole secondary writer of AppMode,
    // and the only reason the `{app_mode_single_writer}` invariant is
    // not literally "only main.rs".
    let relay_mode = Arc::clone(&app_mode);
    let mode_relay_handle = tokio::spawn(async move {
        let mut rx = app_mode_rx;
        while let Some(transition) = rx.recv().await {
            match transition {
                AppModeTransition::Quarantined {
                    contribution_id,
                    schema_type,
                    panic_count,
                    window_start,
                } => {
                    tracing::error!(
                        event = "app_mode_quarantine_relay",
                        contribution_id = ?contribution_id,
                        schema_type = ?schema_type,
                        panic_count,
                        window_start = ?window_start,
                        "scope_cache_reloader signalled quarantine; flipping AppMode"
                    );
                    transition_to(&relay_mode, AppMode::Quarantined).await;
                }
            }
        }
        tracing::debug!("app_mode quarantine relay: sender dropped, exiting");
    });

    // Step 7: ConfigSynced → RebuildTrigger bridge.
    let bridge_tx = trigger_tx.clone();
    let mut config_sync_rx = build_event_bus.tx.subscribe();
    let config_sync_bridge_handle = tokio::spawn(async move {
        loop {
            match config_sync_rx.recv().await {
                Ok(evt) => {
                    if let TaggedKind::ConfigSynced {
                        schema_type,
                        contribution_id,
                        ..
                    } = evt.kind
                    {
                        let trig = RebuildTrigger {
                            contribution_id: Some(contribution_id.clone()),
                            schema_type: Some(schema_type.clone()),
                        };
                        // Reloader channel is bounded (16) + debounced. Drop
                        // on full is safe because triggers are idempotent —
                        // the debounce window collapses bursts and the next
                        // supersession / any subsequent ConfigSynced retries.
                        // Emit a warn so operators can see shed if it ever
                        // becomes load-bearing.
                        if let Err(e) = bridge_tx.try_send(trig) {
                            tracing::warn!(
                                event = "scope_cache_trigger_dropped",
                                contribution_id = %contribution_id,
                                schema_type = %schema_type,
                                reason = %e,
                                "ConfigSynced bridge dropped a rebuild trigger \
                                 (reloader queue full or closed); next \
                                 supersession will retry"
                            );
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::debug!("scope_cache rebuild bridge lagged by {n} events");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    tracing::debug!("scope_cache rebuild bridge: bus closed, exiting");
                    break;
                }
            }
        }
    });
    tracing::info!(
        event = "boot_step",
        step = 7,
        name = "config_synced_bridge_spawned",
        "ConfigSynced → scope_cache rebuild bridge is up"
    );

    // Step 7.5 (Phase 2, plan §2.6 + §3): spawn the Ollama probe task.
    // Reads the current scope_chain via ArcSwap::load_full and resolves
    // `ollama_base_url` + `ollama_probe_interval_secs` for the Local
    // provider. The task loops for process lifetime, writing probe
    // results into walker_ollama_probe's global cache. LocalReadiness
    // reads that cache synchronously during Decision build.
    //
    // Resolver path: `ollama_base_url` is per-provider (scope-4) not
    // per-slot, so we resolve at any slot name — "mid" is used here as
    // a stable anchor. The value is shared across all slots via scope
    // inheritance; there's no per-slot variance to probe separately.
    //
    // Invalidation on ConfigSynced is intentionally NOT wired here —
    // the probe task ticks at the resolved interval and naturally picks
    // up the new base_url on the next tick. A fast-path invalidation
    // listener is a Phase 6 UX nicety, not a Phase 2 correctness
    // concern (readiness is strictly conservative during the gap).
    let probe_cache_reader = Arc::clone(&cache_writer);
    let _ollama_probe_handle = tokio::spawn(async move {
        loop {
            // Resolve current base_url + interval from the live scope
            // chain. Defaults (SYSTEM_DEFAULT url / 300s interval) cover
            // the first-boot empty-chain case.
            let (base_url, interval_secs) = {
                let cache = probe_cache_reader.load_full();
                let chain = &*cache.scope_chain;
                let url = crate::pyramid::walker_resolver::resolve_ollama_base_url(
                    chain,
                    "mid",
                    crate::pyramid::walker_resolver::ProviderType::Local,
                );
                let interval = crate::pyramid::walker_resolver::resolve_ollama_probe_interval_secs(
                    chain,
                    "mid",
                    crate::pyramid::walker_resolver::ProviderType::Local,
                );
                (url, interval)
            };
            crate::pyramid::walker_ollama_probe::probe_and_store(&base_url).await;
            let sleep = if interval_secs == 0 {
                std::time::Duration::from_secs(60)
            } else {
                std::time::Duration::from_secs(interval_secs)
            };
            tokio::time::sleep(sleep).await;
        }
    });
    tracing::info!(
        event = "boot_step",
        step = 7,
        name = "ollama_probe_task_spawned",
        "Walker v3 Phase 2 Ollama probe task is up"
    );

    // Step 9: flip AppMode to Ready. Steps 8 (stale_engine), 10 (HTTP),
    // 11 (chain executor + DADBEAR) are main.rs's responsibility — this
    // coordinator hands control back once the reloader is in steady state.
    transition_to(&app_mode, AppMode::Ready).await;
    tracing::info!(
        event = "boot_step",
        step = 9,
        name = "app_mode_ready",
        "walker-v3 boot coordinator handed off to main.rs; AppMode=Ready"
    );

    BootResult::Ok(BootHandles {
        cache_writer,
        trigger_tx,
        reloader_handle,
        mode_relay_handle,
        config_sync_bridge_handle,
    })
}

/// §2.17.3 scaffolding: placeholder entrypoint for the v3 DDL migration
/// body. Phase 0a-2 intentionally leaves this as a documented stub —
/// calling it returns `Ok(())` without running DDL. Phase §5.3 fills in
/// the transactional migration.
///
/// Left here so the error-path shape (Result<(), anyhow::Error>) is
/// stable for the later phase that implements the body + for audit
/// trails that grep for the migration entrypoint name.
#[allow(dead_code)]
pub fn run_v3_migration(_db_path: &Path) -> Result<()> {
    tracing::info!(
        event = "boot_step",
        step = 4,
        name = "v3_migration_stub",
        "v3 migration body not yet implemented; Phase 0a-2 scope"
    );
    Ok(())
}
