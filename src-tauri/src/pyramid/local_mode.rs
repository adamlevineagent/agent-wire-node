// pyramid/local_mode.rs — Local Mode toggle implementation.
//
// Per `docs/specs/provider-registry.md` §382-395 and ledger entries
// L1/L5 in `docs/plans/deferral-ledger.md`. The Local Mode toggle is
// the user-facing single switch that says "route every model tier
// through a local Ollama instance instead of OpenRouter".
//
// ── Pillar 37 / derived-view design (2026-04-21) ────────────────────
//
// Local Mode is a RUNTIME TOGGLE. The operator's authored
// `dispatch_policy` contribution is never superseded by enable/disable.
// The effective `route_to` list is computed per-reload by
// `dispatch_policy::apply_local_mode_overlay`, which filters non-local
// entries out of the authored policy when
// `pyramid_local_mode_state.enabled = true` and pins
// `defer_maintenance_during_build` on.
//
// This replaces the earlier design that wrote a hardcoded
// `[fleet, ollama-local]` dispatch_policy YAML on enable and a
// reversal YAML on disable. That design violated Pillar 37 (hardcoded
// operational parameters in Rust), erased the operator's authored
// cascade (e.g. `[market, fleet, openrouter, ollama-local]`), hid the
// real route from operators when routing bugs surfaced, and produced
// a restore-chain landmine (bug fc4a55e, 2026-04-21).
//
// `tier_routing` and `build_strategy` are still superseded on enable
// because those are semantic operator-facing changes (force all tiers
// to ollama-local; force concurrency=1 on a single local GPU) that
// legitimately flow through contributions with a proper restore-id
// chain on disable. They are orthogonal to this fix.
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
//   8. Emit a synthetic `ConfigSynced { schema_type: "dispatch_policy" }`
//      event so the `main.rs` listener re-parses the authored
//      `dispatch_policy` and applies the overlay with the now-true
//      `enabled` flag — no shadow write.
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
//   4. Emit a synthetic `ConfigSynced { schema_type: "dispatch_policy" }`
//      event so the overlay drops off and the authored
//      `dispatch_policy` becomes the effective runtime policy again.
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
// supersession history together form the rollback chain for
// tier_routing + build_strategy. `dispatch_policy` needs no restore
// chain — the authored contribution is never mutated, so "restoring"
// it is just stopping the overlay.
//
// Note on `restore_dispatch_policy_contribution_id`: kept in the
// `pyramid_local_mode_state` row for backward compat with pre-fix DBs
// (where it may hold a pointer into a shadow supersession chain) but
// written `None` and never read post-fix. Dropping the column would
// require a migration and buys nothing.

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::pyramid::config_contributions::{
    create_config_contribution, load_active_config_contribution, load_contribution_by_id,
    supersede_config_contribution, sync_config_to_operational,
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
    /// Rich model info from `/api/tags` (Phase 2). Parallel to
    /// `available_models` for backward compat — new consumers read
    /// this field for size, quant, parameter count, etc.
    pub available_model_details: Vec<OllamaModelInfo>,
    pub reachable: bool,
    pub reachability_error: Option<String>,
    pub ollama_provider_id: String,
    pub prior_tier_routing_contribution_id: Option<String>,
    pub prior_build_strategy_contribution_id: Option<String>,
    /// Phase 3 daemon control plane: user-set context window override (None = auto-detect).
    pub context_override: Option<usize>,
    /// Phase 3 daemon control plane: user-set concurrency override (None = default 1).
    pub concurrency_override: Option<usize>,
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
            available_model_details: Vec::new(),
            reachable: false,
            reachability_error: None,
            ollama_provider_id: OLLAMA_LOCAL_PROVIDER_ID.to_string(),
            prior_tier_routing_contribution_id: None,
            prior_build_strategy_contribution_id: None,
            context_override: None,
            concurrency_override: None,
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
    /// Rich model info from `/api/tags` (Phase 2). Parallel to
    /// `available_models` for backward compat.
    pub available_model_details: Vec<OllamaModelInfo>,
}

/// Rich model info extracted from Ollama's `/api/tags` and `/api/show`.
/// Phase 2 daemon control plane: populated by `fetch_ollama_models_rich`
/// (from `/api/tags` — context_window and architecture are None) and
/// enriched lazily by `pyramid_get_model_details` (which calls `/api/show`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaModelInfo {
    pub name: String,
    pub size_bytes: u64,
    pub family: Option<String>,
    pub families: Option<Vec<String>>,
    pub parameter_size: Option<String>,
    pub quantization_level: Option<String>,
    pub context_window: Option<usize>,
    pub architecture: Option<String>,
    pub modified_at: Option<String>,
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

/// Probe `GET {base}/api/tags` and return rich model info for every
/// installed model. Context_window and architecture are left as None —
/// those come from `/api/show` which is lazy-loaded per model.
pub async fn fetch_ollama_models_rich(base_url: &str) -> Result<Vec<OllamaModelInfo>> {
    let native = native_root_for(base_url);
    let url = format!("{native}/api/tags");
    let response = crate::pyramid::llm::HTTP_CLIENT
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
    Ok(parse_tags_response_rich(&body))
}

/// Probe `GET {base}/api/tags`. Returns the parsed model name list on
/// success or a clear error otherwise. Delegates to
/// `fetch_ollama_models_rich` and maps to `Vec<String>` for backward
/// compatibility with all existing callers.
pub async fn fetch_ollama_models(base_url: &str) -> Result<Vec<String>> {
    let rich = fetch_ollama_models_rich(base_url).await?;
    Ok(rich.into_iter().map(|m| m.name).collect())
}

