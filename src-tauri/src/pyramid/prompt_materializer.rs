// pyramid/prompt_materializer.rs — Real prompt materialization for DADBEAR work items.
//
// The compiler creates work items with PLACEHOLDER prompts. At dispatch time,
// the supervisor calls `materialize_prompt()` to build REAL prompts from
// current pyramid state (node content, file content, deltas). This module
// reads the DB and disk to construct the actual system/user prompt pairs
// that will produce meaningful LLM output.
//
// This module does NOT make LLM calls — it only builds prompts.
// It does NOT refactor stale_helpers.rs — it replicates the prompt-building
// logic for each primitive type independently.

use std::collections::BTreeSet;

use anyhow::{Context, Result};
use rusqlite::Connection;
use similar::{ChangeTag, TextDiff};
use tracing::warn;

use super::llm::LlmConfig;
use super::stale_helpers_upper::resolve_live_canonical_node_id;
use super::step_context::compute_prompt_hash;

// ── Public types ──────────────────────────────────────────────────────────

/// A fully materialized prompt ready for LLM dispatch.
#[derive(Debug, Clone)]
pub struct MaterializedPrompt {
    pub system_prompt: String,
    pub user_prompt: String,
    pub model_tier: String,
    pub resolved_model_id: Option<String>,
    pub prompt_hash: String,
    pub temperature: f64,
    pub max_tokens: i64,
}

/// Materialization is not applicable for this primitive (mechanical operation).
#[derive(Debug, Clone)]
pub enum MaterializeResult {
    /// Prompt was built successfully.
    Prompt(MaterializedPrompt),
    /// This primitive is mechanical (no LLM call needed). The supervisor
    /// should apply the operation directly.
    Mechanical { reason: String },
    /// The target no longer exists — the work item is stale and should be
    /// skipped.
    TargetGone { reason: String },
}

// ── Public entry point ────────────────────────────────────────────────────

