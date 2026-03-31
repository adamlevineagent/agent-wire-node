// pyramid/sync.rs — Pyramid publication sync timer
//
// WS-ONLINE-A: Automatic publication of pyramids to the Wire.
// Separate from corpus sync (src/sync.rs) because concerns are fundamentally
// different: corpus sync is file-level, pyramid sync is SQLite-level.
//
// The sync timer ticks at a configurable interval (default 60s) and checks
// each linked pyramid for unpublished builds. If a build completed since the
// last publication, it triggers publication to the Wire.
//
// IMPORTANT: rusqlite::Connection is !Send. All DB access must happen
// synchronously within a lock scope that is dropped BEFORE any .await.
// The async publish phase uses pre-loaded data only.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::db;
use super::publication;
use super::wire_publish::{self, PyramidPublisher};
use super::PyramidState;

// ─── Types ───────────────────────────────────────────────────────────────────

/// Configuration for a single pyramid's publication link.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PyramidPublicationLink {
    pub slug: String,
    pub auto_publish: bool,
}

/// Configuration for a pinned pyramid's download link (WS-ONLINE-D).
#[derive(Debug, Clone)]
pub struct PinnedPyramidLink {
    pub slug: String,
    pub source_tunnel_url: String,
    /// Last known build_id from the remote pyramid (for change detection).
    pub last_known_build_id: Option<String>,
    /// When the last refresh completed.
    pub last_refreshed_at: Option<Instant>,
}

/// State for the pyramid sync timer.
///
/// Tracks which pyramids are linked for publication and when the last tick ran.
/// Also tracks pinned pyramids for download-direction sync (WS-ONLINE-D).
/// Kept separate from corpus `SyncState` to avoid coupling file-level and
/// SQLite-level sync concerns.
pub struct PyramidSyncState {
    /// Pyramids linked for publication, keyed by slug.
    pub linked_pyramids: HashMap<String, PyramidPublicationLink>,
    /// Pinned pyramids linked for download-direction sync, keyed by slug (WS-ONLINE-D).
    pub pinned_pyramids: HashMap<String, PinnedPyramidLink>,
    /// When the last tick completed (for diagnostics).
    pub last_tick: Option<Instant>,
}

impl PyramidSyncState {
    pub fn new() -> Self {
        Self {
            linked_pyramids: HashMap::new(),
            pinned_pyramids: HashMap::new(),
            last_tick: None,
        }
    }

    /// Link a pyramid slug for auto-publication.
    pub fn link_pyramid(&mut self, slug: String, auto_publish: bool) {
        self.linked_pyramids
            .insert(slug.clone(), PyramidPublicationLink { slug, auto_publish });
    }

    /// Unlink a pyramid slug from auto-publication.
    pub fn unlink_pyramid(&mut self, slug: &str) {
        self.linked_pyramids.remove(slug);
    }

    /// Register a pinned pyramid for auto-refresh (WS-ONLINE-D).
    pub fn pin_pyramid(&mut self, slug: String, source_tunnel_url: String) {
        self.pinned_pyramids.insert(
            slug.clone(),
            PinnedPyramidLink {
                slug,
                source_tunnel_url,
                last_known_build_id: None,
                last_refreshed_at: None,
            },
        );
    }

    /// Remove a pinned pyramid from auto-refresh tracking (WS-ONLINE-D).
    pub fn unpin_pyramid(&mut self, slug: &str) {
        self.pinned_pyramids.remove(slug);
    }
}

// ─── Pre-loaded data for publish ─────────────────────────────────────────────

/// Everything needed from the DB to publish a pyramid, collected synchronously.
struct SyncPublishData {
    slug: String,
    nodes_by_depth: Vec<(i64, Vec<super::types::PyramidNode>)>,
    already_published: HashMap<String, String>,
    evidence_weights: HashMap<String, HashMap<String, f64>>,
    current_build_id: String,
}

// ─── Sync Tick ───────────────────────────────────────────────────────────────

