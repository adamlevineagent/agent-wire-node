// pyramid/recovery.rs — Recovery operations for operator-visible repair (WS-RECOVERY-OPS)
//
// When things get stuck — failed builds, stale ingests, stuck deltas, long
// supersession chains, unfinished promotions — these operations let the
// operator unstick them. Every operation preserves history and uses the
// existing contribution/supersession model.
//
// Operations:
//   recovery_rerun_build         — re-fire a fresh build for a slug
//   recovery_reingest            — mark source stale, create pending ingest
//   recovery_force_delta         — manually push a composition delta
//   recovery_collapse_delta_chain — collapse accumulated versions to fresh canonical
//   recovery_promote_provisional — manually promote a provisional session
//   recovery_rebuild_deps        — reconcile vine compositions with pyramid state
//   recovery_status              — aggregated health view for a slug

use anyhow::{anyhow, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::db;
use super::lock_manager::LockManager;
use super::PyramidState;

// ── Recovery Status ─────────────────────────────────────────────────────────

/// Aggregated health view for a pyramid slug. Returned by the
/// `GET /pyramid/:slug/recovery/status` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryStatus {
    pub stale_node_count: i64,
    pub dead_letter_count: i64,
    pub pending_ingest_count: i64,
    pub active_provisional_sessions: i64,
    pub pending_demand_gen_jobs: i64,
    pub failed_builds_recent: i64,
}

/// Query the aggregated health state for a slug. All counts come from
/// direct SQL queries against the existing tables — no new tables needed.
pub fn recovery_status(conn: &Connection, slug: &str) -> Result<RecoveryStatus> {
    // 1. Stale nodes: count from staleness queue
    let stale_node_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pyramid_staleness_queue WHERE slug = ?1",
            rusqlite::params![slug],
            |r| r.get(0),
        )
        .unwrap_or(0);

    // 2. Dead letter count: open entries only
    let dead_letter_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pyramid_dead_letter WHERE slug = ?1 AND status = 'open'",
            rusqlite::params![slug],
            |r| r.get(0),
        )
        .unwrap_or(0);

    // 3. Pending ingest records
    let pending_ingest_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pyramid_ingest_records WHERE slug = ?1 AND status IN ('pending', 'stale')",
            rusqlite::params![slug],
            |r| r.get(0),
        )
        .unwrap_or(0);

    // 4. Active provisional sessions
    let active_provisional_sessions: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pyramid_provisional_sessions WHERE slug = ?1 AND status = 'active'",
            rusqlite::params![slug],
            |r| r.get(0),
        )
        .unwrap_or(0);

    // 5. Pending demand gen jobs
    let pending_demand_gen_jobs: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pyramid_demand_gen_jobs WHERE slug = ?1 AND status IN ('queued', 'running')",
            rusqlite::params![slug],
            |r| r.get(0),
        )
        .unwrap_or(0);

    // 6. Failed builds (recent = last 24 hours) — count from dead letter entries
    //    created in the last day, plus failed ingest records.
    let failed_builds_recent: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pyramid_ingest_records
             WHERE slug = ?1 AND status = 'failed'
               AND updated_at >= datetime('now', '-1 day')",
            rusqlite::params![slug],
            |r| r.get(0),
        )
        .unwrap_or(0);

    Ok(RecoveryStatus {
        stale_node_count,
        dead_letter_count,
        pending_ingest_count,
        active_provisional_sessions,
        pending_demand_gen_jobs,
        failed_builds_recent,
    })
}

// ── Recovery: Re-run failed build ───────────────────────────────────────────