/// Materialize real prompts for a work item based on its primitive and target.
///
/// Reads current pyramid state (node content, file content, deltas) to build
/// the actual prompts that will produce meaningful LLM output.
///
/// Dispatches on (primitive, layer) to the appropriate prompt builder:
///   - stale_check @ L0 → file diff comparison (Template 1)
///   - stale_check @ L1+ → node delta comparison (Template 2)
///   - extract → mechanical (new file ingest, no LLM stale-check)
///   - tombstone → mechanical (deletion, no LLM call)
///   - rename_candidate → rename detection (Template 4)
///   - edge_check / connection_check → TODO (log and return placeholder)
///   - node_stale_check → node delta comparison (Template 2, same as stale_check L1+)
///
/// `observation_event_ids_json` is the JSON array of observation event IDs
/// from the work item — used to look up metadata_json for renames.
pub fn materialize_prompt(
    conn: &Connection,
    slug: &str,
    primitive: &str,
    layer: i64,
    target_id: &str,
    observation_event_ids_json: Option<&str>,
    _config: &LlmConfig,
) -> Result<MaterializeResult> {
    match (primitive, layer) {
        ("stale_check", 0) => materialize_l0_stale_check(conn, slug, target_id),
        ("stale_check", _) => materialize_upper_stale_check(conn, slug, target_id, layer),
        ("node_stale_check", _) => materialize_upper_stale_check(conn, slug, target_id, layer),
        ("extract", _) => Ok(MaterializeResult::Mechanical {
            reason: "New file ingest is mechanical — no LLM stale-check needed".into(),
        }),
        ("tombstone", _) => Ok(MaterializeResult::Mechanical {
            reason: "Tombstone is a mechanical deletion — no LLM call needed".into(),
        }),
        // Post-build accretion v5 Phase 3: role_bound + log_only are handled
        // directly by `apply_mechanical_primitive` (role_bound dispatches
        // the bound chain; log_only emits tracing info). Neither consults
        // the compute queue, so the materializer returns Mechanical to keep
        // the supervisor off the LLM dispatch path. Without this arm the
        // fallback `_ =>` branch below routes the work item through a
        // placeholder LLM prompt and burns compute — Phase 3 verifier fix.
        ("role_bound", _) => Ok(MaterializeResult::Mechanical {
            reason: "role_bound dispatches the bound chain — no LLM prompt materialization".into(),
        }),
        ("log_only", _) => Ok(MaterializeResult::Mechanical {
            reason: "log_only is chronicle-only — no LLM call needed".into(),
        }),
        // v5 Phase 8-2: re_distill work items are queued by the cascade
        // chain's queue_re_distill_for_target mechanical step. They flow
        // through the supervisor's apply_mechanical_primitive re_distill
        // arm which delegates to execute_supersession (that helper runs
        // its own LLM call internally via generate_change_manifest). No
        // outer prompt is needed — returning Mechanical keeps the item
        // off the compute queue so execute_supersession can handle end-
        // to-end supersession (change_manifest LLM call + pyramid_nodes
        // UPDATE + build_version bump + node_re_distilled chronicle).
        //
        // Pre-Phase-8 this primitive fell through to the unknown-primitive
        // placeholder arm, dispatched a garbage "[Unknown primitive
        // 're_distill' at L{layer}...]" prompt, and the default apply
        // arm marked it applied:re_distill without doing anything. That
        // was THE original DADBEAR non-firing bug.
        ("re_distill", _) => Ok(MaterializeResult::Mechanical {
            reason: "re_distill delegates to execute_supersession — LLM call is internal".into(),
        }),
        ("rename_candidate", _) => {
            // Look up the observation event's metadata_json to get old_path/new_path.
            let detail_json = observation_event_ids_json
                .and_then(|ids_json| lookup_observation_metadata(conn, ids_json));
            materialize_rename_check(conn, slug, target_id, detail_json.as_deref())
        }
        ("edge_check", _) | ("connection_check", _) => {
            // TODO: wire up edge/connection check prompts incrementally
            Ok(MaterializeResult::Prompt(MaterializedPrompt {
                system_prompt: format!(
                    "You are evaluating edge validity for node {target_id} in pyramid {slug}."
                ),
                user_prompt: format!(
                    "Check whether the edge to {target_id} is still valid given recent changes."
                ),
                model_tier: "stale_remote".into(),
                resolved_model_id: None,
                prompt_hash: compute_prompt_hash("edge_check_placeholder"),
                temperature: 0.1,
                max_tokens: 512,
            }))
        }
        ("faq_redistill", _) => {
            // TODO: wire up FAQ redistillation prompts
            Ok(MaterializeResult::Prompt(MaterializedPrompt {
                system_prompt: format!("You are re-distilling FAQ categories for pyramid {slug}."),
                user_prompt: format!(
                    "Re-evaluate and update the FAQ category for target {target_id}."
                ),
                model_tier: "stale_remote".into(),
                resolved_model_id: None,
                prompt_hash: compute_prompt_hash("faq_redistill_placeholder"),
                temperature: 0.1,
                max_tokens: 2048,
            }))
        }
        _ => {
            warn!(
                primitive = %primitive,
                layer = layer,
                target_id = %target_id,
                "prompt_materializer: unknown primitive, returning placeholder"
            );
            Ok(MaterializeResult::Prompt(MaterializedPrompt {
                system_prompt: format!(
                    "[Unknown primitive '{primitive}' at L{layer} for {target_id}]"
                ),
                user_prompt: format!(
                    "Target: {target_id}\nPrimitive: {primitive}\nLayer: L{layer}"
                ),
                model_tier: "stale_remote".into(),
                resolved_model_id: None,
                prompt_hash: compute_prompt_hash(&format!("unknown_{primitive}")),
                temperature: 0.1,
                max_tokens: 1024,
            }))
        }
    }
}

// ── L0 stale check (Template 1) ──────────────────────────────────────────

/// Build the Template 1 prompt: compare old node distilled content against
/// current file on disk via a unified diff.
///
/// target_id for L0 stale_check is the file_path.
fn materialize_l0_stale_check(
    conn: &Connection,
    slug: &str,
    file_path: &str,
) -> Result<MaterializeResult> {
    // Read current file from disk.
    let new_content = match std::fs::read_to_string(file_path) {
        Ok(c) => c,
        Err(e) => {
            return Ok(materialize_stale_skip_adjudication(
                slug,
                file_path,
                0,
                "file",
                &format!("Cannot read file from disk: {file_path}: {e}"),
            ));
        }
    };

    // Look up node_ids from pyramid_file_hashes.
    let node_ids = get_file_node_ids(conn, slug, file_path)?;
    if node_ids.is_empty() {
        return Ok(materialize_stale_skip_adjudication(
            slug,
            file_path,
            0,
            "file",
            &format!("No node_ids found in pyramid_file_hashes for {file_path}"),
        ));
    }

    // Concatenate distilled content from all chunks.
    let mut old_content = String::new();
    for nid in &node_ids {
        match get_node_distilled(conn, slug, nid) {
            Ok(c) => {
                if !old_content.is_empty() {
                    old_content.push_str("\n---\n");
                }
                old_content.push_str(&c);
            }
            Err(e) => {
                warn!(node_id = %nid, error = %e, "Failed to get node content during materialization");
            }
        }
    }

    let diff = compute_diff(&old_content, &new_content);

    // Template 1 system prompt (replicated from stale_helpers.rs).
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

    let user_prompt = format!(
        "---\n\nFILE 1 of 1: {}\n\nOLD (pyramid reflects this):\n{}\n\nNEW (current on disk):\n{}\n\nDIFF:\n{}\n",
        file_path, old_content, new_content, diff
    );

    Ok(MaterializeResult::Prompt(MaterializedPrompt {
        system_prompt: system_prompt.to_string(),
        user_prompt,
        model_tier: "stale_remote".into(),
        resolved_model_id: None,
        prompt_hash: compute_prompt_hash(system_prompt),
        temperature: 0.1,
        max_tokens: 1024,
    }))
}

