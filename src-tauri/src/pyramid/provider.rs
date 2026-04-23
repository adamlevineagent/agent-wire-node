// pyramid/provider.rs — Phase 3 provider registry and LlmProvider trait.
//
// Per `docs/specs/provider-registry.md`, this module replaces the
// hardcoded OpenRouter URL in `llm.rs` with a pluggable provider trait.
// Providers are rows in the `pyramid_providers` table. A tier routing
// table maps tier names (`fast_extract`, `web`, `synth_heavy`,
// `stale_remote`, `stale_local`) to a provider + model pair. Per-step
// overrides let users pin a specific model for a specific chain step.
//
// At call time the call path resolves:
//   step.model_tier
//     → step override (if present, per slug/chain/step)
//     → tier routing entry
//     → provider row
//     → provider trait impl
//     → credentials.resolve_var(api_key_ref) → ResolvedSecret
//     → provider.prepare_headers(&secret) → Vec<(String, String)>
//     → provider.chat_completions_url()
//     → provider.augment_request_body(&mut body, &metadata)
//     → HTTP POST
//     → provider.parse_response(&body_text) → (content, usage, generation_id, cost)
//
// The registry loads all rows at startup and mutates in place when the
// IPC surface updates them. Resolution is in-memory and does not touch
// the database.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use super::credentials::{CredentialStore, ResolvedSecret};
use super::types::TokenUsage;

// ── Shared types ────────────────────────────────────────────────────────────

/// Metadata attached to a single LLM request. Providers inject this into
/// the outgoing request body so downstream observability (OpenRouter
/// Broadcast, OTLP) can correlate per-build cost back to specific
/// chain steps.
///
/// Phase 11 extended `RequestMetadata` with `layer`, `check_type`, and
/// `chunk_index` so the full `trace.metadata.*` namespace reaches
/// downstream OTLP destinations. Per `docs/specs/
/// evidence-triage-and-dadbear.md` Part 4, these attribute keys are
/// used by the webhook correlator to look up the originating
/// `pyramid_cost_log` row on confirmation.
#[derive(Debug, Clone, Default)]
pub struct RequestMetadata {
    pub build_id: Option<String>,
    pub slug: Option<String>,
    pub chain_id: Option<String>,
    pub step_name: Option<String>,
    pub depth: Option<i64>,
    pub node_identity: Option<String>,
    pub session_id: Option<String>,
    /// Phase 11: chunk index for per-chunk step calls (e.g., source
    /// extraction). Serialized under `trace.metadata.chunk_index` so
    /// broadcasts for the same step on different chunks can be
    /// distinguished during correlation.
    pub chunk_index: Option<i64>,
    /// Phase 11: layer identifier for layered step calls (e.g.,
    /// synth/reconcile/apex). Serialized under `trace.metadata.layer`.
    pub layer: Option<i64>,
    /// Phase 11: stale-check type / node-check discriminator. Surfaces
    /// in broadcast traces under `trace.metadata.check_type` so the
    /// oversight page can group stale-maintenance calls separately
    /// from normal builds.
    pub check_type: Option<String>,
}

impl RequestMetadata {
    /// Construct a `RequestMetadata` from a Phase 6 `StepContext` so
    /// the Phase 11 LLM call path injects build_id / slug / step_name
    /// / depth / chunk_index into the outgoing trace. The caller can
    /// override or extend individual fields afterwards (e.g., set
    /// `node_identity` from the app's identity table).
    pub fn from_step_context(ctx: &super::step_context::StepContext) -> Self {
        Self {
            build_id: if ctx.build_id.is_empty() {
                None
            } else {
                Some(ctx.build_id.clone())
            },
            slug: if ctx.slug.is_empty() {
                None
            } else {
                Some(ctx.slug.clone())
            },
            step_name: if ctx.step_name.is_empty() {
                None
            } else {
                Some(ctx.step_name.clone())
            },
            depth: Some(ctx.depth),
            chunk_index: ctx.chunk_index,
            // chain_id / layer / check_type / node_identity are not
            // on StepContext today. Callers that know them should
            // fill them in before handing the metadata to the LLM
            // call path.
            chain_id: None,
            layer: None,
            check_type: None,
            node_identity: None,
            session_id: None,
        }
    }
}

/// Unified parsed response. Providers map their native envelope into this
/// shape. `actual_cost_usd` is the authoritative synchronous cost field —
/// `None` for providers that don't report one (Ollama local, custom
/// OAI-compat). `generation_id` is the correlation key for follow-up
/// cost reconciliation via broadcast.
#[derive(Debug, Clone)]
pub struct ParsedLlmResponse {
    pub content: String,
    pub usage: TokenUsage,
    pub generation_id: Option<String>,
    pub actual_cost_usd: Option<f64>,
    pub finish_reason: Option<String>,
}

// ── LlmProvider trait ───────────────────────────────────────────────────────

/// A compute backend that can handle LLM inference requests.
///
/// Implementations are expected to be stateless — the `Provider` row
/// they are built from holds all configuration (base_url, api_key_ref,
/// custom headers). The trait methods take `&self` and return plain
/// values so the trait object lives in the registry for the whole
/// process lifetime.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Display name (e.g., "OpenRouter", "OpenAI-Compat", "Ollama Local").
    fn name(&self) -> &str;

    /// Type tag for the row. Must match the CHECK constraint on
    /// `pyramid_providers.provider_type`.
    fn provider_type(&self) -> ProviderType;

    /// The full chat completions endpoint URL.
    fn chat_completions_url(&self) -> String;

    /// Build HTTP headers for authentication and provider-specific
    /// requirements. The `secret` argument is `None` when the provider's
    /// `api_key_ref` is unset (local Ollama, or a custom endpoint with
    /// auth provided via `config_json.extra_headers` instead).
    fn prepare_headers(&self, secret: Option<&ResolvedSecret>) -> Result<Vec<(String, String)>>;

    /// Parse the provider's response body into the unified shape.
    fn parse_response(&self, body: &str) -> Result<ParsedLlmResponse>;

    /// Whether `response_format` is accepted in the request body. For
    /// per-model gates, the registry stores the list in
    /// `supported_parameters_json` and consults it before setting the
    /// field — this trait method is a provider-wide default.
    fn supports_response_format(&self) -> bool;

    /// Whether the provider supports streaming (SSE).
    fn supports_streaming(&self) -> bool;

    /// Optional: auto-detect context window for a model (e.g. Ollama
    /// `/api/show`). Returns None if detection isn't supported.
    async fn detect_context_window(&self, _model: &str) -> Option<usize> {
        None
    }

    /// Optional: mutate the outgoing request body to add provider-
    /// specific metadata (OpenRouter trace, session_id, etc.).
    fn augment_request_body(&self, _body: &mut Value, _metadata: &RequestMetadata) {}
}

// ── Provider row ────────────────────────────────────────────────────────────

