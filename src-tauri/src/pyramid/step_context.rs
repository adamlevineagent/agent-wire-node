// pyramid/step_context.rs — Phase 6 unified step execution context and
// LLM output cache primitives.
//
// This module introduces the `StepContext` struct that the llm-output-cache
// spec canonically defines, plus the content-addressable cache types
// (`CachedStepOutput`, `CacheEntry`, `CacheHitResult`) and the hash helpers
// (`compute_cache_key`, `compute_inputs_hash`, `compute_prompt_hash`).
//
// `StepContext` is the single execution context threaded through every
// LLM-calling code path. It is the opt-in channel for:
//
//   * cache lookup + storage in `call_model_unified_with_options`
//   * event emission (cache hit / miss / verification failure)
//   * step metadata tracking (slug, build_id, step_name, depth, chunk_index)
//   * model resolution (tier → canonical id)
//   * force-fresh bypass (Phase 13 reroll)
//
// The name `StepContext` is reserved for this struct per the spec's
// "Threading the Cache Context" section. A pre-existing
// `chain_dispatch::StepContext` carries DB handles + live LlmConfig — the
// two types live side-by-side in the codebase and are distinguished at use
// sites via fully-qualified paths. They have different responsibilities.
//
// Phase 6 correctness gates:
//
//   1. `verify_cache_hit` is load-bearing. All four mismatch variants plus
//      corruption detection MUST be exact — a silent false-positive is a
//      silent correctness bug.
//   2. Cache lookup is OPT-IN. When no StepContext is provided (tests,
//      pre-init boot window), the LLM call path skips the cache entirely
//      and falls through to HTTP. This preserves backward compatibility.
//   3. Force-fresh bypass MUST still store the new entry with
//      `supersedes_cache_id` pointing at the prior row — Phase 13's reroll
//      path reads this chain for version history.

use std::sync::Arc;

use sha2::{Digest, Sha256};

use super::event_bus::BuildEventBus;
use super::llm::LlmConfig;

/// Compute a hex-encoded SHA-256 digest of the given bytes.
///
/// Every hash in the content-addressable cache goes through this function.
/// We use SHA-256 specifically (not `std::hash::Hash`) because `std::hash`
/// is NOT stable across Rust versions — a Hash-derived key would silently
/// invalidate the entire cache on a compiler upgrade.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest.iter() {
        use std::fmt::Write;
        let _ = write!(&mut out, "{:02x}", byte);
    }
    out
}

/// Compute the content-addressable cache key for an LLM call.
///
/// `cache_key = sha256(inputs_hash | prompt_hash | model_id)`
///
/// The three components are separated by a literal `|` delimiter so that
/// ambiguity between concatenated hashes and alphabet-like model IDs is
/// impossible (SHA-256 hex output never contains `|`).
pub fn compute_cache_key(inputs_hash: &str, prompt_hash: &str, model_id: &str) -> String {
    let composite = format!("{}|{}|{}", inputs_hash, prompt_hash, model_id);
    sha256_hex(composite.as_bytes())
}

/// Hash the concatenated, variable-substituted system + user prompts that
/// will be sent to the LLM. This captures the ACTUAL input content, not the
/// prompt template.
///
/// Changing the substituted input (e.g. a file edit that propagates into the
/// user prompt) changes `inputs_hash` and invalidates the cache entry,
/// exactly as the spec requires.
pub fn compute_inputs_hash(system_prompt: &str, user_prompt: &str) -> String {
    // The `\n---\n` separator prevents collision between two different
    // pairs that happen to concatenate to the same bytes (e.g. empty
    // system + "ab\n---\ncd" user vs "ab" system + "cd" user).
    let combined = format!("{}\n---\n{}", system_prompt, user_prompt);
    sha256_hex(combined.as_bytes())
}

/// Hash a prompt TEMPLATE file's content. Distinct from `compute_inputs_hash`
/// because the template describes HOW to ask, not what data is being asked
/// about. Editing the template changes `prompt_hash` (cache miss, correct);
/// editing the input data changes `inputs_hash` (cache miss, correct);
/// changing the routed model changes `model_id` (cache miss, correct).
///
/// The caller computes this hash from the prompt file content and caches
/// the result on `ChainContext.prompt_hashes`. This amortizes the hash cost
/// across every step in a single build that uses the same template.
pub fn compute_prompt_hash(template_body: &str) -> String {
    sha256_hex(template_body.as_bytes())
}

