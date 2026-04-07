// pyramid/llm.rs — OpenRouter API client with 3-tier model cascade
//
// Unified entry point: `call_model_unified` returns content + usage + generation_id.
// The legacy `call_model`, `call_model_with_usage`, and `call_model_structured`
// are thin wrappers for backward compatibility.

use anyhow::{anyhow, Result};
use regex::Regex;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::LazyLock;
use tokio::sync::Mutex as TokioMutex;
use tracing::{info, warn};

use super::types::TokenUsage;

// ── Global rate limiter: configurable sliding window ────────────────────────

static RATE_LIMITER: LazyLock<TokioMutex<VecDeque<std::time::Instant>>> =
    LazyLock::new(|| TokioMutex::new(VecDeque::new()));

/// Shared HTTP client — reuses TCP connections and TLS sessions across all LLM calls.
static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .pool_max_idle_per_host(8)
        .build()
        .expect("failed to build shared reqwest::Client")
});

/// Wait until we have capacity in the sliding window before making an LLM call.
/// Parameters come from Tier1Config (llm_rate_limit_max_requests, llm_rate_limit_window_secs).
async fn rate_limit_wait(max_requests: usize, window_secs: f64) {
    if max_requests == 0 {
        return; // rate limiting disabled
    }
    loop {
        let now = std::time::Instant::now();
        let mut window = RATE_LIMITER.lock().await;

        // Evict entries older than the window
        while let Some(&oldest) = window.front() {
            if now.duration_since(oldest).as_secs_f64() >= window_secs {
                window.pop_front();
            } else {
                break;
            }
        }

        if window.len() < max_requests {
            window.push_back(now);
            return;
        }

        // Window full — compute how long until the oldest entry expires
        let oldest = window[0];
        let wait = window_secs - now.duration_since(oldest).as_secs_f64();
        drop(window); // release lock while sleeping
        if wait > 0.0 {
            tokio::time::sleep(std::time::Duration::from_secs_f64(wait + 0.05)).await;
        }
    }
}

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
    /// HTTP status codes that trigger a retry with exponential backoff.
    pub retryable_status_codes: Vec<u16>,
    /// Base sleep duration (seconds) between retries before exponential backoff.
    pub retry_base_sleep_secs: u64,
    /// Number of prompt characters per timeout increment (for scaling formula).
    pub timeout_chars_per_increment: usize,
    /// Seconds added per increment of chars in the timeout scaling formula.
    pub timeout_increment_secs: u64,
    /// Max LLM requests per sliding window (0 = disabled).
    pub rate_limit_max_requests: usize,
    /// Sliding window duration in seconds for rate limiting.
    pub rate_limit_window_secs: f64,
    /// When true, log full LLM response bodies for failed/truncated calls to the debug log file.
    pub llm_debug_logging: bool,
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
            retryable_status_codes: vec![429, 403, 502, 503],
            retry_base_sleep_secs: 1,
            timeout_chars_per_increment: 100_000,
            timeout_increment_secs: 60,
            rate_limit_max_requests: 20,
            rate_limit_window_secs: 5.0,
            llm_debug_logging: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LlmCallOptions {
    pub min_timeout_secs: Option<u64>,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Resolve the context limit for the current model based on config.
fn resolve_context_limit(model: &str, config: &LlmConfig) -> usize {
    if model == config.primary_model {
        config.primary_context_limit
    } else if model == config.fallback_model_1 {
        config.fallback_1_context_limit
    } else {
        // fallback_model_2 or unknown — use the largest limit
        config.fallback_1_context_limit.max(config.primary_context_limit)
    }
}

/// Estimate token count for pre-flight model selection using tiktoken cl100k_base.
/// Falls back to len/4 if the tokenizer fails to initialize.
///
/// Runs on the blocking thread pool (8MB stack) via spawn_blocking because
/// tiktoken's fancy-regex engine is recursive and overflows the 2MB async
/// worker thread stack on large inputs (observed at 699+ doc prompts).
async fn estimate_tokens_llm(system_prompt: &str, user_prompt: &str) -> usize {
    let sys = system_prompt.to_string();
    let usr = user_prompt.to_string();
    tokio::task::spawn_blocking(move || {
        use std::sync::OnceLock;
        static BPE: OnceLock<Option<tiktoken_rs::CoreBPE>> = OnceLock::new();
        let bpe = BPE.get_or_init(|| tiktoken_rs::cl100k_base().ok());
        match bpe {
            Some(encoder) => {
                encoder.encode_with_special_tokens(&sys).len()
                    + encoder.encode_with_special_tokens(&usr).len()
            }
            None => (sys.len() + usr.len()) / 4,
        }
    })
    .await
    .unwrap_or_else(|_| (system_prompt.len() + user_prompt.len()) / 4)
}

/// Short model name for logging (part after the slash).
fn short_name(model: &str) -> &str {
    model.rsplit('/').next().unwrap_or(model)
}

fn compute_timeout(
    prompt_chars: usize,
    options: LlmCallOptions,
    base_secs: u64,
    max_secs: u64,
    chars_per_increment: usize,
    increment_secs: u64,
) -> std::time::Duration {
    let increments = if chars_per_increment > 0 {
        (prompt_chars / chars_per_increment) as u64
    } else {
        0
    };
    let derived_secs = std::cmp::min(max_secs, base_secs + increments * increment_secs);
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
    let est_input_tokens = estimate_tokens_llm(system_prompt, user_prompt).await;

    let mut use_model = if est_input_tokens > config.fallback_1_context_limit {
        info!("[fallback->{}]", short_name(&config.fallback_model_2));
        config.fallback_model_2.clone()
    } else if est_input_tokens > config.primary_context_limit {
        info!("[fallback->{}]", short_name(&config.fallback_model_1));
        config.fallback_model_1.clone()
    } else {
        config.primary_model.clone()
    };

    let client = &*HTTP_CLIENT;
    let url = "https://openrouter.ai/api/v1/chat/completions";

    // Scale timeout with prompt size: base + increment_secs per chars_per_increment, capped at max
    let prompt_chars = system_prompt.len() + user_prompt.len();
    let timeout = compute_timeout(
        prompt_chars,
        options,
        config.base_timeout_secs,
        config.max_timeout_secs,
        config.timeout_chars_per_increment,
        config.timeout_increment_secs,
    );

    for attempt in 0..config.max_retries {
        // Compute effective max_tokens: model context limit minus input, capped at 48K output.
        // Works around OpenRouter counting max_tokens as reserved space.
        let model_limit = resolve_context_limit(&use_model, config);
        let effective_max_tokens = model_limit
            .saturating_sub(est_input_tokens)
            .min(48_000)
            .max(1024);

        let mut body = serde_json::json!({
            "model": use_model,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_prompt}
            ],
            "temperature": temperature,
            "max_tokens": effective_max_tokens
        });
        if let Some(rf) = response_format {
            body.as_object_mut()
                .unwrap()
                .insert("response_format".to_string(), rf.clone());
        }

        // Rate limit: wait for sliding window capacity
        rate_limit_wait(config.rate_limit_max_requests, config.rate_limit_window_secs).await;

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
                if attempt + 1 < config.max_retries {
                    info!(
                        "  request error (timeout={}s, err={}), retry {}...",
                        timeout.as_secs(),
                        e,
                        attempt + 1
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(config.retry_base_sleep_secs)).await;
                    continue;
                }
                return Err(anyhow!(
                    "Request failed after {} attempts (timeout={}s): {}",
                    config.max_retries,
                    timeout.as_secs(),
                    e
                ));
            }
        };

        let status = resp.status().as_u16();

        // HTTP 400: read body, only cascade on context-exceeded errors
        if status == 400 {
            let body_400 = resp.text().await.unwrap_or_default();
            warn!(
                "[LLM] HTTP 400 from {} — body: {}",
                short_name(&use_model),
                &body_400[..body_400.len().min(500)],
            );

            let body_lower = body_400.to_lowercase();
            let is_context_exceeded = body_lower.contains("context")
                || body_lower.contains("too many tokens")
                || body_lower.contains("token limit");

            if is_context_exceeded && use_model != config.fallback_model_2 {
                let prev_model = use_model.clone();
                if use_model == config.primary_model {
                    use_model = config.fallback_model_1.clone();
                } else {
                    use_model = config.fallback_model_2.clone();
                }
                if response_format.is_some() {
                    warn!(
                        "[LLM] Context exceeded with response_format set — cascading from {} to {} (structured output may not be supported on fallback model)",
                        short_name(&prev_model),
                        short_name(&use_model),
                    );
                } else {
                    warn!(
                        "[LLM] Context exceeded on {}, cascading to {}",
                        short_name(&prev_model),
                        short_name(&use_model),
                    );
                }
                continue;
            } else {
                // Not context-related 400 — fall through to retry/backoff on same model
                warn!(
                    "[LLM] HTTP 400 (not context-exceeded) from {}: retrying on same model",
                    short_name(&use_model),
                );
                if attempt + 1 < config.max_retries {
                    let wait = config.retry_base_sleep_secs * 2u64.pow(attempt + 1);
                    tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                    continue;
                }
                return Err(anyhow!(
                    "HTTP 400 (not context-exceeded) after {} attempts: {}",
                    config.max_retries,
                    &body_400[..body_400.len().min(500)],
                ));
            }
        }

        // Retryable HTTP errors with exponential backoff (status codes from config)
        if config.retryable_status_codes.contains(&status) {
            let wait = config.retry_base_sleep_secs * 2u64.pow(attempt + 1);
            info!("  HTTP {}, waiting {}s...", status, wait);
            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            continue;
        }

        // Other non-success status
        if !resp.status().is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            if attempt + 1 < config.max_retries {
                info!("  HTTP {}, retry {}...", status, attempt + 1);
                tokio::time::sleep(std::time::Duration::from_secs(config.retry_base_sleep_secs)).await;
                continue;
            }
            return Err(anyhow!("HTTP {} after {} attempts: {}", status, config.max_retries, body_text));
        }

        let body_text = match resp.text().await {
            Ok(text) => text,
            Err(e) => {
                if attempt + 1 < config.max_retries {
                    info!(
                        "  response-read error (timeout={}s, err={}), retry {}...",
                        timeout.as_secs(),
                        e,
                        attempt + 1
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(config.retry_base_sleep_secs)).await;
                    continue;
                }
                return Err(anyhow!("Failed to read response after {} attempts: {}", config.max_retries, e));
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
                // Log the raw response that failed to parse so we can see what the LLM returned
                if config.llm_debug_logging {
                    let preview_len = body_text.len().min(2000);
                    warn!(
                        "[LLM-DEBUG] Raw response body that failed envelope parse (model={}, len={}):\n{}",
                        short_name(&use_model),
                        body_text.len(),
                        &body_text[..preview_len],
                    );
                }
                if attempt + 1 < config.max_retries {
                    info!("  parse error, retry {}...", attempt + 1);
                    tokio::time::sleep(std::time::Duration::from_secs(config.retry_base_sleep_secs)).await;
                    continue;
                }
                return Err(anyhow!("Failed to parse response after {} attempts: {}", config.max_retries, e));
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

        // Extract finish_reason for diagnostics
        let finish_reason = data
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("finish_reason"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        // Always log finish_reason so it shows up in normal tracing
        info!(
            "[LLM] model={} finish_reason={} prompt_tokens={} completion_tokens={}",
            short_name(&use_model),
            finish_reason,
            usage.prompt_tokens,
            usage.completion_tokens,
        );

        // Debug logging for truncated/abnormal responses
        if config.llm_debug_logging {
            let content_len = content.map(|s| s.len()).unwrap_or(0);
            if finish_reason != "stop" || content_len > 20_000 {
                let preview = content
                    .map(|s| &s[..s.len().min(2000)])
                    .unwrap_or("<null content>");
                warn!(
                    "[LLM-DEBUG] Abnormal response (model={}, finish_reason={}, content_len={}, prompt_tokens={}, completion_tokens={}):\n{}",
                    short_name(&use_model),
                    finish_reason,
                    content_len,
                    usage.prompt_tokens,
                    usage.completion_tokens,
                    preview,
                );
            }
        }

        match content {
            Some(text) => {
                return Ok(LlmResponse {
                    content: text.to_string(),
                    usage,
                    generation_id,
                });
            }
            None => {
                if attempt + 1 < config.max_retries {
                    info!("  null content, retry {}...", attempt + 1);
                    tokio::time::sleep(std::time::Duration::from_secs(config.retry_base_sleep_secs)).await;
                    continue;
                }
                return Err(anyhow!("Model returned null content after {} attempts", config.max_retries));
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

// ── Audited LLM Call (Live Pyramid Theatre) ─────────────────────────────────

use rusqlite::Connection;
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutexSync;

/// Context for recording LLM calls to the audit trail. Thread through build
/// pipelines to capture prompt/response for the Inspector modal.
#[derive(Debug, Clone)]
pub struct AuditContext {
    pub conn: Arc<TokioMutexSync<Connection>>,
    pub slug: String,
    pub build_id: String,
    pub node_id: Option<String>,
    pub step_name: String,
    pub call_purpose: String,
    pub depth: Option<i64>,
}

impl AuditContext {
    /// Create a child context for a different node/purpose while sharing the connection.
    pub fn for_node(&self, node_id: &str, call_purpose: &str, depth: i64) -> Self {
        Self {
            conn: Arc::clone(&self.conn),
            slug: self.slug.clone(),
            build_id: self.build_id.clone(),
            node_id: Some(node_id.to_string()),
            step_name: self.step_name.clone(),
            call_purpose: call_purpose.to_string(),
            depth: Some(depth),
        }
    }

    pub fn with_step(&self, step_name: &str) -> Self {
        Self {
            conn: Arc::clone(&self.conn),
            slug: self.slug.clone(),
            build_id: self.build_id.clone(),
            node_id: self.node_id.clone(),
            step_name: step_name.to_string(),
            call_purpose: self.call_purpose.clone(),
            depth: self.depth,
        }
    }
}

/// Audited LLM call: wraps `call_model_unified` with pre/post audit row writes.
///
/// 1. Inserts a pending audit row BEFORE the call
/// 2. Calls `call_model_unified`
/// 3. Updates the row with response + metrics AFTER
///
/// Returns `(LlmResponse, audit_row_id)`.
pub async fn call_model_audited(
    config: &LlmConfig,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f32,
    max_tokens: usize,
    response_format: Option<&serde_json::Value>,
    audit: &AuditContext,
) -> Result<(LlmResponse, i64)> {
    use super::db;

    // Insert pending row (async lock on tokio mutex)
    let audit_id = {
        let conn = audit.conn.lock().await;
        db::insert_llm_audit_pending(
            &conn,
            &audit.slug,
            &audit.build_id,
            audit.node_id.as_deref(),
            &audit.step_name,
            &audit.call_purpose,
            audit.depth,
            &config.primary_model,
            system_prompt,
            user_prompt,
        )?
    };

    let start = std::time::Instant::now();

    // Actual LLM call
    let result = call_model_unified(
        config,
        system_prompt,
        user_prompt,
        temperature,
        max_tokens,
        response_format,
    )
    .await;

    let latency_ms = start.elapsed().as_millis() as i64;

    match result {
        Ok(resp) => {
            let conn = audit.conn.lock().await;
            let _ = db::complete_llm_audit(
                &conn,
                audit_id,
                &resp.content,
                true,
                resp.usage.prompt_tokens,
                resp.usage.completion_tokens,
                latency_ms,
                resp.generation_id.as_deref(),
            );
            Ok((resp, audit_id))
        }
        Err(e) => {
            let conn = audit.conn.lock().await;
            let _ = db::fail_llm_audit(&conn, audit_id, &e.to_string());
            drop(conn);
            Err(e)
        }
    }
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

// ── Direct (non-cascading) entry point ─────────────────────────────────────

/// Call a specific OpenRouter model directly, bypassing the default 3-tier cascade.
///
/// Used for ASCII-art generation (WS-L) where the cascade would always pick
/// Mercury-2, which empirically fails at this task. The caller pins a specific
/// model_id (e.g. `x-ai/grok-4.20-beta`) and receives the raw content string.
///
/// Unlike `call_model_unified`, this function:
///   * Never cascades on HTTP 400 / context-exceeded.
///   * Takes no `temperature` / `response_format` (art generation is freeform).
///   * Uses a fixed conservative timeout (`base_timeout_secs`).
///
/// Retries on transient errors (`retryable_status_codes`, network, null content)
/// up to `config.max_retries`, same as the unified path.
pub async fn call_model_direct(
    config: &LlmConfig,
    model_id: &str,
    system_prompt: &str,
    user_prompt: &str,
    max_tokens: u32,
) -> Result<String> {
    let client = &*HTTP_CLIENT;
    let url = "https://openrouter.ai/api/v1/chat/completions";
    let timeout = std::time::Duration::from_secs(config.base_timeout_secs);

    for attempt in 0..config.max_retries {
        let body = serde_json::json!({
            "model": model_id,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_prompt}
            ],
            "max_tokens": max_tokens
        });

        rate_limit_wait(config.rate_limit_max_requests, config.rate_limit_window_secs).await;

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
                if attempt + 1 < config.max_retries {
                    info!("  [direct:{}] request error ({}), retry {}...", short_name(model_id), e, attempt + 1);
                    tokio::time::sleep(std::time::Duration::from_secs(config.retry_base_sleep_secs)).await;
                    continue;
                }
                return Err(anyhow!("call_model_direct({}) request failed: {}", model_id, e));
            }
        };

        let status = resp.status().as_u16();
        if config.retryable_status_codes.contains(&status) {
            let wait = config.retry_base_sleep_secs * 2u64.pow(attempt + 1);
            info!("  [direct:{}] HTTP {}, waiting {}s...", short_name(model_id), status, wait);
            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            continue;
        }
        if !resp.status().is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            if attempt + 1 < config.max_retries {
                info!("  [direct:{}] HTTP {}, retry {}...", short_name(model_id), status, attempt + 1);
                tokio::time::sleep(std::time::Duration::from_secs(config.retry_base_sleep_secs)).await;
                continue;
            }
            return Err(anyhow!("HTTP {} after {} attempts: {}", status, config.max_retries, body_text));
        }

        let body_text = match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                if attempt + 1 < config.max_retries {
                    tokio::time::sleep(std::time::Duration::from_secs(config.retry_base_sleep_secs)).await;
                    continue;
                }
                return Err(anyhow!("Failed to read response: {}", e));
            }
        };

        let data: Value = match parse_openrouter_response_body(&body_text) {
            Ok(v) => v,
            Err(e) => {
                if attempt + 1 < config.max_retries {
                    warn!("[direct:{}] parse error, retry {}: {}", short_name(model_id), attempt + 1, e);
                    tokio::time::sleep(std::time::Duration::from_secs(config.retry_base_sleep_secs)).await;
                    continue;
                }
                return Err(anyhow!("parse failed after {} attempts: {}", config.max_retries, e));
            }
        };

        let content = data
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str());

        match content {
            Some(text) => return Ok(text.to_string()),
            None => {
                if attempt + 1 < config.max_retries {
                    info!("  [direct:{}] null content, retry {}...", short_name(model_id), attempt + 1);
                    tokio::time::sleep(std::time::Duration::from_secs(config.retry_base_sleep_secs)).await;
                    continue;
                }
                return Err(anyhow!("null content after {} attempts", config.max_retries));
            }
        }
    }

    Err(anyhow!("call_model_direct({}): max retries exceeded", model_id))
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
        let defaults = LlmConfig::default();
        let timeout = compute_timeout(
            33_000,
            LlmCallOptions {
                min_timeout_secs: Some(420),
            },
            defaults.base_timeout_secs,
            defaults.max_timeout_secs,
            defaults.timeout_chars_per_increment,
            defaults.timeout_increment_secs,
        );
        assert_eq!(timeout.as_secs(), 420);
    }

    #[test]
    fn test_compute_timeout_scales_with_prompt_size() {
        let defaults = LlmConfig::default();
        // 200k chars = 2 increments * 60s = 120s added to base 120s = 240s
        let timeout = compute_timeout(
            200_000,
            LlmCallOptions::default(),
            defaults.base_timeout_secs,
            defaults.max_timeout_secs,
            defaults.timeout_chars_per_increment,
            defaults.timeout_increment_secs,
        );
        assert_eq!(timeout.as_secs(), 240);
    }

    #[test]
    fn test_compute_timeout_capped_at_max() {
        let defaults = LlmConfig::default();
        // Very large prompt should be capped at max_timeout_secs (600)
        let timeout = compute_timeout(
            10_000_000,
            LlmCallOptions::default(),
            defaults.base_timeout_secs,
            defaults.max_timeout_secs,
            defaults.timeout_chars_per_increment,
            defaults.timeout_increment_secs,
        );
        assert_eq!(timeout.as_secs(), 600);
    }
}