/// Database row backing a registered provider. `api_key_ref` and
/// `base_url` may both contain `${VAR_NAME}` references; the registry
/// resolves them against the credential store at call time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    pub id: String,
    pub display_name: String,
    pub provider_type: ProviderType,
    pub base_url: String,
    pub api_key_ref: Option<String>,
    pub auto_detect_context: bool,
    pub supports_broadcast: bool,
    pub broadcast_config_json: Option<String>,
    /// Provider-specific config as a free-form JSON string. We keep it
    /// as a string so the schema can evolve without migrations.
    pub config_json: String,
    pub enabled: bool,
}

impl Provider {
    /// Parse the `config_json` blob and pull extra headers, if any. The
    /// schema is `{ "extra_headers": { "K": "V" } }`. Values in the
    /// returned map may contain `${VAR_NAME}` references — the caller is
    /// responsible for substituting them.
    pub fn extra_headers(&self) -> Result<Vec<(String, String)>> {
        if self.config_json.trim().is_empty() || self.config_json.trim() == "{}" {
            return Ok(vec![]);
        }
        let cfg: Value = serde_json::from_str(&self.config_json)
            .with_context(|| format!("parsing provider `{}` config_json", self.id))?;
        let headers = cfg.get("extra_headers").and_then(|v| v.as_object());
        let Some(headers) = headers else {
            return Ok(vec![]);
        };
        let mut out = Vec::with_capacity(headers.len());
        for (k, v) in headers {
            let Some(s) = v.as_str() else {
                return Err(anyhow!(
                    "provider `{}` config_json.extra_headers.{} is not a string",
                    self.id,
                    k
                ));
            };
            out.push((k.clone(), s.to_string()));
        }
        Ok(out)
    }
}

/// Provider flavor — determines which trait impl is selected at load
/// time and which CHECK constraint value is written to SQLite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderType {
    Openrouter,
    OpenaiCompat,
}

impl ProviderType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderType::Openrouter => "openrouter",
            ProviderType::OpenaiCompat => "openai_compat",
        }
    }

    pub fn from_str(s: &str) -> Result<Self> {
        match s {
            "openrouter" => Ok(ProviderType::Openrouter),
            "openai_compat" => Ok(ProviderType::OpenaiCompat),
            other => Err(anyhow!("unknown provider_type `{other}`")),
        }
    }
}

// ── Tier routing row ───────────────────────────────────────────────────────

/// A mapping from a tier name (`fast_extract`, `web`, `synth_heavy`,
/// `stale_remote`, `stale_local`) to a specific provider and model.
/// Pricing is stored as the OpenRouter-shaped JSON blob per the spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierRoutingEntry {
    pub tier_name: String,
    pub provider_id: String,
    pub model_id: String,
    pub context_limit: Option<usize>,
    pub max_completion_tokens: Option<usize>,
    /// Raw OpenRouter pricing JSON. Values are strings (e.g.
    /// `"0.0000015"` for $1.50 per million tokens).
    pub pricing_json: String,
    pub supported_parameters_json: Option<String>,
    pub notes: Option<String>,
}

impl TierRoutingEntry {
    /// Return whether the model supports `response_format` per the
    /// stored capability list.
    pub fn supports_response_format(&self) -> bool {
        self.supported_parameters_json
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
            .map(|list| {
                list.iter()
                    .any(|p| p == "response_format" || p == "structured_outputs")
            })
            .unwrap_or(false)
    }

    /// Parse the `prompt` pricing rate in USD per token.
    pub fn prompt_price_per_token(&self) -> Option<f64> {
        parse_price_field(&self.pricing_json, "prompt")
    }

    /// Parse the `completion` pricing rate in USD per token.
    pub fn completion_price_per_token(&self) -> Option<f64> {
        parse_price_field(&self.pricing_json, "completion")
    }
}

fn parse_price_field(pricing_json: &str, field: &str) -> Option<f64> {
    if pricing_json.trim().is_empty() {
        return None;
    }
    let v: Value = serde_json::from_str(pricing_json).ok()?;
    let raw = v.get(field)?;
    // OpenRouter returns string-encoded prices. Parse defensively.
    if let Some(s) = raw.as_str() {
        return s.parse::<f64>().ok();
    }
    raw.as_f64()
}

// ── Per-step overrides ─────────────────────────────────────────────────────

/// Per-step override row. `field_name` is one of `model_tier`,
/// `temperature`, `provider_id`, `model_id`, `max_tokens`, etc. The
/// `value_json` holds the JSON-encoded override payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepOverride {
    pub slug: String,
    pub chain_id: String,
    pub step_name: String,
    pub field_name: String,
    pub value_json: String,
}

// ── Provider implementations ───────────────────────────────────────────────

/// OpenRouter provider — the existing hardcoded behavior ported into the
/// trait. Headers include Bearer auth plus the canonical
/// `X-OpenRouter-Title` attribution header and `HTTP-Referer`.
pub struct OpenRouterProvider {
    pub id: String,
    pub display_name: String,
    /// Resolved base_url (no `${VAR_NAME}` references at this point).
    pub base_url: String,
    pub extra_headers: Vec<(String, String)>,
}

#[async_trait]
impl LlmProvider for OpenRouterProvider {
    fn name(&self) -> &str {
        &self.display_name
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::Openrouter
    }

    fn chat_completions_url(&self) -> String {
        // The spec pins OpenRouter's URL as "{base_url}/chat/completions"
        // where base_url = "https://openrouter.ai/api/v1" on the default
        // seed. We tolerate a trailing slash on the base for operator
        // convenience.
        let base = self.base_url.trim_end_matches('/');
        format!("{base}/chat/completions")
    }

    fn prepare_headers(&self, secret: Option<&ResolvedSecret>) -> Result<Vec<(String, String)>> {
        let mut out = Vec::with_capacity(6 + self.extra_headers.len());
        let secret = secret.ok_or_else(|| {
            anyhow!(
                "OpenRouter provider `{}` requires an api_key_ref but the credential resolved to None",
                self.id
            )
        })?;
        out.push(("Authorization".to_string(), secret.as_bearer_header()));
        out.push(("Content-Type".to_string(), "application/json".to_string()));
        // Canonical current name; `X-Title` is the accepted legacy alias.
        out.push((
            "X-OpenRouter-Title".to_string(),
            "Wire Pyramid Engine".to_string(),
        ));
        out.push((
            "X-OpenRouter-Categories".to_string(),
            "knowledge-pyramid".to_string(),
        ));
        out.push((
            "HTTP-Referer".to_string(),
            "https://newsbleach.com".to_string(),
        ));
        // Attribution-only legacy alias for backwards compat with older
        // log parsers in observability.
        out.push(("X-Title".to_string(), "Wire Pyramid Engine".to_string()));
        for (k, v) in &self.extra_headers {
            out.push((k.clone(), v.clone()));
        }
        Ok(out)
    }

