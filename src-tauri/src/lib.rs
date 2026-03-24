// Wire Node — Rust Backend Library
//
// Modules:
//   auth      — Supabase magic link + password authentication, Wire node registration
//   sync      — Document sync engine (folder linking, hash verification, push/pull)
//   server    — HTTP server for serving cached documents with JWT verification
//   credits   — Pull tracking, credit reporting, achievement system
//   tunnel    — Cloudflare Tunnel management (cloudflared lifecycle)
//   messaging — Wire-specific messaging: market surface, credit balance, hosting stats
//   market    — Market daemon: evaluates storage opportunities, auto-hosts/drops documents
//   retention — Proof-of-retention challenges and purge handling

pub mod auth;
pub mod credits;
pub mod http_utils;
pub mod market;
pub mod messaging;
pub mod partner;
pub mod pyramid;
pub mod retention;
pub mod server;
pub mod sync;
pub mod tunnel;
pub mod utils;
pub mod work;

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Shared application state
pub struct AppState {
    pub auth: Arc<RwLock<auth::AuthState>>,
    pub sync_state: Arc<RwLock<sync::SyncState>>,
    pub credits: Arc<RwLock<credits::CreditTracker>>,
    pub tunnel_state: Arc<RwLock<tunnel::TunnelState>>,
    pub market_state: Arc<RwLock<market::MarketState>>,
    pub work_stats: Arc<RwLock<work::WorkStats>>,
    pub config: Arc<RwLock<WireNodeConfig>>,
    pub pyramid: Arc<pyramid::PyramidState>,
    pub partner: Arc<partner::PartnerState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireNodeConfig {
    pub api_url: String,
    pub node_id: String,
    pub storage_cap_gb: f64,
    pub mesh_hosting_enabled: bool,
    pub auto_update_enabled: bool,
    pub document_cache_dir: String,
    pub server_port: u16,
    pub jwt_public_key: String,
    // Supabase credentials for auth flows
    pub supabase_url: String,
    pub supabase_anon_key: String,
    pub tunnel_api_url: String,
}

impl Default for WireNodeConfig {
    fn default() -> Self {
        let document_cache_dir = dirs::data_local_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("wire-node")
            .join("documents");

        Self {
            api_url: "https://newsbleach.com".to_string(),
            node_id: String::new(),
            storage_cap_gb: 10.0,
            mesh_hosting_enabled: false,
            auto_update_enabled: false,
            document_cache_dir: document_cache_dir.to_string_lossy().to_string(),
            server_port: 8765,
            jwt_public_key: String::new(),
            supabase_url: "https://supabase.newsbleach.com".to_string(),
            supabase_anon_key: "eyJhbGciOiAiSFMyNTYiLCAidHlwIjogIkpXVCJ9.eyJyb2xlIjogImFub24iLCAiaXNzIjogInN1cGFiYXNlIiwgImlhdCI6IDE3NDAwMDAwMDAsICJleHAiOiAyMDg3MTI3MzM4fQ.5Og0cJw4IkDdCQVYztlzJSoptuyeWjjtKjwOKUukd-Y".to_string(),
            tunnel_api_url: "https://newsbleach.com".to_string(),
        }
    }
}

impl WireNodeConfig {
    /// Get the cache dir as a PathBuf
    pub fn cache_dir(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(&self.document_cache_dir)
    }

    /// Get the data dir (parent of cache dir) for storing config files
    pub fn data_dir(&self) -> std::path::PathBuf {
        self.cache_dir()
            .parent()
            .unwrap_or(&self.cache_dir())
            .to_path_buf()
    }

    /// Get the node name from hostname
    pub fn node_name(&self) -> String {
        hostname()
    }
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "Wire Node".to_string())
}

pub type SharedState = Arc<AppState>;
