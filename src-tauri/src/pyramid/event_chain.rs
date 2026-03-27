// pyramid/event_chain.rs — Local event bus for chain-triggered cascades (P3.2)
//
// In-process event bus that enables pyramid events (supersession cascades,
// stale detections, build completions) to trigger chain invocations.
// Fan-out capped at 100. Cascade depth tracked and enforced.
// Events fire asynchronously via tokio::spawn — emit returns immediately.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use chrono::Utc;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, RwLock};
use tracing::{info, warn};
use uuid::Uuid;

// ── Event Types ──────────────────────────────────────────────────────────────

/// Events that can be emitted within the pyramid system.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PyramidEvent {
    SupersessionCascade {
        slug: String,
        superseded_entities: Vec<String>,
        source_node_id: String,
        cascade_depth: u32,
    },
    StaleDetected {
        slug: String,
        node_ids: Vec<String>,
        layer: i64,
    },
    BuildComplete {
        slug: String,
        apex_node_id: String,
        node_count: i64,
    },
    NewApexRequested {
        slug: String,
        question: String,
        granularity: u32,
    },
    Custom {
        event_type: String,
        slug: String,
        payload: Value,
    },
}

impl PyramidEvent {
    /// Returns the variant name used for subscription matching.
    pub fn event_type_name(&self) -> &str {
        match self {
            Self::SupersessionCascade { .. } => "SupersessionCascade",
            Self::StaleDetected { .. } => "StaleDetected",
            Self::BuildComplete { .. } => "BuildComplete",
            Self::NewApexRequested { .. } => "NewApexRequested",
            Self::Custom { event_type, .. } => event_type.as_str(),
        }
    }

    /// Returns the slug associated with this event.
    pub fn slug(&self) -> &str {
        match self {
            Self::SupersessionCascade { slug, .. }
            | Self::StaleDetected { slug, .. }
            | Self::BuildComplete { slug, .. }
            | Self::NewApexRequested { slug, .. }
            | Self::Custom { slug, .. } => slug.as_str(),
        }
    }

    /// Returns the cascade depth if this event carries one, otherwise 0.
    pub fn cascade_depth(&self) -> u32 {
        match self {
            Self::SupersessionCascade { cascade_depth, .. } => *cascade_depth,
            _ => 0,
        }
    }

    /// Serialize the event payload to JSON for logging/chain input.
    pub fn to_payload(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

// ── Subscription ─────────────────────────────────────────────────────────────

/// A subscription that binds an event type to a chain invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventSubscription {
    /// Unique subscription ID.
    pub id: String,
    /// Event type to match (variant name, e.g. "SupersessionCascade").
    pub event_type: String,
    /// If set, only events for this slug fire the subscription.
    pub slug_filter: Option<String>,
    /// Chain ID or question set to invoke when the event fires.
    pub chain_template: String,
    /// Maximum cascade depth before this subscription stops firing (default 10).
    pub max_cascade_depth: u32,
    /// Whether this subscription is active.
    pub enabled: bool,
    /// ISO timestamp when this subscription was created.
    #[serde(default)]
    pub created_at: String,
}

// ── Event Log ────────────────────────────────────────────────────────────────

/// A record of an event emission and which subscriptions it matched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventLogEntry {
    /// Event type name.
    pub event: String,
    /// Slug from the event.
    pub slug: String,
    /// ISO timestamp.
    pub timestamp: String,
    /// Subscription IDs that matched.
    pub matched_subscriptions: Vec<String>,
    /// Invocation IDs for chains that were spawned.
    pub chain_invocations: Vec<String>,
    /// Cascade depth of the triggering event.
    pub cascade_depth: u32,
}

// ── Local Event Bus ──────────────────────────────────────────────────────────

