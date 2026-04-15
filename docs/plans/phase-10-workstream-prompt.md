# Workstream: Phase 10 — ToolsMode UI Integration

## Who you are

You are an implementer joining an active 17-phase initiative. Phases 0a, 0b, 1, 2, 3, 4, 5, 6, 7, 8, 9 are shipped. You are the implementer of Phase 10, which wires the `ToolsMode.tsx` frontend to every user-facing config flow the initiative ships. This is the second pure-frontend phase (Phase 8 was the first — YamlConfigRenderer primitive).

Phase 10 is substantial because it's where the whole generative config / contribution / Wire sharing loop becomes visible to the user. Backend is done — your job is to wire.

## Context

`ToolsMode.tsx` already has three tabs (My Tools, Discover, Create) but Discover and Create are placeholders. MyToolsPanel currently fetches published Wire action contributions only. Phase 10 extends all three tabs to become the config contribution surface.

The pieces you're wiring together:

- **Phase 4** — contribution CRUD, supersession chains, notes enforcement, `pyramid_config_contributions` table
- **Phase 5** — Wire Native metadata, `pyramid_publish_to_wire` + `pyramid_dry_run_publish` IPC
- **Phase 8** — `YamlConfigRenderer` React component, `pyramid_get_schema_annotation` + `yaml_renderer_resolve_options` + `yaml_renderer_estimate_cost` IPC
- **Phase 9** — generative config IPC: `pyramid_generate_config`, `pyramid_refine_config`, `pyramid_accept_config`, `pyramid_active_config`, `pyramid_config_versions`, `pyramid_config_schemas`

All the backend plumbing exists. You are building the React UI that drives it.

## Required reading (in order)

### Spec docs

1. `docs/handoffs/handoff-2026-04-09-pyramid-folders-model-routing.md` — deviation protocol.
2. **`docs/specs/config-contribution-and-wire-sharing.md` — read the "Frontend: ToolsMode.tsx" section (around line 713) in full.** That section is the primary implementation contract for Phase 10. Plus scan the IPC Contract section (~line 615) to see what's available.
3. **`docs/specs/yaml-to-ui-renderer.md` — skim.** You're consuming Phase 8's `YamlConfigRenderer`, so understand its props contract (Renderer Contract section ~line 288).
4. **`docs/specs/generative-config-pattern.md` — read the IPC Contract section (~line 300) in full.** You're calling these 6 commands from the frontend.
5. `docs/specs/wire-contribution-mapping.md` — scan the "Publish IPC" + "One-Click Publish Flow" sections (~line 581, 669) for the dry-run publish modal shape.
6. `docs/plans/pyramid-folders-model-routing-full-pipeline-observability.md` — Phase 10 section.
7. `docs/plans/pyramid-folders-model-routing-implementation-log.md` — scan Phase 8, 9 entries for the component/IPC patterns you'll reuse.

### Code reading

8. **`src/components/modes/ToolsMode.tsx` — read in full (~208 lines).** This is the file you extend. Understand the existing `MyToolsPanel` / `DiscoverPanel` / `CreatePanel` shape.
9. **`src/components/YamlConfigRenderer.tsx` — read in full (~640 lines, Phase 8).** You'll mount this in the Create tab and the My Tools version-history drawer.
10. **`src/types/yamlRenderer.ts`** — the TypeScript contract from Phase 8.
11. **`src/hooks/useYamlRendererSources.ts`** — the Phase 8 hook.
12. `src/components/PyramidDetailDrawer.tsx` — existing drawer pattern you can reuse for a contribution-detail drawer.
13. `src/contexts/AppContext.tsx` — find `wireApiCall` and any Tauri `invoke` wrappers the existing code uses.
14. `src/components/AddWorkspace.tsx` + `src/components/Settings.tsx` (if it exists) — reference patterns for form components + state management.
15. Grep for `invoke<` or `invoke(` in `src/` to see how the codebase calls Tauri commands from React. Match that style.
16. `src-tauri/src/main.rs` — find the Phase 9 IPC commands and their exact argument/return shapes. Your frontend needs to call them with matching serde-derived types.

## What to build

### 1. MyToolsPanel extensions

Extend the existing `MyToolsPanel` component to show BOTH published Wire actions (existing behavior) AND local config contributions grouped by `schema_type`.

New sections:

**Section A: Published Wire Actions** (existing — keep as-is)

**Section B: My Configs** (new)
- Fetch via `invoke('pyramid_config_schemas')` to get the list of known schema types
- For each schema type, fetch `invoke('pyramid_active_config', { schema_type, slug: null })` — returns the current active contribution (or None if only bundled default exists)
- Render as a list of cards, one per schema_type, showing:
  - Schema type display name + description (from the schemas list)
  - Current version number ("Version 3 of 5")
  - Triggering note (latest refinement reason)
  - Status: active / draft / proposed
  - Source: local / bundled / wire / import
  - Actions: "View", "Publish to Wire", "View History"
