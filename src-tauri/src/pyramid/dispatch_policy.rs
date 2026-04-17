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

/// A single provider+model in a routing preference chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Returns the first non-fleet local provider's (provider_id, model_id).
    pub fn resolve_local_for_rule(&self, rule_name: &str) -> Option<(String, Option<String>)> {
        for rule in &self.rules {
            if rule.name == rule_name {
                // Find the first non-fleet provider with is_local == true.
                // No fallback to cloud providers — if no local provider
                // is found, return None so the fleet handler returns an
                // error (prevents surprise cloud billing on fleet jobs).
                for entry in &rule.route_to {
                    if entry.provider_id != "fleet" && entry.is_local {
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
}
