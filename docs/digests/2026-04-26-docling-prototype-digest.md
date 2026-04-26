# Docling Prototype Digest — 2026-04-26

Study of `/Users/adamlevine/AI Project Files/agent-wire-docling/` for SDFS Phase 1 (Scan-Driven Filesystem initiative). Prototype status: working, end-to-end validated on 1000+ document corpora.

---

## 1. What it is

The docling prototype (`agent-wire-docling`) is a **local web app** that converts folders of heterogeneous documents (PDF, DOCX, XLSX, PPTX, HTML, plain text, LaTeX, Markdown) into a mirrored tree of clean Markdown + JSON files. It wraps IBM's [Docling](https://github.com/docling-project/docling) (MIT-licensed, CV-model-based document conversion library) behind a Python FastAPI backend, with a Next.js frontend for human review. The core interaction is Scan → Preview (optional taste-test) → Batch Convert → Triage failures. Nothing is uploaded; everything runs locally. It was built as a single-session prototype to prove the conversion pipeline and the `.understanding/folder.yaml` filemap concept from the self-describing-filesystem vision. Its output is a plain folder of files consumable by Wire Node's pyramid build with no shared schema, no shared database, no live integration.

## 2. Architecture shape

- **Languages**: Python 3.11 (backend) + TypeScript/Next.js 15/React 19 (frontend)
- **Key backend modules**:
  - `backend/stratification/` — folder walk (`scanner.py`), format detection via magic bytes + extension map, cheap PDF probes (page count, native vs scanned), per-folder filemap emission (`filemap.py`)
  - `backend/conversion/` — Docling wrapper (`converter.py`), pipeline parameter normalization + hashing (`pipeline.py`), thin async adapter (`convert.py`)
  - `backend/jobs/` — batch runner (`batch.py`), taste session store, triage rollup (`triage.py`)
  - `backend/manifest.py` — dual-write `manifest.json` + `manifest.yaml` index
  - `backend/cli.py` — `awd` CLI (hits the HTTP surface)
- **Key frontend modules**:
  - `components/shell/` — app chrome, sidebar, scan view, folder picker
  - `components/VizDiff/` — two-pane source↔markdown reviewer with confidence gutter
  - `components/TasteTest/` — per-stratum sampling, approval, pipeline tuning
  - `components/BatchRun/` — live progress, post-run triage UI
  - `components/Renderers/` — per-format source renderers (PDF via pdf.js, DOCX, XLSX, PPTX, HTML, text)
- **Contracts** (`contracts/`): OpenAPI spec, SQLite schema, TypeScript interfaces (DoclingDocument subset, Anchor shape, shortcut scopes)
- **Scan/Curate/Build split**: Present.
  - **Scan**: `scanner.py` walks folder, computes SHA-256 per file, detects formats, probes PDFs, assigns strata, emits `.understanding/folder.yaml` per folder
  - **Curate** (optional): Taste-test UI lets the human sample, eyeball, approve/reject per stratum, tune pipeline knobs. Writes decisions to both `taste_sessions` SQLite table and (recently) dual-writes to `folder.yaml`.
  - **Build**: `batch.py` walks filemaps, converts all `user_included=true` (or scanner-suggested includes), writes mirrored output tree, produces `manifest.yaml` + `triage.yaml`
- **Per-folder filemap concept**: `.understanding/folder.yaml` — one YAML file per folder in the source tree. Contains per-file scanner-owned fields (sha256, size, mtime, detected_content_type, detected_stratum, scanner_suggestion, exclusion_reason), user-owned fields (user_included, user_content_type, user_notes), and post-build fields (last_build_at, last_build_pipeline_hash, last_build_output_path, last_build_error). Merge semantics preserve user fields across rescans. Files deleted from disk move to a `deleted:` tombstone list. Fully implemented and tested in `backend/backend/stratification/filemap.py`.

## 3. Reusable

These are concrete artifacts SDFS Phase 1 should adopt outright, with file paths:

### Data shapes

