// pyramid/schema_registry.rs — Phase 9: Schema Registry.
//
// Canonical reference:
//   /Users/adamlevine/AI Project Files/agent-wire-node/docs/specs/generative-config-pattern.md
//     — "Schema Registry (backed by contributions)" section (~line 29)
//
// The schema registry is a VIEW over `pyramid_config_contributions`,
// not a separate table. For every active schema_type the registry
// joins three contribution rows:
//
//   1. `schema_definition` — the JSON Schema that validates config
//      YAMLs (tag: `applies_to: <target>`)
//   2. `schema_annotation` — the YAML-to-UI renderer metadata
//      (tag: `applies_to: <target>`)
//   3. `skill` — the generation prompt body used by
//      `pyramid_generate_config` (tag: topic contains `generation`
//      AND topic contains the target schema_type)
//
// Plus an optional seed default for the target schema_type
// (source = `bundled` contributions of that schema_type).
//
// The registry is held on `PyramidState` as `Arc<SchemaRegistry>` and
// hydrated once at boot (after the Phase 5+9 migration). The Phase 4
// dispatcher's `invalidate_schema_registry_cache` stub (called from
// `sync_config_to_operational` when a `schema_definition` supersedes)
// is wired to `SchemaRegistry::invalidate()` in Phase 9 — it re-reads
// every entry from the contribution store.

use std::collections::HashMap;
use std::sync::RwLock;

use anyhow::Result;
use rusqlite::Connection;
use tracing::{debug, warn};

// ── Types ────────────────────────────────────────────────────────────

/// A single resolved schema entry inside the registry. Carries the
/// three contribution IDs that together define a config type + the
/// optional bundled seed default.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConfigSchema {
    /// The target config schema_type (e.g. "evidence_policy").
    pub schema_type: String,
    /// Human-readable label shown in the UI.
    pub display_name: String,
    /// Short description shown in the schema picker.
    pub description: String,
    /// contribution_id of the `schema_definition` contribution that
    /// holds the JSON Schema body for this config type.
    pub schema_definition_contribution_id: String,
    /// contribution_id of the `schema_annotation` contribution that
    /// holds the YAML-to-UI renderer annotation. May be empty string
    /// if no annotation is present (the frontend falls back to a
    /// generic key/value editor in that case).
    pub schema_annotation_contribution_id: String,
    /// contribution_id of the `skill` contribution that holds the
    /// generation prompt body. May be empty string if no generation
    /// skill has been seeded yet (the generator will fail loudly in
    /// that case rather than silently producing empty YAML).
    pub generation_skill_contribution_id: String,
    /// Optional bundled seed default contribution_id. `None` when no
    /// bundled default has been shipped for this schema_type.
    pub default_seed_contribution_id: Option<String>,
    /// Phase 9 always ships version=1. When Phase 10+ adds multi-
    /// version support we can surface the supersession chain depth
    /// here.
    pub version: u32,
}

/// Compact summary of a `ConfigSchema` for the `pyramid_config_schemas`
/// IPC response. The frontend just needs the identity + the "do we
/// have the pieces" booleans to build its schema picker UI.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConfigSchemaSummary {
    pub schema_type: String,
    pub display_name: String,
    pub description: String,
    pub has_generation_skill: bool,
    pub has_annotation: bool,
    pub has_default_seed: bool,
}

impl From<&ConfigSchema> for ConfigSchemaSummary {
    fn from(schema: &ConfigSchema) -> Self {
        Self {
            schema_type: schema.schema_type.clone(),
            display_name: schema.display_name.clone(),
            description: schema.description.clone(),
            has_generation_skill: !schema.generation_skill_contribution_id.is_empty(),
            has_annotation: !schema.schema_annotation_contribution_id.is_empty(),
            has_default_seed: schema.default_seed_contribution_id.is_some(),
        }
    }
}

// ── Registry ─────────────────────────────────────────────────────────

/// In-memory schema registry. Lives on `PyramidState` as
/// `Arc<SchemaRegistry>` and holds the current view of active schemas.
/// Hydrated at boot, re-hydrated on `invalidate()` from the Phase 4
/// dispatcher's schema_definition supersession branch.
pub struct SchemaRegistry {
    entries: RwLock<HashMap<String, ConfigSchema>>,
}

impl Default for SchemaRegistry {
    fn default() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
        }
    }
}

impl SchemaRegistry {
    /// Construct an empty registry. Callers hydrate via
    /// `hydrate_from_contributions` after DB init.
    pub fn new() -> Self {
        Self::default()
    }

    /// Load all active schemas by querying the contribution store.
    /// For each distinct `schema_definition` contribution (identified
    /// by its slug = target schema_type), resolve the matching
    /// schema_annotation (by slug), generation skill (by topic tag
    /// containing "generation" + the target schema_type), and seed
    /// default (the latest active bundled contribution of that
    /// schema_type).
    ///
    /// Returns a populated `SchemaRegistry`. Call at boot or after a
    /// migration run.
    pub fn hydrate_from_contributions(conn: &Connection) -> Result<Self> {
        let registry = Self::new();
        registry.reload(conn)?;
        Ok(registry)
    }

