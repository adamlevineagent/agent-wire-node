// pyramid/extraction_schema.rs — Dynamic Extraction Schema Generation (Step 1.3)
//
// THE critical quality lever. Current builds score 3.5/10 because extraction is
// generic ("list all functions"). This module generates question-shaped prompts
// that tell L0 extraction exactly what to look for based on the decomposed
// question tree.
//
// Two-phase approach:
//   1. generate_extraction_schema() — runs BEFORE L0 extraction
//      Takes leaf questions → produces extraction_prompt + topic_schema
//   2. generate_synthesis_prompts() — runs AFTER L0, BEFORE L1
//      Takes question tree + L0 summary → produces per-layer synthesis instructions
//
// Uses the "max" tier model (frontier) because prompt generation is judgment work.

use std::path::Path;

use anyhow::Result;
use tracing::{info, warn};

use super::llm::{self, LlmConfig};
use super::question_decomposition::{render_prompt_template, QuestionNode, QuestionTree};
use super::types::{ExtractionSchema, SynthesisPrompts, TopicField};
use super::Tier1Config;

// ── Public API ───────────────────────────────────────────────────────────────

/// Generate a question-shaped extraction schema from the decomposed question tree.
///
/// This runs BEFORE L0 extraction. It examines all leaf questions and produces:
/// - An extraction prompt that tells L0 exactly what to look for in each source file
/// - A topic schema defining what fields each extracted node should have
/// - Orientation guidance for detail level, tone, and emphasis
///
/// The extraction prompt is the key output. Instead of "list every function and its
/// purpose", it produces something like "For each file, extract: (1) Any mechanism
/// that detects when data becomes stale, (2) How staleness signals propagate..."
///
/// Uses the "max" tier (frontier model) since this shapes the entire build quality.
pub async fn generate_extraction_schema(
    leaf_questions: &[QuestionNode],
    material_profile: &str,
    audience: &str,
    tone: &str,
    llm_config: &LlmConfig,
    tier1: &Tier1Config,
    chains_dir: Option<&Path>,
) -> Result<ExtractionSchema> {
    if leaf_questions.is_empty() {
        anyhow::bail!("No leaf questions provided — cannot generate extraction schema");
    }

    // ── 1. Format leaf questions for the prompt ──────────────────────────
    let leaf_list = leaf_questions
        .iter()
        .enumerate()
        .map(|(i, q)| {
            let hint = if q.prompt_hint.is_empty() {
                String::new()
            } else {
                format!(" (hint: {})", q.prompt_hint)
            };
            format!("  {}. {}{}", i + 1, q.question, hint)
        })
        .collect::<Vec<_>>()
        .join("\n");

    // ── 2. Construct prompts ─────────────────────────────────────────────
    let system_prompt = match chains_dir
        .map(|d| d.join("prompts/question/extraction_schema.md"))
        .and_then(|p| std::fs::read_to_string(&p).ok())
    {
        Some(template) => template,
        None => {
            warn!("extraction_schema.md not found — using inline fallback");
            r#"You are designing an extraction schema for a knowledge pyramid builder. Given a set of questions that the pyramid needs to answer, you must produce a focused extraction prompt that tells the system EXACTLY what to look for in each source file.

CRITICAL PRINCIPLE: The extraction prompt must be QUESTION-SHAPED. Do NOT produce generic instructions like "list all functions" or "summarize the file". Instead, produce specific extraction directives that target what the downstream questions actually need.

Example — if the questions include "How does staleness propagate?", the extraction prompt should say:
"For each file, identify: (1) Any mechanism that detects when data becomes stale, (2) How staleness signals propagate to dependent nodes, (3) Threshold values or configurations that control staleness sensitivity, (4) Timer or scheduler implementations related to freshness checking."

Example — if the questions include "What is the user onboarding flow?", the extraction prompt should say:
"For each file, identify: (1) Registration or signup entry points, (2) Validation steps and their ordering, (3) Welcome/tutorial triggers, (4) Default state or configuration set during onboarding."

Respond in JSON with exactly these fields:
{
  "extraction_prompt": "The complete extraction prompt to use for every source file. Must be specific and question-shaped. Start with 'For each file, extract:' followed by numbered directives.",
  "topic_schema": [
    {"name": "field_name", "description": "what this field captures", "required": true/false}
  ],
  "orientation_guidance": "How detailed to be, what tone to use, what to emphasize vs skip."
}

The topic_schema should have 3-8 fields that are specific to this question domain. Generic fields like "summary" or "key_points" are NOT useful. Fields should map to what the questions need.

CRITICAL — AUDIENCE-AWARE EXTRACTION:
The extraction prompt you generate MUST shape the output for the target audience specified below. If the audience is non-technical (e.g., "a smart high school graduate"), the extraction directives should instruct the extractor to:
- Describe WHAT each thing does and WHY it matters to a user, not just its technical implementation
- Avoid jargon — use plain language explanations
- Focus on purpose, behavior, and user-facing value over internal mechanics
- When technical terms are unavoidable, include brief plain-language definitions

If the audience IS technical, the extraction can use appropriate technical vocabulary freely.

Return ONLY the JSON object, no other text."#.to_string()
        }
    };

    let user_prompt = format!(
        r#"Material: {material_profile}
Target audience: {audience}
Tone: {tone}

The pyramid needs to answer these leaf-level questions (each will be answered by synthesizing extracted evidence from source files):

{leaf_list}

Design an extraction schema that will make the L0 extraction pass capture exactly what these questions need. Remember: question-shaped, not generic.

The extraction prompt you produce will be used to describe source files. Those descriptions must be written FOR the target audience above. If the audience is non-technical, the extraction prompt must explicitly instruct: "Write descriptions that {audience} would understand. Explain WHY each feature matters to a user, not just WHAT it does technically. Avoid developer jargon — use plain language.""#,
    );

    // ── 3. Call LLM ─────────────────────────────────────────────────────
    // Model selection is controlled by YAML chain definitions, not Rust overrides.

    let temperature = tier1.extraction_schema_temperature;
    let max_tokens = tier1.extraction_schema_max_tokens;

    for attempt in 0..2u32 {
        let temp = if attempt == 0 { temperature } else { 0.1 };

        let response = llm::call_model_unified(
            llm_config,
            &system_prompt,
            &user_prompt,
            temp,
            max_tokens,
            None,
        )
        .await?;

        info!(
            attempt = attempt,
            tokens_in = response.usage.prompt_tokens,
            tokens_out = response.usage.completion_tokens,
            leaf_count = leaf_questions.len(),
            "extraction schema LLM call complete"
        );

        // ── 4. Parse JSON response ───────────────────────────────────────
        match parse_extraction_schema_response(&response.content) {
            Ok(schema) => return Ok(schema),
            Err(e) => {
                if attempt == 0 {
                    info!(
                        "extraction schema parse failed ({}), retrying with lower temperature",
                        e
                    );
                    continue;
                }
                return Err(e);
            }
        }
    }

    anyhow::bail!("extraction schema generation failed after 2 attempts")
}