// ── L1+ stale check (Template 2) ─────────────────────────────────────────

/// Build the Template 2 prompt: compare the node's current distillation
/// against recent deltas to determine if re-distillation is needed.
///
/// target_id for upper-layer stale_check is the node_id (or thread_id).
fn materialize_upper_stale_check(
    conn: &Connection,
    slug: &str,
    target_id: &str,
    layer: i64,
) -> Result<MaterializeResult> {
    // Resolve to live canonical node via thread system.
    let canonical_id = resolve_live_canonical_node_id(conn, slug, target_id)?;
    let canonical_id = match canonical_id {
        Some(id) => id,
        None => {
            return Ok(materialize_stale_skip_adjudication(
                slug,
                target_id,
                layer,
                "node",
                &format!("No live canonical node found for target {target_id} in slug {slug}"),
            ));
        }
    };

    // Look up the thread_id for this canonical node.
    let thread_id: Option<String> = conn
        .query_row(
            "SELECT thread_id FROM pyramid_threads
             WHERE slug = ?1 AND current_canonical_id = ?2",
            rusqlite::params![slug, canonical_id],
            |row| row.get(0),
        )
        .ok();

    let effective_thread_id = thread_id.unwrap_or_else(|| target_id.to_string());

    // Read the node's distilled content.
    let (distilled, depth) = conn
        .query_row(
            "SELECT distilled, depth FROM pyramid_nodes WHERE id = ?1 AND slug = ?2",
            rusqlite::params![canonical_id, slug],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i32>(1)?)),
        )
        .unwrap_or_else(|_| (String::new(), layer as i32));

    // Read recent deltas for this thread.
    let mut delta_content = String::new();
    let mut stmt = conn.prepare(
        "SELECT content FROM pyramid_deltas
         WHERE slug = ?1 AND thread_id = ?2
         ORDER BY sequence DESC LIMIT 10",
    )?;
    let rows = stmt.query_map(rusqlite::params![slug, effective_thread_id], |row| {
        row.get::<_, String>(0)
    })?;
    for row in rows {
        if let Ok(content) = row {
            if !delta_content.is_empty() {
                delta_content.push_str("\n\n");
            }
            delta_content.push_str(&content);
        }
    }

    if delta_content.is_empty() && distilled.is_empty() {
        return Ok(materialize_stale_skip_adjudication(
            slug,
            target_id,
            layer,
            "node",
            &format!(
                "No distilled content and no deltas for node {canonical_id} (thread {effective_thread_id})"
            ),
        ));
    }

    // Template 2 system prompt (replicated from stale_helpers_upper.rs).
    let system_prompt =
        "You are evaluating whether changes to lower-level knowledge nodes require \
        updating higher-level distillations. Output JSON only.";

    let user_prompt = format!(
        "You are evaluating whether changes to lower-level knowledge nodes require \
        updating higher-level distillations. For each node below, you see the \
        CURRENT distillation and the new delta(s) that have landed since it was written.\n\n\
        \"stale: true\" means: the delta(s) represent information that meaningfully \
        changes what this distillation says. The summary is now incomplete, inaccurate, \
        or misleading without incorporating these changes.\n\n\
        \"stale: false\" means: the delta(s) are minor refinements that don't change \
        the thrust of the distillation. It's still accurate enough.\n\n\
        When in doubt, choose true.\n\n---\n\n\
        NODE 1 of 1:\nCanonical node ID: {canonical}\nThread ID: {thread}\nLayer: L{depth}\n\n\
        Current distillation:\n{distilled}\n\nDelta(s):\n{deltas}\n\n---\n\n\
        Output JSON only. Array of objects, one per node:\n\n\
        [{{\"node_id\": \"...\", \"stale\": true, \"reason\": \"one sentence\"}}]",
        canonical = canonical_id,
        thread = effective_thread_id,
        depth = depth,
        distilled = distilled,
        deltas = if delta_content.is_empty() {
            "(no deltas found)".to_string()
        } else {
            delta_content
        }
    );

    Ok(MaterializeResult::Prompt(MaterializedPrompt {
        system_prompt: system_prompt.to_string(),
        user_prompt,
        model_tier: "stale_remote".into(),
        resolved_model_id: None,
        prompt_hash: compute_prompt_hash(system_prompt),
        temperature: 0.1,
        max_tokens: 2048,
    }))
}

