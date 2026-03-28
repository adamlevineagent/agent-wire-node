// partner/crystal.rs — Crystallization pass: collapse threads that have accumulated enough deltas
//
// The crystallization pass checks all threads for a given slug and collapses
// any that have crossed the delta threshold or distillation size threshold.
// This runs less frequently than the warm pass (typically triggered by
// buffer overflow or explicit request).

use rusqlite::Connection;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::pyramid::db;
use crate::pyramid::delta;
use crate::pyramid::webbing;

/// Result of a crystallization pass.
#[derive(Debug)]
pub struct CrystalResult {
    pub collapses: usize,
}

/// Run crystallization: check all threads for collapse readiness.
pub async fn crystallize(
    reader: &Arc<Mutex<Connection>>,
    writer: &Arc<Mutex<Connection>>,
    slug: &str,
    api_key: &str,
    collapse_model: &str,
    ops: &crate::pyramid::OperationalConfig,
) -> anyhow::Result<CrystalResult> {
    let mut collapses = 0;

    let threads = {
        let conn = reader.lock().await;
        db::get_threads(&conn, slug)?
    };

    for thread in &threads {
        let needs_collapse = {
            let conn = reader.lock().await;
            delta::check_collapse_needed(&conn, slug, &thread.thread_id, ops)?
        };

        if needs_collapse {
            info!(
                "[crystal] Collapsing thread {} ({} deltas)",
                thread.thread_id, thread.delta_count
            );
            match delta::collapse_thread(
                reader,
                writer,
                slug,
                &thread.thread_id,
                api_key,
                collapse_model,
                ops,
            )
            .await
            {
                Ok(new_id) => {
                    info!(
                        "[crystal] Thread {} collapsed -> {}",
                        thread.thread_id, new_id
                    );
                    collapses += 1;
                }
                Err(e) => {
                    tracing::error!(
                        "[crystal] Failed to collapse thread {}: {}",
                        thread.thread_id,
                        e
                    );
                }
            }
        }
    }

    // Fire-and-forget: check and collapse web edges after crystallization
    {
        let reader = reader.clone();
        let writer = writer.clone();
        let slug = slug.to_string();
        let api_key = api_key.to_string();
        let collapse_model = collapse_model.to_string();
        let tier3 = ops.tier3.clone();
        tokio::spawn(async move {
            match webbing::check_and_collapse_edges(
                &reader,
                &writer,
                &slug,
                &api_key,
                &collapse_model,
                &tier3,
            )
            .await
            {
                Ok(collapsed) => {
                    if collapsed > 0 {
                        info!(
                            "[crystal] web edge collapse pass collapsed {} edges",
                            collapsed
                        );
                    }
                }
                Err(e) => {
                    warn!("[crystal] web edge collapse pass failed: {}", e);
                }
            }
        });
    }

    Ok(CrystalResult { collapses })
}
