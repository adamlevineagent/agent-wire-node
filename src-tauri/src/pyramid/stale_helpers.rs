// pyramid/stale_helpers.rs — Real L0 stale-check dispatch helpers
//
// Phase 4a: Replaces placeholder dispatch functions in stale_engine.rs with
// real LLM-powered implementations for L0 mutations:
//   - dispatch_file_stale_check: Diff-based stale detection via LLM
//   - dispatch_new_file_ingest: Ingest new files into pyramid
//   - dispatch_tombstone: Tombstone deleted files
//   - dispatch_rename_check: LLM-powered rename detection

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use similar::{ChangeTag, TextDiff};
use tracing::{info, warn};


use super::config_helper::{config_for_model, estimate_cost};
use super::llm::{call_model_with_usage, extract_json};
use super::naming::{headline_from_path, tombstone_headline};
use super::stale_helpers_upper::{
    resolve_evidence_targets_for_node_ids, resolve_live_canonical_node_id,
    resolve_parent_targets_for_node_ids,
};
use super::types::{FileStaleResult, PendingMutation, RenameResult, StaleCheckResult};

// ── Utility Functions ────────────────────────────────────────────────────────

/// Read file content from disk. Returns an error if the file cannot be read.
fn read_file_content(path: &str) -> Result<String> {
    std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read file from disk: {}", path))
}

/// Generate a unified diff between old and new content using the `similar` crate.
fn compute_diff(old: &str, new: &str) -> String {
    let diff = TextDiff::from_lines(old, new);
    let mut output = String::new();

    for change in diff.iter_all_changes() {
        let sign = match change.tag() {
            ChangeTag::Delete => "-",
            ChangeTag::Insert => "+",
            ChangeTag::Equal => " ",
        };
        output.push_str(sign);
        output.push_str(change.value());
        if !change.value().ends_with('\n') {
            output.push('\n');
        }
    }
    output
}

/// Look up a node's distilled field from the pyramid_nodes table.
fn get_node_content(conn: &Connection, slug: &str, node_id: &str) -> Result<String> {
    conn.query_row(
        "SELECT distilled FROM pyramid_nodes WHERE slug = ?1 AND id = ?2",
        rusqlite::params![slug, node_id],
        |row| row.get::<_, String>(0),
    )
    .with_context(|| format!("Failed to get node content for {}:{}", slug, node_id))
}

/// Look up node_ids from pyramid_file_hashes for a given file path.
fn get_file_node_ids(conn: &Connection, slug: &str, file_path: &str) -> Result<Vec<String>> {
    let json_str: String = conn
        .query_row(
            "SELECT node_ids FROM pyramid_file_hashes WHERE slug = ?1 AND file_path = ?2",
            rusqlite::params![slug, file_path],
            |row| row.get::<_, String>(0),
        )
        .with_context(|| format!("Failed to get file node_ids for {}:{}", slug, file_path))?;

    let ids: Vec<String> = serde_json::from_str(&json_str)
        .with_context(|| format!("Failed to parse node_ids JSON: {}", json_str))?;
    let mut live_ids = Vec::new();
    let mut seen = BTreeSet::new();

    for node_id in ids {
        let resolved = resolve_live_canonical_node_id(conn, slug, &node_id)?
            .unwrap_or_else(|| node_id.clone());
        if seen.insert(resolved.clone()) {
            live_ids.push(resolved);
        }
    }

    Ok(live_ids)
}

/// Compute SHA-256 hash of content bytes, matching watcher.rs pattern.
fn compute_hash(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    hex::encode(hasher.finalize())
}

fn enqueue_parent_confirmed_stales(
    conn: &Connection,
    slug: &str,
    source_node_ids: &[String],
    detail: &str,
    now: &str,
) -> Result<usize> {
    // Content-type dispatch: question pyramids use evidence DAG, mechanical use parent_id
    let content_type: Option<String> = conn
        .query_row(
            "SELECT content_type FROM pyramid_slugs WHERE slug = ?1",
            rusqlite::params![slug],
            |row| row.get(0),
        )
        .ok();

    let parent_targets = if content_type.as_deref() == Some("question") {
        resolve_evidence_targets_for_node_ids(conn, slug, source_node_ids)?
    } else {
        resolve_parent_targets_for_node_ids(conn, slug, source_node_ids)?
    };

    for target in &parent_targets {
        conn.execute(
            "INSERT INTO pyramid_pending_mutations
             (slug, layer, mutation_type, target_ref, detail, cascade_depth, detected_at, processed)
             VALUES (?1, 1, 'confirmed_stale', ?2, ?3, 0, ?4, 0)",
            rusqlite::params![slug, target, detail, now],
        )?;
    }

    Ok(parent_targets.len())
}

// ── dispatch_file_stale_check ────────────────────────────────────────────────

