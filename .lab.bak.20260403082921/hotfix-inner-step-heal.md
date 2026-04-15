# Hotfix: Inner steps in containers don't get on_parse_error

## The Bug
`batch_cluster` has `on_parse_error: "heal"` and `heal_instruction` set in the YAML. But when it fails JSON parse, the error message is "structured output JSON parse failed" — the non-healing branch (line 182 in chain_dispatch.rs). This means `step.on_parse_error` is None despite being set in the YAML.

The step is an inner step inside a `container` step. The container's inner steps may not be getting `on_parse_error` and `heal_instruction` deserialized — either the fields aren't on the inner step struct, or the container execution doesn't pass them through.

## How to check
In `chain_dispatch.rs:174`, before the `if` check, log:
```rust
info!("[CHAIN] step '{}' parse failed, on_parse_error={:?}", step.name, step.on_parse_error);
```

If it logs `on_parse_error=None` for `batch_cluster`, the field isn't being deserialized for inner steps.

## Likely cause
Inner steps in a container are `ChainStep` objects parsed from the `steps` array in YAML. The `on_parse_error` and `heal_instruction` fields should be on `ChainStep` and serde should deserialize them. If they're not, check:
1. Are the fields actually on `ChainStep` in chain_engine.rs?
2. Is there a separate struct for inner steps that doesn't have these fields?
3. Is the container execution cloning/reconstructing inner steps and losing the fields?

## Fix
Ensure `on_parse_error` and `heal_instruction` are on `ChainStep` (not a separate struct) and that inner steps are deserialized with all fields intact.
