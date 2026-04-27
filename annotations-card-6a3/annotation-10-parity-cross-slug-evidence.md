# Annotation 10 — parity: Cross-slug evidence links and vine/counter-pyramid patterns preserved

```yaml
contribution_type: annotation
annotation_verb: positive-observation
target: 21-pyramid-protocol.md#cross-slug-evidence-links
body:
  axis: parity
  finding: >
    V2 preserves cross-slug citation mechanism identically: evidence links traverse
    pyramid boundaries via full handle-paths (@{author}/{slug}/{version}/{depth}/{node_id}).
    This is the "crown jewel" called out in 00-plan.md — the composition primitive
    that enables vines (composing understanding across source pyramids) and
    counter-pyramids (contesting claims via cross-slug evidence). Agent-wire-node's
    cross_pyramid_router.rs implements the same cross-slug dispatch pattern. V2
    re-expresses cross-slug dispatch as a bound role (cross_slug_dispatcher →
    @genesis/chains/cross_slug_dispatcher_starter/1) rather than a Rust module, but
    the mechanism — resolve handle-path across slug boundary, route evidence, handle
    staleness propagation — is preserved. The version component in cross-slug
    references ensures citations remain resolvable even as source pyramids evolve
    through publications.
  invariants:
    preserved:
      - "Full handle-path format for cross-slug references: @{author}/{slug}/{version}/{depth}/{node_id} — version anchoring prevents citation ambiguity across publications"
      - "Cross-slug evidence links use same EvidenceDelta schema as local evidence — no separate cross-pyramid schema"
      - "Vine composition: a vine pyramid composes understanding from N source pyramids via cross-slug evidence links at any depth"
      - "Counter-pyramid pattern: a pyramid contests a claim in another pyramid via cross-slug evidence links pointing at the contested node"
      - "Staleness propagation across slug boundaries: when a bedrock pyramid's evidence changes, cascade_stale events propagate to vines that cite the changed evidence"
    would_break:
      - "Removing version from cross-slug handle-paths would make citations ambiguous across source pyramid publications"
      - "Separating cross-slug evidence schema from local EvidenceDelta would double the evidence dispatch surface"
      - "Removing cross-slug staleness propagation would freeze vine compositions at publish-time state — no live updates"
  evidence:
    v2_citation: "21-pyramid-protocol.md § 6 Cross-slug evidence links and vines (lines 167-280); 17-identity-rename-move-portability.md § Cross-pyramid handle-path stability (lines 149-160)"
    legacy_citation: "src-tauri/src/pyramid/cross_pyramid_router.rs (cross-slug routing); src-tauri/src/pyramid/vine_composition.rs (vine composition propagation); 00-plan.md (calls cross-slug the 'crown jewel')"
  vocab_ref: vocab/playful/vocabulary_entry/v1
  dict_ref: dict/playful/master/v1
  generalized_understanding: >
    Cross-slug evidence is the composition primitive that makes the pyramid system
    a network rather than isolated silos. V2 preserves this mechanism identically
    because it is the structural invariant that enables multi-pyramid understanding:
    a vine pyramid can ask questions whose answers depend on evidence distributed
    across multiple source pyramids; a counter-pyramid can contest a specific claim
    in another pyramid by citing the contested node via cross-slug evidence.
    The key preservation points: (a) version-anchored handle-paths ensure that
    citations resolve to specific publication states — @alice/A/v3/0/L0-0042 is a
    historical fact; (b) cross-slug evidence uses the same EvidenceDelta schema as
    local evidence — no schema bifurcation; (c) staleness propagates across slug
    boundaries via cascade_stale events — when bedrock evidence changes, vines
    are notified. V2's shift is in expressing cross-slug dispatch as a bound role
    (cross_slug_dispatcher → chain) rather than a hardcoded Rust module, making it
    operator-configurable per-pyramid. The 00-plan.md explicitly calls cross-slug
    the "crown jewel" that must survive the v2 transformation intact.
```

**Axis label:** parity
**V2 citation:** `21-pyramid-protocol.md` § 6 Cross-slug evidence links (lines 167–280) + `17-identity-rename-move-portability.md` § Cross-pyramid handle-path stability (lines 149–160)
**Legacy citation:** `src-tauri/src/pyramid/cross_pyramid_router.rs` + `vine_composition.rs`
**Vocab ref:** `vocab/playful/vocabulary_entry/v1`
**Dict ref:** `dict/playful/master/v1`
