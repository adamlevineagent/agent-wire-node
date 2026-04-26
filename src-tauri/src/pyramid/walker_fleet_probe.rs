// Walker v3 — Fleet roster probe cache (Phase 4, plan rev 1.0.2 §2.6 + §3).
//
// Background:
//   The `ProviderReadiness::can_dispatch_now` trait is synchronous
//   (Decision builder holds no async context). Fleet peer roster state
//   lives in `crate::fleet::FleetRoster` behind a tokio RwLock — async.
//   These two are bridged by a module-local sync cache: a background
//   task refreshes the cache on a cadence, and `FleetReadiness` reads
//   the cached snapshot without blocking.
//
//   Mirrors the walker_ollama_probe + walker_market_probe pattern.
//
// Design notes:
//   * OnceLock<Mutex<HashMap<node_id, CachedFleetPeer>>> — sync std Mutex
//     because every reader is in-memory.
//   * Absent map = "not yet populated". `FleetReadiness` reads via
//     `snapshot_reachable_peers` which returns an empty Vec on absent
//     cache → NoReachablePeer. Conservative default that flips to
//     Ready on the first successful background refresh.
//   * `last_seen_at` uses `chrono::DateTime<Utc>` to match
//     `FleetRoster.FleetPeer.last_seen`, since staleness filtering
//     compares those directly.
//   * `is_v1_announcer` is set when the peer's last announce carried
//     `announce_protocol_version < 2` (pre-v3). Peers discovered via
//     heartbeat-only (no direct announce) default to v1 since there is
//     no announce_protocol_version in the heartbeat shape.
//
// Integration:
//   boot.rs step 7.7 (new in Phase 4) spawns a refresh task that reads
//   `LlmConfig.fleet_roster` periodically and projects each FleetPeer
//   into a CachedFleetPeer. Mirrors the walker_ollama_probe spawn
//   site's shape (infinite loop, sleep interval, process lifetime).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Utc};

/// Per-peer snapshot of fleet roster state. Written by the background
/// task; read by `FleetReadiness::can_dispatch_now`.
///
/// `announced_models` comes from `FleetPeer.models_loaded` (what the
/// peer has announced it can serve). Staleness filtering uses
/// `last_seen_at` compared against a `fleet_peer_min_staleness_secs`
/// cutoff.
#[derive(Debug, Clone)]
pub struct CachedFleetPeer {
    pub node_id: String,
    pub node_handle: Option<String>,
    pub announced_models: Vec<String>,
    pub last_seen_at: DateTime<Utc>,
    /// True when the peer's announce_protocol_version < 2 (§5.5.2 strict
    /// mode). v3 requesters refuse to dispatch to v1 announcers because
    /// the wire-format semantics diverge (model_id vs rule_name).
    pub is_v1_announcer: bool,
}

type FleetMap = Mutex<HashMap<String, CachedFleetPeer>>;

static FLEET_PROBE_CACHE: OnceLock<FleetMap> = OnceLock::new();

fn cache_handle() -> &'static FleetMap {
    FLEET_PROBE_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Look up a cached peer by `node_id`. Returns `None` if the peer has
/// never been observed.
#[allow(dead_code)]
pub fn read_cached_peer(node_id: &str) -> Option<CachedFleetPeer> {
    let guard = cache_handle().lock().ok()?;
    guard.get(node_id).cloned()
}

/// Overwrite the cached peer entry for `node_id`. Called by the
/// background refresh task and by tests.
#[allow(dead_code)]
pub fn write_cached_peer(peer: CachedFleetPeer) {
    if let Ok(mut guard) = cache_handle().lock() {
        guard.insert(peer.node_id.clone(), peer);
    }
}

/// Drop a cached peer entry. Used by tests and by the background task
/// when a peer goes away.
#[allow(dead_code)]
pub fn invalidate_cached_peer(node_id: &str) {
    if let Ok(mut guard) = cache_handle().lock() {
        guard.remove(node_id);
    }
}

/// Test-only: wipe the entire cache so adjacent tests don't see each
/// other's writes. Production code MUST NOT call this.
#[allow(dead_code)]
pub fn clear_fleet_cache_for_tests() {
    if let Ok(mut guard) = cache_handle().lock() {
        guard.clear();
    }
}

/// Snapshot all peers whose `last_seen_at` is within `within_secs` of
/// now. Stale peers are excluded. Returns a fresh `Vec<CachedFleetPeer>`
/// so the caller can iterate without holding the lock.
///
/// `within_secs == 0` is permissive — returns every cached peer
/// regardless of freshness. This matches the "no staleness gate"
/// interpretation of SYSTEM_DEFAULT=0, though the resolver's
/// SYSTEM_DEFAULT is 300.
#[allow(dead_code)]
pub fn snapshot_reachable_peers(within_secs: u64) -> Vec<CachedFleetPeer> {
    let guard = match cache_handle().lock() {
        Ok(g) => g,
        Err(_) => return Vec::new(),
    };
    let now = Utc::now();
    let cutoff_secs = within_secs as i64;
    guard
        .values()
        .filter(|p| {
            if cutoff_secs <= 0 {
                return true;
            }
            let age = now.signed_duration_since(p.last_seen_at).num_seconds();
            age <= cutoff_secs
        })
        .cloned()
        .collect()
}

