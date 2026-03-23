// pyramid/stale_helpers.rs — Real L0 stale-check dispatch helpers
//
// Phase 4a: Replaces placeholder dispatch functions in stale_engine.rs with
// real LLM-powered implementations for L0 mutations:
//   - dispatch_file_stale_check: Diff-based stale detection via LLM
//   - dispatch_new_file_ingest: Ingest new files into pyramid
//   - dispatch_tombstone: Tombstone deleted files
//   - dispatch_rename_check: LLM-powered rename detection

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use similar::{ChangeTag, TextDiff};
use tracing::{info, warn};
use uuid::Uuid;

use super::config_helper::{config_for_model, estimate_cost};
use super::llm::{call_model_with_usage, extract_json};
use super::types::{
    FileStaleResult, PendingMutation, RenameResult, StaleCheckResult,
};

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
        .with_context(|| {
            format!(
                "Failed to get file node_ids for {}:{}",
                slug, file_path
            )
        })?;

    let ids: Vec<String> = serde_json::from_str(&json_str)
        .with_context(|| format!("Failed to parse node_ids JSON: {}", json_str))?;
    Ok(ids)
}

/// Compute SHA-256 hash of content bytes, matching watcher.rs pattern.
fn compute_hash(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    hex::encode(hasher.finalize())
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

    let prompt_sections: Vec<(String, String, String, String)> = tokio::task::spawn_blocking(
        move || -> Result<Vec<(String, String, String, String)>> {
            let conn =
                Connection::open(&db).context("Failed to open DB for file stale check")?;
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
        },
    )
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
            if let Ok(conn) = Connection::open(&db_cost) {
                let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
                let _ = conn.execute(
                    "INSERT INTO pyramid_cost_log (slug, operation, model, input_tokens, output_tokens, estimated_cost, source, layer, check_type, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'auto-stale', ?7, ?8, ?9)",
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
pub async fn dispatch_new_file_ingest(
    batch: Vec<PendingMutation>,
    db_path: &str,
) -> Result<()> {
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
        let conn = Connection::open(&db)
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

            // Create a new L0 node for this file
            let node_id = format!("L0-{}", Uuid::new_v4());

            // Truncate content for distilled field (first ~200 lines as summary placeholder)
            let distilled_lines: Vec<&str> = content.lines().take(200).collect();
            let distilled = format!(
                "File: {}\n\n{}",
                file_path,
                distilled_lines.join("\n")
            );

            // Insert node into pyramid_nodes
            conn.execute(
                "INSERT OR REPLACE INTO pyramid_nodes
                 (id, slug, depth, chunk_index, distilled, topics, corrections, decisions,
                  terms, dead_ends, self_prompt, children, parent_id, build_version, created_at)
                 VALUES (?1, ?2, 0, NULL, ?3, '[]', '[]', '[]', '[]', '[]', '', '[]', NULL, 1, ?4)",
                rusqlite::params![node_id, slug_c, distilled, now],
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

            // Write confirmed_stale mutation to WAL for L1 layer
            conn.execute(
                "INSERT INTO pyramid_pending_mutations
                 (slug, layer, mutation_type, target_ref, detail, cascade_depth, detected_at, processed)
                 VALUES (?1, 1, 'confirmed_stale', ?2, 'New file ingested', 0, ?3, 0)",
                rusqlite::params![slug_c, node_id, now],
            )
            .with_context(|| {
                format!("Failed to write L1 confirmed_stale for {}", file_path)
            })?;

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
pub async fn dispatch_tombstone(
    batch: Vec<PendingMutation>,
    db_path: &str,
) -> Result<()> {
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
        let conn = Connection::open(&db)
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

            // Create tombstone node
            let tombstone_id = format!("TOMB-{}", Uuid::new_v4());

            conn.execute(
                "INSERT INTO pyramid_nodes
                 (id, slug, depth, chunk_index, distilled, topics, corrections, decisions,
                  terms, dead_ends, self_prompt, children, parent_id, build_version, created_at)
                 VALUES (?1, ?2, 0, NULL, ?3, '[]', '[]', '[]', '[]', '[]', '', '[]', NULL, 1, ?4)",
                rusqlite::params![tombstone_id, slug_c, tombstone_content, now],
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

            // Write confirmed_stale mutation to WAL for L1 layer
            conn.execute(
                "INSERT INTO pyramid_pending_mutations
                 (slug, layer, mutation_type, target_ref, detail, cascade_depth, detected_at, processed)
                 VALUES (?1, 1, 'confirmed_stale', ?2, ?3, 0, ?4, 0)",
                rusqlite::params![slug_c, tombstone_id, tombstone_content, now],
            )?;

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
    let detail = mutation
        .detail
        .as_deref()
        .unwrap_or("{}");

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

    let (old_distilled, new_content_head) = tokio::task::spawn_blocking(move || -> Result<(String, String)> {
        let conn = Connection::open(&db)
            .context("Failed to open DB for rename check")?;

        // Look up old node content
        let node_ids = get_file_node_ids(&conn, &slug_c, &old_path_c)
            .unwrap_or_default();

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
            if let Ok(conn) = Connection::open(&db_cost) {
                let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
                let _ = conn.execute(
                    "INSERT INTO pyramid_cost_log (slug, operation, model, input_tokens, output_tokens, estimated_cost, source, layer, check_type, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'auto-stale', ?7, ?8, ?9)",
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
        let conn = Connection::open(&db)
            .context("Failed to open DB for rename post-processing")?;
        let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

        if result_c.rename {
            // Confirmed rename: update thread key, update file_hashes, supersede old
            let old_node_ids = get_file_node_ids(&conn, &slug_c, &old_path_c)
                .unwrap_or_default();

            if let Some(old_node_id) = old_node_ids.first() {
                // Create a rename note node
                let new_node_id = format!("L0-{}", Uuid::new_v4());
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
                conn.execute(
                    "INSERT OR REPLACE INTO pyramid_nodes
                     (id, slug, depth, chunk_index, distilled, topics, corrections, decisions,
                      terms, dead_ends, self_prompt, children, parent_id, build_version, created_at)
                     VALUES (?1, ?2, 0, NULL, ?3, '[]', '[]', '[]', '[]', '[]', '', '[]', NULL, 1, ?4)",
                    rusqlite::params![new_node_id, slug_c, distilled, now],
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

                // Write confirmed_stale for L1
                conn.execute(
                    "INSERT INTO pyramid_pending_mutations
                     (slug, layer, mutation_type, target_ref, detail, cascade_depth, detected_at, processed)
                     VALUES (?1, 1, 'confirmed_stale', ?2, ?3, 0, ?4, 0)",
                    rusqlite::params![slug_c, new_node_id, rename_note, now],
                )?;

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

                let tombstone_id = format!("TOMB-{}", Uuid::new_v4());

                conn.execute(
                    "INSERT INTO pyramid_nodes
                     (id, slug, depth, chunk_index, distilled, topics, corrections, decisions,
                      terms, dead_ends, self_prompt, children, parent_id, build_version, created_at)
                     VALUES (?1, ?2, 0, NULL, ?3, '[]', '[]', '[]', '[]', '[]', '', '[]', NULL, 1, ?4)",
                    rusqlite::params![tombstone_id, slug_c, tombstone_content, now],
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

                conn.execute(
                    "INSERT INTO pyramid_pending_mutations
                     (slug, layer, mutation_type, target_ref, detail, cascade_depth, detected_at, processed)
                     VALUES (?1, 1, 'confirmed_stale', ?2, ?3, 0, ?4, 0)",
                    rusqlite::params![slug_c, tombstone_id, tombstone_content, now],
                )?;
            }

            // Ingest the new file
            let new_content = read_file_content(&new_path_c).unwrap_or_default();
            if !new_content.is_empty() {
                let hash = compute_hash(new_content.as_bytes());
                let new_node_id = format!("L0-{}", Uuid::new_v4());

                let distilled_lines: Vec<&str> = new_content.lines().take(200).collect();
                let distilled = format!(
                    "File: {}\n\n{}",
                    new_path_c,
                    distilled_lines.join("\n")
                );

                conn.execute(
                    "INSERT OR REPLACE INTO pyramid_nodes
                     (id, slug, depth, chunk_index, distilled, topics, corrections, decisions,
                      terms, dead_ends, self_prompt, children, parent_id, build_version, created_at)
                     VALUES (?1, ?2, 0, NULL, ?3, '[]', '[]', '[]', '[]', '[]', '', '[]', NULL, 1, ?4)",
                    rusqlite::params![new_node_id, slug_c, distilled, now],
                )?;

                let node_ids_json = serde_json::to_string(&vec![&new_node_id])?;
                conn.execute(
                    "INSERT OR REPLACE INTO pyramid_file_hashes
                     (slug, file_path, hash, chunk_count, node_ids, last_ingested_at)
                     VALUES (?1, ?2, ?3, 1, ?4, ?5)",
                    rusqlite::params![slug_c, new_path_c, hash, node_ids_json, now],
                )?;

                conn.execute(
                    "INSERT INTO pyramid_pending_mutations
                     (slug, layer, mutation_type, target_ref, detail, cascade_depth, detected_at, processed)
                     VALUES (?1, 1, 'confirmed_stale', ?2, 'New file ingested (from rename-false)', 0, ?3, 0)",
                    rusqlite::params![slug_c, new_node_id, now],
                )?;
            }
        }

        Ok(())
    })
    .await??;

    Ok(result)
}