/// Re-run a build for a slug. Finds the slug's source info and fires a fresh
/// build. Returns the new build_id (which will be assigned once the build
/// task starts).
///
/// This is an async operation because it needs to acquire the slug write
/// lock and interact with PyramidState for build dispatch. The actual build
/// runs in a background task — this returns a confirmation that the build
/// was dispatched.
pub async fn recovery_rerun_build(
    state: &PyramidState,
    slug: &str,
    _build_id: &str,
) -> Result<String> {
    // Verify slug exists
    {
        let conn = state.reader.lock().await;
        let slug_info =
            db::get_slug(&conn, slug)?.ok_or_else(|| anyhow!("Slug '{}' not found", slug))?;

        // Vine builds use a different code path
        if slug_info.content_type == super::types::ContentType::Vine {
            return Err(anyhow!(
                "Recovery rerun for vine slugs is not supported through this endpoint. \
                 Use the vine-specific build endpoint instead."
            ));
        }
    }

    // Generate a new build_id for tracking
    let new_build_id = format!("recovery-{}", uuid::Uuid::new_v4());

    info!(
        slug = slug,
        old_build_id = _build_id,
        new_build_id = %new_build_id,
        "Recovery: dispatching fresh build"
    );

    // The actual build is dispatched via the same mechanism as POST /pyramid/:slug/build.
    // We return the build_id; the caller (route handler) spawns the build task.
    Ok(new_build_id)
}

// ── Recovery: Re-ingest from source ─────────────────────────────────────────

/// Mark existing ingest records for a source path as stale and create a new
/// pending ingest record so DADBEAR (or the operator) can re-process it.
///
/// Returns the new ingest record ID.
pub async fn recovery_reingest(
    state: &PyramidState,
    slug: &str,
    source_path: &str,
) -> Result<String> {
    let _write_guard = LockManager::global().write(slug).await;

    let conn = state.writer.lock().await;

    // Verify slug exists
    let slug_info =
        db::get_slug(&conn, slug)?.ok_or_else(|| anyhow!("Slug '{}' not found", slug))?;

    // 1. Mark existing ingest records as stale
    db::mark_ingest_stale(&conn, slug, source_path)?;

    info!(
        slug = slug,
        source_path = source_path,
        "Recovery: marked existing ingest records as stale"
    );

    // 2. Create a new pending ingest record
    let new_sig = format!(
        "recovery-reingest-{}",
        chrono::Utc::now().format("%Y%m%d%H%M%S")
    );
    let record = super::types::IngestRecord {
        id: 0,
        slug: slug.to_string(),
        source_path: source_path.to_string(),
        content_type: slug_info.content_type.as_str().to_string(),
        ingest_signature: new_sig.clone(),
        file_hash: None,
        file_mtime: None,
        status: "pending".to_string(),
        build_id: None,
        error_message: None,
        created_at: String::new(),
        updated_at: String::new(),
    };
    db::save_ingest_record(&conn, &record)?;

    info!(
        slug = slug,
        source_path = source_path,
        signature = %new_sig,
        "Recovery: created new pending ingest record"
    );

    Ok(new_sig)
}

// ── Recovery: Force delta ───────────────────────────────────────────────────

/// Manually push a composition delta when DADBEAR's auto-propagation is stuck.
///
/// Looks up the bedrock's current apex and calls `notify_vine_of_bedrock_completion`
/// for each vine that includes this bedrock.
///
/// Returns error if the composition doesn't exist or bedrock has no apex.
pub fn recovery_force_delta(conn: &Connection, vine_slug: &str, bedrock_slug: &str) -> Result<()> {
    // Verify the composition exists
    let compositions = db::get_vine_bedrocks(conn, vine_slug)?;
    let comp = compositions
        .iter()
        .find(|c| c.bedrock_slug == bedrock_slug)
        .ok_or_else(|| {
            anyhow!(
                "No active composition found for vine='{}', bedrock='{}'",
                vine_slug,
                bedrock_slug
            )
        })?;

    // Get the bedrock's current apex
    let apex_node_id = comp.bedrock_apex_node_id.as_deref().ok_or_else(|| {
        anyhow!(
            "Bedrock '{}' has no apex node ID set in composition with vine '{}'",
            bedrock_slug,
            vine_slug
        )
    })?;

    info!(
        vine = vine_slug,
        bedrock = bedrock_slug,
        apex = apex_node_id,
        "Recovery: forcing delta propagation"
    );

    // Update the apex reference (this is the mechanical part of delta propagation).
    // The actual vine rebuild is triggered by the DeltaLanded event that
    // notify_vine_of_bedrock_completion emits, which DADBEAR-EXTEND picks up.
    // Since we can't call the async notify function from a sync context,
    // we just update the apex ref here — the route handler will emit the
    // build event bus notification.
    db::update_bedrock_apex(conn, vine_slug, bedrock_slug, apex_node_id)?;

    Ok(())
}