    fn parse_response(&self, body: &str) -> Result<ParsedLlmResponse> {
        parse_openai_shaped_response(body, /*expect_cost_field=*/ true)
    }

    fn supports_response_format(&self) -> bool {
        true
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    async fn detect_context_window(&self, _model: &str) -> Option<usize> {
        // Per spec: OpenRouter context limits come from the `/models`
        // endpoint metadata, not a per-call detection. Return None and
        // let the tier_routing row's `context_limit` column carry the
        // value.
        None
    }

    fn augment_request_body(&self, body: &mut Value, metadata: &RequestMetadata) {
        let Some(map) = body.as_object_mut() else {
            return;
        };

        // Phase 11: the `trace` object carries BOTH OpenRouter's
        // recognized hierarchy keys (trace_id / trace_name / span_name
        // / generation_name) AND our custom metadata. OpenRouter's
        // OTLP translation promotes anything else in this object to
        // `trace.metadata.<key>` attributes on the span, which is
        // where the webhook receiver pulls them from for correlation.
        //
        // See `docs/specs/evidence-triage-and-dadbear.md` Part 4 for
        // the authoritative attribute key table.
        let mut trace = serde_json::Map::new();

        // ── OpenRouter-recognized hierarchy keys ────────────────────
        if let Some(b) = &metadata.build_id {
            trace.insert("trace_id".into(), Value::String(b.clone()));
        }
        if let Some(c) = &metadata.chain_id {
            trace.insert("trace_name".into(), Value::String(c.clone()));
        }
        if let Some(s) = &metadata.step_name {
            trace.insert("span_name".into(), Value::String(s.clone()));
            trace.insert("generation_name".into(), Value::String(s.clone()));
        }

        // ── Custom metadata passed through to trace.metadata.* ──────
        // Both flat AND nested-under-`metadata` forms are written so
        // OpenRouter's OTLP translator has a consistent surface to
        // work from regardless of which path it takes. This is
        // intentional belt-and-suspenders: the spec's attribute key
        // convention uses `trace.metadata.<key>`, but earlier phases
        // of the node were writing them flat and we don't want to
        // silently break prior broadcast ingestion. Both paths are
        // legal per the OpenRouter docs — dashboard destinations
        // accept either.
        let mut meta = serde_json::Map::new();
        if let Some(s) = &metadata.slug {
            trace.insert("pyramid_slug".into(), Value::String(s.clone()));
            meta.insert("pyramid_slug".into(), Value::String(s.clone()));
        }
        if let Some(b) = &metadata.build_id {
            trace.insert("build_id".into(), Value::String(b.clone()));
            meta.insert("build_id".into(), Value::String(b.clone()));
        }
        if let Some(s) = &metadata.step_name {
            trace.insert("step_name".into(), Value::String(s.clone()));
            meta.insert("step_name".into(), Value::String(s.clone()));
        }
        if let Some(d) = metadata.depth {
            let n = Value::Number(serde_json::Number::from(d));
            trace.insert("depth".into(), n.clone());
            meta.insert("depth".into(), n);
        }
        if let Some(c) = &metadata.chain_id {
            trace.insert("chain_id".into(), Value::String(c.clone()));
            meta.insert("chain_id".into(), Value::String(c.clone()));
        }
        if let Some(l) = metadata.layer {
            let n = Value::Number(serde_json::Number::from(l));
            trace.insert("layer".into(), n.clone());
            meta.insert("layer".into(), n);
        }
        if let Some(ct) = &metadata.check_type {
            trace.insert("check_type".into(), Value::String(ct.clone()));
            meta.insert("check_type".into(), Value::String(ct.clone()));
        }
        if let Some(ci) = metadata.chunk_index {
            let n = Value::Number(serde_json::Number::from(ci));
            trace.insert("chunk_index".into(), n.clone());
            meta.insert("chunk_index".into(), n);
        }

        if !meta.is_empty() {
            trace.insert("metadata".into(), Value::Object(meta));
        }

        if !trace.is_empty() {
            map.insert("trace".into(), Value::Object(trace));
        }

        // `session_id` scopes per-build sampling in OpenRouter. Prefer
        // the explicit override, otherwise synthesize from slug+build.
        // The webhook correlator splits on `/` and uses the first
        // half as the slug filter for its fallback correlation path.
        let session_id =
            metadata
                .session_id
                .clone()
                .or_else(|| match (&metadata.slug, &metadata.build_id) {
                    (Some(slug), Some(bid)) => Some(format!("{slug}/{bid}")),
                    _ => None,
                });
        if let Some(sid) = session_id {
            map.insert("session_id".into(), Value::String(sid));
        }

        if let Some(user) = &metadata.node_identity {
            map.insert("user".into(), Value::String(user.clone()));
        }
    }
}

// ── OpenAI-compatible provider ──────────────────────────────────────────────

/// OpenAI-compatible provider. Used for local Ollama
/// (`http://localhost:11434/v1`), remote Ollama behind nginx, and any
/// other endpoint that speaks the OpenAI `/chat/completions` JSON shape.
/// Authentication is opt-in: if the `Provider::api_key_ref` is None the
/// Authorization header is omitted entirely.
pub struct OpenAiCompatProvider {
    pub id: String,
    pub display_name: String,
    /// Resolved base_url. May point at `localhost:11434/v1`, a private
    /// intranet endpoint, or a vendor URL.
    pub base_url: String,
    /// Resolved extra headers from `config_json.extra_headers`. This is
    /// how nginx-in-front-of-Ollama deployments inject basic auth or
    /// API gateway headers without using the bearer path.
    pub extra_headers: Vec<(String, String)>,
    /// Tracks whether the provider was configured to require a credential.
    /// The trait is called with `secret: None` when the `api_key_ref`
    /// column is NULL; this lets us distinguish "no auth configured" from
    /// "auth configured but credential missing".
    pub requires_auth: bool,
}

#[async_trait]
impl LlmProvider for OpenAiCompatProvider {
    fn name(&self) -> &str {
        &self.display_name
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::OpenaiCompat
    }

    fn chat_completions_url(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        format!("{base}/chat/completions")
    }

    fn prepare_headers(&self, secret: Option<&ResolvedSecret>) -> Result<Vec<(String, String)>> {
        let mut out = Vec::with_capacity(2 + self.extra_headers.len());
        out.push(("Content-Type".to_string(), "application/json".to_string()));

        if self.requires_auth {
            let secret = secret.ok_or_else(|| {
                anyhow!(
                    "OpenAI-compat provider `{}` has api_key_ref set but the credential could not be resolved",
                    self.id
                )
            })?;
            out.push(("Authorization".to_string(), secret.as_bearer_header()));
        }
        // If there's no api_key_ref configured, we silently drop the
        // bearer header — local Ollama accepts requests with no auth.

        for (k, v) in &self.extra_headers {
            out.push((k.clone(), v.clone()));
        }
        Ok(out)
    }

