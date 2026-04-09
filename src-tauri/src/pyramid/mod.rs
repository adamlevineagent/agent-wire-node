// pyramid/ — Knowledge Pyramid engine
//
// Modules:
//   db      — SQLite schema, migrations, CRUD operations
//   types   — Data model structs (PyramidNode, Slug, Topic, etc.)
//   ingest  — Content ingestion (conversation, code, document)
//   build   — LLM-powered build pipeline (3 variants)
//   query   — Query functions (apex, search, drill, entities, resolved)
//   llm     — OpenRouter API client with 3-tier model cascade
//   slug    — Slug/namespace management
//   routes  — Warp HTTP route handlers

pub mod build;
pub mod build_runner;
pub mod chain_dispatch;
pub mod chain_engine;
pub mod chain_executor;
pub mod chain_loader;
pub mod chain_proposal;
pub mod chain_publish;
pub mod chain_registry;
pub mod chain_resolve;
pub mod characterize;
pub mod config_helper;
pub mod converge_expand;
pub mod cost_model;
pub mod crystallization;
pub mod dadbear_extend;
pub mod db;
pub mod defaults_adapter;
pub mod demand_gen;
pub mod delta;
pub mod event_bus;
pub mod event_chain;
pub mod public_html;
pub mod question_build;
pub mod evidence_answering;
pub mod execution_plan;
pub mod execution_state;
pub mod expression;
pub mod extraction_schema;
pub mod faq;
pub mod ingest;
pub mod llm;
pub mod local_store;
pub mod lock_manager;
pub mod manifest;
pub mod meta;
pub mod naming;
pub mod parity;
pub mod primer;
pub mod publication;
pub mod query;
pub mod question_compiler;
pub mod question_decomposition;
pub mod question_loader;
pub mod question_yaml;
pub mod reconciliation;
pub mod recovery;
pub mod routes;
pub mod slug;
pub mod stale_engine;
pub mod stale_helpers;
pub mod stale_helpers_upper;
pub mod staleness;
pub mod staleness_bridge;
pub mod supersession;
pub mod sync;
pub mod transform_runtime;
pub mod types;
pub mod vine;
pub mod vine_composition;
pub mod vine_prompts;
pub mod vocabulary;
pub mod watcher;
pub mod webbing;
pub mod wire_import;
pub mod wire_publish;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::Mutex;

use self::event_chain::LocalEventBus;
use self::llm::LlmConfig;
use self::stale_engine::PyramidStaleEngine;
use self::types::BuildStatus;
use self::watcher::PyramidFileWatcher;

/// Persistent pyramid configuration stored in `pyramid_config.json`.
///
/// Location: `~/Library/Application Support/wire-node/pyramid_config.json`
///
/// Key fields:
/// - `auth_token`: Bearer token required for ALL HTTP API calls.
///   Set via the desktop app Settings → API Key, or manually in the JSON file.
///   All requests must include header: `Authorization: Bearer <auth_token>`
/// - `openrouter_api_key`: API key for LLM calls via OpenRouter.
/// - `primary_model`: Default LLM model (default: `inception/mercury-2`).
/// - `use_ir_executor`: Enable the IR-based chain executor (default: false).
///   Toggle at runtime via: `POST /pyramid/config` with `{"use_ir_executor": true}`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PyramidConfig {
    #[serde(default)]
    pub openrouter_api_key: String,
    /// Bearer token for HTTP API auth. Required for all API calls.
    /// Set in this config file or via the desktop app Settings → API Key.
    #[serde(default)]
    pub auth_token: String,
    #[serde(default = "default_primary_model")]
    pub primary_model: String,
    #[serde(default = "default_fallback_1")]
    pub fallback_model_1: String,
    #[serde(default = "default_fallback_2")]
    pub fallback_model_2: String,
    #[serde(default = "default_partner_model")]
    pub partner_model: String,
    #[serde(default = "default_collapse_model")]
    pub collapse_model: String,
    #[serde(default)]
    pub use_chain_engine: bool,
    #[serde(default)]
    pub use_ir_executor: bool,
    /// Operational constants organized by tier. All fields have sensible defaults
    /// matching the original hardcoded values, so existing configs are backward compatible.
    #[serde(default)]
    pub operational: OperationalConfig,
    /// WS-ONLINE-G: Absorption rate limiting for absorb-all mode.
    /// Max builds per hour per external operator (default: 3).
    #[serde(default = "default_absorption_rate_limit")]
    pub absorption_rate_limit_per_operator: u32,
    /// WS-ONLINE-G: Daily spend cap for absorb-all builds in credits (default: 100).
    #[serde(default = "default_absorption_daily_cap")]
    pub absorption_daily_spend_cap: u64,
    /// Sprint 4: Auto-execute toggle. When ON, safe plans (navigation, read-only)
    /// execute immediately after planning without showing a preview.
    /// Effectful plans (builds, writes, costs) always show preview regardless.
    #[serde(default)]
    pub auto_execute: bool,
    /// Custom semantic aliases mapping an arbitrary `model_tier` string to a model.
    #[serde(default)]
    pub model_aliases: HashMap<String, String>,
}