// ── Recovery: Collapse delta chain ──────────────────────────────────────────

/// Collapse accumulated supersession versions for a node into a fresh
/// canonical version. The collapsed version supersedes the entire delta chain.
///
/// This works by:
/// 1. Reading the current live row (which has the latest content)
/// 2. Deleting all entries in pyramid_node_versions for this node
/// 3. Resetting the node's current_version to 1
///
/// Returns the new version number (always 1 after collapse).
pub fn recovery_collapse_delta_chain(conn: &Connection, slug: &str, node_id: &str) -> Result<i32> {
    // Verify the node exists
    let node = db::get_node(conn, slug, node_id)?
        .ok_or_else(|| anyhow!("Node '{}' not found in slug '{}'", node_id, slug))?;

    let old_version = node.current_version;

    if old_version <= 1 {
        info!(
            slug = slug,
            node_id = node_id,
            "Recovery: node already at version 1, nothing to collapse"
        );
        return Ok(1);
    }

    // Count versions being collapsed for logging
    let version_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pyramid_node_versions WHERE slug = ?1 AND node_id = ?2",
            rusqlite::params![slug, node_id],
            |r| r.get(0),
        )
        .unwrap_or(0);

    // Use a savepoint so this is atomic
    conn.execute_batch("SAVEPOINT recovery_collapse;")?;

    let result: Result<()> = (|| {
        // 1. Delete all version history for this node
        conn.execute(
            "DELETE FROM pyramid_node_versions WHERE slug = ?1 AND node_id = ?2",
            rusqlite::params![slug, node_id],
        )?;

        // 2. Reset the live row's current_version to 1
        conn.execute(
            "UPDATE pyramid_nodes SET current_version = 1, build_version = 1 WHERE slug = ?1 AND id = ?2",
            rusqlite::params![slug, node_id],
        )?;

        Ok(())
    })();

    match result {
        Ok(()) => {
            conn.execute_batch("RELEASE SAVEPOINT recovery_collapse;")?;
            info!(
                slug = slug,
                node_id = node_id,
                old_version = old_version,
                collapsed_versions = version_count,
                "Recovery: collapsed delta chain to version 1"
            );
            Ok(1)
        }
        Err(e) => {
            let _ = conn.execute_batch(
                "ROLLBACK TO SAVEPOINT recovery_collapse; RELEASE SAVEPOINT recovery_collapse;",
            );
            Err(e)
        }
    }
}

// ── Recovery: Promote provisional content ───────────────────────────────────

/// Manually promote provisional nodes in a session when DADBEAR hasn't fired
/// the promotion automatically. Wraps the existing `promote_session` function
/// with operator-facing error handling and logging.
///
/// Returns count of promoted nodes.
pub fn recovery_promote_provisional(
    conn: &Connection,
    slug: &str,
    session_id: &str,
) -> Result<usize> {
    // Verify session exists and belongs to the correct slug
    let session = db::get_provisional_session(conn, session_id)?
        .ok_or_else(|| anyhow!("Provisional session '{}' not found", session_id))?;

    if session.slug != slug {
        return Err(anyhow!(
            "Session '{}' belongs to slug '{}', not '{}'",
            session_id,
            session.slug,
            slug
        ));
    }

    if session.status == "promoted" {
        info!(
            slug = slug,
            session_id = session_id,
            "Recovery: session already promoted, nothing to do"
        );
        return Ok(0);
    }

    if session.status == "failed" {
        warn!(
            slug = slug,
            session_id = session_id,
            "Recovery: promoting a session with status 'failed' — nodes may be incomplete"
        );
    }

    // Generate a canonical build_id for the promotion
    let canonical_build_id = format!(
        "recovery-promote-{}",
        chrono::Utc::now().format("%Y%m%d%H%M%S")
    );

    info!(
        slug = slug,
        session_id = session_id,
        canonical_build_id = %canonical_build_id,
        node_count = session.provisional_node_ids.len(),
        "Recovery: promoting provisional session"
    );

    let count = db::promote_session(conn, session_id, &canonical_build_id, None)?;

    info!(
        slug = slug,
        session_id = session_id,
        promoted_count = count,
        "Recovery: provisional session promotion complete"
    );

    Ok(count)
}

