// pyramid/local_mode.rs — Phase 18a: Local Mode toggle implementation.
//
// Per `docs/specs/provider-registry.md` §382-395 and ledger entries
// L1/L5 in `docs/plans/deferral-ledger.md`. The Local Mode toggle is
// the user-facing single switch that says "route every model tier
// through a local Ollama instance instead of OpenRouter".
//
// Enable flow:
//   1. Validate base_url + reachability check via `GET /api/tags`.
//   2. Auto-pick the first model from the `/api/tags` list when the
//      caller didn't supply one.
//   3. Auto-detect the model's context window via `/api/show`.
//   4. UPSERT an `ollama-local` row in `pyramid_providers`.
//   5. Snapshot the active `tier_routing` + `build_strategy`
//      contribution_ids into `pyramid_local_mode_state`.
//   6. Build a new `tier_routing` YAML where every prior tier name
//      points at `ollama-local` + the selected model + detected
//      context limit, then supersede the active tier_routing
//      contribution. The dispatcher's `tier_routing` branch
//      runs `upsert_tier_routing_from_contribution` which now also
//      DELETEs stale tier rows.
//   7. Build a new `build_strategy` YAML that forces concurrency to 1
//      on both `initial_build` and `maintenance` (per spec §391:
//      "set concurrency to 1 — home hardware constraint"), then
//      supersede the active build_strategy contribution.
//
// Disable flow:
//   1. Read `pyramid_local_mode_state`. If `enabled = false`, no-op.
//   2. Look up the saved `restore_from_contribution_id` /
//      `restore_build_strategy_contribution_id`. For each that
//      still exists, COPY its `yaml_content` into a new "restore"
//      supersession of the currently-active local-mode contribution
//      with `triggering_note = "local mode disabled — restoring …"`.
//   3. Flip `enabled = false` in the state row but keep the
//      `ollama_base_url` / `ollama_model` so the next enable starts
//      from the user's last picks.
//
// Status flow:
//   1. Read the state row.
//   2. If enabled, refresh the `available_models` list from
//      `/api/tags` and update `reachable` via the same call. The
//      reachability error string is exposed as a separate field so
//      the UI can render a clear "cannot reach Ollama" message.
//
// Reversibility is the load-bearing property: a half-restored state
// where the provider row exists but the tier_routing was reset to
// defaults is a bug. The state row's two restore columns plus the
// supersession history together form the rollback chain.

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

use crate::pyramid::config_contributions::{
    load_active_config_contribution, load_contribution_by_id, supersede_config_contribution,
    sync_config_to_operational,
};
use crate::pyramid::db::{
    self, load_local_mode_state, save_local_mode_state, save_provider, LocalModeStateRow,
    TierRoutingYaml, TierRoutingYamlEntry,
};
use crate::pyramid::event_bus::BuildEventBus;
use crate::pyramid::provider::{Provider, ProviderRegistry, ProviderType};

/// Conventional id for the bundled Ollama-local provider row.
pub const OLLAMA_LOCAL_PROVIDER_ID: &str = "ollama-local";

/// Default fallback context limit when `/api/show` doesn't return one
/// (or when the user is targeting a model the detector can't parse).
/// Documented in spec §388 as "fall back to user-specified context
/// limit with a warning"; we use a conservative 32k floor so the
/// dehydration paths still have headroom.
pub const DEFAULT_OLLAMA_CONTEXT_FALLBACK: usize = 32_000;

// ── IPC payload types ───────────────────────────────────────────────────────

/// Status snapshot returned by every Local Mode IPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalModeStatus {
    pub enabled: bool,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub detected_context_limit: Option<usize>,
    /// Models reported by `GET /api/tags` on the most recent
    /// reachability check. Empty when the call failed or the user
    /// hasn't enabled local mode yet.
    pub available_models: Vec<String>,
    pub reachable: bool,
    pub reachability_error: Option<String>,
    pub ollama_provider_id: String,
    pub prior_tier_routing_contribution_id: Option<String>,
    pub prior_build_strategy_contribution_id: Option<String>,
}

impl LocalModeStatus {
    /// Build a "disabled, never enabled" status row. Used as the
    /// initial state on first boot.
    pub fn disabled_default() -> Self {
        Self {
            enabled: false,
            base_url: None,
            model: None,
            detected_context_limit: None,
            available_models: Vec::new(),
            reachable: false,
            reachability_error: None,
            ollama_provider_id: OLLAMA_LOCAL_PROVIDER_ID.to_string(),
            prior_tier_routing_contribution_id: None,
            prior_build_strategy_contribution_id: None,
        }
    }
}

/// Result of a one-shot probe (no DB writes). Used by the
/// `pyramid_probe_ollama` IPC so the user can "test connection" from
/// the disabled state and see available models before committing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaProbeResult {
    pub reachable: bool,
    pub reachability_error: Option<String>,
    pub available_models: Vec<String>,
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Validate that the URL starts with `http://` or `https://`. Returns
/// the trimmed string on success.
pub fn normalize_base_url(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("Ollama base URL must not be empty");
    }
    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        bail!(
            "Ollama base URL must start with http:// or https:// (got {trimmed:?}). \
             Did you mean http://localhost:11434/v1?"
        );
    }
    Ok(trimmed.trim_end_matches('/').to_string())
}

/// Strip the `/v1` suffix from a base URL so we can hit Ollama's
/// native paths (`/api/tags`, `/api/show`). The OpenAI-compat path
/// is at `{base}/chat/completions`; the native path is one level up.
pub fn native_root_for(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    trimmed
        .strip_suffix("/v1")
        .map(|s| s.to_string())
        .unwrap_or_else(|| trimmed.to_string())
}

