// pyramid/chain_resolve.rs — Reference and prompt template resolution for the chain runtime engine.
//
// Two resolvers:
//   - ChainContext + resolve_ref/resolve_value: resolves `$variable.path` references in YAML chain inputs
//   - resolve_prompt_template: resolves `{{variable}}` in prompt markdown files
//
// See docs/plans/action-chain-refactor-v3.md § Variable Resolution Spec.

use anyhow::{bail, Context, Result};
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use tracing::warn;

use super::chain_executor::ChunkProvider;

static REF_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\$([a-zA-Z_][a-zA-Z0-9_.]*(?:\[[^\]]*\])*)").unwrap()
});
static TEMPLATE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\{\{([^}]+)\}\}").unwrap()
});

// ── ChainContext ─────────────────────────────────────────────────────────

/// Runtime context for resolving `$variable.path` references during chain execution.
#[derive(Clone)]
pub struct ChainContext {
    /// Step outputs keyed by step name.
    /// Wrapped in Arc so that ChainContext::clone() is cheap (ref-count bump, not deep copy).
    /// Mutation sites use Arc::make_mut (cow-clone only when shared).
    pub step_outputs: Arc<HashMap<String, Value>>,
    /// Lazy chunk provider — loads content on-demand from SQLite.
    /// INVARIANT: Only stubs()/len() are called from sync ChainContext methods.
    /// Async methods (load_content, load_header) are called from the executor.
    pub chunks: ChunkProvider,
    /// Slug of the pyramid being built.
    pub slug: String,
    /// Content type (conversation, code, document).
    pub content_type: String,
    /// forEach loop: current item.
    pub current_item: Option<Value>,
    /// forEach loop: current index.
    pub current_index: Option<usize>,
    /// recursive_pair: left node.
    pub pair_left: Option<Value>,
    /// recursive_pair: right node.
    pub pair_right: Option<Value>,
    /// recursive_pair: current depth.
    pub pair_depth: Option<i64>,
    /// recursive_pair: pair index within current depth.
    pub pair_index: Option<usize>,
    /// recursive_pair: true if odd node being carried up.
    pub pair_is_carry: bool,
    /// Sequential accumulators keyed by name (e.g. "running_context").
    pub accumulators: HashMap<String, String>,
    /// Whether a prior build exists for this slug (any nodes present).
    pub has_prior_build: bool,
    /// Initial parameters passed at chain start (e.g., $apex_question, $granularity).
    pub initial_params: HashMap<String, Value>,
    /// Set to true by a `gate` primitive with `break: true` to exit the enclosing loop.
    pub break_loop: bool,
}

impl ChainContext {
    /// Create a new context with the basic build parameters.
    pub fn new(slug: &str, content_type: &str, chunks: ChunkProvider) -> Self {
        Self {
            step_outputs: Arc::new(HashMap::new()),
            chunks,
            slug: slug.to_string(),
            content_type: content_type.to_string(),
            current_item: None,
            current_index: None,
            pair_left: None,
            pair_right: None,
            pair_depth: None,
            pair_index: None,
            pair_is_carry: false,
            accumulators: HashMap::new(),
            has_prior_build: false,
            initial_params: HashMap::new(),
            break_loop: false,
        }
    }