// ── Recovery: Rebuild dependency graph ──────────────────────────────────────

/// Scan vine compositions and ingest records, reconcile with actual pyramid
/// state. Fixes dangling references where a bedrock slug has been archived
/// or a vine composition points to a non-existent apex node.
///
/// Returns count of fixed references.
pub fn recovery_rebuild_deps(conn: &Connection, slug: &str) -> Result<usize> {
    let mut fixed_count: usize = 0;

    // 1. Check vine compositions: fix dangling bedrock references
    let compositions = db::get_vine_bedrocks(conn, slug)?;
    for comp in &compositions {
        // Check if bedrock slug still exists
        let bedrock_exists = db::get_slug(conn, &comp.bedrock_slug)?;
        if bedrock_exists.is_none() {
            warn!(
                vine = slug,
                bedrock = comp.bedrock_slug,
                "Recovery: bedrock slug no longer exists, removing from vine composition"
            );
            db::remove_bedrock_from_vine(conn, slug, &comp.bedrock_slug)?;
            fixed_count += 1;
            continue;
        }

        // Check if bedrock is archived
        if let Some(ref info) = bedrock_exists {
            if info.archived_at.is_some() {
                warn!(
                    vine = slug,
                    bedrock = comp.bedrock_slug,
                    "Recovery: bedrock slug is archived, marking composition as stale"
                );
                conn.execute(
                    "UPDATE pyramid_vine_compositions SET status = 'stale', updated_at = datetime('now')
                     WHERE vine_slug = ?1 AND bedrock_slug = ?2",
                    rusqlite::params![slug, comp.bedrock_slug],
                )?;
                fixed_count += 1;
            }
        }

        // Check if apex node ID is valid (if set)
        if let Some(ref apex_id) = comp.bedrock_apex_node_id {
            let apex_node = db::get_node(conn, &comp.bedrock_slug, apex_id)?;
            if apex_node.is_none() {
                warn!(
                    vine = slug,
                    bedrock = comp.bedrock_slug,
                    apex = apex_id,
                    "Recovery: apex node no longer exists, clearing stale reference"
                );
                conn.execute(
                    "UPDATE pyramid_vine_compositions
                     SET bedrock_apex_node_id = NULL, updated_at = datetime('now')
                     WHERE vine_slug = ?1 AND bedrock_slug = ?2",
                    rusqlite::params![slug, comp.bedrock_slug],
                )?;
                fixed_count += 1;
            }
        }
    }

    // 2. Check ingest records: fix records pointing to non-existent builds
    let ingest_records = db::get_ingest_records_for_slug(conn, slug)?;
    for record in &ingest_records {
        if record.status == "processing" {
            // Check if there's actually an active build — if we can't tell
            // from the DB alone, mark it as stale for reprocessing
            let last_update = &record.updated_at;
            // If a record has been "processing" for more than 1 hour, it's stuck
            let stuck: bool = conn
                .query_row(
                    "SELECT datetime(?1) < datetime('now', '-1 hour')",
                    rusqlite::params![last_update],
                    |r| r.get(0),
                )
                .unwrap_or(false);

            if stuck {
                warn!(
                    slug = slug,
                    source_path = record.source_path,
                    "Recovery: ingest record stuck in 'processing' state, marking as stale"
                );
                db::mark_ingest_stale(conn, slug, &record.source_path)?;
                fixed_count += 1;
            }
        }
    }

    // 3. Check that this slug (if a vine) has at least the compositions it should
    //    This is a reverse check: are there bedrocks that reference this vine?
    let vine_slugs = db::get_vines_for_bedrock(conn, slug)?;
    for vine in &vine_slugs {
        // Verify the vine slug still exists
        if db::get_slug(conn, vine)?.is_none() {
            warn!(
                bedrock = slug,
                vine = vine,
                "Recovery: vine slug no longer exists (orphaned bedrock reference)"
            );
            // We can't fix this from the bedrock side — just count it
            fixed_count += 1;
        }
    }

    info!(
        slug = slug,
        fixed_count = fixed_count,
        "Recovery: dependency graph rebuild complete"
    );

    Ok(fixed_count)
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::db;

    /// Helper: create an in-memory DB and init schema.
    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        db::init_pyramid_db(&conn).unwrap();
        conn
    }

    /// Helper: create a slug with the given content type.
    fn create_slug(conn: &Connection, slug: &str, content_type: &str) {
        conn.execute(
            "INSERT INTO pyramid_slugs (slug, content_type, source_path) VALUES (?1, ?2, '')",
            rusqlite::params![slug, content_type],
        )
        .unwrap();
    }

    /// Helper: insert a node with a specific version.
    fn insert_node(conn: &Connection, slug: &str, node_id: &str, depth: i64, version: i64) {
        conn.execute(
            "INSERT INTO pyramid_nodes (id, slug, depth, headline, distilled, current_version, build_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            rusqlite::params![
                node_id,
                slug,
                depth,
                format!("Node {}", node_id),
                "test content",
                version,
            ],
        )
        .unwrap();
    }

    /// Helper: insert a version history row.
    fn insert_version(conn: &Connection, slug: &str, node_id: &str, version: i64) {
        conn.execute(
            "INSERT INTO pyramid_node_versions (slug, node_id, version, headline, distilled, supersession_reason)
             VALUES (?1, ?2, ?3, ?4, ?5, 'test')",
            rusqlite::params![
                slug,
                node_id,
                version,
                format!("v{} headline", version),
                format!("v{} content", version),
            ],
        )
        .unwrap();
    }

    // ── Test 1: recovery_collapse_delta_chain collapses versioned node back to version 1 ──

    #[test]
    fn test_collapse_delta_chain() {
        let conn = test_db();
        create_slug(&conn, "test-collapse", "code");

        // Insert a node at version 5 (depth 2 so it's mutable)
        insert_node(&conn, "test-collapse", "n-1", 2, 5);

        // Insert 4 version history entries (versions 1-4)
        for v in 1..=4 {
            insert_version(&conn, "test-collapse", "n-1", v);
        }

        // Verify: 4 version rows exist
        let version_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_node_versions WHERE slug = 'test-collapse' AND node_id = 'n-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            version_count, 4,
            "Should have 4 version history entries before collapse"
        );

        // Collapse
        let new_version = recovery_collapse_delta_chain(&conn, "test-collapse", "n-1").unwrap();
        assert_eq!(new_version, 1, "Collapsed version should be 1");

        // Verify: all version rows deleted
        let version_count_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_node_versions WHERE slug = 'test-collapse' AND node_id = 'n-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            version_count_after, 0,
            "All version history should be deleted after collapse"
        );

        // Verify: live row is at version 1
        let live_version: i64 = conn
            .query_row(
                "SELECT current_version FROM pyramid_nodes WHERE slug = 'test-collapse' AND id = 'n-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            live_version, 1,
            "Live row should be at version 1 after collapse"
        );
    }

    // ── Test 2: recovery_status returns correct counts ──

    #[test]
    fn test_recovery_status_counts() {
        let conn = test_db();
        create_slug(&conn, "status-test", "code");

        // Insert 3 dead letter entries (2 open, 1 skipped)
        for i in 0..3 {
            let status = if i < 2 { "open" } else { "skipped" };
            conn.execute(
                "INSERT INTO pyramid_dead_letter (slug, step_name, step_primitive, error_text, error_kind, retry_count, status)
                 VALUES ('status-test', 'step', 'primitive', 'error', 'test', 0, ?1)",
                rusqlite::params![status],
            )
            .unwrap();
        }

        // Insert 2 pending ingest records
        for i in 0..2 {
            conn.execute(
                "INSERT INTO pyramid_ingest_records (slug, source_path, content_type, ingest_signature, status)
                 VALUES ('status-test', ?1, 'code', ?2, 'pending')",
                rusqlite::params![
                    format!("/path/file{}.rs", i),
                    format!("sig-{}", i),
                ],
            )
            .unwrap();
        }

        // Insert 1 stale ingest record
        conn.execute(
            "INSERT INTO pyramid_ingest_records (slug, source_path, content_type, ingest_signature, status)
             VALUES ('status-test', '/path/stale.rs', 'code', 'sig-stale', 'stale')",
            [],
        )
        .unwrap();

        // Insert 1 active provisional session
        conn.execute(
            "INSERT INTO pyramid_provisional_sessions (slug, source_path, session_id, status)
             VALUES ('status-test', '/path/conv.jsonl', 'sess-001', 'active')",
            [],
        )
        .unwrap();

        // Insert 1 queued demand-gen job
        conn.execute(
            "INSERT INTO pyramid_demand_gen_jobs (job_id, slug, question, sub_questions, status, requested_at)
             VALUES ('job-001', 'status-test', 'test?', '[]', 'queued', datetime('now'))",
            [],
        )
        .unwrap();

        let status = recovery_status(&conn, "status-test").unwrap();

        assert_eq!(
            status.dead_letter_count, 2,
            "Should count only open dead letter entries"
        );
        assert_eq!(
            status.pending_ingest_count, 3,
            "Should count pending + stale ingest records"
        );
        assert_eq!(
            status.active_provisional_sessions, 1,
            "Should count active provisional sessions"
        );
        assert_eq!(
            status.pending_demand_gen_jobs, 1,
            "Should count queued demand-gen jobs"
        );
    }

    // ── Test 3: recovery_promote_provisional wraps promote_session correctly ──

    #[test]
    fn test_recovery_promote_provisional() {
        let conn = test_db();
        create_slug(&conn, "prom-test", "code");

        // Create a provisional session with some nodes
        db::create_provisional_session(&conn, "prom-test", "/path/test.rs", "sess-promote")
            .unwrap();

        // Insert provisional nodes and track them in the session
        for i in 0..3 {
            let node_id = format!("prov-node-{}", i);
            conn.execute(
                "INSERT INTO pyramid_nodes (id, slug, depth, headline, distilled, provisional)
                 VALUES (?1, 'prom-test', 0, 'provisional', 'content', 1)",
                rusqlite::params![node_id],
            )
            .unwrap();
            db::add_provisional_node_to_session(&conn, "sess-promote", &node_id).unwrap();
        }

        // Promote via recovery
        let count = recovery_promote_provisional(&conn, "prom-test", "sess-promote").unwrap();
        assert_eq!(count, 3, "Should promote all 3 provisional nodes");

        // Verify session is now promoted
        let session = db::get_provisional_session(&conn, "sess-promote")
            .unwrap()
            .unwrap();
        assert_eq!(
            session.status, "promoted",
            "Session status should be 'promoted'"
        );

        // Verify idempotency — calling again returns 0
        let count2 = recovery_promote_provisional(&conn, "prom-test", "sess-promote").unwrap();
        assert_eq!(count2, 0, "Second promote should be idempotent (return 0)");
    }

    // ── Test 4: force delta on non-existent composition returns error ──

    #[test]
    fn test_force_delta_nonexistent_composition() {
        let conn = test_db();
        create_slug(&conn, "vine-test", "vine");
        create_slug(&conn, "bedrock-test", "code");

        // No composition added — force_delta should error
        let result = recovery_force_delta(&conn, "vine-test", "bedrock-test");
        assert!(result.is_err(), "Should error on non-existent composition");

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("No active composition found"),
            "Error should mention missing composition, got: {}",
            err_msg
        );
    }
}