fn default_primary_model() -> String {
    "inception/mercury-2".into()
}
fn default_fallback_1() -> String {
    "qwen/qwen3.5-flash-02-23".into()
}
fn default_fallback_2() -> String {
    "x-ai/grok-4.20-beta".into()
}
fn default_partner_model() -> String {
    "xiaomi/mimo-v2-pro".into()
}
fn default_collapse_model() -> String {
    "x-ai/grok-4.20-beta".into()
}
fn default_absorption_rate_limit() -> u32 {
    3
}
fn default_absorption_daily_cap() -> u64 {
    100
}

// ── Tier1 default functions (everything-to-YAML: Part 2) ──────────────────

fn default_llm_retryable_status_codes() -> Vec<u16> {
    vec![429, 403, 502, 503]
}
fn default_llm_retry_base_sleep_secs() -> u64 {
    1
}
fn default_llm_timeout_chars_per_increment() -> usize {
    100_000
}
fn default_llm_timeout_increment_secs() -> u64 {
    60
}
fn default_llm_rate_limit_max_requests() -> usize {
    20
}
fn default_llm_rate_limit_window_secs() -> f64 {
    5.0
}

// ── Tier2 default functions (everything-to-YAML: Part 3) ──────────────────

fn default_watcher_exclude_patterns() -> Vec<String> {
    vec![
        "/target/".into(),
        "/node_modules/".into(),
        "/.git/".into(),
        "/dist/".into(),
        "/.next/".into(),
        "/.DS_Store".into(),
        ".tmp.".into(),
        ".swp".into(),
        ".swo".into(),
        "~".into(),
        "/build/".into(),
    ]
}
fn default_rename_similarity_threshold() -> f64 {
    0.5
}
fn default_rename_candidate_window_ms() -> u64 {
    2000
}
fn default_staleness_queue_dequeue_cap() -> usize {
    50
}
fn default_phase_display_duration_secs() -> u64 {
    10
}
fn default_rate_limit_hourly_window_secs() -> u64 {
    3600
}
fn default_rate_limit_daily_window_secs() -> u64 {
    86400
}
fn default_gap_resolution_max_files() -> usize {
    5
}

// ── Operational Config (Tiered) ─────────────────────────────────────────────
//
// All operational constants externalized from Rust source. Organized into tiers
// so operators know the blast radius of changes:
//   Tier 1 (Operator): model selection, concurrency, temperature, max_tokens, retries, pricing
//   Tier 2 (Tunable): staleness threshold, token budgets, timeouts, chunking, headline limits
//   Tier 3 (Expert): delta collapse, webbing, supersession, staleness propagation, stale batching

