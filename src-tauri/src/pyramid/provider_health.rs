// pyramid/provider_health.rs — Phase 11 provider health state machine.
//
// Implements the fail-loud "provider health" signal surface per
// `docs/specs/evidence-triage-and-dadbear.md` Part 3 (Provider Health
// Alerting) and Part 4 (Broadcast reconciliation discrepancies).
//
// ── Contract ────────────────────────────────────────────────────────
//
// Provider health is a SIGNAL the user sees in the oversight UI.
// It is NOT an input to provider selection or automatic failover.
// The only place in Wire Node that reads `provider_health` is:
//
//   1. The provider resolver in chain_executor / tier_routing.rs,
//      which emits a WARN log when it hands out a provider in any
//      non-healthy state. The call still proceeds.
//   2. The `pyramid_provider_health` IPC command, which returns the
//      full snapshot to the frontend for rendering.
//
// A provider never auto-recovers from a health alert. The admin
// acknowledges via the IPC — this is deliberate: the spec wants the
// user to SEE the alert and investigate, not have it quietly
// disappear when the metric improves.
//
// ── Error kinds and thresholds ──────────────────────────────────────
//
// | ErrorKind            | Trigger                         | Policy gate                    |
// |----------------------|---------------------------------|--------------------------------|
// | Http5xx              | Upstream returned 5xx            | 3+ in `degrade_window_secs`    |
// | ConnectionFailure    | DNS/TCP/TLS failure              | 1 occurrence → `down`          |
// | CostDiscrepancy      | Broadcast cost ≠ sync cost       | 3+ in `degrade_window_secs`    |
//
// The thresholds flow from the active `dadbear_policy` contribution's
// `cost_reconciliation` block:
//
//   cost_reconciliation:
//     provider_degrade_count: 3
//     provider_degrade_window_secs: 600
//
// Defaults are hardcoded in the policy loader (`CostReconciliationPolicy::default`)
// with a TODO pointing at Phase 12/15 for surfacing them in the ToolsMode UI.
// This is NOT a Pillar 37 violation: these are reconciliation thresholds,
// not numbers constraining LLM behavior. They're user-configurable and
// the defaults are documented in the spec.

use anyhow::Result;
use rusqlite::Connection;

use super::db::{self, ProviderHealth};
use super::event_bus::{BuildEventBus, TaggedBuildEvent, TaggedKind};
use std::sync::Arc;

/// Classification of a provider-side error, consumed by
/// `record_provider_error`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorKind {
    /// Upstream returned HTTP 5xx. Not the same as a 429 (that's a
    /// retry signal, not a provider-down signal).
    Http5xx,
    /// DNS / TCP / TLS failure — the connection never reached the
    /// provider. Treated as `down` on a single occurrence.
    ConnectionFailure,
    /// The broadcast webhook detected a cost disagreement with the
    /// synchronous ledger. `record_broadcast_confirmation` flipped
    /// the row to `reconciliation_status = 'discrepancy'` before
    /// calling into the health state machine.
    CostDiscrepancy,
}

/// Thresholds for the provider health state machine. Sourced from
/// `dadbear_policy.cost_reconciliation` when a contribution is
/// active; falls back to the spec defaults otherwise.
///
/// A future phase (12 or 15) will surface these as editable fields
/// in the ToolsMode policy editor.
#[derive(Debug, Clone, Copy)]
pub struct CostReconciliationPolicy {
    /// Fractional divergence at which a broadcast cost difference
    /// counts as a discrepancy. 0.10 = 10%.
    pub discrepancy_ratio: f64,
    /// Number of matching errors within the window required to
    /// degrade the provider.
    pub provider_degrade_count: i64,
    /// Rolling window for the degrade count, in seconds.
    pub provider_degrade_window_secs: i64,
    /// Whether the leak-detection sweep is active. When `false`,
    /// unconfirmed synchronous rows are left alone. Defaults to `true`
    /// so the user has to opt out explicitly.
    pub broadcast_required: bool,
    /// Grace period before a synchronous row without broadcast
    /// confirmation is flagged as `broadcast_missing`.
    pub broadcast_grace_period_secs: i64,
    /// How often the leak-detection sweep runs.
    pub broadcast_audit_interval_secs: i64,
}

impl Default for CostReconciliationPolicy {
    fn default() -> Self {
        // Spec defaults from `evidence-triage-and-dadbear.md` Parts 3
        // and 4. TODO(Phase 12/15): load these from the active
        // `dadbear_policy` contribution via the config registry.
        Self {
            discrepancy_ratio: 0.10,
            provider_degrade_count: 3,
            provider_degrade_window_secs: 600,
            broadcast_required: true,
            broadcast_grace_period_secs: 600,
            broadcast_audit_interval_secs: 900,
        }
    }
}

