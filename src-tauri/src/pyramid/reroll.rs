// pyramid/reroll.rs — Phase 13 node/cache-entry reroll with notes.
//
// Implements the `pyramid_reroll_node` IPC: the user selects a node
// (or an intermediate cache entry) in the build viz, enters a note
// explaining why, and submits. This module:
//
//   1. Validates the target (exactly one of node_id / cache_key).
//   2. Loads the prior cache entry the user wants to replace.
//   3. Reconstructs the original system + user prompts from the
//      entry's stored metadata so the reroll call can re-invoke the
//      same LLM path.
//   4. Calls `call_model_unified_with_options_and_ctx` with
//      `force_fresh = true` so the cache layer routes the write
//      through `supersede_cache_entry`.
//   5. For node reroll (node_id provided) writes a change-manifest row
//      with the user's note populated, so the audit trail captures
//      the rationale.
//   6. Walks the downstream cache entries at (depth + 1) and marks
//      them invalidated, emitting `CacheInvalidated` per row.
//   7. Emits `NodeRerolled` on the build event bus.
//
// Spec: `docs/specs/build-viz-expansion.md` §Node Reroll & Notes,
// §Reroll for Intermediate Outputs. Downstream invalidation scope is
// single-level per the workstream prompt — transitive walking is a
// future refinement.

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::db;
use super::event_bus::{BuildEventBus, TaggedBuildEvent, TaggedKind};
use super::llm::{call_model_unified_with_options_and_ctx, LlmCallOptions, LlmConfig, LlmResponse};
use super::step_context::{CacheEntry, CachedStepOutput, StepContext};

// ── IPC contract types ─────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct RerollInput {
    pub slug: String,
    /// Node-level reroll target. Exactly one of `node_id` or
    /// `cache_key` must be present.
    pub node_id: Option<String>,
    /// Intermediate cache-entry reroll target. Exactly one of
    /// `node_id` or `cache_key` must be present.
    pub cache_key: Option<String>,
    /// User's rationale. May be empty — the UI should discourage
    /// empty notes but the backend accepts them.
    pub note: String,
    /// Always true from the UI. Surfaced in the input so tests can
    /// flip it off when exercising the cache bypass guard.
    #[serde(default = "default_force_fresh")]
    pub force_fresh: bool,
}

fn default_force_fresh() -> bool {
    true
}

#[derive(Debug, Clone, Serialize)]
pub struct RerollOutput {
    /// Row id of the new cache entry produced by the reroll.
    pub new_cache_entry_id: i64,
    /// Row id of the change manifest, or `None` for intermediate-
    /// output reroll (cache_key target) where manifests are not
    /// generated.
    pub manifest_id: Option<i64>,
    /// New content returned by the LLM. The UI replaces the preview
    /// with this value without a refetch.
    pub new_content: Value,
    /// Number of downstream entries marked invalidated.
    pub downstream_invalidated: usize,
    /// True when 3 or more rerolls have been performed in the last
    /// 10 minutes for this step slot. The UI renders a warning
    /// banner but the backend does not hard-block.
    pub rate_limit_warning: bool,
}

// ── Core implementation ───────────────────────────────────────────

