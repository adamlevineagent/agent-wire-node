// pyramid/yaml_renderer.rs — Phase 8: YAML-to-UI renderer backend.
//
// Per `docs/specs/yaml-to-ui-renderer.md`. The Phase 8 backend layer
// that feeds the generic React `YamlConfigRenderer` component. Three
// responsibilities:
//
//   1. Load the active `schema_annotation` contribution for a given
//      target config schema_type (the YAML body describes how to
//      render fields for that config).
//   2. Resolve dynamic option sources (`tier_registry`, `provider_list`,
//      `model_list:{provider}`, `node_fields`, `chain_list`,
//      `prompt_files`) to concrete option lists the renderer can
//      hand to `select`/`model_selector` widgets.
//   3. Estimate per-call cost from `pyramid_tier_routing.pricing_json`
//      for fields that carry `show_cost: true`.
//
// **Architectural note (Phase 4 / Phase 5 alignment):** schema
// annotations are loaded from `pyramid_config_contributions` where
// `schema_type = 'schema_annotation'`, NOT from disk. Disk files in
// `chains/schemas/` are seed data that `wire_migration::
// migrate_schema_annotations_to_contributions` walks on first run
// and inserts as contributions. Runtime reads never touch disk.
//
// The annotation's YAML body (stored in
// `pyramid_config_contributions.yaml_content`) is itself a
// `SchemaAnnotation` document keyed on the `applies_to` field —
// `applies_to: chain_step_config`, etc. `pyramid_get_schema_annotation`
// takes a target schema_type and scans active annotation
// contributions for a matching `applies_to`.

use anyhow::{anyhow, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tracing::{debug, warn};

use crate::pyramid::config_contributions::load_active_config_contribution;
use crate::pyramid::provider::{ProviderRegistry, TierRoutingEntry};

// ── SchemaAnnotation types ──────────────────────────────────────────────────

/// The top-level schema annotation document. Mirrors the TypeScript
/// `SchemaAnnotation` interface in `src/types/yamlRenderer.ts` so the
/// frontend can deserialize directly from the IPC response.
///
/// `applies_to` is the target config schema_type — the key the frontend
/// passes to `pyramid_get_schema_annotation`. It lets a single
/// `schema_annotation` contribution describe how to render a
/// particular config type without the caller needing to know which
/// contribution row to look up.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaAnnotation {
    /// Canonical annotation identifier. Spec shows `schema_type:
    /// chain_step_config`; we carry it through as-is for caller
    /// compatibility.
    pub schema_type: String,
    /// Annotation file version (integer). Incremented when the
    /// annotation shape changes.
    pub version: u32,
    /// The target config schema_type this annotation applies to. For
    /// Phase 8, the lookup key. Defaults to `schema_type` when absent
    /// so simple annotation files don't need to repeat themselves.
    #[serde(default)]
    pub applies_to: Option<String>,
    /// Optional display label for the annotation itself (shown above
    /// the form as a header). Falls back to the schema_type.
    #[serde(default)]
    pub label: Option<String>,
    /// Optional descriptive text for the form header.
    #[serde(default)]
    pub description: Option<String>,
    /// Field-level annotations keyed by dotted field path (relative to
    /// the rendered scope). The order the fields appear in the map
    /// is not guaranteed across YAML→JSON→Rust→frontend, so
    /// annotations that depend on order should use the `order` field
    /// on each field annotation.
    #[serde(default)]
    pub fields: BTreeMap<String, FieldAnnotation>,
}

