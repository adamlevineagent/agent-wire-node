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
pub mod chain_registry;
pub mod chain_resolve;
pub mod config_helper;
pub mod db;
pub mod delta;
pub mod faq;
pub mod ingest;
pub mod llm;
pub mod meta;
pub mod naming;
pub mod query;
pub mod routes;
pub mod slug;
pub mod stale_engine;
pub mod stale_helpers;
pub mod stale_helpers_upper;
pub mod types;
pub mod vine;
pub mod vine_prompts;
pub mod watcher;
pub mod webbing;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::Mutex;

use self::llm::LlmConfig;
use self::stale_engine::PyramidStaleEngine;
use self::types::BuildStatus;
use self::watcher::PyramidFileWatcher;

/// Persistent pyramid configuration stored in `pyramid_config.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PyramidConfig {
    #[serde(default)]
    pub openrouter_api_key: String,
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
        let mut cfg = LlmConfig::default();
        cfg.api_key = self.openrouter_api_key.clone();
        cfg.auth_token = self.auth_token.clone();
        cfg.primary_model = self.primary_model.clone();
        cfg.fallback_model_1 = self.fallback_model_1.clone();
        cfg.fallback_model_2 = self.fallback_model_2.clone();
        cfg
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
    /// Currently active build, if any.
    pub active_build: Arc<tokio::sync::RwLock<Option<BuildHandle>>>,
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
}

/// Handle to a running pyramid build.
pub struct BuildHandle {
    /// Slug being built.
    pub slug: String,
    /// Cancellation token — cancel to abort the build.
    pub cancel: tokio_util::sync::CancellationToken,
    /// Live status (progress, elapsed time, etc.)
    pub status: Arc<tokio::sync::RwLock<BuildStatus>>,
}