/// Probe `GET {base}/api/tags`. Returns the parsed model name list on
/// success or a clear error otherwise. Caller decides how to surface.
pub async fn fetch_ollama_models(base_url: &str) -> Result<Vec<String>> {
    let native = native_root_for(base_url);
    let url = format!("{native}/api/tags");
    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .with_context(|| format!("GET {url} failed (is Ollama running?)"))?;
    if !response.status().is_success() {
        bail!("GET {url} returned status {}", response.status());
    }
    let body: serde_json::Value = response
        .json()
        .await
        .with_context(|| format!("parsing /api/tags response from {url}"))?;
    Ok(parse_tags_response(&body))
}

/// Parse Ollama's `/api/tags` response shape into a sorted, unique
/// list of model names. The shape is `{ "models": [{ "name": "x" }, …] }`
/// per the Ollama HTTP API. Returns an empty list when the structure
/// is missing or malformed — never panics.
pub fn parse_tags_response(body: &serde_json::Value) -> Vec<String> {
    let mut out = std::collections::BTreeSet::new();
    let Some(models) = body.get("models").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    for entry in models {
        if let Some(name) = entry.get("name").and_then(|v| v.as_str()) {
            if !name.trim().is_empty() {
                out.insert(name.to_string());
            }
        }
    }
    out.into_iter().collect()
}

/// Probe `GET {base}/api/tags` and return both the reachability state
/// and the model list in a single round trip. Wraps
/// `fetch_ollama_models` in a probe-shaped result so callers can
/// surface "test connection" output without two separate calls.
pub async fn probe_ollama(base_url: &str) -> OllamaProbeResult {
    match fetch_ollama_models(base_url).await {
        Ok(models) => OllamaProbeResult {
            reachable: true,
            reachability_error: None,
            available_models: models,
        },
        Err(err) => OllamaProbeResult {
            reachable: false,
            reachability_error: Some(err.to_string()),
            available_models: Vec::new(),
        },
    }
}

/// Auto-detect a model's context window via Ollama's `/api/show`.
/// Returns `None` when the call fails or the response doesn't expose
/// a recognizable `*.context_length` key.
pub async fn detect_ollama_context_window(base_url: &str, model: &str) -> Option<usize> {
    let native = native_root_for(base_url);
    let url = format!("{native}/api/show");
    let client = reqwest::Client::new();
    let body = serde_json::json!({ "model": model });
    let resp = client
        .post(&url)
        .json(&body)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: serde_json::Value = resp.json().await.ok()?;
    crate::pyramid::provider::parse_ollama_context_length(&v)
}

// ── Status read ─────────────────────────────────────────────────────────────

/// Synchronous DB-only fetch: snapshot the state row into a partial
/// `LocalModeStatus`. When enabled, the caller is expected to follow
/// up with `refresh_status_reachability` (which does the Ollama
/// `/api/tags` probe) AFTER dropping the rusqlite lock, so a 5-second
/// network round trip never holds the reader mutex against other
/// concurrent IPCs. This split was the wanderer fix: the old
/// `get_local_mode_status(&Connection)` held the reader lock across
/// `probe_ollama().await`, blocking every other reader-bound IPC for
/// the duration of the probe.
pub fn load_status_snapshot(conn: &Connection) -> Result<LocalModeStatus> {
    let row = load_local_mode_state(conn)?;

    Ok(LocalModeStatus {
        enabled: row.enabled,
        base_url: row.ollama_base_url,
        model: row.ollama_model,
        detected_context_limit: row.detected_context_limit.map(|n| n as usize),
        available_models: Vec::new(),
        // `reachable` starts false; the probe-refresh step upgrades it
        // to true only when the probe succeeds. The UI distinguishes
        // "enabled + unreachable" (red X) from "disabled" (grey).
        reachable: false,
        reachability_error: None,
        ollama_provider_id: OLLAMA_LOCAL_PROVIDER_ID.to_string(),
        prior_tier_routing_contribution_id: row.restore_from_contribution_id,
        prior_build_strategy_contribution_id: row.restore_build_strategy_contribution_id,
    })
}

/// Async follow-up: if the status is enabled, probe Ollama (WITHOUT
/// holding any rusqlite lock) and merge the reachability +
/// `available_models` fields into the snapshot. No-op when disabled.
/// Callers who hold a writer lock (enable_local_mode / disable end
/// return) can skip calling this — the probe data isn't load-bearing
/// when the caller already just wrote the routing rows.
pub async fn refresh_status_reachability(mut status: LocalModeStatus) -> LocalModeStatus {
    if !status.enabled {
        return status;
    }
    let base_url = status
        .base_url
        .clone()
        .unwrap_or_else(|| "http://localhost:11434/v1".to_string());
    let probe = probe_ollama(&base_url).await;
    status.base_url = Some(base_url);
    status.available_models = probe.available_models;
    status.reachable = probe.reachable;
    status.reachability_error = probe.reachability_error;
    status
}

/// Read the current `pyramid_local_mode_state` row and (when enabled)
/// refresh the reachability + available_models fields. The status
/// snapshot is rebuilt fresh on every call so the UI sees the actual
/// state of the host machine, not a cached value.
///
/// **Warning:** this function holds the caller's `&Connection` across
/// the `probe_ollama().await`. For the enable/disable return paths
/// that's acceptable (they're inside a writer lock that already spans
/// the whole operation). For the `pyramid_get_local_mode_status` IPC
/// handler, use `load_status_snapshot` + `refresh_status_reachability`
/// with an explicit lock drop in between so a 5-second probe doesn't
/// block every concurrent reader-bound IPC.
pub async fn get_local_mode_status(conn: &Connection) -> Result<LocalModeStatus> {
    let snapshot = load_status_snapshot(conn)?;
    Ok(refresh_status_reachability(snapshot).await)
}

