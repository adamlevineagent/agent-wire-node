# Debug Handoff: L0 extraction JSON parse failures

## The Problem
L0 extraction (`doc_extract`) is failing with "No JSON found in: {..." on some documents. The build aborts after 3 failed retries on a single doc (e.g., `architecture/data-model.md`, 27KB).

OpenRouter logs show Mercury 2 returning valid responses with finish reason `stop` and 1-2K output tokens. The JSON starts correctly (`{"headline": "AI Newsroom Data Model", "orientation": ...`) but the Rust parser rejects it.

## What we know
- 101/127 L0 extractions succeed. Only specific docs fail.
- OpenRouter shows `stop` finish reason, not `length` — the response isn't truncated
- Output sizes are 1-2K tokens — reasonable, not bloated
- The Rust parser at `llm.rs:554-620` finds `{` and `}`, extracts the slice, tries `serde_json::from_str`, fails. Then tries trailing comma fix, still fails.
- All Mercury 2, no Qwen cascades
- The extraction prompt was recently updated for density — but the old prompt also had occasional parse failures

## What to investigate

### 1. Log the full failed response
The error log truncates at 200 chars (`&text[..text.len().min(200)]`). We need the FULL response body that fails to parse.

In `llm.rs:extract_json()`, before the final error return at line 616, add:
```rust
warn!("[JSON_DEBUG] Full failed response ({} chars): {}", text.len(), &text[..text.len().min(2000)]);
warn!("[JSON_DEBUG] Extracted slice ({} chars): {}", slice.len(), &slice[..slice.len().min(2000)]);
```

This tells us:
- Is the full response actually complete?
- What does the extracted `{...}` slice look like?
- Where does serde_json choke?

### 2. Try serde_json with error details
Replace `serde_json::from_str::<Value>(slice)` with explicit error logging:
```rust
match serde_json::from_str::<Value>(&fixed) {
    Ok(v) => return Ok(v),
    Err(e) => {
        warn!("[JSON_DEBUG] serde_json error: {} at line {} col {}", e, e.line(), e.column());
        // Also show the chars around the error location
        let err_offset = /* compute byte offset from line/col */;
        warn!("[JSON_DEBUG] Context around error: ...{}...", &fixed[err_offset.saturating_sub(50)..fixed.len().min(err_offset+50)]);
    }
}
```

### 3. Check for common Mercury 2 JSON issues
- **Unescaped newlines in string values** — Mercury 2 sometimes puts literal `\n` in JSON strings without proper escaping
- **Unescaped quotes** — document content with quotes that Mercury 2 embeds without escaping
- **Unicode issues** — em-dashes, smart quotes, special chars that break JSON
- **Trailing content after JSON** — Mercury 2 sometimes appends a sentence after the closing `}`
- **Nested thinking** — despite `/no_think`, some models still emit reasoning that gets mixed into the response

### 4. Test the specific failing doc
Extract chunk index 5 from `core-selected-docs21` and send it manually to Mercury 2 with the extraction prompt. See what comes back raw.

```sql
SELECT content FROM pyramid_chunks WHERE slug='core-selected-docs21' AND chunk_index=5;
```

## Files
- `src-tauri/src/pyramid/llm.rs` — `extract_json()` function, lines 554-620. Add debug logging.
- The specific failing doc: chunk_index 5 = `architecture/data-model.md` (27KB)
- The extraction prompt: `chains/prompts/document/doc_extract.md`

## Expected outcome
The debug logging reveals what specific character or structure in Mercury 2's response breaks `serde_json`. Then we either fix the sanitizer in `extract_json()` or adjust the prompt to avoid triggering the issue.
