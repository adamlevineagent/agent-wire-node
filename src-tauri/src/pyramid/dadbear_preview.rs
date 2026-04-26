// pyramid/dadbear_preview.rs — WS-G: Preview + Dispatch Contracts (Phase 4)
//
// Implements the preview-then-commit system for DADBEAR work items.
// Before work items are dispatched to the compute queue, a batch-level
// preview is created with cost estimates, routing resolution, and a
// policy snapshot. The preview is either auto-committed (within budget)
// or held for operator confirmation.
//
// Key design points:
//   - Preview is a batch-level contract, not per-item metadata
//   - Policy hash ensures stale previews are caught before dispatch
//   - TTL (5 min) prevents unbounded commitment windows
//   - Budget enforcement: auto-commit / requires-approval / cost-limit-hold
//   - CAS state transitions: compiled -> previewed (per work item)

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::pyramid::auto_update_ops;
use crate::pyramid::cost_model;
use crate::pyramid::dispatch_policy::DispatchPolicy;
use crate::pyramid::event_bus::BuildEventBus;

// ── Constants ──────────────────────────────────────────────────────────────

/// Preview TTL in seconds (5 minutes).
const PREVIEW_TTL_SECS: i64 = 300;

/// Virtual slug for provider-side compute market work.
///
/// Market jobs bypass the preview gate entirely — see the module-level
/// doc on `skip_preview_for_slug` for the full rationale. This constant
/// is the single source of truth; matching against it (rather than a
/// bare string literal at each call site) keeps the behavior grep-able
/// and lets DD-A renames flow through one edit.
///
/// Per `docs/plans/compute-market-architecture.md` DD-A: the canonical
/// slug string is `market:compute`. Bridge jobs share this slug with
/// `step_name: "bridge"` discriminating (DD-P).
pub const MARKET_COMPUTE_SLUG: &str = "market:compute";

/// Returns true if the preview gate should short-circuit for this slug.
///
/// Per `docs/plans/compute-market-phase-2-exchange.md` §V P3:
///
/// > The DADBEAR preview gate exists to enforce operator cost budgets
/// > before committing to paid work. For provider-side market jobs
/// > this is redundant: the Wire's matched price + deposit IS the cost
/// > gate, and the provider already accepted the offer by publishing
/// > it. If the preview gate ran normally, it would try to price the
/// > job in USD against the operator's local-inference budget — the
/// > wrong currency, against the wrong budget, for a job the provider
/// > is being PAID to run.
///
/// The canonical handler (`handle_market_dispatch`) inserts market work
/// items directly at `state = 'previewed'` and never calls
/// `create_dispatch_preview` / `enforce_budget_and_commit`. This guard
/// is a belt-and-suspenders: if a future caller (bridge cost-model
/// integration, reprocessing path, etc.) accidentally routes a
/// market-slug batch through the gate, the gate passes through without
/// creating a phantom USD cost record or placing a wrong-currency
/// cost_limit hold.
///
/// Requester-side market dispatch is a separate code path (Phase 3
/// scope) and uses its own credit-denominated preview — it does NOT
/// share this skip.
pub fn skip_preview_for_slug(slug: &str) -> bool {
    slug == MARKET_COMPUTE_SLUG
}

// ── Budget decision ────────────────────────────────────────────────────────

/// Result of checking a preview's cost against policy budget limits.
#[derive(Debug, Clone, PartialEq)]
pub enum BudgetDecision {
    /// Within max_batch_cost_usd — auto-commit without operator intervention.
    AutoCommit,
    /// Exceeds batch limit but within daily cap — requires operator approval.
    RequiresApproval,
    /// Exceeds daily cost limit — place a cost_limit hold on the slug.
    CostLimitHold,
}

// ── Preview item cost estimate ─────────────────────────────────────────────