    fn parse_response(&self, body: &str) -> Result<ParsedLlmResponse> {
        // OpenAI-shaped responses have the same envelope but typically
        // lack `usage.cost`. The parser tolerates its absence.
        parse_openai_shaped_response(body, /*expect_cost_field=*/ false)
    }

    fn supports_response_format(&self) -> bool {
        // Ollama has supported `response_format` since mid-2024. Custom
        // OAI-compat providers vary — per-tier `supported_parameters_json`
        // is the authoritative gate.
        true
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    async fn detect_context_window(&self, model: &str) -> Option<usize> {
        // Per spec: hit Ollama's native `/api/show` endpoint and scan
        // `model_info` for `<arch>.context_length`. The OpenAI-compat
        // endpoint does NOT expose this info, so we strip the trailing
        // `/v1` from the base URL to reach the native path.
        let base = self.base_url.trim_end_matches('/');
        let native_base = base.strip_suffix("/v1").unwrap_or(base);
        let url = format!("{native_base}/api/show");

        let client = reqwest::Client::new();
        let body = serde_json::json!({ "model": model });
        let resp = client
            .post(&url)
            .json(&body)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let v: Value = resp.json().await.ok()?;
        parse_ollama_context_length(&v)
    }
}

/// Extract the context window from an Ollama `/api/show` response. The
/// algorithm:
/// 1. Read `model_info["general.architecture"]` (e.g. "gemma3").
/// 2. Read `model_info["<arch>.context_length"]`.
/// 3. Fallback: scan every `model_info.*.context_length` key.
/// 4. Return None if no candidate is found.
pub fn parse_ollama_context_length(v: &Value) -> Option<usize> {
    let model_info = v.get("model_info")?;
    let obj = model_info.as_object()?;

    if let Some(arch) = obj.get("general.architecture").and_then(|v| v.as_str()) {
        let key = format!("{arch}.context_length");
        if let Some(n) = obj.get(&key).and_then(|v| v.as_u64()) {
            return Some(n as usize);
        }
    }

    for (k, v) in obj {
        if k.ends_with(".context_length") {
            if let Some(n) = v.as_u64() {
                return Some(n as usize);
            }
        }
    }
    None
}

// ── Response parsing ────────────────────────────────────────────────────────

fn parse_openai_shaped_response(body: &str, expect_cost_field: bool) -> Result<ParsedLlmResponse> {
    let data: Value = parse_openai_envelope(body)?;

    let content = data
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .ok_or_else(|| anyhow!("response envelope missing choices[0].message.content"))?
        .to_string();

    let prompt_tokens = data
        .get("usage")
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let completion_tokens = data
        .get("usage")
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let usage = TokenUsage {
        prompt_tokens,
        completion_tokens,
    };

    let generation_id = data
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let actual_cost_usd = data
        .get("usage")
        .and_then(|u| u.get("cost"))
        .and_then(|v| v.as_f64());

    if expect_cost_field && actual_cost_usd.is_none() {
        // Defensive log at debug level — the provider said it reports
        // cost but this response doesn't have it. The caller will fall
        // back to estimated cost. We log at debug (not warn) to avoid
        // spamming on providers that have started returning null for
        // free-promotional calls.
        tracing::debug!("openrouter response missing usage.cost, falling back to estimated cost");
    }

    let finish_reason = data
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("finish_reason"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Ok(ParsedLlmResponse {
        content,
        usage,
        generation_id,
        actual_cost_usd,
        finish_reason,
    })
}

/// Parse an OpenAI-shaped JSON envelope from a response body that may
/// contain SSE `data: ...` lines or leading/trailing prose.
fn parse_openai_envelope(body_text: &str) -> Result<Value> {
    let trimmed = body_text.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("empty response body"));
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
        }
    }

    Err(anyhow!(
        "could not parse OpenAI-shaped JSON envelope from: {}",
        &trimmed[..trimmed.len().min(400)]
    ))
}

// ── ProviderRegistry ────────────────────────────────────────────────────────

/// In-memory registry of providers, tier routing, and per-step overrides.
/// Built from the database at startup and updated by the IPC surface.
/// All reads are cheap (RwLock + HashMap lookup); all mutations serialize
/// through the write lock.
pub struct ProviderRegistry {
    providers: RwLock<HashMap<String, Provider>>,
    tier_routing: RwLock<HashMap<String, TierRoutingEntry>>,
    step_overrides: RwLock<HashMap<StepOverrideKey, StepOverride>>,
    credentials: Arc<CredentialStore>,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct StepOverrideKey {
    pub slug: String,
    pub chain_id: String,
    pub step_name: String,
    pub field_name: String,
}

impl StepOverrideKey {
    pub fn new(slug: &str, chain_id: &str, step_name: &str, field_name: &str) -> Self {
        Self {
            slug: slug.to_string(),
            chain_id: chain_id.to_string(),
            step_name: step_name.to_string(),
            field_name: field_name.to_string(),
        }
    }
}

impl ProviderRegistry {
    /// Construct an empty registry sharing the given credential store.
    /// Tests use this directly; production callers go through
    /// `load_from_db` which also hydrates the in-memory maps.
    pub fn new(credentials: Arc<CredentialStore>) -> Self {
        Self {
            providers: RwLock::new(HashMap::new()),
            tier_routing: RwLock::new(HashMap::new()),
            step_overrides: RwLock::new(HashMap::new()),
            credentials,
        }
    }

    /// Reload providers, tier routing, and step overrides from the
    /// database. Intended for startup + post-IPC-mutation refresh.
    pub fn load_from_db(&self, conn: &rusqlite::Connection) -> Result<()> {
        let providers = super::db::list_providers(conn)?;
        let mut map = HashMap::with_capacity(providers.len());
        for p in providers {
            map.insert(p.id.clone(), p);
        }
        *self.providers.write().expect("providers RwLock poisoned") = map;

        let tier = super::db::get_tier_routing(conn)?;
        *self
            .tier_routing
            .write()
            .expect("tier_routing RwLock poisoned") = tier;

        let overrides = super::db::list_step_overrides(conn)?;
        let mut override_map = HashMap::with_capacity(overrides.len());
        for o in overrides {
            override_map.insert(
                StepOverrideKey::new(&o.slug, &o.chain_id, &o.step_name, &o.field_name),
                o,
            );
        }
        *self
            .step_overrides
            .write()
            .expect("step_overrides RwLock poisoned") = override_map;

        Ok(())
    }

    /// Return a snapshot copy of all provider rows.
    pub fn list_providers(&self) -> Vec<Provider> {
        let guard = self.providers.read().expect("providers RwLock poisoned");
        guard.values().cloned().collect()
    }

    /// Return a snapshot copy of all tier routing rows.
    pub fn list_tier_routing(&self) -> Vec<TierRoutingEntry> {
        let guard = self
            .tier_routing
            .read()
            .expect("tier_routing RwLock poisoned");
        guard.values().cloned().collect()
    }