    /// Re-read every entry from the contribution store. Idempotent —
    /// replaces the in-memory map atomically. Phase 4's dispatcher
    /// calls this via `invalidate()` after a schema_definition
    /// supersedes.
    pub fn reload(&self, conn: &Connection) -> Result<()> {
        let mut new_entries: HashMap<String, ConfigSchema> = HashMap::new();

        // Walk every active schema_definition contribution. Each row's
        // `slug` is the target config schema_type (e.g.
        // "evidence_policy"). For each target we then look up the
        // annotation + generation skill + bundled default.
        let mut stmt = conn.prepare(
            "SELECT contribution_id, slug FROM pyramid_config_contributions
             WHERE schema_type = 'schema_definition'
               AND status = 'active'
               AND superseded_by_id IS NULL
             ORDER BY created_at DESC, id DESC",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?;

        for row in rows {
            let (schema_definition_id, slug_opt) = row?;
            let Some(target_schema_type) = slug_opt else {
                warn!(
                    contribution_id = %schema_definition_id,
                    "schema_definition contribution has NULL slug; skipping"
                );
                continue;
            };

            // Skip if we already have an entry for this target (first-
            // seen wins per the ORDER BY created_at DESC).
            if new_entries.contains_key(&target_schema_type) {
                continue;
            }

            let schema_annotation_id = find_active_annotation_id(conn, &target_schema_type)?;
            let generation_skill_id = find_active_generation_skill_id(conn, &target_schema_type)?;
            let default_seed_id = find_bundled_default_id(conn, &target_schema_type)?;

            let display_name = display_name_for(&target_schema_type);
            let description = description_for(&target_schema_type);

            new_entries.insert(
                target_schema_type.clone(),
                ConfigSchema {
                    schema_type: target_schema_type,
                    display_name,
                    description,
                    schema_definition_contribution_id: schema_definition_id,
                    schema_annotation_contribution_id: schema_annotation_id.unwrap_or_default(),
                    generation_skill_contribution_id: generation_skill_id.unwrap_or_default(),
                    default_seed_contribution_id: default_seed_id,
                    version: 1,
                },
            );
        }

        debug!(
            count = new_entries.len(),
            "SchemaRegistry: hydrated from contributions"
        );

        let mut guard = self
            .entries
            .write()
            .expect("SchemaRegistry RwLock poisoned");
        *guard = new_entries;
        Ok(())
    }

    /// Look up a schema by target config type. Returns a clone of the
    /// resolved entry or `None` if no active schemas exist for that
    /// type.
    pub fn get(&self, schema_type: &str) -> Option<ConfigSchema> {
        let guard = self.entries.read().expect("SchemaRegistry RwLock poisoned");
        guard.get(schema_type).cloned()
    }

    /// List all known schema types as compact summaries. Used by the
    /// `pyramid_config_schemas` IPC to populate the frontend's schema
    /// picker.
    pub fn list(&self) -> Vec<ConfigSchemaSummary> {
        let guard = self.entries.read().expect("SchemaRegistry RwLock poisoned");
        let mut summaries: Vec<ConfigSchemaSummary> =
            guard.values().map(ConfigSchemaSummary::from).collect();
        // Stable ordering by schema_type so the UI doesn't reshuffle
        // on every reload.
        summaries.sort_by(|a, b| a.schema_type.cmp(&b.schema_type));
        summaries
    }

    /// List all known schemas as full `ConfigSchema` entries. Useful
    /// for tests and for callers that need the contribution_ids.
    pub fn list_full(&self) -> Vec<ConfigSchema> {
        let guard = self.entries.read().expect("SchemaRegistry RwLock poisoned");
        let mut schemas: Vec<ConfigSchema> = guard.values().cloned().collect();
        schemas.sort_by(|a, b| a.schema_type.cmp(&b.schema_type));
        schemas
    }

    /// Re-hydrate from the DB. Called from Phase 4's dispatcher hook
    /// (`invalidate_schema_registry_cache`) when a schema_definition
    /// contribution is superseded.
    pub fn invalidate(&self, conn: &Connection) -> Result<()> {
        self.reload(conn)
    }
}

// ── Helper queries ───────────────────────────────────────────────────

/// Find the active schema_annotation contribution for a target config
/// type. Delegates to Phase 8's `yaml_renderer::load_schema_annotation_for`
/// lookup semantics (direct slug match first, then scan fallback).
fn find_active_annotation_id(
    conn: &Connection,
    target_schema_type: &str,
) -> Result<Option<String>> {
    // Direct-slug lookup (the common case — the Phase 5+8 migration
    // keys rows on applies_to).
    let direct: Option<String> = conn
        .query_row(
            "SELECT contribution_id FROM pyramid_config_contributions
             WHERE schema_type = 'schema_annotation'
               AND status = 'active'
               AND superseded_by_id IS NULL
               AND slug = ?1
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
            rusqlite::params![target_schema_type],
            |row| row.get(0),
        )
        .ok();
    if direct.is_some() {
        return Ok(direct);
    }

    // Scan fallback: walk all active schema_annotation contributions
    // and parse each body looking for `applies_to: <target>`. This
    // matches the yaml_renderer's fallback path. Cheap because
    // annotation count is O(number of config types).
    let mut stmt = conn.prepare(
        "SELECT contribution_id, yaml_content FROM pyramid_config_contributions
         WHERE schema_type = 'schema_annotation'
           AND status = 'active'
           AND superseded_by_id IS NULL
         ORDER BY created_at DESC, id DESC",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    for row in rows {
        let (contribution_id, body) = row?;
        if annotation_body_matches(&body, target_schema_type) {
            return Ok(Some(contribution_id));
        }
    }

    Ok(None)
}

/// Check whether a schema_annotation YAML body targets the requested
/// config type. Matches either `applies_to: <target>` or
/// `schema_type: <target>` at the top level. Simple line scan — avoids
/// a full YAML parse.
fn annotation_body_matches(yaml: &str, target: &str) -> bool {
    let mut applies_to: Option<String> = None;
    let mut schema_type: Option<String> = None;
    for line in yaml.lines() {
        if line.starts_with(|c: char| c.is_whitespace()) {
            continue;
        }
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("applies_to:") {
            let value = strip_yaml_value(rest);
            if !value.is_empty() {
                applies_to = Some(value.to_string());
            }
        } else if let Some(rest) = trimmed.strip_prefix("schema_type:") {
            let value = strip_yaml_value(rest);
            if !value.is_empty() {
                schema_type = Some(value.to_string());
            }
        }
    }
    let effective = applies_to.as_deref().or(schema_type.as_deref());
    effective == Some(target)
}

/// Strip YAML whitespace + optional quotes from a value string.
fn strip_yaml_value(raw: &str) -> &str {
    raw.trim()
        .trim_start_matches('"')
        .trim_end_matches('"')
        .trim_start_matches('\'')
        .trim_end_matches('\'')
}

/// Find the active generation skill contribution for a target config
/// type. Generation skills are keyed by slug = `generation/<target>.md`
/// (the path convention Phase 9 uses for the bundled prompt files) OR
/// by a topic tag containing both "generation" and the target name.
///
/// The slug convention is checked first because it matches what the
/// bundled manifest writes on first run. The topic-tag scan is a
/// fallback for future user-contributed generation skills that might
/// use a different slug convention.
fn find_active_generation_skill_id(
    conn: &Connection,
    target_schema_type: &str,
) -> Result<Option<String>> {
    // Slug-convention lookup: matches what the bundled manifest ships.
    let slug = format!("generation/{target_schema_type}.md");
    let direct: Option<String> = conn
        .query_row(
            "SELECT contribution_id FROM pyramid_config_contributions
             WHERE schema_type = 'skill'
               AND status = 'active'
               AND superseded_by_id IS NULL
               AND slug = ?1
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
            rusqlite::params![slug],
            |row| row.get(0),
        )
        .ok();
    if direct.is_some() {
        return Ok(direct);
    }

    // Topic-tag fallback: scan every active skill and parse its
    // metadata JSON, looking for a row whose `topics` contain BOTH
    // "generation" and the target schema_type. Bounded cost (skills
    // count is O(tens to low hundreds) and the JSON parse is cheap).
    let mut stmt = conn.prepare(
        "SELECT contribution_id, wire_native_metadata_json
         FROM pyramid_config_contributions
         WHERE schema_type = 'skill'
           AND status = 'active'
           AND superseded_by_id IS NULL
         ORDER BY created_at DESC, id DESC",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    for row in rows {
        let (contribution_id, metadata_json) = row?;
        if metadata_has_both_topics(&metadata_json, "generation", target_schema_type) {
            return Ok(Some(contribution_id));
        }
    }

    Ok(None)
}

/// Check whether a WireNativeMetadata JSON blob has both the required
/// topic tags. Avoids a full struct parse — just inspects the `topics`
/// array as a `serde_json::Value`.
fn metadata_has_both_topics(json: &str, topic_a: &str, topic_b: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return false;
    };
    let Some(topics) = value.get("topics").and_then(|v| v.as_array()) else {
        return false;
    };
    let has_a = topics.iter().any(|t| t.as_str() == Some(topic_a));
    let has_b = topics.iter().any(|t| t.as_str() == Some(topic_b));
    has_a && has_b
}

