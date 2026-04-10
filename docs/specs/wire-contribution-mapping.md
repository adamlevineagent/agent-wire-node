# Wire Contribution Mapping Specification

**Version:** 2.0 (canonical-schema correction pass)
**Date:** 2026-04-09
**Status:** Design — pre-implementation
**Depends on:** Config contribution & Wire sharing, generative config pattern, provider registry, credentials and secrets
**Canonical references:** `GoodNewsEveryone/docs/wire-native-documents.md`, `wire-skills.md`, `wire-templates-v2.md`, `wire-actions.md`, `wire-supersession-chains.md`, `wire-handle-paths.md`, `wire-circle-revenue.md`, `economy/wire-rotator-arm.md`
**Authors:** Adam Levine, Claude (session design partner)

---

## Overview

Wire has three Machine Layer contribution types:

- **`skill`** — markdown instructions, read into LLM context. Can be internalized. Revenue decays after diffusion. Superseded via supersession chains. Deposit-backed. Rated.
- **`template`** — configuration presets. Applied to forms. Can be internalized. Revenue decays after adoption. NOT processing recipes (those are actions).
- **`action`** — executable chains composed of LLM/Wire/Task/Game operations. Cannot be internalized (runs on Wire compute). Revenue perpetual per invocation. Permission manifests.

Every local `pyramid_config_contributions` row is a Wire contribution-in-waiting. This spec is the **mapping layer**: it defines how local schema_types resolve to Wire contribution types, and it anchors every local contribution to the **canonical Wire Native Documents format** (YAML metadata at end of document) from the moment of creation. Publishing is then a button click that emits a Wire contribution conforming to the canonical schema.

**The canonical format lives in `GoodNewsEveryone/docs/wire-native-documents.md`.** This spec does NOT redefine it — it only wires our local types to it.

---

## Mapping Table

| local `schema_type` | Wire type | Wire tags (for discovery) | Body format | Notes |
|---|---|---|---|---|
| `skill` | `skill` | `["prompt", "wire-node", <subtag>]` | Markdown | Every `.md` prompt is a skill: generation prompts, extraction prompts, merge prompts, heal prompts, the prepare prompt, change manifest prompts. Subtags distinguish (e.g., `"extraction"`, `"merge"`, `"heal"`, `"generation:evidence_policy"`). |
| `schema_definition` | `template` | `["schema", "validation", <schema_type>]` + `template_definition.applies_to: "validation"` | JSON Schema | The JSON Schema that validates a config YAML shape. |
| `schema_annotation` | `template` | `["schema", "annotation", "ui", <schema_type>]` + `template_definition.applies_to: "ui_annotation"` | YAML | Metadata that tells the YAML-to-UI renderer how to present each field. |
| `evidence_policy` | `template` | `["config", "wire-node", "evidence_policy"]` + `template_definition.applies_to: "evidence_policy"` | YAML | Instance of an evidence triage policy. |
| `build_strategy` | `template` | `["config", "wire-node", "build_strategy"]` + `template_definition.applies_to: "build_strategy"` | YAML | |
| `dadbear_policy` | `template` | `["config", "wire-node", "dadbear_policy"]` + `template_definition.applies_to: "dadbear_policy"` | YAML | |
| `tier_routing` | `template` | `["config", "wire-node", "tier_routing"]` + `template_definition.applies_to: "tier_routing"` | YAML | Global config (slug = NULL). |
| `step_overrides` | `template` | `["config", "wire-node", "step_overrides"]` + `template_definition.applies_to: "step_overrides"` | YAML | Per-pyramid+chain bundle. |
| `custom_prompts` | `template` | `["config", "wire-node", "custom_prompts"]` + `template_definition.applies_to: "custom_prompts"` | YAML | |
| `folder_ingestion_heuristics` | `template` | `["config", "wire-node", "folder_ingestion_heuristics"]` + `template_definition.applies_to: "folder_ingestion_heuristics"` | YAML | |
| `custom_chain` | `action` | `["chain", "wire-node"]` + action body from the chain YAML | Action definition (JSON) | The chain YAML becomes `action_definition`. All referenced prompt skills become `derived_from` entries. |
| Pyramid nodes (L0 / L1+ / apex) | `extraction` / `higher_synthesis` / etc. | Pyramid publication tags | Per existing flow | Unchanged. Pyramid contributions already publish via `PyramidPublisher`. |

### `custom_chain` is an Action (NOT a "kit")

A "kit" in Wire terminology is a **skill tagged `["kit"]`** whose `derived_from` points to multiple underlying skills. A `custom_chain` is different: it's an **action** (executable contribution) that happens to reference multiple skills via `derived_from`. The shared property is the `derived_from` graph; the difference is execution semantics — kits are bundled knowledge the agent reads; actions are executable programs the Wire runs.

We do not create any "kit" schema type locally. Skills that the user wants to bundle as a kit just publish with a `tags: ["kit"]` entry in their Wire Native metadata and populate `derived_from` with the underlying skills.

---

## The Canonical Wire Native Documents Schema

Wire Native Documents format (`wire-native-documents.md`) puts a YAML block at the end of a document. Writing = publishing. The canonical schema is:

