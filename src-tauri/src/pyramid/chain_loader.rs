// pyramid/chain_loader.rs — YAML chain loader + prompt resolver
//
// Loads chain definitions from YAML files, resolves `$prompts/...` references
// to actual file contents, and provides chain discovery for the chains directory.

use std::path::Path;

use anyhow::{Context, Result};

use super::chain_engine::{ChainDefinition, ChainMetadata, validate_chain};

/// Load a chain definition from a YAML file.
///
/// Reads the YAML, deserializes into `ChainDefinition`, resolves any
/// `$prompts/...` instruction references to absolute file paths and reads
/// their content, then validates the result.
pub fn load_chain(yaml_path: &Path, chains_dir: &Path) -> Result<ChainDefinition> {
    let raw = std::fs::read_to_string(yaml_path)
        .with_context(|| format!("failed to read chain file: {}", yaml_path.display()))?;

    let mut def: ChainDefinition = serde_yaml::from_str(&raw)
        .with_context(|| format!("failed to parse chain YAML: {}", yaml_path.display()))?;

    resolve_prompt_refs(&mut def, chains_dir)
        .with_context(|| format!("failed to resolve prompt refs in {}", yaml_path.display()))?;

    let result = validate_chain(&def);
    if !result.valid {
        anyhow::bail!(
            "chain \"{}\" failed validation:\n  {}",
            def.id,
            result.errors.join("\n  ")
        );
    }
    if !result.warnings.is_empty() {
        tracing::warn!(
            chain_id = %def.id,
            "chain validation warnings:\n  {}",
            result.warnings.join("\n  ")
        );
    }

    Ok(def)
}

/// Resolve prompt file references in a chain definition.
///
/// Any step instruction starting with `$prompts/` is treated as a file
/// reference relative to `chains_dir`. The reference is replaced with the
/// file's contents so the executor can use it directly.
///
/// Example: `"$prompts/conversation/forward.md"` resolves to
/// `"{chains_dir}/prompts/conversation/forward.md"` and is replaced with
/// the file content.
fn resolve_prompt_refs(def: &mut ChainDefinition, chains_dir: &Path) -> Result<()> {
    for step in &mut def.steps {
        if let Some(ref instruction) = step.instruction {
            if let Some(rel_path) = instruction.strip_prefix("$prompts/") {
                let prompt_path = chains_dir.join("prompts").join(rel_path);
                let content = std::fs::read_to_string(&prompt_path).with_context(|| {
                    format!(
                        "step \"{}\": failed to read prompt file {}",
                        step.name,
                        prompt_path.display()
                    )
                })?;
                step.instruction = Some(content);
            }
        }

        // Also resolve merge_instruction refs
        if let Some(ref merge_instr) = step.merge_instruction {
            if let Some(rel_path) = merge_instr.strip_prefix("$prompts/") {
                let prompt_path = chains_dir.join("prompts").join(rel_path);
                let content = std::fs::read_to_string(&prompt_path).with_context(|| {
                    format!(
                        "step \"{}\": failed to read merge prompt file {}",
                        step.name,
                        prompt_path.display()
                    )
                })?;
                step.merge_instruction = Some(content);
            }
        }
    }
    Ok(())
}

/// Scan the chains directory for all `.yaml` files in `defaults/` and
/// `variants/` subdirectories. Returns metadata for each valid chain.
pub fn discover_chains(chains_dir: &Path) -> Result<Vec<ChainMetadata>> {
    let mut results = Vec::new();

    let scan_dirs = [
        (chains_dir.join("defaults"), true),
        (chains_dir.join("variants"), false),
    ];

    for (dir, is_default) in &scan_dirs {
        if !dir.exists() {
            continue;
        }

        let entries = std::fs::read_dir(dir)
            .with_context(|| format!("failed to read directory: {}", dir.display()))?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();

            if path.extension().map(|e| e == "yaml" || e == "yml").unwrap_or(false) {
                match load_chain_metadata(&path, *is_default) {
                    Ok(meta) => results.push(meta),
                    Err(e) => {
                        tracing::warn!(
                            path = %path.display(),
                            "skipping invalid chain file: {:#}",
                            e
                        );
                    }
                }
            }
        }
    }

    Ok(results)
}

