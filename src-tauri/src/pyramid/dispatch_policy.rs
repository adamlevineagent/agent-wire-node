// dispatch_policy.rs — Contribution-governed LLM dispatch policy.
//
// Defines WorkType, RoutingRule, ResolvedRoute, and the DispatchPolicy
// that maps work metadata to provider preference chains. Loaded from
// the `dispatch_policy` contribution schema type. Resolved at the entry
// of all three LLM call paths in llm.rs.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

// ── Work classification ─────────────────────────────────────────────────────

/// The kind of work an LLM call is performing. Used by routing rules to
/// direct calls to different providers/models based on cost/latency tradeoffs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkType {
    Build,
    Maintenance,
    Evidence,
    Faq,
    Interactive,
    Speculative,
}

// ── Route entry ─────────────────────────────────────────────────────────────

/// Effective "no budget cap" sentinel for [`RouteEntry::max_budget_credits`].
///
/// Value: `2^53 - 1` = `Number.MAX_SAFE_INTEGER`. f64-safe round-trip through
/// JSON — Wire's `/quote` handler (TypeScript) parses `max_budget` as Number,
/// which tops out at this value without precision loss. Larger i64 values
/// corrupt on deserialize.
///
/// `estimated_total = input_tokens * rate_in_per_m / 1M + output_tokens * rate_out_per_m / 1M`
/// is bounded well under this sentinel for any realistic call. Wire's 409
/// `budget_exceeded` thus never fires when `max_budget` is set to this value
/// — by design: the sentinel means "no operator-imposed cap."
pub const NO_BUDGET_CAP: i64 = (1i64 << 53) - 1;

/// A single provider+model in a routing preference chain.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RouteEntry {
    pub provider_id: String,
    /// If set, overrides the provider's default model for this route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// Optional tier name for context_limit/pricing lookups.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier_name: Option<String>,
    /// True for providers that run on local hardware (Ollama, local GPU).
    /// Used by fleet to determine which rules this node can serve.
    #[serde(default)]
    pub is_local: bool,
    /// Optional per-entry credit ceiling fed to Wire's `/quote` `max_budget`
    /// field. `None` → use [`NO_BUDGET_CAP`] (no operator-imposed cap; Wire's
    /// 409 `budget_exceeded` won't fire for this entry). `Some(n)` → Wire
    /// rejects the quote if its estimated total exceeds n, walker advances.
    /// Settings panel (Wave 4) exposes this as "optional credit ceiling."
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_budget_credits: Option<i64>,
}

// ── Match config ────────────────────────────────────────────────────────────

/// Predicate set for a routing rule. All present fields must match (AND).
/// Absent fields are wildcards (match everything).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_type: Option<WorkType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_depth: Option<i64>,
    /// Glob-like pattern for step name matching. Only trailing `*` is supported
    /// (prefix match). e.g. `"summarize_*"` matches any step starting with
    /// `"summarize_"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_pattern: Option<String>,
}

// ── Routing rule ────────────────────────────────────────────────────────────

/// A single routing rule: if match_config matches the call metadata,
/// route to the given provider preference chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingRule {
    pub name: String,
    pub match_config: MatchConfig,
    pub route_to: Vec<RouteEntry>,
    /// If true, skip pool semaphore acquisition for this route.
    #[serde(default)]
    pub bypass_pool: bool,
    /// If true, calls matching this rule are serialized (one at a time).
    #[serde(default)]
    pub sequential: bool,
}

// ── Escalation config ───────────────────────────────────────────────────────

/// Controls how long to wait before escalating to the next provider in the
/// preference chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationConfig {
    /// Seconds to wait on a single provider before trying the next.
    #[serde(default = "default_wait_timeout_secs")]
    pub wait_timeout_secs: u64,
    /// Absolute maximum wait across all providers in the chain.
    #[serde(default = "default_max_wait_secs")]
    pub max_wait_secs: u64,
}

