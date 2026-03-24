// pyramid/llm.rs — OpenRouter API client with 3-tier model cascade

use anyhow::{anyhow, Result};
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;
use tracing::info;

use super::types::TokenUsage;

#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub api_key: String,
    pub auth_token: String,
    pub primary_model: String,
    pub fallback_model_1: String,
    pub fallback_model_2: String,
    pub primary_context_limit: usize,
    pub fallback_1_context_limit: usize,
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
        }
    }
}

/// Short model name for logging (part after the slash).
fn short_name(model: &str) -> &str {
    model.rsplit('/').next().unwrap_or(model)
}

/// Call OpenRouter with automatic model cascade and retry logic.
/// Falls back to larger-context models when input exceeds primary model's limit.
/// Retries on 429/403/502/503, null content, and JSON parse failures.
pub async fn call_model(
    config: &LlmConfig,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f32,
    max_tokens: usize,
) -> Result<String> {
    call_model_inner(config, system_prompt, user_prompt, temperature, max_tokens, None).await
}

/// Call OpenRouter with structured output enforcement via JSON schema.
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
    call_model_inner(config, system_prompt, user_prompt, temperature, max_tokens, Some(&response_format)).await
}

async fn call_model_inner(
    config: &LlmConfig,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f32,
    max_tokens: usize,
    response_format: Option<&serde_json::Value>,
) -> Result<String> {
    let est_total = (system_prompt.len() + user_prompt.len()) / 4 + max_tokens;

    // Pick initial model based on estimated token usage
    let mut use_model = if est_total > config.fallback_1_context_limit {
        info!("[fallback->{}]", short_name(&config.fallback_model_2));
        config.fallback_model_2.clone()
    } else if est_total > config.primary_context_limit {
        info!("[fallback->{}]", short_name(&config.fallback_model_1));
        config.fallback_model_1.clone()
    } else {
        config.primary_model.clone()
    };

    let client = reqwest::Client::new();
    let url = "https://openrouter.ai/api/v1/chat/completions";

    for attempt in 0..5u32 {
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
            body.as_object_mut().unwrap().insert("response_format".to_string(), rf.clone());
        }

        let resp = client
            .post(url)
            .header("Authorization", format!("Bearer {}", config.api_key))
            .header("Content-Type", "application/json")
            .header("HTTP-Referer", "https://newsbleach.com")
            .header("X-Title", "Wire Pyramid Engine")
            .timeout(std::time::Duration::from_secs(120))
            .json(&body)
            .send()
            .await;

        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                if attempt < 4 {
                    info!("  request error, retry {}...", attempt + 1);
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    continue;
                }
                return Err(anyhow!("Request failed after 5 attempts: {}", e));
            }
        };

        let status = resp.status().as_u16();

        // HTTP 400: cascade to next model if not already on last fallback
        if status == 400 && use_model != config.fallback_model_2 {
            if use_model == config.primary_model {
                use_model = config.fallback_model_1.clone();
            } else {
                use_model = config.fallback_model_2.clone();
            }
            info!("[400->{}]", short_name(&use_model));
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

        // Parse response JSON
        let data: Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
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

        match content {
            Some(text) => return Ok(text.to_string()),
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

/// Call OpenRouter with automatic model cascade and retry logic.
/// Same as `call_model()` but also returns token usage from the API response.
pub async fn call_model_with_usage(
    config: &LlmConfig,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f32,
    max_tokens: usize,
) -> Result<(String, TokenUsage)> {
    let est_total = (system_prompt.len() + user_prompt.len()) / 4 + max_tokens;

    let mut use_model = if est_total > config.fallback_1_context_limit {
        info!("[fallback->{}]", short_name(&config.fallback_model_2));
        config.fallback_model_2.clone()
    } else if est_total > config.primary_context_limit {
        info!("[fallback->{}]", short_name(&config.fallback_model_1));
        config.fallback_model_1.clone()
    } else {
        config.primary_model.clone()
    };

    let client = reqwest::Client::new();
    let url = "https://openrouter.ai/api/v1/chat/completions";

    for attempt in 0..5u32 {
        let body = serde_json::json!({
            "model": use_model,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_prompt}
            ],
            "temperature": temperature,
            "max_tokens": max_tokens
        });

        let resp = client
            .post(url)
            .header("Authorization", format!("Bearer {}", config.api_key))
            .header("Content-Type", "application/json")
            .header("HTTP-Referer", "https://newsbleach.com")
            .header("X-Title", "Wire Pyramid Engine")
            .timeout(std::time::Duration::from_secs(120))
            .json(&body)
            .send()
            .await;

        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                if attempt < 4 {
                    info!("  request error, retry {}...", attempt + 1);
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    continue;
                }
                return Err(anyhow!("Request failed after 5 attempts: {}", e));
            }
        };

        let status = resp.status().as_u16();

        if status == 400 && use_model != config.fallback_model_2 {
            if use_model == config.primary_model {
                use_model = config.fallback_model_1.clone();
            } else {
                use_model = config.fallback_model_2.clone();
            }
            info!("[400->{}]", short_name(&use_model));
            continue;
        }

        if matches!(status, 429 | 403 | 502 | 503) {
            let wait = 2u64.pow(attempt + 1);
            info!("  HTTP {}, waiting {}s...", status, wait);
            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            continue;
        }

        if !resp.status().is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            if attempt < 4 {
                info!("  HTTP {}, retry {}...", status, attempt + 1);
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
            return Err(anyhow!("HTTP {} after 5 attempts: {}", status, body_text));
        }

        let data: Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                if attempt < 4 {
                    info!("  parse error, retry {}...", attempt + 1);
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    continue;
                }
                return Err(anyhow!("Failed to parse response after 5 attempts: {}", e));
            }
        };

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

        match content {
            Some(text) => return Ok((text.to_string(), usage)),
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