/// Check if files are stale based on changes, using LLM evaluation.
///
/// For each file_change mutation in the batch:
/// 1. Read current content from disk
/// 2. Get old content from the L0 node's distilled field
/// 3. Compute a unified diff
/// 4. Build the Template 1 prompt and call the LLM
/// 5. Parse response into StaleCheckResults
pub async fn dispatch_file_stale_check(
    batch: Vec<PendingMutation>,
    db_path: &str,
    api_key: &str,
    model: &str,
) -> Result<Vec<StaleCheckResult>> {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let batch_size = batch.len() as i32;

    if batch.is_empty() {
        return Ok(Vec::new());
    }

    let slug = batch[0].slug.clone();
    let batch_id = batch[0].batch_id.clone().unwrap_or_default();

    info!(
        count = batch.len(),
        targets = ?batch.iter().map(|m| &m.target_ref).collect::<Vec<_>>(),
        "dispatch_file_stale_check: evaluating with LLM"
    );

    // Build prompt sections for each file in the batch
    let db = db_path.to_string();
    let slug_c = slug.clone();
    let batch_c = batch.clone();

    let prompt_sections: Vec<(String, String, String, String)> =
        tokio::task::spawn_blocking(move || -> Result<Vec<(String, String, String, String)>> {
            let conn = super::db::open_pyramid_connection(Path::new(&db))
                .context("Failed to open DB for file stale check")?;
            let mut sections = Vec::new();

            for m in &batch_c {
                let file_path = &m.target_ref;

                // Read current file from disk
                let new_content = match read_file_content(file_path) {
                    Ok(c) => c,
                    Err(e) => {
                        warn!(
                            file = %file_path,
                            error = %e,
                            "Cannot read file from disk, marking as stale by default"
                        );
                        sections.push((
                            file_path.clone(),
                            String::new(),
                            String::new(),
                            String::new(),
                        ));
                        continue;
                    }
                };

                // Look up old content from pyramid node(s)
                let node_ids = match get_file_node_ids(&conn, &slug_c, file_path) {
                    Ok(ids) => ids,
                    Err(e) => {
                        warn!(
                            file = %file_path,
                            error = %e,
                            "Cannot find file in pyramid_file_hashes, marking as stale"
                        );
                        sections.push((
                            file_path.clone(),
                            String::new(),
                            new_content,
                            String::new(),
                        ));
                        continue;
                    }
                };

                // Concatenate distilled content from all chunks
                let mut old_content = String::new();
                for nid in &node_ids {
                    match get_node_content(&conn, &slug_c, nid) {
                        Ok(c) => {
                            if !old_content.is_empty() {
                                old_content.push_str("\n---\n");
                            }
                            old_content.push_str(&c);
                        }
                        Err(e) => {
                            warn!(node_id = %nid, error = %e, "Failed to get node content");
                        }
                    }
                }

                let diff = compute_diff(&old_content, &new_content);
                sections.push((file_path.clone(), old_content, new_content, diff));
            }

            Ok(sections)
        })
        .await??;

    // Check if any sections actually have content to evaluate
    let has_content = prompt_sections
        .iter()
        .any(|(_, old, new, _)| !old.is_empty() || !new.is_empty());

    if !has_content {
        // All files were unreadable or missing from pyramid — mark all stale
        return Ok(batch
            .iter()
            .enumerate()
            .map(|(i, m)| StaleCheckResult {
                id: 0,
                slug: m.slug.clone(),
                batch_id: batch_id.clone(),
                layer: m.layer,
                target_id: m.target_ref.clone(),
                stale: true,
                reason: "File unreadable or missing from pyramid — marked stale by default"
                    .to_string(),
                checker_index: i as i32,
                checker_batch_size: batch_size,
                checked_at: now.clone(),
                cost_tokens: None,
                cost_usd: None,
                cascade_depth: m.cascade_depth,
            })
            .collect());
    }

    // Build the full Template 1 prompt
    let system_prompt = "\
You are evaluating whether source file changes require updating the knowledge \
pyramid above them. For each file below, the OLD content is what the pyramid \
currently reflects. The NEW content is the current file on disk.

\"stale: true\" means: the change alters what the file DOES, HOW it works, or \
what it EXPOSES. A new function, a changed algorithm, a modified API surface, \
a fixed bug that changes behavior.

\"stale: false\" means: the change is cosmetic. Formatting, comments, import \
reordering, variable renaming with no semantic change, version bumps with no \
behavior change.

When in doubt, choose true.

Output JSON only. Array of objects, one per file:
[{\"file_path\": \"...\", \"stale\": true, \"reason\": \"one sentence\"}]";

    let mut user_prompt = String::new();
    for (i, (file_path, old_content, new_content, diff)) in prompt_sections.iter().enumerate() {
        user_prompt.push_str(&format!(
            "---\n\nFILE {} of {}: {}\n\nOLD (pyramid reflects this):\n{}\n\nNEW (current on disk):\n{}\n\nDIFF:\n{}\n",
            i + 1,
            batch_size,
            file_path,
            old_content,
            new_content,
            diff
        ));
    }

    // Call LLM
    let config = config_for_model(api_key, model);
    let (response, usage) =
        call_model_with_usage(&config, system_prompt, &user_prompt, 0.1, 1024).await?;

    let total_tokens = usage.prompt_tokens + usage.completion_tokens;
    let cost_usd = estimate_cost(&usage);

    info!(
        prompt_tokens = usage.prompt_tokens,
        completion_tokens = usage.completion_tokens,
        cost_usd = format!("{:.6}", cost_usd),
        "File stale check LLM call complete"
    );

    // Log cost to pyramid_cost_log
    {
        let db_cost = db_path.to_string();
        let slug_cost = slug.clone();
        let model_cost = model.to_string();
        let pt = usage.prompt_tokens;
        let ct = usage.completion_tokens;
        let cost = cost_usd;
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(conn) = super::db::open_pyramid_connection(Path::new(&db_cost)) {
                let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
                let _ = conn.execute(
                    "INSERT INTO pyramid_cost_log (slug, operation, model, input_tokens, output_tokens, estimated_cost, source, layer, check_type, created_at, chain_id, step_name, tier, latency_ms, generation_id, estimated_cost_usd) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'auto-stale', ?7, ?8, ?9, NULL, NULL, NULL, NULL, NULL, NULL)",
                    rusqlite::params![slug_cost, "stale_check", model_cost, pt, ct, cost, 0, "file_stale", now],
                );
            }
        }).await;
    }

    // Parse response
    let json_value = extract_json(&response)?;
    let file_results: Vec<FileStaleResult> = serde_json::from_value(json_value)
        .context("Failed to parse FileStaleResult array from LLM response")?;

    // Convert FileStaleResults to StaleCheckResults, matching by file_path
    let mut results = Vec::new();
    for (i, m) in batch.iter().enumerate() {
        let file_result = file_results
            .iter()
            .find(|fr| fr.file_path == m.target_ref)
            .or_else(|| file_results.get(i));

        let (stale, reason) = match file_result {
            Some(fr) => (fr.stale, fr.reason.clone()),
            None => (
                true,
                "LLM response did not include this file — marked stale by default".to_string(),
            ),
        };

        results.push(StaleCheckResult {
            id: 0,
            slug: m.slug.clone(),
            batch_id: batch_id.clone(),
            layer: m.layer,
            target_id: m.target_ref.clone(),
            stale,
            reason,
            checker_index: i as i32,
            checker_batch_size: batch_size,
            checked_at: now.clone(),
            cost_tokens: Some(total_tokens),
            cost_usd: Some(cost_usd),
            cascade_depth: m.cascade_depth,
        });
    }

    Ok(results)
}

