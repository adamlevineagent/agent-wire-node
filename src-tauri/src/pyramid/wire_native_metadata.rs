// pyramid/wire_native_metadata.rs — Phase 5: canonical Wire Native Documents metadata.
//
// Canonical reference (source of truth, non-negotiable):
//   /Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/wire-native-documents.md
//
// This module mirrors the canonical Wire Native Documents YAML schema in
// Rust. Every field name, every enum variant, every optional/required
// status matches the canonical YAML block described at the END of a
// Wire-native document:
//
//     ---
//     wire:
//       destination: corpus | contribution | both
//       corpus: <name>
//       contribution_type: analysis | assessment | skill | template | action | ...
//       scope: unscoped | fleet | circle:<name>
//       topics: [...]
//       entities: [{ name, type, role }]
//       maturity: draft | design | canon | deprecated
//       derived_from: [{ ref | doc | corpus, weight, justification }]
//       supersedes: <reference-string>
//       related: [{ ref | doc | corpus, rel }]
//       claims: [{ text, trackable, end_date? }]
//       price: <int>  |  pricing_curve: [{ credits, after_hours }]
//       embargo_until: "+48h" | ISO-8601
//       pin_to_lists: [...]
//       notify_subscribers: bool
//       creator_split: [{ operator, slots, justification }]   # circle-scope, sum = 48
//       auto_supersede: bool
//       sync_mode: auto | review | manual
//       sections: { "## Heading": { overrides... } }
//     ---
//
// Canonical alignment is load-bearing. The Rust types produced here
// must serialize to YAML that the Wire's canonical validator can parse
// byte-for-byte, and parse YAML the Wire itself emits. Where the spec
// (`docs/specs/wire-contribution-mapping.md`) and the canonical YAML
// disagree, canonical wins:
//
//   1. `scope` is a flat string (`unscoped`, `fleet`, `circle:<name>`),
//      NOT a tagged-variant object. Serde tagging would produce
//      `{ kind: circle, name: "nightingale" }` which breaks the
//      canonical. Custom (de)serialization keeps it flat.
//   2. `derived_from` / `related` entries are flat records with
//      `ref:` / `doc:` / `corpus:` as mutually-exclusive sibling keys.
//      Not a tagged enum. Custom validation enforces the invariant.
//   3. `supersedes` is a single reference STRING, not a tagged enum.
//      Canonical example: `supersedes: wire-templates.md` or
//      `supersedes: "nightingale/77/3"`.
//   4. `entities[].type` uses the reserved Rust keyword `type` as its
//      YAML key — `#[serde(rename = "type")]` handles it.
//   5. The top-level canonical YAML wraps everything under a `wire:`
//      key inside a `---` block. This module exposes helpers for both
//      the raw `WireNativeMetadata` form AND the wrapped YAML form.
//
// Non-canonical cargo (publication state, resolved slot allocations,
// cached wire contribution IDs) lives in `WirePublicationState`,
// stored separately from `WireNativeMetadata` so the canonical metadata
// stays portable across users.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ── Top-level canonical metadata struct ───────────────────────────────────────

/// The canonical Wire Native Documents metadata, mirroring
/// `wire-native-documents.md` exactly.
///
/// Serializes to the YAML block that goes at the END of a Wire-native
/// document (under the `wire:` key, between `---` fences). Every field
/// name matches the canonical YAML key byte-for-byte.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireNativeMetadata {
    // ── ROUTING ──────────────────────────────────────────────
    pub destination: WireDestination,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corpus: Option<String>,

    pub contribution_type: WireContributionType,

    pub scope: WireScope,

    // ── IDENTITY ─────────────────────────────────────────────
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub topics: Vec<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<WireEntity>,

    pub maturity: WireMaturity,

    // ── RELATIONSHIPS ────────────────────────────────────────
    /// Economic source references. Authors express weights as floats;
    /// the publish pipeline normalizes and allocates 28 rotator-arm
    /// slots via the largest-remainder method. Max 28 sources, min 1
    /// slot each.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub derived_from: Vec<WireRef>,

    /// Publication chain supersession: a single reference STRING per
    /// the canonical schema. Can be a handle-path
    /// (`"nightingale/77/3"`), a doc path (`"wire-templates.md"`), or
    /// a corpus path (`"corpus-name/path.md"`). NOT a tagged enum —
    /// the canonical YAML uses a bare string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,

    /// Non-economic semantic links (graph structure without revenue
    /// flow).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<WireRelatedRef>,

    // ── CLAIMS ───────────────────────────────────────────────
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claims: Vec<WireClaim>,

    // ── ECONOMICS ────────────────────────────────────────────
    /// Single price (mutually exclusive with `pricing_curve`). In
    /// credits. The Wire's minimum is 1 credit per contribution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<u64>,

    /// Time-based pricing curve (mutually exclusive with `price`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing_curve: Option<Vec<WirePricingPoint>>,

    /// Embargo release time — either relative (`"+48h"`) or ISO-8601.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embargo_until: Option<String>,

    // ── DISTRIBUTION ─────────────────────────────────────────
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pin_to_lists: Vec<String>,

    #[serde(default)]
    pub notify_subscribers: bool,

    // ── CIRCLE SPLITS (circle-scoped only) ───────────────────
    /// 48-slot creator allocation across operator meta-pools. Must
    /// sum to 48 when scope is `circle:<name>`. Empty for unscoped /
    /// fleet contributions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub creator_split: Vec<WireCreatorSplit>,

    // ── LIFECYCLE ────────────────────────────────────────────
    #[serde(default)]
    pub auto_supersede: bool,

    pub sync_mode: WireSyncMode,

    // ── DECOMPOSITION ────────────────────────────────────────
    /// Section markers declare contribution boundaries within one
    /// source document. The map key is the section heading
    /// (e.g. `"## Economics"`); the value is a partial override of
    /// the parent metadata for that section.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sections: BTreeMap<String, WireSectionOverride>,
}

