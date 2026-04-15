# Systemic Split Fix — Progress Guarantees, Fallback Termination, Defensive Bounds

## Root Cause

`split_by_lines()` has an infinite loop when a single line exceeds `max_tokens`. Line 2852:
```rust
start = end.saturating_sub(overlap_line_count);
```
When `end = start + 1` (one line consumes the budget) and `overlap_line_count = 1`, `start` stays at `start`. Each iteration pushes a ~1 MB String to `chunks: Vec<String>`, growing to 82+ GB before OOM kill.

**Trigger:** Chunk 710 has a 1,000,057 character line (250K tokens) — a QA log file.

## Systemic Issues

This is not an isolated bug. Three systemic patterns across all split functions:

1. **No progress guarantee** — `split_by_lines` and `split_by_tokens` both use `start = end.saturating_sub(overlap)` which can stall
2. **No fallback termination** — `split_by_sections` → `split_by_lines` → nothing. Character splitting exists but is never used as fallback
3. **No defensive bounds** — no iteration caps, no output validation, unclamped overlap

## Pre-condition: Floor on max_tokens

At the entry point of `split_chunk`, add a floor:
```rust
let max_tokens = max_tokens.max(1000);
```
This prevents `max_tokens == 0` or very small values from producing a memory bomb (1M empty-string chunks in `split_by_tokens` when `chars_per_chunk = 0`). The floor of 1000 tokens (~4K chars per chunk minimum) is safe for any LLM.

## Functions to Fix

All in `chain_executor.rs`:

| Function | Line | Issue |
|---|---|---|
| `split_by_lines` | 2809 | **Infinite loop** on lines > max_tokens. No character fallback. |
| `split_by_sections` | 2720 | Delegates to `split_by_lines` for oversized sections — inherits the infinite loop. `build_overlap_suffix` can return oversized overlap. |
| `split_by_tokens` | 2862 | Theoretical stall if `overlap_chars >= chars_per_chunk` (safe in practice but undefended). |
| `build_overlap_suffix` | 2891 | Can return a string larger than `overlap_tokens` if first line is huge. |
| `split_chunk` | 2919 | No output validation — oversized chunks can pass through. |

## The Fix (5 changes)

### 1. `split_by_lines`: progress guarantee + character fallback

```rust
fn split_by_lines(content: &str, max_tokens: usize, overlap_tokens: usize) -> Vec<String> {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return vec![content.to_string()];
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut start = 0;

    while start < lines.len() {
        let mut end = start;
        let mut tokens = 0usize;

        while end < lines.len() {
            let line_tokens = estimate_tokens(lines[end]);
            if tokens + line_tokens > max_tokens && end > start {
                break;
            }
            tokens += line_tokens;
            end += 1;
        }

        // FIX 1a: If a single line exceeds max_tokens (end == start + 1 and
        // that line is over budget), split it by characters as terminal fallback.
        if end == start + 1 && estimate_tokens(lines[start]) > max_tokens {
            let line_sub_chunks = split_by_tokens(lines[start], max_tokens, overlap_tokens);
            chunks.extend(line_sub_chunks);
            start = end; // advance past the oversized line
            continue;
        }

        let chunk_text = lines[start..end].join("\n");
        chunks.push(chunk_text);

        if end >= lines.len() {
            break;
        }

        let mut overlap_line_count = 0;
        let mut overlap_tok = 0usize;
        for i in (start..end).rev() {
            let lt = estimate_tokens(lines[i]);
            if overlap_tok + lt > overlap_tokens && overlap_line_count > 0 {
                break;
            }
            overlap_tok += lt;
            overlap_line_count += 1;
        }

        let new_start = end.saturating_sub(overlap_line_count);
        // FIX 1b: Guarantee forward progress — start MUST advance by at least 1.
        start = new_start.max(start + 1);
    }

    if chunks.is_empty() {
        vec![content.to_string()]
    } else {
        chunks
    }
}
```

**Key changes:**
- **1a:** When a single line exceeds `max_tokens`, delegate to `split_by_tokens` (byte-level splitting) for that line. This is the terminal fallback that guarantees termination. Overlap continuity is sacrificed at oversized-line boundaries — such content (single lines > 80K tokens) is inherently non-semantic (minified JSON, append-only logs), so overlap loss is acceptable.
- **1b:** `start = new_start.max(start + 1)` — even if overlap wants to stay put, we advance by at least 1 line. Prevents infinite loop for any edge case.

### 2. `split_by_tokens`: byte-consistent splitting + progress guarantee

`estimate_tokens` uses `text.len()` (byte length) / 4, but `split_by_tokens` uses `content.chars().collect()` (character count). For multi-byte UTF-8 (CJK, emoji), a character can be 2-4 bytes, so char-based splitting produces chunks that exceed `max_tokens` by 2-4x.

Fix: switch to byte-based splitting with char-boundary snapping:

```rust
fn split_by_tokens(content: &str, max_tokens: usize, overlap_tokens: usize) -> Vec<String> {
    let bytes_per_chunk = max_tokens * 4;  // consistent with estimate_tokens
    let overlap_bytes = overlap_tokens * 4;

    if content.len() <= bytes_per_chunk {
        return vec![content.to_string()];
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut start = 0;

    while start < content.len() {
        let mut end = (start + bytes_per_chunk).min(content.len());
        // Snap to char boundary (don't split mid-character)
        while end < content.len() && !content.is_char_boundary(end) {
            end += 1;
        }
        chunks.push(content[start..end].to_string());

        if end >= content.len() {
            break;
        }

        let new_start = end.saturating_sub(overlap_bytes);
        // Snap to char boundary
        let mut new_start = new_start;
        while new_start > start && !content.is_char_boundary(new_start) {
            new_start += 1;
        }
        // Progress guarantee — advance by at least 1 byte
        start = new_start.max(start + 1);
        // Snap start to char boundary
        while start < content.len() && !content.is_char_boundary(start) {
            start += 1;
        }
    }

    if chunks.is_empty() {
        vec![content.to_string()]
    } else {
        chunks
    }
}
```

