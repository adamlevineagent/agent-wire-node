// partner/conversation.rs — Message handler, LLM calls, buffer management
//
// Core loop:
//   1. Load session
//   2. Add user message to buffer
//   3. Check buffer overflow → crystallize if needed
//   4. Assemble context window
//   5. Call OpenRouter with tools
//   6. Tool call loop (up to 5 rounds)
//   7. Mechanical lift (move tool results to lifted_results)
//   8. Parse context_schedule
//   9. Add partner response to buffer
//   10. Save session
//   11. Return response

use anyhow::{anyhow, Result};
use serde_json::Value;
use tracing::{info, warn};

use super::context::{apply_context_schedule, assemble_context_window, LlmMessage};
use super::{
    load_session, save_session, BrainState, DennisState, LiftedResult, Message, MessageRole,
    PartnerLlmConfig, PartnerResponse, PartnerState, Session, BUFFER_SOFT_LIMIT, MAX_TOOL_CALLS,
};
use crate::pyramid::query;

// ── Token estimation ────────────────────────────────────────────────

/// Estimate token count from text. Conservative: chars / 3.2
pub fn estimate_tokens(text: &str) -> usize {
    (text.len() as f64 / 3.2) as usize
}

// ── OpenRouter tool definitions ─────────────────────────────────────

/// Build the tools array for the OpenRouter function-calling API.
fn build_tool_definitions() -> Value {
    serde_json::json!([
        {
            "type": "function",
            "function": {
                "name": "pyramid_query",
                "description": "Search or drill into the knowledge pyramid. Use 'search' to find nodes by term, 'drill' to get a node and its children, or 'entities' to list all entities.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": ["search", "drill", "entities"],
                            "description": "The query action to perform"
                        },
                        "slug": {
                            "type": "string",
                            "description": "The pyramid slug to query"
                        },
                        "term": {
                            "type": "string",
                            "description": "Search term (required for 'search' action)"
                        },
                        "node_id": {
                            "type": "string",
                            "description": "Node ID to drill into (required for 'drill' action)"
                        }
                    },
                    "required": ["action", "slug"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "context_schedule",
                "description": "Manage your brain map for the next turn. Hydrate nodes to load them, dehydrate to remove them.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "hydrate": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Node IDs to load into your brain map"
                        },
                        "dehydrate": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Node IDs to remove from your brain map"
                        }
                    }
                }
            }
        }
    ])
}

// ── LLM Call ────────────────────────────────────────────────────────

/// Response from the partner LLM call.
#[derive(Debug)]
pub struct PartnerLlmResponse {
    /// The text content of the response (if any).
    pub content: Option<String>,
    /// Tool calls requested by the model (if any).
    pub tool_calls: Vec<ToolCall>,
}