/// Tier 1 — Operator-level config. Safe to change for different workloads.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Tier1Config {
    // Context limits
    pub primary_context_limit: usize,
    pub fallback_1_context_limit: usize,
    pub high_tier_context_limit: usize,
    pub max_tier_context_limit: usize,
    // Concurrency
    pub answer_concurrency: usize,
    pub stale_max_concurrent_helpers: usize,
    // max_tokens
    pub decomposition_max_tokens: usize,
    pub characterize_max_tokens: usize,
    pub extraction_schema_max_tokens: usize,
    pub synthesis_prompts_max_tokens: usize,
    pub pre_map_max_tokens: usize,
    pub answer_max_tokens: usize,
    pub ir_max_tokens: usize,
    // Temperature
    pub decomposition_temperature: f32,
    pub characterize_temperature: f32,
    pub extraction_schema_temperature: f32,
    pub pre_map_temperature: f32,
    pub answer_temperature: f32,
    pub default_ir_temperature: f32,
    // Retries
    pub llm_max_retries: u32,
    // Pricing (per-million tokens)
    pub default_input_price_per_million: f64,
    pub default_output_price_per_million: f64,
    // Timeouts (structured response minimum timeouts in seconds)
    pub classify_min_timeout_secs: u64,
    pub web_min_timeout_secs: u64,
    pub default_structured_min_timeout_secs: u64,
    // LLM retry/timeout tuning (everything-to-YAML: Part 2)
    #[serde(default = "default_llm_retryable_status_codes")]
    pub llm_retryable_status_codes: Vec<u16>,
    #[serde(default = "default_llm_retry_base_sleep_secs")]
    pub llm_retry_base_sleep_secs: u64,
    #[serde(default = "default_llm_timeout_chars_per_increment")]
    pub llm_timeout_chars_per_increment: usize,
    #[serde(default = "default_llm_timeout_increment_secs")]
    pub llm_timeout_increment_secs: u64,
    /// Max LLM requests per sliding window (rate limiter).
    #[serde(default = "default_llm_rate_limit_max_requests")]
    pub llm_rate_limit_max_requests: usize,
    /// Sliding window duration in seconds for rate limiting.
    #[serde(default = "default_llm_rate_limit_window_secs")]
    pub llm_rate_limit_window_secs: f64,
    /// When true, log full LLM response bodies for failed/truncated calls.
    #[serde(default)]
    pub llm_debug_logging: bool,
}

impl Default for Tier1Config {
    fn default() -> Self {
        Self {
            primary_context_limit: 120_000,
            fallback_1_context_limit: 900_000,
            high_tier_context_limit: 1_000_000,
            max_tier_context_limit: 2_000_000,
            answer_concurrency: 5,
            stale_max_concurrent_helpers: 3,
            decomposition_max_tokens: 4096,
            characterize_max_tokens: 2048,
            extraction_schema_max_tokens: 4096,
            synthesis_prompts_max_tokens: 2048,
            pre_map_max_tokens: 4096,
            answer_max_tokens: 4096,
            ir_max_tokens: 100_000,
            decomposition_temperature: 0.3,
            characterize_temperature: 0.3,
            extraction_schema_temperature: 0.3,
            pre_map_temperature: 0.2,
            answer_temperature: 0.3,
            default_ir_temperature: 0.3,
            llm_max_retries: 5,
            default_input_price_per_million: 0.19,
            default_output_price_per_million: 0.75,
            classify_min_timeout_secs: 420,
            web_min_timeout_secs: 240,
            default_structured_min_timeout_secs: 180,
            llm_retryable_status_codes: default_llm_retryable_status_codes(),
            llm_retry_base_sleep_secs: default_llm_retry_base_sleep_secs(),
            llm_timeout_chars_per_increment: default_llm_timeout_chars_per_increment(),
            llm_timeout_increment_secs: default_llm_timeout_increment_secs(),
            llm_rate_limit_max_requests: default_llm_rate_limit_max_requests(),
            llm_rate_limit_window_secs: default_llm_rate_limit_window_secs(),
            llm_debug_logging: false,
        }
    }
}

/// Tier 2 — Tunable config. Affects quality/performance tradeoffs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Tier2Config {
    pub staleness_threshold: f64,
    pub l0_summary_budget: usize,
    pub pre_map_prompt_budget: usize,
    /// Max nodes per pre-mapping batch (None = no item limit, only token budget).
    #[serde(default)]
    pub pre_map_max_batch_nodes: Option<usize>,
    pub ir_thread_input_char_budget: usize,
    pub distillation_token_budget: usize,
    pub distillation_early_collapse: usize,
    pub llm_base_timeout_secs: u64,
    pub llm_max_timeout_secs: u64,
    pub chunk_target_lines: usize,
    pub max_headline_chars: usize,
    pub max_headline_words: usize,
    pub teaser_max_chars: usize,
    pub granularity_ranges: Vec<(u32, u32)>,
    pub faq_category_threshold: usize,
    // Watcher / stale / rate-limit tuning (everything-to-YAML: Part 3+5)
    #[serde(default = "default_watcher_exclude_patterns")]
    pub watcher_exclude_patterns: Vec<String>,
    #[serde(default = "default_rename_similarity_threshold")]
    pub rename_similarity_threshold: f64,
    #[serde(default = "default_rename_candidate_window_ms")]
    pub rename_candidate_window_ms: u64,
    #[serde(default = "default_staleness_queue_dequeue_cap")]
    pub staleness_queue_dequeue_cap: usize,
    #[serde(default = "default_phase_display_duration_secs")]
    pub phase_display_duration_secs: u64,
    #[serde(default = "default_rate_limit_hourly_window_secs")]
    pub rate_limit_hourly_window_secs: u64,
    #[serde(default = "default_rate_limit_daily_window_secs")]
    pub rate_limit_daily_window_secs: u64,
    #[serde(default = "default_gap_resolution_max_files")]
    pub gap_resolution_max_files: usize,
    /// Token budget for evidence context in answer_single_question.
    /// When candidates exceed this, batching + dehydration kicks in.
    #[serde(default = "default_answer_prompt_budget")]
    pub answer_prompt_budget: usize,
}

