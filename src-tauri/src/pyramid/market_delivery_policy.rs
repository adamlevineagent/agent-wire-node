// market_delivery_policy.rs — Operational policy for compute market dispatch.
//
// Shape-parallel to `fleet_delivery_policy.rs` per architecture §VIII.6
// DD-E / DD-Q. Defines `MarketDeliveryPolicy`, the contribution-controlled
// bundle of timings + caps + economic-gate fees that govern the compute
// market dispatch path.
//
// **Differences from `FleetDeliveryPolicy`:**
//   - Drops dispatcher-side fields (`dispatch_ack_timeout_secs`,
//     `timeout_grace_secs`, `orphan_sweep_interval_secs`,
//     `orphan_sweep_multiplier`) because the dispatcher in the market
//     context is always the Wire (not a fleet peer), so these don't apply.
//   - Drops `peer_staleness_secs` (peer-roster-specific).
//   - Adds four market-specific economic-gate fees: `match_search_fee`,
//     `offer_creation_fee`, `queue_push_fee`, `queue_mirror_debounce_ms`.
//     These were previously separate `economic_parameter` contributions;
//     DD-E folds them into this one policy contribution so the operator
//     knobs all live in one supersedable unit.
//
// **Default values match the seed YAML; they exist only to allow a node
// to boot when the DB row is missing. Canonical operational values live
// in `docs/seeds/market_delivery_policy.yaml`.** Operators tune via the
// seed YAML and via contribution supersession at runtime — the Rust
// `Default` impl is a bootstrap sentinel, not an operational tuning
// surface.
//
// Storage follows the `pyramid_fleet_delivery_policy` pattern: a dedicated
// singleton table (`pyramid_market_delivery_policy`, id=1) holding the
// active contribution's raw YAML text + originating `contribution_id`.
// Table creation lives in `db::init_pyramid_db`.

use rusqlite::{params, Connection, OptionalExtension, Result as SqlResult};
use serde::{Deserialize, Serialize};

/// Operational policy for compute market dispatch. All timings in seconds
/// unless the field name ends in `_ms`. All fees are credit amounts (i64
/// in the financial path; stored as u64 here for policy convenience and
/// cast at the debit call site).
///
/// `#[serde(deny_unknown_fields)]` so operator typos surface loudly —
/// a misnamed key in a contribution YAML would otherwise silently fall
/// back to the Rust default and mask a config error.
///
/// Invariant: every field in this struct has a matching key in the seed
/// YAML at `docs/seeds/market_delivery_policy.yaml`, and `Default::default()`
/// returns the same numeric values as the seed. The seed is the canonical
/// operational source; the `Default` impl is a bootstrap-only sentinel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MarketDeliveryPolicy {
    pub version: u32,

    // ── Provider callback delivery (when a provider POSTs a result) ────────
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

    // ── Admission control ──────────────────────────────────────────────────
    pub max_inflight_jobs: u64,
    pub admission_retry_after_secs: u64,

    // ── Economic gates (per DD-E, absorbed from standalone economic_parameters) ─
    pub match_search_fee: u64,
    pub offer_creation_fee: u64,
    pub queue_push_fee: u64,
    pub queue_mirror_debounce_ms: u64,

    // ── Phase 3 provider delivery worker ─────────────────────────────────────
    /// Added to `callback_post_timeout_secs` to form the total lease
    /// duration during a delivery POST. Rev 0.3-0.5 audit promoted this
    /// from a hardcoded `+5` to a policy field (Pillar 37 spirit) — the
    /// right grace depends on network jitter characteristics and operators
    /// may want to tune it.
    pub lease_grace_secs: u64,

    /// Max concurrent POSTs per delivery-loop tick. Bounded fan-in per
    /// Pillar 44 spirit. With N ready rows and K = max_concurrent_deliveries,
    /// the tick dispatches K POSTs in parallel; remaining rows wait for the
    /// next tick (or the next nudge). Prevents head-of-line blocking under
    /// pathological load where a single slow POST would gate all others.
    pub max_concurrent_deliveries: u64,

    /// Truncation cap for error messages written to `last_error` + chronicle
    /// metadata. Operator-tunable because chronicle verbosity is an
    /// operational concern. Default 1024 is a sane compromise between "enough
    /// to debug a reqwest error" and "not filling pyramid_compute_events
    /// with multi-KB TLS stack dumps."
    pub max_error_message_chars: u64,
}

