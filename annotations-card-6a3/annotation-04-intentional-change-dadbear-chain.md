# Annotation 4 — intentional-change: DADBEAR re-expressed as YAML cascade-handler chain

```yaml
contribution_type: annotation
annotation_verb: delta-finding
target: 32-cascade-handler-starter.md#chain-body
body:
  axis: intentional-change
  finding: >
    V2 defines cascade handling (DADBEAR staleness propagation) as a YAML chain spec with
    8 named steps invoking compute and think primitives, rather than Rust code. The chain
    body (§ Chain body, lines 122-228) iterates over parent nodes of a stale child, calls
    judge for each parent, and conditionally emits rewrite or skip outputs. Agent-wire-node
    implements DADBEAR as Rust code across multiple modules: dadbear_compiler.rs (work item
    compilation, observation grouping), dadbear_supervisor.rs (work item execution dispatch),
    stale_engine.rs (debounce timers, WAL drain), stale_helpers.rs (L0 stale-check dispatch),
    and staleness_bridge.rs (bridging two staleness systems). V2 re-expresses the same
    algorithm — detect staleness → judge materiality → cascade upward — as an observable,
    supersedable chain of named primitives. This is intentional per spec 12: chain
    definitions are contributions, and the cascade handler is just another bound role
    whose behavior can be superseded per-pyramid.
  evidence:
    v2_citation: "32-cascade-handler-starter.md § Chain body (lines 122-228); § Contract input/output (lines 23-67); § Phase 0 closure (lines 396-400)"
    legacy_citation: "src-tauri/src/pyramid/dadbear_compiler.rs (work item compilation, ~1700 lines); src-tauri/src/pyramid/dadbear_supervisor.rs (~3000 lines); src-tauri/src/pyramid/stale_engine.rs (debounce + WAL drain); src-tauri/src/pyramid/stale_helpers.rs (L0 stale checks)"
  vocab_ref: vocab/playful/vocabulary_entry/v1
  dict_ref: dict/playful/master/v1
  generalized_understanding: >
    The DADBEAR→chain migration is one of the highest-value transformations in v2
    because it replaces the most complex Rust subsystem with a readable, supersedable
    chain definition. Agent-wire-node's DADBEAR spans five Rust modules with deep
    coupling — observation events flow through dadbear_compiler into work items, which
    dadbear_supervisor dispatches, while stale_engine manages debounce timers and
    stale_helpers dispatches L0 checks. V2's cascade-handler chain (32) expresses the
    same logic in 8 steps: (1) observe the stale child, (2) lookup parents, (3) for
    each parent, get last-seen evidence snapshot, (4) diff evidence, (5) call judge,
    (6) if materially_changed → cascade_stale to parent, (7) optionally trigger rewrite,
    (8) emit cascade_completed. Each step is a named primitive invocation. The 60%
    posture notes acknowledge that diamond handling (node with multiple ancestors sharing
    a further ancestor) has correct but suboptimal behavior (redundant cascade_stale
    events). Operators can supersede the chain for domain-specific cascade behavior.
    Phase 0 closure verification — mapping every agent-wire-node staleness-handler code
    path — is deferred to the kernel-closure audit (80).
```

**Axis label:** intentional-change
**V2 citation:** `32-cascade-handler-starter.md` § Chain body (lines 122–228) + § Contract (lines 23–67)
**Legacy citation:** `src-tauri/src/pyramid/dadbear_compiler.rs` + `dadbear_supervisor.rs` + `stale_engine.rs` + `stale_helpers.rs`
**Vocab ref:** `vocab/playful/vocabulary_entry/v1`
**Dict ref:** `dict/playful/master/v1`