- **View**: opens a detail drawer showing the YAML via YamlConfigRenderer in read-only mode
- **Publish to Wire**: calls `invoke('pyramid_dry_run_publish', { contribution_id })`, shows the preview modal, user confirms → `invoke('pyramid_publish_to_wire', { contribution_id, confirm: true })`
- **View History**: opens a version history drawer showing `invoke('pyramid_config_versions', { schema_type, slug: null })` as a list of rows with triggering_note + timestamp; clicking a row shows that version's YAML in the renderer (read-only)

**Section C: Pending Proposals** (new, smaller)
- Fetch `invoke('pyramid_pending_proposals', { slug: null })` — shows agent-proposed configs waiting for user review
- For each proposal: show the schema type, agent name, triggering note, and Accept/Reject buttons
- Accept: `invoke('pyramid_accept_proposal', { contribution_id })`
- Reject: `invoke('pyramid_reject_proposal', { contribution_id, reason })`

### 2. CreatePanel (generative config flow)

Replace the existing placeholder with the generative config flow.

**Step 1: Schema picker**
- Fetch `invoke('pyramid_config_schemas')` on mount
- Show as a grid of cards: one per schema type with display name, description, "Generate config" button
- On click, advance to Step 2 with the selected schema_type

**Step 2: Intent entry**
- Textarea: "Describe what you want. The more specific, the better. Example: 'Keep costs low, only maintain pyramids with active agent queries, run everything on local compute.'"
- Submit button: "Generate"
- On submit: call `invoke('pyramid_generate_config', { schema_type, slug: null, intent })`
- Show a loading state while the LLM runs
- On response, advance to Step 3 with the draft contribution

**Step 3: Render + refine**
- Fetch the schema annotation: `invoke('pyramid_get_schema_annotation', { schema_type })`
- Mount `<YamlConfigRenderer schema={annotation} values={parsedYaml} onAccept={...} onNotes={...} />`
- Values are parsed from the draft contribution's `yaml_content`
- Use the existing `useYamlRendererSources` hook for dynamic options + cost estimates
- **Accept**: call `invoke('pyramid_accept_config', { schema_type, slug: null, yaml: values })` → shows success state + returns the user to Step 1 or My Tools
- **Notes**: user types refinement feedback → call `invoke('pyramid_refine_config', { contribution_id, current_yaml: values, note })` → new YAML comes back → re-render with the new values, bump version counter, show the triggering_note
- **Cancel**: return to Step 1 without saving (the draft remains in the DB as a draft contribution that can be deleted later)

**Step 4 (optional): Success state**
- Show "Config accepted. Version N. It's now active." with a link back to My Tools or Create Another

The Create tab orchestrates this state machine. Use `useState` or `useReducer` for the wizard state (schema → intent → draft → accept).

### 3. DiscoverPanel

Replace the placeholder with a lightweight Wire config browser. Full discovery ranking is Phase 14 — Phase 10 ships a basic grep.

- Search input: "Search Wire configs"
- Schema type filter dropdown (populated from `pyramid_config_schemas`)
- On search: call `invoke('pyramid_search_wire_configs', { schema_type, tags: [], query })`
- Show results as cards: title, schema type, author handle, description, "Pull" button
- On Pull: call `invoke('pyramid_pull_wire_config', { wire_contribution_id, slug: null, activate: false })` → shows confirmation "Pulled as proposed, review in My Tools"

If `pyramid_search_wire_configs` or `pyramid_pull_wire_config` don't exist yet (they may be Phase 14 scope), show a "Coming in Phase 14: Wire discovery" message and skip the interactive parts. Document which IPC commands are stubbed.

### 4. Dry-run publish modal

A modal component (new: `src/components/PublishPreviewModal.tsx`) that shows the result of `pyramid_dry_run_publish`. Display:

- Visibility: scope (unscoped/fleet/circle) + destination (corpus/contribution/both)
- Canonical YAML preview (monospace, scrollable)
- Cost breakdown: price or pricing_curve
- Supersession chain preview
- Section decomposition preview (if sections are present)
- Warnings: credential leak detection, Pillar 37 violations, etc.
- Confirm button: → calls `pyramid_publish_to_wire(contribution_id, confirm: true)`
- Cancel button: closes the modal without publishing

### 5. Contribution detail drawer

A new drawer component (new: `src/components/ContributionDetailDrawer.tsx`) that shows a single contribution with:
- Header: schema type + version + status + source + created_at
- Body: YamlConfigRenderer in read-only mode
- Footer actions: Close, Edit (switches to Create flow with this as the base), View History

## Scope boundaries