fn default_answer_prompt_budget() -> usize {
    100_000
}

impl Default for Tier2Config {
    fn default() -> Self {
        Self {
            staleness_threshold: 0.3,
            l0_summary_budget: 100_000,
            pre_map_prompt_budget: 80_000,
            pre_map_max_batch_nodes: None,
            ir_thread_input_char_budget: 90_000,
            distillation_token_budget: 800,
            distillation_early_collapse: 1200,
            llm_base_timeout_secs: 120,
            llm_max_timeout_secs: 600,
            chunk_target_lines: 100,
            max_headline_chars: usize::MAX,
            max_headline_words: usize::MAX,
            teaser_max_chars: 200,
            // Index = granularity (1-5), value = (min, max) hint range
            // Index 0 = default fallback
            granularity_ranges: vec![
                (3, 4), // granularity 0 / default
                (2, 3), // granularity 1
                (3, 4), // granularity 2
                (3, 4), // granularity 3
                (4, 5), // granularity 4
                (5, 6), // granularity 5
            ],
            faq_category_threshold: 20,
            watcher_exclude_patterns: default_watcher_exclude_patterns(),
            rename_similarity_threshold: default_rename_similarity_threshold(),
            rename_candidate_window_ms: default_rename_candidate_window_ms(),
            staleness_queue_dequeue_cap: default_staleness_queue_dequeue_cap(),
            phase_display_duration_secs: default_phase_display_duration_secs(),
            rate_limit_hourly_window_secs: default_rate_limit_hourly_window_secs(),
            rate_limit_daily_window_secs: default_rate_limit_daily_window_secs(),
            gap_resolution_max_files: default_gap_resolution_max_files(),
            answer_prompt_budget: default_answer_prompt_budget(),
        }
    }
}

/// Tier 3 — Expert config. Affects crystallization, webbing, supersession internals.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Tier3Config {
    // Delta / collapse
    pub collapse_threshold: i64,
    pub max_propagation_depth: i64,
    pub self_check_window: i64,
    // Webbing
    pub web_edge_collapse_threshold: i64,
    pub max_edges_per_thread: usize,
    pub edge_decay_rate: f64,
    pub edge_min_relevance: f64,
    // Supersession
    pub contradiction_confidence_threshold: f64,
    pub supersession_priority: f64,
    pub max_trace_depth: i64,
    // Staleness propagation
    pub staleness_max_propagation_depth: i64,
    pub staleness_debounce_secs: u64,
    // Stale batching
    pub batch_cap_nodes: usize,
    pub batch_cap_connections: usize,
    pub batch_cap_renames: usize,
}

impl Default for Tier3Config {
    fn default() -> Self {
        Self {
            collapse_threshold: 50,
            max_propagation_depth: 10,
            self_check_window: 5,
            web_edge_collapse_threshold: 20,
            max_edges_per_thread: 10,
            edge_decay_rate: 0.05,
            edge_min_relevance: 0.1,
            contradiction_confidence_threshold: 0.8,
            supersession_priority: 1.0,
            max_trace_depth: 50,
            staleness_max_propagation_depth: 20,
            staleness_debounce_secs: 10,
            batch_cap_nodes: 5,
            batch_cap_connections: 20,
            batch_cap_renames: 1,
        }
    }
}

/// All operational constants, organized by tier.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OperationalConfig {
    pub tier1: Tier1Config,
    pub tier2: Tier2Config,
    pub tier3: Tier3Config,
}

