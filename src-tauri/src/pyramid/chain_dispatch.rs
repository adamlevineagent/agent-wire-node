// pyramid/chain_dispatch.rs — Step dispatcher for the chain runtime engine
//
// Routes chain steps to either LLM (via OpenRouter) or named Rust mechanical
// functions. Also provides node construction from LLM output and node ID
// generation from patterns.
//
// See docs/plans/action-chain-refactor-v3.md §Phase 4 for full specification.

use anyhow::{anyhow, Result};
use rusqlite::Connection;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Mutex;
use tracing::{info, warn};

use super::chain_engine::{ChainDefaults, ChainStep};
use super::event_bus::BuildEventBus;
use super::execution_plan::{ModelRequirements, Step, StepOperation};
use super::expression::ValueEnv;
use super::llm::{self, AuditContext, LlmConfig, LlmResponse};
use super::naming::headline_from_analysis;
use super::step_context::{compute_prompt_hash, StepContext as CacheStepContext};
use super::transform_runtime;
use super::types::{
    Correction, DebatePosition, DebateTopic, Decision, GapTopic, MetaLayerTopic,
    MetaLayerTopicEntry, NodeShape, PyramidNode, RedTeamEntry, ShapePayload, Term, Topic,
    NODE_SHAPE_DEBATE, NODE_SHAPE_GAP, NODE_SHAPE_META_LAYER,
};
use super::{OperationalConfig, Tier1Config};

// ── Step context ────────────────────────────────────────────────────────────

/// Phase 6 fix pass: build-scoped cache plumbing plus lazy prompt/model
/// hash caches. Lives on `chain_dispatch::ChainDispatchContext` so every LLM call
/// site in the dispatcher (dispatch_ir_llm, dispatch_llm) can construct
/// a per-call `pyramid::step_context::StepContext` without re-hashing the
/// prompt template or re-resolving the tier.
///
/// Cloned via `Arc` — the same instance is shared across every parallel
/// forEach task spawned from a given chain run.
pub struct CacheDispatchBase {
    /// Absolute filesystem path to the pyramid SQLite database. Used by
    /// the cache layer to open ephemeral connections for reads and writes
    /// (which deliberately bypass the writer mutex — the cache is
    /// content-addressable and `INSERT OR REPLACE` on a unique key is
    /// idempotent).
    pub db_path: String,
    /// Build id stamped on every cache row produced by this chain run.
    /// Phase 13's oversight UI reads this column for provenance.
    pub build_id: String,
    /// Optional handle to the tagged build event bus. When present,
    /// cache hit / miss / verification-failed events flow out during
    /// lookups and writes.
    pub bus: Option<Arc<BuildEventBus>>,
    /// Chain name for chronicle task context (e.g., "code-mechanical").
    /// Flows through to ChainDispatchContext.chain_name via CacheAccess.
    pub chain_name: Option<String>,
    /// Content type for chronicle task context (e.g., "code", "document").
    /// Flows through to ChainDispatchContext.content_type via CacheAccess.
    pub content_type: Option<String>,
    /// Phase 6 lazy cache: prompt template path → SHA-256 hex. The same
    /// template path used by multiple steps in the same build hashes
    /// exactly once. Populated by `dispatch_ir_llm` via
    /// `get_or_compute_prompt_hash`. Uses `std::sync::Mutex` because the
    /// operations are non-awaiting and short-lived.
    pub prompt_hashes: Arc<StdMutex<HashMap<String, String>>>,
    /// Phase 6 lazy cache: tier name → canonical model id. Populated by
    /// `dispatch_ir_llm` after tier resolution so every subsequent cache
    /// write uses the same resolved model id within a build.
    pub resolved_models: Arc<StdMutex<HashMap<String, String>>>,
}

impl CacheDispatchBase {
    /// Look up or compute the SHA-256 prompt hash for a template key.
    ///
    /// `key` is typically the instruction path (`step.instruction.as_deref()`)
    /// — any stable identifier for the template body works. The first
    /// call for a given key computes the hash via the provided closure;
    /// every subsequent call hits the cache.
    pub fn get_or_compute_prompt_hash(
        &self,
        key: &str,
        body_provider: impl FnOnce() -> String,
    ) -> String {
        {
            let guard = self
                .prompt_hashes
                .lock()
                .expect("prompt_hashes mutex poisoned");
            if let Some(existing) = guard.get(key) {
                return existing.clone();
            }
        }
        let body = body_provider();
        let hash = compute_prompt_hash(&body);
        let mut guard = self
            .prompt_hashes
            .lock()
            .expect("prompt_hashes mutex poisoned");
        guard.entry(key.to_string()).or_insert_with(|| hash.clone());
        hash
    }

    /// Record a tier → resolved-model mapping for the build.
    pub fn cache_resolved_model(&self, tier: &str, model_id: &str) {
        let mut guard = self
            .resolved_models
            .lock()
            .expect("resolved_models mutex poisoned");
        guard
            .entry(tier.to_string())
            .or_insert_with(|| model_id.to_string());
    }

    /// Look up a previously cached tier → resolved-model mapping.
    pub fn get_resolved_model(&self, tier: &str) -> Option<String> {
        let guard = self
            .resolved_models
            .lock()
            .expect("resolved_models mutex poisoned");
        guard.get(tier).cloned()
    }
}

/// Context available to all chain steps during execution.
#[derive(Clone)]
pub struct ChainDispatchContext {
    pub db_reader: Arc<Mutex<Connection>>,
    pub db_writer: Arc<Mutex<Connection>>,
    pub slug: String,
    pub config: LlmConfig,
    /// Tier 1 operational config for context limits, timeouts, etc.
    pub tier1: Tier1Config,
    /// Full operational config for tier 2/3 values needed during dispatch.
    pub ops: OperationalConfig,
    /// Optional audit context for Theatre LLM audit trail.
    /// When present, all LLM calls in dispatch are recorded.
    pub audit: Option<AuditContext>,
    /// Phase 6 fix pass: cache plumbing + lazy hash caches shared across
    /// every step of a chain run. `None` only in unit tests and legacy
    /// bring-up paths; production executors populate it at dispatch
    /// context construction time.
    pub cache_base: Option<Arc<CacheDispatchBase>>,
    /// Build strategy concurrency cap. Read from the `pyramid_build_strategy`
    /// table at chain execution start. Chain step concurrency is capped to
    /// `min(step.concurrency, concurrency_cap)`. Local mode sets this to 1
    /// because Ollama processes one request at a time.
    pub concurrency_cap: Option<usize>,
    /// Post-build accretion v5 Phase 6b: handle to the full `PyramidState`
    /// so mechanical primitives can recurse into the starter-chain runner.
    /// `call_starter_chain` is the first consumer — it re-invokes
    /// `chain_executor::execute_chain_for_target` with a library chain
    /// (e.g. `starter-evidence-tester`, `starter-reconciler`) so Phase 7
    /// consumers like `debate_steward` / `meta_layer_oracle` can delegate
    /// weighing evidence / merging positions to focused library chains.
    ///
    /// `None` on every dispatch path OTHER than the starter runner. The
    /// full chain executor, IR executor, and dead-letter retry do not
    /// invoke library chains. `call_starter_chain` raises loudly when
    /// this field is `None` — see `feedback_loud_deferrals`.
    pub state: Option<Arc<super::PyramidState>>,
    /// Post-build accretion v5 Phase 6b: chains directory override. When
    /// present, sub-chain invocation resolves `starter-*.yaml` via
    /// `chain_loader::load_chain_by_id(chain_id, chains_dir)`. In practice
    /// this always duplicates `ctx.state.as_ref().unwrap().chains_dir`,
    /// but threading it explicitly keeps the dispatch-time call site one
    /// hop away from the read.
    pub chains_dir: Option<std::path::PathBuf>,
    /// Post-build accretion v5 Phase 6b: the `target_id` that the starter
    /// runner was invoked with. Sub-chain calls inherit this by default so
    /// chain body steps that depend on `target_node_id` (e.g.,
    /// `queue_re_distill_for_target`) keep working across the recursion.
    pub target_id: Option<String>,
    /// Post-build accretion v5 Phase 6b: current sub-chain nesting depth
    /// as observed at THIS dispatch context. The runner increments this
    /// by one when invoking a sub-chain, and the depth-guard in
    /// `call_starter_chain` short-circuits at `MAX_SUB_CHAIN_DEPTH`.
    /// `None` at the top level is treated as depth `0`.
    pub sub_chain_depth: Option<usize>,
}

impl CacheDispatchBase {
    /// Build a fresh `CacheDispatchBase` with empty lazy caches. Called
    /// once per chain run from the executor entry points.
    pub fn new(
        db_path: impl Into<String>,
        build_id: impl Into<String>,
        bus: Option<Arc<BuildEventBus>>,
    ) -> Self {
        Self {
            db_path: db_path.into(),
            build_id: build_id.into(),
            bus,
            chain_name: None,
            content_type: None,
            prompt_hashes: Arc::new(StdMutex::new(HashMap::new())),
            resolved_models: Arc::new(StdMutex::new(HashMap::new())),
        }
    }

    /// Set chain context for chronicle task context.
    pub fn with_chain_context(mut self, chain_name: String, content_type: String) -> Self {
        self.chain_name = Some(chain_name);
        self.content_type = Some(content_type);
        self
    }
}

// ── Top-level dispatcher ────────────────────────────────────────────────────

/// Dispatch a chain step to either LLM or mechanical execution.
///
/// For LLM steps: builds a user prompt from `resolved_input`, calls the model,
/// parses JSON from the response (with automatic retry at temp 0.1 on parse
/// failure).
///
/// For mechanical steps: dispatches to named Rust functions by `rust_function`.
pub async fn dispatch_step(
    step: &ChainStep,
    resolved_input: &Value,
    system_prompt: &str,
    defaults: &ChainDefaults,
    ctx: &ChainDispatchContext,
) -> Result<Value> {
    if step.mechanical {
        let fn_name = step
            .rust_function
            .as_deref()
            .ok_or_else(|| anyhow!("Mechanical step '{}' missing rust_function", step.name))?;
        info!("[CHAIN] step '{}' → mechanical fn '{}'", step.name, fn_name);
        dispatch_mechanical(fn_name, resolved_input, ctx).await
    } else {
        dispatch_llm(step, resolved_input, system_prompt, defaults, ctx).await
    }
}

// ── LLM dispatch ────────────────────────────────────────────────────────────

/// Resolve the model string from step overrides, tier routing, or defaults.
fn resolve_model(step: &ChainStep, defaults: &ChainDefaults, config: &LlmConfig) -> String {
    // Direct model override on step takes highest precedence
    if let Some(ref model) = step.model {
        return model.clone();
    }
    // Direct model override on defaults
    if let Some(ref model) = defaults.model {
        // But only if the step doesn't specify a tier
        if step.model_tier.is_none() {
            return model.clone();
        }
    }
    let tier = step
        .model_tier
        .as_deref()
        .unwrap_or(defaults.model_tier.as_str());

    // Phase 3: consult provider registry tier routing (canonical source)
    if let Some(ref registry) = config.provider_registry {
        if let Ok(resolved) = registry.resolve_tier(tier, None, None, None) {
            return resolved.tier.model_id;
        }
        warn!("[CHAIN] tier '{}' not in registry, falling back to legacy resolution", tier);
    }

    // Legacy fallback: aliases then hardcoded mapping
    if let Some(model) = config.model_aliases.get(tier) {
        return model.clone();
    }
    match tier {
        "low" | "mid" => config.primary_model.clone(),
        "high" => config.fallback_model_1.clone(),
        "max" => config.fallback_model_2.clone(),
        other => {
            warn!("[CHAIN] unknown tier '{}', using primary_model", other);
            config.primary_model.clone()
        }
    }
}

/// Resolve temperature from step override or defaults.
fn resolve_temperature(step: &ChainStep, defaults: &ChainDefaults) -> f32 {
    step.temperature.unwrap_or(defaults.temperature)
}

/// Dispatch a step to the LLM, with JSON-retry at temp 0.1 on parse failure.
///
/// If the step specifies a `model:` override, creates a modified LlmConfig
/// so the override is actually used by call_model().
async fn dispatch_llm(
    step: &ChainStep,
    resolved_input: &Value,
    system_prompt: &str,
    defaults: &ChainDefaults,
    ctx: &ChainDispatchContext,
) -> Result<Value> {
    let temperature = resolve_temperature(step, defaults);
    let resolved_model = resolve_model(step, defaults, &ctx.config);
    let resolved_limit = resolve_context_limit(step, defaults, &ctx.config, &ctx.tier1);
    let max_tokens: usize = ctx.tier1.ir_max_tokens;

    // Apply model override: if the resolved model differs from the config's
    // primary model, create a modified config so call_model() uses it.
    // Uses clone_with_model_override to pin ALL model slots (primary +
    // fallback_1 + fallback_2) to the resolved model, preventing the
    // cascade from escaping to a different provider's models.
    let config_ref;
    let overridden_config;
    if resolved_model != ctx.config.primary_model
        || resolved_limit != ctx.config.primary_context_limit
    {
        let mut cfg = ctx.config.clone_with_model_override(&resolved_model);
        cfg.primary_context_limit = resolved_limit;
        overridden_config = cfg;
        config_ref = &overridden_config;
    } else {
        config_ref = &ctx.config;
    }

    // Build user prompt from resolved input
    let user_prompt =
        serde_json::to_string_pretty(resolved_input).unwrap_or_else(|_| resolved_input.to_string());

    info!(
        "[CHAIN] step '{}' → LLM (temp={}, model={}, prompt_len={})",
        step.name,
        temperature,
        short_model_name(&resolved_model),
        user_prompt.len()
    );

    // If step has a response_schema, use structured outputs for guaranteed JSON
    if let Some(ref schema) = step.response_schema {
        let schema_name = step.name.replace('-', "_");
        info!(
            "[CHAIN] step '{}' → using structured output (schema: {})",
            step.name, schema_name
        );
        // Phase 12: route through the cache-aware structured variant
        // when the dispatch context carries cache_base. See dispatch_ir_llm
        // for the fix-pass retrofit pattern that mirrors this.
        let struct_ctx = ctx.cache_base.as_ref().map(|cb| {
            let prompt_hash = cb.get_or_compute_prompt_hash(
                step.instruction.as_deref().unwrap_or(&step.name),
                || system_prompt.to_string(),
            );
            let mut c = CacheStepContext::new(
                ctx.slug.clone(),
                cb.build_id.clone(),
                format!("{}_structured", step.name),
                "chain_llm_structured",
                0,
                None,
                cb.db_path.clone(),
            )
            .with_model_resolution(
                step.model_tier.clone().unwrap_or_else(|| "mid".to_string()),
                resolved_model.clone(),
            )
            .with_prompt_hash(prompt_hash);
            if let (Some(cn), Some(ct)) = (&cb.chain_name, &cb.content_type) {
                c = c.with_chain_context(cn.clone(), ct.clone());
            }
            if let Some(bus) = &cb.bus {
                c = c.with_bus(bus.clone());
            }
            c
        });
        let response = llm::call_model_structured_and_ctx(
            config_ref,
            struct_ctx.as_ref(),
            system_prompt,
            &user_prompt,
            temperature,
            max_tokens,
            schema,
            &schema_name,
        )
        .await?;
        match llm::extract_json(&response) {
            Ok(json) => return Ok(json),
            Err(e) => {
                info!("[CHAIN] step '{}' parse failed, on_parse_error={:?}", step.name, step.on_parse_error);
                if step.on_parse_error.as_deref() == Some("heal") {
                    info!("[CHAIN] step '{}' → parse failed ({}), attempting self-healing (1 max attempts)", step.name, e);
                    let heal_instruction = step.heal_instruction.as_deref().unwrap_or("Fix the JSON.");
                    let heal_sys = format!("{}\n\n{}", system_prompt, heal_instruction);
                    let heal_user = format!("Target Schema:\n{}\n\nMalformed Response:\n{}\n\nError:\n{}", serde_json::to_string_pretty(schema).unwrap_or_default(), response, e);
                    // Phase 12: heal path inherits the cache plumbing
                    // but with a different step_name so it gets its
                    // own cache row.
                    let heal_ctx = ctx.cache_base.as_ref().map(|cb| {
                        let prompt_hash = compute_prompt_hash(&heal_sys);
                        let mut c = CacheStepContext::new(
                            ctx.slug.clone(),
                            cb.build_id.clone(),
                            format!("{}_heal", step.name),
                            "chain_llm_heal",
                            0,
                            None,
                            cb.db_path.clone(),
                        )
                        .with_model_resolution(
                            step.model_tier.clone().unwrap_or_else(|| "mid".to_string()),
                            resolved_model.clone(),
                        )
                        .with_prompt_hash(prompt_hash);
                        if let (Some(cn), Some(ct)) = (&cb.chain_name, &cb.content_type) {
                            c = c.with_chain_context(cn.clone(), ct.clone());
                        }
                        if let Some(bus) = &cb.bus {
                            c = c.with_bus(bus.clone());
                        }
                        c
                    });
                    let retry_resp = llm::call_model_and_ctx(
                        config_ref,
                        heal_ctx.as_ref(),
                        &heal_sys,
                        &heal_user,
                        0.1,
                        max_tokens,
                    )
                    .await?;
                    return llm::extract_json(&retry_resp).map_err(|he| anyhow!("Step '{}': JSON parse failed after self-healing: {}", step.name, he));
                } else {
                    return Err(anyhow!("Step '{}': structured output JSON parse failed: {}", step.name, e));
                }
            }
        }
    }

    // Phase 12 retrofit: construct a cache-usable StepContext when the
    // dispatch context's cache_base is populated (production chain
    // executor always does). This turns the legacy v2 chain path into
    // a cache-reachable path.
    let dispatch_cache_ctx = ctx.cache_base.as_ref().map(|cb| {
        let prompt_hash = cb.get_or_compute_prompt_hash(
            step.instruction.as_deref().unwrap_or(&step.name),
            || system_prompt.to_string(),
        );
        let mut c = CacheStepContext::new(
            ctx.slug.clone(),
            cb.build_id.clone(),
            step.name.clone(),
            "chain_llm",
            0,
            None,
            cb.db_path.clone(),
        )
        .with_model_resolution(
            step.model_tier.clone().unwrap_or_else(|| "mid".to_string()),
            resolved_model.clone(),
        )
        .with_prompt_hash(prompt_hash);
        if let (Some(cn), Some(ct)) = (&cb.chain_name, &cb.content_type) {
            c = c.with_chain_context(cn.clone(), ct.clone());
        }
        if let Some(bus) = &cb.bus {
            c = c.with_bus(bus.clone());
        }
        c
    });

    // Phase 18b L8 retrofit: cache + audit unified path. Previously the
    // audited branch bypassed the Phase 6 cache; now both audit and
    // cache thread through the unified entry point so audited builds
    // also benefit from the content-addressable cache. The dispatch
    // ctx already builds a `dispatch_cache_ctx` above for the
    // non-audited path; we reuse it for both branches now.
    let dispatch_audit_ctx = ctx.audit.as_ref().map(|audit| AuditContext {
        step_name: step.name.clone(),
        call_purpose: "chain_dispatch".to_string(),
        ..audit.clone()
    });
    let resp = llm::call_model_unified_with_audit_and_ctx(
        config_ref,
        dispatch_cache_ctx.as_ref(),
        dispatch_audit_ctx.as_ref(),
        system_prompt,
        &user_prompt,
        temperature,
        max_tokens,
        None,
        llm::LlmCallOptions::default(),
    )
    .await?;
    let response = resp.content;

    match llm::extract_json(&response) {
        Ok(json) => {
            info!("[CHAIN] step '{}' → JSON parsed OK", step.name);
            Ok(json)
        }
        Err(_first_err) => {
            info!("[CHAIN] step '{}' parse failed, on_parse_error={:?}", step.name, step.on_parse_error);
            if step.on_parse_error.as_deref() == Some("heal") {
                info!("[CHAIN] step '{}' → parse failed ({}), attempting self-healing (1 max attempts)", step.name, _first_err);
                let heal_instruction = step.heal_instruction.as_deref().unwrap_or("Fix the JSON.");
                let heal_sys = format!("{}\n\n{}", system_prompt, heal_instruction);
                let heal_user = format!("Malformed Response:\n{}\n\nError:\n{}", response, _first_err);
                let heal_ctx = ctx.cache_base.as_ref().map(|cb| {
                    let prompt_hash = compute_prompt_hash(&heal_sys);
                    let mut c = CacheStepContext::new(
                        ctx.slug.clone(),
                        cb.build_id.clone(),
                        format!("{}_heal_standard", step.name),
                        "chain_llm_heal",
                        0,
                        None,
                        cb.db_path.clone(),
                    )
                    .with_model_resolution(
                        step.model_tier.clone().unwrap_or_else(|| "mid".to_string()),
                        resolved_model.clone(),
                    )
                    .with_prompt_hash(prompt_hash);
                    if let (Some(cn), Some(ct)) = (&cb.chain_name, &cb.content_type) {
                        c = c.with_chain_context(cn.clone(), ct.clone());
                    }
                    if let Some(bus) = &cb.bus {
                        c = c.with_bus(bus.clone());
                    }
                    c
                });
                let retry_resp = llm::call_model_and_ctx(
                    config_ref,
                    heal_ctx.as_ref(),
                    &heal_sys,
                    &heal_user,
                    0.1,
                    max_tokens,
                )
                .await?;
                return llm::extract_json(&retry_resp).map_err(|he| anyhow!("Step '{}': JSON parse failed after self-healing: {}", step.name, he));
            } else {
                // JSON-retry guarantee: retry at temperature 0.1
                info!(
                    "[CHAIN] step '{}' → JSON parse failed, retrying at temp 0.1",
                    step.name
                );
                // Same cache ctx, different step_name so cache rows
                // are distinct for retry variants.
                let retry_ctx = ctx.cache_base.as_ref().map(|cb| {
                    let prompt_hash = cb.get_or_compute_prompt_hash(
                        step.instruction.as_deref().unwrap_or(&step.name),
                        || system_prompt.to_string(),
                    );
                    let mut c = CacheStepContext::new(
                        ctx.slug.clone(),
                        cb.build_id.clone(),
                        format!("{}_retry_temp01", step.name),
                        "chain_llm_retry",
                        0,
                        None,
                        cb.db_path.clone(),
                    )
                    .with_model_resolution(
                        step.model_tier.clone().unwrap_or_else(|| "mid".to_string()),
                        resolved_model.clone(),
                    )
                    .with_prompt_hash(prompt_hash);
                    if let (Some(cn), Some(ct)) = (&cb.chain_name, &cb.content_type) {
                        c = c.with_chain_context(cn.clone(), ct.clone());
                    }
                    if let Some(bus) = &cb.bus {
                        c = c.with_bus(bus.clone());
                    }
                    c
                });
                let retry_response = llm::call_model_and_ctx(
                    config_ref,
                    retry_ctx.as_ref(),
                    system_prompt,
                    &user_prompt,
                    0.1,
                    max_tokens,
                )
                .await?;

                llm::extract_json(&retry_response).map_err(|e| {
                    anyhow!(
                        "Step '{}': JSON parse failed after retry at temp 0.1: {}",
                        step.name,
                        e
                    )
                })
            }
        }
    }
}

/// Short display name for a model string (last segment after /).
fn short_model_name(model: &str) -> &str {
    model.rsplit('/').next().unwrap_or(model)
}

// ── Mechanical dispatch ─────────────────────────────────────────────────────

/// Known mechanical function names for the v1 registry.
///
/// Post-build accretion v5 Phase 5 adds three v5 targets used by
/// the starter chains that back role_bound dispatches (cascade handlers, the
/// meta-layer oracle, etc.). These are invoked from
/// `chain_executor::execute_chain_for_target` via `dispatch_step` /
/// `dispatch_mechanical`.
///
/// Post-build accretion v5 Phase 6b adds `call_starter_chain` — a sub-chain
/// invocation primitive. Phase 7 consumers (debate_steward, meta-layer
/// oracle, synthesizer) will author chains that call the library chains
/// `starter-evidence-tester` and `starter-reconciler` through this primitive.
const MECHANICAL_FUNCTIONS: &[&str] = &[
    "extract_import_graph",
    "extract_mechanical_metadata",
    "cluster_by_imports",
    "cluster_by_entity_overlap",
    // Post-build accretion v5 Phase 5 — role_bound chain primitives.
    "emit_cascade_handler_invoked",
    "queue_re_distill_for_target",
    "log_and_complete",
    // Post-build accretion v5 Phase 6b — sub-chain invocation primitive.
    "call_starter_chain",
    // Post-build accretion v5 Phase 7a — debate_steward chain primitives.
    "emit_debate_steward_invoked",
    "load_annotation_and_target",
    "append_annotation_to_debate_node",
    // Post-build accretion v5 Phase 7b — meta_layer_oracle upgrade + synthesizer chain.
    // v5 audit P4: `dispatch_synthesizer` wrapper removed — the oracle
    // YAML now calls starter-synthesizer via call_starter_chain with
    // $ref threading directly. The starter runner resolves $refs inside
    // step.input as of P4, so the wrapper is no longer needed.
    "emit_oracle_invoked",
    "decide_crystallization",
    "oracle_finalize",
    "emit_synthesizer_invoked",
    "load_substrate_nodes",
    "create_meta_layer_node",
    // Post-build accretion v5 Phase 7c — gap_dispatcher chain primitives.
    "emit_dispatcher_invoked",
    "load_gap_context",
    "materialize_gap_node",
    // Post-build accretion v5 Phase 7d — utility chain primitives (judge,
    // authorize_question, accretion_handler, sweep). Phase 7d ships the
    // four genesis-bound utility roles as real chains (judge + accretion +
    // sweep + authorize_question), not placeholders. The judge chain's LLM
    // step reuses the generic LLM dispatch path; the mechanical primitives
    // below back the non-LLM steps.
    "emit_judge_invoked",
    "emit_authorize_invoked",
    "load_slug_purpose",
    "emit_accretion_invoked",
    "load_recent_annotations_for_slug",
    "emit_accretion_written",
    "emit_sweep_invoked",
    "count_stale_failed_work_items",
    "reindex_vocab_cache",
    // Post-build accretion v5 Phase 9b-3 — sweep archive mechanicals.
    "archive_stale_failed_work_items",
    "retire_superseded_contributions_past_retention",
];

