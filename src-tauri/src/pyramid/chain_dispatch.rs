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
use super::types::{Correction, Decision, PyramidNode, Term, Topic};
use super::walker_decision::DispatchDecision;
use super::walker_resolver::ProviderType as WalkerProviderType;
use super::{OperationalConfig, Tier1Config};

// ── Walker v3 W2b: Decision-aware model read helpers ────────────────────────
//
// File-private helper that pulls the first OpenRouter model_list entry
// from an attached DispatchDecision. Mirrors the private helper in
// llm.rs (W2a scope) — duplicated here rather than exported because
// W2a/b run in parallel and can't coordinate on a shared helper
// location without creating merge seams. Removed in W3 when the
// Decision becomes the sole source and the legacy fallback expressions
// go.
//
// Callers chain `.unwrap_or_else(|| config.primary_model.clone())` (or
// fall through to the provider-registry / legacy-match chain) to
// preserve the Phase 1 legacy fallback — see §6 migration contract.

fn first_openrouter_model_from_decision(
    decision: Option<&Arc<DispatchDecision>>,
) -> Option<String> {
    decision
        .and_then(|d| d.per_provider.get(&WalkerProviderType::OpenRouter))
        .and_then(|p| p.model_list.as_ref())
        .and_then(|ml| ml.first().cloned())
}

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

// ── Walker v3 W1b: outer-step DispatchDecision builder ─────────────────────
//
// Plan rev 1.0.2 §2.9 DispatchDecision + §6 Phase 1: the Decision is built
// ONCE per chain step at the outer LLM dispatch entry. Every CacheStepContext
// constructed inside the step inherits the same `Arc<DispatchDecision>` via
// `with_dispatch_decision`, so downstream consumers (W2's target) can reach
// the pre-resolved per-provider params + effective call order without
// re-walking the resolver.
//
// Permissive-on-failure: if `DispatchDecision::build` errors (e.g. cascade
// exhausted, all providers NotReady, DB read failure), we log and return
// `None`. The CacheStepContext still reaches consumers with
// `dispatch_decision = None`; legacy consumers fall back to the
// `config.primary_model / fallback_model_{1,2}` chain so steps don't
// hard-fail before W2 migrates the last site. `EVENT_DECISION_BUILD_FAILED`
// is emitted by `build()` itself for the cascade-exhausted case.
//
// `test_capture` module below lets the W1b integration test observe the
// Decision that the executor built without running a real LLM dispatch.
/// Walker v3 W1b: build the step's DispatchDecision at the outer LLM
/// dispatch entry. Exposed as `pub` so the W1b integration test can
/// observe the Decision attachment without running a real LLM dispatch.
/// Production call sites are the private `dispatch_llm` / `dispatch_ir_llm`
/// paths within this module.
pub async fn build_step_dispatch_decision(
    ctx: &ChainDispatchContext,
    slot: &str,
) -> Option<Arc<DispatchDecision>> {
    let conn = ctx.db_reader.lock().await;
    match DispatchDecision::build(slot, &conn) {
        Ok(decision) => {
            let arc = Arc::new(decision);
            test_capture::record_if_enabled(slot, &arc);
            Some(arc)
        }
        Err(e) => {
            tracing::warn!(
                event = "decision_build_failed_in_executor",
                slot = %slot,
                error = ?e,
                "W1b: dispatch decision build failed; falling through to legacy dispatch"
            );
            None
        }
    }
}