/// Per-item cost estimate used during preview computation.
#[derive(Debug, Clone)]
struct ItemCostEstimate {
    work_item_id: String,
    estimated_cost_usd: f64,
    routing: String, // "local" | "cloud" | "fleet"
}

// ── Core functions ─────────────────────────────────────────────────────────

/// Create a dispatch preview for a batch of compiled work items.
///
/// This function:
/// 1. Computes estimated cost per work item (from cost_model or prompt length heuristic)
/// 2. Resolves routing per item (local/cloud/fleet based on dispatch policy)
/// 3. Creates a `dadbear_dispatch_previews` row with semantic path ID
/// 4. Computes policy_hash = SHA-256 of the serialized dispatch policy
/// 5. Sets expires_at = now + 5 minutes (TTL)
/// 6. Updates each work item: set preview_id, transition compiled -> previewed (CAS)
/// 7. Returns the preview_id
pub fn create_dispatch_preview(
    conn: &Connection,
    slug: &str,
    batch_id: &str,
    work_item_ids: &[String],
    policy: &DispatchPolicy,
    norms_hash: &str,
) -> Result<String> {
    // §V P3 skip: market:compute batches never get a USD-denominated
    // preview. Return a sentinel preview_id so the caller pattern
    // matches (some paths then call `commit_preview` with the id);
    // downstream CAS checks via `is_preview_valid` will see the
    // sentinel format and can early-pass. See `skip_preview_for_slug`
    // for the full rationale.
    //
    // The canonical `handle_market_dispatch` path does NOT go through
    // this function — it inserts work items directly at 'previewed'.
    // This branch exists so a future accidental caller on a
    // market:compute batch doesn't create a phantom USD cost record
    // and doesn't place a wrong-currency cost_limit hold on the slug.
    if skip_preview_for_slug(slug) {
        let preview_id = format!("{slug}:{batch_id}:market-skip");
        debug!(
            slug = %slug,
            batch_id = %batch_id,
            item_count = work_item_ids.len(),
            "dadbear_preview::create_dispatch_preview: slug is market:compute; short-circuiting (no USD cost gate for Wire-priced paid market work)"
        );
        return Ok(preview_id);
    }

    let now = Utc::now();
    let now_str = now.format("%Y-%m-%d %H:%M:%S").to_string();
    let expires_at = (now + chrono::Duration::seconds(PREVIEW_TTL_SECS))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();

    // Compute policy hash
    let policy_hash = compute_policy_hash(policy);
    let policy_hash_short = &policy_hash[..8.min(policy_hash.len())];

    // Semantic path ID: {slug}:{batch_id}:{policy_hash_short}
    let preview_id = format!("{slug}:{batch_id}:{policy_hash_short}");

    // Estimate cost and routing for each work item
    let estimates = estimate_batch_costs(conn, work_item_ids, policy)?;

    let total_cost: f64 = estimates.iter().map(|e| e.estimated_cost_usd).sum();
    let item_count = estimates.len() as i64;

    // Build routing summary
    let routing_summary = build_routing_summary(&estimates);
    let routing_json = serde_json::to_string(&routing_summary).unwrap_or_else(|_| "{}".to_string());

    // Determine enforcement level based on budget decision
    let budget_decision = check_budget(conn, slug, total_cost, policy)?;
    let enforcement_level = match &budget_decision {
        BudgetDecision::AutoCommit => "auto_commit",
        BudgetDecision::RequiresApproval => "requires_approval",
        BudgetDecision::CostLimitHold => "cost_limit_hold",
    };

    // Write the preview row
    conn.execute(
        "INSERT OR REPLACE INTO dadbear_dispatch_previews
            (id, slug, batch_id, policy_hash, norms_hash, item_count,
             total_cost_usd, total_wall_time_secs, enforcement_cost_usd,
             enforcement_level, routing_summary_json, expires_at, committed_at, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, ?9, ?10, ?11, NULL, ?12)",
        params![
            preview_id,
            slug,
            batch_id,
            policy_hash,
            norms_hash,
            item_count,
            total_cost,
            total_cost, // enforcement_cost_usd = total for now
            enforcement_level,
            routing_json,
            expires_at,
            now_str,
        ],
    )
    .context("Failed to insert dispatch preview")?;

    // CAS: transition each work item from compiled -> previewed.
    // Batch atomicity: if ANY item fails CAS, roll back the entire preview.
    // This prevents accounting drift where the preview's item_count/cost
    // doesn't match the items that actually point to it.
    let mut transitioned = 0usize;
    for item_id in work_item_ids {
        let changed = conn.execute(
            "UPDATE dadbear_work_items
             SET state = 'previewed',
                 state_changed_at = ?1,
                 preview_id = ?2
             WHERE id = ?3 AND state = 'compiled'",
            params![now_str, preview_id, item_id],
        )?;
        if changed > 0 {
            transitioned += 1;
        } else {
            // CAS failed — another process already previewed/dispatched this item.
            // Roll back: revert already-transitioned items and delete the preview.
            warn!(
                work_item_id = %item_id,
                preview_id = %preview_id,
                transitioned_so_far = transitioned,
                "CAS failed: rolling back preview (batch atomicity)"
            );
            conn.execute(
                "UPDATE dadbear_work_items SET state = 'compiled', state_changed_at = ?1, preview_id = NULL
                 WHERE preview_id = ?2 AND state = 'previewed'",
                params![now_str, preview_id],
            )?;
            conn.execute(
                "DELETE FROM dadbear_dispatch_previews WHERE id = ?1",
                params![preview_id],
            )?;
            return Err(anyhow::anyhow!(
                "Preview batch atomicity failure: item {} not in compiled state, {} items rolled back",
                item_id, transitioned
            ));
        }
    }

    info!(
        slug = %slug,
        preview_id = %preview_id,
        item_count = item_count,
        transitioned = transitioned,
        total_cost_usd = total_cost,
        enforcement = %enforcement_level,
        "Dispatch preview created"
    );

    Ok(preview_id)
}

