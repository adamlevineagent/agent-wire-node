# Handoff: Pyramid Folio Generator

> **Canonical spec:** [`docs/plans/pyramid-folio-generator.md`](../plans/pyramid-folio-generator.md)
>
> This handoff is a pointer. The full refinement cycle, MPS audit, architecture, and punch list live in the plan above.

## TL;DR

Build a `folio` command (CLI + Tauri/API export surface) that flattens a Knowledge Pyramid into a structured, human-readable markdown document.

Key features:
- **`--depth N`** — control traversal depth from apex down
- **`--overlay <slug>`** (repeatable) — filter through question pyramid lenses
- **`--overlay-mode union|intersect`** — compose multiple overlays
- **Handle path provenance** — every section carries `slug/depth/node_id · vN · timestamp`
- **Topic sub-sections** — render `topics[]` as sub-headings, not just `distilled` dumps
- **Web edge cross-refs** — "See also" links using handlepaths
- **Appendices** — Glossary, Errata, Decision Log

## Implementation Location

- Core engine: `mcp-server/src/folio/engine.ts` (or `src-tauri/src/pyramid/folio.rs`)
- Renderer: `mcp-server/src/folio/renderer.ts`
- CLI command: add `folio` to the CLI command registry
- Tauri surface: expose as `export_pyramid` / `pyramid_folio`

## Key Discovery

`build_version` already exists in the node schema (auto-increments on upsert, `db.rs:1761`). Handle paths (`slug/depth/node_id`) are the native addressing scheme. **No schema migration needed.**

## After Implementation

Update the `pyramid-knowledge` skill at `/Users/adamlevine/.claude/skills/pyramid-knowledge/SKILL.md` to document the new `folio` command.

## Full Spec

See [`docs/plans/pyramid-folio-generator.md`](../plans/pyramid-folio-generator.md) for:
- 20-idea diverge table
- Architecture diagram
- Document structure spec
- MPS audit with 3-item punch list
- Verification plan (9 automated tests)
