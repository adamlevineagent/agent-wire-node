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

pub mod app_mode;
pub mod auth;
pub mod boot;
pub mod compute_market;
pub mod compute_queue;
pub mod fleet;
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

pub use app_mode::{guard_app_ready, transition_to as transition_app_mode, AppMode, AppNotReady};

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
    /// Pyramid sync state for publication (WS-ONLINE-A) and pinned refresh (WS-ONLINE-D).
    pub pyramid_sync_state: Arc<tokio::sync::Mutex<pyramid::sync::PyramidSyncState>>,
    /// Phase 1 compute queue: per-model FIFO queues replacing the global
    /// LOCAL_PROVIDER_SEMAPHORE. The GPU processing loop drains from this.
    pub compute_queue: compute_queue::ComputeQueueHandle,
    /// Fleet roster: same-operator peers discovered via heartbeat and
    /// direct announcements. Fleet dispatch and HTTP endpoints both need
    /// access. Arc<RwLock<>> for concurrent read from LLM path + write
    /// from heartbeat/announce handlers.
    pub fleet_roster: Arc<RwLock<fleet::FleetRoster>>,
    /// Phase 2 WS3: Full compute market state (offers, in-flight jobs,
    /// counters, queue mirror seqs). Persisted to
    /// `${app_data_dir}/compute_market_state.json` on every mutation.
    /// IPC handlers (WS7), the dispatch handler (WS5), and the mirror
    /// task (WS6) all read/write through this RwLock.
    pub compute_market_state: Arc<RwLock<compute_market::ComputeMarketState>>,
    /// Phase 2 WS5: MarketDispatchContext Arc bundle — pending-jobs
    /// registry (Phase 3 populates), tunnel state handle (borrowed),
    /// operational policy (hot-reloaded on ConfigSynced). Cloned into
    /// ServerState at boot and passed to the dispatch handler.
    pub compute_market_dispatch: Arc<pyramid::market_dispatch::MarketDispatchContext>,
    /// Persistent node identity (handle + token). Loaded before first
    /// registration attempt. None only if data_dir is unavailable.
    pub node_identity: Option<auth::NodeIdentity>,
    /// Phase 3 requester-side: in-memory map of in-flight market
    /// dispatches awaiting their push-delivery from Wire's delivery
    /// worker at `/v1/compute/job-result`. Register at `/fill` time,
    /// take+fire at inbound-push time. In-memory only; node restart
    /// loses pending and `pyramid_build` retries the step.
    pub pending_market_jobs: pyramid::pending_jobs::PendingJobs,
    /// Walker v3 Phase 0a-2 §2.17.1: in-memory boot/run state machine.
    /// Always starts at `Booting`; flipped to `Ready` by the boot
    /// coordinator (§2.17 step 9). Only the boot coordinator + the
    /// scope_cache_reloader quarantine relay write here
    /// ({invariant: app_mode_single_writer}). NOT persisted — boot
    /// always restarts at `Booting`.
    pub app_mode: Arc<RwLock<AppMode>>,
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