/// In-process event bus for the pyramid system.
///
/// Subscriptions are held in memory (loaded from SQLite on startup).
/// Events fire asynchronously — `emit` spawns tokio tasks and returns
/// invocation IDs immediately. Fan-out capped at `max_fan_out`.
pub struct LocalEventBus {
    subscriptions: Arc<RwLock<Vec<EventSubscription>>>,
    event_log: Arc<Mutex<Vec<EventLogEntry>>>,
    max_fan_out: usize,
}

impl Default for LocalEventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalEventBus {
    /// Create a new event bus with default settings (max fan-out = 100).
    pub fn new() -> Self {
        Self {
            subscriptions: Arc::new(RwLock::new(Vec::new())),
            event_log: Arc::new(Mutex::new(Vec::new())),
            max_fan_out: 100,
        }
    }

    /// Load subscriptions from the database (async version).
    pub async fn load_from_db(&self, conn: &Connection) -> Result<()> {
        let subs = load_subscriptions(conn)?;
        let mut current = self.subscriptions.write().await;
        *current = subs;
        Ok(())
    }

    /// Load subscriptions from the database (sync version for startup).
    /// Call this during app initialization before the async runtime is available.
    pub fn load_from_db_sync(&self, conn: &Connection) -> Result<()> {
        let subs = load_subscriptions(conn)?;
        // RwLock::blocking_write is not available on tokio::sync::RwLock, so
        // we use try_write which succeeds at startup when nothing else holds it.
        match self.subscriptions.try_write() {
            Ok(mut current) => {
                let count = subs.len();
                *current = subs;
                info!(count, "event bus: loaded subscriptions from DB");
                Ok(())
            }
            Err(_) => Err(anyhow!(
                "event bus: could not acquire write lock for subscription load"
            )),
        }
    }

    /// Register a new subscription in memory only (no DB persistence).
    /// Use this from Send-required contexts (route handlers) where holding
    /// a &Connection across awaits is not possible.
    pub async fn subscribe_memory_only(&self, sub: EventSubscription) -> Result<()> {
        if sub.id.is_empty() {
            return Err(anyhow!("subscription id must be non-empty"));
        }
        if sub.event_type.is_empty() {
            return Err(anyhow!("subscription event_type must be non-empty"));
        }
        if sub.chain_template.is_empty() {
            return Err(anyhow!("subscription chain_template must be non-empty"));
        }

        {
            let current = self.subscriptions.read().await;
            if current.iter().any(|s| s.id == sub.id) {
                return Err(anyhow!("subscription with id '{}' already exists", sub.id));
            }
        }

        let mut current = self.subscriptions.write().await;
        current.push(sub);
        Ok(())
    }

    /// Register a new subscription. Persists to DB if a connection is provided.
    pub async fn subscribe(&self, sub: EventSubscription, conn: Option<&Connection>) -> Result<()> {
        if sub.id.is_empty() {
            return Err(anyhow!("subscription id must be non-empty"));
        }
        if sub.event_type.is_empty() {
            return Err(anyhow!("subscription event_type must be non-empty"));
        }
        if sub.chain_template.is_empty() {
            return Err(anyhow!("subscription chain_template must be non-empty"));
        }

        // Check for duplicate ID
        {
            let current = self.subscriptions.read().await;
            if current.iter().any(|s| s.id == sub.id) {
                return Err(anyhow!("subscription with id '{}' already exists", sub.id));
            }
        }

        if let Some(conn) = conn {
            save_subscription(conn, &sub)?;
        }

        let mut current = self.subscriptions.write().await;
        current.push(sub);
        Ok(())
    }

    /// Remove a subscription by ID. Removes from DB if a connection is provided.
    pub async fn unsubscribe(&self, id: &str, conn: Option<&Connection>) -> Result<()> {
        let mut current = self.subscriptions.write().await;
        let before_len = current.len();
        current.retain(|s| s.id != id);
        if current.len() == before_len {
            return Err(anyhow!("subscription '{}' not found", id));
        }

        if let Some(conn) = conn {
            delete_subscription(conn, id)?;
        }

        Ok(())
    }