    /// Resolve a single `$reference` string against the context.
    ///
    /// Returns an error for required refs that cannot be resolved.
    pub fn resolve_ref(&self, ref_str: &str) -> Result<Value> {
        let trimmed = ref_str.trim();
        if !trimmed.starts_with('$') {
            bail!("Not a reference (missing $ prefix): {}", ref_str);
        }
        let path = &trimmed[1..]; // strip leading $

        // ── Built-in scalars ────────────────────────────────────────
        if path == "chunks" {
            return Ok(Value::Array(self.chunks.stubs()));
        }
        if path == "chunks_reversed" {
            let mut reversed = self.chunks.stubs();
            reversed.reverse();
            return Ok(Value::Array(reversed));
        }
        if path == "slug" {
            return Ok(Value::String(self.slug.clone()));
        }
        if path == "content_type" {
            return Ok(Value::String(self.content_type.clone()));
        }
        if path == "has_prior_build" {
            return Ok(Value::Bool(self.has_prior_build));
        }

        // ── forEach loop vars ───────────────────────────────────────
        if path == "item" {
            return self
                .current_item
                .clone()
                .ok_or_else(|| anyhow::anyhow!("Unresolved reference: $item (no active forEach)"));
        }
        if path == "index" {
            return self
                .current_index
                .map(|i| Value::Number(i.into()))
                .ok_or_else(|| {
                    anyhow::anyhow!("Unresolved reference: $index (no active forEach)")
                });
        }

        // ── recursive_pair vars ─────────────────────────────────────
        if path == "pair.left" || path == "pair_left" {
            return self.pair_left.clone().ok_or_else(|| {
                anyhow::anyhow!("Unresolved reference: $pair.left (no active pair)")
            });
        }
        if path == "pair.right" || path == "pair_right" {
            // pair.right can legitimately be null (odd carry)
            return Ok(self.pair_right.clone().unwrap_or(Value::Null));
        }
        if path == "pair.depth" || path == "pair_depth" {
            return self
                .pair_depth
                .map(|d| Value::Number(d.into()))
                .ok_or_else(|| {
                    anyhow::anyhow!("Unresolved reference: $pair.depth (no active pair)")
                });
        }
        if path == "pair.index" || path == "pair_index" {
            return self
                .pair_index
                .map(|i| Value::Number(i.into()))
                .ok_or_else(|| {
                    anyhow::anyhow!("Unresolved reference: $pair.index (no active pair)")
                });
        }
        if path == "pair.is_carry" || path == "pair_is_carry" {
            return Ok(Value::Bool(self.pair_is_carry));
        }

        // ── Accumulator refs (e.g. $running_context) ────────────────
        if let Some(val) = self.accumulators.get(path) {
            return Ok(Value::String(val.clone()));
        }

        // ── Step output refs: $step_name.output.field, $step_name.nodes[i] ──
        self.resolve_step_ref(path)
            .with_context(|| format!("Unresolved reference: ${}", path))
    }

    /// Navigate into step outputs. `path` has the leading `$` already stripped.
    ///
    /// Supported patterns:
    /// - `step_name.output` → full step output
    /// - `step_name.output.field.nested` → dot-path navigation
    /// - `step_name.step_outputs[N]` → array index (N is literal int or `$index`)
    /// - `step_name.nodes` → step_outputs["step_name"]["nodes"]
    /// - `step_name.nodes[i]` → pair mode: current pair_index * 2
    /// - `step_name.nodes[i+1]` → pair mode: current pair_index * 2 + 1
    fn resolve_step_ref(&self, path: &str) -> Result<Value> {
        // Split on first dot to get step_name and the rest
        let (step_name, rest) = match path.find('.') {
            Some(pos) => (&path[..pos], Some(&path[pos + 1..])),
            None => {
                // Could be just a step name — check if it's a known step output
                if let Some(val) = self.step_outputs.get(path) {
                    return Ok(val.clone());
                }
                // Fallback: check initial_params
                if let Some(val) = self.initial_params.get(path) {
                    return Ok(val.clone());
                }
                bail!("Unknown reference: {}", path);
            }
        };

        // Try step_outputs first, then fall back to initial_params
        let source = self
            .step_outputs
            .get(step_name)
            .or_else(|| self.initial_params.get(step_name))
            .ok_or_else(|| anyhow::anyhow!("No output or initial param for \"{}\"", step_name))?;

        let rest = rest.unwrap(); // safe: we're in the Some branch

        // Parse the remaining path segments
        self.navigate_value(source, rest, step_name)
    }