impl Default for OperationalConfig {
    fn default() -> Self {
        Self {
            tier1: Tier1Config::default(),
            tier2: Tier2Config::default(),
            tier3: Tier3Config::default(),
        }
    }
}

impl Default for PyramidConfig {
    fn default() -> Self {
        Self {
            openrouter_api_key: String::new(),
            auth_token: String::new(),
            primary_model: default_primary_model(),
            fallback_model_1: default_fallback_1(),
            fallback_model_2: default_fallback_2(),
            partner_model: default_partner_model(),
            collapse_model: default_collapse_model(),
            use_chain_engine: false,
            use_ir_executor: false,
            operational: OperationalConfig::default(),
            absorption_rate_limit_per_operator: default_absorption_rate_limit(),
            absorption_daily_spend_cap: default_absorption_daily_cap(),
            auto_execute: false,
            model_aliases: HashMap::new(),
        }
    }
}

impl PyramidConfig {
    /// Config file name.
    const FILENAME: &'static str = "pyramid_config.json";

    /// Load from the data directory. Returns default if file doesn't exist.
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(Self::FILENAME);
        match std::fs::read_to_string(&path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Save to the data directory.
    pub fn save(&self, data_dir: &Path) -> anyhow::Result<()> {
        let path = data_dir.join(Self::FILENAME);
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)?;
        Ok(())
    }

    /// Load a profile JSON file and apply it over the current config.
    pub fn apply_profile(&mut self, profile_name: &str, data_dir: &Path) -> anyhow::Result<()> {
        // Look in two places: the canonical data_dir/profiles/, and the
        // legacy ~/.gemini/wire-node/profiles/ location which is where the
        // semantic-aliasing side-quest dropped them on the operator's
        // machine. The canonical location is preferred; the legacy
        // location is a fallback so existing profile sets keep working
        // until the operator copies them over.
        let canonical = data_dir.join("profiles").join(format!("{}.json", profile_name));
        let legacy = dirs::home_dir()
            .map(|h| h.join(".gemini").join("wire-node").join("profiles").join(format!("{}.json", profile_name)));
        let profile_path = if canonical.exists() {
            canonical
        } else if let Some(legacy_path) = legacy.filter(|p| p.exists()) {
            legacy_path
        } else {
            return Err(anyhow::anyhow!(
                "Profile '{}' not found at {:?} (also checked ~/.gemini/wire-node/profiles/)",
                profile_name,
                canonical
            ));
        };

        if !profile_path.exists() {
            return Err(anyhow::anyhow!("Profile '{}' not found at {:?}", profile_name, profile_path));
        }

        let contents = std::fs::read_to_string(&profile_path)?;
        let patch: serde_json::Value = serde_json::from_str(&contents)?;

        // Recursive deep merge
        fn merge(a: &mut serde_json::Value, b: serde_json::Value) {
            match (a, b) {
                (serde_json::Value::Object(a_obj), serde_json::Value::Object(b_obj)) => {
                    for (k, v) in b_obj {
                        if a_obj.contains_key(&k) {
                            merge(a_obj.get_mut(&k).unwrap(), v);
                        } else {
                            a_obj.insert(k, v);
                        }
                    }
                }
                (a, b) => *a = b,
            }
        }

        let mut current_json = serde_json::to_value(&*self)?;
        merge(&mut current_json, patch);
        
        *self = serde_json::from_value(current_json)?;
        Ok(())
    }