#[doc(hidden)]
pub mod test_capture {
    //! Walker v3 W1b test-observability hook.
    //!
    //! The W1b integration test needs to observe that the executor built
    //! and attached a DispatchDecision at outer-step entry, WITHOUT
    //! running a real LLM dispatch (which would require an OpenRouter
    //! key / network + full chain_executor wiring through a 15k-line
    //! file). Gating this hook behind `CAPTURE_ENABLED` keeps the
    //! production dispatch path zero-cost (a single `AtomicBool::load`
    //! per step, opt-in only when a test explicitly calls `enable`).
    //!
    //! Production callers never flip `CAPTURE_ENABLED`. If you find
    //! yourself tempted to enable capture from non-test code: don't —
    //! the capture store grows unbounded and leaks across build runs.
    use super::DispatchDecision;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};

    static CAPTURE_ENABLED: AtomicBool = AtomicBool::new(false);

    #[derive(Debug, Clone)]
    pub struct CapturedDecision {
        pub slot: String,
        pub decision: Arc<DispatchDecision>,
    }

    fn slot_store() -> &'static Mutex<Vec<CapturedDecision>> {
        static STORE: OnceLock<Mutex<Vec<CapturedDecision>>> = OnceLock::new();
        STORE.get_or_init(|| Mutex::new(Vec::new()))
    }

    pub(super) fn record_if_enabled(slot: &str, decision: &Arc<DispatchDecision>) {
        if !CAPTURE_ENABLED.load(Ordering::Relaxed) {
            return;
        }
        if let Ok(mut g) = slot_store().lock() {
            g.push(CapturedDecision {
                slot: slot.to_string(),
                decision: decision.clone(),
            });
        }
    }

    /// Enable capture for the current process. Idempotent. Tests that
    /// want the capture path must call this at start. Safe to leave
    /// enabled for the duration of a test binary.
    pub fn enable() {
        CAPTURE_ENABLED.store(true, Ordering::Relaxed);
    }

    /// Disable capture and clear the store. Called at test teardown
    /// when capture is no longer needed.
    pub fn disable() {
        CAPTURE_ENABLED.store(false, Ordering::Relaxed);
        clear();
    }

    /// Clear any captured decisions. Tests call this at start so state
    /// doesn't leak across tests within the same process.
    pub fn clear() {
        if let Ok(mut g) = slot_store().lock() {
            g.clear();
        }
    }

    /// Return the list of captured decisions so the test can assert on
    /// count + per-slot Decision content.
    pub fn snapshot() -> Vec<CapturedDecision> {
        slot_store()
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
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
        dispatch_mechanical(fn_name, resolved_input, ctx)
    } else {
        dispatch_llm(step, resolved_input, system_prompt, defaults, ctx).await
    }
}

// ── LLM dispatch ────────────────────────────────────────────────────────────