- **`.understanding/folder.yaml` schema** — `backend/backend/stratification/filemap.py` (lines 21–50: `SCANNER_FIELDS`, `USER_FIELDS`, `POST_BUILD_FIELDS`; lines 92–109: `_blank_entry` template). Full schema documented in `docs/filemap-model.md` (lines 22–68). This is the canonical checklist-per-folder shape.
- **`triage.yaml` schema** — `docs/filemap-model.md` (lines 127–170) + implementation at `backend/backend/jobs/triage.py` (lines 43–104). Batch failure rollup with by_reason/by_content_type aggregation, per-failure retry hooks (`retry_with_pipeline`, `mark_as_excluded`, `notes`), and error classification (`convert_422`, `ocr_timeout`, `parse_error`, `unknown`).
- **`PipelineParams` Pydantic model** — `backend/backend/conversion/pipeline.py` (lines 14–42). Clean, validated shape for OCR/VLM/Tables/Enrichments configuration. Stable SHA-256 hashing for dedup (`pipeline_hash`, lines 52–60).
- **`Anchor` / `DoclingDocument` TypeScript interfaces** — `contracts/docling-types.ts`. Narrow, frontend-only subset of Docling's output. Isolates consumers from upstream Docling schema drift.
- **DB schema** — `contracts/db-schema.sql`. Well-documented SQLite DDL with conventions (TEXT for hashes/timestamps, JSON stored as TEXT, WAL mode).

### Parsing logic & heuristics

- **Format detection** — `backend/backend/stratification/scanner.py` (lines 21–32: `_EXT_MAP`; lines 36–40: `_MAGIC_SIGNATURES`; lines 42–62: `_TIER3_EXTS`; lines 65–88: `detect_format()`). Uses magic bytes for PDFs (read first 8 bytes, check `%PDF-`), extension fallback for OOXML/HTML/text. Tier-3 extension exclusion (audio, standalone images, XBRL, JATS). Small, pure functions with no mmap dependency.
- **PDF stratification** — `scanner.py` (lines 174–208). Distinguishes native-text PDFs from scanned PDFs via `pdftotext` byte count per page, bins by page count (1-10, 11-50, 51-200, 201+). Graceful degradation when poppler-utils is missing.
- **Cheap PDF probes** — `scanner.py` (lines 96–168). `pdfinfo` for page count, `pdftotext` for text bytes, `file --mime-type` for MIME. Wrapped in 30s timeouts with subprocess error handling.
- **File entry normalization** — `filemap.py` lines 92–117. `_blank_entry()` + `_normalize_entry()` ensure every filemap entry has all keys filled regardless of input.
- **Merge semantics** — `filemap.py` lines 120–188. `merge_filemap()`: scanner fields rewritten, user fields preserved, post-build fields preserved, new files added, missing files tombstones to `deleted:`. Exactly the behavior SDFS Phase 1 needs for rescan safety.
- **Excluded directory list** — `scanner.py` lines 272–293. Comprehensive list of directories skipped during scan (`.understanding`, `.docling-out`, `.git`, `.venv`, `node_modules`, `__pycache__`, etc.). Should be adopted as-is.

### Contracts

- **HTTP surface for filemap operations** — `backend/backend/stratification/router.py` (lines 398–437):
  - `GET /filemap?folder=<path>` → filemap YAML parsed as JSON
  - `PATCH /filemap?folder=<path>` → merge user-owned fields (body: `{ files: [{ path, user_included?, user_content_type?, user_notes? }] }`)
  - `GET /filetree?root=<path>` → recursive folder tree with coverage counts
- **Atomic write pattern** — `filemap.py` lines 76–89. `tmp + os.fsync + os.replace`. Used consistently in filemap, manifest, and triage writers.

### What to adopt outright

1. `backend/backend/stratification/filemap.py` — the entire module. It's a standalone, tested (see `backend/tests/test_filemap.py`), well-documented implementation of the `.understanding/folder.yaml` concept.
2. `backend/backend/stratification/scanner.py` — format detection, cheap probes, and stratum assignment functions. Pure functions with no FastAPI coupling.
3. `backend/backend/conversion/pipeline.py` — PipelineParams model + `pipeline_hash()`.
4. `backend/backend/jobs/triage.py` — triage rollup schema and error classification.
5. `contracts/docling-types.ts` — isolated Docling surface types.
6. `docs/filemap-model.md` — the authoritative spec document for the filemap concept.

## 4. Avoid

Anti-patterns and design choices SDFS Phase 1 should NOT carry forward:

1. **Dual-write between SQLite and filemap.yaml** — `docs/filemap-model.md` (line 7) explicitly states "SQLite becomes a derived cache", yet the current code dual-writes to both `taste_sessions` SQLite tables and `folder.yaml` for approval decisions (deferral-ledger.md lines 76-77 note this as a known overlap). SDFS Phase 1 should go filemap-only from the start; SQLite is unnecessary for an agent-driven CLI pipeline.