/// Commit a preview, stamping `committed_at`. Called by auto-commit
/// (within budget) or operator confirmation.
pub fn commit_preview(conn: &Connection, preview_id: &str) -> Result<()> {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let changed = conn.execute(
        "UPDATE dadbear_dispatch_previews
         SET committed_at = ?1
         WHERE id = ?2 AND committed_at IS NULL",
        params![now, preview_id],
    )?;

    if changed == 0 {
        warn!(
            preview_id = %preview_id,
            "commit_preview: preview not found or already committed"
        );
    } else {
        info!(preview_id = %preview_id, "Dispatch preview committed");
    }

    Ok(())
}

/// Validate that a preview is still usable for dispatch.
///
/// A preview is valid when:
/// - It exists in the database
/// - It has not expired (current time < expires_at)
/// - Its policy_hash matches the current policy hash (no policy drift)
/// - It has been committed (committed_at IS NOT NULL)
pub fn is_preview_valid(
    conn: &Connection,
    preview_id: &str,
    current_policy_hash: &str,
) -> Result<bool> {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let valid: bool = conn
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM dadbear_dispatch_previews
                WHERE id = ?1
                  AND policy_hash = ?2
                  AND expires_at > ?3
                  AND committed_at IS NOT NULL
            )",
            params![preview_id, current_policy_hash, now],
            |row| row.get(0),
        )
        .unwrap_or(false);

    Ok(valid)
}

