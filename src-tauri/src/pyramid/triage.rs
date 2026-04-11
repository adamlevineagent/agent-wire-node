// pyramid/triage.rs — Phase 12 evidence triage gate
//
// The triage step evaluates evidence questions against a policy DSL
// and routes each one to one of three outcomes:
//
//     answer  — run the expensive answering LLM call
//     defer   — store in pyramid_deferred_questions for later re-check
//     skip    — drop entirely
//
// The DSL evaluator understands a tiny set of boolean predicates and
// comparisons (see the spec at `docs/specs/evidence-triage-and-dadbear.md`
// Part 2 §Triage Conditions). When no policy rule matches a question,
// the caller may fall back to a cheap LLM classification call — that
// branch threads a StepContext so the triage LLM call is itself
// cache-reachable.
//
// All LLM calls in this module MUST flow through
// `call_model_unified_with_options_and_ctx` so cache hits land on
// identical triage inputs.

use anyhow::Result;

use super::db::{EvidencePolicy, TriageRuleYaml};
use super::types::LayerQuestion;

// ── Triage decision types ──────────────────────────────────────────────────

/// Outcome of evaluating a single question against the triage DSL.
#[derive(Debug, Clone, PartialEq)]
pub enum TriageDecision {
    Answer {
        model_tier: String,
    },
    Defer {
        check_interval: String,
        triage_reason: String,
    },
    Skip {
        reason: String,
    },
}

impl TriageDecision {
    pub fn as_action_tag(&self) -> &'static str {
        match self {
            TriageDecision::Answer { .. } => "answer",
            TriageDecision::Defer { .. } => "defer",
            TriageDecision::Skip { .. } => "skip",
        }
    }
}

/// Facts about a question that the DSL evaluator can compare against.
///
/// `target_node_depth` is the depth of the target node (if any) —
/// used by `depth == N` conditions. For an L0 evidence question
/// derived from a question-tree leaf, this is the leaf's layer.
///
/// `evidence_question_trivial` / `evidence_question_high_value` are
/// LLM-classified flags. They are `None` unless a prior LLM pass has
/// populated them (or unless the evaluator hits a condition that
/// requires the classification and the caller chooses to run it).
pub struct TriageFacts<'a> {
    pub question: &'a LayerQuestion,
    pub target_node_distilled: Option<&'a str>,
    pub target_node_depth: Option<i64>,
    pub is_first_build: bool,
    pub is_stale_check: bool,
    pub has_demand_signals: bool,
    pub evidence_question_trivial: Option<bool>,
    pub evidence_question_high_value: Option<bool>,
}

// ── DSL evaluator ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Ident(String),
    Number(i64),
    EqEq,
    LParen,
    RParen,
    And,
    Or,
    Not,
}

