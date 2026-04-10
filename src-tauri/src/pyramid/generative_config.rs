// pyramid/generative_config.rs — Phase 9: Generative Config Pattern.
//
// Canonical reference:
//   /Users/adamlevine/AI Project Files/agent-wire-node/docs/specs/generative-config-pattern.md
//     — primary spec (~422 lines)
//   /Users/adamlevine/AI Project Files/agent-wire-node/docs/specs/config-contribution-and-wire-sharing.md
//     — canonical IPC signatures + Notes Capture Lifecycle rules
//
// This module holds the Phase 9 backend logic for the generative
// config loop. The IPC handlers in `main.rs` are thin wrappers that
// call through to the functions defined here — this keeps main.rs
// focused on registration and argument shaping while the actual
// generation + refinement + accept flow lives in a domain module.
//
// Phase 9 scope (from the workstream brief):
//
//   1. `generate_config_from_intent` — intent → YAML via LLM, creates
//      a draft contribution that the user can edit/accept/refine.
//   2. `refine_config_with_note` — note + current YAML → new YAML via
//      LLM, creates a superseding draft contribution. Empty notes are
//      rejected at the IPC boundary (enforced via `validate_note`).
//   3. `accept_config_draft` — promotes a draft to active, runs the
//      Phase 4 `sync_config_to_operational` dispatcher, returns the
//      full response shape per the spec.
//   4. `active_config_for` — thin wrapper over
//      `load_active_config_contribution`.
//   5. `config_version_history_for` — thin wrapper over
//      `load_config_version_history`.
//   6. `list_config_schemas` — returns the schema registry's summary
//      list.
//
// Every LLM call goes through `call_model_unified_with_options_and_ctx`
// with a fully-populated `StepContext` (primitive = "config_generation"
// or "config_refinement"). This keeps Phase 6's cache working — a
// refinement that produces the same output for the same inputs hits
// the cache and saves the round-trip.
//
// Every contribution write goes through
// `create_config_contribution_with_metadata` + (on accept)
// `sync_config_to_operational_with_registry`. No operational-table
// shortcuts — this is the Phase 4 architectural contract.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::pyramid::config_contributions::{
    create_config_contribution_with_metadata, load_active_config_contribution,
    load_config_version_history, load_contribution_by_id,
    sync_config_to_operational_with_registry, validate_note, ConfigContribution,
};
use crate::pyramid::event_bus::BuildEventBus;
use crate::pyramid::llm::{call_model_unified_with_options_and_ctx, LlmCallOptions, LlmConfig};
use crate::pyramid::provider::ProviderRegistry;
use crate::pyramid::schema_registry::{ConfigSchemaSummary, SchemaRegistry};
use crate::pyramid::step_context::{compute_prompt_hash, StepContext};
use crate::pyramid::wire_native_metadata::{default_wire_native_metadata, WireMaturity};

// ── Response types ──────────────────────────────────────────────────

/// Response from `pyramid_generate_config`. Returns the new draft
/// contribution's ID + the generated YAML body so the frontend can
/// render it via the YAML-to-UI renderer without a follow-up load.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateConfigResponse {
    pub contribution_id: String,
    pub yaml_content: String,
    pub schema_type: String,
    pub version: u32,
}

/// Response from `pyramid_refine_config`. Returns the NEW contribution
/// created by the refinement (not the original), the refined YAML, and
/// the bumped version number (supersession chain depth).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefineConfigResponse {
    pub new_contribution_id: String,
    pub yaml_content: String,
    pub schema_type: String,
    pub version: u32,
}

/// Response from `pyramid_accept_config`. Full contribution state +
/// operational sync outcome. Per the canonical IPC signature in
/// `config-contribution-and-wire-sharing.md` → "IPC Contract" section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptConfigResponse {
    pub contribution_id: String,
    pub yaml_content: String,
    pub version: u32,
    pub triggering_note: String,
    pub status: String,
    pub wire_native_metadata: serde_json::Value,
    pub sync_result: SyncResult,
}

/// Operational sync outcome for `AcceptConfigResponse.sync_result`.
/// Reports the operational table that got the write + any reload
/// hooks that fired. Phase 9 populates these as best-effort
/// diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncResult {
    pub operational_table: String,
    pub reload_triggered: Vec<String>,
}

/// Response from `pyramid_active_config`. Per the spec, returns the
/// current active contribution plus its chain depth and provenance
/// fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveConfigResponse {
    pub contribution_id: String,
    pub yaml_content: String,
    pub version_chain_length: u32,
    pub created_at: String,
    pub triggering_note: Option<String>,
}

// ── LLM call plumbing ───────────────────────────────────────────────

/// Parameters passed to the LLM during generation or refinement.
/// Bundles the resolved skill body + schema JSON + intent + optional
/// current YAML + optional notes so the substituter + context builder
/// have everything in one place.
struct GenerationCallParams<'a> {
    skill_body: &'a str,
    schema_json: &'a str,
    intent: &'a str,
    current_yaml: Option<&'a str>,
    notes: Option<&'a str>,
    slug: Option<&'a str>,
    schema_type: &'a str,
    primitive: &'a str,
    step_name: &'a str,
    build_id: String,
}

/// Substitute the `{schema}`, `{intent}`, `{current_yaml}`, `{notes}`
/// placeholders in a generation skill body. Also handles the simple
/// `{if current_yaml}...{end}` / `{if notes}...{end}` block form —
/// when the corresponding value is absent, the block is removed; when
/// present, the block markers are stripped.
///
/// This is a deliberately simple string-replacement scheme — no Jinja2,
/// no handlebars. The Phase 9 brief explicitly calls out that a
/// heavier templating dep is out of scope.
fn substitute_prompt(
    template: &str,
    schema: &str,
    intent: &str,
    current_yaml: Option<&str>,
    notes: Option<&str>,
) -> String {
    let mut out = template.to_string();

    // Handle conditional blocks first so absent values don't leave
    // stray literals in the body.
    out = process_conditional_block(&out, "{if current_yaml}", "{end}", current_yaml.is_some());
    out = process_conditional_block(&out, "{if notes}", "{end}", notes.is_some());

    out = out.replace("{schema}", schema);
    out = out.replace("{intent}", intent);
    out = out.replace("{current_yaml}", current_yaml.unwrap_or(""));
    out = out.replace("{notes}", notes.unwrap_or(""));

    out
}