fn default_wait_timeout_secs() -> u64 {
    30
}
/// Default wall-clock ceiling for a single dispatched LLM job, in seconds.
///
/// 3600s = 1 hour. Prior default was 300s (5 minutes), which was too short
/// for the long-tail of inference calls. In practice most calls complete in
/// ~10 seconds, but occasional large-output calls run for many minutes —
/// sometimes ~20 — especially on local fleet peers under load. Because the
/// async dispatch path cannot cancel in-flight remote work, timing out at
/// 5 minutes meant we paid the full cost of the work, then rejected its
/// result when the late callback arrived (see `fleet_result_orphaned` in
/// the Chronicle). Defaulting high and letting jobs take as long as they
/// take is the correct posture. Operators who want tighter ceilings can
/// override via the `dispatch_policy` YAML contribution per route / tier.
fn default_max_wait_secs() -> u64 {
    3600
}

impl Default for EscalationConfig {
    fn default() -> Self {
        Self {
            wait_timeout_secs: default_wait_timeout_secs(),
            max_wait_secs: default_max_wait_secs(),
        }
    }
}

// ── Build coordination ──────────────────────────────────────────────────────

/// Controls how builds interact with other subsystems for resource sharing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildCoordinationConfig {
    /// When true, folder builds run one at a time instead of concurrently.
    #[serde(default)]
    pub folder_builds_sequential: bool,
    /// When true, maintenance (stale checks) is deferred while a build is active.
    #[serde(default)]
    pub defer_maintenance_during_build: bool,
    /// When true, DADBEAR auto-update is deferred while a build is active.
    #[serde(default)]
    pub defer_dadbear_during_build: bool,
}

impl Default for BuildCoordinationConfig {
    fn default() -> Self {
        Self {
            folder_builds_sequential: false,
            defer_maintenance_during_build: false,
            defer_dadbear_during_build: false,
        }
    }
}

// ── Provider pool YAML config ───────────────────────────────────────────────

/// Per-provider concurrency and rate-limit configuration as it appears in YAML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderPoolConfig {
    pub concurrency: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<RateLimitConfig>,
}

/// Sliding-window rate limit parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    pub max_requests: usize,
    pub window_secs: f64,
}

// ── YAML top-level shape ────────────────────────────────────────────────────

/// The raw YAML shape of a `dispatch_policy` contribution body.
/// Deserialized directly from the contribution content, then converted
/// to the runtime `DispatchPolicy` via `DispatchPolicy::from_yaml`.
#[derive(Debug, Clone, Deserialize)]
pub struct DispatchPolicyYaml {
    pub version: u32,
    #[serde(default)]
    pub provider_pools: HashMap<String, ProviderPoolConfig>,
    #[serde(default)]
    pub routing_rules: Vec<RoutingRule>,
    #[serde(default)]
    pub escalation: Option<EscalationConfig>,
    #[serde(default)]
    pub build_coordination: Option<BuildCoordinationConfig>,
    /// Auto-commit ceiling per batch (USD). Batches within this cost are
    /// committed without operator confirmation. Batches exceeding it
    /// require manual approval or place a cost_limit hold.
    #[serde(default)]
    pub max_batch_cost_usd: Option<f64>,
    /// Daily cost cap per slug (USD). When cumulative daily spend plus
    /// the preview cost exceeds this, a cost_limit hold is placed.
    #[serde(default)]
    pub max_daily_cost_usd: Option<f64>,
}

// ── Runtime dispatch policy ─────────────────────────────────────────────────

/// The fully-resolved runtime dispatch policy. Built from `DispatchPolicyYaml`
/// and held in the server state. Hot-reloaded when the contribution changes.
#[derive(Debug, Clone, Serialize)]
pub struct DispatchPolicy {
    pub rules: Vec<RoutingRule>,
    pub escalation: EscalationConfig,
    pub build_coordination: BuildCoordinationConfig,
    pub pool_configs: BTreeMap<String, ProviderPoolConfig>,
    /// Auto-commit ceiling per batch (USD). None = no limit (auto-commit all).
    pub max_batch_cost_usd: Option<f64>,
    /// Daily cost cap per slug (USD). None = no daily limit.
    pub max_daily_cost_usd: Option<f64>,
}

// ── Resolved route ──────────────────────────────────────────────────────────

/// The result of resolving a work call against the dispatch policy.
/// Contains the ordered provider preference chain and execution parameters.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedRoute {
    pub providers: Vec<RouteEntry>,
    pub bypass_pool: bool,
    pub sequential_rule_name: Option<String>,
    /// The name of the routing rule that matched. Empty string when no rule matched.
    pub matched_rule_name: String,
    pub escalation_timeout_secs: u64,
    pub max_wait_secs: u64,
}