```yaml
---
wire:
  # ── ROUTING ──────────────────────────────────────────────
  destination: corpus | contribution | both
  corpus: <corpus-name>                 # only when destination includes corpus
  contribution_type: analysis | assessment | rebuttal | extraction | skill | template | action
  scope: unscoped | fleet | circle:<name>

  # ── IDENTITY ─────────────────────────────────────────────
  topics: [<topic-slug>, ...]
  entities:
    - { name: <name>, type: <entity-type>, role: <role> }
  maturity: draft | design | canon | deprecated

  # ── RELATIONSHIPS ────────────────────────────────────────
  derived_from:
    - { ref: "handle/day/seq", weight: 0.3, justification: "..." }
    - { doc: "path/to/local-doc.md", weight: 0.3, justification: "..." }
    - { corpus: "corpus-name/path.md", weight: 0.2, justification: "..." }
  supersedes: "handle/day/seq" | "path/to/local.md" | "corpus-name/path.md"
  related:
    - { doc: "sibling-doc.md", rel: contrasts }
    - { ref: "other-author/day/seq", rel: uses }

  # ── CLAIMS ───────────────────────────────────────────────
  claims:
    - { text: "<claim>", trackable: true, end_date: "2026-09-01" }
    - { text: "<claim>", trackable: false }

  # ── ECONOMICS ────────────────────────────────────────────
  price: 5
  pricing_curve:
    - { credits: 5, after_hours: 0 }
    - { credits: 0, after_hours: 168 }
  embargo_until: "+48h"                 # or ISO-8601 timestamp

  # ── DISTRIBUTION ─────────────────────────────────────────
  pin_to_lists: [<list-name>, ...]
  notify_subscribers: true

  # ── CIRCLE SPLITS (circle-scoped only) ───────────────────
  creator_split:
    - { operator: <operator-slug>, slots: 30, justification: "..." }
    - { operator: <operator-slug>, slots: 18, justification: "..." }
    # Must sum to 48

  # ── LIFECYCLE ────────────────────────────────────────────
  auto_supersede: true
  sync_mode: auto | review | manual

  # ── DECOMPOSITION ────────────────────────────────────────
  sections:
    "## Heading":
      contribution_type: extraction
      topics: [...]
      price: 3
---
```

### Key canonical properties

1. **Three reference formats** — `ref:` (handle-path for contributions), `doc:` (local file path for corpus docs), `corpus:` (corpus-prefixed path for remote corpus docs). Never store UUIDs directly — the sync process resolves paths at publish time.
2. **`derived_from` uses floats, converted to slots at publish time** — authors express relative weight as floats; the sync process normalizes and allocates the 28 source slots per the rotator arm (see `economy/wire-rotator-arm.md`). Minimum 1 slot per source. Maximum 28 sources.
3. **`scope` is a single enum** — `unscoped`, `fleet`, or `circle:<name>`. NOT a `circles[]` array + `public` bool.
4. **`maturity` is a single enum** — `draft | design | canon | deprecated`. NOT two separate `draft: bool` and `deprecated: bool` fields.
5. **`supersedes` holds a reference, not a resolved UUID** — at sync time, the path/handle resolves to the target contribution's ID.
6. **`creator_split` uses 48 slots** (not 100%) — matches the rotator arm's 48 creator slots. Must sum to 48. Allocated per operator meta-pool (not per individual agent).
7. **`sections` enables decomposition** — one source document can produce multiple contributions (e.g., a chain bundle with inline skills).

---

## The `WireNativeMetadata` Struct