/// A tool call from the model.
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// Call the partner model via OpenRouter with multi-turn messages and optional tools.
///
/// Includes retry logic with exponential backoff for transient errors.
///
/// Phase 3: the OpenRouter URL + headers are built via the
/// `OpenRouterProvider` trait impl in `pyramid::provider` so the
/// hardcoded URL literal lives in exactly one place. Partner keeps its
/// own config struct because its request body includes tool-call
/// wiring that the pyramid chain executor doesn't use.
pub async fn call_partner(
    config: &PartnerLlmConfig,
    messages: Vec<LlmMessage>,
    tools: Option<Value>,
) -> Result<PartnerLlmResponse> {
    use crate::pyramid::credentials::ResolvedSecret;
    use crate::pyramid::provider::{LlmProvider, OpenRouterProvider};

    let client = reqwest::Client::new();
    let provider = OpenRouterProvider {
        id: "openrouter".to_string(),
        display_name: "Wire Partner (Dennis)".to_string(),
        base_url: "https://openrouter.ai/api/v1".to_string(),
        extra_headers: vec![],
    };
    let url = provider.chat_completions_url();
    // Partner holds its credential inline for legacy reasons. Wrap it
    // in the same opaque envelope the rest of the stack uses so the
    // prepare_headers surface is provider-uniform. Phase 4 will move
    // this into the credential store.
    let secret = ResolvedSecret::new(config.api_key.clone());
    let built_headers = provider
        .prepare_headers(Some(&secret))
        .map_err(|e| anyhow!("failed to build partner headers: {}", e))?;

    for attempt in 0..5u32 {
        let mut body = serde_json::json!({
            "model": config.partner_model,
            "messages": messages,
            "temperature": 0.7,
            "max_tokens": 4096,
            "provider": {
                "allow_fallbacks": true
            }
        });

        if let Some(ref tools_val) = tools {
            body["tools"] = tools_val.clone();
        }

        let mut request = client
            .post(&url)
            .timeout(std::time::Duration::from_secs(120));
        for (k, v) in &built_headers {
            // Partner keeps the legacy "Wire Partner (Dennis)" title
            // so its broadcasts stay attributed correctly. Override the
            // provider's default title header for this single field.
            if k == "X-Title" || k == "X-OpenRouter-Title" {
                request = request.header(k, "Wire Partner (Dennis)");
            } else {
                request = request.header(k, v);
            }
        }
        let resp = request.json(&body).send().await;

        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                if attempt < 4 {
                    info!("[partner] request error, retry {}...", attempt + 1);
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    continue;
                }
                return Err(anyhow!("Partner request failed after 5 attempts: {}", e));
            }
        };

        let status = resp.status().as_u16();

        // Retryable HTTP errors
        if matches!(status, 429 | 403 | 502 | 503) {
            let wait = 2u64.pow(attempt + 1);
            info!("[partner] HTTP {}, waiting {}s...", status, wait);
            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            continue;
        }

        if !resp.status().is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            if attempt < 4 {
                info!("[partner] HTTP {}, retry {}...", status, attempt + 1);
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
            return Err(anyhow!(
                "Partner HTTP {} after 5 attempts: {}",
                status,
                body_text
            ));
        }

        // Parse response
        let data: Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                if attempt < 4 {
                    info!("[partner] parse error, retry {}...", attempt + 1);
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    continue;
                }
                return Err(anyhow!("Failed to parse partner response: {}", e));
            }
        };

        // Extract message from choices[0].message
        let message = data
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"));

        let message = match message {
            Some(m) => m,
            None => {
                if attempt < 4 {
                    info!("[partner] no message in response, retry {}...", attempt + 1);
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    continue;
                }
                return Err(anyhow!("No message in partner response after 5 attempts"));
            }
        };

        // Extract content
        let content = message
            .get("content")
            .and_then(|c| c.as_str())
            .map(|s| s.to_string());

        // Extract tool calls
        let mut tool_calls = Vec::new();
        if let Some(tc_array) = message.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tc_array {
                let id = tc
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = tc
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                let arguments = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|a| a.as_str())
                    .unwrap_or("{}")
                    .to_string();

                if !name.is_empty() {
                    tool_calls.push(ToolCall {
                        id,
                        name,
                        arguments,
                    });
                }
            }
        }

        return Ok(PartnerLlmResponse {
            content,
            tool_calls,
        });
    }

    Err(anyhow!("Partner max retries exceeded"))
}

// ── Tool Execution ──────────────────────────────────────────────────

