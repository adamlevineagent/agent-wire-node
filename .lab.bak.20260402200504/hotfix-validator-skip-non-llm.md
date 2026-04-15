# Hotfix: Validator should skip instruction check for non-LLM primitives

`chain_engine.rs` validation — the `instruction` requirement applies to primitives that make LLM calls. Container, loop, gate, and split primitives don't call the LLM — they're flow control. The validator should skip the instruction check for these.

```rust
// In validate_chain_definition(), the instruction check:
const NON_LLM_PRIMITIVES: &[&str] = &["container", "loop", "gate", "split"];

if !NON_LLM_PRIMITIVES.contains(&step.primitive.as_str()) && step.instruction.is_none() {
    errors.push(format!("{}: LLM step must specify instruction", prefix));
}
```

Same for any other validation that only applies to LLM-calling steps (model_tier checks, response_schema checks, etc.).