/// Generate per-layer synthesis prompts AFTER L0 extraction completes.
///
/// Takes the question tree and a summary of what L0 actually extracted, then
/// produces prompts for the synthesis layers that reference real evidence.
///
/// This ensures L1 answering doesn't hallucinate — it knows what evidence exists.
pub async fn generate_synthesis_prompts(
    question_tree: &QuestionTree,
    l0_results_summary: &str,
    extraction_schema: &ExtractionSchema,
    audience: Option<&str>,
    llm_config: &LlmConfig,
    tier1: &Tier1Config,
    chains_dir: Option<&Path>,
) -> Result<SynthesisPrompts> {
    // ── 1. Build tree summary for context ────────────────────────────────
    let tree_summary = format_tree_for_prompt(&question_tree.apex, 0);

    // ── 2. Construct prompts ─────────────────────────────────────────────
    let audience_instruction = match audience {
        Some(aud) if !aud.is_empty() => format!(
            r#"

CRITICAL — AUDIENCE-AWARE SYNTHESIS:
The target audience is: {aud}
All three prompts you generate MUST instruct the synthesizer to write for this audience. If the audience is non-technical, the answering_prompt must explicitly say: "Synthesize for {aud}. Translate technical evidence into plain-language explanations. Explain WHY things matter, not just WHAT they do. Avoid jargon." The pre_mapping and web_edge prompts should also reference the audience context."#
        ),
        _ => String::new(),
    };

    let system_prompt = match chains_dir
        .map(|d| d.join("prompts/question/synthesis_prompt.md"))
        .and_then(|p| std::fs::read_to_string(&p).ok())
    {
        Some(template) => render_prompt_template(
            &template,
            &[("audience_instruction", &audience_instruction)],
        ),
        None => {
            warn!("synthesis_prompt.md not found — using inline fallback");
            format!(
                r#"You are designing synthesis prompts for a knowledge pyramid builder. The L0 extraction pass has already completed — you know what evidence was actually extracted. Now you must produce prompts for the synthesis layers that will combine this evidence into answers.

There are three prompts needed:

1. pre_mapping_prompt: Instructions for organizing extracted L0 nodes under the question tree. Which evidence maps to which question? What's missing?

2. answering_prompt: Instructions for synthesizing L0 evidence into L1 answers. Must reference the actual evidence domains that were extracted, not hypothetical ones.

3. web_edge_prompt: Instructions for discovering connections between answered questions. What cross-cutting themes or dependencies exist?
{audience_instruction}

Respond in JSON with exactly these fields:
{{
  "pre_mapping_prompt": "...",
  "answering_prompt": "...",
  "web_edge_prompt": "..."
}}

Each prompt should be 2-4 sentences. Be specific to the actual content, not generic.

Return ONLY the JSON object, no other text."#
            )
        }
    };

    let audience_line = match audience {
        Some(aud) if !aud.is_empty() => format!("\nTarget audience: {aud}\n"),
        _ => String::new(),
    };

    let user_prompt = format!(
        r#"Question tree:
{tree_summary}
{audience_line}
Extraction schema used:
{extraction_prompt}

Topic fields extracted: {topic_fields}

Summary of what L0 extraction actually found:
{l0_results_summary}

Design synthesis prompts that will combine this extracted evidence into answers for the question tree above. The synthesized answers must be written for the target audience."#,
        extraction_prompt = extraction_schema.extraction_prompt,
        topic_fields = extraction_schema
            .topic_schema
            .iter()
            .map(|f| format!("{} ({})", f.name, f.description))
            .collect::<Vec<_>>()
            .join(", "),
    );

    // ── 3. Call LLM ─────────────────────────────────────────────────────
    // Model selection is controlled by YAML chain definitions, not Rust overrides.

    let temperature = tier1.extraction_schema_temperature;
    let max_tokens = tier1.synthesis_prompts_max_tokens;

    for attempt in 0..2u32 {
        let temp = if attempt == 0 { temperature } else { 0.1 };

        let response = llm::call_model_unified(
            llm_config,
            &system_prompt,
            &user_prompt,
            temp,
            max_tokens,
            None,
        )
        .await?;

        info!(
            attempt = attempt,
            tokens_in = response.usage.prompt_tokens,
            tokens_out = response.usage.completion_tokens,
            "synthesis prompts LLM call complete"
        );

        match parse_synthesis_prompts_response(&response.content) {
            Ok(prompts) => return Ok(prompts),
            Err(e) => {
                if attempt == 0 {
                    info!(
                        "synthesis prompts parse failed ({}), retrying with lower temperature",
                        e
                    );
                    continue;
                }
                return Err(e);
            }
        }
    }

    anyhow::bail!("synthesis prompts generation failed after 2 attempts")
}

