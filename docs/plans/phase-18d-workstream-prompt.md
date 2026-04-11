# Workstream: Phase 18d — Schema Migration UI

## Who you are

You are an implementer joining a coordinated fix-pass across the pyramid-folders/model-routing/observability initiative. Phase 18 reclaims 9 dropped cross-phase handoffs. You are implementing workstream **18d**, claiming ledger entry **L6** from `docs/plans/deferral-ledger.md`.

Three other Phase 18 workstreams (18a/18b/18c) run in parallel on their own branches. Do not touch files outside your scope. Your commits land on branch `phase-18d-schema-migration-ui`.

## Context

Phase 9 shipped the `flag_configs_needing_migration` helper and the `needs_migration` column on `pyramid_config_contributions`. When a `schema_definition` contribution supersedes a prior one (e.g., the user refines a schema to add a new field or change a constraint), Phase 4's dispatcher automatically flags every active contribution whose `schema_type` targets the changed schema. The flag is a breadcrumb — it says "this contribution's YAML was written against an older schema, and re-validating it against the new schema may surface issues or need migration."

Phase 10 was supposed to add the user-facing surface: a "Needs Migration" section or badge in ToolsMode that lists flagged configs and lets the user trigger an LLM-assisted migration. Phase 10 explicitly re-deferred it in its own out-of-scope list ("Migrate config UI (no migration skill exists; deferred)"). The underlying primitives are still there — `needs_migration` column, `flag_configs_needing_migration` helper — they're just invisible to the user, and there's no backend flow to actually execute a migration.

**This workstream is effectively a mini-Phase-9 for the reverse direction.** Phase 9's generative flow takes intent → YAML. This workstream's migration flow takes (old_yaml, old_schema, new_schema) → new_yaml via an LLM, with user review + accept semantics matching Phase 9's pattern exactly.

## Ledger entry you claim

| L# | Item | Source |
|---|---|---|
| **L6** | Schema migration UI in ToolsMode — list configs with `needs_migration = 1`, trigger LLM-assisted migration, accept or reject | `docs/specs/generative-config-pattern.md` + `docs/specs/config-contribution-and-wire-sharing.md`; Phase 9 workstream prompt line 392 (defer UI); Phase 10 out-of-scope list |

## Required reading (in order)

1. `docs/plans/phase-18-plan.md` — overall structure; skim.
2. `docs/plans/deferral-ledger.md` — entry L6 in full.
3. **`docs/specs/generative-config-pattern.md`** — full read. Phase 9 shipped the forward direction (intent → YAML). You ship the reverse (old_yaml + new_schema → migrated_yaml). Pattern match on the 3-phase (load → LLM → persist) flow.
4. **`docs/specs/config-contribution-and-wire-sharing.md`** — scan "Schema evolution" section if present, or the contribution supersession semantics.
5. `docs/plans/phase-9-workstream-prompt.md` — the entire thing. Phase 9 is your reference model for how this workstream should look architecturally.
6. `docs/plans/phase-10-workstream-prompt.md` — the Out-of-scope list line "Migrate config UI (no migration skill exists; deferred)" and any surrounding context.

### Code reading

7. **`src-tauri/src/pyramid/schema_registry.rs` lines ~500-530** — `flag_configs_needing_migration` helper. Understand what the flag means and when it's set.
8. **`src-tauri/src/pyramid/db.rs`** — find `needs_migration` column definition on `pyramid_config_contributions`. Verify the column exists and its type (INTEGER 0/1 by convention).
9. **`src-tauri/src/pyramid/config_contributions.rs` line ~692 (`schema_definition` branch)** — the dispatcher that calls `flag_configs_needing_migration`. Understand when the flag gets set.
10. **`src-tauri/src/pyramid/generative_config.rs`** in full (1488 lines) — the Phase 9 flow. Your migration flow mirrors its 3-phase structure: load context → LLM call → persist contribution. Understand `accept_config_draft`, `refine_config_draft`, the contribution shape, the event emissions.
11. `src-tauri/src/pyramid/config_contributions.rs` — `load_active_config_contribution`, `load_contribution_by_id`, `supersede_config_contribution`. Your accept path creates a new contribution via supersession.
12. **`src/components/modes/ToolsMode.tsx`** — the Create and My Tools tabs. You add either a new "Needs Migration" tab OR a badge system on My Tools that surfaces flagged configs.
13. `src/components/CreatePanel.tsx` (Phase 10) — the Phase 9 draft/refine/accept UI. Your migration UI reuses this pattern for the review flow.
14. `src/components/ContributionDetailDrawer.tsx` — you may extend this with a "Migrate..." action when the drawer shows a flagged contribution.

