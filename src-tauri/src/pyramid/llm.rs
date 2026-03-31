// pyramid/llm.rs — OpenRouter API client with 3-tier model cascade
//
// Unified entry point: `call_model_unified` returns content + usage + generation_id.
// The legacy `call_model`, `call_model_with_usage`, and `call_model_structured`
// are thin wrappers for backward compatibility.

use anyhow::{anyhow, Result};
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;
use tracing::{info, warn};

use super::types::TokenUsage;

// ── Response types ───────────────────────────────────────────────────────────

/// Unified response from the LLM client. Every call returns content, token usage,
/// and the OpenRouter generation ID (for cost observatory lookups).
#[derive(Debug, Clone)]
pub struct LlmResponse {
    /// The text content returned by the model.
    pub content: String,
    /// Token usage from the API response (prompt + completion tokens).
    pub usage: TokenUsage,
    /// OpenRouter generation ID (the top-level `id` field in the response JSON).
    /// Used for cost observatory correlation. None if the API didn't return one.
    pub generation_id: Option<String>,
}

// ── Config ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub api_key: String,
    pub auth_token: String,
    pub primary_model: String,
    pub fallback_model_1: String,
    pub fallback_model_2: String,
    pub primary_context_limit: usize,
    pub fallback_1_context_limit: usize,
    /// Max retry attempts for LLM calls (loaded from Tier1Config).
    pub max_retries: u32,
    /// Base timeout in seconds for LLM calls (loaded from Tier2Config).
    pub base_timeout_secs: u64,
    /// Maximum timeout in seconds for LLM calls (loaded from Tier2Config).
    pub max_timeout_secs: u64,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            auth_token: String::new(),
            primary_model: "inception/mercury-2".into(),
            fallback_model_1: "qwen/qwen3.5-flash-02-23".into(),
            fallback_model_2: "x-ai/grok-4.20-beta".into(),
            primary_context_limit: 120_000,
            fallback_1_context_limit: 900_000,
            max_retries: 5,
            base_timeout_secs: 120,
            max_timeout_secs: 600,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LlmCallOptions {
    pub min_timeout_secs: Option<u64>,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Short model name for logging (part after the slash).
fn short_name(model: &str) -> &str {
    model.rsplit('/').next().unwrap_or(model)
}

fn compute_timeout(
    prompt_chars: usize,
    options: LlmCallOptions,
    base_secs: u64,
    max_secs: u64,
) -> std::time::Duration {
    let derived_secs = std::cmp::min(max_secs, base_secs + (prompt_chars / 100_000) as u64 * 60);
    let timeout_secs = options.min_timeout_secs.unwrap_or(0).max(derived_secs);
    std::time::Duration::from_secs(timeout_secs)
}

fn sanitize_json_candidate(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
        .collect()
}

fn parse_openrouter_response_body(body_text: &str) -> Result<Value> {
    let trimmed = body_text.trim();
    if trimmed.is_empty() {
        anyhow::bail!("empty response body");
    }

    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return Ok(value);
    }

    let sse_payload = trimmed
        .lines()
        .filter_map(|line| line.trim().strip_prefix("data:"))
        .map(str::trim)
        .filter(|line| !line.is_empty() && *line != "[DONE]")
        .collect::<Vec<_>>()
        .join("\n");
    if !sse_payload.is_empty() {
        if let Ok(value) = serde_json::from_str::<Value>(&sse_payload) {
            return Ok(value);
        }
    }

    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) {
        if end >= start {
            let candidate = &trimmed[start..=end];
            if let Ok(value) = serde_json::from_str::<Value>(candidate) {
                return Ok(value);
            }

            let sanitized = sanitize_json_candidate(candidate);
            if let Ok(value) = serde_json::from_str::<Value>(&sanitized) {
                return Ok(value);
            }
        }
    }

    let sanitized = sanitize_json_candidate(trimmed);
    if sanitized != trimmed {
        if let Ok(value) = serde_json::from_str::<Value>(&sanitized) {
            return Ok(value);
        }
    }

    anyhow::bail!(
        "could not parse OpenRouter JSON envelope from: {}",
        &trimmed[..trimmed.len().min(400)]
    )
}

// ── Unified entry point ──────────────────────────────────────────────────────