/// Result of verifying a cache hit against the current call's components.
/// All four mismatch variants exist so the caller can emit a specific
/// telemetry event and future debugging has a precise failure mode.
///
/// Do NOT collapse variants — the spec's "Cache Hit Verification" section
/// requires each mismatch to be distinguishable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheHitResult {
    /// All three components match and the `output_json` parses.
    Valid,
    /// `inputs_hash` stored in the row disagrees with the current inputs.
    /// This is the most likely mismatch — it means a cache_key collision
    /// snuck through the SHA-256 barrier (essentially never), or the row
    /// was written by a concurrent process with different inputs.
    MismatchInputs,
    /// `prompt_hash` stored in the row disagrees with the current prompt.
    /// Indicates a template file edited between writes under the same
    /// cache_key, or a concurrent-writer collision.
    MismatchPrompt,
    /// `model_id` stored in the row disagrees with the currently-routed
    /// model. Indicates tier routing changed between writes.
    MismatchModel,
    /// `output_json` stored in the row does not parse as valid JSON.
    /// Indicates storage corruption or partial write. Caller MUST delete
    /// the row and re-run the LLM call.
    CorruptedOutput,
}

/// Cache entry as stored in the `pyramid_step_cache` table. Includes every
/// field required by the retrieval + verification path.
///
/// Not `Eq` because `cost_usd` is an `f64`; use `PartialEq` for test
/// comparisons.
#[derive(Debug, Clone, PartialEq)]
pub struct CachedStepOutput {
    pub id: i64,
    pub slug: String,
    pub build_id: String,
    pub step_name: String,
    pub chunk_index: i64,
    pub depth: i64,
    pub cache_key: String,
    pub inputs_hash: String,
    pub prompt_hash: String,
    pub model_id: String,
    pub output_json: String,
    pub token_usage_json: Option<String>,
    pub cost_usd: Option<f64>,
    pub latency_ms: Option<i64>,
    pub created_at: String,
    pub force_fresh: bool,
    pub supersedes_cache_id: Option<i64>,
    /// Phase 13: user rationale attached to reroll writes.
    pub note: Option<String>,
    /// Phase 13: set by the downstream invalidation walker when a
    /// parent reroll orphans this entry. Non-null values are treated
    /// as a forced cache miss on subsequent lookups.
    pub invalidated_by: Option<String>,
}

/// New cache entry ready for insertion. Callers construct this from the
/// StepContext + the LLM response and hand it to `db::store_cache`.
#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub slug: String,
    pub build_id: String,
    pub step_name: String,
    pub chunk_index: i64,
    pub depth: i64,
    pub cache_key: String,
    pub inputs_hash: String,
    pub prompt_hash: String,
    pub model_id: String,
    pub output_json: String,
    pub token_usage_json: Option<String>,
    pub cost_usd: Option<f64>,
    pub latency_ms: Option<i64>,
    pub force_fresh: bool,
    pub supersedes_cache_id: Option<i64>,
    /// Phase 13: user rationale captured at reroll time. Non-reroll
    /// writes pass `None`.
    pub note: Option<String>,
}

/// Verify that a cache hit is safe to return to the caller. This is the
/// load-bearing correctness gate for the content-addressable cache.
///
/// Performs three equality checks against the stored row, then parses the
/// stored `output_json` to catch corruption. Each failure path returns a
/// distinct variant so the caller can emit precise telemetry and so a
/// future debugger can tell the modes apart.
///
/// The caller is expected to:
///   * emit `CacheHit` event when this returns `Valid`
///   * emit `CacheHitVerificationFailed { reason }` when this returns
///     anything else
///   * delete the stale row and fall through to HTTP on any non-Valid
///     result
pub fn verify_cache_hit(
    cached: &CachedStepOutput,
    current_inputs_hash: &str,
    current_prompt_hash: &str,
    current_model_id: &str,
) -> CacheHitResult {
    // Per the spec: check all three components individually, not just the
    // composite cache_key. A composite collision (vanishingly unlikely
    // under SHA-256 but not impossible) would be caught here.
    if cached.inputs_hash != current_inputs_hash {
        return CacheHitResult::MismatchInputs;
    }
    if cached.prompt_hash != current_prompt_hash {
        return CacheHitResult::MismatchPrompt;
    }
    if cached.model_id != current_model_id {
        return CacheHitResult::MismatchModel;
    }

    // Corruption detection: confirm `output_json` parses as JSON. We do
    // NOT validate any schema here — the cached content may have a
    // per-caller shape. A failed parse is treated as corruption and the
    // caller MUST delete the row.
    if serde_json::from_str::<serde_json::Value>(&cached.output_json).is_err() {
        return CacheHitResult::CorruptedOutput;
    }

    CacheHitResult::Valid
}