    /// Return a snapshot copy of all step overrides.
    pub fn list_step_overrides(&self) -> Vec<StepOverride> {
        let guard = self
            .step_overrides
            .read()
            .expect("step_overrides RwLock poisoned");
        guard.values().cloned().collect()
    }

    /// Look up the provider row for `id`.
    pub fn get_provider(&self, id: &str) -> Option<Provider> {
        let guard = self.providers.read().expect("providers RwLock poisoned");
        guard.get(id).cloned()
    }

    /// Look up the tier routing entry for `tier_name`.
    pub fn get_tier(&self, tier_name: &str) -> Option<TierRoutingEntry> {
        let guard = self
            .tier_routing
            .read()
            .expect("tier_routing RwLock poisoned");
        guard.get(tier_name).cloned()
    }

    /// Look up a per-step override.
    pub fn get_step_override(
        &self,
        slug: &str,
        chain_id: &str,
        step_name: &str,
        field_name: &str,
    ) -> Option<StepOverride> {
        let guard = self
            .step_overrides
            .read()
            .expect("step_overrides RwLock poisoned");
        guard
            .get(&StepOverrideKey::new(slug, chain_id, step_name, field_name))
            .cloned()
    }

    /// Return the provider ID that should be used as the default for
    /// LLM calls. When local mode is active (an enabled non-openrouter
    /// provider exists), returns that provider. Otherwise "openrouter".
    pub fn active_provider_id(&self) -> String {
        let providers = self.providers.read().expect("providers RwLock poisoned");
        for (id, provider) in providers.iter() {
            if id != "openrouter" && provider.enabled {
                return id.clone();
            }
        }
        "openrouter".to_string()
    }

    /// Resolve a tier reference to a concrete provider row + model ID +
    /// supporting metadata. Honors per-step overrides when `slug`,
    /// `chain_id`, and `step_name` are provided.
    pub fn resolve_tier(
        &self,
        tier_name: &str,
        slug: Option<&str>,
        chain_id: Option<&str>,
        step_name: Option<&str>,
    ) -> Result<ResolvedTier> {
        // Per-step overrides first: they can swap provider+model or
        // bypass tier routing entirely. v1 supports a single field —
        // `model_tier` — which renames the tier we look up.
        let mut effective_tier = tier_name.to_string();
        if let (Some(slug), Some(chain_id), Some(step_name)) = (slug, chain_id, step_name) {
            if let Some(override_row) =
                self.get_step_override(slug, chain_id, step_name, "model_tier")
            {
                if let Ok(v) = serde_json::from_str::<String>(&override_row.value_json) {
                    effective_tier = v;
                }
            }
        }

        let tier = self.get_tier(&effective_tier).ok_or_else(|| {
            anyhow!(
                "tier `{effective_tier}` is not defined in pyramid_tier_routing — \
                 add it via Settings → Model Routing or fall back to a seeded tier"
            )
        })?;

        let provider = self.get_provider(&tier.provider_id).ok_or_else(|| {
            anyhow!(
                "tier `{effective_tier}` references provider `{}` which is not registered",
                tier.provider_id
            )
        })?;

        if !provider.enabled {
            return Err(anyhow!(
                "provider `{}` is currently disabled — re-enable it or switch the tier",
                provider.id
            ));
        }

        Ok(ResolvedTier { tier, provider })
    }

    /// Resolve the credential referenced by a provider, substituting
    /// `${VAR_NAME}` as needed. Returns `None` (not an Err) when
    /// `api_key_ref` is unset — the provider does not require auth.
    pub fn resolve_credential_for(&self, provider: &Provider) -> Result<Option<ResolvedSecret>> {
        let Some(key_ref) = provider.api_key_ref.as_deref() else {
            return Ok(None);
        };
        // Two shapes are allowed: a bare variable name (`OPENROUTER_KEY`)
        // or a string that contains a `${...}` pattern. Prefer the bare
        // form for new rows.
        if key_ref.contains("${") {
            let substituted = self.credentials.substitute(key_ref)?;
            return Ok(Some(substituted));
        }
        let secret = self.credentials.resolve_var(key_ref)?;
        Ok(Some(secret))
    }

    /// Resolve the provider's `base_url` field, substituting
    /// `${VAR_NAME}` references if any. Returns a plain String because
    /// the base URL is not itself a secret in the normal case, though
    /// it may embed one for a self-hosted Ollama tunnel. Callers that
    /// need to log the URL should redact anything resembling a token.
    pub fn resolve_base_url(&self, provider: &Provider) -> Result<String> {
        if provider.base_url.contains("${") {
            self.credentials.substitute_to_string(&provider.base_url)
        } else {
            Ok(provider.base_url.clone())
        }
    }

    /// Build a concrete LlmProvider trait object from a Provider row.
    /// Substitutes `${VAR_NAME}` references in the base URL + extra
    /// headers, and resolves the credential (if any) into the output
    /// tuple. The returned tuple is `(trait_impl, optional_secret)`.
    pub fn instantiate_provider(
        &self,
        provider: &Provider,
    ) -> Result<(Box<dyn LlmProvider>, Option<ResolvedSecret>)> {
        let base_url = self.resolve_base_url(provider)?;
        let raw_extra = provider.extra_headers()?;
        let mut extra_headers = Vec::with_capacity(raw_extra.len());
        for (k, v) in raw_extra {
            let value = if v.contains("${") {
                self.credentials.substitute_to_string(&v)?
            } else {
                v
            };
            extra_headers.push((k, value));
        }
        let secret = self.resolve_credential_for(provider)?;

        let impl_box: Box<dyn LlmProvider> = match provider.provider_type {
            ProviderType::Openrouter => Box::new(OpenRouterProvider {
                id: provider.id.clone(),
                display_name: provider.display_name.clone(),
                base_url,
                extra_headers,
            }),
            ProviderType::OpenaiCompat => Box::new(OpenAiCompatProvider {
                id: provider.id.clone(),
                display_name: provider.display_name.clone(),
                base_url,
                extra_headers,
                requires_auth: provider.api_key_ref.is_some(),
            }),
        };

        Ok((impl_box, secret))
    }

    /// Upsert a provider row in memory and persist it. Callers are
    /// responsible for providing a DB connection — the registry does
    /// not hold one itself so it can be shared across reader/writer
    /// mutexes.
    pub fn save_provider(&self, conn: &rusqlite::Connection, provider: Provider) -> Result<()> {
        super::db::save_provider(conn, &provider)?;
        self.providers
            .write()
            .expect("providers RwLock poisoned")
            .insert(provider.id.clone(), provider);
        Ok(())
    }

    /// Delete a provider row from memory and the database.
    pub fn delete_provider(&self, conn: &rusqlite::Connection, id: &str) -> Result<()> {
        super::db::delete_provider(conn, id)?;
        self.providers
            .write()
            .expect("providers RwLock poisoned")
            .remove(id);
        Ok(())
    }

