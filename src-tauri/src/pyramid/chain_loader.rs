// pyramid/chain_loader.rs — YAML chain loader + prompt resolver
//
// Loads chain definitions from YAML files, resolves `$prompts/...` references
// to actual file contents, and provides chain discovery for the chains directory.

use std::path::Path;

use anyhow::{Context, Result};

use super::chain_engine::{validate_chain, ChainDefinition, ChainMetadata};

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
/// Any step instruction starting with `$prompts/` is treated as a
/// reference that should be resolved against the Phase 5
/// `pyramid_config_contributions` store first (via the global prompt
/// cache), with a disk fallback for prompts that weren't migrated or
/// for tests that never stashed a DB path.
///
/// **Phase 5 resolution order** (per `docs/specs/wire-contribution-mapping.md`
/// → "Prompt lookup cache (runtime resolution from contributions)"):
///
/// 1. Try the global prompt cache (`prompt_cache::resolve_prompt_global`).
///    On a cache hit, return the contribution body directly. On a
///    cache miss, the cache opens a short-lived reader connection to
///    the stashed pyramid.db path, queries the active skill
///    contribution, and warms the cache.
/// 2. On any not-found outcome (path not stashed, DB error, no active
///    skill contribution matching the normalized path), fall back to
///    reading the on-disk prompt file at
///    `{chains_dir}/prompts/<rel_path>`. This preserves compatibility
///    with tests and the pre-migration state.
///
/// Example: `"$prompts/conversation/forward.md"` first asks the
/// global prompt cache for `"conversation/forward.md"`; if the Phase
/// 5 migration ran, this hits the migrated `skill` contribution and
/// returns the body. If the cache has no row (fresh test DB, or a
/// prompt added after first-run migration), the loader falls back to
/// `"{chains_dir}/prompts/conversation/forward.md"`.
fn resolve_prompt_refs(def: &mut ChainDefinition, chains_dir: &Path) -> Result<()> {
    resolve_step_refs(&mut def.steps, chains_dir)
}

fn resolve_step_refs(steps: &mut [crate::pyramid::chain_engine::ChainStep], chains_dir: &Path) -> Result<()> {
    let resolve_prompt = |prompt_ref: &str, step_name: &str, field_name: &str| -> Result<String> {
        if let Some(rel_path) = prompt_ref.strip_prefix("$prompts/") {
            // Phase 5: try the contribution store first via the
            // global prompt cache. On hit, return the contribution
            // body directly; on miss, fall back to disk.
            match crate::pyramid::prompt_cache::resolve_prompt_global(prompt_ref) {
                Ok(Some(body)) => {
                    tracing::trace!(
                        step = step_name,
                        field = field_name,
                        prompt = rel_path,
                        "resolved prompt from contribution store (Phase 5 cache hit)"
                    );
                    return Ok(body);
                }
                Ok(None) => {
                    tracing::trace!(
                        step = step_name,
                        field = field_name,
                        prompt = rel_path,
                        "prompt cache miss — falling back to disk"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        step = step_name,
                        field = field_name,
                        prompt = rel_path,
                        error = %e,
                        "prompt cache errored — falling back to disk"
                    );
                }
            }

            let prompt_path = chains_dir.join("prompts").join(rel_path);
            return std::fs::read_to_string(&prompt_path).with_context(|| {
                format!(
                    "step \"{}\": failed to read {} prompt file {}",
                    step_name,
                    field_name,
                    prompt_path.display()
                )
            });
        }
        Ok(prompt_ref.to_string())
    };

    for step in steps {
        if let Some(ref instruction) = step.instruction {
            step.instruction = Some(resolve_prompt(instruction, &step.name, "instruction")?);
        }

        if let Some(ref cluster_instruction) = step.cluster_instruction {
            step.cluster_instruction =
                Some(resolve_prompt(cluster_instruction, &step.name, "cluster")?);
        }

        if let Some(ref merge_instr) = step.merge_instruction {
            step.merge_instruction = Some(resolve_prompt(merge_instr, &step.name, "merge")?);
        }

        if let Some(ref heal_instr) = step.heal_instruction {
            step.heal_instruction = Some(resolve_prompt(heal_instr, &step.name, "heal_instruction")?);
        }

        if let Some(instruction_map) = step.instruction_map.as_mut() {
            for (key, value) in instruction_map.iter_mut() {
                *value = resolve_prompt(value, &step.name, key)?;
            }
        }

        if let Some(ref mut inner_steps) = step.steps {
            resolve_step_refs(inner_steps, chains_dir)?;
        }
    }
    Ok(())
}

