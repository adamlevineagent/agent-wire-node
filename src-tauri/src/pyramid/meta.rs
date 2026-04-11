// pyramid/meta.rs — Meta analysis layers on top of the Knowledge Pyramid
//
// Generates timeline, narrative, and quickstart views from L2 thread data.
// These are stored as META- nodes with depth = -1.

use rusqlite::Connection;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::info;

use crate::pyramid::llm::LlmConfig;
use crate::pyramid::step_context::make_step_ctx_from_llm_config;
use crate::pyramid::{db, llm, types::*};

// ── Meta analysis passes ─────────────────────────────────────────────────────

/// Generate timeline forward — chronological sequence of events from L2 threads.
pub async fn timeline_forward(
    reader: &Arc<Mutex<Connection>>,
    writer: &Arc<Mutex<Connection>>,
    slug: &str,
    base_config: &LlmConfig,
    model: &str,
) -> anyhow::Result<String> {
    let threads = {
        let conn = reader.lock().await;
        db::get_threads(&conn, slug)?
    };

    if threads.is_empty() {
        return Ok("No threads to analyze.".to_string());
    }

    // Read all thread canonicals
    let mut thread_summaries = Vec::new();
    {
        let conn = reader.lock().await;
        for thread in &threads {
            if let Ok(Some(node)) = db::get_node(&conn, slug, &thread.current_canonical_id) {
                thread_summaries.push(format!(
                    "Thread '{}' ({}): {}",
                    thread.thread_name,
                    thread.thread_id,
                    crate::utils::safe_slice_end(&node.distilled, 500),
                ));
            }
        }
    }

    let prompt = format!(
        r#"You are reading the thematic summaries of a knowledge base, roughly in the order they emerged.
Write the timeline — what happened first, what caused each transition, what breakthroughs occurred, where we ended up.
This is the STORY of the content, not the knowledge itself.

THREADS:
{}

Output the timeline as plain text (not JSON). Be concise but capture the arc."#,
        thread_summaries.join("\n\n")
    );

    // Phase 3 fix pass: clone the live config (preserves provider_registry +
    // credential_store) instead of building a fresh `config_for_model`.
    let cfg = base_config.clone_with_model_override(model);
    let system_prompt =
        "You are a meta-analysis engine for a knowledge pyramid. Produce clear, concise analysis.";
    let cache_ctx = make_step_ctx_from_llm_config(
        &cfg,
        "meta_timeline_forward",
        "meta",
        -1,
        None,
        system_prompt,
    );
    let result = llm::call_model_and_ctx(
        &cfg,
        cache_ctx.as_ref(),
        system_prompt,
        &prompt,
        0.3,
        2000,
    )
    .await?;

    // Save as META node
    save_meta_node(
        writer,
        slug,
        "META-timeline-forward",
        "Timeline (Forward)",
        &result,
    )
    .await?;

    Ok(result)
}

/// Generate timeline backward — identify what actually mattered.
pub async fn timeline_backward(
    reader: &Arc<Mutex<Connection>>,
    writer: &Arc<Mutex<Connection>>,
    slug: &str,
    forward_timeline: &str,
    base_config: &LlmConfig,
    model: &str,
) -> anyhow::Result<String> {
    let _ = reader; // reader not needed for this pass

    let prompt = format!(
        r#"You are reading a forward timeline of events. Now walk backwards and mark what actually mattered.
Which decisions were pivotal? Which explorations were dead ends? Which moments changed everything?

FORWARD TIMELINE:
{}

Output the reverse analysis as plain text. Be concise."#,
        forward_timeline
    );

    // Phase 3 fix pass: clone the live config (preserves provider_registry +
    // credential_store) instead of building a fresh `config_for_model`.
    let cfg = base_config.clone_with_model_override(model);
    let system_prompt =
        "You are a meta-analysis engine for a knowledge pyramid. Produce clear, concise analysis.";
    let cache_ctx = make_step_ctx_from_llm_config(
        &cfg,
        "meta_timeline_backward",
        "meta",
        -1,
        None,
        system_prompt,
    );
    let result = llm::call_model_and_ctx(
        &cfg,
        cache_ctx.as_ref(),
        system_prompt,
        &prompt,
        0.3,
        2000,
    )
    .await?;

    save_meta_node(
        writer,
        slug,
        "META-timeline-backward",
        "Timeline (Backward)",
        &result,
    )
    .await?;

    Ok(result)
}

/// Generate narrative — the story arc from both timelines.
pub async fn narrative(
    reader: &Arc<Mutex<Connection>>,
    writer: &Arc<Mutex<Connection>>,
    slug: &str,
    forward_timeline: &str,
    backward_timeline: &str,
    base_config: &LlmConfig,
    model: &str,
) -> anyhow::Result<String> {
    let _ = reader; // reader not needed for this pass

    let prompt = format!(
        r#"You are reading both forward and backward timelines. Write the STORY — not what happened, but WHY it happened, what the arc was, where the breakthroughs were.

FORWARD TIMELINE:
{}

BACKWARD ANALYSIS:
{}

Output the narrative as plain text. This should read like a story, not a report."#,
        forward_timeline, backward_timeline
    );

    // Phase 3 fix pass: clone the live config (preserves provider_registry +
    // credential_store) instead of building a fresh `config_for_model`.
    let cfg = base_config.clone_with_model_override(model);
    let system_prompt =
        "You are a meta-analysis engine for a knowledge pyramid. Produce clear, concise analysis.";
    let cache_ctx = make_step_ctx_from_llm_config(
        &cfg,
        "meta_narrative",
        "meta",
        -1,
        None,
        system_prompt,
    );
    let result = llm::call_model_and_ctx(
        &cfg,
        cache_ctx.as_ref(),
        system_prompt,
        &prompt,
        0.3,
        2000,
    )
    .await?;

    save_meta_node(writer, slug, "META-narrative", "Narrative", &result).await?;

    Ok(result)
}