// ── dispatch_new_file_ingest ─────────────────────────────────────────────────

/// Ingest new files into the pyramid.
///
/// For each new_file mutation:
/// 1. Read file content from disk
/// 2. Create L0 chunk(s) in pyramid_chunks
/// 3. Update pyramid_file_hashes with the new file's hash and node_ids
/// 4. Write confirmed_stale mutations to WAL for the L1 layer
///
/// NOTE: No LLM stale-check needed — new files are always "stale" by definition.
pub async fn dispatch_new_file_ingest(batch: Vec<PendingMutation>, db_path: &str) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }

    let slug = batch[0].slug.clone();

    info!(
        count = batch.len(),
        targets = ?batch.iter().map(|m| &m.target_ref).collect::<Vec<_>>(),
        "dispatch_new_file_ingest: ingesting new files"
    );

    let db = db_path.to_string();
    let slug_c = slug.clone();

    tokio::task::spawn_blocking(move || -> Result<()> {
        let conn = super::db::open_pyramid_connection(Path::new(&db))
            .context("Failed to open DB for new file ingest")?;
        let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

        for m in &batch {
            let file_path = &m.target_ref;

            // Read file content
            let content = match read_file_content(file_path) {
                Ok(c) => c,
                Err(e) => {
                    warn!(file = %file_path, error = %e, "Cannot read new file, skipping");
                    continue;
                }
            };

            // Compute hash
            let hash = compute_hash(content.as_bytes());

            // Create a new L0 node for this file (sequential ID, not UUID)
            let node_id = super::db::next_sequential_node_id(&conn, &slug_c, 0, "");

            // Truncate content for distilled field (first ~200 lines as summary placeholder)
            let distilled_lines: Vec<&str> = content.lines().take(200).collect();
            let distilled = format!(
                "File: {}\n\n{}",
                file_path,
                distilled_lines.join("\n")
            );
            let headline = headline_from_path(file_path).unwrap_or_else(|| "New File".to_string());

            // Insert node into pyramid_nodes
            conn.execute(
                "INSERT OR REPLACE INTO pyramid_nodes
                 (id, slug, depth, chunk_index, headline, distilled, topics, corrections, decisions,
                  terms, dead_ends, self_prompt, children, parent_id, build_version, created_at)
                 VALUES (?1, ?2, 0, NULL, ?3, ?4, '[]', '[]', '[]', '[]', '[]', '', '[]', NULL, 1, ?5)",
                rusqlite::params![node_id, slug_c, headline, distilled, now],
            )
            .with_context(|| format!("Failed to insert L0 node for {}", file_path))?;

            // Update pyramid_file_hashes
            let node_ids_json = serde_json::to_string(&vec![&node_id])?;
            conn.execute(
                "INSERT OR REPLACE INTO pyramid_file_hashes
                 (slug, file_path, hash, chunk_count, node_ids, last_ingested_at)
                 VALUES (?1, ?2, ?3, 1, ?4, ?5)",
                rusqlite::params![slug_c, file_path, hash, node_ids_json, now],
            )
            .with_context(|| format!("Failed to update file_hashes for {}", file_path))?;

            let parent_count = enqueue_parent_confirmed_stales(
                &conn,
                &slug_c,
                std::slice::from_ref(&node_id),
                "New file ingested",
                &now,
            )
            .with_context(|| {
                format!("Failed to write L1 confirmed_stale for {}", file_path)
            })?;

            if parent_count == 0 {
                info!(file = %file_path, node_id = %node_id, "New file ingested without an existing parent thread target");
            }

            info!(file = %file_path, node_id = %node_id, "New file ingested into pyramid");
        }

        Ok(())
    })
    .await??;

    Ok(())
}

// ── dispatch_tombstone ───────────────────────────────────────────────────────

/// Tombstone deleted files in the pyramid.
///
/// For each deleted file:
/// 1. Look up L0 node(s) via pyramid_file_hashes.node_ids
/// 2. Create a tombstone node with a deletion note
/// 3. Set superseded_by on old node(s) pointing to tombstone
/// 4. Re-parent children to tombstone
/// 5. Remove from pyramid_file_hashes
/// 6. Write confirmed_stale mutations to WAL for parent layer
pub async fn dispatch_tombstone(batch: Vec<PendingMutation>, db_path: &str) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }

    let slug = batch[0].slug.clone();

    info!(
        count = batch.len(),
        targets = ?batch.iter().map(|m| &m.target_ref).collect::<Vec<_>>(),
        "dispatch_tombstone: tombstoning deleted files"
    );

    let db = db_path.to_string();
    let slug_c = slug.clone();

    tokio::task::spawn_blocking(move || -> Result<()> {
        let conn = super::db::open_pyramid_connection(Path::new(&db))
            .context("Failed to open DB for tombstoning")?;
        let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

        for m in &batch {
            let file_path = &m.target_ref;

            // Look up existing node(s)
            let node_ids = match get_file_node_ids(&conn, &slug_c, file_path) {
                Ok(ids) => ids,
                Err(e) => {
                    warn!(
                        file = %file_path,
                        error = %e,
                        "Cannot find deleted file in pyramid_file_hashes, skipping tombstone"
                    );
                    continue;
                }
            };

            if node_ids.is_empty() {
                warn!(file = %file_path, "No node_ids found for deleted file, skipping");
                continue;
            }

            // Get the first node's distilled content for the tombstone note
            let old_distilled = node_ids
                .first()
                .and_then(|nid| get_node_content(&conn, &slug_c, nid).ok())
                .unwrap_or_default();

            // Truncate old distilled to one line for the tombstone
            let first_line = old_distilled.lines().next().unwrap_or("(empty)");
            let tombstone_content = format!(
                "File deleted: {}. Previously contained: {}",
                file_path, first_line
            );
            let tombstone_headline = tombstone_headline(file_path);

            // Create tombstone node
            let tombstone_id = super::db::next_sequential_node_id(&conn, &slug_c, 0, "TOMB");

            conn.execute(
                "INSERT INTO pyramid_nodes
                 (id, slug, depth, chunk_index, headline, distilled, topics, corrections, decisions,
                  terms, dead_ends, self_prompt, children, parent_id, build_version, created_at)
                 VALUES (?1, ?2, 0, NULL, ?3, ?4, '[]', '[]', '[]', '[]', '[]', '', '[]', NULL, 1, ?5)",
                rusqlite::params![tombstone_id, slug_c, tombstone_headline, tombstone_content, now],
            )
            .with_context(|| format!("Failed to create tombstone node for {}", file_path))?;

            // Supersede all old nodes and re-parent their children to tombstone
            for old_id in &node_ids {
                // Set superseded_by on old node
                conn.execute(
                    "UPDATE pyramid_nodes SET superseded_by = ?1
                     WHERE slug = ?2 AND id = ?3 AND superseded_by IS NULL",
                    rusqlite::params![tombstone_id, slug_c, old_id],
                )?;

                // Re-parent children: any node whose parent_id was the old node
                // now points to the tombstone (deterministic re-parenting)
                conn.execute(
                    "UPDATE pyramid_nodes SET parent_id = ?1
                     WHERE slug = ?2 AND parent_id = ?3",
                    rusqlite::params![tombstone_id, slug_c, old_id],
                )?;

                info!(
                    old_node = %old_id,
                    tombstone = %tombstone_id,
                    "Superseded old node with tombstone"
                );
            }

            // Remove from pyramid_file_hashes
            conn.execute(
                "DELETE FROM pyramid_file_hashes WHERE slug = ?1 AND file_path = ?2",
                rusqlite::params![slug_c, file_path],
            )?;

            let parent_count = enqueue_parent_confirmed_stales(
                &conn,
                &slug_c,
                &node_ids,
                &tombstone_content,
                &now,
            )?;

            if parent_count == 0 {
                info!(file = %file_path, tombstone = %tombstone_id, "Tombstone created without an existing parent thread target");
            }

            info!(file = %file_path, tombstone = %tombstone_id, "File tombstoned");
        }

        Ok(())
    })
    .await??;

    Ok(())
}

