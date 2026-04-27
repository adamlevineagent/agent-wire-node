# Annotation 7 — net-new: debate_node as explicit first-class epistemic shape

```yaml
contribution_type: annotation
annotation_verb: delta-finding
target: 22-epistemic-state-node-shapes.md#1-debate-node
body:
  axis: net-new
  finding: >
    V2 defines debate_node as a named first-class epistemic shape with structured
    positions, steel-mans, red-teams, vote counts, and explicit lifecycle (created →
    active → collapsed_to_settled / collapsed_to_gap). Agent-wire-node has debate
    implicit — annotations and evidence links can surface disagreement, and the
    reconciler role (reconciler_starter chain) detects silent multiplicity — but
    there is no dedicated debate node type, no debate-specific UI rendering, and no
    explicit debate lifecycle with state transitions. V2's debate_node makes
    multiplicity visible and operable: each debate has a concern (what's contested),
    positions (who claims what with evidence), steel-mans (strongest version of each
    position), red-teams (attacks on each position), and vote counts (lightweight
    alignment signals). The reconciler detects when annotations with different
    positions share the same concern and spawns a debate; the debate_steward manages
    its lifecycle; the judge decides collapse. This is genuinely new substrate —
    agent-wire-node has the conceptual pieces scattered across annotations and
    evidence but never as a first-class epistemic shape.
  evidence:
    v2_citation: "22-epistemic-state-node-shapes.md § 1. Debate node (lines 38-219); § State transitions: Scaffolding→Debate (lines 499-511); § Debate→Scaffolding (lines 524-533); § Debate→Gap (lines 534-543)"
    legacy_citation: "N/A — implicit in agent-wire-node annotations/evidence; no dedicated node type; src-tauri/src/pyramid/reconciler (conceptual only, not a first-class shape)"
  vocab_ref: vocab/playful/vocabulary_entry/v1
  dict_ref: dict/playful/master/v1
  generalized_understanding: >
    debate_node is v2's mechanism for making "silent multiplicity" — the case where
    different agents or operators hold incompatible beliefs about the same concern —
    visible, structured, and resolvable. agent-wire-node's approach is implicit:
    annotations can correct or contradict each other, evidence links can support or
    contradict claims, but there's no container that names the debate, frames the
    positions, and manages the lifecycle. V2's debate_node shape carries:
    (a) concern — what's actually contested; (b) positions — who claims what, each
    with evidence references; (c) steel-mans — the strongest charitable version of
    each position, produced by debate_steward; (d) red-teams — attacks on each
    position to test strength; (e) vote counts — lightweight alignment signals from
    agents/operators; (f) status lifecycle — created → active → collapsed_to_settled
    (evidence converges) or collapsed_to_gap (evidence vacuum). The state transitions
    (22 § State transitions) describe the full graph: Scaffolding→Debate (multiplicity
    named), Debate→Scaffolding (evidence convergence), Debate→Gap (evidence vacuum),
    Gap→Scaffolding (evidence arrival). The 60% posture notes acknowledge that debate
    steward config (soft cap of 10 positions before splitting into sub-debates) is a
    starter heuristic.
```

**Axis label:** net-new
**V2 citation:** `22-epistemic-state-node-shapes.md` § 1 Debate node (lines 38–219) + § State transitions (lines 497–561)
**Legacy citation:** N/A — implicit in agent-wire-node; no dedicated node type
**Vocab ref:** `vocab/playful/vocabulary_entry/v1`
**Dict ref:** `dict/playful/master/v1`