impl CacheHitResult {
    /// Short tag for telemetry / event payloads.
    pub fn reason_tag(&self) -> &'static str {
        match self {
            CacheHitResult::Valid => "valid",
            CacheHitResult::MismatchInputs => "mismatch_inputs",
            CacheHitResult::MismatchPrompt => "mismatch_prompt",
            CacheHitResult::MismatchModel => "mismatch_model",
            CacheHitResult::CorruptedOutput => "corrupted_output",
        }
    }
}

/// Execution context threaded through chain step handlers and LLM call
/// sites. Combines cache lookup/storage plumbing, event bus emission, and
/// step metadata into a single context.
///
/// Created at step dispatch time (chain_executor / stale_helpers_upper /
/// future retrofits) and passed down to `call_model_unified_with_options`
/// as an `Option<&StepContext>`. When `None` is passed (legacy call sites
/// or unit tests), the LLM path skips the cache entirely.
///
/// ## Field groups
///
/// - **Build metadata** (`slug`, `build_id`, `step_name`, `primitive`,
///   `depth`, `chunk_index`): used to locate a row in
///   `pyramid_step_cache` and for telemetry.
/// - **Cache plumbing** (`db_path`, `force_fresh`): the DB path lets the
///   call-site open a fresh connection (cache writes don't take the
///   writer mutex since the cache is content-addressable and INSERT OR
///   REPLACE on a unique key). `force_fresh` flips the bypass path.
/// - **Event bus** (`bus`): for emitting `CacheHit` /
///   `CacheHitVerificationFailed` events. Shared Arc so StepContext stays
///   cheap to clone.
/// - **Model resolution** (`model_tier`, `resolved_model_id`,
///   `resolved_provider_id`, `prompt_hash`): carry the resolved routing
///   information from the upper-layer build into the LLM call site, so
///   the cache key is computed consistently for the whole build.
///
/// ## Mandatory fields for cache lookup
///
/// Cache lookup requires `resolved_model_id` and `prompt_hash` to be set.
/// If either is empty, the cache path is skipped and the call goes
/// straight to HTTP (an explicit opt-out for call sites that can't yet
/// provide these — the Phase 12 retrofits will populate them).
#[derive(Clone)]
pub struct StepContext {
    // ── Build metadata ──────────────────────────────────────────────
    pub slug: String,
    pub build_id: String,
    pub step_name: String,
    pub primitive: String,
    pub depth: i64,
    pub chunk_index: Option<i64>,

    // ── Cache plumbing ──────────────────────────────────────────────
    pub db_path: String,
    pub force_fresh: bool,

    // ── Event emission ──────────────────────────────────────────────
    pub bus: Option<Arc<BuildEventBus>>,

    // ── Model resolution (populated by the executor) ────────────────
    pub model_tier: String,
    pub resolved_model_id: Option<String>,
    pub resolved_provider_id: Option<String>,

    // ── Prompt hash (populated by the executor from ChainContext) ───
    /// SHA-256 of the prompt template body. Empty string means the
    /// caller did not compute a prompt hash — cache lookup is skipped
    /// in that case (equivalent to a forced cache miss).
    pub prompt_hash: String,

    // ── Chronicle context (populated via .with_chain_context()) ─────
    /// Chain strategy name: "code-mechanical", "conversation-episodic", etc.
    /// Empty string when not in a chain build (stale checks, tests).
    pub chain_name: String,
    /// Content type: "code", "document", "conversation".
    /// Empty string when not in a chain build.
    pub content_type: String,
    /// Human-readable task label derived at construction time.
    /// Format: "{step_name} depth {depth} ({chain_name})" when chain_name is set,
    /// or "{step_name} depth {depth}" when chain_name is empty.
    pub task_label: String,