2. **The entire Next.js frontend** — `frontend/` (53 source files, ~30KB of JSX/TSX). This is a human-facing browser UI with VizDiff, taste-test reviewer, keyboard shortcuts, design tokens, dark mode — all of which are irrelevant to SDFS Phase 1's agent-driven scan/curate/build pipeline. None of it should be ported. The `awd` CLI is the correct interface for Phase 1.

3. **The taste-test UX flow** (`frontend/components/TasteTest/` + `backend/jobs/taste.py`). Sampling, side-by-side review, per-stratum pipeline tuning, approval/reject/flag/skip — this is a product surface for human operators eyeballing conversion quality. SDFS Phase 1 either trusts default pipelines or lets editing `folder.yaml`'s `user_included` field serve as curation. The taste-session state machine is dead weight.

4. **`manifest.json` + `manifest.yaml` dual write** — `backend/backend/manifest.py` (lines 117-124, 159-165). Writes both formats on every manifest update; YAML is best-effort with silent failures. SDFS Phase 1 should pick one format (the filemap itself may be sufficient as the index, or a single `manifest.yaml`).

5. **SQLite as primary state store** — `contracts/db-schema.sql` defines 7 tables (scans, strata, scan_docs, docs, jobs, doc_leases, taste_sessions, taste_strata, taste_approvals, _schema_migrations). The filemap model explicitly demotes SQLite to a cache; SDFS Phase 1 should not have an SQLite dependency at all.

6. **`PER_DOC_TIMEOUT_S = 30` default** — `backend/backend/jobs/batch.py` line 49. Too short for large scanned PDFs (a 50-page scan takes 2-4 minutes per README). The env var override pattern is correct, but the default should be much higher (300s or calculated from page count × per-page factor).

7. **`poppler-utils` optionality** — `scanner.py` treats poppler as optional and degrades gracefully (all PDFs collapse into a single `pdf` stratum). For SDFS Phase 1, `pdfinfo`/`pdftotext` should be a hard prerequisite at scan time — without them, the scanner can't distinguish native from scanned PDFs or determine page counts, making pipeline selection impossible.

8. **Scope creep beyond scan + convert** — The prototype's frontend renderers, VizDiff with bidirectional highlight, confidence gutter, keyboard shortcuts, and taste-test session state machine are all product features, not infrastructure. SDFS Phase 1 should be a clean CLI/library layer: scan folders → produce `.understanding/folder.yaml` per folder → convert based on filemap includes → produce mirrored output tree → produce triage rollup. That's the full scope.

## 5. Shape it gives us

What the prototype pre-decides or constrains for the rev-2 SDFS Phase 1 plan:

### CRITICAL: Filename conflict — `folder.yaml` vs `filemap.yaml`

**The prototype uses `.understanding/folder.yaml`**, not `.understanding/filemap.yaml`. This is defined at `backend/backend/stratification/filemap.py` line 24:
```python
FILEMAP_NAME = "folder.yaml"
```

The M3 plan's Q2 answer confirmed `.understanding/filemap.yaml` as the filename. **Decision required:** does SDFS Phase 1 adopt the prototype's `folder.yaml` (aligning with the only working implementation) or override to `filemap.yaml` (per the original spec)?

**Recommendation:** Adopt `folder.yaml`. It's already implemented, tested, and semantically clearer ("this is the folder's manifest"). Changing it risks drift between the spec and the only reference implementation. The `.understanding/` directory name is correct and should be kept.

### Other pre-decided aspects