    /// Upsert a tier routing row.
    pub fn save_tier_routing(
        &self,
        conn: &rusqlite::Connection,
        entry: TierRoutingEntry,
    ) -> Result<()> {
        super::db::save_tier_routing(conn, &entry)?;
        self.tier_routing
            .write()
            .expect("tier_routing RwLock poisoned")
            .insert(entry.tier_name.clone(), entry);
        Ok(())
    }

    /// Delete a tier routing row.
    pub fn delete_tier_routing(&self, conn: &rusqlite::Connection, tier_name: &str) -> Result<()> {
        super::db::delete_tier_routing(conn, tier_name)?;
        self.tier_routing
            .write()
            .expect("tier_routing RwLock poisoned")
            .remove(tier_name);
        Ok(())
    }

    /// Upsert a step override.
    pub fn save_step_override(
        &self,
        conn: &rusqlite::Connection,
        override_row: StepOverride,
    ) -> Result<()> {
        super::db::save_step_override(conn, &override_row)?;
        self.step_overrides
            .write()
            .expect("step_overrides RwLock poisoned")
            .insert(
                StepOverrideKey::new(
                    &override_row.slug,
                    &override_row.chain_id,
                    &override_row.step_name,
                    &override_row.field_name,
                ),
                override_row,
            );
        Ok(())
    }

    /// Delete a step override.
    pub fn delete_step_override(
        &self,
        conn: &rusqlite::Connection,
        slug: &str,
        chain_id: &str,
        step_name: &str,
        field_name: &str,
    ) -> Result<()> {
        super::db::delete_step_override(conn, slug, chain_id, step_name, field_name)?;
        self.step_overrides
            .write()
            .expect("step_overrides RwLock poisoned")
            .remove(&StepOverrideKey::new(slug, chain_id, step_name, field_name));
        Ok(())
    }

    /// Return the underlying credential store handle. Used by the IPC
    /// surface so a single shared `Arc<CredentialStore>` is threaded
    /// through both the registry and the credential management commands.
    pub fn credentials(&self) -> &Arc<CredentialStore> {
        &self.credentials
    }
}

/// Result of resolving a tier name against the registry. Contains both
/// the tier routing row (with pricing + context limit) and the provider
/// row (for credentials and URL resolution).
#[derive(Debug, Clone)]
pub struct ResolvedTier {
    pub tier: TierRoutingEntry,
    pub provider: Provider,
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_store() -> Arc<CredentialStore> {
        let tmp = TempDir::new().unwrap();
        let store = CredentialStore::load(tmp.path()).unwrap();
        store.set("OPENROUTER_KEY", "sk-or-v1-abcDEF123").unwrap();
        // Leak the TempDir so the file stays around for the test duration
        // (no cleanup race with the atomic write on macOS).
        std::mem::forget(tmp);
        Arc::new(store)
    }

    #[test]
    fn openrouter_headers_include_bearer_and_attribution() {
        let store = make_store();
        let provider = OpenRouterProvider {
            id: "openrouter".into(),
            display_name: "OpenRouter".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            extra_headers: vec![],
        };
        let secret = store.resolve_var("OPENROUTER_KEY").unwrap();
        let headers = provider.prepare_headers(Some(&secret)).unwrap();

        let kv: HashMap<_, _> = headers.into_iter().collect();
        assert_eq!(
            kv.get("Authorization").map(String::as_str),
            Some("Bearer sk-or-v1-abcDEF123")
        );
        assert_eq!(
            kv.get("X-OpenRouter-Title").map(String::as_str),
            Some("Wire Pyramid Engine")
        );
        assert!(kv.contains_key("X-Title"), "legacy alias still present");
        assert!(kv.contains_key("HTTP-Referer"));
    }

    #[test]
    fn openrouter_url_is_chat_completions() {
        let provider = OpenRouterProvider {
            id: "openrouter".into(),
            display_name: "OpenRouter".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            extra_headers: vec![],
        };
        assert_eq!(
            provider.chat_completions_url(),
            "https://openrouter.ai/api/v1/chat/completions"
        );
    }