    // ── Per-build dedup for NETWORK_BALANCE_EXHAUSTED ──────────────
    /// Per-build dedup for the `network_balance_exhausted` chronicle
    /// event. Initialized fresh per StepContext. Thread-safe:
    /// `OnceLock::set()` is atomic via stdlib, so concurrent race
    /// callers get exactly one Ok; later callers see Err and skip emit.
    ///
    /// Using `std::sync::OnceLock` (stable Rust 1.70+) — no extra
    /// crate dependency required.
    ///
    /// Leak on build crash: when the StepContext drops, the OnceLock
    /// drops with it. Memory-only, not persistent state.
    pub balance_exhausted_emitted: std::sync::OnceLock<()>,
}

impl std::fmt::Debug for StepContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StepContext")
            .field("slug", &self.slug)
            .field("build_id", &self.build_id)
            .field("step_name", &self.step_name)
            .field("primitive", &self.primitive)
            .field("depth", &self.depth)
            .field("chunk_index", &self.chunk_index)
            .field("db_path", &self.db_path)
            .field("force_fresh", &self.force_fresh)
            .field("bus", &self.bus.as_ref().map(|_| "<bus>"))
            .field("model_tier", &self.model_tier)
            .field("resolved_model_id", &self.resolved_model_id)
            .field("resolved_provider_id", &self.resolved_provider_id)
            .field("prompt_hash", &self.prompt_hash)
            .field("chain_name", &self.chain_name)
            .field("content_type", &self.content_type)
            .field("task_label", &self.task_label)
            .field("balance_exhausted_emitted", &"<oncelock>")
            .finish()
    }
}

impl StepContext {
    /// Construct a cache-capable StepContext directly from the pieces the
    /// retrofit sites have in scope. Callers that have a `ChainContext`
    /// should use `from_chain_context` instead (once the ChainContext
    /// carries the resolved_models + prompt_hashes caches).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        slug: impl Into<String>,
        build_id: impl Into<String>,
        step_name: impl Into<String>,
        primitive: impl Into<String>,
        depth: i64,
        chunk_index: Option<i64>,
        db_path: impl Into<String>,
    ) -> Self {
        Self {
            slug: slug.into(),
            build_id: build_id.into(),
            step_name: step_name.into(),
            primitive: primitive.into(),
            depth,
            chunk_index,
            db_path: db_path.into(),
            force_fresh: false,
            bus: None,
            model_tier: String::new(),
            resolved_model_id: None,
            resolved_provider_id: None,
            prompt_hash: String::new(),
            chain_name: String::new(),
            content_type: String::new(),
            task_label: String::new(),
            balance_exhausted_emitted: std::sync::OnceLock::new(),
        }
    }

    /// Set the model tier name + resolved model id (builder-style). The
    /// resolved id goes into the cache key.
    pub fn with_model_resolution(
        mut self,
        tier: impl Into<String>,
        resolved_model_id: impl Into<String>,
    ) -> Self {
        self.model_tier = tier.into();
        self.resolved_model_id = Some(resolved_model_id.into());
        self
    }

    /// Set the resolved provider id (for telemetry / tracing).
    pub fn with_provider(mut self, provider_id: impl Into<String>) -> Self {
        self.resolved_provider_id = Some(provider_id.into());
        self
    }

    /// Attach the prompt template hash computed upstream (typically via
    /// `ChainContext.prompt_hashes`).
    pub fn with_prompt_hash(mut self, hash: impl Into<String>) -> Self {
        self.prompt_hash = hash.into();
        self
    }

    /// Attach the event bus for cache-related emissions.
    pub fn with_bus(mut self, bus: Arc<BuildEventBus>) -> Self {
        self.bus = Some(bus);
        self
    }

    /// Flip to force-fresh (reroll bypass path).
    pub fn with_force_fresh(mut self, force: bool) -> Self {
        self.force_fresh = force;
        self
    }

    /// Set chain context and derive task_label mechanically.
    /// Only call sites with ChainContext in scope (chain_dispatch, evidence_answering)
    /// use this builder. All other sites get empty defaults via StepContext::new().
    pub fn with_chain_context(
        mut self,
        chain_name: impl Into<String>,
        content_type: impl Into<String>,
    ) -> Self {
        self.chain_name = chain_name.into();
        self.content_type = content_type.into();
        self.task_label = derive_task_label(&self.step_name, self.depth, &self.chain_name);
        self
    }

    /// Return true if this context carries enough information to perform a
    /// cache lookup (resolved model id + prompt hash present).
    pub fn cache_is_usable(&self) -> bool {
        self.resolved_model_id
            .as_ref()
            .map(|m| !m.is_empty())
            .unwrap_or(false)
            && !self.prompt_hash.is_empty()
    }
}