// ── Enable ──────────────────────────────────────────────────────────────────

/// Plan produced by the async prepare phase. Captures everything the
/// sync commit phase needs to write rows without touching the wire.
///
/// Split out by the Phase 18a fix-pass after the build failed with
/// Send errors on `pyramid_enable_local_mode`: holding a
/// `&mut Connection` across `probe_ollama().await` + `detect_ollama_context_window().await`
/// inside an async Tauri command handler makes the enclosing future
/// `!Send` because `rusqlite::Connection` is `!Sync`. The binary's
/// Tauri runtime is multi-threaded, so command futures MUST be Send.
/// `cargo check --lib` does not catch this — only the binary crate
/// elaborates the command futures.
///
/// Fix: keep every `.await` in `prepare_enable_local_mode` (which
/// never touches the DB) and do every DB write in
/// `commit_enable_local_mode` (which is plain `fn`, no async). The
/// IPC handler threads them: first await the prepare, THEN take the
/// writer lock, THEN call commit synchronously, THEN drop the lock.
#[derive(Debug, Clone)]
pub struct EnableLocalModePlan {
    pub base_url: String,
    pub chosen_model: String,
    pub detected_context: usize,
    /// Full list of models reported by `/api/tags`. Carried forward so
    /// the returned status can show the user the other models they
    /// could switch to without re-probing.
    pub available_models: Vec<String>,
}

/// Async prepare phase: validate URL, probe Ollama, pick a model,
/// detect the context window. No DB, no lock — safe to call from any
/// async context. The result is a `EnableLocalModePlan` ready to be
/// committed under the writer lock by `commit_enable_local_mode`.
///
/// Returns an error if Ollama is not reachable, has no models, or the
/// caller's `model_override` doesn't exist on the server.
pub async fn prepare_enable_local_mode(
    base_url_raw: String,
    model_override: Option<String>,
) -> Result<EnableLocalModePlan> {
    // Step 1: validate the URL.
    let base_url = normalize_base_url(&base_url_raw)?;

    // Step 2: reachability + model list. Refuse to half-enable.
    let probe = probe_ollama(&base_url).await;
    if !probe.reachable {
        return Err(anyhow!(
            "Cannot reach Ollama at {base_url}: {}. Start `ollama serve` and try again.",
            probe
                .reachability_error
                .unwrap_or_else(|| "unknown error".to_string())
        ));
    }
    if probe.available_models.is_empty() {
        return Err(anyhow!(
            "Ollama at {base_url} is reachable but reported no installed models. \
             Run `ollama pull <model>` first, then retry."
        ));
    }

    // Step 3: pick a model. Sort the list deterministically so the
    // auto-pick is stable across reboots.
    let mut sorted_models = probe.available_models.clone();
    sorted_models.sort();
    let chosen_model = match model_override {
        Some(m) if !m.trim().is_empty() => {
            if !sorted_models.iter().any(|x| x == &m) {
                return Err(anyhow!(
                    "Ollama at {base_url} does not have a model named `{m}`. \
                     Available: {}",
                    sorted_models.join(", ")
                ));
            }
            m
        }
        _ => sorted_models
            .first()
            .cloned()
            .ok_or_else(|| anyhow!("no Ollama models found"))?,
    };

    // Step 4: detect context window. Fall back to the conservative
    // floor on failure (logged at debug elsewhere).
    let detected_context = detect_ollama_context_window(&base_url, &chosen_model)
        .await
        .unwrap_or(DEFAULT_OLLAMA_CONTEXT_FALLBACK);

    Ok(EnableLocalModePlan {
        base_url,
        chosen_model,
        detected_context,
        available_models: sorted_models,
    })
}

/// Enable Local Mode end to end. See module docs for the full
/// sequence. Returns the post-enable status snapshot.
///
/// Per the spec, this MUST be reversible: the disable path needs the
/// pre-enable contribution_ids stored in
/// `pyramid_local_mode_state.restore_from_contribution_id` /
/// `restore_build_strategy_contribution_id`. Both columns are
/// populated before the new contributions are inserted so a crash
/// between snapshot and supersession can be recovered manually
/// (though the supersession itself is atomic per the underlying
/// `supersede_config_contribution` transaction).
///
/// **Phase 18a fix-pass:** kept as a thin async wrapper around
/// `prepare_enable_local_mode` + `commit_enable_local_mode` for
/// backwards compatibility with existing tests. New code (Tauri
/// command handlers that must keep their future `Send`) should call
/// the two-phase API directly — see `pyramid_enable_local_mode` in
/// `main.rs`.
pub async fn enable_local_mode(
    conn: &mut Connection,
    bus: &Arc<BuildEventBus>,
    registry: &ProviderRegistry,
    base_url_raw: String,
    model_override: Option<String>,
) -> Result<LocalModeStatus> {
    let plan = prepare_enable_local_mode(base_url_raw, model_override).await?;
    commit_enable_local_mode(conn, bus, registry, plan)?;
    // Return the sync snapshot; the caller can refresh reachability
    // outside any lock if they want to re-probe.
    load_status_snapshot(conn)
}