// ── Local Mode overlay ──────────────────────────────────────────────────────

/// Apply the Local Mode overlay to a parsed dispatch_policy YAML.
///
/// Local Mode is a runtime toggle, not a config substitution: the operator's
/// authored `dispatch_policy` contribution is never mutated. When enabled, we
/// derive a filtered view at load time by keeping only `is_local: true`
/// entries in each rule's `route_to` list, dropping rules that become empty,
/// and pinning `build_coordination.defer_maintenance_during_build = true`.
/// When disabled, this is a no-op and returns the YAML unchanged.
///
/// Why here and not in the walker: the walker iterates `route.providers`
/// once per LLM call and is out of scope for this fix (see the PR description
/// for the scope fence). The ConfigSynced listener loads dispatch_policy into
/// `cfg.dispatch_policy` on boot and on every contribution update — applying
/// the overlay at that single choke point means every downstream reader
/// (walker, fleet serving-rule derivation, /status UI) sees the effective
/// policy with zero additional code.
pub fn apply_local_mode_overlay(
    yaml: DispatchPolicyYaml,
    local_mode_enabled: bool,
) -> DispatchPolicyYaml {
    if !local_mode_enabled {
        return yaml;
    }

    let DispatchPolicyYaml {
        version,
        provider_pools,
        routing_rules,
        escalation,
        build_coordination,
        max_batch_cost_usd,
        max_daily_cost_usd,
    } = yaml;

    let filtered_rules: Vec<RoutingRule> = routing_rules
        .into_iter()
        .filter_map(|mut rule| {
            rule.route_to.retain(|entry| entry.is_local);
            if rule.route_to.is_empty() {
                None
            } else {
                Some(rule)
            }
        })
        .collect();

    // Force defer_maintenance_during_build = true when Local Mode is on:
    // a single local GPU cannot share cycles with background stale checks
    // without starving the focused build. Other build_coordination fields
    // carry through unchanged.
    let coordination = {
        let mut c = build_coordination.unwrap_or_default();
        c.defer_maintenance_during_build = true;
        Some(c)
    };

    DispatchPolicyYaml {
        version,
        provider_pools,
        routing_rules: filtered_rules,
        escalation,
        build_coordination: coordination,
        max_batch_cost_usd,
        max_daily_cost_usd,
    }
}

// ── Implementation ──────────────────────────────────────────────────────────

impl DispatchPolicy {
    /// Build the runtime policy from the parsed YAML contribution.
    pub fn from_yaml(yaml: &DispatchPolicyYaml) -> Self {
        Self {
            rules: yaml.routing_rules.clone(),
            escalation: yaml.escalation.clone().unwrap_or_default(),
            build_coordination: yaml.build_coordination.clone().unwrap_or_default(),
            pool_configs: yaml.provider_pools.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            max_batch_cost_usd: yaml.max_batch_cost_usd,
            max_daily_cost_usd: yaml.max_daily_cost_usd,
        }
    }

    /// Resolve by rule name (for fleet dispatch receiving).
    /// Returns the first local provider's (provider_id, model_id),
    /// excluding the walker sentinels `"fleet"` and `"market"` — neither
    /// is a real local handler.
    pub fn resolve_local_for_rule(&self, rule_name: &str) -> Option<(String, Option<String>)> {
        for rule in &self.rules {
            if rule.name == rule_name {
                // Find the first non-sentinel provider with is_local == true.
                // No fallback to cloud providers — if no local provider
                // is found, return None so the fleet handler returns an
                // error (prevents surprise cloud billing on fleet jobs).
                for entry in &rule.route_to {
                    if entry.provider_id != "fleet"
                        && entry.provider_id != "market"
                        && entry.is_local
                    {
                        return Some((entry.provider_id.clone(), entry.model_id.clone()));
                    }
                }
            }
        }
        None
    }

