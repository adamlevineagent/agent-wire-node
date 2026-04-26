// fleet_delivery_policy.rs — Operational policy for async fleet dispatch.
//
// Defines `FleetDeliveryPolicy`, the contribution-controlled bundle of
// timings and caps that govern the async fleet dispatch path: dispatcher
// ACK timeouts, peer callback backoff, outbox retention, worker
// heartbeat, admission control, and peer discovery staleness.
//
// **Default values match the seed YAML; they exist only to allow a node
// to boot when the DB row is missing. Canonical operational values live
// in `docs/seeds/fleet_delivery_policy.yaml`.** Operators tune via the
// seed YAML and via contribution supersession at runtime — the Rust
// `Default` impl is a bootstrap sentinel, not an operational tuning
// surface.
//
// See `docs/plans/async-fleet-dispatch.md` § "Operational Policy" for
// field semantics and the rationale behind each field.
//
// Storage follows the `dispatch_policy` pattern: a dedicated singleton
// table (`pyramid_fleet_delivery_policy`, id=1) holding the active
// contribution's raw YAML text. The raw YAML is re-parsed on every
// read — cheap and avoids schema drift between the stored form and the
// runtime struct.

use rusqlite::{params, Connection, OptionalExtension, Result as SqlResult};
use serde::{Deserialize, Serialize};

/// Operational policy for async fleet dispatch. All timings in seconds.
///
/// `#[serde(deny_unknown_fields)]` so operator typos surface loudly —
/// a misnamed key in a contribution YAML would otherwise silently fall
/// back to the Rust default and mask a config error.
///
/// Invariant: every field in this struct has a matching key in the seed
/// YAML at `docs/seeds/fleet_delivery_policy.yaml`, and `Default::default()`
/// returns the same numeric values as the seed. The seed is the
/// canonical operational source; the `Default` impl is a bootstrap-only
/// sentinel for a node booting with no DB row present.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FleetDeliveryPolicy {
    pub version: u32,

    // ── Dispatcher side ─────────────────────────────────────────────────────
    pub dispatch_ack_timeout_secs: u64,
    pub timeout_grace_secs: u64,
    pub orphan_sweep_interval_secs: u64,
    pub orphan_sweep_multiplier: u64,

    // ── Peer side ───────────────────────────────────────────────────────────
    pub callback_post_timeout_secs: u64,
    pub outbox_sweep_interval_secs: u64,
    pub worker_heartbeat_interval_secs: u64,
    pub worker_heartbeat_tolerance_secs: u64,
    pub backoff_base_secs: u64,
    pub backoff_cap_secs: u64,
    pub max_delivery_attempts: u64,
    pub ready_retention_secs: u64,
    pub delivered_retention_secs: u64,
    pub failed_retention_secs: u64,

    // ── Admission control ───────────────────────────────────────────────────
    pub max_inflight_jobs: u64,
    pub admission_retry_after_secs: u64,

    // ── Peer discovery ──────────────────────────────────────────────────────
    pub peer_staleness_secs: u64,
}

impl Default for FleetDeliveryPolicy {
    /// Bootstrap sentinels — match the seed YAML at
    /// `docs/seeds/fleet_delivery_policy.yaml` numerically, but exist only
    /// to let a node accept dispatches when the operational DB row is
    /// missing. Canonical operational values live in the seed YAML and
    /// are tuned via contribution supersession. Do NOT treat these as
    /// "the right values" — they are the safe-ish bootstrap values.
    fn default() -> Self {
        Self {
            version: 1,

            // Dispatcher side
            dispatch_ack_timeout_secs: 10,
            timeout_grace_secs: 2,
            orphan_sweep_interval_secs: 30,
            orphan_sweep_multiplier: 2,

            // Peer side
            callback_post_timeout_secs: 30,
            outbox_sweep_interval_secs: 15,
            worker_heartbeat_interval_secs: 10,
            worker_heartbeat_tolerance_secs: 30,
            backoff_base_secs: 1,
            backoff_cap_secs: 64,
            max_delivery_attempts: 20,
            ready_retention_secs: 1800,
            delivered_retention_secs: 3600,
            failed_retention_secs: 604800,

            // Admission control
            max_inflight_jobs: 32,
            admission_retry_after_secs: 30,

            // Peer discovery
            peer_staleness_secs: 120,
        }
    }
}