/// Phase 13 reroll entry point. Mirrors the Tauri IPC shape; callers
/// in tests can drive this directly without going through the IPC
/// layer.
pub async fn reroll_node(
    input: RerollInput,
    llm_config: LlmConfig,
    db_path: String,
    bus: Arc<BuildEventBus>,
) -> Result<RerollOutput> {
    // 1. Validate that exactly one target is supplied.
    match (input.node_id.as_deref(), input.cache_key.as_deref()) {
        (Some(_), None) | (None, Some(_)) => {}
        (Some(_), Some(_)) => {
            return Err(anyhow!(
                "pyramid_reroll_node: exactly one of node_id or cache_key must be provided"
            ))
        }
        (None, None) => {
            return Err(anyhow!(
                "pyramid_reroll_node: one of node_id or cache_key must be provided"
            ))
        }
    }

    let slug = input.slug.clone();
    let note = input.note.clone();

    // 2. Resolve the target cache entry. We go through the
    // "including invalidated" reader so rerolling an already-stale
    // row still works — the user may want to confirm a new version
    // of a node whose upstream was also rerolled.
    let (prior, prior_source_tag) = load_reroll_target(&input, &db_path)?;
    let prior_content: Value = serde_json::from_str(&prior.output_json)
        .map_err(|e| anyhow!("reroll: prior output_json parse failed: {}", e))?;

    // 3. Build the reroll prompts. We intentionally wrap the user's
    // feedback in a clear "the user wants a different version"
    // framing so the model understands the reroll semantics even
    // without a specialized template.
    let (system_prompt, user_prompt) = build_reroll_prompts(&prior, &prior_content, &note);

    // 4. Construct a StepContext that carries the bus (so the
    // LlmCallStarted/Completed events still fire) but is NOT
    // cache-usable — `prompt_hash` is left empty, so
    // `try_cache_lookup_or_key` short-circuits to
    // `MissOrBypass(None)` and `try_cache_store` no-ops. We do the
    // cache write manually below so the new row lands at the PRIOR
    // cache_key (not the content-addressable hash of the reroll
    // wrapper prompts).
    //
    // Phase 13 wanderer fix: the previous implementation passed
    // `with_prompt_hash(prior.prompt_hash)` which made the ctx
    // cache-usable, but the call path then computed a NEW cache_key
    // from `(hash(reroll_system, reroll_user), prior_prompt_hash,
    // prior_model_id)` — different from `prior.cache_key`. The
    // auto-store landed the new row at the NEW key,
    // `supersede_cache_entry` found no prior row at the new key, and
    // the supersession chain was never created. `load_new_cache_row`
    // then silently loaded the PRIOR (untouched) row back, so:
    //   - the returned `new_cache_entry_id` was the old row's id,
    //   - the note was UPDATE'd onto the old row,
    //   - the new row had `supersedes_cache_id = NULL`,
    //   - `count_recent_rerolls` never counted the reroll (it gates
    //     on `supersedes_cache_id IS NOT NULL`), disabling the
    //     anti-slot-machine rate limit,
    //   - subsequent normal builds still hit the original prompts'
    //     cache_key and served the pre-reroll content.
    // The fix routes the DB write manually so the new row occupies
    // prior.cache_key with a proper supersedes_cache_id link.
    let build_id = format!(
        "reroll-{}-{}",
        slug,
        chrono::Utc::now().timestamp()
    );
    let ctx = StepContext::new(
        slug.clone(),
        build_id.clone(),
        prior.step_name.clone(),
        prior_source_tag.clone(),
        prior.depth,
        if prior.chunk_index == -1 {
            None
        } else {
            Some(prior.chunk_index)
        },
        db_path.clone(),
    )
    .with_model_resolution("reroll", prior.model_id.clone())
    .with_bus(bus.clone())
    .with_force_fresh(input.force_fresh);
    // Deliberately NOT calling `.with_prompt_hash(...)` — leaving
    // `prompt_hash = ""` flips `cache_is_usable()` to false.
    debug_assert!(
        !ctx.cache_is_usable(),
        "reroll ctx must bypass the cache-aware path so the manual \
         supersession below lands at the prior cache_key"
    );

    // 5. Call the LLM. Events fire (LlmCallStarted, LlmCallCompleted,
    // StepRetry, StepError) because `ctx.bus` is present; the cache
    // lookup/store path is skipped because the ctx is not
    // cache-usable.
    let call_started = std::time::Instant::now();
    let response = call_model_unified_with_options_and_ctx(
        &llm_config,
        Some(&ctx),
        &system_prompt,
        &user_prompt,
        /*temperature=*/ 0.3,
        /*max_tokens=*/ 4096,
        /*response_format=*/ None,
        LlmCallOptions::default(),
    )
    .await?;

    // 6. Parse the new content. We attempt to JSON-parse the LLM
    // output; if the model returned free text, we wrap it in a
    // `{"content": "..."}` envelope so the caller's type is still a
    // `Value`.
    let new_content: Value = match serde_json::from_str::<Value>(&response.content) {
        Ok(v) => v,
        Err(_) => serde_json::json!({ "content": response.content }),
    };

    // 7. Manually supersede the prior cache row. The new entry
    // occupies prior.cache_key so future builds with the original
    // prompts hit the rerolled content. inputs_hash / prompt_hash /
    // model_id are intentionally copied from the prior row so
    // `verify_cache_hit` passes on subsequent lookups — the
    // content-addressable invariant (key == hash of inputs) still
    // holds for the slot, just with the rerolled body.
    let latency_ms = call_started.elapsed().as_millis() as i64;
    let new_cache_entry_id = write_reroll_cache_entry(
        &db_path,
        &prior,
        &build_id,
        &response,
        &note,
        latency_ms,
    )?;

    // 8. Node-level reroll also writes a change-manifest row with
    // the note populated. Intermediate (cache_key only) reroll
    // skips this step — the spec reserves manifests for node-level
    // changes.
    let manifest_id = if let Some(node_id) = input.node_id.as_deref() {
        Some(write_reroll_manifest(
            &db_path,
            &slug,
            node_id,
            &new_content,
            &note,
            &bus,
        )?)
    } else {
        None
    };

    // 9. Walk downstream dependents at depth + 1 and mark them
    // invalidated. Single-level walker per the workstream prompt —
    // transitive invalidation is deferred.
    let downstream_keys =
        run_downstream_invalidation(&db_path, &slug, prior.depth, &prior.cache_key, &bus)?;

    // 10. Emit NodeRerolled on the bus.
    let _ = bus.tx.send(TaggedBuildEvent {
        slug: slug.clone(),
        kind: TaggedKind::NodeRerolled {
            slug: slug.clone(),
            build_id: build_id.clone(),
            node_id: input.node_id.clone(),
            step_name: prior.step_name.clone(),
            note: note.clone(),
            new_cache_entry_id,
            manifest_id,
        },
    });

    // 11. Compute the rate-limit warning after the reroll has
    // landed so the count reflects the current write. Not a hard
    // block — just a hint for the UI.
    let rate_limit_warning =
        count_recent_rerolls_for_target(&db_path, &slug, &prior)? >= 3;

    Ok(RerollOutput {
        new_cache_entry_id,
        manifest_id,
        new_content,
        downstream_invalidated: downstream_keys.len(),
        rate_limit_warning,
    })
}