// ── Enums ─────────────────────────────────────────────────────────────────────

/// Canonical routing destination. Maps a contribution to the corpus
/// pipeline, the contribution pipeline, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireDestination {
    Corpus,
    Contribution,
    Both,
}

impl Default for WireDestination {
    fn default() -> Self {
        WireDestination::Contribution
    }
}

/// Canonical Wire contribution type vocabulary. The graph layer types
/// (`analysis`, `assessment`, etc.) are for pyramid nodes and narrative
/// contributions; the machine layer types (`skill`, `template`,
/// `action`) are for Phase 5's local-configuration contributions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireContributionType {
    // Graph layer
    Analysis,
    Assessment,
    Rebuttal,
    Extraction,
    HigherSynthesis,
    DocumentRecon,
    CorpusRecon,
    Sequence,
    // Machine layer (Phase 5's scope)
    Skill,
    Template,
    Action,
}

/// Canonical maturity ladder. Drafts are private, designs are
/// reviewable, canon is the authoritative latest, deprecated is
/// explicitly retired. Transitions are user-controlled; the refinement
/// loop always resets a contribution to `Draft`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireMaturity {
    Draft,
    Design,
    Canon,
    Deprecated,
}

impl Default for WireMaturity {
    fn default() -> Self {
        WireMaturity::Draft
    }
}

/// Canonical sync mode. `auto` publishes every change without
/// interaction; `review` stages the change for human approval;
/// `manual` requires an explicit publish action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireSyncMode {
    Auto,
    Review,
    Manual,
}

impl Default for WireSyncMode {
    fn default() -> Self {
        WireSyncMode::Review
    }
}

/// Canonical scope enum. Serializes to a FLAT string per the
/// canonical YAML examples:
///
///     scope: unscoped
///     scope: fleet
///     scope: circle:nightingale
///
/// NOT a tagged-variant object. The spec's `#[serde(tag = "kind")]`
/// form would emit `{ kind: circle, name: "nightingale" }`, which
/// breaks the canonical schema. We implement custom (de)serialization
/// to keep the flat form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireScope {
    Unscoped,
    Fleet,
    Circle(String),
}

impl Default for WireScope {
    fn default() -> Self {
        WireScope::Unscoped
    }
}

impl WireScope {
    /// Format the scope as a canonical string (`unscoped`, `fleet`, or
    /// `circle:<name>`).
    pub fn to_canonical_string(&self) -> String {
        match self {
            WireScope::Unscoped => "unscoped".to_string(),
            WireScope::Fleet => "fleet".to_string(),
            WireScope::Circle(name) => format!("circle:{name}"),
        }
    }

    /// Parse a canonical scope string. Accepts `unscoped`, `fleet`, or
    /// `circle:<name>`. Empty circle names are rejected.
    pub fn from_canonical_string(s: &str) -> Result<Self, String> {
        match s {
            "unscoped" => Ok(WireScope::Unscoped),
            "fleet" => Ok(WireScope::Fleet),
            other => {
                if let Some(name) = other.strip_prefix("circle:") {
                    if name.is_empty() {
                        Err("circle scope requires a non-empty name after 'circle:'".to_string())
                    } else {
                        Ok(WireScope::Circle(name.to_string()))
                    }
                } else {
                    Err(format!(
                        "unknown scope {s:?}; expected 'unscoped', 'fleet', or 'circle:<name>'"
                    ))
                }
            }
        }
    }

    /// Return the circle name if this scope is `Circle(name)`, else None.
    pub fn circle_name(&self) -> Option<&str> {
        match self {
            WireScope::Circle(name) => Some(name.as_str()),
            _ => None,
        }
    }
}

impl Serialize for WireScope {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_canonical_string())
    }
}

impl<'de> Deserialize<'de> for WireScope {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        WireScope::from_canonical_string(&s).map_err(serde::de::Error::custom)
    }
}

// ── Nested types ──────────────────────────────────────────────────────────────

/// Structured entity reference. `type` is a Rust keyword so the YAML
/// key rename is applied via serde.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireEntity {
    pub name: String,
    #[serde(rename = "type")]
    pub entity_type: String,
    pub role: String,
}

/// Economic derived-from entry. The canonical YAML shape is a flat
/// record with `ref` / `doc` / `corpus` as mutually-exclusive sibling
/// keys plus `weight` and `justification`:
///
///     - { ref: "nightingale/77/3", weight: 0.3, justification: "..." }
///     - { doc: wire-actions.md, weight: 0.3, justification: "..." }
///
/// The fields are modeled as three `Option<String>` so the canonical
/// shape round-trips byte-for-byte. Exactly one of `ref`/`doc`/`corpus`
/// must be set on a valid entry — enforce via `validate()`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireRef {
    /// Handle-path for contributions (e.g. `"nightingale/77/3"`).
    #[serde(
        rename = "ref",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub ref_: Option<String>,
    /// Local file path for corpus docs (e.g. `"wire-actions.md"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Corpus-prefixed path for remote corpus docs
    /// (e.g. `"wire-docs/wire-actions.md"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corpus: Option<String>,

    /// Float weight at author time. Normalized to 28-slot integer
    /// allocation at publish time per the rotator arm rules.
    pub weight: f64,

    pub justification: String,
}

