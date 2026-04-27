# Annotation 3 — intentional-change: Role-binding as supersedable vocabulary contribution

```yaml
contribution_type: annotation
annotation_verb: delta-finding
target: 31-genesis-role-binding.md#binding-model
body:
  axis: intentional-change
  finding: >
    V2 defines role→handler binding as a vocabulary contribution with supersession
    semantics, making role dispatch operator-configurable per-pyramid. Each binding maps
    a role handle-path (e.g., @genesis/vocabulary/role/judge/1) to a handler chain
    (e.g., @genesis/chains/judge_starter/1) with optional scope (pyramid | subtree:<id>
    | meta_layer:<name>). Agent-wire-node uses hardcoded role dispatch — the role_binding
    module (src-tauri/src/pyramid/role_binding.rs) was introduced in Phase 6c-D as a
    step away from enumerated GENESIS_BINDINGS, but the binding mechanism in agent-wire-node
    remains SQLite-table-based rather than a vocabulary contribution with supersession.
    V2's binding-as-contribution model means operators can publish custom bindings on
    Wire, supersede per-pyramid, and have binding changes propagate via normal
    supersession chain mechanics. Lookup is hierarchical: pyramid-level binding first;
    genesis fallback if missing. This is intentional per spec 13: "DADBEAR is a bound
    role — the on-startup event routing table is just role-binding."
  evidence:
    v2_citation: "31-genesis-role-binding.md § Binding model (lines 16-48); § Lookup and propagation (lines 100-140); § Genesis default bindings (lines 50-99)"
    legacy_citation: "src-tauri/src/pyramid/role_binding.rs (Phase 6c-D role binding table, lines 1-40); src-tauri/src/pyramid/dadbear_compiler.rs map_event_to_primitive (hardcoded dispatch)"
  vocab_ref: vocab/playful/vocabulary_entry/v1
  dict_ref: dict/playful/master/v1
  generalized_understanding: >
    Role-binding as contribution is the mechanism that makes v2's dispatch surface fully
    operator-configurable. In agent-wire-node, role dispatch is distributed across
    hardcoded match arms — map_event_to_primitive in dadbear_compiler.rs, the Phase 6c-D
    role_binding table, and various dispatch sites in chain_executor.rs. V2 collapses all
    of this into one mechanism: a role_binding contribution (conforming to
    @genesis/meta-schema/role_binding/1) that maps (role, scope) → handler chain.
    Genesis ships 15 default bindings at 60% correctness; operators supersede per-pyramid.
    The binding contribution's supersession chain provides full audit history of dispatch
    changes. The 60% posture notes acknowledge that genesis bindings are starter-set only
    — "Directionally correct, not optimal. Expect to supersede." The three resolution
    layers (pyramid → inherited → genesis) give operators progressive override granularity.
    Missing bindings fall back to genesis with an emitted error event (role_binding_missing).
```

**Axis label:** intentional-change
**V2 citation:** `31-genesis-role-binding.md` § Binding model (lines 16–48) + § Genesis default bindings (lines 50–99)
**Legacy citation:** `src-tauri/src/pyramid/role_binding.rs` (Phase 6c-D) + `src-tauri/src/pyramid/dadbear_compiler.rs` `map_event_to_primitive`
**Vocab ref:** `vocab/playful/vocabulary_entry/v1`
**Dict ref:** `dict/playful/master/v1`
