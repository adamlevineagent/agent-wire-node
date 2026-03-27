// pyramid/question_loader.rs — Question YAML v3 loader + prompt resolver
//
// Loads question set definitions from YAML files in `chains/questions/`,
// resolves `$prompts/...` and `prompts/...` references to actual file contents,
// validates the result, and provides discovery for question set files.

use std::path::Path;

use anyhow::{Context, Result};

use super::question_yaml::{
    is_recognized_creates, is_recognized_scope, QuestionSet, QuestionSetMetadata,
};

/// Load a question set from a YAML file.
///
/// Reads the YAML, deserializes into `QuestionSet`, resolves prompt
/// references to file contents (both `$prompts/...` and bare `prompts/...`),
/// validates all fields, then returns the result.
pub fn load_question_set(yaml_path: &Path, chains_dir: &Path) -> Result<QuestionSet> {
    let raw = std::fs::read_to_string(yaml_path)
        .with_context(|| format!("failed to read question file: {}", yaml_path.display()))?;

    let mut qs: QuestionSet = serde_yaml::from_str(&raw)
        .with_context(|| format!("failed to parse question YAML: {}", yaml_path.display()))?;

    resolve_question_prompts(&mut qs, chains_dir)
        .with_context(|| format!("failed to resolve prompts in {}", yaml_path.display()))?;

    validate_question_set(&qs)
        .with_context(|| format!("question set validation failed: {}", yaml_path.display()))?;

    Ok(qs)
}

/// Resolve prompt file references in a question set.
///
/// Handles two reference styles (same as chain_loader.rs):
/// - `$prompts/code/extract.md` → `{chains_dir}/prompts/code/extract.md`
/// - `prompts/code/extract.md`  → `{chains_dir}/prompts/code/extract.md`
///
/// After resolution, the prompt field contains the file's contents.
fn resolve_question_prompts(qs: &mut QuestionSet, chains_dir: &Path) -> Result<()> {
    for (idx, question) in qs.questions.iter_mut().enumerate() {
        question.prompt = resolve_one_prompt(&question.prompt, chains_dir, idx, "prompt")?;

        if let Some(variants) = question.variants.as_mut() {
            for (label, prompt_path) in variants.iter_mut() {
                *prompt_path = resolve_one_prompt(prompt_path, chains_dir, idx, label)?;
            }
        }
    }
    Ok(())
}

/// Resolve a single prompt reference. Strips `$prompts/` or `prompts/` prefix,
/// joins with chains_dir, reads the file, and returns its contents.
/// If neither prefix matches, returns the string as-is (inline content).
fn resolve_one_prompt(
    prompt_ref: &str,
    chains_dir: &Path,
    question_idx: usize,
    field_name: &str,
) -> Result<String> {
    // Strip $prompts/ or prompts/ prefix to get relative path under chains_dir/prompts/
    let rel_path = if let Some(stripped) = prompt_ref.strip_prefix("$prompts/") {
        Some(stripped)
    } else if let Some(stripped) = prompt_ref.strip_prefix("prompts/") {
        Some(stripped)
    } else {
        None
    };

    if let Some(rel) = rel_path {
        let prompt_path = chains_dir.join("prompts").join(rel);
        std::fs::read_to_string(&prompt_path).with_context(|| {
            format!(
                "question [{}] {}: failed to read prompt file {} (resolved from '{}')",
                question_idx,
                field_name,
                prompt_path.display(),
                prompt_ref
            )
        })
    } else {
        Ok(prompt_ref.to_string())
    }
}