impl WireRef {
    /// Validate that exactly one of `ref_`/`doc`/`corpus` is set.
    /// Returns a descriptive error string if the invariant is broken.
    pub fn validate(&self) -> Result<(), String> {
        let count = [self.ref_.is_some(), self.doc.is_some(), self.corpus.is_some()]
            .iter()
            .filter(|x| **x)
            .count();
        match count {
            0 => Err("derived_from entry must set exactly one of ref / doc / corpus".to_string()),
            1 => Ok(()),
            _ => Err("derived_from entry must set only one of ref / doc / corpus".to_string()),
        }
    }

    /// Return the canonical reference string regardless of which field
    /// is set. Panics only if the invariant is already broken (callers
    /// should `validate()` first).
    pub fn canonical_reference(&self) -> String {
        if let Some(r) = &self.ref_ {
            r.clone()
        } else if let Some(d) = &self.doc {
            d.clone()
        } else if let Some(c) = &self.corpus {
            c.clone()
        } else {
            String::new()
        }
    }

    /// Return the kind-tag for this reference: `"ref"`, `"doc"`, or
    /// `"corpus"`.
    pub fn kind(&self) -> &'static str {
        if self.ref_.is_some() {
            "ref"
        } else if self.doc.is_some() {
            "doc"
        } else if self.corpus.is_some() {
            "corpus"
        } else {
            "unknown"
        }
    }
}

/// Non-economic semantic link entry. Same reference-kind shape as
/// `WireRef`, plus a `rel` label instead of a weight/justification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireRelatedRef {
    #[serde(
        rename = "ref",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub ref_: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corpus: Option<String>,

    /// Relation label: `"contrasts"`, `"uses"`, `"extends"`, etc.
    /// Free-form at the canonical layer; the Wire graph may normalize
    /// later.
    pub rel: String,
}

/// Stakeable claim. `trackable: true` makes the claim eligible for
/// reputation scoring after `end_date`; `trackable: false` is an
/// untracked assertion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireClaim {
    pub text: String,
    pub trackable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_date: Option<String>,
}

/// One entry in a pricing curve: `credits` payable for this tier,
/// starting `after_hours` after publication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WirePricingPoint {
    pub credits: u64,
    pub after_hours: u64,
}

/// A circle creator-split entry. `slots` is a per-operator allocation
/// of the 48 creator-pool slots in the rotator arm. The vector's
/// `slots` must sum to EXACTLY 48 for a valid circle-scoped
/// contribution.
///
/// The 48-slot constant is a canonical protocol rule from
/// `wire-circle-revenue.md` + `economy/wire-rotator-arm.md`, NOT a
/// tunable config (Pillar 37 does not apply).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireCreatorSplit {
    pub operator: String,
    pub slots: u32,
    pub justification: String,
}

/// A section-level override for decomposition. All fields are
/// optional — unset fields inherit from the parent metadata.
///
/// This mirrors the canonical YAML:
///
///     sections:
///       "## Economics":
///         contribution_type: extraction
///         topics: [wire-economics]
///         price: 3
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct WireSectionOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contribution_type: Option<WireContributionType>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topics: Option<Vec<String>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entities: Option<Vec<WireEntity>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_from: Option<Vec<WireRef>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<u64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing_curve: Option<Vec<WirePricingPoint>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<WireScope>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maturity: Option<WireMaturity>,
}

// ── Publication state (stored separately from canonical metadata) ─────────────

/// Publication state cached alongside a contribution row. Lives in
/// `pyramid_config_contributions.wire_publication_state_json`. Kept
/// OUT of `WireNativeMetadata` so the canonical metadata stays
/// portable across users — publication IDs are specific to whoever
/// published the contribution.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct WirePublicationState {
    /// Wire UUID once published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire_contribution_id: Option<String>,

    /// Human-readable handle-path (e.g. `"playful/77/3"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle_path: Option<String>,

    /// Wire UUID of the supersession chain root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_root: Option<String>,

    /// Wire UUID of the current chain head.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_head: Option<String>,

    /// ISO-8601 timestamp of the publish call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,

    /// Cached resolution of the most recent publish's `derived_from`.
    /// Stored for inspection only — the canonical `WireNativeMetadata`
    /// still holds the float weights + path references.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub last_resolved_derived_from: Vec<ResolvedDerivedFromEntry>,
}

/// One entry in `WirePublicationState.last_resolved_derived_from`:
/// the original reference, the resolved UUID/handle-path, and the
/// integer slot count allocated by the rotator arm at publish time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedDerivedFromEntry {
    /// Original reference kind: `"ref"`, `"doc"`, or `"corpus"`.
    pub kind: String,
    /// Original reference string (handle-path, doc path, or corpus path).
    pub reference: String,
    /// Author-declared float weight.
    pub weight: f64,
    /// Integer slot allocation from `rotator_allocation::allocate_28_slots`.
    pub allocated_slots: u32,
    /// Resolved Wire contribution UUID, if the reference could be resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire_contribution_id: Option<String>,
    /// Resolved handle-path, if the reference could be resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle_path: Option<String>,
    /// Whether the resolution succeeded at publish time.
    pub resolved: bool,
}

