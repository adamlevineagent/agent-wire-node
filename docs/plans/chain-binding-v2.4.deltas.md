# chain-binding-v2.3 → v2.4 — Simplification Deltas

> **Audit trail.** This document captures the deltas applied to `chain-binding-v2.3.md` after Round 2 of Stage 2 discovery audit (auditors E and F, blind, 2026-04-07) found 9 critical issues clustered around v2.3's *additive* architecture choices.
>
> **The new canonical plan is `chain-binding-v2.4.md`.** This file is the receipts.
>
> Audit reports: `/tmp/discovery-audit-E.md`, `/tmp/discovery-audit-F.md`.

## The pattern

Both v2.2 and v2.3 followed the same failure mode: each round of audit found new criticals, the plan added MORE architecture to address them, and the next round found MORE criticals in the new architecture. We're not converging by adding scope; we're diverging.

v2.4 reverses the direction: aggressive simplification, drop inventions that don't fit cleanly into the existing codebase, accept smaller wins.

## Critical findings from Round 2

### E-1 / F-1 / F-2 — `rust_intrinsic` chain mode is structurally underspecified
v2.3 invented a `Pipeline::RustIntrinsic` variant on `ChainDefinition` and a `target_function` field, plus a `conversation-legacy-chronological.yaml` to declare it. Verified against `chain_engine.rs:89-102`:
- `ChainDefinition` has NO `pipeline` field
- No `Pipeline` enum exists
- The proposed YAML omits 6 required fields (`schema_version`, `name`, `version`, `author`, `defaults`, `steps`)
- Adding a `pipeline:` field requires updating all 7 existing chain YAMLs (or making it `#[serde(default)]`)
- `validate_chain` likely rejects chains with `steps: []`

**v2.4 decision:** drop `rust_intrinsic` entirely. `conversation-legacy-chronological` becomes a magic string the dispatcher recognizes (two-line change in `build_runner.rs:237`). No new YAML. No new ChainDefinition fields. The wizard's chain selector enumerates a hardcoded list of "well-known chain ids" plus whatever's in `pyramid_chain_assignments`.

### E-2 — `ContentType::from_str` shim silently disables main.rs:3965 validation gate
v2.3 proposed `from_str(s) -> Some(Self::new(s))` as "back-compat." This makes `from_str` infallible — `main.rs:3965` `.ok_or_else` becomes dead code, any garbage string passes validation.

**v2.4 decision:** see E-7 / F-7 / F-8 — drop the full newtype refactor. Keep `ContentType` as an enum with a new `Other(String)` variant for free-string content_types. `from_str` stays strict for well-known values; an explicit `from_str_open` parses any string into `Other(s)` if it doesn't match well-known. main.rs:3965 keeps its validation gate intact.

### E-3 / F-6 — Phase 2.5.3 table-recreate loses triggers and accumulated columns
`pyramid_slugs` has three `AFTER DELETE ON pyramid_slugs` triggers (`db.rs:487-505`) the plan never mentions, AND has accumulated 5+ ALTER-added columns (`archived_at`, `updated_at`, `last_published_build_id`, pinning, WS-ONLINE-A..G) the plan's hand-written `CREATE TABLE pyramid_slugs_new (...)` would silently lose.

**v2.4 decision:** the migration uses `PRAGMA table_info(pyramid_slugs)` to introspect the current schema and build the new table dynamically. Triggers are explicitly enumerated and recreated. AND because `Other(String)` is an enum variant (not free string), we can KEEP the CHECK constraint and just expand it to allow `'other'` (or drop the CHECK entirely if introspection works cleanly).

Actually, simpler: with `Other(String)` we can keep the enum at the type level but still serialize to free strings at the DB layer (the variant carries a String, the discriminant disappears in serde). The CHECK still enforces `IN ('code', 'conversation', 'document', 'vine', 'question', 'other')`. To open the value space, we drop the CHECK with the proper migration. That part stays — but it's now optional, and we can ship `Other(String)` without dropping CHECK first if the migration turns out hairy.

### F-3 — `run_legacy_build` already has a working `ContentType::Conversation` arm
At `build_runner.rs:646-657`, `run_legacy_build` already dispatches `ContentType::Conversation` to `build::build_conversation`. v2.3's Phase 1.2 never acknowledged this; it would have created a parallel dispatch path.

**v2.4 decision:** the dispatch fix is just "make `run_legacy_build` reachable for Conversation when the chain assignment says so." That arm at `:647-657` is the actual implementation. Phase 1.2 reduces to: in `build_runner.rs:237`, when content_type is Conversation AND the assigned chain is `conversation-legacy-chronological`, fall through to the legacy path. ~5 lines.

Also: trace `write_tx` plumbing from `run_build_from` into `run_legacy_build` — F-3 notes the variable isn't in scope at the dispatch point. Sub-task: read the function's locals before writing the patch.