/// Process a `{if X}...{end}` block. When `keep == true` the markers
/// are stripped and the content between them is retained. When
/// `keep == false` the entire block (including markers) is removed.
/// Operates on the first occurrence only — Phase 9 prompts use each
/// conditional once.
fn process_conditional_block(
    input: &str,
    open_marker: &str,
    close_marker: &str,
    keep: bool,
) -> String {
    let Some(start) = input.find(open_marker) else {
        return input.to_string();
    };
    let after_open = start + open_marker.len();
    let Some(rel_end) = input[after_open..].find(close_marker) else {
        return input.to_string();
    };
    let end = after_open + rel_end;
    let after_close = end + close_marker.len();

    let mut out = String::with_capacity(input.len());
    out.push_str(&input[..start]);
    if keep {
        // Retain the inner content, trimming any leading/trailing
        // newline that follows the open marker for clean output.
        let inner = &input[after_open..end];
        out.push_str(inner.trim_start_matches('\n'));
    }
    out.push_str(&input[after_close..]);
    out
}

/// Call the LLM to generate a YAML document. Handles:
///   1. Prompt substitution
///   2. StepContext construction (with model resolution + prompt hash)
///   3. The cache-aware LLM entry point
///
/// Returns the raw LLM response content (the caller parses it as
/// YAML).
async fn call_generation_llm(
    llm_config: &LlmConfig,
    bus: &Arc<BuildEventBus>,
    provider_registry: &ProviderRegistry,
    db_path: &str,
    params: GenerationCallParams<'_>,
) -> Result<String> {
    let prompt_body = substitute_prompt(
        params.skill_body,
        params.schema_json,
        params.intent,
        params.current_yaml,
        params.notes,
    );

    // Resolve the model tier — generative config uses `synth_heavy`
    // by default since it's a synthesis task. The tier name is
    // hardcoded per the canonical Phase 3 naming; users can
    // supersede the generation skill to change tiers inline if they
    // prefer a different one.
    let tier = "synth_heavy";
    let resolved = provider_registry.resolve_tier(tier, None, None, None).ok();
    let (model_id, provider_id) = match resolved {
        Some(entry) => (entry.tier.model_id.clone(), entry.provider.id.clone()),
        None => {
            warn!(
                tier,
                "call_generation_llm: tier not resolved via registry; falling back to llm_config.primary_model"
            );
            (llm_config.primary_model.clone(), "openrouter".to_string())
        }
    };

    let prompt_hash = compute_prompt_hash(params.skill_body);
    let ctx = StepContext::new(
        params.slug.unwrap_or("global"),
        params.build_id.clone(),
        params.step_name,
        params.primitive,
        0,
        None,
        db_path,
    )
    .with_model_resolution(tier, model_id)
    .with_provider(provider_id)
    .with_prompt_hash(prompt_hash)
    .with_bus(bus.clone());

    debug!(
        schema_type = params.schema_type,
        primitive = params.primitive,
        build_id = %params.build_id,
        "call_generation_llm: calling LLM via cache-aware path"
    );

    // Every LLM call for Phase 9 goes through the Phase 6 cache-aware
    // entry point — Phase 9 MUST NOT call the legacy shim because
    // that bypasses the content-addressable cache. The `max_tokens`
    // argument is ignored inside the ctx-aware path (it resolves
    // effective max tokens from the model's context window minus
    // input).
    let response = call_model_unified_with_options_and_ctx(
        llm_config,
        Some(&ctx),
        "You are a configuration generator for Wire Node.",
        &prompt_body,
        0.2,
        4096,
        None,
        LlmCallOptions::default(),
    )
    .await?;

    Ok(response.content)
}

// ── Public entry points (called from main.rs IPC handlers) ──────────

/// Inputs loaded from the DB for a generation call. Separated from
/// the LLM call so the IPC handler can drop the DB lock before the
/// await point (rusqlite connections aren't Send across awaits).
#[derive(Debug, Clone)]
pub struct GenerationInputs {
    pub schema_type: String,
    pub slug: Option<String>,
    pub intent: String,
    pub skill_body: String,
    pub schema_json: String,
}

/// Load the inputs required for a fresh generation call. Runs
/// synchronously inside the DB lock; callers drop the lock before
/// invoking `run_generation_llm_call` with the returned inputs.
pub fn load_generation_inputs(
    conn: &Connection,
    schema_registry: &SchemaRegistry,
    schema_type: &str,
    slug: Option<&str>,
    intent: &str,
) -> Result<GenerationInputs> {
    let trimmed_intent = intent.trim();
    if trimmed_intent.is_empty() {
        return Err(anyhow!("intent must not be empty"));
    }

    let schema = schema_registry
        .get(schema_type)
        .ok_or_else(|| anyhow!("no active schema found for schema_type {schema_type:?}"))?;

    if schema.generation_skill_contribution_id.is_empty() {
        return Err(anyhow!(
            "no active generation skill contribution for schema_type {schema_type:?}"
        ));
    }
    let skill = load_contribution_by_id(conn, &schema.generation_skill_contribution_id)?
        .ok_or_else(|| anyhow!("generation skill contribution disappeared"))?;

    let definition = load_contribution_by_id(conn, &schema.schema_definition_contribution_id)?
        .ok_or_else(|| anyhow!("schema_definition contribution disappeared"))?;

    Ok(GenerationInputs {
        schema_type: schema_type.to_string(),
        slug: slug.map(|s| s.to_string()),
        intent: trimmed_intent.to_string(),
        skill_body: skill.yaml_content,
        schema_json: definition.yaml_content,
    })
}