- **Filemap location**: `.understanding/` directory inside each source folder (not in output dir). Confirmed in `docs/filemap-model.md` lines 14-18. Matches the spec's portability claim.
- **Schema version**: `schema_version: 1`, `scanner_version: "0.1.0"`. SDFS Phase 1 should start at `schema_version: 1` or bump to 2 if we change the spec.
- **Scanner suggestion values**: `include`, `exclude_by_pattern`, `exclude_by_size`, `exclude_by_type`, `unsupported`, `failed_extraction`. SDFS Phase 1 should use this exact enum.
- **User inclusion semantics**: `user_included: null` = "inherit scanner suggestion". `true` = explicitly include. `false` = explicitly exclude. This three-state logic is correct and should be kept.
- **Output layout**: Mirrored tree (preserves source directory structure). `convert_source_mirrored()` in `backend/backend/conversion/converter.py` (lines 543-685). Per-file output is `<output_dir>/<rel_path>/<source_name>.{md,json,anchors.json,meta.json}`.
- **Sidecars per doc**: Four files — `.md` (clean Markdown with `<!--- page-break --->`), `.json` (lossless DoclingDocument), `.anchors.json` (element→byte-range map for bidirectional highlight), `.meta.json` (conversion metadata). For SDFS Phase 1, `.anchors.json` may be deferred if bidirectional highlight isn't needed.
- **Dedup key**: `(source_sha256, pipeline_hash)`. This is the idempotency key for no-op resume. Fully correct.
- **Pipeline hash**: SHA-256 of sorted, compact JSON of normalized PipelineParams. Stable and deterministic.
- **Excluded directories**: The prototype's list (`scanner.py` lines 272-293) covers the common cases. SDFS Phase 1 should adopt it and add `wire-archive` if that's the output nesting convention.
- **Batch retry count**: The prototype uses N=3 (1 initial + 2 retries, in `batch.py` line 32). The filemap-model spec (`docs/filemap-model.md` line 123) says N=2. SDFS Phase 1 should pick one — recommend N=2 per the spec.
- **Docling version pin**: 2.90.0 in `backend/pyproject.toml`. SDFS Phase 1 should verify this is still current or pin the latest stable.
- **CLI interface shape**: `awd scan <folder>`, `awd filetree <root>`, `awd filemap <folder>`, `awd batch <output> --root <folder>`, `awd triage <output>`, `awd retry-triage <output>`. This maps cleanly to the SDFS Phase 1 scan/curate/build verbs.

## 6. Open questions surfaced by the study

These need answers before the rev-2 SDFS Phase 1 plan can be written:

1. **`folder.yaml` vs `filemap.yaml`** — Adopt prototype convention or override to original spec? (See §5 above — recommend adopt prototype.)
2. **SQLite dependency** — Does SDFS Phase 1 need any SQLite at all? The filemap model says "derived cache" — can Phase 1 be pure filesystem (filemap YAML files + manifest + triage)?
3. **Curation mechanism** — If SDFS Phase 1 doesn't port the taste-test UI, how does the human curate `user_included`? Via `$EDITOR` on `folder.yaml` directly (the prototype already supports this)? Via CLI commands like `awd include <folder> <file>` and `awd exclude <folder> <file>`?
4. **Output layout** — Does SDFS Phase 1 use the prototype's mirrored-tree layout exactly, or does it need a different nesting (e.g., Wire Node's existing `wire-archive/` convention)?
5. **`.anchors.json` necessity** — The prototype emits per-element byte-range anchors for bidirectional highlight. If SDFS Phase 1 is CLI-only with no UI, are anchors still needed? They carry per-element page/bbox provenance that could be useful for Wire Node's pyramid ingest.
6. **Docling version pin** — The prototype pins 2.90.0. Should SDFS Phase 1 adopt the same pin, or allow a range? Docling 2.x has been stable but the pin date is April 2026 — verify no breaking changes since.
7. **`poppler-utils` as hard dependency** — The prototype treats poppler as optional with degraded scan quality. Should SDFS Phase 1 make it a hard prerequisite (fail-fast at scan time if missing)?
8. **Per-file vs per-folder pipeline assignment** — The prototype assigns pipelines per-stratum (across folders). Does SDFS Phase 1 need per-folder or per-file pipeline overrides? The `folder.yaml` schema has no pipeline field — it would need an extension.
9. **Triage retry surface** — The prototype retries from a `triage.yaml` file edited by the human. Does SDFS Phase 1 need a programmatic retry API (retry all failures with a given pipeline override), or is the human-edits-triage-then-applies flow sufficient?
10. **SDFS Phase 1 deliverable shape** — Is it a Python library consumed by the pyramid build, a standalone CLI (like `awd`), or both? This determines whether the code lives alongside the pyramid or in a separate package.
11. **Image/figure extraction** — The prototype emits images via Docling's `ImageRefMode` (currently placeholder-only, `<!-- image -->`). Does SDFS Phase 1 need real image extraction into `images/<source_stem>/*.png`?
12. **What about non-Docling converters?** — The prototype is Docling-only. Does SDFS Phase 1 need to support other converters (e.g., for audio ASR, specialized formats)? The prototype's tier-3 exclusion list (audio, standalone images, XBRL, JATS) defers these — should SDFS Phase 1 do the same?