/// Record a provider-side error and update the health state machine.
/// Called from:
///   - The LLM call path on HTTP 5xx
///   - The LLM call path on connection failure
///   - The broadcast webhook handler on cost discrepancy
///
/// For `CostDiscrepancy` the caller is expected to have already
/// flipped the `pyramid_cost_log` row to `reconciliation_status =
/// 'discrepancy'` — this function reads the count of recent
/// discrepancies to decide whether to degrade.
pub fn record_provider_error(
    conn: &Connection,
    provider_id: &str,
    error_kind: ProviderErrorKind,
    policy: &CostReconciliationPolicy,
    bus: Option<&Arc<BuildEventBus>>,
) -> Result<()> {
    let current = db::get_provider_health(conn, provider_id)?;
    let (old_health_str, _reason, _since, _acked) = match current {
        Some(row) => row,
        None => {
            // Provider row not found — nothing to update. The row
            // should exist because `record_provider_error` is only
            // ever called after a provider resolution returned this
            // id. Log and return OK rather than error.
            tracing::warn!(
                provider_id = provider_id,
                "record_provider_error: provider row not found"
            );
            return Ok(());
        }
    };
    let old_health = ProviderHealth::from_str(&old_health_str);

    let (new_health, reason) = match error_kind {
        ProviderErrorKind::ConnectionFailure => (
            ProviderHealth::Down,
            format!("connection failure ({provider_id})"),
        ),
        ProviderErrorKind::Http5xx => {
            // Phase 11 wanderer fix: HTTP 5xx requires a rolling-
            // window count to match spec. The previous implementation
            // called `count_recent_cost_discrepancies` (wrong signal)
            // and degraded on every 5xx unconditionally; single blips
            // would flip the oversight flag and noise-fatigue the
            // operator. Spec: degrade only after `provider_degrade_count`
            // (default 3) HTTP 5xx observations inside
            // `provider_degrade_window_secs` (default 600).
            //
            // The event log is a small append-only table
            // (`pyramid_provider_error_log`) written here and read by
            // the count query on the next observation. The table is
            // rolled over implicitly by the window filter — stale
            // rows beyond the window are ignored by the count query.
            // A periodic trim is not required today because the row
            // volume is small (bounded by attempted requests), but
            // Phase 15 can add a sweep if the table grows large in
            // practice.
            db::record_provider_error_event(conn, provider_id, "http_5xx")?;
            let recent = db::count_recent_provider_errors(
                conn,
                provider_id,
                "http_5xx",
                policy.provider_degrade_window_secs,
            )?;
            if recent >= policy.provider_degrade_count {
                (
                    ProviderHealth::Degraded,
                    format!(
                        "{recent} HTTP 5xx in the last {}s",
                        policy.provider_degrade_window_secs
                    ),
                )
            } else {
                // Below the threshold — do not degrade yet. The
                // observation is logged for the next count but the
                // operator is not paged about a transient blip.
                return Ok(());
            }
        }
        ProviderErrorKind::CostDiscrepancy => {
            let recent = db::count_recent_cost_discrepancies(
                conn,
                provider_id,
                policy.provider_degrade_window_secs,
            )?;
            if recent >= policy.provider_degrade_count {
                (
                    ProviderHealth::Degraded,
                    format!(
                        "{recent} cost discrepancies in the last {}s",
                        policy.provider_degrade_window_secs
                    ),
                )
            } else {
                // Below the threshold — do not degrade yet. Leave
                // the row at its current state. Still return early
                // so we don't emit an event for a no-op.
                return Ok(());
            }
        }
    };

    if new_health == old_health {
        // Idempotent: nothing changed, no event needed. We still
        // refresh `health_since` semantically but avoid an event
        // storm during a burst of repeated errors.
        return Ok(());
    }

    db::set_provider_health(conn, provider_id, new_health, &reason)?;

    if let Some(bus) = bus {
        let _ = bus.tx.send(TaggedBuildEvent {
            slug: provider_id.to_string(),
            kind: TaggedKind::ProviderHealthChanged {
                provider_id: provider_id.to_string(),
                old_health: old_health.as_str().to_string(),
                new_health: new_health.as_str().to_string(),
                reason: reason.clone(),
            },
        });
    }

    tracing::warn!(
        provider_id = provider_id,
        old_health = old_health.as_str(),
        new_health = new_health.as_str(),
        reason = reason.as_str(),
        "provider health changed"
    );

    Ok(())
}