This is both a correctness fix (byte/char consistency) and a safety fix (progress guarantee). The chunks produced will satisfy `estimate_tokens(chunk) <= max_tokens` for any encoding.

### 3. `split_by_sections`: clamp overlap prefix

```rust
// Line 2784-2786, change:
let overlap_prefix = build_overlap_suffix(&current_chunk, overlap_tokens);
current_chunk = overlap_prefix;
current_tokens = estimate_tokens(&current_chunk);
// To:
let overlap_prefix = build_overlap_suffix(&current_chunk, overlap_tokens);
// Clamp: overlap must not exceed 25% of budget to guarantee forward progress.
// Truncate to budget instead of dropping entirely — preserves some context.
let clamped_tokens = estimate_tokens(&overlap_prefix);
if clamped_tokens > max_tokens / 4 {
    warn!(
        "[CHAIN] split_by_sections: overlap ({clamped_tokens} tokens) exceeds 25% of budget ({max_tokens}), truncating"
    );
    // Truncate to max_tokens/4 worth of bytes from the END of the overlap
    // (trailing content is more relevant for context continuity)
    let budget_bytes = (max_tokens / 4) * 4;
    let trim_start = overlap_prefix.len().saturating_sub(budget_bytes);
    // Snap to char boundary
    let trim_start = overlap_prefix.ceil_char_boundary(trim_start);
    current_chunk = overlap_prefix[trim_start..].to_string();
    current_tokens = estimate_tokens(&current_chunk);
} else {
    current_chunk = overlap_prefix;
    current_tokens = clamped_tokens;
}
```

This prevents the overlap from consuming most of the budget, which would cause the next section to immediately flush again.

### 4. `build_overlap_suffix`: cap output size

```rust
fn build_overlap_suffix(content: &str, overlap_tokens: usize) -> String {
    if overlap_tokens == 0 {
        return String::new();
    }
    let lines: Vec<&str> = content.lines().collect();
    let mut selected = Vec::new();
    let mut tokens = 0usize;

    for &line in lines.iter().rev() {
        let lt = estimate_tokens(line);
        if tokens + lt > overlap_tokens && !selected.is_empty() {
            break;
        }
        tokens += lt;
        selected.push(line);
        // FIX: If a single line already exceeds overlap budget, stop.
        // Don't accumulate more lines on top of an already-oversized line.
        if tokens > overlap_tokens {
            break;
        }
    }

    selected.reverse();
    selected.join("\n")
}
```

The existing code already handles the "first line exceeds budget" case by including it (good — we want at least some context). The added `break` prevents accumulating beyond one oversized line.

### 5. `split_chunk`: output validation

Add validation after the split, before returning:

```rust
fn split_chunk(
    item: &Value,
    max_tokens: usize,
    strategy: &str,
    overlap_tokens: usize,
) -> Vec<Value> {
    // ... existing code through line 2941 ...

    let sub_texts = match strategy {
        "lines" => split_by_lines(&text_content, max_tokens, overlap_tokens),
        "tokens" => split_by_tokens(&text_content, max_tokens, overlap_tokens),
        _ => split_by_sections(&text_content, max_tokens, overlap_tokens),
    };

    // FIX: Validate output — any sub-text still over budget gets re-split
    // via character splitting as the terminal guarantee.
    let sub_texts: Vec<String> = sub_texts.into_iter().flat_map(|chunk| {
        if estimate_tokens(&chunk) > max_tokens.saturating_mul(2) {
            // Still way over budget — force character split
            split_by_tokens(&chunk, max_tokens, overlap_tokens)
        } else {
            vec![chunk]
        }
    }).collect();

    // ... rest of function unchanged ...
}
```

The `max_tokens * 2` threshold allows small overages (a section that's 1.5x the budget is fine for the LLM — the context limit is usually much larger than `max_input_tokens`). Only re-splits truly oversized chunks that would cause API failures.

## Files Changed

| File | Change |
|---|---|
| `src-tauri/src/pyramid/chain_executor.rs` | Fix 5 functions: `split_by_lines` (progress guarantee + char fallback), `split_by_tokens` (progress guarantee), `split_by_sections` (clamp overlap), `build_overlap_suffix` (cap output), `split_chunk` (output validation) |

## What We Don't Change

- The split strategy selection (`sections`/`lines`/`tokens`) — chain YAML controls this
- The `max_input_tokens` and `split_overlap_tokens` chain config — user-controlled
- The `estimate_tokens` function (char_count / 4) — approximation is fine
- The dispatch path after splitting — already verified clean by telemetry

## Testing

After the fix, run the same build on `docstestapr14memoryleakhunting` with `WIRE_MEM_TELEMETRY=1`. Chunk 710 should:
1. Hit the split path (260K tokens > 80K limit)
2. `split_by_sections` should delegate the oversized section to `split_by_lines`
3. `split_by_lines` should detect the 250K-token line and delegate to `split_by_tokens`
4. `split_by_tokens` splits by character position — guaranteed termination
5. Result: ~4 sub-chunks of ~80K tokens each, total memory ~4 MB, no infinite loop
6. Telemetry should show heap_net flat through the split

The telemetry logging we added (`[MEM-WORK]` logs) can be left in for this verification run, then removed in a cleanup pass.