impl Default for MarketDeliveryPolicy {
    /// Bootstrap sentinels — match the seed YAML at
    /// `docs/seeds/market_delivery_policy.yaml` numerically, but exist only
    /// to let a node accept dispatches when the operational DB row is
    /// missing. Canonical operational values live in the seed YAML and
    /// are tuned via contribution supersession.
    fn default() -> Self {
        Self {
            version: 1,

            // Provider callback delivery
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

            // Economic gates
            match_search_fee: 1,
            offer_creation_fee: 1,
            queue_push_fee: 1,
            queue_mirror_debounce_ms: 500,

            // Phase 3 provider delivery worker
            lease_grace_secs: 5,
            max_concurrent_deliveries: 4,
            max_error_message_chars: 1024,
        }
    }
}

impl MarketDeliveryPolicy {
    /// Parse a `MarketDeliveryPolicy` from a YAML string.
    ///
    /// Accepts two shapes:
    ///   1. Bare policy body (every field on the struct at top level).
    ///   2. Contribution-style body with `schema_type: market_delivery_policy`
    ///      at the top — the seed YAML at
    ///      `docs/seeds/market_delivery_policy.yaml` and contributions synced
    ///      from the Wire take this form.
    ///
    /// `schema_type` is stripped before deserialization so `deny_unknown_fields`
    /// on the struct still catches operator typos in the operational fields.
    pub fn from_yaml(yaml: &str) -> Result<Self, serde_yaml::Error> {
        let mut value: serde_yaml::Value = serde_yaml::from_str(yaml)?;
        if let serde_yaml::Value::Mapping(ref mut map) = value {
            map.remove(serde_yaml::Value::String("schema_type".to_string()));
        }
        serde_yaml::from_value(value)
    }
}

// ── DB helpers ─────────────────────────────────────────────────────────────
//
// Singleton table `pyramid_market_delivery_policy` (id=1) stores the
// active policy's raw YAML text plus the `contribution_id` it was
// synced from. Mirrors `pyramid_fleet_delivery_policy` exactly. Table
// creation lives in `db::init_pyramid_db` alongside the fleet one.

/// Ensure the operational table exists. Idempotent; safe to call on
/// every open. **Test-only**: production code MUST rely on
/// `db::init_pyramid_db` as the single schema source of truth.
#[cfg(test)]
pub fn ensure_table(conn: &Connection) -> SqlResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS pyramid_market_delivery_policy (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            yaml_content TEXT NOT NULL DEFAULT '',
            contribution_id TEXT,
            updated_at TEXT DEFAULT (datetime('now'))
        )",
    )
}

/// Read the active market delivery policy. Returns `Ok(None)` if no row
/// is present OR the stored YAML is empty. Callers should fall back to
/// `MarketDeliveryPolicy::default()` in that case (bootstrap sentinel).
pub fn read_market_delivery_policy(
    conn: &Connection,
) -> SqlResult<Option<MarketDeliveryPolicy>> {
    let row: Option<String> = conn
        .query_row(
            "SELECT yaml_content FROM pyramid_market_delivery_policy WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    match row.filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(yaml) => match MarketDeliveryPolicy::from_yaml(&yaml) {
            Ok(policy) => Ok(Some(policy)),
            Err(e) => Err(rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(e),
            )),
        },
    }
}

/// Upsert the active market delivery policy. Serializes to YAML and
/// writes to the singleton row (id=1).
pub fn upsert_market_delivery_policy(
    conn: &Connection,
    policy: &MarketDeliveryPolicy,
) -> SqlResult<()> {
    let yaml = serde_yaml::to_string(policy)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    upsert_market_delivery_policy_yaml(conn, &yaml, None)
}