**In scope:**
- `ToolsMode.tsx` extensions: MyTools (config contributions + proposals), Create (generative flow), Discover (basic Wire browser)
- `PublishPreviewModal.tsx` for dry-run publish
- `ContributionDetailDrawer.tsx` for single-contribution inspection
- Any needed hooks/utils under `src/hooks/` or `src/utils/`
- TypeScript types for the IPC responses (can be inline or in a dedicated `src/types/configContributions.ts`)
- Frontend tests IF a test runner exists (Phase 8 noted there isn't one — skip if absent)

**Out of scope:**
- Settings → Credentials UI (defer to a future frontend-cleanup phase — see credentials-and-secrets.md)
- ImportPyramidWizard for Phase 7 cache warming (defer — can be a small follow-up phase or included with Phase 14)
- Full Wire discovery ranking UI (Phase 14)
- `condition` property evaluation in YamlConfigRenderer (Phase 8 noted this is deferred; if Phase 10's schemas need it, extend YamlConfigRenderer in a minimal way — otherwise leave deferred)
- Migrate config UI (no migration skill exists; deferred)
- Reroll-with-notes on individual pyramid nodes (Phase 13)
- Schema migration detection + surfacing migration buttons (deferred)
- CSS overhaul — match existing conventions (minimal new styles only)
- The 7 pre-existing unrelated test failures

## Verification criteria

1. **Frontend build:** `npm run build` (or equivalent) — clean, no new TypeScript errors.
2. **No new frontend warnings.** Any lint runner the project has — clean on new files.
3. **Rust unchanged:** `cargo check --lib`, `cargo test --lib pyramid` — same 1048 passing + same 7 pre-existing failures. Phase 10 should NOT modify any Rust code except to register new IPC commands IF needed (and no new commands should be needed — all Phase 10 IPC is already built by Phases 4/5/8/9).
4. **Manual verification path:** document the steps for someone to manually verify in the implementation log — "launch dev server, click ToolsMode → Create, pick a schema, enter intent, refine, accept" etc.

## Deviation protocol

Standard. Most likely deviations:

- **`pyramid_search_wire_configs` / `pyramid_pull_wire_config` don't exist yet.** These are Phase 14 scope. If the commands aren't registered, wire a "Coming in Phase 14" placeholder in Discover and document the IPC dependency.
- **`pyramid_pending_proposals` / `pyramid_accept_proposal` / `pyramid_reject_proposal` call shape.** These are Phase 4 IPC. Verify they exist and their argument shapes match what the frontend expects. Flag any divergence.
- **Schema annotation loading for less common types.** If the schema picker returns a schema_type that has no `schema_annotation` contribution yet (e.g., if Phase 9's bundled manifest didn't ship one for a given type), `pyramid_get_schema_annotation` returns null. Gracefully fall back to a plain textarea for the YAML or show "No UI schema available for this config type."
- **Draft contributions accumulating.** The Create flow creates draft contributions that may not be accepted. Phase 10 doesn't cover draft cleanup — flag if you notice accumulation patterns that would need a "clear drafts" IPC.

## Implementation log protocol

Append Phase 10 entry to `docs/plans/pyramid-folders-model-routing-implementation-log.md`. Document the MyTools/Create/Discover extensions, new modal + drawer components, IPC calls wired, manual verification steps, and verification results. Status: `awaiting-verification`.

## Mandate

- **No backend changes unless strictly necessary.** All Phase 10 IPC exists already. If you need to tweak a handler signature because the frontend shape is clearly better, flag it — but default to matching the existing Rust signatures.
- **Match existing frontend conventions.** Look at `AddWorkspace.tsx`, `PyramidDetailDrawer.tsx`, `Settings*.tsx` (if present) for style, CSS class naming, Tauri invoke patterns, and error handling. Do NOT introduce a new styling system, state manager, or framework.
- **Notes enforcement is backend-owned.** Phase 9's `pyramid_refine_config` rejects empty notes at the IPC boundary. The frontend should also pre-check (don't submit empty notes to the backend) but the backend is the safety net.
- **Fix all bugs found.** Standard repo convention.
- **Commit when done.** Single commit with message `phase-10: toolsmode ui integration`. Body: 5-7 lines summarizing MyTools + Create + Discover + modal + drawer. Do not amend. Do not push.

## End state

Phase 10 is complete when:

1. `ToolsMode.tsx` `MyToolsPanel` shows local config contributions grouped by schema_type, pending proposals, and the existing Wire actions.
2. `ToolsMode.tsx` `CreatePanel` implements the schema-picker → intent → draft → refine → accept state machine.
3. `ToolsMode.tsx` `DiscoverPanel` shows a basic Wire config search OR a clear "Phase 14" placeholder if the IPC is missing.
4. `PublishPreviewModal.tsx` exists and calls `pyramid_dry_run_publish` + `pyramid_publish_to_wire`.
5. `ContributionDetailDrawer.tsx` exists and mounts YamlConfigRenderer in read-only mode.
6. `npm run build` (or equivalent) is clean.
7. `cargo test --lib pyramid` unchanged (1048 passing, same 7 pre-existing failures).
8. Implementation log Phase 10 entry complete with manual verification steps.
9. Single commit on branch `phase-10-toolsmode-ui`.

Begin with the existing ToolsMode.tsx and neighboring components for style reference. Then the spec. Then wire.

Good luck. Build carefully.