## What to build

### 1. Backend: `pyramid_list_configs_needing_migration` IPC

```rust
#[derive(Serialize)]
struct NeedsMigrationEntry {
    contribution_id: String,
    schema_type: String,
    slug: Option<String>,
    current_yaml: String,
    current_schema_contribution_id: String,  // the schema_definition that superseded
    prior_schema_contribution_id: Option<String>,  // the schema the current yaml was written against
    flagged_at: String,
    supersession_note: Option<String>,  // the triggering_note from the schema change
}

#[tauri::command]
async fn pyramid_list_configs_needing_migration(
    state: tauri::State<'_, SharedState>,
) -> Result<Vec<NeedsMigrationEntry>, String>
```

Implementation:
- Query `pyramid_config_contributions WHERE needs_migration = 1 AND status = 'active'`
- For each row, resolve the active `schema_definition` contribution whose `applies_to` (or equivalent routing field) matches the row's `schema_type` — that's the `current_schema_contribution_id`
- Also resolve what schema the current YAML was ORIGINALLY written against. This requires either:
  - (a) Storing a `schema_contribution_id` column on `pyramid_config_contributions` at creation time (clean but a schema addition)
  - (b) Walking the schema_definition supersession chain backward from the current one to find "the schema_definition that was active when the config was created" — tractable via `created_at` comparison
- Return all the data needed for the review UI

**Schema addition note:** option (a) is cleaner long-term but requires adding `schema_contribution_id TEXT` to `pyramid_config_contributions` + backfill existing rows with the nearest schema at their `created_at` time. If this feels like scope creep, use option (b) for this phase and document the migration-by-chain-walk as a known limitation. The backward chain walk is acceptable.

### 2. Backend: `pyramid_propose_config_migration` IPC

```rust
#[derive(Deserialize)]
struct ProposeMigrationInput {
    contribution_id: String,   // the config needing migration
    user_note: Option<String>, // optional user guidance for the LLM
}

#[derive(Serialize)]
struct MigrationProposal {
    draft_id: String,          // a draft row in generative_config state
    old_yaml: String,
    new_yaml: String,          // LLM-generated migrated YAML
    changes_summary: String,   // LLM-generated human-readable diff
    schema_from: String,       // summary of old schema
    schema_to: String,         // summary of new schema
}

#[tauri::command]
async fn pyramid_propose_config_migration(
    state: tauri::State<'_, SharedState>,
    input: ProposeMigrationInput,
) -> Result<MigrationProposal, String>
```