// ── dispatch_rename_check ────────────────────────────────────────────────────

/// Check if a file rename occurred using LLM evaluation.
///
/// 1. Parse detail field as JSON: {"old_path": "...", "new_path": "..."}
/// 2. Look up old node's distilled field
/// 3. Read new file's first 200 lines from disk
/// 4. Build Template 4 prompt and call LLM
/// 5. If rename=true: update thread key, update file_hashes, supersede old L0
/// 6. If rename=false: create tombstone for old + fresh ingest for new
pub async fn dispatch_rename_check(
    mutation: PendingMutation,
    db_path: &str,
    api_key: &str,
    model: &str,
) -> Result<RenameResult> {
    info!(
        target = %mutation.target_ref,
        detail = ?mutation.detail,
        "dispatch_rename_check: evaluating with LLM"
    );

    let slug = mutation.slug.clone();
    let detail = mutation.detail.as_deref().unwrap_or("{}");

    // Parse detail JSON to get old_path and new_path
    let detail_json: serde_json::Value =
        serde_json::from_str(detail).context("Failed to parse rename detail JSON")?;

    let old_path = detail_json
        .get("old_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing old_path in rename detail"))?
        .to_string();

    let new_path = detail_json
        .get("new_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing new_path in rename detail"))?
        .to_string();

    // Gather context from DB and disk
    let db = db_path.to_string();
    let slug_c = slug.clone();
    let old_path_c = old_path.clone();
    let new_path_c = new_path.clone();

    let (old_distilled, new_content_head) =
        tokio::task::spawn_blocking(move || -> Result<(String, String)> {
            let conn = super::db::open_pyramid_connection(Path::new(&db))
                .context("Failed to open DB for rename check")?;

            // Look up old node content
            let node_ids = get_file_node_ids(&conn, &slug_c, &old_path_c).unwrap_or_default();

            let old_distilled = node_ids
                .first()
                .and_then(|nid| get_node_content(&conn, &slug_c, nid).ok())
                .unwrap_or_else(|| format!("(no pyramid content found for {})", old_path_c));

            // Read new file (first 200 lines)
            let new_content = read_file_content(&new_path_c).unwrap_or_default();
            let head_lines: Vec<&str> = new_content.lines().take(200).collect();
            let new_content_head = head_lines.join("\n");

            Ok((old_distilled, new_content_head))
        })
        .await??;

    // Build Template 4 prompt
    let system_prompt = "\
A file disappeared and a new file appeared in the same time window. You are \
determining whether the new file is a continuation of the old file (rename/move) \
or a genuinely different file.

\"rename: true\" means: the new file is clearly the same logical unit as the \
old file, moved or renamed. The content, purpose, and structure are \
recognizably the same even if some code changed in the process.

\"rename: false\" means: these are genuinely different files that happen to \
have appeared and disappeared in the same window.

When in doubt, choose false. A false positive merges unrelated thread histories. \
A false negative just creates a tombstone and a fresh ingest, which is safe.

