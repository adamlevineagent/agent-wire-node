# Research Log

## Initialization
Branch: research/pyramid-quality-handoff / Type: thought / Parent: -
Hypothesis: A fresh series anchored to the handoff and cold start guide will produce cleaner evidence than extending the stale lab.
Changes: Archived the prior `.lab`, read the handoff, and re-read the Chain Developer Guide pillars, patterns, clustering guidance, and failure modes.
Result: Fresh research series initialized on `research/pyramid-quality-handoff`.
Duration: n/a
Status: thought
Insight: The handoff makes the near-term priorities explicit: validate the document schema fix, correct code merge/code clustering semantics, then prove the conversation path.

## THINK — before Experiment 0
Convergence signals: The handoff already points to three distinct baseline risks. Documents may still collapse to 1:1 threads despite the schema simplification. Code still exposes topic-level assignment schema in `code.yaml`, which teaches the wrong grouping behavior. Conversation has not been validated end-to-end.
Untested assumptions: I am assuming the current repository state is close enough to the handoff that fresh no-change builds will expose the true starting line. I have not yet confirmed whether existing built slugs reflect the current YAML/prompt surface, so baseline must use fresh artifacts.
Invalidation risk: Prior slugs and prior lab notes may have been built from earlier prompt states. Reusing them as evidence would blur the fresh series. The baseline must be created from current HEAD with no new repo-file changes.
Next hypothesis: Fresh baseline builds will show documents partially improved, code still structurally mis-taught by topic-level assignment schema, and conversation either untested or exhibiting the same grouping-pathology family.

## THINK — before Experiment 1
Convergence signals: Fresh document builds are failing before execution, so the immediate blocker is chain validation rather than clustering quality. The runtime log shows `document-default` fails validation because the `thread_clustering` container step lacks an `instruction`.
Untested assumptions: I have not yet tested whether the validator merely requires the field to exist or whether the container instruction is actually read during execution. The executor path suggests container instructions are unused and inner steps drive all work.
Invalidation risk: If the validator also rejects other container-like structures later, a single field addition may only clear the first gate. Still, this is the smallest faithful fix and keeps the sub-chain architecture intact.
Next hypothesis: Adding a benign metadata instruction to the document `thread_clustering` container will satisfy validation without changing runtime behavior, allowing the document baseline build to proceed to real clustering and synthesis.