    /// Emit an event without DB persistence. Use from Send-required contexts
    /// (route handlers) where holding a &Connection across awaits is not possible.
    /// The future returned by this method is Send.
    pub async fn emit_memory_only(&self, event: PyramidEvent) -> Result<Vec<String>> {
        let (ids, _log_entry) = self.emit_core(event).await?;
        Ok(ids)
    }

    /// Emit an event, matching it against subscriptions and spawning chain
    /// invocations for each match. Returns invocation IDs.
    ///
    /// Respects:
    /// - `max_fan_out` cap (default 100)
    /// - `max_cascade_depth` per subscription
    /// - `slug_filter` on subscriptions
    /// - `enabled` flag
    ///
    /// Chain invocations are spawned as tokio tasks — this method does not
    /// block on chain execution.
    pub async fn emit(
        &self,
        event: PyramidEvent,
        db_conn: Option<&Connection>,
    ) -> Result<Vec<String>> {
        let (invocation_ids, log_entry) = self.emit_core(event).await?;

        // Persist to DB if connection provided
        if let Some(conn) = db_conn {
            if let Err(e) = save_event_log(conn, &log_entry) {
                warn!(error = %e, "failed to persist event log entry to DB");
            }
        }

        Ok(invocation_ids)
    }

    /// Core emit logic — no &Connection parameter, so the future is Send.
    /// Returns (invocation_ids, log_entry) so callers can optionally persist.
    async fn emit_core(&self, event: PyramidEvent) -> Result<(Vec<String>, EventLogEntry)> {
        let event_type = event.event_type_name().to_string();
        let event_slug = event.slug().to_string();
        let cascade_depth = event.cascade_depth();
        let payload = event.to_payload();

        // Find matching subscriptions
        let matched: Vec<EventSubscription> = {
            let subs = self.subscriptions.read().await;
            subs.iter()
                .filter(|s| {
                    // Must be enabled
                    if !s.enabled {
                        return false;
                    }
                    // Event type must match
                    if s.event_type != event_type {
                        return false;
                    }
                    // Slug filter (if present) must match
                    if let Some(ref filter_slug) = s.slug_filter {
                        if filter_slug != &event_slug {
                            return false;
                        }
                    }
                    // Cascade depth must be within bounds
                    if cascade_depth >= s.max_cascade_depth {
                        return false;
                    }
                    true
                })
                .take(self.max_fan_out)
                .cloned()
                .collect()
        };

        if matched.len() >= self.max_fan_out {
            warn!(
                event_type = %event_type,
                slug = %event_slug,
                "event fan-out capped at {} subscriptions",
                self.max_fan_out
            );
        }

        // Generate invocation IDs and spawn tasks
        let mut invocation_ids = Vec::with_capacity(matched.len());
        let matched_sub_ids: Vec<String> = matched.iter().map(|s| s.id.clone()).collect();

        for sub in &matched {
            let invocation_id = Uuid::new_v4().to_string();
            invocation_ids.push(invocation_id.clone());

            let chain_template = sub.chain_template.clone();
            let sub_id = sub.id.clone();
            let payload_clone = payload.clone();
            let slug_clone = event_slug.clone();

            // Spawn async chain invocation — fire and forget
            tokio::spawn(async move {
                info!(
                    invocation_id = %invocation_id,
                    subscription = %sub_id,
                    chain = %chain_template,
                    slug = %slug_clone,
                    "event-chain invocation spawned (chain execution placeholder)"
                );
                // TODO(P3.2): Wire actual chain execution here.
                // This will call into build_runner or chain_executor with the
                // chain_template and event payload as input. For now, we log
                // the invocation. The actual wiring depends on having a
                // PyramidState reference, which will be passed when the event
                // bus is integrated with the build system.
                let _ = (chain_template, payload_clone, slug_clone);
            });
        }

        // Build log entry
        let log_entry = EventLogEntry {
            event: event_type.clone(),
            slug: event_slug.clone(),
            timestamp: Utc::now().to_rfc3339(),
            matched_subscriptions: matched_sub_ids,
            chain_invocations: invocation_ids.clone(),
            cascade_depth,
        };

        // Keep in-memory log (bounded to last 1000 entries)
        {
            let mut log = self.event_log.lock().await;
            log.push(log_entry.clone());
            if log.len() > 1000 {
                let drain_count = log.len() - 1000;
                log.drain(..drain_count);
            }
        }

        Ok((invocation_ids, log_entry))
    }

