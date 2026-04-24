// pyramid/messages.rs — ChatML → (system_prompt, user_prompt) pair conversion.
//
// Per `compute-market-phase-2-exchange.md` §III and `compute-market-
// architecture.md` §VIII.6 DD-C: market dispatches carry a ChatML
// `messages: Value` payload (array of `{role, content}` objects); the
// downstream `QueueEntry` + Ollama call path expects two strings —
// `system_prompt` and `user_prompt`. This module owns the single
// canonical conversion.
//
// Phase 2 (this workstream) and Phase 4 (bridge) both call through here
// so the shape-normalization lives in one place. The bridge path
// already speaks messages natively but still runs the helper first for
// validation — if the ChatML is structurally broken, we'd rather catch
// it at the handler boundary than downstream in Ollama / OpenRouter.
//
// Policy decisions encoded here (locked in DD-C):
//   - Only `system` / `user` roles are accepted. `assistant` turns
//     are rejected in Phase 2 — market dispatches are single-turn
//     completions; assistant turns would require chat-history
//     semantics we don't implement yet.
//   - Multiple user messages are concatenated with `\n\n`.
//   - The FIRST `system` message becomes `system_prompt`. Subsequent
//     system messages are rejected with `InvalidShape` — DD-C line 751
//     ("first `system` message → `system_prompt`") is explicit and the
//     Wire's fill handler is expected to emit at most one system turn.
//     A dispatch with multiple system turns is a bug upstream; surface
//     it loudly rather than silently concatenating.
//   - At least one user message is required. No user messages = reject.
//   - Missing `role` / `content` fields or non-string `content` →
//     `InvalidShape`.

use serde::{Deserialize, Serialize};

/// Categorized failure modes for `messages_to_prompt_pair`. Each
/// variant corresponds to a 400-class response from the Phase 2
/// dispatch handler (with the variant carried in the response body
/// for operator observability).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum MessagesError {
    /// The payload is not a JSON array, an element is not a JSON
    /// object, or required fields (`role`, `content`) are missing or
    /// the wrong type.
    InvalidShape,
    /// A message's `role` field is something other than `system` /
    /// `user` / `assistant`. The unexpected role string is carried for
    /// operator logs.
    UnknownRole(String),
    /// The conversation has no `user` messages — we have nothing to
    /// ask the model.
    NoUserMessages,
    /// Phase 2 rejects multi-turn conversations. `assistant` messages
    /// imply chat-history semantics we don't implement yet; revisit
    /// when a chat-replay flow is spec'd.
    AssistantTurns,
}

impl std::fmt::Display for MessagesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MessagesError::InvalidShape => {
                write!(
                    f,
                    "messages: malformed shape (expected array of {{role, content}} objects)"
                )
            }
            MessagesError::UnknownRole(r) => {
                write!(
                    f,
                    "messages: unknown role '{r}' (only 'system' and 'user' accepted)"
                )
            }
            MessagesError::NoUserMessages => {
                write!(f, "messages: no user messages present")
            }
            MessagesError::AssistantTurns => {
                write!(
                    f,
                    "messages: assistant turns rejected in Phase 2 (single-turn only)"
                )
            }
        }
    }
}

impl std::error::Error for MessagesError {}

