// Vocabulary Registry — structured command definitions for the planner
//
// Each vocabulary domain defines named commands with:
// - A prompt half (name, description, params) the LLM sees
// - A dispatch half (type, method, path, maps) the executor uses
//
// The vocabulary is the single source of truth for both prompt generation
// and command dispatch. It is shaped as a Wire contribution — bundled at
// compile time, synced from the Wire at runtime, cached locally.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VocabularyDomain {
    pub domain: String,
    pub version: u32,
    pub description: String,
    pub commands: Vec<CommandDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandDef {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub params: Vec<ParamDef>,
    pub dispatch: DispatchEntry,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamDef {
    pub name: String,
    #[serde(rename = "type")]
    pub param_type: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub description: Option<String>,
    /// "args" (default) = LLM supplies value; "context" = executor injects from app state
    #[serde(default = "default_source")]
    pub source: String,
}

fn default_source() -> String {
    "args".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DispatchEntry {
    #[serde(rename = "tauri")]
    Tauri,

    #[serde(rename = "wire_api")]
    WireApi {
        method: String,
        path: String,
        #[serde(default)]
        body_map: Option<HashMap<String, serde_json::Value>>,
        #[serde(default)]
        query_map: Option<HashMap<String, String>>,
        #[serde(default)]
        headers: Option<HashMap<String, String>>,
    },

    #[serde(rename = "operator_api")]
    OperatorApi {
        method: String,
        path: String,
        #[serde(default)]
        body_map: Option<HashMap<String, serde_json::Value>>,
        #[serde(default)]
        query_map: Option<HashMap<String, String>>,
    },

    #[serde(rename = "navigate")]
    Navigate {
        mode: String,
        #[serde(default)]
        view: Option<String>,
        #[serde(default)]
        props_map: Option<HashMap<String, String>>,
    },
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct VocabularyRegistry {
    pub domains: Vec<VocabularyDomain>,
    dispatch_table: HashMap<String, (DispatchEntry, Vec<ParamDef>)>,
}

impl VocabularyRegistry {
    /// Build a registry from a list of parsed domains.
    /// Warns on duplicate command names — last-loaded domain wins.
    pub fn from_domains(domains: Vec<VocabularyDomain>) -> Self {
        let mut dispatch_table = HashMap::new();
        for domain in &domains {
            for cmd in &domain.commands {
                if let Some(existing) = dispatch_table.get(&cmd.name) {
                    let (_, _): &(DispatchEntry, Vec<ParamDef>) = existing;
                    tracing::warn!(
                        "Duplicate command name '{}' in domain '{}' — overwriting previous definition",
                        cmd.name, domain.domain
                    );
                }
                dispatch_table.insert(
                    cmd.name.clone(),
                    (cmd.dispatch.clone(), cmd.params.clone()),
                );
            }
        }
        VocabularyRegistry {
            domains,
            dispatch_table,
        }
    }

    /// Generate LLM-friendly prompt text from the vocabulary.
    /// Excludes dispatch blocks — the LLM only sees name, description, and params.
    pub fn to_prompt_text(&self) -> String {
        let mut out = String::new();
        for domain in &self.domains {
            out.push_str(&format!("# Category: {}\n\n", domain.description));
            out.push_str("## Commands\n\n");
            for cmd in &domain.commands {
                out.push_str(&format!("### {}\n", cmd.name));
                out.push_str(&format!("{}\n", cmd.description));

                // Only show user-facing params (source: args), not context-injected ones
                let user_params: Vec<&ParamDef> = cmd
                    .params
                    .iter()
                    .filter(|p| p.source == "args")
                    .collect();

                if !user_params.is_empty() {
                    out.push_str("Args: { ");
                    let parts: Vec<String> = user_params
                        .iter()
                        .map(|p| {
                            let mut s = format!("{}: {}", p.name, p.param_type);
                            if p.required {
                                s.push_str(" (required)");
                            }
                            if let Some(ref desc) = p.description {
                                s.push_str(&format!(" — {}", desc));
                            }
                            s
                        })
                        .collect();
                    out.push_str(&parts.join(", "));
                    out.push_str(" }\n");
                }
                out.push('\n');
            }
            out.push_str("---\n\n");
        }
        out
    }

    /// Serialize the dispatch table as JSON for the frontend.
    /// Uses the deduplicated dispatch_table to ensure consistency with backend.
    pub fn to_frontend_registry(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        for (name, (dispatch, params)) in &self.dispatch_table {
            let entry = serde_json::json!({
                "dispatch": dispatch,
                "params": params,
            });
            map.insert(name.clone(), entry);
        }
        serde_json::Value::Object(map)
    }

    /// Validate all domains. Returns a list of validation errors.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        let allowed_path_prefixes = ["/api/v1/"];
        let allowed_methods = ["GET", "POST", "PUT", "PATCH", "DELETE"];

        for domain in &self.domains {
            for cmd in &domain.commands {
                match &cmd.dispatch {
                    DispatchEntry::WireApi {
                        ref method,
                        ref path,
                        ref body_map,
                        ref query_map,
                        ref headers,
                    } => {
                        self.validate_api_dispatch(
                            &domain.domain, &cmd.name, method, path,
                            body_map.as_ref(), query_map.as_ref(), headers.as_ref(),
                            &cmd.params, &allowed_path_prefixes, &allowed_methods,
                            &mut errors,
                        );
                    }
                    DispatchEntry::OperatorApi {
                        ref method,
                        ref path,
                        ref body_map,
                        ref query_map,
                    } => {
                        self.validate_api_dispatch(
                            &domain.domain, &cmd.name, method, path,
                            body_map.as_ref(), query_map.as_ref(), None,
                            &cmd.params, &allowed_path_prefixes, &allowed_methods,
                            &mut errors,
                        );
                    }
                    DispatchEntry::Navigate { ref props_map, .. } => {
                        // Validate props_map template tokens match declared params
                        if let Some(pm) = props_map {
                            let param_names: Vec<&str> =
                                cmd.params.iter().map(|p| p.name.as_str()).collect();
                            for val in pm.values() {
                                for token in extract_template_tokens(val) {
                                    if !param_names.contains(&token.as_str()) {
                                        errors.push(format!(
                                            "{}.{}: props_map template '{{{{{}}}}}' has no matching param",
                                            domain.domain, cmd.name, token
                                        ));
                                    }
                                }
                            }
                        }
                    }
                    DispatchEntry::Tauri => {
                        // Tauri commands don't need dispatch validation
                    }
                }
            }
        }
        errors
    }

    #[allow(clippy::too_many_arguments)]
    fn validate_api_dispatch(
        &self,
        domain_name: &str,
        cmd_name: &str,
        method: &str,
        path: &str,
        body_map: Option<&HashMap<String, serde_json::Value>>,
        query_map: Option<&HashMap<String, String>>,
        headers: Option<&HashMap<String, String>>,
        params: &[ParamDef],
        allowed_path_prefixes: &[&str],
        allowed_methods: &[&str],
        errors: &mut Vec<String>,
    ) {
        // Check path prefix
        if !allowed_path_prefixes.iter().any(|p| path.starts_with(p)) {
            errors.push(format!(
                "{}.{}: path '{}' does not start with an allowed prefix",
                domain_name, cmd_name, path
            ));
        }
        // Check method
        if !allowed_methods.contains(&method) {
            errors.push(format!(
                "{}.{}: invalid method '{}'",
                domain_name, cmd_name, method
            ));
        }
        // Check no path traversal
        if path.contains("..") || path.contains("://") {
            errors.push(format!(
                "{}.{}: path '{}' contains traversal or scheme",
                domain_name, cmd_name, path
            ));
        }
        // Check all {{param}} tokens map to declared params
        let param_names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
        for token in extract_template_tokens(path) {
            if !param_names.contains(&token.as_str()) {
                errors.push(format!(
                    "{}.{}: path template '{{{{{}}}}}' has no matching param",
                    domain_name, cmd_name, token
                ));
            }
        }
        if let Some(bm) = body_map {
            for val in bm.values() {
                if let Some(s) = val.as_str() {
                    for token in extract_template_tokens(s) {
                        if !param_names.contains(&token.as_str()) {
                            errors.push(format!(
                                "{}.{}: body_map template '{{{{{}}}}}' has no matching param",
                                domain_name, cmd_name, token
                            ));
                        }
                    }
                }
            }
        }
        if let Some(qm) = query_map {
            for val in qm.values() {
                for token in extract_template_tokens(val) {
                    if !param_names.contains(&token.as_str()) {
                        errors.push(format!(
                            "{}.{}: query_map template '{{{{{}}}}}' has no matching param",
                            domain_name, cmd_name, token
                        ));
                    }
                }
            }
        }
        if let Some(h) = headers {
            for val in h.values() {
                for token in extract_template_tokens(val) {
                    if !param_names.contains(&token.as_str()) {
                        errors.push(format!(
                            "{}.{}: header template '{{{{{}}}}}' has no matching param",
                            domain_name, cmd_name, token
                        ));
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract {{token}} placeholders from a template string.
fn extract_template_tokens(s: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut rest = s;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        if let Some(end) = after.find("}}") {
            tokens.push(after[..end].to_string());
            rest = &after[end + 2..];
        } else {
            break;
        }
    }
    tokens
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Load vocabulary from YAML files in a directory.
pub fn load_from_directory(dir: &std::path::Path) -> Result<VocabularyRegistry, String> {
    let mut domains = Vec::new();
    if !dir.exists() {
        return Ok(VocabularyRegistry::from_domains(domains));
    }

    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("Failed to read vocabulary dir: {}", e))?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map_or(false, |ext| ext == "yaml" || ext == "yml")
        })
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_yaml::from_str::<VocabularyDomain>(&contents) {
                Ok(domain) => domains.push(domain),
                Err(e) => {
                    tracing::warn!(
                        "Failed to parse vocabulary file {:?}: {}",
                        path.file_name(),
                        e
                    );
                    // Per-domain fallback: skip this domain, continue with others
                }
            },
            Err(e) => {
                tracing::warn!(
                    "Failed to read vocabulary file {:?}: {}",
                    path.file_name(),
                    e
                );
            }
        }
    }

    let registry = VocabularyRegistry::from_domains(domains);

    // Validate and warn
    let validation_errors = registry.validate();
    for err in &validation_errors {
        tracing::warn!("Vocabulary validation: {}", err);
    }

    Ok(registry)
}

/// Load bundled vocabulary compiled into the binary.
pub fn load_bundled() -> VocabularyRegistry {
    let yaml_files: &[(&str, &str)] = &[
        ("fleet_manage", include_str!("../../chains/vocabulary_yaml/fleet_manage.yaml")),
        ("fleet_mesh", include_str!("../../chains/vocabulary_yaml/fleet_mesh.yaml")),
        ("fleet_tasks", include_str!("../../chains/vocabulary_yaml/fleet_tasks.yaml")),
        ("knowledge_docs", include_str!("../../chains/vocabulary_yaml/knowledge_docs.yaml")),
        ("knowledge_sync", include_str!("../../chains/vocabulary_yaml/knowledge_sync.yaml")),
        ("navigate", include_str!("../../chains/vocabulary_yaml/navigate.yaml")),
        ("pyramid_build", include_str!("../../chains/vocabulary_yaml/pyramid_build.yaml")),
        ("pyramid_explore", include_str!("../../chains/vocabulary_yaml/pyramid_explore.yaml")),
        ("pyramid_manage", include_str!("../../chains/vocabulary_yaml/pyramid_manage.yaml")),
        ("system", include_str!("../../chains/vocabulary_yaml/system.yaml")),
        ("wire_compose", include_str!("../../chains/vocabulary_yaml/wire_compose.yaml")),
        ("wire_economics", include_str!("../../chains/vocabulary_yaml/wire_economics.yaml")),
        ("wire_games", include_str!("../../chains/vocabulary_yaml/wire_games.yaml")),
        ("wire_search", include_str!("../../chains/vocabulary_yaml/wire_search.yaml")),
        ("wire_social", include_str!("../../chains/vocabulary_yaml/wire_social.yaml")),
    ];

    let mut domains = Vec::new();
    for (name, contents) in yaml_files {
        match serde_yaml::from_str::<VocabularyDomain>(contents) {
            Ok(domain) => domains.push(domain),
            Err(e) => {
                tracing::error!("Failed to parse bundled vocabulary '{}': {}", name, e);
            }
        }
    }
    VocabularyRegistry::from_domains(domains)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bundled_vocabulary_parses() {
        let registry = load_bundled();
        println!("Loaded {} domains", registry.domains.len());
        for domain in &registry.domains {
            println!("  {} — {} commands", domain.domain, domain.commands.len());
        }
        println!("Dispatch table: {} entries", registry.dispatch_table.len());
        assert!(registry.domains.len() > 0, "No domains loaded");
        assert!(registry.dispatch_table.len() > 50, "Too few commands");

        // Check validation
        let errors = registry.validate();
        for err in &errors {
            println!("VALIDATION ERROR: {}", err);
        }

        // Check prompt text
        let prompt = registry.to_prompt_text();
        println!("\n--- PROMPT TEXT ({} chars) ---\n{}", prompt.len(), &prompt[..500.min(prompt.len())]);
        assert!(prompt.len() > 1000, "Prompt text too short: {} chars", prompt.len());
    }
}