    /// Retrieve recent event log entries.
    pub async fn get_log(&self, limit: usize) -> Vec<EventLogEntry> {
        let log = self.event_log.lock().await;
        let start = if log.len() > limit {
            log.len() - limit
        } else {
            0
        };
        log[start..].to_vec()
    }

    /// Get current subscription count (for diagnostics).
    pub async fn subscription_count(&self) -> usize {
        self.subscriptions.read().await.len()
    }

    /// Get all subscriptions (for diagnostics/API).
    pub async fn get_subscriptions(&self) -> Vec<EventSubscription> {
        self.subscriptions.read().await.clone()
    }
}

// ── DB Persistence ───────────────────────────────────────────────────────────

/// Create the event subscription and log tables. Called from `init_pyramid_db`.
pub fn init_event_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pyramid_event_subscriptions (
            id TEXT PRIMARY KEY,
            event_type TEXT NOT NULL,
            slug_filter TEXT,
            chain_template TEXT NOT NULL,
            max_cascade_depth INTEGER NOT NULL DEFAULT 10,
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS pyramid_event_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            event_type TEXT NOT NULL,
            slug TEXT NOT NULL,
            cascade_depth INTEGER NOT NULL DEFAULT 0,
            matched_count INTEGER NOT NULL DEFAULT 0,
            invoked_count INTEGER NOT NULL DEFAULT 0,
            payload TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_event_log_type ON pyramid_event_log(event_type);
        CREATE INDEX IF NOT EXISTS idx_event_log_slug ON pyramid_event_log(slug);
        ",
    )?;
    Ok(())
}