// ── Defaults + helpers ────────────────────────────────────────────────────────

impl Default for WireNativeMetadata {
    /// Empty-default metadata: contribution destination, template
    /// type, unscoped, draft maturity, review sync mode. Callers
    /// typically want `default_wire_native_metadata(schema_type, slug)`
    /// from this module instead — it picks a schema-type-appropriate
    /// `contribution_type` + tags from the Phase 5 mapping table.
    fn default() -> Self {
        Self {
            destination: WireDestination::Contribution,
            corpus: None,
            contribution_type: WireContributionType::Template,
            scope: WireScope::Unscoped,
            topics: Vec::new(),
            entities: Vec::new(),
            maturity: WireMaturity::Draft,
            derived_from: Vec::new(),
            supersedes: None,
            related: Vec::new(),
            claims: Vec::new(),
            price: None,
            pricing_curve: None,
            embargo_until: None,
            pin_to_lists: Vec::new(),
            notify_subscribers: false,
            creator_split: Vec::new(),
            auto_supersede: false,
            sync_mode: WireSyncMode::Review,
            sections: BTreeMap::new(),
        }
    }
}

impl WireNativeMetadata {
    /// Validate the metadata against the canonical invariants. Does
    /// NOT resolve references (that happens at publish time) or check
    /// Wire-specific business rules (e.g. deposit requirements for
    /// skills). Phase 5's publish IPC layers additional validation on
    /// top of this.
    pub fn validate(&self) -> Result<(), String> {
        // Destination / corpus consistency.
        match self.destination {
            WireDestination::Corpus | WireDestination::Both => {
                if self.corpus.as_deref().map(str::is_empty).unwrap_or(true) {
                    return Err(
                        "destination includes corpus but `corpus` field is empty".to_string(),
                    );
                }
            }
            WireDestination::Contribution => {}
        }

        // Price vs pricing_curve mutual exclusion.
        if self.price.is_some() && self.pricing_curve.is_some() {
            return Err("price and pricing_curve are mutually exclusive".to_string());
        }

        // derived_from entries must each set exactly one ref kind.
        // Max 28 sources per the rotator arm rule.
        if self.derived_from.len() > 28 {
            return Err(format!(
                "derived_from has {} entries; maximum is 28 per rotator-arm rules",
                self.derived_from.len()
            ));
        }
        for (i, entry) in self.derived_from.iter().enumerate() {
            entry
                .validate()
                .map_err(|e| format!("derived_from[{i}]: {e}"))?;
            if !(entry.weight.is_finite()) || entry.weight < 0.0 {
                return Err(format!(
                    "derived_from[{i}] weight must be a non-negative finite float"
                ));
            }
        }

        // Related entries must each set exactly one ref kind.
        for (i, entry) in self.related.iter().enumerate() {
            let count = [
                entry.ref_.is_some(),
                entry.doc.is_some(),
                entry.corpus.is_some(),
            ]
            .iter()
            .filter(|x| **x)
            .count();
            if count != 1 {
                return Err(format!(
                    "related[{i}] must set exactly one of ref / doc / corpus"
                ));
            }
        }

        // Claims: trackable claims require end_date.
        for (i, claim) in self.claims.iter().enumerate() {
            if claim.trackable && claim.end_date.as_deref().unwrap_or("").is_empty() {
                return Err(format!(
                    "claims[{i}] is trackable but has no end_date"
                ));
            }
        }

        // Circle scope requires a creator_split summing to 48.
        if matches!(self.scope, WireScope::Circle(_)) {
            if self.creator_split.is_empty() {
                return Err(
                    "circle-scoped contribution requires creator_split (48 slots)".to_string(),
                );
            }
            let sum: u32 = self.creator_split.iter().map(|e| e.slots).sum();
            if sum != 48 {
                return Err(format!(
                    "creator_split sums to {sum}, must equal 48 (canonical protocol constant)"
                ));
            }
            for (i, entry) in self.creator_split.iter().enumerate() {
                if entry.slots == 0 {
                    return Err(format!(
                        "creator_split[{i}] has 0 slots; minimum is 1 per allocation"
                    ));
                }
                if entry.justification.trim().is_empty() {
                    return Err(format!(
                        "creator_split[{i}] requires a justification"
                    ));
                }
            }
        } else if !self.creator_split.is_empty() {
            return Err(
                "creator_split is only valid for circle-scoped contributions".to_string(),
            );
        }

        Ok(())
    }

    /// Serialize the metadata to a canonical-shaped YAML string wrapped
    /// in the `wire:` top-level key (matching the canonical examples
    /// in `wire-native-documents.md`). Does NOT include the `---`
    /// fences — callers wrap those if they need the full document-tail
    /// form.
    pub fn to_canonical_yaml(&self) -> Result<String, serde_yaml::Error> {
        #[derive(Serialize)]
        struct Wrapped<'a> {
            wire: &'a WireNativeMetadata,
        }
        serde_yaml::to_string(&Wrapped { wire: self })
    }

    /// Parse a canonical-shaped YAML string wrapped in `wire:`. The
    /// inverse of `to_canonical_yaml`.
    pub fn from_canonical_yaml(yaml: &str) -> Result<Self, serde_yaml::Error> {
        #[derive(Deserialize)]
        struct Wrapped {
            wire: WireNativeMetadata,
        }
        let wrapped: Wrapped = serde_yaml::from_str(yaml)?;
        Ok(wrapped.wire)
    }

    /// Serialize to JSON (for the `wire_native_metadata_json` column).
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Parse from JSON (for reading the `wire_native_metadata_json`
    /// column). Accepts `"{}"` as an empty-default placeholder (Phase 4
    /// initialized every new row with `"{}"`).
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        let trimmed = json.trim();
        if trimmed.is_empty() || trimmed == "{}" {
            return Ok(WireNativeMetadata::default());
        }
        serde_json::from_str(json)
    }
}