/// Validate a question set: version, type, scopes, creates types, context
/// references, constraints, and sequential_context mode.
pub fn validate_question_set(qs: &QuestionSet) -> Result<()> {
    // Version must be "3.0"
    if qs.version != "3.0" {
        anyhow::bail!(
            "unsupported question YAML version '{}', expected '3.0'",
            qs.version
        );
    }

    // Content type must be one of the known types
    let valid_types = ["code", "document", "conversation"];
    if !valid_types.contains(&qs.r#type.as_str()) {
        anyhow::bail!(
            "unknown question set type '{}', expected one of: {}",
            qs.r#type,
            valid_types.join(", ")
        );
    }

    // Must have at least one question
    if qs.questions.is_empty() {
        anyhow::bail!("question set must have at least one question");
    }

    for (idx, q) in qs.questions.iter().enumerate() {
        let label = format!("question [{}] (\"{}\")", idx, truncate_ask(&q.ask));

        // Validate about scope
        if !is_recognized_scope(&q.about) {
            anyhow::bail!("{}: unrecognized about scope \"{}\"", label, q.about);
        }

        // Validate creates type
        if !is_recognized_creates(&q.creates) {
            anyhow::bail!("{}: unrecognized creates value \"{}\"", label, q.creates);
        }

        // Prompt must be non-empty (file content after resolution)
        if q.prompt.trim().is_empty() {
            anyhow::bail!("{}: prompt is empty after resolution", label);
        }

        // Validate variant prompts are non-empty
        if let Some(variants) = &q.variants {
            for (variant_label, variant_content) in variants {
                if variant_content.trim().is_empty() {
                    anyhow::bail!(
                        "{}: variant '{}' has empty prompt content",
                        label,
                        variant_label
                    );
                }
            }
        }

        // Validate context references
        if let Some(ref ctx) = q.context {
            for entry in ctx {
                if !is_recognized_context_ref(entry) {
                    anyhow::bail!("{}: unrecognized context reference \"{}\"", label, entry);
                }
            }
        }

        // Validate sequential_context mode
        if let Some(seq) = &q.sequential_context {
            if seq.mode != "accumulate" {
                anyhow::bail!(
                    "{}: unsupported sequential_context mode '{}', expected 'accumulate'",
                    label,
                    seq.mode
                );
            }
        }

        // Validate constraints
        if let Some(constraints) = &q.constraints {
            if let Some(min) = constraints.min_groups {
                if min == 0 {
                    anyhow::bail!("{}: constraints.min_groups must be > 0", label);
                }
            }
            if let Some(max) = constraints.max_groups {
                if max == 0 {
                    anyhow::bail!("{}: constraints.max_groups must be > 0", label);
                }
            }
            if let Some(max_items) = constraints.max_items_per_group {
                if max_items == 0 {
                    anyhow::bail!("{}: constraints.max_items_per_group must be > 0", label);
                }
            }
            if let (Some(min), Some(max)) = (constraints.min_groups, constraints.max_groups) {
                if min > max {
                    anyhow::bail!(
                        "{}: constraints.min_groups ({}) > constraints.max_groups ({})",
                        label,
                        min,
                        max
                    );
                }
            }
        }
    }

    Ok(())
}

/// Check whether a context reference string is recognized.
fn is_recognized_context_ref(reference: &str) -> bool {
    matches!(
        reference,
        "L0 classification tags"
            | "L0 web edges"
            | "L1 web edges"
            | "L2 web edges"
            | "sibling headlines"
    )
}

/// Discover all question set files in `chains/questions/`.
/// Returns metadata for each valid question set.
pub fn discover_question_sets(chains_dir: &Path) -> Result<Vec<QuestionSetMetadata>> {
    let questions_dir = chains_dir.join("questions");
    if !questions_dir.exists() {
        return Ok(Vec::new());
    }

    let mut results = Vec::new();
    let entries = std::fs::read_dir(&questions_dir).with_context(|| {
        format!(
            "failed to read questions directory: {}",
            questions_dir.display()
        )
    })?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();

        if path
            .extension()
            .map(|e| e == "yaml" || e == "yml")
            .unwrap_or(false)
        {
            match load_question_set_metadata(&path) {
                Ok(meta) => results.push(meta),
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        "skipping invalid question set file: {:#}",
                        e
                    );
                }
            }
        }
    }

    Ok(results)
}

