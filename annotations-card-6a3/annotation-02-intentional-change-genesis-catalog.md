# Annotation 2 — intentional-change: Genesis vocabulary catalog as shipped binary contributions

```yaml
contribution_type: annotation
annotation_verb: delta-finding
target: 30-genesis-vocabulary-catalog.md#index-by-entry-type-category
body:
  axis: intentional-change
  finding: >
    V2 ships ~145 vocabulary entries as genesis contributions embedded in the app binary,
    covering 14 entry_type categories. Each entry is a full contribution with handle-path
    (e.g., @genesis/vocabulary/annotation_verb/observation/1), body, and supersession
    semantics. Agent-wire-node extracts vocabulary dynamically from apex nodes via LLM
    (vocabulary.rs extract_vocabulary_catalog) — vocabulary is an inference artifact, not
    a shipped contribution. V2 makes the catalog a static, versioned, supersedable
    foundation that can evolve via Wire-level supersession without app-binary upgrades
    (except for meta-schemas which require GENESIS_ROOT_HASH bumps). This is intentional
    per spec 12: vocabulary entries are contributions, not extracted inference artifacts.
    The catalog covers vocabulary_entry_type (15 entries including self-describing root),
    meta_schema (9 entries pinned by hash), contribution_type (~16), event_type (~20),
    annotation_verb (~9), edge_type (~8), node_shape (5), purpose_template (6),
    evidence_verdict (3), role (~15), chain_primitive (~18), observe_selector (~14),
    permission_policy (~4), error_policy (4).
  evidence:
    v2_citation: "30-genesis-vocabulary-catalog.md § Purpose (lines 10-14); § Index by entry_type category (lines 18-34); § 1-14 full category enumerations (lines 39-509)"
    legacy_citation: "src-tauri/src/pyramid/vocabulary.rs extract_vocabulary_catalog (LLM-driven apex extraction, lines 25-56)"
  vocab_ref: vocab/playful/vocabulary_entry/v1
  dict_ref: dict/playful/master/v1
  generalized_understanding: >
    This is the vocabulary-as-contribution pattern applied to the substrate's own
    behavioral floor. In agent-wire-node, the vocabulary catalog is extracted from the
    apex of a built pyramid — it's a downstream artifact of synthesis, not an upstream
    contract. V2 inverts this: genesis vocabulary is the upstream contract that shapes
    what the substrate can express, and every entry is independently supersedable. The
    recursion bottoms at vocabulary_entry_type/vocabulary_entry_type/1 which has
    entry_type = vocabulary_entry_type (self-describing). This means the category system
    itself is extensible — adding a new category of vocabulary (e.g.,
    notification_channel) requires authoring a new vocabulary_entry_type entry first.
    The catalog is the "60% directional" floor; operators and agents supersede entries
    via Wire. The Phase 0 closure audit (80-kernel-closure-v1.md) verifies every
    hardcoded agent-wire-node enum maps to a genesis entry. Until that audit passes, the
    catalog's completeness is a starter inventory claim, not a verified closure.
```

**Axis label:** intentional-change
**V2 citation:** `30-genesis-vocabulary-catalog.md` § Purpose + Index by entry_type category (lines 10–34)
**Legacy citation:** `src-tauri/src/pyramid/vocabulary.rs` `extract_vocabulary_catalog` (LLM-driven, lines 25–56)
**Vocab ref:** `vocab/playful/vocabulary_entry/v1`
**Dict ref:** `dict/playful/master/v1`