/// Post-build accretion v5 Phase 6b: hard ceiling on sub-chain recursion.
///
/// `call_starter_chain` lets one chain invoke another by id. Without a depth
/// guard a cyclic chain graph (chain_A calls chain_B which calls chain_A)
/// would recurse indefinitely. The guard is checked *before* the nested
/// `execute_chain_for_target` invocation and raises loudly when the
/// current `sub_chain_depth` reaches this ceiling.
///
/// This is a constraint on CHAIN BEHAVIOR (recursion depth of the orchestrator),
/// NOT a constraint on LLM output — `feedback_pillar37_no_hedging` specifically
/// exempts such mechanical guards. 5 is deep enough for any realistic
/// evidence-tester→reconciler→synthesizer nesting the Phase 7 consumers
/// would author, and shallow enough to surface a cycle long before it eats
/// the stack.
pub const MAX_SUB_CHAIN_DEPTH: usize = 5;

/// Dispatch a mechanical step to a named Rust function.
///
/// For v1, the actual build.rs functions require signatures that don't match
/// the generic `(input: &Value, ctx: &ChainDispatchContext) -> Result<Value>` contract.
/// The dispatch framework is established here; actual wiring happens in Phase 5
/// when the chain executor replaces the hardcoded build pipeline.
///
/// Phase 5 post-build-accretion v5: made `async` so the v5 role_bound primitives
/// (emit_cascade_handler_invoked, queue_re_distill_for_target) can `.await` the
/// writer mutex and perform a short SQLite INSERT without resorting to
/// `block_in_place` tricks. Existing placeholder arms don't await and are
/// unaffected.
async fn dispatch_mechanical(function_name: &str, input: &Value, ctx: &ChainDispatchContext) -> Result<Value> {
    match function_name {
        "extract_import_graph" => {
            info!("[mechanical] extract_import_graph (placeholder)");
            // Phase 5: wire to build::extract_import_graph(conn, slug, writer_tx)
            // For now, return a stub that matches the ImportGraph shape
            Ok(serde_json::json!({
                "_mechanical": "extract_import_graph",
                "_status": "placeholder",
                "slug": ctx.slug,
                "input": input,
            }))
        }
        "extract_mechanical_metadata" => {
            info!("[mechanical] extract_mechanical_metadata (placeholder)");
            Ok(serde_json::json!({
                "_mechanical": "extract_mechanical_metadata",
                "_status": "placeholder",
                "slug": ctx.slug,
                "input": input,
            }))
        }
        "cluster_by_imports" => {
            info!("[mechanical] cluster_by_imports (placeholder)");
            Ok(serde_json::json!({
                "_mechanical": "cluster_by_imports",
                "_status": "placeholder",
                "input": input,
            }))
        }
        "cluster_by_entity_overlap" => {
            info!("[mechanical] cluster_by_entity_overlap (placeholder)");
            Ok(serde_json::json!({
                "_mechanical": "cluster_by_entity_overlap",
                "_status": "placeholder",
                "input": input,
            }))
        }
        // ── Post-build accretion v5 Phase 5 role_bound primitives ───────────
        //
        // These functions back the starter chains invoked by the supervisor's
        // role_bound dispatch arm. The input shape is free-form JSON; each
        // primitive reads only the fields it needs and tolerates missing
        // ones (work items built by the compiler don't set every field).
        "emit_cascade_handler_invoked" => {
            // Writes a `cascade_handler_invoked` observation event for
            // chronicle traceability. Every cascade handler (role_bound)
            // starter chain emits this at step 1 so the chronicle records
            // that a handler actually fired for the triggering target.
            let target_node_id = input
                .get("target_node_id")
                .or_else(|| input.get("target_id"))
                .and_then(|v| v.as_str());
            let reason = input
                .get("reason")
                .and_then(|v| v.as_str());
            // Preserve any additional metadata already carried by the step's
            // input (work item id, step name, etc.) so downstream observers
                    // have full context.
            let mut metadata = serde_json::Map::new();
            if let Some(obj) = input.as_object() {
                for (k, v) in obj {
                    if k != "target_node_id" && k != "target_id" && k != "reason" {
                        metadata.insert(k.clone(), v.clone());
                    }
                }
            }
            if let Some(r) = reason {
                metadata.insert("reason".to_string(), Value::String(r.to_string()));
            }
            let metadata_json = if metadata.is_empty() {
                None
            } else {
                Some(serde_json::to_string(&Value::Object(metadata))?)
            };

            info!(
                "[mechanical] emit_cascade_handler_invoked slug={} target={:?} reason={:?}",
                ctx.slug, target_node_id, reason
            );
            // Short INSERT via the writer mutex. The async lock awaits the
            // writer briefly — OK because chain runners are already async
            // and no other await happens while the guard is held.
            let conn_guard = ctx.db_writer.lock().await;
            let event_id = super::observation_events::write_observation_event(
                &conn_guard,
                &ctx.slug,
                "chain",
                "cascade_handler_invoked",
                None,
                None,
                None,
                None,
                target_node_id,
                None,
                metadata_json.as_deref(),
            )?;
            drop(conn_guard);
            Ok(serde_json::json!({ "emitted": true, "event_id": event_id }))
        }
        "queue_re_distill_for_target" => {
            // Creates a `re_distill` work item for the target node. Preserves
            // the pre-v5 cascade-always-re-distills behavior under the v5
            // role-binding model — used by starter-cascade-immediate-redistill
            // (the backfill default for existing pyramids).
            //
            // Schema of dadbear_work_items follows the Phase 3 supervisor's
            // compile path (see db::init_pyramid_db for column list).
            let target_node_id = input
                .get("target_node_id")
                .or_else(|| input.get("target_id"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!(
                    "queue_re_distill_for_target: input missing target_node_id / target_id field"
                ))?
                .to_string();
            let layer: i64 = input
                .get("layer")
                .and_then(|v| v.as_i64())
                .unwrap_or(1);
            let reason = input
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("cascade re-distill via starter chain")
                .to_string();
            info!(
                "[mechanical] queue_re_distill_for_target slug={} target={} layer={} reason={}",
                ctx.slug, target_node_id, layer, reason
            );
            // Phase 8 tail-2: propagate observation_event_ids from the
            // triggering (role_bound) work item onto the queued
            // re_distill. This is the routing breadcrumb the supervisor
            // arm uses to resolve `annotated_node_id` metadata when it
            // calls `execute_supersession(..., annotated_node_ids=Some)`.
            // Pre-tail-2 this field was hard-coded to "[]", so the
            // supervisor had no way to find the descendant annotation —
            // the re-distill target (an ancestor) holds no annotations
            // of its own and the prompt's cascade_annotations section
            // was empty, producing no-op manifests for 14/15 annotation
            // types. Carrying the event ids forward closes the routing
            // gap the Phase 8 wanderer flagged.
            let triggering_wi_id = input
                .get("work_item_id")
                .and_then(|v| v.as_str())
                .map(String::from);
            let wi_id = format!("wi-{}", uuid::Uuid::new_v4());
            let conn_guard = ctx.db_writer.lock().await;
            let propagated_obs_ids: String = if let Some(trig_id) =
                triggering_wi_id.as_deref()
            {
                let obs_ids: Option<String> = conn_guard
                    .query_row(
                        "SELECT observation_event_ids FROM dadbear_work_items WHERE id = ?1",
                        rusqlite::params![trig_id],
                        |row| row.get(0),
                    )
                    .ok();
                match obs_ids {
                    Some(s) if !s.trim().is_empty() => s,
                    _ => "[]".to_string(),
                }
            } else {
                "[]".to_string()
            };
            let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
            let epoch_id = format!("epoch-chain-{}", uuid::Uuid::new_v4());
            let batch_id = format!("batch-chain-{}", uuid::Uuid::new_v4());
            let metadata = serde_json::json!({
                "queued_by_chain": "starter-chain",
                "reason": reason,
                "triggering_work_item_id": triggering_wi_id,
            });
            conn_guard.execute(
                "INSERT INTO dadbear_work_items
                    (id, slug, batch_id, epoch_id, step_name, primitive,
                     layer, target_id, system_prompt, user_prompt, model_tier,
                     result_json, observation_event_ids, compiled_at,
                     state, state_changed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                         ?12, ?13, ?14, ?15, ?16)",
                rusqlite::params![
                    wi_id,              // ?1  id
                    ctx.slug,           // ?2  slug
                    batch_id,           // ?3  batch_id
                    epoch_id,           // ?4  epoch_id
                    "cascade_re_distill", // ?5 step_name
                    "re_distill",       // ?6  primitive
                    layer,              // ?7  layer
                    target_node_id,     // ?8  target_id
                    "",                 // ?9  system_prompt
                    "",                 // ?10 user_prompt
                    "mid",              // ?11 model_tier
                    metadata.to_string(), // ?12 result_json
                    propagated_obs_ids, // ?13 observation_event_ids
                    now,                // ?14 compiled_at
                    "compiled",         // ?15 state
                    now,                // ?16 state_changed_at
                ],
            ).map_err(|e| anyhow!(
                "queue_re_distill_for_target: failed to insert work item: {}", e
            ))?;
            drop(conn_guard);
            Ok(serde_json::json!({ "queued": true, "work_item_id": wi_id }))
        }
        "log_and_complete" => {
            // MVP starter-chain no-op: log the step name + a small input
            // summary and return input unchanged. Starter chains whose v6+
            // LLM logic is pending land here so the work item still CASes
            // to `applied` (proving the pipeline) without a silent drop.
            //
            // feedback_loud_deferrals compliance: this is a deliberate,
            // documented no-op that surfaces in the log — not a silent stub.
            let summary = match input {
                Value::Object(o) => {
                    let mut keys: Vec<&str> = o.keys().map(|k| k.as_str()).collect();
                    keys.sort();
                    format!("object keys=[{}]", keys.join(", "))
                }
                Value::Array(a) => format!("array len={}", a.len()),
                Value::String(s) => format!("string len={}", s.len()),
                Value::Null => "null".to_string(),
                other => format!("scalar={}", other),
            };
            info!(
                "[mechanical] log_and_complete slug={} input_summary={}",
                ctx.slug, summary
            );
            Ok(input.clone())
        }
        // ── Post-build accretion v5 Phase 7a: debate_steward primitives ─────
        //
        // The three primitives below back `starter-debate-steward.yaml`, the
        // chain dispatched when a `steel_man` / `red_team` annotation fires
        // `annotation_reacted`. Each accepts the starter-runner threaded
        // input shape (target_node_id / work_item_id / slug merged by the
        // executor) and reads what it needs. Loud-raise on missing required
        // fields per `feedback_loud_deferrals`.
        "emit_debate_steward_invoked" => {
            // Chronicle-only observability event. Writes one row into
            // `dadbear_observation_events` naming the target + annotation.
            //
            // Passes the input object through to the output so later steps
            // in `starter-debate-steward` keep seeing the work_item_id /
            // target_id / annotation_id fields the supervisor stamped on
            // the initial call. The chain executor's step-to-step threading
            // only re-merges `target_node_id` + `slug`; every other field
            // (work_item_id, annotation_id, annotation_type) must survive
            // via output. Without this merge `load_annotation_and_target`
            // would lose `work_item_id` after step 1 and fail the
            // triggering-event backfill.
            let target_node_id = input
                .get("target_node_id")
                .or_else(|| input.get("target_id"))
                .and_then(|v| v.as_str());
            let annotation_id = input.get("annotation_id").and_then(|v| v.as_i64());
            let annotation_type = input
                .get("annotation_type")
                .and_then(|v| v.as_str());
            let mut meta = serde_json::Map::new();
            if let Some(tid) = target_node_id {
                meta.insert(
                    "target_node_id".to_string(),
                    Value::String(tid.to_string()),
                );
            }
            if let Some(aid) = annotation_id {
                meta.insert("annotation_id".to_string(), Value::from(aid));
            }
            if let Some(at) = annotation_type {
                meta.insert(
                    "annotation_type".to_string(),
                    Value::String(at.to_string()),
                );
            }
            let metadata_json = if meta.is_empty() {
                None
            } else {
                Some(serde_json::to_string(&Value::Object(meta))?)
            };
            info!(
                "[mechanical] emit_debate_steward_invoked slug={} target={:?} annotation_id={:?} annotation_type={:?}",
                ctx.slug, target_node_id, annotation_id, annotation_type
            );
            let conn_guard = ctx.db_writer.lock().await;
            let event_id = super::observation_events::write_observation_event(
                &conn_guard,
                &ctx.slug,
                "chain",
                "debate_steward_invoked",
                None,
                None,
                None,
                None,
                target_node_id,
                None,
                metadata_json.as_deref(),
            )?;
            drop(conn_guard);
            let mut out = if let Value::Object(obj) = input {
                obj.clone()
            } else {
                serde_json::Map::new()
            };
            out.insert("emitted".to_string(), Value::from(true));
            out.insert("event_id".to_string(), Value::from(event_id));
            Ok(Value::Object(out))
        }
        "load_annotation_and_target" => {
            // Resolves the triggering observation-event metadata (the
            // annotation_id + annotation_type), loads the matching
            // `pyramid_annotations` row, and reads the target node's
            // shape + payload. Returned as a single object that the next
            // step (append_annotation_to_debate_node) threads on.
            //
            // `annotation_id` / `annotation_type` can be provided directly
            // in the input envelope (future callers) OR derived by looking
            // up the triggering observation event via the work_item's
            // observation_event_ids column (today's path — the supervisor
            // role_bound arm builds input from only work_item_id/step_name/
            // target_id/layer, so metadata is recovered here).
            let target_node_id = input
                .get("target_node_id")
                .or_else(|| input.get("target_id"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!(
                    "load_annotation_and_target: missing target_node_id"
                ))?
                .to_string();

            let mut annotation_id = input.get("annotation_id").and_then(|v| v.as_i64());
            let mut annotation_type = input
                .get("annotation_type")
                .and_then(|v| v.as_str())
                .map(String::from);

            // Back-fill from the triggering observation event if the
            // caller didn't inline annotation metadata.
            if annotation_id.is_none() || annotation_type.is_none() {
                let work_item_id = input
                    .get("work_item_id")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                if let Some(wid) = work_item_id.as_deref() {
                    let conn_guard = ctx.db_reader.lock().await;
                    // Pull observation_event_ids; parse [N,...]; read the
                    // first event's metadata_json for annotation_* fields.
                    let obs_ids_json: Option<String> = conn_guard
                        .query_row(
                            "SELECT observation_event_ids FROM dadbear_work_items WHERE id = ?1",
                            rusqlite::params![wid],
                            |row| row.get(0),
                        )
                        .ok();
                    if let Some(ids_json) = obs_ids_json {
                        if let Ok(ids) = serde_json::from_str::<Vec<i64>>(&ids_json) {
                            if let Some(eid) = ids.first() {
                                let meta: Option<String> = conn_guard
                                    .query_row(
                                        "SELECT metadata_json FROM dadbear_observation_events WHERE id = ?1",
                                        rusqlite::params![eid],
                                        |row| row.get(0),
                                    )
                                    .ok()
                                    .flatten();
                                if let Some(m) = meta {
                                    if let Ok(v) = serde_json::from_str::<Value>(&m) {
                                        if annotation_id.is_none() {
                                            annotation_id =
                                                v.get("annotation_id").and_then(|x| x.as_i64());
                                        }
                                        if annotation_type.is_none() {
                                            annotation_type = v
                                                .get("annotation_type")
                                                .and_then(|x| x.as_str())
                                                .map(String::from);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    drop(conn_guard);
                }
            }

            // Load the annotation body (if we have an id) and the target
            // node's shape view.
            let conn_guard = ctx.db_reader.lock().await;

            let annotation_obj: Value = if let Some(aid) = annotation_id {
                let row: Option<(i64, String, String, String, Option<String>, String, String, String)> = conn_guard
                    .query_row(
                        "SELECT id, slug, node_id, annotation_type, question_context, author,
                                content, created_at
                         FROM pyramid_annotations WHERE id = ?1",
                        rusqlite::params![aid],
                        |r| Ok((
                            r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?,
                            r.get(5)?, r.get(6)?, r.get(7)?,
                        )),
                    )
                    .ok();
                if let Some((id, slug, node_id, aty, qctx, author, content, created_at)) = row {
                    // Keep annotation_type from the DB row canonical; the
                    // metadata-derived value was only a hint.
                    if annotation_type.as_deref() != Some(aty.as_str()) {
                        annotation_type = Some(aty.clone());
                    }
                    serde_json::json!({
                        "id": id,
                        "slug": slug,
                        "node_id": node_id,
                        "annotation_type": aty,
                        "question_context": qctx,
                        "author": author,
                        "content": content,
                        "created_at": created_at,
                    })
                } else {
                    Value::Null
                }
            } else {
                Value::Null
            };

            // Target node: (depth, headline, distilled) + shape view.
            let node_row: Option<(i64, String, String)> = conn_guard
                .query_row(
                    "SELECT depth, headline, distilled FROM pyramid_nodes
                     WHERE slug = ?1 AND id = ?2",
                    rusqlite::params![ctx.slug, target_node_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .ok();
            let shape_view = super::db::get_node_shape(&conn_guard, &ctx.slug, &target_node_id)?;
            drop(conn_guard);

            let target_obj = if let Some((depth, headline, distilled)) = node_row {
                let current_shape = shape_view
                    .as_ref()
                    .map(|v| v.shape.as_str().to_string())
                    .unwrap_or_else(|| "scaffolding".to_string());
                let current_payload = shape_view
                    .as_ref()
                    .and_then(|v| v.payload.as_ref())
                    .and_then(|p| serde_json::to_value(p).ok())
                    .unwrap_or(Value::Null);
                serde_json::json!({
                    "id": target_node_id,
                    "depth": depth,
                    "headline": headline,
                    "distilled": distilled,
                    "current_shape": current_shape,
                    "current_payload": current_payload,
                })
            } else {
                Value::Null
            };

            info!(
                "[mechanical] load_annotation_and_target slug={} target={} annotation_id={:?} annotation_type={:?}",
                ctx.slug, target_node_id, annotation_id, annotation_type,
            );

            // Preserve input fields so threading keeps work_item_id / layer
            // / step_name alive for any downstream step that wants them.
            let mut out = if let Value::Object(obj) = input {
                obj.clone()
            } else {
                serde_json::Map::new()
            };
            out.insert(
                "target_node_id".to_string(),
                Value::String(target_node_id.clone()),
            );
            out.insert(
                "annotation_id".to_string(),
                annotation_id.map(Value::from).unwrap_or(Value::Null),
            );
            out.insert(
                "annotation_type".to_string(),
                annotation_type
                    .map(Value::String)
                    .unwrap_or(Value::Null),
            );
            out.insert("annotation".to_string(), annotation_obj);
            out.insert("target_node".to_string(), target_obj);
            Ok(Value::Object(out))
        }
        "append_annotation_to_debate_node" => {
            // Core write. Given the target node + a steel_man / red_team
            // annotation, either:
            //   (a) Upgrade a Scaffolding node to Debate, seeding the first
            //       position (steel_man) or first red_team (red_team).
            //   (b) Append to an existing Debate's positions[] /
            //       red_teams[] (idempotent — the same annotation_id is
            //       never added twice).
            // Emits `debate_spawned` on the Scaffolding→Debate upgrade.
            //
            // The debate_role mapping (steel_man → position, red_team →
            // red_team) is hardcoded in this primitive today because the
            // vocabulary registry doesn't carry a `debate_role` field yet.
            // Phase 7b+ should lift this to a vocab attribute so new
            // debate-mode annotation types can be published without a
            // code deploy (feedback_generalize_not_enumerate).
            let target_node_id = input
                .get("target_node_id")
                .or_else(|| input.get("target_id"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!(
                    "append_annotation_to_debate_node: missing target_node_id"
                ))?
                .to_string();
            // Inputs may come as threaded context from load_annotation_and_target,
            // or may be set directly by callers.
            let annotation_id: Option<i64> = input.get("annotation_id").and_then(|v| v.as_i64());
            let annotation_type: Option<String> = input
                .get("annotation_type")
                .and_then(|v| v.as_str())
                .map(String::from);
            let annotation_obj = input.get("annotation").cloned().unwrap_or(Value::Null);

            // No annotation context → idempotent no-op. This path is hit
            // when the work item was compiled from a `debate_spawned`
            // observation event (which carries no annotation), keeping
            // the Phase 3 mapping stable while avoiding infinite
            // re-spawn: the second pass finds the target already Debate
            // AND no annotation to append, returns "no_op".
            if annotation_id.is_none() {
                info!(
                    "[mechanical] append_annotation_to_debate_node slug={} target={} → no_op (no annotation_id in input — likely debate_spawned retrigger)",
                    ctx.slug, target_node_id
                );
                return Ok(serde_json::json!({
                    "action": "no_op",
                    "reason": "no annotation_id in input",
                }));
            }
            let annotation_id = annotation_id.unwrap();

            // Pull annotation content + author, preferring the threaded
            // `annotation` object from load_annotation_and_target; fall back
            // to a DB read when absent.
            //
            // Verifier fix (Phase 7a): a missing annotation row was previously
            // papered over with `unwrap_or_default()` → empty content/author.
            // That's a silent deferral per feedback_loud_deferrals — the
            // caller only saw the downstream "unsupported annotation_type ''"
            // error without any pointer to the root cause (deleted / bad
            // annotation_id). Loud-raise here so the operator sees the real
            // issue.
            let ann_content: String;
            let ann_author: String;
            let mut annotation_type_from_db: Option<String> = None;
            {
                let threaded_content = annotation_obj
                    .get("content")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let threaded_author = annotation_obj
                    .get("author")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                if let (Some(c), Some(a)) = (threaded_content, threaded_author) {
                    ann_content = c;
                    ann_author = a;
                } else {
                    let conn_guard = ctx.db_reader.lock().await;
                    let row: Option<(String, String, String)> = conn_guard
                        .query_row(
                            "SELECT content, author, annotation_type FROM pyramid_annotations WHERE id = ?1",
                            rusqlite::params![annotation_id],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                        )
                        .ok();
                    drop(conn_guard);
                    match row {
                        Some((c, a, aty)) => {
                            ann_content = c;
                            ann_author = a;
                            annotation_type_from_db = Some(aty);
                        }
                        None => {
                            return Err(anyhow!(
                                "append_annotation_to_debate_node: annotation_id={} not \
                                 found in pyramid_annotations — stale event or deleted row. \
                                 Target '{}' will not be mutated.",
                                annotation_id,
                                target_node_id,
                            ));
                        }
                    }
                }
            };

            // Resolve annotation_type: prefer threaded input, otherwise the
            // DB-row capture above, otherwise a final direct lookup. Loud
            // raise if all three paths yield None (bad id) or an empty string.
            let annotation_type = match annotation_type
                .or(annotation_type_from_db)
            {
                Some(t) if !t.is_empty() => t,
                _ => {
                    let conn_guard = ctx.db_reader.lock().await;
                    let row: Option<String> = conn_guard
                        .query_row(
                            "SELECT annotation_type FROM pyramid_annotations WHERE id = ?1",
                            rusqlite::params![annotation_id],
                            |r| r.get(0),
                        )
                        .ok();
                    drop(conn_guard);
                    match row {
                        Some(t) if !t.is_empty() => t,
                        _ => {
                            return Err(anyhow!(
                                "append_annotation_to_debate_node: annotation_id={} has no \
                                 annotation_type (row missing or empty). Target '{}' will \
                                 not be mutated.",
                                annotation_id,
                                target_node_id,
                            ));
                        }
                    }
                }
            };

            // Hardcoded debate-role mapping (future: vocab-driven).
            let is_steel_man = annotation_type == "steel_man";
            let is_red_team = annotation_type == "red_team";
            if !is_steel_man && !is_red_team {
                return Err(anyhow!(
                    "append_annotation_to_debate_node: unsupported annotation_type '{}' — \
                     only steel_man / red_team carry a debate_role today. Publish a vocab \
                     entry with a debate_role field (Phase 7b+) to extend.",
                    annotation_type
                ));
            }

            // Read current shape to decide create vs append. We use the
            // writer mutex for the whole transition so concurrent
            // annotations on the same target serialize.
            let conn_guard = ctx.db_writer.lock().await;
            let shape_view =
                super::db::get_node_shape(&conn_guard, &ctx.slug, &target_node_id)?;
            let current_shape = shape_view
                .as_ref()
                .map(|v| v.shape.clone())
                .unwrap_or_else(NodeShape::scaffolding);

            let position_label_for_steel_man = format!("annotation#{annotation_id}");
            let red_team_from_position = "main";

            let (action, updated_debate, shape_was_upgraded) = if current_shape.is_scaffolding() {
                // Create fresh Debate.
                let (positions, action_label) = if is_steel_man {
                    // v5 audit P6: record the annotation id under the
                    // dedicated `source_annotation_ids` channel. The
                    // position-LABEL also carries `annotation#{id}` today
                    // because a steel-manning position from an external
                    // author has no named stance — that's a separate
                    // labeling concern. `evidence_anchors` stays empty:
                    // genuine node-id refs only.
                    let annotation_token = format!("annotation#{annotation_id}");
                    (
                        vec![DebatePosition {
                            label: position_label_for_steel_man.clone(),
                            steel_manning: ann_content.clone(),
                            red_teams: vec![],
                            evidence_anchors: vec![],
                            source_annotation_ids: vec![annotation_token],
                        }],
                        "created_debate",
                    )
                } else {
                    // Red team without any existing position: seed an
                    // empty "main" position carrying the red_team.
                    // v5 audit P6: `annotation#{id}` goes on
                    // `source_annotation_ids` for idempotency dedup;
                    // `evidence_anchors` is reserved for genuine node-id
                    // refs (empty on first seed).
                    let annotation_token = format!("annotation#{annotation_id}");
                    (
                        vec![DebatePosition {
                            label: red_team_from_position.to_string(),
                            steel_manning: String::new(),
                            red_teams: vec![RedTeamEntry {
                                from_position: red_team_from_position.to_string(),
                                argument: ann_content.clone(),
                                evidence_anchors: vec![],
                                source_annotation_ids: vec![annotation_token],
                            }],
                            evidence_anchors: vec![],
                            source_annotation_ids: vec![],
                        }],
                        "created_debate",
                    )
                };
                let concern_line = input
                    .get("target_node")
                    .and_then(|t| t.get("headline"))
                    .and_then(|v| v.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| {
                        format!("Debate on node {}", target_node_id)
                    });
                let debate = DebateTopic {
                    concern: concern_line,
                    positions,
                    cross_refs: vec![],
                    vote_lean: None,
                };
                (action_label, debate, true)
            } else if current_shape.as_str() == NODE_SHAPE_DEBATE {
                // Mutate existing debate payload. Idempotent — we don't
                // append if the annotation's author already appears for
                // the same label.
                let mut debate = match shape_view.unwrap().payload {
                    Some(super::types::ShapePayload::Debate(d)) => d,
                    other => {
                        return Err(anyhow!(
                            "append_annotation_to_debate_node: target '{}' is shape 'debate' but \
                             payload does not deserialize as DebateTopic (got {:?})",
                            target_node_id,
                            other.is_some()
                        ));
                    }
                };
                let action_label = if is_steel_man {
                    let label = position_label_for_steel_man.clone();
                    let annotation_token = format!("annotation#{annotation_id}");
                    // v5 audit P6: idempotency check widened to match the
                    // new field (catch replays regardless of which side of
                    // the split wrote the existing row, for forward/back
                    // compat during rollover).
                    let already = debate.positions.iter().any(|p| {
                        p.label == label
                            || (p.steel_manning == ann_content && !ann_content.is_empty())
                            || p.source_annotation_ids
                                .iter()
                                .any(|a| a == &annotation_token)
                    });
                    if !already {
                        debate.positions.push(DebatePosition {
                            label,
                            steel_manning: ann_content.clone(),
                            red_teams: vec![],
                            evidence_anchors: vec![],
                            source_annotation_ids: vec![annotation_token],
                        });
                    }
                    if already { "no_op" } else { "appended_position" }
                } else {
                    // red_team → append to the first (or "main") position's
                    // red_teams[]. If none exist yet, seed one.
                    //
                    // Verifier fix (Phase 7a): `from_position` is a POSITION
                    // LABEL per the schema (see the round-trip test at
                    // `db::tests::parse_shape_payload_round_trips_populated_debate_with_red_teams_and_votes`
                    // — "Pro" red-teams "Con", "Con" red-teams "Pro"). An
                    // external (annotation-sourced) red_team has no opposing
                    // position to attribute to, so we stamp the LABEL of the
                    // position being red-teamed. This matches the
                    // Scaffolding→Debate seed path (line below, `red_team_from_position = "main"`),
                    // keeps the schema semantics consistent, and avoids
                    // leaking the author's name into a position-label slot.
                    if debate.positions.is_empty() {
                        debate.positions.push(DebatePosition {
                            label: red_team_from_position.to_string(),
                            steel_manning: String::new(),
                            red_teams: vec![],
                            evidence_anchors: vec![],
                            source_annotation_ids: vec![],
                        });
                    }
                    let pos = debate.positions.first_mut().unwrap();
                    let from_position_label = pos.label.clone();
                    // v5 audit P6: idempotency now keyed on
                    // `source_annotation_ids` (not `evidence_anchors` —
                    // that field is for genuine node-id refs). Legacy
                    // rows that stamped the token into `evidence_anchors`
                    // still dedup under the second clause so rollover is
                    // seamless during the migration window.
                    let annotation_token = format!("annotation#{annotation_id}");
                    let already = pos.red_teams.iter().any(|r| {
                        r.argument == ann_content
                            && (r.source_annotation_ids
                                .iter()
                                .any(|a| a == &annotation_token)
                                || r.evidence_anchors.iter().any(|a| a == &annotation_token))
                    });
                    if !already {
                        pos.red_teams.push(RedTeamEntry {
                            from_position: from_position_label,
                            argument: ann_content.clone(),
                            evidence_anchors: vec![],
                            source_annotation_ids: vec![annotation_token],
                        });
                    }
                    if already { "no_op" } else { "appended_red_team" }
                };
                (action_label, debate, false)
            } else {
                return Err(anyhow!(
                    "append_annotation_to_debate_node: target '{}' has shape '{}' — only \
                     Scaffolding and Debate are supported by this primitive.",
                    target_node_id,
                    current_shape
                ));
            };

            // Write back. Scaffolding → Debate sets node_shape =
            // 'debate'; existing Debate just updates shape_payload_json.
            let payload_json = serde_json::to_string(&updated_debate)?;
            conn_guard.execute(
                "UPDATE pyramid_nodes
                 SET node_shape = ?1, shape_payload_json = ?2
                 WHERE slug = ?3 AND id = ?4",
                rusqlite::params![
                    NODE_SHAPE_DEBATE,
                    payload_json,
                    ctx.slug,
                    target_node_id,
                ],
            )?;

            // Emit debate_spawned only on the shape upgrade (and only
            // when we actually did a write — "no_op" still gets here but
            // shape_was_upgraded is false).
            let mut spawned_event_id: Option<i64> = None;
            if shape_was_upgraded {
                let initial_label = updated_debate
                    .positions
                    .first()
                    .map(|p| p.label.clone())
                    .unwrap_or_default();
                let initial_kind = if is_steel_man { "steel_man" } else { "red_team" };
                let meta = serde_json::json!({
                    "target_node_id": target_node_id,
                    "initial_position_label": initial_label,
                    "initial_position_or_red_team": initial_kind,
                    "annotation_id": annotation_id,
                })
                .to_string();
                let eid = super::observation_events::write_observation_event(
                    &conn_guard,
                    &ctx.slug,
                    "chain",
                    "debate_spawned",
                    None,
                    None,
                    None,
                    None,
                    Some(&target_node_id),
                    None,
                    Some(&meta),
                )?;
                spawned_event_id = Some(eid);
            }
            drop(conn_guard);

            info!(
                "[mechanical] append_annotation_to_debate_node slug={} target={} action={} debate_spawned_event_id={:?}",
                ctx.slug, target_node_id, action, spawned_event_id
            );

            let updated_payload_value =
                serde_json::to_value(&updated_debate).unwrap_or(Value::Null);
            Ok(serde_json::json!({
                "action": action,
                "target_node_id": target_node_id,
                "annotation_id": annotation_id,
                "annotation_type": annotation_type,
                "shape_was_upgraded": shape_was_upgraded,
                "debate_spawned_event_id": spawned_event_id,
                "updated_payload": updated_payload_value,
            }))
        }
        // ── Post-build accretion v5 Phase 7b: meta_layer_oracle + synthesizer ─
        //
        // The six primitives below back two starter chains:
        //   * starter-meta-layer-oracle.yaml — decides whether a
        //     purpose_shifted / gap_resolved event warrants a meta-layer,
        //     and hands off to the synthesizer when it does.
        //   * starter-synthesizer.yaml — produces the new meta-layer node.
        //
        // Scope-boundary discipline (feedback_loud_deferrals):
        //   - `decide_crystallization` uses a three-arm heuristic today.
        //     An UNKNOWN event_type returns should_crystallize=false WITH
        //     a non-empty reasoning string — this is a deliberate,
        //     documented skip path, not a silent stub. Phase 8+ replaces
        //     the heuristic with an LLM judge.
        //   - `create_meta_layer_node` never hardcodes LLM-output shape
        //     constraints (feedback_pillar37_no_hedging); counts /
        //     token budgets stay in YAML.
        "emit_oracle_invoked" => {
            // Chronicle trace for the meta_layer_oracle role's first step.
            // Stamps source=chain, event_type=meta_layer_oracle_invoked.
            // Metadata carries source_event_id (from the work item's
            // observation_event_ids column), purpose_id (resolved via
            // load_or_create_purpose), and the triggering event_type so
            // downstream observers can reconstruct what fired this chain
            // without joining back through work_items.
            //
            // Preserves the full input object as the step's output so
            // subsequent steps still see work_item_id / target_id / layer —
            // same preserve-through-output pattern as
            // emit_debate_steward_invoked (Phase 7a).
            let work_item_id = input
                .get("work_item_id")
                .and_then(|v| v.as_str())
                .map(String::from);
            let target_node_id = input
                .get("target_node_id")
                .or_else(|| input.get("target_id"))
                .and_then(|v| v.as_str());

            // Resolve source_event_id + trigger event_type via the work
            // item's observation_event_ids column. Best-effort: a missing
            // work_item_id / missing ids column degrades to None (still
            // useful for the meta_layer_crystallized flow which comes in
            // with an envelope rather than a work item).
            let (source_event_id, trigger_event_type): (Option<i64>, Option<String>) =
                if let Some(wid) = work_item_id.as_deref() {
                    let conn_guard = ctx.db_reader.lock().await;
                    let obs_ids_json: Option<String> = conn_guard
                        .query_row(
                            "SELECT observation_event_ids FROM dadbear_work_items WHERE id = ?1",
                            rusqlite::params![wid],
                            |row| row.get(0),
                        )
                        .ok();
                    let first_id: Option<i64> = obs_ids_json
                        .as_deref()
                        .and_then(|j| serde_json::from_str::<Vec<i64>>(j).ok())
                        .and_then(|v| v.first().copied());
                    let event_type: Option<String> = if let Some(eid) = first_id {
                        conn_guard
                            .query_row(
                                "SELECT event_type FROM dadbear_observation_events WHERE id = ?1",
                                rusqlite::params![eid],
                                |row| row.get(0),
                            )
                            .ok()
                    } else {
                        None
                    };
                    drop(conn_guard);
                    (first_id, event_type)
                } else {
                    (None, None)
                };

            // Resolve purpose_id. load_or_create_purpose is the canonical
            // read; we use the reader connection here because a stock
            // self-heal insert is still fine on the reader (rusqlite
            // sqlite3_open_v2 shares the same file; `create_slug` has
            // already run).
            let purpose_id: Option<i64> = {
                let conn_guard = ctx.db_reader.lock().await;
                super::purpose::load_or_create_purpose(&conn_guard, &ctx.slug)
                    .ok()
                    .map(|p| p.id)
            };

            let mut meta = serde_json::Map::new();
            if let Some(eid) = source_event_id {
                meta.insert("source_event_id".to_string(), Value::from(eid));
            }
            if let Some(pid) = purpose_id {
                meta.insert("purpose_id".to_string(), Value::from(pid));
            }
            if let Some(ref t) = trigger_event_type {
                meta.insert("trigger_event_type".to_string(), Value::String(t.clone()));
            }
            if let Some(tid) = target_node_id {
                meta.insert("target_node_id".to_string(), Value::String(tid.to_string()));
            }
            let metadata_json = if meta.is_empty() {
                None
            } else {
                Some(serde_json::to_string(&Value::Object(meta))?)
            };

            info!(
                "[mechanical] emit_oracle_invoked slug={} trigger={:?} purpose_id={:?} source_event_id={:?}",
                ctx.slug, trigger_event_type, purpose_id, source_event_id,
            );
            let conn_guard = ctx.db_writer.lock().await;
            let event_id = super::observation_events::write_observation_event(
                &conn_guard,
                &ctx.slug,
                "chain",
                "meta_layer_oracle_invoked",
                None,
                None,
                None,
                None,
                target_node_id,
                None,
                metadata_json.as_deref(),
            )?;
            drop(conn_guard);

            // Preserve input fields so threading keeps work_item_id / target
            // / trigger_event_type alive for subsequent steps (decide_
            // crystallization, dispatch_synthesizer).
            let mut out = if let Value::Object(obj) = input {
                obj.clone()
            } else {
                serde_json::Map::new()
            };
            out.insert("emitted".to_string(), Value::from(true));
            out.insert("event_id".to_string(), Value::from(event_id));
            if let Some(eid) = source_event_id {
                out.entry("source_event_id".to_string())
                    .or_insert(Value::from(eid));
            }
            if let Some(pid) = purpose_id {
                out.entry("purpose_id".to_string())
                    .or_insert(Value::from(pid));
            }
            if let Some(ref t) = trigger_event_type {
                out.entry("trigger_event_type".to_string())
                    .or_insert(Value::String(t.clone()));
            }
            Ok(Value::Object(out))
        }
        "decide_crystallization" => {
            // Heuristic decision: given the slug's active purpose + the
            // triggering event type, return
            //   { should_crystallize, purpose_question, reasoning,
            //     covered_substrate_nodes: [...] }
            //
            // Rules (Phase 7b MVP):
            //   purpose_shifted → always crystallize. The shift is a
            //     deliberate operator signal; covered_substrate_nodes
            //     defaults to the current L0 node ids (substrate for the
            //     new meta-layer is the whole lower pyramid).
            //   gap_resolved    → crystallize iff the originating gap
            //     carried candidate_resolutions (there IS substrate to
            //     synthesize on). If no candidates, skip with reasoning.
            //   else            → skip with reasoning naming the event.
            //
            // UNKNOWN event types return should_crystallize=false WITH a
            // non-empty `reasoning` field — this is a DELIBERATE skip
            // path, not a silent stub (feedback_loud_deferrals). Skipping
            // on unknown events is the correct semantic: the oracle only
            // acts on events it understands. Adding a new crystallization
            // trigger requires extending this heuristic (or Phase 8+'s
            // LLM judge) explicitly.
            //
            // Note on feedback_generalize_not_enumerate: if this match
            // grows beyond ~3 arms, lift it to a vocab lookup where each
            // role-triggering event_type carries a crystallization hint
            // in its vocab entry. Today the three arms are load-bearing
            // and well-understood; extracting prematurely would add
            // indirection without benefit.
            let trigger_event_type = input
                .get("trigger_event_type")
                .and_then(|v| v.as_str())
                .map(String::from);

            // Load active purpose for the slug so purpose_question (the
            // LLM input) is rooted in the operator's declaration rather
            // than a stand-in.
            let purpose = {
                let conn_guard = ctx.db_reader.lock().await;
                super::purpose::load_or_create_purpose(&conn_guard, &ctx.slug)?
            };
            let purpose_question = purpose.purpose_text.clone();
            let purpose_id = purpose.id;

            // Resolve covered_substrate_nodes from whatever context we have.
            // For purpose_shifted events there's no target, so we default
            // to the slug's current L0 node ids. For gap_resolved the
            // target_node_id IS the gap, and its candidate_resolutions
            // (read via get_node_shape) determine whether there's
            // substrate.
            let target_node_id = input
                .get("target_node_id")
                .or_else(|| input.get("target_id"))
                .and_then(|v| v.as_str())
                .map(String::from);

            let (should_crystallize, reasoning, covered_substrate_nodes): (
                bool,
                String,
                Vec<String>,
            ) = match trigger_event_type.as_deref() {
                Some("purpose_shifted") => {
                    // Full L0 is the substrate for a purpose-shift meta-layer.
                    let conn_guard = ctx.db_reader.lock().await;
                    let nodes = super::db::get_nodes_at_depth(&conn_guard, &ctx.slug, 0)
                        .unwrap_or_default();
                    drop(conn_guard);
                    let ids: Vec<String> = nodes.into_iter().map(|n| n.id).collect();
                    if ids.is_empty() {
                        // Phase 7b verifier: purpose_shifted on a slug with
                        // no L0 substrate would have produced a meta-layer
                        // synthesized over nothing — the writer now raises
                        // on empty topics, but the oracle itself should
                        // skip rather than dispatch the synthesizer into a
                        // guaranteed failure. feedback_loud_deferrals: the
                        // skip is visible via oracle_finalize's
                        // meta_layer_oracle_skipped event.
                        let reason =
                            "purpose_shifted fired but slug has no L0 substrate to synthesize \
                             over; skipping crystallization. Supersede purpose after at least \
                             one L0 node exists for a meaningful meta-layer."
                                .to_string();
                        (false, reason, vec![])
                    } else {
                        let reason = format!(
                            "purpose_shifted is a deliberate operator signal; crystallizing a \
                             meta-layer over L0 substrate ({} node(s)) aligned to the new purpose.",
                            ids.len()
                        );
                        (true, reason, ids)
                    }
                }
                Some("gap_resolved") => {
                    // Inspect the gap node's candidate_resolutions.
                    let has_candidates = if let Some(tid) = target_node_id.as_deref() {
                        let conn_guard = ctx.db_reader.lock().await;
                        let view = super::db::get_node_shape(&conn_guard, &ctx.slug, tid).ok().flatten();
                        drop(conn_guard);
                        match view.and_then(|v| v.payload) {
                            Some(super::types::ShapePayload::Gap(g)) => {
                                !g.candidate_resolutions.is_empty()
                            }
                            _ => false,
                        }
                    } else {
                        false
                    };
                    if has_candidates {
                        let ids: Vec<String> = target_node_id
                            .as_ref()
                            .map(|t| vec![t.clone()])
                            .unwrap_or_default();
                        let reason = format!(
                            "gap_resolved on '{}' carried candidate_resolutions — substrate \
                             exists for a meta-layer synthesis.",
                            target_node_id.as_deref().unwrap_or("<unknown>")
                        );
                        (true, reason, ids)
                    } else {
                        let reason = format!(
                            "gap_resolved on '{}' has no candidate_resolutions — no substrate \
                             to synthesize on; skipping crystallization.",
                            target_node_id.as_deref().unwrap_or("<unknown>")
                        );
                        (false, reason, vec![])
                    }
                }
                Some(other) => {
                    let reason = format!(
                        "event_type '{}' is not a crystallization trigger in the Phase 7b \
                         heuristic — skipping. Extend decide_crystallization (or wait for \
                         Phase 8+'s LLM judge) to opt new triggers in.",
                        other
                    );
                    (false, reason, vec![])
                }
                None => {
                    // No triggering event type visible — the chain was
                    // invoked outside the normal observation-event path.
                    // Skip with reasoning, don't raise: both the oracle's
                    // own dispatch_synthesizer (when:-gated) and the
                    // terminal log_and_complete still fire, CASing the
                    // work item to `applied`.
                    (
                        false,
                        "no trigger_event_type in input envelope — skipping crystallization \
                         (chain invoked outside the observation-event path)."
                            .to_string(),
                        vec![],
                    )
                }
            };

            info!(
                "[mechanical] decide_crystallization slug={} trigger={:?} should_crystallize={} \
                 substrate_count={} purpose_id={}",
                ctx.slug,
                trigger_event_type,
                should_crystallize,
                covered_substrate_nodes.len(),
                purpose_id,
            );

            let mut out = if let Value::Object(obj) = input {
                obj.clone()
            } else {
                serde_json::Map::new()
            };
            out.insert(
                "should_crystallize".to_string(),
                Value::from(should_crystallize),
            );
            out.insert(
                "purpose_question".to_string(),
                Value::String(purpose_question),
            );
            out.insert("reasoning".to_string(), Value::String(reasoning));
            out.insert(
                "covered_substrate_nodes".to_string(),
                Value::Array(
                    covered_substrate_nodes
                        .into_iter()
                        .map(Value::String)
                        .collect(),
                ),
            );
            out.insert("purpose_id".to_string(), Value::from(purpose_id));
            // v5 audit P4: explicitly set parent_meta_layer_id (null by
            // default in the Phase 7b heuristic — no parent meta-layer
            // lineage today) so downstream callers can $ref it without
            // tripping the starter runner's loud-resolve. Phase 8+ LLM
            // judge may populate it when chaining meta-layers.
            if !out.contains_key("parent_meta_layer_id") {
                out.insert("parent_meta_layer_id".to_string(), Value::Null);
            }
            Ok(Value::Object(out))
        }
        "emit_synthesizer_invoked" => {
            // Chronicle trace for the synthesizer role's first step.
            // source=chain, event_type=synthesizer_invoked.
            // Metadata carries the purpose_question + covered_substrate_nodes
            // count so chronicle consumers can see WHY the synthesizer
            // fired without re-reading the work item's observation_event_ids.
            let purpose_question = input
                .get("purpose_question")
                .and_then(|v| v.as_str())
                .map(String::from);
            let covered_substrate_nodes: Vec<String> = input
                .get("covered_substrate_nodes")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let parent_meta_layer_id = input
                .get("parent_meta_layer_id")
                .and_then(|v| v.as_str())
                .map(String::from);

            let mut meta = serde_json::Map::new();
            if let Some(ref q) = purpose_question {
                meta.insert(
                    "purpose_question".to_string(),
                    Value::String(q.chars().take(200).collect()),
                );
            }
            meta.insert(
                "covered_substrate_node_count".to_string(),
                Value::from(covered_substrate_nodes.len() as i64),
            );
            if let Some(ref p) = parent_meta_layer_id {
                meta.insert(
                    "parent_meta_layer_id".to_string(),
                    Value::String(p.clone()),
                );
            }
            let metadata_json = Some(serde_json::to_string(&Value::Object(meta))?);

            info!(
                "[mechanical] emit_synthesizer_invoked slug={} substrate_count={} parent={:?}",
                ctx.slug,
                covered_substrate_nodes.len(),
                parent_meta_layer_id,
            );
            let conn_guard = ctx.db_writer.lock().await;
            let event_id = super::observation_events::write_observation_event(
                &conn_guard,
                &ctx.slug,
                "chain",
                "synthesizer_invoked",
                None,
                None,
                None,
                None,
                None,
                None,
                metadata_json.as_deref(),
            )?;
            drop(conn_guard);

            let mut out = if let Value::Object(obj) = input {
                obj.clone()
            } else {
                serde_json::Map::new()
            };
            out.insert("emitted".to_string(), Value::from(true));
            out.insert("event_id".to_string(), Value::from(event_id));
            Ok(Value::Object(out))
        }
        "load_substrate_nodes" => {
            // Batch-read each node in `covered_substrate_nodes` + the
            // slug's active purpose. Returned object is the envelope the
            // synthesize_meta_layer LLM step receives as its prompt
            // context:
            //   {
            //     purpose_question, purpose_text, parent_meta_layer_id,
            //     nodes: [{id, distilled, topics}, ...],
            //     covered_substrate_nodes
            //   }
            // feedback_loud_deferrals: if a covered id doesn't resolve,
            // we omit it from `nodes` but keep it in `covered_substrate_nodes`
            // so the LLM is aware the caller listed it. A fully-empty
            // resolution set raises — the synthesizer has nothing to say.
            let purpose_question = input
                .get("purpose_question")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!(
                    "load_substrate_nodes: missing `purpose_question` in input"
                ))?
                .to_string();
            let parent_meta_layer_id = input
                .get("parent_meta_layer_id")
                .cloned()
                .unwrap_or(Value::Null);
            let covered_substrate_nodes: Vec<String> = input
                .get("covered_substrate_nodes")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            // Resolve purpose_text + purpose_id via the slug's active
            // purpose. Audit pass note: purpose_id is carried through
            // the synthesizer chain as an echo-passthrough field so the
            // create_meta_layer_node writer can pin provenance to the
            // purpose that drove THIS synthesis (and not whatever is
            // active by the time the writer runs). This read happens
            // once here at the top of the synthesizer chain; every
            // downstream step (LLM echo + writer) keys off the value
            // captured right here.
            let (purpose_text, purpose_id, max_depth) = {
                let conn_guard = ctx.db_reader.lock().await;
                let p = super::purpose::load_or_create_purpose(&conn_guard, &ctx.slug)?;
                // Find the max depth across covered substrate nodes while
                // we're under the lock — create_meta_layer_node uses it to
                // pin the new node's depth at parent_depth+1.
                let mut max_depth: i64 = 0;
                for id in &covered_substrate_nodes {
                    let node = super::db::get_node(&conn_guard, &ctx.slug, id)?;
                    if let Some(n) = node {
                        if n.depth > max_depth {
                            max_depth = n.depth;
                        }
                    }
                }
                (p.purpose_text, p.id, max_depth)
            };

            // Load each covered node's (distilled, topics) projection.
            // We keep this read tight — no corrections / decisions —
            // because the LLM prompt only needs distilled + topic labels.
            let mut nodes_out: Vec<Value> = Vec::new();
            {
                let conn_guard = ctx.db_reader.lock().await;
                for id in &covered_substrate_nodes {
                    if let Some(node) = super::db::get_node(&conn_guard, &ctx.slug, id)? {
                        let topics_json = serde_json::to_value(&node.topics).unwrap_or(Value::Null);
                        nodes_out.push(serde_json::json!({
                            "id": node.id,
                            "distilled": node.distilled,
                            "topics": topics_json,
                        }));
                    } else {
                        warn!(
                            "[mechanical] load_substrate_nodes slug={} id='{}' not found — \
                             omitting from synthesis context (id will still appear in \
                             covered_substrate_nodes so the LLM sees the ask).",
                            ctx.slug, id
                        );
                    }
                }
                drop(conn_guard);
            }

            if covered_substrate_nodes.is_empty() {
                // Phase 7b verifier (Audit Target 7): an empty covered
                // list means the caller asked to synthesize over nothing.
                // The oracle's purpose_shifted arm now pre-filters the
                // empty-L0 case, so reaching this branch is an upstream
                // contract bug — either decide_crystallization skipped
                // the empty-substrate guard or a future direct-call path
                // sent an unset/empty list. Raise rather than LLM-call
                // with no context.
                return Err(anyhow!(
                    "load_substrate_nodes: covered_substrate_nodes is empty for slug '{}'. \
                     A synthesizer run needs at least one substrate node; an empty envelope \
                     means the oracle or caller is routing a crystallization with no grounding \
                     input. Fix the upstream decide_crystallization rule or the caller's \
                     envelope.",
                    ctx.slug,
                ));
            }
            if nodes_out.is_empty() {
                return Err(anyhow!(
                    "load_substrate_nodes: none of the {} covered substrate node ids resolved \
                     against slug '{}' — the synthesizer has no substrate to synthesize on. \
                     This is almost certainly an upstream bug in decide_crystallization or \
                     the caller's envelope, not an expected empty state.",
                    covered_substrate_nodes.len(),
                    ctx.slug,
                ));
            }

            info!(
                "[mechanical] load_substrate_nodes slug={} covered={} resolved={} max_depth={}",
                ctx.slug,
                covered_substrate_nodes.len(),
                nodes_out.len(),
                max_depth,
            );

            let mut out = if let Value::Object(obj) = input {
                obj.clone()
            } else {
                serde_json::Map::new()
            };
            out.insert(
                "purpose_question".to_string(),
                Value::String(purpose_question),
            );
            out.insert("purpose_text".to_string(), Value::String(purpose_text));
            // Audit pass: also emit purpose_id so the LLM step's echo
            // contract can pass it through to create_meta_layer_node.
            // Without this value in the envelope, the LLM has nothing to
            // echo and the writer's loud-raise on missing purpose_id
            // would fire on every real run.
            out.insert("purpose_id".to_string(), Value::from(purpose_id));
            out.insert(
                "parent_meta_layer_id".to_string(),
                parent_meta_layer_id,
            );
            out.insert("nodes".to_string(), Value::Array(nodes_out));
            out.insert(
                "covered_substrate_nodes".to_string(),
                Value::Array(
                    covered_substrate_nodes
                        .into_iter()
                        .map(Value::String)
                        .collect(),
                ),
            );
            out.insert("_max_substrate_depth".to_string(), Value::from(max_depth));
            Ok(Value::Object(out))
        }
        "create_meta_layer_node" => {
            // Writer. Given the LLM step's output (headline, distilled,
            // topics, covered_substrate_node_ids), construct a new
            // MetaLayer node in pyramid_nodes and emit
            // meta_layer_crystallized.
            //
            // Threading contract: the synthesize_meta_layer step's output
            // is threaded in as `input`, BUT we also need context the LLM
            // step didn't echo (purpose_question, parent_meta_layer_id,
            // _max_substrate_depth). The starter runner re-merges only
            // target_node_id + slug per-step, so we pull the missing
            // context through the step_outputs accumulator via the
            // threaded input's own fields when the caller preserved them,
            // and we additionally tolerate them being provided inline in
            // the input envelope by a future direct-call path.
            //
            // Two-column invariant: node_shape is stored as the canonical
            // string (NODE_SHAPE_META_LAYER) + shape_payload_json carries
            // a JSON-serialized MetaLayerTopic. pyramid_nodes uses a
            // parallel (topics, corrections, ...) column set too; we
            // leave those NULL/empty for meta-layer nodes — readers
            // consult shape_payload_json via get_node_shape() for
            // MetaLayer content.
            let headline = input
                .get("headline")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!(
                    "create_meta_layer_node: missing `headline` string in input — \
                     synthesize_meta_layer LLM step did not produce the required field."
                ))?
                .to_string();
            let distilled = input
                .get("distilled")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!(
                    "create_meta_layer_node: missing `distilled` string in input"
                ))?
                .to_string();

            // covered_substrate_node_ids comes from the LLM step (the
            // audit trail: which substrate nodes actually shaped the
            // synthesis). If missing, fall back to covered_substrate_nodes
            // from the earlier load step so the payload never records an
            // empty substrate list against a non-trivial meta-layer.
            let covered_substrate_nodes: Vec<String> = input
                .get("covered_substrate_node_ids")
                .and_then(|v| v.as_array())
                .or_else(|| input.get("covered_substrate_nodes").and_then(|v| v.as_array()))
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            // topics carries the audit trail — each topic names a theme
            // and lists anchor substrate node ids. The LLM step's
            // response_schema declares `topics` required, but Phase 6a
            // verifier flagged that response_schema is NOT enforced by
            // the current LLM dispatch path. Without a check here a
            // non-strict provider return (`{headline, distilled}` with
            // topics omitted) would write a MetaLayer node with a
            // silently-empty topics array — the prompt's core grounding
            // contract (every topic anchors back to substrate ids) would
            // be lost at the persistence boundary with no signal to the
            // operator.
            //
            // feedback_loud_deferrals: raise loudly on missing /
            // malformed-shaped topics. An empty topics array is also a
            // failure (prompt explicitly calls it a failure mode); a
            // meta-layer with zero topics is structurally meaningless.
            let topics: Vec<MetaLayerTopicEntry> = match input.get("topics") {
                Some(Value::Array(arr)) => arr
                    .iter()
                    .map(|entry| {
                        serde_json::from_value::<MetaLayerTopicEntry>(entry.clone())
                            .map_err(|e| anyhow!(
                                "create_meta_layer_node: topics entry {:?} did not deserialize \
                                 into MetaLayerTopicEntry: {}",
                                entry, e,
                            ))
                    })
                    .collect::<Result<Vec<_>>>()?,
                Some(other) => {
                    return Err(anyhow!(
                        "create_meta_layer_node: `topics` must be a JSON array per the \
                         synthesize_meta_layer response_schema, got {:?}. This is the \
                         audit trail between the synthesis and its substrate; failing \
                         loudly rather than persist a meta-layer with floating claims.",
                        other,
                    ));
                }
                None => {
                    return Err(anyhow!(
                        "create_meta_layer_node: `topics` is missing from the synthesizer \
                         LLM step output. The starter-synthesizer response_schema declares \
                         it required, but the current LLM dispatch path does NOT enforce \
                         response_schema yet (Phase 6a verifier flag). Refusing to persist \
                         a meta-layer without the topic→anchor audit trail; fix the LLM \
                         provider path or the synthesize_meta_layer prompt so topics is \
                         always returned."
                    ));
                }
            };
            if topics.is_empty() {
                return Err(anyhow!(
                    "create_meta_layer_node: topics array is empty. The synthesize_meta_layer \
                     prompt explicitly documents this as a failure mode (\"an empty anchor_nodes \
                     list is a failure mode — the topic is floating\"); a meta-layer with zero \
                     topics has no audit trail back to substrate. Re-prompt or surface the \
                     synthesizer's refusal rather than write a hollow node."
                ));
            }
            for t in &topics {
                if t.anchor_nodes.is_empty() {
                    return Err(anyhow!(
                        "create_meta_layer_node: topic '{}' has no anchor_nodes. The \
                         synthesize_meta_layer prompt requires each topic to anchor at least \
                         one substrate node id; a floating topic breaks drill-down.",
                        t.topic,
                    ));
                }
            }

            // Pull purpose_question + purpose_id + parent_meta_layer_id.
            //
            // Audit pass (race fix): these fields MUST arrive via the
            // threaded input envelope. The prior self-resolve fallthrough
            // (reading `purpose::load_or_create_purpose` here when the
            // input was empty) raced concurrent `supersede_purpose`:
            // between the oracle's decide_crystallization (which captured
            // the purpose driving synthesis) and the writer (which runs
            // after the LLM synthesize step), a second supersede could
            // shift the slug's active purpose. The writer would then
            // label the new MetaLayer with the NEW purpose text while
            // its distilled/topics content was synthesized FROM the OLD
            // purpose — semantically wrong provenance.
            //
            // Fix: load_substrate_nodes reads purpose ONCE at the top of
            // the synthesizer chain and emits purpose_question +
            // purpose_id into its output envelope. The LLM synthesize
            // step's response_schema declares both as required echo
            // passthrough fields (see starter-synthesizer.yaml +
            // synthesize_meta_layer.md). The writer then consumes the
            // echoed values directly — NO self-resolve. If either field
            // is missing, raise loudly per feedback_loud_deferrals: it
            // indicates the LLM dropped the echo or the chain was
            // invoked via a direct-call path that bypassed
            // load_substrate_nodes, and silently falling back to the
            // active purpose reintroduces the race.
            let purpose_question = input
                .get("purpose_question")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow!(
                    "create_meta_layer_node: missing / empty `purpose_question` in input. \
                     The synthesizer's synthesize_meta_layer LLM step is required to echo \
                     the purpose_question it was given verbatim so the writer pins \
                     provenance to the purpose that drove synthesis — not whatever is \
                     active by the time the writer runs (which could have been superseded \
                     mid-chain). If you are calling create_meta_layer_node directly, pass \
                     purpose_question in the input envelope. Do NOT self-resolve from the \
                     slug's active purpose — that reintroduces the race this guard closes."
                ))?
                .to_string();
            let parent_meta_layer_id = input
                .get("parent_meta_layer_id")
                .and_then(|v| v.as_str())
                .map(String::from);
            let purpose_id = input
                .get("purpose_id")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow!(
                    "create_meta_layer_node: missing `purpose_id` (integer) in input. \
                     Same echo-passthrough contract as `purpose_question`; see the \
                     field's error above for full rationale."
                ))?;

            // Compute the new node's depth. Meta layers sit above their
            // substrate, so depth = max(covered_substrate_depths) + 1.
            // Prefer the _max_substrate_depth hint from load_substrate_nodes
            // (single query, correct); fall back to a fresh read when the
            // hint is absent (direct-call path).
            let parent_depth: i64 = match input.get("_max_substrate_depth").and_then(|v| v.as_i64())
            {
                Some(d) => d,
                None => {
                    let conn_guard = ctx.db_reader.lock().await;
                    let mut max_depth: i64 = 0;
                    for id in &covered_substrate_nodes {
                        if let Some(n) = super::db::get_node(&conn_guard, &ctx.slug, id)? {
                            if n.depth > max_depth {
                                max_depth = n.depth;
                            }
                        }
                    }
                    drop(conn_guard);
                    max_depth
                }
            };
            let node_depth = parent_depth + 1;

            // New node id. Format: L{depth}-ML-{short-uuid}. The short-
            // uuid tail keeps the id human-readable in chronicle traces
            // while guaranteeing uniqueness across concurrent oracle runs
            // on the same slug.
            let short_uuid: String = uuid::Uuid::new_v4()
                .to_string()
                .chars()
                .take(8)
                .collect();
            let node_id = format!("L{}-ML-{}", node_depth, short_uuid);

            let payload = MetaLayerTopic {
                purpose_question: purpose_question.clone(),
                parent_meta_layer_id: parent_meta_layer_id.clone(),
                covered_substrate_nodes: covered_substrate_nodes.clone(),
                topics: topics.clone(),
            };
            let payload_json = serde_json::to_string(&payload)?;

            info!(
                "[mechanical] create_meta_layer_node slug={} id={} depth={} covered={} parent_ml={:?}",
                ctx.slug,
                node_id,
                node_depth,
                covered_substrate_nodes.len(),
                parent_meta_layer_id,
            );

            let conn_guard = ctx.db_writer.lock().await;

            // Scaffolding-default column set matches what test seeds use.
            // `topics` / `corrections` / `decisions` / `terms` / `dead_ends`
            // are NULL-safe: MetaLayer content lives in shape_payload_json.
            conn_guard.execute(
                "INSERT INTO pyramid_nodes
                    (id, slug, depth, headline, distilled, self_prompt,
                     build_version, node_shape, shape_payload_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, '', 1, ?6, ?7)",
                rusqlite::params![
                    node_id,
                    ctx.slug,
                    node_depth,
                    headline,
                    distilled,
                    NODE_SHAPE_META_LAYER,
                    payload_json,
                ],
            )
            .map_err(|e| anyhow!(
                "create_meta_layer_node: failed to insert pyramid_nodes row \
                 (slug={} id={} depth={}): {}",
                ctx.slug, node_id, node_depth, e,
            ))?;

            // ── Phase 9b-5: resolve covered Gap-shaped substrate nodes ──
            //
            // When the synthesizer covers a Gap node with a meta-layer,
            // the gap's demand is satisfied by the crystallization.
            // Two mutations, both under the same writer lock we already
            // hold:
            //   1. Flip the GapTopic.demand_state to "closed" (so the
            //      node's shape payload reflects the resolution).
            //   2. Call mark_gap_resolved_with_reason (closes
            //      pyramid_gaps row + emits gap_resolved observation
            //      event — the compiler routes that event via
            //      role_for_event → meta_layer_oracle, closing the
            //      feedback loop: synthesis covers gap → gap resolved
            //      → oracle may crystallize further meta-layer).
            //
            // Multiple Gap-shaped covered nodes: each resolves
            // independently, each gets its own `gap_resolved` event.
            // Per Phase 8-4 contract: resolution_reason =
            // "covered_by_meta_layer", resolved_by = "starter-synthesizer".
            //
            // Per-gap failure is non-fatal to the overall meta-layer
            // create. Log loud and continue; the crystallized event
            // still fires (operators see the pyramid state was
            // updated) — re-running the chain can re-attempt any
            // missed gaps.
            let mut resolved_gaps: Vec<String> = Vec::new();
            for covered_id in &covered_substrate_nodes {
                let shape_view = match super::db::get_node_shape(&conn_guard, &ctx.slug, covered_id) {
                    Ok(Some(v)) => v,
                    Ok(None) => continue,   // covered node missing (shouldn't happen; skip)
                    Err(e) => {
                        tracing::warn!(
                            slug = %ctx.slug,
                            node = %covered_id,
                            error = %e,
                            "create_meta_layer_node: get_node_shape failed for covered node — skipping gap-resolution attempt"
                        );
                        continue;
                    }
                };
                if shape_view.shape.as_str() != NODE_SHAPE_GAP {
                    continue;
                }
                // Unpack GapTopic for the demand_state transition.
                let Some(ShapePayload::Gap(mut gap)) = shape_view.payload else {
                    tracing::warn!(
                        slug = %ctx.slug,
                        node = %covered_id,
                        "create_meta_layer_node: node shape='gap' but payload missing — skipping"
                    );
                    continue;
                };
                if gap.demand_state == "closed" {
                    // Already resolved; still cheap to re-emit the
                    // gap_resolved event so the oracle sees the
                    // covering in THIS crystallization (idempotent).
                } else {
                    gap.demand_state = "closed".to_string();
                    let new_payload = match serde_json::to_string(&gap) {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!(
                                slug = %ctx.slug,
                                node = %covered_id,
                                error = %e,
                                "create_meta_layer_node: failed to re-serialize GapTopic — skipping"
                            );
                            continue;
                        }
                    };
                    if let Err(e) = conn_guard.execute(
                        "UPDATE pyramid_nodes
                            SET shape_payload_json = ?1
                          WHERE slug = ?2 AND id = ?3",
                        rusqlite::params![new_payload, ctx.slug, covered_id],
                    ) {
                        tracing::warn!(
                            slug = %ctx.slug,
                            node = %covered_id,
                            error = %e,
                            "create_meta_layer_node: GapTopic demand_state UPDATE failed — skipping"
                        );
                        continue;
                    }
                }
                // Emit gap_resolved + best-effort update the legacy
                // pyramid_gaps row. mark_gap_resolved_with_reason
                // emits the observation event unconditionally; the
                // UPDATE-by-key is best-effort (post-6c Gap-shaped
                // nodes may not have a pyramid_gaps row at all — the
                // shape payload is the authoritative source, not the
                // legacy table). Node_id becomes the synthetic
                // question_id so chronicle consumers can drill back.
                //
                // gap.concern is used for the UPDATE's question_id
                // match so any legacy row seeded from the same
                // annotation cohort is closed in the same call.
                // Resolution reason + resolved_by are stable tokens
                // per Phase 8-4 contract.
                if let Err(e) = super::db::mark_gap_resolved_with_reason(
                    &conn_guard,
                    &ctx.slug,
                    covered_id,
                    &gap.description,
                    "covered_by_meta_layer",
                    "starter-synthesizer",
                ) {
                    tracing::warn!(
                        slug = %ctx.slug,
                        node = %covered_id,
                        error = %e,
                        "create_meta_layer_node: mark_gap_resolved_with_reason failed"
                    );
                } else {
                    resolved_gaps.push(covered_id.clone());
                }
            }

            // Emit meta_layer_crystallized observation event. Metadata
            // carries the fields the downstream compiler's role_for_event
            // arm will key off (+ purpose_id for chronicle drill-down).
            let meta = serde_json::json!({
                "meta_layer_node_id": node_id,
                "covered_substrate_node_ids": covered_substrate_nodes,
                "purpose_question": purpose_question,
                "purpose_id": purpose_id,
                "parent_meta_layer_id": parent_meta_layer_id,
                "depth": node_depth,
                "resolved_gap_node_ids": resolved_gaps,
            })
            .to_string();
            let crystallized_event_id = super::observation_events::write_observation_event(
                &conn_guard,
                &ctx.slug,
                "chain",
                "meta_layer_crystallized",
                None,
                None,
                None,
                None,
                Some(&node_id),
                Some(node_depth),
                Some(&meta),
            )?;
            drop(conn_guard);

            Ok(serde_json::json!({
                "created": true,
                "meta_layer_node_id": node_id,
                "depth": node_depth,
                "covered_substrate_node_ids": covered_substrate_nodes,
                "crystallized_event_id": crystallized_event_id,
                "resolved_gap_node_ids": resolved_gaps,
            }))
        }
        "oracle_finalize" => {
            // Terminal step for `starter-meta-layer-oracle`. Replaces the
            // Phase 5 generic `log_and_complete` so a SKIP decision lands
            // loudly in the chronicle.
            //
            // Why this exists (verifier Audit Target 1 + feedback_loud_deferrals):
            //   decide_crystallization may legitimately return
            //   should_crystallize=false (unknown trigger, gap with no
            //   candidate_resolutions, chain invoked outside the
            //   observation-event path). When that happens the
            //   dispatch_synthesizer step is when-gated off, the chain
            //   fast-forwards to this terminal step, and the work item
            //   CASes to `applied`. Before this arm existed the only
            //   signal was a single info!() line — operator-invisible in
            //   the chronicle (which is where operators LOOK for
            //   oracle activity). A skipped oracle run must leave a
            //   visible mark.
            //
            // Behavior:
            //   - If the threaded input carries should_crystallize=true
            //     AND a meta_layer_node_id (i.e. create_meta_layer_node
            //     already ran and emitted meta_layer_crystallized), we
            //     don't double-emit; just pass through.
            //   - Otherwise, emit a `meta_layer_oracle_skipped` event
            //     carrying {reasoning, trigger_event_type, target_node_id,
            //     source_event_id, purpose_id} so a chronicle reader can
            //     reconstruct WHY the oracle declined.
            let should_crystallize = input
                .get("should_crystallize")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let created = input
                .get("created")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
                || input.get("meta_layer_node_id").is_some();

            if should_crystallize && created {
                // Happy path already emitted meta_layer_crystallized via
                // create_meta_layer_node. Nothing to add; threading forwards
                // the writer's output verbatim.
                info!(
                    "[mechanical] oracle_finalize slug={} crystallize=true created=true (no-op pass-through)",
                    ctx.slug,
                );
                return Ok(input.clone());
            }

            let reasoning = input
                .get("reasoning")
                .and_then(|v| v.as_str())
                .unwrap_or("no reasoning carried through threading — likely an upstream author bug in the oracle chain")
                .to_string();
            let trigger_event_type = input
                .get("trigger_event_type")
                .and_then(|v| v.as_str())
                .map(String::from);
            let target_node_id = input
                .get("target_node_id")
                .or_else(|| input.get("target_id"))
                .and_then(|v| v.as_str())
                .map(String::from);
            let source_event_id = input
                .get("source_event_id")
                .and_then(|v| v.as_i64());
            let purpose_id = input
                .get("purpose_id")
                .and_then(|v| v.as_i64());

            let mut meta = serde_json::Map::new();
            meta.insert("reasoning".to_string(), Value::String(reasoning.clone()));
            if let Some(ref t) = trigger_event_type {
                meta.insert(
                    "trigger_event_type".to_string(),
                    Value::String(t.clone()),
                );
            }
            if let Some(ref t) = target_node_id {
                meta.insert("target_node_id".to_string(), Value::String(t.clone()));
            }
            if let Some(eid) = source_event_id {
                meta.insert("source_event_id".to_string(), Value::from(eid));
            }
            if let Some(pid) = purpose_id {
                meta.insert("purpose_id".to_string(), Value::from(pid));
            }
            let metadata_json = serde_json::to_string(&Value::Object(meta))?;

            info!(
                "[mechanical] oracle_finalize slug={} crystallize=false reasoning=\"{}\"",
                ctx.slug, reasoning,
            );
            let conn_guard = ctx.db_writer.lock().await;
            let event_id = super::observation_events::write_observation_event(
                &conn_guard,
                &ctx.slug,
                "chain",
                "meta_layer_oracle_skipped",
                None,
                None,
                None,
                None,
                target_node_id.as_deref(),
                None,
                Some(&metadata_json),
            )?;
            drop(conn_guard);

            let mut out = if let Value::Object(obj) = input {
                obj.clone()
            } else {
                serde_json::Map::new()
            };
            out.insert(
                "oracle_finalized".to_string(),
                Value::from(true),
            );
            out.insert(
                "oracle_skipped_event_id".to_string(),
                Value::from(event_id),
            );
            Ok(Value::Object(out))
        }
        // ── Post-build accretion v5 Phase 7c: gap_dispatcher primitives ─────
        //
        // Back starter-gap-dispatcher.yaml, the chain dispatched when a `gap`
        // annotation fires annotation_reacted (vocab handler_chain_id points
        // here). Mirrors the 7a debate_steward shape:
        //   emit_dispatcher_invoked → load_gap_context → materialize_gap_node
        //   → log_and_complete.
        //
        // Idempotency: `materialize_gap_node` keys off annotation id via an
        // `annotation#{id}` evidence-anchor tag on each GapCandidate (same
        // dedup pattern Phase 7a uses on RedTeamEntry.evidence_anchors). A
        // re-compile of the same annotation will find the existing anchor
        // and fall to no_op without re-emitting `gap_detected`.
        //
        // v5 audit P3: role_for_event(gap_detected) returns None — the
        // chain's gap_detected emission is observability-only. The actual
        // dispatch has already fired via annotation_reacted →
        // handler_chain_id (6c-B flip), so there is nothing more to do
        // than write the chronicle event. `gap_dispatcher_skipped` may
        // still fire in the rare case of a direct chain invocation that
        // carries no annotation_id; when that happens it is the loud
        // deferral, not the norm.
        // See project_auto_stale_system.md for the broader map.
        "emit_dispatcher_invoked" => {
            // Chronicle-only observability event — one row in
            // dadbear_observation_events naming the target + annotation.
            // Same threading discipline as emit_debate_steward_invoked: the
            // input envelope's work_item_id / annotation_id / annotation_type
            // fields are passed through so later steps can back-fill.
            let target_node_id = input
                .get("target_node_id")
                .or_else(|| input.get("target_id"))
                .and_then(|v| v.as_str());
            let annotation_id = input.get("annotation_id").and_then(|v| v.as_i64());
            let annotation_type = input
                .get("annotation_type")
                .and_then(|v| v.as_str());
            let mut meta = serde_json::Map::new();
            if let Some(tid) = target_node_id {
                meta.insert(
                    "target_node_id".to_string(),
                    Value::String(tid.to_string()),
                );
            }
            if let Some(aid) = annotation_id {
                meta.insert("annotation_id".to_string(), Value::from(aid));
            }
            if let Some(at) = annotation_type {
                meta.insert(
                    "annotation_type".to_string(),
                    Value::String(at.to_string()),
                );
            }
            let metadata_json = if meta.is_empty() {
                None
            } else {
                Some(serde_json::to_string(&Value::Object(meta))?)
            };
            info!(
                "[mechanical] emit_dispatcher_invoked slug={} target={:?} annotation_id={:?} annotation_type={:?}",
                ctx.slug, target_node_id, annotation_id, annotation_type
            );
            let conn_guard = ctx.db_writer.lock().await;
            let event_id = super::observation_events::write_observation_event(
                &conn_guard,
                &ctx.slug,
                "chain",
                "gap_dispatcher_invoked",
                None,
                None,
                None,
                None,
                target_node_id,
                None,
                metadata_json.as_deref(),
            )?;
            drop(conn_guard);
            let mut out = if let Value::Object(obj) = input {
                obj.clone()
            } else {
                serde_json::Map::new()
            };
            out.insert("emitted".to_string(), Value::from(true));
            out.insert("event_id".to_string(), Value::from(event_id));
            Ok(Value::Object(out))
        }
        "load_gap_context" => {
            // Resolves the triggering annotation + target shape + existing
            // GapTopic payload (if present). Similar pattern to
            // load_annotation_and_target (7a) but also reads the Gap
            // payload so the writer can dedup-append in place.
            //
            // annotation_id / annotation_type may arrive on the input
            // envelope directly, OR be back-filled via the work_item's
            // observation_event_ids column (matching the debate_steward
            // backfill path).
            let target_node_id = input
                .get("target_node_id")
                .or_else(|| input.get("target_id"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!(
                    "load_gap_context: missing target_node_id"
                ))?
                .to_string();

            let mut annotation_id = input.get("annotation_id").and_then(|v| v.as_i64());
            let mut annotation_type = input
                .get("annotation_type")
                .and_then(|v| v.as_str())
                .map(String::from);

            if annotation_id.is_none() || annotation_type.is_none() {
                let work_item_id = input
                    .get("work_item_id")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                if let Some(wid) = work_item_id.as_deref() {
                    let conn_guard = ctx.db_reader.lock().await;
                    let obs_ids_json: Option<String> = conn_guard
                        .query_row(
                            "SELECT observation_event_ids FROM dadbear_work_items WHERE id = ?1",
                            rusqlite::params![wid],
                            |row| row.get(0),
                        )
                        .ok();
                    if let Some(ids_json) = obs_ids_json {
                        if let Ok(ids) = serde_json::from_str::<Vec<i64>>(&ids_json) {
                            if let Some(eid) = ids.first() {
                                let meta: Option<String> = conn_guard
                                    .query_row(
                                        "SELECT metadata_json FROM dadbear_observation_events WHERE id = ?1",
                                        rusqlite::params![eid],
                                        |row| row.get(0),
                                    )
                                    .ok()
                                    .flatten();
                                if let Some(m) = meta {
                                    if let Ok(v) = serde_json::from_str::<Value>(&m) {
                                        if annotation_id.is_none() {
                                            annotation_id =
                                                v.get("annotation_id").and_then(|x| x.as_i64());
                                        }
                                        if annotation_type.is_none() {
                                            annotation_type = v
                                                .get("annotation_type")
                                                .and_then(|x| x.as_str())
                                                .map(String::from);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    drop(conn_guard);
                }
            }

            let conn_guard = ctx.db_reader.lock().await;

            let annotation_obj: Value = if let Some(aid) = annotation_id {
                let row: Option<(i64, String, String, String, Option<String>, String, String, String)> = conn_guard
                    .query_row(
                        "SELECT id, slug, node_id, annotation_type, question_context, author,
                                content, created_at
                         FROM pyramid_annotations WHERE id = ?1",
                        rusqlite::params![aid],
                        |r| Ok((
                            r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?,
                            r.get(5)?, r.get(6)?, r.get(7)?,
                        )),
                    )
                    .ok();
                if let Some((id, slug, node_id, aty, qctx, author, content, created_at)) = row {
                    if annotation_type.as_deref() != Some(aty.as_str()) {
                        annotation_type = Some(aty.clone());
                    }
                    serde_json::json!({
                        "id": id,
                        "slug": slug,
                        "node_id": node_id,
                        "annotation_type": aty,
                        "question_context": qctx,
                        "author": author,
                        "content": content,
                        "created_at": created_at,
                    })
                } else {
                    Value::Null
                }
            } else {
                Value::Null
            };

            let node_row: Option<(i64, String, String)> = conn_guard
                .query_row(
                    "SELECT depth, headline, distilled FROM pyramid_nodes
                     WHERE slug = ?1 AND id = ?2",
                    rusqlite::params![ctx.slug, target_node_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .ok();
            let shape_view = super::db::get_node_shape(&conn_guard, &ctx.slug, &target_node_id)?;
            drop(conn_guard);

            let (current_shape, existing_gap_payload_json, target_obj) =
                if let Some((depth, headline, distilled)) = node_row {
                    let current_shape = shape_view
                        .as_ref()
                        .map(|v| v.shape.as_str().to_string())
                        .unwrap_or_else(|| "scaffolding".to_string());
                    let existing_gap_payload = match shape_view.as_ref().and_then(|v| v.payload.as_ref()) {
                        Some(ShapePayload::Gap(g)) => serde_json::to_value(g).ok(),
                        _ => None,
                    };
                    let current_payload = shape_view
                        .as_ref()
                        .and_then(|v| v.payload.as_ref())
                        .and_then(|p| serde_json::to_value(p).ok())
                        .unwrap_or(Value::Null);
                    let target = serde_json::json!({
                        "id": target_node_id,
                        "depth": depth,
                        "headline": headline,
                        "distilled": distilled,
                        "current_shape": current_shape,
                        "current_payload": current_payload,
                    });
                    (current_shape, existing_gap_payload, target)
                } else {
                    ("scaffolding".to_string(), None, Value::Null)
                };

            info!(
                "[mechanical] load_gap_context slug={} target={} annotation_id={:?} annotation_type={:?} current_shape={}",
                ctx.slug, target_node_id, annotation_id, annotation_type, current_shape,
            );

            let mut out = if let Value::Object(obj) = input {
                obj.clone()
            } else {
                serde_json::Map::new()
            };
            out.insert(
                "target_node_id".to_string(),
                Value::String(target_node_id.clone()),
            );
            out.insert(
                "annotation_id".to_string(),
                annotation_id.map(Value::from).unwrap_or(Value::Null),
            );
            out.insert(
                "annotation_type".to_string(),
                annotation_type
                    .map(Value::String)
                    .unwrap_or(Value::Null),
            );
            out.insert("annotation".to_string(), annotation_obj);
            out.insert("target_node".to_string(), target_obj);
            out.insert("target_shape".to_string(), Value::String(current_shape));
            out.insert(
                "existing_gap_payload".to_string(),
                existing_gap_payload_json.unwrap_or(Value::Null),
            );
            Ok(Value::Object(out))
        }
        "materialize_gap_node" => {
            // Core Gap writer. Given a `gap` annotation + target node:
            //   - Scaffolding → upgrade to Gap shape with a fresh GapTopic
            //     seeded from the annotation, emit `gap_detected`.
            //   - Gap → merge the annotation into the existing payload
            //     (append a GapCandidate carrying an `annotation#{id}`
            //     evidence tag for dedup). Do NOT re-emit `gap_detected`.
            //   - Debate / MetaLayer → skip loud: these typed nodes have
            //     semantic meaning the Gap writer can't safely overwrite.
            //     Phase 8+ decides whether to create a sibling Gap node;
            //     for now we return a no_op action with a reason so the
            //     operator sees it in the chronicle.
            //   - Unknown shape → raise (feedback_loud_deferrals).
            let target_node_id = input
                .get("target_node_id")
                .or_else(|| input.get("target_id"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!(
                    "materialize_gap_node: missing target_node_id"
                ))?
                .to_string();
            let annotation_id: Option<i64> = input.get("annotation_id").and_then(|v| v.as_i64());
            let annotation_obj = input.get("annotation").cloned().unwrap_or(Value::Null);

            if annotation_id.is_none() {
                // feedback_loud_deferrals: the "no annotation_id" path fires
                // when something invokes the gap_dispatcher chain directly
                // (outside the annotation_reacted → handler_chain_id path).
                // Post-v5 audit, this is no longer the gap_detected retrigger
                // — `role_for_event("gap_detected")` returns None — so if
                // this arm fires in production it is a real anomaly, not a
                // cheap expected no_op. Emit `gap_dispatcher_skipped` loudly.
                info!(
                    "[mechanical] materialize_gap_node slug={} target={} → no_op (no annotation_id in input — direct chain invocation without annotation context)",
                    ctx.slug, target_node_id
                );
                let skip_meta = serde_json::json!({
                    "target_node_id": target_node_id,
                    "reason": "no_annotation_id",
                    "detail": "work item carried no annotation_id. After v5 audit P3, role_for_event(gap_detected) returns None so this is not a retrigger cycle — investigate why the chain was dispatched without annotation context.",
                })
                .to_string();
                let conn_guard = ctx.db_writer.lock().await;
                let _ = super::observation_events::write_observation_event(
                    &conn_guard,
                    &ctx.slug,
                    "chain",
                    "gap_dispatcher_skipped",
                    None,
                    None,
                    None,
                    None,
                    Some(&target_node_id),
                    None,
                    Some(&skip_meta),
                );
                drop(conn_guard);
                return Ok(serde_json::json!({
                    "action": "no_op",
                    "reason": "no annotation_id in input",
                }));
            }
            let annotation_id = annotation_id.unwrap();

            // Pull annotation content + author, preferring threaded
            // `annotation` object. Loud-raise on missing row per
            // feedback_loud_deferrals (the verifier fix applied to Phase
            // 7a's append_annotation_to_debate_node — same discipline here).
            let ann_content: String;
            let ann_author: String;
            let ann_question_context: Option<String>;
            {
                let threaded_content = annotation_obj
                    .get("content")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let threaded_author = annotation_obj
                    .get("author")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let threaded_qctx = annotation_obj
                    .get("question_context")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                if let (Some(c), Some(a)) = (threaded_content, threaded_author) {
                    ann_content = c;
                    ann_author = a;
                    ann_question_context = threaded_qctx;
                } else {
                    let conn_guard = ctx.db_reader.lock().await;
                    let row: Option<(String, String, Option<String>)> = conn_guard
                        .query_row(
                            "SELECT content, author, question_context FROM pyramid_annotations WHERE id = ?1",
                            rusqlite::params![annotation_id],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                        )
                        .ok();
                    drop(conn_guard);
                    match row {
                        Some((c, a, q)) => {
                            ann_content = c;
                            ann_author = a;
                            ann_question_context = q;
                        }
                        None => {
                            return Err(anyhow!(
                                "materialize_gap_node: annotation_id={} not \
                                 found in pyramid_annotations — stale event or deleted row. \
                                 Target '{}' will not be mutated.",
                                annotation_id,
                                target_node_id,
                            ));
                        }
                    }
                }
            };

            // Concern line: prefer question_context (tends to be a
            // question), fall back to target node's headline, fall back
            // to a synthesized string. Description is the annotation body.
            let concern_line = ann_question_context
                .filter(|q| !q.is_empty())
                .or_else(|| {
                    input
                        .get("target_node")
                        .and_then(|t| t.get("headline"))
                        .and_then(|v| v.as_str())
                        .map(String::from)
                        .filter(|s| !s.is_empty())
                })
                .unwrap_or_else(|| format!("Gap at node {}", target_node_id));
            let description = ann_content.clone();

            // Serialize the transition under the writer mutex so
            // concurrent `gap` annotations on the same target serialize.
            let conn_guard = ctx.db_writer.lock().await;
            let shape_view =
                super::db::get_node_shape(&conn_guard, &ctx.slug, &target_node_id)?;
            let current_shape = shape_view
                .as_ref()
                .map(|v| v.shape.clone())
                .unwrap_or_else(NodeShape::scaffolding);

            // Phase 7c verifier: the annotation anchor lived on
            // `GapTopic.evidence_anchors` to avoid overloading
            // `GapCandidate.resolution_type`. v5 audit P6 completes the
            // split: annotation tokens move to `source_annotation_ids`,
            // `evidence_anchors` is reserved for genuine node-id refs.
            // Per `feedback_generalize_not_enumerate`: the right field
            // for provenance is a dedicated provenance field.
            let annotation_token = format!("annotation#{annotation_id}");

            let (action, updated_gap, shape_was_upgraded) = if current_shape.is_scaffolding() {
                // Fresh Gap. Seed empty candidate_resolutions (LLM's
                // channel) + empty evidence_anchors (no node-id refs
                // yet) + record the annotation token under
                // source_annotation_ids for idempotent replay.
                let gap = GapTopic {
                    concern: concern_line,
                    description: description.clone(),
                    demand_state: "open".to_string(),
                    candidate_resolutions: Vec::new(),
                    evidence_anchors: Vec::new(),
                    source_annotation_ids: vec![annotation_token.clone()],
                };
                ("created_gap", gap, true)
            } else if current_shape.as_str() == NODE_SHAPE_GAP {
                // Merge into existing GapTopic. Idempotent on the anchor.
                let mut gap = match shape_view.unwrap().payload {
                    Some(ShapePayload::Gap(g)) => g,
                    other => {
                        return Err(anyhow!(
                            "materialize_gap_node: target '{}' is shape 'gap' but \
                             payload does not deserialize as GapTopic (got {:?})",
                            target_node_id,
                            other.is_some()
                        ));
                    }
                };
                // v5 audit P6: dedup against BOTH the new
                // source_annotation_ids channel AND the legacy
                // evidence_anchors slot (for rollover compat: pre-audit
                // rows carry the token on evidence_anchors).
                let already = gap
                    .source_annotation_ids
                    .iter()
                    .chain(gap.evidence_anchors.iter())
                    .any(|a| a == &annotation_token);
                if !already {
                    // Append the token to the provenance channel. Don't
                    // mutate concern / description of an existing gap
                    // (the original concern is load-bearing for operators)
                    // and don't touch candidate_resolutions (LLM channel).
                    gap.source_annotation_ids.push(annotation_token.clone());
                }
                let action_label = if already { "no_op" } else { "appended_anchor" };
                (action_label, gap, false)
            } else if current_shape.as_str() == NODE_SHAPE_DEBATE
                || current_shape.as_str() == NODE_SHAPE_META_LAYER
            {
                // Don't destroy typed nodes — no_op loud. Phase 8 decides
                // whether to create a sibling Gap node (new id). Phase 7c
                // verifier: emit a chronicle `gap_dispatcher_skipped` event
                // alongside the tracing::warn so operators see the skip in
                // the same surface as every other chain action
                // (feedback_loud_deferrals). Mirrors the Phase 7b
                // `meta_layer_oracle_skipped` pattern.
                warn!(
                    "[mechanical] materialize_gap_node slug={} target={} shape={} → skip (Phase 8 will decide sibling-Gap policy)",
                    ctx.slug, target_node_id, current_shape,
                );
                let skip_meta = serde_json::json!({
                    "target_node_id": target_node_id,
                    "annotation_id": annotation_id,
                    "existing_shape": current_shape.as_str(),
                    "reason": "shape_incompatible",
                    "detail": format!(
                        "target '{}' is shape '{}' — gap_dispatcher will not overwrite \
                         a typed node. Phase 8 may create a sibling Gap node.",
                        target_node_id, current_shape,
                    ),
                })
                .to_string();
                let _ = super::observation_events::write_observation_event(
                    &conn_guard,
                    &ctx.slug,
                    "chain",
                    "gap_dispatcher_skipped",
                    None,
                    None,
                    None,
                    None,
                    Some(&target_node_id),
                    None,
                    Some(&skip_meta),
                );
                drop(conn_guard);
                return Ok(serde_json::json!({
                    "action": "skipped_shape_incompatible",
                    "reason": format!(
                        "target '{}' is shape '{}' — gap_dispatcher will not overwrite \
                         a typed node. Phase 8 may create a sibling Gap node.",
                        target_node_id, current_shape,
                    ),
                    "target_node_id": target_node_id,
                    "annotation_id": annotation_id,
                }));
            } else {
                // Unknown shape (scaffolding / debate / meta_layer / gap are
                // the genesis shapes; any other value is a vocab extension
                // the writer doesn't know how to handle yet).
                drop(conn_guard);
                return Err(anyhow!(
                    "materialize_gap_node: target '{}' has unknown shape '{}' — \
                     only scaffolding / gap / debate / meta_layer are handled \
                     (debate + meta_layer skip-loud; scaffolding + gap mutate). \
                     Publish a gap_dispatcher extension before introducing new \
                     node shapes.",
                    target_node_id,
                    current_shape
                ));
            };

            // Write back (scaffolding → gap sets node_shape = 'gap';
            // existing gap just updates shape_payload_json).
            let payload_json = serde_json::to_string(&updated_gap)?;
            conn_guard.execute(
                "UPDATE pyramid_nodes
                 SET node_shape = ?1, shape_payload_json = ?2
                 WHERE slug = ?3 AND id = ?4",
                rusqlite::params![
                    NODE_SHAPE_GAP,
                    payload_json,
                    ctx.slug,
                    target_node_id,
                ],
            )?;

            // Emit gap_detected only on the shape upgrade (scaffolding→gap).
            // Append-to-existing-gap does NOT re-emit (the chronicle already
            // carries the original gap_detected).
            //
            // Phase 7c verifier: metadata carries `concern` in addition to
            // the bookkeeping fields. Downstream consumers (FE gap surface,
            // Phase 8 LLM candidate generation) need a human-readable
            // summary line without a second DB read.
            let mut gap_detected_event_id: Option<i64> = None;
            if shape_was_upgraded {
                let meta = serde_json::json!({
                    "target_node_id": target_node_id,
                    "annotation_id": annotation_id,
                    "author": ann_author,
                    "demand_state": updated_gap.demand_state,
                    "concern": updated_gap.concern,
                })
                .to_string();
                let eid = super::observation_events::write_observation_event(
                    &conn_guard,
                    &ctx.slug,
                    "chain",
                    "gap_detected",
                    None,
                    None,
                    None,
                    None,
                    Some(&target_node_id),
                    None,
                    Some(&meta),
                )?;
                gap_detected_event_id = Some(eid);
            }
            drop(conn_guard);

            info!(
                "[mechanical] materialize_gap_node slug={} target={} action={} gap_detected_event_id={:?}",
                ctx.slug, target_node_id, action, gap_detected_event_id
            );

            let updated_payload_value =
                serde_json::to_value(&updated_gap).unwrap_or(Value::Null);
            Ok(serde_json::json!({
                "action": action,
                "target_node_id": target_node_id,
                "annotation_id": annotation_id,
                "shape_was_upgraded": shape_was_upgraded,
                "gap_detected_event_id": gap_detected_event_id,
                "updated_payload": updated_payload_value,
            }))
        }
        // ── Post-build accretion v5 Phase 6b: sub-chain invocation ──────────
        //
        // Lets a parent chain call a library chain by id. Phase 7's
        // debate_steward / meta_layer_oracle / synthesizer chains drive the
        // post-build accretion judgement flow by wiring the two library
        // chains `starter-evidence-tester` and `starter-reconciler` into
        // their step graphs through `call_starter_chain` steps.
        //
        // Expected input envelope:
        //
        //     {
        //       "chain_id": "starter-evidence-tester",
        //       "input": { /* sub-chain's input object */ }
        //     }
        //
        // Optional fields carried inside `input.input`:
        //     - `_sub_chain_depth: usize` — recursion counter; absent → 0.
        //
        // Raises loudly (see `feedback_loud_deferrals`) when:
        //     - `chain_id` is missing / non-string.
        //     - `ctx.state` is None (the dispatch path wasn't created by the
        //       starter runner — library chains are only callable from there).
        //     - `MAX_SUB_CHAIN_DEPTH` would be reached or exceeded by the
        //       nested call (cycle detected).
        //     - `chain_loader::load_chain_by_id` returns an error (unknown
        //       chain id OR ambiguity).
        //     - The nested `execute_chain_for_target` call errors — the error
        //       is propagated with a context prefix naming the chain.
        "call_starter_chain" => {
            let sub_chain_id = input
                .get("chain_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!(
                    "call_starter_chain: input missing required string field \
                     `chain_id` (e.g. \"starter-evidence-tester\")"
                ))?
                .to_string();

            // Pull the sub-chain's input envelope. `input.input` is what
            // will be threaded to the library chain's first step; default
            // to `{}` when unset (some library chains tolerate empty input).
            //
            // Phase 6b verifier fix: strict-typed envelope. The depth/target
            // stamping below (`Value::Object(ref mut map) = sub_input`) is a
            // no-op when `input` is present-but-non-object, which would silently
            // drop the cycle-guard envelope and orphan any carried target_id.
            // Raise loudly per feedback_loud_deferrals: the caller's chain YAML
            // has a type bug that should surface at first-run rather than as a
            // mysterious downstream failure.
            let mut sub_input = match input.get("input") {
                Some(v) if v.is_object() => v.clone(),
                None => Value::Object(serde_json::Map::new()),
                Some(other) => {
                    return Err(anyhow!(
                        "call_starter_chain: `input` field for sub-chain '{}' \
                         must be a JSON object, got `{}`. Library chains receive \
                         their step input through this envelope; non-object values \
                         would silently drop the cycle-depth + target_id stamping \
                         downstream.",
                        sub_chain_id,
                        match other {
                            Value::String(_) => "string",
                            Value::Number(_) => "number",
                            Value::Bool(_) => "boolean",
                            Value::Null => "null",
                            Value::Array(_) => "array",
                            Value::Object(_) => unreachable!(),
                        },
                    ));
                }
            };

            // Depth guard: read `_sub_chain_depth` from the nested input if
            // the caller already set one, otherwise from the dispatch ctx.
            // The higher wins so a chain cannot reset the counter.
            let ctx_depth = ctx.sub_chain_depth.unwrap_or(0);
            let input_depth = sub_input
                .get("_sub_chain_depth")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize)
                .unwrap_or(0);
            let current_depth = std::cmp::max(ctx_depth, input_depth);

            if current_depth >= MAX_SUB_CHAIN_DEPTH {
                return Err(anyhow!(
                    "call_starter_chain: sub-chain recursion depth {} reached \
                     MAX_SUB_CHAIN_DEPTH={} while invoking '{}' — probable chain cycle. \
                     Document the intended parent/child chain graph; break the cycle \
                     at the chain-authorship level.",
                    current_depth, MAX_SUB_CHAIN_DEPTH, sub_chain_id,
                ));
            }

            // ctx.state is threaded in only by the starter runner. The IR
            // executor, legacy full executor, and dead-letter retry all set
            // ctx.state = None — library-chain invocation is only available
            // from the starter runner. feedback_loud_deferrals: raise
            // instead of silently dropping.
            let sub_state = ctx.state.as_ref().ok_or_else(|| anyhow!(
                "call_starter_chain: no PyramidState wired into the dispatch \
                 context — library-chain invocation is only available under \
                 `chain_executor::execute_chain_for_target`. Cannot invoke \
                 sub-chain '{}' from this dispatch path.",
                sub_chain_id,
            ))?;
            let chains_dir = ctx.chains_dir.as_ref().cloned().unwrap_or_else(|| {
                sub_state.chains_dir.clone()
            });

            // Load the sub-chain via chain_loader::load_chain_by_id so the
            // same discovery semantics (defaults/ + defaults/starter/ +
            // variants/ + ambiguity detection) apply at sub-chain entry.
            let sub_chain = super::chain_loader::load_chain_by_id(
                &sub_chain_id,
                chains_dir.as_path(),
            )
            .map_err(|e| anyhow!(
                "call_starter_chain: failed to load sub-chain '{}': {}",
                sub_chain_id, e,
            ))?;

            // Stamp the incremented depth on the sub-input so nested
            // `call_starter_chain` steps see a monotonic counter even when
            // the dispatch ctx doesn't get rebuilt in between.
            let new_depth = current_depth + 1;
            if let Value::Object(ref mut map) = sub_input {
                map.insert(
                    "_sub_chain_depth".to_string(),
                    Value::from(new_depth as u64),
                );
                // Carry forward the parent's target_node_id / target_id
                // context if the parent had one and the sub-input doesn't
                // already override it.
                if let Some(ref tid) = ctx.target_id {
                    map.entry("target_node_id".to_string())
                        .or_insert_with(|| Value::String(tid.clone()));
                }
            }

            info!(
                "[mechanical] call_starter_chain slug={} sub_chain_id={} depth={}→{}",
                ctx.slug, sub_chain_id, current_depth, new_depth,
            );

            // Recurse into the starter runner. The nested call builds a
            // fresh ChainDispatchContext internally from `sub_state`, so the
            // Arc is shared but no state mutation escapes the sub-chain.
            //
            // Box::pin is required by E0733: `dispatch_step → dispatch_mechanical
            // → call_starter_chain → execute_chain_for_target → dispatch_step`
            // is a recursive async-fn cycle, and Rust needs the returned future
            // to be heap-allocated so the compiler can resolve its size. The
            // depth guard above bounds the recursion so boxing cost is O(1)
            // per level (max MAX_SUB_CHAIN_DEPTH boxes on the stack).
            let sub_state_clone = sub_state.clone();
            let slug_clone = ctx.slug.clone();
            let target_id_clone = ctx.target_id.clone();
            let sub_chain_id_for_err = sub_chain_id.clone();
            let sub_output = Box::pin(async move {
                super::chain_executor::execute_chain_for_target(
                    &sub_state_clone,
                    &sub_chain,
                    &slug_clone,
                    target_id_clone.as_deref(),
                    sub_input,
                )
                .await
            })
            .await
            .map_err(|e| anyhow!(
                "call_starter_chain: sub-chain '{}' failed: {}",
                sub_chain_id_for_err, e,
            ))?;

            // The parent chain sees the sub-chain's final output verbatim —
            // threading behavior is the same as any other mechanical step.
            Ok(sub_output)
        }
        // ── Post-build accretion v5 Phase 7d: utility chain primitives ──────
        //
        // The primitives below back the four Phase 7d starter chains —
        // `starter-judge`, `starter-authorize-question`,
        // `starter-accretion-handler`, and `starter-sweep`. Each follows
        // the same threading contract as Phase 7a/7b/7c: read what you
        // need from `input`, preserve the input fields you didn't consume
        // in the output, loud-raise on missing required fields per
        // `feedback_loud_deferrals`.
        "emit_judge_invoked" => {
            // Chronicle-only trace: records that the generalist `judge`
            // library chain was invoked with a particular claim. The
            // claim is threaded through to the LLM step unchanged.
            let claim = input.get("claim").and_then(|v| v.as_str());
            let criteria = input.get("criteria").and_then(|v| v.as_str());
            let mut meta = serde_json::Map::new();
            if let Some(c) = claim {
                // Cap the claim preview in metadata so unusually-long
                // claims don't bloat the chronicle row.
                let preview: String = c.chars().take(500).collect();
                meta.insert("claim_preview".to_string(), Value::String(preview));
            }
            if let Some(c) = criteria {
                let preview: String = c.chars().take(300).collect();
                meta.insert(
                    "criteria_preview".to_string(),
                    Value::String(preview),
                );
            }
            let metadata_json = if meta.is_empty() {
                None
            } else {
                Some(serde_json::to_string(&Value::Object(meta))?)
            };
            info!(
                "[mechanical] emit_judge_invoked slug={} claim_present={} criteria_present={}",
                ctx.slug,
                claim.is_some(),
                criteria.is_some(),
            );
            let conn_guard = ctx.db_writer.lock().await;
            let event_id = super::observation_events::write_observation_event(
                &conn_guard,
                &ctx.slug,
                "chain",
                "judge_invoked",
                None,
                None,
                None,
                None,
                None, // judge is slug-level (or caller-level), not node-bound
                None,
                metadata_json.as_deref(),
            )?;
            drop(conn_guard);
            // Thread input through — the next LLM step needs claim/context/criteria.
            let mut out = if let Value::Object(obj) = input {
                obj.clone()
            } else {
                serde_json::Map::new()
            };
            out.insert("emitted".to_string(), Value::from(true));
            out.insert("event_id".to_string(), Value::from(event_id));
            Ok(Value::Object(out))
        }
        "emit_authorize_invoked" => {
            // Chronicle-only trace: records that authorize_question was
            // invoked for a slug + question pair. Threads input through
            // so load_slug_purpose can read `slug` and the LLM step can
            // read `question`.
            let question = input.get("question").and_then(|v| v.as_str());
            let slug_in = input.get("slug").and_then(|v| v.as_str());
            let mut meta = serde_json::Map::new();
            if let Some(q) = question {
                let preview: String = q.chars().take(500).collect();
                meta.insert(
                    "question_preview".to_string(),
                    Value::String(preview),
                );
            }
            if let Some(s) = slug_in {
                meta.insert("slug".to_string(), Value::String(s.to_string()));
            }
            let metadata_json = if meta.is_empty() {
                None
            } else {
                Some(serde_json::to_string(&Value::Object(meta))?)
            };
            info!(
                "[mechanical] emit_authorize_invoked slug={} question_present={}",
                ctx.slug,
                question.is_some(),
            );
            let conn_guard = ctx.db_writer.lock().await;
            let event_id = super::observation_events::write_observation_event(
                &conn_guard,
                &ctx.slug,
                "chain",
                "authorize_question_invoked",
                None,
                None,
                None,
                None,
                None,
                None,
                metadata_json.as_deref(),
            )?;
            drop(conn_guard);
            let mut out = if let Value::Object(obj) = input {
                obj.clone()
            } else {
                serde_json::Map::new()
            };
            out.insert("emitted".to_string(), Value::from(true));
            out.insert("event_id".to_string(), Value::from(event_id));
            Ok(Value::Object(out))
        }
        "load_slug_purpose" => {
            // Reads the active purpose row for `slug` via
            // `purpose::load_or_create_purpose` (which seeds a stock
            // purpose from ContentType if the slug has none). Returns
            // the envelope with `purpose_text` + `stock_purpose_key`
            // merged in so the LLM step sees the authoritative gating
            // text.
            //
            // Input `slug` takes precedence over ctx.slug — the caller
            // may be asking about a slug different from the one the
            // chain was invoked against (library-chain pattern). Fall
            // back to ctx.slug if not set.
            let slug_for_purpose = input
                .get("slug")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| ctx.slug.clone());

            let (purpose_text, stock_purpose_key) = {
                let conn_guard = ctx.db_reader.lock().await;
                match super::purpose::load_or_create_purpose(
                    &conn_guard,
                    &slug_for_purpose,
                ) {
                    Ok(p) => (p.purpose_text, p.stock_purpose_key),
                    Err(e) => {
                        drop(conn_guard);
                        // Loud raise: if the slug doesn't exist, this is
                        // a caller bug. Per feedback_loud_deferrals we
                        // don't paper over with an empty purpose.
                        return Err(anyhow!(
                            "load_slug_purpose: failed to load purpose for slug '{}': {}",
                            slug_for_purpose,
                            e,
                        ));
                    }
                }
            };

            info!(
                "[mechanical] load_slug_purpose slug={} purpose_len={} stock_key={:?}",
                slug_for_purpose,
                purpose_text.len(),
                stock_purpose_key,
            );

            let mut out = if let Value::Object(obj) = input {
                obj.clone()
            } else {
                serde_json::Map::new()
            };
            out.insert("slug".to_string(), Value::String(slug_for_purpose));
            out.insert(
                "purpose_text".to_string(),
                Value::String(purpose_text),
            );
            out.insert(
                "stock_purpose_key".to_string(),
                stock_purpose_key
                    .map(Value::String)
                    .unwrap_or(Value::Null),
            );
            Ok(Value::Object(out))
        }
        "emit_accretion_invoked" => {
            // Chronicle-only trace for the accretion_handler role.
            let node_id = input.get("node_id").and_then(|v| v.as_str());
            let window_n = input.get("window_n").and_then(|v| v.as_i64());
            let mut meta = serde_json::Map::new();
            if let Some(n) = node_id {
                meta.insert("node_id".to_string(), Value::String(n.to_string()));
            }
            if let Some(w) = window_n {
                meta.insert("window_n".to_string(), Value::from(w));
            }
            let metadata_json = if meta.is_empty() {
                None
            } else {
                Some(serde_json::to_string(&Value::Object(meta))?)
            };
            info!(
                "[mechanical] emit_accretion_invoked slug={} node_id={:?} window_n={:?}",
                ctx.slug, node_id, window_n,
            );
            let conn_guard = ctx.db_writer.lock().await;
            let event_id = super::observation_events::write_observation_event(
                &conn_guard,
                &ctx.slug,
                "chain",
                "accretion_invoked",
                None,
                None,
                None,
                None,
                node_id,
                None,
                metadata_json.as_deref(),
            )?;
            drop(conn_guard);
            let mut out = if let Value::Object(obj) = input {
                obj.clone()
            } else {
                serde_json::Map::new()
            };
            out.insert("emitted".to_string(), Value::from(true));
            out.insert("event_id".to_string(), Value::from(event_id));
            Ok(Value::Object(out))
        }
        "load_recent_annotations_for_slug" => {
            // Reads up to `window_n` most recent annotations for the
            // slug (optionally scoped to a single node_id) plus the
            // slug's active purpose_text. Returns the envelope the LLM
            // step needs to synthesize an accretion note.
            //
            // `window_n` is the hard cap on how many rows the LLM sees
            // — it protects the prompt from runaway token spend on a
            // slug with thousands of annotations. It shapes LLM input,
            // so per `feedback_pillar37_no_hedging` the number MUST
            // live in the caller's envelope (YAML step.input / chain
            // caller) rather than be baked into Rust. We loud-raise
            // when absent instead of defaulting silently.
            //
            // Phase 7d verifier fix (Pillar 37): was `unwrap_or(20)`
            // which smuggled a token-budget knob into Rust. The 7d
            // trigger wiring (Phase 9) is expected to pass `window_n`
            // at its call site; when operators supersede the
            // accretion_handler binding to a specialized chain, that
            // chain's YAML encodes its own window.
            let window_n = input
                .get("window_n")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow!(
                    "load_recent_annotations_for_slug: required field \
                     `window_n` is missing from the input envelope. \
                     Pass it from the caller (e.g. \
                     `call_starter_chain` input: {{ window_n: N }}) or \
                     from the chain-level initial input — baking a \
                     default into Rust would violate Pillar 37 \
                     (feedback_pillar37_no_hedging), since window_n \
                     shapes the LLM synthesis prompt's token budget."
                ))?
                .max(1);
            let node_id_filter = input
                .get("node_id")
                .and_then(|v| v.as_str())
                .map(String::from);
            let slug_for_load = input
                .get("slug")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| ctx.slug.clone());

            // Load purpose + annotations under a single reader lock.
            // v5 audit P10: accretion_cursor is now a DB column on
            // pyramid_slugs. Read it here and filter annotations to
            // `id > cursor` so repeated invocations don't reprocess the
            // same rows. A fresh slug has cursor=0 (default) which
            // matches every annotation.
            let (purpose_text, annotations, total_count, accretion_cursor) = {
                let conn_guard = ctx.db_reader.lock().await;
                let p = super::purpose::load_or_create_purpose(
                    &conn_guard,
                    &slug_for_load,
                )
                .map_err(|e| anyhow!(
                    "load_recent_annotations_for_slug: failed to load purpose for slug '{}': {}",
                    slug_for_load, e,
                ))?;

                // Read the slug's current accretion cursor (default 0).
                let cursor: i64 = conn_guard
                    .query_row(
                        "SELECT COALESCE(accretion_cursor, 0) FROM pyramid_slugs
                          WHERE slug = ?1",
                        rusqlite::params![slug_for_load],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);

                // Count total annotations strictly beyond the cursor.
                let total: i64 = if let Some(nid) = node_id_filter.as_deref() {
                    conn_guard
                        .query_row(
                            "SELECT COUNT(*) FROM pyramid_annotations
                              WHERE slug = ?1 AND node_id = ?2 AND id > ?3",
                            rusqlite::params![slug_for_load, nid, cursor],
                            |r| r.get(0),
                        )
                        .unwrap_or(0)
                } else {
                    conn_guard
                        .query_row(
                            "SELECT COUNT(*) FROM pyramid_annotations
                              WHERE slug = ?1 AND id > ?2",
                            rusqlite::params![slug_for_load, cursor],
                            |r| r.get(0),
                        )
                        .unwrap_or(0)
                };

                // Load the window_n most recent annotations beyond cursor.
                let mut anns: Vec<Value> = Vec::new();
                if let Some(nid) = node_id_filter.as_deref() {
                    let mut stmt = conn_guard.prepare(
                        "SELECT id, node_id, annotation_type, content, author, created_at
                           FROM pyramid_annotations
                          WHERE slug = ?1 AND node_id = ?2 AND id > ?3
                          ORDER BY id DESC LIMIT ?4",
                    )?;
                    let rows = stmt.query_map(
                        rusqlite::params![slug_for_load, nid, cursor, window_n],
                        |row| {
                            Ok(serde_json::json!({
                                "id": row.get::<_, i64>(0)?,
                                "node_id": row.get::<_, String>(1)?,
                                "annotation_type": row.get::<_, String>(2)?,
                                "content": row.get::<_, String>(3)?,
                                "author": row.get::<_, String>(4)?,
                                "created_at": row.get::<_, String>(5)?,
                            }))
                        },
                    )?;
                    for r in rows {
                        anns.push(r?);
                    }
                } else {
                    let mut stmt = conn_guard.prepare(
                        "SELECT id, node_id, annotation_type, content, author, created_at
                           FROM pyramid_annotations
                          WHERE slug = ?1 AND id > ?2
                          ORDER BY id DESC LIMIT ?3",
                    )?;
                    let rows = stmt.query_map(
                        rusqlite::params![slug_for_load, cursor, window_n],
                        |row| {
                            Ok(serde_json::json!({
                                "id": row.get::<_, i64>(0)?,
                                "node_id": row.get::<_, String>(1)?,
                                "annotation_type": row.get::<_, String>(2)?,
                                "content": row.get::<_, String>(3)?,
                                "author": row.get::<_, String>(4)?,
                                "created_at": row.get::<_, String>(5)?,
                            }))
                        },
                    )?;
                    for r in rows {
                        anns.push(r?);
                    }
                }
                (p.purpose_text, anns, total, cursor)
            };

            // Phase 7d verifier fix: compute the maximum annotation id
            // in the loaded window so `emit_accretion_written` can
            // stamp it as an `accretion_cursor` marker in the event
            // metadata. No DB-backed cursor exists today (Phase 9
            // decides cursor schema + atomic update semantics — see
            // the chain YAML's trigger-investigation block), so until
            // then repeated invocations reprocess the same annotations
            // and emit duplicate accretion notes. Stamping the cursor
            // value on the event gives operators (and future Phase 9
            // wiring) the data needed to reconstruct a cursor from the
            // chronicle without adding a schema migration in 7d.
            // feedback_loud_deferrals: the limitation is now visible
            // in the event stream, not hidden behind a stub.
            let max_annotation_id: Option<i64> = annotations
                .iter()
                .filter_map(|a| a.get("id").and_then(|v| v.as_i64()))
                .max();

            info!(
                "[mechanical] load_recent_annotations_for_slug slug={} loaded={} total={} max_id={:?} node_id_filter={:?}",
                slug_for_load,
                annotations.len(),
                total_count,
                max_annotation_id,
                node_id_filter,
            );

            let mut out = if let Value::Object(obj) = input {
                obj.clone()
            } else {
                serde_json::Map::new()
            };
            out.insert(
                "slug".to_string(),
                Value::String(slug_for_load),
            );
            out.insert(
                "purpose_text".to_string(),
                Value::String(purpose_text),
            );
            out.insert(
                "annotations".to_string(),
                Value::Array(annotations),
            );
            out.insert(
                "annotation_count".to_string(),
                Value::from(total_count),
            );
            out.insert(
                "max_annotation_id".to_string(),
                max_annotation_id
                    .map(Value::from)
                    .unwrap_or(Value::Null),
            );
            // v5 audit P10: thread the cursor value BEFORE this run so
            // emit_accretion_written can confirm it was advanced.
            out.insert(
                "accretion_cursor_before".to_string(),
                Value::from(accretion_cursor),
            );
            Ok(Value::Object(out))
        }
        "emit_accretion_written" => {
            // Chronicle trace recording the accretion note the LLM
            // step produced. The note body + references land in
            // metadata so downstream FAQ / meta-layer consumers can
            // find and cite it without a second DB read.
            //
            // Reads the LLM step's threaded output — specifically the
            // `note` (string) and `references` (array of ints) fields
            // the synthesize_accretion_note step returns. Missing
            // `note` is loud-raised: an accretion_written event with
            // no note is a dropped result, not a deferral target.
            let note = input
                .get("note")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!(
                    "emit_accretion_written: missing `note` in input — the \
                     synthesize_accretion_note LLM step must precede this \
                     primitive and return a `note` field. Threading is \
                     preserve-by-default in the chain executor; if the LLM \
                     step succeeded but `note` is missing, the schema \
                     enforcement failed upstream."
                ))?
                .to_string();
            let references = input
                .get("references")
                .cloned()
                .unwrap_or_else(|| Value::Array(vec![]));
            let slug_for_write = input
                .get("slug")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| ctx.slug.clone());
            let node_id = input.get("node_id").and_then(|v| v.as_str());

            // v5 audit P10: accretion_cursor is a DB column on
            // pyramid_slugs. Carry the maximum annotation id from the
            // preceding load step as the new cursor value. The UPDATE +
            // chronicle insert run in the SAME transaction so replay of
            // the chronicle (without the UPDATE) can't duplicate-process.
            // The chronicle metadata continues to stamp the cursor for
            // audit trail / operator observability, but is no longer the
            // authoritative source.
            //
            // Null max_annotation_id occurs when load_recent_annotations
            // returned zero rows — in that case we should not advance
            // the cursor (nothing was processed).
            let new_cursor: Option<i64> = input
                .get("max_annotation_id")
                .and_then(|v| v.as_i64());
            let accretion_cursor_value = new_cursor
                .map(Value::from)
                .unwrap_or(Value::Null);

            let meta = serde_json::json!({
                "note": note,
                "references": references,
                "accretion_cursor": accretion_cursor_value,
            });
            let metadata_json = serde_json::to_string(&meta)?;

            info!(
                "[mechanical] emit_accretion_written slug={} note_len={} references={} new_cursor={:?}",
                slug_for_write,
                note.len(),
                meta["references"]
                    .as_array()
                    .map(|a| a.len())
                    .unwrap_or(0),
                new_cursor,
            );
            let mut conn_guard = ctx.db_writer.lock().await;
            let tx = conn_guard.transaction()?;
            let event_id = super::observation_events::write_observation_event(
                &tx,
                &slug_for_write,
                "chain",
                "accretion_written",
                None,
                None,
                None,
                None,
                node_id,
                None,
                Some(&metadata_json),
            )?;
            // Advance the cursor atomically with the chronicle write.
            // Monotonic-only: never rewind (defensive against out-of-order
            // callers). NULL max_annotation_id → no advance.
            if let Some(c) = new_cursor {
                tx.execute(
                    "UPDATE pyramid_slugs
                        SET accretion_cursor = ?1
                      WHERE slug = ?2
                        AND COALESCE(accretion_cursor, 0) < ?1",
                    rusqlite::params![c, slug_for_write],
                )?;
            }
            tx.commit()?;
            drop(conn_guard);

            let mut out = if let Value::Object(obj) = input {
                obj.clone()
            } else {
                serde_json::Map::new()
            };
            out.insert("emitted".to_string(), Value::from(true));
            out.insert("event_id".to_string(), Value::from(event_id));
            Ok(Value::Object(out))
        }
        "emit_sweep_invoked" => {
            // Chronicle-only trace: a sweep has begun against this slug.
            let stale_days = input
                .get("stale_days")
                .and_then(|v| v.as_i64());
            let mut meta = serde_json::Map::new();
            if let Some(d) = stale_days {
                meta.insert("stale_days".to_string(), Value::from(d));
            }
            let metadata_json = if meta.is_empty() {
                None
            } else {
                Some(serde_json::to_string(&Value::Object(meta))?)
            };
            info!(
                "[mechanical] emit_sweep_invoked slug={} stale_days={:?}",
                ctx.slug, stale_days,
            );
            let conn_guard = ctx.db_writer.lock().await;
            let event_id = super::observation_events::write_observation_event(
                &conn_guard,
                &ctx.slug,
                "chain",
                "sweep_invoked",
                None,
                None,
                None,
                None,
                None,
                None,
                metadata_json.as_deref(),
            )?;
            drop(conn_guard);
            let mut out = if let Value::Object(obj) = input {
                obj.clone()
            } else {
                serde_json::Map::new()
            };
            out.insert("emitted".to_string(), Value::from(true));
            out.insert("event_id".to_string(), Value::from(event_id));
            Ok(Value::Object(out))
        }
        "count_stale_failed_work_items" => {
            // Measurement-only (Phase 7d MVP): count
            // `dadbear_work_items` in state='failed' whose
            // state_changed_at is older than stale_days days old.
            // Emits a `sweep_stale_failed_counted` chronicle event so
            // operators can key on the number.
            //
            // No mutation — Phase 9 decides archive vs. delete
            // semantics (deletion semantics interact with the
            // supervisor's retry-back-off window). Per
            // feedback_no_deferral_creep: this measurement IS the
            // MVP deliverable, not a stub — a number is real
            // operator signal.
            //
            // Phase 7d verifier fix (operator-policy separation, NOT
            // Pillar 37): stale_days is a policy knob — "what counts
            // as stale?" — that different operators will disagree on.
            //
            // Audit-pass clarification: Pillar 37's literal text is "a
            // number constraining LLM output"; stale_days is a SQL
            // threshold against state_changed_at, not an LLM input, so
            // Pillar 37 doesn't apply. The right architectural principle
            // here is the broader "dispatch code does not decide policy"
            // rule (same rule that keeps temperature / max_tokens out of
            // Rust, just for a different reason). Baking 30 days into the
            // primitive would hard-code retention policy onto every
            // operator that invokes the sweep chain.
            //
            // Loud-raise when absent: Phase 9 cron / schedule wiring
            // passes the operator-specified value; per-slug superseding
            // chains encode their own value in YAML (step.input or a
            // chain-level default block when that schema extension lands).
            // Today the sweep chain has no natural trigger, so the
            // loud-raise fires only if someone invokes the primitive
            // directly without stale_days — which is the author bug it's
            // meant to catch.
            let stale_days = input
                .get("stale_days")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow!(
                    "count_stale_failed_work_items: required field \
                     `stale_days` is missing from the input envelope. \
                     Pass it from the caller (e.g. chain-level \
                     initial input or a `call_starter_chain` input \
                     envelope with {{ stale_days: N }}). Operators \
                     define `stale' — the dispatch code does not."
                ))?
                .max(0);
            let slug_for_count = input
                .get("slug")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| ctx.slug.clone());

            // Threshold: "older than N days ago" → state_changed_at <
            // datetime('now', '-N days').
            let threshold_modifier = format!("-{} days", stale_days);
            let count: i64 = {
                let conn_guard = ctx.db_reader.lock().await;
                conn_guard
                    .query_row(
                        // Phase 9b-3: exclude archived rows so the count
                        // reflects live operator-actionable debt, not
                        // sweep-accumulated cold storage.
                        "SELECT COUNT(*) FROM dadbear_work_items
                          WHERE slug = ?1
                            AND state = 'failed'
                            AND archived_at IS NULL
                            AND state_changed_at < datetime('now', ?2)",
                        rusqlite::params![slug_for_count, threshold_modifier],
                        |r| r.get(0),
                    )
                    .unwrap_or(0)
            };

            info!(
                "[mechanical] count_stale_failed_work_items slug={} stale_days={} count={}",
                slug_for_count, stale_days, count,
            );

            let meta = serde_json::json!({
                "stale_days": stale_days,
                "count": count,
            });
            let metadata_json = serde_json::to_string(&meta)?;
            let conn_guard = ctx.db_writer.lock().await;
            super::observation_events::write_observation_event(
                &conn_guard,
                &slug_for_count,
                "chain",
                "sweep_stale_failed_counted",
                None,
                None,
                None,
                None,
                None,
                None,
                Some(&metadata_json),
            )?;
            drop(conn_guard);

            let mut out = if let Value::Object(obj) = input {
                obj.clone()
            } else {
                serde_json::Map::new()
            };
            out.insert("slug".to_string(), Value::String(slug_for_count));
            out.insert("stale_days".to_string(), Value::from(stale_days));
            out.insert(
                "stale_failed_count".to_string(),
                Value::from(count),
            );
            Ok(Value::Object(out))
        }
        "reindex_vocab_cache" => {
            // Invalidates the vocab_entries process-wide cache and
            // warms it by issuing a `list_vocabulary` for each of the
            // three known vocab_kinds. Emits a
            // `sweep_vocab_reindexed` chronicle event with per-kind
            // counts in metadata.
            super::vocab_entries::invalidate_cache();

            let (ann_count, shape_count, role_count) = {
                let conn_guard = ctx.db_reader.lock().await;
                let a = super::vocab_entries::list_vocabulary(
                    &conn_guard,
                    super::vocab_entries::VOCAB_KIND_ANNOTATION_TYPE,
                )
                .map(|v| v.len() as i64)
                .unwrap_or(0);
                let s = super::vocab_entries::list_vocabulary(
                    &conn_guard,
                    super::vocab_entries::VOCAB_KIND_NODE_SHAPE,
                )
                .map(|v| v.len() as i64)
                .unwrap_or(0);
                let r = super::vocab_entries::list_vocabulary(
                    &conn_guard,
                    super::vocab_entries::VOCAB_KIND_ROLE_NAME,
                )
                .map(|v| v.len() as i64)
                .unwrap_or(0);
                (a, s, r)
            };

            info!(
                "[mechanical] reindex_vocab_cache slug={} annotation_type={} node_shape={} role_name={}",
                ctx.slug, ann_count, shape_count, role_count,
            );

            let meta = serde_json::json!({
                "annotation_type": ann_count,
                "node_shape": shape_count,
                "role_name": role_count,
            });
            let metadata_json = serde_json::to_string(&meta)?;
            let conn_guard = ctx.db_writer.lock().await;
            super::observation_events::write_observation_event(
                &conn_guard,
                &ctx.slug,
                "chain",
                "sweep_vocab_reindexed",
                None,
                None,
                None,
                None,
                None,
                None,
                Some(&metadata_json),
            )?;
            drop(conn_guard);

            let mut out = if let Value::Object(obj) = input {
                obj.clone()
            } else {
                serde_json::Map::new()
            };
            let counts = serde_json::json!({
                "annotation_type": ann_count,
                "node_shape": shape_count,
                "role_name": role_count,
            });
            out.insert("vocab_counts".to_string(), counts);
            Ok(Value::Object(out))
        }
        "archive_stale_failed_work_items" => {
            // Phase 9b-3: graduate the sweep chain from measurement to
            // actual archival. Moves `dadbear_work_items` rows where
            // state='failed' AND state_changed_at < (now - stale_days -
            // retention_days) to `archived_at = now()`. The sweep
            // chain's prior `count_stale_failed_work_items` step ran on
            // stale_days; archival happens on the *longer* window so
            // operators have a recovery period between "stale" and
            // "archived" (count-only) during which they can manually
            // retry / debug.
            //
            // Input envelope:
            //   stale_days       — same as count step (sweep's "what's
            //                      stale?" threshold; read from envelope).
            //   retention_days   — extra days beyond stale before the
            //                      row is archived. Loud-raises if absent.
            //
            // Both values come from the sweep chain's caller (today:
            // pyramid_scheduler dispatch path — the SchedulerConfig's
            // sweep_stale_days + sweep_retention_days) so the policy
            // sits outside Rust.
            //
            // Emits `sweep_archived_failed_work_items` with the count.
            // Idempotent: already-archived rows are filtered by the
            // `archived_at IS NULL` predicate.
            let stale_days = input
                .get("stale_days")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow!(
                    "archive_stale_failed_work_items: required field \
                     `stale_days` is missing from the input envelope. \
                     The sweep chain caller owns this policy knob \
                     (scheduler_parameters.sweep_stale_days today)."
                ))?
                .max(0);
            let retention_days = input
                .get("retention_days")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow!(
                    "archive_stale_failed_work_items: required field \
                     `retention_days` is missing from the input \
                     envelope. Operators define the recovery window \
                     after `stale_days` before archival; the dispatch \
                     code does not hardcode it \
                     (scheduler_parameters.sweep_retention_days today)."
                ))?
                .max(0);
            let slug_for_archive = input
                .get("slug")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| ctx.slug.clone());
            let total_days = stale_days.saturating_add(retention_days);
            let threshold_modifier = format!("-{} days", total_days);

            let archived_count: i64 = {
                let conn_guard = ctx.db_writer.lock().await;
                let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
                let rows = conn_guard
                    .execute(
                        "UPDATE dadbear_work_items
                            SET archived_at = ?1
                          WHERE slug = ?2
                            AND state = 'failed'
                            AND archived_at IS NULL
                            AND state_changed_at < datetime('now', ?3)",
                        rusqlite::params![now, slug_for_archive, threshold_modifier],
                    )
                    .map_err(|e| anyhow!(
                        "archive_stale_failed_work_items: UPDATE failed for slug={}: {}",
                        slug_for_archive, e,
                    ))?;
                rows as i64
            };

            info!(
                "[mechanical] archive_stale_failed_work_items slug={} stale_days={} retention_days={} archived={}",
                slug_for_archive, stale_days, retention_days, archived_count,
            );

            let meta = serde_json::json!({
                "stale_days": stale_days,
                "retention_days": retention_days,
                "threshold_days": total_days,
                "archived_count": archived_count,
            });
            let metadata_json = serde_json::to_string(&meta)?;
            let conn_guard = ctx.db_writer.lock().await;
            super::observation_events::write_observation_event(
                &conn_guard,
                &slug_for_archive,
                "chain",
                "sweep_archived_failed_work_items",
                None, None, None, None, None, None,
                Some(&metadata_json),
            )?;
            drop(conn_guard);

            let mut out = if let Value::Object(obj) = input {
                obj.clone()
            } else {
                serde_json::Map::new()
            };
            out.insert("slug".to_string(), Value::String(slug_for_archive));
            out.insert("archived_count".to_string(), Value::from(archived_count));
            out.insert("stale_days".to_string(), Value::from(stale_days));
            out.insert("retention_days".to_string(), Value::from(retention_days));
            Ok(Value::Object(out))
        }
        "retire_superseded_contributions_past_retention" => {
            // Phase 9b-3: retire long-dead superseded contribution rows.
            // When `status = 'superseded'` AND accepted_at < (now -
            // retention_days), flip status to `retired`. Retirement is
            // soft (no row deletion) — the supersession chain stays
            // intact for audit, but a `retired` row is excluded from
            // active-version queries (which already filter
            // `status = 'active'`), so the semantic is preserved.
            //
            // Retention window lives on the input envelope
            // (`contribution_retention_days`, operator-editable via the
            // scheduler config). Absent or <= 0 → loud-raise (the
            // primitive cannot invent a retention policy).
            //
            // Emits `sweep_retired_superseded_contributions` with the
            // count.
            let retention_days = input
                .get("contribution_retention_days")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow!(
                    "retire_superseded_contributions_past_retention: required \
                     field `contribution_retention_days` is missing from the \
                     input envelope. Operators define supersession retention; \
                     the dispatch code does not hardcode it."
                ))?;
            if retention_days <= 0 {
                return Err(anyhow!(
                    "retire_superseded_contributions_past_retention: \
                     contribution_retention_days must be > 0 (got {retention_days}). \
                     Zero/negative retention would retire every superseded row \
                     immediately; refusing per feedback_loud_deferrals."
                ));
            }
            let slug_for_retire = input
                .get("slug")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| ctx.slug.clone());
            let threshold_modifier = format!("-{} days", retention_days);

            let retired_count: i64 = {
                let conn_guard = ctx.db_writer.lock().await;
                // Scope the retire pass: either slug-scoped (non-NULL slug
                // match) OR the global pool (slug IS NULL). The sweep
                // chain caller may want both passes in turn; this
                // primitive handles one call per invocation and we run
                // it slug-scoped by default (sweep is a per-slug chain).
                let rows = conn_guard
                    .execute(
                        "UPDATE pyramid_config_contributions
                            SET status = 'retired'
                          WHERE status = 'superseded'
                            AND slug = ?1
                            AND COALESCE(accepted_at, created_at) < datetime('now', ?2)",
                        rusqlite::params![slug_for_retire, threshold_modifier],
                    )
                    .map_err(|e| anyhow!(
                        "retire_superseded_contributions_past_retention: UPDATE failed for slug={}: {}",
                        slug_for_retire, e,
                    ))?;
                rows as i64
            };

            info!(
                "[mechanical] retire_superseded_contributions_past_retention slug={} retention_days={} retired={}",
                slug_for_retire, retention_days, retired_count,
            );

            let meta = serde_json::json!({
                "retention_days": retention_days,
                "retired_count": retired_count,
            });
            let metadata_json = serde_json::to_string(&meta)?;
            let conn_guard = ctx.db_writer.lock().await;
            super::observation_events::write_observation_event(
                &conn_guard,
                &slug_for_retire,
                "chain",
                "sweep_retired_superseded_contributions",
                None, None, None, None, None, None,
                Some(&metadata_json),
            )?;
            drop(conn_guard);

            let mut out = if let Value::Object(obj) = input {
                obj.clone()
            } else {
                serde_json::Map::new()
            };
            out.insert("slug".to_string(), Value::String(slug_for_retire));
            out.insert("retention_days".to_string(), Value::from(retention_days));
            out.insert("retired_count".to_string(), Value::from(retired_count));
            Ok(Value::Object(out))
        }
        unknown => Err(anyhow!("Unknown mechanical function: {}", unknown)),
    }
}

/// Check whether a function name is a known mechanical function.
pub fn is_known_mechanical_function(name: &str) -> bool {
    MECHANICAL_FUNCTIONS.contains(&name)
}

// ── Node builder ────────────────────────────────────────────────────────────

/// Convert LLM step output into a PyramidNode.
///
/// Maps standard LLM output fields (distilled, corrections, decisions, terms,
/// topics, headline, dead_ends, self_prompt/orientation) into the PyramidNode
/// struct. Follows the same pattern as `node_from_analysis` in build.rs.
pub fn build_node_from_output(
    output: &Value,
    node_id: &str,
    slug: &str,
    depth: i64,
    chunk_index: Option<i64>,
) -> Result<PyramidNode> {
    // Extract distilled text (try multiple field names for compatibility)
    let distilled = output
        .get("orientation")
        .or_else(|| output.get("distilled"))
        .or_else(|| output.get("purpose"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Extract self_prompt (try "orientation" first, then "self_prompt")
    let self_prompt = output
        .get("orientation")
        .or_else(|| output.get("self_prompt"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Extract dead_ends
    let dead_ends: Vec<String> = output
        .get("dead_ends")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Extract topics (deserialize from JSON array)
    let topics: Vec<Topic> = output
        .get("topics")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| serde_json::from_value(t.clone()).ok())
                .collect()
        })
        .unwrap_or_default();

    // Extract corrections from top-level and from topics
    let mut corrections: Vec<Correction> = Vec::new();
    let mut decisions: Vec<Decision> = Vec::new();

    // Top-level corrections
    if let Some(corrs) = output.get("corrections").and_then(|c| c.as_array()) {
        for c in corrs {
            corrections.push(Correction {
                wrong: c.get("wrong").and_then(|v| v.as_str()).unwrap_or("").into(),
                right: c.get("right").and_then(|v| v.as_str()).unwrap_or("").into(),
                who: c.get("who").and_then(|v| v.as_str()).unwrap_or("").into(),
            });
        }
    }

    // Top-level decisions
    if let Some(decs) = output.get("decisions").and_then(|d| d.as_array()) {
        for d in decs {
            decisions.push(Decision {
                decided: d
                    .get("decided")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .into(),
                why: d.get("why").and_then(|v| v.as_str()).unwrap_or("").into(),
                rejected: d
                    .get("rejected")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .into(),
                ..Default::default()
            });
        }
    }

    // Also pull corrections/decisions from within topics
    if let Some(topics_arr) = output.get("topics").and_then(|t| t.as_array()) {
        for topic in topics_arr {
            if let Some(corrs) = topic.get("corrections").and_then(|c| c.as_array()) {
                for c in corrs {
                    corrections.push(Correction {
                        wrong: c.get("wrong").and_then(|v| v.as_str()).unwrap_or("").into(),
                        right: c.get("right").and_then(|v| v.as_str()).unwrap_or("").into(),
                        who: c.get("who").and_then(|v| v.as_str()).unwrap_or("").into(),
                    });
                }
            }
            if let Some(decs) = topic.get("decisions").and_then(|d| d.as_array()) {
                for d in decs {
                    decisions.push(Decision {
                        decided: d
                            .get("decided")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .into(),
                        why: d.get("why").and_then(|v| v.as_str()).unwrap_or("").into(),
                        rejected: d
                            .get("rejected")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .into(),
                        ..Default::default()
                    });
                }
            }
        }
    }

    // Extract terms
    let terms: Vec<Term> = output
        .get("terms")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| serde_json::from_value(t.clone()).ok())
                .collect()
        })
        .unwrap_or_default();

    // Headline via shared naming utility
    let headline = headline_from_analysis(output, node_id);

    // ── WS-SCHEMA-V2: extract episodic memory fields from LLM output ────
    // These fields are produced by the episodic chain's combine_l0.md and
    // synthesize_recursive.md prompts. Backward-compatible: all fields
    // default to empty/zero when absent from the LLM output (retro chain
    // and question pipeline don't produce them).

    // time_range: {start, end} ISO-8601 timestamps
    let time_range = output.get("time_range").and_then(|tr| {
        let start = tr.get("start").and_then(|s| s.as_str()).map(String::from);
        let end = tr.get("end").and_then(|s| s.as_str()).map(String::from);
        if start.is_some() || end.is_some() {
            Some(super::types::TimeRange { start, end })
        } else {
            None
        }
    });

    // weight: numeric or {tokens, turns, fraction_of_parent} object
    let weight = output
        .get("weight")
        .and_then(|w| {
            if let Some(n) = w.as_f64() {
                Some(n)
            } else if let Some(obj) = w.as_object() {
                // Sum tokens as the primary weight signal
                obj.get("tokens").and_then(|t| t.as_f64())
            } else {
                None
            }
        })
        .unwrap_or(0.0);

    // narrative: string → wrap as single-level NarrativeMultiZoom at zoom 0
    let narrative = output
        .get("narrative")
        .and_then(|n| n.as_str())
        .map(|text| super::types::NarrativeMultiZoom {
            levels: vec![super::types::NarrativeLevel {
                zoom: 0,
                text: text.to_string(),
            }],
        })
        .unwrap_or_default();

    // entities: [{name, role, importance, ...}]
    let entities: Vec<super::types::Entity> = output
        .get("entities")
        .and_then(|e| e.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    let name = e.get("name").and_then(|n| n.as_str())?;
                    Some(super::types::Entity {
                        name: name.to_string(),
                        role: e
                            .get("role")
                            .and_then(|r| r.as_str())
                            .unwrap_or("")
                            .to_string(),
                        importance: e
                            .get("importance")
                            .and_then(|i| i.as_f64())
                            .unwrap_or(0.0),
                        liveness: e
                            .get("liveness")
                            .and_then(|l| l.as_str())
                            .unwrap_or("live")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    // key_quotes: [{quote/text, speaker_role, importance, ...}]
    let key_quotes: Vec<super::types::KeyQuote> = output
        .get("key_quotes")
        .and_then(|q| q.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|q| {
                    // Accept both "quote" and "text" field names
                    let text = q
                        .get("quote")
                        .or_else(|| q.get("text"))
                        .and_then(|t| t.as_str())?;
                    Some(super::types::KeyQuote {
                        text: text.to_string(),
                        speaker: q
                            .get("speaker")
                            .and_then(|s| s.as_str())
                            .unwrap_or("")
                            .to_string(),
                        speaker_role: q
                            .get("speaker_role")
                            .and_then(|r| r.as_str())
                            .unwrap_or("")
                            .to_string(),
                        importance: q
                            .get("importance")
                            .and_then(|i| i.as_f64())
                            .unwrap_or(0.0),
                        chunk_ref: q
                            .get("at")
                            .or_else(|| q.get("chunk_ref"))
                            .and_then(|c| c.as_str())
                            .map(String::from),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    // transitions: {from_prior/prior, into_next/next}
    let transitions = output
        .get("transitions")
        .map(|t| super::types::Transitions {
            prior: t
                .get("from_prior")
                .or_else(|| t.get("prior"))
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string(),
            next: t
                .get("into_next")
                .or_else(|| t.get("next"))
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string(),
        })
        .unwrap_or_default();

    // Extract top-level decisions with stance and importance (episodic schema)
    // Merge with the decisions already extracted above
    if let Some(decs) = output.get("decisions").and_then(|d| d.as_array()) {
        // Only re-extract if we haven't already — check if the existing
        // decisions lack stance info (legacy extraction path)
        let has_stance = decisions.iter().any(|d| !d.stance.is_empty() && d.stance != "other");
        if !has_stance {
            decisions.clear();
            for d in decs {
                decisions.push(Decision {
                    decided: d
                        .get("decided")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .into(),
                    why: d.get("why").and_then(|v| v.as_str()).unwrap_or("").into(),
                    rejected: d
                        .get("rejected")
                        .or_else(|| d.get("alternatives"))
                        .and_then(|v| {
                            if let Some(s) = v.as_str() {
                                Some(s.to_string())
                            } else if let Some(arr) = v.as_array() {
                                Some(
                                    arr.iter()
                                        .filter_map(|a| a.as_str())
                                        .collect::<Vec<_>>()
                                        .join(", "),
                                )
                            } else {
                                None
                            }
                        })
                        .unwrap_or_default(),
                    stance: d
                        .get("stance")
                        .and_then(|v| v.as_str())
                        .unwrap_or("other")
                        .to_string(),
                    importance: d
                        .get("importance")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0),
                    ..Default::default()
                });
            }
        }
    }

    Ok(PyramidNode {
        id: node_id.to_string(),
        slug: slug.to_string(),
        depth,
        chunk_index,
        headline,
        distilled,
        topics,
        corrections,
        decisions,
        terms,
        dead_ends,
        self_prompt,
        children: output
            .get("source_nodes")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| normalize_node_id(s)))
                    .collect()
            })
            .unwrap_or_default(),
        parent_id: None,
        superseded_by: None,
        build_id: None,
        created_at: String::new(),
        time_range,
        weight,
        narrative,
        entities,
        key_quotes,
        transitions,
        ..Default::default()
    })
}

// ── Node ID normalization ────────────────────────────────────────────────────

/// Normalize a node ID to match the zero-padded format used by generate_node_id.
///
/// LLMs sometimes return unpadded IDs like "C-L0-70" when the actual node is
/// "C-L0-070". This function detects the pattern and zero-pads the numeric
/// suffix to 3 digits.
///
/// Examples:
/// - "C-L0-70" → "C-L0-070"
/// - "C-L0-5"  → "C-L0-005"
/// - "C-L0-070" → "C-L0-070" (already correct)
/// - "L1-003" → "L1-003" (already correct)
/// - "L2-1" → "L2-001"
pub(crate) fn normalize_node_id(id: &str) -> String {
    // Match patterns like "PREFIX-DIGITS" where prefix contains letters/hyphens
    if let Some(last_dash) = id.rfind('-') {
        let prefix = &id[..last_dash];
        let suffix = &id[last_dash + 1..];
        if let Ok(num) = suffix.parse::<u32>() {
            // Only pad if suffix is purely numeric and shorter than 3 digits
            if suffix.len() < 3 && suffix.chars().all(|c| c.is_ascii_digit()) {
                return format!("{}-{:03}", prefix, num);
            }
        }
    }
    id.to_string()
}

// ── Node ID generation ──────────────────────────────────────────────────────

/// Generate a node ID from a pattern with substitution.
///
/// Supported placeholders:
/// - `{index:03}` → zero-padded index (3 digits)
/// - `{index:04}` → zero-padded index (4 digits)
/// - `{index}`    → unpadded index
/// - `{depth}`    → current depth
///
/// Examples:
/// - `"L0-{index:03}"` with index=5 → `"L0-005"`
/// - `"L{depth}-{index:03}"` with depth=3, index=2 → `"L3-002"`
pub fn generate_node_id(pattern: &str, index: usize, depth: Option<i64>) -> String {
    let mut result = pattern.to_string();

    // Replace {depth} first (before index patterns that might contain digits)
    if let Some(d) = depth {
        result = result.replace("{depth}", &d.to_string());
    }

    // Replace {index:0N} patterns (zero-padded)
    if let Some(start) = result.find("{index:0") {
        if let Some(end) = result[start..].find('}') {
            let spec = &result[start..start + end + 1];
            // Extract padding width from {index:0N}
            let width_str = &spec["{index:0".len()..spec.len() - 1];
            if let Ok(width) = width_str.parse::<usize>() {
                let formatted = format!("{:0>width$}", index, width = width);
                result = result.replace(spec, &formatted);
            }
        }
    }

    // Replace bare {index}
    result = result.replace("{index}", &index.to_string());

    result
}

// ── IR Dispatch Layer ────────────────────────────────────────────────────────
//
// New dispatch functions for the IR execution path (P1.4 Task B).
// These read from `execution_plan::Step` + `ModelRequirements` instead of
// `ChainStep` + `ChainDefaults`. The legacy functions above are untouched.

/// Resolve the model string from IR `ModelRequirements` and config.
///
/// Priority:
/// 1. `reqs.model` — direct model override on the step
/// 2. `reqs.tier` — mapped through config tiers
/// 3. Falls back to primary_model when tier is absent or unrecognized
pub fn resolve_ir_model(reqs: &ModelRequirements, config: &LlmConfig) -> String {
    // Direct model override takes highest precedence
    if let Some(ref model) = reqs.model {
        return model.clone();
    }
    let tier = reqs.tier.as_deref().unwrap_or("mid");

    // Phase 3: consult provider registry tier routing (canonical source)
    if let Some(ref registry) = config.provider_registry {
        if let Ok(resolved) = registry.resolve_tier(tier, None, None, None) {
            return resolved.tier.model_id;
        }
        warn!("[IR] tier '{}' not in registry, falling back to legacy resolution", tier);
    }

    if let Some(model) = config.model_aliases.get(tier) {
        return model.clone();
    }
    match tier {
        "low" | "mid" => config.primary_model.clone(),
        "high" => config.fallback_model_1.clone(),
        "max" => config.fallback_model_2.clone(),
        other => {
            warn!("[IR] unknown tier '{}', using primary_model", other);
            config.primary_model.clone()
        }
    }
}

/// Resolve the primary context limit (in estimated tokens) for an IR step's model.
///
/// When a step overrides the model via tier or direct model string, the context
/// limit must match the resolved model — otherwise the cascade logic in
/// `call_model_unified` compares input size against the *original* config's
/// primary_context_limit and may incorrectly fall back to a model that ignores
/// response_format/response_schema.
fn resolve_ir_context_limit(
    reqs: &ModelRequirements,
    config: &LlmConfig,
    tier1: &Tier1Config,
) -> usize {
    // Direct model override — we don't know the model's actual limit, so use a
    // generous value (covers most large-context models on OpenRouter).
    if reqs.model.is_some() {
        return tier1.high_tier_context_limit;
    }
    let tier = reqs.tier.as_deref().unwrap_or("mid");

    // Phase 3: consult provider registry for the tier's context limit
    if let Some(ref registry) = config.provider_registry {
        if let Ok(resolved) = registry.resolve_tier(tier, None, None, None) {
            if let Some(limit) = resolved.tier.context_limit {
                return limit;
            }
        }
    }

    // Legacy fallback
    if config.model_aliases.contains_key(tier) {
        return tier1.high_tier_context_limit;
    }
    match tier {
        "low" | "mid" => config.primary_context_limit,
        "high" => tier1.high_tier_context_limit,
        "max" => tier1.max_tier_context_limit,
        _ => config.primary_context_limit,
    }
}

/// Resolve the primary context limit for a legacy chain step's model.
///
/// Same purpose as `resolve_ir_context_limit` but for the legacy `ChainStep` /
/// `ChainDefaults` dispatch path.
fn resolve_context_limit(
    step: &ChainStep,
    defaults: &ChainDefaults,
    config: &LlmConfig,
    tier1: &Tier1Config,
) -> usize {
    // Direct model override on step or defaults
    if step.model.is_some() {
        return tier1.high_tier_context_limit;
    }
    if step.model_tier.is_none() && defaults.model.is_some() {
        return tier1.high_tier_context_limit;
    }
    let tier = step
        .model_tier
        .as_deref()
        .unwrap_or(defaults.model_tier.as_str());

    // Phase 3: consult provider registry for the tier's context limit
    if let Some(ref registry) = config.provider_registry {
        if let Ok(resolved) = registry.resolve_tier(tier, None, None, None) {
            if let Some(limit) = resolved.tier.context_limit {
                return limit;
            }
        }
    }

    // Legacy fallback
    if config.model_aliases.contains_key(tier) {
        return tier1.high_tier_context_limit;
    }
    match tier {
        "low" | "mid" => config.primary_context_limit,
        "high" => tier1.high_tier_context_limit,
        "max" => tier1.max_tier_context_limit,
        _ => config.primary_context_limit,
    }
}

/// Resolve temperature from IR `ModelRequirements`, with a configurable default.
fn resolve_ir_temperature(reqs: &ModelRequirements, tier1: &Tier1Config) -> f32 {
    reqs.temperature.unwrap_or(tier1.default_ir_temperature)
}

fn resolve_ir_max_tokens(step: &Step, tier1: &Tier1Config) -> usize {
    let _ = step;
    tier1.ir_max_tokens
}

fn resolve_ir_llm_call_options(step: &Step, tier1: &Tier1Config) -> llm::LlmCallOptions {
    let min_timeout_secs = if step.response_schema.is_some() {
        match step.primitive.as_deref() {
            Some("classify") => Some(tier1.classify_min_timeout_secs),
            Some("web") => Some(tier1.web_min_timeout_secs),
            _ => Some(tier1.default_structured_min_timeout_secs),
        }
    } else {
        None
    };

    llm::LlmCallOptions { min_timeout_secs, ..Default::default() }
}

/// Dispatch an IR Step to the appropriate execution path.
///
/// Routes based on `step.operation`:
/// - `Llm` → `dispatch_ir_llm`
/// - `Transform` → `transform_runtime::execute_transform`
/// - `Mechanical` → `dispatch_ir_mechanical`
/// - `Wire | Task | Game` → error (Phase 4)
///
/// For LLM steps, returns `(parsed_output, Some(LlmResponse))`.
/// For non-LLM steps, returns `(output, None)`.
pub async fn dispatch_ir_step(
    step: &Step,
    resolved_input: &Value,
    system_prompt: &str,
    ctx: &ChainDispatchContext,
) -> Result<(Value, Option<LlmResponse>)> {
    match step.operation {
        StepOperation::Llm => {
            let (value, response) =
                dispatch_ir_llm(step, resolved_input, system_prompt, ctx).await?;
            Ok((value, Some(response)))
        }
        StepOperation::Transform => {
            let spec = step.transform.as_ref().ok_or_else(|| {
                anyhow!(
                    "IR step '{}' is Transform but has no transform spec",
                    step.id
                )
            })?;
            info!("[IR] step '{}' → transform '{}'", step.id, spec.function);
            let env = ValueEnv::new(resolved_input);
            let resolved_args = transform_runtime::resolve_transform_args(&spec.args, &env)?;
            let result =
                transform_runtime::execute_transform_function(&spec.function, &resolved_args)?;
            Ok((result, None))
        }
        StepOperation::Mechanical => {
            let result = dispatch_ir_mechanical(step, resolved_input, ctx).await?;
            Ok((result, None))
        }
        StepOperation::Wire | StepOperation::Task | StepOperation::Game => Err(anyhow!(
            "IR step '{}': operation {:?} not implemented in local executor (Phase 4)",
            step.id,
            step.operation
        )),
    }
}

/// Dispatch an IR LLM step: resolve model from ModelRequirements, call
/// `call_model_unified`, parse JSON output, retry at temp 0.1 on parse failure.
///
/// Returns `(parsed_json, LlmResponse)` so the caller can log costs from the
/// LlmResponse (usage, generation_id).
///
/// Phase 6 fix pass: builds a per-call `pyramid::step_context::StepContext`
/// from the dispatcher's `cache_base` (when present) and threads it
/// through every HTTP call in this function so the cache is reachable
/// from the production IR chain path.
pub async fn dispatch_ir_llm(
    step: &Step,
    resolved_input: &Value,
    system_prompt: &str,
    ctx: &ChainDispatchContext,
) -> Result<(Value, LlmResponse)> {
    let temperature = resolve_ir_temperature(&step.model_requirements, &ctx.tier1);
    let resolved_model = resolve_ir_model(&step.model_requirements, &ctx.config);
    let resolved_limit =
        resolve_ir_context_limit(&step.model_requirements, &ctx.config, &ctx.tier1);
    let max_tokens = resolve_ir_max_tokens(step, &ctx.tier1);
    let llm_options = resolve_ir_llm_call_options(step, &ctx.tier1);

    // Apply model override: pin ALL model slots (primary + fallback_1 +
    // fallback_2) to the resolved model so cascade stays on the same
    // provider. Also override context limit to match the resolved tier.
    let config_ref;
    let overridden_config;
    if resolved_model != ctx.config.primary_model
        || resolved_limit != ctx.config.primary_context_limit
    {
        let mut cfg = ctx.config.clone_with_model_override(&resolved_model);
        cfg.primary_context_limit = resolved_limit;
        overridden_config = cfg;
        config_ref = &overridden_config;
    } else {
        config_ref = &ctx.config;
    }

    let raw_input_len = serde_json::to_string(resolved_input)
        .unwrap_or_default()
        .len();
    info!(
        "[IR] step '{}' compact_inputs={}, raw_input_len={}",
        step.id, step.compact_inputs, raw_input_len
    );

    // Build user prompt from resolved input
    let user_prompt =
        serde_json::to_string_pretty(resolved_input).unwrap_or_else(|_| resolved_input.to_string());

    info!(
        "[IR] step '{}' → LLM (temp={}, model={}, ctx_limit={}, prompt_len={}, max_tokens={}, timeout_floor={:?})",
        step.id,
        temperature,
        short_model_name(&resolved_model),
        resolved_limit,
        user_prompt.len(),
        max_tokens,
        llm_options.min_timeout_secs
    );

    // Phase 6 fix pass: construct a cache-aware StepContext when the
    // dispatcher has a cache base attached. The base carries the
    // build-scoped db_path / build_id / event bus; per-call we layer
    // the step name, depth, chunk index, resolved model id, and prompt
    // hash on top.
    let cache_ctx = build_cache_ctx_for_ir_step(
        ctx,
        step,
        &resolved_model,
        system_prompt,
        &user_prompt,
    );

    // If step has a response_schema, use structured outputs for guaranteed JSON
    if let Some(ref schema) = step.response_schema {
        let schema_name = step.id.replace('-', "_").replace('.', "_");
        info!(
            "[IR] step '{}' → using structured output (schema: {}, schema_type: {:?})",
            step.id,
            schema_name,
            schema.get("type").and_then(|v| v.as_str()),
        );
        let response_format = serde_json::json!({
            "type": "json_schema",
            "json_schema": {
                "name": schema_name,
                "strict": true,
                "schema": schema
            }
        });
        let response = llm::call_model_unified_with_options_and_ctx(
            config_ref,
            cache_ctx.as_ref(),
            system_prompt,
            &user_prompt,
            temperature,
            max_tokens,
            Some(&response_format),
            llm_options,
        )
        .await?;
        let parsed = llm::extract_json(&response.content).map_err(|e| {
            anyhow!(
                "IR step '{}': structured output JSON parse failed: {}",
                step.id,
                e
            )
        })?;
        return Ok((parsed, response));
    }

    // No response_schema — standard path without structured output enforcement
    info!(
        "[IR] step '{}' → no response_schema, using standard JSON extraction",
        step.id,
    );

    // Standard path: call model, parse JSON, retry at temp 0.1 on failure.
    //
    // Phase 18b L8 retrofit: previously the audited branch bypassed
    // the Phase 6 cache because `call_model_audited` wrote its own
    // audit row and delegated to the non-ctx `call_model_unified`
    // path. Phase 18b unified the cache + audit paths via
    // `call_model_unified_with_audit_and_ctx`, which threads BOTH a
    // StepContext (for cache lookup/storage) and an AuditContext (for
    // the Theatre audit trail) through a single call. Audited cache
    // hits now write a `cache_hit = 1` audit row so the audit trail
    // stays contiguous and DADBEAR Oversight can show savings.
    let ir_audit_ctx = ctx.audit.as_ref().map(|audit| AuditContext {
        step_name: step.id.clone(),
        call_purpose: "ir_dispatch".to_string(),
        ..audit.clone()
    });
    let response = llm::call_model_unified_with_audit_and_ctx(
        config_ref,
        cache_ctx.as_ref(),
        ir_audit_ctx.as_ref(),
        system_prompt,
        &user_prompt,
        temperature,
        max_tokens,
        None,
        llm_options.clone(),
    )
    .await?;

    match llm::extract_json(&response.content) {
        Ok(json) => {
            info!("[IR] step '{}' → JSON parsed OK", step.id);
            Ok((json, response))
        }
        Err(_first_err) => {
            // JSON-retry guarantee: retry at temperature 0.1
            info!(
                "[IR] step '{}' → JSON parse failed, retrying at temp 0.1",
                step.id
            );
            let retry_response = llm::call_model_unified_with_options_and_ctx(
                config_ref,
                cache_ctx.as_ref(),
                system_prompt,
                &user_prompt,
                0.1,
                max_tokens,
                None,
                llm_options,
            )
            .await?;

            let parsed = llm::extract_json(&retry_response.content).map_err(|e| {
                anyhow!(
                    "IR step '{}': JSON parse failed after retry at temp 0.1: {}",
                    step.id,
                    e
                )
            })?;
            Ok((parsed, retry_response))
        }
    }
}

/// Phase 6 fix pass: build a per-call `pyramid::step_context::StepContext`
/// for an IR chain step so the cache hook in
/// `call_model_unified_with_options_and_ctx` is reachable from the
/// production dispatcher path.
///
/// Returns `None` in any of the following cases (the cache is then
/// bypassed for that call, and the LLM path falls through to the
/// legacy HTTP retry loop):
///
/// * The dispatch context has no `cache_base` (unit tests, pre-init
///   boot paths).
/// * The resolved model id is empty.
/// * The instruction key cannot be derived from the step.
///
/// Populates the dispatcher's lazy caches as a side effect: the
/// resolved model id is recorded against the tier, and the prompt hash
/// is cached keyed on the instruction.
fn build_cache_ctx_for_ir_step(
    ctx: &ChainDispatchContext,
    step: &Step,
    resolved_model: &str,
    system_prompt: &str,
    user_prompt: &str,
) -> Option<CacheStepContext> {
    let base = ctx.cache_base.as_ref()?;
    if resolved_model.is_empty() {
        return None;
    }

    let tier = step
        .model_requirements
        .tier
        .clone()
        .unwrap_or_else(|| "mid".to_string());
    base.cache_resolved_model(&tier, resolved_model);

    // Instruction key: prefer the resolved instruction string (the
    // template body as supplied by the IR). Falls back to the step id
    // when no instruction is attached (mechanical-ish LLM steps).
    let instruction_key = step
        .instruction
        .clone()
        .unwrap_or_else(|| step.id.clone());
    let prompt_hash = base.get_or_compute_prompt_hash(&instruction_key, || {
        // Include both the system prompt and the user prompt template
        // in the body snapshot. The caller above already substituted
        // `$var` references, so hashing `system_prompt + user_prompt`
        // gives us a build-scoped snapshot of "the prompt text this
        // step will ship to the LLM" — identical to what the spec
        // calls the "resolved instruction file content".
        let mut combined = String::with_capacity(system_prompt.len() + user_prompt.len() + 8);
        combined.push_str(system_prompt);
        combined.push_str("\n--user--\n");
        combined.push_str(user_prompt);
        combined
    });

    if prompt_hash.is_empty() {
        return None;
    }

    // Step metadata — primitive defaults to the step id when the
    // step has no primitive attached (legacy chain steps that use
    // `rust_function` instead).
    let primitive = step
        .primitive
        .clone()
        .unwrap_or_else(|| step.id.clone());
    let depth = step
        .storage_directive
        .as_ref()
        .and_then(|sd| sd.depth)
        .or_else(|| {
            step.metadata
                .as_ref()
                .and_then(|meta| meta.get("target_depth"))
                .and_then(|d| d.as_i64())
        })
        .unwrap_or(0);

    let mut cache_ctx = CacheStepContext::new(
        ctx.slug.clone(),
        base.build_id.clone(),
        step.id.clone(),
        primitive,
        depth,
        // chunk_index is set by the caller for forEach iterations via
        // the `chunk_index` field already on Step metadata; the IR
        // dispatcher does not currently pass a specific chunk index to
        // dispatch_ir_llm, so we leave it as `None` and rely on the
        // cache key for content addressing. The `pyramid_step_cache`
        // `chunk_index` column is written as -1 (the StepContext
        // default) which aligns with the Phase 2 retrofit pattern for
        // whole-node LLM calls.
        None,
        base.db_path.clone(),
    )
    .with_model_resolution(tier, resolved_model.to_string())
    .with_prompt_hash(prompt_hash);
    if let (Some(cn), Some(ct)) = (&base.chain_name, &base.content_type) {
        cache_ctx = cache_ctx.with_chain_context(cn.clone(), ct.clone());
    }
    if let Some(bus) = base.bus.as_ref() {
        cache_ctx = cache_ctx.with_bus(bus.clone());
    }
    Some(cache_ctx)
}

/// Dispatch an IR mechanical step: look up `step.rust_function` in the registry.
///
/// Same registry as the legacy `dispatch_mechanical` but reads from IR types.
pub async fn dispatch_ir_mechanical(
    step: &Step,
    resolved_input: &Value,
    ctx: &ChainDispatchContext,
) -> Result<Value> {
    let fn_name = step
        .rust_function
        .as_deref()
        .ok_or_else(|| anyhow!("IR mechanical step '{}' missing rust_function", step.id))?;
    info!("[IR] step '{}' → mechanical fn '{}'", step.id, fn_name);
    dispatch_mechanical(fn_name, resolved_input, ctx).await
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::execution_plan::{
        ErrorPolicy, ModelRequirements, Step, StepOperation, TransformSpec,
    };
    use super::*;

    #[test]
    fn test_generate_node_id_basic() {
        assert_eq!(generate_node_id("L0-{index:03}", 5, None), "L0-005");
        assert_eq!(generate_node_id("L0-{index:03}", 42, None), "L0-042");
        assert_eq!(generate_node_id("L0-{index:03}", 0, None), "L0-000");
    }

    #[test]
    fn test_generate_node_id_with_depth() {
        assert_eq!(
            generate_node_id("L{depth}-{index:03}", 2, Some(3)),
            "L3-002"
        );
        assert_eq!(
            generate_node_id("L{depth}-{index:03}", 0, Some(0)),
            "L0-000"
        );
    }

    #[test]
    fn test_generate_node_id_bare_index() {
        assert_eq!(generate_node_id("node-{index}", 7, None), "node-7");
    }

    #[test]
    fn test_generate_node_id_four_digit_pad() {
        assert_eq!(generate_node_id("N{index:04}", 3, None), "N0003");
    }

    #[test]
    fn test_normalize_node_id() {
        // Unpadded → padded
        assert_eq!(normalize_node_id("C-L0-70"), "C-L0-070");
        assert_eq!(normalize_node_id("C-L0-5"), "C-L0-005");
        assert_eq!(normalize_node_id("L2-1"), "L2-001");
        // Already padded → unchanged
        assert_eq!(normalize_node_id("C-L0-070"), "C-L0-070");
        assert_eq!(normalize_node_id("L1-003"), "L1-003");
        // No numeric suffix → unchanged
        assert_eq!(normalize_node_id("apex"), "apex");
    }

    #[test]
    fn test_is_known_mechanical_function() {
        assert!(is_known_mechanical_function("extract_import_graph"));
        assert!(is_known_mechanical_function("cluster_by_imports"));
        assert!(!is_known_mechanical_function("nonexistent_function"));
    }

    #[tokio::test]
    async fn test_dispatch_mechanical_unknown_fn() {
        let ctx = ChainDispatchContext {
            db_reader: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            db_writer: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            slug: "test".into(),
            config: LlmConfig::default(),
            tier1: Tier1Config::default(),
            ops: OperationalConfig::default(),
            audit: None,
            cache_base: None,
            concurrency_cap: None,
            state: None,
            chains_dir: None,
            target_id: None,
            sub_chain_depth: None,
        };
        let result = dispatch_mechanical("nonexistent", &serde_json::json!({}), &ctx).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Unknown mechanical function"));
    }

    #[tokio::test]
    async fn test_dispatch_mechanical_known_fn() {
        let ctx = ChainDispatchContext {
            db_reader: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            db_writer: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            slug: "test-slug".into(),
            config: LlmConfig::default(),
            tier1: Tier1Config::default(),
            ops: OperationalConfig::default(),
            audit: None,
            cache_base: None,
            concurrency_cap: None,
            state: None,
            chains_dir: None,
            target_id: None,
            sub_chain_depth: None,
        };
        let input = serde_json::json!({"files": ["main.rs"]});
        let result = dispatch_mechanical("extract_import_graph", &input, &ctx).await.unwrap();
        assert_eq!(result["_mechanical"], "extract_import_graph");
        assert_eq!(result["_status"], "placeholder");
        assert_eq!(result["slug"], "test-slug");
    }

    #[test]
    fn test_build_node_from_output_minimal() {
        let output = serde_json::json!({
            "headline": "Test Node",
            "distilled": "A test distillation.",
            "topics": [],
            "corrections": [],
            "decisions": [],
            "terms": [],
            "dead_ends": [],
        });
        let node = build_node_from_output(&output, "L0-001", "test-slug", 0, Some(1)).unwrap();
        assert_eq!(node.id, "L0-001");
        assert_eq!(node.slug, "test-slug");
        assert_eq!(node.depth, 0);
        assert_eq!(node.chunk_index, Some(1));
        assert_eq!(node.headline, "Test Node");
        assert_eq!(node.distilled, "A test distillation.");
        assert!(node.children.is_empty());
        assert!(node.parent_id.is_none());
    }

    #[test]
    fn test_build_node_from_output_with_orientation() {
        // "orientation" takes precedence over "distilled"
        let output = serde_json::json!({
            "orientation": "Orientation text",
            "distilled": "Should not be used",
            "headline": "Node",
        });
        let node = build_node_from_output(&output, "L1-000", "s", 1, None).unwrap();
        assert_eq!(node.distilled, "Orientation text");
        assert_eq!(node.self_prompt, "Orientation text");
    }

    #[test]
    fn test_resolve_model_step_override() {
        let step = ChainStep {
            name: "test".into(),
            primitive: "compress".into(),
            model: Some("custom/model".into()),
            instruction: Some("x".into()),
            ..Default::default()
        };
        let defaults = ChainDefaults {
            model_tier: "mid".into(),
            model: None,
            temperature: 0.3,
            on_error: "retry(2)".into(),
        };
        let config = LlmConfig::default();
        assert_eq!(resolve_model(&step, &defaults, &config), "custom/model");
    }

    #[test]
    fn test_resolve_model_tier_mapping() {
        let make_step = |tier: &str| ChainStep {
            name: "test".into(),
            primitive: "compress".into(),
            model_tier: Some(tier.into()),
            instruction: Some("x".into()),
            ..Default::default()
        };
        let defaults = ChainDefaults {
            model_tier: "mid".into(),
            model: None,
            temperature: 0.3,
            on_error: "retry(2)".into(),
        };
        let config = LlmConfig::default();

        assert_eq!(
            resolve_model(&make_step("low"), &defaults, &config),
            config.primary_model
        );
        assert_eq!(
            resolve_model(&make_step("mid"), &defaults, &config),
            config.primary_model
        );
        assert_eq!(
            resolve_model(&make_step("high"), &defaults, &config),
            config.fallback_model_1
        );
        assert_eq!(
            resolve_model(&make_step("max"), &defaults, &config),
            config.fallback_model_2
        );
    }

    // ── IR dispatch tests ───────────────────────────────────────────────────

    /// Helper to build a minimal IR Step for testing.
    fn ir_step(id: &str, op: StepOperation) -> Step {
        Step {
            id: id.to_string(),
            operation: op,
            primitive: None,
            depends_on: vec![],
            iteration: None,
            input: serde_json::json!({}),
            instruction: Some("test prompt".to_string()),
            instruction_map: None,
            compact_inputs: false,
            output_schema: None,
            constraints: None,
            error_policy: ErrorPolicy::Retry(2),
            model_requirements: ModelRequirements::default(),
            storage_directive: None,
            cost_estimate: super::super::execution_plan::CostEstimate::default(),
            action_id: None,
            rust_function: None,
            transform: None,
            when: None,
            context: vec![],
            response_schema: None,
            source_step_name: None,
            converge_metadata: None,
            metadata: None,
            scope: None,
        }
    }

    #[test]
    fn test_resolve_ir_model_direct_override() {
        let reqs = ModelRequirements {
            tier: Some("low".into()),
            model: Some("custom/my-model".into()),
            temperature: None,
        };
        let config = LlmConfig::default();
        // Direct model override wins over tier
        assert_eq!(resolve_ir_model(&reqs, &config), "custom/my-model");
    }

    #[test]
    fn test_resolve_ir_model_tier_mapping() {
        let config = LlmConfig::default();

        let make_reqs = |tier: &str| ModelRequirements {
            tier: Some(tier.into()),
            model: None,
            temperature: None,
        };

        assert_eq!(
            resolve_ir_model(&make_reqs("low"), &config),
            config.primary_model
        );
        assert_eq!(
            resolve_ir_model(&make_reqs("mid"), &config),
            config.primary_model
        );
        assert_eq!(
            resolve_ir_model(&make_reqs("high"), &config),
            config.fallback_model_1
        );
        assert_eq!(
            resolve_ir_model(&make_reqs("max"), &config),
            config.fallback_model_2
        );
    }

    #[test]
    fn test_resolve_ir_model_default_tier() {
        // When tier is None, defaults to "mid" → primary_model
        let reqs = ModelRequirements::default();
        let config = LlmConfig::default();
        assert_eq!(resolve_ir_model(&reqs, &config), config.primary_model);
    }

    #[test]
    fn test_resolve_ir_model_unknown_tier() {
        let reqs = ModelRequirements {
            tier: Some("ultra".into()),
            model: None,
            temperature: None,
        };
        let config = LlmConfig::default();
        // Unknown tier falls back to primary
        assert_eq!(resolve_ir_model(&reqs, &config), config.primary_model);
    }

    #[test]
    fn test_resolve_ir_temperature_override() {
        let reqs = ModelRequirements {
            tier: None,
            model: None,
            temperature: Some(0.7),
        };
        let tier1 = Tier1Config::default();
        assert_eq!(resolve_ir_temperature(&reqs, &tier1), 0.7);
    }

    #[test]
    fn test_resolve_ir_temperature_default() {
        let reqs = ModelRequirements::default();
        let tier1 = Tier1Config::default();
        assert_eq!(resolve_ir_temperature(&reqs, &tier1), 0.3);
    }

    #[test]
    fn test_resolve_ir_timeout_floor_for_structured_classify() {
        let tier1 = Tier1Config::default();
        let mut step = ir_step("clustering", StepOperation::Llm);
        step.primitive = Some("classify".to_string());
        step.response_schema = Some(serde_json::json!({"type": "object"}));

        assert_eq!(resolve_ir_max_tokens(&step, &tier1), tier1.ir_max_tokens);
        assert_eq!(
            resolve_ir_llm_call_options(&step, &tier1).min_timeout_secs,
            Some(tier1.classify_min_timeout_secs)
        );
    }

    #[test]
    fn test_resolve_ir_llm_defaults_for_unstructured_steps() {
        let tier1 = Tier1Config::default();
        let step = ir_step("l1_synthesis", StepOperation::Llm);
        assert_eq!(resolve_ir_max_tokens(&step, &tier1), tier1.ir_max_tokens);
        assert_eq!(
            resolve_ir_llm_call_options(&step, &tier1).min_timeout_secs,
            None
        );
    }

    #[tokio::test]
    async fn test_dispatch_ir_mechanical_routes_correctly() {
        let ctx = ChainDispatchContext {
            db_reader: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            db_writer: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            slug: "ir-test".into(),
            config: LlmConfig::default(),
            tier1: Tier1Config::default(),
            ops: OperationalConfig::default(),
            audit: None,
            cache_base: None,
            concurrency_cap: None,
            state: None,
            chains_dir: None,
            target_id: None,
            sub_chain_depth: None,
        };
        let mut step = ir_step("mech_step", StepOperation::Mechanical);
        step.rust_function = Some("extract_import_graph".into());
        let input = serde_json::json!({"files": ["lib.rs"]});
        let result = dispatch_ir_mechanical(&step, &input, &ctx).await.unwrap();
        assert_eq!(result["_mechanical"], "extract_import_graph");
        assert_eq!(result["_status"], "placeholder");
        assert_eq!(result["slug"], "ir-test");
    }

    #[tokio::test]
    async fn test_dispatch_ir_mechanical_missing_fn_name() {
        let ctx = ChainDispatchContext {
            db_reader: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            db_writer: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            slug: "test".into(),
            config: LlmConfig::default(),
            tier1: Tier1Config::default(),
            ops: OperationalConfig::default(),
            audit: None,
            cache_base: None,
            concurrency_cap: None,
            state: None,
            chains_dir: None,
            target_id: None,
            sub_chain_depth: None,
        };
        let step = ir_step("no_fn", StepOperation::Mechanical);
        // rust_function is None
        let result = dispatch_ir_mechanical(&step, &serde_json::json!({}), &ctx).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("missing rust_function"));
    }

    #[tokio::test]
    async fn test_dispatch_ir_mechanical_unknown_fn() {
        let ctx = ChainDispatchContext {
            db_reader: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            db_writer: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            slug: "test".into(),
            config: LlmConfig::default(),
            tier1: Tier1Config::default(),
            ops: OperationalConfig::default(),
            audit: None,
            cache_base: None,
            concurrency_cap: None,
            state: None,
            chains_dir: None,
            target_id: None,
            sub_chain_depth: None,
        };
        let mut step = ir_step("bad_fn", StepOperation::Mechanical);
        step.rust_function = Some("nonexistent_fn".into());
        let result = dispatch_ir_mechanical(&step, &serde_json::json!({}), &ctx).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Unknown mechanical function"));
    }

    #[tokio::test]
    async fn test_dispatch_ir_step_transform_routes() {
        let ctx = ChainDispatchContext {
            db_reader: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            db_writer: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            slug: "test".into(),
            config: LlmConfig::default(),
            tier1: Tier1Config::default(),
            ops: OperationalConfig::default(),
            audit: None,
            cache_base: None,
            concurrency_cap: None,
            state: None,
            chains_dir: None,
            target_id: None,
            sub_chain_depth: None,
        };
        let mut step = ir_step("count_step", StepOperation::Transform);
        step.transform = Some(TransformSpec {
            function: "count".into(),
            args: serde_json::json!({"collection": [1, 2, 3]}),
        });
        let (result, llm_resp) = dispatch_ir_step(&step, &serde_json::json!({}), "", &ctx)
            .await
            .unwrap();
        assert_eq!(result, serde_json::json!(3));
        assert!(llm_resp.is_none()); // transforms don't produce LlmResponse
    }

    #[tokio::test]
    async fn test_dispatch_ir_step_transform_resolves_args_against_input() {
        let ctx = ChainDispatchContext {
            db_reader: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            db_writer: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            slug: "test".into(),
            config: LlmConfig::default(),
            tier1: Tier1Config::default(),
            ops: OperationalConfig::default(),
            audit: None,
            cache_base: None,
            concurrency_cap: None,
            state: None,
            chains_dir: None,
            target_id: None,
            sub_chain_depth: None,
        };
        let mut step = ir_step("coalesce_step", StepOperation::Transform);
        step.transform = Some(TransformSpec {
            function: "coalesce".into(),
            args: serde_json::json!({
                "values": ["$primary", "$fallback"]
            }),
        });
        let input = serde_json::json!({
            "primary": null,
            "fallback": [1, 2, 3]
        });
        let (result, llm_resp) = dispatch_ir_step(&step, &input, "", &ctx).await.unwrap();
        assert_eq!(result, serde_json::json!([1, 2, 3]));
        assert!(llm_resp.is_none());
    }

    #[tokio::test]
    async fn test_dispatch_ir_step_transform_missing_spec() {
        let ctx = ChainDispatchContext {
            db_reader: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            db_writer: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            slug: "test".into(),
            config: LlmConfig::default(),
            tier1: Tier1Config::default(),
            ops: OperationalConfig::default(),
            audit: None,
            cache_base: None,
            concurrency_cap: None,
            state: None,
            chains_dir: None,
            target_id: None,
            sub_chain_depth: None,
        };
        let step = ir_step("bad_transform", StepOperation::Transform);
        // transform is None
        let result = dispatch_ir_step(&step, &serde_json::json!({}), "", &ctx).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("no transform spec"));
    }

    #[tokio::test]
    async fn test_dispatch_ir_step_wire_not_implemented() {
        let ctx = ChainDispatchContext {
            db_reader: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            db_writer: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            slug: "test".into(),
            config: LlmConfig::default(),
            tier1: Tier1Config::default(),
            ops: OperationalConfig::default(),
            audit: None,
            cache_base: None,
            concurrency_cap: None,
            state: None,
            chains_dir: None,
            target_id: None,
            sub_chain_depth: None,
        };
        let step = ir_step("wire_step", StepOperation::Wire);
        let result = dispatch_ir_step(&step, &serde_json::json!({}), "", &ctx).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not implemented"));
    }

    #[tokio::test]
    async fn test_dispatch_ir_step_task_not_implemented() {
        let ctx = ChainDispatchContext {
            db_reader: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            db_writer: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            slug: "test".into(),
            config: LlmConfig::default(),
            tier1: Tier1Config::default(),
            ops: OperationalConfig::default(),
            audit: None,
            cache_base: None,
            concurrency_cap: None,
            state: None,
            chains_dir: None,
            target_id: None,
            sub_chain_depth: None,
        };
        let step = ir_step("task_step", StepOperation::Task);
        let result = dispatch_ir_step(&step, &serde_json::json!({}), "", &ctx).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not implemented"));
    }

    #[tokio::test]
    async fn test_dispatch_ir_step_game_not_implemented() {
        let ctx = ChainDispatchContext {
            db_reader: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            db_writer: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            slug: "test".into(),
            config: LlmConfig::default(),
            tier1: Tier1Config::default(),
            ops: OperationalConfig::default(),
            audit: None,
            cache_base: None,
            concurrency_cap: None,
            state: None,
            chains_dir: None,
            target_id: None,
            sub_chain_depth: None,
        };
        let step = ir_step("game_step", StepOperation::Game);
        let result = dispatch_ir_step(&step, &serde_json::json!({}), "", &ctx).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not implemented"));
    }

    #[tokio::test]
    async fn test_dispatch_ir_step_mechanical_routes() {
        let ctx = ChainDispatchContext {
            db_reader: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            db_writer: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            slug: "slug".into(),
            config: LlmConfig::default(),
            tier1: Tier1Config::default(),
            ops: OperationalConfig::default(),
            audit: None,
            cache_base: None,
            concurrency_cap: None,
            state: None,
            chains_dir: None,
            target_id: None,
            sub_chain_depth: None,
        };
        let mut step = ir_step("mech", StepOperation::Mechanical);
        step.rust_function = Some("extract_mechanical_metadata".into());
        let (result, llm_resp) = dispatch_ir_step(&step, &serde_json::json!({}), "", &ctx)
            .await
            .unwrap();
        assert_eq!(result["_mechanical"], "extract_mechanical_metadata");
        assert!(llm_resp.is_none()); // mechanical steps don't produce LlmResponse
    }
}