    /// Navigate into a JSON value by a dot-separated path that may contain
    /// array indices like `[0]`, `[$index]`, `[i]`, `[i+1]`.
    fn navigate_value(&self, root: &Value, path: &str, step_name: &str) -> Result<Value> {
        let segments = parse_path_segments(path);
        let mut current = root.clone();

        for segment in &segments {
            match segment {
                PathSegment::Field(name) => {
                    current = current.get(name.as_str()).cloned().ok_or_else(|| {
                        anyhow::anyhow!(
                            "Field \"{}\" not found in step \"{}\" output",
                            name,
                            step_name
                        )
                    })?;
                }
                PathSegment::Index(idx) => {
                    let arr = current.as_array().ok_or_else(|| {
                        anyhow::anyhow!("Expected array for index access in step \"{}\"", step_name)
                    })?;
                    let resolved_idx = self.resolve_index(idx)?;
                    current = arr.get(resolved_idx).cloned().ok_or_else(|| {
                        anyhow::anyhow!(
                            "Index {} out of bounds (len {}) in step \"{}\"",
                            resolved_idx,
                            arr.len(),
                            step_name
                        )
                    })?;
                }
            }
        }

        Ok(current)
    }

    /// Resolve an index expression: literal number, `$index`, `i`, `i+1`.
    fn resolve_index(&self, expr: &IndexExpr) -> Result<usize> {
        match expr {
            IndexExpr::Literal(n) => Ok(*n),
            IndexExpr::CurrentIndex => self
                .current_index
                .ok_or_else(|| anyhow::anyhow!("$index used but no active forEach")),
            IndexExpr::PairI => {
                let pi = self
                    .pair_index
                    .ok_or_else(|| anyhow::anyhow!("[i] used but no active pair"))?;
                Ok(pi * 2)
            }
            IndexExpr::PairIPlusOne => {
                let pi = self
                    .pair_index
                    .ok_or_else(|| anyhow::anyhow!("[i+1] used but no active pair"))?;
                Ok(pi * 2 + 1)
            }
        }
    }

    /// Resolve all `$references` in a JSON value, recursively walking objects/arrays/strings.
    ///
    /// - A string that is entirely a `$ref` → returns the resolved Value (preserves type).
    /// - A string containing `$ref` embedded in other text → stringifies and interpolates.
    /// - Objects and arrays are walked recursively.
    /// - Other types (numbers, bools, null) pass through unchanged.
    pub fn resolve_value(&self, value: &Value) -> Result<Value> {
        match value {
            Value::String(s) => self.resolve_string_value(s),
            Value::Object(map) => {
                let mut resolved = serde_json::Map::new();
                for (k, v) in map {
                    resolved.insert(k.clone(), self.resolve_value(v)?);
                }
                Ok(Value::Object(resolved))
            }
            Value::Array(arr) => {
                let resolved: Result<Vec<Value>> =
                    arr.iter().map(|v| self.resolve_value(v)).collect();
                Ok(Value::Array(resolved?))
            }
            // Numbers, bools, null — pass through
            other => Ok(other.clone()),
        }
    }

    /// Resolve a string that may contain `$ref` patterns.
    fn resolve_string_value(&self, s: &str) -> Result<Value> {
        let trimmed = s.trim();

        // Case 1: Entire string is a single $reference → preserve resolved type
        if trimmed.starts_with('$') && is_single_ref(trimmed) {
            return self.resolve_ref(trimmed);
        }

        // Case 2: String contains embedded $refs → interpolate as strings
        if s.contains('$') {
            let result = interpolate_refs(s, self)?;
            return Ok(Value::String(result));
        }

        // Case 3: No refs — pass through
        Ok(Value::String(s.to_string()))
    }
}

// ── Path parsing helpers ─────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum PathSegment {
    Field(String),
    Index(IndexExpr),
}

#[derive(Debug, Clone)]
enum IndexExpr {
    Literal(usize),
    CurrentIndex, // $index
    PairI,        // i
    PairIPlusOne, // i+1
}