/// Run the LLM call for an initial generation using loaded inputs.
/// The DB lock is NOT held while this runs — the IPC handler drops
/// the lock before calling. Returns the raw LLM output; the caller
/// parses + persists it.
pub async fn run_generation_llm_call(
    llm_config: &LlmConfig,
    bus: &Arc<BuildEventBus>,
    provider_registry: &ProviderRegistry,
    db_path: &str,
    inputs: &GenerationInputs,
) -> Result<String> {
    let build_id = format!("gen-{}", uuid::Uuid::new_v4());
    let params = GenerationCallParams {
        skill_body: &inputs.skill_body,
        schema_json: &inputs.schema_json,
        intent: &inputs.intent,
        current_yaml: None,
        notes: None,
        slug: inputs.slug.as_deref(),
        schema_type: &inputs.schema_type,
        primitive: "config_generation",
        step_name: "generate_config",
        build_id,
    };

    call_generation_llm(llm_config, bus, provider_registry, db_path, params).await
}

/// Persist a freshly-generated draft contribution. Runs inside the
/// writer DB lock after the LLM call. Returns the response shape.
pub fn persist_generated_draft(
    conn: &Connection,
    inputs: &GenerationInputs,
    llm_output: &str,
) -> Result<GenerateConfigResponse> {
    let yaml_content = extract_yaml_body(llm_output);

    // Parse as YAML to validate — Phase 9's safety net is "is this
    // parseable YAML", not structural validation. The `jsonschema`
    // crate is not in deps; full JSON Schema validation lands with
    // Phase 10 alongside the schema_migration flow.
    let _: serde_yaml::Value = serde_yaml::from_str(&yaml_content)
        .map_err(|e| anyhow!("generated YAML is not parseable: {e}; body: {yaml_content}"))?;

    let mut metadata = default_wire_native_metadata(&inputs.schema_type, inputs.slug.as_deref());
    metadata.maturity = WireMaturity::Draft;

    let contribution_id = create_config_contribution_with_metadata(
        conn,
        &inputs.schema_type,
        inputs.slug.as_deref(),
        &yaml_content,
        Some(&inputs.intent),
        "local",
        Some("generative_config"),
        "draft",
        &metadata,
    )?;

    info!(
        contribution_id,
        schema_type = %inputs.schema_type,
        slug = ?inputs.slug,
        "persist_generated_draft: created draft contribution"
    );

    Ok(GenerateConfigResponse {
        contribution_id,
        yaml_content,
        schema_type: inputs.schema_type.clone(),
        version: 1,
    })
}

/// Generate a new config contribution from a user intent string.
/// Convenience wrapper: loads inputs from the DB, calls the LLM,
/// persists the draft.
///
/// **Warning:** this function holds a `&Connection` across an async
/// LLM call and is therefore NOT `Send`-safe. The IPC handler in
/// `main.rs` uses the 3-phase form (`load_generation_inputs` →
/// `run_generation_llm_call` → `persist_generated_draft`) instead.
/// This wrapper is kept for tests + non-async call sites.
#[allow(clippy::too_many_arguments)]
pub async fn generate_config_from_intent(
    conn: &Connection,
    llm_config: &LlmConfig,
    bus: &Arc<BuildEventBus>,
    schema_registry: &SchemaRegistry,
    provider_registry: &ProviderRegistry,
    db_path: &str,
    schema_type: String,
    slug: Option<String>,
    intent: String,
) -> Result<GenerateConfigResponse> {
    let inputs = load_generation_inputs(conn, schema_registry, &schema_type, slug.as_deref(), &intent)?;
    let llm_output =
        run_generation_llm_call(llm_config, bus, provider_registry, db_path, &inputs).await?;
    persist_generated_draft(conn, &inputs, &llm_output)
}

/// Inputs loaded from the DB for a refinement call. Contains the
/// prior contribution, the resolved generation skill + schema
/// definition bodies, and the user-supplied current YAML + note.
#[derive(Debug, Clone)]
pub struct RefinementInputs {
    pub prior: ConfigContribution,
    pub skill_body: String,
    pub schema_json: String,
    pub current_yaml: String,
    pub note: String,
    pub intent: String,
}

/// Load the inputs required for a refinement call. Runs synchronously
/// inside the DB lock; callers drop the lock before invoking
/// `run_refinement_llm_call` with the returned inputs.
pub fn load_refinement_inputs(
    conn: &Connection,
    schema_registry: &SchemaRegistry,
    contribution_id: &str,
    current_yaml: &str,
    note: &str,
) -> Result<RefinementInputs> {
    validate_note(note).map_err(|e| anyhow!(e))?;

    let trimmed_current = current_yaml.trim();
    if trimmed_current.is_empty() {
        return Err(anyhow!("current_yaml must not be empty"));
    }

    let prior = load_contribution_by_id(conn, contribution_id)?
        .ok_or_else(|| anyhow!("contribution {contribution_id} not found"))?;

    let schema = schema_registry
        .get(&prior.schema_type)
        .ok_or_else(|| anyhow!("no active schema for {:?}", prior.schema_type))?;

    if schema.generation_skill_contribution_id.is_empty() {
        return Err(anyhow!(
            "no active generation skill for schema_type {:?}",
            prior.schema_type
        ));
    }
    let skill = load_contribution_by_id(conn, &schema.generation_skill_contribution_id)?
        .ok_or_else(|| anyhow!("generation skill contribution disappeared"))?;

    let definition = load_contribution_by_id(conn, &schema.schema_definition_contribution_id)?
        .ok_or_else(|| anyhow!("schema_definition contribution disappeared"))?;

    let intent = prior
        .triggering_note
        .clone()
        .unwrap_or_else(|| format!("refine existing {}", prior.schema_type));

    Ok(RefinementInputs {
        prior,
        skill_body: skill.yaml_content,
        schema_json: definition.yaml_content,
        current_yaml: trimmed_current.to_string(),
        note: note.trim().to_string(),
        intent,
    })
}

