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

use anyhow::{anyhow, Result};
use tracing::{info, warn};

use super::llm::{self, LlmConfig};
use super::question_decomposition;
use super::step_context::{compute_prompt_hash, StepContext};
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
    // Characterize is a judgment call (see docstring at top of file) — the
    // "max" tier is the declared intent. Resolve that tier through the
    // provider registry before dispatch so the walker sees a provider-valid
    // model id instead of whatever `primary_model` happens to carry (which
    // can be an Ollama tag from a prior Local Mode session and will crash
    // an OpenRouter-served route).
    let temperature = tier1.characterize_temperature;
    let max_tokens = tier1.characterize_max_tokens;

    let (resolved_tier_name, resolved_model_id, resolved_provider_id, call_config): (
        &'static str,
        String,
        Option<String>,
        LlmConfig,
    ) = match llm_config
        .provider_registry
        .as_ref()
        .and_then(|reg| reg.resolve_tier("max", None, None, None).ok())
    {
        Some(resolved) => {
            let model_id = resolved.tier.model_id.clone();
            let provider_id = resolved.provider.id.clone();
            let _context_limit = resolved.tier.context_limit;
            // W3c: no longer overriding LlmConfig.primary_model /
            // primary_context_limit — those fields are deleted. The
            // resolved model_id below threads into dispatch via
            // LlmCallOptions.model_override on the call path.
            let cloned = llm_config.clone();
            ("max", model_id, Some(provider_id), cloned)
        }
        None => {
            // W3c: legacy `llm_config.primary_model` fallback removed.
            // Characterize is a top-level entry point with no outer
            // Decision; if the registry can't resolve the "max" tier we
            // can't dispatch at all.
            return Err(anyhow!(
                "characterize: provider registry has no 'max' tier routing \
                 and walker-v3 W3c removed the legacy LlmConfig.primary_model \
                 fallback. Configure a walker_provider_openrouter contribution \
                 with a 'max' slot model_list entry.",
            ));
        }
    };

    // Try up to 2 times on parse failure (same pattern as decomposition)
    for attempt in 0..2u32 {
        let temp = if attempt == 0 { temperature } else { 0.1 };

        // Build StepContext inline so the resolved tier + model id are
        // stamped correctly. The retrofit helper labels everything as
        // tier="primary" with model=primary_model, which is the hardwiring
        // this fix exists to bypass.
        let cache_ctx: Option<StepContext> = call_config.cache_access.as_ref().and_then(|cache| {
            if system_prompt.is_empty() {
                return None;
            }
            let prompt_hash = compute_prompt_hash(&system_prompt);
            let mut ctx = StepContext::new(
                cache.slug.clone(),
                cache.build_id.clone(),
                "characterize",
                "characterize",
                0,
                None,
                cache.db_path.to_string(),
            )
            .with_model_resolution(resolved_tier_name, resolved_model_id.clone())
            .with_prompt_hash(prompt_hash);
            if let Some(ref pid) = resolved_provider_id {
                ctx = ctx.with_provider(pid.clone());
            }
            if let Some(ref bus) = cache.bus {
                ctx = ctx.with_bus(bus.clone());
            }
            if let Some(ref cn) = cache.chain_name {
                let ct = cache.content_type.as_deref().unwrap_or("");
                ctx = ctx.with_chain_context(cn.clone(), ct.to_string());
            }
            Some(ctx)
        });

        let response = llm::call_model_unified_and_ctx(
            &call_config,
            cache_ctx.as_ref(),
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