fn materialize_stale_skip_adjudication(
    slug: &str,
    target_id: &str,
    layer: i64,
    target_kind: &str,
    candidate_reason: &str,
) -> MaterializeResult {
    let id_field = if target_kind == "file" {
        "file_path"
    } else {
        "node_id"
    };
    let system_prompt = "\
You are adjudicating a stale-check candidate that could not be checked by the \
normal resolver. Do not approve a skip mechanically. Decide whether the \
candidate should be skipped, marked stale, or treated as passing. Output JSON \
only.";
    let user_prompt = format!(
        "Pyramid slug: {slug}\nLayer: L{layer}\nTarget kind: {target_kind}\nTarget ID: {target_id}\n\n\
The stale-check candidate appears skippable because:\n{candidate_reason}\n\n\
Confirm or deny that skip. Use decision \"skip\" only when no meaningful stale \
check can be performed for this candidate. Use decision \"stale\" when the \
candidate should still force repair or operator attention. Use decision \
\"pass\" when the candidate is valid and current.\n\n\
Output JSON only. Array with one object:\n\
[{{\"{id_field}\": \"{target_id}\", \"decision\": \"skip\", \"stale\": false, \"reason\": \"one sentence, verbatim for the Stale Check Log\"}}]"
    );
    let hash_source = format!("{system_prompt}\n{user_prompt}");

    MaterializeResult::Prompt(MaterializedPrompt {
        system_prompt: system_prompt.to_string(),
        user_prompt,
        model_tier: "stale_remote".into(),
        resolved_model_id: None,
        prompt_hash: compute_prompt_hash(&hash_source),
        temperature: 0.1,
        max_tokens: 512,
    })
}

// ── Rename check (Template 4) ─────────────────────────────────────────────

/// Build the Template 4 prompt: determine if a disappeared + appeared file
/// pair is a rename or two unrelated files.
fn materialize_rename_check(
    conn: &Connection,
    slug: &str,
    target_id: &str,
    detail_json: Option<&str>,
) -> Result<MaterializeResult> {
    // Try to extract old_path/new_path from metadata_json first.
    let (old_path, new_path) = if let Some(detail) = detail_json {
        let parsed: serde_json::Value =
            serde_json::from_str(detail).context("Failed to parse rename detail JSON")?;
        let old = parsed
            .get("old_path")
            .and_then(|v| v.as_str())
            .map(String::from);
        let new = parsed
            .get("new_path")
            .and_then(|v| v.as_str())
            .map(String::from);
        match (old, new) {
            (Some(o), Some(n)) => (o, n),
            _ => {
                // Fallback: try parsing from target_id format rename/{old}/{new}
                match parse_rename_target_id(target_id) {
                    Some(pair) => pair,
                    None => {
                        return Ok(MaterializeResult::TargetGone {
                            reason: format!(
                                "Cannot extract old_path/new_path from detail_json or target_id: {target_id}"
                            ),
                        });
                    }
                }
            }
        }
    } else {
        match parse_rename_target_id(target_id) {
            Some(pair) => pair,
            None => {
                return Ok(MaterializeResult::TargetGone {
                    reason: format!(
                        "No detail_json and cannot parse rename target_id: {target_id}"
                    ),
                });
            }
        }
    };

    // Look up old node content.
    let node_ids = get_file_node_ids(conn, slug, &old_path).unwrap_or_default();
    let old_distilled = node_ids
        .first()
        .and_then(|nid| get_node_distilled(conn, slug, nid).ok())
        .unwrap_or_else(|| format!("(no pyramid content found for {})", old_path));

    // Read new file (first 200 lines).
    let new_content = std::fs::read_to_string(&new_path).unwrap_or_default();
    let head_lines: Vec<&str> = new_content.lines().take(200).collect();
    let new_content_head = head_lines.join("\n");

    // Template 4 system prompt (replicated from stale_helpers.rs).
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

    Ok(MaterializeResult::Prompt(MaterializedPrompt {
        system_prompt: system_prompt.to_string(),
        user_prompt,
        model_tier: "stale_remote".into(),
        resolved_model_id: None,
        prompt_hash: compute_prompt_hash(system_prompt),
        temperature: 0.1,
        max_tokens: 256,
    }))
}