/// Test-only: shared serialization lock for fleet-probe tests that
/// mutate the global cache. Any test that calls
/// `clear_fleet_cache_for_tests` / `write_cached_peer` /
/// `invalidate_cached_peer` across parallel sibling tests should
/// acquire this lock first.
///
/// Poisoned guards recover to the inner state — a prior panicking test
/// shouldn't break the current one. Production code MUST NOT call it.
#[allow(dead_code)]
pub fn fleet_probe_test_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

/// Project a `FleetPeer` from the async roster into a `CachedFleetPeer`.
/// `is_v1_announcer_override` lets the refresh task carry forward the
/// protocol version from the most recent announce (since the static
/// FleetPeer struct doesn't carry it yet, it's passed in by the caller
/// that tracked it during announce handling).
///
/// Callers that don't know the protocol version should pass `true`
/// (safest default — the peer is treated as v1 until proved otherwise).
#[allow(dead_code)]
pub fn project_peer(
    peer: &crate::fleet::FleetPeer,
    is_v1_announcer_override: bool,
) -> CachedFleetPeer {
    CachedFleetPeer {
        node_id: peer.node_id.clone(),
        node_handle: peer.handle_path.clone(),
        announced_models: peer.models_loaded.clone(),
        last_seen_at: peer.last_seen,
        is_v1_announcer: is_v1_announcer_override,
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn make_peer(
        id: &str,
        models: &[&str],
        last_seen: DateTime<Utc>,
        is_v1: bool,
    ) -> CachedFleetPeer {
        CachedFleetPeer {
            node_id: id.to_string(),
            node_handle: Some(format!("@op/{id}")),
            announced_models: models.iter().map(|s| s.to_string()).collect(),
            last_seen_at: last_seen,
            is_v1_announcer: is_v1,
        }
    }

    #[test]
    fn test_fleet_probe_stores_reads_roundtrip() {
        let _g = fleet_probe_test_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        clear_fleet_cache_for_tests();
        let peer = make_peer("node-roundtrip", &["gemma3:27b"], Utc::now(), false);
        write_cached_peer(peer.clone());
        let got = read_cached_peer("node-roundtrip").expect("must be present");
        assert_eq!(got.node_id, "node-roundtrip");
        assert_eq!(got.announced_models, vec!["gemma3:27b".to_string()]);
        assert!(!got.is_v1_announcer);

        invalidate_cached_peer("node-roundtrip");
        assert!(read_cached_peer("node-roundtrip").is_none());
    }

    #[test]
    fn test_fleet_probe_snapshot_filters_stale() {
        let _g = fleet_probe_test_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        clear_fleet_cache_for_tests();
        let now = Utc::now();
        write_cached_peer(make_peer("fresh", &["m1"], now, false));
        write_cached_peer(make_peer(
            "stale",
            &["m1"],
            now - Duration::seconds(1000),
            false,
        ));

        // within_secs = 300 → stale peer excluded.
        let fresh_only = snapshot_reachable_peers(300);
        assert_eq!(fresh_only.len(), 1);
        assert_eq!(fresh_only[0].node_id, "fresh");

        // within_secs = 0 → permissive (both peers returned).
        let all = snapshot_reachable_peers(0);
        assert_eq!(all.len(), 2);

        clear_fleet_cache_for_tests();
    }

    #[test]
    fn test_fleet_probe_snapshot_empty_cache_returns_empty() {
        let _g = fleet_probe_test_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        clear_fleet_cache_for_tests();
        let snap = snapshot_reachable_peers(300);
        assert!(snap.is_empty());
    }

    #[test]
    fn test_project_peer_carries_v1_flag() {
        use crate::fleet::FleetPeer;
        use crate::pyramid::tunnel_url::TunnelUrl;
        use std::collections::HashMap;
        let peer = FleetPeer {
            node_id: "projector".into(),
            name: "n".into(),
            tunnel_url: TunnelUrl::parse("https://p.example.com").unwrap(),
            models_loaded: vec!["llama3.2:latest".into()],
            serving_rules: vec![],
            queue_depths: HashMap::new(),
            total_queue_depth: 0,
            last_seen: Utc::now(),
            handle_path: Some("@op/projector".into()),
            announce_protocol_version: 2,
        };
        let projected = project_peer(&peer, true);
        assert_eq!(projected.node_id, "projector");
        assert!(projected.is_v1_announcer);
        assert_eq!(
            projected.announced_models,
            vec!["llama3.2:latest".to_string()]
        );
    }
}