/// Load just the metadata from a chain YAML file (does not resolve prompts).
fn load_chain_metadata(yaml_path: &Path, is_default: bool) -> Result<ChainMetadata> {
    let raw = std::fs::read_to_string(yaml_path)
        .with_context(|| format!("failed to read chain file: {}", yaml_path.display()))?;

    let def: ChainDefinition = serde_yaml::from_str(&raw)
        .with_context(|| format!("failed to parse chain YAML: {}", yaml_path.display()))?;

    Ok(ChainMetadata {
        id: def.id,
        name: def.name,
        content_type: def.content_type,
        version: def.version,
        author: def.author,
        step_count: def.steps.len(),
        file_path: yaml_path.to_string_lossy().into_owned(),
        is_default,
    })
}

/// Write default chain files to the chains directory if they don't exist.
///
/// Creates the directory structure:
/// ```text
/// {chains_dir}/
///   defaults/
///     conversation.yaml
///     code.yaml
///     document.yaml
///   variants/
///   prompts/
///     conversation/
///     code/
///     document/
/// ```
///
/// Called on first run to bootstrap the chain system.
pub fn ensure_default_chains(chains_dir: &Path) -> Result<()> {
    // Create directory structure
    let dirs_to_create = [
        chains_dir.join("defaults"),
        chains_dir.join("variants"),
        chains_dir.join("prompts").join("conversation"),
        chains_dir.join("prompts").join("code"),
        chains_dir.join("prompts").join("document"),
    ];

    for dir in &dirs_to_create {
        if !dir.exists() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("failed to create directory: {}", dir.display()))?;
        }
    }

    // Write default chain YAML files (only if they don't already exist)
    let defaults = [
        ("conversation.yaml", DEFAULT_CONVERSATION_CHAIN),
        ("code.yaml", DEFAULT_CODE_CHAIN),
        ("document.yaml", DEFAULT_DOCUMENT_CHAIN),
    ];

    for (filename, content) in &defaults {
        let path = chains_dir.join("defaults").join(filename);
        if !path.exists() {
            std::fs::write(&path, content)
                .with_context(|| format!("failed to write default chain: {}", path.display()))?;
            tracing::info!(path = %path.display(), "wrote default chain file");
        }
    }

    Ok(())
}

// ── Default chain YAML templates ─────────────────────────────────────────

const DEFAULT_CONVERSATION_CHAIN: &str = r#"schema_version: 1
id: "conversation-default"
name: "Conversation (Default)"
description: "Standard conversation pyramid build pipeline"
content_type: "conversation"
version: "0.1.0"
author: "wire-default"
defaults:
  model_tier: "mid"
  temperature: 0.3
  on_error: "retry(2)"
steps:
  - name: "placeholder"
    primitive: "compress"
    instruction: "Placeholder — will be replaced in Phase 3"
"#;

const DEFAULT_CODE_CHAIN: &str = r#"schema_version: 1
id: "code-default"
name: "Code (Default)"
description: "Standard code analysis pyramid build pipeline"
content_type: "code"
version: "0.1.0"
author: "wire-default"
defaults:
  model_tier: "mid"
  temperature: 0.2
  on_error: "retry(2)"
steps:
  - name: "placeholder"
    primitive: "extract"
    instruction: "Placeholder — will be replaced in Phase 3"
"#;

const DEFAULT_DOCUMENT_CHAIN: &str = r#"schema_version: 1
id: "document-default"
name: "Document (Default)"
description: "Standard document pyramid build pipeline"
content_type: "document"
version: "0.1.0"
author: "wire-default"
defaults:
  model_tier: "mid"
  temperature: 0.2
  on_error: "retry(2)"
steps:
  - name: "placeholder"
    primitive: "extract"
    instruction: "Placeholder — will be replaced in Phase 3"
"#;