### F-4 — `stale_engine` has no `chain_id` or `content_type` in scope
v2.3 said "stale_engine reads the flag at the entry to its propagation loop." Verified: stale_engine.rs, stale_helpers.rs, stale_helpers_upper.rs, staleness.rs, staleness_bridge.rs all lack chain_id as a variable. Adding it requires plumbing chain_id and content_type through 5 entry points, loading ChainDefinition from disk (or a cache), and gating.

**v2.4 decision:** drop the full `supports_staleness` flag system. Replace with a simpler check: at the stale_engine entry point, look up the slug's chain assignment via `chain_registry::get_assignment(conn, slug)` (which IS in scope — conn is available). If the assignment is `Some(("conversation-legacy-chronological", _))`, log a warning and return early. No flag, no ChainDefinition load, no cache. One DB query, one string comparison.

This handles ONLY the chronological binding case, not arbitrary chains. Future chains that want to opt out of staleness add their own check in the same place. Not generic, but real.

### F-5 — Annotations archive wiring targets the wrong files
v2.3 said wire `archive_annotations_for_slug` into `build_runner.rs` and `vine.rs`. Actual `DELETE FROM pyramid_nodes` sites are at `db.rs:2009` (deprecated `delete_nodes_above`), `parity.rs:545`, plus CASCADE paths from `DELETE FROM pyramid_slugs` at `db.rs:1425, :1536`, plus the Phase 2.5.3 table-recreate CASCADE. The rebuild path uses something else (likely `supersede_nodes_above` or `INSERT OR REPLACE`).

**v2.4 decision:** Phase 1.0 starts with a discovery sub-task: locate every production pyramid_nodes row-removal mechanism. If the rebuild path uses `INSERT OR REPLACE` or supersession (not DELETE), then **annotations are not at risk** and Phase 1.0 collapses to "noop, verified." Plan should look at the code first and confirm. Drop the prebuilt archive table until we know it's actually needed.

### F-6 — `pyramid_slugs` table-recreate column drift (covered with E-3 above)

### F-7 / F-8 — Capability flags don't cover `slug.rs` validation or `vine.rs` dispatch
- `slug.rs:168-190`: file-vs-directory validation needs a `source_kind` flag the plan doesn't define
- `vine.rs:569-586`: dispatches to three different Rust build functions; free-string newtype loses compile-time exhaustiveness

**v2.4 decision:** with `ContentType::Other(String)` instead of full newtype, both sites keep compile-time exhaustiveness (well-known variants exhaustively matched, `Other(s)` arm handles open cases). slug.rs and vine.rs add an `Other(s) => { /* default behavior */ }` arm:
- slug.rs `Other(s)`: accept neither file nor directory; let the chain decide
- vine.rs `Other(s)`: error with "vine bunches don't support custom content_type yet" — same as the `Vine` and `Question` arms today

No capability flag system needed. Drop it.

## Major findings

### E-4 / F-9 — Phase 0.0 migration runner placement is ambiguous; legacy `let _ = ALTER` pattern conflicts
**v2.4 decision:** drop the migration runner. Use the existing `let _ = conn.execute("ALTER TABLE ...")` pattern with `PRAGMA table_info` pre-checks for ADD COLUMN cases. For destructive migrations (the pyramid_slugs CHECK drop, IF we still want it), wrap the specific block in an explicit `BEGIN; ...; COMMIT;` inside `init_pyramid_db`. No parallel migration system, no `pyramid_schema_version` table.

The cost: migrations are idempotent only via SQLite's "duplicate column" error swallowing, which is the existing convention. Acceptable for one-user dev.

### E-5 — Phase 1.2 sequenced into middle of Phase 2; Phase 1 done-criteria depend on it
**v2.4 decision:** renumber. Phase 1 contains only the work that doesn't depend on Phase 2 (annotations discovery, build_conversation audit, Pillar 37 sweep, prompt verification, consumer audit). The dispatch fix moves to Phase 2.4 (after the resolver lands). Phase 1's done criteria don't include the dispatch fix.

### E-6 — Migration 5 (content_hash backfill) contradicts NULL semantics
**v2.4 decision:** drop migration 5. Phase 3.4's "NULL means always-changed" is the only behavior. New chunks get content_hash on ingest; old chunks have NULL and get re-extracted on next re-ingest. Accept the wall-clock cost; the user will rebuild affected slugs deliberately.

### E-7 — `run_legacy_build` exhaustive match breaks under newtype
Resolved by E-2 / F-7 / F-8 simplification: keep enum, add `Other(String)` variant. `run_legacy_build` adds an `Other(s) => Err(...)` arm.

