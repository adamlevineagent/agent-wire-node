use anyhow::{anyhow, Result};
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fmt;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExecutionPlan {
    /// Unique identifier for this plan (typically chain_id + timestamp or hash).
    #[serde(default)]
    pub id: Option<String>,
    /// The chain template this was compiled from (e.g., "code-default").
    #[serde(default)]
    pub source_chain_id: Option<String>,
    /// Content type: "code", "document", "conversation".
    #[serde(default)]
    pub source_content_type: Option<String>,
    pub steps: Vec<Step>,
    #[serde(default)]
    pub total_estimated_nodes: u32,
    #[serde(default)]
    pub total_estimated_cost: CostEstimate,
}

impl ExecutionPlan {
    pub fn validate(&self) -> Result<()> {
        let mut step_ids = HashSet::new();
        let known_steps: HashSet<&str> = self.steps.iter().map(|step| step.id.as_str()).collect();

        for step in &self.steps {
            if step.id.trim().is_empty() {
                return Err(anyhow!("execution plan step id must be non-empty"));
            }
            if !step_ids.insert(step.id.as_str()) {
                return Err(anyhow!("duplicate execution plan step id: {}", step.id));
            }

            if let Some(iteration) = &step.iteration {
                if iteration.mode != IterationMode::Single && iteration.over.is_none() {
                    return Err(anyhow!(
                        "step '{}' uses iteration mode {:?} without an over reference",
                        step.id,
                        iteration.mode
                    ));
                }
                if iteration.mode == IterationMode::Single && iteration.over.is_some() {
                    return Err(anyhow!(
                        "step '{}' uses single iteration mode but still declares over",
                        step.id
                    ));
                }
                if iteration.mode == IterationMode::Sequential
                    && iteration.concurrency.unwrap_or(1) != 1
                {
                    return Err(anyhow!(
                        "step '{}' sequential iteration cannot set concurrency > 1",
                        step.id
                    ));
                }
            }

            for dep in &step.depends_on {
                if !known_steps.contains(dep.as_str()) {
                    return Err(anyhow!(
                        "step '{}' depends on unknown step '{}'",
                        step.id,
                        dep
                    ));
                }
            }

            if matches!(step.operation, StepOperation::Transform) && step.transform.is_none() {
                return Err(anyhow!(
                    "step '{}' is a transform but has no transform descriptor",
                    step.id
                ));
            }
            if matches!(step.operation, StepOperation::Mechanical) && step.rust_function.is_none() {
                return Err(anyhow!(
                    "step '{}' is mechanical but has no rust_function",
                    step.id
                ));
            }
            if matches!(
                step.error_policy,
                ErrorPolicy::CarryLeft | ErrorPolicy::CarryUp
            ) {
                let supports_carry = step
                    .iteration
                    .as_ref()
                    .and_then(|iteration| iteration.shape.as_ref())
                    .map(|shape| {
                        matches!(
                            shape,
                            IterationShape::PairAdjacent
                                | IterationShape::RecursivePair
                                | IterationShape::ConvergeReduce
                        )
                    })
                    .unwrap_or(false);
                if !supports_carry {
                    return Err(anyhow!(
                        "step '{}' uses {} without a carry-capable iteration shape",
                        step.id,
                        step.error_policy
                    ));
                }
            }

            if let Some(storage) = &step.storage_directive {
                storage.validate(&step.id)?;
            }
        }

        self.validate_dag()
    }

    fn validate_dag(&self) -> Result<()> {
        let by_id: HashMap<&str, &Step> = self
            .steps
            .iter()
            .map(|step| (step.id.as_str(), step))
            .collect();
        let mut visiting = HashSet::new();
        let mut visited = HashSet::new();

        fn visit<'a>(
            step_id: &'a str,
            by_id: &HashMap<&'a str, &'a Step>,
            visiting: &mut HashSet<&'a str>,
            visited: &mut HashSet<&'a str>,
        ) -> Result<()> {
            if visited.contains(step_id) {
                return Ok(());
            }
            if !visiting.insert(step_id) {
                return Err(anyhow!("cycle detected involving step '{}'", step_id));
            }

            let step = by_id
                .get(step_id)
                .copied()
                .ok_or_else(|| anyhow!("missing step '{}' during DAG validation", step_id))?;
            for dep in &step.depends_on {
                visit(dep, by_id, visiting, visited)?;
            }

            visiting.remove(step_id);
            visited.insert(step_id);
            Ok(())
        }

