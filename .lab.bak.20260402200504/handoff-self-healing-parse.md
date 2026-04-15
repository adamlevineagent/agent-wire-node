# Rust Handoff: Self-healing parse failures

## The Problem
When an LLM call produces malformed output (truncated JSON, markdown instead of JSON, missing fields), the executor retries the entire call from scratch — same input, same prompt, hoping for different output. This is mechanical and slow. A 27-second structured output call that produces 45K tokens of mostly-valid JSON gets thrown away, and the retry produces the same 45K tokens and truncates at the same point.

## The MPS
Use intelligence to fix what's broken instead of retrying blindly. When parsing fails, send the broken output to a small, fast LLM call that heals it.

## How it works

### Step 1: Parse fails
The executor receives a response that `extract_json()` can't parse. Instead of immediately retrying the original call:

### Step 2: Diagnose the failure type
- **Truncated JSON** — response starts with valid JSON but is missing closing brackets/braces. Most common with structured output hitting output cap.
- **Markdown wrapper** — response is valid JSON embedded in markdown prose. Model ignored "Output valid JSON only."
- **Partial structure** — JSON is valid but missing required fields from the schema.
- **Malformed values** — JSON structure is correct but values contain unescaped characters.

### Step 3: Send healing call
A small, fast LLM call receives the broken output and fixes it:

```
You received a malformed LLM response that should have been valid JSON matching this structure: [schema summary].

The response was: [broken output, first 4000 chars]
[if truncated: ...truncated at output cap. The response ends with: [last 500 chars]]

Fix this response:
- If truncated: complete the JSON structure. Close all open brackets and braces. If assignments are clearly missing, add them to "unassigned".
- If wrapped in markdown: extract the JSON object.
- If fields are missing: infer from context or use sensible defaults.

Output ONLY the fixed valid JSON.
```

This call is tiny — ~5K tokens input (broken response excerpt + instruction), ~500-2000 tokens output (just the fix). At Mercury 2's speed, it completes in under a second.

### Step 4: Parse the healed response
If the healing call produces valid JSON, use it. If the healing also fails, THEN fall back to retry.

## YAML surface

```yaml
- name: batch_cluster
  primitive: classify
  instruction: "$prompts/document/doc_cluster.md"
  on_error: "retry(3)"
  # Self-healing for parse failures
  on_parse_error: "heal"
  heal_instruction: "$prompts/shared/heal_json.md"
  heal_model_tier: mid
  heal_max_retries: 2    # try healing twice before falling back to full retry
```

### `on_parse_error` options
- `"heal"` — attempt LLM-based healing before retrying (default when `heal_instruction` is set)
- `"retry"` — current behavior, retry the full call (default when no `heal_instruction`)
- `"extract_partial"` — use whatever valid JSON was found, even if incomplete

### Interaction with `on_error`
`on_parse_error` handles JSON parse failures specifically. `on_error` handles all other failures (HTTP errors, timeouts, etc.). The flow:

```
LLM call → response received → parse attempt
  ├── parse succeeds → done
  └── parse fails → on_parse_error
        ├── "heal" → healing call → parse healed response
        │     ├── healed parse succeeds → done
        │     └── healed parse fails → retry original (on_error)
        ├── "retry" → retry original (on_error)
        └── "extract_partial" → use partial result → done (with warning)
```

## The healing prompt

`chains/prompts/shared/heal_json.md`:

```
You are fixing a malformed LLM response. The response should be valid JSON but isn't — it may be truncated, wrapped in markdown, or have structural errors.

You receive:
- The expected JSON structure (schema description)
- The broken response (or as much of it as fits)
- The type of failure detected

Your job: produce the FIXED valid JSON. Do not regenerate the content — fix what's there.

RULES:
- If truncated: close all open structures. Any items that were being listed when truncation occurred should be included up to the last complete item.
- If markdown: extract the JSON object from the prose.
- If missing fields: use empty arrays [] or empty strings "" for missing required fields.
- Preserve ALL data from the original response. Do not summarize or compress.

Output valid JSON only.
```

## Failure type detection

In `extract_json()`, before returning the error, classify the failure:

```rust
enum ParseFailureType {
    Truncated,       // Found opening { but no matching }
    MarkdownWrapped, // Response starts with # or ``` before JSON
    MalformedValue,  // serde error at specific line/col within valid structure
    NoJsonFound,     // No { or [ found at all
}
```

The failure type is passed to the healing call so the healing prompt knows what kind of fix is needed.

## New fields on ChainStep

```rust
#[serde(default)]
pub on_parse_error: Option<String>,      // "heal" | "retry" | "extract_partial"
#[serde(default)]
pub heal_instruction: Option<String>,     // prompt ref for healing
#[serde(default)]
pub heal_model_tier: Option<String>,      // model for healing calls
#[serde(default)]
pub heal_max_retries: Option<usize>,      // max healing attempts before full retry
```

## Implementation

### In chain_dispatch.rs, after `extract_json()` fails:

```rust
if let Some("heal") = step.on_parse_error.as_deref() {
    if let Some(ref heal_instruction) = step.heal_instruction {
        let failure_type = classify_parse_failure(&response);
        let heal_prompt = build_heal_prompt(
            heal_instruction,
            &response,
            failure_type,
            step.response_schema.as_ref(),
        );
        let healed = call_model(config, &heal_prompt.system, &heal_prompt.user, 0.1, 4096).await?;
        if let Ok(parsed) = extract_json(&healed) {
            return Ok(parsed);  // healing succeeded
        }
        // healing failed, fall through to normal retry
    }
}
```

## Why this is MPS

1. **Intelligence over mechanics** — uses LLM understanding to fix broken output instead of blindly retrying
2. **YAML-controlled** — healing strategy, prompt, model, retry count all in YAML
3. **Contribution-improvable** — an agent that discovers a better healing prompt submits it as a contribution
4. **Fast** — healing call is ~1s vs 27s for a full retry
5. **Composable** — works with any step type, any failure mode, any model

## Files
- `src-tauri/src/pyramid/chain_engine.rs` — new fields on ChainStep
- `src-tauri/src/pyramid/chain_dispatch.rs` — healing flow after parse failure
- `src-tauri/src/pyramid/llm.rs` — `classify_parse_failure()` function, `ParseFailureType` enum
- `chains/prompts/shared/heal_json.md` — healing prompt (new file)