Output JSON only:
{\"rename\": true, \"reason\": \"one sentence\"}";

    let user_prompt = format!(
        "DISAPPEARED:\nPath: {}\nContent summary: {}\n\nAPPEARED:\nPath: {}\nContent (first 200 lines):\n{}",
        old_path, old_distilled, new_path, new_content_head
    );

    // Call LLM
    let config = config_for_model(api_key, model);
    let (response, usage) =
        call_model_with_usage(&config, system_prompt, &user_prompt, 0.1, 256).await?;

    let cost_usd = estimate_cost(&usage);
    info!(
        prompt_tokens = usage.prompt_tokens,
        completion_tokens = usage.completion_tokens,
        cost_usd = format!("{:.6}", cost_usd),
        "Rename check LLM call complete"
    );

    // Log cost to pyramid_cost_log
    {
        let db_cost = db_path.to_string();
        let slug_cost = slug.clone();
        let model_cost = model.to_string();
        let pt = usage.prompt_tokens;
        let ct = usage.completion_tokens;
        let cost = cost_usd;
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(conn) = super::db::open_pyramid_connection(Path::new(&db_cost)) {
                let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
                let _ = conn.execute(
                    "INSERT INTO pyramid_cost_log (slug, operation, model, input_tokens, output_tokens, estimated_cost, source, layer, check_type, created_at, chain_id, step_name, tier, latency_ms, generation_id, estimated_cost_usd) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'auto-stale', ?7, ?8, ?9, NULL, NULL, NULL, NULL, NULL, NULL)",
                    rusqlite::params![slug_cost, "stale_check", model_cost, pt, ct, cost, 0, "rename_check", now],
                );
            }
        }).await;
    }

    // Parse response
    let json_value = extract_json(&response)?;
    let result: RenameResult = serde_json::from_value(json_value)
        .context("Failed to parse RenameResult from LLM response")?;

    info!(
        old_path = %old_path,
        new_path = %new_path,
        rename = result.rename,
        reason = %result.reason,
        "Rename check result"
    );

    // Post-processing
    let db = db_path.to_string();
    let slug_c = slug.clone();
    let result_c = result.clone();
    let old_path_c = old_path.clone();
    let new_path_c = new_path.clone();

    tokio::task::spawn_blocking(move || -> Result<()> {
        let conn = super::db::open_pyramid_connection(Path::new(&db))
            .context("Failed to open DB for rename post-processing")?;
        let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

        if result_c.rename {
            // Confirmed rename: update thread key, update file_hashes, supersede old
            let old_node_ids = get_file_node_ids(&conn, &slug_c, &old_path_c)
                .unwrap_or_default();

            if let Some(old_node_id) = old_node_ids.first() {
                // Create a rename note node (sequential ID, not UUID)
                let new_node_id = super::db::next_sequential_node_id(&conn, &slug_c, 0, "");
                let rename_note = format!(
                    "File renamed: {} -> {}. Reason: {}",
                    old_path_c, new_path_c, result_c.reason
                );

                // Read new file content for the updated node
                let new_content = read_file_content(&new_path_c).unwrap_or_default();
                let distilled_lines: Vec<&str> = new_content.lines().take(200).collect();
                let distilled = format!(
                    "File: {} (renamed from {})\n\n{}",
                    new_path_c, old_path_c, distilled_lines.join("\n")
                );

                // Insert new L0 node
                let headline = headline_from_path(&new_path_c).unwrap_or_else(|| "Renamed File".to_string());
                conn.execute(
                    "INSERT OR REPLACE INTO pyramid_nodes
                     (id, slug, depth, chunk_index, headline, distilled, topics, corrections, decisions,
                      terms, dead_ends, self_prompt, children, parent_id, build_version, created_at)
                     VALUES (?1, ?2, 0, NULL, ?3, ?4, '[]', '[]', '[]', '[]', '[]', '', '[]', NULL, 1, ?5)",
                    rusqlite::params![new_node_id, slug_c, headline, distilled, now],
                )?;

                // Supersede all old nodes
                for oid in &old_node_ids {
                    conn.execute(
                        "UPDATE pyramid_nodes SET superseded_by = ?1
                         WHERE slug = ?2 AND id = ?3 AND superseded_by IS NULL",
                        rusqlite::params![new_node_id, slug_c, oid],
                    )?;

                    // Re-parent children
                    conn.execute(
                        "UPDATE pyramid_nodes SET parent_id = ?1
                         WHERE slug = ?2 AND parent_id = ?3",
                        rusqlite::params![new_node_id, slug_c, oid],
                    )?;
                }

                // Update pyramid_file_hashes: remove old path, add new path
                conn.execute(
                    "DELETE FROM pyramid_file_hashes WHERE slug = ?1 AND file_path = ?2",
                    rusqlite::params![slug_c, old_path_c],
                )?;

                let hash = compute_hash(new_content.as_bytes());
                let node_ids_json = serde_json::to_string(&vec![&new_node_id])?;
                conn.execute(
                    "INSERT OR REPLACE INTO pyramid_file_hashes
                     (slug, file_path, hash, chunk_count, node_ids, last_ingested_at)
                     VALUES (?1, ?2, ?3, 1, ?4, ?5)",
                    rusqlite::params![slug_c, new_path_c, hash, node_ids_json, now],
                )?;

                // Update thread key if a thread references the old path
                conn.execute(
                    "UPDATE pyramid_threads SET thread_name = ?1, current_canonical_id = ?2, updated_at = ?3
                     WHERE slug = ?4 AND current_canonical_id = ?5",
                    rusqlite::params![new_path_c, new_node_id, now, slug_c, old_node_id],
                )?;

                let parent_count = enqueue_parent_confirmed_stales(
                    &conn,
                    &slug_c,
                    &old_node_ids,
                    &rename_note,
                    &now,
                )?;

                if parent_count == 0 {
                    info!(old_path = %old_path_c, new_path = %new_path_c, "Rename completed without an existing parent thread target");
                }

                info!(
                    old_path = %old_path_c,
                    new_path = %new_path_c,
                    old_node = %old_node_id,
                    new_node = %new_node_id,
                    "Rename processed: updated thread and file_hashes"
                );
            }
        } else {
            // Not a rename: tombstone old + fresh ingest for new
            info!(
                old_path = %old_path_c,
                new_path = %new_path_c,
                "Not a rename: will tombstone old and ingest new"
            );

            // Tombstone the old file
            let old_node_ids = get_file_node_ids(&conn, &slug_c, &old_path_c)
                .unwrap_or_default();

            if !old_node_ids.is_empty() {
                let old_distilled = old_node_ids
                    .first()
                    .and_then(|nid| get_node_content(&conn, &slug_c, nid).ok())
                    .unwrap_or_default();
                let first_line = old_distilled.lines().next().unwrap_or("(empty)");
                let tombstone_content = format!(
                    "File deleted: {}. Previously contained: {}",
                    old_path_c, first_line
                );
                let tombstone_headline = tombstone_headline(&old_path_c);

                let tombstone_id = super::db::next_sequential_node_id(&conn, &slug_c, 0, "TOMB");

                conn.execute(
                    "INSERT INTO pyramid_nodes
                     (id, slug, depth, chunk_index, headline, distilled, topics, corrections, decisions,
                      terms, dead_ends, self_prompt, children, parent_id, build_version, created_at)
                     VALUES (?1, ?2, 0, NULL, ?3, ?4, '[]', '[]', '[]', '[]', '[]', '', '[]', NULL, 1, ?5)",
                    rusqlite::params![tombstone_id, slug_c, tombstone_headline, tombstone_content, now],
                )?;

                for oid in &old_node_ids {
                    conn.execute(
                        "UPDATE pyramid_nodes SET superseded_by = ?1
                         WHERE slug = ?2 AND id = ?3 AND superseded_by IS NULL",
                        rusqlite::params![tombstone_id, slug_c, oid],
                    )?;
                    conn.execute(
                        "UPDATE pyramid_nodes SET parent_id = ?1
                         WHERE slug = ?2 AND parent_id = ?3",
                        rusqlite::params![tombstone_id, slug_c, oid],
                    )?;
                }

                conn.execute(
                    "DELETE FROM pyramid_file_hashes WHERE slug = ?1 AND file_path = ?2",
                    rusqlite::params![slug_c, old_path_c],
                )?;

                let parent_count = enqueue_parent_confirmed_stales(
                    &conn,
                    &slug_c,
                    &old_node_ids,
                    &tombstone_content,
                    &now,
                )?;

                if parent_count == 0 {
                    info!(old_path = %old_path_c, tombstone = %tombstone_id, "Rename-false tombstone created without an existing parent thread target");
                }
            }

            // Ingest the new file
            let new_content = read_file_content(&new_path_c).unwrap_or_default();
            if !new_content.is_empty() {
                let hash = compute_hash(new_content.as_bytes());
                let new_node_id = super::db::next_sequential_node_id(&conn, &slug_c, 0, "");

                let distilled_lines: Vec<&str> = new_content.lines().take(200).collect();
                let distilled = format!(
                    "File: {}\n\n{}",
                    new_path_c,
                    distilled_lines.join("\n")
                );

                let headline = headline_from_path(&new_path_c).unwrap_or_else(|| "New File".to_string());
                conn.execute(
                    "INSERT OR REPLACE INTO pyramid_nodes
                     (id, slug, depth, chunk_index, headline, distilled, topics, corrections, decisions,
                      terms, dead_ends, self_prompt, children, parent_id, build_version, created_at)
                     VALUES (?1, ?2, 0, NULL, ?3, ?4, '[]', '[]', '[]', '[]', '[]', '', '[]', NULL, 1, ?5)",
                    rusqlite::params![new_node_id, slug_c, headline, distilled, now],
                )?;

                let node_ids_json = serde_json::to_string(&vec![&new_node_id])?;
                conn.execute(
                    "INSERT OR REPLACE INTO pyramid_file_hashes
                     (slug, file_path, hash, chunk_count, node_ids, last_ingested_at)
                     VALUES (?1, ?2, ?3, 1, ?4, ?5)",
                    rusqlite::params![slug_c, new_path_c, hash, node_ids_json, now],
                )?;

                let parent_count = enqueue_parent_confirmed_stales(
                    &conn,
                    &slug_c,
                    std::slice::from_ref(&new_node_id),
                    "New file ingested (from rename-false)",
                    &now,
                )?;

                if parent_count == 0 {
                    info!(new_path = %new_path_c, node_id = %new_node_id, "Rename-false new file ingested without an existing parent thread target");
                }
            }
        }

        Ok(())
    })
    .await??;

    Ok(result)
}