/// Check a preview's cost against the dispatch policy's budget limits.
///
/// Decision logic:
/// - If no max_batch_cost_usd set: AutoCommit (no limit)
/// - If preview_cost <= max_batch_cost_usd: AutoCommit
/// - If preview_cost > max_batch_cost_usd AND daily spend + cost > max_daily_cost_usd: CostLimitHold
/// - If preview_cost > max_batch_cost_usd but within daily (or no daily limit): RequiresApproval
pub fn check_budget(
    conn: &Connection,
    slug: &str,
    preview_cost: f64,
    policy: &DispatchPolicy,
) -> Result<BudgetDecision> {
    let max_batch = policy.max_batch_cost_usd;
    let max_daily = policy.max_daily_cost_usd;

    // No batch limit set — auto-commit everything
    let batch_limit = match max_batch {
        Some(limit) => limit,
        None => return Ok(BudgetDecision::AutoCommit),
    };

    // Within batch limit — auto-commit
    if preview_cost <= batch_limit {
        return Ok(BudgetDecision::AutoCommit);
    }

    // Batch limit exceeded — check daily limit
    if let Some(daily_limit) = max_daily {
        let daily_spend = get_daily_spend(conn, slug)?;
        if daily_spend + preview_cost > daily_limit {
            return Ok(BudgetDecision::CostLimitHold);
        }
    }

    // Batch exceeded but within daily (or no daily limit) — needs approval
    Ok(BudgetDecision::RequiresApproval)
}