    /// Resolve a work call to a route. Walks rules in order; first match wins.
    ///
    /// - `work_type`: the classification of this LLM call
    /// - `_tier`: reserved for future tier-based routing (currently unused in matching)
    /// - `step_name`: the chain step name (matched against `step_pattern`)
    /// - `depth`: the pyramid layer depth (matched against `min_depth`)
    pub fn resolve_route(
        &self,
        work_type: WorkType,
        _tier: &str,
        step_name: &str,
        depth: Option<i64>,
    ) -> ResolvedRoute {
        for rule in &self.rules {
            if matches_rule(&rule.match_config, work_type, step_name, depth) {
                return ResolvedRoute {
                    providers: rule.route_to.clone(),
                    bypass_pool: rule.bypass_pool,
                    sequential_rule_name: if rule.sequential {
                        Some(rule.name.clone())
                    } else {
                        None
                    },
                    matched_rule_name: rule.name.clone(),
                    escalation_timeout_secs: self.escalation.wait_timeout_secs,
                    max_wait_secs: self.escalation.max_wait_secs,
                };
            }
        }

        // Catch-all default: empty provider list (caller should use its own fallback).
        ResolvedRoute {
            providers: Vec::new(),
            bypass_pool: false,
            sequential_rule_name: None,
            matched_rule_name: String::new(),
            escalation_timeout_secs: self.escalation.wait_timeout_secs,
            max_wait_secs: self.escalation.max_wait_secs,
        }
    }
}

/// Check whether a single rule's match config matches the given call metadata.
/// All present fields must match (AND semantics). Absent fields are wildcards.
fn matches_rule(
    config: &MatchConfig,
    work_type: WorkType,
    step_name: &str,
    depth: Option<i64>,
) -> bool {
    // work_type: must equal if specified
    if let Some(required) = config.work_type {
        if required != work_type {
            return false;
        }
    }

    // min_depth: depth must be present and >= threshold
    if let Some(min) = config.min_depth {
        match depth {
            Some(d) if d >= min => {} // passes
            _ => return false,
        }
    }

    // step_pattern: simple prefix glob (trailing `*` only)
    if let Some(ref pattern) = config.step_pattern {
        if !glob_match_simple(pattern, step_name) {
            return false;
        }
    }

    true
}

