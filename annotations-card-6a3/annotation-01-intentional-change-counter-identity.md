# Annotation 1 — intentional-change: Counter-based node_id replaces UUIDs

```yaml
contribution_type: annotation
annotation_verb: delta-finding
target: 17-identity-rename-move-portability.md#the-identity-model
body:
  axis: intentional-change
  finding: >
    V2 replaces UUID-based internal identity with type-prefixed monotonic counters
    allocated from a per-pyramid counter manifest contribution. Handle-paths remain the
    external identifier; the internal primary key changes from UUID to counter. This is
    a deliberate architectural pivot pinned by Adam's explicit directive ("We never use
    UUIDs, we always use handlepaths"). Agent-wire-node uses UUIDs throughout its SQLite
    schema (pyramid_nodes.id TEXT with UUID values, all FK references UUID-based).
    V2's counter allocation is O(1) per allocation via pyramid lock; counters are scoped
    per-type (L0, L1, L2, F, meta, debate, gap, ann, evt, err) within a pyramid
    namespace. The transition preserves handle-path as the stable external reference
    while making internal identity deterministic, monotonic, and human-readable.
  evidence:
    v2_citation: "17-identity-rename-move-portability.md § The identity model (lines 29-104); § node_id is a canonical counter (lines 43-58); § Invariants #1-6 (lines 236-244)"
    legacy_citation: "src-tauri/src/pyramid/db.rs pyramid_nodes schema (TEXT id column with UUID values); src-tauri/src/pyramid/types.rs node identity types"
  vocab_ref: vocab/playful/vocabulary_entry/v1
  dict_ref: dict/playful/master/v1
  generalized_understanding: >
    This is the most fundamental architectural change in v2. In agent-wire-node, every
    pyramid node carries a UUID — creation order is non-deterministic, identity is
    opaque, and cross-references require UUID resolution. V2 replaces this with
    type-prefixed monotonic counters (L0-0001, F-0007, debate-0003) allocated from a
    counter manifest that is itself a contribution with supersession semantics. The
    counter system carries several load-bearing properties: (a) monotonic allocation
    guarantees temporal ordering within type; (b) type prefixes make node identity
    self-describing; (c) counter manifest as contribution means allocation policy is
    observable and supersedable; (d) no UUIDs in storage means .understanding/ folders
    are fully inspectable with grep. The pyramid lock serializes allocation at sub-ms
    latency. Legacy UUID-bearing code across db.rs, types.rs, and all FK references in
    the SQLite schema must be re-expressed as counter-based identity. The identity shift
    cascades through every contribution type, every evidence link, every cross-slug
    reference, and every Wire publication — node_id replaces UUID as the stable anchor
    across Wire versions (spec 62).
```

**Axis label:** intentional-change
**V2 citation:** `17-identity-rename-move-portability.md` § The identity model + § node_id is a canonical counter (lines 29–104)
**Legacy citation:** `src-tauri/src/pyramid/db.rs` pyramid_nodes table (TEXT id with UUID values)
**Vocab ref:** `vocab/playful/vocabulary_entry/v1`
**Dict ref:** `dict/playful/master/v1`