/// Load just the metadata from a question YAML (does not resolve prompts).
fn load_question_set_metadata(yaml_path: &Path) -> Result<QuestionSetMetadata> {
    let raw = std::fs::read_to_string(yaml_path)
        .with_context(|| format!("failed to read question file: {}", yaml_path.display()))?;

    let qs: QuestionSet = serde_yaml::from_str(&raw)
        .with_context(|| format!("failed to parse question YAML: {}", yaml_path.display()))?;

    Ok(QuestionSetMetadata {
        content_type: qs.r#type,
        version: qs.version,
        question_count: qs.questions.len(),
        file_path: yaml_path.to_string_lossy().into_owned(),
    })
}

/// Truncate ask text for error messages (max 50 chars).
fn truncate_ask(ask: &str) -> String {
    if ask.chars().count() <= 50 {
        ask.to_string()
    } else {
        let truncated: String = ask.chars().take(47).collect();
        format!("{}...", truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Helper: create a unique temporary chains directory with prompt files and question YAMLs.
    fn setup_test_chains_dir() -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "question_loader_test_{}_{}",
            std::process::id(),
            id
        ));
        // Clean up any prior run
        let _ = fs::remove_dir_all(&dir);

        // Create prompt directories and files for code
        let code_prompts = dir.join("prompts").join("code");
        fs::create_dir_all(&code_prompts).unwrap();
        fs::write(code_prompts.join("extract.md"), "Extract prompt content").unwrap();
        fs::write(
            code_prompts.join("config_extract.md"),
            "Config extract content",
        )
        .unwrap();
        fs::write(
            code_prompts.join("frontend_extract.md"),
            "Frontend extract content",
        )
        .unwrap();
        fs::write(code_prompts.join("cluster.md"), "Cluster prompt content").unwrap();
        fs::write(code_prompts.join("web.md"), "Web prompt content").unwrap();
        fs::write(code_prompts.join("thread.md"), "Thread prompt content").unwrap();
        fs::write(
            code_prompts.join("recluster.md"),
            "Recluster prompt content",
        )
        .unwrap();
        fs::write(code_prompts.join("distill.md"), "Distill prompt content").unwrap();

        // Create prompt directories and files for doc
        let doc_prompts = dir.join("prompts").join("doc");
        fs::create_dir_all(&doc_prompts).unwrap();
        fs::write(doc_prompts.join("classify.md"), "Classify prompt content").unwrap();
        fs::write(doc_prompts.join("extract.md"), "Extract prompt content").unwrap();
        fs::write(doc_prompts.join("cluster.md"), "Cluster prompt content").unwrap();
        fs::write(doc_prompts.join("web.md"), "Web prompt content").unwrap();
        fs::write(doc_prompts.join("thread.md"), "Thread prompt content").unwrap();
        fs::write(doc_prompts.join("recluster.md"), "Recluster prompt content").unwrap();
        fs::write(doc_prompts.join("distill.md"), "Distill prompt content").unwrap();

        // Create prompt directories and files for conversation
        let conv_prompts = dir.join("prompts").join("conversation");
        fs::create_dir_all(&conv_prompts).unwrap();
        fs::write(conv_prompts.join("extract.md"), "Extract prompt content").unwrap();
        fs::write(conv_prompts.join("cluster.md"), "Cluster prompt content").unwrap();
        fs::write(conv_prompts.join("thread.md"), "Thread prompt content").unwrap();
        fs::write(conv_prompts.join("web.md"), "Web prompt content").unwrap();
        fs::write(
            conv_prompts.join("recluster.md"),
            "Recluster prompt content",
        )
        .unwrap();
        fs::write(conv_prompts.join("distill.md"), "Distill prompt content").unwrap();

        // Create questions directory with YAML files
        let questions_dir = dir.join("questions");
        fs::create_dir_all(&questions_dir).unwrap();
        fs::write(questions_dir.join("code.yaml"), CODE_YAML).unwrap();
        fs::write(questions_dir.join("document.yaml"), DOCUMENT_YAML).unwrap();
        fs::write(questions_dir.join("conversation.yaml"), CONVERSATION_YAML).unwrap();

        dir
    }

    fn cleanup_test_dir(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    const CODE_YAML: &str = r#"type: code
version: "3.0"
defaults:
  model: inception/mercury-2
  temperature: 0.3
  retry: 2
questions:
  - ask: "What does this file do?"
    about: each file individually
    creates: L0 nodes
    prompt: prompts/code/extract.md
    parallel: 8
    retry: 3
    variants:
      config files: prompts/code/config_extract.md
      frontend (.tsx, .jsx): prompts/code/frontend_extract.md
  - ask: "What are the subsystems?"
    about: all L0 topics at once
    creates: L1 topic assignments
    prompt: prompts/code/cluster.md
    model: qwen/qwen3.5-flash-02-23
    constraints:
      min_groups: 8
      max_groups: 18
      max_items_per_group: 12
    retry: 3
  - ask: "What do files share?"
    about: all L0 nodes at once
    creates: web edges between L0 nodes
    prompt: prompts/code/web.md
    model: qwen/qwen3.5-flash-02-23
    optional: true
  - ask: "Synthesize this subsystem"
    about: each L1 topic's assigned L0 nodes
    creates: L1 nodes
    context:
      - L0 web edges
    prompt: prompts/code/thread.md
    parallel: 5
    retry: 2
  - ask: "What do subsystems share?"
    about: all L1 nodes at once
    creates: web edges between L1 nodes
    prompt: prompts/code/web.md
  - ask: "What are the architectural domains?"
    about: all L1 nodes at once
    creates: L2 nodes
    context:
      - L1 web edges
      - sibling headlines
    prompt: prompts/code/recluster.md
    model: qwen/qwen3.5-flash-02-23
    retry: 3
  - ask: "What connects domains?"
    about: all L2 nodes at once
    creates: web edges between L2 nodes
    prompt: prompts/code/web.md
    optional: true
  - ask: "What is this system?"
    about: all top-level nodes at once
    creates: apex
    context:
      - L2 web edges
    prompt: prompts/code/distill.md
"#;

    const DOCUMENT_YAML: &str = r#"type: document
version: "3.0"
defaults:
  model: inception/mercury-2
  temperature: 0.3
  retry: 2
questions:
  - ask: "What type of document is this?"
    about: the first 20 lines of each file
    creates: L0 classification tags
    prompt: prompts/doc/classify.md
    parallel: 8
  - ask: "What are the key claims?"
    about: each file individually
    creates: L0 nodes
    context:
      - L0 classification tags
    prompt: prompts/doc/extract.md
    parallel: 8
    retry: 3
  - ask: "What are the topics?"
    about: all L0 topics at once
    creates: L1 topic assignments
    context:
      - L0 classification tags
    prompt: prompts/doc/cluster.md
    model: qwen/qwen3.5-flash-02-23
    constraints:
      min_groups: 6
      max_groups: 15
      max_items_per_group: 15
    retry: 3
  - ask: "What do documents share?"
    about: all L0 nodes at once
    creates: web edges between L0 nodes
    context:
      - L0 classification tags
    prompt: prompts/doc/web.md
    model: qwen/qwen3.5-flash-02-23
    optional: true
  - ask: "What is the current state of this topic?"
    about: each L1 topic's assigned L0 nodes
    creates: L1 nodes
    context:
      - L0 web edges
      - L0 classification tags
    prompt: prompts/doc/thread.md
    parallel: 5
    retry: 2
  - ask: "What connects these topics?"
    about: all L1 nodes at once
    creates: web edges between L1 nodes
    prompt: prompts/doc/web.md
  - ask: "What are the major domains?"
    about: all L1 nodes at once
    creates: L2 nodes
    context:
      - L1 web edges
      - sibling headlines
    prompt: prompts/doc/recluster.md
    model: qwen/qwen3.5-flash-02-23
    retry: 3
  - ask: "What are cross-domain dependencies?"
    about: all L2 nodes at once
    creates: web edges between L2 nodes
    prompt: prompts/doc/web.md
    optional: true
  - ask: "What is this corpus about?"
    about: all top-level nodes at once
    creates: apex
    context:
      - L2 web edges
    prompt: prompts/doc/distill.md
"#;

    const CONVERSATION_YAML: &str = r#"type: conversation
version: "3.0"
defaults:
  model: inception/mercury-2
  temperature: 0.3
  retry: 2
questions:
  - ask: "What topics were discussed in this chunk?"
    about: each chunk individually
    creates: L0 nodes
    prompt: prompts/conversation/extract.md
    parallel: 8
    retry: 3
    sequential_context:
      mode: accumulate
      max_chars: 8000
      carry: summary of prior chunks so far
  - ask: "What are the distinct topics?"
    about: all L0 topics at once
    creates: L1 thread assignments
    prompt: prompts/conversation/cluster.md
    model: qwen/qwen3.5-flash-02-23
    constraints:
      min_groups: 4
      max_groups: 12
    retry: 3
  - ask: "What is the full arc of this topic?"
    about: each L1 thread's assigned L0 nodes, ordered chronologically
    creates: L1 nodes
    prompt: prompts/conversation/thread.md
    parallel: 5
    retry: 2
  - ask: "Which topics influenced each other?"
    about: all L1 nodes at once
    creates: web edges between L1 nodes
    prompt: prompts/conversation/web.md
  - ask: "What are the major themes?"
    about: all L1 nodes at once
    creates: L2 nodes
    context:
      - L1 web edges
      - sibling headlines
    prompt: prompts/conversation/recluster.md
    model: qwen/qwen3.5-flash-02-23
    retry: 3
  - ask: "What was this conversation about?"
    about: all top-level nodes at once
    creates: apex
    context:
      - L1 web edges
    prompt: prompts/conversation/distill.md
"#;

    // ── Parse tests ────────────────────────────────────────────────────────

    #[test]
    fn parse_code_yaml_successfully() {
        let dir = setup_test_chains_dir();
        let yaml_path = dir.join("questions").join("code.yaml");
        let qs = load_question_set(&yaml_path, &dir).unwrap();
        assert_eq!(qs.r#type, "code");
        assert_eq!(qs.version, "3.0");
        assert_eq!(qs.questions.len(), 8);
        // Verify prompt was resolved to file content
        assert_eq!(qs.questions[0].prompt, "Extract prompt content");
        cleanup_test_dir(&dir);
    }

    #[test]
    fn parse_document_yaml_successfully() {
        let dir = setup_test_chains_dir();
        let yaml_path = dir.join("questions").join("document.yaml");
        let qs = load_question_set(&yaml_path, &dir).unwrap();
        assert_eq!(qs.r#type, "document");
        assert_eq!(qs.version, "3.0");
        assert_eq!(qs.questions.len(), 9);
        cleanup_test_dir(&dir);
    }

    #[test]
    fn parse_conversation_yaml_successfully() {
        let dir = setup_test_chains_dir();
        let yaml_path = dir.join("questions").join("conversation.yaml");
        let qs = load_question_set(&yaml_path, &dir).unwrap();
        assert_eq!(qs.r#type, "conversation");
        assert_eq!(qs.version, "3.0");
        assert_eq!(qs.questions.len(), 6);
        // Verify sequential_context parsed correctly
        let seq = qs.questions[0].sequential_context.as_ref().unwrap();
        assert_eq!(seq.mode, "accumulate");
        assert_eq!(seq.max_chars, Some(8000));
        cleanup_test_dir(&dir);
    }

    // ── Scope validation tests ─────────────────────────────────────────────

    #[test]
    fn validate_all_scopes_in_code_yaml() {
        let dir = setup_test_chains_dir();
        let yaml_path = dir.join("questions").join("code.yaml");
        let qs = load_question_set(&yaml_path, &dir).unwrap();
        for q in &qs.questions {
            assert!(
                is_recognized_scope(&q.about),
                "unrecognized scope in code.yaml: '{}'",
                q.about
            );
        }
        cleanup_test_dir(&dir);
    }

    #[test]
    fn validate_all_creates_in_code_yaml() {
        let dir = setup_test_chains_dir();
        let yaml_path = dir.join("questions").join("code.yaml");
        let qs = load_question_set(&yaml_path, &dir).unwrap();
        for q in &qs.questions {
            assert!(
                is_recognized_creates(&q.creates),
                "unrecognized creates in code.yaml: '{}'",
                q.creates
            );
        }
        cleanup_test_dir(&dir);
    }

    // ── Rejection tests ────────────────────────────────────────────────────

    #[test]
    fn reject_unknown_scope() {
        let dir = setup_test_chains_dir();
        let yaml = r#"
type: code
version: "3.0"
defaults:
  model: test
questions:
  - ask: "Bad question"
    about: each banana individually
    creates: L0 nodes
    prompt: prompts/code/extract.md
"#;
        let yaml_path = dir.join("questions").join("bad_scope.yaml");
        fs::write(&yaml_path, yaml).unwrap();
        let err = load_question_set(&yaml_path, &dir).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("unrecognized about scope"),
            "expected scope error, got: {}",
            msg
        );
        assert!(msg.contains("each banana individually"), "got: {}", msg);
        cleanup_test_dir(&dir);
    }

    #[test]
    fn reject_unknown_creates_type() {
        let dir = setup_test_chains_dir();
        let yaml = r#"
type: code
version: "3.0"
defaults:
  model: test
questions:
  - ask: "Bad question"
    about: each file individually
    creates: L3 nodes
    prompt: prompts/code/extract.md
"#;
        let yaml_path = dir.join("questions").join("bad_creates.yaml");
        fs::write(&yaml_path, yaml).unwrap();
        let err = load_question_set(&yaml_path, &dir).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("unrecognized creates value"),
            "expected creates error, got: {}",
            msg
        );
        assert!(msg.contains("L3 nodes"), "got: {}", msg);
        cleanup_test_dir(&dir);
    }

    #[test]
    fn reject_missing_prompt_file() {
        let dir = setup_test_chains_dir();
        let yaml = r#"
type: code
version: "3.0"
defaults:
  model: test
questions:
  - ask: "Bad question"
    about: each file individually
    creates: L0 nodes
    prompt: prompts/code/nonexistent.md
"#;
        let yaml_path = dir.join("questions").join("bad_prompt.yaml");
        fs::write(&yaml_path, yaml).unwrap();
        let err = load_question_set(&yaml_path, &dir).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("failed to read prompt file"),
            "expected missing prompt error, got: {}",
            msg
        );
        cleanup_test_dir(&dir);
    }

    // ── Prompt resolution tests ────────────────────────────────────────────

    #[test]
    fn prompt_resolution_with_dollar_prefix() {
        let dir = setup_test_chains_dir();
        let yaml = r#"
type: code
version: "3.0"
defaults:
  model: test
questions:
  - ask: "Test"
    about: each file individually
    creates: L0 nodes
    prompt: $prompts/code/extract.md
"#;
        let yaml_path = dir.join("questions").join("dollar_prompt.yaml");
        fs::write(&yaml_path, yaml).unwrap();
        let qs = load_question_set(&yaml_path, &dir).unwrap();
        // $prompts/ prefix should be resolved to file content
        assert_eq!(qs.questions[0].prompt, "Extract prompt content");
        cleanup_test_dir(&dir);
    }

    #[test]
    fn prompt_resolution_with_bare_prefix() {
        let dir = setup_test_chains_dir();
        let yaml = r#"
type: code
version: "3.0"
defaults:
  model: test
questions:
  - ask: "Test"
    about: each file individually
    creates: L0 nodes
    prompt: prompts/code/extract.md
"#;
        let yaml_path = dir.join("questions").join("bare_prompt.yaml");
        fs::write(&yaml_path, yaml).unwrap();
        let qs = load_question_set(&yaml_path, &dir).unwrap();
        assert_eq!(qs.questions[0].prompt, "Extract prompt content");
        cleanup_test_dir(&dir);
    }

    // ── Discovery tests ────────────────────────────────────────────────────

    #[test]
    fn discovery_finds_all_three_question_sets() {
        let dir = setup_test_chains_dir();
        let metas = discover_question_sets(&dir).unwrap();
        assert_eq!(metas.len(), 3, "should find code, document, conversation");

        let mut types: Vec<&str> = metas.iter().map(|m| m.content_type.as_str()).collect();
        types.sort();
        assert_eq!(types, vec!["code", "conversation", "document"]);
        cleanup_test_dir(&dir);
    }

    #[test]
    fn discovery_returns_empty_for_missing_dir() {
        let dir = std::env::temp_dir().join("question_loader_test_empty_discovery");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let metas = discover_question_sets(&dir).unwrap();
        assert!(metas.is_empty());
        cleanup_test_dir(&dir);
    }

    // ── Version validation ─────────────────────────────────────────────────

    #[test]
    fn reject_wrong_version() {
        let dir = setup_test_chains_dir();
        let yaml = r#"
type: code
version: "2.0"
defaults:
  model: test
questions:
  - ask: "Test"
    about: each file individually
    creates: L0 nodes
    prompt: prompts/code/extract.md
"#;
        let yaml_path = dir.join("questions").join("bad_version.yaml");
        fs::write(&yaml_path, yaml).unwrap();
        let err = load_question_set(&yaml_path, &dir).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("version"),
            "expected version error, got: {}",
            msg
        );
        cleanup_test_dir(&dir);
    }

    // ── Constraint validation ──────────────────────────────────────────────

    #[test]
    fn reject_min_greater_than_max_constraints() {
        let dir = setup_test_chains_dir();
        let yaml = r#"
type: code
version: "3.0"
defaults:
  model: test
questions:
  - ask: "Test"
    about: all L0 topics at once
    creates: L1 topic assignments
    prompt: prompts/code/cluster.md
    constraints:
      min_groups: 20
      max_groups: 5
"#;
        let yaml_path = dir.join("questions").join("bad_constraints.yaml");
        fs::write(&yaml_path, yaml).unwrap();
        let err = load_question_set(&yaml_path, &dir).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("min_groups"),
            "expected constraint error, got: {}",
            msg
        );
        cleanup_test_dir(&dir);
    }

    #[test]
    fn reject_zero_max_items_per_group() {
        let dir = setup_test_chains_dir();
        let yaml = r#"
type: code
version: "3.0"
defaults:
  model: test
questions:
  - ask: "Test"
    about: all L0 topics at once
    creates: L1 topic assignments
    prompt: prompts/code/cluster.md
    constraints:
      max_items_per_group: 0
"#;
        let yaml_path = dir.join("questions").join("bad_max_items.yaml");
        fs::write(&yaml_path, yaml).unwrap();
        let err = load_question_set(&yaml_path, &dir).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("max_items_per_group must be > 0"),
            "expected max_items_per_group error, got: {}",
            msg
        );
        cleanup_test_dir(&dir);
    }

    #[test]
    fn reject_unknown_content_type() {
        let dir = setup_test_chains_dir();
        let yaml = r#"
type: podcast
version: "3.0"
defaults:
  model: test
questions:
  - ask: "Test"
    about: each file individually
    creates: L0 nodes
    prompt: prompts/code/extract.md
"#;
        let yaml_path = dir.join("questions").join("bad_type.yaml");
        fs::write(&yaml_path, yaml).unwrap();
        let err = load_question_set(&yaml_path, &dir).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("unknown question set type"),
            "expected type error, got: {}",
            msg
        );
        cleanup_test_dir(&dir);
    }

    #[test]
    fn reject_empty_questions_list() {
        let dir = setup_test_chains_dir();
        let yaml = r#"
type: code
version: "3.0"
defaults:
  model: test
questions: []
"#;
        let yaml_path = dir.join("questions").join("empty_questions.yaml");
        fs::write(&yaml_path, yaml).unwrap();
        let err = load_question_set(&yaml_path, &dir).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("at least one question"),
            "expected empty questions error, got: {}",
            msg
        );
        cleanup_test_dir(&dir);
    }

    #[test]
    fn reject_unrecognized_context_ref() {
        let dir = setup_test_chains_dir();
        let yaml = r#"
type: code
version: "3.0"
defaults:
  model: test
questions:
  - ask: "Test"
    about: each file individually
    creates: L0 nodes
    prompt: prompts/code/extract.md
    context:
      - imaginary context
"#;
        let yaml_path = dir.join("questions").join("bad_context.yaml");
        fs::write(&yaml_path, yaml).unwrap();
        let err = load_question_set(&yaml_path, &dir).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("unrecognized context reference"),
            "expected context error, got: {}",
            msg
        );
        cleanup_test_dir(&dir);
    }

    #[test]
    fn reject_bad_sequential_context_mode() {
        let dir = setup_test_chains_dir();
        let yaml = r#"
type: code
version: "3.0"
defaults:
  model: test
questions:
  - ask: "Test"
    about: each file individually
    creates: L0 nodes
    prompt: prompts/code/extract.md
    sequential_context:
      mode: windowed
"#;
        let yaml_path = dir.join("questions").join("bad_seq_mode.yaml");
        fs::write(&yaml_path, yaml).unwrap();
        let err = load_question_set(&yaml_path, &dir).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("unsupported sequential_context mode"),
            "expected sequential_context mode error, got: {}",
            msg
        );
        cleanup_test_dir(&dir);
    }

    #[test]
    fn truncate_ask_handles_multibyte_utf8() {
        // 50 chars of emoji (each 4 bytes) would panic with byte slicing
        let ask = "A".repeat(48) + "\u{1F600}\u{1F600}\u{1F600}"; // 48 ASCII + 3 emoji = 51 chars
        let result = truncate_ask(&ask);
        assert!(result.ends_with("..."));
        assert!(result.chars().count() <= 50); // 47 chars + "..."
    }

    fn checked_in_chains_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../chains")
    }

    #[test]
    fn checked_in_code_question_set_resolves_prompts() {
        let chains_dir = checked_in_chains_dir();
        let yaml_path = chains_dir.join("questions").join("code.yaml");
        let qs = load_question_set(&yaml_path, &chains_dir).unwrap();

        assert_eq!(qs.r#type, "code");
        assert!(!qs.questions.is_empty());
        assert!(qs.questions.iter().all(|q| !q.prompt.trim().is_empty()));
    }

    #[test]
    fn checked_in_document_question_set_resolves_prompts() {
        let chains_dir = checked_in_chains_dir();
        let yaml_path = chains_dir.join("questions").join("document.yaml");
        let qs = load_question_set(&yaml_path, &chains_dir).unwrap();

        assert_eq!(qs.r#type, "document");
        assert!(!qs.questions.is_empty());
        assert!(qs.questions.iter().all(|q| !q.prompt.trim().is_empty()));
    }
}
