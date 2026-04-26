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
        if let Some(annotation) =
            try_parse_annotation(&contribution.yaml_content, target_schema_type)
        {
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

    let rows = stmt.query_map([], |row| {
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
///     provider. **Phase 18a (L5):** for Ollama-shaped providers
///     this returns the cached snapshot from a prior network probe;
///     the live network round trip is performed by
///     `resolve_model_list_only` which the IPC layer awaits AFTER
///     dropping the rusqlite lock. The sync entry point falls back
///     to the tier-table view when no cached entry exists yet.
///   - `node_fields` — top-level pyramid node schema field names
///   - `chain_list` — custom chain contributions + their content_type
///   - `prompt_files` — skill contributions (paths)
///
/// Unknown sources return `Ok(vec![])` and log a warning — missing
/// options are not fatal; the select widget shows an empty list and
/// the user sees the raw value.
///
/// This function is fully synchronous so callers can hold a
/// `&Connection` for the DB-only branches without crossing an await
/// point.
pub fn resolve_option_source(
    conn: &Connection,
    provider_registry: &ProviderRegistry,
    source: &str,
) -> Result<Vec<OptionValue>> {
    // Handle parameterized `model_list:{provider_id}` form first.
    if let Some(provider_id) = source.strip_prefix("model_list:") {
        return Ok(resolve_model_list_sync(
            Some(conn),
            provider_registry,
            provider_id,
        ));
    }

    match source {
        "tier_registry" => Ok(resolve_tier_registry(conn, provider_registry)),
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

/// Async entry point for the network-bound `model_list:{provider_id}`
/// branch. Used by the IPC layer so the caller can drop the rusqlite
/// lock before the network round-trip. Same caching + Ollama-shaped
/// detection as the main `resolve_option_source` path.
pub async fn resolve_model_list_only(
    provider_registry: &ProviderRegistry,
    provider_id: &str,
) -> Vec<OptionValue> {
    resolve_model_list_for(provider_registry, provider_id).await
}

/// Synchronous variant of the model-list resolver: returns only the
/// cached or tier-table data, never hits the network. Used by the
/// sync `resolve_option_source` entry point so the rusqlite lock can
/// be held across the call. The IPC layer hits
/// `resolve_model_list_only` separately to refresh the cache.
fn resolve_model_list_sync(
    conn: Option<&Connection>,
    provider_registry: &ProviderRegistry,
    provider_id: &str,
) -> Vec<OptionValue> {
    if let Some(cached) = lookup_cached_models(provider_id) {
        return cached;
    }
    resolve_model_list_from_tier_table(conn, provider_registry, provider_id)
}

// ── Phase 18a (L5): Ollama /api/tags resolver + per-provider cache ──────────

/// Per-provider model-list cache for the `model_list:{provider_id}`
/// resolver. Cached for 30 seconds so Phase 8's mount-time fan-out
/// across N widgets only triggers one round-trip per provider per
/// half-minute. Failures (unreachable Ollama, parse errors) cache an
/// empty list with the same TTL so the UI still degrades gracefully
/// without re-hammering the local socket.
static OLLAMA_TAGS_CACHE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, (Vec<OptionValue>, std::time::Instant)>>,
> = std::sync::OnceLock::new();

const OLLAMA_TAGS_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

fn ollama_tags_cache() -> &'static std::sync::Mutex<
    std::collections::HashMap<String, (Vec<OptionValue>, std::time::Instant)>,
> {
    OLLAMA_TAGS_CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn lookup_cached_models(provider_id: &str) -> Option<Vec<OptionValue>> {
    let cache = ollama_tags_cache().lock().ok()?;
    let (models, fetched_at) = cache.get(provider_id)?;
    if fetched_at.elapsed() <= OLLAMA_TAGS_CACHE_TTL {
        Some(models.clone())
    } else {
        None
    }
}

fn store_cached_models(provider_id: &str, models: Vec<OptionValue>) {
    if let Ok(mut cache) = ollama_tags_cache().lock() {
        cache.insert(provider_id.to_string(), (models, std::time::Instant::now()));
    }
}

/// Heuristic: a provider is "Ollama-shaped" when its `provider_type`
/// is OpenaiCompat AND either its id starts with `ollama` OR its
/// base_url contains the default Ollama port `:11434`. Documented in
/// the spec; users with non-default setups can override via
/// step_overrides or by editing the provider id to start with
/// `ollama`.
fn is_ollama_shaped(provider: &crate::pyramid::provider::Provider) -> bool {
    if !matches!(
        provider.provider_type,
        crate::pyramid::provider::ProviderType::OpenaiCompat
    ) {
        return false;
    }
    if provider.id.starts_with("ollama") {
        return true;
    }
    provider.base_url.contains(":11434")
}

/// Test-only hook: clear the per-provider model cache so adjacent
/// tests don't see each other's stored entries.
#[cfg(test)]
pub fn clear_ollama_tags_cache_for_tests() {
    if let Ok(mut cache) = ollama_tags_cache().lock() {
        cache.clear();
    }
}

/// `tier_registry` resolver — one entry per tier name derived from the
/// walker_* scope chain's declared `model_list` keys (Phase 1 §5.1
/// migration: `pyramid_tier_routing` table is retired and replaced by
/// `walker_provider_*` contributions). The label is the tier_name; the
/// description is synthesized from the walker-declared provider/model
/// pair for that tier (first provider in call order that declares the
/// tier wins for display purposes).
///
/// Per rev 1.0.2 §5.1 + W2e migration plan: data source switches from
/// the legacy `pyramid_tier_routing` rows to the walker ScopeChain's
/// union of `model_list` keys (see `walker_resolver::tier_set_from_chain`).
/// Rich fields (pricing_json, context_limit, max_completion_tokens) come
/// from the per-provider `walker_provider_*` overrides at the same tier.
///
/// Falls back to the legacy `ProviderRegistry::list_tier_routing()` view
/// only when the walker scope cache is empty (pre-bootstrap) OR the
/// connection read fails — preserves UI liveness during first-run migration.
fn resolve_tier_registry(conn: &Connection, registry: &ProviderRegistry) -> Vec<OptionValue> {
    // Walker-first: build the scope chain from active walker_* contributions
    // and derive the tier set + per-tier rich metadata.
    match crate::pyramid::walker_resolver::build_scope_cache_pair(conn) {
        Ok(data) => {
            let chain = &data.chain;
            let tier_names = crate::pyramid::walker_resolver::tier_set_from_chain(chain);
            if !tier_names.is_empty() {
                let mut out: Vec<OptionValue> = tier_names
                    .into_iter()
                    .map(|tier| walker_tier_to_option(chain, &tier))
                    .collect();
                out.sort_by(|a, b| a.label.cmp(&b.label));
                return out;
            }
            // Empty tier set → fall through to legacy registry view so the
            // Settings UI still shows something during the first-boot
            // window before walker_provider_* contributions land.
        }
        Err(err) => {
            warn!(
                error = %err,
                "resolve_tier_registry: walker scope cache load failed; \
                 falling back to legacy pyramid_tier_routing view"
            );
        }
    }

    // Legacy fallback (retired in W3 when the pyramid_tier_routing table
    // is dropped). TODO(W3): remove this branch once walker_provider_*
    // seeds ship in the default bundle.
    let mut out: Vec<OptionValue> = registry
        .list_tier_routing()
        .into_iter()
        .map(tier_entry_to_option)
        .collect();
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

/// Render a single walker-declared tier as an `OptionValue`. Scans the
/// chain's call-order providers and provider scopes for the first one
/// that declares this tier in its `model_list` map, then pulls the rich
/// metadata (pricing_json, context_limit, max_completion_tokens) off
/// the same provider. Mirrors the shape previously produced by
/// `tier_entry_to_option` from a `TierRoutingEntry` row so the frontend
/// contract is unchanged.
fn walker_tier_to_option(
    chain: &crate::pyramid::walker_resolver::ScopeChain,
    tier: &str,
) -> OptionValue {
    use crate::pyramid::walker_resolver::{
        resolve_context_limit, resolve_max_completion_tokens, resolve_model_list,
        resolve_pricing_json, ProviderType,
    };

    // Walk the call order (union of scope 3 then scope 4 provider keys)
    // so display picks up whichever provider declares this tier first.
    let mut provider_and_model: Option<(ProviderType, String)> = None;
    let providers_iter = chain
        .call_order_provider
        .keys()
        .copied()
        .chain(chain.provider.keys().copied());
    let mut seen = std::collections::HashSet::new();
    for pt in providers_iter {
        if !seen.insert(pt) {
            continue;
        }
        if let Some(list) = resolve_model_list(chain, tier, pt) {
            if let Some(model_id) = list.first().cloned() {
                provider_and_model = Some((pt, model_id));
                break;
            }
        }
    }

    let (provider_id, model_id) = match &provider_and_model {
        Some((pt, model)) => (pt.as_str().to_string(), model.clone()),
        None => (String::new(), String::new()),
    };

    let (prompt_price, completion_price, context_limit, max_completion_tokens) =
        match provider_and_model.as_ref().map(|(pt, _)| *pt) {
            Some(pt) => {
                let pricing_value = resolve_pricing_json(chain, tier, pt);
                let (pp, cp) = match pricing_value.as_ref() {
                    Some(v) => (
                        parse_price_from_walker_json(v, "prompt"),
                        parse_price_from_walker_json(v, "completion"),
                    ),
                    None => (None, None),
                };
                (
                    pp,
                    cp,
                    resolve_context_limit(chain, tier, pt),
                    resolve_max_completion_tokens(chain, tier, pt),
                )
            }
            None => (None, None, None, None),
        };

    let description = if !model_id.is_empty() && !provider_id.is_empty() {
        Some(format!("{} via {}", model_id, provider_id))
    } else {
        None
    };

    let meta = serde_json::json!({
        "provider_id": provider_id,
        "model_id": model_id,
        "context_limit": context_limit,
        "max_completion_tokens": max_completion_tokens,
        "prompt_price_per_token": prompt_price,
        "completion_price_per_token": completion_price,
    });

    OptionValue {
        value: tier.to_string(),
        label: tier.to_string(),
        description,
        meta: Some(meta),
    }
}

/// Parse an OpenRouter-shaped pricing field (`"prompt"` / `"completion"`)
/// from the walker `pricing_json` blob. Pricing is stored as a
/// string-encoded decimal (e.g. `"0.0000015"`) per §5.1. Returns `None`
/// on missing or unparseable field.
fn parse_price_from_walker_json(value: &serde_json::Value, field: &str) -> Option<f64> {
    value
        .get(field)
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())
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

/// `model_list:{provider_id}` resolver. For Ollama-shaped providers
/// (Phase 18a / L5), returns the live model list from
/// `GET {base_url}/api/tags` cached for 30 seconds. For everything
/// else, falls back to the "models the tier routing table has routed
/// through this provider" view that Phase 8 shipped.
async fn resolve_model_list_for(
    registry: &ProviderRegistry,
    provider_id: &str,
) -> Vec<OptionValue> {
    // Phase 18a (L5): the Ollama path takes precedence when the
    // provider row is OpenaiCompat + Ollama-shaped. We resolve the
    // base_url through the registry's credential substitution path
    // so users with `${OLLAMA_LOCAL_URL}` in the base_url field still
    // get a working probe.
    if let Some(provider) = registry.get_provider(provider_id) {
        if is_ollama_shaped(&provider) {
            if let Some(cached) = lookup_cached_models(provider_id) {
                return cached;
            }

            // Substitute `${VAR}` references in the base_url through
            // the credential store so the probe targets the user's
            // actual endpoint.
            let resolved_base = match registry.resolve_base_url(&provider) {
                Ok(url) => url,
                Err(err) => {
                    warn!(
                        provider_id,
                        error = %err,
                        "model_list: failed to resolve provider base_url; \
                         falling back to tier-table view"
                    );
                    // Cache an empty list briefly so we don't spin on
                    // a misconfigured base_url.
                    store_cached_models(provider_id, Vec::new());
                    // TODO(W2c/W3): async callers from main.rs don't thread
                    // a `&Connection`. Until W2c updates the caller or a
                    // global ArcSwap<ScopeCache> handle lands, this branch
                    // falls back to the legacy registry view. Safe because
                    // Ollama probe failure is already a degraded path.
                    return resolve_model_list_from_tier_table(None, registry, provider_id);
                }
            };

            match crate::pyramid::local_mode::fetch_ollama_models(&resolved_base).await {
                Ok(model_names) => {
                    let mut out: Vec<OptionValue> = model_names
                        .into_iter()
                        .map(|name| OptionValue {
                            value: name.clone(),
                            label: name.clone(),
                            description: Some(format!("Ollama model — {name}")),
                            meta: Some(serde_json::json!({
                                "provider_id": provider_id,
                                "source": "ollama_api_tags",
                            })),
                        })
                        .collect();
                    out.sort_by(|a, b| a.label.cmp(&b.label));
                    store_cached_models(provider_id, out.clone());
                    return out;
                }
                Err(err) => {
                    warn!(
                        provider_id,
                        error = %err,
                        "model_list: Ollama /api/tags probe failed; degrading to empty list"
                    );
                    store_cached_models(provider_id, Vec::new());
                    return Vec::new();
                }
            }
        }
    }

    // Non-Ollama path: the Phase 8 view, returning what's already
    // configured in tier_routing for this provider.
    // TODO(W2c/W3): async caller doesn't thread a `&Connection`; the
    // walker-data-first path inside `resolve_model_list_from_tier_table`
    // activates only when the sync caller (`resolve_option_source`) runs.
    resolve_model_list_from_tier_table(None, registry, provider_id)
}

/// Phase 8 fallback: build a model list for `provider_id` by enumerating
/// the walker scope chain's `model_list` overrides (Phase 1 §5.1
/// migration). The walker-first path activates when a `&Connection` is
/// available (sync caller via `resolve_option_source`). Async callers
/// (`resolve_model_list_for` in the Ollama-fallback branches) pass
/// `None` and fall back to the legacy `ProviderRegistry::list_tier_routing()`
/// view — retired in W3 when the table is dropped.
fn resolve_model_list_from_tier_table(
    conn: Option<&Connection>,
    registry: &ProviderRegistry,
    provider_id: &str,
) -> Vec<OptionValue> {
    // Walker-first: derive (model_id, tier, per-tier metadata) from the
    // scope chain for the matching ProviderType. `provider_id` from the
    // frontend maps to ProviderType::as_str() (e.g. "openrouter", "local").
    if let Some(conn) = conn {
        if let Some(pt) = provider_type_from_id(provider_id) {
            if let Ok(data) = crate::pyramid::walker_resolver::build_scope_cache_pair(conn) {
                let chain = &data.chain;
                let tier_names = crate::pyramid::walker_resolver::tier_set_from_chain(chain);
                let mut out: Vec<OptionValue> = Vec::new();
                let mut seen = std::collections::HashSet::new();
                for tier in &tier_names {
                    let Some(list) =
                        crate::pyramid::walker_resolver::resolve_model_list(chain, tier, pt)
                    else {
                        continue;
                    };
                    let context_limit =
                        crate::pyramid::walker_resolver::resolve_context_limit(chain, tier, pt);
                    let max_completion_tokens =
                        crate::pyramid::walker_resolver::resolve_max_completion_tokens(
                            chain, tier, pt,
                        );
                    for model_id in list {
                        if !seen.insert(model_id.clone()) {
                            continue;
                        }
                        out.push(OptionValue {
                            value: model_id.clone(),
                            label: model_id.clone(),
                            description: context_limit.map(|n| format!("context: {n} tokens")),
                            meta: Some(serde_json::json!({
                                "provider_id": provider_id,
                                "context_limit": context_limit,
                                "max_completion_tokens": max_completion_tokens,
                            })),
                        });
                    }
                }
                if !out.is_empty() {
                    out.sort_by(|a, b| a.label.cmp(&b.label));
                    return out;
                }
                // Empty walker result → fall through to legacy view so
                // pre-bootstrap UI still shows options.
            }
        }
    }

    // Legacy fallback (retired in W3).
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
            description: entry
                .notes
                .clone()
                .or_else(|| entry.context_limit.map(|n| format!("context: {n} tokens"))),
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

/// Map a UI provider_id string onto a walker `ProviderType`. The four
/// walker provider types use `ProviderType::as_str()` values
/// (`"local"`, `"openrouter"`, `"fleet"`, `"market"`). Returns `None` for
/// legacy provider ids that don't map to a walker variant — callers then
/// fall back to the registry view. Ollama-shaped `ProviderType::OpenaiCompat`
/// rows route through the Ollama-tags path above and never reach here.
fn provider_type_from_id(
    provider_id: &str,
) -> Option<crate::pyramid::walker_resolver::ProviderType> {
    use crate::pyramid::walker_resolver::ProviderType;
    match provider_id {
        "local" => Some(ProviderType::Local),
        "openrouter" => Some(ProviderType::OpenRouter),
        "fleet" => Some(ProviderType::Fleet),
        "market" => Some(ProviderType::Market),
        _ => None,
    }
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
    let rows = stmt.query_map([], |row| {
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
///
/// TODO(W2c/W3/Phase 1): walker v3 — pricing lives on walker_provider_*
/// overrides (see `walker_resolver::resolve_pricing_json`). This pub fn
/// is called from main.rs (W2c-owned) with only `&ProviderRegistry` in
/// scope; no `&Connection` to build the scope cache and no global
/// ArcSwap<ScopeCache> handle on the registry. Two viable upgrades,
/// both out of scope for W2e:
///
///   a. W2c can update the Tauri command (`yaml_renderer_estimate_cost`)
///      to take a `&Connection` from SharedState and thread it through.
///   b. W3 can augment `ProviderRegistry` with an ArcSwap handle at
///      construction so all three yaml_renderer callers share one source
///      of truth without a signature churn.
///
/// Until then, this branch keeps the legacy `pyramid_tier_routing` read
/// path. Acceptable because the table is still populated at W2e time;
/// migration is a W3 responsibility together with field/table deletion.
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
            model_id, "yaml_renderer_estimate_cost: no matching tier_routing row; returning 0.0"
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
        let store = Arc::new(
            CredentialStore::load_from_path(cred_dir.path().join(".credentials")).unwrap(),
        );
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
            pricing_json: r#"{"prompt":"0.0000015","completion":"0.0000060","request":"0"}"#.into(),
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
        let model_ids: Vec<String> = model_options.iter().map(|o| o.value.clone()).collect();
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

    #[tokio::test]
    async fn test_resolve_model_list_ollama_caches_failure() {
        // Phase 18a (L5): when the provider is Ollama-shaped and the
        // probe fails (no Ollama running), the async resolver returns
        // an empty list AND caches the empty result so a follow-up
        // call doesn't re-attempt the network round trip. The sync
        // entry point sees the cached value on the second call.
        clear_ollama_tags_cache_for_tests();
        let conn = mem_conn();
        let registry = empty_registry();
        // Insert an Ollama-shaped provider row pointing at a port
        // that is almost certainly not listening so the probe fails
        // fast and the test isn't flaky on machines running a real
        // Ollama.
        use crate::pyramid::provider::{Provider, ProviderType};
        let provider = Provider {
            id: "ollama-local".into(),
            display_name: "Ollama (local)".into(),
            provider_type: ProviderType::OpenaiCompat,
            base_url: "http://127.0.0.1:1/v1".into(),
            api_key_ref: None,
            auto_detect_context: true,
            supports_broadcast: false,
            broadcast_config_json: None,
            config_json: "{}".into(),
            enabled: true,
        };
        crate::pyramid::db::save_provider(&conn, &provider).unwrap();
        registry.load_from_db(&conn).unwrap();

        // Async call: hits the network and caches the empty failure.
        let _ = resolve_model_list_only(&registry, "ollama-local").await;
        // Sync call: must see the cached empty list.
        let cached = resolve_option_source(&conn, &registry, "model_list:ollama-local").unwrap();
        assert!(cached.is_empty(), "expected cached empty list");
        clear_ollama_tags_cache_for_tests();
    }

    #[test]
    fn test_is_ollama_shaped_heuristic() {
        use crate::pyramid::provider::{Provider, ProviderType};
        let make = |id: &str, ptype: ProviderType, base: &str| Provider {
            id: id.into(),
            display_name: id.into(),
            provider_type: ptype,
            base_url: base.into(),
            api_key_ref: None,
            auto_detect_context: false,
            supports_broadcast: false,
            broadcast_config_json: None,
            config_json: "{}".into(),
            enabled: true,
        };
        // Ollama by id prefix.
        assert!(super::is_ollama_shaped(&make(
            "ollama-local",
            ProviderType::OpenaiCompat,
            "http://localhost:11434/v1"
        )));
        // Ollama by port heuristic.
        assert!(super::is_ollama_shaped(&make(
            "my-local",
            ProviderType::OpenaiCompat,
            "http://127.0.0.1:11434/v1"
        )));
        // OpenAI-compat that isn't Ollama-shaped.
        assert!(!super::is_ollama_shaped(&make(
            "groq-prod",
            ProviderType::OpenaiCompat,
            "https://api.groq.com/openai/v1"
        )));
        // Wrong provider_type — never Ollama-shaped.
        assert!(!super::is_ollama_shaped(&make(
            "openrouter",
            ProviderType::Openrouter,
            "https://openrouter.ai/api/v1"
        )));
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

    /// Wanderer guard: the shipped `chains/schemas/dadbear.schema.yaml`
    /// must have the same field names as the real `DadbearPolicyYaml`
    /// struct in `db.rs`. If someone renames a field in one place and
    /// not the other, the renderer will show empty editors for
    /// non-matching fields when Phase 10 wires it to a live policy.
    /// This test pins the contract by parsing the seed file and
    /// checking every expected key is present.
    #[test]
    fn test_seed_dadbear_annotation_matches_real_policy_fields() {
        let seed_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crate parent must exist")
            .join("chains/schemas/dadbear.schema.yaml");
        let body =
            std::fs::read_to_string(&seed_path).expect("seed dadbear annotation should exist");
        let parsed: SchemaAnnotation =
            serde_yaml::from_str(&body).expect("seed dadbear annotation should parse");
        assert_eq!(parsed.applies_to.as_deref(), Some("dadbear_policy"));
        // Every field name below must exist in `DadbearPolicyYaml` in
        // `pyramid::db`. Keep this list in sync if the struct changes.
        let expected: &[&str] = &[
            "source_path",
            "content_type",
            "scan_interval_secs",
            "debounce_secs",
            "session_timeout_secs",
            "batch_size",
            "enabled",
        ];
        for key in expected {
            assert!(
                parsed.fields.contains_key(*key),
                "dadbear annotation is missing field `{key}` that exists on DadbearPolicyYaml"
            );
        }
        // No stale fields that DadbearPolicyYaml doesn't have.
        let allowed: std::collections::HashSet<&str> = expected.iter().copied().collect();
        for key in parsed.fields.keys() {
            assert!(
                allowed.contains(key.as_str()),
                "dadbear annotation has unknown field `{key}` not present on DadbearPolicyYaml"
            );
        }
    }

    /// Wanderer guard: the shipped `chains/schemas/chain-step.schema.yaml`
    /// must have field names that match the real `ChainStep` struct in
    /// `chain_engine.rs`. This test loads the seed file and checks
    /// every rendered field name is a real chain step property.
    #[test]
    fn test_seed_chain_step_annotation_fields_exist_on_chain_step() {
        let seed_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crate parent must exist")
            .join("chains/schemas/chain-step.schema.yaml");
        let body =
            std::fs::read_to_string(&seed_path).expect("seed chain-step annotation should exist");
        let parsed: SchemaAnnotation =
            serde_yaml::from_str(&body).expect("seed chain-step annotation should parse");
        assert_eq!(parsed.applies_to.as_deref(), Some("chain_step_config"));
        // Field names that must correspond to real `ChainStep` fields.
        let real_chain_step_fields: std::collections::HashSet<&str> = [
            "model_tier",
            "temperature",
            "concurrency",
            "on_error",
            "max_input_tokens",
            "batch_size",
            "split_strategy",
            "dehydrate",
            "compact_inputs",
        ]
        .iter()
        .copied()
        .collect();
        for key in parsed.fields.keys() {
            assert!(
                real_chain_step_fields.contains(key.as_str()),
                "chain-step annotation has field `{key}` not present on ChainStep struct"
            );
        }
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