/// Phase 12 convenience constructor for retrofit call sites that have
/// a slug + some notion of step identity but no upstream ChainContext.
///
/// Computes the prompt hash on-the-fly from `prompt_template_body` (or
/// from the concatenated system+user prompt if no template is
/// available), stamps the resolved model id, and attaches the bus.
///
/// `build_id` is used only for telemetry/provenance on the cache row —
/// the cache KEY is content-addressable (inputs_hash + prompt_hash +
/// model_id) so cache hit/miss behavior is the same across build_ids.
/// For call sites without a real build (e.g. DADBEAR maintenance),
/// pass `None` and a synthetic id like `<slug>-maintenance-<op>` is
/// generated.
#[allow(clippy::too_many_arguments)]
pub fn make_step_context_from_slug(
    slug: &str,
    build_id: Option<&str>,
    step_name: &str,
    primitive: &str,
    depth: i64,
    chunk_index: Option<i64>,
    db_path: &str,
    bus: Option<Arc<BuildEventBus>>,
    model_tier: &str,
    resolved_model_id: &str,
    prompt_template_body: Option<&str>,
    fallback_system_prompt: Option<&str>,
    fallback_user_prompt: Option<&str>,
) -> StepContext {
    let build_id = build_id
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{}-{}", slug, step_name));
    let prompt_hash = match prompt_template_body {
        Some(body) if !body.is_empty() => compute_prompt_hash(body),
        _ => {
            // Derive a stable prompt_hash from the fallback prompts if
            // present; otherwise leave empty (the call will bypass the
            // cache via `cache_is_usable`). This is intentional — a site
            // with no stable prompt body can't participate in the
            // cache.
            match (fallback_system_prompt, fallback_user_prompt) {
                (Some(sys), Some(_)) if !sys.is_empty() => {
                    // Hash the system prompt alone as a proxy for the
                    // template. Not ideal — inputs will double-count —
                    // but deterministic for the same system prompt.
                    compute_prompt_hash(sys)
                }
                _ => String::new(),
            }
        }
    };

    let mut ctx = StepContext::new(
        slug.to_string(),
        build_id,
        step_name.to_string(),
        primitive.to_string(),
        depth,
        chunk_index,
        db_path.to_string(),
    );
    if !resolved_model_id.is_empty() {
        ctx = ctx.with_model_resolution(model_tier.to_string(), resolved_model_id.to_string());
    }
    if !prompt_hash.is_empty() {
        ctx = ctx.with_prompt_hash(prompt_hash);
    }
    if let Some(bus) = bus {
        ctx = ctx.with_bus(bus);
    }
    ctx
}

/// Phase 12 retrofit helper: construct a cache-usable StepContext
/// from an LlmConfig that carries cache plumbing (see
/// `LlmConfig::cache_access`) plus a step identity and the system
/// prompt body. Returns `None` if the config has no cache_access
/// (the caller then bypasses the cache naturally).
///
/// The `resolved_model_id` is taken from `config.primary_model` —
/// this is the simplest consistent choice since every retrofit site
/// that calls `call_model` implicitly uses whatever model the config
/// routes to. For sites that use `call_model_unified_with_options_and_ctx`
/// with an explicit tier, the direct construction path
/// (`StepContext::new` + `with_model_resolution`) remains available.
pub fn make_step_ctx_from_llm_config(
    config: &LlmConfig,
    step_name: &str,
    primitive: &str,
    depth: i64,
    chunk_index: Option<i64>,
    system_prompt: &str,
) -> Option<StepContext> {
    let cache = config.cache_access.as_ref()?;
    if system_prompt.is_empty() {
        return None;
    }
    let prompt_hash = compute_prompt_hash(system_prompt);
    let mut ctx = StepContext::new(
        cache.slug.clone(),
        cache.build_id.clone(),
        step_name.to_string(),
        primitive.to_string(),
        depth,
        chunk_index,
        cache.db_path.to_string(),
    )
    .with_model_resolution("primary", config.primary_model.clone())
    .with_prompt_hash(prompt_hash);
    if let Some(bus) = &cache.bus {
        ctx = ctx.with_bus(bus.clone());
    }
    // Populate chain context from CacheAccess when present.
    if let Some(ref cn) = cache.chain_name {
        let ct = cache.content_type.as_deref().unwrap_or("");
        ctx = ctx.with_chain_context(cn.clone(), ct.to_string());
    }
    Some(ctx)
}