/// Generate quickstart — compressed full grok for onboarding.
pub async fn quickstart(
    reader: &Arc<Mutex<Connection>>,
    writer: &Arc<Mutex<Connection>>,
    slug: &str,
    narrative_text: &str,
    base_config: &LlmConfig,
    model: &str,
) -> anyhow::Result<String> {
    // Also read thread chain tips and web edges for completeness
    let mut context = String::new();
    {
        let conn = reader.lock().await;
        let threads = db::get_threads(&conn, slug)?;
        context.push_str("THREAD CHAIN TIPS:\n");
        for t in &threads {
            if let Ok(Some(dist)) = db::get_distillation(&conn, slug, &t.thread_id) {
                context.push_str(&format!(
                    "- {}: {}\n",
                    t.thread_name,
                    crate::utils::safe_slice_end(&dist.content, 200)
                ));
            }
        }

        let edges = db::get_web_edges(&conn, slug)?;
        if !edges.is_empty() {
            context.push_str("\nCONNECTIONS:\n");
            for e in &edges {
                context.push_str(&format!(
                    "- {} ↔ {}: {}\n",
                    e.thread_a_id,
                    e.thread_b_id,
                    crate::utils::safe_slice_end(&e.relationship, 100)
                ));
            }
        }
    }

    let prompt = format!(
        r#"You are producing a compressed full understanding for onboarding. Someone reading this should GROK the entire knowledge base — not just the highlights, but the shape, the connections, the arc, the direction.

This is not a summary. It's compressed intuition. The reader should understand not just WHAT exists but WHY each piece exists and how everything connects.

NARRATIVE:
{}

CURRENT STATE:
{}

Produce the quickstart (target: under 1500 tokens). The reader should have the same understanding as someone who was there from the beginning."#,
        narrative_text, context
    );

    // Phase 3 fix pass: clone the live config (preserves provider_registry +
    // credential_store) instead of building a fresh `config_for_model`.
    let cfg = base_config.clone_with_model_override(model);
    let system_prompt =
        "You are a meta-analysis engine for a knowledge pyramid. Produce clear, concise analysis.";
    let cache_ctx = make_step_ctx_from_llm_config(
        &cfg,
        "meta_quickstart",
        "meta",
        -1,
        None,
        system_prompt,
    );
    let result = llm::call_model_and_ctx(
        &cfg,
        cache_ctx.as_ref(),
        system_prompt,
        &prompt,
        0.2,
        2000,
    )
    .await?;

    save_meta_node(writer, slug, "META-quickstart", "Quickstart", &result).await?;

    Ok(result)
}

/// Run all 4 meta passes in sequence.
pub async fn run_all_meta_passes(
    reader: &Arc<Mutex<Connection>>,
    writer: &Arc<Mutex<Connection>>,
    slug: &str,
    base_config: &LlmConfig,
    model: &str,
) -> anyhow::Result<String> {
    info!("[meta] Running timeline forward for '{}'", slug);
    let forward = timeline_forward(reader, writer, slug, base_config, model).await?;

    info!("[meta] Running timeline backward for '{}'", slug);
    let backward = timeline_backward(reader, writer, slug, &forward, base_config, model).await?;

    info!("[meta] Running narrative for '{}'", slug);
    let narr = narrative(reader, writer, slug, &forward, &backward, base_config, model).await?;

    info!("[meta] Running quickstart for '{}'", slug);
    let qs = quickstart(reader, writer, slug, &narr, base_config, model).await?;

    info!("[meta] All meta passes complete for '{}'", slug);
    Ok(qs)
}

/// Save a meta node to the pyramid.
async fn save_meta_node(
    writer: &Arc<Mutex<Connection>>,
    slug: &str,
    node_id: &str,
    title: &str,
    content: &str,
) -> anyhow::Result<()> {
    let node = PyramidNode {
        id: node_id.to_string(),
        slug: slug.to_string(),
        depth: -1,
        chunk_index: None,
        headline: title.to_string(),
        distilled: content.to_string(),
        topics: vec![Topic {
            name: title.to_string(),
            current: content.to_string(),
            entities: vec![],
            corrections: vec![],
            decisions: vec![],
            extra: serde_json::Map::new(),
        }],
        corrections: vec![],
        decisions: vec![],
        terms: vec![],
        dead_ends: vec![],
        self_prompt: String::new(),
        children: vec![],
        parent_id: None,
        superseded_by: None,
        build_id: None,
        created_at: String::new(),
        ..Default::default()
    };

    let conn = writer.lock().await;
    db::save_node(&conn, &node, None)?;

    Ok(())
}

/// Get all META nodes for a slug (depth = -1).
pub fn get_meta_nodes(conn: &Connection, slug: &str) -> anyhow::Result<Vec<PyramidNode>> {
    let nodes = db::get_nodes_at_depth(conn, slug, -1)?;
    Ok(nodes)
}