### F-11 — No "Run 4 reference rebuild" fixture in repo
**v2.4 decision:** drop "eyeball convergence" from done criteria. Replace with a mechanical check: "build any conversation pyramid with the chronological binding; verify L0 nodes have non-null distilled content and apex headline." Operator runs this manually. No committed fixture, no automated regression.

### F-12 — Tauri app has no CLI surface; --repair-chains needs new infrastructure
**v2.4 decision:** drop `--repair-chains`. Replace with a settings panel button: "Re-sync bundled chain files (overwrites local edits)." Wired to a Tauri IPC command. UI surface, no CLI parsing.

### F-13 — Pillar 37 sweep affects vine bunches through shared constants
**v2.4 decision:** acknowledged in Phase 0.5 with explicit "vine-affecting" tag in the commit message. Run a vine rebuild as part of Phase 0.5 verification. No split phases.

### F-14 — `default_chain_id` callers not enumerated
**v2.4 decision:** Phase 2.2 includes a grep + listing as a sub-task. Plan v2.4 just says "grep `default_chain_id` and update all callers" — the enumeration happens at implementation time.

## Architecture changes summary

| v2.3 invention | v2.4 replacement | Why |
|---|---|---|
| `Pipeline::RustIntrinsic` chain mode + new YAML + `target_function` field | Magic string in dispatcher, no YAML, no ChainDefinition changes | E-1, F-1, F-2 |
| `ContentType` newtype `pub struct ContentType(pub String);` with `#[serde(transparent)]` | Add `ContentType::Other(String)` enum variant; keep all existing variants | E-2, E-7, F-7, F-8 |
| Capability flags system (`wants_file_watcher`, etc.) on chain definition | Drop. `Other(s)` arms handle open cases per-site | F-7, F-8 |
| `pyramid_schema_version` table + migration runner | Existing `let _ = conn.execute("ALTER TABLE ...")` pattern + PRAGMA table_info pre-checks | E-4, F-9 |
| `archive_annotations_for_slug` + `pyramid_orphaned_annotations` table | Discover deletion sites first; if rebuild uses INSERT OR REPLACE / supersession, no archive needed | F-5 |
| `supports_staleness` flag on ChainDefinition + 5-file plumbing | Direct slug-assignment check at stale_engine entry point | F-4 |
| `include_dir!` + content-hash manifest + first-run reconciliation + `--repair-chains` CLI | Keep existing per-file `include_str!`, manually add the missing files; change `copy_dir_recursive` to skip overwrite; settings-panel button for repair | F-12 |
| Run-4 reference rebuild "eyeball convergence" | Manual smoke test: any conversation .jsonl, verify L0 populated | F-11 |
| Phase 1.4.1 chain registration | Drop entirely | E-1, F-1, F-2 |

## What survives unchanged from v2.3

- Phase 0.1 UTF-8 panic fix (with inline `char_indices` snippet)
- Phase 0.2 delete dead `instruction_map: content_type:` key
- Phase 0.3 delete `generate_extraction_schema`
- Phase 0.4 chunk_transcript regex tightening
- Phase 0.5 Pillar 37 sweep on `build.rs:90-300` (with vine-affecting tag)
- Phase 1.0 annotations *concern* (though implementation may collapse to no-op)
- Phase 1.3 build_conversation audit
- Phase 1.3.5 consumer audit (~15 files)
- Phase 1F frontend constants centralization + fallback objects
- Phase 2.1 `pyramid_chain_defaults` table
- Phase 2.2 `resolve_chain_for_slug` resolver + `default_chain_id` doc-comment update
- Phase 2.3 IPC commands `set_chain_default` / `get_chain_default` / `list_available_chains`
- Phase 2.4 dispatch fix in `build_runner.rs:237` (renumbered from 1.2)
- Phase 2.5 ContentType opening — but as `Other(String)` variant, not newtype
- Phase 2.5.3 `pyramid_slugs` CHECK drop migration — kept but uses PRAGMA table_info introspection and recreates triggers explicitly
- Phase 3.0 L0 parse site spike
- Phase 3.1 chunks temporal columns + content_hash
- Phase 3.2 Topic typed speaker/at fields
- Phase 3.3 `required_topic_fields` validator
- Phase 3.4 hash-mismatch invalidation (with NULL = always-changed)
- Phase 5 docs

## What v2.4 ADDS that v2.3 didn't have

- **`ContentType::Other(String)` enum variant** — preserves compile-time exhaustiveness AND opens the value space. The maximal solution that doesn't break the world.
- **Discovery sub-task in Phase 1.0**: locate actual `pyramid_nodes` row-removal mechanisms before deciding whether to add an archive
- **Settings-panel button for chain re-sync** instead of CLI flag
- **Phase 2.5.3 PRAGMA table_info introspection** for the table-recreate
- **Phase 2.5.3 explicit trigger enumeration**: the three `AFTER DELETE ON pyramid_slugs` triggers at `db.rs:487-505` are recreated against the new table
