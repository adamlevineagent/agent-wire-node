# Hotfix: JSON walker picks up `{handle}` as balanced JSON before the real object

## The Bug
The depth-tracking walker starts at the first `{` in the text. If the LLM response has preamble like "The format is `{handle}/{epoch-day}`" before the JSON, the walker finds `{handle}` as a balanced `{...}` pair (depth 0→1→0 in 8 chars) and returns that as the extracted JSON.

## The Fix
When the walker finds a balanced `{...}` range, try parsing it. If parsing fails, resume scanning from the NEXT `{` after the failed range. Repeat until a valid JSON object is found or no more `{` candidates exist.

```rust
fn find_and_parse_json(text: &str) -> Result<Value> {
    let bytes = text.as_bytes();
    let mut search_from = 0;

    while let Some(rel_start) = text[search_from..].find('{') {
        let start = search_from + rel_start;

        // Walk to find balanced close
        if let Some(end) = find_balanced_close(bytes, start) {
            let slice = &text[start..=end];
            // Try parse (with trailing comma fix)
            if let Ok(v) = try_parse_json(slice) {
                return Ok(v);
            }
        }

        // This { didn't work, try the next one
        search_from = start + 1;
    }

    Err(anyhow!("No valid JSON object found"))
}
```

This handles `{handle}`, `{hydrate, dehydrate}`, or any other small balanced brace pair that appears before the actual JSON object. The walker tries each one, serde rejects it, it moves on to the next `{`.

## Files
- `src-tauri/src/pyramid/llm.rs` — `extract_json()` / `find_json_bounds()`