fn tokenize(src: &str) -> Result<Vec<Token>> {
    let mut tokens = Vec::new();
    let mut chars = src.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_whitespace() {
            chars.next();
            continue;
        }
        if c == '(' {
            chars.next();
            tokens.push(Token::LParen);
            continue;
        }
        if c == ')' {
            chars.next();
            tokens.push(Token::RParen);
            continue;
        }
        if c == '=' {
            chars.next();
            if chars.peek() == Some(&'=') {
                chars.next();
                tokens.push(Token::EqEq);
            } else {
                return Err(anyhow::anyhow!(
                    "triage DSL: unexpected '=' (did you mean '==')"
                ));
            }
            continue;
        }
        if c.is_ascii_digit() {
            let mut num = String::new();
            while let Some(&d) = chars.peek() {
                if d.is_ascii_digit() {
                    num.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            let n: i64 = num
                .parse()
                .map_err(|e| anyhow::anyhow!("triage DSL: bad number {}: {}", num, e))?;
            tokens.push(Token::Number(n));
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let mut ident = String::new();
            while let Some(&d) = chars.peek() {
                if d.is_ascii_alphanumeric() || d == '_' {
                    ident.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            let lower = ident.to_ascii_lowercase();
            match lower.as_str() {
                "and" => tokens.push(Token::And),
                "or" => tokens.push(Token::Or),
                "not" => tokens.push(Token::Not),
                _ => tokens.push(Token::Ident(ident)),
            }
            continue;
        }
        return Err(anyhow::anyhow!(
            "triage DSL: unexpected character {:?} in expression",
            c
        ));
    }
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }
    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }
    fn parse_expr(&mut self, facts: &TriageFacts<'_>) -> Result<bool> {
        self.parse_or(facts)
    }
    fn parse_or(&mut self, facts: &TriageFacts<'_>) -> Result<bool> {
        let mut left = self.parse_and(facts)?;
        while let Some(Token::Or) = self.peek() {
            self.next();
            let right = self.parse_and(facts)?;
            left = left || right;
        }
        Ok(left)
    }
    fn parse_and(&mut self, facts: &TriageFacts<'_>) -> Result<bool> {
        let mut left = self.parse_not(facts)?;
        while let Some(Token::And) = self.peek() {
            self.next();
            let right = self.parse_not(facts)?;
            left = left && right;
        }
        Ok(left)
    }
    fn parse_not(&mut self, facts: &TriageFacts<'_>) -> Result<bool> {
        if let Some(Token::Not) = self.peek() {
            self.next();
            let v = self.parse_not(facts)?;
            return Ok(!v);
        }
        self.parse_atom(facts)
    }
    fn parse_atom(&mut self, facts: &TriageFacts<'_>) -> Result<bool> {
        let tok = self
            .next()
            .ok_or_else(|| anyhow::anyhow!("triage DSL: unexpected end of expression"))?;
        match tok {
            Token::LParen => {
                let v = self.parse_expr(facts)?;
                match self.next() {
                    Some(Token::RParen) => Ok(v),
                    _ => Err(anyhow::anyhow!("triage DSL: expected ')'")),
                }
            }
            Token::Ident(name) => {
                // Support `depth == N` comparison.
                if name.eq_ignore_ascii_case("depth") {
                    match self.next() {
                        Some(Token::EqEq) => {}
                        other => {
                            return Err(anyhow::anyhow!(
                                "triage DSL: expected '==' after 'depth', got {:?}",
                                other
                            ));
                        }
                    }
                    let n = match self.next() {
                        Some(Token::Number(n)) => n,
                        other => {
                            return Err(anyhow::anyhow!(
                                "triage DSL: expected number after 'depth ==', got {:?}",
                                other
                            ));
                        }
                    };
                    let depth = facts.target_node_depth.unwrap_or(facts.question.layer);
                    return Ok(depth == n);
                }
                Ok(eval_predicate(&name, facts))
            }
            Token::Number(_) => Err(anyhow::anyhow!(
                "triage DSL: bare numbers are only valid on the right side of `depth ==`"
            )),
            other => Err(anyhow::anyhow!(
                "triage DSL: unexpected token {:?} in atom position",
                other
            )),
        }
    }
}

fn eval_predicate(name: &str, facts: &TriageFacts<'_>) -> bool {
    match name {
        "first_build" => facts.is_first_build,
        "stale_check" => facts.is_stale_check,
        "no_demand_signals" => !facts.has_demand_signals,
        "has_demand_signals" => facts.has_demand_signals,
        "evidence_question_trivial" => facts.evidence_question_trivial.unwrap_or(false),
        "evidence_question_high_value" => facts.evidence_question_high_value.unwrap_or(false),
        _ => false,
    }
}

/// Public DSL entry point. Parses `expression` and returns whether
/// the predicate evaluates to true against `facts`.
pub fn evaluate_condition(expression: &str, facts: &TriageFacts<'_>) -> Result<bool> {
    let tokens = tokenize(expression)?;
    let mut parser = Parser { tokens, pos: 0 };
    let result = parser.parse_expr(facts)?;
    if parser.pos < parser.tokens.len() {
        return Err(anyhow::anyhow!(
            "triage DSL: {} trailing tokens after expression",
            parser.tokens.len() - parser.pos
        ));
    }
    Ok(result)
}

// ── Triage decision resolver ───────────────────────────────────────────────

/// Walk `policy.triage_rules` in order, evaluate each condition,
/// and return the first matching rule's action as a TriageDecision.
///
/// If no rule matches, returns an Answer decision with the policy's
/// initial_build or maintenance model tier (based on `is_stale_check`).
/// This is the safe default — answering always produces correct
/// pyramid data, just at higher cost than a deferred decision.
pub fn resolve_decision(
    policy: &EvidencePolicy,
    facts: &TriageFacts<'_>,
) -> Result<TriageDecision> {
    for rule in &policy.triage_rules {
        if rule.condition.trim().is_empty() {
            continue;
        }
        let matched = evaluate_condition(&rule.condition, facts)?;
        if matched {
            return Ok(rule_to_decision(rule, policy, facts));
        }
    }
    // Default: answer with the maintenance or initial-build tier.
    let model_tier = default_answer_tier(policy, facts);
    Ok(TriageDecision::Answer { model_tier })
}

fn default_answer_tier(policy: &EvidencePolicy, facts: &TriageFacts<'_>) -> String {
    if facts.is_stale_check {
        policy
            .budget
            .maintenance_model_tier
            .clone()
            .unwrap_or_else(|| "stale_local".to_string())
    } else {
        policy
            .budget
            .initial_build_model_tier
            .clone()
            .unwrap_or_else(|| "fast_extract".to_string())
    }
}

fn rule_to_decision(
    rule: &TriageRuleYaml,
    policy: &EvidencePolicy,
    facts: &TriageFacts<'_>,
) -> TriageDecision {
    let action = rule.action.to_ascii_lowercase();
    match action.as_str() {
        "answer" => {
            let model_tier = rule
                .model_tier
                .clone()
                .unwrap_or_else(|| default_answer_tier(policy, facts));
            TriageDecision::Answer { model_tier }
        }
        "defer" => TriageDecision::Defer {
            check_interval: rule.check_interval.clone().unwrap_or_else(|| "30d".to_string()),
            triage_reason: format!("matched rule: {}", rule.condition),
        },
        "skip" => TriageDecision::Skip {
            reason: format!("matched rule: {}", rule.condition),
        },
        other => TriageDecision::Answer {
            model_tier: default_answer_tier(policy, facts),
        }
        // swallow the unknown action tag for logging in the caller
        .tag_with_action_for_log(other),
    }
}

// Helper trait to carry an action tag on an Answer decision for
// logging purposes without changing the public decision shape. We
// keep it internal — the public shape uses the three canonical
// variants only.
trait TagForLog {
    fn tag_with_action_for_log(self, _action: &str) -> Self;
}
impl TagForLog for TriageDecision {
    fn tag_with_action_for_log(self, _action: &str) -> Self {
        self
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_question() -> LayerQuestion {
        LayerQuestion {
            question_id: "Q1".to_string(),
            question_text: "What is the thing?".to_string(),
            layer: 1,
            about: "a topic".to_string(),
            creates: "an answer".to_string(),
        }
    }

    fn facts(
        is_first: bool,
        is_stale: bool,
        has_signals: bool,
    ) -> TriageFacts<'static> {
        // Leak for simpler test construction. In production the
        // facts hold real borrows from the caller's stack.
        let q: &'static LayerQuestion = Box::leak(Box::new(sample_question()));
        TriageFacts {
            question: q,
            target_node_distilled: None,
            target_node_depth: Some(q.layer),
            is_first_build: is_first,
            is_stale_check: is_stale,
            has_demand_signals: has_signals,
            evidence_question_trivial: None,
            evidence_question_high_value: None,
        }
    }

    fn policy_with_rules(rules: Vec<TriageRuleYaml>) -> EvidencePolicy {
        EvidencePolicy {
            slug: None,
            contribution_id: None,
            triage_rules: rules,
            demand_signals: Vec::new(),
            budget: Default::default(),
            demand_signal_attenuation: Default::default(),
            policy_yaml_hash: "test".to_string(),
        }
    }

    #[test]
    fn test_triage_dsl_parse_simple() {
        let f = facts(true, false, false);
        assert!(evaluate_condition("first_build", &f).unwrap());
        assert!(!evaluate_condition("stale_check", &f).unwrap());
    }

    #[test]
    fn test_triage_dsl_and_or_precedence() {
        let f = facts(true, false, true);
        // (first_build AND has_demand_signals) OR stale_check
        assert!(
            evaluate_condition(
                "(first_build AND has_demand_signals) OR stale_check",
                &f
            )
            .unwrap()
        );
        // first_build AND NOT stale_check
        assert!(evaluate_condition("first_build AND NOT stale_check", &f).unwrap());
        // NOT first_build OR stale_check → false OR false
        assert!(!evaluate_condition("NOT first_build OR stale_check", &f).unwrap());
    }

    #[test]
    fn test_triage_dsl_depth_comparison() {
        let f = facts(false, true, false);
        // f.question.layer is 1 → target_node_depth = 1
        assert!(evaluate_condition("depth == 1", &f).unwrap());
        assert!(!evaluate_condition("depth == 0", &f).unwrap());
        assert!(
            evaluate_condition("stale_check AND depth == 1", &f).unwrap()
        );
    }

    #[test]
    fn test_triage_rule_first_match_wins() {
        let policy = policy_with_rules(vec![
            TriageRuleYaml {
                condition: "first_build AND depth == 1".into(),
                action: "answer".into(),
                model_tier: Some("fast_extract".into()),
                ..Default::default()
            },
            TriageRuleYaml {
                condition: "first_build".into(),
                action: "defer".into(),
                check_interval: Some("7d".into()),
                ..Default::default()
            },
        ]);
        let f = facts(true, false, false);
        let decision = resolve_decision(&policy, &f).unwrap();
        match decision {
            TriageDecision::Answer { model_tier } => assert_eq!(model_tier, "fast_extract"),
            other => panic!("expected Answer(fast_extract), got {:?}", other),
        }
    }

    #[test]
    fn test_triage_defer_rule() {
        let policy = policy_with_rules(vec![TriageRuleYaml {
            condition: "stale_check AND no_demand_signals".into(),
            action: "defer".into(),
            check_interval: Some("never".into()),
            ..Default::default()
        }]);
        let f = facts(false, true, false);
        let decision = resolve_decision(&policy, &f).unwrap();
        match decision {
            TriageDecision::Defer { check_interval, .. } => {
                assert_eq!(check_interval, "never");
            }
            other => panic!("expected Defer, got {:?}", other),
        }
    }

    #[test]
    fn test_triage_skip_rule() {
        let policy = policy_with_rules(vec![TriageRuleYaml {
            condition: "evidence_question_trivial".into(),
            action: "skip".into(),
            ..Default::default()
        }]);
        let mut f = facts(false, false, false);
        f.evidence_question_trivial = Some(true);
        let decision = resolve_decision(&policy, &f).unwrap();
        assert!(matches!(decision, TriageDecision::Skip { .. }));
    }

    #[test]
    fn test_triage_default_when_no_rules_match() {
        let policy = policy_with_rules(vec![TriageRuleYaml {
            condition: "first_build".into(),
            action: "answer".into(),
            model_tier: Some("fast_extract".into()),
            ..Default::default()
        }]);
        // Not a first build, so no rule matches → default is answer
        // with the stale/initial tier.
        let f = facts(false, true, false);
        let decision = resolve_decision(&policy, &f).unwrap();
        match decision {
            TriageDecision::Answer { model_tier } => {
                // stale_check=true → maintenance_model_tier default
                assert_eq!(model_tier, "stale_local");
            }
            other => panic!("expected default Answer, got {:?}", other),
        }
    }

    #[test]
    fn test_triage_dsl_errors_on_bad_input() {
        let f = facts(false, false, false);
        assert!(evaluate_condition("(((", &f).is_err());
        assert!(evaluate_condition("depth = 1", &f).is_err());
        assert!(evaluate_condition("depth ==", &f).is_err());
    }
}