/// Find the bundled default contribution for a target config type.
/// Returns the contribution_id of the latest active row with
/// `source = 'bundled'` and the matching schema_type. May be `None` if
/// no bundled default exists (which is valid — some schema types don't
/// need a seed default).
fn find_bundled_default_id(conn: &Connection, target_schema_type: &str) -> Result<Option<String>> {
    let id: Option<String> = conn
        .query_row(
            "SELECT contribution_id FROM pyramid_config_contributions
             WHERE schema_type = ?1
               AND status = 'active'
               AND superseded_by_id IS NULL
               AND source = 'bundled'
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
            rusqlite::params![target_schema_type],
            |row| row.get(0),
        )
        .ok();
    Ok(id)
}

/// Return a human-readable display name for a known config type.
/// Unknown types fall back to the raw schema_type with underscores
/// replaced by spaces.
fn display_name_for(schema_type: &str) -> String {
    match schema_type {
        "evidence_policy" => "Evidence Policy".to_string(),
        "build_strategy" => "Build Strategy".to_string(),
        "dadbear_policy" => "DADBEAR Policy".to_string(),
        "watch_root" => "Watch Root".to_string(),
        "dadbear_norms" => "DADBEAR Norms".to_string(),
        "tier_routing" => "Tier Routing".to_string(),
        "custom_prompts" => "Custom Prompts".to_string(),
        "folder_ingestion_heuristics" => "Folder Ingestion Heuristics".to_string(),
        "step_overrides" => "Step Overrides".to_string(),
        "wire_discovery_weights" => "Wire Discovery Weights".to_string(),
        "wire_auto_update_settings" => "Wire Auto-Update Settings".to_string(),
        "pyramid_viz_config" => "Pyramid Viz Config".to_string(),
        "reconciliation_result" => "Reconciliation Result".to_string(),
        other => other.replace('_', " "),
    }
}

/// Return a short description for a known config type. Shown in the
/// schema picker UI. Unknown types fall back to a generic description.
fn description_for(schema_type: &str) -> String {
    match schema_type {
        "evidence_policy" => {
            "How the pyramid triages evidence requests — answer, defer, or skip".to_string()
        }
        "build_strategy" => {
            "How the pyramid spends compute across initial build + maintenance phases".to_string()
        }
        "dadbear_policy" => {
            "DADBEAR background auto-update loop policy (scan intervals, staleness propagation)".to_string()
        }
        "watch_root" => {
            "Per-source-path identity for DADBEAR file watching (contribution existence is the enable gate)".to_string()
        }
        "dadbear_norms" => {
            "DADBEAR timing and threshold norms — scan interval, debounce, staleness thresholds (global or per-pyramid)".to_string()
        }
        "tier_routing" => {
            "Maps model tier names to (provider, model) targets".to_string()
        }
        "custom_prompts" => {
            "Steers what the pyramid extracts and synthesizes".to_string()
        }
        "folder_ingestion_heuristics" => {
            "File patterns and ignore rules for folder ingestion".to_string()
        }
        "pyramid_viz_config" => {
            "Configuration for the pyramid visualization engine".to_string()
        }
        "reconciliation_result" => {
            "Post-evidence-loop reconciliation summary (orphans, central nodes, weight map, gaps)"
                .to_string()
        }
        _ => format!("Configuration for {schema_type}"),
    }
}

// ── Phase 9: flag_configs_for_migration helper ───────────────────────

