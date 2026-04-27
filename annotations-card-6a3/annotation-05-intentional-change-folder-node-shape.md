# Annotation 5 — intentional-change: folder_node as 5th explicit shape living inside folder

```yaml
contribution_type: annotation
annotation_verb: delta-finding
target: 22-epistemic-state-node-shapes.md#5-folder-node
body:
  axis: intentional-change
  finding: >
    V2 elevates folder_node to a first-class epistemic node shape (alongside scaffolding,
    debate, gap, meta-layer) with full handle-path identity, supersession semantics, and
    a physical location inside the folder it describes
    (<folder>/.understanding/folder_node/current.md). Agent-wire-node stores folder
    metadata in SQLite tables (pyramid_slugs, pyramid_batches) and scanner code
    (folder_ingestion.rs) — folder structure is operational state, not a contribution
    with identity. V2's design makes folder_node the "central artifact of folder-scope
    ingestion" (20 § folder_node as the canonical mapping layer) — it aggregates
    files + subfolders + coverage stats for a specific folder, carries handle-path
    (F-0007), and survives folder moves via supersession. The physical consequence:
    moving a folder on disk automatically carries its folder_node + all child shells
    + annotations + local evidence — one filesystem `mv` + one folder_node supersession.
    This is intentional per the PUNCHLIST §A rewrite of spec 20.
  evidence:
    v2_citation: "22-epistemic-state-node-shapes.md § 5. Folder node (lines 394-494); 20-understanding-folder-layout.md § folder_node as the canonical mapping layer (lines 26-30); 17-identity-rename-move-portability.md § Folder move is a filesystem mv (lines 134-145)"
    legacy_citation: "src-tauri/src/pyramid/db.rs pyramid_slugs + pyramid_batches tables; src-tauri/src/pyramid/folder_ingestion.rs (scanner-based folder metadata)"
  vocab_ref: vocab/playful/vocabulary_entry/v1
  dict_ref: dict/playful/master/v1
  generalized_understanding: >
    folder_node as a first-class shape is the mechanism that makes v2's self-describing
    filesystem promise work for folder operations. Agent-wire-node's folder handling is
    SQLite-centric — folder state lives in pyramid_slugs rows with source_path, and
    folder ingestion runs as scanner code that writes to operational tables. V2 makes
    folder_node a contribution that lives physically inside the folder, which means:
    (a) folder portability is one filesystem mv — everything moves with the folder;
    (b) folder identity survives rename via supersession (same F-0007 handle-path);
    (c) folder_node's body carries filemap, coverage stats, child folders, tombstones,
    and inheritance defaults — all as contribution fields; (d) the distinction from
    scaffolding is explicit: folder_node aggregates files and subfolders mechanically
    (scanner), scaffolding synthesizes evidence (synthesizer). The 60% posture notes
    (20 §60% posture) acknowledge that progressive supersession of folder_node during
    scan is "directionally correct but not optimal" — rescan supersessions are correct
    but could be coalesced.
```

**Axis label:** intentional-change
**V2 citation:** `22-epistemic-state-node-shapes.md` § 5 (lines 394–494) + `20-understanding-folder-layout.md` § folder_node (lines 26–30) + `17-identity-rename-move-portability.md` § Folder move (lines 134–145)
**Legacy citation:** `src-tauri/src/pyramid/db.rs` pyramid_slugs table + `src-tauri/src/pyramid/folder_ingestion.rs`
**Vocab ref:** `vocab/playful/vocabulary_entry/v1`
**Dict ref:** `dict/playful/master/v1`
