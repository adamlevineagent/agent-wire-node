// pyramid/stale_check_decision.rs -- shared stale-check LLM decision parsing.

use serde_json::Value;
use tracing::warn;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StaleCheckDecisionKind {
    Stale,
    Pass,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StaleCheckDecision {
    pub(crate) kind: StaleCheckDecisionKind,
    pub(crate) reason: String,
}

pub(crate) fn parse_stale_check_decision(content: &str, target_id: &str) -> StaleCheckDecision {
    match super::llm::extract_json(content) {
        Ok(value) => {
            parse_stale_check_decision_value(&value, Some(target_id)).unwrap_or_else(|| {
                StaleCheckDecision {
                    kind: StaleCheckDecisionKind::Stale,
                    reason: "LLM stale check response had no decision entries".to_string(),
                }
            })
        }
        Err(_) => {
            if let Some(kind) = sniff_unparseable_decision(content) {
                return StaleCheckDecision {
                    kind,
                    reason: content.trim().to_string(),
                };
            }

            warn!(
                target_id = %target_id,
                "parse_stale_check_decision: could not parse LLM response as JSON, defaulting to stale"
            );
            StaleCheckDecision {
                kind: StaleCheckDecisionKind::Stale,
                reason: "LLM stale check response was not parseable".to_string(),
            }
        }
    }
}

pub(crate) fn parse_stale_check_decision_value(
    value: &Value,
    target_id: Option<&str>,
) -> Option<StaleCheckDecision> {
    let entries = stale_check_entries(value);
    let entry = matching_stale_check_entry(&entries, target_id)?;
    let target = target_id.unwrap_or("target");
    let reason = entry
        .get("reason")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| format!("LLM stale check for {target} (reason not parseable)"));

    Some(StaleCheckDecision {
        kind: stale_check_decision_kind(entry),
        reason,
    })
}

fn stale_check_entries(value: &Value) -> Vec<&Value> {
    if let Some(entries) = value.as_array() {
        entries.iter().collect()
    } else {
        vec![value]
    }
}

fn matching_stale_check_entry<'a>(
    entries: &'a [&'a Value],
    target_id: Option<&str>,
) -> Option<&'a Value> {
    target_id
        .and_then(|target| {
            entries.iter().copied().find(|entry| {
                entry
                    .get("file_path")
                    .and_then(|v| v.as_str())
                    .map(|s| s == target)
                    .unwrap_or(false)
                    || entry
                        .get("node_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s == target)
                        .unwrap_or(false)
            })
        })
        .or_else(|| entries.first().copied())
}

fn stale_check_decision_kind(entry: &Value) -> StaleCheckDecisionKind {
    let explicit_decision = entry
        .get("decision")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_ascii_lowercase().replace('-', "_"));

    match explicit_decision.as_deref() {
        Some("skip") | Some("skipped") => StaleCheckDecisionKind::Skip,
        Some("pass") | Some("passed") | Some("current") | Some("not_stale") | Some("not stale")
        | Some("no") => StaleCheckDecisionKind::Pass,
        Some("stale") | Some("yes") => StaleCheckDecisionKind::Stale,
        _ => {
            if entry.get("stale").and_then(|v| v.as_bool()).unwrap_or(true) {
                StaleCheckDecisionKind::Stale
            } else {
                StaleCheckDecisionKind::Pass
            }
        }
    }
}

fn sniff_unparseable_decision(content: &str) -> Option<StaleCheckDecisionKind> {
    let lower = content.to_lowercase();
    if lower.contains("\"decision\": \"skip\"")
        || lower.contains("\"decision\":\"skip\"")
        || lower.contains("\"decision\": \"skipped\"")
        || lower.contains("\"decision\":\"skipped\"")
    {
        return Some(StaleCheckDecisionKind::Skip);
    }
    if lower.contains("\"stale\": true") || lower.contains("\"stale\":true") {
        return Some(StaleCheckDecisionKind::Stale);
    }
    if lower.contains("\"stale\": false") || lower.contains("\"stale\":false") {
        return Some(StaleCheckDecisionKind::Pass);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_check_decision_parses_llm_skip_reason_verbatim() {
        let decision = parse_stale_check_decision(
            r#"[{"node_id":"L1-skip","decision":"skip","stale":false,"reason":"LLM confirmed duplicate live thread."}]"#,
            "L1-skip",
        );

        assert_eq!(decision.kind, StaleCheckDecisionKind::Skip);
        assert_eq!(decision.reason, "LLM confirmed duplicate live thread.");
    }

    #[test]
    fn stale_check_decision_keeps_legacy_stale_boolean() {
        let decision = parse_stale_check_decision(
            r#"[{"node_id":"L1-pass","stale":false,"reason":"No semantic change."}]"#,
            "L1-pass",
        );

        assert_eq!(decision.kind, StaleCheckDecisionKind::Pass);
        assert_eq!(decision.reason, "No semantic change.");
    }

    #[test]
    fn unknown_decision_string_falls_back_to_stale_boolean() {
        let decision = parse_stale_check_decision(
            r#"[{"node_id":"L1-pass","decision":"unclear","stale":false,"reason":"Boolean says current."}]"#,
            "L1-pass",
        );

        assert_eq!(decision.kind, StaleCheckDecisionKind::Pass);
        assert_eq!(decision.reason, "Boolean says current.");
    }
}