// ── Wire type resolution (Phase 5 mapping table) ──────────────────────────────

/// Resolve a local `schema_type` (the Phase 4 contribution vocabulary)
/// to its canonical Wire contribution type + default tag set.
///
/// Canonical mapping per `docs/specs/wire-contribution-mapping.md`:
///
/// | schema_type                     | Wire type   | Default tags                             |
/// |---------------------------------|-------------|------------------------------------------|
/// | skill                           | skill       | ["prompt", "wire-node"]                  |
/// | schema_definition               | template    | ["schema", "validation"]                 |
/// | schema_annotation               | template    | ["schema", "annotation", "ui"]           |
/// | evidence_policy                 | template    | ["config", "wire-node", "evidence_policy"]|
/// | build_strategy                  | template    | ["config", "wire-node", "build_strategy"]|
/// | dadbear_policy                  | template    | ["config", "wire-node", "dadbear_policy"]|
/// | tier_routing                    | template    | ["config", "wire-node", "tier_routing"]  |
/// | step_overrides                  | template    | ["config", "wire-node", "step_overrides"]|
/// | custom_prompts                  | template    | ["config", "wire-node", "custom_prompts"]|
/// | folder_ingestion_heuristics     | template    | ["config", "wire-node", ...]             |
/// | custom_chains / custom_chain    | action      | ["chain", "wire-node"]                   |
/// | wire_discovery_weights          | template    | ["config", "wire-node", "discovery"]     |
/// | wire_auto_update_settings       | template    | ["config", "wire-node", "auto_update"]   |
///
/// Pyramid node types (L0/L1/apex) do NOT flow through this helper —
/// they publish via `PyramidPublisher::publish_pyramid_node()` and use
/// graph-layer contribution types directly.
///
/// Returns an error for unknown schema_types; the caller decides
/// whether to fail loudly or fall back to a default.
pub fn resolve_wire_type(schema_type: &str) -> Result<(WireContributionType, Vec<String>), String> {
    match schema_type {
        "skill" => Ok((
            WireContributionType::Skill,
            vec!["prompt".to_string(), "wire-node".to_string()],
        )),
        "schema_definition" => Ok((
            WireContributionType::Template,
            vec!["schema".to_string(), "validation".to_string()],
        )),
        "schema_annotation" => Ok((
            WireContributionType::Template,
            vec![
                "schema".to_string(),
                "annotation".to_string(),
                "ui".to_string(),
            ],
        )),
        "evidence_policy" => Ok((
            WireContributionType::Template,
            vec![
                "config".to_string(),
                "wire-node".to_string(),
                "evidence_policy".to_string(),
            ],
        )),
        "build_strategy" => Ok((
            WireContributionType::Template,
            vec![
                "config".to_string(),
                "wire-node".to_string(),
                "build_strategy".to_string(),
            ],
        )),
        "dadbear_policy" => Ok((
            WireContributionType::Template,
            vec![
                "config".to_string(),
                "wire-node".to_string(),
                "dadbear_policy".to_string(),
            ],
        )),
        "tier_routing" => Ok((
            WireContributionType::Template,
            vec![
                "config".to_string(),
                "wire-node".to_string(),
                "tier_routing".to_string(),
            ],
        )),
        "step_overrides" => Ok((
            WireContributionType::Template,
            vec![
                "config".to_string(),
                "wire-node".to_string(),
                "step_overrides".to_string(),
            ],
        )),
        "custom_prompts" => Ok((
            WireContributionType::Template,
            vec![
                "config".to_string(),
                "wire-node".to_string(),
                "custom_prompts".to_string(),
            ],
        )),
        "folder_ingestion_heuristics" => Ok((
            WireContributionType::Template,
            vec![
                "config".to_string(),
                "wire-node".to_string(),
                "folder_ingestion_heuristics".to_string(),
            ],
        )),
        // The Phase 4 dispatcher uses `custom_chains` (plural) as its
        // branch key; the Phase 5 spec mapping table calls it
        // `custom_chain` (singular). Accept both — they mean the same
        // contribution type.
        "custom_chains" | "custom_chain" => Ok((
            WireContributionType::Action,
            vec!["chain".to_string(), "wire-node".to_string()],
        )),
        "wire_discovery_weights" => Ok((
            WireContributionType::Template,
            vec![
                "config".to_string(),
                "wire-node".to_string(),
                "discovery".to_string(),
            ],
        )),
        "wire_auto_update_settings" => Ok((
            WireContributionType::Template,
            vec![
                "config".to_string(),
                "wire-node".to_string(),
                "auto_update".to_string(),
            ],
        )),
        other => Err(format!(
            "unknown schema_type {other:?}; cannot resolve to Wire contribution type"
        )),
    }
}

