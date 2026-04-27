# Annotation 9 — parity: Pyramid protocol layer-by-layer structure preserved

```yaml
contribution_type: annotation
annotation_verb: positive-observation
target: 21-pyramid-protocol.md#layer-by-layer-protocol
body:
  axis: parity
  finding: >
    V2 preserves the layer-ordered pyramid protocol — scan bedrock files →
    extract L0 evidence → synthesize L1 from L0 clusters → synthesize L2 from L1
    threads → synthesize apex — with EvidenceDelta as the transactional output of
    each layer. This is the structural backbone of the pyramid build process.
    Agent-wire-node's build pipeline follows the identical layer ordering: L0
    extraction from source files, L1 clustering, L2 thread synthesis, apex.
    V2 re-expresses each layer as a chain invocation (bound via role-binding),
    but the layer semantics — bottom-up evidence flow, progressive depth
    construction, EvidenceDelta as transactional unit — are preserved identically.
  invariants:
    preserved:
      - "Layer ordering: L0→L1→L2→...→apex — bottom-up evidence flow; layers cannot be reordered without breaking progressive synthesis"
      - "EvidenceDelta as transactional output: each layer produces an EvidenceDelta containing verdicts + synthesis + gaps — this is the atomic unit that cross-slug evidence links reference"
      - "Progressive depth construction: L1 depends on L0 clusters, L2 depends on L1 threads — depth constraints are structural, not configurable"
      - "Cross-slug evidence links traverse pyramid boundaries at any depth: @{author}/{slug}/{version}/{depth}/{node_id} — preserves the vine/counter-pyramid composition primitive"
      - "Role dispatch at each layer: scan→extract (L0), synthesize (L1-L3+), evidence_test (verdict), cascade (staleness) — each layer has a bound role"
    would_break:
      - "Flattening layers (L0→apex directly) would remove progressive synthesis — intermediate L1/L2 nodes carry the load-bearing evidence distillation"
      - "Removing EvidenceDelta as transactional unit would break cross-slug atomicity — external pyramids would see partial state"
      - "Removing cross-slug evidence links would eliminate the vine composition and counter-pyramid primitives"
  evidence:
    v2_citation: "21-pyramid-protocol.md § Layer-by-layer protocol (lines 30-165); § EvidenceDelta (lines 40-54); § Cross-slug evidence links (lines 167-280)"
    legacy_citation: "agent-wire-node build pipeline: src-tauri/src/pyramid/build_runner.rs (build orchestration); src-tauri/src/pyramid/build.rs (legacy build dispatch); src-tauri/src/pyramid/cross_pyramid_router.rs (cross-slug routing)"
  vocab_ref: vocab/playful/vocabulary_entry/v1
  dict_ref: dict/playful/master/v1
  generalized_understanding: >
    The pyramid protocol is the strongest parity anchor in v2 because it expresses
    a structural invariant that is independent of implementation substrate —
    understanding builds bottom-up from evidence through progressive synthesis.
    V2 re-expresses the protocol as chain invocations with role-binding rather than
    hardcoded Rust pipelines, but the layer semantics are unchanged. The key shift
    is in dispatch flexibility: in agent-wire-node, layer operations are hardcoded
    (build.rs routes to run_legacy_build or run_decomposed_build based on content_type);
    in v2, each layer operation is a role whose handler chain can be superseded
    per-pyramid. The EvidenceDelta — containing verdicts, synthesis output, and gaps
    — is the load-bearing transactional primitive that makes cross-slug evidence
    atomic. V2's protocol spec (21) explicitly calls out the invariant: "Every protocol
    operation yields an EvidenceDelta that may reference any shape in its verdicts or
    gaps fields. Cross-slug evidence links reference shapes in other pyramids identically"
    (21 § Integration with epistemic shapes).
```

**Axis label:** parity
**V2 citation:** `21-pyramid-protocol.md` § Layer-by-layer protocol (lines 30–165) + § EvidenceDelta (lines 40–54) + § Cross-slug evidence (lines 167–280)
**Legacy citation:** `src-tauri/src/pyramid/build_runner.rs` + `build.rs` + `cross_pyramid_router.rs`
**Vocab ref:** `vocab/playful/vocabulary_entry/v1`
**Dict ref:** `dict/playful/master/v1`