/// Simple glob matching: only supports trailing `*` (prefix match).
/// - `"foo_*"` matches any string starting with `"foo_"`
/// - `"*"` matches everything
/// - `"exact"` matches only `"exact"` (exact equality when no `*`)
fn glob_match_simple(pattern: &str, value: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix('*') {
        value.starts_with(prefix)
    } else {
        pattern == value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_local_for_rule_filters_market_sentinel() {
        // Wave 2 task 17: `resolve_local_for_rule` must filter both walker
        // sentinels (`fleet` and `market`). Route = [fleet, market,
        // ollama-local]; only ollama-local should resolve.
        let rule = RoutingRule {
            name: "test-rule".into(),
            match_config: MatchConfig {
                work_type: None,
                min_depth: None,
                step_pattern: None,
            },
            route_to: vec![
                RouteEntry {
                    provider_id: "fleet".into(),
                    model_id: None,
                    tier_name: None,
                    is_local: true,
                    max_budget_credits: None,
                },
                RouteEntry {
                    provider_id: "market".into(),
                    model_id: Some("some-model".into()),
                    tier_name: None,
                    is_local: true,
                    max_budget_credits: None,
                },
                RouteEntry {
                    provider_id: "ollama-local".into(),
                    model_id: Some("llama3".into()),
                    tier_name: None,
                    is_local: true,
                    max_budget_credits: None,
                },
            ],
            bypass_pool: false,
            sequential: false,
        };
        let policy = DispatchPolicy {
            rules: vec![rule],
            escalation: EscalationConfig::default(),
            build_coordination: BuildCoordinationConfig::default(),
            pool_configs: Default::default(),
            max_batch_cost_usd: None,
            max_daily_cost_usd: None,
        };
        let resolved = policy.resolve_local_for_rule("test-rule");
        assert_eq!(
            resolved,
            Some(("ollama-local".to_string(), Some("llama3".to_string()))),
            "market sentinel must be filtered alongside fleet",
        );
    }

    #[test]
    fn test_glob_match_simple() {
        assert!(glob_match_simple("*", "anything"));
        assert!(glob_match_simple("summarize_*", "summarize_layer"));
        assert!(glob_match_simple("summarize_*", "summarize_"));
        assert!(!glob_match_simple("summarize_*", "characterize_layer"));
        assert!(glob_match_simple("exact", "exact"));
        assert!(!glob_match_simple("exact", "exact_not"));
    }

    #[test]
    fn test_matches_rule_wildcard() {
        let config = MatchConfig {
            work_type: None,
            min_depth: None,
            step_pattern: None,
        };
        assert!(matches_rule(&config, WorkType::Build, "any_step", Some(3)));
    }

    #[test]
    fn test_matches_rule_work_type_filter() {
        let config = MatchConfig {
            work_type: Some(WorkType::Build),
            min_depth: None,
            step_pattern: None,
        };
        assert!(matches_rule(&config, WorkType::Build, "step", None));
        assert!(!matches_rule(&config, WorkType::Interactive, "step", None));
    }

    #[test]
    fn test_matches_rule_depth_filter() {
        let config = MatchConfig {
            work_type: None,
            min_depth: Some(3),
            step_pattern: None,
        };
        assert!(matches_rule(&config, WorkType::Build, "step", Some(3)));
        assert!(matches_rule(&config, WorkType::Build, "step", Some(5)));
        assert!(!matches_rule(&config, WorkType::Build, "step", Some(2)));
        assert!(!matches_rule(&config, WorkType::Build, "step", None));
    }

    #[test]
    fn test_resolve_route_first_match_wins() {
        let policy = DispatchPolicy {
            rules: vec![
                RoutingRule {
                    name: "build_local".into(),
                    match_config: MatchConfig {
                        work_type: Some(WorkType::Build),
                        min_depth: None,
                        step_pattern: None,
                    },
                    route_to: vec![RouteEntry {
                        provider_id: "ollama".into(),
                        model_id: None,
                        tier_name: None,
                        is_local: false,
                        max_budget_credits: None,
                    }],
                    bypass_pool: false,
                    sequential: true,
                },
                RoutingRule {
                    name: "catch_all".into(),
                    match_config: MatchConfig {
                        work_type: None,
                        min_depth: None,
                        step_pattern: None,
                    },
                    route_to: vec![RouteEntry {
                        provider_id: "openrouter".into(),
                        model_id: None,
                        tier_name: None,
                        is_local: false,
                        max_budget_credits: None,
                    }],
                    bypass_pool: false,
                    sequential: false,
                },
            ],
            escalation: EscalationConfig::default(),
            build_coordination: BuildCoordinationConfig::default(),
            pool_configs: BTreeMap::new(),
            max_batch_cost_usd: None,
            max_daily_cost_usd: None,
        };

        let route = policy.resolve_route(WorkType::Build, "primary", "summarize_l2", None);
        assert_eq!(route.providers.len(), 1);
        assert_eq!(route.providers[0].provider_id, "ollama");
        assert_eq!(route.sequential_rule_name, Some("build_local".into()));

        let route = policy.resolve_route(WorkType::Interactive, "primary", "chat", None);
        assert_eq!(route.providers[0].provider_id, "openrouter");
        assert!(route.sequential_rule_name.is_none());
    }

    #[test]
    fn test_resolve_route_no_match_returns_empty() {
        let policy = DispatchPolicy {
            rules: vec![RoutingRule {
                name: "build_only".into(),
                match_config: MatchConfig {
                    work_type: Some(WorkType::Build),
                    min_depth: None,
                    step_pattern: None,
                },
                route_to: vec![RouteEntry {
                    provider_id: "ollama".into(),
                    model_id: None,
                    tier_name: None,
                    is_local: false,
                    max_budget_credits: None,
                }],
                bypass_pool: false,
                sequential: false,
            }],
            escalation: EscalationConfig::default(),
            build_coordination: BuildCoordinationConfig::default(),
            pool_configs: BTreeMap::new(),
            max_batch_cost_usd: None,
            max_daily_cost_usd: None,
        };

        let route = policy.resolve_route(WorkType::Interactive, "primary", "chat", None);
        assert!(route.providers.is_empty());
    }

    // ── Local Mode overlay tests ────────────────────────────────────────────

    fn sample_authored_yaml() -> DispatchPolicyYaml {
        // Mirrors bundled-dispatch_policy-default-v1 — market → fleet →
        // openrouter → ollama-local. Only ollama-local has is_local: true.
        serde_yaml::from_str(
            r#"
version: 1
provider_pools:
  openrouter: { concurrency: 20 }
  ollama-local: { concurrency: 1 }
routing_rules:
  - name: default
    match_config: {}
    route_to:
      - { provider_id: market }
      - { provider_id: fleet }
      - { provider_id: openrouter, model_id: "openai/gpt-4o-mini" }
      - { provider_id: ollama-local, is_local: true }
"#,
        )
        .unwrap()
    }

    #[test]
    fn local_mode_overlay_disabled_is_identity() {
        let original = sample_authored_yaml();
        let out = apply_local_mode_overlay(original.clone(), false);
        // Same rule count, same route_to list, same length.
        assert_eq!(out.routing_rules.len(), 1);
        assert_eq!(out.routing_rules[0].route_to.len(), 4);
        // build_coordination untouched when disabled.
        assert!(out.build_coordination.is_none());
    }

    #[test]
    fn local_mode_overlay_filters_to_local_only() {
        let out = apply_local_mode_overlay(sample_authored_yaml(), true);
        // One rule, one entry (ollama-local).
        assert_eq!(out.routing_rules.len(), 1);
        assert_eq!(out.routing_rules[0].route_to.len(), 1);
        assert_eq!(out.routing_rules[0].route_to[0].provider_id, "ollama-local");
        assert!(out.routing_rules[0].route_to[0].is_local);
    }

    #[test]
    fn local_mode_overlay_drops_rules_with_no_local_entries() {
        let mut yaml = sample_authored_yaml();
        // Replace the single rule's route_to with no local entries.
        yaml.routing_rules[0].route_to = vec![
            RouteEntry {
                provider_id: "market".into(),
                ..Default::default()
            },
            RouteEntry {
                provider_id: "openrouter".into(),
                ..Default::default()
            },
        ];
        let out = apply_local_mode_overlay(yaml, true);
        assert!(out.routing_rules.is_empty());
    }

    #[test]
    fn local_mode_overlay_preserves_authored_pools_and_budgets() {
        // Pool configs and cost caps aren't runtime-routing — they should
        // carry through the overlay so operator tuning isn't silently lost
        // while the toggle is on.
        let original = sample_authored_yaml();
        let out = apply_local_mode_overlay(original.clone(), true);
        assert_eq!(out.provider_pools.len(), original.provider_pools.len());
        assert_eq!(out.max_batch_cost_usd, original.max_batch_cost_usd);
        assert_eq!(out.max_daily_cost_usd, original.max_daily_cost_usd);
    }

    #[test]
    fn local_mode_overlay_forces_defer_maintenance() {
        // Even if the authored contribution opts out of defer_maintenance,
        // Local Mode pins it on — a single local GPU cannot share cycles
        // with background stale checks without starving the focused build.
        let mut yaml = sample_authored_yaml();
        yaml.build_coordination = Some(BuildCoordinationConfig {
            folder_builds_sequential: false,
            defer_maintenance_during_build: false,
            defer_dadbear_during_build: false,
        });
        let out = apply_local_mode_overlay(yaml, true);
        assert!(out.build_coordination.unwrap().defer_maintenance_during_build);
    }

    #[test]
    fn local_mode_overlay_preserves_multiple_rules_independently() {
        // Per-rule filter: rules that retain local entries survive; rules
        // that don't, drop out. Ordering is preserved among survivors.
        let yaml: DispatchPolicyYaml = serde_yaml::from_str(
            r#"
version: 1
routing_rules:
  - name: build
    match_config: { work_type: build }
    route_to:
      - { provider_id: market }
      - { provider_id: ollama-local, is_local: true }
  - name: interactive
    match_config: { work_type: interactive }
    route_to:
      - { provider_id: openrouter }
  - name: fallback
    match_config: {}
    route_to:
      - { provider_id: fleet }
      - { provider_id: ollama-local, is_local: true }
"#,
        )
        .unwrap();
        let out = apply_local_mode_overlay(yaml, true);
        assert_eq!(out.routing_rules.len(), 2);
        assert_eq!(out.routing_rules[0].name, "build");
        assert_eq!(out.routing_rules[1].name, "fallback");
    }
}
