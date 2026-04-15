# Handback — Container Validator Is Architecturally Wrong

## Summary
The current Rust chain validator is architecturally wrong for the container/sub-chain model.

It treats every non-mechanical step as an LLM step and requires `instruction`, which breaks the intended `primitive: container` pattern even though container execution does not use `instruction` at runtime.

This should be fixed in Rust, not papered over in YAML.

## Evidence

### Runtime failure
Fresh document and code builds fail immediately with:

```text
chain "document-default" failed validation:
  step[2] "thread_clustering": LLM step must specify instruction
```

and similarly for `code-default`.

From `~/Library/Application Support/wire-node/wire-node.log`:

- `Build failed for 'handoff-doc-baseline0a': chain "document-default" failed validation:`
- `step[2] "thread_clustering": LLM step must specify instruction`
- `Build failed for 'handoff-code-baseline0a': chain "code-default" failed validation:`
- `step[2] "thread_clustering": LLM step must specify instruction`

### YAML shape
The current chains intentionally use:

- [chains/defaults/document.yaml](/Users/adamlevine/AI%20Project%20Files/agent-wire-node/chains/defaults/document.yaml)
- [chains/defaults/code.yaml](/Users/adamlevine/AI%20Project%20Files/agent-wire-node/chains/defaults/code.yaml)

Both define:

```yaml
- name: thread_clustering
  primitive: container
  steps:
    ...
```

That is the intended architecture: a container is a structural/orchestration node whose intelligence lives in its inner sub-steps.

### Executor behavior
The executor does not consume `step.instruction` for containers.

Relevant code:
- [src-tauri/src/pyramid/chain_executor.rs](/Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/pyramid/chain_executor.rs)

`execute_container_step(...)` reads:
- `step.steps`
- `step.for_each`
- `step.save_as`
- `step.depth`
- `step.node_id_pattern`

It does not use `step.instruction`.

### Validator behavior
The validator currently does this:

- [src-tauri/src/pyramid/chain_engine.rs](/Users/adamlevine/AI%20Project%20Files/agent-wire-node/src-tauri/src/pyramid/chain_engine.rs)

```rust
// LLM steps (non-mechanical) must have instruction
if !step.mechanical && step.instruction.is_none() {
    errors.push(format!("{}: LLM step must specify instruction", prefix));
}
```

That rule is too broad. It incorrectly classifies `container`, `loop`, `gate`, and similar orchestration primitives as LLM steps.

## Why This Is Architectural
This is not just a missing field.

The whole point of the system is:
- Rust is the dumb executor
- YAML expresses orchestration patterns
- Container/sub-chain composition is first-class

If Rust requires fake `instruction` fields on orchestration primitives that do not call the model, then Rust is imposing the wrong ontology on the chain language.

That makes the YAML surface less truthful and pushes users toward hacky metadata just to satisfy validation.

## Why A YAML-Only Workaround Is Wrong
I did test the hypothesis that adding a dummy instruction string would likely unblock validation.

That would be a workaround, but it is the wrong fix because:
- it encodes a lie in the chain definition
- it teaches future chain authors the wrong mental model
- it weakens the “schema/structure is the instruction” principle by adding meaningless fields
- it makes the container pattern feel second-class when it should be native

## Recommended Rust Fix
Update chain validation so `instruction` is required only for primitives that actually invoke the model.

Likely LLM primitives:
- `extract`
- `classify`
- `synthesize`
- `web`
- `compress`
- `fuse`

Likely non-LLM orchestration primitives:
- `container`
- `loop`
- `gate`
- `split`

The validator should branch on primitive semantics, not just `!step.mechanical`.

## Expected Outcome After Fix
Once Rust validation is corrected:
- `document.yaml` and `code.yaml` should validate without fake container instructions
- fresh builds should move past validation and reach real chain execution
- the container/sub-chain architecture remains honest and first-class

## Current Research State
- Fresh research branch exists: `research/pyramid-quality-handoff`
- Fresh `.lab` exists and is ready
- Mutation routes are restored and working
- The current blocker is now this validator mismatch, not the route layer

## Recommendation
Fix the validator in Rust first, then resume the YAML/prompt research pass.