/// Scan the chains directory for all `.yaml` files in `defaults/`,
/// `defaults/starter/`, and `variants/` subdirectories. Returns metadata
/// for each valid chain.
///
/// `defaults/starter/` is where post-build accretion v5 starter chains
/// (accretion_handler, judge, reconciler, etc.) ship. It's a sibling
/// directory to the content-type chains in `defaults/` so they don't
/// clash with operator-authored variants.
pub fn discover_chains(chains_dir: &Path) -> Result<Vec<ChainMetadata>> {
    let mut results = Vec::new();

    let scan_dirs = [
        (chains_dir.join("defaults"), true),
        (chains_dir.join("defaults").join("starter"), true),
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

            if path
                .extension()
                .map(|e| e == "yaml" || e == "yml")
                .unwrap_or(false)
            {
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

/// Resolve a chain by its YAML `id:` field. Scans `discover_chains`
/// results for a match, then loads the full chain (with prompts resolved)
/// via `load_chain`.
///
/// Used by post-build accretion v5's role-bound dispatch: when a
/// `StepOperation::RoleBound`-style work item dispatches, the supervisor
/// resolves the binding's `handler_chain_id` to a loaded chain via this
/// function.
///
/// Phase 1 verifier: raises loudly if multiple chains across
/// `defaults/`, `defaults/starter/`, and `variants/` share the same id.
/// Silent first-match would make operator-authored variant chains
/// indistinguishable from starter chains at resolution time and mask
/// clashes introduced by accident. Per feedback_loud_deferrals.
pub fn load_chain_by_id(chain_id: &str, chains_dir: &Path) -> Result<ChainDefinition> {
    let discovered = discover_chains(chains_dir)?;
    let matches: Vec<_> = discovered
        .into_iter()
        .filter(|m| m.id == chain_id)
        .collect();
    match matches.len() {
        0 => Err(anyhow::anyhow!("chain not found by id: '{chain_id}'")),
        1 => load_chain(Path::new(&matches[0].file_path), chains_dir),
        n => {
            let paths: Vec<String> =
                matches.iter().map(|m| m.file_path.clone()).collect();
            Err(anyhow::anyhow!(
                "ambiguous chain id '{chain_id}': {n} chains share this id across discovered directories: {}",
                paths.join(", ")
            ))
        }
    }
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
///     question.yaml
///     extract-only.yaml
///   variants/
///   prompts/
///     conversation/
///     code/
///     document/
///     question/
///     shared/
///     planner/
/// ```
///
/// Called on first run to bootstrap the chain system.
/// Two-tier chain sync strategy:
///
/// **Tier 1 (source tree present):** Copy the entire source tree `chains/` directory
/// into the runtime data dir. Source tree is canonical — always overwrites.
/// Detected via `source_chains_dir` parameter (set from CARGO_MANIFEST_DIR in dev,
/// or checked alongside the binary in release).
///
/// **Tier 2 (no source tree / release standalone):** Bootstrap with embedded defaults,
/// but only write files that don't already exist. Preserves user's runtime chain files
/// across app restarts.
pub fn ensure_default_chains(
    chains_dir: &Path,
    source_chains_dir: Option<&Path>,
) -> Result<()> {
    // Create directory structure
    let dirs_to_create = [
        chains_dir.join("defaults"),
        chains_dir.join("variants"),
        chains_dir.join("prompts").join("conversation"),
        chains_dir.join("prompts").join("conversation-chronological"),
        chains_dir.join("prompts").join("conversation-episodic"),
        chains_dir.join("prompts").join("code"),
        chains_dir.join("prompts").join("document"),
        chains_dir.join("prompts").join("question"),
        chains_dir.join("prompts").join("shared"),
        chains_dir.join("prompts").join("planner"),
        // Phase 16: topical vine prompts for vine-of-vines composition.
        chains_dir.join("prompts").join("vine"),
    ];

    for dir in &dirs_to_create {
        if !dir.exists() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("failed to create directory: {}", dir.display()))?;
        }
    }

    // ── Tier 1: Source tree wins ─────────────────────────────────────────
    if let Some(src) = source_chains_dir {
        if src.exists() && src.is_dir() {
            tracing::info!(
                src = %src.display(),
                dst = %chains_dir.display(),
                "syncing chains from source tree"
            );
            copy_dir_recursive(src, chains_dir)?;
            return Ok(());
        }
    }

    // ── Tier 2: Embedded defaults (bootstrap only) ──────────────────────
    let defaults: &[(&str, &str)] = &[
        ("conversation.yaml", DEFAULT_CONVERSATION_CHAIN),
        ("code.yaml", DEFAULT_CODE_CHAIN),
        ("document.yaml", DEFAULT_DOCUMENT_CHAIN),
        ("question.yaml", include_str!("../../../chains/defaults/question.yaml")),
        ("extract-only.yaml", include_str!("../../../chains/defaults/extract-only.yaml")),
        // Phase 16: topical vine recipe for vine-of-vines composition and
        // folder ingestion (Phase 17). Vines route to this chain via
        // chain_registry::resolve_chain_for_slug.
        (
            "topical-vine.yaml",
            include_str!("../../../chains/defaults/topical-vine.yaml"),
        ),
    ];

    for (filename, content) in defaults {
        let path = chains_dir.join("defaults").join(filename);
        if !path.exists() {
            std::fs::write(&path, content)
                .with_context(|| format!("failed to write default chain: {}", path.display()))?;
            tracing::info!(path = %path.display(), "bootstrapped default chain file");
        }
    }

    // Phase 16: bundle vine prompts for release builds so the topical
    // vine chain can resolve its $prompts/vine/* references even when
    // the source tree isn't present.
    let vine_prompts: &[(&str, &str)] = &[
        (
            "topical_cluster.md",
            include_str!("../../../chains/prompts/vine/topical_cluster.md"),
        ),
        (
            "topical_synthesis.md",
            include_str!("../../../chains/prompts/vine/topical_synthesis.md"),
        ),
        (
            "topical_apex.md",
            include_str!("../../../chains/prompts/vine/topical_apex.md"),
        ),
    ];
    for (filename, content) in vine_prompts {
        let path = chains_dir.join("prompts").join("vine").join(filename);
        if !path.exists() {
            std::fs::write(&path, content)
                .with_context(|| format!("failed to write vine prompt: {}", path.display()))?;
            tracing::info!(path = %path.display(), "bootstrapped bundled vine prompt");
        }
    }

    // Write planner system prompt (bundled at compile time) — bootstrap only
    let planner_prompt_path = chains_dir.join("prompts").join("planner").join("planner-system.md");
    if !planner_prompt_path.exists() {
        let prompt_content = include_str!("../../../chains/prompts/planner/planner-system.md");
        std::fs::write(&planner_prompt_path, prompt_content)
            .with_context(|| format!("failed to write planner prompt: {}", planner_prompt_path.display()))?;
        tracing::info!(path = %planner_prompt_path.display(), "bootstrapped bundled planner-system.md");
    }

    // Write question prompts (bundled at compile time) — bootstrap only
    let question_prompts: &[(&str, &str)] = &[
        ("enhance_question.md", include_str!("../../../chains/prompts/question/enhance_question.md")),
        ("decompose.md", include_str!("../../../chains/prompts/question/decompose.md")),
        ("decompose_delta.md", include_str!("../../../chains/prompts/question/decompose_delta.md")),
        ("extraction_schema.md", include_str!("../../../chains/prompts/question/extraction_schema.md")),
    ];
    for (filename, content) in question_prompts {
        let path = chains_dir.join("prompts").join("question").join(filename);
        if !path.exists() {
            std::fs::write(&path, content)
                .with_context(|| format!("failed to write question prompt: {}", path.display()))?;
            tracing::info!(path = %path.display(), "bootstrapped bundled question prompt");
        }
    }

    // Write shared prompts (bundled at compile time) — bootstrap only
    let shared_prompts: &[(&str, &str)] = &[
        ("heal_json.md", include_str!("../../../chains/prompts/shared/heal_json.md")),
        ("merge_sub_chunks.md", include_str!("../../../chains/prompts/shared/merge_sub_chunks.md")),
    ];
    for (filename, content) in shared_prompts {
        let path = chains_dir.join("prompts").join("shared").join(filename);
        if !path.exists() {
            std::fs::write(&path, content)
                .with_context(|| format!("failed to write shared prompt: {}", path.display()))?;
            tracing::info!(path = %path.display(), "bootstrapped bundled shared prompt");
        }
    }

    Ok(())
}

/// Recursively copy `src` directory contents into `dst`, overwriting existing files.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    if !dst.exists() {
        std::fs::create_dir_all(dst)
            .with_context(|| format!("failed to create dir: {}", dst.display()))?;
    }
    for entry in std::fs::read_dir(src)
        .with_context(|| format!("failed to read dir: {}", src.display()))?
    {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::write(&dst_path, std::fs::read(&src_path)?)
                .with_context(|| format!("failed to copy {} → {}", src_path.display(), dst_path.display()))?;
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

#[cfg(test)]
mod phase16_tests {
    //! Phase 16 chain-loader tests: ensure the bundled topical-vine chain
    //! YAML parses cleanly and validates as a legal chain definition.

    use crate::pyramid::chain_engine::{validate_chain, ChainDefinition};

    /// The bundled topical-vine.yaml is compiled into the binary via
    /// `include_str!` in `ensure_default_chains`. This test reads the same
    /// bundled content and verifies it parses, validates, and declares the
    /// correct `content_type`, `id`, and primitive shape.
    #[test]
    fn test_topical_vine_bundled_chain_parses_and_validates() {
        let bundled = include_str!("../../../chains/defaults/topical-vine.yaml");
        let def: ChainDefinition =
            serde_yaml::from_str(bundled).expect("topical-vine.yaml must parse");

        assert_eq!(def.id, "topical-vine");
        assert_eq!(def.content_type, "vine");
        assert!(def.steps.len() >= 4);

        // Expect the first step to be a cross_build_input step that
        // loads the vine's registered children.
        assert_eq!(def.steps[0].primitive, "cross_build_input");

        // Validation must pass cleanly (no errors).
        let result = validate_chain(&def);
        assert!(
            result.valid,
            "topical-vine chain must validate, got errors: {:?}",
            result.errors
        );
    }

    /// Verify that the chain declares at least one `recursive_pair` step —
    /// this is the recursive pair-adjacent synthesis that builds the vine
    /// apex from cluster summaries.
    #[test]
    fn test_topical_vine_has_recursive_pair_step() {
        let bundled = include_str!("../../../chains/defaults/topical-vine.yaml");
        let def: ChainDefinition = serde_yaml::from_str(bundled).unwrap();
        let has_recursive_pair = def.steps.iter().any(|s| s.recursive_pair);
        assert!(
            has_recursive_pair,
            "topical-vine must include a recursive_pair step that synthesizes up to apex"
        );
    }

    /// Wanderer fix regression test: the `upper_synthesis` recursive_pair
    /// step must declare `depth: 1` (source depth for pairing). The
    /// previous value of `2` caused `execute_recursive_pair` to read
    /// `get_nodes_at_depth(slug, 2)` which returns 0 nodes — because
    /// `cluster_synthesis` writes its cluster nodes at depth 1, not 2.
    /// With `depth: 2` the recursive_pair loop exits immediately with an
    /// empty apex id and the vine build silently completes with no apex.
    /// See chain_executor::execute_recursive_pair for the `starting_depth`
    /// semantics: `starting_depth = step.depth.unwrap_or(1)`, target
    /// depth is `starting_depth + 1`.
    #[test]
    fn test_topical_vine_upper_synthesis_starts_from_depth_1() {
        let bundled = include_str!("../../../chains/defaults/topical-vine.yaml");
        let def: ChainDefinition = serde_yaml::from_str(bundled).unwrap();
        let upper = def
            .steps
            .iter()
            .find(|s| s.recursive_pair)
            .expect("topical-vine must have a recursive_pair step (upper_synthesis)");
        assert_eq!(
            upper.depth,
            Some(1),
            "upper_synthesis must declare depth: 1 (source depth for pairing) — \
             cluster_synthesis writes L1 nodes, so recursive_pair starts at L1 \
             and pairs upward to the apex. depth: 2 would read an empty layer."
        );
    }

    /// Wanderer fix regression test: the `cluster_synthesis` step's input
    /// block must expose the current cluster to the synthesis prompt via
    /// `cluster: "$item"`. Previously the input only carried `children:
    /// "$collect_children.children"`, so the prompt received the full
    /// children array with no cluster context — the LLM could not know
    /// which subset of children to synthesize over.
    ///
    /// `for_each: "$cluster_children.clusters"` sets `ctx.current_item =
    /// cluster`, but `ctx.resolve_value(input)` only substitutes refs
    /// that appear in the input map. Without an explicit `$item`
    /// reference the cluster is invisible to the prompt.
    #[test]
    fn test_topical_vine_cluster_synthesis_passes_cluster_via_item_ref() {
        let bundled = include_str!("../../../chains/defaults/topical-vine.yaml");
        let def: ChainDefinition = serde_yaml::from_str(bundled).unwrap();
        let cs = def
            .steps
            .iter()
            .find(|s| s.name == "cluster_synthesis")
            .expect("topical-vine must have a cluster_synthesis step");
        assert_eq!(
            cs.for_each.as_deref(),
            Some("$cluster_children.clusters"),
            "cluster_synthesis must iterate over clusters from cluster_children"
        );

        let input = cs
            .input
            .as_ref()
            .expect("cluster_synthesis must declare an input block");
        let input_obj = input
            .as_object()
            .expect("cluster_synthesis input must be an object");

        let cluster_ref = input_obj
            .get("cluster")
            .and_then(|v| v.as_str())
            .expect("cluster_synthesis input must pass `cluster: $item` to the prompt");
        assert_eq!(
            cluster_ref, "$item",
            "cluster_synthesis must expose the current cluster to the prompt via $item"
        );

        let children_ref = input_obj
            .get("children")
            .and_then(|v| v.as_str())
            .expect("cluster_synthesis input must also pass the full children array");
        assert_eq!(
            children_ref, "$collect_children.children",
            "cluster_synthesis must pass the full children array so the prompt can look up cluster members"
        );
    }
}

#[cfg(test)]
mod phase1_load_chain_by_id_tests {
    //! Post-build accretion v5 Phase 1 verifier: `load_chain_by_id` must
    //! raise loudly when multiple discovered chains share the same id,
    //! rather than silently picking the first one. Ambiguity between
    //! `defaults/starter/` and `variants/` is easy to introduce and
    //! hard to debug if silent.
    use super::load_chain_by_id;
    use std::fs;
    use tempfile::TempDir;

    fn write_chain_yaml(path: &std::path::Path, id: &str, name: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Minimal-but-valid ChainDefinition matching chain_engine.rs schema.
        let yaml = format!(
            r#"schema_version: 1
id: {id}
name: {name}
description: test chain for ambiguity detection
content_type: code
version: "1.0"
author: phase1-verifier
defaults:
  model_tier: stale_local
  temperature: 0.3
  on_error: abort
steps:
  - name: noop
    primitive: mechanical
    mechanical: true
    rust_function: noop_echo
"#
        );
        fs::write(path, yaml).unwrap();
    }

    #[test]
    fn load_chain_by_id_raises_on_ambiguous_id() {
        let tmp = TempDir::new().unwrap();
        let chains_dir = tmp.path();
        write_chain_yaml(
            &chains_dir.join("defaults").join("starter").join("foo.yaml"),
            "duplicated-id",
            "starter",
        );
        write_chain_yaml(
            &chains_dir.join("variants").join("foo.yaml"),
            "duplicated-id",
            "variant",
        );
        let err = load_chain_by_id("duplicated-id", chains_dir).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("ambiguous"),
            "expected ambiguity message, got: {msg}"
        );
        assert!(
            msg.contains("duplicated-id"),
            "expected chain id in error, got: {msg}"
        );
    }

    #[test]
    fn load_chain_by_id_raises_on_missing_id() {
        let tmp = TempDir::new().unwrap();
        let chains_dir = tmp.path();
        // No chains — directory doesn't exist yet; discover_chains must
        // tolerate that. The error must still be "not found", not a silent
        // default.
        let err = load_chain_by_id("no-such-chain", chains_dir).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("not found"),
            "expected not-found message, got: {msg}"
        );
    }
}