/// Sync commit phase: take the plan produced by
/// `prepare_enable_local_mode` and write every row. This is plain
/// `fn`, not `async fn`, so the caller's Tauri command future stays
/// `Send` even while holding a `&mut Connection` across the call.
///
/// All DB work (provider upsert, state snapshot, tier_routing
/// supersession, build_strategy supersession) runs here.
pub fn commit_enable_local_mode(
    conn: &mut Connection,
    bus: &Arc<BuildEventBus>,
    registry: &ProviderRegistry,
    plan: EnableLocalModePlan,
) -> Result<()> {
    let EnableLocalModePlan {
        base_url,
        chosen_model,
        detected_context,
        available_models: _,
    } = plan;

    // Step 5: upsert the `ollama-local` provider row.
    let provider = Provider {
        id: OLLAMA_LOCAL_PROVIDER_ID.to_string(),
        display_name: "Ollama (local)".to_string(),
        provider_type: ProviderType::OpenaiCompat,
        base_url: base_url.clone(),
        api_key_ref: None,
        auto_detect_context: true,
        supports_broadcast: false,
        broadcast_config_json: None,
        config_json: "{}".to_string(),
        enabled: true,
    };
    save_provider(conn, &provider)?;
    // Refresh the in-memory registry so subsequent reads see the new
    // provider row without waiting for a process restart.
    registry.load_from_db(conn)?;

    // Step 6: snapshot the prior active tier_routing + build_strategy
    // contributions before we supersede them.
    let prior_tier_contribution =
        load_active_config_contribution(conn, "tier_routing", None)?
            .ok_or_else(|| {
                anyhow!(
                    "no active tier_routing contribution to supersede; \
                     bundled defaults should have created one on first boot"
                )
            })?;
    let prior_build_strategy_contribution =
        load_active_config_contribution(conn, "build_strategy", None)?;

    // Step 7: synthesize a new tier_routing YAML that copies every
    // existing tier name and re-points it at ollama-local + the
    // selected model. We carry over the tier names from the prior
    // contribution so chain steps that ask for `web` / `synth_heavy`
    // / etc. don't hit "tier not defined" errors.
    let prior_tier_yaml: TierRoutingYaml =
        serde_yaml::from_str(&prior_tier_contribution.yaml_content)
            .with_context(|| {
                format!(
                    "parsing prior tier_routing contribution {}",
                    prior_tier_contribution.contribution_id
                )
            })?;
    let mut prior_tier_names: std::collections::BTreeSet<String> =
        prior_tier_yaml.entries.iter().map(|e| e.tier_name.clone()).collect();
    // Always include the standard chain tiers so a chain step asking
    // for one of them (even if the prior contribution didn't list it)
    // still resolves cleanly. The prior contribution may have been
    // edited to drop tiers; we don't want to break a build.
    for required in [
        "fast_extract",
        "web",
        "synth_heavy",
        "stale_remote",
        "stale_local",
        "mid",        // code.yaml, document.yaml default tier
        "extractor",  // conversation chain extraction tier
        "high",       // cascade fallback tier (large context)
        "max",        // cascade fallback tier (maximum context)
    ] {
        prior_tier_names.insert(required.to_string());
    }

    let new_entries: Vec<TierRoutingYamlEntry> = prior_tier_names
        .into_iter()
        .map(|tier_name| TierRoutingYamlEntry {
            tier_name,
            provider_id: OLLAMA_LOCAL_PROVIDER_ID.to_string(),
            model_id: chosen_model.clone(),
            context_limit: Some(detected_context as i64),
            max_completion_tokens: None,
            pricing_json: Some("{}".to_string()),
            supported_parameters_json: Some(r#"["response_format"]"#.to_string()),
            notes: Some(format!(
                "local mode — routed via Ollama at {base_url}"
            )),
            priority: Some(1),
            prompt_price_per_token: Some(0.0),
            completion_price_per_token: Some(0.0),
        })
        .collect();
    let new_tier_yaml = TierRoutingYaml { entries: new_entries };
    let new_tier_yaml_string = build_tier_routing_yaml_string(&new_tier_yaml)?;

    // Persist the prior contribution_ids in the state row BEFORE we
    // supersede so a crash between the two writes doesn't leave us
    // unable to restore. The state row is a single UPSERT so the
    // post-condition is also atomic.
    save_local_mode_state(
        conn,
        &LocalModeStateRow {
            enabled: true,
            ollama_base_url: Some(base_url.clone()),
            ollama_model: Some(chosen_model.clone()),
            detected_context_limit: Some(detected_context as i64),
            restore_from_contribution_id: Some(prior_tier_contribution.contribution_id.clone()),
            restore_build_strategy_contribution_id: prior_build_strategy_contribution
                .as_ref()
                .map(|c| c.contribution_id.clone()),
            updated_at: String::new(),
        },
    )?;

    // Step 8: supersede the active tier_routing contribution. The
    // dispatcher's `tier_routing` branch runs the upsert helper that
    // (Phase 18a) DELETEs stale tier rows + INSERTs the new ones,
    // and refreshes the registry cache.
    let new_tier_contribution_id = supersede_config_contribution(
        conn,
        &prior_tier_contribution.contribution_id,
        &new_tier_yaml_string,
        "local mode enabled",
        "local_mode_toggle",
        Some("user"),
    )?;
    // Sync immediately so the operational table picks up the new
    // routing without waiting for the next builder pass.
    let new_tier_contribution = load_contribution_by_id(conn, &new_tier_contribution_id)?
        .ok_or_else(|| anyhow!("local-mode tier contribution disappeared after supersede"))?;
    sync_config_to_operational(conn, bus, &new_tier_contribution)?;
    // Refresh the in-memory tier registry too.
    registry.load_from_db(conn)?;

    // Step 9: supersede the active build_strategy contribution to
    // pin concurrency = 1 (spec §391 — home hardware constraint).
    if let Some(prior_bs) = prior_build_strategy_contribution {
        let new_bs_yaml = build_local_mode_build_strategy_yaml(&prior_bs.yaml_content)?;
        let new_bs_id = supersede_config_contribution(
            conn,
            &prior_bs.contribution_id,
            &new_bs_yaml,
            "local mode enabled — concurrency 1",
            "local_mode_toggle",
            Some("user"),
        )?;
        let new_bs_contribution = load_contribution_by_id(conn, &new_bs_id)?
            .ok_or_else(|| anyhow!("local-mode build_strategy contribution missing after supersede"))?;
        sync_config_to_operational(conn, bus, &new_bs_contribution)?;
    } else {
        // No prior build_strategy contribution? Create a fresh active
        // one carrying just the concurrency floor. This case is
        // unlikely (bundled defaults seed it on first boot) but
        // covered for safety.
        let yaml = "schema_type: build_strategy\n\
                    initial_build:\n  concurrency: 1\nmaintenance:\n  concurrency: 1\n";
        let new_id = crate::pyramid::config_contributions::create_config_contribution(
            conn,
            "build_strategy",
            None,
            yaml,
            Some("local mode enabled — concurrency 1 (no prior contribution)"),
            "local_mode_toggle",
            Some("user"),
            "active",
        )?;
        let contribution = load_contribution_by_id(conn, &new_id)?
            .ok_or_else(|| anyhow!("local-mode build_strategy contribution missing"))?;
        sync_config_to_operational(conn, bus, &contribution)?;
    }

    // Phase 18a deferred: spec §390 calls for deriving dehydration
    // budgets from the detected context limit. The relevant constants
    // live in `OperationalConfig::tier2` (`pre_map_prompt_budget`,
    // `answer_prompt_budget`) and are not currently surfaced as a
    // contribution, so scaling them requires either threading a
    // mutable handle into this module or introducing a new
    // contribution schema_type. Both are beyond Phase 18a scope. The
    // local mode toggle still works against the default budgets;
    // users running tiny-context models may need to manually drop
    // those values via a future Phase 19 dehydration_budget
    // contribution. See `docs/plans/pyramid-folders-model-routing-friction-log.md`
    // for the carry-forward note.

    // Step 10: commit is done; caller rebuilds the status snapshot
    // via `load_status_snapshot` (sync) or `get_local_mode_status`
    // (async) as they see fit.
    Ok(())
}

/// Build a `tier_routing` YAML string from a `TierRoutingYaml` value
/// using `serde_yaml`. Adds a leading `schema_type: tier_routing`
/// stanza so the contribution roundtrips through the dispatcher's
/// schema-aware loaders.
fn build_tier_routing_yaml_string(yaml: &TierRoutingYaml) -> Result<String> {
    // We can't just `serde_yaml::to_string(yaml)` because the field
    // name in the canonical schema is `entries` (which we now match)
    // but we also need the `schema_type:` line at the top per the
    // bundled JSON Schema. Build a wrapper map.
    let mut root = serde_yaml::Mapping::new();
    root.insert(
        serde_yaml::Value::String("schema_type".into()),
        serde_yaml::Value::String("tier_routing".into()),
    );
    let entries_yaml = serde_yaml::to_value(&yaml.entries)
        .context("serializing tier_routing entries")?;
    root.insert(
        serde_yaml::Value::String("entries".into()),
        entries_yaml,
    );
    let value = serde_yaml::Value::Mapping(root);
    serde_yaml::to_string(&value).context("rendering tier_routing YAML")
}

/// Build a `build_strategy` YAML string that takes the prior
/// contribution's YAML and forces concurrency to 1 on both phases.
/// Preserves every other field so a user who customized
/// `evidence_mode` / `webbing` / `quality.*` keeps their tuning.
fn build_local_mode_build_strategy_yaml(prior_yaml: &str) -> Result<String> {
    let mut value: serde_yaml::Value = serde_yaml::from_str(prior_yaml)
        .context("parsing prior build_strategy YAML")?;
    let map = value
        .as_mapping_mut()
        .ok_or_else(|| anyhow!("prior build_strategy YAML is not a mapping"))?;
    // Ensure schema_type is present.
    map.insert(
        serde_yaml::Value::String("schema_type".into()),
        serde_yaml::Value::String("build_strategy".into()),
    );
    for phase in ["initial_build", "maintenance"] {
        let key = serde_yaml::Value::String(phase.into());
        let phase_map = map
            .entry(key.clone())
            .or_insert_with(|| serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
        if let Some(phase_map) = phase_map.as_mapping_mut() {
            phase_map.insert(
                serde_yaml::Value::String("concurrency".into()),
                serde_yaml::Value::Number(serde_yaml::Number::from(1u64)),
            );
        }
    }
    serde_yaml::to_string(&value).context("rendering build_strategy YAML")
}

// ── Disable ─────────────────────────────────────────────────────────────────

/// Disable Local Mode end to end. Restores the prior tier_routing
/// and build_strategy contributions verbatim. Idempotent — calling
/// when `enabled = false` returns the current status unchanged.
///
/// **Phase 18a fix-pass:** kept as a thin async wrapper around
/// `commit_disable_local_mode` for backwards compatibility. New
/// code (Tauri command handlers) should call the sync variant
/// directly to avoid the Send-check trap where holding a
/// `&mut Connection` across `.await` in an async command makes the
/// enclosing future `!Send`.
pub async fn disable_local_mode(
    conn: &mut Connection,
    bus: &Arc<BuildEventBus>,
    registry: &ProviderRegistry,
) -> Result<LocalModeStatus> {
    commit_disable_local_mode(conn, bus, registry)?;
    let snapshot = load_status_snapshot(conn)?;
    Ok(refresh_status_reachability(snapshot).await)
}

/// Sync disable: performs the restoration writes under a caller-
/// held writer lock, returns nothing. Callers rebuild the status
/// snapshot via `load_status_snapshot` (sync) or
/// `get_local_mode_status` (async) after releasing the lock.
///
/// Idempotent: if `enabled = false`, it's a no-op and returns `Ok(())`.
pub fn commit_disable_local_mode(
    conn: &mut Connection,
    bus: &Arc<BuildEventBus>,
    registry: &ProviderRegistry,
) -> Result<()> {
    let row = load_local_mode_state(conn)?;
    if !row.enabled {
        return Ok(());
    }

    // Restore tier_routing first.
    if let Some(restore_id) = row.restore_from_contribution_id.as_deref() {
        if let Some(restore) = load_contribution_by_id(conn, restore_id)? {
            // Find the currently-active tier_routing contribution and
            // supersede it with a copy of the saved YAML. Skip the
            // restore when there isn't an active one (defensive — the
            // dispatcher should always leave one behind).
            if let Some(active_now) =
                load_active_config_contribution(conn, "tier_routing", None)?
            {
                let new_id = supersede_config_contribution(
                    conn,
                    &active_now.contribution_id,
                    &restore.yaml_content,
                    "local mode disabled — restoring prior tier_routing",
                    "local_mode_toggle",
                    Some("user"),
                )?;
                let new_contribution = load_contribution_by_id(conn, &new_id)?.ok_or_else(|| {
                    anyhow!("restored tier_routing contribution missing after supersede")
                })?;
                sync_config_to_operational(conn, bus, &new_contribution)?;
            }
        }
        // If the original contribution was deleted: log + skip.
        // The active local-mode contribution remains in place.
    }

    // Restore build_strategy.
    if let Some(restore_id) = row.restore_build_strategy_contribution_id.as_deref() {
        if let Some(restore) = load_contribution_by_id(conn, restore_id)? {
            if let Some(active_now) =
                load_active_config_contribution(conn, "build_strategy", None)?
            {
                let new_id = supersede_config_contribution(
                    conn,
                    &active_now.contribution_id,
                    &restore.yaml_content,
                    "local mode disabled — restoring prior build_strategy",
                    "local_mode_toggle",
                    Some("user"),
                )?;
                let new_contribution = load_contribution_by_id(conn, &new_id)?.ok_or_else(|| {
                    anyhow!("restored build_strategy contribution missing after supersede")
                })?;
                sync_config_to_operational(conn, bus, &new_contribution)?;
            }
        }
    }

    // Disable the local provider so active_provider_id() falls back to
    // openrouter. Without this, the provider row stays enabled and all
    // LLM calls continue routing to Ollama after the user toggles off.
    if let Ok(Some(mut local_provider)) =
        super::db::get_provider(conn, OLLAMA_LOCAL_PROVIDER_ID)
    {
        local_provider.enabled = false;
        super::db::save_provider(conn, &local_provider)?;
    }

    // Refresh the registry cache so subsequent resolves pick up the
    // restored tier rows + disabled provider.
    registry.load_from_db(conn)?;

    // Flip the state row to disabled but keep the URL/model so the
    // next enable starts from the user's last picks. Clear the
    // restore IDs because they no longer apply to the current state.
    save_local_mode_state(
        conn,
        &LocalModeStateRow {
            enabled: false,
            ollama_base_url: row.ollama_base_url.clone(),
            ollama_model: row.ollama_model.clone(),
            detected_context_limit: row.detected_context_limit,
            restore_from_contribution_id: None,
            restore_build_strategy_contribution_id: None,
            updated_at: String::new(),
        },
    )?;

    Ok(())
}

// Marker re-exports so the helper API stays inside this module's
// namespace from the IPC layer's perspective.
pub use db::load_local_mode_state as read_state;
pub use db::save_local_mode_state as write_state;

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalize_base_url_strips_trailing_slash() {
        assert_eq!(
            normalize_base_url("http://localhost:11434/v1/").unwrap(),
            "http://localhost:11434/v1"
        );
        assert_eq!(
            normalize_base_url("  http://localhost:11434/v1  ").unwrap(),
            "http://localhost:11434/v1"
        );
    }

    #[test]
    fn normalize_base_url_rejects_missing_scheme() {
        assert!(normalize_base_url("localhost:11434").is_err());
        assert!(normalize_base_url("").is_err());
    }

    #[test]
    fn native_root_strips_v1_suffix() {
        assert_eq!(
            native_root_for("http://localhost:11434/v1"),
            "http://localhost:11434"
        );
        assert_eq!(
            native_root_for("http://example.com/v1/"),
            "http://example.com"
        );
        assert_eq!(
            native_root_for("http://example.com/api/"),
            "http://example.com/api"
        );
    }

    #[test]
    fn parse_tags_returns_sorted_unique_models() {
        let body = json!({
            "models": [
                { "name": "gemma3:27b", "modified_at": "..." },
                { "name": "llama3.2:latest", "modified_at": "..." },
                { "name": "gemma3:27b", "modified_at": "..." }, // dup
                { "name": "" }                                  // empty rejected
            ]
        });
        let names = parse_tags_response(&body);
        assert_eq!(names, vec!["gemma3:27b", "llama3.2:latest"]);
    }

    #[test]
    fn parse_tags_handles_empty_array() {
        let body = json!({ "models": [] });
        assert!(parse_tags_response(&body).is_empty());
    }

    #[test]
    fn parse_tags_handles_missing_field() {
        let body = json!({ "other": "stuff" });
        assert!(parse_tags_response(&body).is_empty());
    }

    #[test]
    fn parse_tags_handles_malformed_json() {
        let body = json!("plain string, not an object");
        assert!(parse_tags_response(&body).is_empty());
    }

    #[test]
    fn local_mode_state_table_idempotent_init() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::pyramid::db::init_pyramid_db(&conn).unwrap();
        // Running init a second time must not error.
        crate::pyramid::db::init_pyramid_db(&conn).unwrap();
        let row = load_local_mode_state(&conn).unwrap();
        assert!(!row.enabled);
        assert!(row.ollama_base_url.is_none());
    }

    #[test]
    fn local_mode_state_round_trip() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::pyramid::db::init_pyramid_db(&conn).unwrap();
        save_local_mode_state(
            &conn,
            &LocalModeStateRow {
                enabled: true,
                ollama_base_url: Some("http://localhost:11434/v1".into()),
                ollama_model: Some("gemma3:27b".into()),
                detected_context_limit: Some(131_072),
                restore_from_contribution_id: Some("prior-tier-id".into()),
                restore_build_strategy_contribution_id: Some("prior-bs-id".into()),
                updated_at: String::new(),
            },
        )
        .unwrap();
        let loaded = load_local_mode_state(&conn).unwrap();
        assert!(loaded.enabled);
        assert_eq!(
            loaded.ollama_base_url.as_deref(),
            Some("http://localhost:11434/v1")
        );
        assert_eq!(loaded.ollama_model.as_deref(), Some("gemma3:27b"));
        assert_eq!(loaded.detected_context_limit, Some(131_072));
        assert_eq!(
            loaded.restore_from_contribution_id.as_deref(),
            Some("prior-tier-id")
        );
        assert_eq!(
            loaded.restore_build_strategy_contribution_id.as_deref(),
            Some("prior-bs-id")
        );
    }

    #[test]
    fn build_local_mode_build_strategy_yaml_pins_concurrency_one() {
        let prior = "schema_type: build_strategy\n\
                     initial_build:\n  model_tier: synth_heavy\n  concurrency: 8\n  webbing: true\n\
                     maintenance:\n  model_tier: stale_local\n  concurrency: 4\n";
        let out = build_local_mode_build_strategy_yaml(prior).unwrap();
        assert!(out.contains("schema_type: build_strategy"));
        // Both phases must end up with concurrency: 1.
        let value: serde_yaml::Value = serde_yaml::from_str(&out).unwrap();
        let initial = value.get("initial_build").unwrap();
        let maint = value.get("maintenance").unwrap();
        assert_eq!(
            initial.get("concurrency").unwrap().as_u64(),
            Some(1),
            "initial_build.concurrency must be 1; got {initial:?}"
        );
        assert_eq!(
            maint.get("concurrency").unwrap().as_u64(),
            Some(1),
            "maintenance.concurrency must be 1; got {maint:?}"
        );
        // Other fields must be preserved.
        assert_eq!(
            initial.get("webbing").unwrap().as_bool(),
            Some(true),
            "webbing must round-trip"
        );
        assert_eq!(
            initial.get("model_tier").unwrap().as_str(),
            Some("synth_heavy")
        );
    }

    #[test]
    fn build_tier_routing_yaml_string_uses_entries() {
        let yaml = TierRoutingYaml {
            entries: vec![TierRoutingYamlEntry {
                tier_name: "fast_extract".into(),
                provider_id: OLLAMA_LOCAL_PROVIDER_ID.into(),
                model_id: "gemma3:27b".into(),
                context_limit: Some(131_072),
                max_completion_tokens: None,
                pricing_json: Some("{}".into()),
                supported_parameters_json: None,
                notes: Some("local mode".into()),
                priority: Some(1),
                prompt_price_per_token: Some(0.0),
                completion_price_per_token: Some(0.0),
            }],
        };
        let s = build_tier_routing_yaml_string(&yaml).unwrap();
        assert!(s.contains("schema_type: tier_routing"));
        assert!(s.contains("entries:"));
        assert!(s.contains("ollama-local"));
        assert!(s.contains("gemma3:27b"));
        // The struct must round-trip back through the canonical
        // `entries:` field name (no `tiers:` legacy).
        let parsed: TierRoutingYaml = serde_yaml::from_str(&s).unwrap();
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].tier_name, "fast_extract");
    }

    #[test]
    fn tier_routing_yaml_struct_accepts_legacy_tiers_alias() {
        let legacy = "tiers:\n  - tier_name: web\n    provider_id: openrouter\n    model_id: x-ai/grok-4.1-fast\n";
        let parsed: TierRoutingYaml = serde_yaml::from_str(legacy).unwrap();
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].tier_name, "web");
        assert_eq!(parsed.entries[0].provider_id, "openrouter");
    }

    #[test]
    fn tier_routing_yaml_struct_accepts_canonical_entries() {
        // The bundled seed shape — must parse cleanly into a
        // non-empty list (Phase 4 silently parsed it as empty).
        let canonical = "schema_type: tier_routing\n\
                         entries:\n  - tier_name: fast_extract\n    provider_id: openrouter\n    model_id: inception/mercury-2\n    priority: 1\n  - tier_name: synth_heavy\n    provider_id: openrouter\n    model_id: inception/mercury-2\n    priority: 1\n  - tier_name: stale_local\n    provider_id: openrouter\n    model_id: inception/mercury-2\n    priority: 1\n";
        let parsed: TierRoutingYaml = serde_yaml::from_str(canonical).unwrap();
        assert_eq!(parsed.entries.len(), 3);
        assert!(parsed.entries.iter().any(|e| e.tier_name == "fast_extract"));
        assert!(parsed.entries.iter().any(|e| e.tier_name == "stale_local"));
        // The `priority` field must round-trip without an
        // unknown-field error.
        assert_eq!(parsed.entries[0].priority, Some(1));
    }

    #[tokio::test]
    async fn enable_local_mode_with_unreachable_ollama_errors_clearly() {
        // Build an in-memory DB with the schema initialized.
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::pyramid::db::init_pyramid_db(&conn).unwrap();

        let bus = Arc::new(crate::pyramid::event_bus::BuildEventBus::new());
        let tmp = tempfile::TempDir::new().unwrap();
        let store = Arc::new(
            crate::pyramid::credentials::CredentialStore::load(tmp.path()).unwrap(),
        );
        std::mem::forget(tmp);
        let registry = ProviderRegistry::new(store);
        registry.load_from_db(&conn).unwrap();

        // Pass a base URL that won't reach so the reachability check
        // short-circuits before any DB write.
        let result = enable_local_mode(
            &mut conn,
            &bus,
            &registry,
            "http://127.0.0.1:1/v1".into(),
            None,
        )
        .await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Cannot reach Ollama") || msg.contains("Ollama"),
            "expected Ollama-related error, got: {msg}"
        );
    }

    #[test]
    fn load_status_snapshot_disabled_returns_clean_row() {
        // Wanderer fix: the synchronous snapshot path must never
        // probe the network. On a fresh DB with enabled=false, we
        // should return a fully-populated LocalModeStatus with
        // enabled=false and empty probe fields.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::pyramid::db::init_pyramid_db(&conn).unwrap();
        let snap = load_status_snapshot(&conn).unwrap();
        assert!(!snap.enabled);
        assert!(snap.base_url.is_none());
        assert!(snap.model.is_none());
        assert!(snap.available_models.is_empty());
        assert!(!snap.reachable);
        assert!(snap.reachability_error.is_none());
        assert_eq!(snap.ollama_provider_id, OLLAMA_LOCAL_PROVIDER_ID);
    }

    #[test]
    fn load_status_snapshot_enabled_returns_saved_values_without_probing() {
        // Wanderer fix: the snapshot reads the stored base_url /
        // model without performing the `/api/tags` probe. The probe
        // is deferred to `refresh_status_reachability` so the
        // `pyramid_get_local_mode_status` IPC can release its reader
        // lock before the network round-trip.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::pyramid::db::init_pyramid_db(&conn).unwrap();
        save_local_mode_state(
            &conn,
            &LocalModeStateRow {
                enabled: true,
                ollama_base_url: Some("http://127.0.0.1:1/v1".into()),
                ollama_model: Some("gemma3:27b".into()),
                detected_context_limit: Some(131_072),
                restore_from_contribution_id: Some("prior-tier".into()),
                restore_build_strategy_contribution_id: Some("prior-bs".into()),
                updated_at: String::new(),
            },
        )
        .unwrap();
        let snap = load_status_snapshot(&conn).unwrap();
        assert!(snap.enabled);
        assert_eq!(snap.base_url.as_deref(), Some("http://127.0.0.1:1/v1"));
        assert_eq!(snap.model.as_deref(), Some("gemma3:27b"));
        assert_eq!(snap.detected_context_limit, Some(131_072));
        // Probe fields remain unpopulated on the sync path — the
        // caller is expected to run refresh_status_reachability after
        // releasing the DB lock.
        assert!(snap.available_models.is_empty());
        assert!(!snap.reachable);
        assert!(snap.reachability_error.is_none());
        assert_eq!(
            snap.prior_tier_routing_contribution_id.as_deref(),
            Some("prior-tier")
        );
        assert_eq!(
            snap.prior_build_strategy_contribution_id.as_deref(),
            Some("prior-bs")
        );
    }

    #[tokio::test]
    async fn refresh_status_reachability_disabled_is_noop() {
        // Wanderer fix: the probe step must no-op on a disabled
        // snapshot so we never hit the network when local mode is off.
        let input = LocalModeStatus::disabled_default();
        let out = refresh_status_reachability(input.clone()).await;
        assert!(!out.enabled);
        assert!(out.available_models.is_empty());
        assert!(!out.reachable);
        assert!(out.reachability_error.is_none());
    }

    #[tokio::test]
    async fn refresh_status_reachability_enabled_with_unreachable_captures_error() {
        // Wanderer fix: when enabled and the probe fails, the
        // reachability_error field carries the diagnostic string and
        // reachable stays false. This is what the UI renders as the
        // red "Cannot reach Ollama" status line.
        let mut input = LocalModeStatus::disabled_default();
        input.enabled = true;
        input.base_url = Some("http://127.0.0.1:1/v1".into());
        let out = refresh_status_reachability(input).await;
        assert!(out.enabled);
        assert!(!out.reachable);
        assert!(out.reachability_error.is_some());
        assert!(out.available_models.is_empty());
    }
}