impl FleetDeliveryPolicy {
    /// Parse a `FleetDeliveryPolicy` from a YAML string.
    ///
    /// Accepts two shapes:
    ///   1. Bare policy body (every field on the struct at top level).
    ///   2. Contribution-style body with `schema_type: fleet_delivery_policy`
    ///      at the top — the seed YAML at `docs/seeds/fleet_delivery_policy.yaml`
    ///      and contributions synced from the Wire take this form.
    ///
    /// `schema_type` is stripped before deserialization so `deny_unknown_fields`
    /// on the struct still catches operator typos in the operational fields
    /// (the primary reason `deny_unknown_fields` is there). Any key other
    /// than `schema_type` that doesn't map to a struct field is an error.
    pub fn from_yaml(yaml: &str) -> Result<Self, serde_yaml::Error> {
        // Two-stage parse: go through `serde_yaml::Value`, drop the
        // `schema_type` tag if present, then deserialize the stripped
        // value into the struct. This lets the seed YAML (which carries
        // `schema_type` like every other contribution body) round-trip
        // without losing `deny_unknown_fields`' ability to flag typos.
        let mut value: serde_yaml::Value = serde_yaml::from_str(yaml)?;
        if let serde_yaml::Value::Mapping(ref mut map) = value {
            map.remove(serde_yaml::Value::String("schema_type".to_string()));
        }
        serde_yaml::from_value(value)
    }
}

// ── DB helpers ─────────────────────────────────────────────────────────────
//
// Singleton table `pyramid_fleet_delivery_policy` (id=1) stores the
// active policy's raw YAML text plus the `contribution_id` it was
// synced from. Mirrors `pyramid_dispatch_policy`'s shape exactly —
// same three columns (`yaml_content`, `contribution_id`, `updated_at`),
// same id=1 singleton semantics, same `INSERT ... ON CONFLICT(id) DO UPDATE`
// upsert pattern.
//
// Table creation itself lives in `db::init_pyramid_db`, alongside the
// dispatch_policy table creation, so both dedicated singleton tables
// are bootstrapped at the same DB-init step.

/// Ensure the operational table exists. Idempotent; safe to call on
/// every open. **Test-only**: this is a convenience helper for tests that
/// stand up an in-memory connection. Production code MUST rely on
/// `db::init_pyramid_db` as the single schema source of truth — duplicating
/// the `CREATE TABLE` here and in `init_pyramid_db` risks schema drift.
#[cfg(test)]
pub fn ensure_table(conn: &Connection) -> SqlResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS pyramid_fleet_delivery_policy (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            yaml_content TEXT NOT NULL DEFAULT '',
            contribution_id TEXT,
            updated_at TEXT DEFAULT (datetime('now'))
        )",
    )
}

/// Read the active fleet delivery policy. Returns `Ok(None)` if no row
/// is present OR the stored YAML is empty. Callers should fall back to
/// `FleetDeliveryPolicy::default()` in that case (bootstrap sentinel).
pub fn read_fleet_delivery_policy(conn: &Connection) -> SqlResult<Option<FleetDeliveryPolicy>> {
    let row: Option<String> = conn
        .query_row(
            "SELECT yaml_content FROM pyramid_fleet_delivery_policy WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    match row.filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(yaml) => match FleetDeliveryPolicy::from_yaml(&yaml) {
            Ok(policy) => Ok(Some(policy)),
            // Stored YAML that fails to parse is surfaced as a generic
            // DB error so callers can fall back to the bootstrap default
            // without this function trafficking in a second error type.
            Err(e) => Err(rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(e),
            )),
        },
    }
}

/// Upsert the active fleet delivery policy. Serializes to YAML and
/// writes to the singleton row (id=1).
pub fn upsert_fleet_delivery_policy(
    conn: &Connection,
    policy: &FleetDeliveryPolicy,
) -> SqlResult<()> {
    let yaml = serde_yaml::to_string(policy)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    upsert_fleet_delivery_policy_yaml(conn, &yaml, None)
}