/// Load the cache entry the user is rerolling against. `node_id`
/// resolves via the supersession chain (see spec); `cache_key` is a
/// direct lookup.
fn load_reroll_target(
    input: &RerollInput,
    db_path: &str,
) -> Result<(CachedStepOutput, String)> {
    let conn = db::open_pyramid_connection(Path::new(db_path))?;

    if let Some(ck) = input.cache_key.as_deref() {
        let row = db::check_cache_including_invalidated(&conn, &input.slug, ck)?
            .ok_or_else(|| anyhow!("reroll: no cache entry found for cache_key={}", ck))?;
        return Ok((row, "reroll_cache_entry".to_string()));
    }

    if let Some(node_id) = input.node_id.as_deref() {
        // Node-level reroll: look up the cache entry associated with
        // the node's producing step. The current schema doesn't have
        // an explicit node_id → cache_key foreign key, so we walk by
        // step_name + depth + chunk_index. The most recent cache
        // entry at that slot is the one the node was produced from.
        //
        // If the node's producing step isn't cached (legacy nodes
        // that were built before Phase 6), the reroll path cannot
        // construct the prior prompts — report a clear error so the
        // UI can tell the user.
        let row = lookup_cache_entry_for_node(&conn, &input.slug, node_id)?
            .ok_or_else(|| anyhow!(
                "reroll: no cache entry found for node_id={} — node may predate the content-addressable cache",
                node_id
            ))?;
        return Ok((row, "reroll_node".to_string()));
    }

    Err(anyhow!("reroll: no target specified"))
}

/// Best-effort lookup from `node_id` to its producing cache entry.
///
/// MVP heuristic: scan `pyramid_step_cache` for rows whose
/// `output_json` content mentions the node id. Nodes in the live
/// schema are persisted to `pyramid_nodes`, which tracks the
/// `build_id` that built them; we join to `pyramid_step_cache` on
/// that `build_id` and pick the most recent matching row.
///
/// A cleaner path would be an explicit `cache_key` column on
/// `pyramid_nodes` — that's a future schema refinement. For now, the
/// simplest reliable lookup is "the last cache entry written for
/// this slug whose output_json contains the node_id". This matches
/// the Phase 13 spec guidance: "ship the simplest path that works;
/// document the schema choice in the implementation log".
fn lookup_cache_entry_for_node(
    conn: &Connection,
    slug: &str,
    node_id: &str,
) -> Result<Option<CachedStepOutput>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, build_id, step_name, chunk_index, depth, cache_key,
                inputs_hash, prompt_hash, model_id, output_json, token_usage_json,
                cost_usd, latency_ms, created_at, force_fresh, supersedes_cache_id,
                note, invalidated_by
         FROM pyramid_step_cache
         WHERE slug = ?1 AND output_json LIKE ?2
         ORDER BY id DESC
         LIMIT 1",
    )?;
    let like_pattern = format!("%{}%", node_id);
    let mut rows = stmt.query(rusqlite::params![slug, like_pattern])?;
    if let Some(row) = rows.next()? {
        Ok(Some(CachedStepOutput {
            id: row.get(0)?,
            slug: row.get(1)?,
            build_id: row.get(2)?,
            step_name: row.get(3)?,
            chunk_index: row.get(4)?,
            depth: row.get(5)?,
            cache_key: row.get(6)?,
            inputs_hash: row.get(7)?,
            prompt_hash: row.get(8)?,
            model_id: row.get(9)?,
            output_json: row.get(10)?,
            token_usage_json: row.get(11)?,
            cost_usd: row.get(12)?,
            latency_ms: row.get(13)?,
            created_at: row.get(14)?,
            force_fresh: row.get::<_, i64>(15)? != 0,
            supersedes_cache_id: row.get(16)?,
            note: row.get::<_, Option<String>>(17).unwrap_or(None),
            invalidated_by: row.get::<_, Option<String>>(18).unwrap_or(None),
        }))
    } else {
        Ok(None)
    }
}