// ── dispatch_evidence_set_apex_synthesis ─────────────────────────────────────

/// Synthesize an apex node for evidence sets that have grown beyond a single member.
///
/// For each evidence_set_growth mutation in the batch:
/// 1. Read target_ref (the self_prompt / question text of the evidence set)
/// 2. Load member node IDs via db::get_evidence_set_member_ids()
/// 3. If member_count <= 1: return not-stale (no apex needed yet)
/// 4. If member_count > 1:
///    a. Load all member nodes (headline + distilled)
///    b. Build a context string and call the LLM to synthesize a set apex
///    c. Create/update an ES-{uuid} node with the synthesis
///    d. Return stale result to trigger propagation
pub async fn dispatch_evidence_set_apex_synthesis(
    batch: Vec<PendingMutation>,
    db_path: &str,
    api_key: &str,
    model: &str,
) -> Result<Vec<StaleCheckResult>> {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let batch_size = batch.len() as i32;

    if batch.is_empty() {
        return Ok(Vec::new());
    }

    let slug = batch[0].slug.clone();
    let batch_id = batch[0].batch_id.clone().unwrap_or_default();

    info!(
        count = batch.len(),
        targets = ?batch.iter().map(|m| &m.target_ref).collect::<Vec<_>>(),
        "dispatch_evidence_set_apex_synthesis: evaluating evidence sets"
    );

    let mut results = Vec::new();

    for (i, m) in batch.iter().enumerate() {
        let self_prompt = m.target_ref.clone();

        // Load member IDs from DB
        let db = db_path.to_string();
        let s = slug.clone();
        let sp = self_prompt.clone();
        let members: Vec<(String, String, String)> =
            tokio::task::spawn_blocking(move || -> Result<Vec<(String, String, String)>> {
                let conn = super::db::open_pyramid_connection(Path::new(&db))
                    .context("Failed to open DB for evidence set apex synthesis")?;

                let member_ids = super::db::get_evidence_set_member_ids(&conn, &s, &sp)?;

                if member_ids.len() <= 1 {
                    return Ok(Vec::new());
                }

                // Load headline + distilled for each member
                let mut member_data = Vec::new();
                for mid in &member_ids {
                    let headline: String = conn
                        .query_row(
                            "SELECT COALESCE(headline, '') FROM pyramid_nodes WHERE slug = ?1 AND id = ?2",
                            rusqlite::params![s, mid],
                            |row| row.get(0),
                        )
                        .unwrap_or_default();

                    let distilled: String = conn
                        .query_row(
                            "SELECT COALESCE(distilled, '') FROM pyramid_nodes WHERE slug = ?1 AND id = ?2",
                            rusqlite::params![s, mid],
                            |row| row.get(0),
                        )
                        .unwrap_or_default();

                    member_data.push((mid.clone(), headline, distilled));
                }

                Ok(member_data)
            })
            .await??;

        // If <= 1 member, no apex needed
        if members.is_empty() {
            results.push(StaleCheckResult {
                id: 0,
                slug: slug.clone(),
                batch_id: batch_id.clone(),
                layer: m.layer,
                target_id: self_prompt.clone(),
                stale: false,
                reason: "Evidence set has <= 1 member, no apex synthesis needed".to_string(),
                checker_index: i as i32,
                checker_batch_size: batch_size,
                checked_at: now.clone(),
                cost_tokens: None,
                cost_usd: None,
                cascade_depth: m.cascade_depth,
            });
            continue;
        }

        // Build context from member headlines + distilled text
        let mut context = String::new();
        for (j, (mid, headline, distilled)) in members.iter().enumerate() {
            context.push_str(&format!(
                "--- Evidence {} of {} (node {}) ---\nHeadline: {}\n{}\n\n",
                j + 1,
                members.len(),
                mid,
                headline,
                distilled
            ));
        }

        // Build LLM prompt for apex synthesis
        let system_prompt = "\
You are synthesizing a set of evidence nodes that all answer the same question. \
Each evidence node represents a different source file's contribution to the answer.

Your task: produce a unified headline and a richer distilled summary that \
synthesizes all evidence into a single coherent answer.

Output JSON only:
{\"headline\": \"one sentence synthesis\", \"distilled\": \"multi-paragraph synthesis\"}";

        let user_prompt = format!(
            "Question: {}\n\nEvidence nodes ({} total):\n\n{}",
            self_prompt,
            members.len(),
            context
        );

        // Call LLM
        let config = config_for_model(api_key, model);
        let (response, usage) =
            call_model_with_usage(&config, system_prompt, &user_prompt, 0.2, 1024).await?;

        let cost_usd = estimate_cost(&usage);
        let total_tokens = usage.prompt_tokens + usage.completion_tokens;

        info!(
            prompt_tokens = usage.prompt_tokens,
            completion_tokens = usage.completion_tokens,
            cost_usd = format!("{:.6}", cost_usd),
            self_prompt = %self_prompt,
            "Evidence set apex synthesis LLM call complete"
        );

        // Log cost
        {
            let db_cost = db_path.to_string();
            let slug_cost = slug.clone();
            let model_cost = model.to_string();
            let pt = usage.prompt_tokens;
            let ct = usage.completion_tokens;
            let cost = cost_usd;
            let _ = tokio::task::spawn_blocking(move || {
                if let Ok(conn) = super::db::open_pyramid_connection(Path::new(&db_cost)) {
                    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
                    let _ = conn.execute(
                        "INSERT INTO pyramid_cost_log (slug, operation, model, input_tokens, output_tokens, estimated_cost, source, layer, check_type, created_at, chain_id, step_name, tier, latency_ms, generation_id, estimated_cost_usd) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'auto-stale', ?7, ?8, ?9, NULL, NULL, NULL, NULL, NULL, NULL)",
                        rusqlite::params![slug_cost, "stale_check", model_cost, pt, ct, cost, 0, "evidence_set_apex", now],
                    );
                }
            }).await;
        }

        // Parse response
        let json_value = extract_json(&response)?;
        let headline: String = json_value
            .get("headline")
            .and_then(|v| v.as_str())
            .unwrap_or("Evidence set synthesis")
            .to_string();
        let distilled: String = json_value
            .get("distilled")
            .and_then(|v| v.as_str())
            .unwrap_or(&headline)
            .to_string();

        // Create/update the ES- apex node in the DB
        let db = db_path.to_string();
        let s = slug.clone();
        let sp = self_prompt.clone();
        let hl = headline.clone();
        let dist = distilled.clone();
        let now_c = now.clone();
        let apex_node_id = tokio::task::spawn_blocking(move || -> Result<String> {
            let conn = super::db::open_pyramid_connection(Path::new(&db))
                .context("Failed to open DB for ES apex upsert")?;

            // Check if an existing ES- apex node exists for this self_prompt
            let existing_id: Option<String> = conn
                .query_row(
                    "SELECT id FROM pyramid_nodes
                     WHERE slug = ?1 AND depth = 0 AND id LIKE 'ES-%'
                       AND self_prompt = ?2 AND superseded_by IS NULL",
                    rusqlite::params![s, sp],
                    |row| row.get(0),
                )
                .ok();

            let node_id = existing_id.unwrap_or_else(|| super::db::next_sequential_node_id(&conn, &s, 0, "ES"));

            conn.execute(
                "INSERT OR REPLACE INTO pyramid_nodes
                 (id, slug, depth, chunk_index, headline, distilled, topics, corrections, decisions,
                  terms, dead_ends, self_prompt, children, parent_id, build_version, created_at)
                 VALUES (?1, ?2, 0, NULL, ?3, ?4, '[]', '[]', '[]', '[]', '[]', ?5, '[]', NULL, 1, ?6)",
                rusqlite::params![node_id, s, hl, dist, sp, now_c],
            )
            .context("Failed to upsert ES apex node")?;

            Ok(node_id)
        })
        .await??;

        info!(
            slug = %slug,
            self_prompt = %self_prompt,
            apex_node_id = %apex_node_id,
            member_count = members.len(),
            "Evidence set apex node synthesized"
        );

        results.push(StaleCheckResult {
            id: 0,
            slug: slug.clone(),
            batch_id: batch_id.clone(),
            layer: m.layer,
            target_id: apex_node_id,
            stale: true,
            reason: format!(
                "Evidence set apex synthesized from {} members for question: {}",
                members.len(),
                self_prompt
            ),
            checker_index: i as i32,
            checker_batch_size: batch_size,
            checked_at: now.clone(),
            cost_tokens: Some(total_tokens),
            cost_usd: Some(cost_usd),
            cascade_depth: m.cascade_depth,
        });
    }

    Ok(results)
}

// ── dispatch_targeted_l0_stale_check ────────────────────────────────────────

/// Check if targeted L0 nodes are still valid given their source file's current content.
///
/// For each targeted_l0_stale mutation in the batch:
/// 1. Load the targeted L0 node by ID (self_prompt + distilled)
/// 2. Find its source file path from pyramid_file_hashes (search node_ids for the node ID)
/// 3. Read current file content from disk
/// 4. Call LLM: "Given the question, is this extraction still valid?"
/// 5. Parse response: { "still_valid": true/false, "reason": "..." }
/// 6. Return StaleCheckResult with stale = !still_valid
pub async fn dispatch_targeted_l0_stale_check(
    batch: Vec<PendingMutation>,
    db_path: &str,
    api_key: &str,
    model: &str,
) -> Result<Vec<StaleCheckResult>> {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let batch_size = batch.len() as i32;

    if batch.is_empty() {
        return Ok(Vec::new());
    }

    let slug = batch[0].slug.clone();
    let batch_id = batch[0].batch_id.clone().unwrap_or_default();

    info!(
        count = batch.len(),
        targets = ?batch.iter().map(|m| &m.target_ref).collect::<Vec<_>>(),
        "dispatch_targeted_l0_stale_check: evaluating targeted L0 nodes"
    );

    let mut results = Vec::new();

    for (i, m) in batch.iter().enumerate() {
        let node_id = m.target_ref.clone();

        // Load node data and source file content from DB + disk
        let db = db_path.to_string();
        let s = slug.clone();
        let nid = node_id.clone();

        let node_data: Option<(String, String, String)> =
            tokio::task::spawn_blocking(move || -> Result<Option<(String, String, String)>> {
                let conn = super::db::open_pyramid_connection(Path::new(&db))
                    .context("Failed to open DB for targeted L0 stale check")?;

                // Load self_prompt and distilled from the targeted node
                let node_row: Option<(String, String)> = conn
                    .query_row(
                        "SELECT COALESCE(self_prompt, ''), COALESCE(distilled, '')
                         FROM pyramid_nodes
                         WHERE slug = ?1 AND id = ?2 AND superseded_by IS NULL",
                        rusqlite::params![s, nid],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                    )
                    .ok();

                let (self_prompt, distilled) = match node_row {
                    Some(r) => r,
                    None => return Ok(None),
                };

                if self_prompt.is_empty() {
                    // Not actually a targeted node — skip
                    return Ok(None);
                }

                // Find the source file path by searching pyramid_file_hashes for this node ID
                let file_path: Option<String> = conn
                    .query_row(
                        "SELECT file_path FROM pyramid_file_hashes
                         WHERE slug = ?1 AND EXISTS (SELECT 1 FROM json_each(node_ids) WHERE value = ?2)",
                        rusqlite::params![s, nid],
                        |row| row.get(0),
                    )
                    .ok();

                let file_path = match file_path {
                    Some(fp) => fp,
                    None => return Ok(None),
                };

                // Read current file content from disk
                let file_content = read_file_content(&file_path).unwrap_or_default();
                if file_content.is_empty() {
                    return Ok(None);
                }

                Ok(Some((self_prompt, distilled, file_content)))
            })
            .await??;

        let (self_prompt, distilled, file_content) = match node_data {
            Some(data) => data,
            None => {
                // Node not found, superseded, or file missing — mark stale by default
                results.push(StaleCheckResult {
                    id: 0,
                    slug: slug.clone(),
                    batch_id: batch_id.clone(),
                    layer: m.layer,
                    target_id: node_id.clone(),
                    stale: true,
                    reason: "Targeted L0 node not found, superseded, or source file missing"
                        .to_string(),
                    checker_index: i as i32,
                    checker_batch_size: batch_size,
                    checked_at: now.clone(),
                    cost_tokens: None,
                    cost_usd: None,
                    cascade_depth: m.cascade_depth,
                });
                continue;
            }
        };

        // Build LLM prompt for targeted stale check
        let system_prompt = "\
You are evaluating whether a targeted extraction from a source file is still valid. \
The extraction was made to answer a specific question. The source file has changed.

\"still_valid: true\" means: the extraction still correctly answers the question \
given the current file content. The relevant information is unchanged or the \
changes do not affect the answer.

\"still_valid: false\" means: the file changes have invalidated this extraction. \
The answer to the question has changed, the relevant section was removed, or \
the information is no longer accurate.

When in doubt, choose false.

Output JSON only:
{\"still_valid\": true, \"reason\": \"one sentence\"}";

        let user_prompt = format!(
            "QUESTION:\n{}\n\nCURRENT EXTRACTION:\n{}\n\nCURRENT FILE CONTENT:\n{}",
            self_prompt, distilled, file_content
        );

        // Call LLM (low temperature for factual evaluation)
        let config = config_for_model(api_key, model);
        let (response, usage) =
            call_model_with_usage(&config, system_prompt, &user_prompt, 0.1, 256).await?;

        let cost_usd = estimate_cost(&usage);
        let total_tokens = usage.prompt_tokens + usage.completion_tokens;

        info!(
            prompt_tokens = usage.prompt_tokens,
            completion_tokens = usage.completion_tokens,
            cost_usd = format!("{:.6}", cost_usd),
            node_id = %node_id,
            "Targeted L0 stale check LLM call complete"
        );

        // Log cost to pyramid_cost_log
        {
            let db_cost = db_path.to_string();
            let slug_cost = slug.clone();
            let model_cost = model.to_string();
            let pt = usage.prompt_tokens;
            let ct = usage.completion_tokens;
            let cost = cost_usd;
            let _ = tokio::task::spawn_blocking(move || {
                if let Ok(conn) = super::db::open_pyramid_connection(Path::new(&db_cost)) {
                    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
                    let _ = conn.execute(
                        "INSERT INTO pyramid_cost_log (slug, operation, model, input_tokens, output_tokens, estimated_cost, source, layer, check_type, created_at, chain_id, step_name, tier, latency_ms, generation_id, estimated_cost_usd) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'auto-stale', ?7, ?8, ?9, NULL, NULL, NULL, NULL, NULL, NULL)",
                        rusqlite::params![slug_cost, "stale_check", model_cost, pt, ct, cost, 0, "targeted_l0_stale", now],
                    );
                }
            })
            .await;
        }

        // Parse response
        let json_value = extract_json(&response)?;
        let still_valid = json_value
            .get("still_valid")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let reason = json_value
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("No reason provided")
            .to_string();

        info!(
            node_id = %node_id,
            still_valid,
            reason = %reason,
            "Targeted L0 stale check result"
        );

        results.push(StaleCheckResult {
            id: 0,
            slug: slug.clone(),
            batch_id: batch_id.clone(),
            layer: m.layer,
            target_id: node_id.clone(),
            stale: !still_valid,
            reason,
            checker_index: i as i32,
            checker_batch_size: batch_size,
            checked_at: now.clone(),
            cost_tokens: Some(total_tokens),
            cost_usd: Some(cost_usd),
            cascade_depth: m.cascade_depth,
        });
    }

    Ok(results)
}
