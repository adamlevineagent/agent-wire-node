# Handoff: Zero-Candidate Gap Fix

## Objective
Prevent the `answer_single_question` primitive from making expensive, speculative LLM calls when a question receives zero candidate evidence from the pre-mapping phase. 

Currently, `answer_single_question` sends a prompt to the LLM indicating there is no evidence (e.g. `evidence_context = "(no candidate evidence nodes were mapped to this question)"`). The LLM attempts to synthesize this, often resulting in hallucinatory "Empty nodes" while failing to properly emit a `MISSING` verdict to alert the system. 

By short-circuiting this scenario, we gracefully bypass the LLM, inject an empty placeholder node, and generate a synthetic `MISSING` verdict so `gap_processing` can pick it up for targeted extraction.

## Files to Modify
- `src-tauri/src/pyramid/evidence_answering.rs`

## Implementation Specs

1. Open `src-tauri/src/pyramid/evidence_answering.rs`
2. Locate the `answer_single_question` function.
3. Immediately after `let node_id = format!("L{}-{}", question.layer, Uuid::new_v4());`, insert a short-circuit if `candidate_nodes` is empty:

```rust
    if candidate_nodes.is_empty() {
        return Ok(AnsweredNode {
            node: PyramidNode {
                id: node_id.clone(),
                slug: answer_slug.to_string(),
                depth: question.layer,
                chunk_index: None,
                headline: question.question_text.clone(),
                distilled: "Empty branch: awaiting evidence.".to_string(),
                topics: vec![],
                corrections: vec![],
                decisions: vec![],
                terms: vec![],
                dead_ends: vec![],
                self_prompt: String::new(),
                children: vec![],
                parent_id: None,
                superseded_by: None,
                build_id: None, // stamped in execute_evidence_loop
                created_at: chrono::Utc::now().to_rfc3339(),
            },
            evidence: vec![],
            missing: vec![format!("No candidate evidence was mapped during pre-mapping for question: {}", question.question_text)],
        });
    }
```

4. Simplify the `evidence_context` variable definition directly below the new short-circuit. Since `candidate_nodes` is now guaranteed to be non-empty, remove the `if candidate_nodes.is_empty()` check.

*Before:*
```rust
    // ── Build candidate evidence context ────────────────────────────────
    let evidence_context = if candidate_nodes.is_empty() {
        "(no candidate evidence nodes were mapped to this question)".to_string()
    } else {
        candidate_nodes
            .iter()
            // ...
```

*After:*
```rust
    // ── Build candidate evidence context ────────────────────────────────
    let evidence_context = candidate_nodes
        .iter()
        .map(|n| {
            format!(
                "--- NODE {} ---\nHeadline: {}\nDistilled: {}\nTopics: {}\n",
                n.id,
                n.headline,
                n.distilled,
                n.topics
                    .iter()
                    .map(|t| format!("{}: {}", t.name, t.current))
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
```

## Verification
- Run `cargo check` inside `src-tauri` to ensure types map correctly.
- Ensure `chrono::Utc::now().to_rfc3339()` resolves correctly (if `chrono` isn't imported, use the absolute path or `use chrono::Utc;`).
- When executed in a live build cluster, any pre-mapped questions returning zero candidates will skip the LLM call entirely. The resulting node will be preserved for branch structure, bypassing tree collapse while seeding `gap_processing` with a robust `MISSING` entity.