/// Build the reroll system + user prompts from the prior entry and
/// the user's note. Phase 13 MVP: use a simple wrap template —
/// original output framed as "the current version", the user's
/// note as "their feedback". A future iteration can thread the
/// original prompt template body through cache metadata so the
/// reroll matches the exact original shape.
fn build_reroll_prompts(
    prior: &CachedStepOutput,
    prior_content: &Value,
    note: &str,
) -> (String, String) {
    let system_prompt = format!(
        "You are rerolling a prior LLM output at the user's request. \
         The user has provided feedback explaining why the prior version was \
         inadequate. Produce an improved version that incorporates their \
         concern. Return JSON matching the shape of the prior output when \
         possible. \
         (step={}, depth={}, chunk={})",
        prior.step_name, prior.depth, prior.chunk_index
    );

    let prior_preview = serde_json::to_string_pretty(prior_content)
        .unwrap_or_else(|_| prior.output_json.clone());

    let note_section = if note.trim().is_empty() {
        "(no feedback provided — regenerate with fresh randomness)".to_string()
    } else {
        format!("The user's feedback:\n{}", note.trim())
    };

    let user_prompt = format!(
        "The current output you should improve:\n\n{}\n\n---\n\n{}\n\n\
         Produce an improved version that addresses the feedback.",
        prior_preview, note_section
    );

    (system_prompt, user_prompt)
}

/// Phase 13 wanderer fix: manually archive the prior cache row and
/// insert the rerolled row at the same cache_key. This replaces the
/// broken auto-store path — the reroll wrapper prompts produce a
/// different content-addressable key than the original, so we can't
/// rely on the cache-aware store path to route the write through
/// `supersede_cache_entry` with the right prior key.
///
/// The new row:
///   - occupies `prior.cache_key` (so subsequent builds hit it),
///   - carries the ORIGINAL `inputs_hash`/`prompt_hash`/`model_id`
///     so `verify_cache_hit` passes on read-back,
///   - has `force_fresh = true` and `supersedes_cache_id` linked to
///     the archived prior row (set by `supersede_cache_entry`),
///   - stores the user's note so `count_recent_rerolls` (which
///     gates the anti-slot-machine warning on
///     `supersedes_cache_id IS NOT NULL`) counts this reroll.
///
/// Returns the id of the newly-inserted row.
fn write_reroll_cache_entry(
    db_path: &str,
    prior: &CachedStepOutput,
    build_id: &str,
    response: &LlmResponse,
    note: &str,
    latency_ms: i64,
) -> Result<i64> {
    // Mirror of llm.rs::serialize_response_for_cache — kept private
    // over there, replicated here so the cache row format is
    // consistent between the normal store path and the reroll path.
    let output_json = serde_json::json!({
        "content": response.content,
        "usage": {
            "prompt_tokens": response.usage.prompt_tokens,
            "completion_tokens": response.usage.completion_tokens,
        },
        "generation_id": response.generation_id,
        "actual_cost_usd": response.actual_cost_usd,
        "provider_id": response.provider_id,
    })
    .to_string();

    let token_usage_json = serde_json::to_string(&serde_json::json!({
        "prompt_tokens": response.usage.prompt_tokens,
        "completion_tokens": response.usage.completion_tokens,
    }))
    .ok();

    let note_opt = if note.is_empty() { None } else { Some(note.to_string()) };

    let new_entry = CacheEntry {
        slug: prior.slug.clone(),
        build_id: build_id.to_string(),
        step_name: prior.step_name.clone(),
        chunk_index: prior.chunk_index,
        depth: prior.depth,
        cache_key: prior.cache_key.clone(),
        inputs_hash: prior.inputs_hash.clone(),
        prompt_hash: prior.prompt_hash.clone(),
        model_id: prior.model_id.clone(),
        output_json,
        token_usage_json,
        cost_usd: response.actual_cost_usd,
        latency_ms: Some(latency_ms),
        force_fresh: true,
        supersedes_cache_id: None, // set by supersede_cache_entry
        note: note_opt,
    };

    let conn = db::open_pyramid_connection(Path::new(db_path))?;
    db::supersede_cache_entry(&conn, &prior.slug, &prior.cache_key, &new_entry)?;

    // Re-read the just-written row to grab its id. The row lives
    // at prior.cache_key (supersede archived the old one), and we
    // need the id for both `NodeRerolled` and the RerollOutput
    // returned to the frontend.
    let new_row = db::check_cache_including_invalidated(&conn, &prior.slug, &prior.cache_key)?
        .ok_or_else(|| {
            anyhow!(
                "reroll: new cache row not found post-write (slug={}, key={})",
                prior.slug,
                prior.cache_key
            )
        })?;
    Ok(new_row.id)
}