/// Low-level variant used by the contribution sync path: store the
/// contribution's raw YAML text verbatim (so the operator-authored
/// representation round-trips) plus the originating `contribution_id`.
pub fn upsert_market_delivery_policy_yaml(
    conn: &Connection,
    yaml_content: &str,
    contribution_id: Option<&str>,
) -> SqlResult<()> {
    conn.execute(
        "INSERT INTO pyramid_market_delivery_policy (id, yaml_content, contribution_id, updated_at)
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

    const SEED_YAML: &str =
        include_str!("../../../docs/seeds/market_delivery_policy.yaml");

    #[test]
    fn default_matches_seed_yaml() {
        // The Default impl MUST reproduce the seed YAML field-for-field.
        // If this test fails, either the Default impl drifted or the
        // seed YAML was edited without updating the sentinels. Both
        // are bugs — the comment contract in the module doc is that
        // they coincide.
        let from_default = MarketDeliveryPolicy::default();
        let from_seed = MarketDeliveryPolicy::from_yaml(SEED_YAML)
            .expect("seed YAML must parse");
        assert_eq!(from_default, from_seed);
    }

    #[test]
    fn seed_yaml_parses_cleanly() {
        let policy = MarketDeliveryPolicy::from_yaml(SEED_YAML)
            .expect("seed YAML must parse");
        assert_eq!(policy.version, 1);
        assert_eq!(policy.callback_post_timeout_secs, 30);
        assert_eq!(policy.max_inflight_jobs, 32);
        assert_eq!(policy.match_search_fee, 1);
        assert_eq!(policy.queue_mirror_debounce_ms, 500);
    }

    #[test]
    fn from_yaml_rejects_unknown_fields() {
        let yaml = r#"
schema_type: market_delivery_policy
version: 1
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
match_search_fee: 1
offer_creation_fee: 1
queue_push_fee: 1
queue_mirror_debounce_ms: 500
lease_grace_secs: 5
max_concurrent_deliveries: 4
max_error_message_chars: 1024
# The typo below is what deny_unknown_fields catches.
bogus_field_that_does_not_exist: 42
"#;
        let err = MarketDeliveryPolicy::from_yaml(yaml)
            .expect_err("unknown field must reject");
        let msg = err.to_string();
        assert!(
            msg.contains("bogus_field_that_does_not_exist")
                || msg.contains("unknown field"),
            "error should name the offending field, got: {msg}"
        );
    }

    #[test]
    fn from_yaml_rejects_malformed_yaml() {
        let malformed = "version: 1\ncallback_post_timeout_secs: [1, 2, 3\n";
        assert!(MarketDeliveryPolicy::from_yaml(malformed).is_err());
    }

    #[test]
    fn roundtrip_serialize_then_parse() {
        let original = MarketDeliveryPolicy::default();
        let yaml = serde_yaml::to_string(&original).expect("serialize must succeed");
        let parsed = MarketDeliveryPolicy::from_yaml(&yaml)
            .expect("parse of our own serialization must succeed");
        assert_eq!(original, parsed);
    }

    #[test]
    fn db_upsert_and_read_roundtrip() {
        let conn = Connection::open_in_memory().expect("open in-memory");
        ensure_table(&conn).expect("create table");

        assert!(
            read_market_delivery_policy(&conn)
                .expect("read empty")
                .is_none()
        );

        let mut policy = MarketDeliveryPolicy::default();
        policy.callback_post_timeout_secs = 42;
        policy.max_inflight_jobs = 7;
        policy.match_search_fee = 3;
        upsert_market_delivery_policy(&conn, &policy).expect("upsert");

        let read = read_market_delivery_policy(&conn)
            .expect("read after upsert")
            .expect("row present");
        assert_eq!(read, policy);

        let mut policy2 = MarketDeliveryPolicy::default();
        policy2.queue_mirror_debounce_ms = 999;
        upsert_market_delivery_policy(&conn, &policy2).expect("upsert 2");
        let read2 = read_market_delivery_policy(&conn)
            .expect("read after second upsert")
            .expect("row present");
        assert_eq!(read2, policy2);
        assert_ne!(read2, policy);
    }

    #[test]
    fn db_upsert_raw_yaml_preserves_contribution_text() {
        let conn = Connection::open_in_memory().expect("open in-memory");
        ensure_table(&conn).expect("create table");

        upsert_market_delivery_policy_yaml(&conn, SEED_YAML, Some("contrib-abc"))
            .expect("upsert raw yaml");

        let read = read_market_delivery_policy(&conn)
            .expect("read")
            .expect("row present");
        assert_eq!(read, MarketDeliveryPolicy::default());
    }
}