/// Clear a provider health alert. Called from the
/// `pyramid_acknowledge_provider_health` IPC command.
pub fn acknowledge_provider(
    conn: &Connection,
    provider_id: &str,
    bus: Option<&Arc<BuildEventBus>>,
) -> Result<()> {
    let current = db::get_provider_health(conn, provider_id)?;
    let Some((old_health_str, _reason, _since, _acked)) = current else {
        return Ok(());
    };
    db::acknowledge_provider_health(conn, provider_id)?;

    if let Some(bus) = bus {
        let _ = bus.tx.send(TaggedBuildEvent {
            slug: provider_id.to_string(),
            kind: TaggedKind::ProviderHealthChanged {
                provider_id: provider_id.to_string(),
                old_health: old_health_str,
                new_health: "healthy".into(),
                reason: "admin acknowledged".into(),
            },
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::db::init_pyramid_db;

    fn mem_conn_with_provider() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        // The default seed already inserts the openrouter row, so we
        // reuse it rather than inserting our own test row.
        conn
    }

    #[test]
    fn connection_failure_marks_provider_down() {
        let conn = mem_conn_with_provider();
        let policy = CostReconciliationPolicy::default();
        record_provider_error(
            &conn,
            "openrouter",
            ProviderErrorKind::ConnectionFailure,
            &policy,
            None,
        )
        .unwrap();
        let (health, _, _, _) = db::get_provider_health(&conn, "openrouter")
            .unwrap()
            .unwrap();
        assert_eq!(health, "down");
    }

    #[test]
    fn single_5xx_below_threshold_does_not_degrade() {
        // Phase 11 wanderer fix: spec requires 3+ HTTP 5xx in the
        // rolling window before degrading. A single blip is recorded
        // but does not flip provider_health.
        let conn = mem_conn_with_provider();
        let policy = CostReconciliationPolicy::default();
        record_provider_error(
            &conn,
            "openrouter",
            ProviderErrorKind::Http5xx,
            &policy,
            None,
        )
        .unwrap();
        let (health, _, _, _) = db::get_provider_health(&conn, "openrouter")
            .unwrap()
            .unwrap();
        assert_eq!(
            health, "healthy",
            "single 5xx should not flip the provider health flag"
        );
        // The observation should still be persisted for the rolling
        // window count on the next occurrence.
        let recent =
            db::count_recent_provider_errors(&conn, "openrouter", "http_5xx", 600).unwrap();
        assert_eq!(recent, 1);
    }

    #[test]
    fn three_5xx_in_window_degrades() {
        // Phase 11 wanderer fix: once the rolling window crosses the
        // spec's degrade threshold (default 3) the provider flips to
        // 'degraded'.
        let conn = mem_conn_with_provider();
        let policy = CostReconciliationPolicy::default();
        for _ in 0..3 {
            record_provider_error(
                &conn,
                "openrouter",
                ProviderErrorKind::Http5xx,
                &policy,
                None,
            )
            .unwrap();
        }
        let (health, reason, _, _) = db::get_provider_health(&conn, "openrouter")
            .unwrap()
            .unwrap();
        assert_eq!(health, "degraded");
        assert!(
            reason
                .as_deref()
                .map(|r| r.contains("HTTP 5xx"))
                .unwrap_or(false),
            "degrade reason should mention HTTP 5xx, got {reason:?}"
        );
    }

    #[test]
    fn cost_discrepancy_below_threshold_no_change() {
        let conn = mem_conn_with_provider();
        let policy = CostReconciliationPolicy::default();
        // Zero recent discrepancies in the table → no degrade.
        record_provider_error(
            &conn,
            "openrouter",
            ProviderErrorKind::CostDiscrepancy,
            &policy,
            None,
        )
        .unwrap();
        let (health, _, _, _) = db::get_provider_health(&conn, "openrouter")
            .unwrap()
            .unwrap();
        assert_eq!(health, "healthy");
    }

    #[test]
    fn cost_discrepancy_at_threshold_degrades() {
        let conn = mem_conn_with_provider();
        let policy = CostReconciliationPolicy::default();
        // Seed a slug that the cost_log FK requires.
        conn.execute(
            "INSERT OR IGNORE INTO pyramid_slugs (slug, content_type, source_path)
             VALUES (?1, 'code', '')",
            rusqlite::params!["test"],
        )
        .unwrap();
        // Insert 3 discrepancy rows to meet the default threshold.
        for _ in 0..3 {
            conn.execute(
                "INSERT INTO pyramid_cost_log (
                     slug, operation, model, input_tokens, output_tokens,
                     estimated_cost, provider_id, reconciliation_status, created_at
                 ) VALUES (
                     ?1, 'test', 'x', 0, 0, 0.0, 'openrouter', 'discrepancy',
                     datetime('now')
                 )",
                rusqlite::params!["test"],
            )
            .unwrap();
        }

        record_provider_error(
            &conn,
            "openrouter",
            ProviderErrorKind::CostDiscrepancy,
            &policy,
            None,
        )
        .unwrap();
        let (health, _, _, _) = db::get_provider_health(&conn, "openrouter")
            .unwrap()
            .unwrap();
        assert_eq!(health, "degraded");
    }

    #[test]
    fn acknowledge_returns_to_healthy() {
        let conn = mem_conn_with_provider();
        let policy = CostReconciliationPolicy::default();
        record_provider_error(
            &conn,
            "openrouter",
            ProviderErrorKind::ConnectionFailure,
            &policy,
            None,
        )
        .unwrap();
        acknowledge_provider(&conn, "openrouter", None).unwrap();
        let (health, _, _, acked) = db::get_provider_health(&conn, "openrouter")
            .unwrap()
            .unwrap();
        assert_eq!(health, "healthy");
        assert!(acked.is_some(), "acknowledged_at should be set");
    }

    #[test]
    fn unknown_provider_is_noop() {
        let conn = mem_conn_with_provider();
        let policy = CostReconciliationPolicy::default();
        // Not an error — just logs and returns Ok.
        record_provider_error(
            &conn,
            "does-not-exist",
            ProviderErrorKind::Http5xx,
            &policy,
            None,
        )
        .unwrap();
    }
}