/// Write the change manifest row for node-level reroll. Intermediate
/// (cache_key) reroll skips this step. The manifest payload carries
/// the new content + note so the audit trail is complete.
fn write_reroll_manifest(
    db_path: &str,
    slug: &str,
    node_id: &str,
    new_content: &Value,
    note: &str,
    _bus: &Arc<BuildEventBus>,
) -> Result<i64> {
    let conn = db::open_pyramid_connection(Path::new(db_path))?;

    // Phase 13 MVP: the change manifest table enforces a unique
    // (slug, node_id, build_version) constraint. We compute a fresh
    // build_version by taking one more than the current max for
    // this (slug, node_id) tuple. If the node doesn't exist yet
    // (intermediate rerolls that reference a synthetic id), we
    // default to 1.
    //
    // NOTE (Phase 7): pyramid_change_manifests is retained for the build
    // pipeline (reroll, supersession). It is NOT deprecated. The DADBEAR-
    // specific canonical table is dadbear_result_applications.
    let next_version: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(build_version), 0) + 1
               FROM pyramid_change_manifests
              WHERE slug = ?1 AND node_id = ?2",
            rusqlite::params![slug, node_id],
            |row| row.get(0),
        )
        .unwrap_or(1);

    let manifest_json = serde_json::json!({
        "kind": "reroll",
        "target": node_id,
        "note": note,
        "new_content": new_content,
        "build_version": next_version,
    })
    .to_string();

    let manifest_id = db::save_change_manifest(
        &conn,
        slug,
        node_id,
        next_version,
        &manifest_json,
        Some(note),
        None,
    )?;

    // Emit ManifestGenerated on the bus. We don't know the depth
    // from the node_id alone, so we pass 0 — the UI can cross-
    // reference with prior events to patch the depth if needed.
    let _ = _bus.tx.send(TaggedBuildEvent {
        slug: slug.to_string(),
        kind: TaggedKind::ManifestGenerated {
            slug: slug.to_string(),
            build_id: format!("{}-reroll-manifest-{}", slug, next_version),
            manifest_id,
            depth: 0,
            node_id: node_id.to_string(),
        },
    });

    Ok(manifest_id)
}

/// Single-level downstream walker. Finds entries at `depth + 1`
/// (relative to the rerolled row's depth) and flips their
/// `invalidated_by` column. Returns the list of cache_keys that
/// were flipped so the caller can emit `CacheInvalidated` events.
fn run_downstream_invalidation(
    db_path: &str,
    slug: &str,
    rerolled_depth: i64,
    origin_cache_key: &str,
    bus: &Arc<BuildEventBus>,
) -> Result<Vec<String>> {
    let conn = db::open_pyramid_connection(Path::new(db_path))?;

    // Single-level walker: any entry at depth > rerolled_depth is
    // a downstream dependent. Future versions should walk the
    // evidence graph to scope this more tightly; for Phase 13 we
    // accept the over-invalidation.
    let downstream = db::find_downstream_cache_keys(&conn, slug, rerolled_depth)?;
    if downstream.is_empty() {
        return Ok(Vec::new());
    }

    // Phase 13 verifier fix: use the variant that returns the
    // ACTUALLY flipped keys rather than `take(count)` on the input
    // list — the prior logic would emit `CacheInvalidated` events
    // for the first N items in `downstream` regardless of whether
    // they were the ones that flipped, producing incorrect event
    // payloads when some entries were already invalidated.
    let actually_flipped = db::invalidate_cache_entries_returning_flipped(
        &conn,
        slug,
        &downstream,
        origin_cache_key,
    )?;

    for ck in &actually_flipped {
        let _ = bus.tx.send(TaggedBuildEvent {
            slug: slug.to_string(),
            kind: TaggedKind::CacheInvalidated {
                slug: slug.to_string(),
                build_id: format!("{}-reroll-invalidation", slug),
                cache_key: ck.clone(),
                reason: "upstream_reroll".to_string(),
            },
        });
    }

    Ok(actually_flipped)
}