/// Per-field annotation describing how to render a single field. The
/// spec's "Field Annotation Properties" table is mirrored here 1:1.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FieldAnnotation {
    /// Human-readable field name shown above the widget.
    pub label: String,
    /// Tooltip/description explaining what this field does.
    pub help: String,
    /// Widget type name. See `WidgetType` in the TypeScript contract.
    pub widget: String,
    /// One of `basic`, `advanced`, `hidden`.
    pub visibility: String,
    /// Dotted path to the field this inherits from (shows "← tier
    /// default" in UI when the current value matches the resolved
    /// default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherits_from: Option<String>,
    /// Whether to display an estimated cost-per-call next to this
    /// field. Used for model_tier fields where the backend can
    /// compute a cost from the tier routing table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_cost: Option<bool>,
    /// Static options for `select` widgets (mutually exclusive with
    /// `options_from`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<OptionValue>,
    /// Dynamic option source name. Resolved at mount time via
    /// `yaml_renderer_resolve_options`. See the Phase 8 spec's
    /// "Dynamic Option Sources" table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options_from: Option<String>,
    /// Minimum value for `number`/`slider` widgets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    /// Maximum value for `number`/`slider` widgets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// Step size for `number`/`slider` widgets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    /// Unit label shown after the value (e.g. "tokens", "ms").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
    /// Widget type for items in a `list` widget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_widget: Option<String>,
    /// Dynamic options source for list item widgets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_options_from: Option<String>,
    /// Named group for visual organization (multiple fields with the
    /// same group are rendered under a shared heading).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Show-but-don't-allow-editing flag. For annotation-driven
    /// readonly widgets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only: Option<bool>,
    /// Conditional visibility expression (e.g.
    /// `"split_strategy != null"`). Phase 8 ships the type but the
    /// renderer does not yet evaluate conditions — deferred to
    /// Phase 10.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    /// Explicit display order within a group. Lower numbers render
    /// first. Breaks ties from the BTreeMap's alphabetic key order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<i64>,
}

/// A single option entry for a `select` or `model_selector` widget.
/// Both static (in the annotation file) and dynamic (from
/// `yaml_renderer_resolve_options`) option values use this shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionValue {
    /// The value to write to the YAML when this option is selected.
    pub value: String,
    /// The human-readable label shown in the dropdown.
    pub label: String,
    /// Optional secondary text (shown as a subtitle or tooltip).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional extra data (e.g. context_window for a model option).
    /// Opaque to the renderer — the widget layer can read it for
    /// richer display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Value>,
}

// ── Schema annotation loader ────────────────────────────────────────────────

/// Load the schema annotation for the given target config `schema_type`
/// (e.g. `chain_step_config`, `dadbear_config`). Scans every active
/// `schema_annotation` contribution, parses its YAML body, and
/// returns the first one whose `applies_to` (or `schema_type`)
/// matches.
///
/// Returns `Ok(None)` if no matching annotation is found — the
/// frontend can fall back to a generic key/value editor in that case.
///
/// **Phase 4/5 alignment:** annotations live in
/// `pyramid_config_contributions`, not disk. The Phase 5 migration
/// (extended in Phase 8 to walk `chains/schemas/`) seeds them on
/// first run.
pub fn load_schema_annotation_for(
    conn: &Connection,
    target_schema_type: &str,
) -> Result<Option<SchemaAnnotation>> {
    // First, try the direct path: a contribution whose slug equals the
    // target type. This is the common case — the Phase 5+ migration
    // creates one row per `.schema.yaml` file keyed by the `applies_to`
    // value.
    if let Some(contribution) =
        load_active_config_contribution(conn, "schema_annotation", Some(target_schema_type))?
    {
        if let Some(annotation) = try_parse_annotation(&contribution.yaml_content, target_schema_type) {
            return Ok(Some(annotation));
        } else {
            warn!(
                target_schema_type,
                contribution_id = %contribution.contribution_id,
                "schema_annotation contribution for target did not parse into SchemaAnnotation; \
                 falling back to scan"
            );
        }
    }

    // Fallback: scan every active schema_annotation contribution and
    // parse each body. A contribution file might apply to multiple
    // config types or use `schema_type` as the lookup key rather than
    // `applies_to`. This fallback ensures the first-run seeded files
    // work regardless of how they name the field.
    let mut stmt = conn.prepare(
        "SELECT contribution_id, yaml_content
         FROM pyramid_config_contributions
         WHERE schema_type = 'schema_annotation'
           AND status = 'active'
           AND superseded_by_id IS NULL
         ORDER BY created_at DESC, id DESC",
    )?;

    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;

    for row in rows {
        let (contribution_id, yaml_content) = row?;
        if let Some(annotation) = try_parse_annotation(&yaml_content, target_schema_type) {
            return Ok(Some(annotation));
        } else {
            debug!(
                contribution_id,
                "schema_annotation contribution body did not parse or did not match target"
            );
        }
    }

    Ok(None)
}

