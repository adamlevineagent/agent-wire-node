// pyramid/faq.rs — FAQ engine: auto-generates FAQ nodes from annotations
//
// Three core functions:
//   process_annotation — called after every annotation save; creates/updates FAQ
//   match_faq          — given a free-text question, find the best matching FAQ
//   update_faq_answer  — merge new annotation content into an existing FAQ answer

use anyhow::Result;
use rusqlite::Connection;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};
use uuid::Uuid;

use super::config_helper::estimate_cost;
use super::db;
use super::llm::LlmConfig;
use super::llm::{
    call_model_with_override_and_ctx, call_model_with_usage_with_override_and_ctx, extract_json,
};
use super::step_context::make_step_ctx_from_llm_config_with_model;
use super::types::{FaqCategory, FaqCategoryEntry, FaqDirectory, FaqNode, PyramidAnnotation};

/// Called after every annotation is saved.
///
/// Uses LLM to check if the annotation's question_context matches any existing FAQ.
/// - If match found: updates the existing FAQ with new info, returns it.
/// - If no match AND annotation has question_context: creates new FAQ node, returns it.
/// - If no question_context: returns None (nothing to FAQ-ify).
pub async fn process_annotation(
    reader: &Arc<Mutex<Connection>>,
    writer: &Arc<Mutex<Connection>>,
    slug: &str,
    annotation: &PyramidAnnotation,
    base_config: &LlmConfig,
    model: &str,
) -> Result<Option<FaqNode>> {
    // Only process annotations that have a question_context
    let question_context = match &annotation.question_context {
        Some(q) if !q.trim().is_empty() => q.clone(),
        _ => {
            info!(
                "[faq] annotation {} has no question_context, skipping",
                annotation.id
            );
            return Ok(None);
        }
    };

    // Load existing FAQs for this slug
    let existing_faqs = {
        let conn = reader.lock().await;
        db::get_faq_nodes(&conn, slug)?
    };

    if existing_faqs.is_empty() {
        // No existing FAQs — create a new one directly
        info!(
            "[faq] no existing FAQs for slug '{}', creating new FAQ",
            slug
        );
        let faq =
            create_new_faq(writer, slug, &question_context, annotation, base_config, model).await?;
        return Ok(Some(faq));
    }

    // Build a numbered list of existing FAQ questions for the LLM (Change 3: include match_triggers)
    let faq_list: String = existing_faqs
        .iter()
        .enumerate()
        .map(|(i, f)| {
            format!(
                "{}. [{}] {} (triggers: {})",
                i + 1,
                f.id,
                f.question,
                f.match_triggers.join("; ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let system_prompt = "You are a FAQ deduplication engine. Given a new question and a list of existing FAQ entries, determine if the new question matches an existing FAQ. Respond with EXACTLY one line: either 'MATCH:<faq_id>' (using the bracketed ID from the list) if it matches an existing FAQ, or 'NEW' if it's a genuinely new question. No explanation.";

    let user_prompt = format!(
        "New question: {}\n\nExisting FAQs:\n{}",
        question_context, faq_list
    );

    // W3c: legacy clone_with_model_override removed. Model threads via
    // LlmCallOptions.model_override + explicit step_ctx model arg.
    let cache_ctx = make_step_ctx_from_llm_config_with_model(
        base_config,
        "faq_match_existing",
        "faq",
        -1,
        None,
        system_prompt,
        Some(model),
    );
    let response = call_model_with_override_and_ctx(
        base_config,
        model,
        cache_ctx.as_ref(),
        system_prompt,
        &user_prompt,
        0.1,
        100,
    )
    .await?;
    let response = response.trim();

    if let Some(faq_id) = response.strip_prefix("MATCH:") {
        let faq_id = faq_id.trim();
        info!(
            "[faq] annotation {} matched existing FAQ {}",
            annotation.id, faq_id
        );

        // Update the matched FAQ with new annotation content
        let updated = update_faq_answer(reader, writer, faq_id, annotation, base_config, model).await?;
        Ok(Some(updated))
    } else {
        // NEW — create a fresh FAQ
        info!(
            "[faq] annotation {} generates new FAQ for slug '{}'",
            annotation.id, slug
        );
        let faq =
            create_new_faq(writer, slug, &question_context, annotation, base_config, model).await?;
        Ok(Some(faq))
    }
}

/// Given a free-text question, find the best matching FAQ.
///
/// First tries keyword search across FAQ questions.
/// If ambiguous, uses LLM to pick the best match.
/// Increments hit_count on the matched FAQ.
pub async fn match_faq(
    reader: &Arc<Mutex<Connection>>,
    writer: &Arc<Mutex<Connection>>,
    slug: &str,
    question: &str,
    base_config: &LlmConfig,
    model: &str,
) -> Result<Option<FaqNode>> {
    let all_faqs = {
        let conn = reader.lock().await;
        db::get_faq_nodes(&conn, slug)?
    };

    if all_faqs.is_empty() {
        return Ok(None);
    }

    // Keyword-based pre-filter: split question into words and score each FAQ
    let query_words: Vec<String> = question
        .to_lowercase()
        .split_whitespace()
        .filter(|w| w.len() > 2) // skip very short words
        .map(|w| w.to_string())
        .collect();

    // Change 2: Score against BOTH faq.question AND each string in faq.match_triggers,
    // taking the maximum score across all strings
    let mut scored: Vec<(usize, &FaqNode)> = all_faqs
        .iter()
        .map(|faq| {
            // Score against the canonical question
            let question_lower = faq.question.to_lowercase();
            let question_score = query_words
                .iter()
                .filter(|w| question_lower.contains(w.as_str()))
                .count();

            // Score against each match trigger and take the max
            let trigger_score = faq
                .match_triggers
                .iter()
                .map(|trigger| {
                    let trigger_lower = trigger.to_lowercase();
                    query_words
                        .iter()
                        .filter(|w| trigger_lower.contains(w.as_str()))
                        .count()
                })
                .max()
                .unwrap_or(0);

            let best_score = question_score.max(trigger_score);
            (best_score, faq)
        })
        .collect();

    scored.sort_by(|a, b| b.0.cmp(&a.0));

    // If there's a clear winner with good overlap, use it directly
    if scored.len() == 1 && scored[0].0 > 0 {
        let matched = scored[0].1.clone();
        let conn = writer.lock().await;
        let _ = db::increment_faq_hit(&conn, &matched.id);
        return Ok(Some(matched));
    }

    // Filter to candidates with at least 1 keyword match
    let candidates: Vec<&FaqNode> = scored
        .iter()
        .filter(|(score, _)| *score > 0)
        .map(|(_, faq)| *faq)
        .collect();

    if candidates.is_empty() {
        // No keyword matches — try LLM against all FAQs
        return match_faq_with_llm(writer, question, &all_faqs, base_config, model).await;
    }

    if candidates.len() == 1 {
        let matched = candidates[0].clone();
        let conn = writer.lock().await;
        let _ = db::increment_faq_hit(&conn, &matched.id);
        return Ok(Some(matched));
    }

    // Multiple candidates — use LLM to disambiguate
    match_faq_with_llm(
        writer,
        question,
        &candidates.into_iter().cloned().collect::<Vec<_>>(),
        base_config,
        model,
    )
    .await
}

/// Use LLM to pick the best matching FAQ from a list.
async fn match_faq_with_llm(
    writer: &Arc<Mutex<Connection>>,
    question: &str,
    candidates: &[FaqNode],
    base_config: &LlmConfig,
    model: &str,
) -> Result<Option<FaqNode>> {
    // Change 2: Include match_triggers in the LLM disambiguation prompt
    let faq_list: String = candidates
        .iter()
        .enumerate()
        .map(|(i, f)| {
            format!(
                "{}. [{}] {} (triggers: {})",
                i + 1,
                f.id,
                f.question,
                f.match_triggers.join("; ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let system_prompt = "You are a FAQ matching engine. Given a user question and a list of FAQ entries, pick the best match. Respond with EXACTLY one line: either the FAQ ID in brackets (e.g. 'FAQ-abc123') if there's a good match, or 'NONE' if no FAQ adequately answers the question. No explanation.";

    let user_prompt = format!(
        "User question: {}\n\nAvailable FAQs:\n{}",
        question, faq_list
    );

    // W3c: legacy clone_with_model_override removed. Model threads via
    // LlmCallOptions.model_override + explicit step_ctx model arg.
    let cache_ctx = make_step_ctx_from_llm_config_with_model(
        base_config,
        "faq_disambiguate",
        "faq",
        -1,
        None,
        system_prompt,
        Some(model),
    );
    let response = call_model_with_override_and_ctx(
        base_config,
        model,
        cache_ctx.as_ref(),
        system_prompt,
        &user_prompt,
        0.1,
        100,
    )
    .await?;
    let response = response.trim();

    if response == "NONE" {
        return Ok(None);
    }

    // Find the matched FAQ by ID
    let matched = candidates.iter().find(|f| response.contains(&f.id));
    if let Some(faq) = matched {
        let conn = writer.lock().await;
        let _ = db::increment_faq_hit(&conn, &faq.id);
        Ok(Some(faq.clone()))
    } else {
        warn!("[faq] LLM returned unrecognized FAQ id: {}", response);
        Ok(None)
    }
}

/// Given an existing FAQ and a new annotation, use LLM to produce an updated answer.
///
/// Reads the current answer + new annotation content and produces a refined answer.
/// Also appends the annotation's question_context to match_triggers (deduplicated)
/// and optionally re-generalizes the canonical question.
/// Saves the updated FAQ with the new annotation_id appended.
pub async fn update_faq_answer(
    reader: &Arc<Mutex<Connection>>,
    writer: &Arc<Mutex<Connection>>,
    faq_id: &str,
    new_annotation: &PyramidAnnotation,
    base_config: &LlmConfig,
    model: &str,
) -> Result<FaqNode> {
    // Load the existing FAQ
    let mut faq = {
        let conn = reader.lock().await;
        db::get_faq_node(&conn, faq_id)?
            .ok_or_else(|| anyhow::anyhow!("FAQ node '{}' not found", faq_id))?
    };

    // W3c: legacy clone_with_model_override removed. Model threads via
    // LlmCallOptions.model_override + explicit step_ctx model arg.
    // --- Answer refinement ---
    let system_prompt = "You are a FAQ answer refiner. Given an existing FAQ answer and a new piece of information from an annotation, produce an updated, comprehensive answer that incorporates the new information. Keep it concise and well-structured. Return ONLY the updated answer text, no preamble.";

    let user_prompt = format!(
        "FAQ Question: {}\n\nCurrent Answer:\n{}\n\nNew Annotation Content:\n{}\n\nAnnotation Context: {}",
        faq.question,
        faq.answer,
        new_annotation.content,
        new_annotation.question_context.as_deref().unwrap_or("(none)")
    );

    let update_ctx = make_step_ctx_from_llm_config_with_model(
        base_config,
        "faq_update_answer",
        "faq",
        -1,
        None,
        system_prompt,
        Some(model),
    );
    let updated_answer = call_model_with_override_and_ctx(
        base_config,
        model,
        update_ctx.as_ref(),
        system_prompt,
        &user_prompt,
        0.3,
        2000,
    )
    .await?;

    // Append the new annotation ID
    if !faq.annotation_ids.contains(&new_annotation.id) {
        faq.annotation_ids.push(new_annotation.id);
    }

    // Add the node_id if not already present
    if !faq.related_node_ids.contains(&new_annotation.node_id) {
        faq.related_node_ids.push(new_annotation.node_id.clone());
    }

    faq.answer = updated_answer.trim().to_string();

    // --- Change 4: Accumulate match_triggers (deduplicated) ---
    if let Some(ref qc) = new_annotation.question_context {
        let qc_trimmed = qc.trim().to_string();
        if !qc_trimmed.is_empty() && !faq.match_triggers.contains(&qc_trimmed) {
            faq.match_triggers.push(qc_trimmed);
            info!("[faq] appended new match trigger to FAQ {}", faq.id);
        }
    }

    // --- Change 4: Optionally re-generalize the canonical question ---
    if faq.match_triggers.len() > 1 {
        let triggers_list = faq.match_triggers.join(", ");
        let regen_system = "You are a question generalization engine. Given a current canonical question, accumulated trigger questions, and a new annotation, decide if the canonical question should be broadened to cover all triggers. Reply with ONLY the updated question if it should change, or 'NO_CHANGE' if the current question is sufficient.";
        let regen_user = format!(
            "Current question: {}\nAccumulated triggers: {}\nNew annotation: {}",
            faq.question, triggers_list, new_annotation.content
        );
        let regen_ctx = make_step_ctx_from_llm_config_with_model(
            base_config,
            "faq_regeneralize",
            "faq",
            -1,
            None,
            regen_system,
            Some(model),
        );
        match call_model_with_override_and_ctx(
            base_config,
            model,
            regen_ctx.as_ref(),
            regen_system,
            &regen_user,
            0.2,
            200,
        )
        .await
        {
            Ok(regen_response) => {
                let regen_response = regen_response.trim();
                if regen_response != "NO_CHANGE" && !regen_response.is_empty() {
                    info!(
                        "[faq] re-generalized FAQ {} question: '{}' -> '{}'",
                        faq.id, faq.question, regen_response
                    );
                    faq.question = regen_response.to_string();
                }
            }
            Err(e) => {
                warn!("[faq] re-generalization LLM call failed for FAQ {}: {}, keeping current question", faq.id, e);
            }
        }
    }

    faq.updated_at = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    // Save the updated FAQ
    {
        let conn = writer.lock().await;
        db::save_faq_node(&conn, &faq)?;
    }

    info!(
        "[faq] updated FAQ {} with annotation {}",
        faq.id, new_annotation.id
    );
    Ok(faq)
}

/// Create a brand-new FAQ node from an annotation.
///
/// Change 1: If the annotation content contains "Generalized understanding:",
/// uses LLM to produce a generalized canonical question and stores the original
/// question_context as the first match trigger. Otherwise, gracefully degrades
/// to the old behavior (specific question, no triggers).
async fn create_new_faq(
    writer: &Arc<Mutex<Connection>>,
    slug: &str,
    question: &str,
    annotation: &PyramidAnnotation,
    base_config: &LlmConfig,
    model: &str,
) -> Result<FaqNode> {
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    // Check for the generalization signal in annotation content
    let (final_question, match_triggers) = if let Some(gen_pos) =
        annotation.content.find("Generalized understanding:")
    {
        // Extract the generalized text after the signal
        let generalized_text =
            annotation.content[gen_pos + "Generalized understanding:".len()..].trim();

        // W3c: legacy clone_with_model_override removed.

        let gen_system = "You are a question generalization engine. Produce a single one-sentence generalized question about the underlying mechanism. Output only the question, nothing else.";
        let gen_user = format!(
            "Given this specific question: {}\nand this analysis: {}\nproduce a single one-sentence generalized question about the underlying mechanism. Output only the question, nothing else.",
            question, generalized_text
        );

        let gen_ctx = make_step_ctx_from_llm_config_with_model(
            base_config,
            "faq_generalize",
            "faq",
            -1,
            None,
            gen_system,
            Some(model),
        );
        match call_model_with_override_and_ctx(
            base_config,
            model,
            gen_ctx.as_ref(),
            gen_system,
            &gen_user,
            0.2,
            200,
        )
        .await
        {
            Ok(generalized_question) => {
                let generalized_question = generalized_question.trim().to_string();
                if generalized_question.is_empty() {
                    warn!(
                        "[faq] LLM returned empty generalized question, falling back to original"
                    );
                    (question.to_string(), Vec::new())
                } else {
                    info!(
                        "[faq] generalized question: '{}' -> '{}'",
                        question, generalized_question
                    );
                    // Original question becomes the first match trigger
                    (generalized_question, vec![question.to_string()])
                }
            }
            Err(e) => {
                warn!(
                    "[faq] generalization LLM call failed: {}, falling back to original question",
                    e
                );
                (question.to_string(), Vec::new())
            }
        }
    } else {
        // No generalization signal — graceful degradation
        warn!("[faq] annotation {} lacks 'Generalized understanding:' signal, using specific question as-is", annotation.id);
        (question.to_string(), Vec::new())
    };

    let faq = FaqNode {
        id: format!("FAQ-{}", Uuid::new_v4()),
        slug: slug.to_string(),
        question: final_question,
        answer: annotation.content.clone(),
        related_node_ids: vec![annotation.node_id.clone()],
        annotation_ids: vec![annotation.id],
        hit_count: 0,
        match_triggers,
        created_at: now.clone(),
        updated_at: now,
    };

    {
        let conn = writer.lock().await;
        db::save_faq_node(&conn, &faq)?;
    }

    info!("[faq] created new FAQ {} for slug '{}'", faq.id, slug);
    Ok(faq)
}

// ── FAQ Directory / Category Engine ──────────────────────────────────────────

/// Get the FAQ directory for a slug.
///
/// Takes BOTH reader AND writer because the meta-pass creates categories.
/// If count < faq_category_threshold: returns flat mode with all FAQs.
/// If >= threshold: loads or creates categories via meta-pass.
/// Falls back to flat mode on any LLM failure.
pub async fn get_faq_directory(
    reader: &Arc<Mutex<Connection>>,
    writer: &Arc<Mutex<Connection>>,
    slug: &str,
    base_config: &LlmConfig,
    model: &str,
    tier2: &super::Tier2Config,
) -> Result<FaqDirectory> {
    let all_faqs = {
        let conn = reader.lock().await;
        db::get_faq_nodes(&conn, slug)?
    };

    let total_faqs = all_faqs.len() as i64;

    // Below threshold: flat mode
    if all_faqs.len() < tier2.faq_category_threshold {
        return Ok(FaqDirectory {
            slug: slug.to_string(),
            mode: "flat".to_string(),
            total_faqs,
            categories: Vec::new(),
            uncategorized: all_faqs,
        });
    }

    // Above threshold: try to load existing categories
    let existing_categories = {
        let conn = reader.lock().await;
        db::get_faq_categories(&conn, slug)?
    };

    let categories = if existing_categories.is_empty() {
        // No categories exist — run meta-pass
        match run_faq_category_meta_pass(reader, writer, slug, &all_faqs, base_config, model).await {
            Ok(cats) => cats,
            Err(e) => {
                warn!(
                    "[faq] category meta-pass failed for '{}': {}, falling back to flat mode",
                    slug, e
                );
                return Ok(FaqDirectory {
                    slug: slug.to_string(),
                    mode: "flat".to_string(),
                    total_faqs,
                    categories: Vec::new(),
                    uncategorized: all_faqs,
                });
            }
        }
    } else {
        existing_categories
    };

    // Build category entries
    let mut categorized_faq_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    let mut entries: Vec<FaqCategoryEntry> = Vec::new();

    for cat in categories {
        let faq_count = cat.faq_ids.len() as i64;
        for fid in &cat.faq_ids {
            categorized_faq_ids.insert(fid.clone());
        }
        entries.push(FaqCategoryEntry {
            category: cat,
            faq_count,
            children: None, // not populated on directory view — use drill
        });
    }

    // Collect uncategorized FAQs
    let uncategorized: Vec<FaqNode> = all_faqs
        .into_iter()
        .filter(|f| !categorized_faq_ids.contains(&f.id))
        .collect();

    Ok(FaqDirectory {
        slug: slug.to_string(),
        mode: "hierarchical".to_string(),
        total_faqs,
        categories: entries,
        uncategorized,
    })
}

/// Run the LLM meta-pass to cluster FAQs into 3-7 categories with distilled summaries.
///
/// Creates category rows in the DB. On malformed LLM response, returns error
/// (caller falls back to flat mode).
pub async fn run_faq_category_meta_pass(
    _reader: &Arc<Mutex<Connection>>,
    writer: &Arc<Mutex<Connection>>,
    slug: &str,
    faqs: &[FaqNode],
    base_config: &LlmConfig,
    model: &str,
) -> Result<Vec<FaqCategory>> {
    let faq_list: String = faqs
        .iter()
        .map(|f| {
            format!(
                "- [{}] Q: {} | A: {}",
                f.id,
                f.question,
                &f.answer[..f.answer.len().min(200)]
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let system_prompt = r#"You are a knowledge categorization engine. Given a list of FAQ entries, group them into 3-7 logical categories. Each category should have:
- name: a short descriptive name (2-5 words)
- faq_ids: array of FAQ IDs that belong to this category
- summary: a distilled paragraph that captures the mechanism-level knowledge of this group, so that ~50% of queries can be answered from the summary alone without drilling into individual FAQs

Return ONLY valid JSON: an array of objects with fields "name", "faq_ids", and "summary". No explanation."#;

    let user_prompt = format!(
        "Categorize these {} FAQ entries:\n\n{}",
        faqs.len(),
        faq_list
    );

    // W3c: legacy clone_with_model_override removed.
    let cat_ctx = make_step_ctx_from_llm_config_with_model(
        base_config,
        "faq_categorize",
        "faq",
        -1,
        None,
        system_prompt,
        Some(model),
    );
    let (response, usage) = call_model_with_usage_with_override_and_ctx(
        base_config,
        model,
        cat_ctx.as_ref(),
        system_prompt,
        &user_prompt,
        0.3,
        4096,
    )
    .await?;

    // Log cost
    let cost = estimate_cost(&usage);
    {
        let conn = writer.lock().await;
        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let _ = conn.execute(
            "INSERT INTO pyramid_cost_log (slug, operation, model, input_tokens, output_tokens, estimated_cost, source, layer, check_type, created_at, chain_id, step_name, tier, latency_ms, generation_id, estimated_cost_usd) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'auto-stale', 0, 'faq_category', ?7, NULL, NULL, NULL, NULL, NULL, NULL)",
            rusqlite::params![slug, "faq_category_meta_pass", model, usage.prompt_tokens, usage.completion_tokens, cost, now],
        );
    }

    // Parse the LLM response
    let json_val = extract_json(&response)?;
    let categories_raw: Vec<serde_json::Value> = json_val
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("LLM returned non-array for FAQ categories"))?
        .clone();

    if categories_raw.is_empty() || categories_raw.len() > 10 {
        anyhow::bail!(
            "LLM returned {} categories (expected 3-7)",
            categories_raw.len()
        );
    }

    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let mut result: Vec<FaqCategory> = Vec::new();

    // Delete old categories before inserting new ones
    {
        let conn = writer.lock().await;
        db::delete_faq_categories(&conn, slug)?;
    }

    for raw in categories_raw {
        let name = raw
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("Uncategorized")
            .to_string();

        let summary = raw
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let faq_ids: Vec<String> = raw
            .get("faq_ids")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let cat = FaqCategory {
            id: format!("FAQCAT-{}", uuid::Uuid::new_v4()),
            slug: slug.to_string(),
            name,
            distilled_summary: summary,
            faq_ids,
            created_at: now.clone(),
            updated_at: now.clone(),
        };

        {
            let conn = writer.lock().await;
            db::save_faq_category(&conn, &cat)?;
        }

        result.push(cat);
    }

    info!(
        "[faq] created {} categories for slug '{}'",
        result.len(),
        slug
    );
    Ok(result)
}

/// Drill into a specific FAQ category — load the category and its child FAQs.
pub async fn drill_faq_category(
    reader: &Arc<Mutex<Connection>>,
    slug: &str,
    category_id: &str,
) -> Result<FaqCategoryEntry> {
    let conn = reader.lock().await;

    let cat = db::get_faq_category(&conn, category_id)?
        .ok_or_else(|| anyhow::anyhow!("FAQ category '{}' not found", category_id))?;

    if cat.slug != slug {
        anyhow::bail!(
            "Category '{}' does not belong to slug '{}'",
            category_id,
            slug
        );
    }

    // Load child FAQs by their IDs
    let all_faqs = db::get_faq_nodes(&conn, slug)?;
    let children: Vec<FaqNode> = all_faqs
        .into_iter()
        .filter(|f| cat.faq_ids.contains(&f.id))
        .collect();

    let faq_count = children.len() as i64;

    Ok(FaqCategoryEntry {
        category: cat,
        faq_count,
        children: Some(children),
    })
}
