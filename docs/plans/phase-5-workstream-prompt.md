# Workstream: Phase 5 — Wire Contribution Mapping (Canonical)

## Who you are

You are an implementer joining an active 17-phase initiative. Phases 0a, 0b, 1, 2, 3, and 4 are shipped. You are the implementer of Phase 5, which defines the canonical `WireNativeMetadata` struct that anchors every local `pyramid_config_contributions` row to the Wire Native Documents format from the moment of creation.

Phase 5 is substantial. It's the convergence point between the node's local config contributions (Phase 4) and the Wire's contribution layer. The spec is 1007 lines and the canonical schema it mirrors is in a different repo (`GoodNewsEveryone/docs/wire-native-documents.md`). **Canonical alignment is load-bearing — the field names in your Rust types must match the YAML schema byte-for-byte.** Any drift means our published contributions fail validation against Wire's schema.

## Context: the canonical schema is in another repo

The Wire Native Documents format is defined in `/Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/wire-native-documents.md`. That file is the source of truth. Phase 5's spec (`docs/specs/wire-contribution-mapping.md`) mirrors it in Rust, but the canonical YAML schema is non-negotiable. Read `wire-native-documents.md` end-to-end before writing any Rust types. Any spec-vs-canonical divergence means the canonical wins — flag the spec for correction, do NOT diverge from the canonical.

Additional canonical references (all in `/Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/`):
- `wire-skills.md` — skills as contributions, kits as tagged skills
- `wire-templates-v2.md` — templates as configuration presets
- `wire-actions.md` — executable chains as action contributions
- `wire-supersession-chains.md` — supersession field, same-author rule
- `wire-handle-paths.md` — handle/day/seq identity format
- `wire-circle-revenue.md` — 48 creator slots
- `economy/wire-rotator-arm.md` — 28-slot derived_from allocation via largest remainder

## Required reading (in order, in full unless noted)

### Canonical references (in the GoodNewsEveryone repo)

1. **`/Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/wire-native-documents.md` — read in full.** This is the canonical source of truth for the Wire Native Documents YAML schema. Your Rust types must mirror it exactly.
2. **`/Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/wire-skills.md`** — skills (markdown body contributions) and kits (skills tagged as kits).
3. **`/Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/wire-templates-v2.md`** — templates as configuration presets. Not processing recipes.
4. **`/Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/wire-actions.md`** — actions as executable chain definitions.
5. `/Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/wire-supersession-chains.md` — `supersedes` field, same-author rule, single-child chains, orthogonal to `derived_from`.
6. `/Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/wire-handle-paths.md` — `handle/day/seq` reference format.
7. `/Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/wire-circle-revenue.md` — `creator_split` semantics, 48-slot total.
8. `/Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/economy/wire-rotator-arm.md` — 28-slot `derived_from` allocation rules (largest-remainder method, min 1 per source, max 28 sources).

### Handoff + spec docs (in agent-wire-node repo)

9. `docs/handoffs/handoff-2026-04-09-pyramid-folders-model-routing.md` — original handoff, deviation protocol.
10. **`docs/specs/wire-contribution-mapping.md` — read in full, end-to-end (1007 lines).** This is the implementation contract. Particular attention to: the mapping table (lines 26-48), the `WireNativeMetadata` struct definition (lines 127-277), storage (lines 277-350), creation-time capture (lines 351-365), the 28-slot allocation helper (lines 411-467), wire type resolution (lines 715-739), the publish flow (lines 581-668), seed contributions (lines 740-918), and migration from on-disk prompts/schemas (lines 919-957).
11. `docs/plans/pyramid-folders-model-routing-full-pipeline-observability.md` — Phase 5 section.
12. `docs/plans/pyramid-folders-model-routing-implementation-log.md` — scan Phase 4 entry to see the contribution foundation this phase builds on.

### Code reading