/// Load all subscriptions from the database.
pub fn load_subscriptions(conn: &Connection) -> Result<Vec<EventSubscription>> {
    let mut stmt = conn.prepare(
        "SELECT id, event_type, slug_filter, chain_template, max_cascade_depth, enabled, created_at
         FROM pyramid_event_subscriptions",
    )?;

    let subs = stmt
        .query_map([], |row| {
            Ok(EventSubscription {
                id: row.get(0)?,
                event_type: row.get(1)?,
                slug_filter: row.get(2)?,
                chain_template: row.get(3)?,
                max_cascade_depth: row.get::<_, i64>(4)? as u32,
                enabled: row.get::<_, i64>(5)? != 0,
                created_at: row.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(subs)
}

/// Save a subscription to the database.
pub fn save_subscription(conn: &Connection, sub: &EventSubscription) -> Result<()> {
    let created_at = if sub.created_at.is_empty() {
        Utc::now().to_rfc3339()
    } else {
        sub.created_at.clone()
    };

    conn.execute(
        "INSERT OR REPLACE INTO pyramid_event_subscriptions
         (id, event_type, slug_filter, chain_template, max_cascade_depth, enabled, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            sub.id,
            sub.event_type,
            sub.slug_filter,
            sub.chain_template,
            sub.max_cascade_depth as i64,
            sub.enabled as i64,
            created_at,
        ],
    )?;
    Ok(())
}

/// Delete a subscription from the database.
pub fn delete_subscription(conn: &Connection, id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM pyramid_event_subscriptions WHERE id = ?1",
        [id],
    )?;
    Ok(())
}

/// Save an event log entry to the database.
fn save_event_log(conn: &Connection, entry: &EventLogEntry) -> Result<()> {
    let payload = serde_json::to_string(&serde_json::json!({
        "matched_subscriptions": entry.matched_subscriptions,
        "chain_invocations": entry.chain_invocations,
    }))
    .unwrap_or_default();

    conn.execute(
        "INSERT INTO pyramid_event_log
         (event_type, slug, cascade_depth, matched_count, invoked_count, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            entry.event,
            entry.slug,
            entry.cascade_depth as i64,
            entry.matched_subscriptions.len() as i64,
            entry.chain_invocations.len() as i64,
            payload,
            entry.timestamp,
        ],
    )?;
    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn in_memory_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_event_tables(&conn).unwrap();
        conn
    }

    fn make_sub(id: &str, event_type: &str) -> EventSubscription {
        EventSubscription {
            id: id.to_string(),
            event_type: event_type.to_string(),
            slug_filter: None,
            chain_template: "test-chain".to_string(),
            max_cascade_depth: 10,
            enabled: true,
            created_at: String::new(),
        }
    }

    #[tokio::test]
    async fn subscribe_and_emit_matches_correctly() {
        let bus = LocalEventBus::new();
        let conn = in_memory_db();

        bus.subscribe(make_sub("s1", "BuildComplete"), Some(&conn))
            .await
            .unwrap();

        let event = PyramidEvent::BuildComplete {
            slug: "test-slug".to_string(),
            apex_node_id: "apex-001".to_string(),
            node_count: 42,
        };

        let ids = bus.emit(event, Some(&conn)).await.unwrap();
        assert_eq!(ids.len(), 1);

        let log = bus.get_log(10).await;
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].event, "BuildComplete");
        assert_eq!(log[0].matched_subscriptions.len(), 1);
        assert_eq!(log[0].matched_subscriptions[0], "s1");
    }

    #[tokio::test]
    async fn slug_filter_works() {
        let bus = LocalEventBus::new();
        let conn = in_memory_db();

        let mut sub = make_sub("s1", "BuildComplete");
        sub.slug_filter = Some("only-this-slug".to_string());
        bus.subscribe(sub, Some(&conn)).await.unwrap();

        // Event with non-matching slug should not fire
        let event = PyramidEvent::BuildComplete {
            slug: "other-slug".to_string(),
            apex_node_id: "apex-001".to_string(),
            node_count: 10,
        };
        let ids = bus.emit(event, Some(&conn)).await.unwrap();
        assert_eq!(ids.len(), 0);

        // Event with matching slug should fire
        let event = PyramidEvent::BuildComplete {
            slug: "only-this-slug".to_string(),
            apex_node_id: "apex-002".to_string(),
            node_count: 20,
        };
        let ids = bus.emit(event, Some(&conn)).await.unwrap();
        assert_eq!(ids.len(), 1);
    }

    #[tokio::test]
    async fn max_cascade_depth_prevents_runaway() {
        let bus = LocalEventBus::new();
        let conn = in_memory_db();

        let mut sub = make_sub("s1", "SupersessionCascade");
        sub.max_cascade_depth = 3;
        bus.subscribe(sub, Some(&conn)).await.unwrap();

        // depth 2 < max 3 => should fire
        let event = PyramidEvent::SupersessionCascade {
            slug: "test".to_string(),
            superseded_entities: vec![],
            source_node_id: "n1".to_string(),
            cascade_depth: 2,
        };
        let ids = bus.emit(event, Some(&conn)).await.unwrap();
        assert_eq!(ids.len(), 1);

        // depth 3 >= max 3 => should NOT fire
        let event = PyramidEvent::SupersessionCascade {
            slug: "test".to_string(),
            superseded_entities: vec![],
            source_node_id: "n2".to_string(),
            cascade_depth: 3,
        };
        let ids = bus.emit(event, Some(&conn)).await.unwrap();
        assert_eq!(ids.len(), 0);

        // depth 10 >= max 3 => should NOT fire
        let event = PyramidEvent::SupersessionCascade {
            slug: "test".to_string(),
            superseded_entities: vec![],
            source_node_id: "n3".to_string(),
            cascade_depth: 10,
        };
        let ids = bus.emit(event, Some(&conn)).await.unwrap();
        assert_eq!(ids.len(), 0);
    }

    #[tokio::test]
    async fn max_fan_out_cap_enforced() {
        let bus = LocalEventBus {
            subscriptions: Arc::new(RwLock::new(Vec::new())),
            event_log: Arc::new(Mutex::new(Vec::new())),
            max_fan_out: 3, // low cap for testing
        };
        let conn = in_memory_db();

        // Add 5 subscriptions
        for i in 0..5 {
            bus.subscribe(make_sub(&format!("s{}", i), "BuildComplete"), Some(&conn))
                .await
                .unwrap();
        }

        let event = PyramidEvent::BuildComplete {
            slug: "test".to_string(),
            apex_node_id: "apex".to_string(),
            node_count: 1,
        };
        let ids = bus.emit(event, Some(&conn)).await.unwrap();
        // Should be capped at 3
        assert_eq!(ids.len(), 3);
    }

    #[tokio::test]
    async fn unsubscribe_removes_subscription() {
        let bus = LocalEventBus::new();
        let conn = in_memory_db();

        bus.subscribe(make_sub("s1", "BuildComplete"), Some(&conn))
            .await
            .unwrap();
        assert_eq!(bus.subscription_count().await, 1);

        bus.unsubscribe("s1", Some(&conn)).await.unwrap();
        assert_eq!(bus.subscription_count().await, 0);

        // Verify DB persistence
        let db_subs = load_subscriptions(&conn).unwrap();
        assert_eq!(db_subs.len(), 0);

        // Emit should match nothing now
        let event = PyramidEvent::BuildComplete {
            slug: "test".to_string(),
            apex_node_id: "apex".to_string(),
            node_count: 1,
        };
        let ids = bus.emit(event, Some(&conn)).await.unwrap();
        assert_eq!(ids.len(), 0);
    }

    #[tokio::test]
    async fn event_log_captures_emissions() {
        let bus = LocalEventBus::new();
        let conn = in_memory_db();

        bus.subscribe(make_sub("s1", "StaleDetected"), Some(&conn))
            .await
            .unwrap();

        let event = PyramidEvent::StaleDetected {
            slug: "my-slug".to_string(),
            node_ids: vec!["n1".to_string(), "n2".to_string()],
            layer: 1,
        };
        bus.emit(event, Some(&conn)).await.unwrap();

        let log = bus.get_log(10).await;
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].event, "StaleDetected");
        assert_eq!(log[0].slug, "my-slug");
        assert_eq!(log[0].cascade_depth, 0);
        assert_eq!(log[0].chain_invocations.len(), 1);

        // Verify DB log entry
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_event_log WHERE event_type = 'StaleDetected'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn disabled_subscription_does_not_fire() {
        let bus = LocalEventBus::new();
        let conn = in_memory_db();

        let mut sub = make_sub("s1", "BuildComplete");
        sub.enabled = false;
        bus.subscribe(sub, Some(&conn)).await.unwrap();

        let event = PyramidEvent::BuildComplete {
            slug: "test".to_string(),
            apex_node_id: "apex".to_string(),
            node_count: 1,
        };
        let ids = bus.emit(event, Some(&conn)).await.unwrap();
        assert_eq!(ids.len(), 0);
    }

    #[tokio::test]
    async fn db_persistence_round_trip() {
        let conn = in_memory_db();

        // Save subscriptions directly
        let sub1 = EventSubscription {
            id: "sub-1".to_string(),
            event_type: "BuildComplete".to_string(),
            slug_filter: Some("my-slug".to_string()),
            chain_template: "chain-a".to_string(),
            max_cascade_depth: 5,
            enabled: true,
            created_at: "2026-03-25T12:00:00Z".to_string(),
        };
        let sub2 = EventSubscription {
            id: "sub-2".to_string(),
            event_type: "StaleDetected".to_string(),
            slug_filter: None,
            chain_template: "chain-b".to_string(),
            max_cascade_depth: 10,
            enabled: false,
            created_at: "2026-03-25T13:00:00Z".to_string(),
        };

        save_subscription(&conn, &sub1).unwrap();
        save_subscription(&conn, &sub2).unwrap();

        // Load and verify
        let loaded = load_subscriptions(&conn).unwrap();
        assert_eq!(loaded.len(), 2);

        let s1 = loaded.iter().find(|s| s.id == "sub-1").unwrap();
        assert_eq!(s1.event_type, "BuildComplete");
        assert_eq!(s1.slug_filter.as_deref(), Some("my-slug"));
        assert_eq!(s1.chain_template, "chain-a");
        assert_eq!(s1.max_cascade_depth, 5);
        assert!(s1.enabled);

        let s2 = loaded.iter().find(|s| s.id == "sub-2").unwrap();
        assert_eq!(s2.event_type, "StaleDetected");
        assert!(s2.slug_filter.is_none());
        assert_eq!(s2.chain_template, "chain-b");
        assert_eq!(s2.max_cascade_depth, 10);
        assert!(!s2.enabled);

        // Delete one and verify
        delete_subscription(&conn, "sub-1").unwrap();
        let loaded = load_subscriptions(&conn).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "sub-2");

        // Load into bus from DB
        let bus = LocalEventBus::new();
        bus.load_from_db(&conn).await.unwrap();
        assert_eq!(bus.subscription_count().await, 1);
    }

    #[tokio::test]
    async fn non_matching_event_type_does_not_fire() {
        let bus = LocalEventBus::new();
        let conn = in_memory_db();

        bus.subscribe(make_sub("s1", "BuildComplete"), Some(&conn))
            .await
            .unwrap();

        // Emit a different event type
        let event = PyramidEvent::StaleDetected {
            slug: "test".to_string(),
            node_ids: vec!["n1".to_string()],
            layer: 0,
        };
        let ids = bus.emit(event, Some(&conn)).await.unwrap();
        assert_eq!(ids.len(), 0);
    }

    #[tokio::test]
    async fn custom_event_matches() {
        let bus = LocalEventBus::new();
        let conn = in_memory_db();

        bus.subscribe(make_sub("s1", "my-custom-event"), Some(&conn))
            .await
            .unwrap();

        let event = PyramidEvent::Custom {
            event_type: "my-custom-event".to_string(),
            slug: "test".to_string(),
            payload: serde_json::json!({"key": "value"}),
        };
        let ids = bus.emit(event, Some(&conn)).await.unwrap();
        assert_eq!(ids.len(), 1);
    }

    #[tokio::test]
    async fn subscribe_rejects_empty_fields() {
        let bus = LocalEventBus::new();

        let mut sub = make_sub("", "BuildComplete");
        assert!(bus.subscribe(sub.clone(), None).await.is_err());

        sub.id = "valid-id".to_string();
        sub.event_type = String::new();
        assert!(bus.subscribe(sub.clone(), None).await.is_err());

        sub.event_type = "BuildComplete".to_string();
        sub.chain_template = String::new();
        assert!(bus.subscribe(sub, None).await.is_err());
    }

    #[tokio::test]
    async fn subscribe_rejects_duplicate_id() {
        let bus = LocalEventBus::new();

        bus.subscribe(make_sub("s1", "BuildComplete"), None)
            .await
            .unwrap();
        assert!(bus
            .subscribe(make_sub("s1", "StaleDetected"), None)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn unsubscribe_returns_error_for_missing_id() {
        let bus = LocalEventBus::new();
        assert!(bus.unsubscribe("nonexistent", None).await.is_err());
    }
}