/// Derive a human-readable task label from step context fields.
/// Conditional formatting: if chain_name is non-empty, include it in
/// parentheses. If empty (stale checks, tests), omit to avoid garbled
/// empty parenthetical like "stale_check depth 2 ()".
pub fn derive_task_label(step_name: &str, depth: i64, chain_name: &str) -> String {
    if chain_name.is_empty() {
        format!("{} depth {}", step_name, depth)
    } else {
        format!("{} depth {} ({})", step_name, depth, chain_name)
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Hash helpers ─────────────────────────────────────────────────

    #[test]
    fn test_sha256_hex_is_deterministic_and_lowercase() {
        let a = sha256_hex(b"hello world");
        let b = sha256_hex(b"hello world");
        assert_eq!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(a.len(), 64);
        assert_eq!(a, a.to_lowercase());
    }

    #[test]
    fn test_compute_cache_key_stable_across_runs() {
        let key1 = compute_cache_key("aaa", "bbb", "ccc");
        let key2 = compute_cache_key("aaa", "bbb", "ccc");
        assert_eq!(key1, key2);
        // Non-trivial SHA-256 output, not a pass-through of the input.
        assert_ne!(key1, "aaa|bbb|ccc");
        assert_eq!(key1.len(), 64);
    }

    #[test]
    fn test_compute_cache_key_changes_on_each_component() {
        let base = compute_cache_key("aaa", "bbb", "ccc");
        assert_ne!(base, compute_cache_key("aax", "bbb", "ccc"));
        assert_ne!(base, compute_cache_key("aaa", "bbx", "ccc"));
        assert_ne!(base, compute_cache_key("aaa", "bbb", "ccx"));
    }

    #[test]
    fn test_compute_inputs_hash_separator_prevents_collision() {
        // Two pairs that would concatenate to identical bytes without a
        // separator must produce different hashes.
        let h1 = compute_inputs_hash("ab", "cd");
        let h2 = compute_inputs_hash("a", "bcd");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_compute_prompt_hash_stable() {
        let h1 = compute_prompt_hash("system prompt template body");
        let h2 = compute_prompt_hash("system prompt template body");
        assert_eq!(h1, h2);
        assert_ne!(h1, compute_prompt_hash("system prompt template body!"));
    }

    // ── Cache hit verification ──────────────────────────────────────

    fn make_cached(inputs: &str, prompt: &str, model: &str, output: &str) -> CachedStepOutput {
        CachedStepOutput {
            id: 1,
            slug: "test-slug".into(),
            build_id: "build-1".into(),
            step_name: "step-a".into(),
            chunk_index: -1,
            depth: 0,
            cache_key: compute_cache_key(inputs, prompt, model),
            inputs_hash: inputs.into(),
            prompt_hash: prompt.into(),
            model_id: model.into(),
            output_json: output.into(),
            token_usage_json: None,
            cost_usd: None,
            latency_ms: None,
            created_at: "2026-04-10 00:00:00".into(),
            force_fresh: false,
            supersedes_cache_id: None,
            note: None,
            invalidated_by: None,
        }
    }

    #[test]
    fn test_verify_cache_hit_valid() {
        let cached = make_cached("i1", "p1", "m1", "{\"ok\":true}");
        assert_eq!(
            verify_cache_hit(&cached, "i1", "p1", "m1"),
            CacheHitResult::Valid
        );
    }

    #[test]
    fn test_verify_cache_hit_mismatch_inputs() {
        let cached = make_cached("i1", "p1", "m1", "{\"ok\":true}");
        assert_eq!(
            verify_cache_hit(&cached, "iX", "p1", "m1"),
            CacheHitResult::MismatchInputs
        );
    }

    #[test]
    fn test_verify_cache_hit_mismatch_prompt() {
        let cached = make_cached("i1", "p1", "m1", "{\"ok\":true}");
        assert_eq!(
            verify_cache_hit(&cached, "i1", "pX", "m1"),
            CacheHitResult::MismatchPrompt
        );
    }

    #[test]
    fn test_verify_cache_hit_mismatch_model() {
        let cached = make_cached("i1", "p1", "m1", "{\"ok\":true}");
        assert_eq!(
            verify_cache_hit(&cached, "i1", "p1", "mX"),
            CacheHitResult::MismatchModel
        );
    }

    #[test]
    fn test_verify_cache_hit_corrupted_output() {
        let cached = make_cached("i1", "p1", "m1", "not-json{{");
        assert_eq!(
            verify_cache_hit(&cached, "i1", "p1", "m1"),
            CacheHitResult::CorruptedOutput
        );
    }

    #[test]
    fn test_verify_cache_hit_mismatch_beats_corruption() {
        // If the inputs mismatch AND the output is corrupted, the mismatch
        // variant wins — mismatch tells the caller which component drifted;
        // corruption is the catch-all. Document the ordering explicitly so
        // future refactors don't accidentally swap them.
        let cached = make_cached("i1", "p1", "m1", "not-json");
        assert_eq!(
            verify_cache_hit(&cached, "iX", "p1", "m1"),
            CacheHitResult::MismatchInputs
        );
    }

    #[test]
    fn test_reason_tags() {
        assert_eq!(CacheHitResult::Valid.reason_tag(), "valid");
        assert_eq!(CacheHitResult::MismatchInputs.reason_tag(), "mismatch_inputs");
        assert_eq!(CacheHitResult::MismatchPrompt.reason_tag(), "mismatch_prompt");
        assert_eq!(CacheHitResult::MismatchModel.reason_tag(), "mismatch_model");
        assert_eq!(CacheHitResult::CorruptedOutput.reason_tag(), "corrupted_output");
    }

    // ── StepContext construction ─────────────────────────────────────

    #[test]
    fn test_step_context_new_and_builder() {
        let ctx = StepContext::new(
            "slug-a",
            "build-1",
            "step_a",
            "extract",
            0,
            Some(3),
            "/tmp/pyramid.db",
        )
        .with_model_resolution("fast_extract", "inception/mercury-2")
        .with_provider("openrouter")
        .with_prompt_hash("abc123")
        .with_force_fresh(false);

        assert_eq!(ctx.slug, "slug-a");
        assert_eq!(ctx.build_id, "build-1");
        assert_eq!(ctx.step_name, "step_a");
        assert_eq!(ctx.primitive, "extract");
        assert_eq!(ctx.depth, 0);
        assert_eq!(ctx.chunk_index, Some(3));
        assert_eq!(ctx.db_path, "/tmp/pyramid.db");
        assert!(!ctx.force_fresh);
        assert_eq!(ctx.model_tier, "fast_extract");
        assert_eq!(ctx.resolved_model_id.as_deref(), Some("inception/mercury-2"));
        assert_eq!(ctx.resolved_provider_id.as_deref(), Some("openrouter"));
        assert_eq!(ctx.prompt_hash, "abc123");
        assert!(ctx.bus.is_none());
    }

    #[test]
    fn test_step_context_cache_is_usable_requires_model_and_prompt() {
        let mut ctx = StepContext::new("s", "b", "n", "p", 0, None, "/db");
        assert!(!ctx.cache_is_usable(), "fresh context without model/prompt");

        ctx.resolved_model_id = Some("m1".into());
        assert!(!ctx.cache_is_usable(), "model set but prompt empty");

        ctx.prompt_hash = "phash".into();
        assert!(ctx.cache_is_usable(), "both fields set");

        ctx.resolved_model_id = Some(String::new());
        assert!(!ctx.cache_is_usable(), "empty model string");
    }

    #[test]
    fn test_step_context_force_fresh_toggle() {
        let ctx = StepContext::new("s", "b", "n", "p", 0, None, "/db").with_force_fresh(true);
        assert!(ctx.force_fresh);
    }
}