// ── DB/IO utility functions (local to this module) ────────────────────────

/// Look up a node's distilled field from the pyramid_nodes table.
fn get_node_distilled(conn: &Connection, slug: &str, node_id: &str) -> Result<String> {
    conn.query_row(
        "SELECT distilled FROM pyramid_nodes WHERE slug = ?1 AND id = ?2",
        rusqlite::params![slug, node_id],
        |row| row.get::<_, String>(0),
    )
    .with_context(|| format!("Failed to get node content for {}:{}", slug, node_id))
}

/// Look up node_ids from pyramid_file_hashes for a given file path,
/// resolving each through the supersession chain to the live canonical id.
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

/// Look up the metadata_json from the first observation event referenced by
/// a work item's observation_event_ids JSON array.
fn lookup_observation_metadata(conn: &Connection, ids_json: &str) -> Option<String> {
    let ids: Vec<i64> = serde_json::from_str(ids_json).ok()?;
    let first_id = ids.first()?;
    conn.query_row(
        "SELECT metadata_json FROM dadbear_observation_events WHERE id = ?1",
        rusqlite::params![first_id],
        |row| row.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

/// Attempt to parse the rename target_id format: `rename/{old_path}/{new_path}`.
///
/// Because paths are absolute (start with `/`), the format is actually
/// `rename//abs/old_path//abs/new_path`. We find the boundary by looking
/// for the second `//` after the initial `rename/` prefix.
fn parse_rename_target_id(target_id: &str) -> Option<(String, String)> {
    let rest = target_id.strip_prefix("rename/")?;
    // Both paths are absolute, so the boundary is `//` after the first `/`.
    // Find the position of the second absolute path (next `//` boundary within rest,
    // but we need to handle the case where the first path starts with `/`).
    // Strategy: the old_path starts at rest[0], the new_path starts where we see
    // a `/` followed by another `/` that starts a new absolute path. However,
    // paths can contain `//` inside them. Heuristic: find the rightmost `/` that
    // is followed by `/Users/` or `/home/` or `/tmp/` etc.
    // Simpler: both paths are OS absolute. On macOS/Linux they start with `/`.
    // So the format is `/old_path/new_path` is ambiguous unless we know the boundary.
    // The compiler generates: format!("rename/{old}/{new}") where old and new are
    // both absolute. So the string is: `rename//abs/old//abs/new`
    // We can split on the boundary between the two absolute paths.
    // Look for a `/` preceded by a non-`/` char followed by another `/` — but this
    // is still ambiguous for paths with intermediate segments.
    //
    // Pragmatic approach: since this is a fallback (we prefer metadata_json lookup),
    // just return None and let the caller handle it.
    //
    // Actually, we can be smarter: absolute paths on macOS start with /Users/ or
    // other known roots. The boundary is where we see a `/Users/` (or similar)
    // that isn't the start of the string.
    if let Some(boundary) = rest[1..]
        .find("/Users/")
        .or_else(|| rest[1..].find("/home/"))
        .or_else(|| rest[1..].find("/tmp/"))
    {
        let old_path = &rest[..boundary + 1];
        let new_path = &rest[boundary + 1..];
        if !old_path.is_empty() && !new_path.is_empty() {
            return Some((old_path.to_string(), new_path.to_string()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::db::init_pyramid_db;
    use rusqlite::Connection;

    #[test]
    fn l0_missing_file_materializes_llm_skip_adjudication_prompt() {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();

        let result = materialize_l0_stale_check(
            &conn,
            "skip-adjudication",
            "/tmp/definitely_missing_for_stale_skip.rs",
        )
        .unwrap();

        match result {
            MaterializeResult::Prompt(prompt) => {
                assert!(
                    prompt.user_prompt.contains("\"decision\": \"skip\""),
                    "skip candidates must be routed through an LLM decision prompt: {}",
                    prompt.user_prompt
                );
                assert!(
                    prompt.user_prompt.contains("Cannot read file from disk"),
                    "prompt must carry the resolver reason"
                );
            }
            other => panic!("expected LLM prompt, got {other:?}"),
        }
    }
}