        for step in &self.steps {
            visit(step.id.as_str(), &by_id, &mut visiting, &mut visited)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub id: String,
    pub operation: StepOperation,
    #[serde(default)]
    pub primitive: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub iteration: Option<IterationDirective>,
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub instruction: Option<String>,
    #[serde(default)]
    pub instruction_map: Option<HashMap<String, String>>,
    #[serde(default)]
    pub compact_inputs: bool,
    #[serde(default)]
    pub output_schema: Option<Value>,
    #[serde(default)]
    pub constraints: Option<Vec<Constraint>>,
    pub error_policy: ErrorPolicy,
    #[serde(default)]
    pub model_requirements: ModelRequirements,
    #[serde(default)]
    pub storage_directive: Option<StorageDirective>,
    #[serde(default)]
    pub cost_estimate: CostEstimate,
    #[serde(default)]
    pub action_id: Option<String>,
    #[serde(default)]
    pub rust_function: Option<String>,
    #[serde(default)]
    pub transform: Option<TransformSpec>,
    #[serde(default)]
    pub when: Option<String>,
    #[serde(default)]
    pub context: Vec<ContextEntry>,
    /// Response schema for structured LLM output (separate from output_schema
    /// for cases like cluster_response_schema on classify steps).
    #[serde(default)]
    pub response_schema: Option<Value>,
    /// Original YAML step name (for debugging, logging, resume matching).
    #[serde(default)]
    pub source_step_name: Option<String>,
    /// Node ID pattern for steps that save nodes (e.g., "C-L0-{index:03}").
    /// Already present in StorageDirective, but also kept at Step level
    /// for converge-expanded steps where the pattern is shared.
    #[serde(default)]
    pub converge_metadata: Option<ConvergeMetadata>,
    #[serde(default)]
    pub metadata: Option<Value>,
    /// Execution scope hint for hybrid local+Wire routing (P4.3).
    /// Default is `Local`. Actual Wire routing is future work.
    #[serde(default)]
    pub scope: Option<ExecutionScope>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepOperation {
    Llm,
    Wire,
    Task,
    Game,
    Transform,
    Mechanical,
}

/// Execution scope hint for hybrid local+Wire execution (P4.3).
///
/// Phase 4 default is `Local` for all steps. The routing infrastructure is a
/// forward-looking placeholder — actual Wire-side execution routing is future work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionScope {
    /// Always execute locally (default for Phase 4).
    #[default]
    Local,
    /// Prefer local execution, can route to Wire if a capable agent is available.
    Preferred,
    /// Must execute on the Wire (requires a connected agent with matching capabilities).
    WireOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IterationDirective {
    pub mode: IterationMode,
    #[serde(default)]
    pub over: Option<String>,
    #[serde(default)]
    pub concurrency: Option<usize>,
    #[serde(default)]
    pub accumulate: Option<AccumulatorConfig>,
    #[serde(default)]
    pub shape: Option<IterationShape>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IterationMode {
    Single,
    Parallel,
    Sequential,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IterationShape {
    ForEach,
    PairAdjacent,
    RecursivePair,
    ConvergeReduce,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccumulatorConfig {
    pub field: String,
    #[serde(default)]
    pub seed: Option<Value>,
    #[serde(default)]
    pub max_chars: Option<usize>,
    #[serde(default)]
    pub trim_to: Option<usize>,
    #[serde(default)]
    pub trim_side: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Constraint {
    pub kind: String,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub expression: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelRequirements {
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub temperature: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CostEstimate {
    #[serde(default)]
    pub billable_calls: u32,
    #[serde(default)]
    pub estimated_output_nodes: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageDirective {
    pub kind: StorageKind,
    #[serde(default)]
    pub depth: Option<i64>,
    #[serde(default)]
    pub node_id_pattern: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
}

impl StorageDirective {
    pub fn validate(&self, step_id: &str) -> Result<()> {
        match self.kind {
            StorageKind::Node => {
                if self.depth.is_none() {
                    return Err(anyhow!(
                        "step '{}' stores nodes but does not declare depth",
                        step_id
                    ));
                }
                if self
                    .node_id_pattern
                    .as_deref()
                    .unwrap_or("")
                    .trim()
                    .is_empty()
                {
                    return Err(anyhow!(
                        "step '{}' stores nodes but does not declare node_id_pattern",
                        step_id
                    ));
                }
            }
            StorageKind::WebEdges => {
                if self.depth.is_none() {
                    return Err(anyhow!(
                        "step '{}' stores web_edges but does not declare depth",
                        step_id
                    ));
                }
            }
            StorageKind::StepOnly | StorageKind::Output => {}
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageKind {
    Node,
    WebEdges,
    StepOnly,
    Output,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransformSpec {
    pub function: String,
    #[serde(default)]
    pub args: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextEntry {
    pub label: String,
    #[serde(default)]
    pub reference: Option<String>,
    #[serde(default)]
    pub loader: Option<String>,
    #[serde(default)]
    pub params: Option<Value>,
}

/// Metadata for steps that are part of a converge expansion.
/// Only present on steps generated by the converge expander.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvergeMetadata {
    /// Which converge block this step belongs to (e.g., "upper_layer_synthesis").
    pub converge_id: String,
    /// Round number (0-indexed). None for shortcut steps.
    #[serde(default)]
    pub round: Option<u32>,
    /// Role within the round.
    pub role: ConvergeRole,
    /// Max rounds for this converge block.
    pub max_rounds: u32,
    /// Shortcut threshold (skip classify if remaining <= this count).
    pub shortcut_at: u32,
    /// Fallback on classifier failure.
    #[serde(default)]
    pub classify_fallback: Option<ClassifyFallback>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConvergeRole {
    Classify,
    ClassifyFallback,
    Repair,
    Reduce,
    Shortcut,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassifyFallback {
    /// Fall back to positional groups of N.
    Positional(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorPolicy {
    Abort,
    Skip,
    Retry(u32),
    CarryLeft,
    CarryUp,
}

impl fmt::Display for ErrorPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Abort => write!(f, "abort"),
            Self::Skip => write!(f, "skip"),
            Self::Retry(attempts) => write!(f, "retry({attempts})"),
            Self::CarryLeft => write!(f, "carry_left"),
            Self::CarryUp => write!(f, "carry_up"),
        }
    }
}

impl Serialize for ErrorPolicy {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ErrorPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ErrorPolicyVisitor;

        impl<'de> Visitor<'de> for ErrorPolicyVisitor {
            type Value = ErrorPolicy;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an error policy string")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                parse_error_policy(value).map_err(E::custom)
            }
        }

        deserializer.deserialize_str(ErrorPolicyVisitor)
    }
}

pub fn parse_error_policy(value: &str) -> Result<ErrorPolicy> {
    match value {
        "abort" => Ok(ErrorPolicy::Abort),
        "skip" => Ok(ErrorPolicy::Skip),
        "carry_left" => Ok(ErrorPolicy::CarryLeft),
        "carry_up" => Ok(ErrorPolicy::CarryUp),
        other => {
            let Some(inner) = other
                .strip_prefix("retry(")
                .and_then(|rest| rest.strip_suffix(')'))
            else {
                return Err(anyhow!(
                    "invalid error policy '{}': expected abort, skip, carry_left, carry_up, or retry(N)",
                    other
                ));
            };
            let attempts = inner
                .parse::<u32>()
                .map_err(|_| anyhow!("invalid retry count in '{}'", other))?;
            if !(1..=10).contains(&attempts) {
                return Err(anyhow!("retry count must be 1-10, got {}", attempts));
            }
            Ok(ErrorPolicy::Retry(attempts))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn plan_step(id: &str) -> Step {
        Step {
            id: id.to_string(),
            operation: StepOperation::Llm,
            primitive: Some("extract".to_string()),
            depends_on: vec![],
            iteration: None,
            input: json!({}),
            instruction: Some("prompt".to_string()),
            instruction_map: None,
            compact_inputs: false,
            output_schema: None,
            constraints: None,
            error_policy: ErrorPolicy::Retry(2),
            model_requirements: ModelRequirements::default(),
            storage_directive: None,
            cost_estimate: CostEstimate::default(),
            action_id: None,
            rust_function: None,
            transform: None,
            when: None,
            context: vec![],
            response_schema: None,
            source_step_name: None,
            converge_metadata: None,
            metadata: None,
            scope: None,
        }
    }

    fn make_plan(steps: Vec<Step>) -> ExecutionPlan {
        ExecutionPlan {
            id: None,
            source_chain_id: None,
            source_content_type: None,
            steps,
            total_estimated_nodes: 0,
            total_estimated_cost: CostEstimate::default(),
        }
    }

    #[test]
    fn execution_plan_round_trips() {
        let plan = ExecutionPlan {
            id: Some("plan-001".to_string()),
            source_chain_id: Some("code-default".to_string()),
            source_content_type: Some("code".to_string()),
            steps: vec![plan_step("extract")],
            total_estimated_nodes: 12,
            total_estimated_cost: CostEstimate {
                billable_calls: 4,
                estimated_output_nodes: 12,
            },
        };

        let json = serde_json::to_string(&plan).unwrap();
        let round_trip: ExecutionPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(round_trip.id.as_deref(), Some("plan-001"));
        assert_eq!(round_trip.source_chain_id.as_deref(), Some("code-default"));
        assert_eq!(round_trip.steps.len(), 1);
        assert_eq!(round_trip.total_estimated_nodes, 12);
    }

    #[test]
    fn validation_rejects_cycles() {
        let mut a = plan_step("a");
        let mut b = plan_step("b");
        a.depends_on = vec!["b".to_string()];
        b.depends_on = vec!["a".to_string()];
        let plan = make_plan(vec![a, b]);

        let err = plan.validate().unwrap_err().to_string();
        assert!(err.contains("cycle detected"));
    }

    #[test]
    fn validation_accepts_valid_dag() {
        let mut b = plan_step("b");
        b.depends_on = vec!["a".to_string()];
        let mut c = plan_step("c");
        c.depends_on = vec!["a".to_string(), "b".to_string()];
        let plan = make_plan(vec![plan_step("a"), b, c]);

        plan.validate().expect("valid DAG should pass");
    }

    #[test]
    fn validation_rejects_dangling_depends_on() {
        let mut step = plan_step("orphan");
        step.depends_on = vec!["nonexistent".to_string()];
        let plan = make_plan(vec![step]);

        let err = plan.validate().unwrap_err().to_string();
        assert!(err.contains("unknown step 'nonexistent'"));
    }

    #[test]
    fn validation_rejects_duplicate_step_ids() {
        let plan = make_plan(vec![plan_step("dup"), plan_step("dup")]);

        let err = plan.validate().unwrap_err().to_string();
        assert!(err.contains("duplicate"));
    }

    #[test]
    fn validation_rejects_empty_step_id() {
        let plan = make_plan(vec![plan_step("")]);

        let err = plan.validate().unwrap_err().to_string();
        assert!(err.contains("non-empty"));
    }

    #[test]
    fn validation_rejects_invalid_storage_directive() {
        let mut step = plan_step("save_node");
        step.storage_directive = Some(StorageDirective {
            kind: StorageKind::Node,
            depth: Some(0),
            node_id_pattern: None,
            target: None,
        });
        let plan = make_plan(vec![step]);

        let err = plan.validate().unwrap_err().to_string();
        assert!(err.contains("node_id_pattern"));
    }

    #[test]
    fn validation_rejects_web_edges_without_depth() {
        let mut step = plan_step("web");
        step.storage_directive = Some(StorageDirective {
            kind: StorageKind::WebEdges,
            depth: None,
            node_id_pattern: None,
            target: None,
        });
        let plan = make_plan(vec![step]);

        let err = plan.validate().unwrap_err().to_string();
        assert!(err.contains("web_edges") && err.contains("depth"));
    }

    #[test]
    fn validation_rejects_invalid_carry_policy_usage() {
        let mut step = plan_step("bad_carry");
        step.error_policy = ErrorPolicy::CarryLeft;
        let plan = make_plan(vec![step]);

        let err = plan.validate().unwrap_err().to_string();
        assert!(err.contains("carry-capable"));
    }

    #[test]
    fn validation_rejects_transform_without_spec() {
        let mut step = plan_step("bad_transform");
        step.operation = StepOperation::Transform;
        step.transform = None;
        let plan = make_plan(vec![step]);

        let err = plan.validate().unwrap_err().to_string();
        assert!(err.contains("transform"));
    }

    #[test]
    fn validation_rejects_mechanical_without_rust_function() {
        let mut step = plan_step("bad_mech");
        step.operation = StepOperation::Mechanical;
        step.rust_function = None;
        let plan = make_plan(vec![step]);

        let err = plan.validate().unwrap_err().to_string();
        assert!(err.contains("rust_function"));
    }

    #[test]
    fn error_policy_parsing() {
        assert_eq!(parse_error_policy("abort").unwrap(), ErrorPolicy::Abort);
        assert_eq!(parse_error_policy("skip").unwrap(), ErrorPolicy::Skip);
        assert_eq!(
            parse_error_policy("carry_left").unwrap(),
            ErrorPolicy::CarryLeft
        );
        assert_eq!(
            parse_error_policy("carry_up").unwrap(),
            ErrorPolicy::CarryUp
        );
        assert_eq!(
            parse_error_policy("retry(3)").unwrap(),
            ErrorPolicy::Retry(3)
        );
        assert_eq!(
            parse_error_policy("retry(1)").unwrap(),
            ErrorPolicy::Retry(1)
        );
        assert_eq!(
            parse_error_policy("retry(10)").unwrap(),
            ErrorPolicy::Retry(10)
        );

        assert!(parse_error_policy("retry(0)").is_err());
        assert!(parse_error_policy("retry(11)").is_err());
        assert!(parse_error_policy("retry(abc)").is_err());
        assert!(parse_error_policy("explode").is_err());
    }

    #[test]
    fn error_policy_serde_round_trip() {
        let policies = vec![
            ErrorPolicy::Abort,
            ErrorPolicy::Skip,
            ErrorPolicy::Retry(5),
            ErrorPolicy::CarryLeft,
            ErrorPolicy::CarryUp,
        ];
        for policy in policies {
            let json = serde_json::to_string(&policy).unwrap();
            let round_trip: ErrorPolicy = serde_json::from_str(&json).unwrap();
            assert_eq!(round_trip, policy);
        }
    }

    #[test]
    fn converge_metadata_round_trips() {
        let meta = ConvergeMetadata {
            converge_id: "upper_layer_synthesis".to_string(),
            round: Some(2),
            role: ConvergeRole::Reduce,
            max_rounds: 8,
            shortcut_at: 4,
            classify_fallback: Some(ClassifyFallback::Positional(3)),
        };
        let json = serde_json::to_string(&meta).unwrap();
        let round_trip: ConvergeMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(round_trip.converge_id, "upper_layer_synthesis");
        assert_eq!(round_trip.round, Some(2));
        assert_eq!(round_trip.role, ConvergeRole::Reduce);
        assert_eq!(round_trip.max_rounds, 8);
    }

    #[test]
    fn execution_scope_default_is_local() {
        let scope = ExecutionScope::default();
        assert_eq!(scope, ExecutionScope::Local);
    }

    #[test]
    fn execution_scope_serde_round_trip() {
        let scopes = vec![
            ExecutionScope::Local,
            ExecutionScope::Preferred,
            ExecutionScope::WireOnly,
        ];
        for scope in scopes {
            let json = serde_json::to_string(&scope).unwrap();
            let round_trip: ExecutionScope = serde_json::from_str(&json).unwrap();
            assert_eq!(round_trip, scope);
        }
    }

    #[test]
    fn step_scope_defaults_to_none() {
        let step = plan_step("scoped");
        assert_eq!(step.scope, None);
    }

    #[test]
    fn step_with_scope_round_trips() {
        let mut step = plan_step("scoped");
        step.scope = Some(ExecutionScope::WireOnly);
        let json = serde_json::to_string(&step).unwrap();
        let round_trip: Step = serde_json::from_str(&json).unwrap();
        assert_eq!(round_trip.scope, Some(ExecutionScope::WireOnly));
    }
}