/// One tick of the pyramid sync timer.
///
/// Iterates all linked pyramids. For each slug with `auto_publish` enabled:
/// 1. Checks if a build is currently in_progress — if so, skips (don't publish
///    incomplete pyramid state).
/// 2. Compares the slug's current build_id (MAX from pyramid_nodes) against
///    `last_published_build_id` in pyramid_slugs.
/// 3. If they differ, pre-loads all data from DB (sync), drops the DB lock,
///    then publishes to Wire (async).
///
/// This function is designed to be called from a `tokio::time::interval` loop.
///
/// `tunnel_url` is the current Cloudflare tunnel URL, if connected. Passed
/// through to discovery metadata publication (WS-ONLINE-B).
pub async fn pyramid_sync_tick(
    pyramid_state: &PyramidState,
    sync_state: &Arc<Mutex<PyramidSyncState>>,
    tunnel_url: Option<String>,
) {
    let links: Vec<PyramidPublicationLink> = {
        let state = sync_state.lock().await;
        state
            .linked_pyramids
            .values()
            .filter(|link| link.auto_publish)
            .cloned()
            .collect()
    };

    if links.is_empty() {
        return;
    }

    for link in &links {
        let slug = &link.slug;

        // Check if a build is currently running — skip if so
        {
            let active_builds = pyramid_state.active_build.read().await;
            if let Some(handle) = active_builds.get(slug.as_str()) {
                let status = handle.status.read().await;
                if status.is_running() {
                    tracing::debug!(
                        slug = %slug,
                        "pyramid_sync_tick: build in progress, skipping"
                    );
                    continue;
                }
            }
        }

        // Phase 1 (SYNC): Check build_ids and pre-load all data from DB.
        // The Connection is !Send, so we MUST drop it before any .await.
        let publish_data: Option<SyncPublishData> = {
            let conn = pyramid_state.reader.lock().await;

            // Compare build_ids
            let current_build_id = match db::get_current_build_id(&conn, slug) {
                Ok(Some(id)) if !id.is_empty() => id,
                _ => {
                    tracing::debug!(slug = %slug, "pyramid_sync_tick: no build_id found, skipping");
                    continue;
                }
            };

            let last_published = db::get_last_published_build_id(&conn, slug).unwrap_or(None);
            if last_published.as_deref() == Some(current_build_id.as_str()) {
                tracing::debug!(
                    slug = %slug,
                    build_id = %current_build_id,
                    "pyramid_sync_tick: already published, skipping"
                );
                continue;
            }

            tracing::info!(
                slug = %slug,
                current_build_id = %current_build_id,
                last_published = ?last_published,
                "pyramid_sync_tick: new build detected, loading data for publication"
            );

            // Load slug info
            let slug_info = match db::get_slug(&conn, slug) {
                Ok(Some(info)) => info,
                Ok(None) => {
                    tracing::warn!(slug = %slug, "pyramid_sync_tick: slug not found");
                    continue;
                }
                Err(e) => {
                    tracing::warn!(slug = %slug, error = %e, "pyramid_sync_tick: failed to load slug info");
                    continue;
                }
            };

            // Pre-load nodes by depth
            let mut nodes_by_depth = Vec::new();
            for depth in 0..=slug_info.max_depth {
                match db::get_nodes_at_depth(&conn, slug, depth) {
                    Ok(nodes) if !nodes.is_empty() => {
                        nodes_by_depth.push((depth, nodes));
                    }
                    Ok(_) => {} // empty layer, skip
                    Err(e) => {
                        tracing::warn!(
                            slug = %slug,
                            depth = depth,
                            error = %e,
                            "pyramid_sync_tick: failed to load nodes at depth"
                        );
                    }
                }
            }

            if nodes_by_depth.is_empty() {
                tracing::debug!(slug = %slug, "pyramid_sync_tick: no nodes found, skipping");
                continue;
            }

            // Pre-load already-published mappings
            let already_published: HashMap<String, String> = db::get_all_id_mappings(&conn, slug)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|m| m.wire_uuid.map(|uuid| (m.local_id, uuid)))
                .collect();

            // Pre-load evidence weights
            let mut evidence_weights: HashMap<String, HashMap<String, f64>> = HashMap::new();
            for (_depth, nodes) in &nodes_by_depth {
                for node in nodes {
                    if let Ok(links) = db::get_keep_evidence_for_target_cross(&conn, slug, &node.id)
                    {
                        if !links.is_empty() {
                            let mut child_weights = HashMap::new();
                            for ev_link in links {
                                if let Some(w) = ev_link.weight {
                                    child_weights.insert(ev_link.source_node_id, w);
                                }
                            }
                            if !child_weights.is_empty() {
                                evidence_weights.insert(node.id.clone(), child_weights);
                            }
                        }
                    }
                }
            }

            Some(SyncPublishData {
                slug: slug.clone(),
                nodes_by_depth,
                already_published,
                evidence_weights,
                current_build_id,
            })
        };
        // conn (reader) is dropped here — safe to .await below

        let data = match publish_data {
            Some(d) => d,
            None => continue,
        };

        // Create publisher with current config
        let (wire_url, wire_auth) = {
            let config = pyramid_state.config.read().await;
            let url =
                std::env::var("WIRE_URL").unwrap_or_else(|_| "https://newsbleach.com".to_string());
            let auth = config.auth_token.clone();
            (url, auth)
        };

        if wire_auth.is_empty() {
            tracing::warn!(
                slug = %slug,
                "pyramid_sync_tick: auth_token not configured, skipping publication"
            );
            continue;
        }

        let publisher = PyramidPublisher::new(wire_url, wire_auth);

        // Phase 2 (ASYNC): Publish to Wire — no DB lock held
        match publisher
            .publish_pyramid_idempotent(
                &data.slug,
                &data.nodes_by_depth,
                &data.already_published,
                &data.evidence_weights,
            )
            .await
        {
            Ok(result) => {
                // Phase 3 (SYNC): Persist results back to DB
                let writer = pyramid_state.writer.lock().await;

                // Ensure id_map table exists
                if let Err(e) = wire_publish::init_id_map_table(&writer) {
                    tracing::warn!(slug = %slug, error = %e, "failed to init id_map table");
                }

                // Persist ID mappings
                for mapping in &result.id_mappings {
                    let uuid = mapping
                        .wire_uuid
                        .as_deref()
                        .unwrap_or(&mapping.wire_handle_path);
                    if let Err(e) =
                        wire_publish::save_id_mapping(&writer, &data.slug, &mapping.local_id, uuid)
                    {
                        tracing::warn!(
                            slug = %data.slug,
                            local_id = %mapping.local_id,
                            error = %e,
                            "failed to persist ID mapping"
                        );
                    }
                }

                // Update last_published_build_id
                if let Err(e) =
                    db::set_last_published_build_id(&writer, &data.slug, &data.current_build_id)
                {
                    tracing::warn!(
                        slug = %data.slug,
                        build_id = %data.current_build_id,
                        error = %e,
                        "failed to update last_published_build_id"
                    );
                }

                // WS-ONLINE-B: Collect metadata while we hold the writer lock
                let metadata_data = publication::collect_metadata_publish_data(
                    &writer,
                    &data.slug,
                    tunnel_url.clone(),
                );

                tracing::info!(
                    slug = %data.slug,
                    node_count = result.node_count,
                    build_id = %data.current_build_id,
                    apex_uuid = ?result.apex_wire_uuid,
                    "pyramid_sync_tick: publication complete"
                );
                // writer dropped here — must drop before .await for !Send safety
                drop(writer);

                // WS-ONLINE-B: Publish discovery metadata (async, no DB lock held)
                match metadata_data {
                    Ok(Some(md)) => {
                        match publisher
                            .publish_pyramid_metadata(&md.metadata, md.supersedes_uuid.as_deref())
                            .await
                        {
                            Ok(new_uuid) => {
                                // Re-acquire writer to persist the new metadata UUID
                                let writer = pyramid_state.writer.lock().await;
                                if let Err(e) = db::set_slug_metadata_contribution_id(
                                    &writer, &data.slug, &new_uuid,
                                ) {
                                    tracing::warn!(
                                        slug = %data.slug,
                                        error = %e,
                                        "pyramid_sync_tick: failed to persist metadata UUID"
                                    );
                                }
                                tracing::info!(
                                    slug = %data.slug,
                                    metadata_uuid = %new_uuid,
                                    "pyramid_sync_tick: metadata published"
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    slug = %data.slug,
                                    error = %e,
                                    "pyramid_sync_tick: metadata publish failed (non-fatal)"
                                );
                            }
                        }
                    }
                    Ok(None) => {
                        tracing::debug!(
                            slug = %data.slug,
                            "pyramid_sync_tick: no metadata to publish"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            slug = %data.slug,
                            error = %e,
                            "pyramid_sync_tick: failed to collect metadata (non-fatal)"
                        );
                    }
                }
            }
            Err(e) => {
                tracing::error!(
                    slug = %data.slug,
                    error = %e,
                    "pyramid_sync_tick: publication failed"
                );
            }
        }
    }

    // Update last_tick timestamp
    {
        let mut state = sync_state.lock().await;
        state.last_tick = Some(Instant::now());
    }
}