/// Execute a pyramid_query tool call against the pyramid database.
fn execute_pyramid_query(
    reader: &rusqlite::Connection,
    arguments: &str,
) -> Result<(String, Vec<String>)> {
    let args: Value = serde_json::from_str(arguments)
        .map_err(|e| anyhow!("Invalid pyramid_query arguments: {}", e))?;

    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing 'action' in pyramid_query"))?;

    let slug_name = args
        .get("slug")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing 'slug' in pyramid_query"))?;

    match action {
        "search" => {
            let term = args.get("term").and_then(|v| v.as_str()).unwrap_or("");
            let hits = query::search(reader, slug_name, term)?;
            let node_ids: Vec<String> = hits.iter().map(|h| h.node_id.clone()).collect();
            let result = serde_json::to_string_pretty(&hits)?;
            Ok((result, node_ids))
        }
        "drill" => {
            let node_id = args
                .get("node_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("Missing 'node_id' for drill action"))?;
            let drill_result = query::drill(reader, slug_name, node_id)?;
            let mut node_ids = Vec::new();
            if let Some(ref dr) = drill_result {
                node_ids.push(dr.node.id.clone());
                for child in &dr.children {
                    node_ids.push(child.id.clone());
                }
            }
            let result = serde_json::to_string_pretty(&drill_result)?;
            Ok((result, node_ids))
        }
        "entities" => {
            let entities = query::entities(reader, slug_name)?;
            let result = serde_json::to_string_pretty(&entities)?;
            Ok((result, Vec::new()))
        }
        _ => Err(anyhow!("Unknown pyramid_query action: {}", action)),
    }
}

// ── Main Message Handler ────────────────────────────────────────────

/// Handle an incoming user message.
///
/// This is the core conversation loop. It:
/// 1. Loads or creates the session
/// 2. Adds the user message to the buffer
/// 3. Manages buffer overflow
/// 4. Assembles context and calls the LLM
/// 5. Processes tool calls (up to MAX_TOOL_CALLS rounds)
/// 6. Performs mechanical lift
/// 7. Parses context schedule
/// 8. Saves session state
pub async fn handle_message(
    state: &PartnerState,
    session_id: &str,
    user_message: &str,
) -> Result<PartnerResponse> {
    // 1. Load session
    let mut session = {
        let sessions = state.sessions.lock().await;
        match sessions.get(session_id) {
            Some(s) => s.clone(),
            None => {
                // Try loading from DB
                drop(sessions);
                let db = state.partner_db.lock().await;
                match load_session(&db, session_id)? {
                    Some(s) => s,
                    None => return Err(anyhow!("Session '{}' not found", session_id)),
                }
            }
        }
    };

    // Update Dennis state
    session.dennis_state = DennisState::Listening;
    session.last_active_at = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    // 2. Add user message to buffer
    let user_msg = Message {
        role: MessageRole::User,
        content: user_message.to_string(),
        timestamp: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        token_estimate: estimate_tokens(user_message),
    };
    session.conversation_buffer.push(user_msg);

    // 3. Check buffer overflow
    let buffer_tokens: usize = session
        .conversation_buffer
        .iter()
        .map(|m| m.token_estimate)
        .sum();
    if buffer_tokens > BUFFER_SOFT_LIMIT {
        handle_buffer_overflow(&mut session);
    }

    // 4. Assemble context window
    session.dennis_state = DennisState::Thinking;
    let context_messages = {
        let reader = state.pyramid_reader.lock().await;
        assemble_context_window(&reader, &session, user_message)
    };

    // 5. Call OpenRouter with tools
    let tools = build_tool_definitions();
    let llm_config = state.llm_config.read().await;
    let mut messages_for_llm = context_messages;
    let mut all_lifted: Vec<LiftedResult> = Vec::new();
    let mut final_content = String::new();
    let mut context_schedule_hydrate: Vec<String> = Vec::new();
    let mut context_schedule_dehydrate: Vec<String> = Vec::new();

    // 6. Tool call loop (up to MAX_TOOL_CALLS rounds)
    for round in 0..=MAX_TOOL_CALLS {
        let response = call_partner(
            &llm_config,
            messages_for_llm.clone(),
            if round < MAX_TOOL_CALLS {
                Some(tools.clone())
            } else {
                None
            },
        )
        .await;

        let response = match response {
            Ok(r) => r,
            Err(e) => {
                session.dennis_state =
                    DennisState::Error("I got a bit lost there — want to try again?".to_string());
                // Save session state even on error
                {
                    let db = state.partner_db.lock().await;
                    let _ = save_session(&db, &session);
                }
                let mut sessions = state.sessions.lock().await;
                sessions.insert(session_id.to_string(), session.clone());

                return Err(anyhow!("LLM call failed: {}", e));
            }
        };

        // Collect any text content
        if let Some(ref content) = response.content {
            final_content = content.clone();
        }

        // If no tool calls, we're done
        if response.tool_calls.is_empty() {
            break;
        }

        // Process tool calls
        session.dennis_state = DennisState::Searching;
        // Add the assistant message with tool_calls to the conversation
        let mut assistant_msg = serde_json::json!({
            "role": "assistant",
        });
        if let Some(ref content) = response.content {
            assistant_msg["content"] = serde_json::json!(content);
        }
        // Build tool_calls array
        let tc_json: Vec<Value> = response
            .tool_calls
            .iter()
            .map(|tc| {
                serde_json::json!({
                    "id": tc.id,
                    "type": "function",
                    "function": {
                        "name": tc.name,
                        "arguments": tc.arguments
                    }
                })
            })
            .collect();
        assistant_msg["tool_calls"] = serde_json::json!(tc_json);

        messages_for_llm.push(LlmMessage {
            role: "assistant".to_string(),
            content: response.content.clone().unwrap_or_default(),
            tool_call_id: None,
            tool_calls: Some(serde_json::json!(tc_json)),
        });

        for tc in &response.tool_calls {
            match tc.name.as_str() {
                "pyramid_query" => {
                    let result = {
                        let reader = state.pyramid_reader.lock().await;
                        execute_pyramid_query(&reader, &tc.arguments)
                    };
                    match result {
                        Ok((result_text, node_ids)) => {
                            all_lifted.push(LiftedResult {
                                query: tc.arguments.clone(),
                                result: result_text.clone(),
                                node_ids,
                            });
                            // Add tool result message
                            messages_for_llm.push(LlmMessage {
                                role: "tool".to_string(),
                                content: result_text,
                                tool_call_id: Some(tc.id.clone()),
                                tool_calls: None,
                            });
                        }
                        Err(e) => {
                            messages_for_llm.push(LlmMessage {
                                role: "tool".to_string(),
                                content: format!("Error: {}", e),
                                tool_call_id: Some(tc.id.clone()),
                                tool_calls: None,
                            });
                        }
                    }
                }
                "context_schedule" => {
                    // Parse context schedule
                    if let Ok(args) = serde_json::from_str::<Value>(&tc.arguments) {
                        if let Some(hydrate) = args.get("hydrate").and_then(|v| v.as_array()) {
                            for id in hydrate {
                                if let Some(s) = id.as_str() {
                                    context_schedule_hydrate.push(s.to_string());
                                }
                            }
                        }
                        if let Some(dehydrate) = args.get("dehydrate").and_then(|v| v.as_array()) {
                            for id in dehydrate {
                                if let Some(s) = id.as_str() {
                                    context_schedule_dehydrate.push(s.to_string());
                                }
                            }
                        }
                    }
                    // context_schedule doesn't produce a tool result for the model
                    messages_for_llm.push(LlmMessage {
                        role: "tool".to_string(),
                        content: "Context schedule applied.".to_string(),
                        tool_call_id: Some(tc.id.clone()),
                        tool_calls: None,
                    });
                }
                _ => {
                    // Unknown tool — send error back
                    messages_for_llm.push(LlmMessage {
                        role: "tool".to_string(),
                        content: format!("Unknown tool: {}", tc.name),
                        tool_call_id: Some(tc.id.clone()),
                        tool_calls: None,
                    });
                }
            }
        }
    }

    // 7. Mechanical lift — move tool results to lifted_results
    session.lifted_results.extend(all_lifted);
    if session.lifted_results.len() > 20 {
        session.lifted_results.drain(..session.lifted_results.len() - 20);
    }

    // 8. Apply context schedule
    apply_context_schedule(
        &mut session,
        context_schedule_hydrate,
        context_schedule_dehydrate,
    );

    // 9. Add partner response to buffer
    if final_content.is_empty() {
        final_content =
            "I seem to have lost my train of thought. Could you repeat that?".to_string();
    }

    let partner_msg = Message {
        role: MessageRole::Partner,
        content: final_content.clone(),
        timestamp: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        token_estimate: estimate_tokens(&final_content),
    };
    session.conversation_buffer.push(partner_msg);

    // ── Warm pass: progressive crystallization ──────────────────────

    // Tier 1: zero-cost regex extraction on every user message
    let tier1 = super::warm::tier1_extract(user_message);
    if !tier1.entities.is_empty() {
        info!(
            "[partner] Tier 1 extracted {} entities",
            tier1.entities.len()
        );
    }

    // Tier 2: check if warm pass should run (background, non-blocking)
    if let Some(slug) = &session.slug {
        if super::warm::should_run_warm_pass(&session) {
            let slug = slug.clone();
            let new_messages: Vec<_> = session.conversation_buffer[session.warm_cursor..].to_vec();
            let new_cursor = session.conversation_buffer.len();

            // Update cursor BEFORE spawning to prevent double-run
            session.warm_cursor = new_cursor;

            // Check concurrent-execution guard
            let should_spawn = {
                let mut in_progress = state.warm_in_progress.lock().unwrap();
                if in_progress.contains(&slug) {
                    false // Another warm pass is already running for this slug
                } else {
                    in_progress.insert(slug.clone());
                    true
                }
            };

            if should_spawn {
                let reader = state.pyramid_reader.clone();
                let writer = state.pyramid.writer.clone();
                let api_key = llm_config.api_key.clone();
                let model = llm_config.partner_model.clone();
                let collapse_model = state
                    .pyramid
                    .data_dir
                    .as_ref()
                    .map(|d| crate::pyramid::PyramidConfig::load(d).collapse_model)
                    .unwrap_or_else(|| "x-ai/grok-4.20-beta".into());
                let warm_in_progress = state.warm_in_progress.clone();
                let slug_for_cleanup = slug.clone();

                let ops = state.pyramid.operational.clone();
                tokio::spawn(async move {
                    let result = super::warm::warm_pass(
                        new_messages,
                        &slug,
                        &reader,
                        &writer,
                        &api_key,
                        &model,
                        &collapse_model,
                        &ops,
                    )
                    .await;

                    // Remove from in-progress guard
                    {
                        let mut guard = warm_in_progress.lock().unwrap();
                        guard.remove(&slug_for_cleanup);
                    }

                    match result {
                        Ok(result) => {
                            info!(
                                "[partner] Warm pass complete: {} deltas, {} topics",
                                result.deltas_created,
                                result.new_topics.len()
                            );
                        }
                        Err(e) => {
                            warn!("[partner] Warm pass failed: {}", e);
                        }
                    }
                });
            }
        }
    }

    // Update state
    session.dennis_state = DennisState::Idle;

    // Calculate brain state
    let buffer_tokens: usize = session
        .conversation_buffer
        .iter()
        .map(|m| m.token_estimate)
        .sum();

    let brain_state = BrainState {
        hydrated_node_ids: session.hydrated_node_ids.clone(),
        session_topics: session.session_topics.clone(),
        lifted_results: session.lifted_results.clone(),
        buffer_tokens,
        buffer_capacity: super::BUFFER_HARD_LIMIT,
    };

    // 10. Save session
    {
        let db = state.partner_db.lock().await;
        save_session(&db, &session)?;
    }

    // Update in-memory cache
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.to_string(), session.clone());
    }

    // 11. Return response
    Ok(PartnerResponse {
        message: final_content,
        dennis_state: session.dennis_state,
        brain_state,
        session_id: session_id.to_string(),
    })
}