    /// List every profile available to apply. Walks both the canonical
    /// data_dir/profiles/ directory and the legacy ~/.gemini/wire-node/
    /// profiles/ location, merges by name (canonical wins on conflict),
    /// and returns a sorted list of profile names (without the .json
    /// extension).
    pub fn list_profiles(data_dir: &Path) -> Vec<String> {
        let mut names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut walk = |dir: &Path| {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|s| s.to_str()) == Some("json") {
                        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                            names.insert(stem.to_string());
                        }
                    }
                }
            }
        };
        walk(&data_dir.join("profiles"));
        if let Some(home) = dirs::home_dir() {
            walk(&home.join(".gemini").join("wire-node").join("profiles"));
        }
        names.into_iter().collect()
    }

    /// Convert to an LlmConfig for use with the build pipeline.
    pub fn to_llm_config(&self) -> LlmConfig {
        LlmConfig {
            api_key: self.openrouter_api_key.clone(),
            auth_token: self.auth_token.clone(),
            primary_model: self.primary_model.clone(),
            fallback_model_1: self.fallback_model_1.clone(),
            fallback_model_2: self.fallback_model_2.clone(),
            primary_context_limit: self.operational.tier1.primary_context_limit,
            fallback_1_context_limit: self.operational.tier1.fallback_1_context_limit,
            max_retries: self.operational.tier1.llm_max_retries,
            base_timeout_secs: self.operational.tier2.llm_base_timeout_secs,
            max_timeout_secs: self.operational.tier2.llm_max_timeout_secs,
            retryable_status_codes: self.operational.tier1.llm_retryable_status_codes.clone(),
            retry_base_sleep_secs: self.operational.tier1.llm_retry_base_sleep_secs,
            timeout_chars_per_increment: self.operational.tier1.llm_timeout_chars_per_increment,
            timeout_increment_secs: self.operational.tier1.llm_timeout_increment_secs,
            rate_limit_max_requests: self.operational.tier1.llm_rate_limit_max_requests,
            rate_limit_window_secs: self.operational.tier1.llm_rate_limit_window_secs,
            llm_debug_logging: self.operational.tier1.llm_debug_logging,
            model_aliases: self.model_aliases.clone(),
        }
    }
}

/// State for a running vine build — cancellation token + status.
pub struct VineBuildHandle {
    pub cancel: tokio_util::sync::CancellationToken,
    pub status: String,        // "running", "complete", "failed"
    pub error: Option<String>, // error message if failed
}

/// WS-ONLINE-G: Combined absorption rate-limit and spend-cap state.
///
/// Both the per-operator hourly build count and the global daily spend are held
/// behind a single Mutex to eliminate the TOCTOU race that existed when they
/// were guarded separately.  A single `lock()` call checks both limits and
/// commits both increments atomically — if the daily cap rejects the request
/// the hourly counter is never bumped.
pub struct AbsorptionGate {
    /// Per-operator hourly build count: operator_id → (count, window_start).
    pub hourly: HashMap<String, (u32, std::time::Instant)>,
    /// Global daily spend: (total_credits_spent_today, day_window_start).
    pub daily: (u64, std::time::Instant),
}

impl AbsorptionGate {
    pub fn new() -> Self {
        Self {
            hourly: HashMap::new(),
            daily: (0u64, std::time::Instant::now()),
        }
    }
}

/// Shared state for the pyramid engine.
///
/// Two SQLite connections: `reader` for concurrent reads, `writer` for
/// serialized writes. Both point to the same WAL-mode database file.
pub struct PyramidState {
    /// Read-only connection for query operations.
    pub reader: Arc<Mutex<Connection>>,
    /// Write connection for mutations (slug creation, node saves, etc.)
    pub writer: Arc<Mutex<Connection>>,
    /// LLM configuration (API key, model cascade).
    pub config: Arc<tokio::sync::RwLock<LlmConfig>>,
    /// Active builds keyed by slug name.
    pub active_build: Arc<tokio::sync::RwLock<HashMap<String, BuildHandle>>>,
    /// Data directory for persisting config files. None if not set.
    pub data_dir: Option<PathBuf>,
    /// Per-slug stale engines for auto-update (Phase 7). Keyed by slug name.
    pub stale_engines: Arc<Mutex<HashMap<String, PyramidStaleEngine>>>,
    /// Per-slug file watchers for auto-update (Phase 7). Keyed by slug name.
    pub file_watchers: Arc<Mutex<HashMap<String, PyramidFileWatcher>>>,
    /// Active vine builds, keyed by vine slug. Prevents concurrent builds per slug.
    pub vine_builds: Arc<Mutex<HashMap<String, VineBuildHandle>>>,
    /// Whether to use the chain engine for builds (feature flag).
    pub use_chain_engine: AtomicBool,
    /// Whether to use the IR executor path (compile chain → ExecutionPlan → execute_plan).
    /// Takes precedence over use_chain_engine when true.
    pub use_ir_executor: AtomicBool,
    /// Local event bus for chain-triggered cascades (P3.2).
    pub event_bus: Arc<LocalEventBus>,
    /// Operational config (tiered constants). Loaded once at startup from pyramid_config.json.
    pub operational: Arc<OperationalConfig>,
    /// Directory containing chain YAML files, prompts, and question sets.
    /// In dev mode (debug_assertions), points to the source tree `../chains` directory
    /// so prompt .md files are read live without copying. In release mode, falls back
    /// to `{data_dir}/chains`.
    pub chains_dir: PathBuf,
    /// Rate limiter for remote pyramid queries (WS-ONLINE-C).
    /// Maps operator_id → (query_count, window_start).
    /// 100 queries per minute per operator.
    pub remote_query_rate_limiter: Arc<Mutex<HashMap<String, (u64, std::time::Instant)>>>,
    /// WS-ONLINE-G: Combined per-operator hourly rate limit + global daily spend cap.
    /// Single mutex eliminates the TOCTOU race between the two checks.
    pub absorption_gate: Arc<Mutex<AbsorptionGate>>,
    /// Post-agents-retro web surface: broadcast bus for tagged build
    /// progress events. Named `build_event_bus` to avoid collision with
    /// the pre-existing `event_bus: Arc<LocalEventBus>` used for chain
    /// cascades. Phase 1 WS-B will wire producer sites.
    pub build_event_bus: Arc<crate::pyramid::event_bus::BuildEventBus>,
    /// Supabase project URL for the public web auth flow. `None` until
    /// WS-E lands config loading.
    pub supabase_url: Option<String>,
    /// Supabase anon key for the public web auth flow. `None` until
    /// WS-E lands config loading.
    pub supabase_anon_key: Option<String>,
    /// HMAC secret for CSRF nonce generation/verification on the public
    /// web surface (post-agents-retro WS-A). Generated at startup; rotated
    /// on process restart. Per-request nonces bind cookie session token +
    /// slug + 5-minute time window.
    pub csrf_secret: [u8; 32],
}

