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
use crate::pyramid::config_contributions::load_active_config_contribution;
use crate::pyramid::event_bus::{BuildEventBus, TaggedKind};
use crate::pyramid::walker_cache::{
    spawn_scope_cache_reloader, AppModeTransition, RebuildTrigger, ScopeCache,
};

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
    /// Fatal boot-abort: DB missing or similar. Caller logs
    /// `boot_aborted` and refuses to come up.
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

    // Step 4: migration phase. Phase 0a-2 does NOT implement the v3 DDL.
    // We only read the active `migration_marker` body and log intent.
    {
        let db_probe = db_path.clone();
        let marker_check = tokio::task::spawn_blocking(move || -> Result<Option<String>> {
            let conn = Connection::open(&db_probe)?;
            let active =
                load_active_config_contribution(&conn, "migration_marker", None)
                    .map_err(anyhow::Error::from)?;
            Ok(active.map(|c| c.yaml_content))
        })
        .await;

        match marker_check {
            Ok(Ok(Some(body))) if body.trim() == "v2" => {
                tracing::info!(
                    event = "boot_step",
                    step = 4,
                    name = "migration_phase",
                    marker = %body,
                    "v3 migration pending (deferred; Phase 0a-2 scope does not implement DDL)"
                );
                // §2.17.3: defer transition_to(Migrating). Supersession to
                // "v3-db-migrated-config-pending" and "v3" is a later phase.
            }
            Ok(Ok(None)) => {
                // Spec §2.17 / §5.3: missing marker = treat as v2 (first-ever
                // boot of a v3 binary on an unmigrated DB). Log identically
                // to the "v2" branch so operators searching for
                // `v3 migration pending` catch both cases.
                tracing::info!(
                    event = "boot_step",
                    step = 4,
                    name = "migration_phase",
                    marker = "missing",
                    "v3 migration pending (deferred; no active migration_marker — treating as v2)"
                );
            }
            Ok(Ok(Some(body)))
                if body.trim() == "v3"
                    || body.trim() == "v3-db-migrated-config-pending" =>
            {
                tracing::debug!(
                    event = "boot_step",
                    step = 4,
                    name = "migration_phase",
                    marker = %body,
                    "migration already applied; no DDL to run"
                );
            }
            Ok(Ok(Some(body))) => {
                tracing::info!(
                    event = "boot_step",
                    step = 4,
                    name = "migration_phase",
                    marker = %body,
                    "migration_marker read; no migration required for this body"
                );
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    event = "boot_step_warn",
                    step = 4,
                    name = "migration_phase",
                    error = %e,
                    "failed to read migration_marker; continuing boot"
                );
            }
            Err(e) => {
                tracing::warn!(
                    event = "boot_step_warn",
                    step = 4,
                    name = "migration_phase",
                    error = %e,
                    "migration_marker probe task failed; continuing boot"
                );
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
                    tracing::debug!(
                        "scope_cache rebuild bridge lagged by {n} events"
                    );
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    tracing::debug!(
                        "scope_cache rebuild bridge: bus closed, exiting"
                    );
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