/// Resolve the model string from step overrides, tier routing, or defaults.
///
/// Walker v3 W2b: priority-2 consults the attached `DispatchDecision`'s
/// per-provider `model_list[OpenRouter][0]` when one is available; the
/// provider-registry path drops to priority-3, and the legacy
/// `match tier => primary_model / fallback_model_{1,2}` stays as the
/// final fallback for Decision-less call sites (unit tests, bring-up
/// paths). W3 deletes that legacy match when the struct fields go.
fn resolve_model(
    step: &ChainStep,
    defaults: &ChainDefaults,
    config: &LlmConfig,
    dispatch_decision: Option<&Arc<DispatchDecision>>,
) -> String {
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

    // Walker v3 W2b priority 2: the pre-built DispatchDecision is the
    // canonical source when present. Built once at the outer dispatch
    // entry (W1b) and threaded via step_ctx.dispatch_decision.
    if let Some(model) = first_openrouter_model_from_decision(dispatch_decision) {
        return model;
    }

    // Phase 3: consult provider registry tier routing (transitional
    // fallback when no Decision is attached — e.g. unit tests / bring-up).
    if let Some(ref registry) = config.provider_registry {
        if let Ok(resolved) = registry.resolve_tier(tier, None, None, None) {
            return resolved.tier.model_id;
        }
        warn!("[CHAIN] tier '{}' not in registry, falling back to legacy resolution", tier);
    }

    // W3c: legacy `config.primary_model` / `fallback_model_{1,2}` arms
    // deleted. Aliases remain as an operator-level escape hatch; if
    // neither the Decision, registry, nor an alias covers the tier we
    // stamp `<unknown>` — the call will fail at dispatch time via
    // `call_model_unified`'s RouteSkipped path (no model_list → skip).
    if let Some(model) = config.model_aliases.get(tier) {
        return model.clone();
    }
    warn!(
        "[CHAIN] walker-v3: tier '{}' had no Decision, no registry row, and no alias; \
         returning '<unknown>' — dispatch will surface no-model-available",
        tier,
    );
    "<unknown>".to_string()
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

    // Walker v3 W1b: build the step's DispatchDecision ONCE at the outer
    // LLM entry. Every CacheStepContext constructed below inherits the
    // same Arc via `.with_dispatch_decision`. Permissive on failure —
    // see `build_step_dispatch_decision` for the fallback policy.
    // `slot` mirrors the tier used by cache-site `with_model_resolution`
    // calls so the Decision is keyed to the same scope as the LLM call.
    //
    // W2b reorder: built BEFORE resolve_model so the resolver can read
    // the Decision's OpenRouter model_list as the canonical source.
    let slot = step
        .model_tier
        .clone()
        .unwrap_or_else(|| defaults.model_tier.clone());
    let dispatch_decision = build_step_dispatch_decision(ctx, &slot).await;

    let resolved_model =
        resolve_model(step, defaults, &ctx.config, dispatch_decision.as_ref());
    let _resolved_limit = resolve_context_limit(step, defaults, &ctx.config, &ctx.tier1);
    let max_tokens: usize = ctx.tier1.ir_max_tokens;

    // W3c: the deleted `LlmConfig.primary_model` + `primary_context_limit`
    // fields used to carry the resolved model/limit down into
    // `call_model` via `clone_with_model_override`. The Decision attached
    // to the cache StepContext is now the sole source — `call_model_unified`
    // reads `model_list[0]` / `context_limit` from it directly, so the
    // override clone is unnecessary. `resolved_model` is retained for
    // tracing / cache-row model_resolution below.
    let config_ref = &ctx.config;

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
            // Walker v3 W1b: inherit the step's outer Decision.
            if let Some(d) = &dispatch_decision {
                c = c.with_dispatch_decision(d.clone());
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
                        // Walker v3 W1b: inherit the step's outer Decision.
                        if let Some(d) = &dispatch_decision {
                            c = c.with_dispatch_decision(d.clone());
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
        // Walker v3 W1b: inherit the step's outer Decision.
        if let Some(d) = &dispatch_decision {
            c = c.with_dispatch_decision(d.clone());
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
                    // Walker v3 W1b: inherit the step's outer Decision.
                    if let Some(d) = &dispatch_decision {
                        c = c.with_dispatch_decision(d.clone());
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
                    // Walker v3 W1b: inherit the step's outer Decision.
                    if let Some(d) = &dispatch_decision {
                        c = c.with_dispatch_decision(d.clone());
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
const MECHANICAL_FUNCTIONS: &[&str] = &[
    "extract_import_graph",
    "extract_mechanical_metadata",
    "cluster_by_imports",
    "cluster_by_entity_overlap",
];

/// Dispatch a mechanical step to a named Rust function.
///
/// For v1, the actual build.rs functions require signatures that don't match
/// the generic `(input: &Value, ctx: &ChainDispatchContext) -> Result<Value>` contract.
/// The dispatch framework is established here; actual wiring happens in Phase 5
/// when the chain executor replaces the hardcoded build pipeline.
fn dispatch_mechanical(function_name: &str, input: &Value, ctx: &ChainDispatchContext) -> Result<Value> {
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
/// Priority (Walker v3 W2b):
/// 1. `reqs.model` — operator-supplied explicit model override
/// 2. `DispatchDecision.per_provider[OpenRouter].model_list[0]` — the
///    canonical source when the outer dispatcher has built one
///    (see §2.9 / §6 Phase 1 migration contract)
/// 3. `config.provider_registry.resolve_tier(tier)` — transitional
///    fallback when no Decision is attached (unit tests, bring-up)
/// 4. Legacy hardcoded `match tier => primary_model / fallback_model_{1,2}`
///    — final fallback; W3 removes the match when the struct fields go
///
/// Callers with a live `DispatchDecision` (the in-file `dispatch_ir_llm`
/// at the outer dispatch entry) pass `Some(&arc)`; callers in
/// `chain_executor.rs` (step-output provenance writes, cost-log rows)
/// don't carry a Decision in scope and pass `None`, which routes them
/// through the registry → legacy chain.
pub fn resolve_ir_model(
    reqs: &ModelRequirements,
    config: &LlmConfig,
    dispatch_decision: Option<&Arc<DispatchDecision>>,
) -> String {
    // Priority 1: operator-supplied explicit model override.
    if let Some(ref model) = reqs.model {
        return model.clone();
    }

    // Priority 2: the pre-built DispatchDecision is the canonical source
    // when the outer dispatcher has constructed one. Reads the first
    // OpenRouter model_list entry — matches
    // `first_openrouter_model_from_decision` in llm.rs so the dispatch
    // path and the provenance path agree.
    if let Some(model) = first_openrouter_model_from_decision(dispatch_decision) {
        return model;
    }

    let tier = reqs.tier.as_deref().unwrap_or("mid");

    // Priority 3: consult provider registry tier routing (transitional
    // fallback — used by unit tests and bring-up paths where no
    // DispatchDecision is attached).
    if let Some(ref registry) = config.provider_registry {
        if let Ok(resolved) = registry.resolve_tier(tier, None, None, None) {
            return resolved.tier.model_id;
        }
        warn!("[IR] tier '{}' not in registry, falling back to legacy resolution", tier);
    }

    // W3c: legacy hardcoded per-tier match arms deleted. Aliases
    // remain as the last escape hatch. Missing → `<unknown>` so
    // dispatch surfaces "no model available" rather than silently
    // picking up a hardcoded default.
    if let Some(model) = config.model_aliases.get(tier) {
        return model.clone();
    }
    warn!(
        "[IR] walker-v3: tier '{}' had no Decision, no registry row, and no alias; \
         returning '<unknown>' — dispatch will surface no-model-available",
        tier,
    );
    "<unknown>".to_string()
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

    // W3c: legacy fallback on `LlmConfig.primary_context_limit` replaced
    // with Tier1Config's `primary_context_limit` (same numeric field,
    // now authoritative in operational config).
    if config.model_aliases.contains_key(tier) {
        return tier1.high_tier_context_limit;
    }
    match tier {
        "low" | "mid" => tier1.primary_context_limit,
        "high" => tier1.high_tier_context_limit,
        "max" => tier1.max_tier_context_limit,
        _ => tier1.primary_context_limit,
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

    // W3c: legacy fallback on `LlmConfig.primary_context_limit` replaced
    // with Tier1Config's `primary_context_limit`.
    if config.model_aliases.contains_key(tier) {
        return tier1.high_tier_context_limit;
    }
    match tier {
        "low" | "mid" => tier1.primary_context_limit,
        "high" => tier1.high_tier_context_limit,
        "max" => tier1.max_tier_context_limit,
        _ => tier1.primary_context_limit,
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
            let result = dispatch_ir_mechanical(step, resolved_input, ctx)?;
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

    // Walker v3 W1b: build the step's DispatchDecision ONCE at the outer
    // IR LLM entry. See dispatch_llm for the symmetric wire-in; the same
    // Arc is threaded into every CacheStepContext constructed for this
    // step (structured/retry variants + the standard dispatch ctx).
    //
    // W2b reorder: built BEFORE resolve_ir_model so the resolver can
    // read the Decision's OpenRouter model_list as the canonical source.
    let slot = step
        .model_requirements
        .tier
        .clone()
        .unwrap_or_else(|| "mid".to_string());
    let dispatch_decision = build_step_dispatch_decision(ctx, &slot).await;

    let resolved_model = resolve_ir_model(
        &step.model_requirements,
        &ctx.config,
        dispatch_decision.as_ref(),
    );
    let resolved_limit =
        resolve_ir_context_limit(&step.model_requirements, &ctx.config, &ctx.tier1);
    let max_tokens = resolve_ir_max_tokens(step, &ctx.tier1);
    let llm_options = resolve_ir_llm_call_options(step, &ctx.tier1);

    // W3c: legacy `clone_with_model_override` + `primary_context_limit`
    // override removed. The Decision attached to the IR StepContext
    // carries both slug and context_limit; `call_model_unified` reads
    // those directly.
    let config_ref = &ctx.config;

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
        dispatch_decision.as_ref(),
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
    dispatch_decision: Option<&Arc<DispatchDecision>>,
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
    // Walker v3 W1b: inherit the step's outer Decision (built ONCE per
    // step at the `dispatch_ir_llm` entry). Downstream consumers read
    // `step_ctx.dispatch_decision`; legacy consumers whose site hasn't
    // been migrated to W2 still see `config.primary_model / fallback_*`
    // on the surrounding config, so behavior is unchanged until W2.
    if let Some(d) = dispatch_decision {
        cache_ctx = cache_ctx.with_dispatch_decision(d.clone());
    }
    Some(cache_ctx)
}

/// Dispatch an IR mechanical step: look up `step.rust_function` in the registry.
///
/// Same registry as the legacy `dispatch_mechanical` but reads from IR types.
pub fn dispatch_ir_mechanical(
    step: &Step,
    resolved_input: &Value,
    ctx: &ChainDispatchContext,
) -> Result<Value> {
    let fn_name = step
        .rust_function
        .as_deref()
        .ok_or_else(|| anyhow!("IR mechanical step '{}' missing rust_function", step.id))?;
    info!("[IR] step '{}' → mechanical fn '{}'", step.id, fn_name);
    dispatch_mechanical(fn_name, resolved_input, ctx)
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

    #[test]
    fn test_dispatch_mechanical_unknown_fn() {
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
        };
        let result = dispatch_mechanical("nonexistent", &serde_json::json!({}), &ctx);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Unknown mechanical function"));
    }

    #[test]
    fn test_dispatch_mechanical_known_fn() {
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
        };
        let input = serde_json::json!({"files": ["main.rs"]});
        let result = dispatch_mechanical("extract_import_graph", &input, &ctx).unwrap();
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
        assert_eq!(
            resolve_model(&step, &defaults, &config, None),
            "custom/model"
        );
    }

    #[test]
    fn test_resolve_model_tier_mapping_aliases() {
        // W3c: legacy hardcoded `primary_model` / `fallback_model_{1,2}`
        // fallbacks were deleted. The resolver consults:
        //   1. `reqs.model` / step.model (direct override)
        //   2. Decision (unit test has none)
        //   3. provider_registry (unit test has none)
        //   4. `config.model_aliases` (we seed these for the test)
        //   5. `<unknown>` sentinel — tested in the sibling test below.
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
        let mut config = LlmConfig::default();
        config
            .model_aliases
            .insert("low".into(), "alias/low".into());
        config
            .model_aliases
            .insert("mid".into(), "alias/mid".into());
        config
            .model_aliases
            .insert("high".into(), "alias/high".into());
        config
            .model_aliases
            .insert("max".into(), "alias/max".into());

        assert_eq!(
            resolve_model(&make_step("low"), &defaults, &config, None),
            "alias/low"
        );
        assert_eq!(
            resolve_model(&make_step("mid"), &defaults, &config, None),
            "alias/mid"
        );
        assert_eq!(
            resolve_model(&make_step("high"), &defaults, &config, None),
            "alias/high"
        );
        assert_eq!(
            resolve_model(&make_step("max"), &defaults, &config, None),
            "alias/max"
        );
    }

    #[test]
    fn test_resolve_model_no_decision_no_alias_returns_unknown_sentinel() {
        // W3c: when neither Decision, registry, nor alias covers the tier,
        // the resolver stamps `<unknown>`. `call_model_unified` turns that
        // into a RouteSkipped at dispatch time.
        let step = ChainStep {
            name: "test".into(),
            primitive: "compress".into(),
            model_tier: Some("mid".into()),
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
            resolve_model(&step, &defaults, &config, None),
            "<unknown>"
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
        assert_eq!(resolve_ir_model(&reqs, &config, None), "custom/my-model");
    }

    #[test]
    fn test_resolve_ir_model_tier_mapping_aliases() {
        // W3c: aliases are the last escape hatch after Decision/registry.
        let mut config = LlmConfig::default();
        config
            .model_aliases
            .insert("low".into(), "alias/low".into());
        config
            .model_aliases
            .insert("mid".into(), "alias/mid".into());
        config
            .model_aliases
            .insert("high".into(), "alias/high".into());
        config
            .model_aliases
            .insert("max".into(), "alias/max".into());

        let make_reqs = |tier: &str| ModelRequirements {
            tier: Some(tier.into()),
            model: None,
            temperature: None,
        };

        assert_eq!(
            resolve_ir_model(&make_reqs("low"), &config, None),
            "alias/low"
        );
        assert_eq!(
            resolve_ir_model(&make_reqs("mid"), &config, None),
            "alias/mid"
        );
        assert_eq!(
            resolve_ir_model(&make_reqs("high"), &config, None),
            "alias/high"
        );
        assert_eq!(
            resolve_ir_model(&make_reqs("max"), &config, None),
            "alias/max"
        );
    }

    #[test]
    fn test_resolve_ir_model_default_tier_no_decision_returns_unknown() {
        // W3c: without Decision/registry/alias, default-tier "mid" resolves
        // to `<unknown>` — dispatch surfaces RouteSkipped.
        let reqs = ModelRequirements::default();
        let config = LlmConfig::default();
        assert_eq!(resolve_ir_model(&reqs, &config, None), "<unknown>");
    }

    #[test]
    fn test_resolve_ir_model_unknown_tier_returns_unknown() {
        // W3c: arbitrary tier without any resolver match → `<unknown>`.
        let reqs = ModelRequirements {
            tier: Some("ultra".into()),
            model: None,
            temperature: None,
        };
        let config = LlmConfig::default();
        assert_eq!(resolve_ir_model(&reqs, &config, None), "<unknown>");
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

    #[test]
    fn test_dispatch_ir_mechanical_routes_correctly() {
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
        };
        let mut step = ir_step("mech_step", StepOperation::Mechanical);
        step.rust_function = Some("extract_import_graph".into());
        let input = serde_json::json!({"files": ["lib.rs"]});
        let result = dispatch_ir_mechanical(&step, &input, &ctx).unwrap();
        assert_eq!(result["_mechanical"], "extract_import_graph");
        assert_eq!(result["_status"], "placeholder");
        assert_eq!(result["slug"], "ir-test");
    }

    #[test]
    fn test_dispatch_ir_mechanical_missing_fn_name() {
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
        };
        let step = ir_step("no_fn", StepOperation::Mechanical);
        // rust_function is None
        let result = dispatch_ir_mechanical(&step, &serde_json::json!({}), &ctx);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("missing rust_function"));
    }

    #[test]
    fn test_dispatch_ir_mechanical_unknown_fn() {
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
        };
        let mut step = ir_step("bad_fn", StepOperation::Mechanical);
        step.rust_function = Some("nonexistent_fn".into());
        let result = dispatch_ir_mechanical(&step, &serde_json::json!({}), &ctx);
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
        };
        let mut step = ir_step("mech", StepOperation::Mechanical);
        step.rust_function = Some("extract_mechanical_metadata".into());
        let (result, llm_resp) = dispatch_ir_step(&step, &serde_json::json!({}), "", &ctx)
            .await
            .unwrap();
        assert_eq!(result["_mechanical"], "extract_mechanical_metadata");
        assert!(llm_resp.is_none()); // mechanical steps don't produce LlmResponse
    }

    // ── Walker v3 W1b: outer-step DispatchDecision build + attach ───────
    //
    // `build_step_dispatch_decision` is the one place chain_executor wires
    // the step's DispatchDecision. These unit tests exercise the
    // happy-path (empty DB → SYSTEM_DEFAULTS Decision) and the
    // permissive-on-failure path (uninitialized DB → None, fall-through).

    fn make_w1b_seedable_ctx(conn: Connection) -> ChainDispatchContext {
        ChainDispatchContext {
            db_reader: Arc::new(Mutex::new(conn)),
            db_writer: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            slug: "w1b-test".into(),
            config: LlmConfig::default(),
            tier1: Tier1Config::default(),
            ops: OperationalConfig::default(),
            audit: None,
            cache_base: None,
            concurrency_cap: None,
        }
    }

    fn make_pyramid_config_contributions_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE pyramid_config_contributions (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 contribution_id TEXT NOT NULL UNIQUE,
                 slug TEXT,
                 schema_type TEXT NOT NULL,
                 yaml_content TEXT NOT NULL,
                 wire_native_metadata_json TEXT NOT NULL DEFAULT '{}',
                 wire_publication_state_json TEXT NOT NULL DEFAULT '{}',
                 supersedes_id TEXT,
                 superseded_by_id TEXT,
                 triggering_note TEXT,
                 status TEXT NOT NULL DEFAULT 'active',
                 source TEXT NOT NULL DEFAULT 'local',
                 wire_contribution_id TEXT,
                 created_by TEXT,
                 created_at TEXT NOT NULL DEFAULT (datetime('now')),
                 accepted_at TEXT
             );",
        )
        .unwrap();
        conn
    }

    #[tokio::test]
    async fn test_w1b_build_step_dispatch_decision_empty_db_returns_some_default() {
        test_capture::enable();
        test_capture::clear();
        let conn = make_pyramid_config_contributions_db();
        let ctx = make_w1b_seedable_ctx(conn);
        let d = build_step_dispatch_decision(&ctx, "mid").await;
        assert!(
            d.is_some(),
            "W1b: empty pyramid_config_contributions table means \
             SYSTEM_DEFAULTS — Decision must build successfully"
        );
        let decision = d.unwrap();
        assert_eq!(decision.slot, "mid");
        assert!(!decision.synthetic, "runtime path is not synthetic");
        // test_capture observed it
        let snap = test_capture::snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].slot, "mid");
        assert!(Arc::ptr_eq(&snap[0].decision, &decision));
    }

    #[tokio::test]
    async fn test_w1b_build_step_dispatch_decision_missing_table_is_permissive() {
        // No pyramid_config_contributions table exists. `build` must error
        // (DB read fails) and our helper must log + return None so the
        // legacy dispatch path continues.
        test_capture::enable();
        test_capture::clear();
        let conn = Connection::open_in_memory().unwrap();
        let ctx = make_w1b_seedable_ctx(conn);
        let d = build_step_dispatch_decision(&ctx, "mid").await;
        assert!(
            d.is_none(),
            "W1b: DB read failure must be permissive (None + log)"
        );
        // Capture hook must NOT have fired on a failed build.
        assert!(test_capture::snapshot().is_empty());
    }

    #[tokio::test]
    async fn test_w1b_decision_reflects_seeded_walker_provider_openrouter() {
        // Seed walker_provider_openrouter.overrides.model_list[mid] =
        // ["test-model-id"]. After build, per_provider[OpenRouter].model_list
        // must surface that value.
        test_capture::enable();
        test_capture::clear();
        let conn = make_pyramid_config_contributions_db();
        conn.execute(
            "INSERT INTO pyramid_config_contributions (
                 contribution_id, slug, schema_type, yaml_content, status, source
             ) VALUES (?1, NULL, 'walker_provider_openrouter', ?2, 'active', 'bundled')",
            rusqlite::params![
                "w1b-or-ml",
                r#"
schema_type: walker_provider_openrouter
version: 1
overrides:
  model_list:
    mid: ["test-model-id"]
"#
            ],
        )
        .unwrap();
        let ctx = make_w1b_seedable_ctx(conn);
        let d = build_step_dispatch_decision(&ctx, "mid").await;
        assert!(d.is_some());
        let decision = d.unwrap();
        use crate::pyramid::walker_resolver::ProviderType;
        let or = decision
            .per_provider
            .get(&ProviderType::OpenRouter)
            .expect("OpenRouter must be in effective call order");
        assert_eq!(
            or.model_list.as_deref(),
            Some(&["test-model-id".to_string()][..]),
            "W1b: seeded model_list must reach the Decision"
        );
    }
}
