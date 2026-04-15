# Hotfix: Container step must bubble last inner step's output to outer scope

## The Bug
`thread_narrative` references `$thread_clustering.threads`. `thread_clustering` is a container step with inner steps `batch_cluster` → `merge_clusters`. The container's output should be `merge_clusters`'s output (the last inner step), stored under the container's name in the outer chain context.

Currently: the container runs its inner steps but doesn't store the final output under its own name in the outer `step_outputs`. So `$thread_clustering` is unresolved.

## The Fix
In `execute_container_step()`, after running all inner steps, take the last inner step's output and insert it into the outer context under the container step's name:

```rust
// After inner steps complete:
let last_step_name = &inner_steps.last().unwrap().name;
if let Some(output) = child_ctx.step_outputs.get(last_step_name) {
    ctx.step_outputs.insert(step.name.clone(), output.clone());
}
```

This is what the handoff spec says: "The container step's output is the last inner step's output."

## Error
```
Container step 'thread_narrative' could not resolve forEach ref '$thread_clustering.threads': Unresolved reference: $thread_clustering.threads
```