/// Unified LLM call: returns content + usage + generation_id in a single response.
///
/// This is the canonical entry point. All other `call_model*` functions delegate here.
/// Supports optional `response_format` for structured output enforcement.
pub async fn call_model_unified(
    config: &LlmConfig,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f32,
    max_tokens: usize,
    response_format: Option<&serde_json::Value>,
) -> Result<LlmResponse> {
    call_model_unified_with_options(
        config,
        system_prompt,
        user_prompt,
        temperature,
        max_tokens,
        response_format,
        LlmCallOptions::default(),
    )
    .await
}

pub async fn call_model_unified_with_options(
    config: &LlmConfig,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f32,
    max_tokens: usize,
    response_format: Option<&serde_json::Value>,
    options: LlmCallOptions,
) -> Result<LlmResponse> {
    // Model selection based on INPUT size only — max_tokens (output budget) is
    // irrelevant to whether the prompt fits in the model's context window.
    let est_input_tokens = (system_prompt.len() + user_prompt.len()) / 4;

    let mut use_model = if est_input_tokens > config.fallback_1_context_limit {
        info!("[fallback->{}]", short_name(&config.fallback_model_2));
        config.fallback_model_2.clone()
    } else if est_input_tokens > config.primary_context_limit {
        info!("[fallback->{}]", short_name(&config.fallback_model_1));
        config.fallback_model_1.clone()
    } else {
        config.primary_model.clone()
    };

    let client = reqwest::Client::new();
    let url = "https://openrouter.ai/api/v1/chat/completions";

    // Scale timeout with prompt size: base + 60s per 100K chars, capped at max
    let prompt_chars = system_prompt.len() + user_prompt.len();
    let timeout = compute_timeout(
        prompt_chars,
        options,
        config.base_timeout_secs,
        config.max_timeout_secs,
    );

    for attempt in 0..config.max_retries {
        let mut body = serde_json::json!({
            "model": use_model,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_prompt}
            ],
            "temperature": temperature,
            "max_tokens": max_tokens
        });
        if let Some(rf) = response_format {
            body.as_object_mut()
                .unwrap()
                .insert("response_format".to_string(), rf.clone());
        }

        let resp = client
            .post(url)
            .header("Authorization", format!("Bearer {}", config.api_key))
            .header("Content-Type", "application/json")
            .header("HTTP-Referer", "https://newsbleach.com")
            .header("X-Title", "Wire Pyramid Engine")
            .timeout(timeout)
            .json(&body)
            .send()
            .await;

        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                if attempt < 4 {
                    info!(
                        "  request error (timeout={}s, err={}), retry {}...",
                        timeout.as_secs(),
                        e,
                        attempt + 1
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    continue;
                }
                return Err(anyhow!(
                    "Request failed after 5 attempts (timeout={}s): {}",
                    timeout.as_secs(),
                    e
                ));
            }
        };

        let status = resp.status().as_u16();

        // HTTP 400: cascade to next model if not already on last fallback
        if status == 400 && use_model != config.fallback_model_2 {
            let prev_model = use_model.clone();
            if use_model == config.primary_model {
                use_model = config.fallback_model_1.clone();
            } else {
                use_model = config.fallback_model_2.clone();
            }
            if response_format.is_some() {
                warn!(
                    "[LLM] HTTP 400 with response_format set — cascading from {} to {} (structured output may not be supported on fallback model)",
                    short_name(&prev_model),
                    short_name(&use_model),
                );
            } else {
                info!("[400->{}]", short_name(&use_model));
            }
            continue;
        }

        // Retryable HTTP errors with exponential backoff
        if matches!(status, 429 | 403 | 502 | 503) {
            let wait = 2u64.pow(attempt + 1);
            info!("  HTTP {}, waiting {}s...", status, wait);
            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            continue;
        }

        // Other non-success status
        if !resp.status().is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            if attempt < 4 {
                info!("  HTTP {}, retry {}...", status, attempt + 1);
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
            return Err(anyhow!("HTTP {} after 5 attempts: {}", status, body_text));
        }

        let body_text = match resp.text().await {
            Ok(text) => text,
            Err(e) => {
                if attempt < 4 {
                    info!(
                        "  response-read error (timeout={}s, err={}), retry {}...",
                        timeout.as_secs(),
                        e,
                        attempt + 1
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    continue;
                }
                return Err(anyhow!("Failed to read response after 5 attempts: {}", e));
            }
        };

        // Parse response JSON
        let data: Value = match parse_openrouter_response_body(&body_text) {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    "[LLM] response envelope parse failed on {} attempt {}: {}",
                    short_name(&use_model),
                    attempt + 1,
                    e
                );
                if attempt < 4 {
                    info!("  parse error, retry {}...", attempt + 1);
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    continue;
                }
                return Err(anyhow!("Failed to parse response after 5 attempts: {}", e));
            }
        };

        // Extract content from choices[0].message.content
        let content = data
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str());

        // Extract token usage, falling back to zeros if missing
        let usage = TokenUsage {
            prompt_tokens: data
                .get("usage")
                .and_then(|u| u.get("prompt_tokens"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            completion_tokens: data
                .get("usage")
                .and_then(|u| u.get("completion_tokens"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
        };

        // Extract generation_id from the top-level `id` field in the OpenRouter response
        let generation_id = data
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        match content {
            Some(text) => {
                return Ok(LlmResponse {
                    content: text.to_string(),
                    usage,
                    generation_id,
                });
            }
            None => {
                if attempt < 4 {
                    info!("  null content, retry {}...", attempt + 1);
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    continue;
                }
                return Err(anyhow!("Model returned null content after 5 attempts"));
            }
        }
    }

    Err(anyhow!("Max retries exceeded"))
}

// ── Backward-compatible wrappers ─────────────────────────────────────────────

/// Call OpenRouter with automatic model cascade and retry logic.
/// Falls back to larger-context models when input exceeds primary model's limit.
/// Retries on 429/403/502/503, null content, and JSON parse failures.
///
/// Returns only the content string. For usage/generation_id, use `call_model_unified`.
pub async fn call_model(
    config: &LlmConfig,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f32,
    max_tokens: usize,
) -> Result<String> {
    let resp = call_model_unified(
        config,
        system_prompt,
        user_prompt,
        temperature,
        max_tokens,
        None,
    )
    .await?;
    Ok(resp.content)
}

/// Call OpenRouter with automatic model cascade and retry logic.
/// Same as `call_model()` but also returns token usage from the API response.
///
/// For generation_id as well, use `call_model_unified`.
pub async fn call_model_with_usage(
    config: &LlmConfig,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f32,
    max_tokens: usize,
) -> Result<(String, TokenUsage)> {
    let resp = call_model_unified(
        config,
        system_prompt,
        user_prompt,
        temperature,
        max_tokens,
        None,
    )
    .await?;
    Ok((resp.content, resp.usage))
}

/// Call OpenRouter with structured output enforcement via JSON schema.
///
/// Returns only the content string. For usage/generation_id, use `call_model_unified`
/// with a manually constructed `response_format`.
pub async fn call_model_structured(
    config: &LlmConfig,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f32,
    max_tokens: usize,
    response_schema: &serde_json::Value,
    schema_name: &str,
) -> Result<String> {
    let response_format = serde_json::json!({
        "type": "json_schema",
        "json_schema": {
            "name": schema_name,
            "strict": true,
            "schema": response_schema
        }
    });
    let resp = call_model_unified(
        config,
        system_prompt,
        user_prompt,
        temperature,
        max_tokens,
        Some(&response_format),
    )
    .await?;
    Ok(resp.content)
}

// ── JSON extraction ──────────────────────────────────────────────────────────

/// Extract JSON from a response that may include markdown fences or thinking tags.
pub fn extract_json(text: &str) -> Result<Value> {
    let mut text = text.trim().to_string();

    // Strip <think>...</think> tags
    static THINK_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)<think>.*?</think>").unwrap());
    text = THINK_RE.replace_all(&text, "").trim().to_string();

    // Remove markdown fences (``` lines)
    if text.contains("```") {
        let lines: Vec<&str> = text
            .lines()
            .filter(|l| !l.trim().starts_with("```"))
            .collect();
        text = lines.join("\n").trim().to_string();
    }

    // Find JSON delimiters — try both object {…} and array […]
    let obj_start = text.find('{');
    let obj_end = text.rfind('}');
    let arr_start = text.find('[');
    let arr_end = text.rfind(']');

    // Pick the outermost valid JSON range (object or array, whichever starts first)
    let (start, end) = match ((obj_start, obj_end), (arr_start, arr_end)) {
        ((Some(os), Some(oe)), (Some(as_), Some(ae))) if oe >= os && ae >= as_ => {
            if os <= as_ {
                (os, oe)
            } else {
                (as_, ae)
            }
        }
        ((Some(os), Some(oe)), _) if oe >= os => (os, oe),
        (_, (Some(as_), Some(ae))) if ae >= as_ => (as_, ae),
        _ => {
            return Err(anyhow!(
                "No JSON found in: {}",
                &text[..text.len().min(200)]
            ))
        }
    };

    let slice = &text[start..=end];

    // Try parsing as-is
    if let Ok(v) = serde_json::from_str::<Value>(slice) {
        return Ok(v);
    }

    // Fix trailing commas and retry
    static COMMA_BRACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r",\s*}").unwrap());
    static COMMA_BRACKET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r",\s*]").unwrap());
    let fixed = COMMA_BRACE.replace_all(slice, "}");
    let fixed = COMMA_BRACKET.replace_all(&fixed, "]");

    if let Ok(v) = serde_json::from_str::<Value>(&fixed) {
        return Ok(v);
    }

    Err(anyhow!(
        "No JSON found in: {}",
        &text[..text.len().min(200)]
    ))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_llm_response_from_openrouter_json() {
        // Simulates parsing the fields that call_model_unified extracts
        let data: Value = serde_json::json!({
            "id": "gen-abc123def456",
            "choices": [{
                "message": {
                    "content": "Hello, world!"
                }
            }],
            "usage": {
                "prompt_tokens": 42,
                "completion_tokens": 7
            }
        });

        let content = data
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap();

        let usage = TokenUsage {
            prompt_tokens: data
                .get("usage")
                .and_then(|u| u.get("prompt_tokens"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            completion_tokens: data
                .get("usage")
                .and_then(|u| u.get("completion_tokens"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
        };

        let generation_id = data
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        assert_eq!(content, "Hello, world!");
        assert_eq!(usage.prompt_tokens, 42);
        assert_eq!(usage.completion_tokens, 7);
        assert_eq!(generation_id.as_deref(), Some("gen-abc123def456"));
    }

    #[test]
    fn test_generation_id_missing_gracefully() {
        // OpenRouter may omit the id field in some error/edge cases
        let data: Value = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "response text"
                }
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5
            }
        });

        let generation_id = data
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        assert_eq!(generation_id, None);
    }

    #[test]
    fn test_usage_missing_gracefully() {
        // If usage block is absent, we fall back to zeros
        let data: Value = serde_json::json!({
            "id": "gen-xyz",
            "choices": [{
                "message": {
                    "content": "ok"
                }
            }]
        });

        let usage = TokenUsage {
            prompt_tokens: data
                .get("usage")
                .and_then(|u| u.get("prompt_tokens"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            completion_tokens: data
                .get("usage")
                .and_then(|u| u.get("completion_tokens"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
        };

        assert_eq!(usage.prompt_tokens, 0);
        assert_eq!(usage.completion_tokens, 0);
    }

    #[test]
    fn test_parse_openrouter_response_body_accepts_prefixed_json() {
        let raw = "noise before {\"id\":\"gen-1\",\"choices\":[{\"message\":{\"content\":\"hi\"}}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2}} trailing";
        let parsed = parse_openrouter_response_body(raw).unwrap();
        assert_eq!(parsed["id"], "gen-1");
    }

    #[test]
    fn test_parse_openrouter_response_body_accepts_sse_data_lines() {
        let raw = "data: {\"id\":\"gen-2\",\"choices\":[{\"message\":{\"content\":\"hi\"}}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2}}\n\ndata: [DONE]";
        let parsed = parse_openrouter_response_body(raw).unwrap();
        assert_eq!(parsed["id"], "gen-2");
    }

    #[test]
    fn test_extract_json_basic() {
        let input = r#"Here is the result: {"key": "value"} done"#;
        let result = extract_json(input).unwrap();
        assert_eq!(result["key"], "value");
    }

    #[test]
    fn test_extract_json_with_think_tags() {
        let input = r#"<think>reasoning here</think>{"answer": 42}"#;
        let result = extract_json(input).unwrap();
        assert_eq!(result["answer"], 42);
    }

    #[test]
    fn test_extract_json_with_markdown_fences() {
        let input = "```json\n{\"a\": 1}\n```";
        let result = extract_json(input).unwrap();
        assert_eq!(result["a"], 1);
    }

    #[test]
    fn test_extract_json_trailing_comma() {
        let input = r#"{"items": ["a", "b",]}"#;
        let result = extract_json(input).unwrap();
        assert_eq!(result["items"][0], "a");
    }

    #[test]
    fn test_compute_timeout_respects_min_timeout_floor() {
        let timeout = compute_timeout(
            33_000,
            LlmCallOptions {
                min_timeout_secs: Some(420),
            },
        );
        assert_eq!(timeout.as_secs(), 420);
    }
}