/// Mark every active config contribution whose `schema_type` matches
/// the superseded schema_definition's target. Sets `needs_migration = 1`
/// so ToolsMode can surface a "Migrate" button. Phase 10 wires the
/// actual LLM-assisted migration flow; Phase 9 just sets the flag.
///
/// Called from Phase 4's dispatcher `schema_definition` branch (the
/// `flag_configs_for_migration` stub that Phase 9 is wiring up).
///
/// Returns the number of rows flagged.
pub fn flag_configs_needing_migration(
    conn: &Connection,
    target_schema_type: &str,
) -> Result<usize> {
    let rows = conn.execute(
        "UPDATE pyramid_config_contributions
         SET needs_migration = 1
         WHERE schema_type = ?1
           AND status = 'active'
           AND superseded_by_id IS NULL",
        rusqlite::params![target_schema_type],
    )?;
    debug!(
        target_schema_type,
        rows_flagged = rows,
        "flag_configs_needing_migration: set needs_migration = 1"
    );
    Ok(rows)
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::config_contributions::create_config_contribution_with_metadata;
    use crate::pyramid::db::init_pyramid_db;
    use crate::pyramid::wire_migration::walk_bundled_contributions_manifest;
    use crate::pyramid::wire_native_metadata::{default_wire_native_metadata, WireMaturity};

    fn mem_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        conn
    }

    fn seed_schema_definition(conn: &Connection, target: &str) -> String {
        let mut meta = default_wire_native_metadata("schema_definition", Some(target));
        meta.maturity = WireMaturity::Canon;
        meta.topics.push(target.to_string());
        create_config_contribution_with_metadata(
            conn,
            "schema_definition",
            Some(target),
            "{\"type\":\"object\"}",
            Some("test seed"),
            "bundled",
            Some("test"),
            "active",
            &meta,
        )
        .unwrap()
    }

    fn seed_schema_annotation(conn: &Connection, target: &str) -> String {
        let mut meta = default_wire_native_metadata("schema_annotation", Some(target));
        meta.maturity = WireMaturity::Canon;
        let body =
            format!("schema_type: {target}\napplies_to: {target}\nversion: 1\nfields: {{}}\n");
        create_config_contribution_with_metadata(
            conn,
            "schema_annotation",
            Some(target),
            &body,
            Some("test seed"),
            "bundled",
            Some("test"),
            "active",
            &meta,
        )
        .unwrap()
    }

    fn seed_generation_skill(conn: &Connection, target: &str) -> String {
        let slug = format!("generation/{target}.md");
        let mut meta = default_wire_native_metadata("skill", Some(&slug));
        meta.maturity = WireMaturity::Canon;
        meta.topics.push("generation".to_string());
        meta.topics.push(target.to_string());
        create_config_contribution_with_metadata(
            conn,
            "skill",
            Some(&slug),
            "generation prompt body",
            Some("test seed"),
            "bundled",
            Some("test"),
            "active",
            &meta,
        )
        .unwrap()
    }

    fn seed_bundled_default(conn: &Connection, target: &str) -> String {
        let mut meta = default_wire_native_metadata(target, None);
        meta.maturity = WireMaturity::Canon;
        let body = format!("schema_type: {target}\n");
        create_config_contribution_with_metadata(
            conn,
            target,
            None,
            &body,
            Some("test seed"),
            "bundled",
            Some("test"),
            "active",
            &meta,
        )
        .unwrap()
    }

    #[test]
    fn test_hydrate_from_contributions_empty() {
        let conn = mem_conn();
        let registry = SchemaRegistry::hydrate_from_contributions(&conn).unwrap();
        assert_eq!(registry.list().len(), 0);
    }

    #[test]
    fn test_hydrate_finds_minimal_schema_entry() {
        let conn = mem_conn();
        let definition_id = seed_schema_definition(&conn, "evidence_policy");
        let registry = SchemaRegistry::hydrate_from_contributions(&conn).unwrap();
        let entry = registry.get("evidence_policy").unwrap();
        assert_eq!(entry.schema_type, "evidence_policy");
        assert_eq!(entry.schema_definition_contribution_id, definition_id);
        assert!(entry.schema_annotation_contribution_id.is_empty());
        assert!(entry.generation_skill_contribution_id.is_empty());
        assert_eq!(entry.default_seed_contribution_id, None);
    }

    #[test]
    fn test_hydrate_joins_all_pieces() {
        let conn = mem_conn();
        let definition_id = seed_schema_definition(&conn, "evidence_policy");
        let annotation_id = seed_schema_annotation(&conn, "evidence_policy");
        let skill_id = seed_generation_skill(&conn, "evidence_policy");
        let default_id = seed_bundled_default(&conn, "evidence_policy");

        let registry = SchemaRegistry::hydrate_from_contributions(&conn).unwrap();
        let entry = registry.get("evidence_policy").unwrap();
        assert_eq!(entry.schema_definition_contribution_id, definition_id);
        assert_eq!(entry.schema_annotation_contribution_id, annotation_id);
        assert_eq!(entry.generation_skill_contribution_id, skill_id);
        assert_eq!(
            entry.default_seed_contribution_id.as_deref(),
            Some(default_id.as_str())
        );
    }

    #[test]
    fn test_list_returns_sorted_summaries() {
        let conn = mem_conn();
        seed_schema_definition(&conn, "evidence_policy");
        seed_schema_definition(&conn, "build_strategy");
        seed_schema_definition(&conn, "dadbear_policy");

        let registry = SchemaRegistry::hydrate_from_contributions(&conn).unwrap();
        let summaries = registry.list();
        let names: Vec<&str> = summaries.iter().map(|s| s.schema_type.as_str()).collect();
        assert_eq!(
            names,
            vec!["build_strategy", "dadbear_policy", "evidence_policy"]
        );
        assert_eq!(summaries[0].display_name, "Build Strategy");
    }

    #[test]
    fn test_invalidate_re_reads() {
        let conn = mem_conn();
        let registry = SchemaRegistry::hydrate_from_contributions(&conn).unwrap();
        assert_eq!(registry.list().len(), 0);

        // Seed a row after hydration — registry should NOT see it
        // until we call invalidate.
        seed_schema_definition(&conn, "evidence_policy");
        assert!(registry.get("evidence_policy").is_none());

        registry.invalidate(&conn).unwrap();
        assert!(registry.get("evidence_policy").is_some());
    }

    #[test]
    fn test_hydrate_from_bundled_manifest() {
        // Run the Phase 9 bundled migration and verify the registry
        // picks up all 5 schema types from the shipped manifest.
        let conn = mem_conn();
        let report = walk_bundled_contributions_manifest(&conn).unwrap();
        assert!(
            report.inserted >= 15,
            "expected >=15 bundled rows, got {}",
            report.inserted
        );
        assert_eq!(report.failed, 0);

        let registry = SchemaRegistry::hydrate_from_contributions(&conn).unwrap();
        let summaries = registry.list();
        let schema_types: Vec<&str> = summaries.iter().map(|s| s.schema_type.as_str()).collect();
        assert!(schema_types.contains(&"evidence_policy"));
        assert!(schema_types.contains(&"build_strategy"));
        assert!(schema_types.contains(&"dadbear_policy"));
        assert!(schema_types.contains(&"tier_routing"));
        assert!(schema_types.contains(&"custom_prompts"));

        // Every resolved schema should have a generation skill and a
        // bundled default (annotation may be optional for some types
        // where Phase 8 already seeded them).
        for summary in &summaries {
            assert!(
                summary.has_generation_skill,
                "{} has no generation skill",
                summary.schema_type
            );
            assert!(
                summary.has_default_seed,
                "{} has no default seed",
                summary.schema_type
            );
        }
    }

    /// Phase 0a-2 WS1 / Phase 0b WS-C: assert that each runtime-state
    /// schema (Phase 0a-2: `migration_marker`, `onboarding_state`,
    /// `node_identity_history`) AND each walker_* scope carrier
    /// (Phase 0b: `walker_provider_local`, `walker_provider_openrouter`,
    /// `walker_provider_fleet`, `walker_provider_market`,
    /// `walker_call_order`, `walker_slot_policy`) resolves ALL FOUR
    /// parts at boot — schema_definition, schema_annotation,
    /// generation_skill, and default_seed.
    ///
    /// Plan rev 1.0.2 named the four-part-completeness gap in
    /// schema_registry boot; WS-C extends the Phase 0a-2 assertion to
    /// the six walker_* schemas shipped by WS-B. The
    /// `compute_market_offer` case is tested separately (see
    /// `test_compute_market_offer_skill_registered_even_though_schema_parts_are_phase_2`)
    /// because Phase 2 owns its schema_definition / schema_annotation /
    /// default_seed per §8.1 — Phase 0b ships only the generation_skill.
    #[test]
    fn test_walker_schemas_four_part_complete() {
        let conn = mem_conn();
        let report = walk_bundled_contributions_manifest(&conn).unwrap();
        assert_eq!(
            report.failed, 0,
            "bundled manifest walk had {} failures",
            report.failed
        );

        let registry = SchemaRegistry::hydrate_from_contributions(&conn).unwrap();

        for schema_type in [
            // Phase 0a-2 WS1 (kept):
            "migration_marker",
            "onboarding_state",
            "node_identity_history",
            // Phase 0b WS-C additions — the six walker_* scope carriers
            // from WS-B's bundled manifest:
            "walker_provider_local",
            "walker_provider_openrouter",
            "walker_provider_fleet",
            "walker_provider_market",
            "walker_call_order",
            "walker_slot_policy",
        ] {
            let entry = registry.get(schema_type).unwrap_or_else(|| {
                panic!("{schema_type}: no schema_definition resolved from bundled manifest")
            });
            assert!(
                !entry.schema_definition_contribution_id.is_empty(),
                "{schema_type}: schema_definition contribution_id empty"
            );
            assert!(
                !entry.schema_annotation_contribution_id.is_empty(),
                "{schema_type}: schema_annotation contribution_id missing — four-part bundle incomplete"
            );
            assert!(
                !entry.generation_skill_contribution_id.is_empty(),
                "{schema_type}: generation_skill contribution_id missing — four-part bundle incomplete"
            );
            assert!(
                entry.default_seed_contribution_id.is_some(),
                "{schema_type}: default_seed contribution_id missing — four-part bundle incomplete"
            );
        }
    }

    /// Phase 0b WS-C: the `compute_market_offer` schema is intentionally
    /// split across phases per §8.1 — Phase 0b ships ONLY the
    /// `generation_skill` (so the walker-era Tools > Create card isn't
    /// dead text for bridge operators), and Phase 2 (compute market)
    /// ships the remaining three parts (schema_definition +
    /// schema_annotation + default_seed) alongside the provider-side
    /// offer CRUD. The seven-skill claim of §6 Phase 0b ("six walker_*
    /// + compute_market_offer") rests on the skill — NOT the full
    /// four-part bundle — landing here.
    ///
    /// This test asserts the skill contribution is in the manifest
    /// and that the SchemaRegistry does NOT surface a full entry for
    /// `compute_market_offer` (the absence of a schema_definition row
    /// is what Phase 2 fills in).
    #[test]
    fn test_compute_market_offer_skill_registered_even_though_schema_parts_are_phase_2() {
        let conn = mem_conn();
        let report = walk_bundled_contributions_manifest(&conn).unwrap();
        assert_eq!(
            report.failed, 0,
            "bundled manifest walk had {} failures",
            report.failed
        );

        // The bundled manifest ships a `skill` row with slug
        // `generation/compute_market_offer.md` (see
        // bundled_contributions.json entry
        // `bundled-skill-generation-compute_market_offer-v1`).
        let skill_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_config_contributions
                 WHERE schema_type = 'skill'
                   AND status = 'active'
                   AND superseded_by_id IS NULL
                   AND slug = 'generation/compute_market_offer.md'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            skill_count, 1,
            "Phase 0b WS-B ships exactly one `generation/compute_market_offer.md` skill; \
             found {skill_count}. Seven-skill claim of §6 Phase 0b depends on this row."
        );

        // The three Phase-2-owned parts (schema_definition,
        // schema_annotation, default_seed of schema_type
        // `compute_market_offer`) are NOT in the Phase 0b bundle. The
        // registry therefore has no entry for this schema_type — a
        // full-part registry entry will only appear once Phase 2 seeds
        // schema_definition.
        let registry = SchemaRegistry::hydrate_from_contributions(&conn).unwrap();
        assert!(
            registry.get("compute_market_offer").is_none(),
            "Phase 0b WS-B must not register a schema_definition for compute_market_offer; \
             that belongs to Phase 2 per §8.1. Registry returned an entry."
        );
    }

    /// Phase 0b WS-C: enforces plan §6 Phase 0b "every tier name used
    /// in bundled chain YAMLs resolves to a non-empty `model_list` via
    /// at least one provider-type in the bundled call-order."
    ///
    /// Purpose: catch Day-1 `tier_unresolved` regressions — a chain
    /// referencing a tier string that no bundled walker_provider_*
    /// `model_list` declares would fail at runtime with an empty
    /// resolve + chronicle event. This test catches the class before
    /// release.
    ///
    /// Scope:
    ///   * Bundled chain YAMLs = the chain recipes the binary ships
    ///     via `include_str!` in `chain_loader.rs::ensure_default_chains`
    ///     (tier-2 "embedded defaults"). The three placeholder constants
    ///     (`DEFAULT_CONVERSATION_CHAIN`, `DEFAULT_CODE_CHAIN`,
    ///     `DEFAULT_DOCUMENT_CHAIN`) reference only `mid` and are
    ///     trivially covered; the three real include_str! chains are
    ///     `question.yaml`, `extract-only.yaml`, `topical-vine.yaml`.
    ///   * Tier union = keys of `overrides.model_list` in every
    ///     bundled `walker_provider_*` default_seed (scope 4). The
    ///     bundled `walker_call_order.overrides_by_provider` scope-3
    ///     map is also scanned, but WS-B's seed leaves it empty.
    ///
    /// KNOWN COVERAGE GAP (WS-C finding, 2026-04-22): WS-B's bundled
    /// scope-4 seeds currently cover `{max, high, mid, extractor}`
    /// (openrouter) ∪ `{mid, high, extractor}` (market) = `{max, high,
    /// mid, extractor}`. Bundled chain YAMLs also reference `web` and
    /// `synth_heavy`:
    ///   * `question.yaml`:   `synth_heavy`, `extractor`, `web`
    ///   * `topical-vine.yaml`: `synth_heavy`, `web`
    ///   * `extract-only.yaml`: `mid`
    ///   * `DEFAULT_CONVERSATION_CHAIN` / `DEFAULT_CODE_CHAIN` /
    ///     `DEFAULT_DOCUMENT_CHAIN`: `mid`
    ///
    /// The bundled seeds therefore fail to cover `synth_heavy` and
    /// `web` — a real Day-1 `tier_unresolved` hole that WS-B's seed
    /// pass needs to close (add the two tiers to walker_provider_openrouter
    /// `model_list`, or extend walker_provider_market, OR the chain
    /// YAMLs must be migrated off those tier names).
    ///
    /// This test is scoped to a NON-INCLUSIVE allowlist — it checks
    /// only the tiers that ARE covered by bundled seeds, plus asserts
    /// the covered-set is non-empty. A companion `#[ignore]`'d test
    /// below encodes the full plan-spec assertion and fails loudly so
    /// the gap is visible on demand (`cargo test -- --ignored`).
    ///
    /// Once WS-B's follow-up lands seeds for the remaining tiers, the
    /// `#[ignore]` attribute should be removed (or this narrower test
    /// deleted) and the strict assertion becomes the canonical guard.
    #[test]
    fn test_bundled_tier_coverage() {
        let conn = mem_conn();
        walk_bundled_contributions_manifest(&conn).unwrap();

        let covered = bundled_provider_tier_union(&conn);
        assert!(
            !covered.is_empty(),
            "bundled walker_provider_* seeds declare zero tiers — \
             WS-B's manifest insertion is broken or missing model_list keys"
        );

        // Tiers referenced by the three placeholder constants + the
        // three real include_str! chains that ARE within bundled
        // scope-4 seed coverage as of WS-B. Kept narrow because
        // extending walker_provider_openrouter's model_list is WS-A /
        // WS-B follow-up scope, not WS-C's.
        let tiers_from_bundled_chains_that_are_covered: &[&str] = &["mid", "extractor"];

        for tier in tiers_from_bundled_chains_that_are_covered {
            assert!(
                covered.contains(*tier),
                "tier `{tier}` is referenced by bundled chains but not declared in any \
                 bundled walker_provider_* model_list — Day-1 `tier_unresolved` regression. \
                 Covered tiers: {:?}",
                {
                    let mut v: Vec<&str> = covered.iter().map(String::as_str).collect();
                    v.sort();
                    v
                }
            );
        }
    }

    /// Phase 0b WS-C: the strict form of `test_bundled_tier_coverage`
    /// that encodes the full plan §6 Phase 0b assertion — every tier
    /// referenced by a bundled chain YAML must resolve to a non-empty
    /// `model_list` via at least one provider-type in the bundled
    /// walker_* seeds. Now passes after Adam's 2026-04-22 fill-in of
    /// `synth_heavy` (moonshotai/kimi-k2.6) and `web` (xiaomi/mimo-v2.5-pro)
    /// on walker_provider_openrouter.
    ///
    /// Run with: `cargo test --lib test_bundled_tier_coverage_strict`
    #[test]
    fn test_bundled_tier_coverage_strict() {
        let conn = mem_conn();
        walk_bundled_contributions_manifest(&conn).unwrap();

        let covered = bundled_provider_tier_union(&conn);

        // Tiers referenced by bundled chain YAMLs shipped via
        // `include_str!` in `chain_loader.rs::ensure_default_chains`.
        // Pairs are (chain_name, tier_name) to produce a precise
        // failure message.
        let bundled_chain_tier_refs: &[(&str, &str)] = &[
            // extract-only.yaml
            ("extract-only.yaml", "mid"),
            // question.yaml
            ("question.yaml", "synth_heavy"),
            ("question.yaml", "extractor"),
            ("question.yaml", "web"),
            // topical-vine.yaml
            ("topical-vine.yaml", "synth_heavy"),
            ("topical-vine.yaml", "web"),
            // DEFAULT_CONVERSATION_CHAIN / _CODE_ / _DOCUMENT_
            // placeholder constants in chain_loader.rs
            ("DEFAULT_CONVERSATION_CHAIN", "mid"),
            ("DEFAULT_CODE_CHAIN", "mid"),
            ("DEFAULT_DOCUMENT_CHAIN", "mid"),
            // Walker v3 completion (2026-04-23): Rust-side Pattern-4
            // dispatch sites declare these tier names via the canonical
            // make_step_ctx_from_llm_config slot parameter. Each must
            // resolve to a non-empty model_list in the bundled seed.
            ("evidence_answering.rs (Pattern-4)", "evidence_loop"),
            ("stale_helpers.rs (Pattern-4)", "stale_l0"),
            ("stale_helpers_upper.rs (Pattern-4)", "stale_upper"),
            ("reroll.rs (Pattern-4)", "reroll"),
        ];

        let mut uncovered: Vec<(&str, &str)> = Vec::new();
        for (chain, tier) in bundled_chain_tier_refs {
            if !covered.contains(*tier) {
                uncovered.push((*chain, *tier));
            }
        }
        assert!(
            uncovered.is_empty(),
            "bundled chain tier references uncovered by any bundled \
             walker_provider_* model_list: {uncovered:?}. Covered tiers: {:?}",
            {
                let mut v: Vec<&str> = covered.iter().map(String::as_str).collect();
                v.sort();
                v
            }
        );
    }

    /// Phase 0b WS-C helper: compute the union of tier-name keys
    /// across every bundled `walker_provider_*` default_seed's
    /// `overrides.model_list`, plus every `walker_call_order`
    /// `overrides_by_provider.<provider>.model_list` (scope 3). These
    /// are the tiers a runtime walker can resolve from the shipped
    /// manifest before any operator refinement.
    ///
    /// Scoped to the four walker_provider_* carrier schema_types and
    /// the walker_call_order carrier. Reads the freshly-walked
    /// manifest rows out of `pyramid_config_contributions` so the
    /// helper stays honest to what the manifest ACTUALLY installs.
    fn bundled_provider_tier_union(conn: &Connection) -> std::collections::HashSet<String> {
        let mut out: std::collections::HashSet<String> = std::collections::HashSet::new();

        // Scope 4: each walker_provider_* carrier has its own default_seed
        // row keyed on schema_type = "walker_provider_*".
        let provider_schemas = [
            "walker_provider_local",
            "walker_provider_openrouter",
            "walker_provider_fleet",
            "walker_provider_market",
        ];
        for schema_type in provider_schemas {
            let yaml_opt: Option<String> = conn
                .query_row(
                    "SELECT yaml_content FROM pyramid_config_contributions
                     WHERE schema_type = ?1
                       AND status = 'active'
                       AND superseded_by_id IS NULL
                       AND source = 'bundled'
                     ORDER BY created_at DESC, id DESC
                     LIMIT 1",
                    rusqlite::params![schema_type],
                    |row| row.get(0),
                )
                .ok();
            if let Some(yaml) = yaml_opt {
                extend_with_model_list_keys(&yaml, "overrides", &mut out);
            }
        }

        // Scope 3: walker_call_order.overrides_by_provider.<provider>.model_list
        let call_order_yaml_opt: Option<String> = conn
            .query_row(
                "SELECT yaml_content FROM pyramid_config_contributions
                 WHERE schema_type = 'walker_call_order'
                   AND status = 'active'
                   AND superseded_by_id IS NULL
                   AND source = 'bundled'
                 ORDER BY created_at DESC, id DESC
                 LIMIT 1",
                [],
                |row| row.get(0),
            )
            .ok();
        if let Some(yaml) = call_order_yaml_opt {
            extend_with_call_order_tier_keys(&yaml, &mut out);
        }

        out
    }

    /// Parse `yaml_content`, descend into `<container>.model_list`, and
    /// collect map-key names (each key is a tier name). Silently no-op
    /// on parse failures or non-map model_list values — the test
    /// asserts separately that the covered-set is non-empty, so any
    /// silent failure surfaces there.
    fn extend_with_model_list_keys(
        yaml: &str,
        container_key: &str,
        out: &mut std::collections::HashSet<String>,
    ) {
        let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(yaml) else {
            return;
        };
        let Some(container) = doc.get(container_key) else {
            return;
        };
        let Some(model_list) = container.get("model_list") else {
            return;
        };
        let Some(map) = model_list.as_mapping() else {
            return;
        };
        for (key, _v) in map {
            if let Some(s) = key.as_str() {
                out.insert(s.to_string());
            }
        }
    }

    /// Parse `walker_call_order` YAML and collect tier-name keys from
    /// every `overrides_by_provider.<provider>.model_list` entry. The
    /// YAML's top-level `model_list` is not a valid field for this
    /// schema; tiers only live under per-provider override maps.
    fn extend_with_call_order_tier_keys(yaml: &str, out: &mut std::collections::HashSet<String>) {
        let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(yaml) else {
            return;
        };
        let Some(by_provider) = doc.get("overrides_by_provider") else {
            return;
        };
        let Some(map) = by_provider.as_mapping() else {
            return;
        };
        for (_provider, overrides) in map {
            let Some(model_list) = overrides.get("model_list") else {
                continue;
            };
            let Some(inner) = model_list.as_mapping() else {
                continue;
            };
            for (tier, _slugs) in inner {
                if let Some(s) = tier.as_str() {
                    out.insert(s.to_string());
                }
            }
        }
    }

    /// Phase 0b WS-C: lightweight skill-placeholder sanity test.
    /// For each bundled generation_skill body, extract every
    /// `{{placeholder}}` token and assert it is in the canonical
    /// placeholder set declared in
    /// `generative_config.rs::PlaceholderKey::from_token`:
    ///   * openrouter_live_slugs
    ///   * ollama_available_models
    ///   * market_surface_slugs
    ///   * patience_secs_default
    ///   * retry_http_count_default
    ///   * max_budget_credits_default
    ///
    /// Single-brace `{schema}` / `{intent}` / `{current_yaml}` /
    /// `{notes}` tokens are skill-template variables handled by
    /// `substitute_prompt_v2` directly (not placeholders) so they
    /// aren't checked here.
    ///
    /// Catches the classic skill-prompt typo ("{{openrouter_slugs}}"
    /// instead of "{{openrouter_live_slugs}}") before release. Does
    /// NOT duplicate any existing generative_config test — grep for
    /// "bundled.*placeholder" in generative_config.rs is empty.
    #[test]
    fn test_bundled_skills_reference_existing_placeholders() {
        use std::collections::HashSet;

        let known: HashSet<&'static str> = [
            "openrouter_live_slugs",
            "ollama_available_models",
            "market_surface_slugs",
            "patience_secs_default",
            "retry_http_count_default",
            "max_budget_credits_default",
        ]
        .into_iter()
        .collect();

        let conn = mem_conn();
        walk_bundled_contributions_manifest(&conn).unwrap();

        let mut stmt = conn
            .prepare(
                "SELECT slug, yaml_content FROM pyramid_config_contributions
                 WHERE schema_type = 'skill'
                   AND status = 'active'
                   AND superseded_by_id IS NULL
                   AND source = 'bundled'
                   AND slug LIKE 'generation/%'",
            )
            .unwrap();

        let rows: Vec<(String, String)> = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                    row.get::<_, String>(1)?,
                ))
            })
            .unwrap()
            .filter_map(Result::ok)
            .collect();

        assert!(
            !rows.is_empty(),
            "no bundled generation/* skills found; manifest walk failed or schema drift"
        );

        let mut unknown_refs: Vec<(String, String)> = Vec::new();
        for (slug, body) in &rows {
            for tok in extract_double_brace_tokens(body) {
                if !known.contains(tok.as_str()) {
                    unknown_refs.push((slug.clone(), tok));
                }
            }
        }
        assert!(
            unknown_refs.is_empty(),
            "bundled generation skills reference unknown {{{{placeholder}}}} tokens \
             (not in PlaceholderKey::from_token): {unknown_refs:?}"
        );
    }

    /// Scan a string for `{{token}}` patterns and return the tokens
    /// (content between the braces, trimmed). Ignores single-brace
    /// `{name}` expansions (those are skill-template variables, not
    /// placeholders). Bounded loop — no regex dependency.
    fn extract_double_brace_tokens(src: &str) -> Vec<String> {
        let mut out = Vec::new();
        let bytes = src.as_bytes();
        let mut i = 0;
        while i + 1 < bytes.len() {
            if bytes[i] == b'{' && bytes[i + 1] == b'{' {
                // Find the closing "}}"
                let start = i + 2;
                let mut j = start;
                while j + 1 < bytes.len() {
                    if bytes[j] == b'}' && bytes[j + 1] == b'}' {
                        break;
                    }
                    j += 1;
                }
                if j + 1 < bytes.len() {
                    let tok = std::str::from_utf8(&bytes[start..j])
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if !tok.is_empty() {
                        out.push(tok);
                    }
                    i = j + 2;
                    continue;
                } else {
                    break;
                }
            }
            i += 1;
        }
        out
    }

    #[test]
    fn test_annotation_body_matches_applies_to() {
        assert!(annotation_body_matches(
            "schema_type: chain_step_config\napplies_to: evidence_policy\nfields: {}\n",
            "evidence_policy"
        ));
        assert!(!annotation_body_matches(
            "schema_type: chain_step_config\napplies_to: evidence_policy\nfields: {}\n",
            "build_strategy"
        ));
        assert!(annotation_body_matches(
            "schema_type: dadbear_policy\nfields: {}\n",
            "dadbear_policy"
        ));
    }

    #[test]
    fn test_metadata_has_both_topics_matches() {
        let json = r#"{"contribution_type":"skill","topics":["generation","evidence_policy","wire-node"]}"#;
        assert!(metadata_has_both_topics(
            json,
            "generation",
            "evidence_policy"
        ));
        assert!(!metadata_has_both_topics(
            json,
            "generation",
            "build_strategy"
        ));
    }

    #[test]
    fn test_flag_configs_needing_migration_sets_column() {
        let conn = mem_conn();
        let target = "evidence_policy";
        let default_id = seed_bundled_default(&conn, target);

        // Before flagging: needs_migration = 0
        let before: i64 = conn
            .query_row(
                "SELECT needs_migration FROM pyramid_config_contributions
                 WHERE contribution_id = ?1",
                rusqlite::params![default_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(before, 0);

        let flagged = flag_configs_needing_migration(&conn, target).unwrap();
        assert_eq!(flagged, 1);

        let after: i64 = conn
            .query_row(
                "SELECT needs_migration FROM pyramid_config_contributions
                 WHERE contribution_id = ?1",
                rusqlite::params![default_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(after, 1);
    }

    #[test]
    fn test_flag_configs_skips_superseded_rows() {
        let conn = mem_conn();
        // Seed a bundled default and then a user supersession.
        let default_id = seed_bundled_default(&conn, "evidence_policy");

        // Manually mark the bundled row as superseded so the flagger
        // skips it.
        conn.execute(
            "UPDATE pyramid_config_contributions
             SET status = 'superseded'
             WHERE contribution_id = ?1",
            rusqlite::params![default_id],
        )
        .unwrap();

        let flagged = flag_configs_needing_migration(&conn, "evidence_policy").unwrap();
        assert_eq!(flagged, 0, "superseded rows must not be flagged");
    }
}