13. `src-tauri/src/pyramid/config_contributions.rs` — the whole file. This is Phase 4's contribution table + CRUD + dispatcher. Phase 5 extends the creation paths to initialize `wire_native_metadata_json` with canonical metadata instead of the `'{}'` stub.
14. `src-tauri/src/pyramid/db.rs` — targeted. Find the `pyramid_config_contributions` table definition (Phase 4). You'll add no new columns but will change what the creation CRUD writes into `wire_native_metadata_json`.
15. `src-tauri/src/pyramid/types.rs` — scan for existing type conventions. You'll add many new types (`WireNativeMetadata`, `WireRef`, `WireDestination`, `WireScope`, `WireMaturity`, `WireContributionType`, `WireEntity`, `WireRelatedRef`, `WireClaim`, `WirePricingCurve`, `WireCreatorSplit`, `WireSection`, `WirePublicationState`, etc.).
16. `src-tauri/src/pyramid/wire_publish.rs` — existing file. Pyramid contributions already publish via `PyramidPublisher`. Phase 5 adds `publish_contribution_with_metadata()` for config contributions alongside the existing node publication flow.
17. `src-tauri/src/pyramid/chain_loader.rs` — the on-disk chain YAML loader. Phase 5's migration reads chain YAML files + their referenced prompts and converts them to contributions (custom_chain bundles). You do NOT have to touch the loader itself — just read it to understand the directory structure.
18. `chains/prompts/` directory — inspect the layout. Phase 5's prompt migration reads every `.md` file here and creates a `skill` contribution per file on first run (bundled seeds). The prompt cache then serves prompts from contributions instead of disk.
19. `src-tauri/src/main.rs` — find the existing IPC command block. You'll register new commands.

## What to build

### 1. Canonical `WireNativeMetadata` struct (in `types.rs` or new `wire_native_metadata.rs`)

Mirror the canonical schema from `wire-native-documents.md` exactly. Every field name, every enum variant, every optional/required status. If the canonical schema has a field you don't recognize, DO NOT skip it — ask the planner or use the spec's mirror.

Define these types (field names MUST match the canonical YAML keys exactly):

```rust
// Top-level struct
pub struct WireNativeMetadata {
    // Routing
    pub destination: WireDestination,
    pub corpus: Option<String>,
    pub contribution_type: WireContributionType,
    pub scope: WireScope,
    // Identity
    pub topics: Vec<String>,
    pub entities: Vec<WireEntity>,
    pub maturity: WireMaturity,
    // Relationships
    pub derived_from: Vec<WireRef>,       // float weights at author time
    pub supersedes: Option<String>,       // path reference, NOT resolved UUID
    pub related: Vec<WireRelatedRef>,
    // Claims
    pub claims: Vec<WireClaim>,
    // Economics
    pub price: Option<i64>,                // mutually exclusive with pricing_curve
    pub pricing_curve: Option<Vec<WirePricingCurveEntry>>,
    pub embargo_until: Option<String>,
    // Distribution
    pub pin_to_lists: Vec<String>,
    pub notify_subscribers: bool,
    // Circle splits (circle-scoped only)
    pub creator_split: Vec<WireCreatorSplit>,   // must sum to 48
    // Lifecycle
    pub auto_supersede: bool,
    pub sync_mode: WireSyncMode,
    // Decomposition
    pub sections: Option<HashMap<String, WireSection>>,
}

pub enum WireDestination { Corpus, Contribution, Both }
pub enum WireContributionType { Analysis, Assessment, Rebuttal, Extraction, Skill, Template, Action, /* + whatever else the canonical enumerates */ }
pub enum WireScope { Unscoped, Fleet, Circle(String) }
pub enum WireMaturity { Draft, Design, Canon, Deprecated }
pub enum WireSyncMode { Auto, Review, Manual }

pub struct WireEntity { pub name: String, pub type_: String, pub role: String }
pub struct WireRef {
    pub ref_: Option<String>,      // handle/day/seq for Wire contribution
    pub doc: Option<String>,       // path/to/local-doc.md
    pub corpus: Option<String>,    // corpus-name/path.md
    pub weight: f64,               // floats at author time
    pub justification: String,
}
// (etc. for each nested type)
```

**Serialization:** use `#[serde(rename = "...")]` for field renames where Rust naming conventions don't match canonical YAML. Validate with a round-trip test (`canonical_yaml → deserialize → serialize → byte-identical_yaml`).