/// Parse a dot-separated path like `output.nodes[i+1].field` into segments.
fn parse_path_segments(path: &str) -> Vec<PathSegment> {
    let mut segments = Vec::new();
    let mut remaining = path;

    while !remaining.is_empty() {
        // Check for bracket index at current position
        if remaining.starts_with('[') {
            if let Some(close) = remaining.find(']') {
                let idx_str = &remaining[1..close];
                let expr = parse_index_expr(idx_str);
                segments.push(PathSegment::Index(expr));
                remaining = &remaining[close + 1..];
                // Skip trailing dot
                if remaining.starts_with('.') {
                    remaining = &remaining[1..];
                }
                continue;
            }
        }

        // Find next delimiter (dot or bracket)
        let next_dot = remaining.find('.');
        let next_bracket = remaining.find('[');

        let end = match (next_dot, next_bracket) {
            (Some(d), Some(b)) => d.min(b),
            (Some(d), None) => d,
            (None, Some(b)) => b,
            (None, None) => remaining.len(),
        };

        if end > 0 {
            segments.push(PathSegment::Field(remaining[..end].to_string()));
        }

        remaining = &remaining[end..];
        if remaining.starts_with('.') {
            remaining = &remaining[1..];
        }
    }

    segments
}

fn parse_index_expr(s: &str) -> IndexExpr {
    let trimmed = s.trim();
    if trimmed == "$index" {
        IndexExpr::CurrentIndex
    } else if trimmed == "i" {
        IndexExpr::PairI
    } else if trimmed == "i+1" {
        IndexExpr::PairIPlusOne
    } else if let Ok(n) = trimmed.parse::<usize>() {
        IndexExpr::Literal(n)
    } else {
        // Fallback — treat as literal 0 (this shouldn't happen with valid refs)
        warn!(
            expr = trimmed,
            "Unparseable index expression in chain reference, falling back to index 0 — \
             this likely indicates a bug in the YAML chain definition"
        );
        IndexExpr::Literal(0)
    }
}

/// Check if a string is a single `$ref` (no embedded text around it).
fn is_single_ref(s: &str) -> bool {
    let trimmed = s.trim();
    if !trimmed.starts_with('$') {
        return false;
    }
    // A single ref has no spaces (except within brackets) and no surrounding text
    // Simple heuristic: it's a single ref if after the $ there are only word chars, dots, brackets, +
    let after_dollar = &trimmed[1..];
    after_dollar
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '[' || c == ']' || c == '+')
}

/// Interpolate `$ref` patterns embedded in a larger string.
///
/// Finds each `$ref` token and replaces it with the stringified resolved value.
fn interpolate_refs(s: &str, ctx: &ChainContext) -> Result<String> {
    // Match $identifier patterns (including dots and brackets)
    let re = &*REF_PATTERN;
    let mut result = String::new();
    let mut last_end = 0;

    for cap in re.captures_iter(s) {
        let m = cap.get(0).unwrap();
        result.push_str(&s[last_end..m.start()]);

        let ref_str = m.as_str();
        match ctx.resolve_ref(ref_str) {
            Ok(val) => {
                result.push_str(&value_to_interpolation_string(&val));
            }
            Err(e) => {
                bail!(
                    "Failed to resolve embedded reference \"{}\" in string \"{}\": {}",
                    ref_str,
                    s,
                    e
                );
            }
        }
        last_end = m.end();
    }
    result.push_str(&s[last_end..]);
    Ok(result)
}

/// Convert a Value to a string suitable for interpolation.
fn value_to_interpolation_string(val: &Value) -> String {
    match val {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        // For arrays/objects, serialize to compact JSON
        other => serde_json::to_string(other).unwrap_or_else(|_| "null".to_string()),
    }
}

// ── Prompt template resolver ─────────────────────────────────────────────