/// Parse a YAML body into a `SchemaAnnotation` and return it only if
/// it targets the requested config schema_type. Returns `None` on
/// parse failure or on a mismatch — the caller is responsible for
/// logging its own diagnostic.
fn try_parse_annotation(yaml_content: &str, target_schema_type: &str) -> Option<SchemaAnnotation> {
    let annotation: SchemaAnnotation = serde_yaml::from_str(yaml_content).ok()?;
    let effective_target = annotation
        .applies_to
        .as_deref()
        .unwrap_or(annotation.schema_type.as_str());
    if effective_target == target_schema_type {
        Some(annotation)
    } else {
        None
    }
}

/// Look up the active annotation contribution by its raw
/// contribution_id. Used by tests and by the Phase 8 migration path to
/// verify a specific insertion.
pub fn load_annotation_by_contribution_id(
    conn: &Connection,
    contribution_id: &str,
) -> Result<Option<SchemaAnnotation>> {
    let row = conn
        .query_row(
            "SELECT yaml_content FROM pyramid_config_contributions
             WHERE contribution_id = ?1 AND schema_type = 'schema_annotation'",
            rusqlite::params![contribution_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(yaml_content) = row else {
        return Ok(None);
    };
    let annotation: SchemaAnnotation = serde_yaml::from_str(&yaml_content)
        .map_err(|e| anyhow!("schema_annotation body failed to parse: {e}"))?;
    Ok(Some(annotation))
}

// ── Dynamic option resolution ───────────────────────────────────────────────

/// Resolve a dynamic option source name to a concrete list of
/// `OptionValue`s. The renderer calls this once per unique source at
/// mount time and caches the result.
///
/// Sources supported in Phase 8:
///   - `tier_registry` — tier names with provider/model/context metadata
///   - `provider_list` — registered providers
///   - `model_list:{provider_id}` — models routed through a specific
///     provider (from tier routing; Phase 10 adds a live Ollama
///     `/api/tags` query)
///   - `node_fields` — top-level pyramid node schema field names
///   - `chain_list` — custom chain contributions + their content_type
///   - `prompt_files` — skill contributions (paths)
///
/// Unknown sources return `Ok(vec![])` and log a warning — missing
/// options are not fatal; the select widget shows an empty list and
/// the user sees the raw value.
pub fn resolve_option_source(
    conn: &Connection,
    provider_registry: &ProviderRegistry,
    source: &str,
) -> Result<Vec<OptionValue>> {
    // Handle parameterized `model_list:{provider_id}` form first.
    if let Some(provider_id) = source.strip_prefix("model_list:") {
        return Ok(resolve_model_list_for(provider_registry, provider_id));
    }

    match source {
        "tier_registry" => Ok(resolve_tier_registry(provider_registry)),
        "provider_list" => Ok(resolve_provider_list(provider_registry)),
        "node_fields" => Ok(resolve_node_fields()),
        "chain_list" => Ok(resolve_chain_list(conn)?),
        "prompt_files" => Ok(resolve_prompt_files(conn)?),
        other => {
            warn!(
                source = other,
                "yaml_renderer_resolve_options: unknown source; returning empty list"
            );
            Ok(Vec::new())
        }
    }
}

/// `tier_registry` resolver — one entry per tier row with the
/// provider+model+context_window as metadata. The label is the
/// tier_name; the description is the human-readable description in
/// the tier routing table.
fn resolve_tier_registry(registry: &ProviderRegistry) -> Vec<OptionValue> {
    let mut out: Vec<OptionValue> = registry
        .list_tier_routing()
        .into_iter()
        .map(tier_entry_to_option)
        .collect();
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

/// Convert a `TierRoutingEntry` into an `OptionValue`. Exposes the
/// provider id, model id, context window, and per-token pricing as
/// metadata so the `model_selector` composite widget can render rich
/// provider+model+context blurbs without a second IPC round-trip.
fn tier_entry_to_option(entry: TierRoutingEntry) -> OptionValue {
    let prompt_price = entry.prompt_price_per_token();
    let completion_price = entry.completion_price_per_token();
    let description = match entry.notes.as_deref() {
        Some(notes) if !notes.trim().is_empty() => Some(notes.to_string()),
        _ => Some(format!("{} via {}", entry.model_id, entry.provider_id)),
    };

    let meta = serde_json::json!({
        "provider_id": entry.provider_id,
        "model_id": entry.model_id,
        "context_limit": entry.context_limit,
        "max_completion_tokens": entry.max_completion_tokens,
        "prompt_price_per_token": prompt_price,
        "completion_price_per_token": completion_price,
    });

    OptionValue {
        value: entry.tier_name.clone(),
        label: entry.tier_name,
        description,
        meta: Some(meta),
    }
}

/// `provider_list` resolver — one entry per registered provider. The
/// value is the provider id; the label is its display_name.
fn resolve_provider_list(registry: &ProviderRegistry) -> Vec<OptionValue> {
    let mut out: Vec<OptionValue> = registry
        .list_providers()
        .into_iter()
        .map(|p| OptionValue {
            value: p.id.clone(),
            label: p.display_name.clone(),
            description: Some(format!(
                "{} · {}",
                p.provider_type.as_str(),
                if p.enabled { "enabled" } else { "disabled" }
            )),
            meta: Some(serde_json::json!({
                "provider_type": p.provider_type.as_str(),
                "base_url": p.base_url,
                "enabled": p.enabled,
            })),
        })
        .collect();
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

/// `model_list:{provider_id}` resolver — returns the models the tier
/// routing table has routed through the given provider. This is the
/// "what's configured" view, not the "what's available on the remote"
/// view — Phase 10 adds the Ollama `/api/tags` live query for that.
fn resolve_model_list_for(registry: &ProviderRegistry, provider_id: &str) -> Vec<OptionValue> {
    let mut out: Vec<OptionValue> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for entry in registry.list_tier_routing() {
        if entry.provider_id != provider_id {
            continue;
        }
        if !seen.insert(entry.model_id.clone()) {
            continue;
        }
        out.push(OptionValue {
            value: entry.model_id.clone(),
            label: entry.model_id.clone(),
            description: entry.notes.clone().or_else(|| {
                entry
                    .context_limit
                    .map(|n| format!("context: {n} tokens"))
            }),
            meta: Some(serde_json::json!({
                "provider_id": entry.provider_id,
                "context_limit": entry.context_limit,
                "max_completion_tokens": entry.max_completion_tokens,
            })),
        });
    }
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

/// `node_fields` resolver — static list of top-level pyramid node
/// schema field names. Drives dehydration rules in chain step
/// annotations. The list mirrors the fields the executor
/// reads/writes on node rows. Keep this in sync with
/// `extraction_schema.rs` when new fields are added.
fn resolve_node_fields() -> Vec<OptionValue> {
    const FIELDS: &[(&str, &str)] = &[
        ("headline", "Headline — the node's pithy title line"),
        ("distilled", "Distilled summary — the primary body text"),
        ("topics", "Topics — tag set for classification"),
        ("terms", "Terms — vocabulary entries defined in this node"),
        ("decisions", "Decisions — committed or proposed choices"),
        ("dead_ends", "Dead ends — explored but abandoned options"),
        ("questions", "Questions — open evidence prompts"),
        ("evidence", "Evidence — supporting citations"),
        ("open_threads", "Open threads — unresolved discussions"),
        ("entities", "Entities — people, places, systems referenced"),
        ("references", "References — links to other nodes"),
    ];
    FIELDS
        .iter()
        .map(|(value, description)| OptionValue {
            value: (*value).to_string(),
            label: (*value).to_string(),
            description: Some((*description).to_string()),
            meta: None,
        })
        .collect()
}

/// `chain_list` resolver — reads `custom_chain` contributions from
/// `pyramid_config_contributions` and returns one entry per chain.
/// Value is the chain slug (which is the chain `id` field); label is
/// the chain's `name` (or slug fallback); description is the chain's
/// `content_type`.
fn resolve_chain_list(conn: &Connection) -> Result<Vec<OptionValue>> {
    let mut stmt = conn.prepare(
        "SELECT slug, yaml_content
         FROM pyramid_config_contributions
         WHERE schema_type = 'custom_chain'
           AND status = 'active'
           AND superseded_by_id IS NULL
         ORDER BY slug",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?))
        })?;

    let mut out = Vec::new();
    for row in rows {
        let (slug, yaml_content) = row?;
        let Some(slug) = slug else {
            continue;
        };
        let (name, content_type) = extract_chain_name_and_type(&yaml_content);
        out.push(OptionValue {
            value: slug.clone(),
            label: name.unwrap_or_else(|| slug.clone()),
            description: content_type.map(|t| format!("content_type: {t}")),
            meta: None,
        });
    }
    Ok(out)
}

/// Best-effort YAML scan for a chain's `name` and `content_type`
/// fields. Avoids a full `serde_yaml` deserialize because chains
/// carry huge `steps:` blocks that are expensive to parse and we only
/// need two top-level scalars for the dropdown.
fn extract_chain_name_and_type(yaml: &str) -> (Option<String>, Option<String>) {
    let mut name = None;
    let mut content_type = None;
    for line in yaml.lines() {
        let trimmed = line.trim_start();
        // Top-level fields only (no indentation).
        if line.starts_with(|c: char| c.is_whitespace()) {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("name:") {
            name = Some(strip_yaml_scalar(rest));
        } else if let Some(rest) = trimmed.strip_prefix("content_type:") {
            content_type = Some(strip_yaml_scalar(rest));
        }
        if name.is_some() && content_type.is_some() {
            break;
        }
    }
    (name, content_type)
}

fn strip_yaml_scalar(raw: &str) -> String {
    raw.trim()
        .trim_start_matches('"')
        .trim_end_matches('"')
        .trim_start_matches('\'')
        .trim_end_matches('\'')
        .to_string()
}

/// `prompt_files` resolver — reads `skill` contributions from
/// `pyramid_config_contributions` and returns each slug (which is the
/// normalized prompt path) as an option. Used by chain step
/// annotations that want to pick a custom instruction prompt.
fn resolve_prompt_files(conn: &Connection) -> Result<Vec<OptionValue>> {
    let mut stmt = conn.prepare(
        "SELECT slug
         FROM pyramid_config_contributions
         WHERE schema_type = 'skill'
           AND status = 'active'
           AND superseded_by_id IS NULL
           AND slug IS NOT NULL
         ORDER BY slug",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        let slug = row?;
        out.push(OptionValue {
            value: format!("$prompts/{slug}"),
            label: slug.clone(),
            description: None,
            meta: None,
        });
    }
    Ok(out)
}

// ── Cost estimation ─────────────────────────────────────────────────────────

/// Compute an estimated per-call cost for the given provider+model
/// pair, using the pricing_json stored on the matching tier_routing
/// row. Returns `0.0` when the pair is not found or when pricing data
/// is absent — the UI can show "cost unavailable" in that case.
///
/// The formula is the straightforward
/// `input_tokens * prompt_price_per_token + output_tokens * completion_price_per_token`.
/// Cost data lives on the tier routing rows (not the provider rows)
/// because one provider can serve multiple models at different
/// prices.
pub fn estimate_cost(
    provider_registry: &ProviderRegistry,
    provider_id: &str,
    model_id: &str,
    avg_input_tokens: u64,
    avg_output_tokens: u64,
) -> f64 {
    let tier_entry = provider_registry
        .list_tier_routing()
        .into_iter()
        .find(|t| t.provider_id == provider_id && t.model_id == model_id);

    let Some(entry) = tier_entry else {
        warn!(
            provider_id,
            model_id,
            "yaml_renderer_estimate_cost: no matching tier_routing row; returning 0.0"
        );
        return 0.0;
    };

    let prompt_price = entry.prompt_price_per_token().unwrap_or(0.0);
    let completion_price = entry.completion_price_per_token().unwrap_or(0.0);

    (avg_input_tokens as f64) * prompt_price + (avg_output_tokens as f64) * completion_price
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::config_contributions::create_config_contribution;
    use crate::pyramid::credentials::CredentialStore;
    use crate::pyramid::db::init_pyramid_db;
    use std::sync::Arc;

    fn mem_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        conn
    }

    fn empty_registry() -> ProviderRegistry {
        let cred_dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(CredentialStore::load_from_path(cred_dir.path().join(".credentials")).unwrap());
        // Leak the TempDir so the credentials file path stays valid for
        // the life of the test (TempDir drops on end-of-scope which
        // would break later registry reads).
        std::mem::forget(cred_dir);
        ProviderRegistry::new(store)
    }

    fn seed_annotation(conn: &Connection, applies_to: &str, yaml_body: &str) -> String {
        create_config_contribution(
            conn,
            "schema_annotation",
            Some(applies_to),
            yaml_body,
            Some("test seed"),
            "bundled",
            Some("test"),
            "active",
        )
        .unwrap()
    }

    #[test]
    fn test_load_schema_annotation_from_contribution() {
        let conn = mem_conn();

        let yaml = r#"
schema_type: chain_step_config
applies_to: chain_step_config
version: 1
label: "Chain Step Configuration"
fields:
  model_tier:
    label: "Model Tier"
    help: "Which tier to use"
    widget: select
    options_from: tier_registry
    visibility: basic
    show_cost: true
  temperature:
    label: "Temperature"
    help: "Sampling temperature"
    widget: slider
    min: 0.0
    max: 1.0
    step: 0.05
    visibility: basic
"#;
        let _id = seed_annotation(&conn, "chain_step_config", yaml);

        let loaded = load_schema_annotation_for(&conn, "chain_step_config")
            .expect("load should succeed")
            .expect("annotation should be present");

        assert_eq!(loaded.schema_type, "chain_step_config");
        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.applies_to.as_deref(), Some("chain_step_config"));
        assert_eq!(loaded.fields.len(), 2);

        let model_tier = loaded.fields.get("model_tier").unwrap();
        assert_eq!(model_tier.widget, "select");
        assert_eq!(model_tier.visibility, "basic");
        assert_eq!(model_tier.options_from.as_deref(), Some("tier_registry"));
        assert_eq!(model_tier.show_cost, Some(true));

        let temperature = loaded.fields.get("temperature").unwrap();
        assert_eq!(temperature.widget, "slider");
        assert_eq!(temperature.min, Some(0.0));
        assert_eq!(temperature.max, Some(1.0));
        assert_eq!(temperature.step, Some(0.05));
    }

    #[test]
    fn test_load_schema_annotation_missing_returns_none() {
        let conn = mem_conn();
        let loaded = load_schema_annotation_for(&conn, "nonexistent_config").unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn test_load_schema_annotation_falls_back_to_scan() {
        let conn = mem_conn();

        // Seed an annotation whose slug doesn't match the lookup
        // target but whose body's `applies_to` does. The scan
        // fallback should find it.
        let yaml = r#"
schema_type: my_misnamed_annotation
applies_to: dadbear_policy
version: 1
fields:
  enabled:
    label: "Enabled"
    help: "Run DADBEAR on this pyramid"
    widget: toggle
    visibility: basic
"#;
        let _id = create_config_contribution(
            &conn,
            "schema_annotation",
            Some("my_misnamed_annotation"),
            yaml,
            Some("test seed"),
            "bundled",
            Some("test"),
            "active",
        )
        .unwrap();

        let loaded = load_schema_annotation_for(&conn, "dadbear_policy")
            .unwrap()
            .expect("fallback scan should match via applies_to");
        assert_eq!(loaded.applies_to.as_deref(), Some("dadbear_policy"));
        assert!(loaded.fields.contains_key("enabled"));
    }

    #[test]
    fn test_resolve_options_tier_registry_empty() {
        let conn = mem_conn();
        let registry = empty_registry();
        let options = resolve_option_source(&conn, &registry, "tier_registry").unwrap();
        assert!(options.is_empty());
    }

    #[test]
    fn test_resolve_options_tier_registry_seeded() {
        let conn = mem_conn();
        let registry = empty_registry();

        // `init_pyramid_db` auto-seeds the default provider +
        // 4 tier_routing rows (`fast_extract`, `web`, `synth_heavy`,
        // `stale_remote`) via `seed_default_provider_registry`. Upsert
        // our known pricing/context into the `fast_extract` row so the
        // test assertions don't depend on the seed's exact values.
        use crate::pyramid::provider::TierRoutingEntry;
        let tier_a = TierRoutingEntry {
            tier_name: "fast_extract".into(),
            provider_id: "openrouter".into(),
            model_id: "openai/gpt-4o-mini".into(),
            context_limit: Some(128_000),
            max_completion_tokens: Some(16_000),
            pricing_json: r#"{"prompt":"0.0000015","completion":"0.0000060","request":"0"}"#
                .into(),
            supported_parameters_json: None,
            notes: Some("Cheap extraction".into()),
        };
        crate::pyramid::db::save_tier_routing(&conn, &tier_a).unwrap();

        registry.load_from_db(&conn).unwrap();

        // There are 4 seeded tiers from init_pyramid_db.
        let tier_options = resolve_option_source(&conn, &registry, "tier_registry").unwrap();
        assert_eq!(tier_options.len(), 4);
        let by_tier: std::collections::HashMap<String, _> = tier_options
            .iter()
            .map(|o| (o.value.clone(), o.clone()))
            .collect();
        assert!(by_tier.contains_key("fast_extract"));
        assert!(by_tier.contains_key("synth_heavy"));
        assert!(by_tier.contains_key("web"));
        assert!(by_tier.contains_key("stale_remote"));

        // Our upserted fast_extract row carries the expected metadata.
        let fast_meta = by_tier["fast_extract"].meta.as_ref().unwrap();
        assert_eq!(
            fast_meta.get("provider_id").and_then(|v| v.as_str()),
            Some("openrouter")
        );
        assert_eq!(
            fast_meta.get("context_limit").and_then(|v| v.as_i64()),
            Some(128_000)
        );
        assert_eq!(
            fast_meta.get("model_id").and_then(|v| v.as_str()),
            Some("openai/gpt-4o-mini")
        );

        // `provider_list` returns the single default provider.
        let provider_options = resolve_option_source(&conn, &registry, "provider_list").unwrap();
        assert_eq!(provider_options.len(), 1);
        assert_eq!(provider_options[0].value, "openrouter");
        assert_eq!(provider_options[0].label, "OpenRouter");

        // `model_list:openrouter` returns one entry per unique model
        // across the 4 seeded tiers.
        let model_options =
            resolve_option_source(&conn, &registry, "model_list:openrouter").unwrap();
        assert!(!model_options.is_empty());
        let model_ids: Vec<String> =
            model_options.iter().map(|o| o.value.clone()).collect();
        assert!(model_ids.contains(&"openai/gpt-4o-mini".to_string()));

        let none_options =
            resolve_option_source(&conn, &registry, "model_list:nosuchprovider").unwrap();
        assert!(none_options.is_empty());
    }

    #[test]
    fn test_resolve_options_node_fields_is_static() {
        let conn = mem_conn();
        let registry = empty_registry();
        let options = resolve_option_source(&conn, &registry, "node_fields").unwrap();
        let names: Vec<&str> = options.iter().map(|o| o.value.as_str()).collect();
        assert!(names.contains(&"headline"));
        assert!(names.contains(&"distilled"));
        assert!(names.contains(&"topics"));
        assert!(names.contains(&"evidence"));
    }

    #[test]
    fn test_resolve_options_chain_list_reads_custom_chain_contributions() {
        let conn = mem_conn();
        let registry = empty_registry();

        // Seed two custom_chain contributions.
        let _id_a = create_config_contribution(
            &conn,
            "custom_chain",
            Some("question-pipeline"),
            "schema_version: 1\nid: question-pipeline\nname: Question Pipeline\ncontent_type: question\n",
            Some("test"),
            "bundled",
            Some("test"),
            "active",
        )
        .unwrap();
        let _id_b = create_config_contribution(
            &conn,
            "custom_chain",
            Some("code-pipeline"),
            "schema_version: 1\nid: code-pipeline\nname: Code Pipeline\ncontent_type: code\n",
            Some("test"),
            "bundled",
            Some("test"),
            "active",
        )
        .unwrap();

        let options = resolve_option_source(&conn, &registry, "chain_list").unwrap();
        assert_eq!(options.len(), 2);
        let code = options.iter().find(|o| o.value == "code-pipeline").unwrap();
        assert_eq!(code.label, "Code Pipeline");
        assert_eq!(code.description.as_deref(), Some("content_type: code"));
    }

    #[test]
    fn test_resolve_options_prompt_files_reads_skill_contributions() {
        let conn = mem_conn();
        let registry = empty_registry();

        let _id = create_config_contribution(
            &conn,
            "skill",
            Some("conversation/forward.md"),
            "# Forward prompt body",
            Some("test"),
            "bundled",
            Some("test"),
            "active",
        )
        .unwrap();

        let options = resolve_option_source(&conn, &registry, "prompt_files").unwrap();
        assert_eq!(options.len(), 1);
        assert_eq!(options[0].value, "$prompts/conversation/forward.md");
        assert_eq!(options[0].label, "conversation/forward.md");
    }

    #[test]
    fn test_resolve_options_unknown_source_returns_empty() {
        let conn = mem_conn();
        let registry = empty_registry();
        let options = resolve_option_source(&conn, &registry, "what_is_this").unwrap();
        assert!(options.is_empty());
    }

    #[test]
    fn test_estimate_cost_from_seeded_tier() {
        let conn = mem_conn();
        let registry = empty_registry();

        use crate::pyramid::provider::{Provider, ProviderType, TierRoutingEntry};
        let provider = Provider {
            id: "openrouter".into(),
            display_name: "OpenRouter".into(),
            provider_type: ProviderType::Openrouter,
            base_url: "https://openrouter.ai/api/v1".into(),
            api_key_ref: Some("OPENROUTER_KEY".into()),
            auto_detect_context: true,
            supports_broadcast: true,
            broadcast_config_json: None,
            config_json: "{}".into(),
            enabled: true,
        };
        crate::pyramid::db::save_provider(&conn, &provider).unwrap();
        let tier = TierRoutingEntry {
            tier_name: "fast_extract".into(),
            provider_id: "openrouter".into(),
            model_id: "openai/gpt-4o-mini".into(),
            context_limit: Some(128_000),
            max_completion_tokens: Some(16_000),
            pricing_json: r#"{"prompt":"0.0000015","completion":"0.000006"}"#.into(),
            supported_parameters_json: None,
            notes: None,
        };
        crate::pyramid::db::save_tier_routing(&conn, &tier).unwrap();
        registry.load_from_db(&conn).unwrap();

        // 1,000 input tokens * 1.5e-6 + 500 output tokens * 6e-6
        //   = 0.0015 + 0.003 = 0.0045
        let cost = estimate_cost(&registry, "openrouter", "openai/gpt-4o-mini", 1_000, 500);
        assert!((cost - 0.0045).abs() < 1e-9, "expected 0.0045, got {cost}");
    }

    #[test]
    fn test_estimate_cost_missing_pair_returns_zero() {
        let conn = mem_conn();
        let _ = conn;
        let registry = empty_registry();
        let cost = estimate_cost(&registry, "nosuchprovider", "nosuchmodel", 1_000, 500);
        assert_eq!(cost, 0.0);
    }

    #[test]
    fn test_annotation_serializes_preserving_optional_fields() {
        // Round-trip: parse from YAML, serialize to JSON, verify the
        // structure the frontend sees matches expectations.
        let yaml = r#"
schema_type: dadbear_policy
applies_to: dadbear_policy
version: 1
fields:
  scan_interval_secs:
    label: "Scan Interval"
    help: "How often DADBEAR polls the folder (seconds)"
    widget: number
    min: 1
    max: 3600
    step: 1
    suffix: "sec"
    visibility: basic
    order: 1
"#;
        let parsed: SchemaAnnotation = serde_yaml::from_str(yaml).unwrap();
        let json = serde_json::to_value(&parsed).unwrap();
        let fields = json.get("fields").unwrap();
        let scan = fields.get("scan_interval_secs").unwrap();
        assert_eq!(scan.get("widget").and_then(|v| v.as_str()), Some("number"));
        assert_eq!(scan.get("min").and_then(|v| v.as_f64()), Some(1.0));
        assert_eq!(scan.get("suffix").and_then(|v| v.as_str()), Some("sec"));
        assert_eq!(scan.get("order").and_then(|v| v.as_i64()), Some(1));
        // Unset fields should be omitted from the JSON.
        assert!(scan.get("options").is_none());
        assert!(scan.get("options_from").is_none());
        assert!(scan.get("inherits_from").is_none());
    }
}