The Rust type mirrors the canonical schema exactly:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireNativeMetadata {
    // ── ROUTING ──────────────────────────────────────
    pub destination: WireDestination,      // corpus | contribution | both
    pub corpus: Option<String>,             // corpus name when destination includes corpus
    pub contribution_type: WireContributionType, // analysis | assessment | skill | template | action | ...
    pub scope: WireScope,                   // unscoped | fleet | circle:<name>

    // ── IDENTITY ─────────────────────────────────────
    pub topics: Vec<String>,                // topic slugs
    pub entities: Vec<WireEntity>,          // structured entity references
    pub maturity: WireMaturity,             // draft | design | canon | deprecated

    // ── RELATIONSHIPS ────────────────────────────────
    pub derived_from: Vec<WireRef>,         // economic, uses path references
    pub supersedes: Option<WireRefKey>,     // publication chain, uses path reference
    pub related: Vec<WireRelatedRef>,       // non-economic semantic links

    // ── CLAIMS ───────────────────────────────────────
    pub claims: Vec<WireClaim>,

    // ── ECONOMICS ────────────────────────────────────
    pub price: Option<u64>,                 // single price (mutually exclusive with pricing_curve)
    pub pricing_curve: Option<Vec<PricingPoint>>,
    pub embargo_until: Option<String>,      // relative ("+48h") or ISO-8601

    // ── DISTRIBUTION ─────────────────────────────────
    pub pin_to_lists: Vec<String>,
    pub notify_subscribers: bool,

    // ── CIRCLE SPLITS (circle-scoped only) ───────────
    pub creator_split: Vec<CreatorSlotAllocation>,

    // ── LIFECYCLE ────────────────────────────────────
    pub auto_supersede: bool,
    pub sync_mode: WireSyncMode,            // auto | review | manual

    // ── DECOMPOSITION ────────────────────────────────
    pub sections: HashMap<String, SectionOverride>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireDestination { Corpus, Contribution, Both }

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireContributionType {
    // Graph layer types
    Analysis, Assessment, Rebuttal, Extraction, HigherSynthesis, DocumentRecon, CorpusRecon, Sequence,
    // Machine layer types
    Skill, Template, Action,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WireScope {
    Unscoped,
    Fleet,
    Circle { name: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireMaturity { Draft, Design, Canon, Deprecated }

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireSyncMode { Auto, Review, Manual }

/// A derived_from entry — MUST be one of `ref` / `doc` / `corpus`, never a resolved UUID.
/// Path references resolve at sync time via the path→UUID map.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WireRefKey {
    Ref { r#ref: String },           // handle-path: "author/day/seq"
    Doc { doc: String },             // local file path: "wire-actions.md"
    Corpus { corpus: String },       // corpus-prefixed path: "wire-docs/wire-actions.md"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireRef {
    #[serde(flatten)]
    pub key: WireRefKey,
    pub weight: f64,                 // float; normalized to 28 slots at publish time
    pub justification: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireRelatedRef {
    #[serde(flatten)]
    pub key: WireRefKey,
    pub rel: String,                 // "contrasts", "uses", "extends", etc.
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireEntity {
    pub name: String,
    #[serde(rename = "type")]
    pub entity_type: String,         // "org", "person", "concept", "mechanism", "product", etc.
    pub role: String,                // "subject", "referenced", "example", etc.
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireClaim {
    pub text: String,
    pub trackable: bool,
    pub end_date: Option<String>,    // required if trackable == true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingPoint {
    pub credits: u64,
    pub after_hours: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatorSlotAllocation {
    pub operator: String,            // operator slug (lowercase, kebab-case)
    pub slots: u8,                   // 1..=48, sum across entries must equal 48
    pub justification: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionOverride {
    pub contribution_type: Option<WireContributionType>,
    pub topics: Option<Vec<String>>,
    pub entities: Option<Vec<WireEntity>>,
    pub price: Option<u64>,
    pub derived_from: Option<Vec<WireRef>>,
    // ... any field from WireNativeMetadata can be overridden per section
}
```

### Why no UUID in the struct

The canonical format keeps references as paths or handle-paths, resolved at sync time. Our struct matches: `WireRefKey` is an enum of the three canonical reference kinds, never storing a UUID directly. After publish, the backend writes the published Wire contribution's handle-path and UUID to a separate `wire_publication_state` column (see "Publication State" below), not into the `WireNativeMetadata` itself.

This means:
- Wire Native metadata is portable across users (paths resolve against each user's local corpus + the Wire graph)
- Supersession across users works: when user B pulls user A's contribution, user B can refine + supersede, and the `supersedes` reference is a path/handle-path that points at user A's published version
- The metadata serialized to Wire Native YAML always matches the canonical format byte-for-byte

---

## Storage

### `wire_native_metadata_json` column

A new column on `pyramid_config_contributions`:

```sql
ALTER TABLE pyramid_config_contributions
  ADD COLUMN wire_native_metadata_json TEXT NOT NULL DEFAULT '{}';
```

Stores the full `WireNativeMetadata` as JSON. Default on creation:

```json
{
  "destination": "contribution",
  "corpus": null,
  "contribution_type": "template",
  "scope": {"kind": "unscoped"},
  "topics": [],
  "entities": [],
  "maturity": "draft",
  "derived_from": [],
  "supersedes": null,
  "related": [],
  "claims": [],
  "price": null,
  "pricing_curve": null,
  "embargo_until": null,
  "pin_to_lists": [],
  "notify_subscribers": false,
  "creator_split": [],
  "auto_supersede": false,
  "sync_mode": "review",
  "sections": {}
}
```

### `wire_publication_state` column (separate from metadata)

Publication state is stored separately from the canonical metadata so the metadata stays Wire-Native-portable. A new column on `pyramid_config_contributions`:

```sql
ALTER TABLE pyramid_config_contributions
  ADD COLUMN wire_publication_state_json TEXT NOT NULL DEFAULT '{}';
```

The struct:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WirePublicationState {
    pub wire_contribution_id: Option<String>, // Wire UUID once published
    pub handle_path: Option<String>,           // e.g. "playful/77/3"
    pub chain_root: Option<String>,            // Wire UUID of the supersession chain root
    pub chain_head: Option<String>,            // Wire UUID of the current chain head
    pub published_at: Option<String>,          // ISO-8601 timestamp
    pub last_resolved_derived_from: Option<Vec<ResolvedRef>>, // cached resolutions from last publish
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedRef {
    pub key: WireRefKey,                       // original reference
    pub wire_contribution_id: String,          // resolved UUID
    pub handle_path: String,                   // resolved handle-path
    pub weight: f64,
    pub allocated_slots: u8,                   // 28-slot allocation at publish time
}
```

Resolving happens at publish time; the resolved state is cached for inspection but the canonical `WireNativeMetadata` still holds path references.

---

## Creation-Time Capture

Every path that creates a `pyramid_config_contributions` row initializes the metadata. The defaults depend on the origin:

| Creation path | Default metadata init |
|---|---|
| `pyramid_generate_config` (LLM from intent) | `contribution_type` = mapping for the schema_type. `topics` = [`wire-node`, `<schema_type>`]. `maturity` = `draft`. `sync_mode` = `review`. |
| `pyramid_refine_config` (notes-based supersession) | Inherited from prior version. `maturity` = `draft` (re-review needed). `supersedes` = `WireRefKey::Ref { r#ref: prior.handle_path }` if prior is Wire-published, else `None`. |
| `pyramid_propose_config` (agent proposal) | Same as above. Entities include `{ name: agent_name, type: "agent", role: "author" }`. |
| `pyramid_pull_wire_config` (Wire pull) | Full metadata from the pulled Wire contribution, preserved verbatim. `maturity` left as pulled. `supersedes` preserved (points back to any chain the pulled version sat in). |
| Bootstrap migration from legacy tables | Empty defaults. `maturity` = `canon`. `description` via prepare LLM on first publish. |
| Bundled seed contributions (first-run ship) | Full metadata from bundle manifest. `maturity` = `canon`. `scope` = `unscoped`. |

---

## "Prepare" LLM Enrichment

Wire has a `prepare` command that enriches metadata additively. Wire Node mirrors it for local contributions.

### `pyramid_prepare_wire_metadata`

```
POST pyramid_prepare_wire_metadata
  Input: { contribution_id: String }
  Output: { wire_metadata: WireNativeMetadata }
```

### The prepare skill is itself a contribution

The prepare prompt is a `skill` contribution (`schema_type: "skill"`, tags `["prepare", "wire-node"]`). The default seed ships bundled. Users can supersede with their own version — the enrichment algorithm is subject to the same improvement system as every other Wire Node behavior.

### Additive merge rules

| Field | Overwrite behavior |
|---|---|
| `destination` | Fill if at default (`contribution`) and the content implies otherwise |
| `corpus` | Fill if destination includes corpus and corpus is `None` |
| `contribution_type` | Fill if the current value matches the mapping default (template) and the content implies a more specific type |
| `scope` | Never touched — user-controlled |
| `topics` | Union with current, deduped by slug |
| `entities` | Union with current, deduped by `(name, entity_type)` |
| `maturity` | Never touched — lifecycle is user-controlled |
| `derived_from` | Union with current, deduped by `WireRefKey` |
| `supersedes` | Never touched — set only by supersession logic |
| `related` | Union with current, deduped by `(WireRefKey, rel)` |
| `claims` | Union with current, deduped by `text` |
| `price` | Fill only if `None` AND `pricing_curve` is also `None` |
| `pricing_curve` | Fill only if `None` AND `price` is also `None` |
| `embargo_until` | Never touched |
| `pin_to_lists` | Never touched — distribution is user-controlled |
| `notify_subscribers` | Never touched |
| `creator_split` | Never touched — circle allocation is user-controlled |
| `auto_supersede` | Never touched |
| `sync_mode` | Never touched |
| `sections` | Fill if empty AND the content has clear section markers |

The prepare endpoint passes the content + current metadata to the prepare skill, gets back a suggested delta, applies the additive merge on the server, and returns the merged metadata for UI review.

---

## `derived_from` and the Rotator Arm

Per `wire-rotator-arm.md`, contributions allocate **exactly 28 source slots** among `derived_from` entries. Each source gets ≥1 slot; maximum 28 sources. Sum must equal 28.

### Canonical format uses floats

The canonical Wire Native Documents schema shows `weight: 0.3` as a float. This is the author-declared **relative weight**. The sync process at publish time:

1. Reads the floats
2. Normalizes them to sum to 1.0
3. Converts to 28-slot integer allocation via largest-remainder method
4. Enforces: each source ≥1 slot; reject if >28 sources

### Largest-remainder conversion

```rust
fn allocate_28_slots(weights: &[f64]) -> Vec<u8> {
    assert!(weights.len() <= 28, "max 28 sources");
    let sum: f64 = weights.iter().sum();
    let normalized: Vec<f64> = weights.iter().map(|w| w / sum * 28.0).collect();
    // Floor each, then distribute remainders to highest fractional parts
    let mut slots: Vec<u8> = normalized.iter().map(|n| n.floor() as u8).collect();
    let mut remainders: Vec<(usize, f64)> = normalized.iter()
        .enumerate()
        .map(|(i, n)| (i, n - n.floor()))
        .collect();
    let allocated: u8 = slots.iter().sum();
    let remaining = 28 - allocated;
    remainders.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    for i in 0..remaining as usize {
        slots[remainders[i].0] += 1;
    }
    // Enforce minimum 1 per source
    for slot in slots.iter_mut() {
        if *slot == 0 { *slot = 1; }
    }
    // If enforcing minimums pushed us over 28, rebalance
    let total: u8 = slots.iter().sum();
    if total > 28 {
        // Remove slots from the largest allocations until sum = 28
        while slots.iter().sum::<u8>() > 28 {
            let max_idx = slots.iter().enumerate().max_by_key(|(_, &s)| s).unwrap().0;
            slots[max_idx] -= 1;
        }
    }
    slots
}
```

The resolved slot allocation is stored in `wire_publication_state.last_resolved_derived_from` for inspection but the canonical metadata keeps the float weights.

### No-sources case

An action or template with no sources (entirely original) omits `derived_from`. At publish time, all 28 source slots revert to the creator per rotator arm rules: creator gets 48 + 28 = 76/80 (95%), Wire and Graph Fund each get 2.

---

## Custom Chain Bundles and Inline Skills

A `custom_chain` action has a bundled format (chain YAML + all referenced prompts). The bundle format lives in `config-contribution-and-wire-sharing.md` → "Custom Chain Bundle Serialization".

### Publishing a custom chain bundle

The canonical `sections` mechanism handles this cleanly. A custom chain contribution has:

- **Top-level**: the action definition (the chain YAML becomes the action's `action_definition` field at publish time)
- **Sections**: one entry per bundled prompt, each becoming a skill contribution in its own right

Example Wire Native metadata for a custom chain:

```yaml
---
wire:
  destination: contribution
  contribution_type: action
  scope: unscoped
  topics: [code-analysis, chain, wire-node]
  entities:
    - { name: custom-code-pipeline, type: chain, role: subject }
  maturity: design
  derived_from:
    - { doc: "prompts/question/source_extract.md", weight: 0.4, justification: "Primary extraction step" }
    - { doc: "prompts/shared/merge_sub_chunks.md", weight: 0.2, justification: "Chunk merge step" }
  price: 10
  sync_mode: review
  sections:
    "## prompts/question/source_extract.md":
      contribution_type: skill
      topics: [prompt, extraction, wire-node]
      price: 1
    "## prompts/shared/merge_sub_chunks.md":
      contribution_type: skill
      topics: [prompt, merge, wire-node]
      price: 1
---
```

At publish time, the sync process:

1. Publishes each `sections` entry as its own contribution first (the skills)
2. Captures the handle-paths of the published skills
3. Resolves the top-level `derived_from` against those handle-paths
4. Publishes the action with the resolved `derived_from` pointing at the just-published skills
5. Each skill earns its own nano-payment trail; the action earns perpetual invocation fees; the action's `derived_from` routes source chain royalties back to the skills

### If the referenced skills are already published

If the prompts have already been published as standalone skills (separate contributions created by previous publish flows), the bundle's `derived_from` references them directly via `ref:` (handle-path), and `sections` is empty. The author picks between bundling-with-the-action and referencing-pre-published-skills at publish time. The dry-run preview shows both options.

---

## Supersession Across Local and Wire

Local supersession uses the `supersedes_id` column on `pyramid_config_contributions` (see `config-contribution-and-wire-sharing.md`). Wire supersession uses the canonical `WireNativeMetadata.supersedes` field.

### Auto-population on refinement

When a local contribution is refined (notes-based loop), the new version's metadata is initialized from the prior version's metadata with the following overrides:

- `maturity` reset to `draft` (re-review needed before publish)
- `supersedes` set to a `WireRefKey` pointing at the prior version:
  - If prior was Wire-published: `WireRefKey::Ref { r#ref: prior.handle_path }`
  - If prior was pulled from Wire: same (the pulled version's handle-path)
  - If prior was local-only: `None` (can't supersede a non-published entity on Wire; at publish time the new contribution is a fresh root)

### Never silently break a chain

If a prior version was Wire-published and the user tries to publish the refined version without `supersedes` populated, the dry-run preview warns:

> This contribution appears to refine `playful/89/1` but is not marked as superseding it. Publishing without a `supersedes` reference creates a fresh chain root — the publication chain from the prior version will not continue. Set `supersedes` in the metadata or confirm you want a fresh chain.

### Proposed supersessions

Wire supports proposed supersessions (PR-like): agent B proposes a supersession to agent A's chain; A reviews and accepts/rejects. We use this for agent proposals:

- `pyramid_propose_config` creates a local contribution with `source: "proposed"`
- At Wire publish time, if the proposing agent differs from the chain root's author, the metadata is augmented with `proposes_supersession_of` instead of (or in addition to) `supersedes`
- The Wire `wire_contribute` call includes `proposes_supersession_of` and waits for the chain owner's acceptance

### Circle scope extends supersession authority

Per `wire-supersession-chains.md`, circle-scoped contributions can be superseded by any agent assigned to the circle. When our spec publishes a contribution with `scope: { kind: "circle", name: "..." }`, it inherits the circle supersession authority automatically — no local bookkeeping needed.

---

## Circle Splits (48 Slots, Operator Meta-Pools)

When a contribution is circle-scoped (`scope: { kind: "circle", name: "..." }`), the 48 creator slots in the rotator arm are distributed among circle participant **operator meta-pools** via `creator_split`.

### Rules (from `wire-circle-revenue.md`)

- Allocation is between operators, not individual agents — `operator` field is the operator slug
- Must sum to 48
- Each allocation has a required `justification` (audit trail)
- Visible only to circle members (private to the circle)
- Changeable per-contribution — no inheritance from prior supersession

### UI integration

When the user sets `scope` to `circle:<name>` in ToolsMode, the metadata editor:

1. Queries `pyramid_circle_members(circle_name)` to get the list of participating operators
2. Presents an allocation UI with the 48-slot pool and per-operator allocations
3. Requires justifications inline
4. Validates sum = 48 before save

If no `creator_split` is provided for a circle contribution, the Wire sync rejects at dry-run with: "Circle-scoped contribution requires creator_split (48 slots across N operators)."

---

## One-Click Publish Flow

Every contribution already carries the full canonical metadata. Publishing is review + confirm + write.

### Flow

1. User opens ToolsMode → My Tools, selects a contribution to publish
2. User reviews the full Wire Native metadata inline (all sections, via the YAML-to-UI renderer applied to the WireNativeMetadata schema annotation)
3. User clicks "Publish to Wire"
4. Backend validates:
   - `maturity != Draft` (or user confirms publishing a draft)
   - Per-type required fields present (see "Validation" below)
   - For skills: deposit available
   - For circle scope: `creator_split` sums to 48
   - For `derived_from`: all referenced paths/handles resolve against the local path→UUID map; ≤28 sources
   - For `price` vs `pricing_curve`: exactly one is set
   - Credential variable references surface as warnings (see `credentials-and-secrets.md`)
5. Backend calls `pyramid_dry_run_publish` — returns full preview
6. UI shows dry-run preview: visibility, cost breakdown, supersession chain, resolved references with slot allocation, section decomposition preview, warnings
7. User confirms → backend calls `pyramid_publish_to_wire` with `confirm: true`
8. Backend executes the publish:
   - If `sections` has entries: publish each section first (depth-first for nested chain/skill bundles), capture handle-paths
   - Resolve top-level `derived_from` against just-published sections + already-published Wire contributions
   - Allocate 28 source slots via largest-remainder
   - Call `PyramidPublisher::publish_contribution_with_metadata()` → `wire_contribute(...)` with canonical Wire type + resolved references + serialized metadata
9. Wire returns `contribution_id` + `handle_path`
10. Backend writes `WirePublicationState` to the local row
11. UI updates to show "Published" with handle-path link

### Validation per contribution type

| `contribution_type` | Required fields |
|---|---|
| `skill` | `topics` non-empty, `price >= 1` (Wire minimum), content body is valid markdown |
| `template` | `topics` non-empty, `template_definition.applies_to` present, content body is valid YAML/JSON |
| `action` | `topics` non-empty, `action_definition` parses as valid chain schema, permission manifest present |
| `analysis` / `assessment` / etc. (pyramid types) | Use existing `PyramidPublisher` validation, not this path |

### Dry-run preview output

```
POST pyramid_dry_run_publish
  Input: { contribution_id: String }
  Output: {
    wire_type: String,                 // "skill" | "template" | "action"
    wire_tags: [String],
    visibility: String,                // "unscoped" | "fleet" | "circle:nightingale"
    cost_breakdown: {
      deposit_credits: u64,             // for skills
      publish_fee: u64,                 // Wire platform fee
      estimated_total: u64,
    },
    supersession_chain: [
      { handle_path, wire_contribution_id, maturity, published_at }
    ],
    derived_from_resolved: [
      {
        original_ref: WireRefKey,
        resolved_handle_path: String,
        resolved_wire_contribution_id: String,
        weight: f64,
        allocated_slots: u8,
        resolved: bool,                 // false = missing/stale
      }
    ],
    section_decomposition: [
      { section_heading, contribution_type, will_publish: bool }
    ],
    creator_split_resolved: [
      { operator: String, slots: u8, justification: String, operator_exists_on_wire: bool }
    ],
    warnings: [String],
  }
```

Warnings include:

- Credential variable references (see `credentials-and-secrets.md`)
- `derived_from` references that fail to resolve (stale or not-yet-published)
- Section headings that don't match any section in the body
- Missing `end_date` on trackable claims
- Price not set AND pricing_curve not set (both null) for non-draft publish
- Circle scope without `creator_split` populated
- `embargo_until` in the past
- `pricing_curve` that doesn't reach 0 credits (no "free eventually" tail)

---

## Publish IPC

```
# Metadata management
POST pyramid_prepare_wire_metadata
  Input: { contribution_id: String }
  Output: { wire_metadata: WireNativeMetadata }

POST pyramid_update_wire_metadata
  Input: { contribution_id: String, wire_metadata: WireNativeMetadata }
  Output: { ok: bool }

GET pyramid_get_wire_metadata
  Input: { contribution_id: String }
  Output: { wire_metadata: WireNativeMetadata, publication_state: WirePublicationState }

# Publish lifecycle
POST pyramid_dry_run_publish
  Input: { contribution_id: String }
  Output: DryRunPreview (see above)

POST pyramid_publish_to_wire
  Input: { contribution_id: String, confirm: bool }
  Output: { wire_contribution_id: String, handle_path: String, sections_published: u32 }

# Section / bundle composition
GET pyramid_resolve_chain_derived_from
  Input: { contribution_id: String }
  Output: { derived_from: [WireRef], missing_skills: [String] }
```

### Validation enforced at the IPC boundary

- `pyramid_publish_to_wire` rejects `confirm != true`
- `pyramid_publish_to_wire` rejects `maturity == Draft` unless an explicit `force_draft: true` override is passed (separate field; not default)
- `pyramid_update_wire_metadata` rejects:
  - `price.is_some() && pricing_curve.is_some()` (mutually exclusive)
  - `price.map_or(false, |p| p < 1)` (Wire minimum)
  - `scope` is `circle:<name>` but `creator_split` is empty
  - `creator_split` entries sum != 48 (when non-empty)
  - `derived_from.len() > 28`
  - `claims` entry with `trackable: true` but no `end_date`
- `pyramid_prepare_wire_metadata` enforces the additive merge rules server-side (UI cannot bypass)

---

## Wire Type Resolution at Publish Time

```rust
fn wire_type_for_schema(schema_type: &str) -> WireContributionType {
    match schema_type {
        "skill" => WireContributionType::Skill,
        "schema_definition"
        | "schema_annotation"
        | "evidence_policy"
        | "build_strategy"
        | "dadbear_policy"
        | "tier_routing"
        | "step_overrides"
        | "custom_prompts"
        | "folder_ingestion_heuristics" => WireContributionType::Template,
        "custom_chain" => WireContributionType::Action,
        _ => panic!("schema_type {} not mapped; pyramid node types use PyramidPublisher path", schema_type),
    }
}
```

If the metadata's explicit `contribution_type` field differs from this default (e.g., a `schema_type: skill` with explicit `contribution_type: action` due to decomposition), the explicit metadata wins. Sections within a custom chain override normally.

---

## Seed Contributions Ship with the Binary

Wire Node ships with bundled seed contributions for every built-in capability:

- **Skills**: every generation prompt, extraction prompt, merge prompt, heal prompt, prepare prompt, change manifest prompt
- **Templates**: every `schema_definition`, `schema_annotation`, and default `evidence_policy`, `dadbear_policy`, `folder_ingestion_heuristics`, etc.
- **Actions**: default built-in chains (`code_pyramid`, `document_pyramid`, `conversation_pyramid`)

On first run, each is inserted into `pyramid_config_contributions` with:

- `source = "bundled"`
- `status = "active"`
- `wire_native_metadata_json.maturity = Canon`
- `wire_native_metadata_json.destination = Contribution` (users can change before publishing)
- `wire_native_metadata_json.scope = Unscoped`

Users can refine the bundled contributions via notes, publish their own versions to Wire, or pull alternatives. The bundled defaults are starting points, not absolute standards — see `generative-config-pattern.md` → "Seed Defaults Architecture".

### Bundle manifest

Bundled contributions ship in a JSON manifest inside the app binary:

```
wire-node/
  assets/
    bundled_contributions.json
```

### Manifest Format

```json
{
  "manifest_version": 1,
  "app_version": "0.5.0",
  "generated_at": "2026-04-09T00:00:00Z",
  "contributions": [
    {
      "contribution_id": "bundled-skill-source-extract-code-v1",
      "schema_type": "skill",
      "slug": null,
      "yaml_content": "# Source Extractor: Code\n\nYou are an extractor focused on...\n\n## Instructions\n...",
      "wire_native_metadata": {
        "destination": "contribution",
        "corpus": null,
        "contribution_type": "skill",
        "scope": {"kind": "unscoped"},
        "topics": ["prompt", "wire-node", "extraction", "code"],
        "entities": [
          {"name": "source_extract", "type": "prompt", "role": "subject"}
        ],
        "maturity": "canon",
        "derived_from": [],
        "supersedes": null,
        "related": [],
        "claims": [],
        "price": 1,
        "pricing_curve": null,
        "embargo_until": null,
        "pin_to_lists": [],
        "notify_subscribers": false,
        "creator_split": [],
        "auto_supersede": false,
        "sync_mode": "review",
        "sections": {}
      },
      "status": "active",
      "source": "bundled",
      "triggering_note": "Bundled default shipped with wire-node v0.5.0",
      "created_at": "2026-04-09T00:00:00Z"
    },
    {
      "contribution_id": "bundled-schema-definition-evidence-policy-v1",
      "schema_type": "schema_definition",
      "slug": null,
      "yaml_content": "{\"$schema\":\"http://json-schema.org/draft-07/schema#\",\"type\":\"object\",\"required\":[\"triage_rules\",\"demand_signals\",\"budget\"],\"properties\":{...}}",
      "wire_native_metadata": {
        "destination": "contribution",
        "contribution_type": "template",
        "scope": {"kind": "unscoped"},
        "topics": ["schema", "validation", "evidence_policy", "wire-node"],
        "entities": [],
        "maturity": "canon",
        "derived_from": [],
        "supersedes": null,
        "related": [],
        "claims": [],
        "price": 1,
        "sync_mode": "review",
        "sections": {}
      },
      "status": "active",
      "source": "bundled",
      "triggering_note": "Bundled default shipped with wire-node v0.5.0",
      "created_at": "2026-04-09T00:00:00Z"
    }
  ]
}
```

### Manifest Properties

| Field | Type | Purpose |
|---|---|---|
| `manifest_version` | u32 | Format version of the manifest itself; bumps on breaking manifest schema changes |
| `app_version` | String | wire-node release version that shipped this bundle |
| `generated_at` | ISO 8601 | When the manifest was produced (CI build time) |
| `contributions` | Array | Every bundled contribution, one entry per row |

Each contribution entry mirrors the `pyramid_config_contributions` row shape exactly:
- `contribution_id` — must start with `bundled-` prefix, must be unique across the manifest
- `schema_type` — one of the valid schema_types
- `slug` — `null` for global configs, string for per-pyramid configs (bundled seeds are always global, but the field is preserved for uniformity)
- `yaml_content` — the full body (markdown for skills, JSON for schema_definitions, YAML for everything else)
- `wire_native_metadata` — full canonical `WireNativeMetadata` (see this spec's struct definition)
- `status` — always `active` for bundled contributions
- `source` — always `bundled`
- `triggering_note` — human-readable description (shown in the version history UI)
- `created_at` — set to the app release timestamp

### Bootstrap Flow

```rust
pub fn bootstrap_bundled_contributions(
    conn: &Connection,
    app_version: &str,
) -> Result<BootstrapReport> {
    let manifest: BundleManifest = serde_json::from_str(
        include_str!("../../assets/bundled_contributions.json")
    )?;

    let mut inserted = 0;
    let mut skipped = 0;
    let mut upgraded = 0;

    for entry in manifest.contributions {
        // Check if this exact contribution_id is already in the table
        if contribution_exists(conn, &entry.contribution_id)? {
            skipped += 1;
            continue;
        }

        // Insert the bundled contribution
        insert_config_contribution(conn, &entry)?;
        inserted += 1;

        // Run operational sync for each bundled contribution
        sync_config_to_operational(conn, &entry.into_config_contribution())?;
    }

    // Record bootstrap in pyramid_schema_history so we can track which manifests have run
    record_bootstrap_run(conn, app_version, manifest.manifest_version, inserted, skipped)?;

    Ok(BootstrapReport { inserted, skipped, upgraded })
}
```

### App Upgrade Path

On app upgrade (new `app_version`), bootstrap runs again with the new manifest. The new manifest may contain:

- **Same contribution_ids as before** → skipped (already in the table)
- **New contribution_ids (new bundled defaults added)** → inserted as new active contributions
- **New versions of existing contributions** (`bundled-evidence-policy-default-v2` supersedes `v1`) → inserted, user sees them in the version history browser, NOT auto-applied

The app upgrade NEVER auto-supersedes a user's active contribution. If the user has refined `evidence_policy` with their own notes, the refined version remains active after upgrade. The new bundled version is available as an alternative the user can "restore to" via the version history UI.

### Manifest Validation

Before bootstrap inserts any rows, the entire manifest is validated:

- JSON Schema validates the manifest shape
- Each contribution's `yaml_content` is validated against its `schema_definition` (which must also be in the manifest OR already active)
- `contribution_id` uniqueness check across the manifest
- `wire_native_metadata` round-trip test (deserialize, re-serialize, verify equality)

A validation failure aborts the entire bootstrap and surfaces a loud error on app startup. The user is blocked until the manifest is fixed (bug in the shipped binary) or the corrupted app file is reinstalled.

---

## Migration from On-Disk Prompts and Schemas

Pre-spec, prompts live at `chains/prompts/**/*.md` and schemas at `chains/schemas/*.schema.yaml`. Migration:

1. Walk `chains/prompts/` recursively, excluding `_archived/`
2. For each `.md`:
   - Create a `skill` contribution with `schema_type = "skill"`
   - `yaml_content` = the markdown body
   - `source = "bundled"` (shipped) or `"migration"` (user-added)
   - `wire_native_metadata.contribution_type = Skill`
   - `wire_native_metadata.topics = ["prompt", "wire-node", inferred_subtopic]`
3. Walk `chains/schemas/`:
   - For each `.schema.yaml`: create a `schema_annotation` contribution
   - For each `.json` JSON schema: create a `schema_definition` contribution
4. Point all runtime prompt lookups at the contribution store (via a cached lookup indexed by the original path)
5. Leave on-disk files in place for one release cycle as fallback
6. After verification, remove on-disk files in a follow-up release

### Prompt lookup at runtime

The chain executor currently reads `$prompts/question/source_extract.md` as a file path. After migration it resolves:

```rust
fn resolve_prompt(prompt_ref: &str) -> Result<String> {
    // prompt_ref is "$prompts/question/source_extract.md" (legacy) or a handle-path
    let normalized = normalize_prompt_ref(prompt_ref);
    let cached = prompt_lookup_cache().get(&normalized);
    if let Some(skill_id) = cached {
        return load_active_skill_body(skill_id);
    }
    // Fallback: on-disk file (during migration transition)
    fs::read_to_string(on_disk_path(&normalized))
}
```

The cache is invalidated whenever a `skill` contribution is created, updated, or superseded — covers new prompts, edits, and rollbacks.

---

## Files Modified

| Area | Files |
|---|---|
| DB schema | `db.rs` — `wire_native_metadata_json` + `wire_publication_state_json` columns on `pyramid_config_contributions` |
| Metadata struct | New `wire_native_metadata.rs` — `WireNativeMetadata`, `WireRefKey`, `WireRef`, all enums, serde |
| Largest-remainder | New `rotator_allocation.rs` — `allocate_28_slots()` helper |
| Prepare LLM | New `wire_prepare.rs` — enrichment endpoint; uses the bundled prepare skill |
| Publish flow | `wire_publish.rs` — extend `PyramidPublisher` with `publish_contribution_with_metadata()`, `dry_run_publish()`, section-aware recursion |
| Section publish | New `section_publisher.rs` — depth-first publish of bundle sections |
| Seeds / bootstrap | New `bundled_contributions.rs` — load manifest, insert on first run |
| Prompt/schema migration | `db.rs` — walk prompt + schema dirs, create skill/template contributions |
| Prompt lookup cache | New `prompt_cache.rs` — path → active skill_id resolution with invalidation |
| IPC commands | `main.rs` or `routes.rs` — new commands listed in IPC Contract section |
| Frontend | `ToolsMode.tsx` — metadata review form (rendered via `YamlConfigRenderer` against `WireNativeMetadata` schema annotation), dry-run preview modal, publish button, section decomposition viewer |

---

## Implementation Order

1. **Canonical metadata struct** — `WireNativeMetadata` + all sub-types + serde round-trip tests against the canonical YAML examples from `wire-native-documents.md`
2. **DB schema** — add `wire_native_metadata_json` + `wire_publication_state_json` columns
3. **Creation-time capture** — update every path that creates a config contribution to init metadata with correct defaults per the table above
4. **Schema annotation for `WireNativeMetadata`** — so the YAML-to-UI renderer can edit the metadata itself; ship as a bundled template contribution
5. **Bundled contributions bootstrap** — manifest, first-run insert
6. **Prompt and schema migration** — walk disk, create skill/template contributions, point runtime at contribution store via the cache
7. **Largest-remainder slot allocation** — helper + tests
8. **Prepare endpoint** — LLM enrichment with additive merge
9. **Dry-run publish** — preview generator with full resolution + validation
10. **Section publisher** — depth-first publish of `sections` entries before top-level
11. **Publish endpoint** — actual Wire publish + write-back to `wire_publication_state`
12. **Supersession chain carryover** — auto-populate `supersedes` on refinement
13. **Proposed supersessions** — `proposes_supersession_of` for agent proposals on other authors' chains
14. **Frontend** — metadata review form, dry-run modal, publish button, published-state badge

Phases 1-6 are foundational. Phases 7-11 are the publish pipeline. Phases 12-14 are refinements + UI.

---

## Open Questions

1. **Path→UUID map lifetime**: The sync process maintains a path → Wire UUID map (see `wire-native-documents.md` → "Path Resolution"). Does this map belong in `db.rs` as its own table, or can it live inside `wire_publication_state_json`? Recommend: separate table `pyramid_wire_path_map (path TEXT PK, wire_uuid TEXT, handle_path TEXT, updated_at TEXT)`. Shared across all contributions; populated on every successful publish.

2. **Weight semantics for `derived_from` on skills/templates**: Rotator arm uses 28 integer slots for contributions. For skills and templates — which are also contributions — the same rule applies. The canonical `weight: 0.3` format is the authoring convenience; slots are the ground truth. Recommend: enforce the 28-slot rule uniformly at validation time.

3. **`sections` depth limit**: Can a chain bundle reference another chain bundle via sections (recursive decomposition)? Recommend: yes, max depth 3 to prevent accidental explosion. Validate at dry-run time.

4. **Prepare skill for non-markdown contributions**: Skills are markdown. For templates (YAML/JSON body), the prepare skill needs to know how to extract topics/entities from a YAML body. Recommend: ship a separate prepare skill per body format (`prepare:markdown`, `prepare:yaml`, `prepare:json`), selected by the destination contribution's body format.

5. **Maturity transitions in the notes refinement loop**: Each refinement resets `maturity` to `Draft`. But what if the prior version was `Canon`? Recommend: refinement always resets to `Draft`; user can manually promote before publish. Canon→Draft transitions are expected during iteration.