/// Resolve `{{variable}}` and `{{variable.path.nested}}` references in a prompt template string.
///
/// Uses the step's resolved input map for variable values. Unresolved `{{ref}}` is an error.
pub fn resolve_prompt_template(template: &str, input: &Value) -> Result<String> {
    let re = &*TEMPLATE_PATTERN;
    let mut result = String::new();
    let mut last_end = 0;

    for cap in re.captures_iter(template) {
        let m = cap.get(0).unwrap();
        let var_path = cap.get(1).unwrap().as_str().trim();

        result.push_str(&template[last_end..m.start()]);

        let resolved = navigate_json_path(input, var_path)
            .with_context(|| format!("Unresolved prompt variable: {{{{{}}}}}", var_path))?;

        result.push_str(&value_to_interpolation_string(&resolved));
        last_end = m.end();
    }
    result.push_str(&template[last_end..]);
    Ok(result)
}

/// Navigate a dot-separated path into a JSON value: `field.nested.deep`.
fn navigate_json_path(root: &Value, path: &str) -> Result<Value> {
    let parts: Vec<&str> = path.split('.').collect();
    let mut current = root.clone();

    for part in &parts {
        current = current
            .get(*part)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Field \"{}\" not found in path \"{}\"", part, path))?;
    }

    Ok(current)
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_context() -> ChainContext {
        use super::super::chain_executor::ChunkProvider;
        let mut ctx = ChainContext::new(
            "my-slug",
            "conversation",
            ChunkProvider::with_count(3),
        );

        // Add a step output
        Arc::make_mut(&mut ctx.step_outputs).insert(
            "forward_pass".to_string(),
            json!({
                "output": {
                    "distilled": "distilled content",
                    "tags": ["a", "b"]
                },
                "nodes": [
                    {"id": "L0-000", "headline": "First"},
                    {"id": "L0-001", "headline": "Second"},
                    {"id": "L0-002", "headline": "Third"}
                ]
            }),
        );

        ctx.accumulators.insert(
            "running_context".to_string(),
            "accumulated so far".to_string(),
        );

        ctx
    }

    #[test]
    fn resolve_slug() {
        let ctx = test_context();
        let val = ctx.resolve_ref("$slug").unwrap();
        assert_eq!(val, json!("my-slug"));
    }

    #[test]
    fn resolve_content_type() {
        let ctx = test_context();
        let val = ctx.resolve_ref("$content_type").unwrap();
        assert_eq!(val, json!("conversation"));
    }

    #[test]
    fn resolve_chunks() {
        let ctx = test_context();
        let val = ctx.resolve_ref("$chunks").unwrap();
        assert_eq!(val.as_array().unwrap().len(), 3);
    }

    #[test]
    fn resolve_chunks_reversed() {
        let ctx = test_context();
        let val = ctx.resolve_ref("$chunks_reversed").unwrap();
        let arr = val.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        // ChunkProvider stubs are {"index": N}, reversed = [2, 1, 0]
        assert_eq!(arr[0], json!({"index": 2}));
        assert_eq!(arr[2], json!({"index": 0}));
    }

    #[test]
    fn resolve_has_prior_build() {
        let ctx = test_context();
        assert_eq!(ctx.resolve_ref("$has_prior_build").unwrap(), json!(false));
    }

    #[test]
    fn resolve_nested_step_output() {
        let ctx = test_context();
        let val = ctx.resolve_ref("$forward_pass.output.distilled").unwrap();
        assert_eq!(val, json!("distilled content"));
    }

    #[test]
    fn resolve_step_output_array() {
        let ctx = test_context();
        let val = ctx.resolve_ref("$forward_pass.nodes[0]").unwrap();
        assert_eq!(val, json!({"id": "L0-000", "headline": "First"}));
    }

    #[test]
    fn resolve_step_output_array_element_2() {
        let ctx = test_context();
        let val = ctx.resolve_ref("$forward_pass.nodes[2]").unwrap();
        assert_eq!(val["headline"], json!("Third"));
    }

    #[test]
    fn resolve_foreach_item_and_index() {
        let mut ctx = test_context();
        ctx.current_item = Some(json!({"text": "chunk1"}));
        ctx.current_index = Some(1);

        assert_eq!(ctx.resolve_ref("$item").unwrap(), json!({"text": "chunk1"}));
        assert_eq!(ctx.resolve_ref("$index").unwrap(), json!(1));
    }

    #[test]
    fn resolve_pair_left_right() {
        let mut ctx = test_context();
        ctx.pair_left = Some(json!({"headline": "Left node"}));
        ctx.pair_right = Some(json!({"headline": "Right node"}));
        ctx.pair_depth = Some(2);
        ctx.pair_index = Some(0);
        ctx.pair_is_carry = false;

        assert_eq!(
            ctx.resolve_ref("$pair.left").unwrap(),
            json!({"headline": "Left node"})
        );
        assert_eq!(
            ctx.resolve_ref("$pair.right").unwrap(),
            json!({"headline": "Right node"})
        );
        assert_eq!(ctx.resolve_ref("$pair.depth").unwrap(), json!(2));
        assert_eq!(ctx.resolve_ref("$pair.index").unwrap(), json!(0));
        assert_eq!(ctx.resolve_ref("$pair.is_carry").unwrap(), json!(false));
    }

    #[test]
    fn resolve_pair_right_null_when_carry() {
        let mut ctx = test_context();
        ctx.pair_left = Some(json!("only node"));
        // pair_right is None — odd carry
        ctx.pair_is_carry = true;

        assert_eq!(ctx.resolve_ref("$pair.right").unwrap(), json!(null));
        assert_eq!(ctx.resolve_ref("$pair.is_carry").unwrap(), json!(true));
    }

    #[test]
    fn resolve_accumulator() {
        let ctx = test_context();
        let val = ctx.resolve_ref("$running_context").unwrap();
        assert_eq!(val, json!("accumulated so far"));
    }

    #[test]
    fn resolve_pair_i_index() {
        let mut ctx = test_context();
        ctx.pair_index = Some(1); // pair_index 1 → nodes[2] and nodes[3]

        let val = ctx.resolve_ref("$forward_pass.nodes[i]").unwrap();
        assert_eq!(val, json!({"id": "L0-002", "headline": "Third"}));
    }

    #[test]
    fn resolve_pair_i_plus_1_index() {
        let mut ctx = test_context();
        ctx.pair_index = Some(0); // pair_index 0 → nodes[0] and nodes[1]

        let val = ctx.resolve_ref("$forward_pass.nodes[i+1]").unwrap();
        assert_eq!(val, json!({"id": "L0-001", "headline": "Second"}));
    }

    #[test]
    fn resolve_dollar_index_in_brackets() {
        let mut ctx = test_context();
        ctx.current_index = Some(2);

        let val = ctx.resolve_ref("$forward_pass.nodes[$index]").unwrap();
        assert_eq!(val["headline"], json!("Third"));
    }

    #[test]
    fn resolve_value_preserves_type_for_whole_ref() {
        let ctx = test_context();
        // A JSON string that is entirely a $ref should resolve to the ref's type
        let input = json!("$chunks");
        let resolved = ctx.resolve_value(&input).unwrap();
        assert!(resolved.is_array());
        assert_eq!(resolved.as_array().unwrap().len(), 3);
    }

    #[test]
    fn resolve_value_interpolates_embedded_ref() {
        let mut ctx = test_context();
        ctx.current_index = Some(3);

        let input = json!("Chunk $index of conversation");
        let resolved = ctx.resolve_value(&input).unwrap();
        assert_eq!(resolved, json!("Chunk 3 of conversation"));
    }

    #[test]
    fn resolve_value_walks_objects() {
        let ctx = test_context();
        let input = json!({
            "left": "$forward_pass.nodes[0]",
            "right": "$forward_pass.nodes[1]",
            "label": "test"
        });
        let resolved = ctx.resolve_value(&input).unwrap();
        assert_eq!(resolved["left"]["headline"], json!("First"));
        assert_eq!(resolved["right"]["headline"], json!("Second"));
        assert_eq!(resolved["label"], json!("test"));
    }

    #[test]
    fn resolve_value_walks_arrays() {
        let ctx = test_context();
        let input = json!(["$slug", "$content_type"]);
        let resolved = ctx.resolve_value(&input).unwrap();
        assert_eq!(resolved[0], json!("my-slug"));
        assert_eq!(resolved[1], json!("conversation"));
    }

    #[test]
    fn resolve_value_passthrough_non_refs() {
        let ctx = test_context();
        let input = json!(42);
        assert_eq!(ctx.resolve_value(&input).unwrap(), json!(42));

        let input = json!(true);
        assert_eq!(ctx.resolve_value(&input).unwrap(), json!(true));

        let input = json!(null);
        assert_eq!(ctx.resolve_value(&input).unwrap(), json!(null));
    }

    #[test]
    fn unresolved_ref_errors() {
        let ctx = test_context();
        let result = ctx.resolve_ref("$nonexistent_step.output");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Unresolved reference"), "got: {}", err_msg);
    }

    #[test]
    fn unresolved_item_without_foreach_errors() {
        let ctx = test_context();
        let result = ctx.resolve_ref("$item");
        assert!(result.is_err());
    }

    #[test]
    fn unresolved_pair_left_without_pair_errors() {
        let ctx = test_context();
        let result = ctx.resolve_ref("$pair.left");
        assert!(result.is_err());
    }

    // ── Prompt template tests ────────────────────────────────────

    #[test]
    fn prompt_simple_variable() {
        let input = json!({"left": "hello world", "right": "goodbye"});
        let result = resolve_prompt_template("A: {{left}}\nB: {{right}}", &input).unwrap();
        assert_eq!(result, "A: hello world\nB: goodbye");
    }

    #[test]
    fn prompt_nested_variable() {
        let input = json!({"data": {"summary": "important stuff"}});
        let result = resolve_prompt_template("Summary: {{data.summary}}", &input).unwrap();
        assert_eq!(result, "Summary: important stuff");
    }

    #[test]
    fn prompt_number_variable() {
        let input = json!({"count": 42});
        let result = resolve_prompt_template("Count: {{count}}", &input).unwrap();
        assert_eq!(result, "Count: 42");
    }

    #[test]
    fn prompt_unresolved_errors() {
        let input = json!({"left": "hello"});
        let result = resolve_prompt_template("{{missing_var}}", &input);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Unresolved prompt variable"),
            "got: {}",
            err_msg
        );
    }

    #[test]
    fn prompt_no_variables_passthrough() {
        let input = json!({});
        let result = resolve_prompt_template("No variables here.", &input).unwrap();
        assert_eq!(result, "No variables here.");
    }

    #[test]
    fn prompt_json_object_serialized() {
        let input = json!({"payload": {"a": 1, "b": 2}});
        let result = resolve_prompt_template("Data: {{payload}}", &input).unwrap();
        // Should serialize as compact JSON
        assert!(result.contains("\"a\":1") || result.contains("\"a\": 1"));
    }

    #[test]
    fn embedded_ref_with_slug() {
        let ctx = test_context();
        let input = json!("Building pyramid for $slug now");
        let resolved = ctx.resolve_value(&input).unwrap();
        assert_eq!(resolved, json!("Building pyramid for my-slug now"));
    }

    #[test]
    fn multiple_embedded_refs() {
        let mut ctx = test_context();
        ctx.current_index = Some(2);
        let input = json!("Slug=$slug index=$index");
        let resolved = ctx.resolve_value(&input).unwrap();
        assert_eq!(resolved, json!("Slug=my-slug index=2"));
    }
}