/// Convert a ChatML-shape `messages: Value` into a
/// `(system_prompt, user_prompt)` pair for the `QueueEntry` /
/// Ollama call path. See module-level docs for the accepted shape
/// and policy decisions.
pub fn messages_to_prompt_pair(
    messages: &serde_json::Value,
) -> Result<(String, String), MessagesError> {
    let arr = messages.as_array().ok_or(MessagesError::InvalidShape)?;

    let mut system_prompt: Option<String> = None;
    let mut user_parts: Vec<String> = Vec::new();

    for entry in arr {
        let obj = entry.as_object().ok_or(MessagesError::InvalidShape)?;

        let role = obj
            .get("role")
            .and_then(|v| v.as_str())
            .ok_or(MessagesError::InvalidShape)?;

        // `content` must be present AND a string. Some ChatML dialects
        // allow content to be an array of content-parts (multimodal);
        // Phase 2 dispatches are text-only, so we reject array content
        // as shape-invalid rather than silently flattening to "".
        let content = obj
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or(MessagesError::InvalidShape)?;

        match role {
            "system" => {
                // DD-C: "first `system` message → `system_prompt`". A
                // second system turn is a spec violation — fail loud
                // instead of silently concatenating.
                if system_prompt.is_some() {
                    return Err(MessagesError::InvalidShape);
                }
                system_prompt = Some(content.to_string());
            }
            "user" => user_parts.push(content.to_string()),
            "assistant" => return Err(MessagesError::AssistantTurns),
            other => return Err(MessagesError::UnknownRole(other.to_string())),
        }
    }

    if user_parts.is_empty() {
        return Err(MessagesError::NoUserMessages);
    }

    // No system messages is fine — empty string for `system_prompt` is
    // a valid downstream input (Ollama / OpenRouter treat it as "no
    // system preamble").
    let system_prompt = system_prompt.unwrap_or_default();
    let user_prompt = user_parts.join("\n\n");
    Ok((system_prompt, user_prompt))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn happy_path_single_system_single_user() {
        let messages = json!([
            { "role": "system", "content": "You are a helpful assistant." },
            { "role": "user", "content": "What is the capital of France?" },
        ]);
        let (sys, usr) = messages_to_prompt_pair(&messages).unwrap();
        assert_eq!(sys, "You are a helpful assistant.");
        assert_eq!(usr, "What is the capital of France?");
    }

    #[test]
    fn user_only_produces_empty_system_prompt() {
        let messages = json!([{ "role": "user", "content": "Hi" }]);
        let (sys, usr) = messages_to_prompt_pair(&messages).unwrap();
        assert_eq!(sys, "");
        assert_eq!(usr, "Hi");
    }

    #[test]
    fn multiple_user_messages_concatenate_with_double_newline() {
        let messages = json!([
            { "role": "user", "content": "First question." },
            { "role": "user", "content": "Also this." },
        ]);
        let (_, usr) = messages_to_prompt_pair(&messages).unwrap();
        assert_eq!(usr, "First question.\n\nAlso this.");
    }

    #[test]
    fn multiple_system_messages_rejected_as_invalid_shape() {
        // DD-C: "first `system` message → `system_prompt`". A second
        // system turn is a spec violation — the Wire's fill handler is
        // expected to emit at most one system turn. Reject loudly so
        // operators catch Wire-side regressions rather than silently
        // concatenating.
        let messages = json!([
            { "role": "system", "content": "Preamble A." },
            { "role": "system", "content": "Preamble B." },
            { "role": "user", "content": "go" },
        ]);
        assert_eq!(
            messages_to_prompt_pair(&messages).unwrap_err(),
            MessagesError::InvalidShape
        );
    }

    #[test]
    fn interleaved_system_and_user_preserves_user_order() {
        // One system message (first, per spec) + multiple user
        // messages interleaved. User messages are concatenated in
        // source order; the single system becomes system_prompt.
        let messages = json!([
            { "role": "system", "content": "A" },
            { "role": "user", "content": "1" },
            { "role": "user", "content": "2" },
        ]);
        let (sys, usr) = messages_to_prompt_pair(&messages).unwrap();
        assert_eq!(sys, "A");
        assert_eq!(usr, "1\n\n2");
    }

    #[test]
    fn system_after_user_is_accepted_if_only_one_system() {
        // DD-C doesn't require the single system turn to come first —
        // it just says "first system message → system_prompt". One
        // system turn in any position is fine; the rejection rule is
        // specifically about a SECOND system turn.
        let messages = json!([
            { "role": "user", "content": "hi" },
            { "role": "system", "content": "be concise" },
        ]);
        let (sys, usr) = messages_to_prompt_pair(&messages).unwrap();
        assert_eq!(sys, "be concise");
        assert_eq!(usr, "hi");
    }

    #[test]
    fn no_user_messages_returns_no_user_messages() {
        let messages = json!([{ "role": "system", "content": "Alone" }]);
        assert_eq!(
            messages_to_prompt_pair(&messages).unwrap_err(),
            MessagesError::NoUserMessages
        );
    }

    #[test]
    fn empty_array_returns_no_user_messages() {
        let messages = json!([]);
        assert_eq!(
            messages_to_prompt_pair(&messages).unwrap_err(),
            MessagesError::NoUserMessages
        );
    }

    #[test]
    fn assistant_turn_is_rejected() {
        let messages = json!([
            { "role": "user", "content": "Hi" },
            { "role": "assistant", "content": "Hello" },
            { "role": "user", "content": "Again" },
        ]);
        assert_eq!(
            messages_to_prompt_pair(&messages).unwrap_err(),
            MessagesError::AssistantTurns
        );
    }

    #[test]
    fn unknown_role_carries_role_string() {
        let messages = json!([
            { "role": "user", "content": "Hi" },
            { "role": "function_call", "content": "{}" },
        ]);
        match messages_to_prompt_pair(&messages).unwrap_err() {
            MessagesError::UnknownRole(r) => assert_eq!(r, "function_call"),
            e => panic!("expected UnknownRole, got {e:?}"),
        }
    }

    #[test]
    fn non_array_payload_is_invalid_shape() {
        // ChatML requires an array at the top level. Object / string /
        // number all fail.
        let object = json!({ "messages": [] });
        assert_eq!(
            messages_to_prompt_pair(&object).unwrap_err(),
            MessagesError::InvalidShape
        );
        assert_eq!(
            messages_to_prompt_pair(&json!("not an array")).unwrap_err(),
            MessagesError::InvalidShape
        );
        assert_eq!(
            messages_to_prompt_pair(&json!(42)).unwrap_err(),
            MessagesError::InvalidShape
        );
    }

    #[test]
    fn non_object_element_is_invalid_shape() {
        let messages = json!([
            { "role": "user", "content": "ok" },
            "bare string mid-array"
        ]);
        assert_eq!(
            messages_to_prompt_pair(&messages).unwrap_err(),
            MessagesError::InvalidShape
        );
    }

    #[test]
    fn missing_role_is_invalid_shape() {
        let messages = json!([{ "content": "orphan" }]);
        assert_eq!(
            messages_to_prompt_pair(&messages).unwrap_err(),
            MessagesError::InvalidShape
        );
    }

    #[test]
    fn missing_content_is_invalid_shape() {
        let messages = json!([{ "role": "user" }]);
        assert_eq!(
            messages_to_prompt_pair(&messages).unwrap_err(),
            MessagesError::InvalidShape
        );
    }

    #[test]
    fn non_string_content_is_invalid_shape() {
        // Multimodal content-part arrays are not supported in Phase 2;
        // reject loudly rather than flatten to empty string.
        let messages = json!([
            { "role": "user", "content": [{"type": "text", "text": "hi"}] }
        ]);
        assert_eq!(
            messages_to_prompt_pair(&messages).unwrap_err(),
            MessagesError::InvalidShape
        );
        // Also null / number.
        let messages = json!([{ "role": "user", "content": null }]);
        assert_eq!(
            messages_to_prompt_pair(&messages).unwrap_err(),
            MessagesError::InvalidShape
        );
        let messages = json!([{ "role": "user", "content": 42 }]);
        assert_eq!(
            messages_to_prompt_pair(&messages).unwrap_err(),
            MessagesError::InvalidShape
        );
    }

    #[test]
    fn empty_content_strings_are_accepted() {
        // An empty string is a valid content value — the operator might
        // deliberately send a priming-only prompt with an empty user
        // turn to probe the system prompt's behavior. Policy: accept
        // empty content as long as at least one user message exists.
        let messages = json!([{ "role": "user", "content": "" }]);
        let (sys, usr) = messages_to_prompt_pair(&messages).unwrap();
        assert_eq!(sys, "");
        assert_eq!(usr, "");
    }

    #[test]
    fn messages_error_serializes_with_kind_and_detail() {
        // Handler response body shape: `{ "kind": "...", "detail": "..." }`.
        let e = MessagesError::UnknownRole("tool".to_string());
        let json_str = serde_json::to_string(&e).unwrap();
        assert!(
            json_str.contains("\"kind\":\"unknown_role\""),
            "serialized form: {json_str}"
        );
        assert!(
            json_str.contains("\"detail\":\"tool\""),
            "serialized form: {json_str}"
        );

        let e = MessagesError::NoUserMessages;
        let json_str = serde_json::to_string(&e).unwrap();
        // Unit variants carry no detail.
        assert!(
            json_str.contains("\"kind\":\"no_user_messages\""),
            "serialized form: {json_str}"
        );
    }
}