// ─── WS-ONLINE-D: Pinned pyramid auto-refresh tick ──────────────────────────

/// One tick of the pinned pyramid refresh timer.
///
/// Iterates all pinned pyramids. For each:
/// 1. Fetches the remote pyramid's metadata to check for updated build timestamps.
/// 2. If the remote build_id has changed, re-pulls the full export.
/// 3. Upserts the new nodes into local SQLite.
///
/// The refresh interval is controlled by the caller (suggested: 5 minutes).
pub async fn pinned_pyramid_refresh_tick(
    pyramid_state: &super::PyramidState,
    sync_state: &Arc<Mutex<PyramidSyncState>>,
) {
    use super::slug;
    use super::wire_import::RemotePyramidClient;

    let pinned_links: Vec<PinnedPyramidLink> = {
        let state = sync_state.lock().await;
        state.pinned_pyramids.values().cloned().collect()
    };

    if pinned_links.is_empty() {
        return;
    }

    // Get Wire JWT for authenticating with remote nodes
    let wire_jwt = {
        let config = pyramid_state.config.read().await;
        config.auth_token.clone()
    };

    if wire_jwt.is_empty() {
        tracing::debug!("pinned_refresh_tick: no auth_token configured, skipping");
        return;
    }

    let wire_server_url =
        std::env::var("WIRE_URL").unwrap_or_else(|_| "https://newsbleach.com".to_string());

    for link in &pinned_links {
        let client = RemotePyramidClient::new(
            link.source_tunnel_url.clone(),
            wire_jwt.clone(),
            wire_server_url.clone(),
        );

        // Check for updated build by fetching metadata (apex node is a lightweight check)
        let remote_build_id = match client.remote_apex(&link.slug).await {
            Ok(apex) => {
                // Extract build_id from the apex response if present
                apex.node
                    .get("build_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            }
            Err(e) => {
                tracing::debug!(
                    slug = %link.slug,
                    tunnel_url = %link.source_tunnel_url,
                    error = %e,
                    "pinned_refresh_tick: failed to check remote apex (non-fatal)"
                );
                continue;
            }
        };

        // Compare with last known build_id
        let needs_refresh = match (&link.last_known_build_id, &remote_build_id) {
            (Some(last), Some(current)) => last != current,
            (None, Some(_)) => true, // Never refreshed, remote has a build
            _ => false,
        };

        if !needs_refresh {
            tracing::debug!(
                slug = %link.slug,
                "pinned_refresh_tick: no new build, skipping"
            );
            continue;
        }

        tracing::info!(
            slug = %link.slug,
            old_build_id = ?link.last_known_build_id,
            new_build_id = ?remote_build_id,
            "pinned_refresh_tick: new build detected, re-pulling"
        );

        // Pull the full export
        match client.pull_remote_pyramid(&link.slug).await {
            Ok(nodes) => {
                // Upsert into local SQLite
                let writer = pyramid_state.writer.lock().await;
                match slug::pin_remote_pyramid(&writer, &link.slug, &link.source_tunnel_url, &nodes)
                {
                    Ok(count) => {
                        tracing::info!(
                            slug = %link.slug,
                            node_count = count,
                            "pinned_refresh_tick: refresh complete"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            slug = %link.slug,
                            error = %e,
                            "pinned_refresh_tick: failed to upsert nodes"
                        );
                        continue;
                    }
                }
                // writer dropped here

                // Update last_known_build_id in sync state
                {
                    let mut state = sync_state.lock().await;
                    if let Some(entry) = state.pinned_pyramids.get_mut(&link.slug) {
                        entry.last_known_build_id = remote_build_id;
                        entry.last_refreshed_at = Some(Instant::now());
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    slug = %link.slug,
                    error = %e,
                    "pinned_refresh_tick: failed to pull remote pyramid"
                );
            }
        }
    }
}