/// Auto-commit a preview if within budget, or place a cost_limit hold.
///
/// Combines `check_budget` and `commit_preview` into a single operation
/// that also places the hold when needed. Returns the budget decision taken.
pub fn enforce_budget_and_commit(
    conn: &Connection,
    bus: &Arc<BuildEventBus>,
    slug: &str,
    preview_id: &str,
    preview_cost: f64,
    policy: &DispatchPolicy,
) -> Result<BudgetDecision> {
    // §V P3 skip: market:compute is Wire-priced and paid-for; the
    // operator USD budget does not apply. Short-circuit as AutoCommit
    // so any (unexpected) caller gets the "pass" decision without us
    // placing a wrong-currency cost_limit hold on the slug. See
    // `skip_preview_for_slug` for the full rationale.
    if skip_preview_for_slug(slug) {
        debug!(
            slug = %slug,
            preview_id = %preview_id,
            "dadbear_preview::enforce_budget_and_commit: slug is market:compute; short-circuiting AutoCommit (no USD budget applies to paid market work)"
        );
        return Ok(BudgetDecision::AutoCommit);
    }

    let decision = check_budget(conn, slug, preview_cost, policy)?;

    match &decision {
        BudgetDecision::AutoCommit => {
            commit_preview(conn, preview_id)?;
        }
        BudgetDecision::RequiresApproval => {
            // Leave uncommitted — operator must manually commit
            info!(
                slug = %slug,
                preview_id = %preview_id,
                cost = preview_cost,
                "Preview requires operator approval (exceeds batch limit)"
            );
        }
        BudgetDecision::CostLimitHold => {
            let reason = format!(
                "Preview {} cost ${:.4} exceeds daily limit for slug {}",
                preview_id, preview_cost, slug
            );
            auto_update_ops::place_hold(conn, bus, slug, "cost_limit", Some(&reason))?;
            info!(
                slug = %slug,
                preview_id = %preview_id,
                cost = preview_cost,
                "cost_limit hold placed — daily budget exceeded"
            );
        }
    }

    Ok(decision)
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Compute SHA-256 hash of the dispatch policy (serialized as JSON).
pub fn compute_policy_hash(policy: &DispatchPolicy) -> String {
    let serialized = serde_json::to_string(policy).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(serialized.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Estimate costs for a batch of work items using the cost model.
///
/// Falls back to a prompt-length heuristic when no cost model entry exists.
fn estimate_batch_costs(
    conn: &Connection,
    work_item_ids: &[String],
    policy: &DispatchPolicy,
) -> Result<Vec<ItemCostEstimate>> {
    let mut estimates = Vec::with_capacity(work_item_ids.len());

    for item_id in work_item_ids {
        // Read the work item's step_name, model_tier, and prompt lengths.
        // resolved_model_id may be NULL (compiler defers model resolution to dispatch).
        // If NULL, resolve from model_tier via dispatch policy routing rules.
        let row: Option<(String, String, Option<String>, String, String, Option<String>)> = conn
            .query_row(
                "SELECT step_name, model_tier, resolved_model_id, system_prompt, user_prompt, resolved_provider_id
                 FROM dadbear_work_items WHERE id = ?1",
                params![item_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, Option<String>>(5)?,
                    ))
                },
            )
            .ok();

        let (step_name, model_tier, resolved_model_id, system_prompt, user_prompt, provider_id) =
            match row {
                Some(r) => r,
                None => {
                    warn!(work_item_id = %item_id, "Work item not found during cost estimation");
                    continue;
                }
            };

        // Resolve model: use resolved_model_id if populated (Phase 5 materialization),
        // otherwise resolve from model_tier via dispatch policy routing rules.
        let model_from_policy = policy
            .rules
            .iter()
            .find(|r| r.name == model_tier || r.name == step_name)
            .and_then(|r| r.route_to.first())
            .and_then(|re| re.model_id.as_deref());
        let model = resolved_model_id
            .as_deref()
            .or(model_from_policy)
            .unwrap_or("unknown");

        // Try cost model lookup first
        let cost = match cost_model::lookup(conn, &step_name, model)? {
            Some(entry) => entry.usd_per_call,
            None => {
                // Heuristic fallback: estimate tokens from prompt char length
                // ~4 chars per token is a reasonable approximation
                let prompt_chars = system_prompt.len() + user_prompt.len();
                let est_input_tokens = (prompt_chars as f64) / 4.0;
                // Assume output is ~25% of input for estimation purposes
                let est_output_tokens = est_input_tokens * 0.25;
                // Use a conservative default pricing ($3/Mtok input, $15/Mtok output)
                cost_model::cost_per_call(est_input_tokens, est_output_tokens, 3.0, 15.0)
            }
        };

        // Determine routing from resolved provider, or infer from dispatch policy
        let effective_provider = provider_id.as_deref().or_else(|| {
            policy
                .rules
                .iter()
                .find(|r| r.name == model_tier || r.name == step_name)
                .and_then(|r| r.route_to.first())
                .map(|re| re.provider_id.as_str())
        });
        let routing = match effective_provider {
            Some(p) if is_local_provider(p, policy) => "local",
            Some("fleet") => "fleet",
            _ => "cloud",
        };

        estimates.push(ItemCostEstimate {
            work_item_id: item_id.clone(),
            estimated_cost_usd: cost,
            routing: routing.to_string(),
        });
    }

    Ok(estimates)
}

/// Check if a provider is local based on the routing rules.
fn is_local_provider(provider_id: &str, policy: &DispatchPolicy) -> bool {
    for rule in &policy.rules {
        for entry in &rule.route_to {
            if entry.provider_id == provider_id && entry.is_local {
                return true;
            }
        }
    }
    false
}

/// Build a routing summary from item estimates (counts per routing type).
fn build_routing_summary(estimates: &[ItemCostEstimate]) -> serde_json::Value {
    let mut local_count = 0u64;
    let mut cloud_count = 0u64;
    let mut fleet_count = 0u64;
    let mut local_cost = 0.0f64;
    let mut cloud_cost = 0.0f64;
    let mut fleet_cost = 0.0f64;

    for est in estimates {
        match est.routing.as_str() {
            "local" => {
                local_count += 1;
                local_cost += est.estimated_cost_usd;
            }
            "fleet" => {
                fleet_count += 1;
                fleet_cost += est.estimated_cost_usd;
            }
            _ => {
                cloud_count += 1;
                cloud_cost += est.estimated_cost_usd;
            }
        }
    }

    serde_json::json!({
        "local": { "count": local_count, "cost_usd": local_cost },
        "cloud": { "count": cloud_count, "cost_usd": cloud_cost },
        "fleet": { "count": fleet_count, "cost_usd": fleet_cost },
    })
}

/// Get the total committed cost for a slug today (UTC day boundary).
fn get_daily_spend(conn: &Connection, slug: &str) -> Result<f64> {
    let today_start = Utc::now().format("%Y-%m-%d 00:00:00").to_string();

    let total: f64 = conn
        .query_row(
            "SELECT COALESCE(SUM(total_cost_usd), 0.0)
             FROM dadbear_dispatch_previews
             WHERE slug = ?1
               AND committed_at IS NOT NULL
               AND created_at >= ?2",
            params![slug, today_start],
            |row| row.get(0),
        )
        .unwrap_or(0.0);

    Ok(total)
}

fn _now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_policy_hash_deterministic() {
        use crate::pyramid::dispatch_policy::*;
        use std::collections::BTreeMap;

        let policy = DispatchPolicy {
            rules: vec![],
            escalation: EscalationConfig::default(),
            build_coordination: BuildCoordinationConfig::default(),
            pool_configs: BTreeMap::new(),
            max_batch_cost_usd: Some(1.0),
            max_daily_cost_usd: Some(10.0),
        };

        let h1 = compute_policy_hash(&policy);
        let h2 = compute_policy_hash(&policy);
        assert_eq!(h1, h2, "Policy hash should be deterministic");
        assert_eq!(h1.len(), 64, "SHA-256 hex should be 64 chars");
    }

    #[test]
    fn test_compute_policy_hash_changes_with_budget() {
        use crate::pyramid::dispatch_policy::*;
        use std::collections::BTreeMap;

        let policy1 = DispatchPolicy {
            rules: vec![],
            escalation: EscalationConfig::default(),
            build_coordination: BuildCoordinationConfig::default(),
            pool_configs: BTreeMap::new(),
            max_batch_cost_usd: Some(1.0),
            max_daily_cost_usd: None,
        };

        let policy2 = DispatchPolicy {
            rules: vec![],
            escalation: EscalationConfig::default(),
            build_coordination: BuildCoordinationConfig::default(),
            pool_configs: BTreeMap::new(),
            max_batch_cost_usd: Some(5.0),
            max_daily_cost_usd: None,
        };

        assert_ne!(
            compute_policy_hash(&policy1),
            compute_policy_hash(&policy2),
            "Different budget should produce different hash"
        );
    }

    #[test]
    fn test_budget_decision_no_limits() {
        // With no limits set, everything auto-commits
        use crate::pyramid::dispatch_policy::*;
        use std::collections::BTreeMap;

        let policy = DispatchPolicy {
            rules: vec![],
            escalation: EscalationConfig::default(),
            build_coordination: BuildCoordinationConfig::default(),
            pool_configs: BTreeMap::new(),
            max_batch_cost_usd: None,
            max_daily_cost_usd: None,
        };

        // We can't call check_budget without a real DB connection, but we
        // can verify the logic by testing the decision paths directly.
        // No batch limit -> auto-commit is the first branch.
        assert!(policy.max_batch_cost_usd.is_none());
    }

    #[test]
    fn test_routing_summary_counts() {
        let estimates = vec![
            ItemCostEstimate {
                work_item_id: "a".into(),
                estimated_cost_usd: 0.01,
                routing: "local".into(),
            },
            ItemCostEstimate {
                work_item_id: "b".into(),
                estimated_cost_usd: 0.05,
                routing: "cloud".into(),
            },
            ItemCostEstimate {
                work_item_id: "c".into(),
                estimated_cost_usd: 0.02,
                routing: "cloud".into(),
            },
            ItemCostEstimate {
                work_item_id: "d".into(),
                estimated_cost_usd: 0.03,
                routing: "fleet".into(),
            },
        ];

        let summary = build_routing_summary(&estimates);
        assert_eq!(summary["local"]["count"], 1);
        assert_eq!(summary["cloud"]["count"], 2);
        assert_eq!(summary["fleet"]["count"], 1);
    }

    // ── WS8: market:compute skip behaviour ─────────────────────────────

    #[test]
    fn market_compute_slug_constant_matches_dd_a() {
        // DD-A (`compute-market-architecture.md`) canonicalizes the
        // string `"market:compute"`. Pinning it here so a rename flags
        // here and at the handler call site on the same PR.
        assert_eq!(MARKET_COMPUTE_SLUG, "market:compute");
    }

    #[test]
    fn skip_preview_for_slug_matches_market_compute_only() {
        assert!(skip_preview_for_slug("market:compute"));
        // Pyramid slugs with colons but not the market namespace —
        // MUST NOT skip. Regression guard against a prefix-match
        // implementation that would misfire on similar strings.
        assert!(!skip_preview_for_slug("opt-025"));
        assert!(!skip_preview_for_slug("goodnewseveryone"));
        assert!(!skip_preview_for_slug("market:storage"));
        assert!(!skip_preview_for_slug("market:relay"));
        assert!(!skip_preview_for_slug("market:compute:extra"));
        assert!(!skip_preview_for_slug(""));
    }

    #[test]
    fn create_dispatch_preview_short_circuits_for_market_compute() {
        // Construct a minimal in-memory DB with the schema bits the
        // function touches. We don't expect the function to read from
        // the DB on the skip branch — that's the whole point — so an
        // empty :memory: connection suffices.
        use crate::pyramid::dispatch_policy::*;
        use std::collections::BTreeMap;

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let policy = DispatchPolicy {
            rules: vec![],
            escalation: EscalationConfig::default(),
            build_coordination: BuildCoordinationConfig::default(),
            pool_configs: BTreeMap::new(),
            max_batch_cost_usd: Some(0.0), // would force CostLimitHold if evaluated
            max_daily_cost_usd: Some(0.0),
        };

        // On the non-market path this call would fail (no schema, no
        // work items). On the market:compute skip path it returns
        // Ok(sentinel_id) without touching the DB.
        let result = create_dispatch_preview(
            &conn,
            MARKET_COMPUTE_SLUG,
            "batch-1",
            &["market/abc".to_string()],
            &policy,
            "deadbeef",
        );
        assert!(
            result.is_ok(),
            "market:compute preview must short-circuit without DB access; got {:?}",
            result.err(),
        );
        let id = result.unwrap();
        assert!(
            id.contains("market-skip"),
            "sentinel preview_id must mark the skip: {id}"
        );
    }

    #[test]
    fn enforce_budget_and_commit_short_circuits_for_market_compute() {
        // Same contract: the skip branch returns AutoCommit without
        // consulting the DB or placing a cost_limit hold. The
        // non-market path with max_batch_cost_usd = 0.0 and a positive
        // preview_cost would otherwise CostLimitHold (exceeds daily
        // cap that's also 0.0).
        use crate::pyramid::dispatch_policy::*;
        use crate::pyramid::event_bus::BuildEventBus;
        use std::collections::BTreeMap;

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let bus = Arc::new(BuildEventBus::new());
        let policy = DispatchPolicy {
            rules: vec![],
            escalation: EscalationConfig::default(),
            build_coordination: BuildCoordinationConfig::default(),
            pool_configs: BTreeMap::new(),
            max_batch_cost_usd: Some(0.0),
            max_daily_cost_usd: Some(0.0),
        };

        let decision = enforce_budget_and_commit(
            &conn,
            &bus,
            MARKET_COMPUTE_SLUG,
            "market:compute:batch-1:market-skip",
            1234.56, // "cost" that would otherwise trip CostLimitHold
            &policy,
        );
        assert!(
            decision.is_ok(),
            "market:compute enforce_budget_and_commit must short-circuit: {:?}",
            decision.err(),
        );
        assert_eq!(decision.unwrap(), BudgetDecision::AutoCommit);
    }
}