Implementation (follows Phase 9's 3-phase pattern — reuse infrastructure):
- **Phase 1 (load):** fetch the flagged contribution, the current schema_definition, the prior schema_definition, build a context bundle
- **Phase 2 (LLM):** call an LLM with a prompt template that says "here's the old YAML that was valid against this old schema, here's the new schema, produce a migrated YAML that is valid against the new schema while preserving the user's intent from the old one." Use a canonical prompt template stored in `chains/prompts/migration/migrate_config.md` that you ship as part of this workstream.
- **Phase 3 (persist draft):** store the proposal as a DRAFT contribution (status = 'draft', `source = 'migration_proposal'`, linked back to the flagged contribution via `supersedes_id`). This matches Phase 9's draft/accept semantics — nothing lands as `active` until the user explicitly accepts.
- Emit a `ConfigMigrationProposed` event (new `TaggedKind` variant) for the DADBEAR Oversight page and any UI listening.

Return the proposal for the UI to render.

### 3. Backend: `pyramid_accept_config_migration` IPC

```rust
#[derive(Deserialize)]
struct AcceptMigrationInput {
    draft_id: String,
    accept_note: Option<String>,  // user's note about why they accepted
}

#[tauri::command]
async fn pyramid_accept_config_migration(
    state: tauri::State<'_, SharedState>,
    input: AcceptMigrationInput,
) -> Result<AcceptMigrationOutcome, String>
```

Implementation:
- Validate the draft exists and is still draft status
- Call `supersede_config_contribution(prior_id, new_yaml, note, "migration", Some("user"))` — this transactionally supersedes the old contribution with the migrated one, clears the `needs_migration` flag on the new row (it's freshly valid), and fires `sync_config_to_operational` via the Phase 4 dispatcher
- Emit a `ConfigMigrationAccepted` event
- Return the new contribution_id

### 4. Backend: `pyramid_reject_config_migration` IPC

For the case where the user reviews the LLM's migration proposal and rejects it. Deletes the draft row but leaves the original contribution flagged (so the user can try again later or migrate manually).

### 5. Backend: LLM prompt template

Ship a new file: `chains/prompts/migration/migrate_config.md`. Template shape:

```markdown
You are migrating a config YAML from one schema version to another.

## Old schema (the YAML below was valid against this)
{old_schema_yaml}

## New schema (the YAML must be valid against this after migration)
{new_schema_yaml}

## User's current YAML (old schema)
{old_yaml}

{if user_note}
## User guidance
{user_note}
{end}

## Output rules

- Output ONLY the migrated YAML, no prose before or after
- Preserve every value the user explicitly set in the old YAML, as long as the new schema still accepts it
- Remove fields that no longer exist in the new schema
- Add required new fields with sensible defaults drawn from the old YAML's semantic intent where possible
- For fields whose type changed, coerce values only if lossless; otherwise drop and note in a YAML comment (`# migrated: value dropped, old schema used X, new schema requires Y`)
- Include inline YAML comments for every non-trivial transformation so the user can review
```

Ship this as a bundled contribution in `bundled_contributions.json` with `schema_type: skill` (same slot as Phase 9's generation prompts) so users can refine the migration prompt itself via the generative flow. Fully self-describing per the architectural frame.

### 6. Frontend: ToolsMode "Needs Migration" surface

Two options:

**Option A:** Add a new top-level tab to ToolsMode alongside My Tools / Create / Discover — "Needs Migration" with a badge showing the count.

**Option B:** Add a banner to the top of the My Tools tab that says "N configs need migration [Review →]" and opens a modal/drawer.

**Recommendation: Option A.** It matches the existing tab structure and gives the migration flow a dedicated home.

Components:

- **`MigrationPanel.tsx`** — the new tab. Calls `pyramid_list_configs_needing_migration` on mount. Renders a list of flagged configs with:
  - Schema type + slug
  - "Flagged because: {supersession_note}"
  - "Propose migration" button
  - On click, opens the review modal

- **`MigrationReviewModal.tsx`** — the review UI. Calls `pyramid_propose_config_migration`, shows:
  - Side-by-side YAML diff (old vs new)
  - Changes summary
  - User note textarea (optional)
  - "Accept migration" button → calls `pyramid_accept_config_migration`
  - "Reject" button → calls `pyramid_reject_config_migration`
  - "Edit before accepting" → opens a YAML editor with the LLM's proposal pre-filled; on save, treats as a user-authored variant of the migration

### 7. Frontend: badge in My Tools

Even with the dedicated tab, add a small badge to each flagged contribution in the existing My Tools list (a "migration needed" chip next to the schema_type label) so users see the flag wherever they encounter the contribution.

### 8. Tests

Rust tests:
- `pyramid_list_configs_needing_migration` returns only `needs_migration = 1 AND status = 'active'` rows
- `pyramid_propose_config_migration` creates a draft contribution (status = 'draft')
- `pyramid_accept_config_migration` supersedes and clears the flag
- `pyramid_reject_config_migration` deletes the draft but leaves the flag
- Backward schema chain walk finds the correct prior schema_definition for a config with a timestamp

Frontend tests (if runner exists): MigrationPanel renders the list; MigrationReviewModal renders the diff.

## Scope boundaries

**In scope:**
- Four new IPCs: list, propose, accept, reject
- LLM prompt template for migration (bundled as skill contribution)
- ToolsMode Needs Migration tab + review modal
- My Tools badge for flagged contributions
- Rust tests for the IPC contract + chain walk
- Implementation log entry

**Out of scope:**
- Auto-migration on schema supersession (always needs user review)
- Bulk migrate-all button (defer — one-at-a-time is safer)
- Migration history (the supersession chain already captures it)
- Cross-schema-type migrations (only same-schema_type with a superseded schema_definition)

## Verification criteria

1. **Rust clean:** `cargo check --lib` — 3 pre-existing warnings allowed.
2. **Test count:** `cargo test --lib pyramid` — prior count + new Phase 18d tests.
3. **Frontend build:** `npm run build` clean.
4. **IPC registrations:** all four new IPCs defined + in `invoke_handler!`.
5. **Manual verification:**
   - Seed: a config with `needs_migration = 1` (either a real flagged row or a test fixture)
   - Navigate to ToolsMode → Needs Migration tab
   - Click "Propose migration"
   - Review the LLM proposal
   - Accept
   - Confirm: the original contribution is superseded, the new one is active, the flag is cleared, and the config is re-validated

## Deviation protocol

- **Schema chain walk complexity:** option (a) schema_contribution_id column is cleaner but requires migration + backfill. If complex, use (b) chain walk + document.
- **LLM prompt template:** ship the bundled version even if the LLM call quality is only so-so on v1 — the user can refine the prompt via generative config.
- **Tab vs banner for surfacing:** pick one, document.

## Mandate

- **`feedback_always_scope_frontend.md`:** the Needs Migration tab must be visible and clickable in the built app. Backend IPCs alone don't count.
- **Reuse Phase 9's draft/accept pattern exactly.** Don't invent a new flow — parallel to the existing generative config flow.
- **User review is mandatory.** Never auto-apply a migration. Even if the LLM's proposal looks good, it goes through draft → review → accept.
- **No Pillar 37 violations.** Don't hardcode migration rules, batch sizes, or prompt wording beyond the bundled template.

## Commit format

Single commit on `phase-18d-schema-migration-ui`:

```
phase-18d: schema migration UI + LLM-assisted flow

<5-8 line body summarizing:
- Four new IPCs: list/propose/accept/reject
- Bundled migrate_config prompt skill contribution
- ToolsMode Needs Migration tab + MigrationReviewModal + My Tools badge
- Schema-chain walk (option A or B) for prior schema resolution
- Claims L6 from deferral-ledger.md>
```

Do not amend. Do not push. Do not merge.

## Implementation log

Append Phase 18d entry to `docs/plans/pyramid-folders-model-routing-implementation-log.md`:
1. The four new IPCs + shapes
2. Bundled prompt template
3. Frontend components + mount
4. Schema chain walk method (A or B)
5. Tests added
6. Manual verification steps
7. Deviations
8. Status: `awaiting-verification`

## End state

Phase 18d is complete when:
1. Needs Migration tab renders in ToolsMode
2. Review modal proposes, accepts, or rejects migrations
3. Flagged contributions are visibly flagged in My Tools
4. `cargo check --lib` + `cargo test --lib pyramid` + `npm run build` clean
5. Single commit on branch `phase-18d-schema-migration-ui`

Begin with Phase 9's generative_config.rs as your template — the migration flow is its mirror image.

Good luck.