### 2. `WirePublicationState` struct (stored separately from canonical metadata)

```rust
pub struct WirePublicationState {
    pub wire_contribution_id: Option<String>,
    pub handle_path: Option<String>,       // e.g., "playful/77/3"
    pub chain_root: Option<String>,
    pub chain_head: Option<String>,
    pub last_resolved_derived_from: Vec<ResolvedDerivedFromEntry>,
}
```

This is what goes into `pyramid_config_contributions.wire_publication_state_json`. Per the spec: keep publication state (resolved UUIDs, handle-paths, chain refs) **separate from the canonical metadata** so the canonical metadata stays portable across users.

### 3. Wire type resolution

Implement `resolve_wire_type(schema_type: &str) -> (WireContributionType, Vec<String> /* tags */)` per the spec's "Wire Type Resolution at Publish Time" section + the mapping table. Match each local `schema_type` to its Wire type and default tag set.

### 4. Creation-time capture

Extend Phase 4's `create_config_contribution()` + `supersede_config_contribution()` in `config_contributions.rs` to initialize `wire_native_metadata_json` with a canonical default (not `'{}'`) per the "Creation-Time Capture" table in the spec. Different `schema_type`s get different initial metadata (e.g., `skill` gets `contribution_type: skill`, `dadbear_policy` gets `contribution_type: template` + `template_definition.applies_to: "dadbear_policy"`).

Provide a helper `default_wire_native_metadata(schema_type: &str, slug: Option<&str>) -> WireNativeMetadata` that returns a sensible default with `maturity: draft`, `scope: unscoped`, topics/entities empty, no derived_from, etc. The user/LLM can refine later.

### 5. 28-slot largest-remainder allocation helper

Per `wire-rotator-arm.md` and the spec's `derived_from` and the Rotator Arm section:

```rust
/// Allocate 28 slots among N sources proportionally to their float weights.
/// - Minimum 1 slot per source
/// - Maximum 28 sources (reject if > 28)
/// - Uses the largest-remainder (Hamilton) method
/// - Returns Vec<(source_index, slot_count)> summing to 28
pub fn allocate_28_slots(weights: &[f64]) -> Result<Vec<usize>, RotatorAllocError>
```

Test exhaustively: exact float sums, rounding edge cases, single source (gets 28), 28 sources (each gets 1), empty input (error), >28 sources (error), zero weights (error or equal split — spec should say).

### 6. On-disk prompt/schema migration

Per the spec's "Migration from On-Disk Prompts and Schemas" section:

1. On first run (check for a marker like `_prompt_migration_marker` sentinel contribution row), walk `chains/prompts/**/*.md` and create a `skill` contribution per file with `source = 'bundled'`, `status = 'active'`, `yaml_content = <markdown body>`. The contribution's `wire_native_metadata.contribution_type = skill`, with topic tags derived from the file's directory (e.g., `chains/prompts/conversation-episodic/forward.md` gets topics `["prompt", "extraction", "conversation-episodic"]`).
2. Walk `chains/defaults/**/*.yaml` (chain YAML files) and create a `custom_chain` contribution per file with `source = 'bundled'`. The `yaml_content` is the bundle format from the spec's "Custom Chain Bundle Serialization" section — serialize both the chain YAML and all prompts it references into a single bundle.
3. Walk any schema definition files (if they exist on disk) and create `schema_definition` contributions. If no schema files exist on disk yet (Phase 9 is the first to define them), skip this step with a TODO comment.

Idempotent via marker sentinel + per-file hash check.

### 7. Prompt lookup cache (runtime resolution from contributions)

Add a `PromptCache` that serves prompt bodies from `pyramid_config_contributions` (not disk). The runtime lookup key is the prompt path (e.g., `"conversation-episodic/forward.md"`) mapped to the contribution's `yaml_content`. The cache is populated on first lookup and invalidated when a `skill` contribution is superseded or created.

The existing `chain_loader::load_prompt` path should transparently hit the cache first, falling back to disk for files not yet migrated (rare — should only happen for chains that land AFTER first-run migration).

Wire up the `invalidate_prompt_cache()` stub from Phase 4's dispatcher to actually invalidate this cache.