/// Collect all leaf questions from a question tree.
///
/// Walks the tree recursively and returns references to all nodes where
/// `is_leaf == true`. These are the terminal questions that drive L0 extraction.
pub fn collect_leaf_questions(tree: &QuestionTree) -> Vec<&QuestionNode> {
    let mut leaves = Vec::new();
    collect_leaves_recursive(&tree.apex, &mut leaves);
    leaves
}

// ── Internal helpers ─────────────────────────────────────────────────────────

fn collect_leaves_recursive<'a>(node: &'a QuestionNode, leaves: &mut Vec<&'a QuestionNode>) {
    if node.is_leaf {
        leaves.push(node);
    } else {
        for child in &node.children {
            collect_leaves_recursive(child, leaves);
        }
    }
}

/// Format a question tree node for inclusion in a prompt.
fn format_tree_for_prompt(node: &QuestionNode, depth: usize) -> String {
    let indent = "  ".repeat(depth);
    let leaf_marker = if node.is_leaf { " [LEAF]" } else { "" };
    let mut lines = vec![format!("{indent}Q: {}{leaf_marker}", node.question,)];

    for child in &node.children {
        lines.push(format_tree_for_prompt(child, depth + 1));
    }

    lines.join("\n")
}

/// Parse the LLM response into an ExtractionSchema.
fn parse_extraction_schema_response(content: &str) -> Result<ExtractionSchema> {
    let json_value = llm::extract_json(content)?;

    // Extract extraction_prompt
    let extraction_prompt = json_value
        .get("extraction_prompt")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing 'extraction_prompt' in response"))?
        .to_string();

    // Extract topic_schema
    let topic_schema = match json_value.get("topic_schema") {
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|item| {
                let name = item.get("name")?.as_str()?.to_string();
                let description = item.get("description")?.as_str()?.to_string();
                let required = item
                    .get("required")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                Some(TopicField {
                    name,
                    description,
                    required,
                })
            })
            .collect(),
        _ => vec![],
    };

    // Extract orientation_guidance
    let orientation_guidance = json_value
        .get("orientation_guidance")
        .and_then(|v| v.as_str())
        .unwrap_or("Extract with moderate detail, prioritizing accuracy over completeness.")
        .to_string();

    if topic_schema.len() < 3 {
        warn!(
            field_count = topic_schema.len(),
            "Extraction schema has {} fields (expected 3-8), extraction quality may be degraded",
            topic_schema.len()
        );
    }

    Ok(ExtractionSchema {
        extraction_prompt,
        topic_schema,
        orientation_guidance,
    })
}