/// Parse Ollama's `/api/tags` response into rich `OllamaModelInfo` entries.
/// Extracts name, size, details (family, families, parameter_size,
/// quantization_level), and modified_at from each model entry.
/// Context_window and architecture are None (populated lazily via `/api/show`).
/// Returns a sorted (by name), deduplicated list. Never panics.
pub fn parse_tags_response_rich(body: &serde_json::Value) -> Vec<OllamaModelInfo> {
    let mut seen = std::collections::BTreeMap::<String, OllamaModelInfo>::new();
    let Some(models) = body.get("models").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    for entry in models {
        let Some(name) = entry.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        if name.trim().is_empty() {
            continue;
        }
        let size_bytes = entry.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
        let details = entry.get("details");
        let family = details
            .and_then(|d| d.get("family"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let families = details
            .and_then(|d| d.get("families"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            });
        let parameter_size = details
            .and_then(|d| d.get("parameter_size"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let quantization_level = details
            .and_then(|d| d.get("quantization_level"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let modified_at = entry
            .get("modified_at")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        seen.entry(name.to_string()).or_insert(OllamaModelInfo {
            name: name.to_string(),
            size_bytes,
            family,
            families,
            parameter_size,
            quantization_level,
            context_window: None,
            architecture: None,
            modified_at,
        });
    }
    seen.into_values().collect()
}

/// Parse Ollama's `/api/tags` response shape into a sorted, unique
/// list of model names. Backward-compat wrapper around
/// `parse_tags_response_rich`.
pub fn parse_tags_response(body: &serde_json::Value) -> Vec<String> {
    parse_tags_response_rich(body)
        .into_iter()
        .map(|m| m.name)
        .collect()
}

/// Probe `GET {base}/api/tags` and return both the reachability state
/// and the model list (with rich details) in a single round trip.
/// Wraps `fetch_ollama_models_rich` in a probe-shaped result so
/// callers can surface "test connection" output without two calls.
pub async fn probe_ollama(base_url: &str) -> OllamaProbeResult {
    match fetch_ollama_models_rich(base_url).await {
        Ok(rich_models) => {
            let names: Vec<String> = rich_models.iter().map(|m| m.name.clone()).collect();
            OllamaProbeResult {
                reachable: true,
                reachability_error: None,
                available_models: names,
                available_model_details: rich_models,
            }
        }
        Err(err) => OllamaProbeResult {
            reachable: false,
            reachability_error: Some(err.to_string()),
            available_models: Vec::new(),
            available_model_details: Vec::new(),
        },
    }
}

/// Auto-detect a model's context window via Ollama's `/api/show`.
/// Returns `None` when the call fails or the response doesn't expose
/// a recognizable `*.context_length` key.
pub async fn detect_ollama_context_window(base_url: &str, model: &str) -> Option<usize> {
    let native = native_root_for(base_url);
    let url = format!("{native}/api/show");
    let body = serde_json::json!({ "model": model });
    let resp = crate::pyramid::llm::HTTP_CLIENT
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

/// Fetch full model details for a single model via `/api/show`.
/// Returns an `OllamaModelInfo` with `context_window` and
/// `architecture` filled in (from `/api/show`'s `model_info` object).
/// The base fields (size_bytes, family, etc.) come from `/api/show`'s
/// top-level `details` object. Used by the `pyramid_get_model_details`
/// IPC for lazy-loading per-model detail cards.
pub async fn fetch_model_details(base_url: &str, model: &str) -> Result<OllamaModelInfo> {
    let native = native_root_for(base_url);
    let url = format!("{native}/api/show");
    let req_body = serde_json::json!({ "model": model });
    let resp = crate::pyramid::llm::HTTP_CLIENT
        .post(&url)
        .json(&req_body)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .with_context(|| format!("POST {url} for model {model} failed"))?;
    if !resp.status().is_success() {
        bail!("POST {url} for model {model} returned status {}", resp.status());
    }
    let v: serde_json::Value = resp
        .json()
        .await
        .with_context(|| format!("parsing /api/show response for {model}"))?;

    // Extract context_window via the existing parser.
    let context_window = crate::pyramid::provider::parse_ollama_context_length(&v);

    // Extract architecture from model_info.general.architecture.
    let architecture = v
        .get("model_info")
        .and_then(|mi| mi.get("general.architecture"))
        .and_then(|a| a.as_str())
        .map(|s| s.to_string());

    // Extract details from the top-level details object (same structure
    // as /api/tags entries but at the root level of /api/show).
    let details = v.get("details");
    let family = details
        .and_then(|d| d.get("family"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let families = details
        .and_then(|d| d.get("families"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        });
    let parameter_size = details
        .and_then(|d| d.get("parameter_size"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let quantization_level = details
        .and_then(|d| d.get("quantization_level"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // /api/show exposes size at the top level (same as /api/tags).
    let size_bytes = v.get("size").and_then(|s| s.as_u64()).unwrap_or(0);

    let modified_at = v
        .get("modified_at")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Ok(OllamaModelInfo {
        name: model.to_string(),
        size_bytes,
        family,
        families,
        parameter_size,
        quantization_level,
        context_window,
        architecture,
        modified_at,
    })
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
        available_model_details: Vec::new(),
        // `reachable` starts false; the probe-refresh step upgrades it
        // to true only when the probe succeeds. The UI distinguishes
        // "enabled + unreachable" (red X) from "disabled" (grey).
        reachable: false,
        reachability_error: None,
        ollama_provider_id: OLLAMA_LOCAL_PROVIDER_ID.to_string(),
        prior_tier_routing_contribution_id: row.restore_from_contribution_id,
        prior_build_strategy_contribution_id: row.restore_build_strategy_contribution_id,
        context_override: row.context_override.map(|n| n as usize),
        concurrency_override: row.concurrency_override.map(|n| n as usize),
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
    status.available_model_details = probe.available_model_details;
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

/// Force the live `cfg.dispatch_policy` to reload from the operational
/// table so the Local Mode overlay picks up the current state-row
/// `enabled` flag. Does NOT modify any contribution — the operator's
/// authored `dispatch_policy` is left exactly as they wrote it.
///
/// Implementation: emit a synthetic `ConfigSynced { schema_type:
/// "dispatch_policy", ... }` event. The `dispatch_policy` listener in
/// `main.rs` re-reads the YAML from `pyramid_dispatch_policy`, runs
/// `apply_local_mode_overlay` with the latest `pyramid_local_mode_state.
/// enabled`, and writes the filtered runtime policy onto the live
/// `LlmConfig`. Because this is not a real contribution sync, the
/// `contribution_id` field carries a synthetic `local_mode_refresh`
/// marker — consumers that care about prior/next contribution IDs
/// (none today for dispatch_policy) can filter on it.
fn refresh_dispatch_policy_for_local_mode(
    conn: &Connection,
    bus: &Arc<BuildEventBus>,
) -> Result<()> {
    // Load the currently-active authored dispatch_policy contribution
    // to stamp its id onto the event. If none exists (fresh install
    // before bundled seed), emit the event anyway with an empty id —
    // the listener's DB read will find nothing and leave the live
    // cfg.dispatch_policy untouched.
    let active = load_active_config_contribution(conn, "dispatch_policy", None)?;
    let contribution_id = active
        .as_ref()
        .map(|c| c.contribution_id.clone())
        .unwrap_or_default();

    let _ = bus.tx.send(crate::pyramid::event_bus::TaggedBuildEvent {
        slug: String::new(),
        kind: crate::pyramid::event_bus::TaggedKind::ConfigSynced {
            slug: None,
            schema_type: "dispatch_policy".to_string(),
            contribution_id,
            prior_contribution_id: None,
        },
    });
    Ok(())
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

    // Read existing state to check for context_override (AD-4).
    let pre_existing_row = load_local_mode_state(conn)?;
    let effective_context = pre_existing_row
        .context_override
        .map(|n| n as usize)
        .unwrap_or(detected_context);

    // Step 5: upsert the `ollama-local` provider row.
    // Include num_ctx in config_json so Ollama allocates the context window
    // Wire Node expects (AD-4 num_ctx pass-through).
    let provider = Provider {
        id: OLLAMA_LOCAL_PROVIDER_ID.to_string(),
        display_name: "Ollama (local)".to_string(),
        provider_type: ProviderType::OpenaiCompat,
        base_url: base_url.clone(),
        api_key_ref: None,
        auto_detect_context: true,
        supports_broadcast: false,
        broadcast_config_json: None,
        config_json: {
            // Read-modify-write: preserve existing keys (e.g. extra_headers
            // for nginx-fronted Ollama) while merging num_ctx.
            let mut cfg: serde_json::Value = db::get_provider(conn, OLLAMA_LOCAL_PROVIDER_ID)
                .ok()
                .flatten()
                .and_then(|p| serde_json::from_str(&p.config_json).ok())
                .unwrap_or_else(|| serde_json::json!({}));
            cfg["num_ctx"] = serde_json::json!(effective_context);
            cfg.to_string()
        },
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
            context_limit: Some(effective_context as i64),
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
    //
    // Read-modify-write: preserve context_override, concurrency_override
    // from the pre-existing row so user overrides survive enable cycles.
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
            context_override: pre_existing_row.context_override,
            concurrency_override: pre_existing_row.concurrency_override,
            restore_dispatch_policy_contribution_id: None, // post-fix: never written; overlay replaces shadow
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

    // Step 10 (Pillar 37 / everything-is-a-contribution fix):
    //
    // Local Mode used to supersede the operator's `dispatch_policy`
    // contribution with a hardcoded YAML fragment — erasing the authored
    // `[market, fleet, openrouter, ollama-local]` cascade and replacing
    // it with a stripped-down `[fleet, ollama-local]` chain. That violated
    // Pillar 37 (hardcoded operational parameters in Rust), hid the real
    // route from the operator when routing bugs surfaced (they'd see a
    // substituted policy and couldn't tell what was their config vs.
    // Local Mode's substitution), and its restore path was the landmine
    // behind bug fc4a55e.
    //
    // The replacement is a derived view: the operator's authored
    // `dispatch_policy` contribution stays untouched, and
    // `dispatch_policy::apply_local_mode_overlay` filters non-local
    // `route_to` entries at the ConfigSynced load point in `main.rs`.
    // Disable is trivially reversible — the next ConfigSynced reload
    // simply stops filtering.
    //
    // We still trigger a ConfigSynced refresh here so the live
    // `cfg.dispatch_policy` flips to the local-only view without
    // waiting for an unrelated contribution write. The authored
    // contribution is re-loaded from DB and overlaid with the now-true
    // `local_mode_enabled`; no shadow write happens.
    refresh_dispatch_policy_for_local_mode(conn, bus)?;

    // Step 11: re-apply concurrency override if the user had set one
    // before a prior disable. The enable path pinned concurrency=1 on
    // build_strategy above, so without this step a returning user would
    // see concurrency revert to 1. dispatch_policy is now derived-view
    // (no shadow write), so `set_concurrency_override` edits the
    // authored `provider_pools.ollama-local.concurrency` directly — the
    // operator's explicit pool setting persists across toggle cycles.
    let current_row = load_local_mode_state(conn)?;
    if let Some(c) = current_row.concurrency_override {
        if c > 1 {
            set_concurrency_override(conn, bus, registry, Some(c as usize))?;
        }
    }

    // Step 12: commit is done; caller rebuilds the status snapshot
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

    // Pre-fix DB compatibility: pre-2026-04-21 enable paths wrote a
    // shadow `dispatch_policy` contribution and stashed the authored
    // one in `restore_dispatch_policy_contribution_id`. Post-fix, that
    // column is never written, but DBs upgraded mid-toggle may still
    // hold a pointer to a pre-fix-stashed authored policy. Honor it
    // once, collapse back to the authored contribution, and clear the
    // column so subsequent toggles go through the clean overlay path.
    if let Some(restore_id) = row.restore_dispatch_policy_contribution_id.as_deref() {
        if let Some(restore) = load_contribution_by_id(conn, restore_id)? {
            if let Some(active_now) =
                load_active_config_contribution(conn, "dispatch_policy", None)?
            {
                let new_id = supersede_config_contribution(
                    conn,
                    &active_now.contribution_id,
                    &restore.yaml_content,
                    "local mode disabled — one-time restore of pre-fix shadow dispatch_policy",
                    "local_mode_toggle",
                    Some("user"),
                )?;
                let new_contribution = load_contribution_by_id(conn, &new_id)?.ok_or_else(|| {
                    anyhow!("restored dispatch_policy contribution missing after supersede")
                })?;
                sync_config_to_operational(conn, bus, &new_contribution)?;
            }
        }
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
    // Preserve context_override + concurrency_override (AD-4 persistence rule).
    //
    // Order matters: the dispatch_policy refresh below reads
    // `pyramid_local_mode_state.enabled` to decide whether to apply the
    // overlay. Flipping to `enabled: false` must happen BEFORE the
    // refresh so the overlay drops off and the live cfg.dispatch_policy
    // returns to the authored view.
    save_local_mode_state(
        conn,
        &LocalModeStateRow {
            enabled: false,
            ollama_base_url: row.ollama_base_url.clone(),
            ollama_model: row.ollama_model.clone(),
            detected_context_limit: row.detected_context_limit,
            restore_from_contribution_id: None,
            restore_build_strategy_contribution_id: None,
            context_override: row.context_override,
            concurrency_override: row.concurrency_override,
            restore_dispatch_policy_contribution_id: None,
            updated_at: String::new(),
        },
    )?;

    // Pillar 37 fix: dispatch_policy is never superseded by Local Mode,
    // so disable is a no-op for the contribution itself. We just trigger
    // a ConfigSynced refresh so the live `cfg.dispatch_policy` flips
    // from the filtered view back to the authored view. The overlay
    // reads the state-row flag we flipped above.
    //
    // Note: the `restore_dispatch_policy_contribution_id` state column
    // is kept for backward compat with pre-fix DBs but never written
    // post-fix (see enable path). Pre-fix DBs that had a shadow
    // supersession chain are already handled by an earlier fix
    // (fc4a55e); once disabled cleanly post-fix, those chains collapse
    // to the authored contribution and stay there.
    refresh_dispatch_policy_for_local_mode(conn, bus)?;

    Ok(())
}

// Marker re-exports so the helper API stays inside this module's
// namespace from the IPC layer's perspective.
pub use db::load_local_mode_state as read_state;
pub use db::save_local_mode_state as write_state;

// ── Hot-swap (AD-1) ────────────────────────────────────────────────────────

/// Async prepare phase for model hot-swap. Validates the model name,
/// checks Ollama reachability, verifies the model exists, and probes
/// `/api/show` for the context window. No DB work.
pub async fn prepare_switch_local_model(
    base_url: &str,
    model: &str,
) -> Result<(String, usize)> {
    if model.trim().is_empty() {
        bail!("Model name must not be empty");
    }
    // Verify Ollama is reachable and the model exists (matches enable validation)
    let probe = probe_ollama(base_url).await;
    if !probe.reachable {
        return Err(anyhow!(
            "Cannot reach Ollama at {base_url}: {}. Is Ollama running?",
            probe.reachability_error.unwrap_or_else(|| "unknown error".to_string())
        ));
    }
    if !probe.available_models.iter().any(|m| m == model) {
        return Err(anyhow!(
            "Model '{}' not found on Ollama at {base_url}. Available: {}",
            model,
            probe.available_models.join(", ")
        ));
    }
    let detected_context = detect_ollama_context_window(base_url, model)
        .await
        .unwrap_or(DEFAULT_OLLAMA_CONTEXT_FALLBACK);
    Ok((model.to_string(), detected_context))
}

/// Sync commit phase for model hot-swap. Runs under the writer lock.
/// Updates tier_routing with the new model, writes num_ctx to provider
/// config_json, and read-modify-writes the state row.
pub fn commit_switch_local_model(
    conn: &mut Connection,
    bus: &Arc<BuildEventBus>,
    registry: &ProviderRegistry,
    model: String,
    detected_context: usize,
) -> Result<()> {
    // Read current state — need base_url, context_override, etc.
    let row = load_local_mode_state(conn)?;
    if !row.enabled {
        bail!("Local mode is not enabled — cannot switch model");
    }
    let base_url = row
        .ollama_base_url
        .as_deref()
        .ok_or_else(|| anyhow!("local mode enabled but no base_url in state row"))?;

    // Effective context: override wins over detected.
    let effective_context = row
        .context_override
        .map(|n| n as usize)
        .unwrap_or(detected_context);

    // Find the currently-active tier_routing contribution (NOT restore_from —
    // that points at the PRE-local-mode contribution for the disable path).
    let active_tier = load_active_config_contribution(conn, "tier_routing", None)?
        .ok_or_else(|| anyhow!("no active tier_routing contribution to supersede during model switch"))?;

    // Build new tier_routing YAML with the new model + effective context.
    let prior_tier_yaml: TierRoutingYaml =
        serde_yaml::from_str(&active_tier.yaml_content)
            .with_context(|| format!("parsing active tier_routing for model switch"))?;
    let mut prior_tier_names: std::collections::BTreeSet<String> =
        prior_tier_yaml.entries.iter().map(|e| e.tier_name.clone()).collect();
    for required in [
        "fast_extract", "web", "synth_heavy", "stale_remote", "stale_local",
        "mid", "extractor", "high", "max",
    ] {
        prior_tier_names.insert(required.to_string());
    }
    let new_entries: Vec<TierRoutingYamlEntry> = prior_tier_names
        .into_iter()
        .map(|tier_name| TierRoutingYamlEntry {
            tier_name,
            provider_id: OLLAMA_LOCAL_PROVIDER_ID.to_string(),
            model_id: model.clone(),
            context_limit: Some(effective_context as i64),
            max_completion_tokens: None,
            pricing_json: Some("{}".to_string()),
            supported_parameters_json: Some(r#"["response_format"]"#.to_string()),
            notes: Some(format!("local mode — model switch via Ollama at {base_url}")),
            priority: Some(1),
            prompt_price_per_token: Some(0.0),
            completion_price_per_token: Some(0.0),
        })
        .collect();
    let new_tier_yaml = TierRoutingYaml { entries: new_entries };
    let new_tier_yaml_string = build_tier_routing_yaml_string(&new_tier_yaml)?;

    // Update provider config_json with num_ctx so Ollama allocates the context window.
    // Read-modify-write: preserve existing keys (e.g. extra_headers for nginx-fronted Ollama).
    if let Ok(Some(mut provider)) = super::db::get_provider(conn, OLLAMA_LOCAL_PROVIDER_ID) {
        let mut cfg: serde_json::Value = serde_json::from_str(&provider.config_json)
            .unwrap_or_else(|_| serde_json::json!({}));
        cfg["num_ctx"] = serde_json::json!(effective_context);
        provider.config_json = cfg.to_string();
        super::db::save_provider(conn, &provider)?;
    }

    // Supersede tier_routing.
    let new_tier_id = supersede_config_contribution(
        conn,
        &active_tier.contribution_id,
        &new_tier_yaml_string,
        &format!("model switch to {model}"),
        "local_mode_switch",
        Some("user"),
    )?;
    let new_tier_contribution = load_contribution_by_id(conn, &new_tier_id)?
        .ok_or_else(|| anyhow!("model-switch tier contribution missing after supersede"))?;
    sync_config_to_operational(conn, bus, &new_tier_contribution)?;

    // Read-modify-write state row: update model + detected_context,
    // preserve everything else (AD-1: does NOT touch restore columns).
    save_local_mode_state(
        conn,
        &LocalModeStateRow {
            ollama_model: Some(model),
            detected_context_limit: Some(detected_context as i64),
            ..row
        },
    )?;

    // Refresh in-memory registry.
    registry.load_from_db(conn)?;

    Ok(())
}

// ── Context Override (AD-4, Phase 3) ──────────────────────────────────────

/// Sync commit: set or clear the context window override.
///
/// When `limit` is `Some(n)`: stores `context_override = n` in the state
/// row, supersedes the active `tier_routing` contribution with the
/// override value as `context_limit`, and writes `num_ctx` to the
/// `ollama-local` provider's `config_json` so Ollama actually allocates
/// the context window Wire Node expects.
///
/// When `limit` is `None`: clears the override, restoring the
/// auto-detected context limit from the state row's
/// `detected_context_limit`.
///
/// Must be called under the writer lock. Caller is responsible for
/// the active-build guard (checked in the IPC layer).
pub fn set_context_override(
    conn: &mut Connection,
    bus: &Arc<BuildEventBus>,
    registry: &ProviderRegistry,
    limit: Option<usize>,
) -> Result<()> {
    let row = load_local_mode_state(conn)?;
    if !row.enabled {
        bail!("Local mode is not enabled — cannot set context override");
    }
    let base_url = row
        .ollama_base_url
        .as_deref()
        .ok_or_else(|| anyhow!("local mode enabled but no base_url in state row"))?;

    // Compute effective context: override wins, else auto-detected.
    let detected = row
        .detected_context_limit
        .map(|n| n as usize)
        .unwrap_or(DEFAULT_OLLAMA_CONTEXT_FALLBACK);
    let effective_context = limit.unwrap_or(detected);

    // --- Supersede tier_routing with the effective context ---
    let active_tier = load_active_config_contribution(conn, "tier_routing", None)?
        .ok_or_else(|| anyhow!("no active tier_routing contribution for context override"))?;

    let prior_tier_yaml: TierRoutingYaml =
        serde_yaml::from_str(&active_tier.yaml_content)
            .with_context(|| "parsing active tier_routing for context override")?;
    let mut prior_tier_names: std::collections::BTreeSet<String> =
        prior_tier_yaml.entries.iter().map(|e| e.tier_name.clone()).collect();
    for required in [
        "fast_extract", "web", "synth_heavy", "stale_remote", "stale_local",
        "mid", "extractor", "high", "max",
    ] {
        prior_tier_names.insert(required.to_string());
    }
    let model = row
        .ollama_model
        .as_deref()
        .ok_or_else(|| anyhow!("local mode enabled but no model in state row"))?;
    let new_entries: Vec<TierRoutingYamlEntry> = prior_tier_names
        .into_iter()
        .map(|tier_name| TierRoutingYamlEntry {
            tier_name,
            provider_id: OLLAMA_LOCAL_PROVIDER_ID.to_string(),
            model_id: model.to_string(),
            context_limit: Some(effective_context as i64),
            max_completion_tokens: None,
            pricing_json: Some("{}".to_string()),
            supported_parameters_json: Some(r#"["response_format"]"#.to_string()),
            notes: Some(format!(
                "local mode — context override via Ollama at {base_url}"
            )),
            priority: Some(1),
            prompt_price_per_token: Some(0.0),
            completion_price_per_token: Some(0.0),
        })
        .collect();
    let new_tier_yaml = TierRoutingYaml { entries: new_entries };
    let new_tier_yaml_string = build_tier_routing_yaml_string(&new_tier_yaml)?;

    let note = match limit {
        Some(n) => format!("context override set to {n}"),
        None => "context override cleared — using auto-detected".to_string(),
    };
    let new_tier_id = supersede_config_contribution(
        conn,
        &active_tier.contribution_id,
        &new_tier_yaml_string,
        &note,
        "local_mode_context_override",
        Some("user"),
    )?;
    let new_tier_contribution = load_contribution_by_id(conn, &new_tier_id)?
        .ok_or_else(|| anyhow!("context-override tier contribution missing after supersede"))?;
    sync_config_to_operational(conn, bus, &new_tier_contribution)?;

    // --- Update provider config_json num_ctx (read-modify-write) ---
    if let Ok(Some(mut provider)) = super::db::get_provider(conn, OLLAMA_LOCAL_PROVIDER_ID) {
        let mut cfg: serde_json::Value = serde_json::from_str(&provider.config_json)
            .unwrap_or_else(|_| serde_json::json!({}));
        cfg["num_ctx"] = serde_json::json!(effective_context);
        provider.config_json = cfg.to_string();
        super::db::save_provider(conn, &provider)?;
    }

    // --- Read-modify-write state row: set context_override, preserve everything else ---
    save_local_mode_state(
        conn,
        &LocalModeStateRow {
            context_override: limit.map(|n| n as i64),
            ..row
        },
    )?;

    // Refresh in-memory registry.
    registry.load_from_db(conn)?;

    Ok(())
}

// ── Concurrency Override (AD-5, Phase 3) ──────────────────────────────────

/// Maximum concurrency the user can set via the override.
pub const MAX_CONCURRENCY: usize = 12;

/// Sync commit: set or clear the concurrency override.
///
/// When `concurrency` is `Some(n)`: clamps to 1..=MAX_CONCURRENCY, then
/// supersedes BOTH the active `build_strategy` (for_each cap) AND the
/// active `dispatch_policy` (provider_pools.ollama-local.concurrency)
/// contributions in lockstep.
///
/// When `concurrency` is `None`: restores default concurrency (1).
///
/// Must be called under the writer lock. Caller is responsible for
/// the active-build guard.
pub fn set_concurrency_override(
    conn: &mut Connection,
    bus: &Arc<BuildEventBus>,
    registry: &ProviderRegistry,
    concurrency: Option<usize>,
) -> Result<()> {
    let row = load_local_mode_state(conn)?;
    if !row.enabled {
        bail!("Local mode is not enabled — cannot set concurrency override");
    }

    // Clamp or default to 1.
    let effective = concurrency
        .map(|n| n.clamp(1, MAX_CONCURRENCY))
        .unwrap_or(1);

    // --- Supersede build_strategy with the concurrency value ---
    let active_bs = load_active_config_contribution(conn, "build_strategy", None)?
        .ok_or_else(|| anyhow!("no active build_strategy contribution for concurrency override"))?;

    // Parse, modify concurrency in both phases, re-serialize.
    let mut bs_value: serde_yaml::Value = serde_yaml::from_str(&active_bs.yaml_content)
        .context("parsing active build_strategy for concurrency override")?;
    let bs_map = bs_value
        .as_mapping_mut()
        .ok_or_else(|| anyhow!("active build_strategy YAML is not a mapping"))?;
    bs_map.insert(
        serde_yaml::Value::String("schema_type".into()),
        serde_yaml::Value::String("build_strategy".into()),
    );
    for phase in ["initial_build", "maintenance"] {
        let key = serde_yaml::Value::String(phase.into());
        let phase_map = bs_map
            .entry(key.clone())
            .or_insert_with(|| serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
        if let Some(phase_map) = phase_map.as_mapping_mut() {
            phase_map.insert(
                serde_yaml::Value::String("concurrency".into()),
                serde_yaml::Value::Number(serde_yaml::Number::from(effective as u64)),
            );
        }
    }
    let new_bs_yaml = serde_yaml::to_string(&bs_value)
        .context("rendering build_strategy YAML for concurrency override")?;

    let bs_note = match concurrency {
        Some(n) => format!("concurrency override set to {}", n.clamp(1, MAX_CONCURRENCY)),
        None => "concurrency override cleared — restoring default 1".to_string(),
    };
    let new_bs_id = supersede_config_contribution(
        conn,
        &active_bs.contribution_id,
        &new_bs_yaml,
        &bs_note,
        "local_mode_concurrency_override",
        Some("user"),
    )?;
    let new_bs_contribution = load_contribution_by_id(conn, &new_bs_id)?
        .ok_or_else(|| anyhow!("concurrency-override build_strategy contribution missing after supersede"))?;
    sync_config_to_operational(conn, bus, &new_bs_contribution)?;

    // --- Supersede dispatch_policy with updated provider_pools concurrency ---
    let active_dp = load_active_config_contribution(conn, "dispatch_policy", None)?
        .ok_or_else(|| anyhow!("no active dispatch_policy contribution for concurrency override"))?;

    // Parse, modify provider_pools.ollama-local.concurrency, re-serialize.
    let mut dp_value: serde_yaml::Value = serde_yaml::from_str(&active_dp.yaml_content)
        .context("parsing active dispatch_policy for concurrency override")?;
    let dp_map = dp_value
        .as_mapping_mut()
        .ok_or_else(|| anyhow!("active dispatch_policy YAML is not a mapping"))?;
    // Ensure provider_pools mapping exists.
    let pools_key = serde_yaml::Value::String("provider_pools".into());
    let pools = dp_map
        .entry(pools_key.clone())
        .or_insert_with(|| serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
    if let Some(pools_map) = pools.as_mapping_mut() {
        let ollama_key = serde_yaml::Value::String(OLLAMA_LOCAL_PROVIDER_ID.into());
        let ollama_pool = pools_map
            .entry(ollama_key.clone())
            .or_insert_with(|| serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
        if let Some(ollama_map) = ollama_pool.as_mapping_mut() {
            ollama_map.insert(
                serde_yaml::Value::String("concurrency".into()),
                serde_yaml::Value::Number(serde_yaml::Number::from(effective as u64)),
            );
        }
    }
    let new_dp_yaml = serde_yaml::to_string(&dp_value)
        .context("rendering dispatch_policy YAML for concurrency override")?;

    let dp_note = match concurrency {
        Some(n) => format!("concurrency override set to {} — pool updated", n.clamp(1, MAX_CONCURRENCY)),
        None => "concurrency override cleared — pool restored to 1".to_string(),
    };
    let new_dp_id = supersede_config_contribution(
        conn,
        &active_dp.contribution_id,
        &new_dp_yaml,
        &dp_note,
        "local_mode_concurrency_override",
        Some("user"),
    )?;
    let new_dp_contribution = load_contribution_by_id(conn, &new_dp_id)?
        .ok_or_else(|| anyhow!("concurrency-override dispatch_policy contribution missing after supersede"))?;
    sync_config_to_operational(conn, bus, &new_dp_contribution)?;

    // --- Read-modify-write state row: set concurrency_override, preserve everything else ---
    save_local_mode_state(
        conn,
        &LocalModeStateRow {
            concurrency_override: concurrency.map(|n| n.clamp(1, MAX_CONCURRENCY) as i64),
            ..row
        },
    )?;

    // Refresh in-memory registry.
    registry.load_from_db(conn)?;

    Ok(())
}

// ── Phase 4 Daemon Control Plane: Pull + Delete ────────────────────────────

/// Reserved non-pyramid slug for Ollama pull events (AD-3). Used as the
/// outer `TaggedBuildEvent.slug` so downstream consumers can distinguish
/// pull progress from pyramid build events. Must NOT be empty string —
/// empty-slug events pollute the `useCrossPyramidTimeline` hook's `bySlug`
/// Map, creating a phantom timeline entry for a non-existent pyramid.
pub const OLLAMA_EVENT_SLUG: &str = "__ollama__";

/// Pull an Ollama model with streaming progress, broadcasting each chunk
/// as a `TaggedBuildEvent` on the build event bus.
///
/// Ollama's `POST /api/pull` returns newline-delimited JSON chunks in
/// phases:
///   1. `{"status": "pulling manifest"}` (no bytes)
///   2. `{"status": "pulling <digest>", "digest": "...", "total": N, "completed": N}`
///   3. `{"status": "verifying sha256 digest"}` (no bytes)
///   4. `{"status": "writing manifest"}` (no bytes)
///   5. `{"status": "success"}` (complete)
///
/// Between chunks, the `cancel` flag is checked. If set, the stream is
/// dropped and the function returns an error. The caller (the IPC handler)
/// is responsible for managing the `cancel` flag and the `pull_in_progress`
/// mutex.
pub async fn pull_ollama_model(
    base_url: &str,
    model: &str,
    bus: &Arc<BuildEventBus>,
    cancel: &AtomicBool,
) -> Result<()> {
    use crate::pyramid::event_bus::{TaggedBuildEvent, TaggedKind};

    let native = native_root_for(base_url);
    let url = format!("{native}/api/pull");
    let req_body = serde_json::json!({ "model": model });

    let response = crate::pyramid::llm::HTTP_CLIENT
        .post(&url)
        .json(&req_body)
        .send()
        .await
        .with_context(|| format!("POST {url} for model {model} failed (is Ollama running?)"))?;

    if !response.status().is_success() {
        bail!(
            "POST {url} for model {model} returned status {}",
            response.status()
        );
    }

    // Stream the response body chunk by chunk. Ollama sends newline-
    // delimited JSON — each chunk may contain one or more complete JSON
    // lines, or a partial line that spans two chunks. We accumulate a
    // line buffer and process complete lines as they arrive.
    let mut line_buf = String::new();
    let mut response = response;

    loop {
        // Check cancellation before each chunk read.
        if cancel.load(Ordering::Relaxed) {
            // Drop the response (closes the connection) and report.
            drop(response);
            bail!("Pull of model {model} was cancelled");
        }

        let chunk = response
            .chunk()
            .await
            .with_context(|| format!("reading pull stream for model {model}"))?;

        let Some(chunk) = chunk else {
            // Stream ended without a "success" status. This can happen
            // if the model was already present and Ollama short-circuits.
            break;
        };

        // Append raw bytes to the line buffer.
        let text = String::from_utf8_lossy(&chunk);
        line_buf.push_str(&text);

        // Process complete lines (newline-delimited JSON).
        while let Some(newline_pos) = line_buf.find('\n') {
            let line: String = line_buf.drain(..=newline_pos).collect();
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            // Parse the JSON line. If it fails, log and skip — Ollama
            // occasionally sends empty or malformed lines during early
            // manifest phases.
            let Ok(obj) = serde_json::from_str::<serde_json::Value>(line) else {
                tracing::debug!(line = line, "skipping unparseable pull chunk");
                continue;
            };

            let status = obj
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let completed_bytes = obj.get("completed").and_then(|v| v.as_u64());
            let total_bytes = obj.get("total").and_then(|v| v.as_u64());

            // Broadcast the event.
            let _ = bus.tx.send(TaggedBuildEvent {
                slug: OLLAMA_EVENT_SLUG.to_string(),
                kind: TaggedKind::OllamaPull {
                    model: model.to_string(),
                    status: status.clone(),
                    completed_bytes,
                    total_bytes,
                },
            });

            // Terminal condition: Ollama sends {"status": "success"}
            // when the pull is fully complete.
            if status == "success" {
                return Ok(());
            }
        }
    }

    // If we exit the loop without seeing "success", the pull completed
    // (no more chunks) but may have been a no-op (model already present).
    // Emit a synthetic success event so the frontend always gets a
    // terminal event.
    let _ = bus.tx.send(crate::pyramid::event_bus::TaggedBuildEvent {
        slug: OLLAMA_EVENT_SLUG.to_string(),
        kind: crate::pyramid::event_bus::TaggedKind::OllamaPull {
            model: model.to_string(),
            status: "success".to_string(),
            completed_bytes: None,
            total_bytes: None,
        },
    });

    Ok(())
}

/// Delete an Ollama model via `DELETE /api/delete`.
///
/// Note: DELETE with a JSON body is non-standard HTTP but matches
/// Ollama's API. reqwest handles it correctly.
///
/// The caller (IPC handler) is responsible for checking that the model
/// is not the currently-active model before calling this function.
pub async fn delete_ollama_model(base_url: &str, model: &str) -> Result<()> {
    let native = native_root_for(base_url);
    let url = format!("{native}/api/delete");
    let req_body = serde_json::json!({ "model": model });

    let response = crate::pyramid::llm::HTTP_CLIENT
        .delete(&url)
        .json(&req_body)
        .send()
        .await
        .with_context(|| format!("DELETE {url} for model {model} failed (is Ollama running?)"))?;

    if !response.status().is_success() {
        let status = response.status();
        // Try to read error body for a better message.
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "(no body)".to_string());
        bail!(
            "DELETE {url} for model {model} returned status {status}: {body}"
        );
    }

    Ok(())
}

// ── Phase 6 Daemon Control Plane: Experimental Territory (AD-6) ─────────────

/// High-level participation presets for how this node joins the Wire's
/// decentralized markets (compute, storage, relay) plus the private fleet.
/// This is the durable operator-intent layer; later phases derive dispatch
/// behavior and peer capability from it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ComputeParticipationMode {
    Coordinator,
    Hybrid,
    Worker,
}

/// Fleet MPS WS1: durable source of truth for this node's participation
/// posture across compute / storage / relay markets plus the private fleet.
/// Per DD-I (architecture §VIII.6), the canonical shape is 10 fields:
/// one mode preset + 8 projectable dispatch/serving/hosting/usage booleans
/// + one always-explicit `allow_serving_while_degraded` knob.
///
/// The 8 projectable booleans are `Option<bool>` so the contribution can
/// express "use the mode's preset" (None) distinctly from "explicit
/// override" (Some(v)). Resolution to concrete booleans happens at
/// config-read time via `effective_booleans()` — explicit values win over
/// the mode projection.
///
/// `allow_serving_while_degraded` is NEVER projected. It's an operational
/// safety knob the operator sets independently of market participation.
///
/// ## CONSUMER WARNING
///
/// **Do NOT read the `Option<bool>` fields directly to make gating
/// decisions.** A bare `.unwrap_or(false)` is the exact wrong thing — it
/// treats "project from mode" as "forbidden," the INVERSE of the DD-I
/// semantic. Every consumer (compute admission gates, offer publication,
/// LLM dispatch selection, storage hosting gate, relay forwarding gate)
/// MUST call [`ComputeParticipationPolicy::effective_booleans`] and read
/// the resolved concrete booleans off the returned
/// [`EffectiveParticipationPolicy`].
///
/// The raw Option fields exist so the contribution can honestly store
/// "operator picked mode=X and left the rest default" distinctly from
/// "operator explicitly overrode this field." That distinction matters
/// for `set_compute_participation_policy` roundtrips and for legacy YAML
/// canonicalization — it should NOT leak into gating code.
// NOTE: deny_unknown_fields was removed in walker-re-plan-wire-2.1 Wave 5 task 35.
// Two Phase-3 knobs (`market_dispatch_threshold_queue_depth`, `market_dispatch_eager`)
// were removed from the struct when the walker retired them. Legacy YAML rows that
// still carry those keys must silently deserialize — the fields are no longer read,
// but rejecting them would break rollout for any operator whose persisted contribution
// was written before Wave 5. Any other unknown field is a caller bug we'd like to catch,
// but deny_unknown_fields can't distinguish the two cases. Tradeoff accepted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComputeParticipationPolicy {
    pub schema_type: String,
    pub mode: ComputeParticipationMode,

    // The 8 projectable booleans. Absent means "project from mode"; present
    // means "explicit override, ignore mode preset."
    //
    // DO NOT READ DIRECTLY FOR GATING — call `effective_booleans()` instead.
    // See the CONSUMER WARNING in the struct-level doc above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_fleet_dispatch: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_fleet_serving: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_market_dispatch: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_market_visibility: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_storage_pulling: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_storage_hosting: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_relay_usage: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_relay_serving: Option<bool>,

    // Always-explicit. Defaults to false on any path that omits it.
    #[serde(default)]
    pub allow_serving_while_degraded: bool,

    // ── Phase 3 requester-side knobs ────────────────────────────────
    //
    // These gate the "should this inference call go to the compute
    // market?" decision in `call_model_unified`. They are NOT
    // projectable from `mode` — operational tuning, not intent.
    //
    // All three default via `serde(default)` so legacy YAMLs without
    // them continue to deserialize cleanly. Getter fallbacks live in
    // `effective_booleans` via a parallel struct access pattern (see
    // MarketDispatchKnobs below).

    /// Wall-clock budget for a single market dispatch end-to-end
    /// (match + fill + await-push + fallback-poll-on-timeout). On
    /// timeout, the node falls back to local inference.
    ///
    /// Default is 900_000ms (15 min) — sized so real cross-node
    /// inference on a 26B+ local-GPU provider has ample headroom and
    /// walker doesn't race against Wire's own `dispatch_deadline_at`.
    /// A 60s default was observed to time out regularly against real
    /// BEHEM latencies (39-49s observed), causing walker retry loops,
    /// orphaned `/purchase` deposits, and `job_not_found` on `/fill`
    /// when the retry raced Wire's `purchase_expiry` cron.
    ///
    /// Operators tuning for interactive-only workloads can supersede
    /// `compute_participation_policy` with a lower value.
    ///
    /// TODO (canonical): remove this ceiling entirely in favor of
    /// awaiting until `purchase_response.dispatch_deadline_at` + grace,
    /// so Wire owns the deadline and walker has no independent timer.
    #[serde(default = "default_market_dispatch_max_wait_ms")]
    pub market_dispatch_max_wait_ms: u64,
}

fn default_market_dispatch_max_wait_ms() -> u64 {
    900_000
}

/// Resolved booleans after applying mode projection + explicit overrides.
/// This is what consuming code (admission gates, offer publication, LLM
/// dispatch selection) should read, NOT the raw `Option<bool>` fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectiveParticipationPolicy {
    pub mode: ComputeParticipationMode,
    pub allow_fleet_dispatch: bool,
    pub allow_fleet_serving: bool,
    pub allow_market_dispatch: bool,
    pub allow_market_visibility: bool,
    pub allow_storage_pulling: bool,
    pub allow_storage_hosting: bool,
    pub allow_relay_usage: bool,
    pub allow_relay_serving: bool,
    pub allow_serving_while_degraded: bool,

    // Phase 3 operational knob — passed through unchanged from the
    // raw policy (no projection). See `ComputeParticipationPolicy`
    // for semantics.
    pub market_dispatch_max_wait_ms: u64,
}

/// Per DD-I: compute the 8 projectable booleans implied by a mode preset.
/// Returned in the order
/// `(fleet_dispatch, fleet_serving, market_dispatch, market_visibility,
///   storage_pulling, storage_hosting, relay_usage, relay_serving)`
/// — dispatch/usage pair + serving/hosting pair across fleet/compute/
/// storage/relay.
///
/// The projection follows the spec verbatim:
///   - `coordinator`: all dispatch/usage = true; all serving/hosting +
///     market_visibility + relay_serving = false.
///   - `hybrid`: all 8 = true.
///   - `worker`: all serving/hosting + market_visibility + relay_serving =
///     true; all dispatch/usage = false.
pub fn project_mode(mode: ComputeParticipationMode) -> (bool, bool, bool, bool, bool, bool, bool, bool) {
    match mode {
        ComputeParticipationMode::Coordinator => (
            true,  // allow_fleet_dispatch
            false, // allow_fleet_serving
            true,  // allow_market_dispatch
            false, // allow_market_visibility
            true,  // allow_storage_pulling
            false, // allow_storage_hosting
            true,  // allow_relay_usage
            false, // allow_relay_serving
        ),
        ComputeParticipationMode::Hybrid => (true, true, true, true, true, true, true, true),
        ComputeParticipationMode::Worker => (
            false, // allow_fleet_dispatch
            true,  // allow_fleet_serving
            false, // allow_market_dispatch
            true,  // allow_market_visibility
            false, // allow_storage_pulling
            true,  // allow_storage_hosting
            false, // allow_relay_usage
            true,  // allow_relay_serving
        ),
    }
}

impl ComputeParticipationPolicy {
    /// Resolve the policy to concrete booleans. For each of the 8
    /// projectable fields: explicit `Some(v)` wins; `None` takes the
    /// mode's projection. `allow_serving_while_degraded` is copied
    /// through unchanged.
    pub fn effective_booleans(&self) -> EffectiveParticipationPolicy {
        let (fd, fs, md, mv, sp, sh, ru, rs) = project_mode(self.mode);
        EffectiveParticipationPolicy {
            mode: self.mode,
            allow_fleet_dispatch: self.allow_fleet_dispatch.unwrap_or(fd),
            allow_fleet_serving: self.allow_fleet_serving.unwrap_or(fs),
            allow_market_dispatch: self.allow_market_dispatch.unwrap_or(md),
            allow_market_visibility: self.allow_market_visibility.unwrap_or(mv),
            allow_storage_pulling: self.allow_storage_pulling.unwrap_or(sp),
            allow_storage_hosting: self.allow_storage_hosting.unwrap_or(sh),
            allow_relay_usage: self.allow_relay_usage.unwrap_or(ru),
            allow_relay_serving: self.allow_relay_serving.unwrap_or(rs),
            allow_serving_while_degraded: self.allow_serving_while_degraded,
            market_dispatch_max_wait_ms: self.market_dispatch_max_wait_ms,
        }
    }
}

impl Default for ComputeParticipationPolicy {
    /// Default: hybrid mode with compute-market requester and fleet
    /// participation on for fresh installs. Per the operational purpose
    /// lock (docs/plans/call-model-unified-market-integration.md §Purpose):
    /// a GPU-less tester should build a pyramid via the network without
    /// seeing a market word. That demands compute market dispatch be
    /// on-by-default for fresh installs — the cooperative network is
    /// the point, not an opt-in.
    ///
    /// Persisted explicit policies always win via `effective_booleans`'s
    /// `.unwrap_or(projection)` — existing operators with a persisted
    /// contribution aren't affected by changes here.
    ///
    /// Design: the four requester/visibility/storage/relay capabilities
    /// that follow the mode projection use `None` so the Hybrid/Worker/
    /// Coordinator projection applies naturally. Operators who pick
    /// Worker mode later in Settings get worker semantics without re-
    /// persisting every toggle.
    fn default() -> Self {
        Self {
            schema_type: "compute_participation_policy".to_string(),
            mode: ComputeParticipationMode::Hybrid,
            // Fleet: participate (matches shipped default).
            allow_fleet_dispatch: Some(true),
            allow_fleet_serving: Some(true),
            // Compute market: follow mode projection (Hybrid → true).
            // Fresh installs get cooperative-network-on; persisted explicit
            // `Some(false)` still wins via unwrap_or in effective_booleans.
            allow_market_dispatch: None,
            allow_market_visibility: None,
            // Storage market: off by default — S1 hasn't shipped, but
            // when it does, operators opt in explicitly.
            allow_storage_pulling: Some(false),
            allow_storage_hosting: Some(false),
            // Relay market: off by default — R1 hasn't shipped, but
            // when it does, operators opt in explicitly.
            allow_relay_usage: Some(false),
            allow_relay_serving: Some(false),
            // Operational safety: off by default.
            allow_serving_while_degraded: false,
            // Phase 3 dispatch wall-clock.
            market_dispatch_max_wait_ms: default_market_dispatch_max_wait_ms(),
        }
    }
}

/// Read the current compute participation policy contribution, or return a
/// default that preserves today's fleet semantics.
pub fn get_compute_participation_policy(conn: &Connection) -> Result<ComputeParticipationPolicy> {
    match load_active_config_contribution(conn, "compute_participation_policy", None)? {
        Some(contrib) => Ok(serde_yaml::from_str(&contrib.yaml_content)?),
        None => Ok(ComputeParticipationPolicy::default()),
    }
}

/// Create or supersede the compute participation policy contribution.
pub fn set_compute_participation_policy(
    conn: &mut Connection,
    bus: &Arc<BuildEventBus>,
    policy: &ComputeParticipationPolicy,
) -> Result<()> {
    let yaml_str = serde_yaml::to_string(policy)?;
    let prior = load_active_config_contribution(conn, "compute_participation_policy", None)?;
    if let Some(prior_contrib) = prior {
        supersede_config_contribution(
            conn,
            &prior_contrib.contribution_id,
            &yaml_str,
            "compute participation policy updated",
            "user",
            Some("user"),
        )?;
    } else {
        create_config_contribution(
            conn,
            "compute_participation_policy",
            None,
            &yaml_str,
            Some("compute participation policy created"),
            "user",
            Some("user"),
            "active",
        )?;
    }
    if let Some(contrib) = load_active_config_contribution(conn, "compute_participation_policy", None)?
    {
        sync_config_to_operational(conn, bus, &contrib)?;
    }
    Ok(())
}

/// Read the current experimental territory contribution.
/// Returns the YAML content as a JSON value, or a default if none exists.
pub fn get_experimental_territory(conn: &Connection) -> Result<serde_json::Value> {
    match load_active_config_contribution(conn, "experimental_territory", None)? {
        Some(contrib) => {
            let val: serde_json::Value = serde_yaml::from_str(&contrib.yaml_content)?;
            Ok(val)
        }
        None => Ok(default_experimental_territory()),
    }
}

fn default_experimental_territory() -> serde_json::Value {
    serde_json::json!({
        "schema_type": "experimental_territory",
        "dimensions": {
            "model_selection": { "status": "locked" },
            "context_limit": { "status": "locked" },
            "concurrency": { "status": "locked" }
        }
    })
}

/// Set the experimental territory. Creates or supersedes the contribution.
pub fn set_experimental_territory(
    conn: &mut Connection,
    bus: &Arc<BuildEventBus>,
    territory_json: serde_json::Value,
) -> Result<()> {
    let yaml_str = serde_yaml::to_string(&territory_json)?;
    let prior = load_active_config_contribution(conn, "experimental_territory", None)?;
    if let Some(prior_contrib) = prior {
        supersede_config_contribution(
            conn,
            &prior_contrib.contribution_id,
            &yaml_str,
            "experimental territory updated",
            "user",
            Some("user"),
        )?;
    } else {
        create_config_contribution(
            conn,
            "experimental_territory",
            None,
            &yaml_str,
            Some("experimental territory created"),
            "user",
            Some("user"),
            "active",
        )?;
    }
    // Sync (no-op for this type, but fires ConfigSynced event).
    if let Some(contrib) = load_active_config_contribution(conn, "experimental_territory", None)? {
        sync_config_to_operational(conn, bus, &contrib)?;
    }
    Ok(())
}

// ── Cascade model field rebuild ─────────────────────────────────────────────

/// After a provider-registry refresh (local mode toggle, tier routing
/// contribution apply), re-resolve the cascade model fields on the live
/// `LlmConfig` from the current tier routing table. Ensures
/// `call_model_unified`'s cascade sends the correct model name for
/// whichever provider is now active.
///
/// Was previously in main.rs; moved here so HTTP route handlers can
/// call the same logic without depending on the binary's `SharedState`
/// alias.
pub async fn rebuild_cascade_from_registry(pyramid: &std::sync::Arc<super::PyramidState>) {
    let reg = &pyramid.provider_registry;
    let mut live = pyramid.config.write().await;
    if let Ok(r) = reg.resolve_tier("mid", None, None, None) {
        live.primary_model = r.tier.model_id.clone();
        if let Some(limit) = r.tier.context_limit {
            live.primary_context_limit = limit;
        }
    }
    if let Ok(r) = reg.resolve_tier("high", None, None, None) {
        live.fallback_model_1 = r.tier.model_id.clone();
        if let Some(limit) = r.tier.context_limit {
            live.fallback_1_context_limit = limit;
        }
    }
    if let Ok(r) = reg.resolve_tier("max", None, None, None) {
        live.fallback_model_2 = r.tier.model_id.clone();
    }
    tracing::info!(
        primary = %live.primary_model,
        fallback_1 = %live.fallback_model_1,
        fallback_2 = %live.fallback_model_2,
        "rebuilt cascade model fields from tier routing",
    );
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(deprecated)] // tests exercise serde shape incl. deprecated fields.
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
                context_override: None,
                concurrency_override: None,
                restore_dispatch_policy_contribution_id: None,
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
                context_override: None,
                concurrency_override: None,
                restore_dispatch_policy_contribution_id: None,
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

    // ── Phase 2 WS1a: ComputeParticipationPolicy (DD-I canonical 10 fields) ──

    #[test]
    fn compute_participation_policy_default_matches_bundled_yaml() {
        // Default must match the bundled YAML on all projectable fields.
        // Current stance (see Default impl docs): compute-market requester
        // defaults are None so Hybrid projection applies — cooperative
        // network on for fresh installs per the operational purpose lock.
        // Storage + relay stay explicitly off pending S1/R1.
        let p = ComputeParticipationPolicy::default();
        assert_eq!(p.mode, ComputeParticipationMode::Hybrid);
        assert_eq!(p.allow_fleet_dispatch, Some(true));
        assert_eq!(p.allow_fleet_serving, Some(true));
        // Market requester/visibility: None → Hybrid projection → true in effective_booleans.
        assert_eq!(p.allow_market_dispatch, None);
        assert_eq!(p.allow_market_visibility, None);
        assert_eq!(p.allow_storage_pulling, Some(false));
        assert_eq!(p.allow_storage_hosting, Some(false));
        assert_eq!(p.allow_relay_usage, Some(false));
        assert_eq!(p.allow_relay_serving, Some(false));
        assert!(!p.allow_serving_while_degraded);
    }

    #[test]
    fn compute_participation_policy_default_effective_enables_market() {
        // Critical purpose-lock assertion: a fresh install's default
        // policy, after effective_booleans projection, must have
        // allow_market_dispatch=true. Otherwise GPU-less testers hit a
        // dead-end gate on every call.
        let p = ComputeParticipationPolicy::default();
        let eff = p.effective_booleans();
        assert!(eff.allow_market_dispatch,
            "fresh install must have market dispatch ENABLED by default (purpose lock)");
    }

    #[test]
    fn project_mode_coordinator_matches_dd_i_spec() {
        // DD-I: coordinator = all *_dispatch + *_usage = true;
        // all *_serving + *_hosting + market_visibility + relay_serving = false.
        let (fd, fs, md, mv, sp, sh, ru, rs) =
            project_mode(ComputeParticipationMode::Coordinator);
        assert!(fd, "coordinator.allow_fleet_dispatch");
        assert!(!fs, "coordinator.allow_fleet_serving");
        assert!(md, "coordinator.allow_market_dispatch");
        assert!(!mv, "coordinator.allow_market_visibility");
        assert!(sp, "coordinator.allow_storage_pulling");
        assert!(!sh, "coordinator.allow_storage_hosting");
        assert!(ru, "coordinator.allow_relay_usage");
        assert!(!rs, "coordinator.allow_relay_serving");
    }

    #[test]
    fn project_mode_hybrid_matches_dd_i_spec() {
        // DD-I: hybrid = all 8 projectable booleans = true.
        let (fd, fs, md, mv, sp, sh, ru, rs) =
            project_mode(ComputeParticipationMode::Hybrid);
        assert!(fd && fs && md && mv && sp && sh && ru && rs,
            "hybrid must project all 8 booleans to true");
    }

    #[test]
    fn project_mode_worker_matches_dd_i_spec() {
        // DD-I: worker = all *_serving + *_hosting + market_visibility +
        // relay_serving = true; all *_dispatch + *_usage = false.
        let (fd, fs, md, mv, sp, sh, ru, rs) =
            project_mode(ComputeParticipationMode::Worker);
        assert!(!fd, "worker.allow_fleet_dispatch");
        assert!(fs, "worker.allow_fleet_serving");
        assert!(!md, "worker.allow_market_dispatch");
        assert!(mv, "worker.allow_market_visibility");
        assert!(!sp, "worker.allow_storage_pulling");
        assert!(sh, "worker.allow_storage_hosting");
        assert!(!ru, "worker.allow_relay_usage");
        assert!(rs, "worker.allow_relay_serving");
    }

    #[test]
    fn effective_booleans_explicit_overrides_mode() {
        // DD-I: when explicit booleans are set, they override the mode
        // projection. An operator on "worker" mode who explicitly sets
        // allow_fleet_dispatch = true (e.g. for a temporary burst)
        // should get fleet_dispatch = true in the effective policy even
        // though worker mode's projection says false.
        let p = ComputeParticipationPolicy {
            schema_type: "compute_participation_policy".into(),
            mode: ComputeParticipationMode::Worker,
            allow_fleet_dispatch: Some(true),   // explicit override
            allow_fleet_serving: None,          // project from worker
            allow_market_dispatch: None,
            allow_market_visibility: None,
            allow_storage_pulling: None,
            allow_storage_hosting: None,
            allow_relay_usage: None,
            allow_relay_serving: None,
            allow_serving_while_degraded: false,
            market_dispatch_max_wait_ms: 60_000,
        };
        let eff = p.effective_booleans();
        assert!(eff.allow_fleet_dispatch, "explicit true must win over worker projection");
        assert!(eff.allow_fleet_serving, "None projects to worker default (true)");
        assert!(!eff.allow_market_dispatch, "None projects to worker default (false)");
        assert!(eff.allow_market_visibility, "None projects to worker default (true)");
    }

    #[test]
    fn effective_booleans_all_none_uses_pure_projection() {
        // With every projectable boolean = None, effective_booleans
        // should match project_mode() exactly. This is the "operator
        // sent mode=X and nothing else" ergonomic path.
        for mode in [
            ComputeParticipationMode::Coordinator,
            ComputeParticipationMode::Hybrid,
            ComputeParticipationMode::Worker,
        ] {
            let p = ComputeParticipationPolicy {
                schema_type: "compute_participation_policy".into(),
                mode,
                allow_fleet_dispatch: None,
                allow_fleet_serving: None,
                allow_market_dispatch: None,
                allow_market_visibility: None,
                allow_storage_pulling: None,
                allow_storage_hosting: None,
                allow_relay_usage: None,
                allow_relay_serving: None,
                allow_serving_while_degraded: false,
                market_dispatch_max_wait_ms: 60_000,
            };
            let eff = p.effective_booleans();
            let (fd, fs, md, mv, sp, sh, ru, rs) = project_mode(mode);
            assert_eq!(eff.allow_fleet_dispatch, fd);
            assert_eq!(eff.allow_fleet_serving, fs);
            assert_eq!(eff.allow_market_dispatch, md);
            assert_eq!(eff.allow_market_visibility, mv);
            assert_eq!(eff.allow_storage_pulling, sp);
            assert_eq!(eff.allow_storage_hosting, sh);
            assert_eq!(eff.allow_relay_usage, ru);
            assert_eq!(eff.allow_relay_serving, rs);
        }
    }

    #[test]
    fn policy_yaml_roundtrip_preserves_none_and_some() {
        // Serialize with None fields → absent in YAML. Deserialize
        // absent fields → None again. Serialize with Some(v) → present
        // with value. Deserialize present → Some(v). The
        // `skip_serializing_if = "Option::is_none"` attribute is what
        // keeps None from rendering as `~` or `null` in YAML.
        let p = ComputeParticipationPolicy {
            schema_type: "compute_participation_policy".into(),
            mode: ComputeParticipationMode::Hybrid,
            allow_fleet_dispatch: Some(true),
            allow_fleet_serving: None,
            allow_market_dispatch: Some(false),
            allow_market_visibility: None,
            allow_storage_pulling: None,
            allow_storage_hosting: None,
            allow_relay_usage: None,
            allow_relay_serving: None,
            allow_serving_while_degraded: true,
            market_dispatch_max_wait_ms: 60_000,
        };
        let yaml = serde_yaml::to_string(&p).unwrap();
        // None fields are absent — no stray `null` or `~`.
        assert!(!yaml.contains("allow_fleet_serving"),
            "None field must not serialize:\n{yaml}");
        assert!(!yaml.contains("allow_market_visibility"),
            "None field must not serialize:\n{yaml}");
        // Some fields render with their value.
        assert!(yaml.contains("allow_fleet_dispatch: true"));
        assert!(yaml.contains("allow_market_dispatch: false"));
        // Roundtrip preserves the mix.
        let back: ComputeParticipationPolicy = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn policy_yaml_with_only_schema_type_and_mode_deserializes_with_all_none() {
        // Operator-facing ergonomic: a YAML with just the two required
        // fields (`schema_type` + `mode`) must parse — every projectable
        // boolean defaults to None, `allow_serving_while_degraded`
        // defaults to false. Exercises the full 3-mode matrix to make
        // sure projection hits every branch.
        for (mode_str, mode_enum) in [
            ("coordinator", ComputeParticipationMode::Coordinator),
            ("hybrid", ComputeParticipationMode::Hybrid),
            ("worker", ComputeParticipationMode::Worker),
        ] {
            let yaml = format!(
                "schema_type: compute_participation_policy\nmode: {mode_str}\n"
            );
            let p: ComputeParticipationPolicy = serde_yaml::from_str(&yaml).unwrap();
            assert_eq!(p.mode, mode_enum);
            assert_eq!(p.allow_fleet_dispatch, None);
            assert_eq!(p.allow_fleet_serving, None);
            assert_eq!(p.allow_market_dispatch, None);
            assert_eq!(p.allow_market_visibility, None);
            assert_eq!(p.allow_storage_pulling, None);
            assert_eq!(p.allow_storage_hosting, None);
            assert_eq!(p.allow_relay_usage, None);
            assert_eq!(p.allow_relay_serving, None);
            assert!(!p.allow_serving_while_degraded);
            // Effective values must come from the projection for this mode.
            let eff = p.effective_booleans();
            let (fd, fs, md, mv, sp, sh, ru, rs) = project_mode(mode_enum);
            assert_eq!(eff.allow_fleet_dispatch, fd);
            assert_eq!(eff.allow_fleet_serving, fs);
            assert_eq!(eff.allow_market_dispatch, md);
            assert_eq!(eff.allow_market_visibility, mv);
            assert_eq!(eff.allow_storage_pulling, sp);
            assert_eq!(eff.allow_storage_hosting, sh);
            assert_eq!(eff.allow_relay_usage, ru);
            assert_eq!(eff.allow_relay_serving, rs);
        }
    }

    #[test]
    fn policy_yaml_silently_absorbs_retired_walker_knobs() {
        // Walker Wave 5 removed `market_dispatch_threshold_queue_depth` and
        // `market_dispatch_eager` from the struct. Legacy persisted rows
        // still carry those keys and must deserialize cleanly — rejecting
        // them would break rollout for any operator whose policy was
        // written before Wave 5. `deny_unknown_fields` was removed on the
        // struct to permit this; the tradeoff is that typos like
        // `allow_market_visiblity` now silently no-op instead of erroring.
        let legacy_walker_yaml = "schema_type: compute_participation_policy\n\
                                  mode: hybrid\n\
                                  market_dispatch_threshold_queue_depth: 10\n\
                                  market_dispatch_eager: true\n\
                                  market_dispatch_max_wait_ms: 60000\n";
        let p: ComputeParticipationPolicy =
            serde_yaml::from_str(legacy_walker_yaml).expect("retired knobs absorb silently");
        assert_eq!(p.mode, ComputeParticipationMode::Hybrid);
        assert_eq!(p.market_dispatch_max_wait_ms, 60_000);
    }

    #[test]
    fn effective_booleans_copies_allow_serving_while_degraded_unchanged() {
        // Per DD-I: `allow_serving_while_degraded` is NEVER projected.
        // It's an operator-configurable safety knob that must pass
        // through `effective_booleans()` byte-for-byte.
        let mut p = ComputeParticipationPolicy::default();
        p.allow_serving_while_degraded = true;
        assert!(p.effective_booleans().allow_serving_while_degraded);
        p.allow_serving_while_degraded = false;
        assert!(!p.effective_booleans().allow_serving_while_degraded);
    }

    #[test]
    fn policy_yaml_from_legacy_six_field_shape_still_parses() {
        // Backwards-compat: contributions created before WS1a had the
        // pre-extension 6-field shape. Those YAMLs must still parse
        // (missing fields → None → project from mode).
        let legacy_yaml = "schema_type: compute_participation_policy\n\
                           mode: hybrid\n\
                           allow_market_visibility: false\n\
                           allow_serving_while_degraded: false\n\
                           allow_fleet_dispatch: true\n\
                           allow_fleet_serving: true\n";
        let p: ComputeParticipationPolicy = serde_yaml::from_str(legacy_yaml).unwrap();
        assert_eq!(p.mode, ComputeParticipationMode::Hybrid);
        assert_eq!(p.allow_fleet_dispatch, Some(true));
        assert_eq!(p.allow_fleet_serving, Some(true));
        assert_eq!(p.allow_market_visibility, Some(false));
        // New fields absent in legacy → None → project from hybrid → true.
        assert_eq!(p.allow_market_dispatch, None);
        assert_eq!(p.allow_storage_pulling, None);
        assert_eq!(p.allow_storage_hosting, None);
        assert_eq!(p.allow_relay_usage, None);
        assert_eq!(p.allow_relay_serving, None);
        let eff = p.effective_booleans();
        assert!(eff.allow_market_visibility == false,
            "explicit false in legacy YAML wins over hybrid projection");
        // Unset new fields inherit hybrid's "all true" projection.
        // NOTE: this means a node with the old 6-field contribution
        // would, after upgrading to WS1a code, project market_dispatch
        // + storage_* + relay_* to true per DD-I hybrid. If that
        // behavior is not desired, the operator or the first-boot
        // migration code should rewrite the contribution to include
        // explicit booleans for the new 5 fields.
        assert!(eff.allow_market_dispatch);
        assert!(eff.allow_storage_hosting);
        assert!(eff.allow_relay_serving);
    }

    #[test]
    fn bundled_dispatch_policy_seed_has_routing_rules_with_providers() {
        // Regression guard: local_mode disable path falls back to
        // bundled-dispatch_policy-default-v1 when no prior policy was
        // captured. That fallback must contain `routing_rules` with a
        // non-empty `route_to` — otherwise the walker's resolve_route
        // returns zero providers and every subsequent build fails
        // "no viable route" (0m 0s | 0/0 steps).
        //
        // The old hardcoded fallback YAML was gutted (empty
        // provider_pools, no routing_rules) — this test would have
        // caught that regression at unit-test time.
        let manifest = crate::pyramid::wire_migration::load_bundled_manifest().unwrap();
        let seed = manifest
            .contributions
            .iter()
            .find(|e| e.contribution_id == "bundled-dispatch_policy-default-v1")
            .expect("bundled manifest must contain dispatch_policy-default-v1");

        // Raw YAML check: the two tokens the walker needs.
        assert!(
            seed.yaml_content.contains("routing_rules:"),
            "bundled dispatch_policy seed is missing `routing_rules:` — walker will \
             resolve an empty route and every build will fail"
        );
        assert!(
            seed.yaml_content.contains("route_to:"),
            "bundled dispatch_policy seed is missing `route_to:` — walker has no \
             entries to iterate"
        );

        // Structural check: parse + confirm route_to is non-empty.
        let parsed: crate::pyramid::dispatch_policy::DispatchPolicyYaml =
            serde_yaml::from_str(&seed.yaml_content).expect("bundled seed must be valid YAML");
        assert!(
            !parsed.routing_rules.is_empty(),
            "bundled seed must ship with at least one routing_rule"
        );
        let default_rule = parsed
            .routing_rules
            .iter()
            .find(|r| r.name == "default")
            .expect("bundled seed must have a `default` routing rule");
        assert!(
            !default_rule.route_to.is_empty(),
            "bundled seed's default rule must have at least one route_to entry"
        );

        // Constructor check: the full pipeline used by
        // sync_config_to_operational must build a DispatchPolicy with
        // non-empty rules.
        let policy = crate::pyramid::dispatch_policy::DispatchPolicy::from_yaml(&parsed);
        assert!(
            !policy.rules.is_empty(),
            "constructed DispatchPolicy must have rules the walker can resolve"
        );
    }

    #[test]
    fn enable_disable_cycle_preserves_authored_dispatch_policy() {
        // Pillar 37 regression guard (2026-04-21): the enable/disable
        // cycle must NEVER supersede the authored `dispatch_policy`
        // contribution. Local Mode is a runtime toggle; the effective
        // policy is derived by `apply_local_mode_overlay` at the
        // ConfigSynced load point, not by writing a shadow contribution.
        //
        // Test: seed a custom dispatch_policy, commit enable, commit
        // disable, then read the active dispatch_policy contribution
        // back and assert its id + yaml are byte-identical to the
        // authored seed.
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::pyramid::db::init_pyramid_db(&conn).unwrap();
        let bus = Arc::new(crate::pyramid::event_bus::BuildEventBus::new());
        let tmp = tempfile::TempDir::new().unwrap();
        let store = Arc::new(
            crate::pyramid::credentials::CredentialStore::load(tmp.path()).unwrap(),
        );
        std::mem::forget(tmp);
        let registry = ProviderRegistry::new(store);

        // Seed an authored dispatch_policy that mirrors the production
        // bundled seed: market → fleet → openrouter → ollama-local.
        let authored_yaml = "version: 1\n\
                             provider_pools:\n  openrouter: { concurrency: 20 }\n  ollama-local: { concurrency: 1 }\n\
                             routing_rules:\n  - name: default\n    match_config: {}\n    route_to:\n      - { provider_id: market }\n      - { provider_id: fleet }\n      - { provider_id: openrouter, model_id: \"openai/gpt-4o-mini\" }\n      - { provider_id: ollama-local, is_local: true }\n";
        let authored_id = crate::pyramid::config_contributions::create_config_contribution(
            &conn,
            "dispatch_policy",
            None,
            authored_yaml,
            Some("authored by operator"),
            "test",
            Some("user"),
            "active",
        )
        .unwrap();

        // Also seed a tier_routing so commit_enable_local_mode's
        // prior_tier_contribution read succeeds. Minimal shape.
        crate::pyramid::config_contributions::create_config_contribution(
            &conn,
            "tier_routing",
            None,
            "schema_type: tier_routing\nentries:\n  - tier_name: mid\n    provider_id: openrouter\n    model_id: x\n",
            None,
            "test",
            Some("user"),
            "active",
        )
        .unwrap();

        registry.load_from_db(&conn).unwrap();

        // Commit enable with a synthetic plan (skips the /api/tags probe).
        let plan = EnableLocalModePlan {
            base_url: "http://localhost:11434/v1".into(),
            chosen_model: "llama3".into(),
            detected_context: 32_000,
            available_models: vec!["llama3".into()],
        };
        commit_enable_local_mode(&mut conn, &bus, &registry, plan).unwrap();

        // Post-enable: state flipped, authored dispatch_policy still
        // the active contribution with unchanged yaml.
        let row_after_enable = load_local_mode_state(&conn).unwrap();
        assert!(row_after_enable.enabled, "enable did not flip state");
        let dp_after_enable =
            load_active_config_contribution(&conn, "dispatch_policy", None)
                .unwrap()
                .expect("authored dispatch_policy still active after enable");
        assert_eq!(
            dp_after_enable.contribution_id, authored_id,
            "enable must not supersede the authored dispatch_policy"
        );
        assert_eq!(
            dp_after_enable.yaml_content.trim(),
            authored_yaml.trim(),
            "authored dispatch_policy YAML must be byte-identical after enable"
        );

        // Commit disable.
        commit_disable_local_mode(&mut conn, &bus, &registry).unwrap();

        // Post-disable: state flipped back, authored dispatch_policy
        // STILL the active contribution with unchanged yaml.
        let row_after_disable = load_local_mode_state(&conn).unwrap();
        assert!(!row_after_disable.enabled, "disable did not flip state");
        let dp_after_disable =
            load_active_config_contribution(&conn, "dispatch_policy", None)
                .unwrap()
                .expect("authored dispatch_policy still active after disable");
        assert_eq!(
            dp_after_disable.contribution_id, authored_id,
            "disable must leave the authored dispatch_policy as-is"
        );
        assert_eq!(
            dp_after_disable.yaml_content.trim(),
            authored_yaml.trim(),
            "authored dispatch_policy YAML must survive the full toggle cycle"
        );
    }

    #[test]
    fn default_matches_bundled_contribution_yaml() {
        // The Rust Default impl and the bundled YAML in
        // src-tauri/assets/bundled_contributions.json must represent
        // the same conservative-fresh-install posture. If one changes
        // without the other, a fresh install would differ from what
        // the Rust path produces when no contribution is present.
        //
        // Parse the actual bundled manifest via `load_bundled_manifest`
        // so the test catches drift between Rust and JSON without
        // requiring the inline YAML to be kept in sync by hand.
        let manifest = crate::pyramid::wire_migration::load_bundled_manifest().unwrap();
        let bundled_default = manifest
            .contributions
            .iter()
            .find(|e| {
                e.schema_type == "compute_participation_policy"
                    && e.contribution_id
                        .starts_with("bundled-compute_participation_policy-default-")
            })
            .expect("bundled manifest must contain a compute_participation_policy default");
        let parsed: ComputeParticipationPolicy =
            serde_yaml::from_str(&bundled_default.yaml_content).unwrap();
        assert_eq!(parsed, ComputeParticipationPolicy::default());
    }
}