/// Run the LLM call for a refinement using loaded inputs. The DB
/// lock is NOT held while this runs.
pub async fn run_refinement_llm_call(
    llm_config: &LlmConfig,
    bus: &Arc<BuildEventBus>,
    provider_registry: &ProviderRegistry,
    db_path: &str,
    inputs: &RefinementInputs,
) -> Result<String> {
    let build_id = format!("refine-{}", uuid::Uuid::new_v4());
    let params = GenerationCallParams {
        skill_body: &inputs.skill_body,
        schema_json: &inputs.schema_json,
        intent: inputs.intent.trim(),
        current_yaml: Some(&inputs.current_yaml),
        notes: Some(&inputs.note),
        slug: inputs.prior.slug.as_deref(),
        schema_type: &inputs.prior.schema_type,
        primitive: "config_refinement",
        step_name: "refine_config",
        build_id,
    };

    call_generation_llm(llm_config, bus, provider_registry, db_path, params).await
}

/// Persist a refined draft contribution. Runs inside the writer DB
/// lock after the LLM call completes.
pub fn persist_refined_draft(
    conn: &mut Connection,
    inputs: &RefinementInputs,
    llm_output: &str,
) -> Result<RefineConfigResponse> {
    let yaml_content = extract_yaml_body(llm_output);

    let _: serde_yaml::Value = serde_yaml::from_str(&yaml_content)
        .map_err(|e| anyhow!("refined YAML is not parseable: {e}; body: {yaml_content}"))?;

    let new_id = create_draft_supersession(conn, &inputs.prior, &yaml_content, &inputs.note)?;

    info!(
        prior_contribution_id = %inputs.prior.contribution_id,
        new_contribution_id = %new_id,
        schema_type = %inputs.prior.schema_type,
        "persist_refined_draft: created draft supersession"
    );

    let history = load_config_version_history(
        conn,
        &inputs.prior.schema_type,
        inputs.prior.slug.as_deref(),
    )?;
    let version = (history.len() as u32) + 1;

    Ok(RefineConfigResponse {
        new_contribution_id: new_id,
        yaml_content,
        schema_type: inputs.prior.schema_type.clone(),
        version,
    })
}

/// Refine an existing contribution with a user note. Convenience
/// wrapper over the 3-phase form (`load_refinement_inputs` →
/// `run_refinement_llm_call` → `persist_refined_draft`). Kept for
/// tests — the IPC handler uses the 3-phase form directly so it can
/// drop the DB lock across the LLM await.
#[allow(clippy::too_many_arguments)]
pub async fn refine_config_with_note(
    conn: &mut Connection,
    llm_config: &LlmConfig,
    bus: &Arc<BuildEventBus>,
    schema_registry: &SchemaRegistry,
    provider_registry: &ProviderRegistry,
    db_path: &str,
    contribution_id: String,
    current_yaml: String,
    note: String,
) -> Result<RefineConfigResponse> {
    let inputs = load_refinement_inputs(conn, schema_registry, &contribution_id, &current_yaml, &note)?;
    let llm_output =
        run_refinement_llm_call(llm_config, bus, provider_registry, db_path, &inputs).await?;
    persist_refined_draft(conn, &inputs, &llm_output)
}

/// Inline transaction: mark the prior contribution as `superseded`
/// and create a new draft contribution that `supersedes_id → prior`.
///
/// Used by `refine_config_with_note` because `supersede_config_contribution`
/// forces the new row to `active`, which is wrong for the Phase 9
/// draft flow.
fn create_draft_supersession(
    conn: &mut Connection,
    prior: &ConfigContribution,
    new_yaml_content: &str,
    triggering_note: &str,
) -> Result<String> {
    if triggering_note.trim().is_empty() {
        return Err(anyhow!("triggering_note must not be empty"));
    }

    let tx = conn.transaction()?;

    // Carry forward the prior's canonical metadata with maturity reset
    // to Draft (matching supersede_config_contribution semantics).
    let mut new_metadata =
        crate::pyramid::wire_native_metadata::WireNativeMetadata::from_json(
            &prior.wire_native_metadata_json,
        )
        .unwrap_or_else(|_| default_wire_native_metadata(&prior.schema_type, prior.slug.as_deref()));
    new_metadata.maturity = WireMaturity::Draft;

    let metadata_json = new_metadata
        .to_json()
        .map_err(|e| anyhow!("failed to serialize wire_native_metadata: {e}"))?;

    let new_id = uuid::Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO pyramid_config_contributions (
            contribution_id, slug, schema_type, yaml_content,
            wire_native_metadata_json, wire_publication_state_json,
            supersedes_id, superseded_by_id, triggering_note,
            status, source, wire_contribution_id, created_by, accepted_at
         ) VALUES (
            ?1, ?2, ?3, ?4,
            ?5, '{}',
            ?6, NULL, ?7,
            'draft', 'local', NULL, 'generative_config', NULL
         )",
        rusqlite::params![
            new_id,
            prior.slug,
            prior.schema_type,
            new_yaml_content,
            metadata_json,
            prior.contribution_id,
            triggering_note,
        ],
    )?;

    // Mark the prior row as superseded (only if it was active — draft
    // supersessions of drafts are also valid, in which case we leave
    // the prior draft status alone but update the link).
    tx.execute(
        "UPDATE pyramid_config_contributions
         SET superseded_by_id = ?1,
             status = CASE WHEN status = 'active' THEN 'superseded' ELSE status END
         WHERE contribution_id = ?2",
        rusqlite::params![new_id, prior.contribution_id],
    )?;

    tx.commit()?;
    Ok(new_id)
}