/// Parse the LLM response into SynthesisPrompts.
fn parse_synthesis_prompts_response(content: &str) -> Result<SynthesisPrompts> {
    let json_value = llm::extract_json(content)?;

    let pre_mapping_prompt = json_value
        .get("pre_mapping_prompt")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing 'pre_mapping_prompt' in response"))?
        .to_string();

    let answering_prompt = json_value
        .get("answering_prompt")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing 'answering_prompt' in response"))?
        .to_string();

    let web_edge_prompt = json_value
        .get("web_edge_prompt")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing 'web_edge_prompt' in response"))?
        .to_string();

    Ok(SynthesisPrompts {
        pre_mapping_prompt,
        answering_prompt,
        web_edge_prompt,
    })
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_leaf(question: &str, hint: &str) -> QuestionNode {
        QuestionNode {
            id: String::new(),
            question: question.to_string(),
            about: "each file individually".to_string(),
            creates: "L0 nodes".to_string(),
            prompt_hint: hint.to_string(),
            children: vec![],
            is_leaf: true,
        }
    }

    fn make_parent(question: &str, children: Vec<QuestionNode>) -> QuestionNode {
        QuestionNode {
            id: String::new(),
            question: question.to_string(),
            about: "all sub-answers".to_string(),
            creates: "L1 nodes".to_string(),
            prompt_hint: String::new(),
            children,
            is_leaf: false,
        }
    }

    #[test]
    fn collect_leaves_from_simple_tree() {
        use super::super::question_decomposition::DecompositionConfig;

        let tree = QuestionTree {
            apex: make_parent(
                "How does the system work?",
                vec![
                    make_leaf("How does ingestion work?", "data flow"),
                    make_leaf("How does querying work?", "search patterns"),
                ],
            ),
            content_type: "code".to_string(),
            config: DecompositionConfig::default(),
            audience: None,
        };

        let leaves = collect_leaf_questions(&tree);
        assert_eq!(leaves.len(), 2);
        assert_eq!(leaves[0].question, "How does ingestion work?");
        assert_eq!(leaves[1].question, "How does querying work?");
    }

    #[test]
    fn collect_leaves_nested() {
        use super::super::question_decomposition::DecompositionConfig;

        let tree = QuestionTree {
            apex: make_parent(
                "Top",
                vec![
                    make_parent(
                        "Mid A",
                        vec![make_leaf("Leaf A1", ""), make_leaf("Leaf A2", "")],
                    ),
                    make_leaf("Leaf B", ""),
                ],
            ),
            content_type: "code".to_string(),
            config: DecompositionConfig::default(),
            audience: None,
        };

        let leaves = collect_leaf_questions(&tree);
        assert_eq!(leaves.len(), 3);
        assert_eq!(leaves[0].question, "Leaf A1");
        assert_eq!(leaves[1].question, "Leaf A2");
        assert_eq!(leaves[2].question, "Leaf B");
    }

    #[test]
    fn parse_extraction_schema_clean_json() {
        let input = r#"{"extraction_prompt":"For each file, extract: (1) staleness detection, (2) propagation paths","topic_schema":[{"name":"staleness_mechanism","description":"How staleness is detected","required":true},{"name":"propagation_path","description":"How staleness spreads","required":false}],"orientation_guidance":"Be thorough on timing mechanisms"}"#;
        let schema = parse_extraction_schema_response(input).unwrap();
        assert!(schema.extraction_prompt.contains("staleness detection"));
        assert_eq!(schema.topic_schema.len(), 2);
        assert_eq!(schema.topic_schema[0].name, "staleness_mechanism");
        assert!(schema.topic_schema[0].required);
        assert!(!schema.topic_schema[1].required);
    }

    #[test]
    fn parse_extraction_schema_with_markdown_fences() {
        let input = r#"```json
{
  "extraction_prompt": "For each file, extract authentication flows",
  "topic_schema": [{"name": "auth_flow", "description": "auth mechanism", "required": true}],
  "orientation_guidance": "Focus on security"
}
```"#;
        let schema = parse_extraction_schema_response(input).unwrap();
        assert!(schema.extraction_prompt.contains("authentication"));
    }

    #[test]
    fn parse_extraction_schema_missing_prompt_errors() {
        let input = r#"{"topic_schema":[],"orientation_guidance":"ok"}"#;
        assert!(parse_extraction_schema_response(input).is_err());
    }

    #[test]
    fn parse_synthesis_prompts_clean_json() {
        let input = r#"{"pre_mapping_prompt":"Map nodes to questions","answering_prompt":"Synthesize evidence","web_edge_prompt":"Find connections"}"#;
        let prompts = parse_synthesis_prompts_response(input).unwrap();
        assert_eq!(prompts.pre_mapping_prompt, "Map nodes to questions");
        assert_eq!(prompts.answering_prompt, "Synthesize evidence");
        assert_eq!(prompts.web_edge_prompt, "Find connections");
    }

    #[test]
    fn parse_synthesis_prompts_missing_field_errors() {
        let input = r#"{"pre_mapping_prompt":"ok","answering_prompt":"ok"}"#;
        assert!(parse_synthesis_prompts_response(input).is_err());
    }

    #[test]
    fn format_tree_produces_readable_output() {
        let tree_node = make_parent(
            "How does the system work?",
            vec![
                make_leaf("How does ingestion work?", ""),
                make_leaf("How does querying work?", ""),
            ],
        );
        let output = format_tree_for_prompt(&tree_node, 0);
        assert!(output.contains("Q: How does the system work?"));
        assert!(output.contains("  Q: How does ingestion work? [LEAF]"));
    }

    #[test]
    fn parse_extraction_schema_empty_topic_schema_ok() {
        let input = r#"{"extraction_prompt":"Extract everything about APIs","topic_schema":[],"orientation_guidance":"High level"}"#;
        let schema = parse_extraction_schema_response(input).unwrap();
        assert_eq!(schema.topic_schema.len(), 0);
        assert!(schema.extraction_prompt.contains("APIs"));
    }
}