impl PyramidState {
    /// Create a build-scoped copy of this state with its own reader connection.
    ///
    /// The build's reader won't compete with CLI/frontend queries for the shared
    /// reader Mutex. All other fields (writer, config, active_build, etc.) are
    /// shared via Arc so mutations are visible to both.
    pub fn with_build_reader(&self) -> anyhow::Result<Arc<PyramidState>> {
        let db_path = self
            .data_dir
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("data_dir not set, cannot open build reader"))?
            .join("pyramid.db");
        let build_reader = db::open_pyramid_connection(&db_path)?;
        Ok(Arc::new(PyramidState {
            reader: Arc::new(Mutex::new(build_reader)),
            writer: self.writer.clone(),
            config: self.config.clone(),
            active_build: self.active_build.clone(),
            data_dir: self.data_dir.clone(),
            stale_engines: self.stale_engines.clone(),
            file_watchers: self.file_watchers.clone(),
            vine_builds: self.vine_builds.clone(),
            // Snapshot copies — won't reflect runtime changes to the originals.
            // This is fine: these flags are only toggled in parity tests, not at runtime.
            use_chain_engine: AtomicBool::new(
                self.use_chain_engine
                    .load(std::sync::atomic::Ordering::Relaxed),
            ),
            use_ir_executor: AtomicBool::new(
                self.use_ir_executor
                    .load(std::sync::atomic::Ordering::Relaxed),
            ),
            event_bus: self.event_bus.clone(),
            operational: self.operational.clone(),
            chains_dir: self.chains_dir.clone(),
            remote_query_rate_limiter: self.remote_query_rate_limiter.clone(),
            absorption_gate: self.absorption_gate.clone(),
            build_event_bus: self.build_event_bus.clone(),
            supabase_url: self.supabase_url.clone(),
            supabase_anon_key: self.supabase_anon_key.clone(),
            csrf_secret: self.csrf_secret,
        }))
    }
}

/// Handle to a running pyramid build.
pub struct BuildHandle {
    /// Slug being built.
    pub slug: String,
    /// Cancellation token — cancel to abort the build.
    pub cancel: tokio_util::sync::CancellationToken,
    /// Live status (progress, elapsed time, etc.)
    pub status: Arc<tokio::sync::RwLock<BuildStatus>>,
    /// Layer-level build state for the v2 pyramid visualization.
    pub layer_state: Arc<tokio::sync::RwLock<types::BuildLayerState>>,
    /// When the build started — used to compute elapsed time live.
    pub started_at: std::time::Instant,
}