/// Accept a config contribution, promoting it to `active` and running
/// the Phase 4 operational sync dispatcher. Per the canonical IPC
/// signature in `config-contribution-and-wire-sharing.md`.
///
/// Phase 9 handles two cases:
///   (a) An existing draft contribution identified by (schema_type,
///       slug) — find the latest draft and promote it
///   (b) A direct YAML payload — create a fresh active contribution
///
/// Both cases trigger `sync_config_to_operational_with_registry` so
/// the executor sees the new value on its next read.
#[allow(clippy::too_many_arguments)]
pub fn accept_config_draft(
    conn: &mut Connection,
    bus: &Arc<BuildEventBus>,
    schema_registry: &Arc<SchemaRegistry>,
    schema_type: String,
    slug: Option<String>,
    yaml: Option<serde_json::Value>,
    triggering_note: Option<String>,
) -> Result<AcceptConfigResponse> {
    // Prefer the direct-YAML path when provided (the frontend passes
    // the edited YAML directly from the renderer). If absent, look
    // for the latest draft contribution for the (schema_type, slug)
    // pair and promote it.
    let (contribution_id, yaml_content, note) = if let Some(yaml_value) = yaml {
        // Serialize the incoming YAML value to a string. If the
        // frontend sends a JSON object, emit its YAML equivalent;
        // if it sends a string, use it as-is.
        let yaml_str = match yaml_value {
            serde_json::Value::String(s) => s,
            other => serde_yaml::to_string(&other)
                .map_err(|e| anyhow!("failed to serialize accepted YAML: {e}"))?,
        };

        let note = triggering_note.unwrap_or_else(|| {
            format!("Accepted {} config", schema_type)
        });
        if note.trim().is_empty() {
            return Err(anyhow!("triggering_note must not be empty or whitespace"));
        }

        let mut metadata = default_wire_native_metadata(&schema_type, slug.as_deref());
        metadata.maturity = WireMaturity::Canon;

        let new_id = create_config_contribution_with_metadata(
            conn,
            &schema_type,
            slug.as_deref(),
            &yaml_str,
            Some(&note),
            "local",
            Some("user"),
            "active",
            &metadata,
        )?;

        (new_id, yaml_str, note)
    } else {
        // Look for the latest draft contribution for this (type, slug).
        let latest_draft = find_latest_draft(conn, &schema_type, slug.as_deref())?
            .ok_or_else(|| {
                anyhow!(
                    "no draft contribution found for schema_type={schema_type:?}, slug={slug:?}"
                )
            })?;

        let note = triggering_note
            .or_else(|| latest_draft.triggering_note.clone())
            .unwrap_or_else(|| format!("Accepted draft {}", schema_type));

        // Promote the draft to active: run an inline transaction that
        // flips its status and supersedes any prior active row.
        promote_draft_to_active(conn, &latest_draft)?;
        (
            latest_draft.contribution_id.clone(),
            latest_draft.yaml_content.clone(),
            note,
        )
    };

    // Re-load the promoted contribution to pass into the sync
    // dispatcher.
    let contribution = load_contribution_by_id(conn, &contribution_id)?
        .ok_or_else(|| anyhow!("contribution {contribution_id} disappeared after accept"))?;

    // Phase 9 invariant: accept MUST trigger sync_config_to_operational.
    // Uses the _with_registry variant so schema_definition
    // supersessions wire the Phase 9 stubs (invalidate_schema_registry_cache
    // + flag_configs_for_migration).
    sync_config_to_operational_with_registry(conn, bus, &contribution, Some(schema_registry))
        .map_err(|e| anyhow!("sync_config_to_operational failed: {e}"))?;

    let operational_table = operational_table_for(&schema_type);
    let reload_triggered = reload_hooks_for(&schema_type);

    // Compute version chain length for the response.
    let history = load_config_version_history(conn, &schema_type, slug.as_deref())?;
    let version = history.len() as u32;

    let metadata_value: serde_json::Value =
        serde_json::from_str(&contribution.wire_native_metadata_json)
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

    // Also look up the prior contribution to update the schema
    // registry if this accept was for a schema_definition. The
    // dispatcher already fires `invalidate_schema_registry_cache`
    // inside the schema_definition branch, so no extra work here —
    // just log for visibility.
    if schema_type == "schema_definition" {
        debug!(
            contribution_id,
            "accept_config_draft: schema_definition accepted, dispatcher handled invalidate"
        );
    }

    Ok(AcceptConfigResponse {
        contribution_id,
        yaml_content,
        version,
        triggering_note: note,
        status: "active".to_string(),
        wire_native_metadata: metadata_value,
        sync_result: SyncResult {
            operational_table,
            reload_triggered,
        },
    })
}

