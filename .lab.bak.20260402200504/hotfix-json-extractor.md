# Hotfix: JSON extractor picks up brackets inside string values

## The Bug
`llm.rs:extract_json()` finds JSON boundaries using `text.find('{')`, `text.rfind('}')`, `text.find('[')`, `text.rfind(']')`. It picks whichever of `{` or `[` comes first.

The LLM response contains markdown content inside JSON string values — things like `[Revoke]`, `[slug]`, `{handle}/{epoch-day}/{sequence}`. When the response has any preamble text (even a space or newline) before the JSON object, a `[` inside a string value can appear before the actual `{` — causing the extractor to grab `[Revoke]...last }` as the "JSON" instead of the actual `{...}` object.

Debug output confirms:
```
Extracted slice (26424 chars): [Revoke]` action. |
Extracted slice (5 chars): [kit]
Extracted slice (6 chars): [slug]
Extracted slice (33 chars): {handle}/{epoch‑day}/{sequence}
```

## The Fix
Try parsing as an object (`{...}`) first. Only try array (`[...]`) if no valid object is found.

```rust
pub fn extract_json(text: &str) -> Result<Value> {
    // ... existing think tag and fence stripping ...

    // Try object first (all our prompts produce objects)
    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            if end >= start {
                let slice = &text[start..=end];
                if let Ok(v) = try_parse_json(slice) {
                    return Ok(v);
                }
            }
        }
    }

    // Fall back to array
    if let Some(start) = text.find('[') {
        if let Some(end) = text.rfind(']') {
            if end >= start {
                let slice = &text[start..=end];
                if let Ok(v) = try_parse_json(slice) {
                    return Ok(v);
                }
            }
        }
    }

    Err(anyhow!("No JSON found in: {}", &text[..text.len().min(200)]))
}

fn try_parse_json(slice: &str) -> Result<Value> {
    if let Ok(v) = serde_json::from_str::<Value>(slice) {
        return Ok(v);
    }
    // Fix trailing commas
    static COMMA_BRACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r",\s*}").unwrap());
    static COMMA_BRACKET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r",\s*]").unwrap());
    let fixed = COMMA_BRACE.replace_all(slice, "}");
    let fixed = COMMA_BRACKET.replace_all(&fixed, "]");
    serde_json::from_str::<Value>(&fixed).map_err(|e| anyhow!("JSON parse failed: {}", e))
}
```

The key change: try `{...}` extraction first, independently of `[...]`. The current code picks whichever delimiter comes first, which is wrong when brackets appear inside JSON string values.

## Files
- `src-tauri/src/pyramid/llm.rs` — `extract_json()` function