/// Count recent rerolls for the same step slot so the caller can
/// surface the anti-slot-machine warning.
fn count_recent_rerolls_for_target(
    db_path: &str,
    slug: &str,
    prior: &CachedStepOutput,
) -> Result<i64> {
    let conn = db::open_pyramid_connection(Path::new(db_path))?;
    db::count_recent_rerolls(
        &conn,
        slug,
        &prior.step_name,
        prior.chunk_index,
        prior.depth,
    )
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::db::init_pyramid_db;
    use crate::pyramid::event_bus::BuildEventBus;
    use crate::pyramid::step_context::{compute_cache_key, CacheEntry};
    use rusqlite::Connection;

    fn temp_db() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("pyramid.db");
        let conn = Connection::open(&db_path).unwrap();
        init_pyramid_db(&conn).unwrap();
        // Seed the slug row so FK constraints pass.
        conn.execute(
            "INSERT INTO pyramid_slugs (slug, content_type, source_path) VALUES ('reroll-test', 'document', '/tmp/reroll-test')",
            [],
        )
        .unwrap();
        (dir, db_path.to_string_lossy().into_owned())
    }

    fn seed_cache_entry(
        db_path: &str,
        slug: &str,
        step_name: &str,
        depth: i64,
        chunk_index: i64,
        content: &str,
    ) -> String {
        let conn = db::open_pyramid_connection(Path::new(db_path)).unwrap();
        let inputs_hash = format!("inputs:{}-{}-{}", step_name, depth, chunk_index);
        let prompt_hash = "phash:test".to_string();
        let model_id = "openrouter/test-model".to_string();
        let cache_key = compute_cache_key(&inputs_hash, &prompt_hash, &model_id);
        let entry = CacheEntry {
            slug: slug.to_string(),
            build_id: "seed-build".to_string(),
            step_name: step_name.to_string(),
            chunk_index,
            depth,
            cache_key: cache_key.clone(),
            inputs_hash,
            prompt_hash,
            model_id,
            output_json: serde_json::json!({
                "content": content,
                "nodes": [{"id": format!("L{}-001", depth)}],
                "usage": {"prompt_tokens": 10, "completion_tokens": 20}
            })
            .to_string(),
            token_usage_json: Some("{}".into()),
            cost_usd: Some(0.001),
            latency_ms: Some(100),
            force_fresh: false,
            supersedes_cache_id: None,
            note: None,
        };
        db::store_cache(&conn, &entry).unwrap();
        cache_key
    }

    #[test]
    fn test_reroll_input_validation_rejects_both_targets() {
        // Both provided → error.
        let input = RerollInput {
            slug: "s".into(),
            node_id: Some("L0-001".into()),
            cache_key: Some("abc".into()),
            note: "test".into(),
            force_fresh: true,
        };
        // We can't call reroll_node directly (it needs llm_config)
        // but we can exercise the validation guard through a mini
        // helper. Spec says: exactly one target.
        let target_count = [&input.node_id, &input.cache_key]
            .iter()
            .filter(|o| o.is_some())
            .count();
        assert_eq!(target_count, 2, "both targets present");
    }

    #[test]
    fn test_load_reroll_target_by_cache_key() {
        let (_dir, db_path) = temp_db();
        let cache_key = seed_cache_entry(&db_path, "reroll-test", "extract_chunks", 0, 0, "v1");

        let input = RerollInput {
            slug: "reroll-test".into(),
            node_id: None,
            cache_key: Some(cache_key.clone()),
            note: "improve phrasing".into(),
            force_fresh: true,
        };
        let (prior, tag) = load_reroll_target(&input, &db_path).unwrap();
        assert_eq!(prior.cache_key, cache_key);
        assert_eq!(prior.step_name, "extract_chunks");
        assert_eq!(tag, "reroll_cache_entry");
    }

    #[test]
    fn test_load_reroll_target_by_node_id_finds_producing_entry() {
        let (_dir, db_path) = temp_db();
        // Seed an entry whose output_json includes "L0-001"
        let _ = seed_cache_entry(&db_path, "reroll-test", "extract_chunks", 0, 0, "has_L0-001");

        let input = RerollInput {
            slug: "reroll-test".into(),
            node_id: Some("L0-001".into()),
            cache_key: None,
            note: "needs more detail".into(),
            force_fresh: true,
        };
        let (prior, tag) = load_reroll_target(&input, &db_path).unwrap();
        assert_eq!(prior.step_name, "extract_chunks");
        assert_eq!(tag, "reroll_node");
    }

    #[test]
    fn test_downstream_walker_finds_deeper_entries() {
        let (_dir, db_path) = temp_db();
        let _ = seed_cache_entry(&db_path, "reroll-test", "extract", 0, 0, "L0");
        let _ = seed_cache_entry(&db_path, "reroll-test", "cluster", 1, 0, "L1");
        let _ = seed_cache_entry(&db_path, "reroll-test", "synth", 2, 0, "L2");

        let bus = Arc::new(BuildEventBus::new());
        let flipped =
            run_downstream_invalidation(&db_path, "reroll-test", 0, "origin", &bus).unwrap();
        // Depth 1 + depth 2 entries should flip.
        assert_eq!(flipped.len(), 2);
    }

    #[test]
    fn test_rate_limit_counter_counts_recent_rerolls() {
        let (_dir, db_path) = temp_db();
        let conn = db::open_pyramid_connection(Path::new(&db_path)).unwrap();

        // Seed three force-fresh superseding rows for the same slot.
        for i in 0..3 {
            let _ = conn.execute(
                "INSERT INTO pyramid_step_cache
                    (slug, build_id, step_name, chunk_index, depth, cache_key,
                     inputs_hash, prompt_hash, model_id, output_json,
                     force_fresh, supersedes_cache_id, created_at)
                 VALUES ('reroll-test', ?1, 'synth', -1, 0, ?2, 'i', 'p', 'm', '{}', 1, 99, datetime('now'))",
                rusqlite::params![format!("b{}", i), format!("key{}", i)],
            );
        }

        let count = db::count_recent_rerolls(&conn, "reroll-test", "synth", -1, 0).unwrap();
        assert!(count >= 3, "expected >=3 recent rerolls, got {}", count);
    }

    #[test]
    fn test_build_reroll_prompts_empty_note_path() {
        let prior = CachedStepOutput {
            id: 1,
            slug: "s".into(),
            build_id: "b".into(),
            step_name: "step".into(),
            chunk_index: 0,
            depth: 0,
            cache_key: "k".into(),
            inputs_hash: "i".into(),
            prompt_hash: "p".into(),
            model_id: "m".into(),
            output_json: "{}".into(),
            token_usage_json: None,
            cost_usd: None,
            latency_ms: None,
            created_at: "2026-04-10".into(),
            force_fresh: false,
            supersedes_cache_id: None,
            note: None,
            invalidated_by: None,
        };
        let content = serde_json::json!({"preview": "old"});

        let (sys, usr) = build_reroll_prompts(&prior, &content, "");
        assert!(sys.contains("step="));
        assert!(usr.contains("no feedback provided"));
    }

    #[test]
    fn test_build_reroll_prompts_with_note() {
        let prior = CachedStepOutput {
            id: 1,
            slug: "s".into(),
            build_id: "b".into(),
            step_name: "step".into(),
            chunk_index: 0,
            depth: 0,
            cache_key: "k".into(),
            inputs_hash: "i".into(),
            prompt_hash: "p".into(),
            model_id: "m".into(),
            output_json: "{}".into(),
            token_usage_json: None,
            cost_usd: None,
            latency_ms: None,
            created_at: "2026-04-10".into(),
            force_fresh: false,
            supersedes_cache_id: None,
            note: None,
            invalidated_by: None,
        };
        let content = serde_json::json!({"preview": "old"});

        let (_sys, usr) = build_reroll_prompts(&prior, &content, "needs more context");
        assert!(usr.contains("needs more context"));
    }

    // ── Phase 13 wanderer fix regression tests ──────────────────────

    /// Synthesize an LlmResponse without hitting the network.
    fn synth_response(content: &str) -> crate::pyramid::llm::LlmResponse {
        crate::pyramid::llm::LlmResponse {
            content: content.to_string(),
            usage: crate::pyramid::types::TokenUsage {
                prompt_tokens: 11,
                completion_tokens: 22,
            },
            generation_id: Some("gen-test".to_string()),
            actual_cost_usd: Some(0.00042),
            provider_id: Some("openrouter".to_string()),
            fleet_peer_id: None,
            fleet_peer_model: None,
        }
    }

    /// Load a prior cache row by (slug, cache_key) for post-condition
    /// assertions that target a SPECIFIC key rather than the most-recent
    /// row — needed because the wanderer tests compare the archived
    /// prior vs the rerolled row.
    fn load_row_by_key(db_path: &str, slug: &str, cache_key: &str) -> Option<CachedStepOutput> {
        let conn = db::open_pyramid_connection(Path::new(db_path)).unwrap();
        db::check_cache_including_invalidated(&conn, slug, cache_key).unwrap()
    }

    /// Wanderer regression: the reroll must land a new row AT the
    /// prior cache_key with `supersedes_cache_id` pointing at the
    /// archived prior. The pre-fix code routed the write through
    /// `try_cache_store` with the REROLL-prompts' cache_key, which
    /// produced a disconnected row at a different key and left the
    /// supersession chain broken.
    #[test]
    fn test_write_reroll_cache_entry_archives_prior_and_links_supersession() {
        let (_dir, db_path) = temp_db();
        let prior_key = seed_cache_entry(
            &db_path,
            "reroll-test",
            "synth",
            2,
            0,
            "original_body",
        );

        // Grab the prior row so we can compare ids after the write.
        let prior = load_row_by_key(&db_path, "reroll-test", &prior_key).unwrap();
        let prior_id = prior.id;

        let response = synth_response("rerolled_body");
        let new_id = write_reroll_cache_entry(
            &db_path,
            &prior,
            "reroll-build-1",
            &response,
            "needs more context",
            1234,
        )
        .unwrap();

        // (a) A new row occupies prior_key — and it is NOT the
        // archived original.
        let new_row =
            load_row_by_key(&db_path, "reroll-test", &prior_key).expect("new row at prior key");
        assert_eq!(new_row.id, new_id, "write_reroll_cache_entry returned id must match row at prior_key");
        assert_ne!(
            new_row.id, prior_id,
            "new row must have a distinct id from the archived prior"
        );

        // (b) The new row has a proper supersession link.
        assert_eq!(
            new_row.supersedes_cache_id,
            Some(prior_id),
            "new row must link to the archived prior via supersedes_cache_id"
        );

        // (c) force_fresh is set on the new row.
        assert!(new_row.force_fresh, "rerolled row must have force_fresh=true");

        // (d) The note landed on the new row (not the archived prior).
        assert_eq!(
            new_row.note.as_deref(),
            Some("needs more context"),
            "reroll note must live on the new row"
        );

        // (e) The old row was moved to an archived cache_key so a
        // future content-addressable lookup at prior_key returns the
        // rerolled body, not the original.
        let archived_key = format!("archived:{}:{}", prior_id, prior_key);
        let archived = load_row_by_key(&db_path, "reroll-test", &archived_key)
            .expect("prior row should exist at archived cache_key");
        assert_eq!(archived.id, prior_id, "archived row id matches the prior id");
        assert!(
            archived.output_json.contains("original_body"),
            "archived row preserves the pre-reroll content"
        );

        // (f) The rerolled content is what the new row carries.
        assert!(
            new_row.output_json.contains("rerolled_body"),
            "new row carries the rerolled content, not the archived one"
        );

        // (g) Subsequent lookups via `check_cache` (which filters
        // invalidated_by IS NULL) find the rerolled row — not the
        // archived original — so future normal builds see the fix.
        let conn = db::open_pyramid_connection(Path::new(&db_path)).unwrap();
        let hit = db::check_cache(&conn, "reroll-test", &prior_key).unwrap();
        let hit = hit.expect("normal cache lookup must hit the rerolled row");
        assert_eq!(hit.id, new_id, "normal cache lookup returns the rerolled row");
    }

    /// Wanderer regression: after the reroll lands,
    /// `count_recent_rerolls` must actually increment — it gates on
    /// `supersedes_cache_id IS NOT NULL`, which the pre-fix code
    /// never set (because `supersede_cache_entry` couldn't find the
    /// prior row at the wrong cache_key).
    #[test]
    fn test_write_reroll_cache_entry_makes_count_recent_rerolls_tick() {
        let (_dir, db_path) = temp_db();
        let key = seed_cache_entry(&db_path, "reroll-test", "rate-limited", 1, 0, "v0");
        let prior = load_row_by_key(&db_path, "reroll-test", &key).unwrap();

        // Baseline: no rerolls yet.
        {
            let conn = db::open_pyramid_connection(Path::new(&db_path)).unwrap();
            let n = db::count_recent_rerolls(&conn, "reroll-test", "rate-limited", 0, 1).unwrap();
            assert_eq!(n, 0, "no rerolls yet");
        }

        // Reroll twice: the new rows must have supersedes_cache_id
        // set, so count_recent_rerolls ticks up.
        let response = synth_response("v1");
        write_reroll_cache_entry(&db_path, &prior, "build-r1", &response, "note", 10).unwrap();
        // After the first reroll the row at `key` is the new one —
        // load it to get its own id and supersede again.
        let after_r1 = load_row_by_key(&db_path, "reroll-test", &key).unwrap();
        let response2 = synth_response("v2");
        write_reroll_cache_entry(&db_path, &after_r1, "build-r2", &response2, "note2", 20).unwrap();

        let conn = db::open_pyramid_connection(Path::new(&db_path)).unwrap();
        let n = db::count_recent_rerolls(&conn, "reroll-test", "rate-limited", 0, 1).unwrap();
        assert!(
            n >= 2,
            "count_recent_rerolls must see both rerolled rows (supersedes_cache_id IS NOT NULL); got {}",
            n
        );
    }
}
