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

use std::path::Path;

use anyhow::Result;
use tracing::{info, warn};

use super::llm::{self, LlmConfig};
use super::question_decomposition;
use super::types::CharacterizationResult;
use super::Tier1Config;

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
    tier1: &Tier1Config,
    chains_dir: Option<&Path>,
) -> Result<CharacterizationResult> {
    characterize_with_fallback(
        source_path,
        apex_question,
        llm_config,
        None,
        tier1,
        chains_dir,
    )
    .await
}

/// Characterize source material with an optional L0 summary fallback.
///
/// When source files are no longer on disk (moved, deleted, remote), the folder
/// map will be empty. If `l0_fallback` is provided, it's used instead — this
/// gives the LLM the same quality of context from the existing pyramid's L0
/// summaries rather than hard-failing.
pub async fn characterize_with_fallback(
    source_path: &str,
    apex_question: &str,
    llm_config: &LlmConfig,
    l0_fallback: Option<&str>,
    tier1: &Tier1Config,
    chains_dir: Option<&Path>,
) -> Result<CharacterizationResult> {
    // ── 1. Build folder map from source path for LLM context ─────────────
    // Defense-in-depth: parse JSON array source_paths (e.g. '["/path"]') into
    // the first path. Existing DB rows may have this format from the old
    // AddWorkspace flow that used JSON.stringify(paths).
    let effective_path = if source_path.trim().starts_with('[') {
        serde_json::from_str::<Vec<String>>(source_path)
            .ok()
            .and_then(|v| v.into_iter().next())
            .unwrap_or_else(|| source_path.to_string())
    } else {
        source_path.to_string()
    };
    let folder_map = question_decomposition::build_folder_map(&effective_path);
    let folder_context: String = match folder_map.as_deref() {
        Some(map) if !map.is_empty() => map.to_string(),
        _ => {
            // Folder map is empty — source files may be gone. Fall back to L0 summaries.
            match l0_fallback {
                Some(fallback) if !fallback.is_empty() => {
                    info!("source path unavailable, falling back to L0 summaries for characterization");
                    fallback.to_string()
                }
                _ => {
                    return Err(anyhow::anyhow!(
                        "Source path '{}' is not accessible and no L0 fallback available",
                        effective_path
                    ))
                }
            }
        }
    };
    let folder_context = folder_context.as_str();

    // ── 2. Construct prompts ─────────────────────────────────────────────
    let system_prompt = match chains_dir
        .map(|d| d.join("prompts/question/characterize.md"))
        .and_then(|p| std::fs::read_to_string(&p).ok())
    {
        Some(template) => template,
        None => {
            warn!("characterize.md not found — using inline fallback");
            r#"You are analyzing a folder of source material to prepare for building a knowledge pyramid. Given the user's question and the folder contents below, determine:

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

Return ONLY the JSON object, no other text."#.to_string()
        }
    };

    let user_prompt = format!(
        r#"User's question: "{apex_question}"

Source material:
{folder_context}

Analyze this material and produce the characterization."#,
    );

    // ── 3. Call LLM ────────────────────────────────────────────────────────
    // Model selection is controlled by YAML chain definitions, not Rust overrides.
    // See Inviolable #4: "YAML is the single source of truth for model selection."

    let temperature = tier1.characterize_temperature;
    let max_tokens = tier1.characterize_max_tokens;

    // Try up to 2 times on parse failure (same pattern as decomposition)
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
            "characterization LLM call complete"
        );

        // ── 4. Parse JSON response ───────────────────────────────────────
        match parse_characterization_response(&response.content) {
            Ok(result) => return Ok(result),
            Err(e) => {
                if attempt == 0 {
                    info!(
                        "characterization parse failed ({}), retrying with lower temperature",
                        e
                    );
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
        assert_eq!(
            result.interpreted_question,
            "How does the build system work?"
        );
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