### 8. Wire publication IPC: `publish_contribution_with_metadata`

Add to `wire_publish.rs`:

```rust
pub async fn publish_contribution_with_metadata(
    publisher: &PyramidPublisher,
    contribution: &ConfigContribution,
) -> Result<PublishOutcome>
```

1. Deserialize `contribution.wire_native_metadata_json` into `WireNativeMetadata`
2. Call `resolve_wire_type(&contribution.schema_type)` to get the Wire type
3. Resolve `derived_from` path references against the local `pyramid_id_map` and Wire graph
4. Allocate 28 slots via `allocate_28_slots` over the resolved derived_from weights
5. Emit the canonical YAML block with slots-as-integers (not floats)
6. POST to the Wire's contribution endpoint with the serialized YAML
7. On success: write back `wire_contribution_id`, `handle_path`, `chain_root`, `chain_head`, `last_resolved_derived_from` into `wire_publication_state_json`
8. Update `maturity: draft → design` (or whatever the user set) on first publish
9. Record in `pyramid_id_map`

### 9. Dry-run publish IPC

```rust
pub async fn dry_run_publish(
    publisher: &PyramidPublisher,
    contribution: &ConfigContribution,
) -> Result<DryRunReport>
```

Does everything `publish_contribution_with_metadata` does EXCEPT the actual HTTP POST. Returns:
- `resolved_derived_from`: each source with its allocated slot count
- `visibility`: canonical scope + tags
- `cost_breakdown`: price or pricing_curve
- `supersession_chain`: how this contribution links into existing Wire chains
- `warnings`: credential references detected via `credential_resolver::collect_references` (from Phase 3), Pillar 37 violations, etc.

This surfaces the publish preview to the user without writing to Wire.

### 10. IPC endpoints

Register in `invoke_handler!`:

- `pyramid_publish_to_wire(contribution_id: String, confirm: bool)` — calls `publish_contribution_with_metadata` only when `confirm: true`
- `pyramid_dry_run_publish(contribution_id: String)` — calls `dry_run_publish`

Do NOT implement `pyramid_search_wire_configs` or `pyramid_pull_wire_config` — those are Phase 10 scope (ToolsMode UI).

### 11. Tests

- Canonical round-trip: build a `WireNativeMetadata`, serialize to YAML, parse back, verify byte-identical (or field-equivalent) to the canonical schema
- `allocate_28_slots`: exhaustive edge cases (1 source, 28 sources, weights summing to exactly 28, weights requiring largest-remainder rounding, >28 sources → error)
- Creation-time capture: call `create_config_contribution` for each of the 14 schema_types, verify `wire_native_metadata_json` is populated with a sensible default (not `'{}'`)
- Wire type resolution: every entry in the mapping table
- Prompt migration idempotency: run migration twice, verify no duplicate `skill` contributions
- Prompt cache: lookup a migrated prompt, verify body matches, supersede the contribution, verify cache returns the new body
- Dry-run publish: construct a contribution with `derived_from` entries, call dry_run_publish, verify the report includes resolved slots and no credential leaks

## Scope boundaries

