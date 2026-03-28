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
pub mod characterize;
pub mod chain_dispatch;
pub mod chain_engine;
pub mod chain_executor;
pub mod chain_loader;
pub mod chain_registry;
pub mod chain_resolve;
pub mod config_helper;
pub mod converge_expand;
pub mod crystallization;
pub mod db;
pub mod defaults_adapter;
pub mod delta;
pub mod evidence_answering;
pub mod event_chain;
pub mod execution_plan;
pub mod execution_state;
pub mod extraction_schema;
pub mod expression;
pub mod faq;
pub mod ingest;
pub mod llm;
pub mod meta;
pub mod naming;
pub mod parity;
pub mod publication;
pub mod query;
pub mod question_compiler;
pub mod reconciliation;
pub mod question_decomposition;
pub mod question_loader;
pub mod question_yaml;
pub mod routes;
pub mod slug;
pub mod stale_engine;
pub mod stale_helpers;
pub mod staleness;
pub mod staleness_bridge;
pub mod stale_helpers_upper;
pub mod supersession;
pub mod local_store;
pub mod transform_runtime;
pub mod types;
pub mod vine;
pub mod vine_prompts;
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
}

impl Default for Tier2Config {
    fn default() -> Self {
        Self {
            staleness_threshold: 0.3,
            l0_summary_budget: 100_000,
            pre_map_prompt_budget: 80_000,
            ir_thread_input_char_budget: 90_000,
            distillation_token_budget: 800,
            distillation_early_collapse: 1200,
            llm_base_timeout_secs: 120,
            llm_max_timeout_secs: 600,
            chunk_target_lines: 100,
            max_headline_chars: 72,
            max_headline_words: 8,
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
        }
    }
}

/// State for a running vine build — cancellation token + status.
pub struct VineBuildHandle {
    pub cancel: tokio_util::sync::CancellationToken,
    pub status: String,        // "running", "complete", "failed"
    pub error: Option<String>, // error message if failed
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
}

/// Handle to a running pyramid build.
pub struct BuildHandle {
    /// Slug being built.
    pub slug: String,
    /// Cancellation token — cancel to abort the build.
    pub cancel: tokio_util::sync::CancellationToken,
    /// Live status (progress, elapsed time, etc.)
    pub status: Arc<tokio::sync::RwLock<BuildStatus>>,
    /// When the build started — used to compute elapsed time live.
    pub started_at: std::time::Instant,
}