// ── Buffer Overflow ─────────────────────────────────────────────────

/// Handle buffer overflow by removing the oldest messages.
///
/// In the future this will crystallize via forward pass → provisional L0 nodes.
/// For now, it simply truncates the oldest messages to stay under the soft limit.
fn handle_buffer_overflow(session: &mut Session) {
    let total: usize = session
        .conversation_buffer
        .iter()
        .map(|m| m.token_estimate)
        .sum();

    if total <= BUFFER_SOFT_LIMIT || session.conversation_buffer.len() <= 2 {
        return;
    }

    let mut running = total;
    let mut remove_count = 0;
    for msg in &session.conversation_buffer {
        if running <= BUFFER_SOFT_LIMIT || session.conversation_buffer.len() - remove_count <= 2 {
            break;
        }
        running -= msg.token_estimate;
        remove_count += 1;
    }

    if remove_count > 0 {
        let removed: Vec<_> = session.conversation_buffer.drain(0..remove_count).collect();
        let new_total: usize = session
            .conversation_buffer
            .iter()
            .map(|m| m.token_estimate)
            .sum();
        info!(
            "[partner] Buffer overflow: removed {} oldest messages ({} tokens removed), buffer now {} tokens",
            removed.len(),
            total - new_total,
            new_total,
        );

        // Adjust warm_cursor to account for removed messages
        if session.warm_cursor >= remove_count {
            session.warm_cursor -= remove_count;
        } else {
            session.warm_cursor = 0;
        }
    }
}