/// Find the most recent draft contribution for a (schema_type, slug)
/// pair. Returns the raw row for promotion.
fn find_latest_draft(
    conn: &Connection,
    schema_type: &str,
    slug: Option<&str>,
) -> Result<Option<ConfigContribution>> {
    let select = "SELECT id, contribution_id, slug, schema_type, yaml_content,
        wire_native_metadata_json, wire_publication_state_json,
        supersedes_id, superseded_by_id, triggering_note,
        status, source, wire_contribution_id, created_by,
        created_at, accepted_at
     FROM pyramid_config_contributions";

    let sql = if slug.is_some() {
        format!(
            "{select}
             WHERE slug = ?1 AND schema_type = ?2 AND status = 'draft'
             ORDER BY created_at DESC, id DESC
             LIMIT 1"
        )
    } else {
        format!(
            "{select}
             WHERE slug IS NULL AND schema_type = ?1 AND status = 'draft'
             ORDER BY created_at DESC, id DESC
             LIMIT 1"
        )
    };

    let row = if let Some(slug_val) = slug {
        conn.query_row(&sql, rusqlite::params![slug_val, schema_type], row_to_contribution)
    } else {
        conn.query_row(&sql, rusqlite::params![schema_type], row_to_contribution)
    };

    match row {
        Ok(contribution) => Ok(Some(contribution)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(anyhow!("find_latest_draft query failed: {e}")),
    }
}

fn row_to_contribution(row: &rusqlite::Row) -> rusqlite::Result<ConfigContribution> {
    Ok(ConfigContribution {
        id: row.get("id")?,
        contribution_id: row.get("contribution_id")?,
        slug: row.get("slug")?,
        schema_type: row.get("schema_type")?,
        yaml_content: row.get("yaml_content")?,
        wire_native_metadata_json: row.get("wire_native_metadata_json")?,
        wire_publication_state_json: row.get("wire_publication_state_json")?,
        supersedes_id: row.get("supersedes_id")?,
        superseded_by_id: row.get("superseded_by_id")?,
        triggering_note: row.get("triggering_note")?,
        status: row.get("status")?,
        source: row.get("source")?,
        wire_contribution_id: row.get("wire_contribution_id")?,
        created_by: row.get("created_by")?,
        created_at: row.get("created_at")?,
        accepted_at: row.get("accepted_at")?,
    })
}

/// Promote a draft contribution to `active`. Flips the draft's status,
/// sets `accepted_at`, and supersedes any prior active row for the
/// same (schema_type, slug).
fn promote_draft_to_active(
    conn: &mut Connection,
    draft: &ConfigContribution,
) -> Result<()> {
    let tx = conn.transaction()?;

    // Find the prior active contribution (if any) to supersede it.
    let prior_id: Option<String> = if let Some(ref slug_val) = draft.slug {
        tx.query_row(
            "SELECT contribution_id FROM pyramid_config_contributions
             WHERE slug = ?1 AND schema_type = ?2
               AND status = 'active' AND superseded_by_id IS NULL
               AND contribution_id != ?3
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
            rusqlite::params![slug_val, draft.schema_type, draft.contribution_id],
            |row| row.get(0),
        )
        .ok()
    } else {
        tx.query_row(
            "SELECT contribution_id FROM pyramid_config_contributions
             WHERE slug IS NULL AND schema_type = ?1
               AND status = 'active' AND superseded_by_id IS NULL
               AND contribution_id != ?2
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
            rusqlite::params![draft.schema_type, draft.contribution_id],
            |row| row.get(0),
        )
        .ok()
    };

    // Promote the draft.
    tx.execute(
        "UPDATE pyramid_config_contributions
         SET status = 'active',
             accepted_at = datetime('now'),
             supersedes_id = COALESCE(supersedes_id, ?1)
         WHERE contribution_id = ?2",
        rusqlite::params![prior_id, draft.contribution_id],
    )?;

    // Supersede the prior (if any).
    if let Some(prior) = prior_id {
        tx.execute(
            "UPDATE pyramid_config_contributions
             SET status = 'superseded',
                 superseded_by_id = ?1
             WHERE contribution_id = ?2",
            rusqlite::params![draft.contribution_id, prior],
        )?;
    }

    tx.commit()?;
    Ok(())
}

/// Return the operational table name for a given schema_type. Used by
/// `AcceptConfigResponse.sync_result.operational_table`. Falls back
/// to an empty string for schema types without dedicated operational
/// tables (e.g. skill, schema_definition, schema_annotation).
fn operational_table_for(schema_type: &str) -> String {
    match schema_type {
        "dadbear_policy" => "pyramid_dadbear_config".to_string(),
        "evidence_policy" => "pyramid_evidence_policy".to_string(),
        "build_strategy" => "pyramid_build_strategy".to_string(),
        "tier_routing" => "pyramid_tier_routing".to_string(),
        "custom_prompts" => "pyramid_custom_prompts".to_string(),
        "step_overrides" => "pyramid_step_overrides".to_string(),
        "folder_ingestion_heuristics" => "pyramid_folder_ingestion_heuristics".to_string(),
        _ => String::new(),
    }
}

/// Return the set of reload-hook names that fired during sync for a
/// given schema_type. Purely diagnostic — mirrors the hooks the
/// dispatcher actually calls.
fn reload_hooks_for(schema_type: &str) -> Vec<String> {
    match schema_type {
        "dadbear_policy" => vec!["trigger_dadbear_reload".to_string()],
        "evidence_policy" => vec!["reevaluate_deferred_questions".to_string()],
        "tier_routing" | "step_overrides" => {
            vec!["invalidate_provider_resolver_cache".to_string()]
        }
        "custom_prompts" | "skill" | "custom_chains" => {
            vec!["invalidate_prompt_cache".to_string()]
        }
        "schema_definition" => vec![
            "invalidate_schema_registry_cache".to_string(),
            "flag_configs_for_migration".to_string(),
        ],
        "schema_annotation" => vec!["invalidate_schema_annotation_cache".to_string()],
        _ => vec![],
    }
}

/// Load the active config for a (schema_type, slug) pair. Thin
/// wrapper over Phase 4's helper; returns the canonical Phase 9
/// response shape.
pub fn active_config_for(
    conn: &Connection,
    schema_type: &str,
    slug: Option<&str>,
) -> Result<Option<ActiveConfigResponse>> {
    let Some(active) = load_active_config_contribution(conn, schema_type, slug)? else {
        return Ok(None);
    };
    let history = load_config_version_history(conn, schema_type, slug)?;
    Ok(Some(ActiveConfigResponse {
        contribution_id: active.contribution_id,
        yaml_content: active.yaml_content,
        version_chain_length: history.len() as u32,
        created_at: active.created_at,
        triggering_note: active.triggering_note,
    }))
}

/// Return the full version history for a (schema_type, slug) pair.
/// Thin wrapper over Phase 4's helper.
pub fn config_version_history_for(
    conn: &Connection,
    schema_type: &str,
    slug: Option<&str>,
) -> Result<Vec<ConfigContribution>> {
    load_config_version_history(conn, schema_type, slug)
}

/// List all schemas from the registry. Returns compact summaries
/// suitable for the frontend schema picker.
pub fn list_config_schemas(registry: &SchemaRegistry) -> Vec<ConfigSchemaSummary> {
    registry.list()
}

/// Extract the YAML body from an LLM response. Best-effort — the spec
/// says "output only the YAML document, no prose before or after", but
/// LLMs occasionally wrap output in ```yaml fences or prefix an
/// explanation. This helper strips both patterns.
fn extract_yaml_body(raw: &str) -> String {
    let trimmed = raw.trim();

    // Case 1: triple-backtick fence. Accept both ```yaml and plain ```
    // fences and extract the inner content.
    if let Some(body) = extract_fenced_block(trimmed) {
        return body;
    }

    // Case 2: the LLM added a leading "Here is the YAML:" line; strip
    // everything up to the first `schema_type:` declaration if the
    // first non-empty line doesn't already look like YAML.
    if !trimmed.starts_with("schema_type") && !trimmed.starts_with("---") {
        if let Some(idx) = trimmed.find("schema_type:") {
            return trimmed[idx..].trim().to_string();
        }
    }

    trimmed.to_string()
}

/// Extract the content between triple-backtick fences. Returns `None`
/// if no fence is found. Handles both `\`\`\`yaml\n...\n\`\`\`` and
/// `\`\`\`\n...\n\`\`\``.
fn extract_fenced_block(input: &str) -> Option<String> {
    let fence_start = input.find("```")?;
    // Skip the optional language tag on the first line.
    let after_open = &input[fence_start + 3..];
    let after_open_line = after_open
        .find('\n')
        .map(|idx| &after_open[idx + 1..])
        .unwrap_or(after_open);
    let fence_end = after_open_line.find("```")?;
    Some(after_open_line[..fence_end].trim().to_string())
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::db::init_pyramid_db;
    use crate::pyramid::wire_migration::walk_bundled_contributions_manifest;

    fn mem_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        conn
    }

    #[test]
    fn test_substitute_prompt_basic() {
        let tmpl = "Schema: {schema}\nIntent: {intent}\n";
        let out = substitute_prompt(tmpl, "{}", "be local", None, None);
        assert!(out.contains("Schema: {}"));
        assert!(out.contains("Intent: be local"));
    }

    #[test]
    fn test_substitute_prompt_with_conditional_blocks_absent() {
        let tmpl = "Prefix\n{if current_yaml}CURRENT: {current_yaml}{end}\nMid\n{if notes}NOTES: {notes}{end}\nSuffix";
        let out = substitute_prompt(tmpl, "{}", "x", None, None);
        assert!(!out.contains("CURRENT"));
        assert!(!out.contains("NOTES"));
        assert!(out.contains("Prefix"));
        assert!(out.contains("Mid"));
        assert!(out.contains("Suffix"));
    }

    #[test]
    fn test_substitute_prompt_with_conditional_blocks_present() {
        let tmpl = "{if current_yaml}CURRENT: {current_yaml}{end}\n{if notes}NOTES: {notes}{end}\n";
        let out = substitute_prompt(tmpl, "{}", "x", Some("existing"), Some("change it"));
        assert!(out.contains("CURRENT: existing"));
        assert!(out.contains("NOTES: change it"));
    }

    #[test]
    fn test_extract_yaml_body_plain() {
        let raw = "schema_type: evidence_policy\nbudget: {}\n";
        assert_eq!(
            extract_yaml_body(raw),
            "schema_type: evidence_policy\nbudget: {}"
        );
    }

    #[test]
    fn test_extract_yaml_body_fenced() {
        let raw = "```yaml\nschema_type: evidence_policy\nbudget: {}\n```";
        let out = extract_yaml_body(raw);
        assert!(out.starts_with("schema_type: evidence_policy"));
        assert!(out.contains("budget: {}"));
    }

    #[test]
    fn test_extract_yaml_body_with_prose_prefix() {
        let raw = "Here is your YAML:\n\nschema_type: evidence_policy\nbudget: {}\n";
        let out = extract_yaml_body(raw);
        assert!(out.starts_with("schema_type: evidence_policy"));
    }

    #[test]
    fn test_active_config_for_returns_none_when_empty() {
        let conn = mem_conn();
        let result = active_config_for(&conn, "evidence_policy", None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_list_config_schemas_from_bundled_manifest() {
        let conn = mem_conn();
        walk_bundled_contributions_manifest(&conn).unwrap();
        let registry = SchemaRegistry::hydrate_from_contributions(&conn).unwrap();
        let summaries = list_config_schemas(&registry);
        let names: Vec<&str> = summaries.iter().map(|s| s.schema_type.as_str()).collect();
        assert!(names.contains(&"evidence_policy"));
        assert!(names.contains(&"build_strategy"));
        assert!(names.contains(&"dadbear_policy"));
        assert!(names.contains(&"tier_routing"));
        assert!(names.contains(&"custom_prompts"));
    }

    #[test]
    fn test_create_draft_supersession_marks_prior_superseded() {
        let mut conn = mem_conn();
        walk_bundled_contributions_manifest(&conn).unwrap();
        // Use the bundled evidence_policy default as the prior.
        let active = load_active_config_contribution(&conn, "evidence_policy", None)
            .unwrap()
            .unwrap();

        let new_id = create_draft_supersession(
            &mut conn,
            &active,
            "schema_type: evidence_policy\nbudget:\n  max_concurrent_evidence: 2\n",
            "bump concurrency",
        )
        .unwrap();

        // New row is a draft pointing at the prior.
        let new_row = load_contribution_by_id(&conn, &new_id).unwrap().unwrap();
        assert_eq!(new_row.status, "draft");
        assert_eq!(new_row.supersedes_id.as_deref(), Some(active.contribution_id.as_str()));
        assert_eq!(new_row.created_by.as_deref(), Some("generative_config"));

        // Prior row is superseded.
        let prior_row = load_contribution_by_id(&conn, &active.contribution_id)
            .unwrap()
            .unwrap();
        assert_eq!(prior_row.status, "superseded");
        assert_eq!(prior_row.superseded_by_id.as_deref(), Some(new_id.as_str()));
    }

    #[test]
    fn test_accept_config_draft_direct_yaml_activates_and_syncs() {
        let mut conn = mem_conn();
        walk_bundled_contributions_manifest(&conn).unwrap();
        let registry = Arc::new(SchemaRegistry::hydrate_from_contributions(&conn).unwrap());
        let bus = Arc::new(BuildEventBus::new());

        // Use evidence_policy for the direct-YAML accept test because
        // its operational sync doesn't require a `source_path` field
        // (dadbear_policy expects one from the legacy schema). The
        // sync dispatcher round-trips this YAML into
        // pyramid_evidence_policy via db::upsert_evidence_policy.
        let yaml = serde_json::Value::String(
            "triage_rules: []\ndemand_signals: []\nbudget: {}\n".to_string()
        );

        let resp = accept_config_draft(
            &mut conn,
            &bus,
            &registry,
            "evidence_policy".to_string(),
            Some("my-slug".to_string()),
            Some(yaml),
            Some("quick test accept".to_string()),
        )
        .unwrap();

        assert_eq!(resp.status, "active");
        assert!(!resp.contribution_id.is_empty());
        assert_eq!(resp.sync_result.operational_table, "pyramid_evidence_policy");

        // Verify the contribution landed with active status.
        let contribution = load_contribution_by_id(&conn, &resp.contribution_id)
            .unwrap()
            .unwrap();
        assert_eq!(contribution.status, "active");
        assert_eq!(contribution.created_by.as_deref(), Some("user"));
    }

    #[test]
    fn test_accept_config_draft_missing_draft_errors_cleanly() {
        let mut conn = mem_conn();
        let registry = Arc::new(SchemaRegistry::hydrate_from_contributions(&conn).unwrap());
        let bus = Arc::new(BuildEventBus::new());

        let err = accept_config_draft(
            &mut conn,
            &bus,
            &registry,
            "evidence_policy".to_string(),
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("no draft contribution"));
    }

    #[test]
    fn test_load_refinement_inputs_rejects_empty_note() {
        let conn = mem_conn();
        walk_bundled_contributions_manifest(&conn).unwrap();
        let registry = SchemaRegistry::hydrate_from_contributions(&conn).unwrap();

        // Seed a prior draft to refine.
        let bundled_active =
            load_active_config_contribution(&conn, "evidence_policy", None)
                .unwrap()
                .unwrap();

        // Empty string rejected.
        let err = load_refinement_inputs(
            &conn,
            &registry,
            &bundled_active.contribution_id,
            "schema_type: evidence_policy\n",
            "",
        )
        .unwrap_err();
        assert!(err.to_string().contains("must not be empty"));

        // Whitespace-only rejected.
        let err = load_refinement_inputs(
            &conn,
            &registry,
            &bundled_active.contribution_id,
            "schema_type: evidence_policy\n",
            "   \n\t  ",
        )
        .unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn test_load_generation_inputs_rejects_empty_intent() {
        let conn = mem_conn();
        walk_bundled_contributions_manifest(&conn).unwrap();
        let registry = SchemaRegistry::hydrate_from_contributions(&conn).unwrap();

        let err =
            load_generation_inputs(&conn, &registry, "evidence_policy", None, "").unwrap_err();
        assert!(err.to_string().contains("intent must not be empty"));

        let err =
            load_generation_inputs(&conn, &registry, "evidence_policy", None, "   ")
                .unwrap_err();
        assert!(err.to_string().contains("intent must not be empty"));
    }

    #[test]
    fn test_load_generation_inputs_rejects_unknown_schema_type() {
        let conn = mem_conn();
        walk_bundled_contributions_manifest(&conn).unwrap();
        let registry = SchemaRegistry::hydrate_from_contributions(&conn).unwrap();

        let err = load_generation_inputs(&conn, &registry, "totally_made_up", None, "x")
            .unwrap_err();
        assert!(err.to_string().contains("no active schema"));
    }

    #[test]
    fn test_load_generation_inputs_loads_bundled_bodies() {
        let conn = mem_conn();
        walk_bundled_contributions_manifest(&conn).unwrap();
        let registry = SchemaRegistry::hydrate_from_contributions(&conn).unwrap();

        let inputs = load_generation_inputs(
            &conn,
            &registry,
            "evidence_policy",
            None,
            "local-only, conservative",
        )
        .unwrap();
        assert_eq!(inputs.schema_type, "evidence_policy");
        assert_eq!(inputs.intent, "local-only, conservative");
        assert!(inputs.skill_body.contains("evidence"));
        assert!(inputs.schema_json.contains("evidence_policy"));
    }

    #[test]
    fn test_accept_config_promotes_draft_and_supersedes_prior() {
        let mut conn = mem_conn();
        walk_bundled_contributions_manifest(&conn).unwrap();
        let registry = Arc::new(SchemaRegistry::hydrate_from_contributions(&conn).unwrap());
        let bus = Arc::new(BuildEventBus::new());

        // The bundled evidence_policy is already active. Create a new
        // draft (as though a user generated via the UI).
        let prior_active = load_active_config_contribution(&conn, "evidence_policy", None)
            .unwrap()
            .unwrap();

        // Create a draft via create_draft_supersession to match the
        // refine path's semantics.
        let draft_id = create_draft_supersession(
            &mut conn,
            &prior_active,
            "triage_rules: []\ndemand_signals: []\nbudget: {}\n",
            "user refinement",
        )
        .unwrap();

        // Accept without a yaml payload — should find the draft and
        // promote it.
        let resp = accept_config_draft(
            &mut conn,
            &bus,
            &registry,
            "evidence_policy".to_string(),
            None,
            None,
            Some("accepted the draft".to_string()),
        )
        .unwrap();

        assert_eq!(resp.contribution_id, draft_id);
        assert_eq!(resp.status, "active");
        assert_eq!(resp.sync_result.operational_table, "pyramid_evidence_policy");

        // Verify the draft was flipped to active.
        let draft_row = load_contribution_by_id(&conn, &draft_id).unwrap().unwrap();
        assert_eq!(draft_row.status, "active");
    }
}