    #[test]
    fn openrouter_parses_usage_cost() {
        let body = serde_json::json!({
            "id": "gen-xyz",
            "choices": [{
                "message": { "content": "hi" },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "cost": 0.00345
            }
        })
        .to_string();

        let provider = OpenRouterProvider {
            id: "openrouter".into(),
            display_name: "OpenRouter".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            extra_headers: vec![],
        };
        let parsed = provider.parse_response(&body).unwrap();
        assert_eq!(parsed.content, "hi");
        assert_eq!(parsed.usage.prompt_tokens, 10);
        assert_eq!(parsed.usage.completion_tokens, 5);
        assert_eq!(parsed.actual_cost_usd, Some(0.00345));
        assert_eq!(parsed.generation_id.as_deref(), Some("gen-xyz"));
        assert_eq!(parsed.finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn openai_compat_no_auth_when_no_ref() {
        let provider = OpenAiCompatProvider {
            id: "ollama-local".into(),
            display_name: "Ollama Local".into(),
            base_url: "http://localhost:11434/v1".into(),
            extra_headers: vec![],
            requires_auth: false,
        };
        let headers = provider.prepare_headers(None).unwrap();
        let kv: HashMap<_, _> = headers.into_iter().collect();
        assert!(!kv.contains_key("Authorization"));
        assert!(kv.contains_key("Content-Type"));
    }

    #[test]
    fn openai_compat_errors_when_ref_set_but_secret_missing() {
        let provider = OpenAiCompatProvider {
            id: "ollama-prod".into(),
            display_name: "Prod Ollama".into(),
            base_url: "https://ollama.internal/v1".into(),
            extra_headers: vec![],
            requires_auth: true,
        };
        assert!(provider.prepare_headers(None).is_err());
    }

    #[test]
    fn openai_compat_chat_url_strips_trailing_slash() {
        let provider = OpenAiCompatProvider {
            id: "ollama-local".into(),
            display_name: "Ollama Local".into(),
            base_url: "http://localhost:11434/v1/".into(),
            extra_headers: vec![],
            requires_auth: false,
        };
        assert_eq!(
            provider.chat_completions_url(),
            "http://localhost:11434/v1/chat/completions"
        );
    }

    #[test]
    fn openai_compat_parses_response_without_cost() {
        let body = serde_json::json!({
            "id": "chatcmpl-abc",
            "choices": [{
                "message": { "content": "hello" },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 3,
                "completion_tokens": 2
            }
        })
        .to_string();

        let provider = OpenAiCompatProvider {
            id: "ollama-local".into(),
            display_name: "Ollama Local".into(),
            base_url: "http://localhost:11434/v1".into(),
            extra_headers: vec![],
            requires_auth: false,
        };
        let parsed = provider.parse_response(&body).unwrap();
        assert_eq!(parsed.content, "hello");
        assert_eq!(parsed.usage.prompt_tokens, 3);
        assert_eq!(parsed.actual_cost_usd, None);
    }

    #[test]
    fn ollama_detect_context_window_parses_arch_prefix() {
        let response = serde_json::json!({
            "model_info": {
                "general.architecture": "gemma3",
                "gemma3.context_length": 131072,
                "gemma3.embedding_length": 4608
            }
        });
        assert_eq!(parse_ollama_context_length(&response), Some(131072));
    }

    #[test]
    fn ollama_detect_context_window_falls_back_to_suffix_scan() {
        let response = serde_json::json!({
            "model_info": {
                "general.architecture": "unknown-arch",
                "llama.context_length": 8192
            }
        });
        assert_eq!(parse_ollama_context_length(&response), Some(8192));
    }

    #[test]
    fn ollama_detect_context_window_returns_none_when_missing() {
        let response = serde_json::json!({
            "model_info": {
                "general.architecture": "gemma3"
            }
        });
        assert_eq!(parse_ollama_context_length(&response), None);
    }

    #[test]
    fn pricing_json_parses_string_values() {
        let tier = TierRoutingEntry {
            tier_name: "web".into(),
            provider_id: "openrouter".into(),
            model_id: "x-ai/grok-4.1-fast".into(),
            context_limit: Some(2_000_000),
            max_completion_tokens: None,
            pricing_json: r#"{"prompt":"0.0000015","completion":"0.0000060","request":"0"}"#.into(),
            supported_parameters_json: None,
            notes: None,
        };
        assert!((tier.prompt_price_per_token().unwrap() - 0.0000015).abs() < 1e-12);
        assert!((tier.completion_price_per_token().unwrap() - 0.0000060).abs() < 1e-12);
    }

    #[test]
    fn pricing_json_handles_missing_fields() {
        let tier = TierRoutingEntry {
            tier_name: "local".into(),
            provider_id: "ollama-local".into(),
            model_id: "gemma3:27b".into(),
            context_limit: None,
            max_completion_tokens: None,
            pricing_json: "{}".into(),
            supported_parameters_json: None,
            notes: None,
        };
        assert!(tier.prompt_price_per_token().is_none());
    }

    #[test]
    fn request_metadata_augments_trace_with_session() {
        let provider = OpenRouterProvider {
            id: "openrouter".into(),
            display_name: "OpenRouter".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            extra_headers: vec![],
        };
        let metadata = RequestMetadata {
            build_id: Some("b-42".into()),
            slug: Some("my-slug".into()),
            chain_id: Some("code_chain".into()),
            step_name: Some("extract".into()),
            depth: Some(1),
            node_identity: Some("node-abc".into()),
            session_id: None,
            chunk_index: None,
            layer: None,
            check_type: None,
        };
        let mut body = serde_json::json!({ "model": "x", "messages": [] });
        provider.augment_request_body(&mut body, &metadata);
        let map = body.as_object().unwrap();
        let trace = map.get("trace").unwrap().as_object().unwrap();
        assert_eq!(trace.get("build_id").unwrap(), "b-42");
        // Phase 11: slug is now written as `pyramid_slug` in the
        // trace object per the spec's attribute-key convention.
        assert_eq!(trace.get("pyramid_slug").unwrap(), "my-slug");
        assert_eq!(trace.get("step_name").unwrap(), "extract");
        assert_eq!(map.get("session_id").unwrap(), "my-slug/b-42");
        assert_eq!(map.get("user").unwrap(), "node-abc");
        // Phase 11: OpenRouter-recognized hierarchy keys.
        assert_eq!(trace.get("trace_id").unwrap(), "b-42");
        assert_eq!(trace.get("trace_name").unwrap(), "code_chain");
        assert_eq!(trace.get("span_name").unwrap(), "extract");
        // Phase 11: `metadata` sub-object also present for OTLP
        // translation per trace.metadata.* attribute keys.
        let meta = trace.get("metadata").unwrap().as_object().unwrap();
        assert_eq!(meta.get("pyramid_slug").unwrap(), "my-slug");
        assert_eq!(meta.get("build_id").unwrap(), "b-42");
        assert_eq!(meta.get("step_name").unwrap(), "extract");
    }

    #[test]
    fn request_metadata_injects_layer_chunk_check_type() {
        let provider = OpenRouterProvider {
            id: "openrouter".into(),
            display_name: "OpenRouter".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            extra_headers: vec![],
        };
        let metadata = RequestMetadata {
            build_id: Some("b-99".into()),
            slug: Some("target".into()),
            step_name: Some("stale_check".into()),
            depth: Some(2),
            chunk_index: Some(4),
            layer: Some(1),
            check_type: Some("node_stale".into()),
            ..Default::default()
        };
        let mut body = serde_json::json!({ "model": "x", "messages": [] });
        provider.augment_request_body(&mut body, &metadata);
        let trace = body
            .as_object()
            .unwrap()
            .get("trace")
            .unwrap()
            .as_object()
            .unwrap();
        let meta = trace.get("metadata").unwrap().as_object().unwrap();
        assert_eq!(meta.get("layer").unwrap(), 1);
        assert_eq!(meta.get("chunk_index").unwrap(), 4);
        assert_eq!(meta.get("check_type").unwrap(), "node_stale");
        assert_eq!(meta.get("depth").unwrap(), 2);
    }

    #[test]
    fn request_metadata_from_step_context() {
        use super::super::step_context::StepContext;
        let ctx = StepContext::new(
            "slug-a",
            "build-1",
            "source_extract",
            "extraction",
            3,
            Some(7),
            "/tmp/test.db",
        );
        let metadata = RequestMetadata::from_step_context(&ctx);
        assert_eq!(metadata.slug.as_deref(), Some("slug-a"));
        assert_eq!(metadata.build_id.as_deref(), Some("build-1"));
        assert_eq!(metadata.step_name.as_deref(), Some("source_extract"));
        assert_eq!(metadata.depth, Some(3));
        assert_eq!(metadata.chunk_index, Some(7));
    }

    #[test]
    fn supported_parameters_json_detects_response_format() {
        let tier = TierRoutingEntry {
            tier_name: "web".into(),
            provider_id: "openrouter".into(),
            model_id: "x".into(),
            context_limit: None,
            max_completion_tokens: None,
            pricing_json: "{}".into(),
            supported_parameters_json: Some(r#"["tools","response_format","temperature"]"#.into()),
            notes: None,
        };
        assert!(tier.supports_response_format());

        let tier_no_rf = TierRoutingEntry {
            supported_parameters_json: Some(r#"["tools","temperature"]"#.into()),
            ..tier.clone()
        };
        assert!(!tier_no_rf.supports_response_format());
    }

    #[test]
    fn extra_headers_parses_config_json() {
        let provider = Provider {
            id: "ollama-prod".into(),
            display_name: "Prod Ollama".into(),
            provider_type: ProviderType::OpenaiCompat,
            base_url: "https://ollama.internal/v1".into(),
            api_key_ref: None,
            auto_detect_context: false,
            supports_broadcast: false,
            broadcast_config_json: None,
            config_json: r#"{"extra_headers":{"X-Api-Key":"${OLLAMA_GATEWAY_KEY}"}}"#.into(),
            enabled: true,
        };
        let headers = provider.extra_headers().unwrap();
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].0, "X-Api-Key");
        assert_eq!(headers[0].1, "${OLLAMA_GATEWAY_KEY}");
    }

    #[test]
    fn extra_headers_empty_config_returns_empty() {
        let mut provider = Provider {
            id: "p".into(),
            display_name: "p".into(),
            provider_type: ProviderType::Openrouter,
            base_url: "https://openrouter.ai/api/v1".into(),
            api_key_ref: Some("OPENROUTER_KEY".into()),
            auto_detect_context: false,
            supports_broadcast: false,
            broadcast_config_json: None,
            config_json: "{}".into(),
            enabled: true,
        };
        assert!(provider.extra_headers().unwrap().is_empty());
        provider.config_json = "".into();
        assert!(provider.extra_headers().unwrap().is_empty());
    }

    #[test]
    fn request_metadata_augments_respects_explicit_session_id() {
        let provider = OpenRouterProvider {
            id: "openrouter".into(),
            display_name: "OpenRouter".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            extra_headers: vec![],
        };
        let metadata = RequestMetadata {
            build_id: Some("b-1".into()),
            slug: Some("slug-1".into()),
            session_id: Some("custom-session".into()),
            ..Default::default()
        };
        let mut body = serde_json::json!({});
        provider.augment_request_body(&mut body, &metadata);
        assert_eq!(
            body.as_object().unwrap().get("session_id").unwrap(),
            "custom-session"
        );
    }

    #[test]
    fn registry_resolve_tier_instantiates_openrouter_for_seeded_defaults() {
        // End-to-end wiring test: a fresh DB + seeded defaults + live
        // credential store should let us resolve any of Adam's four
        // seeded tiers, instantiate the backing provider, and produce
        // a working chat_completions_url without hitting the network.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::pyramid::db::init_pyramid_db(&conn).unwrap();

        let tmp = TempDir::new().unwrap();
        let store = Arc::new(CredentialStore::load(tmp.path()).unwrap());
        store.set("OPENROUTER_KEY", "sk-or-v1-test").unwrap();
        std::mem::forget(tmp);

        let registry = ProviderRegistry::new(store);
        registry.load_from_db(&conn).unwrap();

        // fast_extract → mercury-2
        let resolved = registry
            .resolve_tier("fast_extract", None, None, None)
            .expect("fast_extract must resolve");
        assert_eq!(resolved.tier.model_id, "inception/mercury-2");
        assert_eq!(resolved.provider.id, "openrouter");

        let (impl_box, secret) = registry.instantiate_provider(&resolved.provider).unwrap();
        assert!(secret.is_some());
        assert_eq!(
            impl_box.chat_completions_url(),
            "https://openrouter.ai/api/v1/chat/completions"
        );

        // web → grok-4.1-fast (2M)
        let web_resolved = registry.resolve_tier("web", None, None, None).unwrap();
        assert_eq!(web_resolved.tier.model_id, "x-ai/grok-4.1-fast");
        assert_eq!(web_resolved.tier.context_limit, Some(2_000_000));

        // synth_heavy → minimax/minimax-m2.7
        let sh = registry
            .resolve_tier("synth_heavy", None, None, None)
            .unwrap();
        assert_eq!(sh.tier.model_id, "minimax/minimax-m2.7");

        // stale_remote → minimax/minimax-m2.7
        let sr = registry
            .resolve_tier("stale_remote", None, None, None)
            .unwrap();
        assert_eq!(sr.tier.model_id, "minimax/minimax-m2.7");

        // stale_local must NOT exist in the seeded set — Adam's
        // explicit decision. Resolving it must surface the clear
        // "tier not defined" error so the user knows to register a
        // local provider.
        let local = registry.resolve_tier("stale_local", None, None, None);
        assert!(local.is_err(), "stale_local must not be seeded");
        let msg = local.unwrap_err().to_string();
        assert!(
            msg.contains("stale_local") && msg.contains("not defined"),
            "clear tier-missing error required, got: {msg}"
        );
    }

    #[test]
    fn registry_step_override_takes_precedence_over_tier() {
        // End-to-end override wiring: a per-step override row for
        // `model_tier` should redirect resolve_tier to a different
        // entry.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::pyramid::db::init_pyramid_db(&conn).unwrap();

        let tmp = TempDir::new().unwrap();
        let store = Arc::new(CredentialStore::load(tmp.path()).unwrap());
        store.set("OPENROUTER_KEY", "sk-or-v1-test").unwrap();
        std::mem::forget(tmp);

        // Insert a fake per-step override that redirects
        // `(my-slug, code_pyramid, extract).model_tier` from
        // `fast_extract` → `synth_heavy`.
        crate::pyramid::db::save_step_override(
            &conn,
            &StepOverride {
                slug: "my-slug".into(),
                chain_id: "code_pyramid".into(),
                step_name: "extract".into(),
                field_name: "model_tier".into(),
                value_json: r#""synth_heavy""#.into(),
            },
        )
        .unwrap();

        let registry = ProviderRegistry::new(store);
        registry.load_from_db(&conn).unwrap();

        let without_override = registry
            .resolve_tier("fast_extract", None, None, None)
            .unwrap();
        assert_eq!(without_override.tier.model_id, "inception/mercury-2");

        let with_override = registry
            .resolve_tier(
                "fast_extract",
                Some("my-slug"),
                Some("code_pyramid"),
                Some("extract"),
            )
            .unwrap();
        assert_eq!(with_override.tier.model_id, "minimax/minimax-m2.7");
    }

    #[test]
    fn registry_missing_credential_surfaces_clear_error() {
        // When a provider's `api_key_ref` points at a variable that
        // isn't defined, the user should see the
        // "Settings → Credentials" hint, not a generic auth failure.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::pyramid::db::init_pyramid_db(&conn).unwrap();

        let tmp = TempDir::new().unwrap();
        let store = Arc::new(CredentialStore::load(tmp.path()).unwrap());
        std::mem::forget(tmp);
        // Intentionally do NOT set OPENROUTER_KEY.

        let registry = ProviderRegistry::new(store);
        registry.load_from_db(&conn).unwrap();

        let resolved = registry.resolve_tier("web", None, None, None).unwrap();
        // `instantiate_provider` returns `(Box<dyn LlmProvider>, Option<ResolvedSecret>)`
        // which cannot be unwrap_err'd because ResolvedSecret has no
        // Debug impl (that's the whole opacity point). Match explicitly.
        let err = match registry.instantiate_provider(&resolved.provider) {
            Ok(_) => panic!("instantiate_provider should fail when credential is missing"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("OPENROUTER_KEY"), "got: {err}");
        assert!(err.contains("Settings → Credentials"), "got: {err}");
    }
}