/// Produce a sensible default `WireNativeMetadata` for a newly-created
/// config contribution. Per the spec's "Creation-Time Capture" table:
/// every contribution starts with `maturity: Draft`, `scope: Unscoped`,
/// `sync_mode: Review`, and a schema-type-specific `contribution_type`
/// + topic tags from the mapping table.
///
/// `slug` (if present) is added to the topic list so the contribution
/// is discoverable by the pyramid it scopes to. `None` slugs produce
/// global-config metadata (no per-slug topic).
pub fn default_wire_native_metadata(
    schema_type: &str,
    slug: Option<&str>,
) -> WireNativeMetadata {
    // Resolve type + default tags. Unknown types fall back to the
    // generic "template" shape so the dispatcher never rejects a
    // creation path on an unrecognized schema — but the mapping table
    // should cover every known vocabulary entry.
    let (contribution_type, mut topics) = match resolve_wire_type(schema_type) {
        Ok((ct, tags)) => (ct, tags),
        Err(_) => (
            WireContributionType::Template,
            vec!["config".to_string(), "wire-node".to_string()],
        ),
    };

    // Add the slug as a topic so the contribution is discoverable via
    // per-pyramid searches. Skip for global configs (no slug).
    if let Some(slug) = slug {
        if !topics.iter().any(|t| t == slug) {
            topics.push(slug.to_string());
        }
    }

    // Skills are bundled on first-run at `canon` maturity; everything
    // else starts as `draft` and promotes through user review.
    // `default_wire_native_metadata` is used for freshly-CREATED rows,
    // so draft is the right default — the bundled-seed migration path
    // overrides maturity explicitly.
    WireNativeMetadata {
        destination: WireDestination::Contribution,
        corpus: None,
        contribution_type,
        scope: WireScope::Unscoped,
        topics,
        entities: Vec::new(),
        maturity: WireMaturity::Draft,
        derived_from: Vec::new(),
        supersedes: None,
        related: Vec::new(),
        claims: Vec::new(),
        price: None,
        pricing_curve: None,
        embargo_until: None,
        pin_to_lists: Vec::new(),
        notify_subscribers: false,
        creator_split: Vec::new(),
        auto_supersede: false,
        sync_mode: WireSyncMode::Review,
        sections: BTreeMap::new(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_serializes_flat() {
        let scope = WireScope::Unscoped;
        let yaml = serde_yaml::to_string(&scope).unwrap();
        assert_eq!(yaml.trim(), "unscoped");

        let scope = WireScope::Fleet;
        let yaml = serde_yaml::to_string(&scope).unwrap();
        assert_eq!(yaml.trim(), "fleet");

        let scope = WireScope::Circle("nightingale".to_string());
        let yaml = serde_yaml::to_string(&scope).unwrap();
        assert_eq!(yaml.trim(), "circle:nightingale");
    }

    #[test]
    fn scope_round_trips() {
        let cases = [
            WireScope::Unscoped,
            WireScope::Fleet,
            WireScope::Circle("playful".to_string()),
        ];
        for scope in &cases {
            let yaml = serde_yaml::to_string(scope).unwrap();
            let parsed: WireScope = serde_yaml::from_str(&yaml).unwrap();
            assert_eq!(&parsed, scope);
        }
    }

    #[test]
    fn scope_rejects_invalid() {
        let err = WireScope::from_canonical_string("circle:").unwrap_err();
        assert!(err.contains("non-empty"));

        let err = WireScope::from_canonical_string("garbage").unwrap_err();
        assert!(err.contains("unknown scope"));
    }

    #[test]
    fn wire_ref_validates_exclusive() {
        let valid = WireRef {
            ref_: Some("nightingale/77/3".to_string()),
            doc: None,
            corpus: None,
            weight: 0.5,
            justification: "source".to_string(),
        };
        valid.validate().unwrap();

        let none = WireRef {
            ref_: None,
            doc: None,
            corpus: None,
            weight: 0.5,
            justification: "source".to_string(),
        };
        assert!(none.validate().is_err());

        let multi = WireRef {
            ref_: Some("x/1/1".to_string()),
            doc: Some("d.md".to_string()),
            corpus: None,
            weight: 0.5,
            justification: "source".to_string(),
        };
        assert!(multi.validate().is_err());
    }

    #[test]
    fn canonical_round_trip_minimal() {
        let meta = default_wire_native_metadata("evidence_policy", Some("my-slug"));
        let yaml = meta.to_canonical_yaml().unwrap();
        let parsed = WireNativeMetadata::from_canonical_yaml(&yaml).unwrap();
        assert_eq!(parsed, meta);
    }

    #[test]
    fn canonical_round_trip_full() {
        let mut meta = WireNativeMetadata {
            destination: WireDestination::Both,
            corpus: Some("wire-design-docs".to_string()),
            contribution_type: WireContributionType::Analysis,
            scope: WireScope::Circle("nightingale".to_string()),
            topics: vec!["wire-architecture".to_string(), "actions".to_string()],
            entities: vec![
                WireEntity {
                    name: "TSMC".to_string(),
                    entity_type: "org".to_string(),
                    role: "subject".to_string(),
                },
                WireEntity {
                    name: "Rotator Arm".to_string(),
                    entity_type: "mechanism".to_string(),
                    role: "referenced".to_string(),
                },
            ],
            maturity: WireMaturity::Design,
            derived_from: vec![
                WireRef {
                    ref_: Some("nightingale/77/3".to_string()),
                    doc: None,
                    corpus: None,
                    weight: 0.3,
                    justification: "Baltic analysis".to_string(),
                },
                WireRef {
                    ref_: None,
                    doc: Some("wire-actions.md".to_string()),
                    corpus: None,
                    weight: 0.3,
                    justification: "details the action system".to_string(),
                },
                WireRef {
                    ref_: None,
                    doc: None,
                    corpus: Some("wire-docs/synthesis-primitives.md".to_string()),
                    weight: 0.2,
                    justification: "vocabulary source".to_string(),
                },
            ],
            supersedes: Some("wire-templates.md".to_string()),
            related: vec![WireRelatedRef {
                ref_: None,
                doc: Some("wire-skills.md".to_string()),
                corpus: None,
                rel: "contrasts".to_string(),
            }],
            claims: vec![WireClaim {
                text: "Four operation types are sufficient".to_string(),
                trackable: true,
                end_date: Some("2026-09-01".to_string()),
            }],
            price: Some(5),
            pricing_curve: None,
            embargo_until: Some("+48h".to_string()),
            pin_to_lists: vec!["compiler-updates".to_string()],
            notify_subscribers: true,
            creator_split: vec![
                WireCreatorSplit {
                    operator: "playful-universe".to_string(),
                    slots: 30,
                    justification: "architecture and design".to_string(),
                },
                WireCreatorSplit {
                    operator: "partner-agent".to_string(),
                    slots: 18,
                    justification: "synthesis and documentation".to_string(),
                },
            ],
            auto_supersede: true,
            sync_mode: WireSyncMode::Review,
            sections: BTreeMap::new(),
        };
        meta.sections.insert(
            "## Economics".to_string(),
            WireSectionOverride {
                contribution_type: Some(WireContributionType::Extraction),
                topics: Some(vec!["wire-economics".to_string()]),
                price: Some(3),
                ..Default::default()
            },
        );
        meta.validate().unwrap();

        let yaml = meta.to_canonical_yaml().unwrap();
        let parsed = WireNativeMetadata::from_canonical_yaml(&yaml).unwrap();
        assert_eq!(parsed, meta);

        // Second round-trip — emitting the parsed version should
        // produce identical YAML.
        let yaml2 = parsed.to_canonical_yaml().unwrap();
        assert_eq!(yaml, yaml2);
    }

    #[test]
    fn canonical_yaml_has_wire_wrapper() {
        let meta = default_wire_native_metadata("skill", Some("my-slug"));
        let yaml = meta.to_canonical_yaml().unwrap();
        assert!(
            yaml.starts_with("wire:"),
            "canonical YAML must wrap metadata under top-level `wire:` key, got: {yaml}"
        );
    }

    #[test]
    fn canonical_parses_bare_derived_from() {
        // Round-trip a derived_from entry from the canonical example
        // in `wire-native-documents.md` line 49:
        //   - { ref: "nightingale/77/3", weight: 0.3, justification: "Baltic analysis" }
        let yaml = r#"
wire:
  destination: contribution
  contribution_type: analysis
  scope: unscoped
  maturity: draft
  sync_mode: review
  derived_from:
    - { ref: "nightingale/77/3", weight: 0.3, justification: "Baltic analysis" }
    - { doc: "wire-actions.md", weight: 0.7, justification: "source spec" }
"#;
        let meta = WireNativeMetadata::from_canonical_yaml(yaml).unwrap();
        assert_eq!(meta.derived_from.len(), 2);
        assert_eq!(meta.derived_from[0].ref_.as_deref(), Some("nightingale/77/3"));
        assert!(meta.derived_from[0].doc.is_none());
        assert_eq!(meta.derived_from[0].weight, 0.3);
        assert_eq!(meta.derived_from[1].doc.as_deref(), Some("wire-actions.md"));
        meta.derived_from[0].validate().unwrap();
        meta.derived_from[1].validate().unwrap();
    }

    #[test]
    fn validate_rejects_price_and_curve_together() {
        let mut meta = default_wire_native_metadata("skill", None);
        meta.price = Some(5);
        meta.pricing_curve = Some(vec![WirePricingPoint {
            credits: 3,
            after_hours: 0,
        }]);
        let err = meta.validate().unwrap_err();
        assert!(err.contains("mutually exclusive"));
    }

    #[test]
    fn validate_rejects_corpus_destination_without_corpus_name() {
        let mut meta = default_wire_native_metadata("skill", None);
        meta.destination = WireDestination::Corpus;
        let err = meta.validate().unwrap_err();
        assert!(err.contains("corpus"));
    }

    #[test]
    fn validate_rejects_too_many_derived_from() {
        let mut meta = default_wire_native_metadata("skill", None);
        for i in 0..29 {
            meta.derived_from.push(WireRef {
                ref_: Some(format!("author/1/{i}")),
                doc: None,
                corpus: None,
                weight: 1.0,
                justification: "src".to_string(),
            });
        }
        let err = meta.validate().unwrap_err();
        assert!(err.contains("maximum is 28"));
    }

    #[test]
    fn validate_rejects_circle_without_creator_split() {
        let mut meta = default_wire_native_metadata("skill", None);
        meta.scope = WireScope::Circle("nightingale".to_string());
        let err = meta.validate().unwrap_err();
        assert!(err.contains("creator_split"));
    }

    #[test]
    fn validate_rejects_creator_split_not_summing_to_48() {
        let mut meta = default_wire_native_metadata("skill", None);
        meta.scope = WireScope::Circle("nightingale".to_string());
        meta.creator_split = vec![WireCreatorSplit {
            operator: "op-a".to_string(),
            slots: 20,
            justification: "a".to_string(),
        }];
        let err = meta.validate().unwrap_err();
        assert!(err.contains("48"));
    }

    #[test]
    fn validate_accepts_valid_circle_split() {
        let mut meta = default_wire_native_metadata("skill", None);
        meta.scope = WireScope::Circle("nightingale".to_string());
        meta.creator_split = vec![
            WireCreatorSplit {
                operator: "op-a".to_string(),
                slots: 30,
                justification: "primary".to_string(),
            },
            WireCreatorSplit {
                operator: "op-b".to_string(),
                slots: 18,
                justification: "secondary".to_string(),
            },
        ];
        meta.validate().unwrap();
    }

    #[test]
    fn validate_rejects_trackable_claim_without_end_date() {
        let mut meta = default_wire_native_metadata("skill", None);
        meta.claims = vec![WireClaim {
            text: "bold claim".to_string(),
            trackable: true,
            end_date: None,
        }];
        let err = meta.validate().unwrap_err();
        assert!(err.contains("trackable"));
    }

    #[test]
    fn validate_rejects_non_circle_with_creator_split() {
        let mut meta = default_wire_native_metadata("skill", None);
        meta.creator_split = vec![WireCreatorSplit {
            operator: "op-a".to_string(),
            slots: 48,
            justification: "solo".to_string(),
        }];
        let err = meta.validate().unwrap_err();
        assert!(err.contains("circle-scoped"));
    }

    #[test]
    fn resolve_wire_type_maps_every_known_schema_type() {
        let cases = [
            ("skill", WireContributionType::Skill),
            ("schema_definition", WireContributionType::Template),
            ("schema_annotation", WireContributionType::Template),
            ("evidence_policy", WireContributionType::Template),
            ("build_strategy", WireContributionType::Template),
            ("dadbear_policy", WireContributionType::Template),
            ("tier_routing", WireContributionType::Template),
            ("step_overrides", WireContributionType::Template),
            ("custom_prompts", WireContributionType::Template),
            ("folder_ingestion_heuristics", WireContributionType::Template),
            ("custom_chains", WireContributionType::Action),
            ("custom_chain", WireContributionType::Action),
            ("wire_discovery_weights", WireContributionType::Template),
            ("wire_auto_update_settings", WireContributionType::Template),
        ];
        for (schema_type, expected) in cases {
            let (actual, tags) = resolve_wire_type(schema_type).unwrap();
            assert_eq!(actual, expected, "schema_type {schema_type}");
            assert!(
                !tags.is_empty(),
                "expected non-empty default tags for {schema_type}"
            );
        }
        assert!(resolve_wire_type("totally_unknown").is_err());
    }

    #[test]
    fn default_metadata_picks_correct_contribution_type() {
        let meta = default_wire_native_metadata("skill", Some("my-pyramid"));
        assert_eq!(meta.contribution_type, WireContributionType::Skill);
        assert!(meta.topics.iter().any(|t| t == "prompt"));
        assert!(meta.topics.iter().any(|t| t == "wire-node"));
        assert!(meta.topics.iter().any(|t| t == "my-pyramid"));
        assert_eq!(meta.maturity, WireMaturity::Draft);
        assert!(matches!(meta.scope, WireScope::Unscoped));

        let meta = default_wire_native_metadata("dadbear_policy", Some("my-pyramid"));
        assert_eq!(meta.contribution_type, WireContributionType::Template);

        let meta = default_wire_native_metadata("custom_chain", None);
        assert_eq!(meta.contribution_type, WireContributionType::Action);
    }

    #[test]
    fn publication_state_defaults_empty() {
        let state = WirePublicationState::default();
        assert!(state.wire_contribution_id.is_none());
        assert!(state.last_resolved_derived_from.is_empty());
    }

    #[test]
    fn publication_state_round_trips() {
        let state = WirePublicationState {
            wire_contribution_id: Some("wire-uuid".to_string()),
            handle_path: Some("playful/77/3".to_string()),
            chain_root: Some("root-uuid".to_string()),
            chain_head: Some("head-uuid".to_string()),
            published_at: Some("2026-04-10T12:00:00Z".to_string()),
            last_resolved_derived_from: vec![ResolvedDerivedFromEntry {
                kind: "doc".to_string(),
                reference: "wire-actions.md".to_string(),
                weight: 0.5,
                allocated_slots: 14,
                wire_contribution_id: Some("src-uuid".to_string()),
                handle_path: Some("author/1/1".to_string()),
                resolved: true,
            }],
        };
        let json = serde_json::to_string(&state).unwrap();
        let parsed: WirePublicationState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, state);
    }

    #[test]
    fn from_json_accepts_empty_placeholder() {
        let meta = WireNativeMetadata::from_json("{}").unwrap();
        assert_eq!(meta, WireNativeMetadata::default());
        let meta = WireNativeMetadata::from_json("  ").unwrap();
        assert_eq!(meta, WireNativeMetadata::default());
    }

    #[test]
    fn from_json_parses_full_struct() {
        let meta = default_wire_native_metadata("skill", Some("my-slug"));
        let json = meta.to_json().unwrap();
        let parsed = WireNativeMetadata::from_json(&json).unwrap();
        assert_eq!(parsed, meta);
    }
}