**In scope:**
- Canonical `WireNativeMetadata` struct + all nested types
- Wire type resolution from `schema_type`
- Creation-time capture (update Phase 4's create/supersede paths)
- `WirePublicationState` struct (separate from metadata)
- 28-slot largest-remainder allocation helper
- On-disk prompt migration to skill contributions (idempotent)
- On-disk chain migration to custom_chain bundle contributions (idempotent)
- Prompt lookup cache backed by contributions
- `publish_contribution_with_metadata` + `dry_run_publish`
- 2 IPC endpoints
- Tests for every new piece

**Out of scope:**
- Prepare LLM enrichment (LLM call that auto-populates topics/tags/entities from the contribution body) — Phase 9 scope
- ToolsMode frontend UI (publish button, dry-run preview rendering) — Phase 10
- Wire config search / pull IPC — Phase 10
- Wire discovery ranking — Phase 14
- `pyramid_chain_publications` migration to contributions — this spec says Phase 5 does it; if the existing `pyramid_chain_publications` table is empty on current dev installs, skip with a TODO noting Phase 5 handled it
- Custom chain disk sync (writing bundled chains back to disk on accept) — Phase 9 scope per Phase 4's stub pattern
- JSON Schema validation (Phase 9 provides schemas)
- The existing 7 pre-existing unrelated test failures

## Verification criteria

1. `cargo check --lib`, `cargo build --lib` — clean, zero new warnings.
2. `cargo test --lib pyramid::wire_native_metadata` (or wherever you put the tests) — all new tests passing.
3. `cargo test --lib pyramid` — 855+ passing (Phase 4's count) + your new Phase 5 tests. Same 7 pre-existing failures. No new ones.
4. Canonical round-trip test passes for the full `WireNativeMetadata` schema — serialize a fully-populated struct to YAML, parse the canonical YAML block from `wire-native-documents.md`, verify field parity.
5. On a fresh DB, `init_pyramid_db` triggers both the Phase 4 DADBEAR migration AND Phase 5's prompt migration. Re-running must not duplicate rows.
6. A `skill` contribution created post-migration has `wire_native_metadata_json` with `contribution_type: "skill"`, not `"{}"`.

## Deviation protocol

Standard. Most likely deviations:
- **Canonical schema drift** — if `wire-contribution-mapping.md` (the spec) disagrees with `wire-native-documents.md` (the canonical) in any field name or enum variant, canonical wins. Flag the spec for correction via the friction log, but implement against the canonical.
- **Missing canonical field** — if the canonical schema has a field the spec didn't mention, include it in the Rust struct and flag it.
- **Prompt/schema migration edge cases** — if a prompt file has non-UTF-8 content, unusual encoding, or references another prompt that doesn't exist, flag it in the friction log and skip the file (don't abort the whole migration).

## Implementation log protocol

Append Phase 5 entry to `docs/plans/pyramid-folders-model-routing-implementation-log.md`. Document the new types, creation-time capture changes, 28-slot helper, prompt migration, prompt cache, publication IPC, tests, and verification results. Status: `awaiting-verification`.

## Mandate

- **Canonical alignment is load-bearing.** If the spec and canonical disagree, canonical wins.
- **Correct before fast.** Every field name matters. Round-trip YAML tests are the safety net.
- **No new scope.** ToolsMode UI is Phase 10. Wire discovery is Phase 14. Prepare LLM enrichment is Phase 9.
- **Pillar 37 watch.** The 28-slot allocation (fixed at 28) is a canonical protocol constant from the rotator arm economy — NOT a Pillar 37 violation. The minimum-1-per-source rule is likewise a protocol constraint. Document these in code comments so nobody mistakes them for tunable config.
- **Fix all bugs found.** Standard.
- **Commit when done.** Single commit with message `phase-5: wire contribution mapping (canonical)`. Body: 5-7 lines summarizing types + creation-time capture + migration + cache + publish IPC + tests. Do not amend. Do not push.

## End state

Phase 5 is complete when:

1. `WireNativeMetadata` struct + all nested types exist and round-trip with the canonical YAML.
2. Every Phase 4 contribution creation path initializes canonical metadata (not `'{}'`).
3. `resolve_wire_type()` correctly maps every local `schema_type` to its Wire type + tag set.
4. 28-slot largest-remainder allocator exists and is tested exhaustively.
5. On-disk prompt migration runs on first init_pyramid_db and creates skill contributions (idempotent).
6. On-disk chain migration creates custom_chain bundle contributions (idempotent).
7. `PromptCache` serves prompts from contributions; `invalidate_prompt_cache` from Phase 4's stub is wired up.
8. `publish_contribution_with_metadata` + `dry_run_publish` exist in `wire_publish.rs`.
9. `pyramid_publish_to_wire` + `pyramid_dry_run_publish` IPC endpoints registered.
10. All tests pass, no regressions.
11. Implementation log Phase 5 entry complete.
12. Single commit on branch `phase-5-wire-contribution-mapping`.

Begin with the canonical references in the GoodNewsEveryone repo. They are non-negotiable. Then the spec. Then the code.

Good luck. Build carefully.