/// Low-level variant used by the contribution sync path: store the
/// contribution's raw YAML text verbatim (so the operator-authored
/// representation round-trips) plus the originating `contribution_id`.
/// Matches the `dispatch_policy` storage pattern exactly.
pub fn upsert_fleet_delivery_policy_yaml(
    conn: &Connection,
    yaml_content: &str,
    contribution_id: Option<&str>,
) -> SqlResult<()> {
    conn.execute(
        "INSERT INTO pyramid_fleet_delivery_policy (id, yaml_content, contribution_id, updated_at)
         VALUES (1, ?1, ?2, datetime('now'))
         ON CONFLICT(id) DO UPDATE SET
            yaml_content = excluded.yaml_content,
            contribution_id = excluded.contribution_id,
            updated_at = datetime('now')",
        params![yaml_content, contribution_id],
    )?;
    Ok(())
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SEED_YAML: &str = include_str!("../../../docs/seeds/fleet_delivery_policy.yaml");

    #[test]
    fn default_matches_seed_yaml() {
        // The Default impl MUST reproduce the seed YAML field-for-field.
        // If this test fails, either the Default impl drifted or the
        // seed YAML was edited without updating the sentinels. Both
        // are bugs — the comment contract in the module doc is that
        // they coincide.
        let from_default = FleetDeliveryPolicy::default();
        let from_seed = FleetDeliveryPolicy::from_yaml(SEED_YAML).expect("seed YAML must parse");
        assert_eq!(from_default, from_seed);
    }

    #[test]
    fn seed_yaml_parses_cleanly() {
        let policy = FleetDeliveryPolicy::from_yaml(SEED_YAML).expect("seed YAML must parse");
        // Spot-check a few operational fields beyond just "parsed without
        // error", so that a seed YAML edit that changes a number is
        // caught here rather than silently propagating.
        assert_eq!(policy.version, 1);
        assert_eq!(policy.dispatch_ack_timeout_secs, 10);
        assert_eq!(policy.max_inflight_jobs, 32);
        assert_eq!(policy.failed_retention_secs, 604800);
        assert_eq!(policy.peer_staleness_secs, 120);
    }

    #[test]
    fn from_yaml_rejects_unknown_fields() {
        let yaml = r#"
schema_type: fleet_delivery_policy
version: 1
dispatch_ack_timeout_secs: 10
timeout_grace_secs: 2
orphan_sweep_interval_secs: 30
orphan_sweep_multiplier: 2
callback_post_timeout_secs: 30
outbox_sweep_interval_secs: 15
worker_heartbeat_interval_secs: 10
worker_heartbeat_tolerance_secs: 30
backoff_base_secs: 1
backoff_cap_secs: 64
max_delivery_attempts: 20
ready_retention_secs: 1800
delivered_retention_secs: 3600
failed_retention_secs: 604800
max_inflight_jobs: 32
admission_retry_after_secs: 30
peer_staleness_secs: 120
# The typo below is what deny_unknown_fields catches.
bogus_field_that_does_not_exist: 42
"#;
        let err = FleetDeliveryPolicy::from_yaml(yaml).expect_err("unknown field must reject");
        let msg = err.to_string();
        assert!(
            msg.contains("bogus_field_that_does_not_exist") || msg.contains("unknown field"),
            "error should name the offending field, got: {msg}"
        );
    }

    #[test]
    fn from_yaml_rejects_malformed_yaml() {
        // Structural breakage (unclosed bracket). serde_yaml surfaces a
        // parse error, which we propagate unchanged.
        let malformed = "version: 1\ndispatch_ack_timeout_secs: [1, 2, 3\n";
        assert!(FleetDeliveryPolicy::from_yaml(malformed).is_err());
    }

    #[test]
    fn roundtrip_serialize_then_parse() {
        let original = FleetDeliveryPolicy::default();
        let yaml = serde_yaml::to_string(&original).expect("serialize must succeed");
        let parsed = FleetDeliveryPolicy::from_yaml(&yaml)
            .expect("parse of our own serialization must succeed");
        assert_eq!(original, parsed);
    }

    #[test]
    fn db_upsert_and_read_roundtrip() {
        let conn = Connection::open_in_memory().expect("open in-memory");
        ensure_table(&conn).expect("create table");

        // Empty DB returns None — caller falls back to default.
        assert!(read_fleet_delivery_policy(&conn)
            .expect("read empty")
            .is_none());

        // Write a non-default policy so we can tell read-back worked.
        let mut policy = FleetDeliveryPolicy::default();
        policy.dispatch_ack_timeout_secs = 42;
        policy.max_inflight_jobs = 7;
        upsert_fleet_delivery_policy(&conn, &policy).expect("upsert");

        let read = read_fleet_delivery_policy(&conn)
            .expect("read after upsert")
            .expect("row present");
        assert_eq!(read, policy);

        // Second upsert overwrites the first (singleton semantics).
        let mut policy2 = FleetDeliveryPolicy::default();
        policy2.callback_post_timeout_secs = 99;
        upsert_fleet_delivery_policy(&conn, &policy2).expect("upsert 2");
        let read2 = read_fleet_delivery_policy(&conn)
            .expect("read after second upsert")
            .expect("row present");
        assert_eq!(read2, policy2);
        assert_ne!(read2, policy);
    }

    #[test]
    fn db_upsert_raw_yaml_preserves_contribution_text() {
        // The contribution sync path stores the operator-authored YAML
        // verbatim. Confirm that variant writes cleanly and the read
        // path parses what we wrote.
        let conn = Connection::open_in_memory().expect("open in-memory");
        ensure_table(&conn).expect("create table");

        upsert_fleet_delivery_policy_yaml(&conn, SEED_YAML, Some("contrib-abc"))
            .expect("upsert raw yaml");

        let read = read_fleet_delivery_policy(&conn)
            .expect("read")
            .expect("row present");
        assert_eq!(read, FleetDeliveryPolicy::default());
    }
}
