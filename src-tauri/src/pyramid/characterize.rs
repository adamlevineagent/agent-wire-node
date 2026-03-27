// pyramid/characterize.rs — Material Characterization Step (Phase 1, Step 1.1)
//
// Analyzes source material before building a knowledge pyramid. Determines
// what kind of material it is, restates the user's question precisely,
// identifies the likely audience, and sets the appropriate tone.
//
// This is the first step in the two-chain build pattern:
//   1. characterize() → CharacterizationResult (user reviews/confirms)
//   2. run_decomposed_build() with characterization → full build
//
// Uses the "max" tier model (fallback_model_2 / frontier) because
// characterization is a judgment call, not extraction.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::info;

use super::llm::{self, LlmConfig};
use super::question_decomposition;
use super::types::CharacterizationResult;

// ── Public API ───────────────────────────────────────────────────────────────

/// Characterize source material before building a knowledge pyramid.
///
/// Examines the folder structure and the user's apex question to determine:
/// - What kind of material this is (code repo, design docs, mixed, etc.)
/// - What the user is really asking (restated in precise terms)
/// - Who the likely audience is
/// - What tone the pyramid should use
///
/// Uses the "max" tier (frontier model) since this is a judgment call.
pub async fn characterize(
    source_path: &str,
    apex_question: &str,
    llm_config: &LlmConfig,
) -> Result<CharacterizationResult> {
    // ── 1. Build folder map from source path for LLM context ─────────────
    let folder_map = question_decomposition::build_folder_map(source_path);
    let folder_context = match folder_map.as_deref() {
        Some(map) if !map.is_empty() => map,
        _ => return Err(anyhow::anyhow!("Invalid source path: folder map is empty")),
    };

    // ── 2. Construct prompts ─────────────────────────────────────────────
    let system_prompt = r#"You are analyzing a folder of source material to prepare for building a knowledge pyramid. Given the user's question and the folder contents below, determine:

1) What kind of material this is (code repo, design docs, mixed, conversation logs, etc.)
2) What the user is really asking (restate in precise terms)
3) Who the likely audience is
4) What tone the pyramid should use

Respond in JSON with exactly these fields:
{
  "material_profile": "description of what the source material is",
  "interpreted_question": "the user's question restated precisely",
  "audience": "who will consume this pyramid",
  "tone": "what tone to use (technical, conversational, executive, etc.)"
}

Return ONLY the JSON object, no other text."#;

    let user_prompt = format!(
        r#"User's question: "{apex_question}"

Source material:
{folder_context}

Analyze this material and produce the characterization."#,
    );

    // ── 3. Call LLM using the "max" tier (frontier model) ────────────────
    // Override primary model to force the frontier model for characterization,
    // same pattern as question_decomposition::call_decomposition_llm.
    let mut char_config = llm_config.clone();
    char_config.primary_model = llm_config.fallback_model_2.clone();

    let temperature = 0.3;
    let max_tokens: usize = 2048;

    // Try up to 2 times on parse failure (same pattern as decomposition)
    for attempt in 0..2u32 {
        let temp = if attempt == 0 { temperature } else { 0.1 };

        let response = llm::call_model_unified(
            &char_config,
            system_prompt,
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
            "characterization LLM call complete"
        );

        // ── 4. Parse JSON response ───────────────────────────────────────
        match parse_characterization_response(&response.content) {
            Ok(result) => return Ok(result),
            Err(e) => {
                if attempt == 0 {
                    info!("characterization parse failed ({}), retrying with lower temperature", e);
                    continue;
                }
                return Err(e);
            }
        }
    }

    anyhow::bail!("characterization failed after 2 attempts")
}

// ── Internal helpers ─────────────────────────────────────────────────────────

/// Parse the LLM response into a CharacterizationResult.
///
/// Uses the same extract_json helper as the rest of the codebase to handle
/// markdown fences, think tags, and trailing commas.
fn parse_characterization_response(content: &str) -> Result<CharacterizationResult> {
    let json_value = llm::extract_json(content)?;

    let result: CharacterizationResult = serde_json::from_value(json_value).map_err(|e| {
        anyhow::anyhow!(
            "Failed to deserialize CharacterizationResult: {} — raw: {}",
            e,
            &content[..content.len().min(300)]
        )
    })?;

    Ok(result)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_clean_json() {
        let input = r#"{"material_profile":"Rust code repository","interpreted_question":"How does the build system work?","audience":"developers","tone":"technical"}"#;
        let result = parse_characterization_response(input).unwrap();
        assert_eq!(result.material_profile, "Rust code repository");
        assert_eq!(result.interpreted_question, "How does the build system work?");
        assert_eq!(result.audience, "developers");
        assert_eq!(result.tone, "technical");
    }

    #[test]
    fn parse_json_with_markdown_fences() {
        let input = r#"```json
{
  "material_profile": "Design documents",
  "interpreted_question": "What is the architecture?",
  "audience": "engineering team",
  "tone": "conversational"
}
```"#;
        let result = parse_characterization_response(input).unwrap();
        assert_eq!(result.material_profile, "Design documents");
    }

    #[test]
    fn parse_json_with_think_tags() {
        let input = r#"<think>Let me analyze this...</think>{"material_profile":"Mixed","interpreted_question":"Overview","audience":"general","tone":"executive"}"#;
        let result = parse_characterization_response(input).unwrap();
        assert_eq!(result.material_profile, "Mixed");
        assert_eq!(result.tone, "executive");
    }

    #[test]
    fn parse_json_with_trailing_comma() {
        let input = r#"{"material_profile":"Code","interpreted_question":"What?","audience":"devs","tone":"technical",}"#;
        let result = parse_characterization_response(input).unwrap();
        assert_eq!(result.material_profile, "Code");
    }

    #[test]
    fn parse_invalid_json_returns_error() {
        let input = "This is not JSON at all";
        assert!(parse_characterization_response(input).is_err());
    }

    #[test]
    fn parse_missing_field_returns_error() {
        let input = r#"{"material_profile":"Code","audience":"devs"}"#;
        assert!(parse_characterization_response(input).is_err());
    }
}
